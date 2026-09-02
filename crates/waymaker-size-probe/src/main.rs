//! Example firmware whose only purpose is to be linked and measured.
//!
//! Design document §04 states the code-flash budget as an *incremental* number: 8 KiB for
//! the kernel plus the flash adapter, on `thumbv6m-none-eabi`, with release-size settings.
//! An incremental number needs two images, so this one is built twice — once with none of
//! the layers linked and once with them — and the budget is the difference. `cargo xtask
//! size` drives that, once per feature.
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
//! buffer into `.bss` and charge the engine for it. The statics gate is therefore
//! `waymaker_core::budget::ENGINE_RAM_BYTES` — what is left of the 768 B once the page is
//! accounted for. It is a floor on §04's runtime RAM and not the rule itself: section
//! sizes cannot see a cursor or a context that lives on the caller's stack.

#![no_std]
#![no_main]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
// The linker reports that it cannot find `_start`. That is the design, not a defect: see
// the module documentation. Allowed here so that a warning nobody can act on does not
// train readers to ignore the size job's output.
#![allow(
    linker_messages,
    reason = "the probe image is linked but never started"
)]

use core::panic::PanicInfo;

/// Required of any `no_std` binary, and never reached: nothing runs this image.
///
/// `const` because the workspace's nursery lints ask for it and there is no reason to
/// refuse: a diverging `loop {}` is a legal constant evaluation, and `#[panic_handler]`
/// only requires the signature, not the constness.
#[panic_handler]
const fn panic(_info: &PanicInfo<'_>) -> ! {
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

#[cfg(feature = "engine")]
waymaker_core::activity_kinds! {
    pub(crate) PROBE_ACTIVITIES {
        /// Stands in for a real activity: the probe never dispatches anything.
        DOWNLOAD = 1,
        /// A second kind, so the distinctness check has a pair to compare.
        VERIFY_SIGNATURE = 2,
    }
}

/// The kernel and the flash adapter, when they are linked in.
///
/// # What a delta can and cannot charge for
///
/// Only for code the linker keeps, and with `lto = "fat"` and `--gc-sections` the linker
/// keeps what this function reaches. Enabling the optional dependency is not enough, and
/// neither is naming the crate: a public function nothing calls is discarded, and the row
/// reports the same twenty-odd bytes of arithmetic below while the real firmware grows.
///
/// That is not left to memory. `cargo xtask check-layering` fails on any public function
/// of a layer this file does not call, and names it. The calls below are that rule's
/// answer, not decoration.
///
/// # Why the results are folded together
///
/// Every value each call produces is folded into the returned `usize` and pushed through
/// [`core::hint::black_box`], so the optimiser cannot decide the whole function is dead and
/// report a delta of zero for code that is really there. `wrapping_add` because the sum is
/// a keep-alive rather than a quantity, and `usize::try_from(..).unwrap_or(0)` because a
/// `u32` sequence and a `u64` run id are both wider than a pointer on `thumbv6m-none-eabi`.
#[cfg(feature = "engine")]
fn engine() -> usize {
    // `use ... as _` links the crate without naming an item, which is all there is to name
    // in it until rung 0.2. It is what makes this row genuinely "core plus the flash
    // adapter" rather than core alone.
    use waymaker_flash as _;

    use waymaker_core::{
        ActivityName, DecodeError, EffectIdAllocator, EffectSeq, KernelError, RecordKind,
        RecordRef, RunId,
    };

    let registered = waymaker_core::budget::TypeSize::of::<u32>("u32");
    let mut kept = core::hint::black_box(registered.size)
        .wrapping_add(core::hint::black_box(
            waymaker_core::budget::KERNEL_STATE_TOTAL_BYTES,
        ))
        .wrapping_add(core::hint::black_box(
            waymaker_core::budget::INCREMENTAL_CODE_FLASH_BYTES,
        ));

    // Effect identity: a fresh run issues, and a resumed one at the ceiling refuses. Both
    // paths are linked, so the delta charges for the branch that makes exhaustion terminal
    // rather than only for the happy one.
    let mut fresh = EffectIdAllocator::for_run(RunId(core::hint::black_box(1)));
    kept = kept.wrapping_add(match fresh.allocate() {
        Ok(id) => usize::try_from(id.seq.0)
            .unwrap_or(0)
            .wrapping_add(usize::try_from(id.run.0).unwrap_or(0)),
        Err(error) => error.message().len(),
    });
    kept = kept.wrapping_add(
        fresh
            .peek()
            .map_or(0, |seq| usize::try_from(seq.0).unwrap_or(0)),
    );
    kept = kept.wrapping_add(usize::try_from(fresh.run().0).unwrap_or(0));

    let mut spent = EffectIdAllocator::resume(
        RunId(core::hint::black_box(2)),
        Some(core::hint::black_box(EffectSeq::MAX)),
    );
    kept = kept.wrapping_add(match spent.allocate() {
        Ok(id) => usize::try_from(id.seq.0).unwrap_or(0),
        Err(error) => error.message().len(),
    });
    kept = kept.wrapping_add(
        core::hint::black_box(EffectSeq::FIRST)
            .successor()
            .map_or(0, |seq| usize::try_from(seq.0).unwrap_or(0)),
    );

    // Activity kinds: the rodata table, the linear lookup, and the distinctness check the
    // macro also asserts at compile time.
    kept = kept.wrapping_add(
        ActivityName::lookup(PROBE_ACTIVITIES, core::hint::black_box(DOWNLOAD)).map_or(0, str::len),
    );
    kept = kept.wrapping_add(usize::from(ActivityName::kinds_are_distinct(
        core::hint::black_box(PROBE_ACTIVITIES),
    )));
    kept = kept.wrapping_add(usize::from(core::hint::black_box(VERIFY_SIGNATURE).0));

    // The error vocabulary: both `message` bodies, and the conversion that carries a decode
    // failure across the seam with `?`.
    kept = kept.wrapping_add(
        core::hint::black_box(DecodeError::IntegrityFailed)
            .message()
            .len(),
    );
    kept = kept.wrapping_add(
        KernelError::from(core::hint::black_box(DecodeError::Truncated))
            .message()
            .len(),
    );

    // The borrowed record view: the kind mapping the encoder reads, over a record whose
    // payload the optimiser cannot see through.
    let completed = RecordRef::RunCompleted {
        result: core::hint::black_box(b"done"),
    };
    kept = kept.wrapping_add(usize::from(completed.kind().0));
    kept = kept.wrapping_add(usize::from(
        core::hint::black_box(RecordKind::EFFECT_SCHEDULED).0,
    ));

    // `Display` is a trait impl, so `size-probe-reach` counts its `fmt`. Retained as a
    // function pointer rather than by formatting something: taking the pointer keeps the
    // impl body for measurement, while an actual `{}` would link `core::fmt::write` and
    // charge this row for machinery the impls were written to avoid.
    let show_kernel: fn(&KernelError, &mut core::fmt::Formatter<'_>) -> core::fmt::Result =
        <KernelError as core::fmt::Display>::fmt;
    let show_decode: fn(&DecodeError, &mut core::fmt::Formatter<'_>) -> core::fmt::Result =
        <DecodeError as core::fmt::Display>::fmt;
    core::hint::black_box(show_kernel);
    core::hint::black_box(show_decode);

    core::hint::black_box(kept)
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
