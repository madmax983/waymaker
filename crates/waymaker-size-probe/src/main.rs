//! Example firmware whose only purpose is to be linked and measured.
//!
//! Design document §04 states the code-flash budget as an *incremental* number: 8 KiB for
//! the kernel plus the flash adapter, on `thumbv6m-none-eabi`, with release-size settings.
//! An incremental number needs two images, so this one is built twice — once with none of
//! the layers linked and once with them — and the budget is the difference. `cargo xtask
//! size` drives that, once per feature combination.
//!
//! # Why there is no runtime here
//!
//! There is no `cortex-m-rt`, no linker script, and no entry symbol. The image is never
//! flashed and never run; it is linked so that its section headers can be read. Leaving
//! out the runtime keeps the baseline at very nearly nothing, which is what makes the
//! delta a measurement of Waymaker rather than of a startup file. The linker says it
//! cannot find `_start` and does not set a start address, which is exactly right for an
//! image that will never be started.
//!
//! # Why there is no `unsafe`
//!
//! A firmware entry point normally needs `#[unsafe(no_mangle)]`, and the workspace denies
//! `unsafe_code`. It is not needed: [`RETAIN`] is a `#[used]` static holding a function
//! pointer, which survives `--gc-sections` and drags the code it points at along with it.
//! So the probe obeys the same `#![forbid(unsafe_code)]` rule as the crates it measures,
//! and there is no exemption for a reviewer to weigh.
//!
//! # Why there is no scratch page
//!
//! The runtime RAM budget is stated "with a 512 B scratch page", and the page is
//! caller-owned: the engine borrows it. A probe that declared one would put the caller's
//! buffer into `.bss` and charge the engine for it. The RAM gate is therefore
//! `waymaker_core::budget::ENGINE_RAM_BYTES` — what is left of the 768 B once the page is
//! accounted for.

#![no_std]
#![no_main]
#![forbid(unsafe_code)]
// The linker reports that it cannot find `_start`. That is the design, not a defect: see
// the module documentation. Allowed here so that a warning nobody can act on does not
// train readers to ignore the size job's output.
#![allow(
    linker_messages,
    reason = "the probe image is linked but never started"
)]

use core::panic::PanicInfo;

/// Required of any `no_std` binary, and never reached: nothing runs this image.
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {}
}

/// Everything the probe wants the linker to keep, behind an opaque barrier.
///
/// [`core::hint::black_box`] is what stops the optimiser from folding the whole thing
/// away and reporting a delta of zero for code that is really there.
fn probe() -> usize {
    let mut kept = core::hint::black_box(0_usize);
    kept = kept.wrapping_add(engine());
    kept = kept.wrapping_add(facade());
    core::hint::black_box(kept)
}

/// The kernel and the flash adapter, when they are linked in.
#[cfg(feature = "engine")]
fn engine() -> usize {
    // Rung 0.1 replaces this with the record codec, the cursor, and a replay step: the
    // shape stays the same, and every function the probe reaches is a function the delta
    // charges for.
    core::hint::black_box(waymaker_core::budget::KERNEL_STATE_TOTAL_BYTES).wrapping_add(
        core::hint::black_box(waymaker_core::budget::INCREMENTAL_CODE_FLASH_BYTES),
    )
}

/// Nothing, in the baseline image that measures a firmware without Waymaker in it.
#[cfg(not(feature = "engine"))]
const fn engine() -> usize {
    0
}

/// The Embassy façade, when it is linked in.
///
/// Subject to the same hazard as [`engine`]: the façade's cost is what this function
/// reaches, and rung 0.4 must add the dispatcher step here for the `facade` row to mean
/// anything.
#[cfg(feature = "facade")]
fn facade() -> usize {
    use waymaker_embassy as _;

    // Rung 0.4: drive one dispatcher step here.
    core::hint::black_box(0)
}

/// Nothing, in an image built without the façade.
#[cfg(not(feature = "facade"))]
const fn facade() -> usize {
    0
}

/// The one symbol the linker is told to keep.
///
/// `#[used]` puts it in `llvm.used`, which the linker treats as a root, so `--gc-sections`
/// keeps it and everything [`probe`] reaches. This is the whole reason the probe needs no
/// entry point and therefore no `unsafe`.
#[used]
static RETAIN: fn() -> usize = probe;
