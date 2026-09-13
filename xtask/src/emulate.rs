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
//! every machine [`selected_machines`] names, starts each under QEMU, and requires the
//! rig's three moments to have happened and said so: the ARM pair on every run, and the
//! ESP32-S3 when [`XTENSA_OPT_IN_ENV`] opts it in.
//!
//! # Why three machines, and why their answers must agree
//!
//! [`MACHINES`] names a Cortex-M0 and a Cortex-M4, which are **ARMv6-M** and **ARMv7E-M** —
//! the two instruction sets `docs::HARDWARE_TARGETS`'s two power-cut rows are stated over —
//! and an ESP32-S3, which is **Xtensa LX7**, a third encoding from a different vendor
//! lineage. A rig that had only ever executed on one encoding has measured that encoding,
//! and two ARM cores agreeing is a weaker statement than three cores from two families
//! agreeing.
//!
//! The three censuses are then required to be *equal*, which is the sharper half. All three
//! runs drive the same seed through the same deterministic plan, so a difference is not a
//! tolerance — it is the rig behaving differently on two cores, which is exactly
//! the class of defect a host test binary cannot see and the class a fleet would find. A
//! `u64` shift lowered through `compiler_builtins` on one core and an instruction on the
//! other, an alignment assumption, a `usize` narrowing: none of those show up as anything
//! else here.
//!
//! The third machine is opt-in rather than always-on, for the reason
//! [`XTENSA_OPT_IN_ENV`] states: its build and boot need the locally provisioned Espressif
//! stack — the `esp` toolchain, the fork's QEMU, the private offline cargo cache,
//! `esptool`, and the fork's shared libraries — which no clean runner has. A run that
//! always required all three would fail its preflight before either ARM boot on any
//! machine but the provisioned one, so the ARM pair is the gate CI runs and the S3 joins
//! where it is provisioned. An opted-in machine whose dependencies are absent still fails
//! the run: the opt-in chooses the machines, it never excuses a missing one.
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
//! A board. None of the three machines has a NOR part, a supply that can be removed, a
//! reset-cause register or a backup domain, so `docs::HARDWARE_TARGETS` stays `Not run`
//! and this stage may not be cited to move a row of it. See
//! [ADR 0040](https://github.com/madmax983/waymaker/blob/main/docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use syn::parse::Parser as _;
use syn::visit::Visit as _;

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
/// Generous against the second or so all three boots really take, and finite on purpose: an image
/// that hangs would otherwise consume the job's whole timeout and report as infrastructure
/// rather than as a defect. A panic in the rig exits non-zero rather than hanging — the
/// ARM image installs a `#[panic_handler]` that does, and the Xtensa image says so on the
/// UART — so reaching this is a livelock, which is a finding of its own.
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
    /// What sets this core apart in how its image is built and booted.
    pub kind: MachineKind,
}

/// What sets one machine apart in how its image is built and booted.
///
/// Not the ISA alone. The ESP32-S3 differs from the ARM pair in how its image is built
/// (the Espressif Rust fork, `-Z build-std`, a linker script with `.literal` first), in
/// how QEMU starts it (a flash image the ROM boots from offset 0x0, not an ELF on
/// `-kernel`), and in how the harness knows it finished (it never exits — the census is
/// watched for on the serial log, and the harness terminates QEMU itself). One enum keeps
/// the three answers in one place, so a machine cannot gain a new build recipe without
/// gaining its matching boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineKind {
    /// An ARM Cortex-M: the workspace toolchain builds the image, QEMU starts the ELF
    /// with `-kernel`, and the guest exits on its own through semihosting.
    Arm,
    /// An ESP32-S3 (Xtensa LX7): the Espressif fork toolchain builds the image, QEMU
    /// boots a flash image the ROM loads from offset 0x0, and the guest never exits.
    Xtensa,
}

/// The cores the rig can run on.
///
/// The ARM pair runs in every invocation; the ESP32-S3 joins when [`XTENSA_OPT_IN_ENV`]
/// opts it in — see [`selected_machines`].
///
/// Three, and their *architectures* are the reason rather than their part numbers. Neither
/// ARM machine is a row of `docs::HARDWARE_TARGETS`: a Cortex-M0 is not a Cortex-M0+ —
/// same instruction set, different core — and QEMU has no M0+ machine at all. What the
/// three buy is that every instruction the rig executes has been executed under every
/// encoding Waymaker is built for, and that two ARM cores agreeing is checked against a
/// third core from another family.
pub const MACHINES: &[Machine] = &[
    Machine {
        name: "cortex-m0",
        qemu: "microbit",
        target: "thumbv6m-none-eabi",
        architecture: "ARMv6-M",
        why: "the architecture design document §04's budgets are stated for, and the target every firmware stage builds",
        kind: MachineKind::Arm,
    },
    Machine {
        name: "cortex-m4",
        qemu: "mps2-an386",
        target: "thumbv7em-none-eabi",
        architecture: "ARMv7E-M",
        why: "a second encoding, because a rig that only ever ran on one has measured that one",
        kind: MachineKind::Arm,
    },
    Machine {
        name: "esp32s3",
        qemu: "esp32s3",
        target: "xtensa-esp32s3-none-elf",
        architecture: "Xtensa LX7",
        why: "a third encoding from another vendor lineage, because two cores from one family agreeing is a weaker statement than three cores from two families agreeing",
        kind: MachineKind::Xtensa,
    },
];

/// Opts the ESP32-S3 into `cargo xtask emulate`.
///
/// The S3's build and boot need the locally provisioned Espressif stack — the `esp`
/// toolchain, the fork's QEMU, the private offline cargo cache, `esptool`, and the
/// fork's shared libraries — which no clean runner has. Setting this variable to any
/// non-empty value adds the machine to the run; without it the gate covers the ARM
/// pair, which is what the CI `emulation` job runs. An opted-in machine whose
/// dependencies are absent still fails the run: the opt-in chooses the machines, it
/// never excuses a missing one.
pub const XTENSA_OPT_IN_ENV: &str = "WAYMAKER_XTENSA_OPT_IN";

/// The machines one run boots: the ARM pair always, the ESP32-S3 when opted in.
///
/// Reads [`XTENSA_OPT_IN_ENV`]; `machines_for` is the pure half, so tests do not touch
/// the process environment.
#[must_use]
pub fn selected_machines() -> Vec<Machine> {
    machines_for(is_xtensa_opted_in())
}

/// Whether [`XTENSA_OPT_IN_ENV`] opts the ESP32-S3 into this run.
///
/// Any non-empty value counts: the variable is a switch, not a path, and an empty one is
/// what `VAR=` leaves behind rather than an opt-in anyone meant.
fn is_xtensa_opted_in() -> bool {
    std::env::var_os(XTENSA_OPT_IN_ENV).is_some_and(|value| !value.is_empty())
}

/// [`selected_machines`] with the opt-in as a parameter, so tests do not touch the
/// process environment.
fn machines_for(xtensa_opt_in: bool) -> Vec<Machine> {
    MACHINES
        .iter()
        .copied()
        .filter(|machine| machine.kind == MachineKind::Arm || xtensa_opt_in)
        .collect()
}

/// The linker arguments the Xtensa image needs, and nothing else does.
///
/// Kept out of `.cargo/config.toml` for the reason [`LINK_ARGS`] states: a target-scoped
/// `rustflags` entry there would put this script on every Xtensa build in the workspace.
/// `-Tmemory-xtensa.x` places the image in a non-overlapping split of the S3's SRAM —
/// `.text`/`.literal` in the instruction-bus view, `.rodata`/`.data`/`.bss` in the
/// data-bus view (the two views alias the same physical SRAM, so the split is what keeps
/// the linker from stacking them; `.rodata` is data-bus-side because ordinary loads
/// cannot reach instruction-bus addresses) — `.literal` before `.text`, or the Xtensa linker
/// reports a dangerous `l32r` relocation — and
/// `-nostartfiles` keeps the toolchain's `crt0.o`, which defines its own `_start`, out of
/// the link. `-L` names the directory the script lives in. The name is not `memory.x` on
/// purpose: the ARM build's `cortex-m-rt` `INCLUDE`s a user `memory.x` defining FLASH and
/// RAM, so the Xtensa script cannot share the name in the same `-L` directory.
pub const XTENSA_LINK_ARGS: &str =
    "-C link-arg=-Tmemory-xtensa.x -C link-arg=-nostartfiles -L crates/waymaker-emu";

/// The linker arguments one machine's image is linked with.
///
/// [`LINK_ARGS`] for the ARM pair, [`XTENSA_LINK_ARGS`] for the ESP32-S3: the two recipes
/// differ in script, in start files, and in nothing else.
#[must_use]
pub const fn link_args_for(machine: Machine) -> &'static str {
    match machine.kind {
        MachineKind::Arm => LINK_ARGS,
        MachineKind::Xtensa => XTENSA_LINK_ARGS,
    }
}

/// How one machine's image reaches QEMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMedia {
    /// QEMU loads the linked ELF directly, with `-kernel`.
    Elf,
    /// `esptool elf2image` converts the ELF to the format the S3 ROM understands, and the
    /// result is placed at offset 0x0 of a 4 MiB flash image QEMU attaches with
    /// `-drive if=mtd` — the offset the ROM loads the boot image from, the one real
    /// hardware boots.
    FlashImage,
}

/// How one machine's image reaches QEMU: the ELF on `-kernel`, or the flash image the
/// ROM boots.
#[must_use]
pub const fn boot_media_for(machine: Machine) -> BootMedia {
    match machine.kind {
        MachineKind::Arm => BootMedia::Elf,
        MachineKind::Xtensa => BootMedia::FlashImage,
    }
}

/// Whether the guest stops on its own.
///
/// On ARM the image exits through semihosting, so the harness waits for the child. On the
/// ESP32-S3 there is no exit: the harness watches the serial log for a complete census
/// and terminates QEMU itself, because a guest that never exits would otherwise outlive
/// every timeout and read as a hang rather than as a finished run.
#[must_use]
pub const fn guest_exits_for(machine: Machine) -> bool {
    match machine.kind {
        MachineKind::Arm => true,
        MachineKind::Xtensa => false,
    }
}

/// The arguments one machine's QEMU run takes, after the program.
///
/// Pure, so that the difference between "an ELF on `-kernel`" and "a flash image on
/// `-drive if=mtd`" is a case in `tests` rather than a QEMU run inside a test. `image` is
/// the ELF for [`BootMedia::Elf`] machines and the flash image for
/// [`BootMedia::FlashImage`] ones; `serial_log` is where the Xtensa machine's UART goes,
/// and is unused on ARM.
#[must_use]
pub fn qemu_args_for(machine: Machine, image: &Path, serial_log: &Path) -> Vec<String> {
    let mut args = vec![
        "-nographic".to_owned(),
        "-machine".to_owned(),
        machine.qemu.to_owned(),
    ];
    match machine.kind {
        MachineKind::Arm => {
            args.push("-semihosting-config".to_owned());
            args.push("enable=on,target=native".to_owned());
            args.push("-kernel".to_owned());
            args.push(image.display().to_string());
        }
        MachineKind::Xtensa => {
            args.push("-drive".to_owned());
            args.push(format!("file={},if=mtd,format=raw", image.display()));
            args.push("-serial".to_owned());
            args.push(format!("file:{}", serial_log.display()));
            args.push("-monitor".to_owned());
            args.push("none".to_owned());
            args.push("-no-reboot".to_owned());
        }
    }
    args
}

/// Whether the Xtensa serial log so far holds a complete census.
///
/// The Xtensa guest never exits, so the harness cannot wait for a child status the way it
/// does on ARM: it polls the log and stops QEMU once this is true. A complete [`parse`]
/// is the condition rather than the `ok` line alone, for the reason `parse` states — a
/// truncated write has the counts and not the word, and stopping there would report on a
/// rig that may not have finished.
#[must_use]
pub fn xtensa_complete(log: &str) -> bool {
    parse(log).is_some()
}

/// Whether the Xtensa serial log so far reports a terminal guest failure.
///
/// The guest prints `{PREFIX} failed <message>` when the rig refused its own census and
/// `{PREFIX} panicked: <message> at <location>` from its panic handler — one line on
/// purpose, because `PanicInfo`'s `Display` would otherwise put a newline between the
/// location and the message and the kill below would land before the message is written
/// (Codex P2, `review_comment` 3997629597) — then drains the UART and parks —
/// no census ever follows, so waiting for one would burn the whole [`TIMEOUT`] and
/// misreport a failure as a livelock while discarding the UART's own account of it.
/// Only terminated lines count: the guest's UART writer pushes this log byte by byte and
/// the harness polls it mid-write, while `str::lines` yields the unterminated trailing
/// fragment as a line — matching a bare `failed `/`panicked:` prefix would kill QEMU
/// before the diagnostic and its newline arrive, truncating the very output the detector
/// exists to preserve. The newline is at most one poll away (the guest drains the UART
/// before parking), so requiring it delays the kill rather than risking it.
/// Pure, so the line matching is a case in `tests` rather than a QEMU run inside a test.
#[must_use]
pub fn xtensa_failed(log: &str) -> bool {
    log.split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .any(|line| {
            let Some(rest) = line.trim().strip_prefix(PREFIX) else {
                return false;
            };
            let rest = rest.trim_start();
            rest == "failed" || rest.starts_with("failed ") || rest.starts_with("panicked:")
        })
}

/// The emulator binary one machine runs under.
///
/// `qemu-system-arm` comes off `PATH`. The Espressif fork is carried by no package
/// manager, so the harness looks under the workspace root —
/// `esp32s3/qemu/bin/qemu-system-xtensa`, alongside the other workspace-local Xtensa
/// assets — unless `WAYMAKER_XTENSA_QEMU` names another binary.
pub const XTENSA_QEMU_ENV: &str = "WAYMAKER_XTENSA_QEMU";

/// The emulator binary one machine runs under: off `PATH` for ARM, under the workspace
/// root for the ESP32-S3.
///
/// Reads [`XTENSA_QEMU_ENV`]; `emulator_path_for` is the pure half, so tests do not
/// touch the process environment.
fn emulator_for(machine: Machine, root: &Path) -> PathBuf {
    emulator_path_for(machine, root, std::env::var_os(XTENSA_QEMU_ENV))
}

/// [`emulator_for`] with the override as a parameter, so tests do not touch the
/// process environment.
fn emulator_path_for(
    machine: Machine,
    root: &Path,
    xtensa_qemu: Option<std::ffi::OsString>,
) -> PathBuf {
    match machine.kind {
        MachineKind::Arm => PathBuf::from(EMULATOR),
        MachineKind::Xtensa => xtensa_qemu.map_or_else(
            || root.join("esp32s3/qemu/bin/qemu-system-xtensa"),
            PathBuf::from,
        ),
    }
}

/// The cargo that builds the Xtensa image.
///
/// Not the workspace toolchain's: `xtensa-esp32s3-none-elf` exists only in the Espressif
/// Rust fork, which `espup` installs as the `esp` rustup toolchain. `WAYMAKER_ESP_CARGO`
/// overrides the default, which is the fork's own cargo binary.
///
/// # Errors
///
/// A message when neither the override nor the default names a cargo — the build cannot
/// start, and this gate fails closed rather than falling back to a toolchain that does
/// not know the target.
pub const ESP_CARGO_ENV: &str = "WAYMAKER_ESP_CARGO";

fn esp_cargo() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(ESP_CARGO_ENV) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var("HOME").map_err(|_| {
        format!(
            "HOME is not set, so the `esp` toolchain's cargo cannot be found; set {ESP_CARGO_ENV} to its path"
        )
    })?;
    Ok(PathBuf::from(home).join(".rustup/toolchains/esp/bin/cargo"))
}

/// The directory holding the Xtensa GCC driver, for `PATH`.
///
/// The fork's rustc links through the GCC driver, which is not on `PATH` by default. The
/// build prepends this directory rather than requiring the caller to have sourced the
/// toolchain's export script. The driver lives two levels under the version directory —
/// `<version>/xtensa-esp-elf/bin` — and the returned path names the `bin` itself: the
/// version directory alone does not put the driver on `PATH`.
///
/// # Errors
///
/// A message when the toolchain layout has no GCC in it — the link would fail on a
/// missing driver, and an early sentence beats a late linker error.
fn esp_gcc_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME")
        .map_err(|_| "HOME is not set, so the `esp` toolchain's GCC cannot be found".to_owned())?;
    let home = Path::new(&home);
    esp_gcc_dir_in(home).ok_or_else(|| {
        format!(
            "no Xtensa GCC found under {}",
            home.join(".rustup/toolchains/esp/xtensa-esp-elf").display()
        )
    })
}

/// [`esp_gcc_dir`] with the home directory as a parameter, so tests can point it at a
/// fixture instead of mutating the process environment.
fn esp_gcc_dir_in(home: &Path) -> Option<PathBuf> {
    let base = home.join(".rustup/toolchains/esp/xtensa-esp-elf");
    let mut dirs: Vec<(PathBuf, Vec<u64>)> = std::fs::read_dir(base)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("xtensa-esp-elf/bin").is_dir())
        .map(|path| {
            let version = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(gcc_dir_version)
                .unwrap_or_default();
            (path, version)
        })
        .collect();
    // The newest version wins, compared numerically — sorting the directory *names*
    // lexicographically put `esp-15.x` before `esp-9.x`, so the older driver silently
    // won. Names that do not parse rank below every parsed one; the name breaks ties
    // so the order stays deterministic.
    dirs.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    dirs.pop().map(|(dir, _)| dir.join("xtensa-esp-elf/bin"))
}

/// The version a `xtensa-esp-elf` install directory name carries.
///
/// Espressif's layout names its directories `esp-<version>_<build-date>`, for example
/// `esp-15.2.0_20250920`. Component-wise numeric comparison is what "newest" means:
/// the version and date chunks compare as numbers, in order, so `esp-15.x` beats
/// `esp-9.x` even though it sorts before it as a string. A name that does not parse
/// yields no components and loses to every parsed name.
fn gcc_dir_version(dir_name: &str) -> Vec<u64> {
    let name = dir_name.strip_prefix("esp-").unwrap_or(dir_name);
    let mut version = Vec::new();
    for chunk in name.split('_') {
        for part in chunk.split('.') {
            let Ok(number) = part.parse::<u64>() else {
                return Vec::new();
            };
            version.push(number);
        }
    }
    version
}

/// The cargo home the Xtensa build uses.
///
/// Private to the ESP32-S3 work rather than the user's `~/.cargo`: the fork's cargo is a
/// different cargo, and its registry cache carries what the spike verified.
/// `WAYMAKER_ESP_CARGO_HOME` overrides the default, which joins `esp32s3/cargo-home`
/// under the workspace root.
pub const ESP_CARGO_HOME_ENV: &str = "WAYMAKER_ESP_CARGO_HOME";

fn esp_cargo_home(root: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os(ESP_CARGO_HOME_ENV) {
        return PathBuf::from(path);
    }
    root.join("esp32s3/cargo-home")
}

/// A directory on the front of `PATH`, keeping what was already there.
fn prepend_to_path(dir: &Path) -> std::ffi::OsString {
    prepend_to_env(dir, "PATH")
}

/// A directory on the front of one environment variable, keeping what was already there.
///
/// `prepend_to_path` reads `PATH`, which is wrong for every other variable: the QEMU
/// libraries must preserve `LD_LIBRARY_PATH` and `esptool`'s must preserve `PYTHONPATH`,
/// not inherit whatever happened to be on `PATH`.
fn prepend_to_env(dir: &Path, variable: &str) -> std::ffi::OsString {
    prepend_dir(dir, std::env::var_os(variable).as_deref())
}

/// [`prepend_to_env`] with the prior value as a parameter, so tests do not touch the
/// process environment.
fn prepend_dir(dir: &Path, old: Option<&std::ffi::OsStr>) -> std::ffi::OsString {
    let mut value = std::ffi::OsString::from(dir);
    if let Some(old) = old {
        if !old.is_empty() {
            value.push(":");
            value.push(old);
        }
    }
    value
}

/// Where `esptool` is importable from.
///
/// The workspace-local pip target the spike verified; `WAYMAKER_ESPTOOL_PY` overrides it.
///
/// # Errors
///
/// A message when neither the override nor the default is set — the flash image cannot be
/// built without `esptool`.
pub const ESPTOOL_PY_ENV: &str = "WAYMAKER_ESPTOOL_PY";

fn esptool_python_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(ESPTOOL_PY_ENV) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var("HOME").map_err(|_| {
        format!(
            "HOME is not set, so `esptool` cannot be found; set {ESPTOOL_PY_ENV} to its directory"
        )
    })?;
    Ok(PathBuf::from(home).join("workspace/esptools/py"))
}

/// The directory QEMU's own libraries live in.
///
/// The Espressif fork binary links against libraries no package manager provides here
/// (`libslirp.so.0` among them); the spike verified it runs with this directory on
/// `LD_LIBRARY_PATH`. `WAYMAKER_XTENSA_LD_PATH` overrides the default.
///
/// # Errors
///
/// A message when neither the override nor the default is set — QEMU would fail on a
/// missing shared library before the machine exists, and an early sentence beats a
/// confusing `ldd` error.
pub const XTENSA_LD_PATH_ENV: &str = "WAYMAKER_XTENSA_LD_PATH";

fn xtensa_ld_library_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(XTENSA_LD_PATH_ENV) {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var("HOME").map_err(|_| {
        format!(
            "HOME is not set, so QEMU's library directory cannot be found; set {XTENSA_LD_PATH_ENV} to its path"
        )
    })?;
    Ok(PathBuf::from(home).join("workspace/tooling/qemu-esp32/libs/usr/lib/x86_64-linux-gnu"))
}

/// The environment one machine's QEMU launch needs.
///
/// ARM needs none: `qemu-system-arm` comes from the distribution's packages with its
/// libraries. The Espressif fork binary links against libraries no package manager
/// provides here, which the spike verified live in the directory
/// [`xtensa_ld_library_path`] names — so the Xtensa launch puts it on `LD_LIBRARY_PATH`,
/// ahead of whatever was already there, and without it the run fails before the machine
/// exists rather than inside the guest.
///
/// # Errors
///
/// [`xtensa_ld_library_path`]'s message, when the library directory cannot be found.
fn qemu_env_for(machine: Machine) -> Result<Vec<(std::ffi::OsString, std::ffi::OsString)>, String> {
    match machine.kind {
        MachineKind::Arm => Ok(Vec::new()),
        MachineKind::Xtensa => {
            let dir = xtensa_ld_library_path()?;
            Ok(vec![(
                std::ffi::OsString::from("LD_LIBRARY_PATH"),
                prepend_to_env(&dir, "LD_LIBRARY_PATH"),
            )])
        }
    }
}

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

/// What became of one machine's boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The image ran, said what it did, and the census holds.
    Ran(Census),
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
            Self::Ran(census) => Some(census),
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
    /// The machines this run was asked to boot — [`selected_machines`] at the time.
    ///
    /// The report remembers the selection so [`Report::shortfall`] can tell "a machine
    /// produced no row" from "the ESP32-S3 was never opted in": the first fails the
    /// gate, the second is the run the caller asked for.
    pub machines: Vec<Machine>,
}

impl Report {
    /// Why this run is not a pass, if it is not.
    ///
    /// Three demands. Every *selected* machine ran; every machine's census holds; and the
    /// machines **agree**. The third is the one a single-machine gate cannot make, and it
    /// is the reason there are three: the plan is deterministic, so cores that disagree
    /// about what the rig did are cores executing the rig differently.
    #[must_use]
    pub fn shortfall(&self) -> Option<String> {
        if self.rows.len() != self.machines.len() {
            return Some(format!(
                "{} machines were selected and {} ran; a machine that produced no row is a machine nothing watched",
                self.machines.len(),
                self.rows.len()
            ));
        }
        for row in &self.rows {
            match &row.outcome {
                Outcome::Unmeasurable(why) => {
                    return Some(format!("{}: {why}", row.machine.name));
                }
                Outcome::Ran(census) => {
                    if let Some(shortfall) = census.shortfall() {
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
                        "{first} and {} disagree about what the rig did: {expected:?} against {census:?}. The plan is deterministic, so this is the rig behaving differently on two cores rather than a tolerance",
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
                    Outcome::Ran(census) => format!(
                        "cases {}+{} · iterations {} · cuts {} · resumes {} · redeliveries {} · verdicts {} · effects {}",
                        census.cases_passed,
                        census.cases_exempt,
                        census.iterations,
                        census.cuts,
                        census.resumes,
                        census.redeliveries,
                        census.verdicts,
                        census.dispatched
                    ),
                    Outcome::Unmeasurable(why) => format!("unmeasurable: {why}"),
                }
            );
        }
        // The run covers the ARM pair, and the ESP32-S3 when it was opted in: the
        // paragraph names the machines that actually ran, because "three" on a two-core
        // run would claim a measurement that did not happen.
        let (cores, agreement) = match self.machines.len() {
            2 => ("both cores", "the two"),
            3 => ("all three cores", "the three"),
            _ => ("every selected core", "they"),
        };
        let _ = writeln!(
            out,
            "\nWhat this establishes is that the rig executes on {cores}, and that {agreement} agree\n\
             about what it did. What it does not establish is a board: no emulated machine has a\n\
             NOR part, a supply to remove, a reset-cause register or a backup domain, so every\n\
             row of the hardware table stays `Not run`. See ADR 0040."
        );
        out
    }
}

/// Reads a boot's own lines into a census.
///
/// Pure, so that every way an image can fail to say what it did is a case in
/// `tests` rather than a QEMU run inside a test. Returns `None` when a required line is
/// absent — which is what an image that exited zero having printed nothing looks like.
#[must_use]
pub fn parse(output: &str) -> Option<Census> {
    let mut census = Census::default();
    let mut cases = false;
    let mut rig = false;
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
        }
    }
    // All three, and `ok` is not enough on its own: the image writes it last, so a truncated
    // run has the counts and not the word, and an image that printed only the word did not
    // run. Requiring the set is what makes a partial read a failure.
    (cases && rig && ok).then_some(census)
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

/// Builds every selected image, starts every selected machine, and reports what they said.
///
/// # Errors
///
/// A message when the run could not be started at all — no QEMU, or an image that would not
/// build. A machine that started and misbehaved is a [`Row`] rather than an error, so that
/// the other machines still run and the report shows every selected machine.
pub fn measure(root: &Path) -> Result<Report, String> {
    let machines = selected_machines();
    // Before anything is built, so that a runner without an emulator says so in a second
    // rather than after three firmware builds.
    for machine in &machines {
        which(*machine, root)?;
    }
    let mut rows = Vec::new();
    for machine in &machines {
        let outcome = match build(root, *machine).and_then(|image| start(*machine, &image, root)) {
            Ok((census, output)) => {
                rows.push(Row {
                    machine: *machine,
                    outcome: census.map_or_else(
                        || {
                            Outcome::Unmeasurable(
                                "the image ran and printed no census this gate can read, which is not a measurement that passed".to_owned(),
                            )
                        },
                        Outcome::Ran,
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
    Ok(Report { rows, machines })
}

/// Fails unless `machine`'s emulator runs and reports a version.
///
/// Before anything is built, so that a runner without QEMU says so in a second rather than
/// after three firmware builds. Runs with the launch's own environment — the Espressif
/// fork binary needs its library directory even to answer `--version`.
fn which(machine: Machine, root: &Path) -> Result<(), String> {
    let program = emulator_for(machine, root);
    let mut command = Command::new(&program);
    command
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (key, value) in qemu_env_for(machine)? {
        command.env(key, value);
    }
    command
        .status()
        .map_err(|error| {
            format!(
                "could not run `{}`: {error}. This gate fails closed rather than skipping: a boot that did not happen is not a boot that passed.",
                program.display()
            )
        })
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(format!("`{} --version` exited {status}", program.display()))
            }
        })
}

/// Links `machine`'s image and answers where it landed.
///
/// For [`BootMedia::Elf`] machines that is the ELF QEMU starts; for
/// [`BootMedia::FlashImage`] it is the flash image the ROM boots.
fn build(root: &Path, machine: Machine) -> Result<PathBuf, String> {
    match machine.kind {
        MachineKind::Arm => build_arm(root, machine),
        MachineKind::Xtensa => build_xtensa(root, machine),
    }
}

/// Links `machine`'s ARM image and answers where it landed.
fn build_arm(root: &Path, machine: Machine) -> Result<PathBuf, String> {
    let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    let status = Command::new(cargo)
        .current_dir(root)
        .env("RUSTFLAGS", link_args_for(machine))
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

/// Links the Xtensa image and packs the flash image the S3 ROM boots.
///
/// The image is built with the `esp` toolchain's cargo rather than the workspace's: only
/// the Espressif fork knows `xtensa-esp32s3-none-elf`, and only its nightly cargo takes
/// `-Z build-std`, which the target needs because the fork ships prebuilt std for the
/// host alone. Two environment details matter. `RUSTUP_TOOLCHAIN=esp` is set on the
/// command because the workspace's `rust-toolchain.toml` pins the host channel, and the
/// fork's cargo — a rustup-managed toolchain binary — would otherwise let that file
/// redirect the build back to a toolchain that does not know the target. `--offline` is
/// passed because the private cargo home below already carries the verified registry, and
/// an index refresh is a network dependency a gate should not have. `esptool elf2image`
/// then converts the ELF to the format the ROM understands, and the 4 MiB flash image
/// carries it at offset 0x0.
fn build_xtensa(root: &Path, machine: Machine) -> Result<PathBuf, String> {
    let cargo = esp_cargo()?;
    let cargo_home = esp_cargo_home(root);
    let gcc_dir = esp_gcc_dir()?;
    let status = Command::new(&cargo)
        .current_dir(root)
        .env("RUSTUP_TOOLCHAIN", "esp")
        .env("CARGO_HOME", &cargo_home)
        .env("PATH", prepend_to_path(&gcc_dir))
        .env("RUSTFLAGS", link_args_for(machine))
        .args([
            "build",
            "--locked",
            "--offline",
            "--profile",
            PROFILE,
            "-p",
            PACKAGE,
            "--features",
            FEATURE,
            "--target",
            machine.target,
            "-Z",
            "build-std=core",
        ])
        .status()
        .map_err(|error| {
            format!(
                "could not run the `esp` toolchain's cargo at {}: {error}",
                cargo.display()
            )
        })?;
    if !status.success() {
        return Err(format!(
            "{PACKAGE} did not link for {}; an image that does not link is the first thing this stage exists to catch",
            machine.target
        ));
    }
    let elf = root
        .join("target")
        .join(machine.target)
        .join(PROFILE)
        .join(PACKAGE);
    if !elf.is_file() {
        return Err(format!(
            "the image built and {} is not there",
            elf.display()
        ));
    }
    flash_image(&elf)
}

/// Converts the Xtensa ELF into the 4 MiB flash image the S3 ROM boots.
///
/// `esptool elf2image` produces the image the ROM understands (magic `0xE9`, segment
/// headers, entry point, checksum); the flash image is 4 MiB of erased (`0xFF`) flash
/// with that image at offset 0x0 — the offset the S2/S3/C3 ROM loads the boot image from,
/// the one real hardware boots. The recipe the spike verified.
fn flash_image(elf: &Path) -> Result<PathBuf, String> {
    // Four mebibytes of erased (`0xFF`) flash: the size the S2/S3/C3 ROM expects, and the
    // one real hardware boots.
    const FLASH_SIZE: usize = 4 * 1024 * 1024;
    let dir = elf
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", elf.display()))?;
    let app = dir.join("waymaker-emu.bin");
    let flash = dir.join("flash_image.bin");
    let pylibs = esptool_python_path()?;
    let status = Command::new("python3")
        .env("PYTHONPATH", prepend_to_env(&pylibs, "PYTHONPATH"))
        .args([
            "-m",
            "esptool",
            "--chip",
            "esp32s3",
            "elf2image",
            "--flash-mode",
            "dio",
            "--flash-freq",
            "80m",
            "--flash-size",
            "4MB",
            "-o",
        ])
        .arg(&app)
        .arg(elf)
        .status()
        .map_err(|error| format!("could not run `python3 -m esptool`: {error}"))?;
    if !status.success() {
        return Err(format!("`esptool elf2image` of {} failed", elf.display()));
    }
    let app_bytes = std::fs::read(&app)
        .map_err(|error| format!("could not read {}: {error}", app.display()))?;
    let mut image = vec![0xFF_u8; FLASH_SIZE];
    let Some(slot) = image.get_mut(..app_bytes.len()) else {
        return Err(format!(
            "the boot image is {} bytes, which does not fit the 4 MiB flash image",
            app_bytes.len()
        ));
    };
    slot.copy_from_slice(&app_bytes);
    std::fs::write(&flash, &image)
        .map_err(|error| format!("could not write {}: {error}", flash.display()))?;
    Ok(flash)
}

/// Starts `image` on `machine` and reads what it wrote.
fn start(machine: Machine, image: &Path, root: &Path) -> Result<(Option<Census>, String), String> {
    match machine.kind {
        MachineKind::Arm => start_arm(machine, image),
        MachineKind::Xtensa => start_xtensa(machine, image, root),
    }
}

/// Starts `image` on `machine` and reads what it wrote.
fn start_arm(machine: Machine, image: &Path) -> Result<(Option<Census>, String), String> {
    let mut child = Command::new(EMULATOR)
        .args(qemu_args_for(machine, image, Path::new("")))
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

/// Starts the flash image on the ESP32-S3 machine and reads what it wrote.
///
/// The guest never exits, so there is no child status to wait for: the harness polls the
/// serial log until [`xtensa_complete`] sees a full census, then terminates QEMU itself. A
/// run whose log carries the guest's `failed`/`panicked` line is failed at once rather
/// than at the timeout — the guest parks after printing it, so the census will never
/// come, and the line is the run's own account of what went wrong. A run that outlives
/// [`TIMEOUT`] with neither is failed the way a hung ARM run is: the image installs a
/// panic handler that says so on the UART, so reaching the timeout really is a livelock
/// rather than a panic.
fn start_xtensa(
    machine: Machine,
    image: &Path,
    root: &Path,
) -> Result<(Option<Census>, String), String> {
    let log = image
        .parent()
        .map(|dir| dir.join("uart0.log"))
        .ok_or_else(|| format!("{} has no parent directory", image.display()))?;
    // A stale log from an earlier run would read as this run's census.
    let _ = std::fs::remove_file(&log);
    let program = emulator_for(machine, root);
    let mut command = Command::new(&program);
    command
        .args(qemu_args_for(machine, image, &log))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (key, value) in qemu_env_for(machine)? {
        command.env(key, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", program.display()))?;

    let started = Instant::now();
    let census = loop {
        match child.try_wait() {
            Err(error) => return Err(format!("could not wait for {}: {error}", program.display())),
            Ok(Some(status)) => {
                // The guest never exits; a status here is QEMU itself dying.
                let _ = child.wait();
                let output = xtensa_output(&log, &mut child);
                return Err(format!(
                    "the emulator exited {status} before the census was complete, having written: {output}"
                ));
            }
            Ok(None) => {}
        }
        let log_text = std::fs::read_to_string(&log).unwrap_or_default();
        if xtensa_complete(&log_text) {
            let _ = child.kill();
            let _ = child.wait();
            break parse(&log_text);
        }
        // The guest says `failed`/`panicked` and then parks: the census will never come,
        // so failing now keeps the UART's own account in the error instead of burning the
        // timeout and misreporting a failure as a livelock.
        if xtensa_failed(&log_text) {
            let _ = child.kill();
            let _ = child.wait();
            let output = xtensa_output(&log, &mut child);
            return Err(format!(
                "the guest reported failure before any census, having written: {output}"
            ));
        }
        if started.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "the image did not print a census within {}s. It installs a panic handler that says so on the UART, so this is a livelock rather than a panic",
                TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    Ok((census, xtensa_output(&log, &mut child)))
}

/// Everything the Xtensa run wrote: the serial log, then whatever QEMU itself said.
///
/// Kept so that a failure is investigable from the log rather than only named — the
/// report's reason the ARM half's `output` field exists.
fn xtensa_output(log: &Path, child: &mut std::process::Child) -> String {
    let mut output = std::fs::read_to_string(log).unwrap_or_default();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut output);
    }
    output
}

/// The rule id this module's gate reports under.
pub const RULE: &str = "emulation-boot";

/// The crate-root attributes the emulated image must carry.
///
/// `#![no_std]` and `#![no_main]` are what make it firmware rather than a host binary that
/// happens to be built for a target — and a `#![no_main]` binary is the only kind that can
/// have a reset vector placed at all.
pub const REQUIRED_ATTRIBUTES: &[&str] = &["#![no_std]", "#![no_main]"];

/// The identifier the crate is allowed to name, and the keyword it is almost never.
///
/// The whole of the ARM exception this crate carries is that a *macro* it invokes expands
/// to `unsafe`: `#[cortex_m_rt::entry]` writes the exported symbol the reset vector points
/// at, and `debug::exit` performs the semihosting call. Neither is hand-written here, and
/// this is what says so — the crate may name the lint (`unsafe_code`, in the `allow` it
/// declares) and may never write the keyword on ARM. The Xtensa startup is the exception to
/// the exception: there is no `cortex-m-rt` for Xtensa, so its stack install, `.bss`
/// zeroing and UART pokes are hand-written behind a `#[cfg(target_arch = "xtensa")]` gate
/// the rule below checks. Without the ARM half, `#![allow(unsafe_code)]` would be a licence
/// for the whole crate rather than for two macro expansions, and the one place in this
/// workspace where `unsafe` is permitted would be the one place nothing checks.
pub const PERMITTED_LINT_NAME: &str = "unsafe_code";

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
/// The *`unsafe`* half: no file of the crate writes the keyword — except the Xtensa startup
/// module, whose `unsafe` the crate root gates on `#[cfg(target_arch = "xtensa")]`. See
/// [`PERMITTED_LINT_NAME`]: on ARM the exception stays scoped to two macro expansions, and
/// the Xtensa hand-written half can never reach an ARM image because the gate is what
/// decides whether the module compiles at all.
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
/// `rust-toolchain.toml` and named by a pipeline stage — except the ESP32-S3's, which only
/// the Espressif fork toolchain knows and no channel a toolchain file can name ships. A
/// machine added to the table with no stage behind it is a core this gate claims to run on
/// and never does; a target the toolchain does not pin is a stage that fails on a fresh
/// checkout for a reason that is not a defect.
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

/// No file of the crate writes the `unsafe` keyword — except the Xtensa startup module.
///
/// The carve-out is the gate, not the keyword: a file's `unsafe` is permitted only when the
/// crate root declares that file's module behind `#[cfg(target_arch = "xtensa")]`, which is
/// what decides whether the module compiles into an image at all. The Xtensa startup has no
/// `cortex-m-rt` to expand, so its stack install, `.bss` zeroing and UART pokes cannot be
/// spelled without the keyword; gating the module keeps that hand-written half out of every
/// ARM image, where the exception covers two macro expansions and nothing else.
fn check_no_handwritten_unsafe(files: &[&crate::size::LayerSource]) -> Vec<crate::Violation> {
    let mut violations = Vec::new();
    let root = files.iter().find(|source| source.path.ends_with("main.rs"));
    for source in files {
        let code = crate::source::code_only(&source.contents);
        let bytes = code.as_bytes();
        let mut at = 0;
        let mut handwritten = false;
        while let Some(found) = code.get(at..).and_then(|rest| rest.find("unsafe")) {
            let start = at.saturating_add(found);
            let end = start.saturating_add("unsafe".len());
            let before = start
                .checked_sub(1)
                .and_then(|index| bytes.get(index).copied());
            let after = bytes.get(end).copied();
            let is_word_start = before.is_none_or(|byte| !is_identifier_byte(byte));
            // `unsafe_code` is the lint's name and the one thing this crate may say. The
            // keyword is never followed by an identifier byte, so the two cannot be confused.
            let is_the_lint = after.is_some_and(is_identifier_byte);
            if is_word_start && !is_the_lint {
                handwritten = true;
                break;
            }
            at = end;
        }
        if handwritten
            && !root.is_some_and(|root| xtensa_gated_module(&root.path, files, &source.path))
        {
            violations.push(crate::Violation::new(
                RULE,
                source.path.clone(),
                "writes the `unsafe` keyword. The exception this crate carries is for two macro expansions on ARM — the reset vector and the semihosting exit — and hand-written `unsafe` outside the `#[cfg(target_arch = \"xtensa\")]`-gated startup module is the thing nothing else would catch",
            ));
        }
    }
    violations
}

/// Whether `path` is the file of a module the crate declares, and every
/// declaration that resolves to it carries the `#[cfg(target_arch = "xtensa")]` gate.
///
/// Parsed, not scanned (issues #51, #97): [`crate::parse::parse_rust`] reads every
/// collected source file as a syntax tree — the crate root's module items are not the
/// whole story, because a nested module can include the gated file too (Codex P2,
/// `review_comment` 3995327724: an ungated `mod helpers;` in the root with
/// `#[path = "xtensa.rs"] mod arm_startup;` in `helpers.rs` compiles the same
/// hand-written `unsafe` into an ARM image the root's gated `mod xtensa;` never
/// excuses — and Codex P2, `review_comment` 3995553789: an ungated
/// `mod wrapper { #[path = "../xtensa.rs"] mod arm_startup; }` in the root resolves
/// the path against `src/wrapper/`, not `src/` — and Codex P2, `review_comment`
/// 3995614205: a `#[cfg_attr(.., path = "...")]` on an enclosing inline module
/// conditionally redirects that directory, and every conditional directory is a
/// candidate a nested declaration can resolve against). The gate is one of the attributes on a matching `mod` item being
/// exactly `#[cfg(target_arch = "xtensa")]`. The exemption is refused unless *every*
/// declaration resolving to the file is gated: an ungated
/// `#[path = "xtensa.rs"] mod arm_startup;` includes the same file in an ARM image,
/// where the hand-written `unsafe` the keyword check found has no exception to stand
/// under — and so does `#[cfg_attr(target_arch = "arm", path = "xtensa.rs")] mod arm_startup;`,
/// whose predicate this checker does not evaluate and whose conditional path therefore
/// counts as reaching the file.
/// `#[path]` on the gated declaration itself is honored the same way — the file it names
/// is the gated one, not `<ident>.rs`. A macro inclusion is a declaration too:
/// `include!("xtensa.rs")` pastes the file's contents — hand-written `unsafe` included —
/// into the including file's compilation, and it is an `ItemMacro`, not an `ItemMod`,
/// so [`module_declarations`] never sees it (Codex P2, `review_comment` 3995859257: an
/// ARM-reachable `include!("xtensa.rs")` compiled the gated file's hand-written `unsafe`
/// into an ARM image no `mod` declaration the gate check reads). Every `include!`
/// resolving to the file must therefore carry the gate as well; one whose argument is
/// not a string literal names a file the checker cannot enumerate and fails closed, as
/// an unparsable file does — and as does a `macro_rules!` body that could generate an
/// inclusion, whose nested `include!` the invocation walk never sees (Codex P2,
/// `review_comment` 3995964718) — and as does one that could generate a module
/// declaration, whose nested `mod` item the module-declaration walk never sees
/// (Codex P2, `review_comment` 3997403828: an ARM-reachable `macro_rules!`
/// expanding to `#[path = "xtensa.rs"] mod arm_startup;` compiles the gated
/// file's hand-written `unsafe` into an ARM image the separately gated
/// `mod xtensa;` still excuses). `include_str!` and `include_bytes!` produce data, not
/// compiled code, so they are not declarations. A file that does not parse fails
/// closed: no gate is found.
fn xtensa_gated_module(root_path: &str, files: &[&crate::size::LayerSource], path: &str) -> bool {
    let target = normalize_path(Path::new(path));
    let mut declared = false;
    // Names of `macro_rules!` definitions anywhere in the crate, so the
    // inclusion walk can tell a locally defined macro (body scanned) from an
    // external one (body invisible — fail closed). Collected once: the walk
    // runs per file.
    let local_macros = local_macro_names(files);
    for source in files {
        let Ok(tree) = crate::parse::parse_rust(&source.contents) else {
            // A file whose module items cannot be read may hide an ungated
            // declaration reaching the file: fail closed, as the root does.
            return false;
        };
        let Some(dirs) = resolution_dirs(&source.path, source.path == root_path) else {
            // A file with no directory to resolve its declarations against is one
            // whose declarations cannot be enumerated: fail closed.
            return false;
        };
        for (module, module_dirs, path_dirs) in module_declarations(&tree, &dirs) {
            if !module_resolves_to(&path_dirs, &module_dirs, module, &target) {
                continue;
            }
            declared = true;
            if !has_xtensa_cfg(&module.attrs) {
                // Declared without the gate, so the file compiles into an ARM image
                // and the exception does not apply.
                return false;
            }
        }
        let Some(inclusions) = include_declarations(&tree, &dirs.file_dir, &local_macros) else {
            // An `include!` whose argument this checker cannot resolve may hide an
            // ungated inclusion reaching the file: fail closed, as the root does.
            return false;
        };
        for (attrs, included) in &inclusions {
            if normalize_path(included) != target {
                continue;
            }
            declared = true;
            if !has_xtensa_cfg(attrs) {
                // Included without the gate, so the file compiles into an ARM image
                // and the exception does not apply.
                return false;
            }
        }
    }
    declared
}

/// The directories `mod` items in one source file resolve against.
///
/// `#[path = "..."]` — and a `#[cfg_attr(.., path = "...")]` — on a declaration at the
/// file's top level names the file relative to the declaring file's directory; nested
/// inside an inline module it resolves against the inline module's directory instead
/// (see [`module_declarations`]). A plain `mod <ident>;` resolves to
/// `<module_dir>/<ident>.rs` or `<module_dir>/<ident>/mod.rs`, where the module
/// directory is the crate root's own directory for the root file, `<dir>/<stem>/`
/// for `<dir>/<stem>.rs`, and `<dir>/` for `<dir>/mod.rs`.
struct ResolutionDirs {
    /// The directory top-level `#[path]` spellings are relative to.
    file_dir: PathBuf,
    /// The directory plain `mod <ident>;` resolves against.
    module_dir: PathBuf,
}

fn resolution_dirs(path: &str, is_root: bool) -> Option<ResolutionDirs> {
    let file = Path::new(path);
    let file_dir = file.parent()?.to_path_buf();
    let module_dir = if is_root || file.file_name().and_then(|name| name.to_str()) == Some("mod.rs")
    {
        file_dir.clone()
    } else {
        file_dir.join(file.file_stem()?)
    };
    Some(ResolutionDirs {
        file_dir,
        module_dir,
    })
}

/// Every non-inline `mod` item in the file: the item, the candidate module
/// directories a plain `mod <ident>;` resolves against, and the candidate
/// directories `#[path]` spellings resolve against.
///
/// [`syn::visit::Visit`] walks the whole syntax tree: a `mod` item inside an inline
/// module or a function body still compiles its file, so it counts. The module
/// directory follows inline nesting — inside `mod wrapper` in a file whose module
/// directory is `dir`, a plain declaration resolves against `dir/wrapper` — while a
/// `mod` in a function body resolves against the enclosing directory, a function
/// declaring no module directory of its own.
///
/// The path directory is the innermost enclosing inline module's directory: rustc
/// resolves `#[path]` inside an inline module body against that directory, not the
/// declaring file's (Codex P2, `review_comment` 3995553789 — `mod wrapper {
/// #[path = "../xtensa.rs"] mod arm_startup; }` in the root loads `src/xtensa.rs`,
/// resolved against `src/wrapper/`). A declaration at the file's top level keeps the
/// declaring file's directory. A `#[path]` on the inline module itself redirects its
/// directory the way rustc redirects it, so nested declarations resolve against the
/// redirected one. A `#[cfg_attr(.., path = "...")]` on an inline module is modeled as
/// one extra candidate directory per conditional path (Codex P2, `review_comment`
/// 3995614205 — `#[cfg_attr(target_arch = "arm", path = "custom/deep")] mod wrapper`
/// makes a nested `#[path = "../../xtensa.rs"]` load `src/xtensa.rs` on ARM, while the
/// plain `src/wrapper/` misses it). This checker does not evaluate predicates, so a
/// conditional path counts as a directory the module could live in; the extra
/// candidates can only add declarations that must carry the gate, never remove them —
/// the exemption stays fail-closed.
fn module_declarations<'ast>(
    file: &'ast syn::File,
    dirs: &ResolutionDirs,
) -> Vec<(&'ast syn::ItemMod, Vec<PathBuf>, Vec<PathBuf>)> {
    struct Declarations<'ast> {
        file_dir: PathBuf,
        dir_stack: Vec<Vec<PathBuf>>,
        found: Vec<(&'ast syn::ItemMod, Vec<PathBuf>, Vec<PathBuf>)>,
    }

    impl<'ast> syn::visit::Visit<'ast> for Declarations<'ast> {
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            // An inline module declares no file, but its nested declarations do —
            // against the inline module's directory, which its own `#[path]`
            // redirects the way rustc redirects it, and which each `#[cfg_attr(..,
            // path = "...")]` on it conditionally redirects too.
            let parents = self.dir_stack.last().cloned().unwrap_or_default();
            if module.content.is_none() {
                // A `#[path]` at the file's top level resolves against the
                // declaring file's directory; nested in an inline module, against
                // the inline module's directory.
                let path_dirs = if self.dir_stack.len() > 1 {
                    parents.clone()
                } else {
                    vec![self.file_dir.clone()]
                };
                self.found.push((module, parents, path_dirs));
            } else if let Some((_, items)) = &module.content {
                let mut nested: Vec<PathBuf> = Vec::new();
                for parent in &parents {
                    let plain = module.attrs.iter().find_map(path_value).map_or_else(
                        || parent.join(module.ident.to_string()),
                        |path| parent.join(path),
                    );
                    nested.push(plain);
                    // Conditional first: a `#[cfg_attr]` predicate this checker does
                    // not evaluate may still be active, so each conditional path is
                    // one more directory the nested declarations could resolve
                    // against.
                    nested.extend(
                        module
                            .attrs
                            .iter()
                            .flat_map(cfg_attr_path_values)
                            .map(|path| parent.join(path)),
                    );
                }
                self.dir_stack.push(nested);
                for item in items {
                    self.visit_item(item);
                }
                self.dir_stack.pop();
            }
        }
    }

    let mut declarations = Declarations {
        file_dir: dirs.file_dir.clone(),
        dir_stack: vec![vec![dirs.module_dir.clone()]],
        found: Vec::new(),
    };
    declarations.visit_file(file);
    declarations.found
}

/// Every `include!("...")` the checker can resolve: the attributes on the
/// invocation and the file it names, resolved against the including file's
/// directory.
///
/// rustc resolves `include!` against the file containing the invocation — not
/// against any module directory — so the including file's own directory is the
/// only directory an inclusion needs. The last path segment is the name that
/// counts: `core::include!` pastes code the same way a bare `include!` does,
/// and treating any other macro that happens to be named `include` as an
/// inclusion can only add declarations that must carry the gate, never remove
/// them. A renamed import is tracked the same way: `use core::include as
/// paste;` names the builtin through the local name `paste`, so a `paste!`
/// invocation is an inclusion with the invocation's own attributes — without
/// this the inclusion walk would miss it and the hand-written `unsafe` could
/// reach an ARM image under the Xtensa exemption (Codex P2, `review_comment`
/// 3996723105). `include_str!` and `include_bytes!` produce data, not compiled code,
/// so they cannot carry hand-written `unsafe` into an image and are not
/// declarations here. An `include!` whose argument is not a string literal (for
/// example `include!(concat!(...))`) names a file this checker cannot enumerate:
/// like an unparsable file, that fails closed and the whole exemption is refused
/// (Codex P2, `review_comment` 3995859257). A `macro_rules!` definition whose
/// body could generate an inclusion fails closed the same way: [`syn`] leaves
/// the body as opaque tokens, so a nested `include!` never reaches the
/// invocation walk, and the definition could paste the gated file into an ARM
/// image at any invocation site (Codex P2, `review_comment` 3995964718). A
/// nested invocation of a macro the checker cannot account for fails closed
/// the same way: the body holds no `include!` / `mod` shape for the token
/// scans to see, so each nested invocation path is classified with the
/// top-level accounting rule — `macro_rules! wrapper { () => {
/// evil::load_startup!(); } }` cannot smuggle the dependency's invisible
/// expansion past the definition scan (Codex P2, `review_comment`
/// 3997946868). The body
/// scan sees the file's renamed imports too: the parsed-invocation alias
/// handling never reaches opaque tokens, so `use core::include as paste;` plus
/// `paste!(...)` in a body would otherwise slip past (Codex P2,
/// Compiler-builtin function-like macros whose expansions are fixed by rustc.
///
/// A builtin's expansion is not influenced by any dependency and contains no
/// items at all — so it can neither declare a module nor paste one through
/// `include!`. The inclusion walk may therefore ignore invocations that are
/// really these builtins. "Really" is doing the work: the whitelist applies
/// only when the name is not shadowed — no local `macro_rules!` defines it,
/// no `use` imports it from another crate, no `use` binds it through an
/// unresolvable local path (`use crate::shim::assert;` may name a re-exported
/// foreign macro — Codex P2, `review_comment` 3997946866), and the file has no
/// `#[macro_use] extern crate` whose textual-scope macros could shadow a
/// builtin. Anything shadowed falls back to fail-closed (Codex P2,
/// `review_comment` 3997510667).
const BUILTIN_MACROS: &[&str] = &[
    "addr_of",
    "addr_of_mut",
    "asm",
    "assert",
    "assert_eq",
    "assert_ne",
    "cfg",
    "column",
    "compile_error",
    "concat",
    "dbg",
    "env",
    "file",
    "format_args",
    "format_args_nl",
    "global_asm",
    "include_bytes",
    "include_str",
    "line",
    "log_syntax",
    "matches",
    "module_path",
    "naked_asm",
    "option_env",
    "panic",
    "stringify",
    "todo",
    "trace_macros",
    "unimplemented",
    "unreachable",
    "vec",
    "write",
    "writeln",
];

/// External macros whose expansions have been verified, by reading the
/// Cargo.lock-pinned dependency source, to contain no `mod` item and no
/// `include!` — so an invocation can neither declare the gated module nor
/// paste it.
///
/// Each entry is `(crate_name, macro_name)` with the crate name as it appears
/// in `use` paths (underscores, not dashes). The exemption applies only when
/// the invocation resolves to exactly that pair: a single-segment invocation
/// imported from the named crate (renames honored), or a path rooted at the
/// named crate. A bare name is never trusted on its own — another crate could
/// export the same name with a hostile body, and the checker cannot read it
/// (Codex P2, `review_comment` 3997510667) — and a path rooted at the named
/// crate is trusted only when no local item shadows the root
/// ([`shadowed_path_roots`]; Codex P2, `review_comment` 3997890784).
///
/// `cortex-m-semihosting 0.5.0`'s `hprintln!` is the only external macro the
/// firmware invokes; its arms expand to `$crate::export::hstdout_str` /
/// `$crate::export::hstdout_fmt` — no `mod`, no `include!`. Re-verify against
/// the pinned source if the dependency version changes: a new macro body is
/// new unexamined expansion.
const EXTERNAL_MACRO_EXEMPTIONS: &[(&str, &str)] = &[("cortex_m_semihosting", "hprintln")];

/// External procedural-attribute macros whose expansions have been verified, by
/// reading the Cargo.lock-pinned dependency source, to contain no `mod` item
/// and no `include!` — so an attribute applied to an ARM-reachable item can
/// neither declare the gated module nor paste it.
///
/// Each entry is `(crate_name, macro_name)` with the crate name as it appears
/// in `use` paths (underscores, not dashes). The exemption applies only when
/// the attribute resolves to exactly that pair: a single-segment attribute
/// imported from the named crate (renames honored), or a path rooted at the
/// named crate. A bare name is never trusted on its own — no prelude provides
/// attribute macros, so a bare `#[entry]` with no import cannot be the pinned
/// one, and another crate could export the same name with a hostile body the
/// checker cannot read (Codex P2, `review_comment` 3997629593) — and a path
/// rooted at the named crate is trusted only when no local item shadows the
/// root ([`shadowed_path_roots`]; Codex P2, `review_comment` 3997890784).
///
/// `cortex-m-rt 0.7.6`'s `#[entry]` is the only attribute macro the firmware
/// applies; its expansion renames the input function, emits the exported
/// trampoline that calls it, and hoists `static mut` locals into explicit
/// arguments — no `mod`, no `include!` (read in the pinned
/// `cortex-m-rt-macros` source). Re-verify against the pinned source if the
/// dependency version changes: a new macro body is new unexamined expansion.
const EXTERNAL_ATTRIBUTE_EXEMPTIONS: &[(&str, &str)] = &[("cortex_m_rt", "entry")];

/// Builtin attributes: inert compiler directives whose tokens cannot paste
/// code, so the inclusion walk ignores them.
///
/// The whitelist applies only when the name is not shadowed: a `use` importing
/// the name from another crate may name an attribute macro instead of the
/// builtin (`use evil::cfg;` plus `#[cfg(..)]`), a `use` binding the name
/// through an unresolvable local path may name one re-exported from another
/// crate (`use crate::shim::cfg;` — Codex P2, `review_comment` 3997946866),
/// and a glob import from an external crate (`use other::*;`) may bring an
/// attribute macro of the name into scope. Any of them fails the exemption
/// closed. `#[macro_use] extern crate` cannot shadow an attribute — textual
/// scope carries only `macro_rules!` macros, which are never usable as
/// attributes — so it is not consulted here.
///
/// `derive` is a builtin attribute with macro-shaped tokens, so it is
/// classified separately ([`STD_DERIVES`]) rather than ignored here.
const BUILTIN_ATTRIBUTES: &[&str] = &[
    "allow",
    "automatically_derived",
    "cfg",
    "cfg_attr",
    "cold",
    "crate_name",
    "crate_type",
    "deny",
    "deprecated",
    "diagnostic",
    "doc",
    "export_name",
    "feature",
    "forbid",
    "global_allocator",
    "ignore",
    "inline",
    "lang",
    "link",
    "link_name",
    "link_section",
    "must_use",
    "naked",
    "no_builtins",
    "no_coverage",
    "no_debug",
    "no_implicit_prelude",
    "no_main",
    "no_mangle",
    "no_start",
    "no_std",
    "non_exhaustive",
    "optimize",
    "panic_handler",
    "path",
    "prelude_import",
    "recursion_limit",
    "repr",
    "rustfmt",
    "should_panic",
    "target_feature",
    "test",
    "thread_local",
    "track_caller",
    "type_length_limit",
    "unsafe",
    "used",
    "warn",
    "windows_subsystem",
];

/// Derive macros whose expansions are fixed by the compiler: they emit trait
/// impls for the deriving type only, so no `mod` item and no `include!` can
/// come out of them.
///
/// Like the builtin attributes, the whitelist applies only when unshadowed: a
/// `use` importing the name from another crate (`use other::Debug;`) or a glob
/// import from an external crate may name a custom derive instead. Any other
/// derive name — a custom derive, or a crate-qualified path like
/// `#[derive(serde::Serialize)]` — is an unexamined external expansion and
/// fails the exemption closed. `macro_rules!` cannot define a derive, so local
/// macros and `#[macro_use] extern crate` cannot shadow these.
const STD_DERIVES: &[&str] = &[
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "Eq",
    "Hash",
    "Ord",
    "PartialEq",
    "PartialOrd",
];

/// Classifies attributes for the inclusion walk's fail-closed rule.
///
/// Procedural attribute macros are a macro surface the item/statement/
/// expression visits never see: an external `#[startup]` on an ARM-reachable
/// item can expand — in its defining crate, invisible to this checker — to
/// `#[path = "xtensa.rs"] mod arm_startup;`, declaring the gated file in an
/// ARM image the separately gated `mod xtensa;` still excuses (Codex P2,
/// `review_comment` 3997629593). The walk therefore accounts for every
/// attribute the way it accounts for every macro invocation:
/// - builtin attributes ([`BUILTIN_ATTRIBUTES`]) are inert compiler
///   directives, ignored only when unshadowed by an external import —
///   except `cfg_attr`, whose emitted attributes are classified recursively
///   (an emitted procedural macro is an unexamined external expansion, Codex
///   P2, `review_comment` 3997773904);
/// - `derive` dispatches to derive macros by name, and only the std derives
///   ([`STD_DERIVES`]) are ignored;
/// - the pinned external-attribute exemptions
///   ([`EXTERNAL_ATTRIBUTE_EXEMPTIONS`]) resolve through the file's imports
///   (renames honored) or a path rooted at the exempted crate — a bare name
///   is never trusted;
/// - anything else is not accounted for, and fails the exemption closed,
///   because its expansion is one the checker cannot enumerate.
struct AttributeGate<'a> {
    macro_imports: &'a MacroImports,
    shadowed_roots: &'a HashSet<String>,
}

impl AttributeGate<'_> {
    /// Whether the attribute is one the checker can account for without
    /// failing closed.
    fn is_accounted_for(&self, attr: &syn::Attribute) -> bool {
        let path = attr.path();
        if path.segments.len() > 1 {
            // A multi-segment attribute path names its crate outright: only a
            // pinned exemption passes. (`crate::` / `self::` / `super::`
            // resolve inside the crate, where no attribute macro can be
            // defined — a procedural attribute always comes from another
            // crate — so those fail closed too.) The pinned exemption trusts
            // the root to name the extern crate, but a local item shadows the
            // extern prelude: `mod cortex_m_rt { pub use evil::entry; }` makes
            // `#[cortex_m_rt::entry]` apply evil's macro, not the verified
            // one — so a shadowed root fails closed (Codex P2, `review_comment`
            // 3997890784).
            let root = path
                .segments
                .first()
                .map(|segment| segment.ident.to_string())
                .unwrap_or_default();
            let name = path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
                .unwrap_or_default();
            if self.shadowed_roots.contains(&root) {
                return false;
            }
            return EXTERNAL_ATTRIBUTE_EXEMPTIONS.contains(&(root.as_str(), name.as_str()));
        }
        let name = path
            .segments
            .first()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default();
        if name == "derive" {
            return self.derive_is_accounted_for(attr);
        }
        if let Some((defining_crate, original)) = self.macro_imports.imports.get(&name) {
            // Imported from another crate: only the pinned exemption passes,
            // keyed on the (crate, name) pair.
            return EXTERNAL_ATTRIBUTE_EXEMPTIONS
                .contains(&(defining_crate.as_str(), original.as_str()));
        }
        if self.macro_imports.local_imports.contains(&name) {
            // Bound through a local path (`use crate::shim::cfg;`) the
            // checker cannot resolve through the module tree: it may name an
            // attribute macro re-exported from another crate, so the
            // attribute is not provably the builtin. Same hole as the macro
            // side (Codex P2, `review_comment` 3997946866).
            return false;
        }
        if self.macro_imports.has_external_glob {
            // `use other::*;` may bring an attribute macro of this name into
            // scope, shadowing whatever the name would otherwise mean.
            return false;
        }
        if EXTERNAL_ATTRIBUTE_EXEMPTIONS
            .iter()
            .any(|(_, macro_name)| *macro_name == name)
        {
            // A bare `#[entry]` with no import names no crate: it cannot
            // resolve to the pinned exemption, and no prelude provides
            // attribute macros.
            return false;
        }
        if name == "cfg_attr" {
            // `cfg_attr` is a builtin only in the sense that rustc evaluates
            // it: the attributes it emits are classified like any attribute
            // written out by hand. `#[cfg_attr(target_arch = "arm",
            // evil::startup)]` applies the procedural macro under the
            // predicate, so the whitelist must not swallow it (Codex P2,
            // `review_comment` 3997773904).
            return self.cfg_attr_is_accounted_for(attr);
        }
        BUILTIN_ATTRIBUTES.contains(&name.as_str())
    }

    /// Whether a `#[derive(...)]` is one the checker can account for: every
    /// derived name must be a std derive ([`STD_DERIVES`]) — whose expansion is
    /// trait impls only — unshadowed by an external import. A custom or
    /// crate-qualified derive is an unexamined external expansion, as is a
    /// derive list the checker cannot parse.
    fn derive_is_accounted_for(&self, attr: &syn::Attribute) -> bool {
        let parsed = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        );
        let Ok(paths) = parsed else {
            return false;
        };
        for path in paths {
            let mut segments = path.segments.iter();
            let (Some(first), None) = (segments.next(), segments.next()) else {
                // Not a single-segment path: `#[derive(serde::Serialize)]`
                // names a derive from another crate.
                return false;
            };
            let name = first.ident.to_string();
            if !STD_DERIVES.contains(&name.as_str()) {
                return false;
            }
            // `macro_rules!` cannot define a derive, so local macros and
            // `#[macro_use] extern crate` cannot shadow one — but a `use`
            // importing the name from another crate can, as can a `use`
            // binding it through an unresolvable local path (Codex P2,
            // `review_comment` 3997946866).
            if self.macro_imports.imports.contains_key(&name)
                || self.macro_imports.local_imports.contains(&name)
                || self.macro_imports.has_external_glob
            {
                return false;
            }
        }
        true
    }

    /// Whether a `#[cfg_attr(..)]` is one the checker can account for: every
    /// attribute it can emit — the metas after the predicate — must itself be
    /// accounted for, recursing into nested `cfg_attr`s. The checker does not
    /// evaluate the predicate, so an emitted attribute counts as applied; an
    /// argument list the checker cannot parse fails closed, like everything
    /// else the checker cannot enumerate.
    fn cfg_attr_is_accounted_for(&self, attr: &syn::Attribute) -> bool {
        let parsed = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        );
        let Ok(metas) = parsed else {
            return false;
        };
        let mut emitted = metas.into_iter();
        // The first meta is the predicate, not an emitted attribute.
        if emitted.next().is_none() {
            return false;
        }
        for meta in emitted {
            // Re-wrap the meta as an outer attribute so the whole classifier —
            // shadowing checks, `derive` handling, nested `cfg_attr` — applies
            // to it unchanged.
            let Ok(mut emitted_attrs) = syn::Attribute::parse_outer.parse2(quote::quote!(#[#meta]))
            else {
                return false;
            };
            let Some(emitted_attr) = emitted_attrs.pop() else {
                return false;
            };
            if !self.is_accounted_for(&emitted_attr) {
                return false;
            }
        }
        true
    }
}

/// Macro names a file imports from other crates: local name to (defining
/// crate, original name).
struct MacroImports {
    imports: HashMap<String, (String, String)>,
    /// Names a `use` binds through a path rooted at `crate::`, `self::`, or
    /// `super::`: the checker cannot resolve the path through the module
    /// tree, so a bare invocation of the name may name a macro re-exported
    /// from another crate (`mod shim { pub use evil::assert; }` in another
    /// file, plus `use crate::shim::assert;`) rather than the builtin or
    /// local definition the name suggests. Like [`imports`], this walks
    /// every `use` in the syntax tree and over-approximates scope — the
    /// error direction is fail-closed (Codex P2, `review_comment`
    /// 3997946866).
    local_imports: HashSet<String>,
    /// A glob import from an external crate (`use dependency::*;`): any macro
    /// name may come from it, so no single-segment invocation can be proven
    /// to be a builtin.
    has_external_glob: bool,
}

/// Every macro name a file imports from another crate, plus every name it
/// binds through a local path.
///
/// Only `use` paths rooted at an external crate name count for [`imports`]:
/// `crate::`, `self::`, and `super::` resolve inside the crate, and `core::`
/// / `std::` name the builtin namespace. Renames are honored (`use
/// cortex_m_semihosting::hprintln as hlog;` maps `hlog` to
/// `(cortex_m_semihosting, hprintln)`). Names bound through `crate::` /
/// `self::` / `super::` go to [`local_imports`] instead: the checker cannot
/// resolve those paths through the module tree, so they are recorded without
/// a defining crate and fail closed at classification time. Like
/// [`include_aliases`], this walks every `use` in the syntax tree and
/// over-approximates scope — the error direction is fail-closed: a name that
/// is not really imported from the recorded crate only costs a refused
/// exemption, never a granted one.
fn external_macro_imports(file: &syn::File) -> MacroImports {
    fn is_local_root(root: &str) -> bool {
        matches!(root, "crate" | "self" | "super" | "core" | "std")
    }

    fn is_unresolvable_local_root(root: &str) -> bool {
        matches!(root, "crate" | "self" | "super")
    }

    fn is_builtin_root(root: &str) -> bool {
        matches!(root, "core" | "std")
    }

    /// Record one bound name: precisely as `(crate, name)` when the root
    /// names an external crate, or as an unresolvable local binding when the
    /// root is `crate::` / `self::` / `super::`. `core::` / `std::` name the
    /// builtin namespace and are not recorded at all: the builtin whitelists
    /// classify those names directly, exactly as before the local-import
    /// split — recording them here would, for example, make `use
    /// core::fmt::Debug;` shadow the `#[derive(Debug)]` exemption, or `use
    /// core::cfg;` shadow the `#[cfg]` one, both of which compile and both of
    /// which must keep their exemptions.
    fn record_name(out: &mut MacroImports, root: &str, local: String, original: String) {
        if is_unresolvable_local_root(root) {
            out.local_imports.insert(local);
        } else if !is_builtin_root(root) {
            out.imports.insert(local, (root.to_owned(), original));
        }
    }

    fn walk(tree: &syn::UseTree, root: Option<&String>, out: &mut MacroImports) {
        match tree {
            syn::UseTree::Path(path) => {
                let next = root.cloned().or_else(|| Some(path.ident.to_string()));
                walk(&path.tree, next.as_ref(), out);
            }
            syn::UseTree::Name(name) => {
                if let Some(root) = root {
                    record_name(out, root, name.ident.to_string(), name.ident.to_string());
                }
            }
            syn::UseTree::Rename(rename) => {
                if let Some(root) = root {
                    record_name(
                        out,
                        root,
                        rename.rename.to_string(),
                        rename.ident.to_string(),
                    );
                }
            }
            syn::UseTree::Glob(_) => {
                if let Some(root) = root {
                    if !is_local_root(root) {
                        out.has_external_glob = true;
                    }
                }
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    walk(item, root, out);
                }
            }
        }
    }

    struct Collector {
        out: MacroImports,
    }

    impl<'ast> syn::visit::Visit<'ast> for Collector {
        fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
            walk(&node.tree, None, &mut self.out);
            syn::visit::visit_item_use(self, node);
        }
    }

    let mut collector = Collector {
        out: MacroImports {
            imports: HashMap::new(),
            local_imports: HashSet::new(),
            has_external_glob: false,
        },
    };
    collector.visit_file(file);
    collector.out
}

/// Whether the file brings another crate's macros into textual scope wholesale.
///
/// `#[macro_use] extern crate dependency;` puts that crate's exported macros
/// in textual scope, where they can shadow even compiler builtins — so with
/// one present, no single-segment invocation can be proven to be a builtin and
/// the whitelist does not apply (Codex P2, `review_comment` 3997510667).
fn has_macro_use_extern_crate(file: &syn::File) -> bool {
    struct Finder {
        found: bool,
    }

    impl<'ast> syn::visit::Visit<'ast> for Finder {
        fn visit_item_extern_crate(&mut self, node: &'ast syn::ItemExternCrate) {
            if node
                .attrs
                .iter()
                .any(|attr| attr.path().is_ident("macro_use"))
            {
                self.found = true;
            }
            syn::visit::visit_item_extern_crate(self, node);
        }
    }

    let mut finder = Finder { found: false };
    finder.visit_file(file);
    finder.found
}

/// The names of every `macro_rules!` definition in the scanned crate.
///
/// A `macro_rules!` body in any scanned file is examined by the definition
/// scan in [`include_declarations`], so an invocation naming a local macro is
/// accounted for: the definition's tokens and the invocation's tokens are
/// both scanned for `include!`/`mod` shapes. Scope is deliberately
/// over-approximated crate-wide — a name defined anywhere local counts as
/// local everywhere — because the body scan's error direction is fail-closed
/// anyway, and a cross-file `#[macro_export]` macro is still this crate's
/// macro. A file that does not parse is skipped: the caller fails closed on
/// it separately.
fn local_macro_names(files: &[&crate::size::LayerSource]) -> HashSet<String> {
    struct Collector<'a> {
        names: &'a mut HashSet<String>,
    }

    impl<'ast> syn::visit::Visit<'ast> for Collector<'_> {
        fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
            if node.mac.path.is_ident("macro_rules") {
                if let Some(name) = node.ident.as_ref() {
                    self.names.insert(name.to_string());
                }
            }
            syn::visit::visit_item_macro(self, node);
        }
    }

    let mut names = HashSet::new();
    for source in files {
        let Ok(tree) = crate::parse::parse_rust(&source.contents) else {
            continue;
        };
        Collector { names: &mut names }.visit_file(&tree);
    }
    names
}

/// The inclusion walk's state: every `include!` the checker can resolve,
/// and whether it met anything it cannot enumerate (which fails the
/// exemption closed).
struct Inclusions<'ast> {
    file_dir: PathBuf,
    found: Vec<(&'ast [syn::Attribute], PathBuf)>,
    unresolvable: bool,
    /// Local names that name the builtin `include!` macro through a
    /// renamed import (`use core::include as paste;`): an invocation
    /// `paste!(...)` pastes code exactly like `include!` does, so the walk
    /// records it as an inclusion (Codex P2, `review_comment` 3996723105).
    include_aliases: HashSet<String>,
    /// Names of `macro_rules!` definitions in the scanned crate: an
    /// invocation naming one is accounted for by the definition scan.
    local_macros: HashSet<String>,
    /// Macro names this file imports from other crates, for resolving
    /// single-segment invocations to their defining crate.
    macro_imports: MacroImports,
    /// `#[macro_use] extern crate` in this file: textual-scope macros may
    /// shadow builtins, so the builtin whitelist does not apply.
    has_macro_use_extern: bool,
    /// Names a local item or import binds that could shadow an extern crate
    /// root in a path's leading segment ([`shadowed_path_roots`]): a pinned
    /// `(crate, name)` exemption fails closed on a shadowed root.
    shadowed_roots: HashSet<String>,
}

impl<'ast> Inclusions<'ast> {
    /// Whether the invocation is one the checker can account for without
    /// failing closed.
    ///
    /// A compiler builtin ([`BUILTIN_MACROS`]) has a fixed expansion with
    /// no items in it — but only when the name is not shadowed: a local
    /// `macro_rules!` with the same name, a `use` importing it from
    /// another crate, or a `#[macro_use] extern crate` all mean the
    /// invocation may not be the builtin. A path rooted at `core::` or
    /// `std::` names the builtin namespace directly. A path rooted at
    /// `crate::`, `self::`, or `super::` resolves inside the crate: a locally
    /// defined macro is accounted for by the definition scan, unless an
    /// import of the same name from another crate shadows it. Any other
    /// multi-segment root may name an external crate and fails closed — a
    /// local name never excuses an explicit external path (Codex P2,
    /// `review_comment` 3997773907). A bare invocation naming both a local
    /// `macro_rules!` and an import from another crate resolves to the import
    /// — the local definition's textual scope covers only its own module — so
    /// imports resolve before the crate-wide local-name set (Codex P2,
    /// `review_comment` 3997890781). An external macro
    /// is accounted for only on the pinned exemption list
    /// ([`EXTERNAL_MACRO_EXEMPTIONS`]), resolved through the file's
    /// imports or a path rooted at the exempted crate. A `use` rooted at
    /// `crate::` / `self::` / `super::` binds the name through a local path
    /// the checker cannot resolve through the module tree, so it fails
    /// closed like an unresolvable external import (Codex P2,
    /// `review_comment` 3997946866). Anything else —
    /// notably a macro exported by another crate, whose body this checker
    /// cannot read — may expand to a `mod` item or an `include!` the
    /// token scans cannot see, so it is not accounted for (Codex P2,
    /// `review_comment` 3997510667).
    fn macro_is_accounted_for(&self, mac: &syn::Macro) -> bool {
        self.macro_path_is_accounted_for(&mac.path)
    }

    /// Whether an invocation of the macro named by `path` is one the checker
    /// can account for without failing closed.
    ///
    /// The path form of [`macro_is_accounted_for`]: the definition scan
    /// classifies macro invocations nested inside `macro_rules!` bodies,
    /// which [`syn`] leaves as opaque tokens rather than parsed [`syn::Macro`]
    /// nodes (Codex P2, `review_comment` 3997946868).
    fn macro_path_is_accounted_for(&self, path: &syn::Path) -> bool {
        let Some(last) = path.segments.last() else {
            return false;
        };
        let name = last.ident.to_string();
        if path.segments.len() > 1 {
            let root = path
                .segments
                .first()
                .map(|segment| segment.ident.to_string())
                .unwrap_or_default();
            if root == "core" || root == "std" {
                return BUILTIN_MACROS.contains(&name.as_str());
            }
            // The pinned external-macro exemption trusts the path's root to
            // name the exempted crate — but a local item shadows the extern
            // prelude, so a shadowed root may resolve to a hostile local
            // definition instead of the verified macro. Same hole as the
            // attribute side (Codex P2, `review_comment` 3997890784), found in
            // this arm while verifying it.
            if self.shadowed_roots.contains(&root) {
                return false;
            }
            if EXTERNAL_MACRO_EXEMPTIONS.contains(&(root.as_str(), name.as_str())) {
                return true;
            }
            // `crate::` / `self::` / `super::` resolve inside the crate — but
            // an import of the same name from another crate shadows the local
            // definition (`use dependency::name;` plus `self::name!()` invokes
            // the dependency's macro), so imports resolve before the local
            // exemption. Any other root may name an external crate:
            // `dependency::load_startup!()` alongside an unrelated local
            // `macro_rules! load_startup` invokes the dependency's macro,
            // whose expansion is invisible here — and a bare leading
            // identifier may name a local module path the checker cannot
            // resolve. Both fail closed (Codex P2, `review_comment`
            // 3997773907).
            if root == "crate" || root == "self" || root == "super" {
                if let Some((defining_crate, original)) = self.macro_imports.imports.get(&name) {
                    return EXTERNAL_MACRO_EXEMPTIONS
                        .contains(&(defining_crate.as_str(), original.as_str()));
                }
                // A `use` binding the last segment through another local path
                // may name a re-exported foreign macro — `use
                // crate::shim::assert;` plus `self::assert!()` invokes
                // `evil::assert` if `shim` re-exports it — so the qualified
                // path is not provably local either (Codex P2,
                // `review_comment` 3997946866).
                if self.macro_imports.local_imports.contains(&name) {
                    return false;
                }
                if self.macro_imports.has_external_glob || self.has_macro_use_extern {
                    return false;
                }
                return self.local_macros.contains(&name);
            }
            return false;
        }
        // The file's imports resolve before the crate-wide over-approximated
        // local-name set: a same-named `macro_rules!` in another module is not
        // in textual scope at the invocation site, so a `use` importing the
        // name from another crate is what the bare invocation resolves to
        // (Codex P2, `review_comment` 3997890781). Checking the local set
        // first would grant the exemption on the strength of an unrelated
        // definition while the dependency's invisible expansion decides what
        // compiles. When the name resolves to neither, the builtin whitelist
        // applies — still after the import and shadowing checks, so an
        // imported name is never mistaken for the builtin. (`imports` never
        // holds `core::` / `std::` roots — [`external_macro_imports`] does
        // not record the builtin namespace — so no branch for them is needed
        // here.)
        if let Some((defining_crate, original)) = self.macro_imports.imports.get(&name) {
            return EXTERNAL_MACRO_EXEMPTIONS
                .contains(&(defining_crate.as_str(), original.as_str()));
        }
        // A `use` rooted at `crate::` / `self::` / `super::` binds the name
        // through a local path the checker cannot resolve through the module
        // tree — `use crate::shim::assert;` may name `evil::assert`
        // re-exported by `mod shim { pub use evil::assert; }` in another
        // file — so the invocation is neither provably the builtin nor
        // provably the vetted local definition an unrelated same-named
        // `macro_rules!` elsewhere would suggest. Like external imports,
        // local imports resolve before the crate-wide local-name set; an
        // unresolvable binding fails closed (Codex P2, `review_comment`
        // 3997946866).
        if self.macro_imports.local_imports.contains(&name) {
            return false;
        }
        if self.local_macros.contains(&name) {
            return true;
        }
        if self.macro_imports.has_external_glob || self.has_macro_use_extern {
            return false;
        }
        BUILTIN_MACROS.contains(&name.as_str())
    }

    /// Classify one attribute for the inclusion walk: an attribute the
    /// [`AttributeGate`] cannot account for fails the exemption closed.
    fn record_attribute(&mut self, attr: &'ast syn::Attribute) {
        let gate = AttributeGate {
            macro_imports: &self.macro_imports,
            shadowed_roots: &self.shadowed_roots,
        };
        if !gate.is_accounted_for(attr) {
            self.unresolvable = true;
        }
    }

    fn record(&mut self, attrs: &'ast [syn::Attribute], mac: &'ast syn::Macro) {
        let is_include = mac.path.segments.last().is_some_and(|segment| {
            segment.ident == "include" || self.include_aliases.contains(&segment.ident.to_string())
        });
        // A renamed import names the builtin `include!` through a
        // different last segment: `use core::include as paste;`
        // followed by `paste!("xtensa.rs")` pastes the gated file, but
        // the path's last segment is `paste` and the invocation tokens
        // hold only the string literal, so neither the path check nor
        // `tokens_could_include` below sees an inclusion. The rename is
        // a name binding, but a qualified path still resolves through
        // it: `self::paste!("xtensa.rs")` inside the importing module,
        // or `crate::paste!` through a re-export, pastes the gated file
        // exactly like the single-segment form — verified against rustc.
        // The checker cannot prove which binding a qualified path names,
        // so any path whose last segment is a collected alias counts as
        // an inclusion: the error direction is fail-closed, like the
        // literal `include` check, which already matches on the last
        // segment. An unrelated macro sharing the name only costs an
        // extra inclusion record, never a lost one (Codex P2,
        // `review_comment` 3997221943).
        if is_include {
            match mac.parse_body::<syn::LitStr>() {
                Ok(literal) => self
                    .found
                    .push((attrs, self.file_dir.join(literal.value()))),
                Err(_) => self.unresolvable = true,
            }
            return;
        }
        // An invocation the checker cannot account for — a macro exported by
        // another crate, whose body is invisible here — may expand to a
        // `mod` item or an `include!` that neither the definition scan
        // (which examines only `macro_rules!` bodies in this crate) nor
        // the token scans below can see: `use dependency::load_startup;`
        // followed by an argument-free `load_startup!();` carries no
        // `mod`/`include!` tokens at all, while the dependency's expansion
        // declares the gated file (Codex P2, `review_comment` 3997510667).
        // A builtin with a fixed expansion, a crate-local macro whose body
        // is scanned, and the pinned external-macro exemptions are
        // accounted for; anything else fails the exemption closed.
        if !self.macro_is_accounted_for(mac) {
            self.unresolvable = true;
            return;
        }
        // A macro invocation other than `include!` may still expand into an
        // inclusion: `macro_rules! pass { ($($t:tt)*) => { $($t)* } }` followed
        // by `pass!(include!("xtensa.rs"))` pastes the gated file wherever the
        // wrapper forwards its tokens, while the definition's body holds no
        // `include!` shape and the invocation's path is not `include` — so the
        // walk records neither. The invocation's own token stream is opaque to
        // the checker: the expansion may drop, reorder, or paste the tokens, so
        // an `include!`-shaped token inside it is an inclusion the checker
        // cannot enumerate, and fails closed like a computed `include!` path or
        // an `include!`-shaped `macro_rules!` body (Codex P2, `review_comment`
        // 3996149092) — and a `mod`-shaped token inside it is a module
        // declaration the checker cannot enumerate either, failing closed the
        // same way (Codex P2, `review_comment` 3997403828).
        if tokens_could_include(&mac.tokens, &self.include_aliases)
            || tokens_could_declare_module(&mac.tokens)
        {
            self.unresolvable = true;
        }
    }
}

impl<'ast> syn::visit::Visit<'ast> for Inclusions<'ast> {
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        // Procedural attribute macros are invisible to the item/statement/
        // expression visits below: an external attribute expanding to
        // `#[path = "xtensa.rs"] mod arm_startup;` would otherwise grant
        // the exemption over a declaration the checker never sees (Codex
        // P2, `review_comment` 3997629593). Every attribute is classified
        // by `record_attribute`: builtins ignored, `derive` and the pinned
        // `#[entry]` exemption accounted for, anything else fail-closed.
        self.record_attribute(node);
        syn::visit::visit_attribute(self, node);
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        // A `macro_rules!` definition keeps its body as opaque tokens: an
        // `include!` nested inside it is invisible to the invocation walk —
        // neither the definition (an `ItemMacro` whose path is
        // `macro_rules`, not `include`) nor any invocation (whose path is
        // the macro's name) is recorded — so the definition could paste the
        // gated file into an ARM image at an invocation site this checker
        // cannot enumerate. An inclusion the checker cannot enumerate fails
        // closed, like a computed `include!` path (Codex P2,
        // `review_comment` 3995964718) — and so does a `mod` item nested
        // inside it, which the module-declaration walk never sees either
        // (Codex P2, `review_comment` 3997403828) — and so does a nested
        // invocation of a macro the checker cannot account for, whose
        // expansion neither shape scan can see (Codex P2, `review_comment`
        // 3997946868). A definition is not an
        // invocation: once its body is scanned it is never classified by
        // `record` (which would otherwise fail closed on the
        // `macro_rules` path itself, an external-looking name).
        if node.mac.path.is_ident("macro_rules") {
            if tokens_could_include(&node.mac.tokens, &self.include_aliases)
                || tokens_could_declare_module(&node.mac.tokens)
                || !nested_invocations_are_accounted_for(self, &node.mac.tokens)
            {
                self.unresolvable = true;
            }
            return;
        }
        self.record(&node.attrs, &node.mac);
    }

    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        self.record(&node.attrs, &node.mac);
    }

    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        self.record(&node.attrs, &node.mac);
    }
}

/// `review_comment` 3997052796). A macro
/// invocation other than `include!` whose own token stream could generate an
/// inclusion fails closed the same way: the walk records only invocations whose
/// path is `include`, while a forwarding wrapper's expansion pastes its tokens
/// unseen — `pass!(include!("xtensa.rs"))` expands into the inclusion even
/// though neither the definition nor the invocation is recorded (Codex P2,
/// `review_comment` 3996149092). A `mod` item is a declaration the same way an
/// `include!` is: a `macro_rules!` body expanding to `#[path = "xtensa.rs"]
/// mod arm_startup;` compiles the gated file on ARM while the
/// module-declaration walk — which sees only parsed `mod` items — never sees
/// the expansion, and a forwarding wrapper's invocation tokens can carry the
/// same declaration (`pass!(#[path = "xtensa.rs"] mod arm_startup;)`, Codex P2,
/// `review_comment` 3997403828). Either shape in opaque tokens is a declaration
/// the checker cannot enumerate, so either fails the exemption closed.
///
/// A macro exported by another crate is the same hole one step further out:
/// the definition scan examines only `macro_rules!` bodies in this crate, so
/// `use dependency::load_startup;` + `load_startup!();` — expanding in the
/// dependency to `#[path = "xtensa.rs"] mod arm_startup;` — carries neither a
/// visible `mod` token nor an `include!` shape, and the separately gated `mod
/// xtensa;` still excuses the file (Codex P2, `review_comment` 3997510667).
/// The walk therefore accounts for every invocation it does not record as an
/// inclusion: compiler builtins (fixed expansions, unshadowed),
/// crate-local `macro_rules!` (bodies scanned), and the pinned external-macro
/// exemptions — anything else fails the exemption closed, because its
/// expansion is one the checker cannot enumerate. Procedural attribute macros
/// get the same treatment through the visitor's `visit_attribute`: an external attribute
/// expanding to `#[path = "xtensa.rs"] mod arm_startup;` is the same hole on a
/// surface the item/statement/expression visits never see, so builtin
/// attributes are ignored, `derive` admits only the std derives, `#[entry]` is
/// pinned to its verified expansion, and anything else fails closed (Codex P2,
/// `review_comment` 3997629593).
fn include_declarations<'ast>(
    file: &'ast syn::File,
    file_dir: &Path,
    local_macros: &HashSet<String>,
) -> Option<Vec<(&'ast [syn::Attribute], PathBuf)>> {
    let mut inclusions = Inclusions {
        file_dir: file_dir.to_path_buf(),
        found: Vec::new(),
        unresolvable: false,
        include_aliases: include_aliases(file),
        local_macros: local_macros.clone(),
        macro_imports: external_macro_imports(file),
        has_macro_use_extern: has_macro_use_extern_crate(file),
        shadowed_roots: shadowed_path_roots(file),
    };
    inclusions.visit_file(file);
    (!inclusions.unresolvable).then_some(inclusions.found)
}

/// Every local name a file gives the builtin `include!` macro through a
/// renamed import (`use core::include as paste;`, `use core::{include as
/// paste};`).
///
/// A `paste!(...)` invocation pastes code exactly like `include!` does, so the
/// inclusion walk in [`include_declarations`] treats it as an inclusion with
/// the invocation's own attributes. Without this, an ARM-reachable renamed
/// invocation of the gated file slips past the walk — the path's last segment
/// is `paste`, not `include`, and the invocation tokens hold only the string
/// literal the `tokens_could_include` scan ignores — and the hand-written
/// `unsafe` reaches an ARM image under the Xtensa exemption (Codex P2,
/// `review_comment` 3996723105). Plain `use core::include;` (or a glob) needs
/// no alias: the invocation path is still literally `include`. A qualified
/// path can still resolve through the alias — `self::paste!` inside the
/// importing module, or `crate::paste!` through a re-export, pastes exactly
/// like the single-segment form (verified against rustc) — so the inclusion
/// walk matches the alias on any path's last segment, like the literal
/// `include` check (Codex P2, `review_comment` 3997221943).
///
/// The alias may be imported by a `use` nested inside an inline module (Codex
/// P2, `review_comment` 3996992622): `mod wrapper { use core::include as
/// paste; paste!("xtensa.rs"); }`. The walk records invocations anywhere in
/// the file, so aliases are collected from every `use` item in the whole
/// syntax tree — not just the file's top-level items. This over-approximates
/// lexical scope (a `use` inside a function body also names the macro
/// file-wide), but the error direction is fail-closed: a name that is not
/// really the builtin `include!` only costs an extra inclusion record, never
/// a lost one.
///
/// Only renames rooted at `core` or `std` count: the leaf name alone is not
/// evidence of the builtin — `use evil::include as paste;` names another
/// crate's macro, whose expansion is invisible here and could paste the gated
/// file, so it must fall through to the external-macro classification and
/// fail closed instead of being recorded as a resolved inclusion (Codex P2,
/// `review_comment` 3997890782).
fn include_aliases(file: &syn::File) -> HashSet<String> {
    fn walk(tree: &syn::UseTree, root: Option<&String>, aliases: &mut HashSet<String>) {
        match tree {
            syn::UseTree::Path(path) => {
                let next = root.cloned().or_else(|| Some(path.ident.to_string()));
                walk(&path.tree, next.as_ref(), aliases);
            }
            syn::UseTree::Name(_) | syn::UseTree::Glob(_) => {}
            syn::UseTree::Rename(rename) => {
                if rename.ident == "include"
                    && root.is_some_and(|name| name == "core" || name == "std")
                {
                    aliases.insert(rename.rename.to_string());
                }
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    walk(item, root, aliases);
                }
            }
        }
    }

    struct Collector {
        aliases: HashSet<String>,
    }

    impl<'ast> syn::visit::Visit<'ast> for Collector {
        fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
            walk(&node.tree, None, &mut self.aliases);
            syn::visit::visit_item_use(self, node);
        }
    }

    let mut collector = Collector {
        aliases: HashSet::new(),
    };
    collector.visit_file(file);
    collector.aliases
}

/// Names in the file that can shadow an extern crate in a path's leading
/// segment.
///
/// A local item shadows the extern prelude in name resolution, so a pinned
/// `(crate, name)` exemption whose path is rooted at the exempted crate
/// (`cortex_m_rt::entry`, `cortex_m_semihosting::hprintln`) may actually
/// resolve to a local item with a hostile definition: `mod cortex_m_rt { pub
/// use evil::entry; }` makes `#[cortex_m_rt::entry]` apply evil's procedural
/// macro, not the verified one (Codex P2, `review_comment` 3997890784 — and
/// the same hole on the macro side, found in the same shape while verifying
/// it). Scope is over-approximated file-wide, like [`local_macro_names`]: any
/// item or import binding the name anywhere in the file fails the exemption
/// closed, since the checker cannot prove the root still names the extern
/// crate.
fn shadowed_path_roots(file: &syn::File) -> HashSet<String> {
    fn use_names(tree: &syn::UseTree, out: &mut HashSet<String>) {
        match tree {
            syn::UseTree::Path(path) => use_names(&path.tree, out),
            syn::UseTree::Name(name) => {
                out.insert(name.ident.to_string());
            }
            syn::UseTree::Rename(rename) => {
                out.insert(rename.rename.to_string());
            }
            syn::UseTree::Glob(_) => {}
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    use_names(item, out);
                }
            }
        }
    }

    struct Collector<'a> {
        names: &'a mut HashSet<String>,
    }

    impl<'ast> syn::visit::Visit<'ast> for Collector<'_> {
        fn visit_item(&mut self, node: &'ast syn::Item) {
            match node {
                syn::Item::Const(item) => {
                    self.names.insert(item.ident.to_string());
                }
                syn::Item::Enum(item) => {
                    self.names.insert(item.ident.to_string());
                }
                syn::Item::ExternCrate(item) => {
                    self.names.insert(item.ident.to_string());
                    if let Some((_, rename)) = &item.rename {
                        self.names.insert(rename.to_string());
                    }
                }
                syn::Item::Fn(item) => {
                    self.names.insert(item.sig.ident.to_string());
                }
                syn::Item::Macro(item) => {
                    if let Some(name) = &item.ident {
                        self.names.insert(name.to_string());
                    }
                }
                syn::Item::Mod(item) => {
                    self.names.insert(item.ident.to_string());
                }
                syn::Item::Static(item) => {
                    self.names.insert(item.ident.to_string());
                }
                syn::Item::Struct(item) => {
                    self.names.insert(item.ident.to_string());
                }
                syn::Item::Trait(item) => {
                    self.names.insert(item.ident.to_string());
                }
                syn::Item::TraitAlias(item) => {
                    self.names.insert(item.ident.to_string());
                }
                syn::Item::Type(item) => {
                    self.names.insert(item.ident.to_string());
                }
                syn::Item::Union(item) => {
                    self.names.insert(item.ident.to_string());
                }
                syn::Item::Use(item) => use_names(&item.tree, self.names),
                _ => {}
            }
            syn::visit::visit_item(self, node);
        }
    }

    let mut names = HashSet::new();
    Collector { names: &mut names }.visit_file(file);
    names
}

/// Whether the token tree holds an `include!`-shaped invocation: the identifier
/// `include` — or a local rename of it collected by [`include_aliases`] —
/// immediately followed by `!`, at any group nesting depth.
///
/// [`syn`] leaves a `macro_rules!` body as opaque tokens, so the inclusion walk
/// in [`include_declarations`] never sees an `include!` nested inside one; the
/// definition could still paste the gated file into an ARM image wherever it is
/// invoked. The same opacity covers a non-`include` macro invocation's own token
/// stream: the walk records only invocations whose path is `include`, while a
/// forwarding wrapper pastes its tokens unseen. The parsed-invocation walk
/// records renamed imports (`use core::include as paste;` makes `paste!(...)`
/// an inclusion), but it never sees invocations hidden in opaque tokens — so
/// without the aliases a `macro_rules!` body holding `paste!("xtensa.rs")`
/// would slip past the hard-coded `include` check while rustc expands the
/// alias and pastes the gated file (Codex P2, `review_comment` 3997052796).
/// The scan does not try to resolve which file the nested inclusion names —
/// token text cannot see through nested macro layers the way the
/// parsed-argument walk does — so any `include!` shape, literal or aliased,
/// fails the exemption closed, exactly like an `include!` with a computed path
/// (Codex P2, `review_comment` 3995964718; `review_comment` 3996149092 extends the
/// scan to invocation tokens). A `macro_rules!` whose body holds no such shape
/// is not an inclusion and leaves the exemption alone. The `!` must stand alone:
/// `include != x` is a comparison, not a macro invocation. A `mod` item hidden in
/// the same opaque tokens is a declaration the same way, and is scanned
/// separately by [`tokens_could_declare_module`] (Codex P2, `review_comment`
/// 3997403828).
fn tokens_could_include(
    tokens: &proc_macro2::TokenStream,
    include_aliases: &HashSet<String>,
) -> bool {
    let mut tokens = tokens.clone().into_iter().peekable();
    while let Some(token) = tokens.next() {
        match token {
            proc_macro2::TokenTree::Group(group) => {
                if tokens_could_include(&group.stream(), include_aliases) {
                    return true;
                }
            }
            proc_macro2::TokenTree::Ident(ident) => {
                let followed_by_bang = tokens.peek().is_some_and(|next| {
                    matches!(
                        next,
                        proc_macro2::TokenTree::Punct(punct)
                            if punct.as_char() == '!'
                                && punct.spacing() == proc_macro2::Spacing::Alone
                    )
                });
                let names_include = ident == "include"
                    || include_aliases.iter().any(|alias| ident == alias.as_str());
                if names_include && followed_by_bang {
                    return true;
                }
            }
            proc_macro2::TokenTree::Punct(_) | proc_macro2::TokenTree::Literal(_) => {}
        }
    }
    false
}

/// Whether the token tree holds a `mod` item: the keyword `mod` at any group
/// nesting depth.
///
/// [`syn`] leaves a `macro_rules!` body as opaque tokens, so a definition expanding
/// to `#[path = "xtensa.rs"] mod arm_startup;` never reaches the module-declaration
/// walk in [`module_declarations`] — neither the definition (an `ItemMacro` whose
/// path is `macro_rules`, not a `mod` item) nor any invocation (whose path is the
/// macro's name) is recorded — while rustc compiles the gated file into an ARM
/// image at every invocation site (Codex P2, `review_comment` 3997403828). The
/// same opacity covers a non-`include` macro invocation's own token stream: a
/// forwarding wrapper pastes its tokens unseen, so `pass!(#[path = "xtensa.rs"]
/// mod arm_startup;)` expands into the declaration even though the walk records
/// neither the definition nor the invocation as a module. A `mod` item the checker
/// cannot resolve is a declaration it cannot enumerate, so any `mod` keyword in
/// opaque tokens fails the exemption closed, like an `include!`-shaped one. The
/// scan does not distinguish a file declaration (`mod arm_startup;`) from an
/// inline module (`mod wrapper { ... }`): an inline module declares no file and
/// cannot reach the gated file, but telling them apart in opaque tokens is exactly
/// the resolution the checker cannot do — and the error direction stays
/// fail-closed, like the `include!`-shape scan's refusal of any file. `mod` is a
/// strict keyword, so the identifier can only ever be a module item; a raw
/// identifier (`r#mod`) spells differently and never declares one.
///
/// A nested macro invocation is neither an `include!` shape nor a `mod` item, so
/// the shape scans above cannot see the expansion hiding behind its name:
/// [`nested_invocation_paths`] finds those invocations in opaque tokens and the
/// definition scan classifies each one with the top-level accounting rule
/// (Codex P2, `review_comment` 3997946868).
fn tokens_could_declare_module(tokens: &proc_macro2::TokenStream) -> bool {
    for token in tokens.clone() {
        match token {
            proc_macro2::TokenTree::Group(group) => {
                if tokens_could_declare_module(&group.stream()) {
                    return true;
                }
            }
            proc_macro2::TokenTree::Ident(ident) => {
                if ident == "mod" {
                    return true;
                }
            }
            proc_macro2::TokenTree::Punct(_) | proc_macro2::TokenTree::Literal(_) => {}
        }
    }
    false
}

/// Every macro-invocation path nested inside opaque tokens.
///
/// A `!` standing alone and immediately followed by a parenthesized, bracketed,
/// or braced group is a macro invocation; the path is the run of path tokens
/// (idents, `::`, `$crate`) immediately before it. Groups are descended into,
/// so invocations nested inside an invocation's arguments are found too.
/// `None` marks an invocation shape whose path does not parse as a [`syn::Path`]:
/// `$crate` names the defining (local) crate, which `syn` cannot spell, so a
/// leading `$` is dropped before parsing, and a still-unparseable path fails
/// closed at classification time.
///
/// [`syn`] leaves a `macro_rules!` body as opaque tokens, so a nested
/// invocation's path never reaches [`Inclusions::record`] — and the shape scans
/// ([`tokens_could_include`], [`tokens_could_declare_module`]) only see
/// `include!` / `mod` tokens, not the expansion an external macro hides behind
/// its name. `macro_rules! wrapper { () => { evil::load_startup!(); } }`
/// therefore smuggled the dependency's invisible expansion past both scans:
/// the body holds neither shape, and the later `wrapper!()` is trusted as a
/// local macro — while the expansion could declare
/// `#[path = "xtensa.rs"] mod arm_startup;` on ARM (Codex P2, `review_comment`
/// 3997946868). The definition scan closes that hole by classifying every
/// nested path with the same rule the top-level walk applies.
fn nested_invocation_paths(tokens: &proc_macro2::TokenStream) -> Vec<Option<syn::Path>> {
    fn is_bang(tree: &proc_macro2::TokenTree) -> bool {
        matches!(
            tree,
            proc_macro2::TokenTree::Punct(punct)
                if punct.as_char() == '!' && punct.spacing() == proc_macro2::Spacing::Alone
        )
    }

    fn is_invocation_group(tree: &proc_macro2::TokenTree) -> bool {
        matches!(
            tree,
            proc_macro2::TokenTree::Group(group)
                if !matches!(group.delimiter(), proc_macro2::Delimiter::None)
        )
    }

    fn is_path_token(tree: &proc_macro2::TokenTree) -> bool {
        match tree {
            proc_macro2::TokenTree::Ident(_) => true,
            proc_macro2::TokenTree::Punct(punct) => matches!(punct.as_char(), ':' | '$'),
            proc_macro2::TokenTree::Group(_) | proc_macro2::TokenTree::Literal(_) => false,
        }
    }

    fn walk(tokens: &proc_macro2::TokenStream, paths: &mut Vec<Option<syn::Path>>) {
        let trees: Vec<proc_macro2::TokenTree> = tokens.clone().into_iter().collect();
        let mut run: Vec<proc_macro2::TokenTree> = Vec::new();
        let mut index = 0;
        while let Some(tree) = trees.get(index) {
            if is_bang(tree) && trees.get(index + 1).is_some_and(is_invocation_group) {
                let mut path_tokens = std::mem::take(&mut run);
                // `$crate::foo!()`: `$` is not a path token `syn` parses —
                // drop it; `$crate` names the defining (local) crate.
                if path_tokens.first().is_some_and(|first| {
                    matches!(
                        first,
                        proc_macro2::TokenTree::Punct(punct) if punct.as_char() == '$'
                    )
                }) {
                    path_tokens.remove(0);
                }
                let stream: proc_macro2::TokenStream = path_tokens.into_iter().collect();
                paths.push(syn::parse2::<syn::Path>(stream).ok());
                if let Some(proc_macro2::TokenTree::Group(group)) = trees.get(index + 1) {
                    walk(&group.stream(), paths);
                }
                index += 2;
                continue;
            }
            if is_path_token(tree) {
                run.push(tree.clone());
            } else {
                run.clear();
                if let proc_macro2::TokenTree::Group(group) = tree {
                    walk(&group.stream(), paths);
                }
            }
            index += 1;
        }
    }

    let mut paths = Vec::new();
    walk(tokens, &mut paths);
    paths
}

/// Whether every macro invocation nested inside opaque tokens (a
/// `macro_rules!` body) is one the checker can account for.
///
/// Each nested path is classified with
/// [`Inclusions::macro_path_is_accounted_for`], the same rule the top-level
/// walk applies to parsed invocations: a nested builtin is fine (fixed
/// expansion), a nested local macro is fine (its own definition is vetted by
/// this same scan — every definition in the crate is visited independently,
/// so no transitive walk is needed), and anything else fails the exemption
/// closed, because its expansion is one the checker cannot enumerate (Codex
/// P2, `review_comment` 3997946868).
fn nested_invocations_are_accounted_for(
    inclusions: &Inclusions<'_>,
    tokens: &proc_macro2::TokenStream,
) -> bool {
    nested_invocation_paths(tokens).iter().all(|path| {
        path.as_ref()
            .is_some_and(|path| inclusions.macro_path_is_accounted_for(path))
    })
}

/// Whether the `mod` declaration resolves to `target`.
///
/// A plain `mod <ident>;` resolves to `<module_dir>/<ident>.rs` or
/// `<module_dir>/<ident>/mod.rs`; `#[path = "..."]` overrides both and names the
/// file relative to the declaration's path directory (`path_dir`): the declaring
/// file's directory at the file's top level, the innermost enclosing inline module's
/// directory inside one (see [`module_declarations`]).
/// `#[cfg_attr(.., path = "...")]` selects its file whenever its predicate holds — and
/// this checker does not evaluate predicates — so every conditional path naming the file
/// counts as reaching it no matter what a plain `#[path]` beside it says, and they are
/// read first.
/// Each directory is a candidate list: an inline module's `#[cfg_attr(.., path)]`
/// forks the directories its nested declarations resolve against, so every candidate
/// is tried and the first match wins — an ARM-reachable conditional inclusion cannot
/// hide behind the plain directory.
/// An inline `mod <ident> { ... }` declares no file — any `unsafe` it holds sits in the
/// declaring file itself — so it never resolves to a separate path. A same-named file
/// elsewhere (e.g., `helpers/xtensa.rs`) is a different module the declaration never
/// named.
fn module_resolves_to(
    path_dirs: &[PathBuf],
    module_dirs: &[PathBuf],
    module: &syn::ItemMod,
    target: &Path,
) -> bool {
    if module.content.is_some() {
        return false;
    }
    let names_target = |dir: &Path, value: &str| normalize_path(&dir.join(value)) == target;
    // Conditional first: a `#[cfg_attr]` predicate this checker does not evaluate may
    // still be active, and rustc applies both path spellings in order — so a
    // conditional path naming the file reaches it regardless of the plain `#[path]`.
    if module
        .attrs
        .iter()
        .flat_map(cfg_attr_path_values)
        .any(|value| path_dirs.iter().any(|dir| names_target(dir, &value)))
    {
        return true;
    }
    if let Some(renamed) = module.attrs.iter().find_map(path_value) {
        return path_dirs.iter().any(|dir| names_target(dir, &renamed));
    }
    let ident = module.ident.to_string();
    module_dirs.iter().any(|dir| {
        normalize_path(&dir.join(format!("{ident}.rs"))) == target
            || normalize_path(&dir.join(format!("{ident}/mod.rs"))) == target
    })
}

/// Lexically normalizes a path for module-resolution comparison: `.` segments are
/// dropped and `..` segments pop the segment before them. `#[path]` spellings like
/// `"./xtensa.rs"` or `"sub/../xtensa.rs"` name the same file as `"xtensa.rs"`, and
/// rustc resolves them identically — the exemption check must too, or an ungated
/// declaration with a non-canonical spelling would slip past the gate.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(component),
        }
    }
    out
}

/// The value of a `#[path = "..."]` attribute, if this attribute is one.
///
/// Read off the parsed `Meta::NameValue` rather than re-scanning the tokens: the value
/// must be a string literal, and anything else is not a path this checker understands.
fn path_value(attr: &syn::Attribute) -> Option<String> {
    if !attr.path().is_ident("path") {
        return None;
    }
    meta_str_value(&attr.meta, "path")
}

/// Every `path = "..."` a `#[cfg_attr(.., path = "...")]` applies, if this attribute is one.
///
/// A `#[cfg_attr]` is a predicate followed by the attributes it applies when the
/// predicate holds: the first comma-separated item is the predicate — which this checker
/// does not evaluate — and the rest are ordinary attributes, so each `path = "..."`
/// among them is read with the same [`meta_str_value`] as a plain `#[path]`. Those
/// applied attributes may themselves be `cfg_attr`s, which rustc expands recursively (Codex P2,
/// `review_comment` 3995679544 — `#[cfg_attr(target_arch = "arm", cfg_attr(any(), path
/// = "xtensa.rs"))]` applies `path = "xtensa.rs"` on ARM), so the search recurses into
/// nested `cfg_attr` lists, skipping each level's predicate. One outer `cfg_attr` can
/// hold several nested alternatives naming different files (Codex P2, `review_comment`
/// 3995728343 — an inactive first branch naming `other.rs` followed by an active
/// `target_arch = "arm"` branch naming `xtensa.rs` still compiles `xtensa.rs` on ARM),
/// and because the checker does not evaluate predicates, every alternative is
/// collected rather than only the first: the extra reach can only add declarations
/// that must carry the gate, never remove them, and the exemption stays fail-closed.
/// Anything but a string literal is not a path this checker understands.
fn cfg_attr_path_values(attr: &syn::Attribute) -> Vec<String> {
    if !attr.path().is_ident("cfg_attr") {
        return Vec::new();
    }
    let syn::Meta::List(list) = &attr.meta else {
        return Vec::new();
    };
    let Ok(metas) = list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return Vec::new();
    };
    let mut items = metas.iter();
    items.next(); // The cfg predicate: not a path override.
    let mut paths = Vec::new();
    cfg_attr_applied_paths(items, &mut paths);
    paths
}

/// Every `path = "..."` among the attributes a `#[cfg_attr]` applies after its
/// predicate, recursing into nested `cfg_attr` lists.
///
/// rustc expands a nested `cfg_attr` in place once the outer predicate holds, so a
/// `path` at any nesting depth is a path the module could resolve to — and with
/// several nested alternatives, each one is. A nested list that fails to parse is
/// skipped rather than aborting the search: an unreadable branch must not hide the
/// readable alternatives. The extra reach can only add declarations that must carry
/// the gate, never remove them, and the exemption stays fail-closed.
fn cfg_attr_applied_paths<'meta, I>(metas: I, paths: &mut Vec<String>)
where
    I: Iterator<Item = &'meta syn::Meta>,
{
    for meta in metas {
        match meta {
            syn::Meta::List(nested) if nested.path.is_ident("cfg_attr") => {
                let Ok(inner) = nested.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                ) else {
                    continue;
                };
                let mut inner = inner.iter();
                inner.next(); // The inner cfg predicate: not a path override.
                cfg_attr_applied_paths(inner, paths);
            }
            other => {
                if let Some(path) = meta_str_value(other, "path") {
                    paths.push(path);
                }
            }
        }
    }
}

/// The string value of a `name = "..."` name-value meta, if that is what `meta` is.
fn meta_str_value(meta: &syn::Meta, name: &str) -> Option<String> {
    let syn::Meta::NameValue(pair) = meta else {
        return None;
    };
    if !pair.path.is_ident(name) {
        return None;
    }
    let syn::Expr::Lit(literal) = &pair.value else {
        return None;
    };
    let syn::Lit::Str(text) = &literal.lit else {
        return None;
    };
    Some(text.value())
}

/// Whether one of the attributes is exactly `#[cfg(target_arch = "xtensa")]`.
///
/// Structural, like [`crate::parse`] callers elsewhere: path `cfg` with the single
/// `target_arch = "xtensa"` predicate. `#[cfg(all(target_arch = "xtensa", ...))]` is a
/// different predicate and does not count — the gate the exception is scoped by is the
/// plain one.
fn has_xtensa_cfg(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        let mut gated = false;
        let parsed = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("target_arch") {
                let value: syn::LitStr = meta.value()?.parse()?;
                gated = value.value() == "xtensa";
            }
            Ok(())
        });
        parsed.is_ok() && gated
    })
}

/// Whether `byte` can appear inside a Rust identifier.
const fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
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
        // The Xtensa target is built by the Espressif fork toolchain, not the workspace's:
        // `rust-toolchain.toml` pins targets for the workspace channel, and no channel a
        // toolchain file can name ships `xtensa-esp32s3-none-elf`. `MachineKind::Xtensa` is
        // the declaration of which toolchain builds it — requiring the pin as well would
        // demand a toolchain file that cannot exist.
        if machine.kind == MachineKind::Xtensa {
            continue;
        }
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
            "{PREFIX} cases passed=21 exempt=2\n{PREFIX} rig iterations=12 cuts=12 resumes=12 unextendable=0 redeliveries=8 verdicts=24 dispatched=42\n{PREFIX} ok\n"
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

    fn ran(name: &'static str, census: Census) -> Row {
        let machine = MACHINES
            .iter()
            .find(|machine| machine.name == name)
            .copied()
            .unwrap_or(MACHINES[0]);
        Row {
            machine,
            outcome: Outcome::Ran(census),
            output: String::new(),
        }
    }

    fn clean_report() -> Report {
        Report {
            rows: MACHINES
                .iter()
                .map(|machine| ran(machine.name, clean_census()))
                .collect(),
            machines: MACHINES.to_vec(),
        }
    }

    fn toolchain_pinning_every_machine() -> String {
        // The ARM machines' targets, by name, in a `[toolchain] targets` list. The
        // ESP32-S3's is deliberately absent: no channel a toolchain file can name ships
        // `xtensa-esp32s3-none-elf`, and `check_machines_are_reachable` does not ask for
        // it.
        let targets: Vec<&str> = MACHINES
            .iter()
            .filter(|machine| machine.kind == MachineKind::Arm)
            .map(|machine| machine.target)
            .collect();
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
        assert_eq!(parse(&clean_output()), Some(clean_census()));
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
        assert_eq!(parse(&noisy), Some(clean_census()));
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
    fn cores_that_disagree_about_the_run_fail_the_gate() {
        // The check a single-machine gate cannot make, now across three cores. The plan is
        // deterministic, so this is the rig behaving differently on two of them.
        let mut report = clean_report();
        let esp32s3 = report
            .rows
            .iter_mut()
            .find(|row| row.machine.kind == MachineKind::Xtensa)
            .unwrap();
        esp32s3.outcome = Outcome::Ran(Census {
            dispatched: 41,
            ..clean_census()
        });
        let shortfall = report.shortfall().unwrap_or_default();
        assert!(
            shortfall.contains("disagree"),
            "the cores disagreeing must be reported as such: {shortfall}"
        );
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
    }

    #[test]
    fn the_machines_are_three_different_targets() {
        // A machine that repeated another's encoding would double the run time and buy
        // nothing: the equality check would compare a core with itself.
        assert_eq!(MACHINES.len(), 3);
        for (index, machine) in MACHINES.iter().enumerate() {
            for other in MACHINES.iter().skip(index.saturating_add(1)) {
                assert_ne!(machine.target, other.target);
                assert_ne!(machine.architecture, other.architecture);
            }
        }
    }

    #[test]
    fn the_third_machine_is_the_esp32s3() {
        let machine = MACHINES
            .iter()
            .find(|machine| machine.name == "esp32s3")
            .unwrap();
        assert_eq!(machine.kind, MachineKind::Xtensa);
        assert_eq!(machine.target, "xtensa-esp32s3-none-elf");
        assert_eq!(machine.qemu, "esp32s3");
    }

    #[test]
    fn the_esp32s3_qemu_defaults_to_the_workspace_local_fork() {
        // The documented provisioning puts the Espressif QEMU fork at
        // `esp32s3/qemu/bin/qemu-system-xtensa` under the workspace root, alongside
        // the other workspace-local Xtensa assets. Joining `../esp32s3/...`
        // resolved to the workspace's parent instead, so `which` failed before any
        // image was built unless `WAYMAKER_XTENSA_QEMU` was set.
        let machine = MACHINES
            .iter()
            .find(|machine| machine.kind == MachineKind::Xtensa)
            .copied()
            .unwrap();
        assert_eq!(
            emulator_path_for(machine, Path::new("/workspace/waymaker"), None),
            PathBuf::from("/workspace/waymaker/esp32s3/qemu/bin/qemu-system-xtensa"),
        );
    }

    #[test]
    fn the_qemu_override_names_another_binary() {
        let machine = MACHINES
            .iter()
            .find(|machine| machine.kind == MachineKind::Xtensa)
            .copied()
            .unwrap();
        assert_eq!(
            emulator_path_for(
                machine,
                Path::new("/workspace/waymaker"),
                Some(std::ffi::OsString::from("/opt/qemu/bin/qemu-system-xtensa")),
            ),
            PathBuf::from("/opt/qemu/bin/qemu-system-xtensa"),
        );
    }

    #[test]
    fn the_arm_emulator_comes_off_path() {
        let machine = MACHINES
            .iter()
            .find(|machine| machine.kind == MachineKind::Arm)
            .copied()
            .unwrap();
        assert_eq!(
            emulator_path_for(machine, Path::new("/workspace/waymaker"), None),
            PathBuf::from(EMULATOR),
        );
    }

    #[test]
    fn prepend_dir_preserves_the_prior_value() {
        // `prepend_to_path` read `PATH` for every variable, so `LD_LIBRARY_PATH` and
        // `PYTHONPATH` inherited whatever happened to be on `PATH`. The pure core takes
        // the prior value as a parameter, so this touches no process state.
        assert_eq!(
            prepend_dir(Path::new("/first"), None),
            std::ffi::OsString::from("/first"),
            "nothing to preserve"
        );
        assert_eq!(
            prepend_dir(Path::new("/first"), Some(std::ffi::OsStr::new(""))),
            std::ffi::OsString::from("/first"),
            "an empty prior value adds no trailing separator"
        );
        assert_eq!(
            prepend_dir(
                Path::new("/first"),
                Some(std::ffi::OsStr::new("/already/there"))
            ),
            std::ffi::OsString::from("/first:/already/there"),
            "the variable's own prior value comes second"
        );
    }

    #[test]
    fn the_gcc_dir_is_the_bin_dir_not_the_version_dir() {
        // Regression: the driver lives at `<version>/xtensa-esp-elf/bin`, and returning
        // the version directory put a driver-less directory on `PATH` — a link that
        // failed on `xtensa-esp32s3-elf-gcc not found` while the toolchain sat intact.
        let home =
            std::env::temp_dir().join(format!("waymaker-esp-gcc-fixture-{}", std::process::id()));
        let bin = home
            .join(".rustup/toolchains/esp/xtensa-esp-elf/esp-15.2.0_20250920/xtensa-esp-elf/bin");
        std::fs::create_dir_all(&bin).expect("fixture");
        let found = esp_gcc_dir_in(&home).expect("a GCC layout is present");
        assert_eq!(found, bin, "the driver directory, not its grandparent");
        assert!(
            esp_gcc_dir_in(&std::env::temp_dir().join("waymaker-esp-gcc-fixture-absent")).is_none(),
            "no layout, no directory"
        );
        std::fs::remove_dir_all(&home).ok();
    }

    /// Build a fake `$HOME` whose `xtensa-esp-elf` holds one GCC directory per name,
    /// and return both the home and the expected driver `bin` of `winner`.
    fn gcc_fixture(
        tag: &str,
        names: &[&str],
        winner: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let home = std::env::temp_dir().join(format!(
            "waymaker-esp-gcc-multi-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        for name in names {
            std::fs::create_dir_all(
                home.join(".rustup/toolchains/esp/xtensa-esp-elf")
                    .join(name)
                    .join("xtensa-esp-elf/bin"),
            )
            .expect("fixture");
        }
        let bin = home
            .join(".rustup/toolchains/esp/xtensa-esp-elf")
            .join(winner)
            .join("xtensa-esp-elf/bin");
        (home, bin)
    }

    #[test]
    fn the_newest_gcc_wins_numerically_not_lexicographically() {
        // Regression (Codex P2): the directories sorted *lexicographically*, so with
        // `esp-9.x` and `esp-15.x` present, `esp-15...` sorted before `esp-9...` and
        // "last wins" put the *older* driver first on PATH.
        let (home, bin) = gcc_fixture(
            "numeric",
            &["esp-9.0.0_20210101", "esp-15.2.0_20250920"],
            "esp-15.2.0_20250920",
        );
        let found = esp_gcc_dir_in(&home).expect("a GCC layout is present");
        assert_eq!(
            found, bin,
            "the numerically newest GCC, not the lexicographic last"
        );
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn the_newer_build_date_wins_at_equal_versions() {
        // Two drops of the same GCC: the later build date is the newer install.
        let (home, bin) = gcc_fixture(
            "date",
            &["esp-15.2.0_20250920", "esp-15.2.0_20251001"],
            "esp-15.2.0_20251001",
        );
        let found = esp_gcc_dir_in(&home).expect("a GCC layout is present");
        assert_eq!(found, bin, "the later-dated install of the same version");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn unparseable_gcc_names_stay_deterministic_and_lose_to_parsed_ones() {
        // A directory that does not parse as `esp-<version>_<date>` cannot claim
        // "newest"; it ranks below every parsed one, and among themselves the order
        // is still the deterministic name order.
        let (home, bin) = gcc_fixture(
            "unparseable",
            &["custom-toolchain", "esp-14.2.0_20241119"],
            "esp-14.2.0_20241119",
        );
        let found = esp_gcc_dir_in(&home).expect("a GCC layout is present");
        assert_eq!(found, bin, "a parsed version beats an unparseable name");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn each_machine_resolves_its_linker_script() {
        // Per-target build support: the ARM images link against `link.x` from
        // `cortex-m-rt` (which `INCLUDE`s the crate's `memory.x` for the FLASH/RAM
        // regions); the Xtensa image links `memory-xtensa.x` — `.literal` before
        // `.text`, and no toolchain `crt0` supplying its own `_start`.
        for machine in MACHINES {
            let args: Vec<&str> = link_args_for(*machine).split_whitespace().collect();
            match machine.kind {
                MachineKind::Arm => {
                    assert!(args.contains(&"link-arg=-Tlink.x"), "{args:?}");
                    assert!(
                        !args.iter().any(|arg| arg.contains("-nostartfiles")),
                        "{args:?}"
                    );
                }
                MachineKind::Xtensa => {
                    // Not `-Tmemory.x`: that name is the ARM build's, `INCLUDE`d by
                    // `cortex-m-rt` for FLASH/RAM, and the two scripts cannot share it
                    // in the same `-L` directory.
                    assert!(args.contains(&"link-arg=-Tmemory-xtensa.x"), "{args:?}");
                    assert!(
                        !args.iter().any(|arg| arg.contains("-Tmemory.x")),
                        "{args:?}"
                    );
                    assert!(
                        args.iter().any(|arg| arg.contains("-nostartfiles")),
                        "{args:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn each_machine_resolves_its_boot_media() {
        // ARM boots the ELF directly on `-kernel`; the ESP32-S3 ROM boots a flash image
        // from offset 0x0, so the harness must build one.
        for machine in MACHINES {
            let media = boot_media_for(*machine);
            match machine.kind {
                MachineKind::Arm => assert_eq!(media, BootMedia::Elf),
                MachineKind::Xtensa => assert_eq!(media, BootMedia::FlashImage),
            }
        }
    }

    /// One `ORIGIN = <addr>, LENGTH = <size>` region of `memory-xtensa.x`.
    fn xtensa_memory_region(script: &str, name: &str) -> (u32, u32) {
        let line = script
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("memory-xtensa.x has no {name} region"));
        // The value after `ORIGIN =` / `LENGTH =`: `= 0x40370000` trims to the hex.
        let value_after = |key: &str| {
            line.split(key)
                .nth(1)
                .and_then(|rest| rest.split(',').next())
                .map_or_else(
                    || panic!("{name} region has no {key}: {line}"),
                    |value| value.trim().trim_start_matches('=').trim(),
                )
        };
        let origin = value_after("ORIGIN")
            .strip_prefix("0x")
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .unwrap_or_else(|| panic!("{name} region has no hex ORIGIN: {line}"));
        let length = value_after("LENGTH");
        let length = length
            .strip_suffix('K')
            .map(|kibi| kibi.parse::<u32>().expect("LENGTH in KiB") * 1024)
            .or_else(|| {
                length
                    .strip_prefix("0x")
                    .map(|hex| u32::from_str_radix(hex, 16).expect("LENGTH in hex"))
            })
            .unwrap_or_else(|| panic!("LENGTH is neither KiB nor hex: {line}"));
        (origin, origin + length)
    }

    #[test]
    fn the_xtensa_linker_script_stays_inside_mapped_sram() {
        // ESP32-S3 TRM Table 13-3: instruction-bus SRAM is 0x4037_0000..0x403E_0000
        // and data-bus SRAM is 0x3FC8_8000..0x3FD0_0000. The first 32 KiB below
        // 0x3FC8_8000 has no data-bus address, and anything at or above 0x3FD0_0000
        // is not SRAM at all: a region the linker may fill past the window faults
        // on the first access, on silicon if not under QEMU.
        //
        // The two bus views alias the SAME physical HP SRAM, so the script grants
        // the linker a non-overlapping split of it (Codex P2, review_comment
        // 3994548771): IRAM in the instruction-bus view, DRAM in the data-bus
        // view, adjacent and disjoint in physical SRAM. Granting both views their
        // full mapped ranges would let the linker place .text at the same physical
        // address as .data/.bss, and loading data or zero_bss() would then
        // overwrite instructions. Data stays in the data-bus view on purpose: the
        // S3's instruction SRAM is fetch-only for ordinary loads/stores, so
        // .data/.bss linked into IRAM would fault on silicon.
        let script_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/waymaker-emu/memory-xtensa.x");
        let script = std::fs::read_to_string(&script_path).expect("memory-xtensa.x is committed");
        let (iram_start, iram_end) = xtensa_memory_region(&script, "IRAM");
        let (dram_start, dram_end) = xtensa_memory_region(&script, "DRAM");
        assert!(
            (0x4037_0000..=0x403E_0000).contains(&iram_start)
                && (0x4037_0000..=0x403E_0000).contains(&iram_end),
            "IRAM {iram_start:#X}..{iram_end:#X} exceeds the mapped instruction-bus SRAM"
        );
        assert!(
            (0x3FC8_8000..=0x3FD0_0000).contains(&dram_start)
                && (0x3FC8_8000..=0x3FD0_0000).contains(&dram_end),
            "DRAM {dram_start:#X}..{dram_end:#X} exceeds the mapped data-bus SRAM"
        );
        // The two views need a shared frame to compare disjointness. TRM Table
        // 15.3-3 gives the aliasing: IBUS 0x4037_8000.. maps onto DBUS
        // 0x3FC8_8000.. (offset 0x6F_0000); IBUS 0x4037_0000..0x4037_7FFF
        // (Blocks 0-1) have no data-bus address at all, so they cannot alias
        // DRAM. The closure below applies that same offset from the window
        // bases, which is exact for every IBUS address at or above 0x4037_8000.
        let physical_of_ibus = |ibus_addr: u32| ibus_addr - 0x4037_0000 + 0x3FC8_0000;
        let (iram_phys_start, iram_phys_end) =
            (physical_of_ibus(iram_start), physical_of_ibus(iram_end));
        assert!(
            iram_phys_end <= dram_start || dram_end <= iram_phys_start,
            "IRAM {iram_start:#X}..{iram_end:#X} and DRAM {dram_start:#X}..{dram_end:#X} \
             alias the same physical SRAM"
        );
        // The stack `_start` installs is a bare address, not a linker region: it must
        // be the top of the mapped data-bus SRAM, with the 32 bytes of slack the
        // startup code documents — not an address above mapped SRAM — and physically
        // above the linked DRAM region, so the downward-growing stack can never
        // reach the linked image.
        let startup_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/waymaker-emu/src/xtensa.rs");
        let startup = std::fs::read_to_string(&startup_path).expect("xtensa.rs is committed");
        let stack_top = startup
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("const STACK_TOP"))
            .and_then(|line| line.split('=').nth(1))
            .and_then(|value| value.trim().trim_end_matches(';').strip_prefix("0x"))
            .and_then(|hex| u32::from_str_radix(&hex.replace('_', ""), 16).ok())
            .expect("xtensa.rs names a hex STACK_TOP");
        assert_eq!(
            stack_top,
            0x3FD0_0000 - 32,
            "STACK_TOP {stack_top:#X} is not 32 B below the top of the mapped data-bus SRAM"
        );
        assert!(
            stack_top >= dram_end,
            "STACK_TOP {stack_top:#X} overlaps the linked DRAM region ending at {dram_end:#X}"
        );
    }

    /// `(name, memory region, collects .rodata, body)` of each output section in the
    /// script's `SECTIONS` block.
    ///
    /// Each output section ends at `} > REGION`; the section name is the first token
    /// of the last declaration line before its body's `{`. `/DISCARD/` has no region
    /// and is skipped.
    fn xtensa_output_sections(script: &str) -> Vec<(String, String, bool, String)> {
        let sections = script
            .split("SECTIONS")
            .nth(1)
            .unwrap_or_else(|| panic!("memory-xtensa.x has no SECTIONS block"));
        let mut out = Vec::new();
        let mut rest = sections;
        while let Some(end) = rest.find("} >") {
            let (head, tail) = rest.split_at(end);
            let region = tail["} >".len()..]
                .split_whitespace()
                .next()
                .unwrap_or_else(|| panic!("output section has no region: {head}"))
                .to_owned();
            let body_start = head
                .rfind('{')
                .unwrap_or_else(|| panic!("output section has no body: {head}"));
            let (declaration, body) = head.split_at(body_start);
            let name = declaration
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .and_then(|line| line.split_whitespace().next())
                .unwrap_or_else(|| panic!("output section has no name: {declaration}"))
                .to_owned();
            out.push((name, region, body.contains("*(.rodata"), body.to_owned()));
            rest = &tail["} >".len()..];
        }
        out
    }

    #[test]
    fn the_xtensa_linker_script_keeps_rodata_in_the_data_bus_view() {
        // Codex P2 (review_comment 3995126210): `.rodata` held the guest's
        // byte-addressed constants — the format strings the census prints — at
        // instruction-bus addresses. But the S3's instruction SRAM is fetch-only
        // for ordinary loads/stores (only `l32r` literal loads reach it), and
        // blocks 0-1 have no data-bus address at all, so the first byte
        // `write_str` read with an ordinary load would fault on silicon before
        // the census was complete. `.rodata` therefore lives in the data-bus
        // view; only `.literal` pools and `.text` stay in IRAM.
        let script_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/waymaker-emu/memory-xtensa.x");
        let script = std::fs::read_to_string(&script_path).expect("memory-xtensa.x is committed");
        let sections = xtensa_output_sections(&script);
        let rodata_sections: Vec<&(String, String, bool, String)> = sections
            .iter()
            .filter(|(_, _, collects, _)| *collects)
            .collect();
        assert_eq!(
            rodata_sections.len(),
            1,
            ".rodata must be collected by exactly one output section: {sections:?}"
        );
        let (name, region, _, _) = rodata_sections[0];
        assert_eq!(
            region, "DRAM",
            ".rodata is collected by the {name} section mapped to {region}, not the data-bus view"
        );
        assert!(
            !sections
                .iter()
                .any(|(_, section_region, collects, _)| section_region == "IRAM" && *collects),
            "an IRAM-mapped section still collects .rodata: {sections:?}"
        );
    }

    #[test]
    fn the_xtensa_linker_script_word_aligns_the_bss_end() {
        // Codex P2 (review_comment 3995859254): only the `.bss` output section's
        // start carries `ALIGN(4)` — a final input with byte or halfword
        // alignment would leave `_ebss` unaligned, and `zero_bss()` clears whole
        // `u32` words, so its last volatile write would clear up to three bytes
        // past `.bss`. Rounding the end up to a word boundary keeps every word
        // write inside the section; the rounded-up bytes sit in unlinked DRAM,
        // which nothing else uses.
        let script_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/waymaker-emu/memory-xtensa.x");
        let script = std::fs::read_to_string(&script_path).expect("memory-xtensa.x is committed");
        let sections = xtensa_output_sections(&script);
        let bss_body = sections
            .iter()
            .find(|(name, _, _, _)| name == ".bss")
            .map_or_else(
                || panic!("memory-xtensa.x has no .bss output section: {sections:?}"),
                |(_, _, _, body)| body.as_str(),
            );
        let align = bss_body
            .find(". = ALIGN(4);")
            .unwrap_or_else(|| panic!(".bss never rounds its end up to a word: {bss_body}"));
        let ebss = bss_body
            .find("_ebss =")
            .unwrap_or_else(|| panic!(".bss never defines _ebss: {bss_body}"));
        assert!(
            align < ebss,
            ".bss rounds its end up to a word only after defining _ebss: {bss_body}"
        );
    }

    #[test]
    fn the_xtensa_start_installs_the_stack_in_a_naked_entry() {
        // Codex P2 (review_comment 3996354964): switching the a1 stack pointer
        // in inline `asm!` with `options(nostack)` contradicts the
        // inline-assembly contract — `nostack` promises the compiler the stack
        // pointer is not modified, and "no locals, nothing live across the
        // block" is an argument about today's codegen, not about the contract.
        // The stack is therefore installed in a naked entry point. The windowed
        // ABI still needs what the earlier naked attempt omitted: the caller
        // must have executed `entry` before the windowed call into Rust, so the
        // template hand-writes it (the entry-less form built and disassembled
        // plausibly but faulted in QEMU). This test locks the design in: a
        // future "simplification" back to inline asm reintroduces the contract
        // violation.
        let startup_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/waymaker-emu/src/xtensa.rs");
        let startup = std::fs::read_to_string(&startup_path).expect("xtensa.rs is committed");
        let def = startup.find("fn _start").expect("xtensa.rs defines _start");
        let attr_start = startup[..def].rfind("\nfn ").map_or(0, |index| index + 1);
        assert!(
            startup[attr_start..def].contains("#[unsafe(naked)]"),
            "_start must be a naked entry point: switching a1 in inline asm contradicts the nostack contract"
        );
        assert!(
            startup[attr_start..def].contains("entry"),
            "_start must document why the entry is hand-written, or someone will drop it and reintroduce the QEMU fault"
        );
        let body_end = startup[def..]
            .find("\nfn ")
            .map_or(startup.len(), |index| def + index);
        let body = &startup[def..body_end];
        // The hand-written entry the windowed call requires: without it the
        // guest faulted in QEMU even though the disassembly looked right.
        assert!(
            body.contains("\"entry a1, 32\""),
            "_start must hand-write the entry prologue the windowed call requires"
        );
        // The stack address is built from the STACK_TOP constant with plain
        // immediates (`naked_asm!` takes no `in(reg)` operands): `addi`'s 12-bit
        // signed immediate cannot add the low half, so the template builds
        // STACK_TOP + 32 and subtracts 32. No re-typed address pieces.
        assert!(
            body.contains("STACK_TOP"),
            "_start must derive the stack address from the STACK_TOP constant"
        );
        assert!(
            !body.contains("0x3FCF") && !body.contains("0x3FD0"),
            "_start must not re-type the stack address as a literal"
        );
        assert!(
            body.contains("sub a1, a1, a2"),
            "_start must build STACK_TOP + 32 and subtract 32 (addi cannot add 0xFFE0)"
        );
        assert!(
            !body.contains("in(reg)"),
            "_start is naked_asm!: in(reg) operands are rejected there, hence the hand-rolled immediates"
        );
        // The windowed call into Rust: direct `call8` against the symbol (its
        // ±128 KiB range covers the image; the link fails loudly if it ever did
        // not), and a1 needs no handoff — the window rotation keeps the caller's
        // a1 as the callee's a1.
        assert!(
            body.contains("call8 {main}"),
            "_start must enter Rust with the windowed call8 the ABI requires"
        );
        assert!(
            body.contains("main = sym firmware_main"),
            "_start must call the firmware_main symbol, not a re-typed address"
        );
        assert!(
            !body.contains("l32r"),
            "_start must not hand-write l32r: the Xtensa linker mangles its relocations"
        );
        assert!(
            !body.contains("nostack"),
            "_start must not switch stacks in inline asm: nostack promises the compiler the stack pointer is untouched"
        );
    }

    #[test]
    fn the_xtensa_breach_code_rides_the_failed_line() {
        // Codex P2 (review_comment 3996354967): on `Trouble::Breach` the guest
        // printed the terminated `failed` line and then a separate `breach
        // code=` line, but the harness kills QEMU as soon as a terminated
        // `failed` line reaches the serial log — a poll landing between the two
        // writes loses the code identifying the violated outcome. The code
        // therefore rides on the failed line itself, inside the one terminated
        // line the detector matches.
        let startup_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/waymaker-emu/src/xtensa.rs");
        let startup = std::fs::read_to_string(&startup_path).expect("xtensa.rs is committed");
        assert!(
            startup.contains("\"{} failed {} (breach code={})\""),
            "the breach code must be printed on the failed line itself, so the harness kill cannot separate them"
        );
        assert!(
            !startup.contains("\"{} breach code={}\""),
            "there must be no standalone breach line left to race the harness kill"
        );
    }

    #[test]
    fn only_the_arm_guests_exit_on_their_own() {
        // The Xtensa guest has no exit mechanism: the harness must watch the serial log
        // for the census and terminate QEMU itself, or every run would read as a hang.
        for machine in MACHINES {
            assert_eq!(
                guest_exits_for(*machine),
                machine.kind == MachineKind::Arm,
                "{}",
                machine.name
            );
        }
    }

    #[test]
    fn the_arm_qemu_command_boots_an_elf_with_semihosting() {
        for machine in MACHINES
            .iter()
            .filter(|machine| machine.kind == MachineKind::Arm)
        {
            let args = qemu_args_for(*machine, Path::new("waymaker-emu"), Path::new("uart0.log"));
            let joined = args.join(" ");
            assert!(joined.contains("-kernel"), "{joined}");
            assert!(joined.contains("semihosting"), "{joined}");
            assert!(!joined.contains("if=mtd"), "{joined}");
        }
    }

    #[test]
    fn the_xtensa_qemu_command_boots_a_flash_image() {
        let machine = MACHINES
            .iter()
            .find(|machine| machine.kind == MachineKind::Xtensa)
            .unwrap();
        let args = qemu_args_for(
            *machine,
            Path::new("flash_image.bin"),
            Path::new("uart0.log"),
        );
        let joined = args.join(" ");
        assert!(joined.contains("-machine esp32s3"), "{joined}");
        assert!(joined.contains("if=mtd"), "{joined}");
        assert!(joined.contains("-serial"), "{joined}");
        assert!(!joined.contains("-kernel"), "{joined}");
        assert!(!joined.contains("semihosting"), "{joined}");
    }

    #[test]
    fn the_xtensa_qemu_launch_carries_its_own_libraries() {
        // The Espressif fork binary links against libraries no package manager provides
        // (`libslirp.so.0` among them): the spike ran it with this directory on
        // `LD_LIBRARY_PATH`, and without it the launch fails before the machine exists.
        for machine in MACHINES {
            let env = qemu_env_for(*machine).unwrap();
            match machine.kind {
                MachineKind::Arm => assert!(env.is_empty()),
                MachineKind::Xtensa => {
                    assert_eq!(env.len(), 1);
                    assert_eq!(env[0].0, "LD_LIBRARY_PATH");
                    assert!(!env[0].1.is_empty());
                }
            }
        }
    }

    /// The ESP32-S3 ROM banner plus a complete census: the transcript the harness polls
    /// while the guest — which never exits — keeps running.
    fn xtensa_transcript() -> String {
        format!(
            "ESP-ROM:esp32s3-20231206\r\n\
             rst:0x1 (POWERON),boot:0x8 (SPI_FAST_FLASH_BOOT)\r\n\
             SPIWP:0xee\r\n\
             mode:DIO, clock div:1\r\n\
             load:0x3fce3808,len:0x43c\r\n\
             {PREFIX} cases passed=21 exempt=2\n\
             {PREFIX} rig iterations=12 cuts=12 resumes=12 unextendable=0 redeliveries=8 verdicts=24 dispatched=42\n\
             {PREFIX} ok\n"
        )
    }

    #[test]
    fn the_esp32s3_transcript_parses_despite_the_rom_banner() {
        // The ROM banner is the emulator's own noise — the guest's census lines end in a
        // bare `\n`, as the spike's verified UART log shows — and `parse` reads past
        // both.
        assert_eq!(parse(&xtensa_transcript()), Some(clean_census()));
        assert!(xtensa_complete(&xtensa_transcript()));
    }

    #[test]
    fn a_transcript_without_the_last_word_is_not_complete() {
        // The guest never exits, so the harness polls: stopping on the counts alone would
        // report on a rig that may not have finished — the truncated write `parse`
        // already refuses.
        let partial = xtensa_transcript().replace(&format!("{PREFIX} ok\n"), "");
        assert_eq!(parse(&partial), None);
        assert!(!xtensa_complete(&partial));
    }

    /// The ESP32-S3 ROM banner plus the guest's failure lines: what the UART holds when
    /// the rig refused its own census and the guest parked.
    fn xtensa_failure_transcript() -> String {
        format!(
            "ESP-ROM:esp32s3-20231206\r\n\
             rst:0x1 (POWERON),boot:0x8 (SPI_FAST_FLASH_BOOT)\r\n\
             {PREFIX} cases passed=21 exempt=2\n\
             {PREFIX} failed no iteration ran, so the image started and the rig did not\n"
        )
    }

    #[test]
    fn a_guest_failure_line_is_terminal() {
        // The guest prints `failed` and then parks: the census will never come, so the
        // harness must not wait out the timeout and misreport a failure as a livelock.
        assert!(xtensa_failed(&xtensa_failure_transcript()));
        assert!(!xtensa_complete(&xtensa_failure_transcript()));
    }

    #[test]
    fn a_breach_failure_line_is_terminal_with_its_code() {
        // Codex P2 (review_comment 3996354967): the breach code rides on the failed
        // line itself — the harness kill fires on the first terminated `failed`
        // line, so the detector must still recognize the new format and the code
        // must be inside that one line, where the kill cannot separate them.
        let transcript = format!("{PREFIX} failed a verdict was a breach (breach code=7)\n");
        assert!(xtensa_failed(&transcript));
        assert!(!xtensa_complete(&transcript));
        assert!(transcript.contains("(breach code=7)"));
    }

    #[test]
    fn a_guest_panic_line_is_terminal() {
        let transcript = format!("{PREFIX} panicked: explicit panic at 'boom'\n");
        assert!(xtensa_failed(&transcript));
        assert!(!xtensa_complete(&transcript));
    }

    #[test]
    fn a_guest_panic_report_is_one_line() {
        // Codex P2 (review_comment 3997629597): `PanicInfo`'s `Display` inserts
        // a newline between the location and the message when a panic has
        // both, so `panicked: {info}` was two terminated lines — and
        // `start_xtensa` kills QEMU on the first terminated `panicked:` line,
        // before the message is written and before `uart_drain` runs, losing
        // the diagnostic the detector exists to preserve. The Xtensa guest now
        // prints the message and the location as one line, so the kill cannot
        // separate the diagnostic from its newline. This pins the shape the
        // harness keys its kill on: the whole report — message and location —
        // inside the first terminated line.
        let transcript = format!("{PREFIX} panicked: boom at src/main.rs:3:5\n");
        assert!(xtensa_failed(&transcript));
        assert!(!xtensa_complete(&transcript));
        assert!(transcript.contains("boom"));
    }

    #[test]
    fn a_half_written_failure_line_does_not_end_the_run() {
        // The UART appends this log byte by byte while the harness polls it, and
        // `str::lines` yields the unterminated trailing fragment as a line: a poll
        // that lands on the bare prefix mid-write must not kill QEMU before the
        // diagnostic and its newline arrive — that output is what the failure
        // detector exists to preserve.
        let transcript = format!("{PREFIX} cases passed=21 exempt=2\n{PREFIX} failed ");
        assert!(!xtensa_failed(&transcript));
    }

    #[test]
    fn a_half_written_panic_line_does_not_end_the_run() {
        // Same truncation race on the panic path: the colon may be on the wire
        // while the info — and the newline — is not.
        let transcript = format!("{PREFIX} panicked:");
        assert!(!xtensa_failed(&transcript));
    }

    #[test]
    fn a_clean_transcript_is_not_a_failure() {
        assert!(!xtensa_failed(&xtensa_transcript()));
    }

    #[test]
    fn unprefixed_noise_is_not_a_guest_failure() {
        // The line must be the guest's own: emulator chatter containing the word must not
        // end the run.
        let transcript = "qemu: warning: something failed to initialize\n".to_owned()
            + &xtensa_transcript().replace(&format!("{PREFIX} ok\n"), "");
        assert!(!xtensa_failed(&transcript));
    }

    #[test]
    fn three_cores_that_agree_pass() {
        assert_eq!(clean_report().shortfall(), None);
    }

    #[test]
    fn the_esp32s3_is_opt_in() {
        // The S3's build and boot need the locally provisioned Espressif stack — fork
        // toolchain, fork QEMU, private offline cargo cache, esptool, fork libraries —
        // which no clean runner has. Without the opt-in the run covers the ARM pair, so
        // a clean runner's `cargo xtask emulate` measures the two cores it can start
        // rather than failing the preflight on the one it cannot.
        let arm: Vec<&str> = machines_for(false)
            .iter()
            .map(|machine| machine.name)
            .collect();
        assert_eq!(arm, vec!["cortex-m0", "cortex-m4"]);
        assert_eq!(machines_for(true), MACHINES);
    }

    #[test]
    fn an_opted_out_run_passes_on_the_arm_pair() {
        // The gate's first demand is that every *selected* machine ran: two rows for two
        // selected machines is a pass, not a missing machine.
        let machines = machines_for(false);
        let report = Report {
            rows: machines
                .iter()
                .map(|machine| ran(machine.name, clean_census()))
                .collect(),
            machines,
        };
        assert_eq!(report.shortfall(), None);
        let rendered = report.render();
        assert!(rendered.contains("both cores"), "{rendered}");
        assert!(!rendered.contains("all three cores"), "{rendered}");
    }

    #[test]
    fn an_opted_in_run_still_requires_every_selected_machine() {
        // Opting in and then losing the machine's row is the failure the first demand
        // exists for — the opt-in chooses the machines, it never excuses a missing one.
        let mut report = clean_report();
        report.rows.pop();
        assert!(report.shortfall().is_some());
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
    fn hand_written_unsafe_in_an_xtensa_gated_module_is_permitted() {
        // The Xtensa startup has no `cortex-m-rt` to expand: the stack-pointer install,
        // the `.bss` zeroing and the UART register pokes cannot be spelled without
        // `unsafe`. The exception is scoped by the `#[cfg(target_arch = "xtensa")]` gate
        // on the module declaration — an ARM image can never contain the code — and this
        // is what checks the gate rather than the keyword.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn hand_written_unsafe_in_an_ungated_module_is_reported() {
        // The same `unsafe` without the gate: the module would compile into the ARM
        // image, where the exception covers two macro expansions and nothing
        // hand-written.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             mod helper;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/helper.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert!(
            !check(&sources).is_empty(),
            "an ungated module's `unsafe` must be reported"
        );
    }

    #[test]
    fn hand_written_unsafe_in_a_composite_cfg_module_is_reported() {
        // `#[cfg(any(target_arch = "arm", target_arch = "xtensa"))]` compiles the
        // module into the ARM image, so the Xtensa-only exception must not apply —
        // the gate check has to compare the attribute structurally and accept only
        // the single `#[cfg(target_arch = "xtensa")]` predicate (Codex P2,
        // `review_comment` 3996215973: the old visitor kept the flag the Xtensa
        // entry set and exempted a module that compiles on ARM).
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(any(target_arch = \"arm\", target_arch = \"xtensa\"))]\n\
             mod xtensa;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert!(
            !check(&sources).is_empty(),
            "a composite cfg that compiles on ARM must not carry the Xtensa exemption"
        );
    }

    #[test]
    fn hand_written_unsafe_in_a_nested_module_named_xtensa_is_reported() {
        // The exception is scoped to the file the root's gated declaration resolves
        // to, not to its stem: `helpers/xtensa.rs` is a module the root never
        // declared, and its `unsafe` must not ride on the root's own gated
        // `mod xtensa;`.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod helpers;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/helpers/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a same-named nested module is not the gated one: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("helpers/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_ungated_path_attribute_reaching_the_gated_file_refuses_the_exemption() {
        // `#[path = "xtensa.rs"] mod arm_startup;` includes the same file in an ARM
        // image: the gated `mod xtensa;` beside it does not keep the exception alive.
        // This is the hole the exemption check must not have — one ungated declaration
        // reaching the file is enough to refuse it.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[path = \"xtensa.rs\"]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated #[path] declaration reaching the file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_noncanonical_path_spelling_reaching_the_gated_file_refuses_the_exemption() {
        // `#[path = "sub/../xtensa.rs"]` names the same file as `#[path = "xtensa.rs"]`
        // — rustc resolves both identically — so an ungated declaration with the
        // non-canonical spelling must refuse the exemption just the same. (A plain
        // `"./xtensa.rs"` already resolves: `Path` equality normalizes `.` away.)
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[path = \"sub/../xtensa.rs\"]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated #[path = \"sub/../xtensa.rs\"] declaration reaching the file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_gated_path_attribute_keeps_the_exemption() {
        // `#[path]` on the gated declaration itself names the gated file: the exemption
        // follows the attribute to `startup.rs` rather than demanding `<ident>.rs`.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             #[path = \"startup.rs\"]\n\
             mod xtensa;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/startup.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn a_cfg_attr_path_reaching_the_gated_file_refuses_the_exemption() {
        // `#[cfg_attr(target_arch = "arm", path = "xtensa.rs")] mod arm_startup;`
        // includes the same file in an ARM image. The checker does not evaluate cfg
        // predicates, so a conditional path naming the gated file counts as reaching
        // it, and the ungated declaration refuses the exemption — the plain-`#[path]`
        // fix left this spelling of the hole open (Codex P2, review_comment
        // 3995126216).
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", path = \"xtensa.rs\")]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated #[cfg_attr(.., path = ..)] declaration reaching the file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_plain_path_beside_a_cfg_attr_path_naming_the_gated_file_refuses_the_exemption() {
        // A plain `#[path]` beside a `#[cfg_attr(.., path = ..)]` does not hide the
        // file: rustc applies both in order and the conditional one may still be
        // active, so the conditional path is read first and still refuses the
        // exemption when it names the gated file.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[path = \"other.rs\"]\n\
             #[cfg_attr(target_arch = \"arm\", path = \"xtensa.rs\")]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a plain #[path] does not hide a #[cfg_attr(.., path = ..)] naming the gated file: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_cfg_attr_path_naming_another_file_keeps_the_exemption() {
        // The conditional path names a different file than the gated one, and the
        // default module name does too: no ungated declaration reaches the gated
        // file, so the exemption stands.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", path = \"arm_startup.rs\")]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn an_ungated_include_macro_reaching_the_gated_file_refuses_the_exemption() {
        // Codex P2 (review_comment 3995859257): `include!("xtensa.rs")` pastes the
        // gated file's contents — hand-written `unsafe` included — into the
        // including file's compilation, but it is an `ItemMacro`, not an
        // `ItemMod`, so the module-declaration walk never sees it. An ungated
        // inclusion in an ARM-reachable file compiles the same `unsafe` into an
        // ARM image the gated `mod xtensa;` never excuses, and must refuse the
        // exemption like any other ungated declaration.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             include!(\"xtensa.rs\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated include! reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_gated_include_macro_keeps_the_exemption() {
        // The gate travels with the inclusion: a `#[cfg(target_arch = "xtensa")]`
        // on the `include!` keeps the file out of ARM images, so the exemption
        // stands — and a gated inclusion alone is a declaration the exemption can
        // rest on.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             include!(\"xtensa.rs\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn an_include_macro_naming_another_file_keeps_the_exemption() {
        // The inclusion names a different file than the gated one: no ungated
        // inclusion reaches the gated file, so the exemption stands.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             include!(\"other.rs\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/other.rs"),
                contents: "pub fn other() {}\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn an_include_macro_in_a_nested_module_file_resolves_against_the_file() {
        // `include!` resolves against the file containing the invocation, not
        // against any module directory: in `src/helpers.rs` it names
        // `src/xtensa.rs`, and the ungated inclusion there refuses the exemption
        // even though the gated `mod xtensa;` sits beside the gated declaration
        // in the root.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod helpers;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/helpers.rs"),
                contents: "include!(\"xtensa.rs\");\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated include! in a nested module file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_include_macro_with_a_computed_path_refuses_the_exemption() {
        // `include!(concat!(...))` names a file the checker cannot enumerate: like
        // an unparsable file, it fails closed and the exemption is refused, since
        // the computed path may reach the gated file on ARM.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             include!(concat!(\"xten\", \"sa.rs\"));\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an include! with a computed path fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_macro_rules_body_hiding_an_include_of_the_gated_file_refuses_the_exemption() {
        // Codex P2 (review_comment 3995964718): a `macro_rules!` body is opaque
        // tokens to `syn`, so `include!("xtensa.rs")` nested inside one never
        // reaches the inclusion walk — neither the definition (an `ItemMacro`
        // whose path is `macro_rules`, not `include`) nor the invocation (whose
        // path is the macro's name) is recorded. An ARM-reachable invocation
        // pastes the gated file's hand-written `unsafe` into an ARM image no
        // recorded declaration excuses, so a definition that could generate an
        // inclusion fails the exemption closed.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! load_startup {{\n\
                 () => {{ include!(\"xtensa.rs\"); }};\n\
             }}\n\
             load_startup!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an include! hidden in a macro_rules! body fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_macro_rules_body_without_an_include_keeps_the_exemption() {
        // Only a body that could generate an inclusion fails closed: a
        // `macro_rules!` with no `include!` in its token tree is not a
        // declaration reaching the gated file, so the exemption stands.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! spin {{\n\
                 () => {{ core::hint::spin_loop() }};\n\
             }}\n\
             spin!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn a_macro_rules_body_hiding_an_include_of_another_file_refuses_the_exemption() {
        // The checker does not resolve which file a nested `include!` names — a
        // textual token scan cannot see through nested macro layers the way the
        // parsed-argument walk does — so any `include!` shape in a
        // `macro_rules!` body fails closed, like a computed `include!` path,
        // even when the literal names another file.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! load_data {{\n\
                 () => {{ include!(\"data.rs\"); }};\n\
             }}\n\
             load_data!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/data.rs"),
                contents: "pub fn data() {}\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an include! of any file hidden in a macro_rules! body fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_macro_invocation_hiding_an_include_of_the_gated_file_refuses_the_exemption() {
        // Codex P2 (review_comment 3996149092): the invocation walk records only
        // macros whose path is `include`, so `pass!(include!("xtensa.rs"))` is
        // ignored even though its own token stream holds the inclusion — and the
        // `pass!` definition's body (`$($t)*`) holds no `include!` shape for the
        // `macro_rules!` body scan to catch. rustc expands the forwarding wrapper
        // into the inclusion, pasting the gated file's hand-written `unsafe` into
        // an ARM image no recorded declaration excuses: a non-`include`
        // invocation whose tokens could generate an inclusion fails the exemption
        // closed.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! pass {{\n\
                 ($($t:tt)*) => {{ $($t)* }};\n\
             }}\n\
             pass!(include!(\"xtensa.rs\"));\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an include! hidden in a macro invocation's tokens fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_macro_invocation_hiding_an_include_of_another_file_refuses_the_exemption() {
        // The checker cannot see through the wrapper's expansion: `pass!` may
        // forward its tokens (generating the inclusion) or drop them, so any
        // `include!` shape in a non-`include` invocation's tokens fails closed,
        // like a computed `include!` path, even when the nested literal names
        // another file.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! pass {{\n\
                 ($($t:tt)*) => {{ $($t)* }};\n\
             }}\n\
             pass!(include!(\"data.rs\"));\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/data.rs"),
                contents: "pub fn data() {}\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an include! of any file hidden in a macro invocation's tokens fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_macro_invocation_without_an_include_keeps_the_exemption() {
        // Only an invocation whose tokens could generate an inclusion fails
        // closed: a wrapper invocation with no `include!` in its token tree is
        // not a declaration reaching the gated file, so the exemption stands.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! pass {{\n\
                 ($($t:tt)*) => {{ $($t)* }};\n\
             }}\n\
             pass!(core::hint::spin_loop());\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn a_macro_rules_body_hiding_a_module_declaration_of_the_gated_file_refuses_the_exemption() {
        // Codex P2 (review_comment 3997403828): `syn` leaves a `macro_rules!`
        // body as opaque tokens, so `#[path = "xtensa.rs"] mod arm_startup;`
        // nested inside one never reaches the module-declaration walk —
        // neither the definition (an `ItemMacro` whose path is `macro_rules`,
        // not a `mod` item) nor the invocation (whose path is the macro's
        // name) is recorded. An ARM-reachable invocation compiles the gated
        // file's hand-written `unsafe` into an ARM image no recorded
        // declaration excuses (verified against rustc), so a definition that
        // could generate a module declaration fails the exemption closed,
        // like one that could generate an inclusion.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! load_startup {{\n\
                 () => {{ #[path = \"xtensa.rs\"] mod arm_startup; }};\n\
             }}\n\
             load_startup!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a module declaration hidden in a macro_rules! body fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_macro_rules_body_hiding_a_module_declaration_of_another_file_refuses_the_exemption() {
        // The checker does not resolve which file a `mod` item hidden in
        // opaque tokens names — a `#[path]` inside could resolve against any
        // invocation site's directory, which the definition-side scan cannot
        // enumerate — so any `mod` shape in a `macro_rules!` body fails
        // closed, like an `include!` shape, even when the path names another
        // file.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! load_data {{\n\
                 () => {{ #[path = \"data.rs\"] mod data; }};\n\
             }}\n\
             load_data!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/data.rs"),
                contents: "pub fn data() {}\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a module declaration of any file hidden in a macro_rules! body fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_macro_invocation_hiding_a_module_declaration_refuses_the_exemption() {
        // Codex P2 (review_comment 3997403828): the same hole at the
        // invocation-token call site — the walk records only `mod` items, so
        // `pass!(#[path = "xtensa.rs"] mod arm_startup;)` is ignored even
        // though its own token stream holds the declaration, and the `pass!`
        // definition's body (`$($t)*`) holds no `mod` shape for the
        // `macro_rules!`-body scan to catch. rustc expands the forwarding
        // wrapper into the declaration (verified against rustc), compiling the
        // gated file's hand-written `unsafe` into an ARM image no recorded
        // declaration excuses: a non-`include` invocation whose tokens could
        // generate a module declaration fails the exemption closed.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! pass {{\n\
                 ($($t:tt)*) => {{ $($t)* }};\n\
             }}\n\
             pass!(#[path = \"xtensa.rs\"] mod arm_startup;);\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a module declaration hidden in a macro invocation's tokens fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_external_macro_invocation_with_no_visible_tokens_refuses_the_exemption() {
        // Codex P2 (review_comment 3997510667): the definition scan examines
        // only `macro_rules!` bodies in this crate, so a macro exported by a
        // dependency is invisible to it — and an argument-free invocation
        // carries no `mod`/`include!` tokens for the invocation-token scan
        // either. `use dependency::load_startup;` + `load_startup!();`
        // expanding (in the dependency) to `#[path = "xtensa.rs"] mod
        // arm_startup;` compiles the gated file's hand-written `unsafe` into
        // an ARM image the separately gated `mod xtensa;` still excuses. An
        // invocation the checker cannot account for fails the exemption
        // closed.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use dependency::load_startup;\n\
             load_startup!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an unresolvable external macro invocation fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_unknown_external_attribute_macro_refuses_the_exemption() {
        // Codex P2 (review_comment 3997629593): the inclusion walk only
        // overrides visits for function-like item, statement, and expression
        // macros — never attributes. `use evil_startup::startup;` plus
        // `#[startup]` on an ARM-reachable item, expanding (in the dependency,
        // invisible to this checker) to `#[path = "xtensa.rs"] mod arm_startup;`,
        // compiles the gated file's hand-written `unsafe` into an ARM image
        // the separately gated `mod xtensa;` still excuses. An attribute macro
        // the checker cannot account for fails the exemption closed.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use evil_startup::startup;\n\
             #[startup]\n\
             fn arm_main() {{}}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an unaccounted-for attribute macro fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_unknown_bare_attribute_macro_refuses_the_exemption() {
        // The same fail-closed rule with no import to resolve through: a bare
        // `#[startup]` names no pinned exemption, so its expansion is
        // unknowable and the exemption is refused.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[startup]\n\
             fn arm_main() {{}}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a bare attribute macro the checker cannot resolve fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn the_pinned_entry_attribute_keeps_the_exemption() {
        // The firmware's one attribute macro: `#[entry]` imported from
        // `cortex-m-rt` (Cargo.lock-pinned 0.7.6), whose expansion renames the
        // input function, emits the exported trampoline calling it, and hoists
        // `static mut` locals into explicit arguments — verified in the pinned
        // `cortex-m-rt-macros` source to contain no `mod` item and no
        // `include!`. The exemption is keyed on the (crate, name) pair, so the
        // gate stays green on the real crate without trusting bare names.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use cortex_m_rt::entry;\n\
             #[entry]\n\
             fn main() -> ! {{ loop {{}} }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "the pinned #[entry] exemption keeps the gate green: {violations:?}",
        );
    }

    #[test]
    fn a_renamed_entry_import_keeps_the_exemption() {
        // Renames are honored: `use cortex_m_rt::entry as start;` still
        // resolves the attribute to the verified (crate, name) pair.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use cortex_m_rt::entry as start;\n\
             #[start]\n\
             fn main() -> ! {{ loop {{}} }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "a renamed import of the exempted attribute keeps the gate green: {violations:?}",
        );
    }

    #[test]
    fn an_entry_attribute_from_another_crate_refuses_the_exemption() {
        // The (crate, name) key is doing the work: `#[entry]` imported from a
        // different crate is a different, unexamined expansion.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use evil_rt::entry;\n\
             #[entry]\n\
             fn main() -> ! {{ loop {{}} }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an #[entry] from an unexamined crate fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_bare_entry_attribute_with_no_import_refuses_the_exemption() {
        // A bare `#[entry]` with no import names no crate: it cannot resolve
        // to the pinned exemption, and no prelude provides attribute macros.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[entry]\n\
             fn main() -> ! {{ loop {{}} }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a bare #[entry] with no import fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_local_module_shadowing_the_pinned_attribute_crate_refuses_the_exemption() {
        // Codex P2 (review_comment 3997890784): the multi-segment attribute
        // arm trusted `(root, name) == ("cortex_m_rt", "entry")` outright. A
        // local item shadows the extern prelude in path resolution, so
        // `mod cortex_m_rt { pub use evil::entry; }` plus
        // `#[cortex_m_rt::entry]` applies evil's procedural macro — an
        // unexamined expansion that could declare the gated file — not the
        // pinned one. The pinned exemption applies only when the root still
        // names the extern crate.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod cortex_m_rt {{ pub use evil::entry; }}\n\
             #[cortex_m_rt::entry]\n\
             fn arm_main() {{}}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a pinned attribute path shadowed by a local module fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_pinned_attribute_path_with_no_shadowing_keeps_the_exemption() {
        // The carve-out the shadowing fix keeps: `#[cortex_m_rt::entry]` with
        // no local item of that name still resolves to the pinned
        // `cortex-m-rt` attribute macro, whose expansion was verified to
        // contain no `mod` item and no `include!`.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cortex_m_rt::entry]\n\
             fn arm_main() {{}}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(
            check(&sources),
            Vec::new(),
            "an unshadowed pinned attribute path keeps the exemption"
        );
    }

    #[test]
    fn a_local_module_shadowing_the_pinned_macro_crate_refuses_the_exemption() {
        // The same shadowing hole on the macro side, found while verifying
        // review_comment 3997890784: `cortex_m_semihosting::hprintln!()` with a
        // local `mod cortex_m_semihosting` re-exporting evil's macro resolves
        // to the hostile expansion, not the pinned exemption — so the pinned
        // external-macro exemption fails closed on a shadowed root too.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod cortex_m_semihosting {{ pub use evil::hprintln; }}\n\
             cortex_m_semihosting::hprintln!(\"x\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a pinned macro path shadowed by a local module fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_pinned_macro_path_with_no_shadowing_keeps_the_exemption() {
        // The carve-out the shadowing fix keeps: `cortex_m_semihosting::hprintln!`
        // with no local item of that name still resolves to the pinned
        // `cortex-m-semihosting` macro.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             cortex_m_semihosting::hprintln!(\"x\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(
            check(&sources),
            Vec::new(),
            "an unshadowed pinned macro path keeps the exemption"
        );
    }

    #[test]
    fn std_derives_keep_the_exemption() {
        // The firmware derives `Clone`, `Copy`, `Debug`, `Default`,
        // `PartialEq`, `Eq`: std derives expand to trait impls only — no `mod`
        // item and no `include!` can come out — so they are ignored, and the
        // gate stays green on the real crate.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]\n\
             struct Census {{ passed: u32 }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "std derives keep the gate green: {violations:?}",
        );
    }

    #[test]
    fn an_external_derive_refuses_the_exemption() {
        // A derive from another crate is an unexamined external expansion: a
        // custom derive can emit arbitrary items, so it fails closed like any
        // other unaccounted-for macro.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use serde::Serialize;\n\
             #[derive(Serialize)]\n\
             struct Census {{ passed: u32 }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an external derive fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_shadowed_builtin_attribute_refuses_the_exemption() {
        // A `use` importing a builtin attribute's name from another crate may
        // name an attribute macro instead of the builtin: the builtin
        // whitelist applies only when the name is not shadowed.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use evil::cfg;\n\
             #[cfg(all())]\n\
             fn arm_main() {{}}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a shadowed builtin attribute fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_unknown_bare_macro_invocation_refuses_the_exemption() {
        // The same fail-closed rule with no import to resolve through: a bare
        // `mystery!();` names no local `macro_rules!`, no builtin, and no
        // imported external macro, so its expansion is unknowable and the
        // exemption is refused.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mystery!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a macro invocation the checker cannot resolve fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_external_crate_shadowing_a_builtin_name_refuses_the_exemption() {
        // The builtin whitelist applies only when the name really is the
        // builtin: `use evil::concat;` imports the name from another crate, so
        // `concat!(...)` may be that crate's macro rather than rustc's, and
        // the invocation fails closed like any other external one.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use evil::concat;\n\
             concat!(\"a\", \"b\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a builtin name imported from an external crate is not the builtin: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_builtin_name_imported_through_a_local_reexport_refuses_the_exemption() {
        // Codex P2 (review_comment 3997946866): the builtin whitelist applies
        // only when the name really is the builtin, and a `use` rooted at
        // `crate::` / `self::` / `super::` does not prove that — the path may
        // resolve through a local module re-exporting another crate's macro.
        // `pub use evil::assert;` inside `mod shim` (in another file), plus
        // `use crate::shim::assert;` in the root, makes the root's bare
        // `assert!()` invoke `evil::assert`, whose expansion is invisible
        // here: the invocation fails closed like any other unresolvable one.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod shim;\n\
             use crate::shim::assert;\n\
             assert!(true);\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/shim.rs"),
                contents: "pub use evil::assert;\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a builtin name imported through a local re-export is not the builtin: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_builtin_name_reexported_through_a_local_module_in_the_same_file_refuses_the_exemption() {
        // The same hole with the re-export in the file itself: `mod shim {
        // pub use evil::assert; }` plus `use crate::shim::assert;`. The
        // whole-tree `use` walk already records the nested external import,
        // so this failed closed before the local-import rule existed too —
        // the test locks that behavior in.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod shim {{ pub use evil::assert; }}\n\
             use crate::shim::assert;\n\
             assert!(true);\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a builtin name re-exported through a same-file local module is not the builtin: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_local_macro_invoked_bare_keeps_the_exemption() {
        // A `macro_rules!` definition in the crate is vetted by the
        // definition scan, so a bare invocation of it stays accounted for —
        // the local-import rule only fires when a `use` actually binds the
        // name through a local path.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! check {{ () => {{ let _ = 1u8; }} }}\n\
             check!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "a bare invocation of a vetted local macro stays accounted for: {violations:?}",
        );
    }

    #[test]
    fn an_external_macro_nested_in_a_macro_rules_body_refuses_the_exemption() {
        // Codex P2 (review_comment 3997946868): a `macro_rules!` body is
        // opaque tokens, and the definition scan only looked for `include!`
        // / `mod` shapes in it — so `macro_rules! wrapper { () => {
        // evil::load_startup!(); } }` smuggled an external macro's invisible
        // expansion past both scans: the body holds neither shape, and the
        // later `wrapper!()` is trusted as a local macro, while the expansion
        // could declare `#[path = "xtensa.rs"] mod arm_startup;` on ARM.
        // Nested invocations are classified with the same rule the
        // top-level walk applies: anything unaccounted for fails the
        // exemption closed.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! wrapper {{ () => {{ evil::load_startup!(); }} }}\n\
             wrapper!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an external macro nested in a macro_rules! body fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_builtin_macro_nested_in_a_macro_rules_body_keeps_the_exemption() {
        // A nested builtin has a fixed expansion with no items in it, so it
        // can neither declare the gated module nor paste it — the definition
        // scan still accounts for the local macro around it.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! wrapper {{ () => {{ let _ = concat!(\"a\", \"b\"); }} }}\n\
             wrapper!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "a builtin nested in a macro_rules! body stays accounted for: {violations:?}",
        );
    }

    #[test]
    fn a_local_macro_nested_in_a_macro_rules_body_keeps_the_exemption() {
        // A nested local macro is fine too: its own definition is vetted by
        // the same definition scan, so no transitive walk is needed — every
        // definition in the crate is visited independently.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! inner {{ () => {{ panic!(\"x\"); }} }}\n\
             macro_rules! wrapper {{ () => {{ inner!(); }} }}\n\
             wrapper!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "a local macro nested in a macro_rules! body stays accounted for: {violations:?}",
        );
    }

    #[test]
    fn a_builtin_name_imported_from_core_keeps_the_exemption() {
        // `core::` / `std::` name the builtin namespace, not another crate:
        // the local-import split must not start treating them as external or
        // unresolvable imports. `use core::assert;` plus `assert!(..)` is
        // still the builtin, with its fixed expansion.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use core::assert;\n\
             fn driver() {{\n\
                 assert!(true);\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "a builtin imported from core stays accounted for: {violations:?}",
        );
    }

    #[test]
    fn a_builtin_attribute_imported_from_core_keeps_the_exemption() {
        // `use core::cfg;` compiles, and `#[cfg(..)]` stays the builtin
        // attribute. Recording the builtin namespace in `imports` failed
        // this closed, as if `cfg` came from an unpinned external crate.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             use core::cfg;\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "a builtin attribute imported from core stays accounted for: {violations:?}",
        );
    }

    #[test]
    fn a_std_derive_named_through_a_core_import_keeps_the_exemption() {
        // `use core::fmt::Debug;` binds the trait, not a derive macro — a
        // `use` cannot import a builtin derive — so `#[derive(Debug)]` still
        // names the std derive whose expansion is trait impls only.
        // Recording `core::` roots in `imports` failed this closed by
        // treating the import as shadowing the derive.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use core::fmt::Debug;\n\
             #[derive(Debug)]\n\
             struct Tag;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "a std derive named through a core import stays accounted for: {violations:?}",
        );
    }

    #[test]
    fn a_builtin_macro_invocation_keeps_the_exemption() {
        // Compiler builtins have expansions fixed by rustc that contain no
        // items at all — so no `mod` declaration and no `include!` — and the
        // firmware invokes them (`format_args!`, `core::ptr::addr_of_mut!`,
        // `asm!` family). Failing closed on them would refuse the exemption
        // on the real crate for zero soundness gain: no false positive here.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             static mut CELL: u8 = 0;\n\
             fn driver() {{\n\
                 let _ = format_args!(\"x\");\n\
                 let _ = concat!(\"a\", \"b\");\n\
                 let _ = vec![1u8];\n\
                 let _ = core::ptr::addr_of_mut!(CELL);\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "builtin macro invocations are accounted for: {violations:?}",
        );
    }

    #[test]
    fn the_pinned_external_macro_exemption_keeps_the_exemption() {
        // The firmware's one external macro: `hprintln!` imported from
        // `cortex-m-semihosting` (Cargo.lock-pinned 0.5.0), whose arms expand
        // to `$crate::export::hstdout_str` / `hstdout_fmt` — verified in the
        // pinned source to contain no `mod` item and no `include!`. The
        // exemption is keyed on the (crate, name) pair, so the gate stays
        // green on the real crate without trusting bare names.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use cortex_m_semihosting::hprintln;\n\
             hprintln!(\"hello\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "the pinned hprintln! exemption keeps the gate green: {violations:?}",
        );
    }

    #[test]
    fn a_renamed_import_of_the_exempted_external_macro_keeps_the_exemption() {
        // Renames are honored: `use cortex_m_semihosting::hprintln as hlog;`
        // still resolves the invocation to the verified (crate, name) pair.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use cortex_m_semihosting::hprintln as hlog;\n\
             hlog!(\"hello\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "a renamed exempted macro keeps the gate green: {violations:?}",
        );
    }

    #[test]
    fn a_local_macro_invocation_with_clean_tokens_keeps_the_exemption() {
        // A `macro_rules!` defined in the crate is already accounted for: its
        // body is scanned by the definition walk and its invocation tokens by
        // the invocation walk. A clean body plus clean tokens keeps the
        // exemption — the fail-closed rule is for macros whose bodies the
        // checker cannot read, not local ones.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! hello {{\n\
                 () => {{}};\n\
             }}\n\
             hello!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations,
            Vec::new(),
            "a clean local macro keeps the exemption: {violations:?}",
        );
    }

    #[test]
    fn a_gated_cfg_attr_path_keeps_the_exemption() {
        // The plain `#[cfg(target_arch = "xtensa")]` gate is what scopes the
        // exception: a `#[cfg_attr]`-conditional path on a gated declaration still
        // carries it, so the exemption stands.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             #[cfg_attr(target_arch = \"xtensa\", path = \"xtensa.rs\")]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn a_nested_cfg_attr_path_reaching_the_gated_file_refuses_the_exemption() {
        // `#[cfg_attr(target_arch = "arm", cfg_attr(any(), path = "xtensa.rs"))] mod
        // arm_startup;` includes the same file in an ARM image: rustc expands the
        // inner `cfg_attr` once the outer predicate holds, so the inner `path`
        // applies — the checker must recurse into nested `cfg_attr`s before granting
        // the unsafe exemption (Codex P2, review_comment 3995679544).
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", cfg_attr(any(), path = \"xtensa.rs\"))]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated nested #[cfg_attr(.., cfg_attr(.., path = ..))] declaration reaching the file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_procedural_attribute_emitted_by_cfg_attr_refuses_the_exemption() {
        // Codex P2 (review_comment 3997773904): `#[cfg_attr(target_arch =
        // "arm", evil::startup)]` — the outer `cfg_attr` is a builtin, but the
        // attribute it emits under the predicate is a procedural macro from
        // another crate, whose expansion is invisible here and could declare
        // the gated file. The classifier must recurse into the attributes a
        // `cfg_attr` emits rather than trusting the outer name.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", evil::startup)]\n\
             fn driver() {{}}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a procedural attribute smuggled in through cfg_attr fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_builtin_attribute_emitted_by_cfg_attr_keeps_the_exemption() {
        // The recursion is not a blanket refusal: a `cfg_attr` emitting only
        // inert builtins is still accounted for, so the gate stays green on
        // ordinary conditional attributes.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", allow(dead_code))]\n\
             fn driver() {{}}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(
            check(&sources),
            Vec::new(),
            "a cfg_attr emitting only builtins keeps the exemption"
        );
    }

    #[test]
    fn an_external_macro_path_sharing_a_local_name_refuses_the_exemption() {
        // Codex P2 (review_comment 3997773907): `dependency::load_startup!()`
        // alongside an unrelated local `macro_rules! load_startup` with a
        // clean body — the path names the dependency's macro, whose expansion
        // is invisible here and could declare the gated file. The local-name
        // shortcut must not override an explicit external root.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! load_startup {{\n\
                 () => {{}};\n\
             }}\n\
             dependency::load_startup!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an external macro path is not excused by a same-named local macro: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_imported_external_macro_beats_an_unrelated_local_definition_refuses_the_exemption() {
        // Codex P2 (review_comment 3997890781): the bare-invocation arm
        // consulted the crate-wide over-approximated local-name set before the
        // file's imports. With an unrelated `macro_rules! load_startup` in
        // another module and `use dependency::load_startup;` in the
        // ARM-reachable file, the bare `load_startup!()` resolves to the
        // dependency's macro — textual scope covers only the defining module —
        // whose expansion is invisible here and could declare the gated file.
        // Imports must resolve before the local-name shortcut, as the
        // `crate::`-rooted path arm already does.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use dependency::load_startup;\n\
             load_startup!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/other.rs"),
                contents: "macro_rules! load_startup { () => {}; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an imported external macro is not excused by a same-named local definition: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_crate_rooted_path_to_a_local_macro_keeps_the_exemption() {
        // The carve-out the external-root fix keeps: `crate::` / `self::` /
        // `super::` resolve inside the crate, so a path to a local
        // `macro_rules!` — whose body the definition scan already examined —
        // stays accounted for.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             macro_rules! load_startup {{\n\
                 () => {{}};\n\
             }}\n\
             fn driver() {{\n\
                 crate::load_startup!();\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(
            check(&sources),
            Vec::new(),
            "a crate-rooted path to a local macro keeps the exemption"
        );
    }

    #[test]
    fn a_doubly_nested_cfg_attr_path_reaching_the_gated_file_refuses_the_exemption() {
        // rustc's `cfg_attr` expansion is recursive to any depth, so the search for
        // the applied `path` must be too: a `path` three `cfg_attr`s deep still
        // names the file rustc loads.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", cfg_attr(any(), cfg_attr(all(), path = \"xtensa.rs\")))]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a doubly nested cfg_attr path reaching the file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_later_nested_cfg_attr_alternative_naming_the_gated_file_refuses_the_exemption() {
        // One outer `cfg_attr` can hold several nested `cfg_attr` alternatives naming
        // different files: rustc expands each in place once the outer predicate holds,
        // so an inactive first branch naming `decoy.rs` followed by an active later
        // branch naming `xtensa.rs` still compiles the gated file's hand-written
        // `unsafe` into an ARM image. The checker does not evaluate predicates, so it
        // must collect every alternative's path — seeing only the first one granted
        // the exemption (Codex P2, review_comment 3995728343).
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", cfg_attr(any(), path = \"decoy.rs\"), cfg_attr(all(), path = \"xtensa.rs\"))]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a later nested #[cfg_attr(.., path = ..)] alternative naming the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn every_nested_cfg_attr_alternative_path_is_collected() {
        // The collector behind the exemption check returns every `path` an outer
        // `cfg_attr` can apply — not just the first nested alternative's — because
        // the checker never evaluates predicates and a later alternative may be the
        // active one (Codex P2, review_comment 3995728343).
        use syn::parse::Parser as _;
        let mut attrs =
            syn::Attribute::parse_outer
                .parse_str(
                    r#"#[cfg_attr(target_arch = "arm", cfg_attr(any(), path = "other.rs"), cfg_attr(all(), path = "xtensa.rs"))]"#,
                )
                .expect("test attribute parses");
        assert_eq!(attrs.len(), 1);
        assert_eq!(
            cfg_attr_path_values(&attrs.pop().expect("one attribute")),
            vec!["other.rs".to_owned(), "xtensa.rs".to_owned()],
        );
    }

    #[test]
    fn a_nested_cfg_attr_path_naming_another_file_keeps_the_exemption() {
        // A nested `cfg_attr` path naming a different file is not the gated file:
        // the recursion must not invent reachability that is not there.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", cfg_attr(any(), path = \"arm_startup.rs\"))]\n\
             mod arm_startup;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/arm_startup.rs"),
                contents: "pub fn start() {}\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn an_ungated_path_attribute_in_a_nested_module_refuses_the_exemption() {
        // Codex P2 (review_comment 3995327724): the exemption used to read only the
        // crate root's module items, so an ungated `mod helpers;` in the root plus
        // `#[path = "xtensa.rs"] mod arm_startup;` in `helpers.rs` slipped the same
        // file's hand-written `unsafe` into an ARM image behind the root's gated
        // `mod xtensa;`. Declarations are collected from every source file now, each
        // with its own resolution directory, so the nested ungated declaration
        // reaching the gated file refuses the exemption.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod helpers;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/helpers.rs"),
                contents: "#[path = \"xtensa.rs\"]\nmod arm_startup;\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated #[path] in a nested module reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_gated_path_attribute_in_a_nested_module_keeps_the_exemption() {
        // The nested declaration carries the Xtensa gate itself, so it reaches the
        // file on no target the exception does not cover: the traversal must not
        // mistake "declared in another file" for "declared without the gate".
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod helpers;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/helpers.rs"),
                contents:
                    "#[cfg(target_arch = \"xtensa\")]\n#[path = \"xtensa.rs\"]\nmod arm_startup;\n"
                        .to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn an_ungated_path_attribute_in_an_inline_module_refuses_the_exemption() {
        // Codex P2 (review_comment 3995553789): `module_declarations` already computed
        // the inline module's directory, but `module_resolves_to` joined every `#[path]`
        // to the declaring file's directory — so `mod wrapper { #[path = "../xtensa.rs"]
        // mod arm_startup; }` in the root resolved to `xtensa.rs` instead of
        // `src/xtensa.rs` (rustc resolves it against `src/wrapper/`), and the ungated
        // declaration reaching the gated file slipped past the gate.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod wrapper {{\n\
             #[path = \"../xtensa.rs\"]\n\
             mod arm_startup;\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated #[path] in an inline module reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_gated_path_attribute_in_an_inline_module_keeps_the_exemption() {
        // The nested declaration carries the Xtensa gate itself, so it reaches the
        // file on no target the exception does not cover — even though the path now
        // resolves against the inline module's directory.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod wrapper {{\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             #[path = \"../xtensa.rs\"]\n\
             mod arm_startup;\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn an_ungated_path_attribute_in_a_doubly_nested_inline_module_refuses_the_exemption() {
        // The directory follows the whole inline chain: `#[path = "../../xtensa.rs"]`
        // inside `mod outer { mod inner { ... } }` resolves against `src/outer/inner/`.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod outer {{\n\
             mod inner {{\n\
             #[path = \"../../xtensa.rs\"]\n\
             mod arm_startup;\n\
             }}\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated #[path] in a doubly nested inline module reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_path_attribute_on_an_inline_module_redirects_nested_resolution() {
        // rustc honors `#[path]` on the inline module itself: `#[path = "deep/"] mod
        // wrapper` makes a nested `#[path = "xtensa.rs"]` resolve to `src/deep/xtensa.rs`
        // (verified against rustc), so the ungated nested declaration reaching the gated
        // file must refuse the exemption.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             #[path = \"deep/xtensa.rs\"]\n\
             mod xtensa;\n\
             #[path = \"deep/\"]\n\
             mod wrapper {{\n\
             #[path = \"xtensa.rs\"]\n\
             mod arm_startup;\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/deep/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated #[path] nested in a path-redirected inline module reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("deep/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_nested_module_not_reaching_the_gated_file_keeps_the_exemption() {
        // The traversal reads every file's module items, but only a declaration
        // resolving to the gated file can refuse the exemption: `helpers.rs`
        // declaring an unrelated module must not disturb it.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod helpers;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/helpers.rs"),
                contents: "mod other;\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn a_cfg_attr_path_on_an_inline_module_reaching_the_gated_file_refuses_the_exemption() {
        // Codex P2 (review_comment 3995614205): a `#[cfg_attr(.., path = \"...\")]` on
        // an enclosing inline module redirects its directory the way a plain `#[path]`
        // does — on ARM, `#[cfg_attr(target_arch = \"arm\", path = \"custom/deep\")] mod
        // wrapper` makes the nested `#[path = \"../../xtensa.rs\"] mod arm_startup;`
        // resolve against `src/custom/deep/`, loading `src/xtensa.rs`. The checker
        // resolved the nested path against the plain `src/wrapper/`, saw only the
        // gated root declaration, and wrongly granted the exemption.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", path = \"custom/deep\")]\n\
             mod wrapper {{\n\
             #[path = \"../../xtensa.rs\"]\n\
             mod arm_startup;\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated declaration nested in a cfg_attr-redirected inline module reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_gated_declaration_under_a_cfg_attr_redirected_inline_module_keeps_the_exemption() {
        // The same conditional redirect, but the nested declaration carries the
        // Xtensa gate itself: every declaration reaching the file is gated, so the
        // exemption stands — the candidate directory must not refuse it.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", path = \"custom/deep\")]\n\
             mod wrapper {{\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             #[path = \"../../xtensa.rs\"]\n\
             mod arm_startup;\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn a_cfg_attr_path_on_an_inline_module_not_reaching_the_file_keeps_the_exemption() {
        // The conditional redirect is modeled, but the nested declaration names a
        // different file: the extra candidate directory must not manufacture a
        // declaration the code never made.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             #[cfg_attr(target_arch = \"arm\", path = \"custom/deep\")]\n\
             mod wrapper {{\n\
             #[path = \"other.rs\"]\n\
             mod arm_startup;\n\
             }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn hand_written_unsafe_behind_some_other_gate_is_reported() {
        // A gate that is not the Xtensa one does not scope the exception: the code could
        // still reach an image the exception does not cover.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"arm\")]\n\
             mod xtensa;\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert!(!check(&sources).is_empty());
    }

    #[test]
    fn a_machine_whose_target_is_not_pinned_is_reported() {
        for machine in MACHINES
            .iter()
            .filter(|machine| machine.kind == MachineKind::Arm)
        {
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
    fn the_xtensa_machine_needs_no_workspace_toolchain_pin() {
        // `xtensa-esp32s3-none-elf` exists only in the Espressif fork: no channel a
        // `rust-toolchain.toml` can name ships it, so the pin the ARM machines need would
        // describe a toolchain file that cannot exist. `MachineKind::Xtensa` is the
        // declaration of which toolchain builds it, and the harness fails closed when
        // that toolchain is absent.
        let arm_only: Vec<&str> = MACHINES
            .iter()
            .filter(|machine| machine.kind == MachineKind::Arm)
            .map(|machine| machine.target)
            .collect();
        let toolchain = format!("[toolchain]\nchannel = \"1.97\"\ntargets = {arm_only:?}\n");
        let violations = check_emulation_boot(
            Some(&tests_support::clean_manifest()),
            &tests_support::clean_sources(),
            Some(&toolchain),
            STAGES,
        );
        assert_eq!(violations, Vec::new());
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

    #[test]
    fn an_aliased_include_macro_reaching_the_gated_file_refuses_the_exemption() {
        // Codex P2 (review_comment 3996723105): `use core::include as paste;`
        // imports the builtin `include!` under another name, so an ARM-reachable
        // `paste!("xtensa.rs")` pastes the gated file's hand-written `unsafe`
        // into an ARM image — while the inclusion walk sees only the macro path
        // `paste` and `tokens_could_include` sees only the string literal. An
        // ungated aliased inclusion reaching the gated file must refuse the
        // exemption like any other ungated declaration.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use core::include as paste;\n\
             paste!(\"xtensa.rs\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an ungated aliased include! reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_nested_module_aliasing_include_macro_refuses_the_exemption() {
        // Codex P2 (review_comment 3996992622): the alias collector examined only
        // the file's top-level `use` items, so `mod wrapper { use core::include
        // as paste; paste!("xtensa.rs"); }` never registered `paste`. The ungated
        // inclusion of the gated file then slipped past the walk and carried
        // hand-written `unsafe` into an ARM image under the Xtensa exemption.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod wrapper {{ use core::include as paste; paste!(\"xtensa.rs\"); }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a nested-module aliased include! reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_aliased_include_macro_with_a_computed_path_refuses_the_exemption() {
        // The aliased invocation's argument is not a string literal, so the file
        // it names cannot be enumerated: like a computed `include!` path, it
        // fails the exemption closed.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use core::include as paste;\n\
             paste!(concat!(\"xtensa\", \".rs\"));\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an aliased include! with a computed path fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_aliased_include_macro_naming_another_file_keeps_the_exemption() {
        // The grouped `use` spelling is collected too, but the aliased inclusion
        // names a different file than the gated one: no ungated inclusion reaches
        // the gated file, so the exemption stands.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use core::{{include as paste}};\n\
             paste!(\"other.rs\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/other.rs"),
                contents: "pub fn other() {}\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn a_self_qualified_alias_include_macro_refuses_the_exemption() {
        // Codex P2 (review_comment 3997221943): the parsed-invocation check
        // matched a collected alias only on a single-segment path, so `mod
        // wrapper { use core::include as paste; self::paste!("xtensa.rs"); }`
        // slipped past — the path's last segment is `paste`, and the invocation
        // tokens hold only the string literal. Verified against rustc: the
        // qualified alias resolves and pastes the gated file's hand-written
        // `unsafe` into an ARM image while the exemption stood on the gated
        // `mod xtensa;` alone. A qualified path ending in a collected alias
        // must refuse the exemption like the single-segment form.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod wrapper {{ use core::include as paste; self::paste!(\"xtensa.rs\"); }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a self-qualified aliased include! reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_crate_qualified_alias_include_macro_refuses_the_exemption() {
        // Codex P2 (review_comment 3997221943): the same single-segment
        // restriction missed `crate::paste!("xtensa.rs")` reaching through a
        // `pub use core::include as paste;` re-export — verified against rustc
        // to resolve and paste. The last segment is the collected alias, so it
        // is an inclusion like the single-segment form.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             pub use core::include as paste;\n\
             mod wrapper {{ crate::paste!(\"xtensa.rs\"); }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "a crate-qualified aliased include! reaching the gated file refuses the exemption: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_self_qualified_alias_include_macro_naming_another_file_keeps_the_exemption() {
        // The qualified alias names a different file than the gated one: no
        // ungated inclusion reaches the gated file, so the exemption stands —
        // the widened last-segment match must not false-positive.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             mod wrapper {{ use core::include as paste; self::paste!(\"other.rs\"); }}\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/other.rs"),
                contents: "pub fn other() {}\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn a_gated_aliased_include_macro_keeps_the_exemption() {
        // The gate travels with the aliased inclusion: a `#[cfg(target_arch =
        // "xtensa")]` on `paste!("xtensa.rs")` keeps the file out of ARM images,
        // so the exemption stands.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use core::include as paste;\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             paste!(\"xtensa.rs\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn an_aliased_include_macro_in_a_macro_rules_body_refuses_the_exemption() {
        // Codex P2 (review_comment 3997052796): the opaque-token scan
        // `tokens_could_include` recognized only the literal identifier
        // `include`, so `use core::include as paste;` plus a `macro_rules!`
        // body containing `paste!("xtensa.rs")` slipped past both the
        // parsed-invocation alias handling (a macro body is opaque tokens,
        // never visited as an invocation) and the body scan (which never saw
        // the alias). rustc resolves the alias textually at the definition
        // site and pastes the gated file's hand-written `unsafe` into an ARM
        // image while the exemption stood on the gated `mod xtensa;` alone —
        // so an aliased inclusion shape in a macro body fails closed like a
        // literal one.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use core::include as paste;\n\
             macro_rules! load_startup {{\n\
                 () => {{ paste!(\"xtensa.rs\"); }};\n\
             }}\n\
             load_startup!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an aliased include! hidden in a macro_rules! body fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn an_aliased_include_macro_in_a_wrapper_invocation_refuses_the_exemption() {
        // The same hole at the other `tokens_could_include` call site: the
        // invocation-token scan in `record` (Codex P2, review_comment
        // 3996149092) saw only the literal `include`, so a forwarding wrapper
        // `pass!(paste!("xtensa.rs"))` with the alias in scope slipped past
        // the parsed-invocation check (the invocation's path is `pass`) and
        // the token scan (which never saw the alias). The wrapper's expansion
        // is opaque, so the aliased shape fails closed like the literal one.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use core::include as paste;\n\
             macro_rules! pass {{\n\
                 ($($t:tt)*) => {{ $($t)* }};\n\
             }}\n\
             pass!(paste!(\"xtensa.rs\"));\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an aliased include! hidden in a wrapper invocation fails closed: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_macro_rules_body_with_an_unused_alias_keeps_the_exemption() {
        // The alias only matters when it is actually invoked: a `use
        // core::include as paste;` whose `macro_rules!` bodies never call
        // `paste!` is not an inclusion, so the exemption stands — the scan
        // matches the alias only in invocation position (`paste` followed by
        // `!`), and a bare `paste` identifier is not one.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use core::include as paste;\n\
             macro_rules! spin {{\n\
                 () => {{ let paste = 1; let _ = paste; }};\n\
             }}\n\
             spin!();\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
        ];
        assert_eq!(check(&sources), Vec::new());
    }

    #[test]
    fn an_include_alias_imported_from_another_crate_is_not_an_alias() {
        // Codex P2 (review_comment 3997890782): `include_aliases` recorded a
        // rename as an alias for the builtin `include!` whenever the imported
        // leaf was named `include`, without checking the root. `use
        // evil::include as paste;` plus `paste!("other.rs");` was treated as a
        // resolved inclusion of `other.rs` and returned before the
        // external-macro classification — which would have failed closed, since
        // imports resolve `paste` to `(evil, include)`, not the pinned
        // exemption. The hostile macro's expansion could include the gated
        // file, so only aliases rooted at `core`/`std` count as the builtin.
        let root = format!(
            "#![no_std]\n#![no_main]\n#![allow(unsafe_code, reason = \"the reset vector\")]\n\
             pub const PREFIX: &str = \"{PREFIX}\";\n\
             #[cfg(target_arch = \"xtensa\")]\n\
             mod xtensa;\n\
             use evil::include as paste;\n\
             paste!(\"other.rs\");\n"
        );
        let sources = vec![
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/main.rs"),
                contents: root,
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/xtensa.rs"),
                contents: "fn poke() { unsafe { core::ptr::null::<u8>().read() }; }\n".to_owned(),
            },
            LayerSource {
                crate_name: PACKAGE.to_owned(),
                path: format!("crates/{PACKAGE}/src/other.rs"),
                contents: "pub fn other() {}\n".to_owned(),
            },
        ];
        let violations = check(&sources);
        assert_eq!(
            violations.len(),
            1,
            "an include rename from another crate is an unaccounted external macro: {violations:?}"
        );
        assert!(
            violations[0].subject.ends_with("src/xtensa.rs"),
            "{violations:?}"
        );
    }
}
