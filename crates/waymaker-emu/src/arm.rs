//! The ARM startup: reset vector, semihosting console, semihosting exit.
//!
//! `cortex-m-rt` supplies the vector table and the entry symbol. The `unsafe` this module
//! relies on is macro expansion — `#[entry]` and the semihosting exit — rather than
//! hand-written code, which is the half of the workspace's `unsafe_code` exception the
//! `emulation-boot` rule checks for.

use cortex_m_rt::entry;
use cortex_m_semihosting::{debug, hprintln};
use waymaker_rig::run::Rig;

use crate::boot::{Census, Trouble};
use crate::nor::Nor;
use crate::{PREFIX, halt};

/// Runs the rig, reports the census, and exits through semihosting.
#[entry]
fn main() -> ! {
    // Both locals, not statics: `cortex-m-rt` puts the stack at the top of the 16 KiB the
    // memory map declares, and a `static` would need interior mutability this crate has no
    // way to spell without more of the exception it already carries.
    let mut part = Nor::new();
    let mut page = [0_u8; Rig::PAGE_BYTES];

    match crate::boot::run(&mut part, &mut page) {
        Ok(census) => {
            report(&census);
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

/// Writes the census as two lines the harness parses and a person can read.
fn report(census: &Census) {
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
