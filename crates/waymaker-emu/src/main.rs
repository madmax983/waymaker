//! Firmware that starts under an emulator, runs the rig, and says what it did.
//!
//! # What this crate is for
//!
//! The `rig-firmware` stage builds `waymaker-rig` for `thumbv6m-none-eabi` with `--lib`, and
//! `CLAUDE.md` says exactly what that does and does not buy: *"`cargo build --lib` produces
//! an rlib and never links, so no global allocator is required and an `extern crate alloc`
//! under any of them compiles clean."* A crate that compiles for a target is not a crate that
//! runs on one. Nothing in this repository had ever executed a Waymaker instruction outside
//! an x86 test binary.
//!
//! This image closes that. It is linked — with a reset vector, a vector table and a memory
//! map — for three QEMU machines, started on whichever of them the harness selects, and
//! required to run the rig's three moments and say so. `cargo xtask emulate` is the
//! harness; [`crate::boot`] is what runs.
//!
//! # Why three machines
//!
//! `-machine microbit` is an nRF51822, whose core is a Cortex-M0: **ARMv6-M**, the
//! architecture `thumbv6m-none-eabi` targets and the one design document §04's budgets are
//! stated for. `-machine mps2-an386` is a Cortex-M4: **ARMv7E-M**, which is the second of the
//! two cores `docs::HARDWARE_TARGETS` names. `-machine esp32s3` is an ESP32-S3, whose core is
//! an Xtensa LX7: a third encoding from a different vendor lineage. Between them the rig's
//! code is executed on every instruction set it is written for, which one machine could not
//! do — a rig that had only ever run on one encoding has measured that encoding — and two
//! ARM cores agreeing is a weaker statement than three cores from two families agreeing.
//!
//! # What it is not
//!
//! A board run. There is no NOR part in any of the three machines, no supply to remove, no
//! reset-cause register, no backup domain and no retained-RAM question, so the two power-cut
//! rows and the RTC row of `docs::HARDWARE_TARGETS` stay `Not run` and this image may not be
//! cited to move them. The Cortex-M0 is also not a Cortex-M0+: the architecture is the same
//! and the core is not. [ADR 0040] argues all of that rather than leaving a green check to
//! imply otherwise.
//!
//! # Why there is `unsafe` here, and nowhere else
//!
//! On ARM, `#[cortex_m_rt::entry]` expands to the exported symbol the reset vector points at,
//! and `cortex_m_semihosting::debug::exit` is how a guest tells QEMU what to exit with.
//! Neither can be written without the attribute the workspace denies. On Xtensa there is no
//! `cortex-m-rt` to expand: the startup in `xtensa` — installing the stack pointer, zeroing
//! `.bss`, poking the UART registers — is hand-written, and hand-written it must be. The
//! workspace manifest names this exact escape — *"`deny` keeps a documented exception a
//! reviewable one-line `#![allow(unsafe_code)]` plus an ADR"* — and this is the one crate
//! that takes it. It is a crate nothing depends on, that is never published, and that no
//! layer, test-support crate or firmware image links.
//!
//! [ADR 0040]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md

#![no_std]
#![no_main]
#![warn(missing_docs)]
// The Xtensa startup is hand-written assembly — a naked entry point installing the
// stack — the compiler only knows for that architecture under this feature. Gated,
// so no other target ever sees the attribute: on stable it would be a hard error.
#![cfg_attr(target_arch = "xtensa", feature(asm_experimental_arch))]
// The one exception in the workspace, argued in the module documentation above and in
// ADR 0040. `allow` rather than the `forbid` every other crate carries, because a reset
// vector and a semihosting exit cannot be spelled without it — and scoped to a crate nothing
// depends on and no image links. The Xtensa half of the exception is hand-written rather
// than macro-expanded, and lives behind the `#[cfg(target_arch = "xtensa")]` gate on the
// module below, so no ARM image can contain it.
#![allow(
    unsafe_code,
    reason = "the reset vector and the semihosting exit on ARM; the Xtensa startup's stack install, .bss zeroing and UART MMIO, gated to that target; see ADR 0040"
)]

pub mod boot;
pub mod nor;

// One startup per target family. The ARM half links against `cortex-m-rt`; the Xtensa half
// spells out what that crate expands, because no equivalent exists for Xtensa. `boot` and
// `nor` stay shared and target-independent: the rig the harness measures is the same rig on
// all three machines.
#[cfg(target_arch = "arm")]
mod arm;
#[cfg(target_arch = "xtensa")]
mod xtensa;

/// The prefix every line this image writes carries.
///
/// Read by `cargo xtask emulate` rather than only by a person: a boot that started, exited
/// zero and printed nothing is a boot that measured nothing, and the harness fails a run in
/// which the lines below are absent. Kept in one constant so the image and the harness cannot
/// drift apart over a space.
pub const PREFIX: &str = "waymaker-emu:";

/// Parks the core, for the callers that are only reached where the exit did not exit.
///
/// A spinning hint rather than a bare `loop {}`, which is what the lint asks for and what a
/// core would rather be told. Reached only where there is nothing left to do and nothing
/// left to say: outside a semihosting host on ARM, after the census — drained — on Xtensa.
pub(crate) fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
