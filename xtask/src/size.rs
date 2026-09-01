//! The size gate.
//!
//! Design document §04 states three of the four budgets as numbers a build can be
//! measured against, and says of the code-flash one that it "is a gate, not an unverified
//! claim". This module is the gate. It links [`PROBE_PACKAGE`] once per feature
//! combination on [`crate::pipeline::FIRMWARE_TARGET`] with the release-size profile,
//! reads the section headers out of each image, and compares every row against a baseline
//! image that links no Waymaker at all.
//!
//! # Why a delta
//!
//! The budget is incremental: "≤ 8 KiB core + flash adapter". An absolute size would
//! charge Waymaker for the panic handler, the ARM exception index, and whatever else a
//! bare image carries, and would drift with the toolchain rather than with this
//! repository. Two images and a subtraction measure the thing the budget is about.
//!
//! # Why the matrix is derived
//!
//! §04 also requires that "adding Serde, Postcard, `defmt`, Embassy, or a CRC
//! implementation must show its own incremental cost". A hand-written list of feature
//! combinations is a list a new feature can be left out of, silently, by the pull request
//! that would most want measuring. [`matrix`] therefore reads the features each layer
//! declares out of `cargo metadata`, so adding a feature adds a row and there is nothing
//! to remember.
//!
//! # What is gated and what is only reported
//!
//! The `default` and `facade` rows are gated: they are the engine as it ships with no
//! optional cost enabled, which is what the v0.1 targets describe. A per-feature row is
//! reported with its incremental cost but not gated, because the design document sets no
//! per-feature budget — it requires the cost to be *shown*. The base-branch diff is what
//! makes an unbudgeted row's growth visible in review.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::Violation;
use crate::coverage::uninstrumented_cargo;
use crate::elf::{self, Section};
use crate::graph::PackageGraph;
use crate::pipeline::FIRMWARE_TARGET;
use crate::policy;

/// The example firmware the matrix links.
pub const PROBE_PACKAGE: &str = "waymaker-size-probe";

/// The probe feature that builds the binary at all.
pub const PROBE_FEATURE: &str = "probe";

/// The probe feature that links the kernel and the flash adapter.
pub const ENGINE_FEATURE: &str = "engine";

/// The probe feature that also links the Embassy façade.
pub const FACADE_FEATURE: &str = "facade";

/// Where `cargo xtask size` writes the report it then gates and uploads.
pub const REPORT_PATH: &str = "target/waymaker-size.json";

/// Where the base-branch worktree is checked out, relative to the workspace root.
///
/// Under `target/` so that it is already ignored by git and already removed by
/// `cargo clean`, and suffixed with the process id by [`baseline_worktree`] so that two
/// gates running at once — two tests, or a developer and a hook — cannot check out over
/// one another.
const BASELINE_WORKTREE_PATH: &str = "target/waymaker-size-base";

/// The directory this process checks the base branch out into.
#[must_use]
fn baseline_worktree(root: &Path) -> PathBuf {
    root.join(format!("{BASELINE_WORKTREE_PATH}-{}", std::process::id()))
}

/// The target directory the matrix builds into.
///
/// Its own directory so that a size run does not evict the rest of the pipeline's build
/// cache: each row is a different feature selection of the same crates, so they would
/// otherwise take turns invalidating one another and everything else.
///
/// Each variant then gets a subdirectory of its own, from [`variant_build_dir`]. Cargo's
/// build lock serialises the builds but not the read that follows one, and every variant
/// links to the same file name — so a shared directory lets one run's `baseline` image be
/// uplifted over another run's `default` between the build and the read. That is not only
/// a flaky test: the row that gets read is a real image of the wrong variant, so a delta
/// of zero passes the gate with nothing to show for it.
const BUILD_DIR: &str = "target/waymaker-size-build";

/// The directory one variant links into, named after the variant.
fn variant_build_dir(build_dir: &Path, variant: &str) -> PathBuf {
    // `/` appears in every feature row's name (`waymaker-core/serde`) and would otherwise
    // make a nested directory per crate; the other two are for the benefit of any future
    // feature name a filesystem would object to.
    let slug: String = variant
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => character,
            _ => '-',
        })
        .collect();
    build_dir.join(slug)
}

/// The version stamped into the JSON report.
const REPORT_SCHEMA: u64 = 1;

/// The row every other row is an increment on: an image with no Waymaker in it.
pub const BASELINE_ROW: &str = "baseline";

/// The engine as it ships with no optional cost enabled.
pub const DEFAULT_ROW: &str = "default";

/// Incremental code-flash budget, from [`waymaker_core::budget`].
pub const INCREMENTAL_CODE_FLASH_BUDGET_BYTES: u64 =
    waymaker_core::budget::INCREMENTAL_CODE_FLASH_BYTES as u64;

/// Runtime RAM the engine may own, once the caller's scratch page is accounted for.
pub const ENGINE_RAM_BUDGET_BYTES: u64 = waymaker_core::budget::ENGINE_RAM_BYTES as u64;

/// Kernel state budget, from [`waymaker_core::budget`].
pub const KERNEL_STATE_BUDGET_BYTES: u64 = waymaker_core::budget::KERNEL_STATE_BYTES as u64;

/// One image the matrix links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    /// How the row is named in the report: `baseline`, `default`, `facade`, or
    /// `<crate>/<feature>`.
    pub name: String,
    /// The feature selection passed to `cargo build --features`.
    pub features: Vec<String>,
    /// The row whose cost this one is an increment on: `baseline` for the engine rows,
    /// `default` or `facade` for a feature row.
    ///
    /// Recorded rather than inferred so that the report can say what a feature actually
    /// cost, and so that a feature row identical to its base can be named as the
    /// unexercised measurement it is.
    pub measured_against: String,
    /// Whether exceeding a budget on this row fails the gate.
    pub gated: bool,
}

/// Every image the matrix links, in report order.
///
/// The first row is always the baseline: an image built with none of the layers linked,
/// which every other row is measured against. Returns nothing for a workspace that has no
/// size probe, which is how a base branch from before this gate existed is recognised
/// rather than measured as zero.
#[must_use]
pub fn matrix(graph: &PackageGraph) -> Vec<Variant> {
    if graph.find(PROBE_PACKAGE).is_none() {
        return Vec::new();
    }

    let mut variants = vec![
        Variant {
            name: BASELINE_ROW.to_owned(),
            features: vec![PROBE_FEATURE.to_owned()],
            measured_against: BASELINE_ROW.to_owned(),
            gated: false,
        },
        Variant {
            name: DEFAULT_ROW.to_owned(),
            features: vec![PROBE_FEATURE.to_owned(), ENGINE_FEATURE.to_owned()],
            measured_against: BASELINE_ROW.to_owned(),
            gated: true,
        },
        Variant {
            // Reported, not gated. Design document §04 states the 8 KiB for "core + flash
            // adapter", and the Embassy façade is neither. Gating it here would either
            // fail a build for a cost the budget never covered, or — worse, once someone
            // raised the number to make it pass — quietly widen the kernel's budget to pay
            // for the façade. The façade's own cost is the `Δ vs default` column, and it
            // gets a budget of its own in `waymaker_core::budget` when it needs one.
            name: FACADE_FEATURE.to_owned(),
            features: vec![PROBE_FEATURE.to_owned(), FACADE_FEATURE.to_owned()],
            measured_against: DEFAULT_ROW.to_owned(),
            gated: false,
        },
    ];

    for spec in policy::LAYERS {
        let Some(package) = graph.find(spec.name) else {
            continue;
        };
        // The façade's own features need the façade linked; everything below it is
        // measured on the engine. Derived from the crate name rather than from a table, so
        // a feature added to either crate lands in the right image without a decision.
        let base = if spec.name == policy::EMBASSY_FACADE {
            FACADE_FEATURE
        } else {
            ENGINE_FEATURE
        };
        for feature in &package.features {
            // `default` is not an optional cost, it is the absence of one, and design
            // document §04 requires it to stay empty. The `default` row above already
            // measures it.
            if feature == "default" {
                continue;
            }
            variants.push(Variant {
                name: format!("{}/{feature}", spec.name),
                features: vec![
                    PROBE_FEATURE.to_owned(),
                    base.to_owned(),
                    format!("{}/{feature}", spec.name),
                ],
                measured_against: if base == FACADE_FEATURE {
                    FACADE_FEATURE.to_owned()
                } else {
                    DEFAULT_ROW.to_owned()
                },
                gated: false,
            });
        }
    }

    variants
}

/// The section sizes of one linked image.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SectionSizes {
    /// `.text` and the sections the linker split out of it.
    pub text: u64,
    /// `.rodata` and its split-out sections.
    pub rodata: u64,
    /// `.data` and its split-out sections.
    pub data: u64,
    /// `.bss` and its split-out sections.
    pub bss: u64,
    /// Every allocated section whose bytes are stored in the image.
    ///
    /// Wider than `text + rodata + data` on purpose: `.ARM.exidx` costs flash and is
    /// named after neither. The budget is about bytes programmed into the part, so the
    /// gated number is the one that counts all of them.
    pub flash: u64,
    /// Every allocated section that is writable and not thread-local, which is what
    /// occupies RAM.
    ///
    /// Thread-local sections are excluded because `.tdata` and `.tbss` are a template that
    /// a thread's storage is initialised *from*, not storage itself, so counting both
    /// charges the same bytes twice. Nothing else is excluded: without a linker script
    /// there is no memory map to say that `.got` or `.init_array` were placed in flash, so
    /// they are counted as RAM. That errs toward failing the gate rather than passing it,
    /// which is the direction a budget should err in.
    pub ram: u64,
}

/// `SHF_TLS`: the section holds thread-local storage.
const SHF_TLS: u64 = 0x400;

/// The named sections the report breaks out.
///
/// Order is not significant: no entry is a prefix of another under [`in_section`], which
/// matches either the whole name or the name followed by a `.`.
const REPORTED_SECTIONS: &[&str] = &[".text", ".rodata", ".data", ".bss"];

impl SectionSizes {
    /// Adds up the sections of one image.
    #[must_use]
    pub fn of(sections: &[Section]) -> Self {
        let mut sizes = Self::default();
        for section in sections {
            if section.occupies_storage() {
                sizes.flash = sizes.flash.saturating_add(section.size);
            }
            if section.allocated() && section.writable() && section.flags & SHF_TLS == 0 {
                sizes.ram = sizes.ram.saturating_add(section.size);
            }
            // `.text.unlikely`, `.bss.probe` and friends: the linker splits a section and
            // names the pieces after it, and a report that missed them would show a
            // shrinking `.text` for a growing image.
            match REPORTED_SECTIONS
                .iter()
                .find(|name| in_section(&section.name, name))
            {
                Some(&".text") => sizes.text = sizes.text.saturating_add(section.size),
                Some(&".rodata") => sizes.rodata = sizes.rodata.saturating_add(section.size),
                Some(&".data") => sizes.data = sizes.data.saturating_add(section.size),
                Some(&".bss") => sizes.bss = sizes.bss.saturating_add(section.size),
                _ => {}
            }
        }
        sizes
    }

    /// This image's sizes minus `baseline`'s, floored at zero.
    ///
    /// Saturating rather than signed: a row that links *less* than the baseline is not a
    /// negative cost, it is a measurement error or a linker that dropped something, and
    /// letting it offset another row's growth is exactly the arithmetic a budget must not
    /// do.
    #[must_use]
    pub const fn saturating_delta(&self, baseline: &Self) -> Self {
        Self {
            text: self.text.saturating_sub(baseline.text),
            rodata: self.rodata.saturating_sub(baseline.rodata),
            data: self.data.saturating_sub(baseline.data),
            bss: self.bss.saturating_sub(baseline.bss),
            flash: self.flash.saturating_sub(baseline.flash),
            ram: self.ram.saturating_sub(baseline.ram),
        }
    }
}

/// Whether `name` is `section` or one of the pieces the linker split out of it.
fn in_section(name: &str, section: &str) -> bool {
    name.strip_prefix(section)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// One measured row of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The variant's name.
    pub name: String,
    /// The feature selection the image was built with.
    pub features: Vec<String>,
    /// The row whose cost this one is an increment on.
    pub measured_against: String,
    /// The measured sections.
    pub sizes: SectionSizes,
    /// Whether exceeding a budget on this row fails the gate.
    pub gated: bool,
}

impl Row {
    /// Records one measurement.
    #[must_use]
    pub fn new(
        name: &str,
        features: &[&str],
        measured_against: &str,
        sizes: SectionSizes,
        gated: bool,
    ) -> Self {
        Self {
            name: name.to_owned(),
            features: features.iter().map(|f| (*f).to_owned()).collect(),
            measured_against: measured_against.to_owned(),
            sizes,
            gated,
        }
    }
}

/// The kernel's live state, as `waymaker-core` declares it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KernelState {
    /// The sum of [`Self::types`].
    pub total: u64,
    /// Each registered type and its size.
    pub types: Vec<(String, u64)>,
}

impl KernelState {
    /// Reads the registry out of [`waymaker_core::budget`].
    ///
    /// These are sizes for the host, because that is the target `xtask` is compiled for.
    /// The budget is stated for `thumbv6m-none-eabi`, where a type holding a pointer is
    /// *smaller* — so the host figure is an upper bound on the target one. Gating it is
    /// therefore conservative: it can fail early, never late, and a build it fails might
    /// have fitted on the target. The authoritative check is the `const` assertion in
    /// [`waymaker_core::budget`], which every row of the matrix but the baseline evaluates,
    /// because every one of those compiles `waymaker-core` for the firmware target.
    #[must_use]
    pub fn measured() -> Self {
        Self {
            total: waymaker_core::budget::KERNEL_STATE_TOTAL_BYTES as u64,
            types: waymaker_core::budget::KERNEL_STATE_TYPES
                .iter()
                .map(|entry| (entry.name.to_owned(), entry.size as u64))
                .collect(),
        }
    }
}

/// One of the gates a measurement is held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// Design document §04's 8 KiB for the kernel plus the flash adapter.
    IncrementalCodeFlash,
    /// What the engine may own in statics once the caller's scratch page is counted.
    ///
    /// Named for what it measures. Section sizes see `.data` and `.bss` and nothing else,
    /// so this is a floor on §04's runtime RAM rather than the rule itself: a cursor,
    /// context or record header that lives on the caller's stack moves no writable section,
    /// and neither does a deeper call frame. Calling this "runtime RAM" would report a
    /// budget as enforced that section sizes cannot enforce.
    EngineStatics,
    /// `waymaker-core` state only, no page buffer.
    KernelState,
}

impl Budget {
    /// The limit in bytes.
    #[must_use]
    pub const fn limit(self) -> u64 {
        match self {
            Self::IncrementalCodeFlash => INCREMENTAL_CODE_FLASH_BUDGET_BYTES,
            Self::EngineStatics => ENGINE_RAM_BUDGET_BYTES,
            Self::KernelState => KERNEL_STATE_BUDGET_BYTES,
        }
    }

    /// The budget's name, as design document §04 writes it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::IncrementalCodeFlash => "incremental code flash",
            Self::EngineStatics => "engine statics (.data + .bss)",
            Self::KernelState => "kernel state",
        }
    }
}

/// Why the gate failed.
///
/// Two shapes, because there are two ways to fail and they read very differently. A budget
/// was exceeded, and the message names the measurement and the limit; or the report cannot
/// be held to a budget at all, and the message says what is missing. Forcing the second
/// through the first produced "missing baseline on baseline: 0 B measured against a 0 B
/// budget, over by 0 B", which tells a reader at two in the morning nothing whatsoever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetShortfall {
    /// A measurement exceeded its budget.
    Exceeded {
        /// Which budget.
        budget: Budget,
        /// The row the number was measured on.
        subject: String,
        /// What was measured.
        measured: u64,
    },
    /// The report cannot be gated, so it has not passed.
    Unmeasurable {
        /// What is missing.
        detail: String,
    },
}

impl fmt::Display for BudgetShortfall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exceeded {
                budget,
                subject,
                measured,
            } => write!(
                f,
                "{} on `{subject}`: {measured} B measured against a {} B budget, over by {} B",
                budget.name(),
                budget.limit(),
                measured.saturating_sub(budget.limit())
            ),
            Self::Unmeasurable { detail } => {
                write!(f, "nothing was measured: {detail}")
            }
        }
    }
}

/// The measured size of every image in the matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeReport {
    rows: Vec<Row>,
    kernel_state: KernelState,
}

impl SizeReport {
    /// Collects measured rows into a report.
    #[must_use]
    pub const fn new(rows: Vec<Row>, kernel_state: KernelState) -> Self {
        Self { rows, kernel_state }
    }

    /// Every measured row, in matrix order.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The kernel state registry the report was taken with.
    #[must_use]
    pub const fn kernel_state(&self) -> &KernelState {
        &self.kernel_state
    }

    /// The row every other row is measured against.
    #[must_use]
    pub fn baseline(&self) -> Option<&Row> {
        self.rows.iter().find(|row| row.name == BASELINE_ROW)
    }

    /// The row named `name`.
    #[must_use]
    pub fn row(&self, name: &str) -> Option<&Row> {
        self.rows.iter().find(|row| row.name == name)
    }

    /// How much bigger `name` is than the baseline.
    #[must_use]
    pub fn delta_of(&self, name: &str) -> Option<SectionSizes> {
        let baseline = self.baseline()?;
        let row = self.row(name)?;
        Some(row.sizes.saturating_delta(&baseline.sizes))
    }

    /// How much bigger `name` is than the row it is an increment on.
    ///
    /// For a feature row that is the feature's own cost, which is the number design
    /// document §04 asks every optional feature to show.
    #[must_use]
    pub fn increment_of(&self, name: &str) -> Option<SectionSizes> {
        let row = self.row(name)?;
        if row.measured_against == row.name {
            return None;
        }
        let base = self.row(&row.measured_against)?;
        Some(row.sizes.saturating_delta(&base.sizes))
    }

    /// Rows that measured exactly what they are an increment on, and so measured nothing.
    ///
    /// A feature row is built by enabling the feature and linking the probe again. If the
    /// probe never calls anything the feature adds, `--gc-sections` and fat LTO discard all
    /// of it and the row comes back byte for byte identical to its base. The row is still
    /// derived automatically — nobody has to remember to add it — but its number is zero
    /// for a reason that has nothing to do with the feature being free.
    ///
    /// This cannot be a gate, because a feature that genuinely costs nothing is
    /// indistinguishable from one the probe does not exercise. So it is said out loud,
    /// every run, naming the row and what to do about it.
    #[must_use]
    pub fn notices(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|row| row.measured_against != row.name)
            .filter(|row| {
                self.row(&row.measured_against)
                    .is_some_and(|base| base.sizes == row.sizes)
            })
            .map(|row| {
                format!(
                    "`{}` measured exactly the same image as `{}`, so its incremental cost is 0 B: either it costs nothing, or {} does not reach any code the feature adds and the linker discarded it. Design document \u{a7}04 asks every optional feature to show its own cost, so give the probe something to call.",
                    row.name, row.measured_against, PROBE_PACKAGE
                )
            })
            .collect()
    }

    /// Every budget this report exceeds.
    ///
    /// A report with no baseline row is itself a shortfall: without one there is no delta
    /// to measure, and a gate that quietly passed in that state would pass on every run
    /// where the baseline build failed.
    #[must_use]
    pub fn shortfalls(&self) -> Vec<BudgetShortfall> {
        let mut shortfalls = Vec::new();

        if self.kernel_state.total > KERNEL_STATE_BUDGET_BYTES {
            shortfalls.push(BudgetShortfall::Exceeded {
                budget: Budget::KernelState,
                subject: "waymaker-core".to_owned(),
                measured: self.kernel_state.total,
            });
        }

        // No `if !rows.is_empty()` escape. A report with no rows is a report of a run that
        // did not happen — a truncated artifact, a build that produced nothing — and the
        // one thing it must not do is exit zero.
        if self.rows.is_empty() {
            shortfalls.push(BudgetShortfall::Unmeasurable {
                detail: "the report has no rows, so no image was linked".to_owned(),
            });
            return shortfalls;
        }

        let Some(baseline) = self.baseline() else {
            shortfalls.push(BudgetShortfall::Unmeasurable {
                detail: "the report has no `baseline` row, so there is nothing to measure the other rows against".to_owned(),
            });
            return shortfalls;
        };

        // An image with no stored bytes at all is not a small image, it is a file the
        // linker did not produce or the parser could not read. Section headers can be
        // stripped, flags cleared, or a wrong artifact measured, and every one of those
        // reads as a delta of zero against a delta of zero.
        if baseline.sizes.flash == 0 {
            shortfalls.push(BudgetShortfall::Unmeasurable {
                detail: "the baseline image reports no bytes in flash at all, which no linked firmware does; its section headers were probably stripped or the wrong file was measured".to_owned(),
            });
        }

        for row in self.rows.iter().filter(|row| row.gated) {
            let delta = row.sizes.saturating_delta(&baseline.sizes);
            if delta.flash > INCREMENTAL_CODE_FLASH_BUDGET_BYTES {
                shortfalls.push(BudgetShortfall::Exceeded {
                    budget: Budget::IncrementalCodeFlash,
                    subject: row.name.clone(),
                    measured: delta.flash,
                });
            }
            if delta.ram > ENGINE_RAM_BUDGET_BYTES {
                shortfalls.push(BudgetShortfall::Exceeded {
                    budget: Budget::EngineStatics,
                    subject: row.name.clone(),
                    measured: delta.ram,
                });
            }
        }

        shortfalls
    }

    /// Why the gate failed, or `None` if it did not.
    #[must_use]
    pub fn shortfall_report(&self) -> Option<String> {
        let shortfalls = self.shortfalls();
        if shortfalls.is_empty() {
            return None;
        }
        let mut message = vec![format!(
            "{} budget(s) exceeded, measured on {FIRMWARE_TARGET} with the release-size profile:",
            shortfalls.len()
        )];
        for shortfall in &shortfalls {
            message.push(format!("\n  {shortfall}"));
        }
        message.push(
            "\n\nThe budgets are design document \u{a7}04. They are gates rather than claims: a change that needs more space needs the table changed in waymaker_core::budget, in the same pull request, with a reason."
                .to_owned(),
        );
        Some(message.concat())
    }

    /// A table with one row per image, its section deltas, and what it cost over its base.
    #[must_use]
    pub fn render(&self) -> String {
        let width = self
            .rows
            .iter()
            .map(|row| row.name.len())
            .max()
            .unwrap_or(0)
            .max("variant".len());

        let mut table = vec![
            format!("section sizes on {FIRMWARE_TARGET}, release-size profile\n"),
            format!(
                "  {:<width$}  {:>9} {:>9} {:>9} {:>9}  {:>9} {:>9}  {:>12}\n",
                "variant",
                "\u{394}.text",
                "\u{394}.rodata",
                "\u{394}.data",
                "\u{394}.bss",
                "\u{394}flash",
                "\u{394}ram",
                "over base",
            ),
        ];

        for row in &self.rows {
            // The baseline's own row shows what a firmware with no Waymaker in it costs,
            // so its columns are absolute and everything else is a delta against it.
            let delta = self.delta_of(&row.name).unwrap_or(row.sizes);
            let increment = self.increment_of(&row.name).map_or_else(
                || "-".to_owned(),
                |increment| format!("+{} flash", increment.flash),
            );
            table.push(format!(
                "  {:<width$}  {:>9} {:>9} {:>9} {:>9}  {:>9} {:>9}  {increment:>12}{}\n",
                row.name,
                delta.text,
                delta.rodata,
                delta.data,
                delta.bss,
                delta.flash,
                delta.ram,
                if row.gated { "  gated" } else { "" },
            ));
        }

        table.push(format!(
            "\nbudgets: incremental code flash {INCREMENTAL_CODE_FLASH_BUDGET_BYTES} B, engine statics {ENGINE_RAM_BUDGET_BYTES} B (of {} B runtime RAM, less a {} B caller-owned scratch page)\n",
            waymaker_core::budget::RUNTIME_RAM_BYTES,
            waymaker_core::budget::SCRATCH_PAGE_BYTES,
        ));
        table.push(
            "runtime RAM: statics only. A cursor, context or record header on the caller's stack moves no writable section, so \u{394}ram is a floor on design document \u{a7}04's runtime RAM and not the rule itself; stack accounting needs a call graph and arrives with the code that has one.\n"
                .to_owned(),
        );
        table.push(format!(
            "kernel state: {} B of {KERNEL_STATE_BUDGET_BYTES} B across {} registered type(s), sized for the host, which is an upper bound on the target; the gate for {FIRMWARE_TARGET} is the const assertion in waymaker_core::budget, which every row above but the baseline compiles\n",
            self.kernel_state.total,
            self.kernel_state.types.len(),
        ));
        for notice in self.notices() {
            table.push(format!("\nnotice: {notice}\n"));
        }
        table.concat()
    }

    /// The report as JSON, for the CI artifact and for the base-branch diff.
    #[must_use]
    pub fn to_json(&self) -> String {
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|row| {
                let mut entry = Map::new();
                entry.insert("name".to_owned(), Value::from(row.name.clone()));
                entry.insert("features".to_owned(), Value::from(row.features.clone()));
                entry.insert(
                    "measured_against".to_owned(),
                    Value::from(row.measured_against.clone()),
                );
                entry.insert("gated".to_owned(), Value::from(row.gated));
                entry.insert("text".to_owned(), Value::from(row.sizes.text));
                entry.insert("rodata".to_owned(), Value::from(row.sizes.rodata));
                entry.insert("data".to_owned(), Value::from(row.sizes.data));
                entry.insert("bss".to_owned(), Value::from(row.sizes.bss));
                entry.insert("flash".to_owned(), Value::from(row.sizes.flash));
                entry.insert("ram".to_owned(), Value::from(row.sizes.ram));
                // The deltas are what the issue asks the job to record and what a reader
                // wants; they are derivable from the baseline row, but a consumer that has
                // to re-derive them is a consumer that can derive them differently.
                if let Some(delta) = self.delta_of(&row.name) {
                    entry.insert("delta".to_owned(), delta_json(&delta));
                }
                if let Some(increment) = self.increment_of(&row.name) {
                    entry.insert("increment".to_owned(), delta_json(&increment));
                }
                Value::Object(entry)
            })
            .collect();

        let types: Vec<Value> = self
            .kernel_state
            .types
            .iter()
            .map(|(name, size)| {
                let mut entry = Map::new();
                entry.insert("name".to_owned(), Value::from(name.clone()));
                entry.insert("size".to_owned(), Value::from(*size));
                Value::Object(entry)
            })
            .collect();

        let mut kernel_state = Map::new();
        kernel_state.insert("total".to_owned(), Value::from(self.kernel_state.total));
        kernel_state.insert("types".to_owned(), Value::Array(types));

        let mut budgets = Map::new();
        budgets.insert(
            "incremental_code_flash".to_owned(),
            Value::from(INCREMENTAL_CODE_FLASH_BUDGET_BYTES),
        );
        budgets.insert(
            "engine_ram".to_owned(),
            Value::from(ENGINE_RAM_BUDGET_BYTES),
        );
        budgets.insert(
            "kernel_state".to_owned(),
            Value::from(KERNEL_STATE_BUDGET_BYTES),
        );

        let mut document = Map::new();
        document.insert("schema".to_owned(), Value::from(REPORT_SCHEMA));
        document.insert("target".to_owned(), Value::from(FIRMWARE_TARGET));
        document.insert("budgets".to_owned(), Value::Object(budgets));
        document.insert("kernel_state".to_owned(), Value::Object(kernel_state));
        document.insert("rows".to_owned(), Value::Array(rows));

        format!("{:#}\n", Value::Object(document))
    }

    /// Reads a report written by [`Self::to_json`].
    ///
    /// # Errors
    ///
    /// Returns [`SizeError`] if the document is not JSON, is not a size report, or carries
    /// a schema version this build does not know how to read. All three fail closed: a
    /// baseline that cannot be read is a baseline that is missing, not one that matched.
    pub fn from_json(json: &str) -> Result<Self, SizeError> {
        let document: Value = serde_json::from_str(json)
            .map_err(|err| SizeError::new(format!("could not parse the size report: {err}")))?;

        let schema = document
            .get("schema")
            .and_then(Value::as_u64)
            .ok_or_else(|| SizeError::new("the size report has no `schema` version"))?;
        if schema != REPORT_SCHEMA {
            return Err(SizeError::new(format!(
                "the size report is schema {schema}, but this build reads schema {REPORT_SCHEMA}"
            )));
        }

        // The budgets are stated for one target, so a report taken on another is not a
        // report this gate can hold to them.
        let target = document
            .get("target")
            .and_then(Value::as_str)
            .ok_or_else(|| SizeError::new("the size report names no `target`"))?;
        if target != FIRMWARE_TARGET {
            return Err(SizeError::new(format!(
                "the size report was taken on {target}, but the budgets in design document \u{a7}04 are stated for {FIRMWARE_TARGET}"
            )));
        }

        let rows = document
            .get("rows")
            .and_then(Value::as_array)
            .ok_or_else(|| SizeError::new("the size report has no `rows` array"))?
            .iter()
            .map(|row| {
                Ok(Row {
                    name: row
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| SizeError::new("a size report row has no `name`"))?
                        .to_owned(),
                    features: row
                        .get("features")
                        .and_then(Value::as_array)
                        .map(|features| {
                            features
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    measured_against: row
                        .get("measured_against")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            SizeError::new(
                                "a size report row does not say what it is measured against",
                            )
                        })?
                        .to_owned(),
                    gated: row
                        .get("gated")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| SizeError::new("a size report row has no `gated` flag"))?,
                    sizes: SectionSizes {
                        text: number(row, "text")?,
                        rodata: number(row, "rodata")?,
                        data: number(row, "data")?,
                        bss: number(row, "bss")?,
                        flash: number(row, "flash")?,
                        ram: number(row, "ram")?,
                    },
                })
            })
            .collect::<Result<Vec<Row>, SizeError>>()?;

        let kernel_state = document
            .get("kernel_state")
            .ok_or_else(|| SizeError::new("the size report has no `kernel_state`"))?;
        let kernel_state = KernelState {
            total: number(kernel_state, "total")?,
            types: kernel_state
                .get("types")
                .and_then(Value::as_array)
                .ok_or_else(|| SizeError::new("the size report's `kernel_state` has no `types`"))?
                .iter()
                .map(|entry| {
                    let name = entry
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| SizeError::new("a kernel state entry has no `name`"))?;
                    Ok((name.to_owned(), number(entry, "size")?))
                })
                .collect::<Result<Vec<(String, u64)>, SizeError>>()?,
        };

        Ok(Self { rows, kernel_state })
    }
}

/// A required byte count.
///
/// Missing and mistyped both fail rather than defaulting to zero. A report is only read
/// back to be gated or diffed, and in both of those a silent zero is the most convincing
/// possible way to say nothing is wrong: a truncated artifact would gate clean, and
/// `--json` would then re-emit the laundered numbers as a well-formed report that no
/// downstream reader could tell from a real measurement.
/// One set of section deltas, as JSON.
fn delta_json(sizes: &SectionSizes) -> Value {
    let mut entry = Map::new();
    entry.insert("text".to_owned(), Value::from(sizes.text));
    entry.insert("rodata".to_owned(), Value::from(sizes.rodata));
    entry.insert("data".to_owned(), Value::from(sizes.data));
    entry.insert("bss".to_owned(), Value::from(sizes.bss));
    entry.insert("flash".to_owned(), Value::from(sizes.flash));
    entry.insert("ram".to_owned(), Value::from(sizes.ram));
    Value::Object(entry)
}

fn number(value: &Value, field: &str) -> Result<u64, SizeError> {
    value.get(field).and_then(Value::as_u64).ok_or_else(|| {
        SizeError::new(format!(
            "the size report has no `{field}`, or it is not a byte count"
        ))
    })
}

/// What one row did between two reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDiff {
    /// The row's name.
    pub name: String,
    /// The base branch's sizes, absent when the row is new.
    pub before: Option<SectionSizes>,
    /// This branch's sizes, absent when the row was removed.
    pub after: Option<SectionSizes>,
}

impl RowDiff {
    /// The change in stored bytes, or `None` where one side is missing.
    #[must_use]
    pub fn flash_change(&self) -> Option<i128> {
        let before = self.before?;
        let after = self.after?;
        Some(i128::from(after.flash) - i128::from(before.flash))
    }

    /// How the change reads in the table: `+40`, `-8`, or `0`.
    #[must_use]
    pub fn render_flash_change(&self) -> String {
        self.flash_change().map_or_else(
            || "?".to_owned(),
            |change| {
                if change > 0 {
                    format!("+{change}")
                } else {
                    change.to_string()
                }
            },
        )
    }
}

/// Every row whose *cost* differs between `base` and `head`, plus the rows only one has.
///
/// Compared on deltas against each report's own baseline rather than on absolute sizes.
/// The two reports are built with the same toolchain in the same job today, so the two
/// agree — but a rustc bump or a change to the panic handler moves every absolute number
/// while changing nobody's incremental cost, and a diff that reported all of that as
/// "changed" would be a diff people stop reading.
#[must_use]
pub fn diff(base: &SizeReport, head: &SizeReport) -> Vec<RowDiff> {
    let mut diffs = Vec::new();

    for row in head.rows() {
        let after = head.delta_of(&row.name);
        let before = base.row(&row.name).and_then(|_| base.delta_of(&row.name));
        if before == after && before.is_some() {
            continue;
        }
        diffs.push(RowDiff {
            name: row.name.clone(),
            before,
            after,
        });
    }

    for row in base.rows() {
        if head.row(&row.name).is_none() {
            diffs.push(RowDiff {
                name: row.name.clone(),
                before: base.delta_of(&row.name),
                after: None,
            });
        }
    }

    diffs
}

/// How the kernel state registry changed between two reports.
///
/// Reported beside the row diff because kernel state is the one budget the section sizes
/// cannot see: it is asserted at compile time and carried in the registry, so a change to
/// it would otherwise be invisible in a diff of linked images.
#[must_use]
pub fn kernel_state_change(base: &SizeReport, head: &SizeReport) -> Option<String> {
    let (before, after) = (base.kernel_state(), head.kernel_state());
    if before == after {
        return None;
    }
    Some(format!(
        "kernel state {} B across {} type(s) -> {} B across {} type(s)",
        before.total,
        before.types.len(),
        after.total,
        after.types.len(),
    ))
}

/// The diff as a table, or a line saying there is nothing to show.
#[must_use]
pub fn render_diff(diffs: &[RowDiff]) -> String {
    if diffs.is_empty() {
        return "size against the base branch: no change\n".to_owned();
    }

    let width = diffs
        .iter()
        .map(|entry| entry.name.len())
        .max()
        .unwrap_or(0)
        .max("variant".len());

    let mut table = vec!["size against the base branch\n".to_owned()];
    for entry in diffs {
        let detail = match (entry.before, entry.after) {
            (Some(before), Some(after)) => format!(
                "flash {} -> {} ({}{}), ram {} -> {}",
                before.flash,
                after.flash,
                if after.flash >= before.flash {
                    "+"
                } else {
                    "-"
                },
                after.flash.abs_diff(before.flash),
                before.ram,
                after.ram,
            ),
            (None, Some(after)) => format!("new: flash {}, ram {}", after.flash, after.ram),
            (Some(before), None) => {
                format!("removed: was flash {}, ram {}", before.flash, before.ram)
            }
            (None, None) => "no measurement on either side".to_owned(),
        };
        table.push(format!("  {:<width$}  {detail}\n", entry.name));
    }
    table.concat()
}

/// The size gate could not run, so it does not know whether it passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeError {
    message: String,
}

impl SizeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SizeError {}

/// The executable `package` produced, from a `--message-format json` build stream.
///
/// # Errors
///
/// Returns [`SizeError`] if the stream names no executable for `package`, which is what a
/// `required-features` selection that did not match looks like: cargo succeeds and builds
/// nothing, and a gate that measured the previous run's file would report a size that no
/// longer exists.
pub fn executable_path(stream: &str, package: &str) -> Result<PathBuf, SizeError> {
    for line in stream.lines() {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        if message
            .get("target")
            .and_then(|target| target.get("name"))
            .and_then(Value::as_str)
            != Some(package)
        {
            continue;
        }
        if let Some(path) = message.get("executable").and_then(Value::as_str) {
            return Ok(PathBuf::from(path));
        }
    }

    Err(SizeError::new(format!(
        "the build produced no executable for `{package}`; the `{PROBE_FEATURE}` feature selects its binary, so a build without it compiles nothing to measure"
    )))
}

/// Rule: the metadata describes the workspace at `root` and not one above it.
///
/// `cargo metadata` resolves the nearest manifest at or above the working directory. The
/// base-branch worktree is checked out under `target/`, so a base commit that predates the
/// workspace manifest — or one where the checkout failed — leaves cargo walking up into
/// the *current* workspace and measuring it. The diff then reads "no change" for every
/// row, which is the most convincing possible way to report nothing at all.
///
/// # Errors
///
/// Returns [`SizeError`] if the metadata names a different workspace root, or none.
pub fn check_workspace_root(metadata: &str, root: &Path) -> Result<(), SizeError> {
    let document: Value = serde_json::from_str(metadata)
        .map_err(|err| SizeError::new(format!("could not parse cargo metadata: {err}")))?;
    let reported = document
        .get("workspace_root")
        .and_then(Value::as_str)
        .ok_or_else(|| SizeError::new("cargo metadata names no `workspace_root`"))?;

    // Canonicalised because the worktree path is built by joining and the metadata path
    // comes back from cargo; a `.` component or a symlinked `target` would otherwise read
    // as a different workspace.
    let same = match (std::fs::canonicalize(reported), std::fs::canonicalize(root)) {
        (Ok(left), Ok(right)) => left == right,
        _ => Path::new(reported) == root,
    };
    if same {
        Ok(())
    } else {
        Err(SizeError::new(format!(
            "cargo resolved the workspace at {reported} rather than {}, so the measurement would be of a different tree; the checkout probably has no Cargo.toml of its own",
            root.display()
        )))
    }
}

/// Links every image in the matrix for the workspace at `root` and measures it.
///
/// # Errors
///
/// Returns [`SizeError`] if the workspace cannot be resolved, if it has no size probe, or
/// if any image fails to build or to be read.
pub fn measure(root: &Path) -> Result<SizeReport, SizeError> {
    measure_into(root, &root.join(BUILD_DIR))
}

/// Measures the workspace at `root`, linking into `build_dir`.
///
/// Separated so that the base-branch measurement can link into a directory that outlives
/// its worktree: a build inside the worktree is deleted with it, so the base half of every
/// pull request would be a cold build for ever, invisible to any CI build cache.
///
/// # Errors
///
/// As [`measure`].
pub fn measure_into(root: &Path, build_dir: &Path) -> Result<SizeReport, SizeError> {
    let metadata = crate::run_cargo_metadata(root)
        .map_err(|err| SizeError::new(format!("could not resolve the workspace: {err}")))?;
    check_workspace_root(&metadata, root)?;
    let graph = PackageGraph::from_cargo_metadata(&metadata)
        .map_err(|err| SizeError::new(format!("could not parse cargo metadata: {err}")))?;

    let variants = matrix(&graph);
    if variants.is_empty() {
        return Err(SizeError::new(format!(
            "this workspace has no `{PROBE_PACKAGE}`, so there is no example firmware to link and measure"
        )));
    }

    let mut rows = Vec::with_capacity(variants.len());
    for variant in variants {
        let image = build_variant(root, build_dir, &variant)?;
        let bytes = std::fs::read(&image).map_err(|err| {
            SizeError::new(format!(
                "could not read the linked image at {}: {err}",
                image.display()
            ))
        })?;
        let sections = elf::sections(&bytes)
            .map_err(|err| SizeError::new(format!("could not read {}: {err}", image.display())))?;
        rows.push(Row {
            name: variant.name,
            features: variant.features,
            measured_against: variant.measured_against,
            sizes: SectionSizes::of(&sections),
            gated: variant.gated,
        });
    }

    Ok(SizeReport::new(rows, KernelState::measured()))
}

/// Links one image and returns the path to it.
fn build_variant(root: &Path, build_dir: &Path, variant: &Variant) -> Result<PathBuf, SizeError> {
    // `uninstrumented_cargo` because a size run under `cargo llvm-cov` would otherwise
    // inherit its `RUSTC_WRAPPER` and `RUSTFLAGS` and try to build instrumented firmware,
    // which has no `profiler_builtins` for this target — and, worse, can succeed from a
    // stale cache and measure the wrong bytes.
    let output = uninstrumented_cargo()
        .current_dir(root)
        .args([
            "build",
            "--locked",
            "--release",
            "--message-format",
            "json-render-diagnostics",
            "--target",
            FIRMWARE_TARGET,
            "--target-dir",
        ])
        .arg(variant_build_dir(build_dir, &variant.name))
        .args([
            "--package",
            PROBE_PACKAGE,
            "--no-default-features",
            "--features",
        ])
        .arg(variant.features.join(","))
        .output()
        .map_err(|err| {
            SizeError::new(format!(
                "could not run cargo build for `{}`: {err}",
                variant.name
            ))
        })?;

    if !output.status.success() {
        return Err(SizeError::new(format!(
            "linking `{}` failed ({}): {}",
            variant.name,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let stream = String::from_utf8_lossy(&output.stdout);
    executable_path(&stream, PROBE_PACKAGE)
}

/// Rule: the size probe is still the thing the size gate thinks it is.
///
/// Every number in the report is a delta between two builds of one crate, so the probe's
/// shape is part of the measurement. Four ways for it to stop measuring anything, all of
/// which leave a green pipeline behind:
///
/// * the crate disappears, and the gate has nothing to link;
/// * its binary loses `required-features`, at which point `cargo build --workspace` and
///   `cargo clippy --all-targets` start trying to link firmware for the host and the fix
///   somebody reaches for is to make the probe `std`;
/// * a layer stops being an optional dependency, so the baseline links it too and every
///   delta collapses to zero;
/// * the crate root stops being `#![no_std]`, and the deltas become measurements of the
///   standard library.
#[must_use]
pub fn check_size_probe(
    graph: &PackageGraph,
    manifest: Option<&str>,
    source: Option<&str>,
) -> Vec<Violation> {
    let Some(package) = graph.find(PROBE_PACKAGE) else {
        return vec![Violation::new(
            "size-probe",
            PROBE_PACKAGE,
            "the workspace has no size probe, so `cargo xtask size` has no example firmware to link; design document \u{a7}04 requires the budgets to be measured rather than claimed",
        )];
    };

    let mut violations = Vec::new();

    if !package.default_features.is_empty() {
        violations.push(Violation::new(
            "size-probe",
            PROBE_PACKAGE,
            format!(
                "default feature enables {}; the probe's default must be empty so that the baseline image links no layer at all",
                package.default_features.join(", ")
            ),
        ));
    }

    match package
        .bins
        .iter()
        .find(|bin| bin.required_features.iter().any(|f| f == PROBE_FEATURE))
    {
        Some(_) => {}
        None if package.bins.is_empty() => violations.push(Violation::new(
            "size-probe",
            PROBE_PACKAGE,
            "has no binary target, so there is nothing to link and measure",
        )),
        None => violations.push(Violation::new(
            "size-probe",
            PROBE_PACKAGE,
            format!(
                "its binary is not behind `required-features = [\"{PROBE_FEATURE}\"]`, so host builds and `cargo clippy --all-targets` will try to link firmware for the host"
            ),
        )),
    }

    for feature in [PROBE_FEATURE, ENGINE_FEATURE, FACADE_FEATURE] {
        if !package.features.iter().any(|declared| declared == feature) {
            violations.push(Violation::new(
                "size-probe",
                PROBE_PACKAGE,
                format!("does not declare the `{feature}` feature, which the size matrix selects"),
            ));
        }
    }
    violations.extend(check_probe_features(manifest));

    violations.extend(check_probe_manifest(manifest));
    violations.extend(check_probe_source(source));
    violations
}

/// What each probe feature must enable for its row to measure anything.
///
/// Checking that the feature *exists* is not enough: `engine = []` is a plausible thing to
/// write while debugging a link failure, it satisfies every other rule, and it collapses
/// every delta in the report to zero because the baseline and the engine then link exactly
/// the same image.
const REQUIRED_PROBE_FEATURES: &[(&str, &[&str])] = &[
    (ENGINE_FEATURE, &["dep:waymaker-core", "dep:waymaker-flash"]),
    (FACADE_FEATURE, &[ENGINE_FEATURE, "dep:waymaker-embassy"]),
];

/// Rule: each probe feature still enables the crates its row is supposed to measure.
fn check_probe_features(manifest: Option<&str>) -> Vec<Violation> {
    let Some(parsed) = manifest.and_then(|manifest| manifest.parse::<toml::Table>().ok()) else {
        // Already reported by `check_probe_manifest`.
        return Vec::new();
    };
    let features = parsed.get("features").and_then(toml::Value::as_table);

    let mut violations = Vec::new();
    for (feature, required) in REQUIRED_PROBE_FEATURES {
        let enabled: Vec<&str> = features
            .and_then(|table| table.get(*feature))
            .and_then(toml::Value::as_array)
            .map(|entries| entries.iter().filter_map(toml::Value::as_str).collect())
            .unwrap_or_default();
        for wanted in *required {
            if !enabled.contains(wanted) {
                violations.push(Violation::new(
                    "size-probe",
                    PROBE_PACKAGE,
                    format!(
                        "its `{feature}` feature does not enable `{wanted}`, so the `{feature}` row links the same image as the row below it and every delta reads zero"
                    ),
                ));
            }
        }
    }
    violations
}

/// Rule: every layer is an optional dependency of the probe.
fn check_probe_manifest(manifest: Option<&str>) -> Vec<Violation> {
    let Some(manifest) = manifest else {
        return vec![Violation::new(
            "size-probe",
            PROBE_PACKAGE,
            "the probe's manifest could not be read, so the rules about it did not run",
        )];
    };

    // `toml::Table` rather than `toml::Value`: a document is a table, and parsing it as a
    // value fails on a perfectly good manifest.
    let Ok(parsed) = manifest.parse::<toml::Table>() else {
        return vec![Violation::new(
            "size-probe",
            PROBE_PACKAGE,
            "the probe's manifest is not valid TOML",
        )];
    };

    let dependencies = parsed.get("dependencies").and_then(toml::Value::as_table);
    policy::LAYERS
        .iter()
        .filter_map(|spec| {
            let optional = dependencies
                .and_then(|table| table.get(spec.name))
                .and_then(|dep| dep.get("optional"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            (!optional).then(|| {
                Violation::new(
                    "size-probe",
                    PROBE_PACKAGE,
                    format!(
                        "does not depend on `{}` as `optional = true`; a layer the baseline image also links contributes nothing to the delta, and every budget then reads as zero",
                        spec.name
                    ),
                )
            })
        })
        .collect()
}

/// Attributes the probe's crate root must carry for its measurements to mean anything.
const PROBE_REQUIRED_ATTRIBUTES: &[&str] =
    &["#![no_std]", "#![no_main]", "#![forbid(unsafe_code)]"];

/// Rule: the probe is still bare-metal firmware.
fn check_probe_source(source: Option<&str>) -> Vec<Violation> {
    let Some(source) = source else {
        return vec![Violation::new(
            "size-probe",
            PROBE_PACKAGE,
            "the probe has no crate root, so the attribute rules did not run on it",
        )];
    };

    let attributes = crate::source::inner_attributes(source);

    PROBE_REQUIRED_ATTRIBUTES
        .iter()
        .filter(|required| !attributes.iter().any(|line| line == *required))
        .map(|required| {
            Violation::new(
                "size-probe",
                PROBE_PACKAGE,
                format!("src/main.rs is missing `{required}`"),
            )
        })
        .collect()
}

/// One source file of a firmware layer, for the reach rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerSource {
    /// The crate the file belongs to.
    pub crate_name: String,
    /// Its path, for a violation message.
    pub path: String,
    /// Its contents.
    pub contents: String,
}

/// A public function of a layer, which the probe must reach for its cost to be measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicFunction {
    /// The crate that declares it.
    pub crate_name: String,
    /// The file it is declared in.
    pub path: String,
    /// Its name.
    pub name: String,
}

/// Every public function the layers declare, outside their test modules.
///
/// Scanned rather than parsed, like every other rule here: the question is which names
/// exist, not what they mean. `#[cfg(test)]` modules are skipped by brace depth, because a
/// test helper is not code the firmware links.
#[must_use]
pub fn public_functions(sources: &[LayerSource]) -> Vec<PublicFunction> {
    let mut found = Vec::new();
    for source in sources {
        let mut depth: i32 = 0;
        let mut test_module: Option<i32> = None;
        let mut pending_test_attribute = false;

        for line in source.contents.lines() {
            let trimmed = line.trim();

            if !trimmed.starts_with("//") {
                if trimmed.contains("#[cfg(test)]") {
                    pending_test_attribute = true;
                }
                if pending_test_attribute && trimmed.contains('{') {
                    test_module = Some(depth);
                    pending_test_attribute = false;
                }
            }

            if test_module.is_none() && !trimmed.starts_with("//") {
                if let Some(name) = function_name(trimmed) {
                    found.push(PublicFunction {
                        crate_name: source.crate_name.clone(),
                        path: source.path.clone(),
                        name: name.to_owned(),
                    });
                }
            }

            depth += i32::try_from(trimmed.matches('{').count()).unwrap_or(0);
            depth -= i32::try_from(trimmed.matches('}').count()).unwrap_or(0);
            if test_module.is_some_and(|opened| depth <= opened) {
                test_module = None;
            }
        }
    }
    found
}

/// The name declared by a `pub fn` line, if the line declares one.
fn function_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("pub ")?;
    // `pub const fn`, `pub async fn`, `pub unsafe fn`, `pub extern "C" fn`, and any
    // combination: skip everything up to the `fn` keyword.
    let rest = rest.split_once("fn ")?.1;
    let name = rest
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .find(|token| !token.is_empty())?;
    (!name.is_empty()).then_some(name)
}

/// Rule: the probe reaches every public function the layers declare.
///
/// This is the rule that stops the whole gate becoming decorative. A delta charges only for
/// code the linker keeps, and with `lto = "fat"` and `--gc-sections` the linker keeps what
/// the probe reaches — so a layer can grow an arbitrary amount of code, and the 8 KiB gate
/// keeps reporting the same twenty-odd bytes of the probe's own arithmetic. Nothing else
/// notices: the row is not identical to its base, because the probe's constants already
/// make it bigger, so `notices` stays quiet and the positive-delta test still passes.
///
/// Enabling the optional dependency is not enough, and neither is naming the crate: only a
/// call retains a function. So the rule asks for the one thing that makes the number real —
/// that every public function appears in the probe — and names the ones that do not.
///
/// A function the probe genuinely should not charge for is a function that should not be
/// public, or a deliberate exception; either is a conversation in review, which is where a
/// decision about what the budget covers belongs.
#[must_use]
pub fn check_probe_reach(sources: &[LayerSource], probe: Option<&str>) -> Vec<Violation> {
    let functions = public_functions(sources);
    if functions.is_empty() {
        return Vec::new();
    }

    let Some(probe) = probe else {
        return vec![Violation::new(
            "size-probe-reach",
            PROBE_PACKAGE,
            "the probe has no crate root, so nothing can be said about what it reaches",
        )];
    };

    functions
        .into_iter()
        .filter(|function| !mentions(probe, &function.name))
        .map(|function| {
            Violation::new(
                "size-probe-reach",
                PROBE_PACKAGE,
                format!(
                    "does not call `{}`, declared in {}, so the linker discards it and no row charges for it; add a call in the probe or the size report understates {} for ever",
                    function.name, function.path, function.crate_name
                ),
            )
        })
        .collect()
}

/// Whether `source` calls `name`, ignoring what its comments happen to say.
///
/// Comments are stripped first, and the reason is a fail-open this rule walked straight
/// into: `waymaker-core` declares `TypeSize::of`, and the probe's prose contains the
/// English word "of" five times, so the rule reported the function as reached while the
/// linker discarded it. Prose is not a call. A `//` inside a string literal is stripped
/// too, which can only make the rule stricter — the safe direction for a rule whose whole
/// job is to notice something missing.
fn mentions(source: &str, name: &str) -> bool {
    let code: String = source
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<&str>>()
        .join("\n");
    let mut rest = code.as_str();
    while let Some(at) = rest.find(name) {
        let before = rest.get(..at).and_then(|text| text.chars().next_back());
        let after = rest
            .get(at.saturating_add(name.len())..)
            .and_then(|text| text.chars().next());
        let boundary = |character: Option<char>| {
            character.is_none_or(|character| !character.is_alphanumeric() && character != '_')
        };
        if boundary(before) && boundary(after) {
            return true;
        }
        let Some(next) = rest.get(at.saturating_add(1)..) else {
            return false;
        };
        rest = next;
    }
    false
}

/// The base branch a pull request is being measured against, as CI reports it.
///
/// Read from the environment rather than passed on the command line so that the workflow
/// runs the same fixed string a developer runs, which is what lets the pipeline table
/// compare the two byte for byte.
#[must_use]
pub fn base_ref_from_environment() -> Option<String> {
    std::env::var("GITHUB_BASE_REF")
        .ok()
        .map(|reference| reference.trim().to_owned())
        .filter(|reference| !reference.is_empty())
}

/// Measures the workspace as it stands on `reference`.
///
/// The base branch is checked out into a detached worktree and measured with *this*
/// build of the gate, not with whatever the base branch's own `xtask` would do. That is
/// deliberate: it is how the very pull request that introduces the gate can still produce
/// a diff, and how a later change to the accounting rules compares like with like.
///
/// # Errors
///
/// Returns [`SizeError`] if the reference cannot be resolved, the worktree cannot be
/// created, or the base branch cannot be measured — most often because it predates the
/// size probe. Every one of those is reported by the caller as "no baseline" rather than
/// as a failure: a missing comparison is not a budget breach.
pub fn measure_baseline(root: &Path, reference: &str) -> Result<SizeReport, SizeError> {
    let commit = resolve_ref(root, reference)?;
    sweep_leaked_worktrees(root);
    let worktree = baseline_worktree(root);
    remove_worktree(root, &worktree);

    let status = git(root)
        .args(["worktree", "add", "--detach", "--force"])
        .arg(&worktree)
        .arg(&commit)
        .output()
        .map_err(|err| SizeError::new(format!("could not run git worktree add: {err}")))?;
    if !status.status.success() {
        return Err(SizeError::new(format!(
            "could not check out `{reference}` ({commit}) to measure it: {}",
            String::from_utf8_lossy(&status.stderr).trim()
        )));
    }

    // Linked into a directory beside the head build rather than inside the worktree, so
    // that the base half survives the worktree's removal and a CI build cache can see it.
    // Otherwise every pull request pays for a cold build of the base branch, for ever.
    let measured = measure_into(&worktree, &root.join(BUILD_DIR).with_extension("base"));
    remove_worktree(root, &worktree);
    measured
}

/// How long a base worktree must have sat untouched before a sweep will remove it.
///
/// A size run is minutes of work, so a directory hours old belongs to a process that is no
/// longer running. Age rather than liveness because there is no portable way to ask whether
/// a process id is alive, and the failure mode of guessing wrong is the worse one: removing
/// a worktree a concurrent run is still measuring makes *that* run report "not compared",
/// which is a silently lost comparison rather than a visible error.
const LEAKED_WORKTREE_AGE: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Removes base worktrees left behind by runs that were killed before they could clean up.
///
/// `git worktree prune` cannot help with these: it drops registrations whose directory is
/// gone, and a killed run leaves the directory in place. Each leak is a full checkout, and
/// a cancelled pull-request run is the normal case rather than the exception —
/// `cancel-in-progress` sees to that — so without this they accumulate one per cancelled
/// run on any runner whose disk outlives the job.
fn sweep_leaked_worktrees(root: &Path) {
    let base = root.join(BASELINE_WORKTREE_PATH);
    let (Some(parent), Some(prefix)) = (
        base.parent().map(Path::to_path_buf),
        base.file_name().and_then(|name| name.to_str()),
    ) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return;
    };

    let mut swept = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&format!("{prefix}-")));
        if !named {
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok());
        if !is_leaked(age) {
            continue;
        }
        let _ = std::fs::remove_dir_all(&path);
        swept = true;
    }

    if swept {
        let _ = git(root).args(["worktree", "prune"]).output();
    }
}

/// Whether a base worktree of this age is one a killed run left behind.
///
/// An unreadable timestamp is treated as "not leaked": a directory whose age cannot be
/// established might be in use, and leaving a stale one on disk costs space, while removing
/// a live one costs a measurement.
const fn is_leaked(age: Option<std::time::Duration>) -> bool {
    match age {
        Some(age) => age.as_secs() >= LEAKED_WORKTREE_AGE.as_secs(),
        None => false,
    }
}

/// The commit `reference` names, trying the remote-tracking form first.
///
/// A workflow reports the base branch as a bare name such as `main`, and a CI checkout
/// usually has it only as `origin/main`.
fn resolve_ref(root: &Path, reference: &str) -> Result<String, SizeError> {
    for candidate in [format!("origin/{reference}"), reference.to_owned()] {
        let output = git(root)
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{candidate}^{{commit}}"))
            .output();
        if let Ok(output) = output
            && output.status.success()
        {
            let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !commit.is_empty() {
                return Ok(commit);
            }
        }
    }
    Err(SizeError::new(format!(
        "`{reference}` does not name a commit in this checkout; a shallow clone has no base branch to compare against, so fetch it with `fetch-depth: 0`"
    )))
}

/// Removes the worktree this process created, and its registration.
fn remove_worktree(root: &Path, worktree: &Path) {
    // Best effort on both halves: `git worktree remove` fails when there is nothing to
    // remove, and the directory can outlive its registration if a previous run was killed.
    let _ = git(root)
        .args(["worktree", "remove", "--force"])
        .arg(worktree)
        .output();
    let _ = std::fs::remove_dir_all(worktree);
    let _ = git(root).args(["worktree", "prune"]).output();
}

fn git(root: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command.current_dir(root);
    command
}

/// Writes `report` to `path`, creating the directory it lives in.
///
/// # Errors
///
/// Returns [`SizeError`] if the file cannot be written.
pub fn write_report(path: &Path, report: &SizeReport) -> Result<(), SizeError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            SizeError::new(format!(
                "could not create {} for the size report: {err}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(path, report.to_json()).map_err(|err| {
        SizeError::new(format!(
            "could not write the size report to {}: {err}",
            path.display()
        ))
    })
}

/// Reads a report written by [`write_report`].
///
/// # Errors
///
/// Returns [`SizeError`] if the file cannot be read or is not a size report.
pub fn read_report(path: &Path) -> Result<SizeReport, SizeError> {
    let json = std::fs::read_to_string(path).map_err(|err| {
        SizeError::new(format!(
            "could not read the size report at {}: {err}",
            path.display()
        ))
    })?;
    SizeReport::from_json(&json)
}

/// Fixtures describing a size probe that does not exist on disk.
#[cfg(test)]
pub mod tests_support {
    use super::{PROBE_FEATURE, PROBE_REQUIRED_ATTRIBUTES, REQUIRED_PROBE_FEATURES};
    use crate::policy::LAYERS;

    /// A probe manifest that satisfies every rule in [`super::check_size_probe`].
    ///
    /// Rendered from the same tables the rules read, so that a rule tightened without the
    /// fixture being updated fails loudly here rather than leaving the fixture describing
    /// a probe the gate would now reject.
    #[must_use]
    pub fn clean_probe_manifest() -> String {
        let dependencies = LAYERS
            .iter()
            .map(|spec| {
                format!(
                    "{} = {{ path = \"../{}\", optional = true }}\n",
                    spec.name, spec.name
                )
            })
            .collect::<Vec<String>>()
            .concat();
        let features = REQUIRED_PROBE_FEATURES
            .iter()
            .map(|(feature, enables)| {
                let enabled = enables
                    .iter()
                    .map(|name| format!("\"{name}\""))
                    .collect::<Vec<String>>()
                    .join(", ");
                format!("{feature} = [{enabled}]\n")
            })
            .collect::<Vec<String>>()
            .concat();
        format!(
            "[package]\nname = \"waymaker-size-probe\"\n\n[dependencies]\n{dependencies}\n[features]\ndefault = []\n{PROBE_FEATURE} = []\n{features}"
        )
    }

    /// A probe crate root that satisfies every rule in [`super::check_size_probe`].
    #[must_use]
    pub fn clean_probe_source() -> String {
        PROBE_REQUIRED_ATTRIBUTES
            .iter()
            .map(|attribute| format!("{attribute}\n"))
            .collect::<Vec<String>>()
            .concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::tests_support::{Class, ElfBuilder, SectionSpec};
    use crate::elf::{SHF_ALLOC, SHF_EXECINSTR, SHF_WRITE};
    use crate::graph::{Package, PackageGraph};

    fn workspace(core_features: &[&str], embassy_features: &[&str]) -> PackageGraph {
        PackageGraph::new(vec![
            Package::new("waymaker-core").with_features(core_features),
            Package::new("waymaker-flash"),
            Package::new("waymaker-embassy").with_features(embassy_features),
            Package::new(PROBE_PACKAGE).with_features(&["probe", "engine", "facade"]),
        ])
    }

    #[test]
    fn the_matrix_always_starts_with_a_baseline_and_the_default_engine() {
        let variants = matrix(&workspace(&[], &[]));
        let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["baseline", "default", "facade"]);

        let baseline = variants.first().expect("a baseline row");
        assert_eq!(baseline.features, [PROBE_FEATURE]);
        assert!(
            !baseline.gated,
            "the baseline is what other rows are gated against"
        );

        let default = variants.get(1).expect("a default row");
        assert_eq!(default.features, [PROBE_FEATURE, ENGINE_FEATURE]);
        assert!(default.gated);
    }

    #[test]
    fn every_declared_feature_of_every_layer_becomes_its_own_row() {
        let variants = matrix(&workspace(&["serde", "postcard", "crc-soft"], &["defmt"]));
        let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "baseline",
                "default",
                "facade",
                "waymaker-core/crc-soft",
                "waymaker-core/postcard",
                "waymaker-core/serde",
                "waymaker-embassy/defmt",
            ]
        );
    }

    #[test]
    fn a_feature_of_the_facade_is_measured_with_the_facade_linked() {
        let variants = matrix(&workspace(&["serde"], &["defmt"]));
        let core_row = find(&variants, "waymaker-core/serde");
        assert_eq!(
            core_row.features,
            [PROBE_FEATURE, ENGINE_FEATURE, "waymaker-core/serde"]
        );
        let facade_row = find(&variants, "waymaker-embassy/defmt");
        assert_eq!(
            facade_row.features,
            [PROBE_FEATURE, FACADE_FEATURE, "waymaker-embassy/defmt"]
        );
    }

    #[test]
    fn the_default_feature_is_not_a_row_of_its_own() {
        let variants = matrix(&workspace(&["default", "serde"], &[]));
        assert!(
            !variants.iter().any(|v| v.name.contains("/default")),
            "{:?}",
            variants.iter().map(|v| &v.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_workspace_without_a_probe_yields_no_matrix() {
        let graph = PackageGraph::new(vec![Package::new("waymaker-core")]);
        assert!(matrix(&graph).is_empty());
    }

    #[test]
    fn section_sizes_split_flash_from_ram_by_flags_and_type() {
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(
                ".text",
                100,
                SHF_ALLOC | SHF_EXECINSTR,
            ))
            .with(SectionSpec::progbits(".rodata", 20, SHF_ALLOC))
            .with(SectionSpec::progbits(".data", 8, SHF_ALLOC | SHF_WRITE))
            .with(SectionSpec::nobits(".bss", 256, SHF_ALLOC | SHF_WRITE))
            .with(SectionSpec::progbits(".ARM.exidx", 16, SHF_ALLOC))
            .with(SectionSpec::progbits(".comment", 999, 0))
            .build();
        let sizes = SectionSizes::of(&crate::elf::sections(&image).expect("readable"));

        assert_eq!(sizes.text, 100);
        assert_eq!(sizes.rodata, 20);
        assert_eq!(sizes.data, 8);
        assert_eq!(sizes.bss, 256);
        // `.comment` is not allocated and pays for nothing.
        assert_eq!(sizes.flash, 100 + 20 + 8 + 16);
        assert_eq!(sizes.ram, 8 + 256);
    }

    #[test]
    fn linker_split_sections_are_folded_into_the_section_they_belong_to() {
        let image = ElfBuilder::new(Class::Elf32)
            .with(SectionSpec::progbits(
                ".text",
                10,
                SHF_ALLOC | SHF_EXECINSTR,
            ))
            .with(SectionSpec::progbits(
                ".text.unlikely",
                5,
                SHF_ALLOC | SHF_EXECINSTR,
            ))
            .with(SectionSpec::nobits(".bss.probe", 32, SHF_ALLOC | SHF_WRITE))
            .build();
        let sizes = SectionSizes::of(&crate::elf::sections(&image).expect("readable"));
        assert_eq!(sizes.text, 15);
        assert_eq!(sizes.bss, 32);
    }

    /// A baseline image with enough in it to look like something a linker produced.
    fn baseline_sizes() -> SectionSizes {
        SectionSizes {
            text: 20,
            rodata: 4,
            flash: 40,
            ..SectionSizes::default()
        }
    }

    fn baseline_row() -> Row {
        Row::new(
            BASELINE_ROW,
            &[PROBE_FEATURE],
            BASELINE_ROW,
            baseline_sizes(),
            false,
        )
    }

    fn default_row(flash_over_baseline: u64, ram_over_baseline: u64) -> Row {
        let base = baseline_sizes();
        Row::new(
            DEFAULT_ROW,
            &[PROBE_FEATURE, ENGINE_FEATURE],
            BASELINE_ROW,
            SectionSizes {
                text: base.text + flash_over_baseline,
                flash: base.flash + flash_over_baseline,
                bss: ram_over_baseline,
                ram: ram_over_baseline,
                ..base
            },
            true,
        )
    }

    fn report(default_flash: u64, default_ram: u64) -> SizeReport {
        SizeReport::new(
            vec![baseline_row(), default_row(default_flash, default_ram)],
            KernelState::measured(),
        )
    }

    fn rendered(shortfalls: &[BudgetShortfall]) -> String {
        shortfalls
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_report_within_every_budget_has_no_shortfalls() {
        assert!(report(1_024, 64).shortfalls().is_empty());
    }

    #[test]
    fn exceeding_the_code_flash_budget_names_the_offending_number() {
        let over = INCREMENTAL_CODE_FLASH_BUDGET_BYTES + 1;
        let message = rendered(&report(over, 0).shortfalls());
        assert!(message.contains(&over.to_string()), "{message}");
        assert!(
            message.contains(&INCREMENTAL_CODE_FLASH_BUDGET_BYTES.to_string()),
            "{message}"
        );
        assert!(message.contains(DEFAULT_ROW), "{message}");
        assert!(message.contains("over by 1 B"), "{message}");
    }

    #[test]
    fn exceeding_the_engine_statics_budget_names_the_offending_number() {
        let over = ENGINE_RAM_BUDGET_BYTES + 1;
        let message = rendered(&report(0, over).shortfalls());
        assert!(message.contains(&over.to_string()), "{message}");
        assert!(message.contains("engine statics"), "{message}");
    }

    #[test]
    fn the_report_does_not_claim_to_have_measured_stack_usage() {
        // Section sizes cannot see the stack, and a report that said "runtime RAM: ok"
        // would be claiming a budget it did not evaluate.
        let table = report(0, 0).render();
        assert!(table.contains("runtime RAM: statics only"), "{table}");
        assert!(table.contains("engine statics"), "{table}");
    }

    #[test]
    fn a_report_with_no_rows_at_all_fails_rather_than_passing_empty() {
        let empty = SizeReport::new(Vec::new(), KernelState::measured());
        let message = rendered(&empty.shortfalls());
        assert!(message.contains("no rows"), "{message}");
        assert!(empty.shortfall_report().is_some());
    }

    #[test]
    fn a_baseline_with_nothing_in_flash_is_not_a_measurement() {
        // An image with no stored bytes is one whose section headers were stripped or
        // whose file was never linked, and every delta against it reads zero.
        let report = SizeReport::new(
            vec![Row::new(
                BASELINE_ROW,
                &[PROBE_FEATURE],
                BASELINE_ROW,
                SectionSizes::default(),
                false,
            )],
            KernelState::measured(),
        );
        let message = rendered(&report.shortfalls());
        assert!(message.contains("no bytes in flash"), "{message}");
    }

    #[test]
    fn an_unmeasurable_report_does_not_render_as_a_zero_byte_budget() {
        let message = BudgetShortfall::Unmeasurable {
            detail: "the report has no rows".to_owned(),
        }
        .to_string();
        assert!(message.contains("nothing was measured"), "{message}");
        assert!(!message.contains("0 B budget"), "{message}");
    }

    #[test]
    fn a_budget_knows_its_own_limit_and_name() {
        assert_eq!(
            Budget::IncrementalCodeFlash.limit(),
            INCREMENTAL_CODE_FLASH_BUDGET_BYTES
        );
        assert_eq!(Budget::EngineStatics.limit(), ENGINE_RAM_BUDGET_BYTES);
        assert_eq!(Budget::KernelState.limit(), KERNEL_STATE_BUDGET_BYTES);
        assert_eq!(Budget::KernelState.name(), "kernel state");
    }

    #[test]
    fn the_gate_measures_a_delta_rather_than_an_absolute_size() {
        // A baseline that is itself large must not be charged to the engine.
        let big_baseline = Row::new(
            BASELINE_ROW,
            &[PROBE_FEATURE],
            BASELINE_ROW,
            SectionSizes {
                text: 4_096,
                flash: 4_096,
                ..SectionSizes::default()
            },
            false,
        );
        let report = SizeReport::new(
            vec![big_baseline, default_row(64, 0)],
            KernelState::measured(),
        );
        let delta = report.delta_of(DEFAULT_ROW).expect("a default row");
        assert_eq!(delta.flash, 0, "a smaller image is not a negative cost");
        assert!(report.shortfalls().is_empty());
    }

    /// A feature row costing `flash_over_default` more than the `default` row it sits on.
    fn feature_row(name: &str, default: &Row, flash_over_default: u64) -> Row {
        Row::new(
            name,
            &[PROBE_FEATURE, ENGINE_FEATURE, name],
            DEFAULT_ROW,
            SectionSizes {
                flash: default.sizes.flash + flash_over_default,
                text: default.sizes.text + flash_over_default,
                ..default.sizes
            },
            false,
        )
    }

    #[test]
    fn an_ungated_row_is_reported_but_not_failed() {
        let default = default_row(20, 0);
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default.clone(),
                feature_row(
                    "waymaker-core/serde",
                    &default,
                    INCREMENTAL_CODE_FLASH_BUDGET_BYTES + 1,
                ),
            ],
            KernelState::measured(),
        );
        assert!(report.shortfalls().is_empty());
        assert!(report.render().contains("waymaker-core/serde"));
    }

    #[test]
    fn a_feature_row_reports_its_cost_over_the_engine_rather_than_over_the_baseline() {
        let default = default_row(100, 0);
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default.clone(),
                feature_row("waymaker-core/serde", &default, 32),
            ],
            KernelState::measured(),
        );
        let increment = report
            .increment_of("waymaker-core/serde")
            .expect("a feature row is an increment on the engine");
        assert_eq!(increment.flash, 32);
        assert!(report.render().contains("+32 flash"), "{}", report.render());
    }

    #[test]
    fn an_engine_row_identical_to_the_baseline_is_named_too() {
        // The same failure one level down: the layers linked but contributed nothing,
        // which is what a dead-stripped engine looks like from here.
        let report = SizeReport::new(
            vec![baseline_row(), default_row(0, 0)],
            KernelState::measured(),
        );
        let notices = report.notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices
                .first()
                .is_some_and(|notice| notice.contains(DEFAULT_ROW)),
            "{notices:?}"
        );
    }

    #[test]
    fn a_feature_row_identical_to_its_base_is_named_as_measuring_nothing() {
        // This is the failure that makes an automatically derived matrix worthless: the
        // row appears, and reads zero because the probe never calls the feature.
        let default = default_row(20, 0);
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default.clone(),
                feature_row("waymaker-core/serde", &default, 0),
            ],
            KernelState::measured(),
        );
        let notices = report.notices();
        assert_eq!(notices.len(), 1, "{notices:?}");
        let notice = notices.first().map(String::as_str).unwrap_or_default();
        assert!(notice.contains("waymaker-core/serde"), "{notice}");
        assert!(notice.contains(PROBE_PACKAGE), "{notice}");
        assert!(report.render().contains("notice:"));
    }

    #[test]
    fn a_feature_row_that_cost_something_is_not_a_notice() {
        let default = default_row(20, 0);
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default.clone(),
                feature_row("waymaker-core/serde", &default, 8),
            ],
            KernelState::measured(),
        );
        assert!(report.notices().is_empty(), "{:?}", report.notices());
    }

    #[test]
    fn a_report_with_no_baseline_row_fails_rather_than_reporting_zero() {
        let report = SizeReport::new(vec![default_row(0, 0)], KernelState::measured());
        let message = rendered(&report.shortfalls());
        assert!(message.contains("baseline"), "{message}");
    }

    #[test]
    fn the_kernel_state_budget_is_gated_too() {
        let report = SizeReport::new(
            vec![baseline_row()],
            KernelState {
                total: KERNEL_STATE_BUDGET_BYTES + 7,
                types: vec![("Cursor".to_owned(), KERNEL_STATE_BUDGET_BYTES + 7)],
            },
        );
        let message = rendered(&report.shortfalls());
        assert!(
            message.contains(&(KERNEL_STATE_BUDGET_BYTES + 7).to_string()),
            "{message}"
        );
    }

    #[test]
    fn the_report_renders_every_row_with_its_per_section_deltas() {
        let table = report(512, 32).render();
        assert!(table.contains(BASELINE_ROW), "{table}");
        assert!(table.contains(DEFAULT_ROW), "{table}");
        assert!(table.contains("512"), "{table}");
        // The issue asks the job to record `.text` / `.rodata` / `.bss` deltas by name.
        for column in [
            "\u{394}.text",
            "\u{394}.rodata",
            "\u{394}.data",
            "\u{394}.bss",
        ] {
            assert!(table.contains(column), "{column} is missing from\n{table}");
        }
    }

    #[test]
    fn the_json_report_carries_the_deltas_as_well_as_the_absolute_sizes() {
        let json = report(512, 32).to_json();
        let document: Value = serde_json::from_str(&json).expect("its own JSON parses");
        let rows = document
            .get("rows")
            .and_then(Value::as_array)
            .expect("rows");
        let default = rows
            .iter()
            .find(|row| row.get("name").and_then(Value::as_str) == Some(DEFAULT_ROW))
            .expect("a default row");
        assert_eq!(
            default
                .get("delta")
                .and_then(|d| d.get("flash"))
                .and_then(Value::as_u64),
            Some(512)
        );
        assert_eq!(
            default
                .get("delta")
                .and_then(|d| d.get("bss"))
                .and_then(Value::as_u64),
            Some(32)
        );
        assert_eq!(
            default.get("flash").and_then(Value::as_u64),
            Some(baseline_sizes().flash + 512),
            "the absolute size is still there"
        );
    }

    #[test]
    fn a_report_survives_a_json_round_trip() {
        let original = report(700, 40);
        let restored =
            SizeReport::from_json(&original.to_json()).expect("its own JSON is readable");
        assert_eq!(restored, original);
    }

    #[test]
    fn json_that_is_not_a_size_report_is_rejected() {
        assert!(SizeReport::from_json("{}").is_err());
        assert!(SizeReport::from_json("not json").is_err());
    }

    #[test]
    fn a_row_missing_a_size_is_rejected_rather_than_read_as_zero() {
        // `--report` gates a document this process did not produce: a truncated CI
        // artifact must fail the gate, not sail through it with every number defaulted.
        let json = report(512, 32).to_json();
        let mut document: Value = serde_json::from_str(&json).expect("its own JSON parses");
        if let Some(row) = document
            .get_mut("rows")
            .and_then(Value::as_array_mut)
            .and_then(|rows| rows.first_mut())
            .and_then(Value::as_object_mut)
        {
            row.remove("flash");
        }
        let error = SizeReport::from_json(&document.to_string())
            .expect_err("a row with no flash figure has not been measured");
        assert!(error.to_string().contains("flash"), "{error}");
    }

    #[test]
    fn a_row_whose_gated_flag_is_missing_is_rejected() {
        let json = report(0, 0).to_json();
        let mut document: Value = serde_json::from_str(&json).expect("parses");
        if let Some(row) = document
            .get_mut("rows")
            .and_then(Value::as_array_mut)
            .and_then(|rows| rows.get_mut(1))
            .and_then(Value::as_object_mut)
        {
            row.remove("gated");
        }
        assert!(SizeReport::from_json(&document.to_string()).is_err());
    }

    #[test]
    fn a_report_taken_on_another_target_is_rejected() {
        let json = report(0, 0)
            .to_json()
            .replace(FIRMWARE_TARGET, "x86_64-unknown-linux-gnu");
        let error =
            SizeReport::from_json(&json).expect_err("the budgets are stated for one target");
        assert!(error.to_string().contains(FIRMWARE_TARGET), "{error}");
    }

    #[test]
    fn a_diff_names_what_grew_what_shrank_and_what_is_new() {
        let base = report(500, 16);
        let default = default_row(700, 16);
        let head = SizeReport::new(
            vec![
                baseline_row(),
                default.clone(),
                feature_row("waymaker-core/serde", &default, 900),
            ],
            KernelState::measured(),
        );

        let table = render_diff(&diff(&base, &head));
        assert!(table.contains(DEFAULT_ROW), "{table}");
        assert!(table.contains("+200"), "{table}");
        assert!(table.contains("waymaker-core/serde"), "{table}");
        assert!(table.contains("new"), "{table}");
    }

    #[test]
    fn a_diff_ignores_a_baseline_that_moved_without_changing_any_cost() {
        // A rustc bump or a change to the panic handler moves every absolute number while
        // nobody's incremental cost changes. A diff that shouted about that is a diff
        // people stop reading.
        let base = report(500, 16);
        let bigger_baseline = Row::new(
            BASELINE_ROW,
            &[PROBE_FEATURE],
            BASELINE_ROW,
            SectionSizes {
                flash: baseline_sizes().flash + 1_000,
                text: baseline_sizes().text + 1_000,
                ..baseline_sizes()
            },
            false,
        );
        let shifted_default = Row::new(
            DEFAULT_ROW,
            &[PROBE_FEATURE, ENGINE_FEATURE],
            BASELINE_ROW,
            SectionSizes {
                flash: bigger_baseline.sizes.flash + 500,
                text: bigger_baseline.sizes.text + 500,
                bss: 16,
                ram: 16,
                ..bigger_baseline.sizes
            },
            true,
        );
        let head = SizeReport::new(
            vec![bigger_baseline, shifted_default],
            KernelState::measured(),
        );
        assert!(diff(&base, &head).is_empty(), "{:?}", diff(&base, &head));
    }

    #[test]
    fn a_kernel_state_change_is_reported_beside_the_rows() {
        let base = report(0, 0);
        let head = SizeReport::new(
            vec![baseline_row(), default_row(0, 0)],
            KernelState {
                total: 24,
                types: vec![("Cursor".to_owned(), 24)],
            },
        );
        let change = kernel_state_change(&base, &head).expect("the registry changed");
        assert!(change.contains("24"), "{change}");
        assert!(kernel_state_change(&base, &base).is_none());
    }

    #[test]
    fn a_diff_of_a_report_against_itself_says_nothing_changed() {
        let rendered = render_diff(&diff(&report(500, 16), &report(500, 16)));
        assert!(rendered.contains("no change"), "{rendered}");
    }

    #[test]
    fn a_row_that_disappeared_is_reported_as_removed() {
        let base = report(500, 16);
        let head = SizeReport::new(vec![baseline_row()], KernelState::measured());
        let table = render_diff(&diff(&base, &head));
        assert!(table.contains("removed"), "{table}");
    }

    #[test]
    fn the_artifact_path_of_a_build_comes_from_the_cargo_message_stream() {
        let stream = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"waymaker-core","kind":["lib"]},"executable":null}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"waymaker-size-probe","kind":["bin"]},"executable":"/w/target/probe"}"#,
            "\n",
            r#"{"reason":"build-finished","success":true}"#,
            "\n",
        );
        assert_eq!(
            executable_path(stream, PROBE_PACKAGE).expect("the stream names one executable"),
            std::path::PathBuf::from("/w/target/probe")
        );
    }

    #[test]
    fn a_build_that_produced_no_executable_is_an_error_rather_than_a_zero() {
        let stream = r#"{"reason":"build-finished","success":true}"#;
        let error = executable_path(stream, PROBE_PACKAGE).expect_err("must fail closed");
        assert!(error.to_string().contains("executable"), "{error}");
    }

    /// The budget as `waymaker-core` declares it, widened for comparison.
    fn declared(bytes: usize) -> u64 {
        u64::try_from(bytes).expect("a budget in bytes fits in a u64")
    }

    #[test]
    fn the_budgets_are_the_ones_waymaker_core_declares() {
        // The gate must not carry its own copy of a number the kernel already states.
        assert_eq!(
            INCREMENTAL_CODE_FLASH_BUDGET_BYTES,
            declared(waymaker_core::budget::INCREMENTAL_CODE_FLASH_BYTES)
        );
        assert_eq!(
            ENGINE_RAM_BUDGET_BYTES,
            declared(waymaker_core::budget::ENGINE_RAM_BYTES)
        );
        assert_eq!(
            KERNEL_STATE_BUDGET_BYTES,
            declared(waymaker_core::budget::KERNEL_STATE_BYTES)
        );
    }

    #[test]
    fn metadata_from_the_workspace_being_measured_is_accepted() {
        let root = std::env::current_dir().expect("a working directory");
        let metadata = format!(r#"{{"workspace_root":{:?}}}"#, root.display().to_string());
        check_workspace_root(&metadata, &root).expect("the current workspace is itself");
    }

    #[test]
    fn metadata_from_a_workspace_above_the_one_asked_for_is_rejected() {
        let root = std::env::current_dir().expect("a working directory");
        let nested = root.join("target").join("waymaker-size-base");
        let metadata = format!(r#"{{"workspace_root":{:?}}}"#, root.display().to_string());
        let error = check_workspace_root(&metadata, &nested)
            .expect_err("a parent workspace is not the one being measured");
        assert!(error.to_string().contains("rather than"), "{error}");
    }

    #[test]
    fn metadata_with_no_workspace_root_is_rejected() {
        assert!(check_workspace_root("{}", Path::new("/w")).is_err());
        assert!(check_workspace_root("not json", Path::new("/w")).is_err());
    }

    // --- the probe rule ------------------------------------------------------------
    //
    // `check_size_probe` is what stops the whole gate from being silently disarmed, so
    // every branch of it is a test. Each of these describes a probe that does not exist.

    fn probe_graph() -> PackageGraph {
        PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash"),
            Package::new("waymaker-embassy"),
            Package::new(PROBE_PACKAGE)
                .with_features(&[ENGINE_FEATURE, FACADE_FEATURE, PROBE_FEATURE])
                .with_bin(PROBE_PACKAGE, &[PROBE_FEATURE]),
        ])
    }

    fn probe_violations(
        graph: &PackageGraph,
        manifest: Option<&str>,
        source: Option<&str>,
    ) -> String {
        check_size_probe(graph, manifest, source)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_well_formed_probe_passes_every_rule() {
        let violations = check_size_probe(
            &probe_graph(),
            Some(&tests_support::clean_probe_manifest()),
            Some(&tests_support::clean_probe_source()),
        );
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_workspace_with_no_probe_at_all_is_reported() {
        let graph = PackageGraph::new(vec![Package::new("waymaker-core")]);
        let message = probe_violations(&graph, None, None);
        assert!(message.contains("no size probe"), "{message}");
    }

    #[test]
    fn a_probe_with_a_non_empty_default_feature_is_reported() {
        let graph = PackageGraph::new(vec![
            Package::new(PROBE_PACKAGE)
                .with_features(&[ENGINE_FEATURE, FACADE_FEATURE, PROBE_FEATURE])
                .with_bin(PROBE_PACKAGE, &[PROBE_FEATURE])
                .with_default_features(&[ENGINE_FEATURE]),
        ]);
        let message = probe_violations(
            &graph,
            Some(&tests_support::clean_probe_manifest()),
            Some(&tests_support::clean_probe_source()),
        );
        assert!(message.contains("default must be empty"), "{message}");
    }

    #[test]
    fn a_probe_binary_without_required_features_is_reported() {
        // Without them, `cargo build --workspace` and `cargo clippy --all-targets` start
        // trying to link `#![no_main]` firmware for the host.
        let graph = PackageGraph::new(vec![
            Package::new(PROBE_PACKAGE)
                .with_features(&[ENGINE_FEATURE, FACADE_FEATURE, PROBE_FEATURE])
                .with_bin(PROBE_PACKAGE, &[]),
        ]);
        let message = probe_violations(
            &graph,
            Some(&tests_support::clean_probe_manifest()),
            Some(&tests_support::clean_probe_source()),
        );
        assert!(message.contains("required-features"), "{message}");
    }

    #[test]
    fn a_probe_with_no_binary_at_all_is_reported() {
        let graph = PackageGraph::new(vec![Package::new(PROBE_PACKAGE).with_features(&[
            ENGINE_FEATURE,
            FACADE_FEATURE,
            PROBE_FEATURE,
        ])]);
        let message = probe_violations(
            &graph,
            Some(&tests_support::clean_probe_manifest()),
            Some(&tests_support::clean_probe_source()),
        );
        assert!(message.contains("no binary target"), "{message}");
    }

    #[test]
    fn a_probe_missing_a_feature_the_matrix_selects_is_reported() {
        let graph = PackageGraph::new(vec![
            Package::new(PROBE_PACKAGE)
                .with_features(&[PROBE_FEATURE])
                .with_bin(PROBE_PACKAGE, &[PROBE_FEATURE]),
        ]);
        let message = probe_violations(
            &graph,
            Some(&tests_support::clean_probe_manifest()),
            Some(&tests_support::clean_probe_source()),
        );
        assert!(message.contains(ENGINE_FEATURE), "{message}");
        assert!(message.contains(FACADE_FEATURE), "{message}");
    }

    #[test]
    fn a_layer_that_is_not_an_optional_dependency_is_reported() {
        // A layer the baseline image also links contributes nothing to any delta.
        let manifest = tests_support::clean_probe_manifest().replace(
            "waymaker-core = { path = \"../waymaker-core\", optional = true }",
            "waymaker-core = { path = \"../waymaker-core\" }",
        );
        let message = probe_violations(
            &probe_graph(),
            Some(&manifest),
            Some(&tests_support::clean_probe_source()),
        );
        assert!(message.contains("optional = true"), "{message}");
        assert!(message.contains("waymaker-core"), "{message}");
    }

    #[test]
    fn a_feature_that_stopped_enabling_its_layers_is_reported() {
        // `engine = []` satisfies every rule about feature *names* and collapses every
        // delta in the report to zero.
        let manifest = tests_support::clean_probe_manifest().replace(
            "engine = [\"dep:waymaker-core\", \"dep:waymaker-flash\"]",
            "engine = []",
        );
        let message = probe_violations(
            &probe_graph(),
            Some(&manifest),
            Some(&tests_support::clean_probe_source()),
        );
        assert!(message.contains("dep:waymaker-core"), "{message}");
        assert!(message.contains("every delta reads zero"), "{message}");
    }

    #[test]
    fn a_probe_manifest_that_is_not_toml_is_reported() {
        let message = probe_violations(
            &probe_graph(),
            Some("this is not = = toml"),
            Some(&tests_support::clean_probe_source()),
        );
        assert!(message.contains("not valid TOML"), "{message}");
    }

    #[test]
    fn a_probe_that_stopped_being_bare_metal_firmware_is_reported() {
        let message = probe_violations(
            &probe_graph(),
            Some(&tests_support::clean_probe_manifest()),
            Some("//! Not firmware any more.\n"),
        );
        for attribute in PROBE_REQUIRED_ATTRIBUTES {
            assert!(message.contains(attribute), "{attribute} in {message}");
        }
    }

    #[test]
    fn a_commented_out_probe_attribute_does_not_count() {
        // The shared scanner in `source` is what makes this true; the rule inherits it.
        let source = tests_support::clean_probe_source().replace("#![no_main]", "// #![no_main]");
        let message = probe_violations(
            &probe_graph(),
            Some(&tests_support::clean_probe_manifest()),
            Some(&source),
        );
        assert!(message.contains("#![no_main]"), "{message}");
    }

    #[test]
    fn a_probe_with_no_readable_manifest_or_source_is_reported_rather_than_skipped() {
        let message = probe_violations(&probe_graph(), None, None);
        assert!(message.contains("manifest could not be read"), "{message}");
        assert!(message.contains("no crate root"), "{message}");
    }

    // --- the reach rule -------------------------------------------------------------

    fn kernel(contents: &str) -> Vec<LayerSource> {
        vec![LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: "crates/waymaker-core/src/lib.rs".to_owned(),
            contents: contents.to_owned(),
        }]
    }

    fn reach_violations(sources: &[LayerSource], probe: &str) -> String {
        check_probe_reach(sources, Some(probe))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_public_function_the_probe_never_calls_is_reported() {
        // The failure this rule exists for: the layer grows code, the probe does not, the
        // linker discards it, and the 8 KiB gate keeps measuring the probe's own
        // arithmetic. Nothing else catches it — the row is not identical to its base,
        // because the probe's own constants already made it bigger.
        let message = reach_violations(
            &kernel("pub fn advance(&mut self) {}\n"),
            "fn probe() -> usize { 0 }\n",
        );
        assert!(message.contains("advance"), "{message}");
        assert!(message.contains("understates"), "{message}");
    }

    #[test]
    fn a_public_function_the_probe_calls_is_accepted() {
        assert!(
            check_probe_reach(
                &kernel("pub fn advance() {}\n"),
                Some("fn probe() { waymaker_core::advance(); }\n"),
            )
            .is_empty()
        );
    }

    #[test]
    fn the_probes_prose_is_not_a_call() {
        // The rule walked into this: `TypeSize::of` was reported as reached because the
        // probe's documentation contains the English word "of".
        let message = reach_violations(
            &kernel("impl T {\n    pub const fn of<X>() -> Self {}\n}\n"),
            "//! The shape of the probe, and the cost of it.\nfn probe() {}\n",
        );
        assert!(message.contains("`of`"), "{message}");
    }

    #[test]
    fn a_name_inside_a_longer_identifier_is_not_a_call() {
        let message = reach_violations(
            &kernel("pub fn seal() {}\n"),
            "fn probe() { let sealed_record = 0; }\n",
        );
        assert!(message.contains("seal"), "{message}");
    }

    #[test]
    fn every_shape_of_public_function_is_found() {
        let functions = public_functions(&kernel(
            "pub fn plain() {}\n\
             pub const fn constant() {}\n\
             pub async fn eventual() {}\n\
             pub unsafe fn risky() {}\n\
             pub extern \"C\" fn abi() {}\n\
             impl T {\n    pub fn method(&self) {}\n}\n",
        ));
        let names: Vec<&str> = functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["plain", "constant", "eventual", "risky", "abi", "method"]
        );
    }

    #[test]
    fn a_private_function_is_not_the_probes_business() {
        assert!(public_functions(&kernel("fn hidden() {}\n")).is_empty());
        assert!(public_functions(&kernel("pub(crate) fn internal() {}\n")).is_empty());
    }

    #[test]
    fn a_function_in_a_test_module_is_not_linked_and_is_not_required() {
        let functions = public_functions(&kernel(
            "pub fn shipped() {}\n\
             #[cfg(test)]\n\
             mod tests {\n    pub fn helper() {}\n}\n\
             pub fn also_shipped() {}\n",
        ));
        let names: Vec<&str> = functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["shipped", "also_shipped"]);
    }

    #[test]
    fn a_commented_out_declaration_is_not_a_function() {
        assert!(public_functions(&kernel("// pub fn ghost() {}\n")).is_empty());
    }

    #[test]
    fn a_workspace_whose_layers_have_no_public_functions_yet_has_nothing_to_reach() {
        // Rung 0.0. The rule must be silent rather than demanding the probe call nothing.
        assert!(check_probe_reach(&kernel("//! Docs only.\n"), Some("")).is_empty());
    }

    #[test]
    fn a_probe_with_no_source_cannot_be_shown_to_reach_anything() {
        let violations = check_probe_reach(&kernel("pub fn advance() {}\n"), None);
        assert!(!violations.is_empty());
    }

    #[test]
    fn a_worktree_a_concurrent_run_is_using_is_not_swept() {
        // The sweep was added to stop killed runs leaking checkouts, and removing a live
        // one costs the run using it its whole base comparison — which `baseline_diff`
        // downgrades to "not compared", so it is lost silently.
        assert!(!is_leaked(Some(std::time::Duration::from_secs(0))));
        assert!(!is_leaked(Some(std::time::Duration::from_secs(60 * 30))));
        assert!(!is_leaked(None), "an unknown age might be a live run");
    }

    #[test]
    fn a_worktree_older_than_any_run_could_be_is_swept() {
        assert!(is_leaked(Some(LEAKED_WORKTREE_AGE)));
        assert!(is_leaked(Some(
            LEAKED_WORKTREE_AGE + std::time::Duration::from_secs(1)
        )));
    }

    #[test]
    fn a_variant_builds_into_a_directory_named_after_it() {
        // `/` in `waymaker-core/serde` would otherwise nest a directory per crate, and
        // every variant sharing one directory is what let one row's image be read for
        // another's.
        let root = Path::new("/w/target/size");
        assert_eq!(
            variant_build_dir(root, "waymaker-core/serde"),
            root.join("waymaker-core-serde")
        );
        assert_ne!(
            variant_build_dir(root, BASELINE_ROW),
            variant_build_dir(root, DEFAULT_ROW)
        );
    }

    fn find<'a>(variants: &'a [Variant], name: &str) -> &'a Variant {
        variants
            .iter()
            .find(|variant| variant.name == name)
            .unwrap_or_else(|| panic!("{name} is missing"))
    }
}
