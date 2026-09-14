//! The ARM startup: reset vector, semihosting console, semihosting exit, stack painting.
//!
//! `cortex-m-rt` supplies the vector table and the entry symbol. Most of the `unsafe` this
//! module relies on is macro expansion — `#[entry]` and the semihosting exit — rather than
//! hand-written code, which is the first half of the workspace's `unsafe_code` exception the
//! `emulation-boot` rule checks for. [`crate::stack`] is the second half, ARM-only: painting
//! and reading back the unused stack needs a raw fill and a raw read, named by [ADR 0045].
//!
//! [ADR 0045]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0045-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md

use cortex_m_rt::entry;
use cortex_m_semihosting::{debug, hprintln};
use waymaker_rig::run::Rig;

use crate::boot::{Census, Trouble};
use crate::nor::Nor;
use crate::stack;
use crate::{PREFIX, halt};

/// Runs the rig, reports the census and stack usage, and exits through semihosting.
#[entry]
fn main() -> ! {
    // Read before anything else. Every byte below this reading is unused stack at the
    // moment `stack::paint` is called. `measured_run` is `#[inline(never)]` for the same
    // reason: its locals, `part` and `page` among them, must sit in a frame of their own,
    // below this one, and must never share this frame with anything read here.
    let depth_from = stack::current_stack_pointer();
    let headroom = stack::available_bytes(depth_from);
    if headroom <= u32::try_from(stack::GUARD_BYTES).unwrap_or(u32::MAX) {
        hprintln!(
            "{} failed the stack region between the linker's `_stack_end` and the current stack pointer leaves no room to paint or measure",
            PREFIX
        );
        debug::exit(debug::EXIT_FAILURE);
        halt();
    }

    let resolved = stack::paint(depth_from);
    let outcome = measured_run();
    // Both figures come from this one call, against `resolved` — the bound `paint` actually
    // used — rather than two separate calls each re-deriving their own live reading. Two
    // calls could disagree by the few bytes each one's own frame costs, which could report
    // `used` short of `available` even where a run disturbed every byte `paint` painted.
    let (used, available) = stack::high_water_mark(resolved);

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
/// Both locals, not statics. `cortex-m-rt` puts the stack at the top of the 16 KiB the memory
/// map declares, and a `static` would need interior mutability this crate has no way to spell
/// without more of the exception it already carries. This function is kept out of `main`'s
/// own frame, and marked so the compiler cannot fold it back in: `stack::paint` must not reach
/// memory `main` still holds. `part` and `page` live in this frame instead, below the stack
/// pointer `main` read before calling it.
#[inline(never)]
fn measured_run() -> Result<Census, Trouble> {
    let mut part = Nor::new();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    crate::boot::run(&mut part, &mut page)
}

/// Writes the census and stack usage as three lines the harness parses and a person can read.
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
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    hprintln!("{} panicked: {}", PREFIX, info);
    debug::exit(debug::EXIT_FAILURE);
    halt()
}
