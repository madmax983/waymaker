//! The allocation gate and the instruction figure, both measured by Valgrind.
//!
//! `CLAUDE.md` has carried this sentence under "What is not checked" since rung 0.1:
//! *"Allocation, as a measurement ... the allocation half is structural — a `no_std` crate
//! with no dependencies and no `extern crate alloc` cannot allocate ... A global allocator
//! that counted allocations would need the `unsafe` this workspace denies."*
//!
//! Both halves of that are true and the conclusion does not follow. `#![no_std]` and the
//! absence of `extern crate alloc` are facts about a *crate*; what a firmware pays for is
//! what the linked image does, and the gap between them is every route by which `alloc`
//! arrives without anybody writing the words — a dependency that turns a feature on, a
//! generic instantiated where it is available, a `format!` on a branch nothing links today.
//! The structural argument is a good argument and it is not a measurement.
//!
//! DHAT closes it, and closes it without the `unsafe`, because it needs no global allocator
//! at all: Valgrind intercepts `malloc` in the *binary*, below anything Rust can express. So
//! the claim becomes a number, and [`ENGINE_HEAP_BLOCKS`] is what the number must be.
//!
//! # The two tools, and why both
//!
//! **DHAT** answers *did the engine allocate*. It is a gate.
//!
//! **Callgrind** answers *what did the engine cost*. It is published rather than gated, for
//! the reason [`crate::wear`] publishes write amplification without a budget: §04 states no
//! instruction target, and a ceiling invented here would be a number nobody agreed to. What
//! it is good for is comparison — an instruction count is deterministic where a wall clock
//! is not, so the same commit measures the same on a loaded runner and on an idle one, and a
//! figure that moved is a change rather than a neighbour.
//!
//! # What the figure is not
//!
//! What a part executes. This runs on the host, on the host's instruction set, under a
//! profile that is not the one a board is flashed with — [`PROFILE`] says which and why. A
//! Cortex-M0+ has another encoding, another cost per instruction and no branch predictor, so
//! nothing here converts into a cycle count on the hardware §04's budgets are stated for. It
//! is a *relative* cost signal for the host, and the boards owe the real one exactly as
//! [`crate::docs::HARDWARE_TARGETS`] records for everything else.
//!
//! # How a byte is attributed
//!
//! By symbol and by source path, the way [`crate::size`] attributes a byte of code flash —
//! and for its reason, which is that the alternative is asking a reader to believe a total.
//! A DHAT program point carries the stack that allocated, innermost frame first, and the
//! first frame of it belonging to *any* workspace crate is the code that decided to
//! allocate. If that crate is one of the [`engine_crates`], the engine allocated.
//!
//! The direction is deliberate. `waymaker-fault` models media in a `Vec`, so engine code
//! calling [`waymaker_flash::storage::StableStorage::program`] on it reaches an allocation
//! in every workload — through the *model*, which a real driver is not. Charging that to the
//! engine would make this gate unpassable for a reason that is not a defect, so the
//! innermost workspace frame decides and `waymaker-fault` is the harness.
//!
//! The *path* half is what makes that survive an optimiser. A function inlined into another
//! crate keeps its own source file in the debug info and loses its crate from the printed
//! symbol — Valgrind prints the inlined frame as a bare name — so an attribution reading
//! names alone would charge inlined engine code to whoever inlined it. ADR 0029 records
//! exactly that as an accepted limit of the code-flash gate; here it is the gate passing for
//! the reason it exists to catch, so both tools are asked for full source paths
//! ([`FULL_PATHS`]) and the path is consulted first.

pub mod workload;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::coverage::CrateRoot;

/// The cargo profile the workload is linked under.
///
/// Declared in the workspace manifest. Not `release`: that row is what §04's code-flash
/// budget is measured against, and a gate that edited it to make a second measurement
/// possible would move the number the first one publishes.
pub const PROFILE: &str = "profiling";

/// What the engine's heap use must be, in blocks.
///
/// Zero — and blocks rather than bytes, because a zero-byte allocation is still an
/// allocation: `malloc(0)` returns a pointer, and a firmware that reached it has an
/// allocator linked whatever the byte count says.
pub const ENGINE_HEAP_BLOCKS: u64 = 0;

/// The argument that makes both tools print whole source paths.
///
/// Empty on purpose: `--fullpath-after=<prefix>` prints what follows `prefix`, and an empty
/// prefix is the whole path. Without it Valgrind prints a bare file name, and an attribution
/// that reads paths would have nothing to read — which matters most for inlined frames,
/// where the file is the only thing that still names the crate that wrote the code.
pub const FULL_PATHS: &str = "--fullpath-after=";

/// Where the JSON report is written.
pub const REPORT_PATH: &str = "target/waymaker-profile.json";

/// Where the tools' own output is written, relative to the workspace root.
pub const OUTPUT_DIR: &str = "target/profile";

/// The subcommand the tools are pointed at.
pub const WORKLOAD_COMMAND: &str = "profile-workload";

/// The line a workload prints so that the outer run can read its denominator.
///
/// Read rather than assumed: a workload whose effect count came from the table below would
/// publish a cost per effect over a number nothing measured, and the error would *flatter*
/// the engine — the direction [`crate::wear`] avoids by counting what the device was asked
/// for rather than deriving it.
const UNITS_MARKER: &str = "waymaker-profile-workload: units=";

/// One thing the tools are run over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Workload {
    /// The name [`WORKLOAD_COMMAND`] takes.
    pub name: &'static str,
    /// What it drives, in one line, carried into the report.
    pub what: &'static str,
    /// What one unit of this workload's work is: an effect, or a conformance case.
    ///
    /// Not every workload has effects. The published figure is a cost *per* something, and a
    /// report whose column meant an effect on three rows and a case on the fourth would be a
    /// column nobody can read.
    pub unit: &'static str,
    /// How many units it is expected to complete.
    ///
    /// Checked against what the run reports rather than trusted: the two disagreeing is a
    /// workload that changed and a table that did not, and the table is not the measurement.
    pub units: u16,
}

/// The workloads, in report order.
///
/// One per part of the engine that the others do not reach. A single workload would publish
/// a zero about whichever part it happened to drive, and every other part's first allocation
/// would pass this gate — which is not a hypothetical: the first version of this table had
/// two rows, and `waymaker-embassy` and `waymaker-conformance` were gated in name while
/// nothing executed a line of either. [`ProfileReport::shortfall_report`] is what stops that
/// returning, and these are what make it pass.
pub const WORKLOADS: &[Workload] = &[
    Workload {
        name: "journal",
        what: "§09's frame codec and commit seal, §10's reserve, and the recovery scan, driven by the rig",
        unit: "effect",
        units: crate::wear::EFFECTS,
    },
    Workload {
        name: "driver",
        what: "§06's kernel boundary and §07's effect protocol, driven to a terminal record",
        unit: "effect",
        units: 2,
    },
    Workload {
        name: "facade",
        what: "§06's OTA example through `poll_ota`: `waymaker-embassy`'s Ctx and its four futures",
        unit: "effect",
        units: 3,
    },
    Workload {
        name: "conformance",
        what: "§12's storage contract, as `waymaker-conformance` runs it against the fault model",
        unit: "case",
        units: 22,
    },
];

/// A workspace crate, as an attribution sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceCrate {
    /// The package name.
    pub name: String,
    /// The package name underscored, which is how it appears inside a symbol.
    pub symbol: String,
    /// The directory its sources are under, with a trailing separator, as the tools print it.
    pub directory: String,
    /// Whether an allocation attributed here is the engine's.
    pub engine: bool,
}

/// The crates whose allocation count is gated at [`ENGINE_HEAP_BLOCKS`].
///
/// The three layers plus [`crate::policy::NO_STD_TEST_SUPPORT_CRATES`] — derived from the
/// layering table rather than listed again, so a crate joining either category is gated here
/// without anybody remembering a row. Those three test-support crates belong for the reason
/// `policy` gives for their `#![no_std]`: each claims to be allocation-free, and
/// `waymaker-drive`'s claim is issue #28's own "done when".
#[must_use]
pub fn engine_crates() -> Vec<&'static str> {
    crate::policy::LAYERS
        .iter()
        .map(|layer| layer.name)
        .chain(crate::policy::NO_STD_TEST_SUPPORT_CRATES.iter().copied())
        .collect()
}

/// Every workspace crate a frame can name, engine and harness alike.
///
/// The harness half is what makes the engine's zero mean anything: `waymaker-fault` models
/// media in a `Vec` and `xtask` is the process, so an allocation reached *through* engine
/// code lands on one of them and the engine's own count stays a statement about the engine.
///
/// # Errors
///
/// [`ProfileError`] when a crate this gate attributes by is not among `roots`. Fails closed
/// for [`crate::coverage`]'s reason: a crate missing from the attribution does not attribute
/// to nobody, it attributes to *whoever called it*, and on this gate that is the harness.
pub fn workspace_crates(roots: &[CrateRoot]) -> Result<Vec<WorkspaceCrate>, ProfileError> {
    let engine = engine_crates();
    let mut names: Vec<&str> = engine.clone();
    for name in crate::policy::TEST_SUPPORT_CRATES
        .iter()
        .chain(crate::policy::MEASUREMENT_CRATES)
        .chain(crate::policy::HOST_TOOLS)
    {
        if !names.contains(name) {
            names.push(name);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let root = roots.iter().find(|root| root.name == name).ok_or_else(|| {
                ProfileError::new(format!(
                    "{name} is a crate this gate attributes by and the workspace metadata has no directory for it; an unattributed crate is charged to its caller, which here is the harness"
                ))
            })?;
            Ok(WorkspaceCrate {
                name: name.to_owned(),
                symbol: name.replace('-', "_"),
                directory: format!("{}{}", root.directory.display(), std::path::MAIN_SEPARATOR),
                engine: engine.contains(&name),
            })
        })
        .collect()
}

/// What one workload allocated, split three ways.
///
/// Three rather than two, because "the engine allocated nothing" is worth nothing beside a
/// process that allocated nothing at all. The runtime column is what says DHAT was watching.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Heap {
    /// Bytes allocated by a frame in an [`engine_crates`] crate.
    pub engine_bytes: u64,
    /// Blocks allocated by a frame in an [`engine_crates`] crate.
    pub engine_blocks: u64,
    /// Bytes allocated by a workspace crate that is not the engine.
    pub harness_bytes: u64,
    /// Blocks allocated by a workspace crate that is not the engine.
    pub harness_blocks: u64,
    /// Bytes allocated where no frame named a workspace crate: `std`, libc, and whatever the
    /// process does before `main`.
    pub runtime_bytes: u64,
    /// Blocks allocated where no frame named a workspace crate.
    pub runtime_blocks: u64,
}

impl Heap {
    /// Every block the process allocated.
    #[must_use]
    pub const fn total_blocks(&self) -> u64 {
        self.engine_blocks
            .saturating_add(self.harness_blocks)
            .saturating_add(self.runtime_blocks)
    }

    /// Every byte the process allocated.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.engine_bytes
            .saturating_add(self.harness_bytes)
            .saturating_add(self.runtime_bytes)
    }
}

/// What one workload cost, in instructions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    /// Instructions in code an [`engine_crates`] crate wrote.
    pub engine: u64,
    /// Instructions in code another workspace crate wrote.
    pub harness: u64,
    /// Instructions no workspace crate wrote.
    pub runtime: u64,
}

impl Cost {
    /// Every instruction the process executed.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.engine
            .saturating_add(self.harness)
            .saturating_add(self.runtime)
    }

    /// The engine's instructions per unit of work, in hundredths, rounded **up**.
    ///
    /// Hundredths for [`waymaker_rig::wear::PerEffect`]'s reason: an integer quotient
    /// publishes 68 035 where 68 035.125 was measured. Rounded up rather than truncated for
    /// the same reason one step further down — 544 281 over 8 is 68 035.125, and truncating
    /// at the hundredth publishes 68 035.12, which is *still* less than was measured. A cost
    /// figure that understates is worse than no figure, because it is believed; Codex found
    /// the truncating version on the first review of this gate.
    ///
    /// `wear`'s own figure truncates and its test states the tolerance it accepts. This one
    /// does not need a tolerance, so it does not have one.
    #[must_use]
    pub fn engine_per_unit_hundredths(&self, units: u32) -> Option<u64> {
        let divisor = u64::from(units);
        let scaled = self.engine.checked_mul(100)?;
        let quotient = scaled.checked_div(divisor)?;
        // Ceiling without a second multiply: add one when anything was lost.
        Some(match scaled.checked_rem(divisor) {
            Some(0) | None => quotient,
            Some(_) => quotient.saturating_add(1),
        })
    }
}

/// What callgrind said, and which engine crates it said it about.
///
/// The second half is not decoration. This gate names six crates and holds each to zero heap
/// blocks, and a crate the workloads never execute scores that zero for the wrong reason —
/// it is the score a crate that had been deleted would get. Reach is measured in
/// *instructions* rather than allocations for the obvious reason: every crate that ran has
/// instructions, and a crate that ran correctly has no allocations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attribution {
    /// What the run cost.
    pub cost: Cost,
    /// Engine crates that executed at least one instruction.
    pub reached: BTreeSet<String>,
}

/// One workload, measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkloadProfile {
    /// The row's name, from [`WORKLOADS`].
    pub workload: String,
    /// What it drives.
    pub what: String,
    /// What one unit of this workload's work is, from its row.
    pub unit: String,
    /// Units the run reported completing.
    pub units: u32,
    /// Engine crates that executed at least one instruction in this run.
    ///
    /// The half that makes the gate's list of gated crates mean something. A crate the
    /// workloads never execute can never be attributed an allocation, so a zero for it is
    /// the zero a crate that is not there would score — which is what
    /// [`ProfileReport::shortfall_report`] refuses.
    pub reached: BTreeSet<String>,
    /// What it allocated.
    pub heap: Heap,
    /// What it cost.
    pub cost: Cost,
}

/// Why a row is, or is not, a pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The engine allocated nothing, and both tools had something to say.
    Clean,
    /// The engine allocated. The gate's whole purpose.
    Allocated {
        /// How many blocks.
        blocks: u64,
        /// How many bytes.
        bytes: u64,
    },
    /// The measurement did not happen, so its zero says nothing.
    ///
    /// Every gate in this workspace fails closed, and this is that rule met by a tool: a
    /// DHAT run that saw no allocation anywhere was not watching, and the zero it prints for
    /// the engine is the zero it would print for a process that never started.
    Unmeasurable(String),
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Clean => formatter.write_str("ok"),
            Self::Allocated { blocks, bytes } => {
                write!(formatter, "allocated {blocks} block(s), {bytes} B")
            }
            Self::Unmeasurable(why) => write!(formatter, "unmeasurable: {why}"),
        }
    }
}

impl WorkloadProfile {
    /// What this row is, gate-wise.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        if self.units == 0 {
            return Verdict::Unmeasurable(format!(
                "the workload completed no {}, so there is nothing the figures are per",
                self.unit
            ));
        }
        if self.heap.total_blocks() == 0 {
            return Verdict::Unmeasurable(
                "DHAT saw no allocation anywhere in the process, so a zero for the engine is \
                 the zero a run that never started would print"
                    .to_owned(),
            );
        }
        if self.cost.engine == 0 {
            return Verdict::Unmeasurable(
                "callgrind attributed no instruction to any engine crate, so either the \
                 engine did not run or the image carries nothing to attribute by"
                    .to_owned(),
            );
        }
        if self.heap.engine_blocks > ENGINE_HEAP_BLOCKS {
            return Verdict::Allocated {
                blocks: self.heap.engine_blocks,
                bytes: self.heap.engine_bytes,
            };
        }
        Verdict::Clean
    }
}

/// Every workload, measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileReport {
    rows: Vec<WorkloadProfile>,
}

impl ProfileReport {
    /// A report over `rows`.
    #[must_use]
    pub const fn new(rows: Vec<WorkloadProfile>) -> Self {
        Self { rows }
    }

    /// The rows, in report order.
    #[must_use]
    pub fn rows(&self) -> &[WorkloadProfile] {
        &self.rows
    }

    /// The row named `workload`, if the report has one.
    #[must_use]
    pub fn row(&self, workload: &str) -> Option<&WorkloadProfile> {
        self.rows.iter().find(|row| row.workload == workload)
    }

    /// The table the command prints.
    #[must_use]
    pub fn render(&self) -> String {
        let width = self
            .rows
            .iter()
            .map(|row| row.workload.len())
            .max()
            .unwrap_or(0)
            .max("workload".len());
        let mut table = vec![format!(
            "\nheap and instructions, measured under valgrind on the host, cargo profile `{PROFILE}`\n  {:<width$}  {:>11} {:>13} {:>9} {:>12} {:>13} {:>12}  {}\n",
            "workload",
            "units",
            "engine blks",
            "engine B",
            "other blks",
            "engine Ir",
            "Ir/unit",
            "verdict",
        )];
        for row in &self.rows {
            table.push(format!(
                "  {:<width$}  {:>11} {:>13} {:>9} {:>12} {:>13} {:>12}  {}\n",
                row.workload,
                format!("{} {}s", row.units, row.unit),
                row.heap.engine_blocks,
                row.heap.engine_bytes,
                row.heap
                    .harness_blocks
                    .saturating_add(row.heap.runtime_blocks),
                row.cost.engine,
                render_hundredths(row.cost.engine_per_unit_hundredths(row.units)),
                row.verdict(),
            ));
        }
        table.push(format!(
            "the engine is {}: the gate is {ENGINE_HEAP_BLOCKS} block, and blocks rather than bytes because malloc(0) is an allocator in the image whatever the byte count says.\n",
            engine_crates().join(", ")
        ));
        table.push(
            "the \"other blks\" column is the harness and the runtime, and it is printed because a zero beside a zero is a tool that was not watching: waymaker-fault models media in a Vec, so engine code reaching it allocates through the model rather than in the engine, and the innermost workspace frame is what decides which.\n"
                .to_owned(),
        );
        table.push(
            "the instruction figures are a host cost under a host profile, deterministic enough to compare two commits and convertible into no cycle count on any part: \u{a7}04 states no instruction budget, so they are published and not gated, and the boards owe the real figure exactly as docs::HARDWARE_TARGETS records for everything else.\n"
                .to_owned(),
        );
        table.concat()
    }

    /// Engine crates no row of this report reached, in name order.
    ///
    /// Empty is what a complete run looks like. Anything in it is a crate this gate claims
    /// to hold at [`ENGINE_HEAP_BLOCKS`] and has not looked at.
    #[must_use]
    pub fn unreached_engine_crates(&self) -> Vec<String> {
        let reached: BTreeSet<&str> = self
            .rows
            .iter()
            .flat_map(|row| row.reached.iter().map(String::as_str))
            .collect();
        engine_crates()
            .into_iter()
            .filter(|name| !reached.contains(name))
            .map(str::to_owned)
            .collect()
    }

    /// Every row that is not a pass, or `None` when they all are.
    #[must_use]
    pub fn shortfall_report(&self) -> Option<String> {
        let mut lines: Vec<String> = self
            .rows
            .iter()
            .filter_map(|row| match row.verdict() {
                Verdict::Clean => None,
                other => Some(format!("  {}: {other}", row.workload)),
            })
            .collect();
        // A report with no rows is not a report that passed. Every other gate here fails
        // closed on a measurement that did not happen, and a table nobody filled in is the
        // emptiest version of that.
        if self.rows.is_empty() {
            lines.push("  no workload was measured, so nothing was gated".to_owned());
        }
        // The same, one row down: a table with a row per workload is only a table about the
        // engine while every workload has one.
        for workload in WORKLOADS {
            if self.row(workload.name).is_none() {
                lines.push(format!(
                    "  {}: WORKLOADS declares this workload and the report has no row for it",
                    workload.name
                ));
            }
        }
        // And the check that gives the gated list its meaning. A crate no workload executes
        // cannot be attributed an allocation, so its zero is the score a deleted crate would
        // get — the gate would cover it in name and not in fact. Codex found exactly that on
        // the first review of this gate, with `waymaker-embassy` linked and unexecuted and
        // `waymaker-conformance` not in the dependency graph at all.
        for unreached in self.unreached_engine_crates() {
            lines.push(format!(
                "  {unreached}: no workload executed an instruction of this crate, so holding it to {ENGINE_HEAP_BLOCKS} heap blocks measured nothing. Give it a workload, or stop calling it engine code."
            ));
        }
        if lines.is_empty() {
            return None;
        }
        Some(format!(
            "profile: {} row(s) failed\n{}\n\nThe kernel is `no_std`, `no_alloc` and dependency-free (kernel-is-dependency-free), and an allocation in an engine crate is that decision no longer holding. DHAT recorded the stack that did it: see {OUTPUT_DIR}/.\n",
            lines.len(),
            lines.join("\n")
        ))
    }
}

/// Renders a hundredths figure to two decimal places, or a dash.
fn render_hundredths(value: Option<u64>) -> String {
    value.map_or_else(
        || "-".to_owned(),
        |hundredths| format!("{}.{:02}", hundredths / 100, hundredths % 100),
    )
}

/// Why a profile could not be taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileError {
    /// What went wrong, in one line.
    pub message: String,
}

impl ProfileError {
    /// A failure, described.
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProfileError {}

/// Splits a DHAT stack frame into the symbol it names and where that symbol lives.
///
/// A frame reads `0x1F2E4B: <waymaker_fault::device::Device>::with_bit_rule
/// (/home/.../device.rs:122)`, or `0x4846828: malloc (in /usr/libexec/...so)` for something
/// with no source. Split rather than searched whole, because the two halves are read by
/// different rules and reading them together is what makes a *type argument* look like the
/// crate that wrote the code — see [`attribute_symbol`].
#[must_use]
pub fn split_frame(frame: &str) -> (&str, &str) {
    let after_address = frame.split_once(": ").map_or(frame, |(_, rest)| rest);
    after_address
        .rfind(" (")
        .and_then(|at| {
            let symbol = after_address.get(..at)?;
            let location = after_address.get(at.saturating_add(2)..)?;
            Some((symbol, location.trim_end_matches(')')))
        })
        .unwrap_or((after_address, ""))
}

/// The crate whose sources a path is under.
///
/// The most trustworthy of the three readers, and the only one an optimiser cannot move: a
/// function inlined into another crate keeps its own source file in the debug info, while
/// the printed symbol loses its crate entirely. ADR 0029 records name-only attribution
/// mistaking an inlined body for its caller as an accepted limit of the code-flash gate;
/// here that would be this gate passing for the reason it exists to catch, so the path is
/// asked first and [`FULL_PATHS`] is what makes there be one to ask.
#[must_use]
pub fn attribute_path<'a>(
    path: &str,
    workspace: &'a [WorkspaceCrate],
) -> Option<&'a WorkspaceCrate> {
    workspace
        .iter()
        .filter_map(|entry| path.find(&entry.directory).map(|at| (at, entry)))
        .min_by_key(|(at, _)| *at)
        .map(|(_, entry)| entry)
}

/// The crate a symbol's path is rooted in.
///
/// Rooted, not mentioned. `with_capacity_in<waymaker_core::activity::ActivityKind,
/// alloc::alloc::Global>` is `alloc`'s code with a kernel type passed to it, and a reader
/// that searched the whole string would charge a harness `Vec` to `waymaker-core` — this
/// gate did, on its first run, and the row it failed was a `Vec` in the workload beside it.
/// A generic argument is not a definition.
///
/// So a demangled name counts only where the crate is the path root: at the start, or at the
/// start of a qualified path, where `<waymaker_fault::device::Device as
/// waymaker_flash::storage::StableStorage>::read` is `waymaker-fault`'s because that is the
/// crate that wrote the `impl`. Anything still mangled goes to
/// [`crate::size::defining_crate`], the reader the code-flash gate already attributes by.
#[must_use]
pub fn attribute_symbol<'a>(
    symbol: &str,
    workspace: &'a [WorkspaceCrate],
) -> Option<&'a WorkspaceCrate> {
    workspace
        .iter()
        .find(|entry| {
            symbol.starts_with(&format!("{}::", entry.symbol))
                || symbol.starts_with(&format!("<{}::", entry.symbol))
        })
        .or_else(|| {
            symbol
                .split(|character: char| {
                    !(character.is_ascii_alphanumeric() || character == '_' || character == '$')
                })
                .filter(|token| token.starts_with("_R") || token.starts_with("_Z"))
                .find_map(|token| {
                    crate::size::defining_crate(token)
                        .and_then(|name| workspace.iter().find(|entry| entry.symbol == name))
                })
        })
}

/// The crate a whole DHAT stack frame belongs to: its path, else its symbol.
#[must_use]
pub fn attribute_frame<'a>(
    frame: &str,
    workspace: &'a [WorkspaceCrate],
) -> Option<&'a WorkspaceCrate> {
    let (symbol, location) = split_frame(frame);
    attribute_path(location, workspace).or_else(|| attribute_symbol(symbol, workspace))
}

/// Reads a DHAT JSON export into a [`Heap`].
///
/// # Errors
///
/// [`ProfileError`] when the export is not DHAT's JSON, is not a heap-mode run, or carries a
/// program point whose frames or totals cannot be read. Each is a failure rather than a row
/// skipped: a parser that dropped what it did not understand would report a smaller heap
/// than the process had, which is the direction this gate must not be wrong in.
pub fn parse_dhat(json: &str, workspace: &[WorkspaceCrate]) -> Result<Heap, ProfileError> {
    let document: Value = serde_json::from_str(json)
        .map_err(|error| ProfileError::new(format!("the DHAT export is not JSON: {error}")))?;
    // Heap mode is the only mode whose `tb` is bytes allocated. A copy-mode or ad-hoc run
    // parses cleanly and means something else entirely.
    match document.get("mode").and_then(Value::as_str) {
        Some("heap") => {}
        Some(other) => {
            return Err(ProfileError::new(format!(
                "the DHAT export is a `{other}` run; this gate reads heap mode"
            )));
        }
        None => {
            return Err(ProfileError::new(
                "the DHAT export names no mode, so it is not one this gate can read",
            ));
        }
    }
    let frames: Vec<&str> = document
        .get("ftbl")
        .and_then(Value::as_array)
        .ok_or_else(|| ProfileError::new("the DHAT export has no frame table"))?
        .iter()
        .map(|frame| frame.as_str().unwrap_or_default())
        .collect();
    let points = document
        .get("pps")
        .and_then(Value::as_array)
        .ok_or_else(|| ProfileError::new("the DHAT export has no program points"))?;

    let mut heap = Heap::default();
    for point in points {
        let bytes = point
            .get("tb")
            .and_then(Value::as_u64)
            .ok_or_else(|| ProfileError::new("a DHAT program point has no total bytes"))?;
        let blocks = point
            .get("tbk")
            .and_then(Value::as_u64)
            .ok_or_else(|| ProfileError::new("a DHAT program point has no total blocks"))?;
        let stack = point
            .get("fs")
            .and_then(Value::as_array)
            .ok_or_else(|| ProfileError::new("a DHAT program point has no frames"))?;
        // Innermost first, which is the order DHAT writes them in and the order this
        // attribution rests on: the frame nearest the allocation is the code that decided to
        // make it. Read the other way round, every allocation in the process would be
        // charged to whichever crate declares `main` — which is `xtask`, so the gate would
        // pass on a workload that allocated in the kernel on every record.
        let owner = stack
            .iter()
            .filter_map(Value::as_u64)
            .filter_map(|index| usize::try_from(index).ok())
            .filter_map(|index| frames.get(index))
            .find_map(|frame| attribute_frame(frame, workspace));
        match owner {
            Some(entry) if entry.engine => {
                heap.engine_bytes = heap.engine_bytes.saturating_add(bytes);
                heap.engine_blocks = heap.engine_blocks.saturating_add(blocks);
            }
            Some(_) => {
                heap.harness_bytes = heap.harness_bytes.saturating_add(bytes);
                heap.harness_blocks = heap.harness_blocks.saturating_add(blocks);
            }
            None => {
                heap.runtime_bytes = heap.runtime_bytes.saturating_add(bytes);
                heap.runtime_blocks = heap.runtime_blocks.saturating_add(blocks);
            }
        }
    }
    Ok(heap)
}

/// Reads a callgrind output file into a [`Cost`].
///
/// # The cross-check
///
/// Callgrind writes its own process total on a `summary:` or `totals:` line, and this
/// refuses to answer unless the per-function costs add up to it. That is not belt and
/// braces: three separate ways of misreading this format produce a plausible number.
/// Counting the cost line after a `calls=` as self cost double-counts every callee, so the
/// figure grows with call depth. Reading `fl=` and `fn=` out of one name table gets the
/// wrong names, because callgrind compresses each position kind in a namespace of its own
/// and the ids collide. And a cost line whose event column is missed silently contributes
/// zero. The sum is the one thing that catches all three.
///
/// # Errors
///
/// [`ProfileError`] when the file declares no `events:` line, does not count `Ir` among
/// them, declares no total, or the per-function costs do not add up to that total.
pub fn parse_callgrind(
    text: &str,
    workspace: &[WorkspaceCrate],
) -> Result<Attribution, ProfileError> {
    let events = text
        .lines()
        .find_map(|line| line.strip_prefix("events:"))
        .ok_or_else(|| ProfileError::new("the callgrind output declares no `events:` line"))?;
    let column = events
        .split_whitespace()
        .position(|event| event == "Ir")
        .ok_or_else(|| {
            ProfileError::new(format!(
                "the callgrind output counts `{}` and not `Ir`",
                events.trim()
            ))
        })?;

    // One table per position kind. `fl=`/`fi=`/`fe=`/`cfi=` share the file namespace and
    // `fn=`/`cfn=` the function one, and their ids overlap — in a real run of this workspace
    // by more than a hundred — so a single table hands one kind the other's names.
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut functions: BTreeMap<String, String> = BTreeMap::new();
    let mut file: Option<String> = None;
    let mut function: Option<String> = None;
    // The cost line after a `calls=` is the *inclusive* cost of that call rather than self
    // cost. Summed as self cost it counts every callee again at every level.
    let mut inclusive_next = false;
    let mut cost = Cost::default();
    let mut reached: BTreeSet<String> = BTreeSet::new();

    for line in text.lines() {
        if let Some(rest) = line
            .strip_prefix("fl=")
            .or_else(|| line.strip_prefix("fi="))
            .or_else(|| line.strip_prefix("fe="))
        {
            // `fi=` and `fe=` are how callgrind reports code inlined from another file, so
            // taking them as the current file is what makes this attribution inline-accurate
            // rather than merely inline-tolerant.
            file = Some(resolve(rest, &mut files));
            inclusive_next = false;
        } else if let Some(rest) = line.strip_prefix("cfi=") {
            // A called file: it declares a name the file table may be asked for later, and
            // it does not change where the costs below belong.
            resolve(rest, &mut files);
        } else if let Some(rest) = line.strip_prefix("cfn=") {
            resolve(rest, &mut functions);
        } else if let Some(rest) = line.strip_prefix("fn=") {
            function = Some(resolve(rest, &mut functions));
            inclusive_next = false;
        } else if line.starts_with("calls=") {
            inclusive_next = true;
        } else if line
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_digit() || matches!(first, '+' | '-' | '*'))
        {
            if inclusive_next {
                inclusive_next = false;
                continue;
            }
            let instructions = line
                .split_whitespace()
                .skip(1)
                .nth(column)
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let owner = file
                .as_deref()
                .and_then(|path| attribute_path(path, workspace))
                .or_else(|| {
                    function
                        .as_deref()
                        .and_then(|name| attribute_symbol(name, workspace))
                });
            match owner {
                Some(entry) if entry.engine => {
                    cost.engine = cost.engine.saturating_add(instructions);
                    if instructions > 0 {
                        reached.insert(entry.name.clone());
                    }
                }
                Some(_) => cost.harness = cost.harness.saturating_add(instructions),
                None => cost.runtime = cost.runtime.saturating_add(instructions),
            }
        }
    }

    let declared = text
        .lines()
        .find_map(|line| {
            line.strip_prefix("summary:")
                .or_else(|| line.strip_prefix("totals:"))
        })
        .and_then(|totals| totals.split_whitespace().nth(column))
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| {
            ProfileError::new(
                "the callgrind output declares no total, so there is nothing to check the \
                 per-function costs against",
            )
        })?;
    if cost.total() != declared {
        return Err(ProfileError::new(format!(
            "the per-function costs add up to {} and callgrind's own total is {declared}; this gate read the file wrongly and the figure is not about the engine",
            cost.total()
        )));
    }
    Ok(Attribution { cost, reached })
}

/// Resolves callgrind's name compression: `(7) some::name` declares, `(7)` refers back.
fn resolve(rest: &str, names: &mut BTreeMap<String, String>) -> String {
    let trimmed = rest.trim();
    let Some(after) = trimmed.strip_prefix('(') else {
        return trimmed.to_owned();
    };
    let Some((id, name)) = after.split_once(')') else {
        return trimmed.to_owned();
    };
    let name = name.trim();
    if name.is_empty() {
        return names.get(id).cloned().unwrap_or_default();
    }
    names.insert(id.to_owned(), name.to_owned());
    name.to_owned()
}

/// Runs both tools over every workload and reports what they found.
///
/// # Errors
///
/// [`ProfileError`] when the workspace cannot be resolved, the workload cannot be built,
/// valgrind is not on the path, a tool exits non-zero, or its output cannot be read. Fails
/// closed throughout: a workload that could not be measured is not a workload to leave out
/// of the table.
pub fn measure(root: &Path) -> Result<ProfileReport, ProfileError> {
    let metadata = crate::run_cargo_metadata(root)
        .map_err(|error| ProfileError::new(format!("could not resolve the workspace: {error}")))?;
    let graph = crate::graph::PackageGraph::from_cargo_metadata(&metadata)
        .map_err(|error| ProfileError::new(format!("could not parse cargo metadata: {error}")))?;
    let workspace = workspace_crates(&crate::coverage::crate_roots(&graph))?;

    let binary = build_workload(root)?;
    let output = root.join(OUTPUT_DIR);
    std::fs::create_dir_all(&output).map_err(|error| {
        ProfileError::new(format!(
            "could not create {} for the tool output: {error}",
            output.display()
        ))
    })?;
    let rows = WORKLOADS
        .iter()
        .map(|workload| measure_workload(&binary, &output, *workload, &workspace))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ProfileReport { rows })
}

/// Builds the workload binary under [`PROFILE`] and answers where it landed.
fn build_workload(root: &Path) -> Result<PathBuf, ProfileError> {
    let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    let status = Command::new(cargo)
        .current_dir(root)
        .args([
            "build",
            "--locked",
            "-p",
            "xtask",
            "--bin",
            "xtask",
            "--profile",
            PROFILE,
        ])
        .status()
        .map_err(|error| ProfileError::new(format!("could not run cargo: {error}")))?;
    if !status.success() {
        return Err(ProfileError::new(format!(
            "the workload did not build under profile `{PROFILE}`"
        )));
    }
    let binary = root.join("target").join(PROFILE).join("xtask");
    if binary.is_file() {
        Ok(binary)
    } else {
        Err(ProfileError::new(format!(
            "the workload built and {} is not there",
            binary.display()
        )))
    }
}

/// One workload, under both tools.
fn measure_workload(
    binary: &Path,
    output: &Path,
    workload: Workload,
    workspace: &[WorkspaceCrate],
) -> Result<WorkloadProfile, ProfileError> {
    let dhat_out = output.join(format!("dhat-{}.json", workload.name));
    let callgrind_out = output.join(format!("callgrind-{}.out", workload.name));
    let units = run_tool(
        binary,
        workload,
        &[
            "--tool=dhat".to_owned(),
            FULL_PATHS.to_owned(),
            format!("--dhat-out-file={}", dhat_out.display()),
        ],
    )?;
    let again = run_tool(
        binary,
        workload,
        &[
            "--tool=callgrind".to_owned(),
            FULL_PATHS.to_owned(),
            format!("--callgrind-out-file={}", callgrind_out.display()),
            // Instruction counts alone. The cache and branch simulators model a processor
            // this code will never run on, and their numbers would read as facts about a
            // part rather than about a host.
            "--cache-sim=no".to_owned(),
            "--branch-sim=no".to_owned(),
        ],
    )?;
    // The two runs are the same deterministic workload, so a disagreement is a workload that
    // is not deterministic — which would make every figure here a figure about one run of it.
    if units != again {
        return Err(ProfileError::new(format!(
            "{}: the DHAT run completed {units} {}s and the callgrind run {again}; the workload is not deterministic, so neither figure is about it",
            workload.name, workload.unit
        )));
    }
    if units != u32::from(workload.units) {
        return Err(ProfileError::new(format!(
            "{}: the workload completed {units} {}s and WORKLOADS declares {}; the table and the workload disagree, and the table is not the measurement",
            workload.name, workload.unit, workload.units
        )));
    }

    let heap = parse_dhat(&read(&dhat_out)?, workspace)
        .map_err(|error| ProfileError::new(format!("{}: {error}", workload.name)))?;
    let attribution = parse_callgrind(&read(&callgrind_out)?, workspace)
        .map_err(|error| ProfileError::new(format!("{}: {error}", workload.name)))?;
    Ok(WorkloadProfile {
        workload: workload.name.to_owned(),
        what: workload.what.to_owned(),
        unit: workload.unit.to_owned(),
        units,
        reached: attribution.reached,
        heap,
        cost: attribution.cost,
    })
}

/// Runs `binary` under valgrind with `arguments`, and answers the units it reported.
fn run_tool(binary: &Path, workload: Workload, arguments: &[String]) -> Result<u32, ProfileError> {
    let output = Command::new("valgrind")
        .args(arguments)
        .arg(binary)
        .arg(WORKLOAD_COMMAND)
        .arg(workload.name)
        .output()
        .map_err(|error| {
            ProfileError::new(format!(
                "could not run valgrind: {error}. It is not part of the pinned toolchain, so it is installed by the pipeline (`apt-get install valgrind`) — a missing tool is an install failure rather than a gate that quietly passed."
            ))
        })?;
    if !output.status.success() {
        return Err(ProfileError::new(format!(
            "{}: the workload exited {} under {}\n{}",
            workload.name,
            output.status,
            arguments.first().map_or("valgrind", String::as_str),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    read_units(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        ProfileError::new(format!(
            "{}: the workload printed no unit count, so there is no denominator the figures are per",
            workload.name
        ))
    })
}

/// The unit count a workload printed, from its standard output.
#[must_use]
pub fn read_units(stdout: &str) -> Option<u32> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(UNITS_MARKER))
        .and_then(|count| count.trim().parse::<u32>().ok())
}

/// Reads a tool's output file.
fn read(path: &Path) -> Result<String, ProfileError> {
    std::fs::read_to_string(path).map_err(|error| {
        ProfileError::new(format!(
            "could not read {}: {error}. The tool exited zero and wrote nothing this gate can read, which is not a measurement that passed.",
            path.display()
        ))
    })
}

/// Runs one workload and prints the line the outer run reads.
///
/// This is the inner half of the command: the process the tools are pointed at.
///
/// # Errors
///
/// [`workload::WorkloadError`] when the name is not a workload, or the workload does not run
/// to its declared end.
pub fn run_workload(name: &str) -> Result<u32, workload::WorkloadError> {
    let units = workload::run(name)?;
    println!("{UNITS_MARKER}{units}");
    Ok(units)
}

/// Writes the report as JSON, for the CI artifact.
///
/// # Errors
///
/// [`ProfileError`] when the file cannot be written.
pub fn write_report(path: &Path, report: &ProfileReport) -> Result<(), ProfileError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            ProfileError::new(format!(
                "could not create {} for the profile report: {error}",
                parent.display()
            ))
        })?;
    }
    std::fs::write(path, to_json(report))
        .map_err(|error| ProfileError::new(format!("could not write {}: {error}", path.display())))
}

/// The report as JSON.
#[must_use]
pub fn to_json(report: &ProfileReport) -> String {
    let rows: Vec<Value> = report
        .rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "workload": row.workload,
                "what": row.what,
                "unit": row.unit,
                "units": row.units,
                "reached": row.reached,
                "engine_heap_bytes": row.heap.engine_bytes,
                "engine_heap_blocks": row.heap.engine_blocks,
                "harness_heap_bytes": row.heap.harness_bytes,
                "harness_heap_blocks": row.heap.harness_blocks,
                "runtime_heap_bytes": row.heap.runtime_bytes,
                "runtime_heap_blocks": row.heap.runtime_blocks,
                "engine_instructions": row.cost.engine,
                "harness_instructions": row.cost.harness,
                "runtime_instructions": row.cost.runtime,
                // Hundredths rather than a quotient, for the reason `wear` gives: a consumer
                // handed an integer quotient is handed less than was measured.
                "engine_instructions_per_unit_hundredths":
                    row.cost.engine_per_unit_hundredths(row.units),
                "verdict": row.verdict().to_string(),
            })
        })
        .collect();
    let document = serde_json::json!({
        "profile": PROFILE,
        "engine_heap_blocks_budget": ENGINE_HEAP_BLOCKS,
        "engine_crates": engine_crates(),
        "workloads": rows,
    });
    format!("{document:#}\n")
}

/// Reads a report written earlier by [`write_report`].
///
/// This is what makes the command testable without valgrind: a report produced once can be
/// gated again, exactly as `cargo xtask coverage --report` and `cargo xtask size --report`
/// each allow.
///
/// # Errors
///
/// [`ProfileError`] when the file cannot be read or is not a report this gate wrote.
pub fn read_report(path: &Path) -> Result<ProfileReport, ProfileError> {
    parse_report(&read(path)?)
}

/// [`read_report`], over the text rather than the file.
///
/// # Errors
///
/// [`ProfileError`] when the text is not a report this gate wrote.
pub fn parse_report(text: &str) -> Result<ProfileReport, ProfileError> {
    let document: Value = serde_json::from_str(text)
        .map_err(|error| ProfileError::new(format!("the profile report is not JSON: {error}")))?;
    let workloads = document
        .get("workloads")
        .and_then(Value::as_array)
        .ok_or_else(|| ProfileError::new("the profile report names no workloads"))?;
    let rows = workloads
        .iter()
        .map(|row| {
            let number = |key: &str| -> Result<u64, ProfileError> {
                row.get(key).and_then(Value::as_u64).ok_or_else(|| {
                    ProfileError::new(format!("a profile report row has no `{key}`"))
                })
            };
            Ok(WorkloadProfile {
                workload: row
                    .get("workload")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ProfileError::new("a profile report row has no name"))?
                    .to_owned(),
                what: row
                    .get("what")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                unit: row
                    .get("unit")
                    .and_then(Value::as_str)
                    .unwrap_or("unit")
                    .to_owned(),
                units: u32::try_from(number("units")?).unwrap_or(u32::MAX),
                reached: row
                    .get("reached")
                    .and_then(Value::as_array)
                    .ok_or_else(|| ProfileError::new("a profile report row has no `reached`"))?
                    .iter()
                    .filter_map(|name| name.as_str().map(str::to_owned))
                    .collect(),
                heap: Heap {
                    engine_bytes: number("engine_heap_bytes")?,
                    engine_blocks: number("engine_heap_blocks")?,
                    harness_bytes: number("harness_heap_bytes")?,
                    harness_blocks: number("harness_heap_blocks")?,
                    runtime_bytes: number("runtime_heap_bytes")?,
                    runtime_blocks: number("runtime_heap_blocks")?,
                },
                cost: Cost {
                    engine: number("engine_instructions")?,
                    harness: number("harness_instructions")?,
                    runtime: number("runtime_instructions")?,
                },
            })
        })
        .collect::<Result<Vec<_>, ProfileError>>()?;
    Ok(ProfileReport { rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace laid out where a checkout is, so that the path attribution has paths.
    fn roots() -> Vec<CrateRoot> {
        let mut roots: Vec<CrateRoot> = crate::policy::LAYERS
            .iter()
            .map(|layer| layer.name)
            .chain(crate::policy::TEST_SUPPORT_CRATES.iter().copied())
            .chain(crate::policy::MEASUREMENT_CRATES.iter().copied())
            .map(|name| CrateRoot {
                name: name.to_owned(),
                directory: PathBuf::from(format!("/w/crates/{name}")),
            })
            .collect();
        for name in crate::policy::HOST_TOOLS {
            roots.push(CrateRoot {
                name: (*name).to_owned(),
                directory: PathBuf::from(format!("/w/{name}")),
            });
        }
        roots
    }

    fn workspace() -> Vec<WorkspaceCrate> {
        workspace_crates(&roots()).expect("every crate this gate attributes by has a directory")
    }

    /// The bytes below are copied out of a real run of the `journal` workload under DHAT
    /// 3.22, trimmed to the program point that allocates the modelled part's media. Real
    /// rather than invented, because every rule in this module is a rule about a shape only
    /// the tool produces — and a fixture written from the documentation would agree with
    /// whatever the parser did.
    const MEDIA_STACK: &[&str] = &[
        "0x4846828: malloc (in /usr/libexec/valgrind/vgpreload_dhat-amd64-linux.so)",
        "0x24FDF2: <alloc::raw_vec::RawVecInner>::try_allocate_in (in /w/target/profiling/xtask)",
        "0x1F3C5D: with_capacity_in<u8, alloc::alloc::Global> (/rustc/8bab26f/library/alloc/src/vec/mod.rs:977)",
        "0x1F3C5D: alloc::vec::from_elem::<u8> (/rustc/8bab26f/library/alloc/src/vec/mod.rs:3708)",
        "0x1F3D81: <waymaker_fault::device::Device>::with_bit_rule (/w/crates/waymaker-fault/src/device.rs:122)",
        "0x1E6719: journal (/w/xtask/src/profile/workload.rs:120)",
        "0x1E6719: xtask::profile::workload::run (/w/xtask/src/profile/workload.rs:95)",
    ];

    /// The same shape with an engine crate where the harness one was — a `waymaker-flash`
    /// frame nearest the allocation, under the same std plumbing and the same caller.
    const ENGINE_STACK: &[&str] = &[
        "0x4846828: malloc (in /usr/libexec/valgrind/vgpreload_dhat-amd64-linux.so)",
        "0x1F3C5D: alloc::vec::from_elem::<u8> (/rustc/8bab26f/library/alloc/src/vec/mod.rs:3708)",
        "0x1F3D81: <waymaker_flash::append::Journal>::stage (/w/crates/waymaker-flash/src/append.rs:220)",
        "0x1E6719: journal (/w/xtask/src/profile/workload.rs:120)",
    ];

    /// A DHAT export over `points`, each a `(bytes, blocks, stack)`.
    fn dhat_export(points: &[(u64, u64, &[&str])]) -> String {
        let mut frames: Vec<&str> = Vec::new();
        let mut pps: Vec<Value> = Vec::new();
        for (bytes, blocks, stack) in points {
            let indices: Vec<usize> = stack
                .iter()
                .map(|frame| {
                    frames
                        .iter()
                        .position(|held| held == frame)
                        .unwrap_or_else(|| {
                            frames.push(frame);
                            frames.len() - 1
                        })
                })
                .collect();
            pps.push(serde_json::json!({ "tb": bytes, "tbk": blocks, "fs": indices }));
        }
        serde_json::json!({
            "dhatFileVersion": 2,
            "mode": "heap",
            "ftbl": frames,
            "pps": pps,
        })
        .to_string()
    }

    #[test]
    fn a_dhat_frame_splits_into_its_symbol_and_its_source() {
        let (symbol, location) = split_frame(
            "0x1F3D81: <waymaker_fault::device::Device>::with_bit_rule (/w/crates/waymaker-fault/src/device.rs:122)",
        );
        assert_eq!(symbol, "<waymaker_fault::device::Device>::with_bit_rule");
        assert_eq!(location, "/w/crates/waymaker-fault/src/device.rs:122");

        // A frame with no source at all still yields a symbol, because the symbol is what is
        // read when there is no path to read.
        let (symbol, location) = split_frame("0x4846828: malloc (in /usr/libexec/valgrind/x.so)");
        assert_eq!(symbol, "malloc");
        assert_eq!(location, "in /usr/libexec/valgrind/x.so");
    }

    #[test]
    fn a_generic_argument_is_not_the_crate_that_wrote_the_code() {
        // The defect this rule exists for, and it was a live one: the first run of this gate
        // failed the `driver` row over a four-byte `Vec<ActivityKind>` allocated by the
        // *harness* beside it. The frame is `alloc`'s code with a kernel type passed to it,
        // and a reader that searched the whole string charged it to `waymaker-core`.
        let workspace = workspace();
        let generic =
            "with_capacity_in<waymaker_core::activity::ActivityKind, alloc::alloc::Global>";
        assert_eq!(attribute_symbol(generic, &workspace), None);

        // And the shape it must still catch: the crate as the path root, plain and qualified.
        assert_eq!(
            attribute_symbol("waymaker_core::record::RecordRef::kind", &workspace)
                .map(|entry| entry.name.as_str()),
            Some("waymaker-core")
        );
        assert_eq!(
            attribute_symbol(
                "<waymaker_fault::device::Device as waymaker_flash::storage::StableStorage>::read",
                &workspace,
            )
            .map(|entry| entry.name.as_str()),
            // The crate that wrote the `impl`, not the crate that declared the trait.
            Some("waymaker-fault")
        );
    }

    #[test]
    fn a_mangled_name_is_read_by_the_reader_the_code_flash_gate_uses() {
        let workspace = workspace();
        // Valgrind demangles what it recognises and prints the rest as it found it. `size`
        // already has the reader for that case, and this is it being reached.
        let mangled =
            "0x1234: _RNvCs4Ab_14waymaker_flash5frame6encode (in /w/target/profiling/xtask)";
        assert_eq!(
            attribute_frame(mangled, &workspace).map(|entry| entry.name.as_str()),
            Some("waymaker-flash")
        );
    }

    #[test]
    fn an_inlined_body_is_charged_to_the_crate_that_wrote_it() {
        // The half a name cannot answer. An inlined frame loses its crate from the printed
        // symbol — Valgrind prints a bare `stage` — and keeps its source file, which is why
        // the path is asked first and why both tools are run with `--fullpath-after=`.
        let workspace = workspace();
        let inlined = "0x1F3D81: stage (/w/crates/waymaker-flash/src/append.rs:220)";
        assert_eq!(split_frame(inlined).0, "stage");
        assert_eq!(attribute_symbol("stage", &workspace), None);
        assert_eq!(
            attribute_frame(inlined, &workspace).map(|entry| entry.name.as_str()),
            Some("waymaker-flash")
        );
    }

    #[test]
    fn a_stack_is_attributed_to_its_innermost_workspace_frame() {
        // The direction the whole gate rests on. `waymaker-fault` models media in a `Vec`,
        // so engine code reaching it allocates through the model; charging that to the
        // engine would make this gate unpassable for a reason that is not a defect.
        let workspace = workspace();
        let heap = parse_dhat(&dhat_export(&[(24576, 1, MEDIA_STACK)]), &workspace)
            .expect("a real DHAT export");
        assert_eq!(heap.engine_blocks, 0);
        assert_eq!(heap.harness_blocks, 1);
        assert_eq!(heap.harness_bytes, 24576);
        assert_eq!(heap.total_blocks(), 1);
    }

    #[test]
    fn an_allocation_whose_innermost_workspace_frame_is_the_engine_fails_the_gate() {
        // The tooth. A gate that has never been observed failing is a gate nobody has
        // measured, and this is the one thing that says this one can: the same real stack
        // with an engine frame where the harness one was.
        let workspace = workspace();
        let heap = parse_dhat(&dhat_export(&[(24576, 1, ENGINE_STACK)]), &workspace)
            .expect("a real DHAT export");
        assert_eq!(heap.engine_blocks, 1);
        assert_eq!(heap.engine_bytes, 24576);

        let row = WorkloadProfile {
            workload: "journal".to_owned(),
            what: String::new(),
            unit: "effect".to_owned(),
            units: 8,
            reached: engine_crates().into_iter().map(str::to_owned).collect(),
            heap,
            cost: Cost {
                engine: 1,
                harness: 0,
                runtime: 0,
            },
        };
        assert_eq!(
            row.verdict(),
            Verdict::Allocated {
                blocks: 1,
                bytes: 24576
            }
        );
        assert!(
            ProfileReport::new(vec![row])
                .shortfall_report()
                .is_some_and(|report| report.contains("allocated 1 block")),
            "the gate reported a clean run over an engine allocation"
        );
    }

    #[test]
    fn a_zero_byte_allocation_is_still_an_allocation() {
        // Why the budget is in blocks. `malloc(0)` returns a pointer, and a firmware that
        // reached it has an allocator linked whatever the byte count says.
        let workspace = workspace();
        let heap = parse_dhat(&dhat_export(&[(0, 1, ENGINE_STACK)]), &workspace)
            .expect("a real DHAT export");
        assert_eq!((heap.engine_bytes, heap.engine_blocks), (0, 1));
        let row = WorkloadProfile {
            workload: "journal".to_owned(),
            what: String::new(),
            unit: "effect".to_owned(),
            units: 8,
            reached: engine_crates().into_iter().map(str::to_owned).collect(),
            heap,
            cost: Cost {
                engine: 1,
                harness: 0,
                runtime: 0,
            },
        };
        assert!(matches!(
            row.verdict(),
            Verdict::Allocated { blocks: 1, .. }
        ));
    }

    #[test]
    fn a_run_that_saw_no_allocation_at_all_is_unmeasurable_rather_than_clean() {
        // Every gate here fails closed. DHAT that saw nothing was not watching, and the zero
        // it prints for the engine is the zero it would print for a process that never
        // started.
        let row = WorkloadProfile {
            workload: "journal".to_owned(),
            what: String::new(),
            unit: "effect".to_owned(),
            units: 8,
            reached: engine_crates().into_iter().map(str::to_owned).collect(),
            heap: Heap::default(),
            cost: Cost {
                engine: 1,
                harness: 0,
                runtime: 0,
            },
        };
        assert!(matches!(row.verdict(), Verdict::Unmeasurable(_)));
    }

    #[test]
    fn a_run_that_attributed_no_instruction_to_the_engine_is_unmeasurable() {
        // The same rule for the other tool: an image with nothing to attribute by, or a
        // workload that never entered the engine, both land here rather than on a pass.
        let row = WorkloadProfile {
            workload: "journal".to_owned(),
            what: String::new(),
            unit: "effect".to_owned(),
            units: 8,
            reached: engine_crates().into_iter().map(str::to_owned).collect(),
            heap: Heap {
                runtime_blocks: 16,
                runtime_bytes: 28435,
                ..Heap::default()
            },
            cost: Cost {
                engine: 0,
                harness: 1,
                runtime: 1,
            },
        };
        assert!(matches!(row.verdict(), Verdict::Unmeasurable(_)));
    }

    #[test]
    fn a_workload_that_completed_no_effect_has_nothing_the_figures_are_per() {
        let row = WorkloadProfile {
            workload: "driver".to_owned(),
            what: String::new(),
            unit: "effect".to_owned(),
            units: 0,
            reached: engine_crates().into_iter().map(str::to_owned).collect(),
            heap: Heap {
                runtime_blocks: 16,
                ..Heap::default()
            },
            cost: Cost {
                engine: 1,
                harness: 0,
                runtime: 0,
            },
        };
        assert!(matches!(row.verdict(), Verdict::Unmeasurable(_)));
    }

    #[test]
    fn an_empty_report_and_a_missing_row_each_fail() {
        // A table nobody filled in is the emptiest version of a measurement that did not
        // happen, and a table with a row missing is the next emptiest.
        assert!(ProfileReport::new(Vec::new()).shortfall_report().is_some());
        let one = WorkloadProfile {
            workload: WORKLOADS
                .first()
                .map_or_else(String::new, |workload| workload.name.to_owned()),
            what: String::new(),
            unit: "effect".to_owned(),
            units: 8,
            reached: engine_crates().into_iter().map(str::to_owned).collect(),
            heap: Heap {
                runtime_blocks: 16,
                ..Heap::default()
            },
            cost: Cost {
                engine: 1,
                harness: 0,
                runtime: 0,
            },
        };
        let report = ProfileReport::new(vec![one]);
        assert!(
            report
                .shortfall_report()
                .is_some_and(|shortfall| shortfall.contains("has no row for it")),
            "a report missing a declared workload passed"
        );
    }

    #[test]
    fn a_dhat_export_that_is_not_a_heap_run_is_refused() {
        let workspace = workspace();
        let copy = serde_json::json!({ "mode": "copy", "ftbl": [], "pps": [] }).to_string();
        assert!(parse_dhat(&copy, &workspace).is_err());
        assert!(parse_dhat("{}", &workspace).is_err());
        // A program point missing a total is a parse this gate must not complete: a dropped
        // point is a smaller heap than the process had.
        let short =
            serde_json::json!({ "mode": "heap", "ftbl": [], "pps": [{ "tb": 4 }] }).to_string();
        assert!(parse_dhat(&short, &workspace).is_err());
    }

    /// Real callgrind 3.22 output, trimmed. Two functions in two crates, a call between
    /// them, and — deliberately — the id `7` used for both a file and a function, which is
    /// what a real run of this workspace does more than a hundred times.
    const CALLGRIND: &str = "\
version: 1
creator: callgrind-3.22.0
positions: line
events: Ir
summary: 330

ob=(1) /w/target/profiling/xtask
fl=(7) /w/crates/waymaker-flash/src/append.rs
fn=(7) <waymaker_flash::append::Journal>::stage
100 200
cfi=(9) /w/crates/waymaker-fault/src/device.rs
cfn=(11) <waymaker_fault::device::Device>::program
calls=1 122
100 1000
+2 30

fl=(9)
fn=(11)
122 100

fl=(13) /rustc/8bab26f/library/alloc/src/vec/mod.rs
fn=(15) with_capacity_in<waymaker_core::activity::ActivityKind, alloc::alloc::Global>
977 -
977 -
";

    #[test]
    fn callgrind_self_cost_excludes_the_inclusive_cost_of_a_call() {
        // The failure this parser is written against: the cost line after a `calls=` is the
        // inclusive cost of the callee, and summed as self cost it counts every callee again
        // at every level, so the figure grows with call depth. Here it is 1000 against a
        // 330-instruction process — an error nobody would notice as a plausible number.
        let workspace = workspace();
        let cost = parse_callgrind(CALLGRIND, &workspace)
            .expect("real callgrind output")
            .cost;
        assert_eq!(cost.engine, 230, "the call's inclusive cost was counted");
        assert_eq!(cost.harness, 100);
        assert_eq!(cost.runtime, 0);
        assert_eq!(cost.total(), 330);
    }

    #[test]
    fn callgrind_file_and_function_names_are_compressed_in_separate_tables() {
        // `fl=(7)` and `fn=(7)` are different names, and in a real run of this workspace the
        // two namespaces collide on more than a hundred ids. Read from one table, `fl=(9)`
        // resolves to a function name, no path matches, and the harness row lands in
        // `runtime` — the totals still add up, which is what makes it worth a test of its
        // own rather than a line in the one above.
        let workspace = workspace();
        let cost = parse_callgrind(CALLGRIND, &workspace)
            .expect("real callgrind output")
            .cost;
        assert_eq!(
            cost.harness, 100,
            "the second block's file resolved to something other than waymaker-fault"
        );
    }

    #[test]
    fn callgrind_costs_that_do_not_add_up_to_the_declared_total_are_refused() {
        // The cross-check, and it is the only thing that catches all three ways of
        // misreading this format at once.
        let workspace = workspace();
        let wrong = CALLGRIND.replace("summary: 330", "summary: 331");
        assert!(parse_callgrind(&wrong, &workspace).is_err());
        // And a file with no total at all: there is then nothing to check against, which is
        // a measurement that did not happen rather than one that passed.
        let untotalled = CALLGRIND.replace("summary: 330", "");
        assert!(parse_callgrind(&untotalled, &workspace).is_err());
    }

    #[test]
    fn callgrind_output_counting_something_other_than_instructions_is_refused() {
        let workspace = workspace();
        let cycles = CALLGRIND.replace("events: Ir", "events: Dr Dw");
        assert!(parse_callgrind(&cycles, &workspace).is_err());
        assert!(parse_callgrind("nothing at all", &workspace).is_err());
    }

    #[test]
    fn a_crate_the_gate_attributes_by_and_cannot_place_is_an_error() {
        // Fails closed for `coverage`'s reason. An unattributed crate does not attribute to
        // nobody — it attributes to whoever called it, which on this gate is the harness, so
        // a missing directory would quietly widen what the engine is allowed to do.
        let mut roots = roots();
        roots.retain(|root| root.name != "waymaker-core");
        assert!(workspace_crates(&roots).is_err());
    }

    #[test]
    fn the_engine_is_the_layers_and_the_crates_that_claim_to_be_allocation_free() {
        let engine = engine_crates();
        for layer in crate::policy::LAYERS {
            assert!(engine.contains(&layer.name), "{} is not gated", layer.name);
        }
        for name in crate::policy::NO_STD_TEST_SUPPORT_CRATES {
            assert!(
                engine.contains(name),
                "{name} claims no allocation and is not gated"
            );
        }
        // And the harness is not the engine, which is what makes the engine's zero a
        // statement rather than an accounting identity.
        let workspace = workspace();
        for name in ["waymaker-fault", "waymaker-spec", "xtask"] {
            assert_eq!(
                workspace
                    .iter()
                    .find(|entry| entry.name == name)
                    .map(|entry| entry.engine),
                Some(false),
                "{name} is gated as engine code"
            );
        }
    }

    #[test]
    fn a_report_survives_being_written_and_read_back() {
        // What `--report` rests on, and what the CI artifact is worth.
        let report = ProfileReport::new(vec![WorkloadProfile {
            workload: "journal".to_owned(),
            what: "the writer".to_owned(),
            unit: "effect".to_owned(),
            units: 8,
            reached: engine_crates().into_iter().map(str::to_owned).collect(),
            heap: Heap {
                harness_blocks: 1,
                harness_bytes: 24576,
                runtime_blocks: 15,
                runtime_bytes: 3859,
                ..Heap::default()
            },
            cost: Cost {
                engine: 544_281,
                harness: 182_729,
                runtime: 581_316,
            },
        }]);
        let read = parse_report(&to_json(&report)).expect("a report this gate wrote");
        assert_eq!(read, report);
        assert_eq!(
            read.row("journal").map(WorkloadProfile::verdict),
            Some(Verdict::Clean)
        );
        assert!(parse_report("{}").is_err());
    }

    #[test]
    fn a_per_unit_figure_is_never_less_than_what_was_measured() {
        // The invariant, asserted as an invariant rather than as a literal — which is how
        // the truncating version passed its own test. Codex found it: 544_281 over 8 is
        // 68_035.125, and truncating at the hundredth publishes 68_035.12, which is still
        // less than was measured. A cost figure that understates is worse than none.
        let cost = Cost {
            engine: 544_281,
            harness: 0,
            runtime: 0,
        };
        let hundredths = cost
            .engine_per_unit_hundredths(8)
            .expect("a run with units in it");
        assert_eq!(hundredths, 6_803_513);
        assert_eq!(render_hundredths(Some(hundredths)), "68035.13");
        assert!(
            u128::from(hundredths) * 8 >= u128::from(cost.engine) * 100,
            "{hundredths} per unit over 8 units understates {}",
            cost.engine
        );
        // Over a range of divisors, not the one the report happens to use: the defect was
        // invisible at every divisor that divides exactly.
        for units in 1_u32..=64 {
            let figure = cost
                .engine_per_unit_hundredths(units)
                .expect("a run with units in it");
            assert!(
                u128::from(figure) * u128::from(units) >= u128::from(cost.engine) * 100,
                "{figure} per unit over {units} units understates {}",
                cost.engine
            );
        }
        // An exact division must not be inflated by the rounding either.
        assert_eq!(
            Cost {
                engine: 800,
                harness: 0,
                runtime: 0
            }
            .engine_per_unit_hundredths(8),
            Some(10_000)
        );
        assert_eq!(cost.engine_per_unit_hundredths(0), None);
        assert_eq!(render_hundredths(None), "-");
    }

    #[test]
    fn an_engine_crate_no_workload_executes_fails_the_gate() {
        // The hole Codex found on the first review, as a test. `engine_crates()` had six
        // names; the workloads linked and executed four. An allocation in either of the
        // other two produced no frame, so both rows stayed clean and the gate covered them
        // in name only.
        let short: BTreeSet<String> = engine_crates()
            .into_iter()
            .take(2)
            .map(str::to_owned)
            .collect();
        let rows: Vec<WorkloadProfile> = WORKLOADS
            .iter()
            .map(|workload| WorkloadProfile {
                workload: workload.name.to_owned(),
                what: String::new(),
                unit: workload.unit.to_owned(),
                units: u32::from(workload.units),
                reached: short.clone(),
                heap: Heap {
                    runtime_blocks: 16,
                    ..Heap::default()
                },
                cost: Cost {
                    engine: 1,
                    harness: 0,
                    runtime: 0,
                },
            })
            .collect();
        let report = ProfileReport::new(rows);
        let unreached = report.unreached_engine_crates();
        assert_eq!(unreached.len(), engine_crates().len() - 2);
        let shortfall = report
            .shortfall_report()
            .expect("a report that measured four of six gated crates is not a pass");
        for name in &unreached {
            assert!(
                shortfall.contains(name.as_str()),
                "the gate did not name {name} as unreached"
            );
        }
        assert!(shortfall.contains("measured nothing"));
    }

    #[test]
    fn a_run_that_reaches_every_engine_crate_has_nothing_unreached() {
        // The other direction, so the check above cannot pass by always reporting something.
        let rows: Vec<WorkloadProfile> = WORKLOADS
            .iter()
            .map(|workload| WorkloadProfile {
                workload: workload.name.to_owned(),
                what: String::new(),
                unit: workload.unit.to_owned(),
                units: u32::from(workload.units),
                reached: engine_crates().into_iter().map(str::to_owned).collect(),
                heap: Heap {
                    runtime_blocks: 16,
                    ..Heap::default()
                },
                cost: Cost {
                    engine: 1,
                    harness: 0,
                    runtime: 0,
                },
            })
            .collect();
        let report = ProfileReport::new(rows);
        assert!(report.unreached_engine_crates().is_empty());
        assert!(report.shortfall_report().is_none());
    }

    #[test]
    fn reach_is_read_out_of_the_callgrind_costs() {
        // Not declared beside the row: a crate is reached because instructions were
        // attributed to it, which is the same reading the cost column comes from.
        let workspace = workspace();
        let attribution = parse_callgrind(CALLGRIND, &workspace).expect("real callgrind output");
        assert!(attribution.reached.contains("waymaker-flash"));
        // The harness is not the engine, so it is not reach even though it ran.
        assert!(!attribution.reached.contains("waymaker-fault"));
        // And a crate with a cost line of zero is not reached by it.
        let zeroed = CALLGRIND
            .replace("100 200", "100 0")
            .replace("+2 30", "+2 0");
        let zeroed = zeroed.replace("summary: 330", "summary: 100");
        assert!(
            !parse_callgrind(&zeroed, &workspace)
                .expect("real callgrind output")
                .reached
                .contains("waymaker-flash"),
            "a crate that executed no instruction was counted as reached"
        );
    }

    #[test]
    fn the_unit_count_is_read_from_the_workload_rather_than_assumed() {
        assert_eq!(read_units("waymaker-profile-workload: units=8\n"), Some(8));
        assert_eq!(
            read_units("noise\nwaymaker-profile-workload: units=2"),
            Some(2)
        );
        assert_eq!(read_units("nothing the outer run can use"), None);
    }

    #[test]
    fn every_declared_workload_has_a_name_the_inner_command_answers_to() {
        // A row nothing can run is a row that would fail the whole command at the first
        // measurement, and this is that found at `cargo test` instead.
        for workload in WORKLOADS {
            assert!(
                workload::run(workload.name).is_ok(),
                "{} is declared and does not run",
                workload.name
            );
        }
        assert!(workload::run("not-a-workload").is_err());
    }

    #[test]
    fn a_workload_completes_the_units_the_table_declares() {
        // The check `measure` makes against a live run, made here too so that a workload and
        // its row drifting apart is a red `cargo test` rather than a red pipeline.
        for workload in WORKLOADS {
            assert_eq!(
                workload::run(workload.name).ok(),
                Some(u32::from(workload.units)),
                "{} does not complete what WORKLOADS declares",
                workload.name
            );
        }
    }

    #[test]
    fn the_rendered_table_names_every_workload_and_says_what_the_figures_are_not() {
        let report = ProfileReport::new(
            WORKLOADS
                .iter()
                .map(|workload| WorkloadProfile {
                    workload: workload.name.to_owned(),
                    what: workload.what.to_owned(),
                    unit: workload.unit.to_owned(),
                    units: u32::from(workload.units),
                    reached: engine_crates().into_iter().map(str::to_owned).collect(),
                    heap: Heap {
                        runtime_blocks: 16,
                        ..Heap::default()
                    },
                    cost: Cost {
                        engine: 1,
                        harness: 0,
                        runtime: 0,
                    },
                })
                .collect(),
        );
        let rendered = report.render();
        for workload in WORKLOADS {
            assert!(
                rendered.contains(workload.name),
                "{} is not in the table",
                workload.name
            );
        }
        assert!(rendered.contains("no cycle count on any part"));
        assert!(report.shortfall_report().is_none());
    }
}
