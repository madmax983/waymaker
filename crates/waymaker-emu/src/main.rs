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
//! map — started on two QEMU machines, and required to run the rig's three moments and say
//! so. `cargo xtask emulate` is the harness; [`crate::boot`] is what runs.
//!
//! # Why two machines
//!
//! `-machine microbit` is an nRF51822, whose core is a Cortex-M0: **ARMv6-M**, the
//! architecture `thumbv6m-none-eabi` targets and the one design document §04's budgets are
//! stated for. `-machine mps2-an386` is a Cortex-M4: **ARMv7E-M**, which is the second of the
//! two cores `docs::HARDWARE_TARGETS` names. Between them the rig's code is executed on both
//! instruction sets it is written for, which one machine could not do — a rig that had only
//! ever run on one encoding has measured that encoding.
//!
//! # What it is not
//!
//! A board run. There is no NOR part in either machine, no supply to remove, no reset-cause
//! register, no backup domain and no retained-RAM question, so the two power-cut rows and the
//! RTC row of `docs::HARDWARE_TARGETS` stay `Not run` and this image may not be cited to move
//! them. The Cortex-M0 is also not a Cortex-M0+: the architecture is the same and the core is
//! not. [ADR 0040] argues all of that rather than leaving a green check to imply otherwise.
//!
//! # Why there is `unsafe` here, and nowhere else
//!
//! Three reasons, and no more. `#[cortex_m_rt::entry]` expands to the exported symbol the
//! reset vector points at, and `cortex_m_semihosting::debug::exit` is how a guest tells QEMU
//! what to exit with — neither can be written without the attribute the workspace denies.
//! [`crate::stack`] is the third: reading how far a run disturbed a painted stack needs a raw
//! fill and a raw read, named by [ADR 0041]. The workspace manifest names this exact escape —
//! *"`deny` keeps a documented exception a reviewable one-line `#![allow(unsafe_code)]` plus
//! an ADR"* — and this is the one crate that takes it. It is a crate nothing depends on, that
//! is never published, and that no layer, test-support crate or firmware image links.
//!
//! [ADR 0040]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md
//! [ADR 0041]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0041-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md

#![no_std]
#![no_main]
#![warn(missing_docs)]
// The one exception in the workspace, argued in the module documentation above and in
// ADR 0040 and ADR 0041. `allow` rather than the `forbid` every other crate carries, because
// a reset vector, a semihosting exit and a stack high-water mark cannot be spelled without
// it — and scoped to a crate nothing depends on and no image links.
#![allow(
    unsafe_code,
    reason = "the reset vector, the semihosting exit, and the stack high-water mark; see ADR 0040 and ADR 0041"
)]

pub mod boot;
pub mod nor;
pub mod stack;

use core::panic::PanicInfo;

use cortex_m_rt::entry;
use cortex_m_semihosting::{debug, hprintln};
use waymaker_rig::run::Rig;

use crate::boot::{Census, Trouble};
use crate::nor::Nor;

/// The prefix every line this image writes carries.
///
/// Read by `cargo xtask emulate` rather than only by a person: a boot that started, exited
/// zero and printed nothing is a boot that measured nothing, and the harness fails a run in
/// which the lines below are absent. Kept in one constant so the image and the harness cannot
/// drift apart over a space.
pub const PREFIX: &str = "waymaker-emu:";

#[entry]
fn main() -> ! {
    // Taken before anything else, so that every byte below it is unused stack at the moment
    // `stack::paint` is called. `measured_run` is `#[inline(never)]` for the same reason:
    // its locals — `part` and `page` among them — must sit in a frame of their own, below
    // this one, and never share this function's frame with `marker`.
    let marker = 0_u8;
    let depth_from = core::ptr::addr_of!(marker) as usize;
    let available = stack::available_bytes(depth_from);
    if available == 0 {
        hprintln!(
            "{} failed the stack region between the linker's `__ebss` and this boot's own marker is empty; there is nothing to paint or measure",
            PREFIX
        );
        debug::exit(debug::EXIT_FAILURE);
        halt();
    }

    stack::paint(depth_from);
    let outcome = measured_run();
    let used = stack::high_water_mark(depth_from);

    match outcome {
        Ok(census) => {
            report(&census, used, available);
            hprintln!("{} ok", PREFIX);
            debug::exit(debug::EXIT_SUCCESS);
        }
        Err(trouble) => {
            hprintln!("{} failed {}", PREFIX, trouble.message());
            if let Trouble::Breach(outcome) = trouble {
                hprintln!("{} breach code={}", PREFIX, outcome.code());
            }
            debug::exit(debug::EXIT_FAILURE);
        }
    }

    // `debug::exit` does not return under QEMU. Reached only if this image is ever started
    // somewhere semihosting is not enabled, where hanging is the honest thing to do: exiting
    // zero would report a pass for a run whose result nobody could read.
    halt()
}

/// Runs the boot's own locals in a frame of their own.
///
/// Both locals, not statics: `cortex-m-rt` puts the stack at the top of the 16 KiB the memory
/// map declares, and a `static` would need interior mutability this crate has no way to
/// spell without more of the exception it already carries. Kept out of `main`'s own frame,
/// and marked so the compiler cannot fold it back in: `stack::paint` must not reach memory
/// `main` still holds, and this function's frame is where `part` and `page` live instead.
#[inline(never)]
fn measured_run() -> Result<Census, Trouble> {
    let mut part = Nor::new();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    boot::run(&mut part, &mut page)
}

/// Writes the census as three lines the harness parses and a person can read.
fn report(census: &Census, stack_used: u32, stack_available: u32) {
    hprintln!(
        "{} cases passed={} exempt={}",
        PREFIX,
        census.cases_passed,
        census.cases_exempt
    );
    hprintln!(
        "{} rig iterations={} cuts={} resumes={} unextendable={} redeliveries={} verdicts={} dispatched={}",
        PREFIX,
        census.iterations,
        census.cuts,
        census.resumes,
        census.unextendable,
        census.redeliveries,
        census.verdicts_passed,
        census.dispatched
    );
    hprintln!(
        "{} stack used={} available={}",
        PREFIX,
        stack_used,
        stack_available
    );
}

/// Exits non-zero, so that a panic anywhere in the rig fails the stage rather than hanging it.
///
/// The default `cortex-m-rt` handler loops, which under a harness with a timeout is a
/// *timeout* rather than a failure — the same result for a rig that panicked and a rig that
/// took too long, which is one distinction too few for a gate.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    hprintln!("{} panicked: {}", PREFIX, info);
    debug::exit(debug::EXIT_FAILURE);
    halt()
}

/// Stops, for the two callers above that are only reached where `debug::exit` did not exit.
///
/// A spinning hint rather than a bare `loop {}`, which is what the lint asks for and what a
/// core would rather be told: this is reached only outside a semihosting host, where there is
/// nothing left to do and nothing left to say.
fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
