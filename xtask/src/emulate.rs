//! The emulated boot: the rig executed on ARM, and a census the harness checks.
//!
//! # What this gate is for
//!
//! `CLAUDE.md` records the limit this closes, and records it about the workspace's own
//! firmware stages: *"`cargo build --lib` produces an rlib and never links, so no global
//! allocator is required and an `extern crate alloc` under any of them compiles clean."* The
//! `rig-firmware`, `drive-firmware` and `firmware` stages establish that the code *compiles*
//! for the part. Nothing established that any of it runs.
//!
//! So this builds [`PACKAGE`] — a linked image with a reset vector and a memory map — for
//! both of [`MACHINES`], starts each under QEMU, and requires the rig's three moments to have
//! happened and said so.
//!
//! # Why two machines, and why their answers must agree
//!
//! [`MACHINES`] names a Cortex-M0 and a Cortex-M4, which are **ARMv6-M** and **ARMv7E-M** —
//! the two instruction sets `docs::HARDWARE_TARGETS`'s two power-cut rows are stated over. A
//! rig that had only ever executed on one encoding has measured that encoding.
//!
//! The two censuses are then required to be *equal*, which is the sharper half. Both runs
//! drive the same seed through the same deterministic plan, so a difference is not a
//! tolerance — it is the rig behaving differently on two instruction sets, which is exactly
//! the class of defect a host test binary cannot see and the class a fleet would find. A
//! `u64` shift lowered through `compiler_builtins` on one core and an instruction on the
//! other, an alignment assumption, a `usize` narrowing: none of those show up as anything
//! else here.
//!
//! # How it fails closed
//!
//! Every way this run can decline to be a measurement is a failure rather than a skip, which
//! is the rule [`crate::profile`] states as "a measurement that did not happen is not a
//! measurement that passed". QEMU absent, the image absent, a non-zero exit, a run that
//! outlives [`TIMEOUT`], **and — the one that matters — a run that exited zero having printed
//! nothing**. An image whose `main` returned before it reached the rig exits exactly the way
//! a complete one does, so the exit code is the weakest of the checks here and the census is
//! the strongest.
//!
//! # What it does not establish
//!
//! A board. Neither machine has a NOR part, a supply that can be removed, a reset-cause
//! register or a backup domain, so `docs::HARDWARE_TARGETS` stays `Not run` and this stage
//! may not be cited to move a row of it. See
//! [ADR 0040](https://github.com/madmax983/waymaker/blob/main/docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md).
//!
//! # What it now measures about the stack
//!
//! Each image paints its own unused stack before the rig runs and reports how far the paint
//! was disturbed after — [`StackUsage`], read from a third census line. This is a real,
//! on-target figure where before there was none, and it closes a limit `CLAUDE.md`'s budgets
//! section names: §04's runtime RAM figure is stack-blind for call-chain depth, and this is
//! the depth for *this* image, on *this* run. It is not the same figure. This image links
//! `waymaker-rig` and `waymaker-conformance` alongside the three layers, so what is reported
//! is the whole call chain's depth, not the engine's share of it. It is a lower bound rather
//! than an exact reading — a frame can reserve bytes it never writes, which the paint pattern
//! cannot see — and it fails closed only for running out of room, against no invented §04
//! ceiling, never compared between the two machines: different cores compile the same source
//! into different instructions, so a different byte count is expected rather than a finding.
//! See
//! [ADR 0041](https://github.com/madmax983/waymaker/blob/main/docs/adr/0041-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md).

use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The crate that is built, started and read.
pub const PACKAGE: &str = "waymaker-emu";

/// The feature its binary is behind.
///
/// The size probe's trick, for the size probe's reason: without it `cargo build --workspace`
/// would try to link a `#![no_main]` firmware binary for the host.
pub const FEATURE: &str = "emu";

/// The cargo profile the images are linked under.
///
/// `release`, which is what design document §04's budgets are measured against — so the code
/// that runs here is the code an image would carry, optimised the way it would be optimised.
/// A debug image would execute different instructions and would not fit the tighter of the
/// two memory maps.
pub const PROFILE: &str = "release";

/// The emulator this gate drives.
pub const EMULATOR: &str = "qemu-system-arm";

/// The linker arguments the images need, and nothing else does.
///
/// Passed through `RUSTFLAGS` on this one child `cargo` rather than written into
/// `.cargo/config.toml`, and that is a rule rather than a preference: a target-scoped
/// `rustflags` entry there would put `-Tlink.x` on **every** `thumbv6m-none-eabi` build in
/// the workspace, including the three layers and the size probe, neither of which has a
/// `cortex-m-rt` to supply one — so the code-flash measurement would be taken against an
/// image linked differently from the one the budget is stated for. `cargo-config-profile` is
/// the rule that would otherwise have to grow a fourth thing to forbid.
///
/// `-L` names the directory `memory.x` lives in: `link.x` comes from `cortex-m-rt`'s own
/// build script and `INCLUDE`s it, and a linker search path is how it is found.
pub const LINK_ARGS: &str = "-C link-arg=-Tlink.x -L crates/waymaker-emu";

/// How long one emulated boot may take before the run is failed.
///
/// Generous against the second or so both boots really take, and finite on purpose: an image
/// that hangs would otherwise consume the job's whole timeout and report as infrastructure
/// rather than as a defect. A panic in the rig exits non-zero rather than hanging — the
/// image installs a `#[panic_handler]` that does — so reaching this is a livelock, which is
/// a finding of its own.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// The prefix every line the image writes carries.
///
/// Declared in both places and compared, rather than shared through a dependency: `xtask`
/// depending on the emulated image would put a `#![no_main]` firmware crate into the host
/// build. `check_emulation_boot` is the rule that fails a build in which the two drift.
pub const PREFIX: &str = "waymaker-emu:";

/// One core the rig is executed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Machine {
    /// Short identifier, used in the report and in a violation.
    pub name: &'static str,
    /// The `-machine` QEMU is given.
    pub qemu: &'static str,
    /// The Rust target the image is built for.
    pub target: &'static str,
    /// The architecture the core implements, for the report's human half.
    pub architecture: &'static str,
    /// Why this core is in the list.
    pub why: &'static str,
}

/// The cores the rig is required to run on.
///
/// Two, and their *architectures* are the reason rather than their part numbers. Neither is a
/// row of `docs::HARDWARE_TARGETS`: a Cortex-M0 is not a Cortex-M0+ — same instruction set,
/// different core — and QEMU has no M0+ machine at all. What the pair buys is that every
/// instruction the rig executes has been executed under both encodings Waymaker is built for.
pub const MACHINES: &[Machine] = &[
    Machine {
        name: "cortex-m0",
        qemu: "microbit",
        target: "thumbv6m-none-eabi",
        architecture: "ARMv6-M",
        why: "the architecture design document §04's budgets are stated for, and the target every firmware stage builds",
    },
    Machine {
        name: "cortex-m4",
        qemu: "mps2-an386",
        target: "thumbv7em-none-eabi",
        architecture: "ARMv7E-M",
        why: "a second encoding, because a rig that only ever ran on one has measured that one",
    },
];

/// What one boot said it did.
///
/// Parsed from the image's own lines rather than assumed from an exit code, because the two
/// answer different questions: an exit code says the image stopped, and this says what it had
/// done when it stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Census {
    /// Conformance cases the media model passed.
    pub cases_passed: u32,
    /// Conformance cases the geometry made unaskable.
    pub cases_exempt: u32,
    /// Iterations driven to a stop.
    pub iterations: u32,
    /// Iterations the plan's cutter fired in.
    pub cuts: u32,
    /// Cut runs carried to their end by a resume.
    pub resumes: u32,
    /// Cut runs whose journal had no append point.
    pub unextendable: u32,
    /// Resumes that redelivered an effect whose schedule had no completion.
    pub redeliveries: u32,
    /// Verdicts that were a pass.
    pub verdicts: u32,
    /// Effects dispatched, across every iteration and every resume.
    pub dispatched: u32,
}

impl Census {
    /// What the harness demands of a boot before it will call it a measurement.
    ///
    /// The image checks its own census too, and this is not a duplicate of that check: the
    /// image's copy is what makes it exit non-zero, and this one is what catches an image
    /// that never ran the check — one that exited zero from somewhere else, or whose output
    /// was truncated, or that was built from a source the boot logic had been removed from.
    /// A gate that trusted the subject's own verdict would be reading a claim rather than
    /// taking a measurement.
    #[must_use]
    pub fn shortfall(&self) -> Option<String> {
        if self.cases_passed == 0 {
            return Some("no conformance case passed, so the media model the rig ran over was never asked whether it obeys §12".to_owned());
        }
        if self.iterations == 0 {
            return Some("no iteration ran, so the image started and the rig did not".to_owned());
        }
        if self.cuts == 0 {
            return Some(
                "no iteration was cut, so only clean runs were driven and recovery answered nothing"
                    .to_owned(),
            );
        }
        if self.resumes + self.unextendable != self.cuts {
            return Some(format!(
                "{} iterations were cut and {} were resumed or refused, so a cut run was left unaccounted for",
                self.cuts,
                self.resumes + self.unextendable
            ));
        }
        if self.verdicts != self.iterations + self.resumes {
            return Some(format!(
                "{} runs were judged and {} verdicts passed, so a run reached no verdict",
                self.iterations + self.resumes,
                self.verdicts
            ));
        }
        None
    }
}

/// How much of the painted stack one run disturbed.
///
/// Read from the image's own third census line, alongside [`Census`] rather than folded into
/// it: the two are parsed together and always answered together, but they are never compared
/// the same way. [`Report::shortfall`]'s cross-machine equality check reads [`Census`] alone
/// — two cores executing the same deterministic plan must agree about what the rig did, and
/// this is what a difference there would mean. They are not expected to use the same number
/// of stack bytes doing it: a Cortex-M0 and a Cortex-M4 compile the same source into
/// different instructions, so a different figure here is expected and not itself a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StackUsage {
    /// Bytes disturbed, from the deepest point the run reached up to the stack pointer
    /// reading taken before it started.
    pub used: u32,
    /// Bytes the paint covered: the whole region between the linker's `_stack_end` and that
    /// same reading.
    pub available: u32,
}

impl StackUsage {
    /// Why this is not a measurement, if it is not.
    ///
    /// Two ways, both "a measurement that did not happen is not a measurement that passed":
    /// a region reported as empty painted nothing and scanned nothing, and a region disturbed
    /// all the way down could not tell a run that used every byte from one that used one more
    /// than this image could see.
    #[must_use]
    pub fn shortfall(&self) -> Option<String> {
        if self.available == 0 {
            return Some(
                "the stack region reported as empty, so nothing was painted and nothing was measured"
                    .to_owned(),
            );
        }
        if self.used >= self.available {
            return Some(format!(
                "used {} of {} available bytes: the paint was disturbed all the way down, so the true high-water mark could not be read",
                self.used, self.available
            ));
        }
        None
    }
}

/// What became of one machine's boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The image ran, said what it did, and the census holds.
    Ran(Census, StackUsage),
    /// The run is not a measurement, and this is why.
    ///
    /// One variant rather than several, for [`crate::profile::Verdict::Unmeasurable`]'s
    /// reason: a reader wants the sentence, and every way of getting here fails the build.
    Unmeasurable(String),
}

impl Outcome {
    /// The census, if there is one.
    #[must_use]
    pub const fn census(&self) -> Option<&Census> {
        match self {
            Self::Ran(census, _) => Some(census),
            Self::Unmeasurable(_) => None,
        }
    }

    /// The stack usage, if there is one.
    #[must_use]
    pub const fn stack(&self) -> Option<&StackUsage> {
        match self {
            Self::Ran(_, stack) => Some(stack),
            Self::Unmeasurable(_) => None,
        }
    }
}

/// One machine and what its boot came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The core.
    pub machine: Machine,
    /// What its boot came to.
    pub outcome: Outcome,
    /// Everything the image wrote, kept so that a failure is investigable from the log
    /// rather than only named.
    pub output: String,
}

/// Every machine's boot, and whether the run as a whole is a pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// One row per machine, in [`MACHINES`] order.
    pub rows: Vec<Row>,
}

impl Report {
    /// Why this run is not a pass, if it is not.
    ///
    /// Three demands. Every machine ran; every machine's census holds; and the machines
    /// **agree**. The third is the one a single-machine gate cannot make, and it is the
    /// reason there are two: the plan is deterministic, so two cores that disagree about what
    /// the rig did are two cores executing the rig differently.
    #[must_use]
    pub fn shortfall(&self) -> Option<String> {
        if self.rows.len() != MACHINES.len() {
            return Some(format!(
                "{} machines are declared and {} ran; a machine that produced no row is a machine nothing watched",
                MACHINES.len(),
                self.rows.len()
            ));
        }
        for row in &self.rows {
            match &row.outcome {
                Outcome::Unmeasurable(why) => {
                    return Some(format!("{}: {why}", row.machine.name));
                }
                Outcome::Ran(census, stack) => {
                    if let Some(shortfall) = census.shortfall() {
                        return Some(format!("{}: {shortfall}", row.machine.name));
                    }
                    if let Some(shortfall) = stack.shortfall() {
                        return Some(format!("{}: {shortfall}", row.machine.name));
                    }
                }
            }
        }
        let mut seen: Option<(&str, &Census)> = None;
        for row in &self.rows {
            let Some(census) = row.outcome.census() else {
                continue;
            };
            match seen {
                None => seen = Some((row.machine.name, census)),
                Some((first, expected)) if expected != census => {
                    return Some(format!(
                        "{first} and {} disagree about what the rig did: {expected:?} against {census:?}. The plan is deterministic, so this is the rig behaving differently on two instruction sets",
                        row.machine.name
                    ));
                }
                Some(_) => {}
            }
        }
        None
    }

    /// The report as a table.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("emulated boot\n");
        for row in &self.rows {
            let _ = writeln!(
                out,
                "  {:<10} {:<12} {:<9} {}",
                row.machine.name,
                row.machine.qemu,
                row.machine.architecture,
                match &row.outcome {
                    Outcome::Ran(census, stack) => format!(
                        "cases {}+{} · iterations {} · cuts {} · resumes {} · redeliveries {} · verdicts {} · effects {} · stack {} of {}",
                        census.cases_passed,
                        census.cases_exempt,
                        census.iterations,
                        census.cuts,
                        census.resumes,
                        census.redeliveries,
                        census.verdicts,
                        census.dispatched,
                        stack.used,
                        stack.available
                    ),
                    Outcome::Unmeasurable(why) => format!("unmeasurable: {why}"),
                }
            );
        }
        out.push_str(
            "\nWhat this establishes is that the rig executes on both instruction sets, and that both\n\
             agree about what it did. What it does not establish is a board: neither machine has a NOR\n\
             part, a supply to remove, a reset-cause register or a backup domain, so every row of the\n\
             hardware table stays `Not run`. See ADR 0040.\n\n\
             Stack is the whole image's own call-chain depth on this run, not the engine's share of it,\n\
             and the two machines are not required to agree: different cores compile the same source\n\
             into different instructions. See ADR 0041.\n",
        );
        out
    }
}

/// Reads a boot's own lines into a census and its stack usage.
///
/// Pure, so that every way an image can fail to say what it did is a case in
/// `tests` rather than a QEMU run inside a test. Returns `None` when a required line is
/// absent — which is what an image that exited zero having printed nothing looks like.
#[must_use]
pub fn parse(output: &str) -> Option<(Census, StackUsage)> {
    let mut census = Census::default();
    let mut stack = StackUsage::default();
    let mut cases = false;
    let mut rig = false;
    let mut stack_seen = false;
    let mut ok = false;
    for line in output.lines() {
        let Some(rest) = line.trim().strip_prefix(PREFIX) else {
            continue;
        };
        let rest = rest.trim();
        if rest == "ok" {
            ok = true;
        } else if let Some(fields) = rest.strip_prefix("cases ") {
            census.cases_passed = field(fields, "passed")?;
            census.cases_exempt = field(fields, "exempt")?;
            cases = true;
        } else if let Some(fields) = rest.strip_prefix("rig ") {
            census.iterations = field(fields, "iterations")?;
            census.cuts = field(fields, "cuts")?;
            census.resumes = field(fields, "resumes")?;
            census.unextendable = field(fields, "unextendable")?;
            census.redeliveries = field(fields, "redeliveries")?;
            census.verdicts = field(fields, "verdicts")?;
            census.dispatched = field(fields, "dispatched")?;
            rig = true;
        } else if let Some(fields) = rest.strip_prefix("stack ") {
            stack.used = field(fields, "used")?;
            stack.available = field(fields, "available")?;
            stack_seen = true;
        }
    }
    // All four, and `ok` is not enough on its own: the image writes it last, so a truncated
    // run has the counts and not the word, and an image that printed only the word did not
    // run. Requiring the set is what makes a partial read a failure.
    (cases && rig && stack_seen && ok).then_some((census, stack))
}

/// Reads `name=<number>` out of a space-separated field list.
fn field(fields: &str, name: &str) -> Option<u32> {
    fields.split_whitespace().find_map(|field| {
        field
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
            .and_then(|value| value.parse().ok())
    })
}

/// Builds both images, starts both machines, and reports what they said.
///
/// # Errors
///
/// A message when the run could not be started at all — no QEMU, or an image that would not
/// build. A machine that started and misbehaved is a [`Row`] rather than an error, so that
/// the other machine still runs and the report shows both.
pub fn measure(root: &Path) -> Result<Report, String> {
    which(EMULATOR)?;
    let mut rows = Vec::new();
    for machine in MACHINES {
        let outcome = match build(root, *machine).and_then(|image| start(*machine, &image)) {
            Ok((parsed, output)) => {
                rows.push(Row {
                    machine: *machine,
                    outcome: parsed.map_or_else(
                        || {
                            Outcome::Unmeasurable(
                                "the image ran and printed no census this gate can read, which is not a measurement that passed".to_owned(),
                            )
                        },
                        |(census, stack)| Outcome::Ran(census, stack),
                    ),
                    output,
                });
                continue;
            }
            Err(why) => Outcome::Unmeasurable(why),
        };
        rows.push(Row {
            machine: *machine,
            outcome,
            output: String::new(),
        });
    }
    Ok(Report { rows })
}

/// Fails unless `tool` is on the path.
///
/// Before anything is built, so that a runner without QEMU says so in a second rather than
/// after two firmware builds.
fn which(tool: &str) -> Result<(), String> {
    Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| {
            format!(
                "could not run `{tool}`: {error}. No rustup profile carries it and this gate fails closed rather than skipping: a boot that did not happen is not a boot that passed."
            )
        })
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(format!("`{tool} --version` exited {status}"))
            }
        })
}

/// Links `machine`'s image and answers where it landed.
fn build(root: &Path, machine: Machine) -> Result<PathBuf, String> {
    let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    let status = Command::new(cargo)
        .current_dir(root)
        .env("RUSTFLAGS", LINK_ARGS)
        .args([
            "build",
            "--locked",
            "--profile",
            PROFILE,
            "-p",
            PACKAGE,
            "--features",
            FEATURE,
            "--target",
            machine.target,
        ])
        .status()
        .map_err(|error| format!("could not run cargo: {error}"))?;
    if !status.success() {
        return Err(format!(
            "{PACKAGE} did not link for {}; an image that does not link is the first thing this stage exists to catch",
            machine.target
        ));
    }
    let image = root
        .join("target")
        .join(machine.target)
        .join(PROFILE)
        .join(PACKAGE);
    if image.is_file() {
        Ok(image)
    } else {
        Err(format!(
            "the image built and {} is not there",
            image.display()
        ))
    }
}

/// Starts `image` on `machine` and reads what it wrote.
fn start(machine: Machine, image: &Path) -> Result<(Option<(Census, StackUsage)>, String), String> {
    let mut child = Command::new(EMULATOR)
        .args([
            "-machine",
            machine.qemu,
            "-nographic",
            "-semihosting-config",
            "enable=on,target=native",
            "-kernel",
        ])
        .arg(image)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start {EMULATOR}: {error}"))?;

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Err(error) => return Err(format!("could not wait for {EMULATOR}: {error}")),
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "the image did not stop within {}s. It installs a panic handler that exits, so this is a livelock rather than a panic",
                        TIMEOUT.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };

    // Three lines of output, so a pipe cannot fill and deadlock the wait above.
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output);
    }
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut output);
    }

    if !status.success() {
        // The image's own lines joined onto one, because this string becomes a cell of the
        // report's table and a multi-line cell is a table nobody can read. The image says why
        // it refused — its census check runs before this one — so the sentence is worth
        // carrying rather than dropping.
        let wrote: Vec<&str> = output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        return Err(format!(
            "the image exited {status}, having written: {}",
            if wrote.is_empty() {
                "nothing".to_owned()
            } else {
                wrote.join(" / ")
            }
        ));
    }
    Ok((parse(&output), output))
}

/// The rule id this module's gate reports under.
pub const RULE: &str = "emulation-boot";

/// The crate-root attributes the emulated image must carry.
///
/// `#![no_std]` and `#![no_main]` are what make it firmware rather than a host binary that
/// happens to be built for a target — and a `#![no_main]` binary is the only kind that can
/// have a reset vector placed at all.
pub const REQUIRED_ATTRIBUTES: &[&str] = &["#![no_std]", "#![no_main]"];

/// The identifier the crate is allowed to name, and the keyword it is not.
///
/// Most of the exception this crate carries is that a *macro* it invokes expands to
/// `unsafe`: `#[cortex_m_rt::entry]` writes the exported symbol the reset vector points at,
/// and `debug::exit` performs the semihosting call. Neither is hand-written here, and this is
/// what says so — the crate may name the lint (`unsafe_code`, in the `allow` it declares) and
/// may write the keyword only where [`PERMITTED_UNSAFE_FUNCTIONS`] names. Without it,
/// `#![allow(unsafe_code)]` would be a licence for the whole crate rather than for two macro
/// expansions and one measurement, and the one place in this workspace where `unsafe` is
/// permitted would be the one place nothing checks.
pub const PERMITTED_LINT_NAME: &str = "unsafe_code";

/// The one file [`PERMITTED_UNSAFE_FUNCTIONS`] is read from.
///
/// The crate-relative path rather than a bare file name: `check_image_attributes` matches
/// `main.rs` by suffix to *impose* an obligation, where every file named that way is held to
/// it and a decoy costs nothing. This suffix *grants* a permission, so a short one would let a
/// `stack.rs` filed in any directory of the crate borrow it — a sharper version of the same
/// limit, named in `CLAUDE.md`'s "what is not checked" rather than left implied.
pub const STACK_MODULE: &str = "crates/waymaker-emu/src/stack.rs";

/// The only functions in [`STACK_MODULE`] that may write the `unsafe` keyword.
///
/// The reset vector and the semihosting exit are macro expansions; this is the third
/// exception and the last, and it is hand-written rather than expanded, so it is pinned by
/// name instead of being invisible to this scan the way a macro's own `unsafe` is. ADR 0041
/// is the reason either function needs it at all: a raw fill and a raw read, over the region
/// between the linker's `_stack_end` and a stack-pointer reading taken before either runs.
///
/// A name alone is not the pin: `check_no_handwritten_unsafe` also requires each to be
/// declared exactly once — `crate::source::declaration_count`, `effect-protocol`'s own
/// guard against "a decoy above the real one is what a first-match scan reads" — and requires
/// every `unsafe` inside to sit at that function body's own nesting depth,
/// `crate::source::nesting_depth_at`, the guard `effect-protocol` also carries against a
/// nested item or a closure hiding a second, unrelated `unsafe`.
pub const PERMITTED_UNSAFE_FUNCTIONS: &[&str] = &["paint", "high_water_mark"];

/// Fails a build in which the emulated image stops being the thing this gate started.
///
/// Five halves, and each one is a way a green `emulate` stage could mean less than it reads
/// as.
///
/// The *attributes* half: the image's crate root declares [`REQUIRED_ATTRIBUTES`], and
/// declares its `unsafe_code` exception with a `reason`. A firmware image that quietly became
/// a host binary would still run under nothing, and an unreasoned `allow` is the exception
/// the workspace manifest says must be reviewable.
///
/// The *`unsafe`* half: no file of the crate writes the keyword outside
/// [`PERMITTED_UNSAFE_FUNCTIONS`] and the one linker-symbol block [`STACK_MODULE`] permits.
/// See [`PERMITTED_LINT_NAME`] — this is what keeps the workspace's one `allow(unsafe_code)`
/// scoped to two macro expansions and one measurement rather than to a crate.
///
/// The *prefix* half: the image and [`PREFIX`] agree. The harness reads the image's lines to
/// decide whether the run was a measurement, so a space added on one side and not the other
/// would turn every future run into "the image printed no census" — which fails closed, and
/// fails for a reason nobody would find quickly.
///
/// The *manifest* half: the binary is behind [`FEATURE`]. Without `required-features`,
/// `cargo build --workspace` and `cargo clippy --all-targets` link a `#![no_main]` firmware
/// binary for the host, where there is no reset vector to place — so the whole workspace's
/// build stages go red for a crate that is not what they are about.
///
/// The *machines* half: every core in [`MACHINES`] has its Rust target pinned in
/// `rust-toolchain.toml` and named by a pipeline stage. A machine added to the table with no
/// stage behind it is a core this gate claims to run on and never does; a target the
/// toolchain does not pin is a stage that fails on a fresh checkout for a reason that is not
/// a defect.
///
/// What it cannot see is whether the image *does* anything: that is
/// [`Report::shortfall`] and [`Census::shortfall`], which read what the run printed. A
/// scanner and a run answer different questions, and both are here.
#[must_use]
pub fn check_emulation_boot(
    manifest: Option<&str>,
    sources: &[crate::size::LayerSource],
    toolchain: Option<&str>,
    stages: &[crate::pipeline::Stage],
) -> Vec<crate::Violation> {
    let mut violations = Vec::new();
    let files: Vec<&crate::size::LayerSource> = sources
        .iter()
        .filter(|source| source.crate_name == PACKAGE)
        .collect();

    if files.is_empty() {
        violations.push(crate::Violation::new(
            RULE,
            PACKAGE,
            "the emulated image is not in the workspace, so the `emulate` stage has nothing to start and this rule checks nothing",
        ));
        return violations;
    }

    violations.extend(check_image_attributes(&files));
    violations.extend(check_no_handwritten_unsafe(&files));
    violations.extend(check_prefix_agrees(&files));
    violations.extend(check_binary_is_gated(manifest));
    violations.extend(check_machines_are_reachable(toolchain, stages));
    violations
}

/// The crate root carries the firmware attributes and a reasoned exception.
fn check_image_attributes(files: &[&crate::size::LayerSource]) -> Vec<crate::Violation> {
    let mut violations = Vec::new();
    let Some(root) = files.iter().find(|source| source.path.ends_with("main.rs")) else {
        violations.push(crate::Violation::new(
            RULE,
            PACKAGE,
            "has no `main.rs`, so the image the `emulate` stage starts is not where this rule looks",
        ));
        return violations;
    };
    let code = crate::source::code_only(&root.contents);
    for attribute in REQUIRED_ATTRIBUTES {
        if !code.contains(attribute) {
            violations.push(crate::Violation::new(
                RULE,
                root.path.clone(),
                format!(
                    "does not declare `{attribute}`; an image without it is a host binary, and a host binary has no reset vector for a machine to start"
                ),
            ));
        }
    }
    // Read on the comment-stripped source rather than on the raw file. The module
    // documentation *quotes* the workspace manifest's sentence about
    // `#![allow(unsafe_code)]`, so a scan of the raw text finds the prose before the
    // attribute and reports a missing `reason` on a crate that has one — which is what a
    // first version of this rule did. `code_only` also removes string literals, which costs
    // nothing here: the check is that a `reason` is *declared*, and its wording is a
    // reviewer's business.
    let Some(allow) = attribute_body(&code, "#![allow(") else {
        violations.push(crate::Violation::new(
            RULE,
            root.path.clone(),
            format!(
                "declares no `#![allow({PERMITTED_LINT_NAME}, reason = ..)]`; this crate cannot compile without the exception, and an exception nothing states is one nobody reviewed"
            ),
        ));
        return violations;
    };
    if !allow.contains(PERMITTED_LINT_NAME) {
        violations.push(crate::Violation::new(
            RULE,
            root.path.clone(),
            format!("declares an `#![allow(..)]` that does not name `{PERMITTED_LINT_NAME}`"),
        ));
    } else if !allow.contains("reason") {
        violations.push(crate::Violation::new(
            RULE,
            root.path.clone(),
            format!(
                "allows `{PERMITTED_LINT_NAME}` without a `reason`; the workspace manifest asks for \"a reviewable one-line `#![allow(unsafe_code)]` plus an ADR\", and half of that is the sentence"
            ),
        ));
    }
    violations
}

/// The body of the first attribute in `contents` opening with `opener`.
fn attribute_body<'a>(contents: &'a str, opener: &str) -> Option<&'a str> {
    let start = contents.find(opener)?.checked_add(opener.len())?;
    let rest = contents.get(start..)?;
    let end = rest.find(")]")?;
    rest.get(..end)
}

/// No file of the crate writes the `unsafe` keyword outside [`PERMITTED_UNSAFE_FUNCTIONS`].
fn check_no_handwritten_unsafe(files: &[&crate::size::LayerSource]) -> Vec<crate::Violation> {
    let mut violations = Vec::new();
    for source in files {
        let code = crate::source::code_only(&source.contents);
        let permitted = source
            .path
            .replace('\\', "/")
            .ends_with(STACK_MODULE)
            .then(|| PermittedStackSpans::collect(&code));
        let bytes = code.as_bytes();
        let mut at = 0;
        while let Some(found) = code.get(at..).and_then(|rest| rest.find("unsafe")) {
            let start = at.saturating_add(found);
            let end = start.saturating_add("unsafe".len());
            let before = start
                .checked_sub(1)
                .and_then(|index| bytes.get(index).copied());
            let after = bytes.get(end).copied();
            let is_word_start = before.is_none_or(|byte| !is_identifier_byte(byte));
            // `unsafe_code` is the lint's name and the one thing every file may say. The
            // keyword is never followed by an identifier byte, so the two cannot be confused.
            let is_the_lint = after.is_some_and(is_identifier_byte);
            let is_permitted = permitted.as_ref().is_some_and(|spans| spans.covers(start));
            if is_word_start && !is_the_lint && !is_permitted {
                violations.push(crate::Violation::new(
                    RULE,
                    source.path.clone(),
                    "writes the `unsafe` keyword. The exception this crate carries is for two macro expansions — the reset vector and the semihosting exit — and one measurement — the stack high-water mark, confined to the two functions PERMITTED_UNSAFE_FUNCTIONS names and the one linker-symbol block STACK_MODULE's own shape allows — and hand-written `unsafe` anywhere else is the thing nothing else would catch",
                ));
                break;
            }
            at = end;
        }
    }
    violations
}

/// Whether `byte` can appear inside a Rust identifier.
const fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The linker symbol naming the stack's lowest address, and the one the extern block must
/// name.
///
/// `_stack_end`, not `__ebss`: naming the wrong one would be invisible to every rule here, so
/// this is the one place the correct symbol is written down and checked rather than only
/// argued in [`crate::stack`]'s own documentation — which is a module this crate does not
/// have; the name is repeated for the reader who reaches this file first.
const STACK_FLOOR_SYMBOL: &str = "_stack_end";

/// The linker symbol naming the stack's highest address, and the extern block's other
/// permitted name.
///
/// Named so `crate::stack::clamp_to_stack_region` can hold a caller's reading inside memory
/// this image actually owns, whatever the reading says — [ADR 0041]'s soundness fix for
/// `paint` and `high_water_mark` being safe `pub fn`s over a caller-supplied bound.
///
/// [ADR 0041]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0041-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md
const STACK_CEILING_SYMBOL: &str = "_stack_start";

/// Every place [`PERMITTED_UNSAFE_FUNCTIONS`] and the one linker-symbol block permit `unsafe`
/// in a `stack.rs`, computed once per file rather than re-derived per occurrence.
struct PermittedStackSpans {
    /// The absolute byte offset of the one legitimate `unsafe` keyword in each permitted
    /// function that has exactly one, at that function's own nesting depth zero.
    ///
    /// A single offset per function, not a body range: `covers` checking containment alone
    /// would permit a *second*, unrelated `unsafe` statement placed beside the legitimate one
    /// at the same depth — a sibling rather than a nested decoy, and one nesting depth cannot
    /// tell apart on its own. Requiring exactly one such offset and matching it exactly closes
    /// that: a function with two depth-zero `unsafe` keywords permits neither, because which
    /// one is real would be a guess.
    function_unsafe_at: Vec<usize>,
    /// The byte range of the one well-formed `unsafe extern "C" { .. }` block, if the file
    /// has one — declaring only [`STACK_FLOOR_SYMBOL`] and nothing else.
    extern_block: Option<(usize, usize)>,
}

impl PermittedStackSpans {
    /// Reads every permitted span out of `code`, a `stack.rs` already through
    /// [`crate::source::code_only`].
    fn collect(code: &str) -> Self {
        let function_unsafe_at = PERMITTED_UNSAFE_FUNCTIONS
            .iter()
            .filter_map(|name| {
                let header = format!("fn {name}");
                // `declaration_count` rather than trusting the first match:
                // `crate::source::braced_body` takes the first boundary match and the first
                // `{` after it, so a bodiless `fn paint(..);` signature declared earlier in
                // the file — a trait method, an `extern` declaration — would resolve the span
                // onto whatever block follows it instead. A function declared more than once
                // is refused the same way: which one is "the" permitted body would be a
                // guess, and this rule does not guess. Neither check alone is enough: a lone
                // bodiless signature has a `declaration_count` of exactly one too, so
                // `has_a_body` is what actually tells the two apart.
                if crate::source::declaration_count(code, &header) != 1
                    || !has_a_body(code, &header)
                {
                    return None;
                }
                let body = crate::source::braced_body(code, &header)?;
                let (from, _) = span_of(code, body);
                sole_depth_zero_unsafe(body).map(|offset| from.saturating_add(offset))
            })
            .collect();
        Self {
            function_unsafe_at,
            extern_block: extern_block_span(code),
        }
    }

    /// Whether the `unsafe` starting at `start` in `code` is one of the spans this file
    /// permits.
    fn covers(&self, start: usize) -> bool {
        if self
            .extern_block
            .is_some_and(|(from, to)| start >= from && start < to)
        {
            return true;
        }
        self.function_unsafe_at.contains(&start)
    }
}

/// The byte offset of the one `unsafe` keyword in `body` that sits at the body's own nesting
/// depth zero, if there is exactly one.
///
/// Depth zero: not inside a nested `fn`, `impl` or `mod` declared within the body, and not
/// inside a closure or a call argument list either — `effect-protocol`'s own guard against a
/// step "in a closure nothing runs", the same shape of decoy met here for the same reason.
/// Two or more such occurrences is refused rather than picking the first: a sibling `unsafe`
/// placed beside the legitimate one is a decoy depth alone cannot tell from the real one, so
/// requiring exactly one is what actually closes it.
fn sole_depth_zero_unsafe(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut at = 0;
    let mut found = None;
    while let Some(offset) = body.get(at..).and_then(|rest| rest.find("unsafe")) {
        let start = at.saturating_add(offset);
        let end = start.saturating_add("unsafe".len());
        let before = start
            .checked_sub(1)
            .and_then(|index| bytes.get(index).copied());
        let after = bytes.get(end).copied();
        let is_word_start = before.is_none_or(|byte| !is_identifier_byte(byte));
        let is_the_lint = after.is_some_and(is_identifier_byte);
        if is_word_start && !is_the_lint && crate::source::nesting_depth_at(body, start) == 0 {
            if found.is_some() {
                return None;
            }
            found = Some(start);
        }
        at = end;
    }
    found
}

/// Whether `code`'s one declaration of `header` — a token-boundary match, as
/// [`crate::source::declaration_count`] counts it — reaches a `{` before it reaches a `;`.
///
/// A real function's signature cannot contain a bare `;`; a bodiless one — a trait method, an
/// `extern "C"` declaration — always ends with one before any `{` of its own. Without this, a
/// header with `declaration_count` of exactly one but no body of its own would still let
/// [`crate::source::braced_body`] resolve onto whatever block happens to follow it.
fn has_a_body(code: &str, header: &str) -> bool {
    let continues = |character: char| character.is_alphanumeric() || character == '_';
    code.match_indices(header).any(|(index, _)| {
        let before = code
            .get(..index)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|character| !continues(character));
        let rest = code.get(index + header.len()..).unwrap_or_default();
        let after = rest
            .chars()
            .next()
            .is_none_or(|character| !continues(character));
        if !(before && after) {
            return false;
        }
        match (rest.find('{'), rest.find(';')) {
            (Some(brace), Some(semicolon)) => brace < semicolon,
            (Some(_), None) => true,
            (None, _) => false,
        }
    })
}

/// The byte range of `body`, a substring of `code`.
///
/// `body` is always a slice of `code` here — [`crate::source::braced_body`] only ever returns
/// one — so the subtraction below is two offsets into the one allocation, never a comparison
/// of unrelated pointers.
fn span_of(code: &str, body: &str) -> (usize, usize) {
    let start = body.as_ptr() as usize - code.as_ptr() as usize;
    (start, start.saturating_add(body.len()))
}

/// The byte range of the one well-formed `unsafe extern "C" { .. }` block in `code`, if it has
/// one.
///
/// Well-formed means: the block declares [`STACK_FLOOR_SYMBOL`] and exactly one `static`, and
/// no `fn` — which is what stops `unsafe extern "C" fn trap_handler() { .. }`, a foreign
/// *function* item whose `unsafe` also reads as "extern" immediately following it, from being
/// read as this block. A second `unsafe extern` block declaring something else is scored on
/// its own content and refused on its own account, so nothing here needs to also count how
/// many such blocks the file has.
fn extern_block_span(code: &str) -> Option<(usize, usize)> {
    let mut at = 0;
    while let Some(found) = code.get(at..).and_then(|rest| rest.find("unsafe extern")) {
        let keyword_start = at.saturating_add(found);
        let after_keyword = keyword_start.saturating_add("unsafe".len());
        let before_ok = keyword_start
            .checked_sub(1)
            .and_then(|index| code.as_bytes().get(index).copied())
            .is_none_or(|byte| !is_identifier_byte(byte));
        let rest = code.get(after_keyword..).unwrap_or_default();
        let trimmed = rest.trim_start();
        // A foreign block opens with `{` once its ABI string, if any, is skipped; a foreign
        // *function* opens with `fn`. Whichever comes first in the raw text — after "extern"
        // and its optional `"C"` — decides which this is, so the ABI string cannot be used to
        // push a `{` in front of a `fn` this scan would otherwise catch.
        let opens_block = trimmed
            .trim_start_matches("extern")
            .trim_start()
            .trim_start_matches('"')
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .trim_start_matches('"')
            .trim_start()
            .starts_with('{');
        if before_ok && opens_block {
            if let Some(body) = crate::source::braced_body(rest, "extern") {
                let (from, to) = span_of(code, body);
                let inner = &code[from..to];
                let names_floor = crate::source::names_identifier(inner, STACK_FLOOR_SYMBOL);
                let names_ceiling = crate::source::names_identifier(inner, STACK_CEILING_SYMBOL);
                let two_statics = crate::source::declaration_count(inner, "static") == 2;
                let no_fn = crate::source::declaration_count(inner, "fn") == 0;
                if names_floor && names_ceiling && two_statics && no_fn {
                    // The span offered to callers starts at the `unsafe` keyword itself, so a
                    // `covers` check against the keyword's own occurrence succeeds.
                    return Some((keyword_start, to));
                }
            }
        }
        at = after_keyword;
    }
    None
}

/// The image declares the prefix the harness reads.
fn check_prefix_agrees(files: &[&crate::size::LayerSource]) -> Vec<crate::Violation> {
    let needle = format!("\"{PREFIX}\"");
    if files.iter().any(|source| source.contents.contains(&needle)) {
        return Vec::new();
    }
    vec![crate::Violation::new(
        RULE,
        PACKAGE,
        format!(
            "declares no {needle}; the harness reads the image's own lines to decide whether a boot was a measurement, so a prefix the two disagree about turns every run into \"the image printed no census\""
        ),
    )]
}

/// The binary is behind [`FEATURE`].
fn check_binary_is_gated(manifest: Option<&str>) -> Vec<crate::Violation> {
    let Some(manifest) = manifest else {
        return vec![crate::Violation::new(
            RULE,
            PACKAGE,
            "has no manifest this gate can read",
        )];
    };
    let Ok(document) = manifest.parse::<toml::Table>() else {
        return vec![crate::Violation::new(RULE, PACKAGE, "is not valid TOML")];
    };
    let gated = document
        .get("bin")
        .and_then(toml::Value::as_array)
        .is_some_and(|bins| {
            bins.iter().all(|bin| {
                bin.get("required-features")
                    .and_then(toml::Value::as_array)
                    .is_some_and(|features| {
                        features
                            .iter()
                            .filter_map(toml::Value::as_str)
                            .any(|feature| feature == FEATURE)
                    })
            })
        });
    if gated {
        Vec::new()
    } else {
        vec![crate::Violation::new(
            RULE,
            PACKAGE,
            format!(
                "declares a `[[bin]]` that is not behind `required-features = [\"{FEATURE}\"]`; without it every host build in the workspace tries to link a `#![no_main]` firmware binary"
            ),
        )]
    }
}

/// Every machine has a pinned target and a stage that builds for it.
fn check_machines_are_reachable(
    toolchain: Option<&str>,
    stages: &[crate::pipeline::Stage],
) -> Vec<crate::Violation> {
    let mut violations = Vec::new();
    let targets: Vec<String> = toolchain
        .and_then(|toolchain| toolchain.parse::<toml::Table>().ok())
        .and_then(|document| {
            document
                .get("toolchain")
                .and_then(|section| section.get("targets"))
                .and_then(toml::Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(toml::Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
        })
        .unwrap_or_default();
    let runs_the_gate = stages
        .iter()
        .any(|stage| stage.command.contains("xtask emulate"));
    if !runs_the_gate {
        violations.push(crate::Violation::new(
            RULE,
            crate::pipeline::WORKFLOW_PATH,
            "no pipeline stage runs `cargo xtask emulate`, so the image is built by nothing and started by nothing",
        ));
    }
    for machine in MACHINES {
        if !targets.iter().any(|target| target == machine.target) {
            violations.push(crate::Violation::new(
                RULE,
                crate::pipeline::TOOLCHAIN_PATH,
                format!(
                    "does not pin `{}` in [toolchain] targets, which the `{}` machine's image is built for",
                    machine.target, machine.name
                ),
            ));
        }
    }
    violations
}

/// Fixtures describing an emulated image every half of [`check_emulation_boot`] accepts.
///
/// Public to the crate's tests for [`crate::size::tests_support`]'s reason: a test about one
/// half of this rule takes a clean image and breaks exactly that half, which is the only way
/// to show the half is wired in when the rule id is already fired by a sibling.
#[cfg(test)]
pub mod tests_support {
    use super::{FEATURE, PACKAGE, PREFIX};

    /// A manifest whose binary is behind the feature.
    #[must_use]
    pub fn clean_manifest() -> String {
        format!(
            "[package]\nname = \"{PACKAGE}\"\n\n[[bin]]\nname = \"{PACKAGE}\"\npath = \"src/main.rs\"\nrequired-features = [\"{FEATURE}\"]\n\n[features]\ndefault = []\n{FEATURE} = []\n"
        )
    }

    /// A crate root carrying the firmware attributes and a reasoned exception.
    #[must_use]
    pub fn clean_root() -> String {
        format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\npub const PREFIX: &str = \"{PREFIX}\";\n"
        )
    }

    /// The image's sources, as the collector hands them over.
    #[must_use]
    pub fn clean_sources() -> Vec<crate::size::LayerSource> {
        vec![crate::size::LayerSource {
            crate_name: PACKAGE.to_owned(),
            path: format!("crates/{PACKAGE}/src/main.rs"),
            contents: clean_root(),
        }]
    }

    /// The literal, shipped `stack.rs`.
    ///
    /// Not a caricature of its shape — the file itself, read at compile time. A hand-written
    /// stand-in would drift from the real module the day either changed without the other,
    /// and this is the one test in the suite that must answer for the actual file the
    /// `emulate` stage links.
    #[must_use]
    pub fn real_stack_module() -> String {
        include_str!("../../crates/waymaker-emu/src/stack.rs").to_owned()
    }

    /// A minimal `stack.rs` carrying exactly the shape this rule permits.
    ///
    /// [`super::PERMITTED_UNSAFE_FUNCTIONS`] and the linker-symbol block, for tests that
    /// construct a decoy beside it rather than testing the shape itself —
    /// [`real_stack_module`] already does that.
    #[must_use]
    pub fn clean_stack_module() -> String {
        "unsafe extern \"C\" {\n    static _stack_end: u8;\n    static _stack_start: u8;\n}\n\
         pub fn paint(depth_from: usize) {\n    unsafe {\n        core::ptr::write_volatile(depth_from as *mut u8, 0xA5);\n    }\n}\n\
         pub fn high_water_mark(depth_from: usize) -> u32 {\n    let deepest = unsafe { core::ptr::read_volatile(depth_from as *const u8) };\n    deepest as u32\n}\n"
            .to_owned()
    }

    /// [`clean_sources`] with `stack_module` added at [`super::STACK_MODULE`]'s own path.
    #[must_use]
    pub fn sources_with_stack_module(stack_module: String) -> Vec<crate::size::LayerSource> {
        let mut sources = clean_sources();
        sources.push(crate::size::LayerSource {
            crate_name: PACKAGE.to_owned(),
            path: super::STACK_MODULE.to_owned(),
            contents: stack_module,
        });
        sources
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::STAGES;
    use crate::size::LayerSource;

    /// Exactly what the image writes, which is what makes this a test about the format
    /// rather than about a string a test invented.
    fn clean_output() -> String {
        format!(
            "{PREFIX} cases passed=21 exempt=2\n{PREFIX} rig iterations=12 cuts=12 resumes=12 unextendable=0 redeliveries=8 verdicts=24 dispatched=42\n{PREFIX} stack used=1024 available=12000\n{PREFIX} ok\n"
        )
    }

    fn clean_census() -> Census {
        Census {
            cases_passed: 21,
            cases_exempt: 2,
            iterations: 12,
            cuts: 12,
            resumes: 12,
            unextendable: 0,
            redeliveries: 8,
            verdicts: 24,
            dispatched: 42,
        }
    }

    fn clean_stack_usage() -> StackUsage {
        StackUsage {
            used: 1024,
            available: 12000,
        }
    }

    fn ran(name: &'static str, census: Census, stack: StackUsage) -> Row {
        let machine = MACHINES
            .iter()
            .find(|machine| machine.name == name)
            .copied()
            .unwrap_or(MACHINES[0]);
        Row {
            machine,
            outcome: Outcome::Ran(census, stack),
            output: String::new(),
        }
    }

    fn clean_report() -> Report {
        Report {
            rows: MACHINES
                .iter()
                .map(|machine| ran(machine.name, clean_census(), clean_stack_usage()))
                .collect(),
        }
    }

    fn toolchain_pinning_every_machine() -> String {
        let targets: Vec<&str> = MACHINES.iter().map(|machine| machine.target).collect();
        format!("[toolchain]\nchannel = \"1.97\"\ntargets = {targets:?}\n")
    }

    fn check(sources: &[LayerSource]) -> Vec<crate::Violation> {
        check_emulation_boot(
            Some(&tests_support::clean_manifest()),
            sources,
            Some(&toolchain_pinning_every_machine()),
            STAGES,
        )
    }

    /// The image's root with `find` replaced by `replacement`, so a test can break one half.
    fn root_with(find: &str, replacement: &str) -> Vec<LayerSource> {
        vec![LayerSource {
            crate_name: PACKAGE.to_owned(),
            path: format!("crates/{PACKAGE}/src/main.rs"),
            contents: tests_support::clean_root().replace(find, replacement),
        }]
    }

    #[test]
    fn a_complete_boot_parses_into_the_census_it_printed() {
        assert_eq!(
            parse(&clean_output()),
            Some((clean_census(), clean_stack_usage()))
        );
    }

    #[test]
    fn a_boot_that_printed_nothing_is_not_a_census() {
        // The failure this whole gate exists for: an image whose `main` returned before it
        // reached the rig exits exactly the way a complete one does.
        assert_eq!(parse(""), None);
    }

    #[test]
    fn a_boot_that_printed_only_the_last_word_is_not_a_census() {
        assert_eq!(parse(&format!("{PREFIX} ok\n")), None);
    }

    #[test]
    fn a_truncated_boot_is_not_a_census() {
        // The counts are there and the word is not, which is what a run cut off mid-write
        // leaves. Accepting it would report on a rig that may not have finished.
        let truncated = clean_output().replace(&format!("{PREFIX} ok\n"), "");
        assert_eq!(parse(&truncated), None);
    }

    #[test]
    fn a_line_missing_a_field_is_not_a_census() {
        // A field dropped from the image and not from the parser: fail closed rather than
        // default the number to zero, which `Census::shortfall` would then report as a rig
        // that did nothing — the right verdict for the wrong reason.
        let missing = clean_output().replace(" cuts=12", "");
        assert_eq!(parse(&missing), None);
    }

    #[test]
    fn the_emulators_own_noise_is_ignored() {
        let noisy = format!("qemu: warning: something\n{}", clean_output());
        assert_eq!(parse(&noisy), Some((clean_census(), clean_stack_usage())));
    }

    #[test]
    fn a_boot_with_no_stack_line_is_not_a_census() {
        // The fourth required line, added beside `cases`, `rig` and `ok`: a run this gate
        // cannot read a stack figure from is not a run that measured one.
        let missing =
            clean_output().replace(&format!("{PREFIX} stack used=1024 available=12000\n"), "");
        assert_eq!(parse(&missing), None);
    }

    #[test]
    fn a_clean_stack_usage_has_no_shortfall() {
        assert_eq!(clean_stack_usage().shortfall(), None);
    }

    #[test]
    fn a_stack_reported_empty_is_refused() {
        let stack = StackUsage {
            available: 0,
            ..clean_stack_usage()
        };
        assert!(stack.shortfall().is_some());
    }

    #[test]
    fn a_stack_used_to_the_edge_of_what_was_painted_is_refused() {
        // The paint was disturbed all the way down, so the true high-water mark — which may
        // be deeper still — could not be read. Reported rather than assumed clean.
        let stack = StackUsage {
            used: 12000,
            available: 12000,
        };
        assert!(stack.shortfall().is_some());
    }

    #[test]
    fn a_clean_census_has_no_shortfall() {
        assert_eq!(clean_census().shortfall(), None);
    }

    #[test]
    fn a_census_with_no_conformance_case_is_refused() {
        let census = Census {
            cases_passed: 0,
            ..clean_census()
        };
        assert!(census.shortfall().is_some());
    }

    #[test]
    fn a_census_with_no_iteration_is_refused() {
        let census = Census {
            iterations: 0,
            ..clean_census()
        };
        assert!(census.shortfall().is_some());
    }

    #[test]
    fn a_census_with_no_cut_is_refused() {
        // The sharpest of the five. A boot that drove only clean runs never asked recovery
        // anything, and it would otherwise look exactly like a boot that did.
        let census = Census {
            cuts: 0,
            resumes: 0,
            verdicts: 12,
            ..clean_census()
        };
        assert!(census.shortfall().is_some());
    }

    #[test]
    fn a_cut_run_that_was_never_resumed_is_refused() {
        let census = Census {
            resumes: 11,
            ..clean_census()
        };
        assert!(census.shortfall().is_some());
    }

    #[test]
    fn a_run_that_reached_no_verdict_is_refused() {
        let census = Census {
            verdicts: 23,
            ..clean_census()
        };
        assert!(census.shortfall().is_some());
    }

    #[test]
    fn a_cut_refused_for_want_of_an_append_point_still_accounts_for_itself() {
        // ADR 0018's anti-bricking rule is not a failure of the rig, and the census counts it
        // separately so that a boot in which *every* cut landed there could not read as a
        // boot that resumed.
        let census = Census {
            resumes: 10,
            unextendable: 2,
            verdicts: 22,
            ..clean_census()
        };
        assert_eq!(census.shortfall(), None);
    }

    #[test]
    fn a_clean_report_has_no_shortfall() {
        assert_eq!(clean_report().shortfall(), None);
    }

    #[test]
    fn two_cores_that_disagree_about_the_run_fail_the_gate() {
        // The check a single-machine gate cannot make. The plan is deterministic, so this is
        // the rig behaving differently on two instruction sets.
        let mut report = clean_report();
        if let Some(row) = report.rows.last_mut() {
            row.outcome = Outcome::Ran(
                Census {
                    dispatched: 41,
                    ..clean_census()
                },
                clean_stack_usage(),
            );
        }
        let shortfall = report.shortfall().unwrap_or_default();
        assert!(
            shortfall.contains("disagree"),
            "the two cores disagreeing must be reported as such: {shortfall}"
        );
    }

    #[test]
    fn two_cores_that_disagree_about_stack_used_still_pass_the_gate() {
        // The comparison this gate must not make: a Cortex-M0 and a Cortex-M4 compile the
        // same source into different instructions, so a different stack figure is expected
        // and is not itself a finding. Only `Census` is compared across machines.
        let mut report = clean_report();
        if let Some(row) = report.rows.last_mut() {
            row.outcome = Outcome::Ran(
                clean_census(),
                StackUsage {
                    used: 2048,
                    ..clean_stack_usage()
                },
            );
        }
        assert_eq!(report.shortfall(), None);
    }

    #[test]
    fn a_machine_that_produced_no_row_fails_the_gate() {
        let mut report = clean_report();
        report.rows.pop();
        assert!(report.shortfall().is_some());
    }

    #[test]
    fn a_machine_that_could_not_be_measured_fails_the_gate() {
        let mut report = clean_report();
        if let Some(row) = report.rows.first_mut() {
            row.outcome = Outcome::Unmeasurable("qemu is not installed".to_owned());
            assert!(row.outcome.census().is_none());
        }
        assert!(report.shortfall().is_some());
    }

    #[test]
    fn the_report_renders_every_machine() {
        let rendered = clean_report().render();
        for machine in MACHINES {
            assert!(rendered.contains(machine.name), "{rendered}");
            assert!(rendered.contains(machine.architecture), "{rendered}");
        }
        // And says what it is not, because a table of green rows beside a hardware table of
        // `Not run` rows is exactly the reading this stage must not invite.
        assert!(
            rendered.contains("does not establish is a board"),
            "{rendered}"
        );
        // And says what the stack figure is not: a claim the two machines must agree on.
        assert!(rendered.contains("not required to agree"), "{rendered}");
        assert!(rendered.contains("stack 1024 of 12000"), "{rendered}");
    }

    #[test]
    fn the_machines_are_two_different_architectures() {
        // A second machine that repeated the first's encoding would double the run time and
        // buy nothing: the equality check would compare a core with itself.
        assert!(MACHINES.len() >= 2);
        for (index, machine) in MACHINES.iter().enumerate() {
            for other in MACHINES.iter().skip(index.saturating_add(1)) {
                assert_ne!(machine.target, other.target);
                assert_ne!(machine.architecture, other.architecture);
            }
        }
    }

    #[test]
    fn a_clean_image_passes_every_half() {
        assert_eq!(check(&tests_support::clean_sources()), Vec::new());
    }

    #[test]
    fn an_image_that_is_not_in_the_workspace_is_reported() {
        let violations = check(&[]);
        assert_eq!(violations.len(), 1);
        assert!(violations.iter().all(|violation| violation.rule == RULE));
    }

    #[test]
    fn an_image_that_stopped_being_no_std_is_reported() {
        assert!(!check(&root_with("#![no_std]", "")).is_empty());
    }

    #[test]
    fn an_image_that_stopped_being_no_main_is_reported() {
        // A host binary has no reset vector, so there is nothing for a machine to start.
        assert!(!check(&root_with("#![no_main]", "")).is_empty());
    }

    #[test]
    fn an_image_with_no_main_rs_is_reported() {
        let elsewhere = vec![LayerSource {
            crate_name: PACKAGE.to_owned(),
            path: format!("crates/{PACKAGE}/src/boot.rs"),
            contents: tests_support::clean_root(),
        }];
        assert!(!check(&elsewhere).is_empty());
    }

    #[test]
    fn an_unreasoned_exception_is_reported() {
        let violations = check(&root_with(
            "#![allow(unsafe_code, reason = \"the reset vector\")]",
            "#![allow(unsafe_code)]",
        ));
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("reason")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_exception_for_some_other_lint_is_reported() {
        let violations = check(&root_with(
            "#![allow(unsafe_code, reason = \"the reset vector\")]",
            "#![allow(dead_code, reason = \"the reset vector\")]",
        ));
        assert!(!violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn an_image_with_no_exception_at_all_is_reported() {
        assert!(
            !check(&root_with(
                "#![allow(unsafe_code, reason = \"the reset vector\")]",
                ""
            ))
            .is_empty()
        );
    }

    #[test]
    fn a_reasoned_exception_quoted_in_a_doc_comment_does_not_stand_in_for_the_attribute() {
        // The first version of this rule read the raw file, found the module documentation's
        // *quotation* of the workspace manifest before the attribute, and reported a missing
        // `reason` on a crate that had one. Comments are stripped now, and this is what says
        // so — from the direction that matters: a crate whose only `#![allow(unsafe_code)]`
        // is inside a comment has no exception at all.
        let commented = vec![LayerSource {
            crate_name: PACKAGE.to_owned(),
            path: format!("crates/{PACKAGE}/src/main.rs"),
            contents: format!(
                "#![no_std]\n#![no_main]\n//! see `#![allow(unsafe_code, reason = \"x\")]`\npub const PREFIX: &str = \"{PREFIX}\";\n"
            ),
        }];
        assert!(!check(&commented).is_empty());
    }

    #[test]
    fn hand_written_unsafe_is_reported() {
        // The half the whole exception rests on. The crate may name the *lint* and may never
        // write the keyword: everything `#![allow(unsafe_code)]` is carried for is two macro
        // expansions, and without this the attribute would be a licence for the crate.
        let violations = check(&root_with(
            "pub const PREFIX",
            "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\npub const PREFIX",
        ));
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("`unsafe` keyword")),
            "{violations:?}"
        );
    }

    #[test]
    fn hand_written_unsafe_in_a_sibling_module_is_reported() {
        // The scan reads every file of the crate rather than the root alone, because the
        // claim is about the crate.
        let mut sources = tests_support::clean_sources();
        sources.push(LayerSource {
            crate_name: PACKAGE.to_owned(),
            path: format!("crates/{PACKAGE}/src/nor.rs"),
            contents: "unsafe fn peek() {}\n".to_owned(),
        });
        assert!(!check(&sources).is_empty());
    }

    #[test]
    fn the_real_stack_module_passes() {
        // The literal shipped file, read at compile time — not a stand-in for its shape.
        // ADR 0041's own claim: the linker-symbol block, and `unsafe` confined to the two
        // functions `PERMITTED_UNSAFE_FUNCTIONS` names — nothing this rule should catch.
        assert_eq!(
            check(&tests_support::sources_with_stack_module(
                tests_support::real_stack_module()
            )),
            Vec::new()
        );
    }

    #[test]
    fn unsafe_in_paint_or_high_water_mark_is_permitted_only_in_stack_rs() {
        // The exception is a (file, function) pair, not a function name alone: a decoy
        // `fn paint` elsewhere in the crate must not borrow it.
        let mut sources = tests_support::clean_sources();
        sources.push(LayerSource {
            crate_name: PACKAGE.to_owned(),
            path: format!("crates/{PACKAGE}/src/nor.rs"),
            contents: "pub fn paint(depth_from: usize) {\n    unsafe { core::ptr::write_volatile(depth_from as *mut u8, 0) };\n}\n".to_owned(),
        });
        assert!(!check(&sources).is_empty());
    }

    #[test]
    fn unsafe_in_stack_rs_outside_the_two_named_functions_is_reported() {
        // The exception is these two functions and no others: a third function in the same
        // file, even one that looks like a helper the other two might plausibly call, still
        // has to answer to this rule.
        let sources = tests_support::sources_with_stack_module(format!(
            "{}\npub fn extra() {{ unsafe {{ core::ptr::null::<u8>().read() }}; }}\n",
            tests_support::clean_stack_module()
        ));
        let violations = check(&sources);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("`unsafe` keyword")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_decoy_stack_rs_in_another_directory_is_rejected() {
        // The suffix names the crate-relative path rather than a bare file name precisely so
        // this fails: a permission this loose would be worth more than the obligation
        // `check_image_attributes` grants the same way for `main.rs`.
        let mut sources = tests_support::clean_sources();
        sources.push(LayerSource {
            crate_name: PACKAGE.to_owned(),
            path: format!("crates/{PACKAGE}/src/elsewhere/stack.rs"),
            contents: "pub fn paint(depth_from: usize) {\n    unsafe { core::ptr::write_volatile(depth_from as *mut u8, 0) };\n}\n".to_owned(),
        });
        assert!(!check(&sources).is_empty());
    }

    #[test]
    fn a_bodiless_signature_above_the_real_function_does_not_borrow_its_exemption() {
        // The first-match weakness `crate::source::declaration_count` alone does not close:
        // a bodiless `fn high_water_mark(..);` signature has a `declaration_count` of exactly
        // one too, so `braced_body`'s first `{` after it still resolves onto whatever block
        // follows — here, a `mod` whose `static` initialiser runs `unsafe` at that block's own
        // depth zero, which a nesting-depth check alone would not catch either. `has_a_body`
        // is the check that tells a real function from a signature with no braces of its own.
        let decoy = "pub trait Depth {\n    fn high_water_mark(depth_from: usize) -> u32;\n}\n\
             mod guts {\n    pub static X: u32 = unsafe { core::mem::transmute(0u32) };\n}\n\
             unsafe extern \"C\" {\n    static _stack_end: u8;\n    static _stack_start: u8;\n}\n\
             pub fn paint(depth_from: usize) {\n    unsafe { core::ptr::write_volatile(depth_from as *mut u8, 0xA5) };\n}\n"
            .to_owned();
        let sources = tests_support::sources_with_stack_module(decoy);
        let violations = check(&sources);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("`unsafe` keyword")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_foreign_function_item_is_not_the_permitted_extern_block() {
        // `unsafe extern "C" fn trap_handler() { .. }` is a foreign *function*, and its
        // `unsafe` is also immediately followed by the word `extern` — the naive test this
        // rule used to make. The fix asks what actually opens next: a function, not a block.
        let decoy = format!(
            "{}\npub unsafe extern \"C\" fn trap_handler(slot: *mut u32) {{ let _ = slot; }}\n",
            tests_support::clean_stack_module()
        );
        let sources = tests_support::sources_with_stack_module(decoy);
        let violations = check(&sources);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("`unsafe` keyword")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_second_extern_block_naming_something_else_is_reported() {
        // Each `unsafe extern` occurrence is scored on its own content. A second block that
        // does not name the stack floor symbol, or declares more than the one static, is
        // refused on its own account — nothing here needs to also count how many blocks the
        // file has.
        let decoy = format!(
            "{}\nunsafe extern \"C\" {{\n    static mut SOMEBODY_ELSES_STATE: u32;\n}}\n",
            tests_support::clean_stack_module()
        );
        let sources = tests_support::sources_with_stack_module(decoy);
        let violations = check(&sources);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("`unsafe` keyword")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_nested_function_inside_paint_does_not_inherit_its_exemption() {
        // `span_contains`'s old containment check permitted `unsafe` anywhere inside `fn
        // paint`'s outer braces, including a nested item declared within it — the same shape
        // of decoy `effect-protocol` refuses inside its own pinned bodies. Nesting depth,
        // measured from the permitted function's own body, is what tells the two apart.
        let decoy = "unsafe extern \"C\" {\n    static _stack_end: u8;\n    static _stack_start: u8;\n}\n\
             pub fn paint(depth_from: usize) {\n    \
                 fn reconfigure_mpu() {\n        unsafe { core::arch::asm!(\"nop\") };\n    }\n    \
                 reconfigure_mpu();\n    \
                 unsafe { core::ptr::write_volatile(depth_from as *mut u8, 0xA5) };\n\
             }\n\
             pub fn high_water_mark(depth_from: usize) -> u32 {\n    let deepest = unsafe { core::ptr::read_volatile(depth_from as *const u8) };\n    deepest as u32\n}\n"
            .to_owned();
        let sources = tests_support::sources_with_stack_module(decoy);
        let violations = check(&sources);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("`unsafe` keyword")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_sibling_unsafe_beside_the_legitimate_one_is_reported() {
        // Containment plus depth-zero is not uniqueness: a *second*, unrelated `unsafe`
        // statement placed beside the legitimate fill — not nested inside it — sits at the
        // same nesting depth and would pass a check that only asked "is this contained in the
        // permitted function, at depth zero". `sole_depth_zero_unsafe` refuses the whole
        // function once it finds two, because which one is real would be a guess.
        let decoy = "unsafe extern \"C\" {\n    static _stack_end: u8;\n    static _stack_start: u8;\n}\n\
             pub fn paint(depth_from: usize) {\n    \
                 unsafe { core::ptr::write_volatile(0x4000_0000 as *mut u8, 0) };\n    \
                 unsafe { core::ptr::write_volatile(depth_from as *mut u8, 0xA5) };\n\
             }\n\
             pub fn high_water_mark(depth_from: usize) -> u32 {\n    let deepest = unsafe { core::ptr::read_volatile(depth_from as *const u8) };\n    deepest as u32\n}\n"
            .to_owned();
        let sources = tests_support::sources_with_stack_module(decoy);
        let violations = check(&sources);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("`unsafe` keyword")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_clean_stack_module_with_two_linker_symbols_passes() {
        // The positive twin of the sibling-unsafe test above: exactly one `unsafe` per
        // permitted function, and an extern block naming both `_stack_end` and `_stack_start`
        // — the shape `clamp_to_stack_region` needs — is not itself a violation.
        let sources = tests_support::sources_with_stack_module(tests_support::clean_stack_module());
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn an_unsafe_inside_a_comment_is_not_reported() {
        let mut sources = tests_support::clean_sources();
        sources.push(LayerSource {
            crate_name: PACKAGE.to_owned(),
            path: format!("crates/{PACKAGE}/src/nor.rs"),
            contents: "// there is no unsafe { } here\nfn safe() {}\n".to_owned(),
        });
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn naming_the_lint_is_not_writing_the_keyword() {
        // The clean image already names `unsafe_code` in its `allow`. If the scan could not
        // tell the identifier from the keyword, every clean run would be a violation.
        assert_eq!(check(&tests_support::clean_sources()), Vec::new());
        assert!(tests_support::clean_root().contains(PERMITTED_LINT_NAME));
    }

    #[test]
    fn a_prefix_the_harness_cannot_read_is_reported() {
        let violations = check(&root_with(PREFIX, "waymaker-emulator:"));
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains(PREFIX)),
            "{violations:?}"
        );
    }

    #[test]
    fn a_binary_that_is_not_behind_the_feature_is_reported() {
        let ungated = tests_support::clean_manifest()
            .replace(&format!("required-features = [\"{FEATURE}\"]\n"), "");
        let violations = check_emulation_boot(
            Some(&ungated),
            &tests_support::clean_sources(),
            Some(&toolchain_pinning_every_machine()),
            STAGES,
        );
        assert!(!violations.is_empty());
    }

    #[test]
    fn a_missing_or_unreadable_manifest_is_reported() {
        for manifest in [None, Some("this is not toml = = =")] {
            let violations = check_emulation_boot(
                manifest,
                &tests_support::clean_sources(),
                Some(&toolchain_pinning_every_machine()),
                STAGES,
            );
            assert!(!violations.is_empty(), "{manifest:?}");
        }
    }

    #[test]
    fn a_machine_whose_target_is_not_pinned_is_reported() {
        for machine in MACHINES {
            let thinned = toolchain_pinning_every_machine().replace(machine.target, "x86_64");
            let violations = check_emulation_boot(
                Some(&tests_support::clean_manifest()),
                &tests_support::clean_sources(),
                Some(&thinned),
                STAGES,
            );
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(machine.target)),
                "{machine:?} {violations:?}"
            );
        }
    }

    #[test]
    fn a_pipeline_that_never_starts_the_image_is_reported() {
        // A stage table with no `xtask emulate` in it is a rule pinning an image nothing
        // builds and nothing starts.
        let violations = check_emulation_boot(
            Some(&tests_support::clean_manifest()),
            &tests_support::clean_sources(),
            Some(&toolchain_pinning_every_machine()),
            &[],
        );
        assert!(!violations.is_empty());
    }

    #[test]
    fn the_real_pipeline_starts_the_image() {
        assert!(
            STAGES
                .iter()
                .any(|stage| stage.command.contains("xtask emulate"))
        );
    }

    #[test]
    fn the_real_pipeline_lints_the_image() {
        // The crate is behind `required-features`, so the host lint stage never compiles it.
        // Without a stage of its own, the one crate that allows unsafe code is the one crate
        // clippy never reads.
        assert!(
            STAGES
                .iter()
                .any(|stage| stage.command.contains("clippy") && stage.command.contains(PACKAGE))
        );
    }
}
