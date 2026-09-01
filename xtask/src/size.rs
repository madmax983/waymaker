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
pub const BASELINE_WORKTREE_PATH: &str = "target/waymaker-size-base";

/// The directory this process checks the base branch out into.
#[must_use]
pub fn baseline_worktree(root: &Path) -> PathBuf {
    root.join(format!("{BASELINE_WORKTREE_PATH}-{}", std::process::id()))
}

/// The target directory the matrix builds into.
///
/// Its own directory so that a size run does not evict the rest of the pipeline's build
/// cache: each row is a different feature selection of the same crates, so they would
/// otherwise take turns invalidating one another and everything else.
pub const BUILD_DIR: &str = "target/waymaker-size-build";

/// The version stamped into the JSON report.
pub const REPORT_SCHEMA: u64 = 1;

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
            name: "baseline".to_owned(),
            features: vec![PROBE_FEATURE.to_owned()],
            gated: false,
        },
        Variant {
            name: "default".to_owned(),
            features: vec![PROBE_FEATURE.to_owned(), ENGINE_FEATURE.to_owned()],
            gated: true,
        },
        Variant {
            name: FACADE_FEATURE.to_owned(),
            features: vec![PROBE_FEATURE.to_owned(), FACADE_FEATURE.to_owned()],
            gated: true,
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
    /// Every allocated section that is writable, which is what occupies RAM.
    pub ram: u64,
}

/// The named sections the report breaks out, longest prefix first.
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
            if section.allocated() && section.writable() {
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
    name == section || name.starts_with(&format!("{section}."))
}

/// One measured row of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The variant's name.
    pub name: String,
    /// The feature selection the image was built with.
    pub features: Vec<String>,
    /// The measured sections.
    pub sizes: SectionSizes,
    /// Whether exceeding a budget on this row fails the gate.
    pub gated: bool,
}

impl Row {
    /// Records one measurement.
    #[must_use]
    pub fn new(name: &str, features: &[&str], sizes: SectionSizes, gated: bool) -> Self {
        Self {
            name: name.to_owned(),
            features: features.iter().map(|f| (*f).to_owned()).collect(),
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
    /// smaller, so the authoritative check is the `const` assertion in
    /// [`waymaker_core::budget`] — which every row of the matrix evaluates, because every
    /// row compiles `waymaker-core` for the firmware target. This figure is reported so
    /// that the number has a name and a place in the artifact, and gated as well because
    /// a host figure over budget means the target figure is over budget too.
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

/// A budget that was exceeded, and by how much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetShortfall {
    /// Which budget: `incremental code flash`, `engine RAM`, `kernel state`.
    pub budget: &'static str,
    /// The row or type the number was measured on.
    pub subject: String,
    /// What was measured.
    pub measured: u64,
    /// What design document §04 allows.
    pub limit: u64,
}

impl fmt::Display for BudgetShortfall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} on `{}`: {} B measured against a {} B budget, over by {} B",
            self.budget,
            self.subject,
            self.measured,
            self.limit,
            self.measured.saturating_sub(self.limit)
        )
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

    /// Every measured row, for a test that needs to describe a workspace that does not
    /// exist.
    #[must_use]
    pub const fn rows_mut(&mut self) -> &mut Vec<Row> {
        &mut self.rows
    }

    /// The kernel state registry the report was taken with.
    #[must_use]
    pub const fn kernel_state(&self) -> &KernelState {
        &self.kernel_state
    }

    /// The row every other row is measured against.
    #[must_use]
    pub fn baseline(&self) -> Option<&Row> {
        self.rows.iter().find(|row| row.name == "baseline")
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

    /// Every budget this report exceeds.
    ///
    /// A report with no baseline row is itself a shortfall: without one there is no delta
    /// to measure, and a gate that quietly passed in that state would pass on every run
    /// where the baseline build failed.
    #[must_use]
    pub fn shortfalls(&self) -> Vec<BudgetShortfall> {
        let mut shortfalls = Vec::new();

        if self.kernel_state.total > KERNEL_STATE_BUDGET_BYTES {
            shortfalls.push(BudgetShortfall {
                budget: "kernel state",
                subject: "waymaker-core".to_owned(),
                measured: self.kernel_state.total,
                limit: KERNEL_STATE_BUDGET_BYTES,
            });
        }

        let Some(baseline) = self.baseline() else {
            if !self.rows.is_empty() {
                shortfalls.push(BudgetShortfall {
                    budget: "missing baseline",
                    subject: "baseline".to_owned(),
                    measured: 0,
                    limit: 0,
                });
            }
            return shortfalls;
        };

        for row in self.rows.iter().filter(|row| row.gated) {
            let delta = row.sizes.saturating_delta(&baseline.sizes);
            if delta.flash > INCREMENTAL_CODE_FLASH_BUDGET_BYTES {
                shortfalls.push(BudgetShortfall {
                    budget: "incremental code flash",
                    subject: row.name.clone(),
                    measured: delta.flash,
                    limit: INCREMENTAL_CODE_FLASH_BUDGET_BYTES,
                });
            }
            if delta.ram > ENGINE_RAM_BUDGET_BYTES {
                shortfalls.push(BudgetShortfall {
                    budget: "engine RAM",
                    subject: row.name.clone(),
                    measured: delta.ram,
                    limit: ENGINE_RAM_BUDGET_BYTES,
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

    /// A table with one row per image, and the delta against the baseline beside it.
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
                "  {:<width$}  {:>8} {:>8} {:>8} {:>8}  {:>10} {:>8}\n",
                "variant", ".text", ".rodata", ".data", ".bss", "Δflash", "Δram",
            ),
        ];

        let baseline = self.baseline().map(|row| row.sizes);
        for row in &self.rows {
            let delta = baseline.map(|base| row.sizes.saturating_delta(&base));
            let (flash, ram) = delta.map_or_else(
                || ("?".to_owned(), "?".to_owned()),
                |delta| (format!("+{}", delta.flash), format!("+{}", delta.ram)),
            );
            table.push(format!(
                "  {:<width$}  {:>8} {:>8} {:>8} {:>8}  {:>10} {:>8}{}\n",
                row.name,
                row.sizes.text,
                row.sizes.rodata,
                row.sizes.data,
                row.sizes.bss,
                flash,
                ram,
                if row.gated { "  gated" } else { "" },
            ));
        }

        table.push(format!(
            "\nbudgets: incremental code flash {INCREMENTAL_CODE_FLASH_BUDGET_BYTES} B, engine RAM {ENGINE_RAM_BUDGET_BYTES} B (of {} B runtime RAM, less a {} B caller-owned scratch page)\n",
            waymaker_core::budget::RUNTIME_RAM_BYTES,
            waymaker_core::budget::SCRATCH_PAGE_BYTES,
        ));
        table.push(format!(
            "kernel state: {} B of {KERNEL_STATE_BUDGET_BYTES} B across {} registered type(s), sized for the host; the gate for {FIRMWARE_TARGET} is the const assertion in waymaker_core::budget, which every row above compiles\n",
            self.kernel_state.total,
            self.kernel_state.types.len(),
        ));
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
                entry.insert("gated".to_owned(), Value::from(row.gated));
                entry.insert("text".to_owned(), Value::from(row.sizes.text));
                entry.insert("rodata".to_owned(), Value::from(row.sizes.rodata));
                entry.insert("data".to_owned(), Value::from(row.sizes.data));
                entry.insert("bss".to_owned(), Value::from(row.sizes.bss));
                entry.insert("flash".to_owned(), Value::from(row.sizes.flash));
                entry.insert("ram".to_owned(), Value::from(row.sizes.ram));
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
                    gated: row.get("gated").and_then(Value::as_bool).unwrap_or(false),
                    sizes: SectionSizes {
                        text: number(row, "text"),
                        rodata: number(row, "rodata"),
                        data: number(row, "data"),
                        bss: number(row, "bss"),
                        flash: number(row, "flash"),
                        ram: number(row, "ram"),
                    },
                })
            })
            .collect::<Result<Vec<Row>, SizeError>>()?;

        let kernel_state = document
            .get("kernel_state")
            .ok_or_else(|| SizeError::new("the size report has no `kernel_state`"))?;
        let kernel_state = KernelState {
            total: number(kernel_state, "total"),
            types: kernel_state
                .get("types")
                .and_then(Value::as_array)
                .map(|types| {
                    types
                        .iter()
                        .filter_map(|entry| {
                            Some((
                                entry.get("name")?.as_str()?.to_owned(),
                                number(entry, "size"),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };

        Ok(Self { rows, kernel_state })
    }
}

fn number(value: &Value, field: &str) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or(0)
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
}

/// Every row that differs between `base` and `head`, plus the rows only one of them has.
#[must_use]
pub fn diff(base: &SizeReport, head: &SizeReport) -> Vec<RowDiff> {
    let mut diffs = Vec::new();

    for row in head.rows() {
        let before = base.row(&row.name).map(|other| other.sizes);
        if before == Some(row.sizes) {
            continue;
        }
        diffs.push(RowDiff {
            name: row.name.clone(),
            before,
            after: Some(row.sizes),
        });
    }

    for row in base.rows() {
        if head.row(&row.name).is_none() {
            diffs.push(RowDiff {
                name: row.name.clone(),
                before: Some(row.sizes),
                after: None,
            });
        }
    }

    diffs
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
        let image = build_variant(root, &variant)?;
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
            sizes: SectionSizes::of(&sections),
            gated: variant.gated,
        });
    }

    Ok(SizeReport::new(rows, KernelState::measured()))
}

/// Links one image and returns the path to it.
fn build_variant(root: &Path, variant: &Variant) -> Result<PathBuf, SizeError> {
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
            BUILD_DIR,
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

    violations.extend(check_probe_manifest(manifest));
    violations.extend(check_probe_source(source));
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
pub const PROBE_REQUIRED_ATTRIBUTES: &[&str] =
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

    let attributes: Vec<String> = source
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("#!["))
        .map(|line| line.split_whitespace().collect::<String>())
        .collect();

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

    let measured = measure(&worktree);
    remove_worktree(root, &worktree);
    measured
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
        if let Ok(output) = output {
            if output.status.success() {
                let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if !commit.is_empty() {
                    return Ok(commit);
                }
            }
        }
    }
    Err(SizeError::new(format!(
        "`{reference}` does not name a commit in this checkout; a shallow clone has no base branch to compare against, so fetch it with `fetch-depth: 0`"
    )))
}

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
pub mod tests_support {
    use super::{ENGINE_FEATURE, FACADE_FEATURE, PROBE_FEATURE, PROBE_REQUIRED_ATTRIBUTES};
    use crate::policy::LAYERS;

    /// A probe manifest that satisfies every rule in [`super::check_size_probe`].
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
        format!(
            "[package]\nname = \"waymaker-size-probe\"\n\n[dependencies]\n{dependencies}\n[features]\ndefault = []\n{PROBE_FEATURE} = []\n{ENGINE_FEATURE} = []\n{FACADE_FEATURE} = []\n"
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

    fn report(default_flash: u64, default_ram: u64) -> SizeReport {
        SizeReport::new(
            vec![
                Row::new("baseline", &[PROBE_FEATURE], SectionSizes::default(), false),
                Row::new(
                    "default",
                    &[PROBE_FEATURE, ENGINE_FEATURE],
                    SectionSizes {
                        text: default_flash,
                        flash: default_flash,
                        bss: default_ram,
                        ram: default_ram,
                        ..SectionSizes::default()
                    },
                    true,
                ),
            ],
            KernelState::measured(),
        )
    }

    #[test]
    fn a_report_within_every_budget_has_no_shortfalls() {
        assert!(report(1_024, 64).shortfalls().is_empty());
    }

    #[test]
    fn exceeding_the_code_flash_budget_names_the_offending_number() {
        let over = INCREMENTAL_CODE_FLASH_BUDGET_BYTES + 1;
        let shortfalls = report(over, 0).shortfalls();
        let rendered = shortfalls
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains(&over.to_string()), "{rendered}");
        assert!(
            rendered.contains(&INCREMENTAL_CODE_FLASH_BUDGET_BYTES.to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("default"), "{rendered}");
    }

    #[test]
    fn exceeding_the_engine_ram_budget_names_the_offending_number() {
        let over = ENGINE_RAM_BUDGET_BYTES + 1;
        let rendered = report(0, over)
            .shortfalls()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains(&over.to_string()), "{rendered}");
        assert!(rendered.contains("RAM"), "{rendered}");
    }

    #[test]
    fn the_gate_measures_a_delta_rather_than_an_absolute_size() {
        // A baseline that is itself large must not be charged to the engine.
        let mut report = report(64, 0);
        report.rows_mut().first_mut().expect("a baseline").sizes = SectionSizes {
            text: 4_096,
            flash: 4_096,
            ..SectionSizes::default()
        };
        let delta = report.delta_of("default").expect("a default row");
        assert_eq!(delta.flash, 0, "a smaller image is not a negative cost");
        assert!(report.shortfalls().is_empty());
    }

    #[test]
    fn an_ungated_row_is_reported_but_not_failed() {
        let mut report = report(0, 0);
        report.rows_mut().push(Row::new(
            "waymaker-core/serde",
            &[PROBE_FEATURE, ENGINE_FEATURE, "waymaker-core/serde"],
            SectionSizes {
                flash: INCREMENTAL_CODE_FLASH_BUDGET_BYTES + 1,
                ..SectionSizes::default()
            },
            false,
        ));
        assert!(report.shortfalls().is_empty());
        assert!(report.render().contains("waymaker-core/serde"));
    }

    #[test]
    fn a_report_with_no_baseline_row_fails_rather_than_reporting_zero() {
        let report = SizeReport::new(
            vec![Row::new(
                "default",
                &[PROBE_FEATURE, ENGINE_FEATURE],
                SectionSizes::default(),
                true,
            )],
            KernelState::measured(),
        );
        let shortfalls = report.shortfalls();
        assert!(
            shortfalls
                .iter()
                .any(|shortfall| shortfall.to_string().contains("baseline")),
            "{shortfalls:?}"
        );
    }

    #[test]
    fn the_kernel_state_budget_is_gated_too() {
        let report = SizeReport::new(
            vec![Row::new(
                "baseline",
                &[PROBE_FEATURE],
                SectionSizes::default(),
                false,
            )],
            KernelState {
                total: KERNEL_STATE_BUDGET_BYTES + 7,
                types: vec![("Cursor".to_owned(), KERNEL_STATE_BUDGET_BYTES + 7)],
            },
        );
        let rendered = report
            .shortfalls()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains(&(KERNEL_STATE_BUDGET_BYTES + 7).to_string()),
            "{rendered}"
        );
    }

    #[test]
    fn the_report_renders_every_row_with_its_delta() {
        let rendered = report(512, 32).render();
        assert!(rendered.contains("baseline"), "{rendered}");
        assert!(rendered.contains("default"), "{rendered}");
        assert!(rendered.contains("512"), "{rendered}");
        assert!(rendered.contains(".text"), "{rendered}");
        assert!(rendered.contains(".bss"), "{rendered}");
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
    fn a_diff_names_what_grew_what_shrank_and_what_is_new() {
        let base = report(500, 16);
        let mut head = report(700, 16);
        head.rows_mut().push(Row::new(
            "waymaker-core/serde",
            &[PROBE_FEATURE, ENGINE_FEATURE, "waymaker-core/serde"],
            SectionSizes {
                flash: 900,
                ..SectionSizes::default()
            },
            false,
        ));

        let rendered = render_diff(&diff(&base, &head));
        assert!(rendered.contains("default"), "{rendered}");
        assert!(rendered.contains("+200"), "{rendered}");
        assert!(rendered.contains("waymaker-core/serde"), "{rendered}");
        assert!(rendered.contains("new"), "{rendered}");
    }

    #[test]
    fn a_diff_of_a_report_against_itself_says_nothing_changed() {
        let rendered = render_diff(&diff(&report(500, 16), &report(500, 16)));
        assert!(rendered.contains("no change"), "{rendered}");
    }

    #[test]
    fn a_row_that_disappeared_is_reported_as_removed() {
        let base = report(500, 16);
        let mut head = report(500, 16);
        head.rows_mut().retain(|row| row.name != "default");
        let rendered = render_diff(&diff(&base, &head));
        assert!(rendered.contains("removed"), "{rendered}");
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

    fn find<'a>(variants: &'a [Variant], name: &str) -> &'a Variant {
        variants
            .iter()
            .find(|variant| variant.name == name)
            .unwrap_or_else(|| panic!("{name} is missing"))
    }
}
