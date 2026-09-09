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

/// Where the base branch at `commit` links into.
///
/// Keyed by commit, so runs comparing different bases cannot read one another's images and
/// runs comparing the same base can share the work.
#[must_use]
fn baseline_build_dir(root: &Path, commit: &str) -> PathBuf {
    let short: String = commit
        .chars()
        .take(12)
        .filter(char::is_ascii_alphanumeric)
        .collect();
    root.join(format!("{BUILD_DIR}-base-{short}"))
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

/// The `--config` override that keeps the symbol table in the linked image.
///
/// Design document §04's budgets are measured against `[profile.release]` as the workspace
/// declares it, and this changes one setting of it: `strip`. That setting removes
/// unallocated sections and nothing else, so every number the gate reads is the same on
/// either side of it — and [`check_symbols_are_not_measured`] holds each image to that
/// rather than taking it on trust.
const STRIP_NOTHING: &str = "profile.release.strip=\"none\"";

/// The version stamped into the JSON report.
const REPORT_SCHEMA: u64 = 2;

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
    let Some(probe) = graph.find(PROBE_PACKAGE) else {
        return Vec::new();
    };
    let probe_features = probe.features.clone();

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
            // The probe's own mirror when it declares one, so the probe can `#[cfg]` on
            // the row and reach the code it exists to measure. `check_probe_mirrors` is
            // what makes the mirror compulsory; the fallback keeps a workspace whose probe
            // predates that rule measurable rather than unbuildable.
            let mirror = mirror_feature(spec.name, feature);
            let selector = if probe_features.contains(&mirror) {
                mirror
            } else {
                format!("{}/{feature}", spec.name)
            };
            variants.push(Variant {
                name: format!("{}/{feature}", spec.name),
                features: vec![PROBE_FEATURE.to_owned(), base.to_owned(), selector],
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

/// The crate name the probe's own symbols carry.
///
/// Cargo package names use `-` and Rust paths use `_`, and a mangled symbol carries the
/// path spelling. Derived rather than written down, so a renamed probe cannot leave the
/// attribution matching nothing and reading every image as all layers.
#[must_use]
pub fn probe_crate_name() -> String {
    PROBE_PACKAGE.replace('-', "_")
}

/// The crate that *declares* `mangled`, as Rust's name mangling records it.
///
/// The first crate-root component of the path, which is where the body was written. A
/// monomorphised generic names two crates — `waymaker_flash::frame::encode_with` linked
/// for the probe carries `waymaker_flash` in its path and `waymaker_size_probe` as the
/// instantiating crate — and crediting the byte count to the instantiation would hand
/// every generic in the engine back to the row this attribution exists to correct.
///
/// `None` for a symbol no Rust compiler mangled: `__aeabi_memcpy` and the rest belong to
/// nobody here, and everything that belongs to nobody stays charged to the layers.
#[must_use]
pub fn defining_crate(mangled: &str) -> Option<&str> {
    v0_crate(mangled).or_else(|| legacy_crate(mangled))
}

/// The `v0` production for a trait's own provided method, `<Self as Trait>::method`.
///
/// It is the one production that puts the crate roots the other way round: `Y <type>
/// <path>` names the *self* type first and the trait second, and the body of a provided
/// method is declared with the trait. So the first crate root of such a name is the crate
/// that wrote the `impl`, not the crate that wrote the code.
///
/// A layer trait with a default body, implemented for one of the probe's types, is
/// therefore a layer's bytes under the probe's name — and this gate *subtracts* what it
/// reads as the probe's, so that is a budget loosened silently. No trait in the layers has
/// a provided method today, and `size-probe-reach` pushes the probe toward implementing
/// every one they add.
const QUALIFIED: char = 'Y';

/// The crate root of a `v0` mangled name (`_RNvCs<hash>_14waymaker_flash5frame…`).
///
/// Two ways this scanner declines to answer, and both charge the bytes to the layers,
/// which is the bias the rest of the attribution already has:
///
/// * a [`QUALIFIED`] path anywhere before the crate root, because the first crate root of
///   one is not the crate that wrote the body and skipping the self type would need most
///   of the grammar rather than one production of it;
/// * any name whose first parseable crate root is not one, which a `C` inside a long
///   base-62 disambiguator can produce. It cannot spell this workspace's probe: a
///   disambiguator holds no `_`, and every crate name here does.
fn v0_crate(mangled: &str) -> Option<&str> {
    let path = mangled.strip_prefix("_R")?;
    // The first `C` that begins a well-formed crate root. Scanned rather than parsed: the
    // components before it are namespace and backreference codes, and one production is
    // all a scan has to recognise — as long as it refuses the one production that would
    // make recognising it the wrong answer.
    let at = path
        .match_indices('C')
        .find(|(at, _)| {
            path.get(at.saturating_add(1)..)
                .and_then(crate_root)
                .is_some()
        })
        .map(|(at, _)| at)?;
    if path.get(..at)?.contains(QUALIFIED) {
        return None;
    }
    crate_root(path.get(at.saturating_add(1)..)?)
}

/// The identifier of a crate root, given the bytes after its `C`.
fn crate_root(rest: &str) -> Option<&str> {
    // An optional disambiguator: `s`, base-62 digits, `_`.
    let rest = match rest.strip_prefix('s') {
        Some(after) => {
            let (disambiguator, tail) = after.split_once('_')?;
            disambiguator
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
                .then_some(tail)?
        }
        None => rest,
    };
    // `u` marks a punycode identifier, whose decoded spelling is not what a symbol from
    // this workspace carries; the encoded one is still a name, and reading it is better
    // than reading the crate root after it.
    identifier(rest.strip_prefix('u').unwrap_or(rest))
}

/// The crate root of a `legacy` mangled name (`_ZN14waymaker_flash5frame…`).
fn legacy_crate(mangled: &str) -> Option<&str> {
    identifier(mangled.strip_prefix("_ZN")?)
}

/// A length-prefixed identifier, if `rest` opens with one.
///
/// A length longer than what follows it answers `None` rather than the characters that
/// happen to be there: a truncation of a crate name is a crate name, and attributing bytes
/// to it is worse than attributing them to nobody.
fn identifier(rest: &str) -> Option<&str> {
    let digits = rest.len().saturating_sub(
        rest.trim_start_matches(|character: char| character.is_ascii_digit())
            .len(),
    );
    let (length, tail) = rest.split_at_checked(digits)?;
    let length: usize = length.parse().ok()?;
    // v0 separates the length from an identifier that would otherwise start with a digit.
    let name = tail.strip_prefix('_').unwrap_or(tail).get(..length)?;
    let mut characters = name.chars();
    let starts = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_');
    let continues =
        characters.all(|character| character.is_ascii_alphanumeric() || character == '_');
    (starts && continues).then_some(name)
}

/// How many of the image's stored bytes the symbol table attributes to `crate_name`.
///
/// Only symbols in a section that costs flash, because that is the budget being read: a
/// `.bss` symbol is RAM, and a symbol in a section that was discarded is nothing.
/// `SHN_UNDEF` and the reserved indices name no section and are attributed to nobody.
///
/// # Bytes, not entries
///
/// The measure of the *address ranges* the crate's symbols cover, not the sum of their
/// sizes. Under `lto = "fat"` and `opt-level = "z"` the linker folds identical function
/// bodies and can leave several mangled names on one body, so a sum counts the same stored
/// bytes once per name — and this figure is *subtracted* from the budget, so an overstated
/// one is a budget quietly loosened.
///
/// A range another crate's symbol also covers is credited to nobody, for the same reason
/// in the other direction: a body folded together with a layer's is not the probe's to
/// take off the layers' bill. Unattributable symbols count as another crate's here, since
/// `__aeabi_memcpy` folded onto a probe body is no more the probe's than a layer's is.
///
/// `st_value` is used as the symbol table holds it, with the ARM interworking bit still on
/// a Thumb function. Two ranges are comparable only where both carry it, so the bit can
/// only ever manufacture an overlap — which removes credit from the crate being measured,
/// and so errs toward charging the layers.
#[must_use]
pub fn attributed_flash(sections: &[Section], symbols: &[elf::Symbol], crate_name: &str) -> u64 {
    let stored = |symbol: &elf::Symbol| {
        symbol.size > 0
            && symbol.section_index != elf::SHN_UNDEF
            && sections
                .get(usize::from(symbol.section_index))
                .is_some_and(Section::occupies_storage)
    };

    let mut sections_seen: Vec<u16> = symbols
        .iter()
        .filter(|symbol| stored(symbol))
        .map(|symbol| symbol.section_index)
        .collect();
    sections_seen.sort_unstable();
    sections_seen.dedup();

    let ranges = |index: u16, mine: bool| {
        merged(
            symbols
                .iter()
                .filter(|symbol| stored(symbol) && symbol.section_index == index)
                .filter(|symbol| (defining_crate(&symbol.name) == Some(crate_name)) == mine)
                .map(|symbol| (symbol.address, symbol.address.saturating_add(symbol.size)))
                .collect(),
        )
    };

    sections_seen.into_iter().fold(0_u64, |total, index| {
        total.saturating_add(length_outside(&ranges(index, true), &ranges(index, false)))
    })
}

/// `ranges`, sorted, with everything that touches or overlaps joined into one.
fn merged(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// How many bytes of `mine` no range of `theirs` covers. Both are [`merged`].
fn length_outside(mine: &[(u64, u64)], theirs: &[(u64, u64)]) -> u64 {
    mine.iter().fold(0_u64, |total, &(start, end)| {
        let covered = theirs
            .iter()
            .fold(0_u64, |covered, &(other_start, other_end)| {
                let overlap = end.min(other_end).saturating_sub(start.max(other_start));
                covered.saturating_add(overlap)
            });
        total.saturating_add(end.saturating_sub(start).saturating_sub(covered))
    })
}

/// Rule: the symbol table this attribution is read from costs no byte the gate measures.
///
/// The matrix links its images with `strip` off so that there are symbols to attribute,
/// and gates the section sizes of that same image. The two are the same measurement only
/// while the symbol table is unallocated — which every linker makes it, and which is
/// therefore worth asking rather than assuming, because the failure is a budget quietly
/// read against a bigger image than the one that ships.
///
/// An image with no symbol table at all fails here too: a zero attribution is
/// indistinguishable from a probe that cost nothing, and reading it as the latter restores
/// the figure this module exists to correct.
///
/// # Errors
///
/// Returns [`SizeError`] if the image carries no symbol table, or if a symbol, string or
/// debug section is allocated.
pub fn check_symbols_are_not_measured(sections: &[Section]) -> Result<(), SizeError> {
    // Exactly one, not at least one. `elf::symbols` reads every `SHT_SYMTAB` section
    // there is, so a second one would attribute the same bytes twice — which subtracts
    // them twice, in the direction that passes.
    let tables = sections
        .iter()
        .filter(|section| section.kind == elf::SHT_SYMTAB)
        .count();
    if tables != 1 {
        return Err(SizeError::new(format!(
            "the linked image carries {tables} symbol tables; the matrix links with `strip` off precisely so that there is one, and every one of them is read"
        )));
    }

    for section in sections {
        let stripped = matches!(section.kind, elf::SHT_SYMTAB | elf::SHT_STRTAB)
            || section.name.starts_with(".debug");
        if stripped && section.allocated() {
            return Err(SizeError::new(format!(
                "`{}` is allocated, so stripping it would change a section the budget is measured on; the attribution and the gated sizes would then be readings of two different images",
                section.name
            )));
        }
    }

    Ok(())
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
    /// Stored bytes this image's symbol table attributes to the probe crate itself.
    ///
    /// Design document §04's code-flash budget is stated for "core + flash adapter", and
    /// the probe is neither: its `match` arms, folds and calls exist only to keep the
    /// layers' code alive past `--gc-sections`. Recorded per row rather than subtracted on
    /// the spot so that the report can state the split as well as gate the corrected
    /// figure.
    pub probe_flash: u64,
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
        probe_flash: u64,
        gated: bool,
    ) -> Self {
        Self {
            name: name.to_owned(),
            features: features.iter().map(|f| (*f).to_owned()).collect(),
            measured_against: measured_against.to_owned(),
            sizes,
            probe_flash,
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
    /// Reads the registry out of [`waymaker_core::budget`], for the checkout `xtask` was
    /// built from.
    ///
    /// Only ever `Some` for that checkout: this reads the crate linked into this binary, so
    /// asking it about a base-branch worktree would answer about the head. Callers
    /// measuring another tree pass `None`, and the report says the figure is unknown rather
    /// than printing the head's.
    ///
    /// These are sizes for the host, because that is the target `xtask` is compiled for.
    /// The budget is stated for `thumbv6m-none-eabi`, where a type holding a pointer is
    /// *smaller* — so the host figure is an upper bound on the target one. Gating it is
    /// therefore conservative: it can fail early, never late, and a build it fails might
    /// have fitted on the target. The authoritative check is the `const` assertion in
    /// [`waymaker_core::budget`], which every row of the matrix but the baseline evaluates,
    /// because every one of those compiles `waymaker-core` for the firmware target.
    #[must_use]
    pub fn measured() -> Option<Self> {
        Some(Self {
            total: waymaker_core::budget::KERNEL_STATE_TOTAL_BYTES as u64,
            types: waymaker_core::budget::KERNEL_STATE_TYPES
                .iter()
                .map(|entry| (entry.name.to_owned(), entry.size as u64))
                .collect(),
        })
    }
}

/// One of the gates a measurement is held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    /// The incremental code-flash gate for the kernel plus the flash adapter.
    ///
    /// Design document §04 states 8 KiB as a *v0.1* target; the number this variant gates on
    /// is `waymaker_core::budget::INCREMENTAL_CODE_FLASH_BYTES`, which ADR 0017 raised once
    /// for rung 0.2's two-bank lifecycle. Naming a figure here rather than the constant is
    /// how a gate's own documentation ends up describing a gate it no longer is.
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
    kernel_state: Option<KernelState>,
}

impl SizeReport {
    /// Collects measured rows into a report.
    #[must_use]
    pub const fn new(rows: Vec<Row>, kernel_state: Option<KernelState>) -> Self {
        Self { rows, kernel_state }
    }

    /// Every measured row, in matrix order.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The kernel state registry the report was taken with, where it could be read.
    ///
    /// `None` for a checkout this build of `xtask` was not compiled against — the base
    /// branch — because the only registry it can read is its own.
    #[must_use]
    pub const fn kernel_state(&self) -> Option<&KernelState> {
        self.kernel_state.as_ref()
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

    /// How much more of `name`'s image the symbol table attributes to the probe than it
    /// does of the baseline's.
    ///
    /// A reported figure, and it saturates like every other delta here: a probe that got
    /// *smaller* reads as 0 rather than as a negative cost. [`Self::layers_flash_of`] does
    /// not go through it for that reason — it subtracts each image's non-probe bytes, so a
    /// shrinking probe is counted as the layer growth it is.
    #[must_use]
    pub fn probe_delta_of(&self, name: &str) -> Option<u64> {
        let baseline = self.baseline()?;
        let row = self.row(name)?;
        Some(row.probe_flash.saturating_sub(baseline.probe_flash))
    }

    /// The layers' share of one row's flash delta, given the baseline it is measured
    /// against.
    ///
    /// Takes the rows rather than their names. [`Self::row`] answers with the *first* row
    /// carrying a name, and the gate is applied to the rows it iterates — so a report with
    /// two rows called `default` would have had every one of them gated on the figure of
    /// the first. `--report` gates a document this process did not produce, which is the
    /// same reason the `gated` flag is not taken at its word.
    ///
    /// Each image's non-probe bytes, subtracted — rather than the image delta with the
    /// probe delta taken off it. The two agree while the probe grows and part company when
    /// it *shrinks*, because a saturating subtraction of the probe term discards the sign
    /// and hands the difference to nobody: an image 12 200 B larger whose probe is 200 B
    /// smaller is 12 400 B of layer growth, and the other order reports 12 200 B. That is
    /// the loosening direction, so the sign is kept where it can be.
    ///
    /// `probe_flash` is refused above where it exceeds the image it was read from, so
    /// neither inner subtraction saturates. The outer one does, for
    /// [`SectionSizes::saturating_delta`]'s reason: a row that links *less* than the
    /// baseline is a measurement fault rather than a negative cost.
    #[must_use]
    const fn layers_of(row: &Row, baseline: &Row) -> u64 {
        let row_layers = row.sizes.flash.saturating_sub(row.probe_flash);
        let baseline_layers = baseline.sizes.flash.saturating_sub(baseline.probe_flash);
        row_layers.saturating_sub(baseline_layers)
    }

    /// The layers' share of `name`'s flash delta: the image delta less what the probe's
    /// own code grew by.
    ///
    /// This is the number design document §04's code-flash budget is stated over, and the
    /// number [`Self::shortfalls`] gates. Everything the symbol table does not name as the
    /// probe's stays here — `.rodata` strings, `compiler_builtins`, the alignment padding
    /// between functions — because a byte nobody can attribute is a byte the layers
    /// brought, and a budget should err toward failing.
    ///
    /// Saturating for [`SectionSizes::saturating_delta`]'s reason: a probe share larger
    /// than the image it was read from is a measurement fault, and
    /// [`Self::shortfalls`] refuses it rather than letting it credit the layers.
    #[must_use]
    pub fn layers_flash_of(&self, name: &str) -> Option<u64> {
        Some(Self::layers_of(self.row(name)?, self.baseline()?))
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

        if let Some(kernel_state) = self.kernel_state.as_ref()
            && kernel_state.total > KERNEL_STATE_BUDGET_BYTES
        {
            shortfalls.push(BudgetShortfall::Exceeded {
                budget: Budget::KernelState,
                subject: "waymaker-core".to_owned(),
                measured: kernel_state.total,
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

        // The report says which rows are gated, and `--report` gates a document this
        // process did not produce. A report with no `default` row, or one whose `gated`
        // flag says false, would otherwise leave the loop below with nothing to check and
        // exit zero — letting the document choose whether it is gated.
        if !self
            .rows
            .iter()
            .any(|row| row.name == DEFAULT_ROW && row.gated)
        {
            shortfalls.push(BudgetShortfall::Unmeasurable {
                detail: format!(
                    "the report has no gated `{DEFAULT_ROW}` row, which is the configuration design document \u{a7}04's budgets are stated for"
                ),
            });
        }

        // Every image the matrix links *is* the probe, so a row that attributes nothing to
        // it is a row whose symbol table was not read — a stripped image, a parser that
        // came back empty, a report written by an older build. Reading that as "the probe
        // cost nothing" restores the figure this correction exists to remove, silently and
        // in the direction that passes. Every row, not only the gated ones: the report
        // states the split for all of them, and a misstatement is worth as much as a wrong
        // gate to whoever reads it.
        for row in &self.rows {
            if row.probe_flash == 0 {
                shortfalls.push(BudgetShortfall::Unmeasurable {
                    detail: format!(
                        "`{}` attributes no byte at all to `{PROBE_PACKAGE}`, but every image the matrix links is the probe; its symbol table was not read",
                        row.name
                    ),
                });
            }
            if row.probe_flash > row.sizes.flash {
                shortfalls.push(BudgetShortfall::Unmeasurable {
                    detail: format!(
                        "`{}` attributes {} B to `{PROBE_PACKAGE}` out of an image holding {} B, which is not a reading of that image",
                        row.name, row.probe_flash, row.sizes.flash
                    ),
                });
            }
        }

        for row in self.rows.iter().filter(|row| row.gated) {
            let delta = row.sizes.saturating_delta(&baseline.sizes);
            // The layers' share rather than the image delta. Design document §04 states
            // the budget for "core + flash adapter", and issue #72 is that the probe's own
            // arithmetic had grown to more than a third of what was being gated.
            let layers = Self::layers_of(row, baseline);
            // A gated row links the kernel and the flash adapter, which cannot cost
            // nothing — the baseline's own zero is refused above for that reason. Two
            // routes reach this one, and both are faults rather than results: the linker
            // discarded the layers, so the image did not grow; or the probe's attributed
            // growth swallowed the whole delta, which `layers_of` saturates rather than
            // reporting as a negative cost. The per-image bound on `probe_flash` above
            // cannot see either, because it compares against the whole image.
            if layers == 0 {
                shortfalls.push(BudgetShortfall::Unmeasurable {
                    detail: format!(
                        "`{}` leaves the layers 0 B of a {} B image delta, {} B of which is attributed to `{PROBE_PACKAGE}`; a row that links the engine cannot cost nothing",
                        row.name,
                        delta.flash,
                        row.probe_flash.saturating_sub(baseline.probe_flash),
                    ),
                });
            }
            if layers > INCREMENTAL_CODE_FLASH_BUDGET_BYTES {
                shortfalls.push(BudgetShortfall::Exceeded {
                    budget: Budget::IncrementalCodeFlash,
                    subject: row.name.clone(),
                    measured: layers,
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
                "  {:<width$}  {:>9} {:>9} {:>9} {:>9}  {:>9} {:>9} {:>9} {:>9}  {:>12}\n",
                "variant",
                "\u{394}.text",
                "\u{394}.rodata",
                "\u{394}.data",
                "\u{394}.bss",
                "\u{394}flash",
                "probe",
                "layers",
                "\u{394}ram",
                "over base",
            ),
        ];

        for row in &self.rows {
            // The baseline's own row shows what a firmware with no Waymaker in it costs,
            // so its columns are absolute and everything else is a delta against it.
            let delta = self.delta_of(&row.name).unwrap_or(row.sizes);
            let probe = self.probe_delta_of(&row.name).unwrap_or(row.probe_flash);
            let layers = self
                .layers_flash_of(&row.name)
                .unwrap_or_else(|| row.sizes.flash.saturating_sub(row.probe_flash));
            let increment = self.increment_of(&row.name).map_or_else(
                || "-".to_owned(),
                |increment| format!("+{} flash", increment.flash),
            );
            table.push(format!(
                "  {:<width$}  {:>9} {:>9} {:>9} {:>9}  {:>9} {:>9} {:>9} {:>9}  {increment:>12}{}\n",
                row.name,
                delta.text,
                delta.rodata,
                delta.data,
                delta.bss,
                delta.flash,
                probe,
                layers,
                delta.ram,
                if row.gated { "  gated" } else { "" },
            ));
        }

        table.push(format!(
            "\nbudgets: incremental code flash {INCREMENTAL_CODE_FLASH_BUDGET_BYTES} B, engine statics {ENGINE_RAM_BUDGET_BYTES} B (of {} B runtime RAM, less a {} B caller-owned scratch page)\n",
            waymaker_core::budget::RUNTIME_RAM_BYTES,
            waymaker_core::budget::SCRATCH_PAGE_BYTES,
        ));
        table.push(format!(
            "code flash: `layers` is what is gated. `\u{394}flash` is the whole image delta and `probe` is the part of it the symbol table names as {PROBE_PACKAGE}'s own arithmetic, which exists only to keep the layers' code alive past --gc-sections. Both are deltas against the baseline image, whose own probe symbols are {} B. Every byte no symbol attributes to the probe stays in `layers`.\n",
            self.baseline().map_or(0, |row| row.probe_flash),
        ));
        table.push(
            "runtime RAM: statics only. A cursor, context or record header on the caller's stack moves no writable section, so \u{394}ram is a floor on design document \u{a7}04's runtime RAM and not the rule itself; stack accounting needs a call graph and arrives with the code that has one.\n"
                .to_owned(),
        );
        table.push(self.kernel_state.as_ref().map_or_else(
            || "kernel state: not read; this report is of a checkout `xtask` was not built against, so the only registry it could read would be the wrong one\n".to_owned(),
            |kernel_state| format!(
                "kernel state: {} B of {KERNEL_STATE_BUDGET_BYTES} B across {} registered type(s), sized for the host, which is an upper bound on the target; the gate for {FIRMWARE_TARGET} is the const assertion in waymaker_core::budget, which every row above but the baseline compiles\n",
                kernel_state.total,
                kernel_state.types.len(),
            ),
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
                entry.insert("probe_flash".to_owned(), Value::from(row.probe_flash));
                // Derived, like `delta` below, and for the same reason: this is the number
                // the gate holds to the budget, and a consumer that has to re-derive it is
                // a consumer that can derive it differently.
                if let Some(layers) = self.layers_flash_of(&row.name) {
                    entry.insert("layers_flash".to_owned(), Value::from(layers));
                }
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

        let kernel_state = self.kernel_state.as_ref().map_or(Value::Null, |state| {
            let types: Vec<Value> = state
                .types
                .iter()
                .map(|(name, size)| {
                    let mut entry = Map::new();
                    entry.insert("name".to_owned(), Value::from(name.clone()));
                    entry.insert("size".to_owned(), Value::from(*size));
                    Value::Object(entry)
                })
                .collect();
            let mut object = Map::new();
            object.insert("total".to_owned(), Value::from(state.total));
            object.insert("types".to_owned(), Value::Array(types));
            Value::Object(object)
        });

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
        document.insert("kernel_state".to_owned(), kernel_state);
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
                    probe_flash: number(row, "probe_flash")?,
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

        check_row_names_are_unique(&rows)?;

        let kernel_state = document
            .get("kernel_state")
            .ok_or_else(|| SizeError::new("the size report has no `kernel_state`"))?;
        if kernel_state.is_null() {
            return Ok(Self {
                rows,
                kernel_state: None,
            });
        }
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

        Ok(Self {
            rows,
            kernel_state: Some(kernel_state),
        })
    }
}

/// Rule: no two rows of a report carry one name.
///
/// Every lookup in a report is by name and answers with the first row carrying one, so a
/// document with two `default` rows would have every one of them gated on the figure of the
/// first, whatever the second held. `--report` reads a document this process did not
/// produce, which is the same reason the `gated` flag is not taken at its word.
fn check_row_names_are_unique(rows: &[Row]) -> Result<(), SizeError> {
    for (at, row) in rows.iter().enumerate() {
        if rows.iter().take(at).any(|earlier| earlier.name == row.name) {
            return Err(SizeError::new(format!(
                "the size report has two rows called `{}`, and every figure in it is looked up by name",
                row.name
            )));
        }
    }
    Ok(())
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
    /// The base branch's probe share, for a row the budget is held against.
    ///
    /// Reported beside the layers' figure because it is the term the gate *subtracts*, and
    /// the one a contributor moves most easily: `size-probe-reach` obliges the probe to
    /// call every public function a layer declares, so probe code grows whenever library
    /// code does. Without it a pull request that adds 2 KiB to each reads as `flash +4000,
    /// layers +0`, with the subtraction recoverable only by arithmetic.
    pub before_probe: Option<u64>,
    /// This branch's probe share, on the same terms.
    pub after_probe: Option<u64>,
    /// The base branch's layers' share, for a row the budget is held against.
    ///
    /// Only for a gated row. The layers' share is defined against the baseline image,
    /// which is what the budget is stated over; a feature row's own cost is measured
    /// against the engine underneath it, and reporting a baseline-relative figure beside it
    /// would report the engine's growth again as every feature's — the thing
    /// [`diff`] exists to avoid.
    pub before_layers: Option<u64>,
    /// This branch's layers' share, on the same terms.
    pub after_layers: Option<u64>,
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

/// Every row whose *own cost* differs between `base` and `head`, plus the rows only one
/// has.
///
/// Two choices here, and both are about what a reader should be able to conclude from a
/// line in this table.
///
/// Costs rather than absolute sizes: the two reports are built with the same toolchain in
/// the same job today, so the absolute numbers agree — but a rustc bump or a change to the
/// panic handler moves every one of them while changing nobody's incremental cost, and a
/// diff that reported all of that as "changed" is a diff people stop reading.
///
/// Each row against the base it declares, rather than every row against `baseline`: a
/// feature row's cost is what the feature added, not what the feature added plus whatever
/// the engine underneath it did. Diffing them all against `baseline` means a hundred bytes
/// of engine growth is reported once as the engine's and again as every feature's, which
/// contradicts the `measured_against` relationship the report itself prints and buries the
/// one row that actually changed.
#[must_use]
pub fn diff(base: &SizeReport, head: &SizeReport) -> Vec<RowDiff> {
    // `increment_of` is `None` for a row that is its own base — `baseline` — where the
    // delta against itself is the honest answer.
    let cost = |report: &SizeReport, name: &str| {
        report.increment_of(name).or_else(|| report.delta_of(name))
    };
    let gated = |report: &SizeReport, name: &str| report.row(name).is_some_and(|row| row.gated);
    let layers = |report: &SizeReport, name: &str| {
        gated(report, name)
            .then(|| report.layers_flash_of(name))
            .flatten()
    };
    let probe = |report: &SizeReport, name: &str| {
        gated(report, name)
            .then(|| report.probe_delta_of(name))
            .flatten()
    };

    let mut diffs = Vec::new();

    for row in head.rows() {
        let after = cost(head, &row.name);
        let after_layers = layers(head, &row.name);
        let after_probe = probe(head, &row.name);
        let known = base.row(&row.name).is_some();
        let before = known.then(|| cost(base, &row.name)).flatten();
        let before_layers = known.then(|| layers(base, &row.name)).flatten();
        let before_probe = known.then(|| probe(base, &row.name)).flatten();
        if known && before == after && before_layers == after_layers && before_probe == after_probe
        {
            continue;
        }
        diffs.push(RowDiff {
            name: row.name.clone(),
            before,
            after,
            before_probe,
            after_probe,
            before_layers,
            after_layers,
        });
    }

    for row in base.rows() {
        if head.row(&row.name).is_none() {
            diffs.push(RowDiff {
                name: row.name.clone(),
                before: cost(base, &row.name),
                after: None,
                before_probe: probe(base, &row.name),
                after_probe: None,
                before_layers: layers(base, &row.name),
                after_layers: None,
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
    match (base.kernel_state(), head.kernel_state()) {
        (Some(before), Some(after)) if before == after => None,
        (Some(before), Some(after)) => Some(format!(
            "kernel state {} B across {} type(s) -> {} B across {} type(s)",
            before.total,
            before.types.len(),
            after.total,
            after.types.len(),
        )),
        // The base branch's registry cannot be read from here, so silence would be a claim
        // that it did not change. It says so instead.
        _ => Some(
            "kernel state: not compared; the base branch's registry cannot be read by this build, and the const assertion in waymaker_core::budget is what gates it"
                .to_owned(),
        ),
    }
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
        // The split, where there is a gated figure. `flash` above is the whole image;
        // `probe` is what the gate subtracts and `layers` is what design document §04's
        // budget is stated over. Both are printed, because a row where the two moved by
        // the same amount reads as no change in the gated number and is not one.
        let split = match (
            entry.before_probe,
            entry.after_probe,
            entry.before_layers,
            entry.after_layers,
        ) {
            (Some(probe_before), Some(probe_after), Some(before), Some(after)) => format!(
                ", probe {probe_before} -> {probe_after} ({}), layers {before} -> {after} ({})",
                signed(probe_before, probe_after),
                signed(before, after),
            ),
            _ => String::new(),
        };
        table.push(format!("  {:<width$}  {detail}{split}\n", entry.name));
    }
    table.concat()
}

/// How a figure moved, as `+40` or `-8`.
fn signed(before: u64, after: u64) -> String {
    format!(
        "{}{}",
        if after >= before { "+" } else { "-" },
        after.abs_diff(before)
    )
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
    measure_into(root, &root.join(BUILD_DIR), KernelState::measured())
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
pub fn measure_into(
    root: &Path,
    build_dir: &Path,
    kernel_state: Option<KernelState>,
) -> Result<SizeReport, SizeError> {
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
        let machine = elf::machine(&bytes)
            .map_err(|err| SizeError::new(format!("could not read {}: {err}", image.display())))?;
        if machine != elf::EM_ARM {
            return Err(SizeError::new(format!(
                "{} is for machine {machine:#x}, not ARM ({:#x}); the budgets in design document \u{a7}04 are stated for {FIRMWARE_TARGET}, and a host image parses cleanly and measures plausibly",
                image.display(),
                elf::EM_ARM
            )));
        }
        let sections = elf::sections(&bytes)
            .map_err(|err| SizeError::new(format!("could not read {}: {err}", image.display())))?;
        check_symbols_are_not_measured(&sections).map_err(|err| {
            SizeError::new(format!("{} cannot be attributed: {err}", image.display()))
        })?;
        let symbols = elf::symbols(&bytes).map_err(|err| {
            SizeError::new(format!(
                "could not read the symbols of {}: {err}",
                image.display()
            ))
        })?;
        rows.push(Row {
            name: variant.name,
            features: variant.features,
            measured_against: variant.measured_against,
            sizes: SectionSizes::of(&sections),
            probe_flash: attributed_flash(&sections, &symbols, &probe_crate_name()),
            gated: variant.gated,
        });
    }

    Ok(SizeReport::new(rows, kernel_state))
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
            // The gated profile strips symbols, and the gate needs them: the code-flash
            // figure is the image delta less what the symbol table attributes to the
            // probe. Stripping removes only unallocated sections, so it moves no byte this
            // gate measures — which `check_symbols_are_not_measured` asks of every image
            // rather than assuming, because the whole point is that the sizes and the
            // attribution are readings of one image.
            "--config",
            STRIP_NOTHING,
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
    violations.extend(check_probe_mirrors(graph, manifest));

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

/// The probe feature that mirrors `feature` of `layer`.
///
/// Derived from both names rather than taken from a table, so a feature added to a layer
/// has one place to be mirrored and no row to remember. The layer's own prefix is dropped
/// — every layer carries it — and what is left names the layer, so two layers declaring
/// one feature name mirror it under two probe features.
#[must_use]
fn mirror_feature(layer: &str, feature: &str) -> String {
    format!(
        "{}-{feature}",
        layer.strip_prefix("waymaker-").unwrap_or(layer)
    )
}

/// Rule: the probe mirrors every layer feature, so the row for it reaches the code.
///
/// `--features waymaker-embassy/postcard` enables the layer's feature and defines no `cfg`
/// in the probe, so the probe cannot write a call the feature turns on: the row links the
/// codec and reaches none of it, and reports the delta of an image nobody exercised. A
/// mirror feature of the probe's own is what the probe can `#[cfg]` on.
///
/// Derived rather than tabulated, in both halves: [`mirror_feature`] names the mirror, and
/// a layer feature that has none fails here rather than producing a quiet row of zero.
#[must_use]
fn check_probe_mirrors(graph: &PackageGraph, manifest: Option<&str>) -> Vec<Violation> {
    let Some(probe) = graph.find(PROBE_PACKAGE) else {
        // Already reported by `check_size_probe`.
        return Vec::new();
    };
    let parsed = manifest.and_then(|manifest| manifest.parse::<toml::Table>().ok());
    let features = parsed
        .as_ref()
        .and_then(|parsed| parsed.get("features"))
        .and_then(toml::Value::as_table);

    let mut violations = Vec::new();
    for spec in policy::LAYERS {
        let Some(package) = graph.find(spec.name) else {
            continue;
        };
        for feature in &package.features {
            if feature == "default" {
                continue;
            }
            let mirror = mirror_feature(spec.name, feature);
            let selector = format!("{}/{feature}", spec.name);
            if !probe.features.contains(&mirror) {
                violations.push(Violation::new(
                    "size-probe",
                    PROBE_PACKAGE,
                    format!(
                        "does not declare `{mirror}`, so the `{selector}` row links that \
                         feature and can reach none of it: a probe cannot `#[cfg]` on a \
                         feature of another crate"
                    ),
                ));
                continue;
            }
            let enabled: Vec<&str> = features
                .and_then(|table| table.get(&mirror))
                .and_then(toml::Value::as_array)
                .map(|entries| entries.iter().filter_map(toml::Value::as_str).collect())
                .unwrap_or_default();
            if !enabled.contains(&selector.as_str()) {
                violations.push(Violation::new(
                    "size-probe",
                    PROBE_PACKAGE,
                    format!(
                        "its `{mirror}` feature does not enable `{selector}`, so the row \
                         named after that feature measures an image without it"
                    ),
                ));
            }
        }
    }
    violations
}

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

/// The kind of block a function is being declared inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    /// A `trait` declaration: its methods are as callable as the trait is.
    Trait,
    /// An `impl <Trait> for <Type>`: its methods are as callable as the trait is.
    TraitImpl,
    /// Anything else — an inherent `impl`, a module, a function body.
    Other,
}

/// Every function of the layers that a caller outside the crate can reach.
///
/// `pub fn` is not the whole answer, and assuming it was left a hole big enough to drive a
/// storage backend through: a method of a `trait`, and a method of an `impl Trait for
/// Type`, carry no `pub` at all — the trait's visibility is what makes them callable. A
/// layer could implement the whole storage protocol, have every byte of it dead-stripped,
/// and this rule would have said nothing.
///
/// Scanned rather than parsed, like every other rule here, and `#[cfg(test)]` modules are
/// skipped by brace depth: a test helper is not code the firmware links.
#[must_use]
pub fn public_functions(sources: &[LayerSource]) -> Vec<PublicFunction> {
    let mut found = Vec::new();
    for source in sources {
        let mut depth: i32 = 0;
        let mut test_module: Option<i32> = None;
        let mut pending_test_attribute = false;
        // `(depth the block's body starts at, what kind of block it is)`.
        let mut blocks: Vec<(i32, Block)> = Vec::new();
        // A `trait` or `impl` header can span lines — a `where` clause puts the opening
        // brace on a line of its own — so the kind is remembered from the line that
        // declares it until the line that opens it.
        let mut pending: Option<Block> = None;

        for line in source.contents.lines() {
            let trimmed = line.trim();
            let opens = i32::try_from(trimmed.matches('{').count()).unwrap_or(0);
            let closes = i32::try_from(trimmed.matches('}').count()).unwrap_or(0);

            if !trimmed.starts_with("//") {
                if trimmed.contains("#[cfg(test)]") {
                    pending_test_attribute = true;
                }
                if pending_test_attribute && opens > 0 {
                    test_module = Some(depth);
                    pending_test_attribute = false;
                }
            }

            if test_module.is_none() && !trimmed.starts_with("//") {
                // A method counts only as a direct child of the block that makes it
                // callable. Without that, a nested `fn` inside a trait method's body would
                // be required of the probe, which cannot call it.
                let enclosing = blocks
                    .last()
                    .filter(|(body_depth, _)| *body_depth == depth)
                    .map_or(Block::Other, |(_, kind)| *kind);

                // Any leading attributes are set aside first: every classifier below reads
                // the start of the line, and `#[rustfmt::skip] impl X { pub fn y() {} }` is
                // one line that survives `cargo fmt`.
                let classified = without_leading_attributes(trimmed);
                if let Some(name) = function_name(classified) {
                    // A block header and one of its members on the same line —
                    // `#[rustfmt::skip] impl Ctx { pub fn seal_now(..) { .. } }`. Neither
                    // test below sees it: the line does not begin with `pub `, and the
                    // block that makes the method callable is declared on this very line
                    // rather than above it. Review of issue #35 landed exactly that and
                    // watched nine surface pins and `size-probe-reach` stay green, so it is
                    // closed in the reader they share rather than in one rule.
                    let declared_here = declaration_kind(classified);
                    let inline = declared_here.is_some() && opens > 0;
                    // The member's *own* prefix, which is what follows the block's opening
                    // brace — not the whole line before the `fn` keyword. Testing that the
                    // line ended in `pub` read `impl Bank { pub const fn raw()` as private,
                    // because the prefix ends in the modifier; the same for `pub async`,
                    // `pub unsafe` and `pub extern "C"`. Codex round 1 found it.
                    let marked_public = classified.split_once(" fn ").is_some_and(|(before, _)| {
                        declares_public(before.rsplit('{').next().unwrap_or("").trim())
                    });
                    let callable = declares_public(classified)
                        || matches!(enclosing, Block::Trait | Block::TraitImpl)
                        || (inline
                            && (marked_public
                                || matches!(declared_here, Some(Block::Trait | Block::TraitImpl))));
                    if callable {
                        found.push(PublicFunction {
                            crate_name: source.crate_name.clone(),
                            path: source.path.clone(),
                            name: name.to_owned(),
                        });
                    }
                }

                if let Some(kind) = declaration_kind(classified) {
                    pending = Some(kind);
                }
                if opens > 0 {
                    blocks.push((
                        depth.saturating_add(1),
                        pending.take().unwrap_or(Block::Other),
                    ));
                } else if trimmed.ends_with(';') {
                    // A declaration that ended without a body takes its kind with it.
                    pending = None;
                }
            }

            depth += opens;
            depth -= closes;
            if test_module.is_some_and(|opened| depth <= opened) {
                test_module = None;
            }
            while blocks
                .last()
                .is_some_and(|(body_depth, _)| depth < *body_depth)
            {
                blocks.pop();
            }
        }
    }
    found
}

/// What a line declares, if it begins a `trait` or an `impl`.
///
/// Returned from the line that *declares* the block rather than the one that opens it,
/// because those are not always the same line: a `where` clause puts the opening brace on
/// its own, and classifying that bare `{` would read every method of the impl as an
/// ordinary private one and quietly drop them from the reach rule.
///
/// `None` for every other line, so that a block nobody declared — a `mod`, a function body
/// — is pushed as [`Block::Other`] and the stack still mirrors the brace depth.
fn declaration_kind(line: &str) -> Option<Block> {
    let declaration = line.strip_prefix("pub ").unwrap_or(line);
    let declaration = declaration
        .split_once("(crate)")
        .map_or(declaration, |(_, rest)| rest.trim_start());
    if declaration.starts_with("trait ") {
        return Some(Block::Trait);
    }
    if declaration.starts_with("impl") {
        // `impl Storage for Bank` implements a trait; `impl Bank` does not. Only the first
        // makes its unmarked methods callable from outside.
        return Some(if declaration.contains(" for ") {
            Block::TraitImpl
        } else {
            Block::Other
        });
    }
    None
}

/// `line` with any leading attributes removed.
///
/// `#[rustfmt::skip] impl Bank { pub fn raw() {} }` is one line, survives `cargo fmt`, and
/// every classifier here reads the start of a line — so an attribute in front of the item
/// hid the `impl` from `declaration_kind` and the `pub` from the visibility test. Codex
/// round 3 found it, in the same one-line form round 1's finding was about.
///
/// Brackets are matched rather than counted to the first `]`, so `#[cfg(all(a, b))]` is one
/// attribute — and a bracket inside an *ordinary* string literal is not a bracket, so
/// `#[expect(lint, reason = "]")]` is one too. Codex round 4 found the version that read
/// every `]` as syntax and left the classifier standing on `")]` rather than on the item.
///
/// Two literal forms are outside it, and both leave the item **unclassified** rather than
/// reporting a private function as public — the direction that under-reports. A `']'`
/// *character* literal, because telling one from the lifetime in
/// `#[foo(bar = "x")] impl<'a> …` needs a tokeniser rather than a scan. And a *raw* string,
/// because `"` both opens and closes here: `#[doc = r#"a"]b"#]` is read as ending at the
/// quote inside it. Codex round 6 found that one, and it is issue #108.
///
/// Three rounds have now landed on this function, each closing one construct and leaving
/// the next. What closes the class is lexing the attribute rather than scanning it, which
/// is #108's own point; this reads what a reviewer can check by eye.
pub(crate) fn without_leading_attributes(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some(after) = rest.strip_prefix("#[") {
        let mut depth = 1_u32;
        let mut quoted = false;
        let mut escaped = false;
        let mut end = None;
        for (index, character) in after.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match character {
                '\\' if quoted => escaped = true,
                '"' => quoted = !quoted,
                '[' if !quoted => depth = depth.saturating_add(1),
                ']' if !quoted => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(index.saturating_add(1));
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end.and_then(|end| after.get(end..)) else {
            return rest;
        };
        rest = end.trim_start();
    }
    rest
}

/// Whether a declaration's prefix marks it `pub`, and not `pub(crate)`.
///
/// The same visibility the non-inline path reads with `starts_with("pub ")`, split out so
/// that the two agree: a `pub(crate)` member is not public in either, and a modifier between
/// the visibility and the keyword changes neither.
fn declares_public(prefix: &str) -> bool {
    prefix == "pub" || prefix.starts_with("pub ")
}

/// The name declared by a function signature, if the line declares one.
fn function_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("pub ").unwrap_or(line);
    // `const fn`, `async fn`, `unsafe fn`, `extern "C" fn`, and any combination: skip
    // everything up to the `fn` keyword, but only where `fn` starts the token.
    let rest = if let Some(rest) = rest.strip_prefix("fn ") {
        rest
    } else {
        let (before, after) = rest.split_once(" fn ")?;
        // Guards against `let f = |x| ...; // returns fn foo` style lines: everything
        // before the keyword must look like modifiers rather than an expression.
        if before.contains('(') || before.contains('=') {
            return None;
        }
        after
    };
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
///
/// # A floor, not a proof
///
/// This is a scanner, so it establishes that every public function's *name* appears in the
/// probe in call position — not that each was called. Two layers declaring the same name
/// are satisfied by one call to either, and a call behind a generic or a trait object is
/// credited to the name written rather than to the body that runs. Deciding those needs a
/// call graph. What it does catch, mechanically and every time, is the case that arrives
/// silently: a layer gains a function and nobody wires the probe up to it.
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

/// Whether `source` calls `name`, rather than merely containing the word.
///
/// Two things this deliberately does not credit, both of which it once did:
///
/// * anything in a comment. `waymaker-core` declares `TypeSize::of`, and the probe's own
///   documentation contains the English word "of" five times, so the rule reported the
///   function as reached while the linker discarded it. Prose is not a call.
/// * a bare word. A name is credited only where it stands in call or path position —
///   preceded by `::` or `.`, or followed by `(`, `::<`, or `)`. A local variable that
///   happens to share a function's name is not a call either.
///
/// # What it still cannot tell
///
/// Which function was called, when two layers declare the same name. `waymaker-core` and
/// `waymaker-flash` both being free to declare `new`, one call satisfies the rule for
/// both, and the uncalled one is still dead-stripped. Deciding that needs name resolution
/// — a call graph, not a scanner — so this rule is a floor: it catches a public function
/// nobody wired up, which is the common case and the one that arrives silently. It is not
/// a proof that every function is reached, and the module documentation says so.
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
            .unwrap_or_default();

        let whole_word = before
            .is_none_or(|character| !character.is_alphanumeric() && character != '_')
            && after
                .chars()
                .next()
                .is_none_or(|character| !character.is_alphanumeric() && character != '_');

        // A path segment (`budget::of`, `bank.seal`) or a call (`of(`, `of::<u32>(`).
        let in_path = rest
            .get(..at)
            .is_some_and(|text| text.ends_with("::") || text.ends_with('.'));
        let called = {
            let next = after.trim_start();
            next.starts_with('(') || next.starts_with("::<") || next.starts_with("::")
        };

        if whole_word && (in_path || called) {
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
    // No kernel state. `KernelState::measured` reads the `waymaker-core` that *this*
    // `xtask` was compiled against, which is the head's — so recording it for the base too
    // would put the same registry on both sides of the diff and make a pull request that
    // changes the registry look like one that did not. Unknown is the truth here, and the
    // diff says so.
    // Named for the commit rather than for this process: two runs comparing *different*
    // bases must not share a directory — cargo's lock serialises their builds but is
    // released before the artifact is read, so one could measure the other's image and
    // report a diff against the wrong commit. Two runs comparing the *same* base share it
    // safely, because the same input produces the same artifact, and that is also what lets
    // a build cache survive from one run to the next.
    let measured = measure_into(&worktree, &baseline_build_dir(root, &commit), None);
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

/// `git`, run against `root` and against nothing the environment says.
///
/// A git hook exports `GIT_DIR`, `GIT_INDEX_FILE` and friends, pointing at the repository
/// being committed to. Inherited here, `git worktree add` writes into *that* repository's
/// index rather than into the checkout this gate is measuring — which is a measurement of
/// a tree nobody asked about, taken silently. `current_dir` alone does not stop it: the
/// environment outranks the working directory.
fn git(root: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command.current_dir(root);
    for inherited in [
        "GIT_DIR",
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
        "GIT_PREFIX",
    ] {
        command.env_remove(inherited);
    }
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
    fn a_layer_feature_the_probe_mirrors_is_selected_by_the_probes_own_feature() {
        let mut graph = workspace(&[], &["postcard"]);
        graph = PackageGraph::new(
            graph
                .packages()
                .iter()
                .map(|package| {
                    if package.name == PROBE_PACKAGE {
                        Package::new(PROBE_PACKAGE).with_features(&[
                            "probe",
                            "engine",
                            "facade",
                            "embassy-postcard",
                        ])
                    } else {
                        package.clone()
                    }
                })
                .collect(),
        );

        let variants = matrix(&graph);
        let row = find(&variants, "waymaker-embassy/postcard");
        assert_eq!(
            row.features,
            [PROBE_FEATURE, FACADE_FEATURE, "embassy-postcard"],
            "a `<layer>/<feature>` selector enables the layer feature but defines no cfg in \
             the probe, so the row links the code and reaches none of it"
        );
    }

    #[test]
    fn the_probe_feature_that_mirrors_a_layer_feature_is_derived_from_both_names() {
        assert_eq!(
            mirror_feature("waymaker-embassy", "postcard"),
            "embassy-postcard"
        );
        assert_eq!(mirror_feature("waymaker-core", "serde"), "core-serde");
    }

    #[test]
    fn a_layer_feature_the_probe_does_not_mirror_is_reported() {
        let violations = check_probe_mirrors(
            &workspace(&[], &["postcard"]),
            Some(&tests_support::clean_probe_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.rule == "size-probe"
                    && violation.detail.contains("embassy-postcard")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_mirror_that_does_not_enable_its_layer_feature_is_reported() {
        let manifest = format!(
            "{}embassy-postcard = [\"facade\"]\n",
            tests_support::clean_probe_manifest()
        );
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash"),
            Package::new("waymaker-embassy").with_features(&["postcard"]),
            Package::new(PROBE_PACKAGE).with_features(&[
                "probe",
                "engine",
                "facade",
                "embassy-postcard",
            ]),
        ]);

        let violations = check_probe_mirrors(&graph, Some(&manifest));

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("waymaker-embassy/postcard")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_mirrored_layer_feature_is_not_reported() {
        let manifest = format!(
            "{}embassy-postcard = [\"facade\", \"waymaker-embassy/postcard\"]\n",
            tests_support::clean_probe_manifest()
        );
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash"),
            Package::new("waymaker-embassy").with_features(&["postcard"]),
            Package::new(PROBE_PACKAGE).with_features(&[
                "probe",
                "engine",
                "facade",
                "embassy-postcard",
            ]),
        ]);

        assert!(check_probe_mirrors(&graph, Some(&manifest)).is_empty());
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

    /// What the symbol table attributes to the probe in the baseline image.
    ///
    /// Non-zero because a baseline image always holds the probe's own entry point, and a
    /// row where nothing is attributed to the probe is a row whose symbol table was not
    /// read — which the gate refuses rather than reads as "the probe cost nothing".
    const BASELINE_PROBE_FLASH: u64 = 10;

    fn baseline_row() -> Row {
        Row::new(
            BASELINE_ROW,
            &[PROBE_FEATURE],
            BASELINE_ROW,
            baseline_sizes(),
            BASELINE_PROBE_FLASH,
            false,
        )
    }

    fn default_row(flash_over_baseline: u64, ram_over_baseline: u64) -> Row {
        default_row_with_probe(flash_over_baseline, ram_over_baseline, BASELINE_PROBE_FLASH)
    }

    /// A `default` row whose symbol table attributes `probe_flash` bytes to the probe.
    fn default_row_with_probe(
        flash_over_baseline: u64,
        ram_over_baseline: u64,
        probe_flash: u64,
    ) -> Row {
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
            probe_flash,
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

    /// A stored section, for the attribution tests.
    fn stored(name: &str) -> crate::elf::Section {
        crate::elf::Section {
            name: name.to_owned(),
            size: 0,
            kind: 1,
            flags: crate::elf::SHF_ALLOC,
        }
    }

    /// A section that is reserved in RAM and stored nowhere.
    fn reserved(name: &str) -> crate::elf::Section {
        crate::elf::Section {
            name: name.to_owned(),
            size: 0,
            kind: crate::elf::SHT_NOBITS,
            flags: crate::elf::SHF_ALLOC | crate::elf::SHF_WRITE,
        }
    }

    fn symbol(name: &str, address: u64, size: u64, section_index: u16) -> crate::elf::Symbol {
        crate::elf::Symbol {
            name: name.to_owned(),
            address,
            size,
            section_index,
        }
    }

    #[test]
    fn the_defining_crate_of_a_generic_is_the_crate_that_declares_it() {
        // `waymaker_flash::frame::encode_with::<Catalogued>`, instantiated by the probe,
        // as `llvm-nm` prints it from the linked image. The probe's name is in the symbol
        // too, at the end, and charging the byte count to it would hand every generic in
        // the engine back to the row being corrected.
        let mangled = concat!(
            "_RINvNtCscrinc51sKky_14waymaker_flash5frame11encode_with",
            "NtNtB4_9integrity10CataloguedECs2RP2q5hjpwT_19waymaker_size_probe",
        );
        assert_eq!(defining_crate(mangled), Some("waymaker_flash"));
    }

    #[test]
    fn a_traits_own_provided_method_is_charged_to_nobody_rather_than_to_the_impl() {
        // `<waymaker_size_probe::ProbeCheck as waymaker_flash::IntegrityCheck>::frame_check`
        // for a method the trait provides: the body is `waymaker-flash`'s, and `Y` names
        // the self type first. Reading the first crate root would subtract a layer's bytes
        // from the budget under the probe's name, which is the one direction that loosens
        // it. Refused instead, so the bytes stay with the layers.
        let mangled = concat!(
            "_RNvYNtCsebY0CHkT2OO_19waymaker_size_probe10ProbeCheck",
            "NtCsfjwSggzmCUk_14waymaker_flash14IntegrityCheck11frame_checkB4_",
        );
        assert_eq!(defining_crate(mangled), None);

        // The mirror, which the linked image really carries:
        // `<waymaker_flash::storage::Geometry as core::cmp::PartialEq>::ne`. `core` wrote
        // the body, and refusing charges it to the layers, which is where it already was.
        let borrowed = concat!(
            "_RNvYNtNtCscrinc51sKky_14waymaker_flash7storage8Geometry",
            "NtNtCsaKixe4yIu8C_4core3cmp9PartialEq2neCs2RP2q5hjpwT_19waymaker_size_probe",
        );
        assert_eq!(defining_crate(borrowed), None);
    }

    #[test]
    fn an_impl_of_a_layer_trait_for_a_probe_type_is_still_the_probes() {
        // `X` is the other trait-impl production and it reads the right way round: the
        // impl path comes first, so a method body written in the probe is the probe's.
        // Refusing this one too would charge the probe's own code to the layers on every
        // `impl StableStorage for ProbeMedia` method there is.
        let mangled = concat!(
            "_RNvXs0_Cs2RP2q5hjpwT_19waymaker_size_probeNtB5_10ProbeMedia",
            "NtNtCscrinc51sKky_14waymaker_flash7storage13StableStorage4read",
        );
        assert_eq!(defining_crate(mangled), Some("waymaker_size_probe"));
    }

    #[test]
    fn the_defining_crate_of_a_probe_function_is_the_probe() {
        assert_eq!(
            defining_crate("_RNvCs2RP2q5hjpwT_19waymaker_size_probe6engine"),
            Some("waymaker_size_probe")
        );
    }

    #[test]
    fn a_legacy_mangled_symbol_names_its_crate_too() {
        assert_eq!(
            defining_crate("_ZN14waymaker_flash5frame6encode17h0123456789abcdefE"),
            Some("waymaker_flash")
        );
    }

    #[test]
    fn a_symbol_no_rust_compiler_mangled_belongs_to_no_crate() {
        assert_eq!(defining_crate("__aeabi_memcpy"), None);
        assert_eq!(defining_crate("memcpy"), None);
    }

    #[test]
    fn a_crate_component_that_runs_off_the_end_of_the_symbol_names_nothing() {
        // A length longer than the characters that follow it. Reading it anyway would
        // attribute bytes to a crate name that is a truncation of a real one.
        assert_eq!(defining_crate("_RNvCs1_99waymaker"), None);
    }

    #[test]
    fn attribution_counts_only_symbols_in_sections_that_cost_flash() {
        let sections = vec![stored(".null"), stored(".text"), reserved(".bss")];
        let symbols = vec![
            symbol("_RNvCs1_19waymaker_size_probe6engine", 0x1000, 100, 1),
            // In `.bss`: RAM, not flash, so it is not part of the figure being corrected.
            symbol("_RNvCs1_19waymaker_size_probe5state", 0x9000, 40, 2),
            symbol("_RNvCs1_14waymaker_flash5frame", 0x2000, 7, 1),
        ];
        assert_eq!(
            attributed_flash(&sections, &symbols, "waymaker_size_probe"),
            100
        );
    }

    #[test]
    fn a_symbol_pointing_at_no_section_is_attributed_to_nothing() {
        let sections = vec![stored(".null"), stored(".text")];
        // `SHN_UNDEF` and an index past the table: an undefined symbol and a corrupt one.
        let symbols = vec![
            symbol("_RNvCs1_19waymaker_size_probe6engine", 0x1000, 100, 0),
            symbol("_RNvCs1_19waymaker_size_probe6second", 0x2000, 100, 9),
        ];
        assert_eq!(
            attributed_flash(&sections, &symbols, "waymaker_size_probe"),
            0
        );
    }

    #[test]
    fn two_names_on_one_body_are_the_bytes_of_one_body() {
        // `lto = "fat"` and `opt-level = "z"` fold identical function bodies and can leave
        // several mangled names on the survivor. This figure is *subtracted* from the
        // budget, so counting the bytes once per name loosens it.
        let sections = vec![stored(".null"), stored(".text")];
        let symbols = vec![
            symbol("_RNvCs1_19waymaker_size_probe6engine", 0x1000, 100, 1),
            symbol("_RNvCs1_19waymaker_size_probe4twin", 0x1000, 100, 1),
            // Partly overlapping rather than identical, which is the same question asked
            // of a linker that folded a body into the tail of another.
            symbol("_RNvCs1_19waymaker_size_probe5third", 0x1040, 100, 1),
        ];
        assert_eq!(
            attributed_flash(&sections, &symbols, "waymaker_size_probe"),
            0x1040 + 100 - 0x1000
        );
    }

    #[test]
    fn a_body_two_crates_both_name_is_credited_to_neither() {
        // The mirror of the fold above: a body the linker shared between the probe and a
        // layer is not the probe's to take off the layers' bill. An unattributable name
        // counts as another crate's for the same reason.
        let sections = vec![stored(".null"), stored(".text")];
        let symbols = vec![
            symbol("_RNvCs1_19waymaker_size_probe6engine", 0x1000, 100, 1),
            symbol("_RNvCs1_14waymaker_flash5frame4body", 0x1000, 100, 1),
            symbol("_RNvCs1_19waymaker_size_probe4mine", 0x2000, 40, 1),
            symbol("__aeabi_memcpy", 0x2000, 40, 1),
        ];
        assert_eq!(
            attributed_flash(&sections, &symbols, "waymaker_size_probe"),
            0
        );
    }

    #[test]
    fn one_crates_bodies_in_two_sections_are_both_counted() {
        // Ranges are compared within a section: two sections address independently, so an
        // address in `.text` says nothing about the same address in `.rodata`.
        let sections = vec![stored(".null"), stored(".text"), stored(".rodata")];
        let symbols = vec![
            symbol("_RNvCs1_19waymaker_size_probe6engine", 0x1000, 100, 1),
            symbol("_RNvCs1_19waymaker_size_probe5table", 0x1000, 40, 2),
        ];
        assert_eq!(
            attributed_flash(&sections, &symbols, "waymaker_size_probe"),
            140
        );
    }

    #[test]
    fn the_gated_figure_is_the_delta_less_the_probes_own_growth() {
        // 1 000 B more image than the baseline, 400 B of which the symbol table names as
        // the probe's own arithmetic. Design document \u{a7}04's budget is for the layers.
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_000, 0, BASELINE_PROBE_FLASH + 400),
            ],
            KernelState::measured(),
        );
        assert_eq!(
            report.delta_of(DEFAULT_ROW).map(|sizes| sizes.flash),
            Some(1_000)
        );
        assert_eq!(report.layers_flash_of(DEFAULT_ROW), Some(600));
    }

    #[test]
    fn the_budget_is_held_against_the_layers_rather_than_against_the_image() {
        // Over budget as an image, inside it as the layers: the probe's own growth is what
        // the difference is, and charging it to the kernel is the whole of issue #72.
        let over_as_an_image = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(
                    INCREMENTAL_CODE_FLASH_BUDGET_BYTES + 100,
                    0,
                    BASELINE_PROBE_FLASH + 200,
                ),
            ],
            KernelState::measured(),
        );
        assert!(
            over_as_an_image.shortfalls().is_empty(),
            "{:?}",
            over_as_an_image.shortfalls()
        );

        let over_as_the_layers = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(
                    INCREMENTAL_CODE_FLASH_BUDGET_BYTES + 100,
                    0,
                    BASELINE_PROBE_FLASH + 50,
                ),
            ],
            KernelState::measured(),
        );
        assert!(
            rendered(&over_as_the_layers.shortfalls()).contains("incremental code flash"),
            "{:?}",
            over_as_the_layers.shortfalls()
        );
    }

    #[test]
    fn a_row_with_nothing_attributed_to_the_probe_is_not_a_measurement() {
        // Every image the matrix links is the probe, so a zero here is a symbol table that
        // was stripped, not a probe that cost nothing — and reading it as zero silently
        // restores the number issue #72 exists to correct.
        let report = SizeReport::new(
            vec![baseline_row(), default_row_with_probe(1_000, 0, 0)],
            KernelState::measured(),
        );
        assert!(
            rendered(&report.shortfalls()).contains("nothing was measured"),
            "{:?}",
            report.shortfalls()
        );
    }

    #[test]
    fn an_ungated_row_with_nothing_attributed_to_the_probe_is_not_a_measurement_either() {
        // The report states the split for every row, so a row whose symbol table was not
        // read misstates one — which is worth as much as a wrong gate to whoever reads it.
        let default = default_row(20, 0);
        let mut feature = feature_row("waymaker-core/serde", &default, 8);
        feature.probe_flash = 0;
        let report = SizeReport::new(
            vec![baseline_row(), default, feature],
            KernelState::measured(),
        );
        assert!(
            rendered(&report.shortfalls()).contains("waymaker-core/serde"),
            "{:?}",
            report.shortfalls()
        );
    }

    #[test]
    fn a_probe_share_larger_than_the_image_is_not_a_measurement() {
        let report = SizeReport::new(
            vec![baseline_row(), default_row_with_probe(10, 0, 1_000_000)],
            KernelState::measured(),
        );
        assert!(
            rendered(&report.shortfalls()).contains("nothing was measured"),
            "{:?}",
            report.shortfalls()
        );
    }

    #[test]
    fn the_report_states_the_split_on_every_run() {
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_000, 0, BASELINE_PROBE_FLASH + 400),
            ],
            KernelState::measured(),
        );
        let rendered = report.render();
        assert!(rendered.contains("probe"), "{rendered}");
        assert!(rendered.contains("layers"), "{rendered}");
        // The corrected figure and the image delta both, so a reader can see the split
        // rather than take the gate's word for it.
        assert!(rendered.contains("600"), "{rendered}");
        assert!(rendered.contains("1000"), "{rendered}");
    }

    #[test]
    fn the_json_report_carries_the_probe_share_and_the_layers_figure() {
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_000, 0, BASELINE_PROBE_FLASH + 400),
            ],
            KernelState::measured(),
        );
        let json: serde_json::Value =
            serde_json::from_str(&report.to_json()).expect("the report is JSON");
        let row = json["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["name"] == DEFAULT_ROW)
            .expect("a default row")
            .clone();
        assert_eq!(row["probe_flash"], serde_json::json!(410));
        assert_eq!(row["layers_flash"], serde_json::json!(600));
        assert_eq!(
            SizeReport::from_json(&report.to_json()).expect("readable"),
            report
        );
    }

    #[test]
    fn a_row_missing_its_probe_share_is_rejected_rather_than_read_as_zero() {
        let report = SizeReport::new(
            vec![baseline_row(), default_row(1_000, 0)],
            KernelState::measured(),
        );
        let json = report
            .to_json()
            .replace("\"probe_flash\"", "\"probe_flesh\"");
        assert!(SizeReport::from_json(&json).is_err());
    }

    #[test]
    fn a_symbol_table_that_costs_flash_is_not_one_stripping_can_remove() {
        // The gate reads symbols out of an image built without `strip`, and gates the
        // section sizes of that same image. That is only the same measurement while the
        // symbol table is unallocated, so the image is asked rather than assumed.
        let allocated_symbols = vec![
            crate::elf::Section {
                name: ".symtab".to_owned(),
                size: 400,
                kind: crate::elf::SHT_SYMTAB,
                flags: crate::elf::SHF_ALLOC,
            },
            stored(".text"),
        ];
        assert!(check_symbols_are_not_measured(&allocated_symbols).is_err());
    }

    #[test]
    fn an_image_with_no_symbol_table_cannot_be_attributed() {
        assert!(check_symbols_are_not_measured(&[stored(".text")]).is_err());
    }

    #[test]
    fn an_image_with_two_symbol_tables_cannot_be_attributed() {
        // Every one is read, so a second table attributes the same bytes twice — and the
        // gate subtracts what it attributes.
        let twice = vec![
            crate::elf::Section {
                name: ".symtab".to_owned(),
                size: 400,
                kind: crate::elf::SHT_SYMTAB,
                flags: 0,
            },
            crate::elf::Section {
                name: ".symtab.other".to_owned(),
                size: 400,
                kind: crate::elf::SHT_SYMTAB,
                flags: 0,
            },
            stored(".text"),
        ];
        assert!(check_symbols_are_not_measured(&twice).is_err());
    }

    #[test]
    fn a_diff_shows_a_probe_that_grew_beside_the_layers_that_did_not() {
        // The term the gate subtracts is the one a contributor moves most easily, so a
        // pull request that grows both by the same amount must not read as no change.
        let base = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_000, 0, BASELINE_PROBE_FLASH + 400),
            ],
            KernelState::measured(),
        );
        let head = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_200, 0, BASELINE_PROBE_FLASH + 600),
            ],
            KernelState::measured(),
        );
        let rendered = render_diff(&diff(&base, &head));
        assert!(rendered.contains("probe 400 -> 600 (+200)"), "{rendered}");
        assert!(rendered.contains("layers 600 -> 600 (+0)"), "{rendered}");
    }

    #[test]
    fn the_report_names_the_probe_bytes_both_terms_are_measured_against() {
        // Both columns are deltas against the baseline, and the baseline's own probe
        // symbols appear in no column. A reader checking the subtraction needs the number.
        let report = report(1_000, 0);
        assert!(
            report
                .render()
                .contains(&format!("own probe symbols are {BASELINE_PROBE_FLASH} B")),
            "{}",
            report.render()
        );
    }

    #[test]
    fn an_image_whose_symbol_table_costs_nothing_is_measurable() {
        let sections = vec![
            crate::elf::Section {
                name: ".symtab".to_owned(),
                size: 400,
                kind: crate::elf::SHT_SYMTAB,
                flags: 0,
            },
            stored(".text"),
        ];
        assert_eq!(check_symbols_are_not_measured(&sections), Ok(()));
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
                BASELINE_PROBE_FLASH,
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
            BASELINE_PROBE_FLASH,
            false,
        );
        let report = SizeReport::new(
            vec![big_baseline, default_row(64, 0)],
            KernelState::measured(),
        );
        let delta = report.delta_of(DEFAULT_ROW).expect("a default row");
        assert_eq!(delta.flash, 0, "a smaller image is not a negative cost");
        // And a saturated zero on a gated row is reported rather than passed: that row
        // links the kernel and the flash adapter, so a delta of nothing is a measurement
        // fault, not a free engine.
        assert!(
            rendered(&report.shortfalls()).contains("cannot cost nothing"),
            "{:?}",
            report.shortfalls()
        );
    }

    #[test]
    fn a_probe_that_shrank_is_layer_growth_rather_than_nothing() {
        // The layers' share is each image's non-probe bytes, subtracted. Taking the probe
        // *delta* off the image delta instead loses the sign when the probe shrinks, and
        // loses it in the direction that passes: this row is 12200 B larger with 200 B less
        // probe in it, which is 12400 B of layers and over the budget.
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(12_200, 0, BASELINE_PROBE_FLASH - 5),
            ],
            KernelState::measured(),
        );
        assert_eq!(report.layers_flash_of(DEFAULT_ROW), Some(12_205));
        assert_eq!(report.probe_delta_of(DEFAULT_ROW), Some(0));
    }

    #[test]
    fn a_gated_row_whose_probe_share_swallows_the_whole_delta_is_not_a_measurement() {
        // The per-image bound cannot see this: the probe share is well under the image it
        // was read from, and still leaves the layers nothing.
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_000, 0, BASELINE_PROBE_FLASH + 1_000),
            ],
            KernelState::measured(),
        );
        assert_eq!(report.layers_flash_of(DEFAULT_ROW), Some(0));
        assert!(
            rendered(&report.shortfalls()).contains("cannot cost nothing"),
            "{:?}",
            report.shortfalls()
        );
    }

    #[test]
    fn a_report_with_two_rows_of_one_name_is_rejected_rather_than_gated_on_the_first() {
        // Every figure in a report is looked up by name, so a second `default` row would
        // be gated on the first one's numbers whatever it held. `--report` reads a
        // document this process did not produce.
        let report = SizeReport::new(
            vec![baseline_row(), default_row(20, 0), default_row(20, 0)],
            KernelState::measured(),
        );
        let error = SizeReport::from_json(&report.to_json())
            .expect_err("two rows of one name is not a report");
        assert!(error.to_string().contains("two rows called"), "{error}");
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
            default.probe_flash,
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
    fn a_report_with_no_gated_default_row_does_not_get_to_gate_itself() {
        // `--report` gates a document this process did not produce. Letting the document's
        // own `gated` flags decide which rows are checked means a report with the flag
        // cleared, or with the row removed, gates nothing and exits zero.
        let ungated = Row::new(
            DEFAULT_ROW,
            &[PROBE_FEATURE, ENGINE_FEATURE],
            BASELINE_ROW,
            SectionSizes {
                flash: baseline_sizes().flash + INCREMENTAL_CODE_FLASH_BUDGET_BYTES + 1,
                ..baseline_sizes()
            },
            BASELINE_PROBE_FLASH,
            false,
        );
        let message = rendered(
            &SizeReport::new(vec![baseline_row(), ungated], KernelState::measured()).shortfalls(),
        );
        assert!(message.contains("no gated `default` row"), "{message}");

        let missing = SizeReport::new(vec![baseline_row()], KernelState::measured()).shortfalls();
        assert!(
            rendered(&missing).contains("no gated `default` row"),
            "{missing:?}"
        );
    }

    #[test]
    fn the_kernel_state_of_a_checkout_this_build_cannot_read_is_not_the_head_s() {
        // `KernelState::measured` reads the `waymaker-core` linked into this binary. For a
        // base-branch worktree that is the head's registry, so recording it would put the
        // same figure on both sides of the diff and make a change to the registry invisible.
        let base = SizeReport::new(vec![baseline_row(), default_row(0, 0)], None);
        let head = report(0, 0);

        assert!(base.kernel_state().is_none());
        assert!(base.shortfalls().iter().all(|shortfall| !matches!(
            shortfall,
            BudgetShortfall::Exceeded {
                budget: Budget::KernelState,
                ..
            }
        )));
        assert!(
            base.render().contains("kernel state: not read"),
            "{}",
            base.render()
        );

        let change = kernel_state_change(&base, &head).expect("an unknown side is not a match");
        assert!(change.contains("not compared"), "{change}");
    }

    #[test]
    fn a_report_with_an_unknown_kernel_state_survives_a_json_round_trip() {
        let original = SizeReport::new(vec![baseline_row(), default_row(0, 0)], None);
        let restored =
            SizeReport::from_json(&original.to_json()).expect("its own JSON is readable");
        assert_eq!(restored, original);
        assert!(restored.kernel_state().is_none());
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
            Some(KernelState {
                total: KERNEL_STATE_BUDGET_BYTES + 7,
                types: vec![("Cursor".to_owned(), KERNEL_STATE_BUDGET_BYTES + 7)],
            }),
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
    fn engine_growth_is_not_reported_again_as_every_features_growth() {
        // A feature row's cost is what the feature added, not that plus whatever the engine
        // underneath it did. Diffing every row against `baseline` reports one hundred bytes
        // of engine growth once as the engine's and again as each feature's, which buries
        // the row that actually changed.
        let small = default_row(20, 0);
        let base = SizeReport::new(
            vec![
                baseline_row(),
                small.clone(),
                feature_row("waymaker-core/serde", &small, 8),
            ],
            KernelState::measured(),
        );

        let grown = default_row(120, 0);
        let head = SizeReport::new(
            vec![
                baseline_row(),
                grown.clone(),
                // The feature still costs the same 8 B on top of the engine.
                feature_row("waymaker-core/serde", &grown, 8),
            ],
            KernelState::measured(),
        );

        let diffs = diff(&base, &head);
        let names: Vec<&str> = diffs.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(
            names,
            [DEFAULT_ROW],
            "only the engine changed, so only the engine row should be reported"
        );
        assert_eq!(
            diffs.first().and_then(RowDiff::flash_change),
            Some(100),
            "and it should be reported once, at its real size"
        );
    }

    #[test]
    fn a_feature_that_really_did_grow_is_still_reported() {
        let default = default_row(20, 0);
        let base = SizeReport::new(
            vec![
                baseline_row(),
                default.clone(),
                feature_row("waymaker-core/serde", &default, 8),
            ],
            KernelState::measured(),
        );
        let head = SizeReport::new(
            vec![
                baseline_row(),
                default.clone(),
                feature_row("waymaker-core/serde", &default, 40),
            ],
            KernelState::measured(),
        );
        let diffs = diff(&base, &head);
        let names: Vec<&str> = diffs.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["waymaker-core/serde"]);
        assert_eq!(diffs.first().and_then(RowDiff::flash_change), Some(32));
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
            BASELINE_PROBE_FLASH,
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
            BASELINE_PROBE_FLASH,
            true,
        );
        let head = SizeReport::new(
            vec![bigger_baseline, shifted_default],
            KernelState::measured(),
        );
        assert!(diff(&base, &head).is_empty(), "{:?}", diff(&base, &head));
    }

    #[test]
    fn a_diff_reports_the_gated_figure_beside_the_image_it_was_read_from() {
        let base = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_000, 0, BASELINE_PROBE_FLASH + 400),
            ],
            KernelState::measured(),
        );
        // 200 B more image, all of it the probe's own arithmetic. The image grew and the
        // layers did not, which is the distinction issue #72 is about.
        let head = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_200, 0, BASELINE_PROBE_FLASH + 600),
            ],
            KernelState::measured(),
        );
        let diffs = diff(&base, &head);
        let default = diffs
            .iter()
            .find(|entry| entry.name == DEFAULT_ROW)
            .expect("the default row moved");
        assert_eq!(default.before_layers, Some(600));
        assert_eq!(default.after_layers, Some(600));
        let rendered = render_diff(&diffs);
        assert!(rendered.contains("layers 600 -> 600"), "{rendered}");
        assert!(rendered.contains("flash 1000 -> 1200"), "{rendered}");
    }

    #[test]
    fn a_row_whose_layers_moved_is_a_diff_even_when_its_image_did_not() {
        // The probe shrank by exactly what the kernel grew. The image is byte for byte the
        // same size, and the number the budget is held against went up.
        let base = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_000, 0, BASELINE_PROBE_FLASH + 400),
            ],
            KernelState::measured(),
        );
        let head = SizeReport::new(
            vec![
                baseline_row(),
                default_row_with_probe(1_000, 0, BASELINE_PROBE_FLASH + 300),
            ],
            KernelState::measured(),
        );
        let diffs = diff(&base, &head);
        assert!(
            diffs.iter().any(|entry| entry.name == DEFAULT_ROW),
            "{diffs:?}"
        );
        assert!(render_diff(&diffs).contains("layers 600 -> 700"));
    }

    #[test]
    fn an_ungated_row_carries_no_layers_figure() {
        // A feature row's own cost is measured against the engine underneath it, and the
        // layers' share is defined against the baseline. Printing the second beside the
        // first reports the engine's growth again as every feature's.
        let default = default_row(20, 0);
        let report = SizeReport::new(
            vec![
                baseline_row(),
                default.clone(),
                feature_row("waymaker-core/serde", &default, 8),
            ],
            KernelState::measured(),
        );
        let diffs = diff(&report, &report);
        assert!(
            diffs.is_empty(),
            "a report against itself changed nothing: {diffs:?}"
        );
        let grown = SizeReport::new(
            vec![
                baseline_row(),
                default.clone(),
                feature_row("waymaker-core/serde", &default, 40),
            ],
            KernelState::measured(),
        );
        let feature = diff(&report, &grown)
            .into_iter()
            .find(|entry| entry.name == "waymaker-core/serde")
            .expect("the feature row moved");
        assert_eq!(feature.before_layers, None);
        assert_eq!(feature.after_layers, None);
    }

    #[test]
    fn a_kernel_state_change_is_reported_beside_the_rows() {
        let base = report(0, 0);
        let head = SizeReport::new(
            vec![baseline_row(), default_row(0, 0)],
            Some(KernelState {
                total: 24,
                types: vec![("Cursor".to_owned(), 24)],
            }),
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
    fn a_trait_method_is_reachable_even_though_it_carries_no_pub() {
        // The hole a `pub `-prefix scan leaves: a trait's methods and a trait impl's
        // methods are as callable as the trait, and neither is written `pub`. A layer could
        // implement the whole storage protocol and have every byte of it dead-stripped.
        let functions = public_functions(&kernel(
            "pub trait Storage {\n\
            \x20   fn seal(&self, x: u32) -> u32 { x }\n\
            \x20   fn erase(&self);\n\
             }\n\
             impl Storage for Bank {\n\
            \x20   fn erase(&self) {}\n\
             }\n",
        ));
        let names: Vec<&str> = functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["seal", "erase", "erase"]);
    }

    #[test]
    fn an_inherent_impls_private_method_is_not_required_of_the_probe() {
        // The other direction: `impl Bank { fn helper() }` is private, so demanding the
        // probe call it would be a rule nobody could satisfy.
        let functions = public_functions(&kernel(
            "impl Bank {\n\
            \x20   pub fn scan(&self) {}\n\
            \x20   fn helper(&self) {}\n\
             }\n",
        ));
        let names: Vec<&str> = functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["scan"]);
    }

    #[test]
    fn a_trait_impl_whose_header_spans_lines_is_still_a_trait_impl() {
        // A `where` clause puts the opening brace on a line of its own. Classifying that
        // bare `{` reads every method of the impl as an ordinary private one, so the whole
        // implementation drops out of the reach rule and is free to be dead-stripped.
        let functions = public_functions(&kernel(
            "impl<T> Storage for Bank<T>\n\
             where\n\
            \x20   T: Copy,\n\
             {\n\
            \x20   fn erase(&self) {}\n\
             }\n",
        ));
        let names: Vec<&str> = functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["erase"]);
    }

    #[test]
    fn a_trait_declaration_whose_header_spans_lines_is_still_a_trait() {
        let functions = public_functions(&kernel(
            "pub trait Storage<T>\n\
             where\n\
            \x20   T: Copy,\n\
             {\n\
            \x20   fn seal(&self) {}\n\
             }\n",
        ));
        let names: Vec<&str> = functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["seal"]);
    }

    #[test]
    fn an_inherent_impl_with_a_multiline_header_stays_inherent() {
        // The other direction: the remembered kind must not turn a plain `impl` into a
        // trait impl and start demanding its private helpers.
        let functions = public_functions(&kernel(
            "impl<T> Bank<T>\n\
             where\n\
            \x20   T: Copy,\n\
             {\n\
            \x20   fn helper(&self) {}\n\
             }\n",
        ));
        assert!(functions.is_empty(), "{functions:?}");
    }

    #[test]
    fn a_remembered_declaration_does_not_leak_into_the_next_block() {
        let functions = public_functions(&kernel(
            "impl Storage for Bank\n\
             {\n\
            \x20   fn erase(&self) {}\n\
             }\n\
             mod inner {\n\
            \x20   fn hidden() {}\n\
             }\n",
        ));
        let names: Vec<&str> = functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["erase"]);
    }

    #[test]
    fn two_runs_against_different_bases_do_not_share_a_build_directory() {
        // Cargo serialises the builds and releases its lock before the artifact is read,
        // so a shared directory lets one run measure the other's image and report a diff
        // against the wrong commit.
        let root = Path::new("/w");
        assert_ne!(
            baseline_build_dir(root, "abc123def456"),
            baseline_build_dir(root, "fed654cba321")
        );
        // The same base is shared on purpose: same input, same artifact, and a build cache
        // that survives from one run to the next.
        assert_eq!(
            baseline_build_dir(root, "abc123def4567890"),
            baseline_build_dir(root, "abc123def4567890")
        );
    }

    #[test]
    fn a_function_nested_inside_a_trait_method_body_is_not_required() {
        let functions = public_functions(&kernel(
            "impl Storage for Bank {\n\
            \x20   fn erase(&self) {\n\
            \x20       fn inner() {}\n\
            \x20   }\n\
             }\n",
        ));
        let names: Vec<&str> = functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        assert_eq!(names, ["erase"]);
    }

    #[test]
    fn a_name_that_is_only_a_local_variable_is_not_a_call() {
        // `mentions` used to credit a bare word anywhere in the file.
        let message = reach_violations(
            &kernel("pub fn seal() {}\n"),
            "fn probe() { let seal = 3; }\n",
        );
        assert!(message.contains("`seal`"), "{message}");
    }

    #[test]
    fn a_call_in_path_position_counts() {
        for probe in [
            "fn probe() { waymaker_core::seal(); }\n",
            "fn probe() { bank.seal(); }\n",
            "fn probe() { seal::<u32>(); }\n",
            "fn probe() { let f = seal(); }\n",
        ] {
            assert!(
                check_probe_reach(&kernel("pub fn seal() {}\n"), Some(probe)).is_empty(),
                "{probe} should count as a call"
            );
        }
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
