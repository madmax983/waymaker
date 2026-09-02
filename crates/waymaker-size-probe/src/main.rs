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
///
/// # Why the two halves are `#[inline(never)]`
///
/// So that each row of the size report measures its own layer. With `lto = "fat"` the
/// optimiser is free to inline [`engine`] into this function and then make different
/// choices depending on whether [`facade`] is there to inline beside it — which showed up
/// as a facade image eight bytes *smaller* than the engine image it strictly contains.
/// That is codegen noise rather than a measurement, and at rung 0.1 the façade contributes
/// no code of its own, so noise is all the difference between the two rows was. Keeping
/// both out of line compiles the engine identically in every variant, so a variant that
/// adds a feature adds bytes.
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
#[inline(never)]
fn engine() -> usize {
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

    kept = kept.wrapping_add(record_codec());
    kept = kept.wrapping_add(replay_cursor());

    core::hint::black_box(kept)
}

/// The streaming replay cursor: one run's history walked forwards, record by record.
///
/// Split out of [`engine`] for readability alone: it is called unconditionally from there,
/// so no figure the size report prints changes either way — the report's rows are one per
/// linked feature variant plus the kernel-state registry, never one per probe function.
///
/// Every public function of `waymaker_core::replay` is called here, which is
/// `size-probe-reach`'s requirement. Both halves of `advance` are linked: a legal record
/// and one that halts the cursor, so the incremental delta charges for the refusal that
/// makes recovery stop at a bad record rather than only for the happy path.
///
/// There is no scratch page here on purpose. The cursor borrows nothing, so a probe that
/// declared a page would put the caller's buffer into `.bss` and charge the engine's
/// runtime-RAM row for memory §04 excludes from it.
#[cfg(feature = "engine")]
#[inline(never)]
fn replay_cursor() -> usize {
    use waymaker_core::replay::{Position, ReplayCursor, Step};
    use waymaker_core::{ActivityKind, EffectSeq, RecordRef, RunId};

    let mut cursor = ReplayCursor::new(RunId(core::hint::black_box(3)));
    let mut kept = usize::try_from(cursor.run().0).unwrap_or(0);
    kept = kept.wrapping_add(usize::from(cursor.position().is_terminal()));

    kept = kept.wrapping_add(
        match cursor.advance(RecordRef::RunStarted {
            workflow_kind: core::hint::black_box(1),
            workflow_version: core::hint::black_box(1),
            input: core::hint::black_box(b"in"),
        }) {
            Ok(Step::RunStarted { input, .. }) => input.len(),
            Ok(step) => usize::from(matches!(step, Step::RunCompleted { .. })),
            Err(error) => error.message().len(),
        },
    );
    kept = kept.wrapping_add(
        match cursor.advance(RecordRef::EffectScheduled {
            seq: core::hint::black_box(EffectSeq::FIRST),
            kind: ActivityKind(core::hint::black_box(1)),
            input_len: core::hint::black_box(2),
            input_crc: core::hint::black_box(9),
        }) {
            Ok(Step::EffectScheduled(pending)) => usize::from(pending.kind.0),
            Ok(_) => 0,
            Err(error) => error.message().len(),
        },
    );
    kept = kept.wrapping_add(
        cursor
            .pending()
            .map_or(0, |pending| usize::from(pending.input_len)),
    );
    kept = kept.wrapping_add(
        cursor
            .next_seq()
            .map_or(0, |seq| usize::try_from(seq.0).unwrap_or(0)),
    );
    // Refused: an effect is unresolved, so a fresh identity would abandon the redelivery.
    kept = kept.wrapping_add(match cursor.next_effect_id() {
        Ok(id) => usize::try_from(id.seq.0).unwrap_or(0),
        Err(error) => error.message().len(),
    });
    kept = kept.wrapping_add(
        match cursor.advance(RecordRef::EffectCompleted {
            seq: core::hint::black_box(EffectSeq::FIRST),
            result: core::hint::black_box(b"out"),
        }) {
            Ok(Step::EffectCompleted { result, .. }) => result.len(),
            Ok(_) => 0,
            Err(error) => error.message().len(),
        },
    );
    // The halting path: a record that cannot legally follow, and the sticky refusal after.
    kept = kept.wrapping_add(
        match cursor.advance(RecordRef::EffectFailed {
            seq: core::hint::black_box(EffectSeq::MAX),
            error: core::hint::black_box(b"no"),
        }) {
            Ok(Step::EffectFailed { error, .. }) => error.len(),
            Ok(_) => 0,
            Err(error) => error.message().len(),
        },
    );
    kept = kept.wrapping_add(match cursor.position() {
        Position::Halted(error) => error.message().len(),
        Position::BeforeRun
        | Position::Replaying
        | Position::AwaitingOutcome
        | Position::RunCompleted
        | Position::RunFailed => 0,
    });

    core::hint::black_box(kept)
}

/// The record codec: encode a frame, decode it back, and walk it with the append scan.
///
/// Split out of [`engine`] rather than appended to it because it needs a page to write
/// into, and a `let` array in the middle of that function would read as state rather than
/// as a buffer. Every public function of `waymaker-flash::frame` is called from here —
/// that is `size-probe-reach`'s requirement, and it is also what makes the flash adapter's
/// share of the code-flash delta a real number rather than the cost of naming the crate.
///
/// The page is deliberately not a `static`: the runtime RAM row measures `.data` and
/// `.bss`, and a scratch buffer here is the caller-owned page §04 excludes from the
/// engine's own budget.
#[cfg(feature = "engine")]
#[inline(never)]
fn record_codec() -> usize {
    use waymaker_core::{ActivityKind, EffectSeq, RecordRef};
    use waymaker_flash::frame::{self, Decoded, ProgramAlign, Scan};

    let Some(align) = ProgramAlign::new(core::hint::black_box(4)) else {
        return 0;
    };
    let mut kept = usize::from(align.get());
    kept = kept.wrapping_add(
        usize::try_from(frame::input_digest(core::hint::black_box(b"in"))).unwrap_or(0),
    );
    kept = kept.wrapping_add(usize::from(frame::permits_unknown_record_skip(
        core::hint::black_box(1),
    )));
    kept = kept.wrapping_add(align.round_up(core::hint::black_box(21)).unwrap_or(0));

    let mut page = [0_u8; 64];
    let record = RecordRef::EffectScheduled {
        seq: EffectSeq(core::hint::black_box(1)),
        kind: ActivityKind(core::hint::black_box(2)),
        input_len: core::hint::black_box(3),
        input_crc: core::hint::black_box(4),
    };
    kept = kept.wrapping_add(match frame::encoded_len(&record, align) {
        Ok(len) => len,
        Err(error) => error.message().len(),
    });
    let written = match frame::encode(&record, align, &mut page) {
        Ok(written) => written,
        Err(error) => return kept.wrapping_add(error.message().len()),
    };
    kept = kept.wrapping_add(written);

    // Both arms of the decode: a record and a frame this firmware cannot interpret. The
    // second is the forward-compatibility branch, and a delta that only charged for the
    // first would understate every future firmware that meets an unknown kind.
    kept = kept.wrapping_add(
        match frame::decode(page.get(..written).unwrap_or_default()) {
            Ok(frame) => match frame.decoded {
                Decoded::Record(decoded) => usize::from(decoded.kind().0)
                    .wrapping_add(frame.frame_len)
                    .wrapping_add(usize::from(frame.format_version)),
                Decoded::UnknownKind(kind) => usize::from(kind.0),
            },
            Err(error) => error.message().len(),
        },
    );

    let mut scan = Scan::new(&page, align);
    for step in &mut scan {
        kept = kept.wrapping_add(match step {
            Ok(record) => usize::from(record.kind().0),
            Err(error) => error.message().len(),
        });
    }
    // One more step after the walk. `Scan` is fused, so this is the path a caller takes
    // when it keeps asking after history has ended — and it is also the call
    // `size-probe-reach` reads, because a `for` loop names no method for a scanner to
    // find.
    kept = kept.wrapping_add(match scan.next() {
        Some(Ok(record)) => usize::from(record.kind().0),
        Some(Err(error)) => error.message().len(),
        None => 0,
    });
    kept = kept.wrapping_add(scan.offset());

    core::hint::black_box(kept)
}

/// Nothing, in the baseline image that measures a firmware without Waymaker in it.
#[cfg(not(feature = "engine"))]
#[inline(never)]
fn record_codec() -> usize {
    core::hint::black_box(0)
}

/// Nothing, in the baseline image that measures a firmware without Waymaker in it.
#[cfg(not(feature = "engine"))]
#[inline(never)]
fn engine() -> usize {
    core::hint::black_box(0)
}

/// The Embassy façade, when it is linked in.
///
/// Subject to the same hazard as [`engine`]: the façade's cost is what this function
/// reaches, and rung 0.4 must add the dispatcher step here for the `facade` row to mean
/// anything.
#[cfg(feature = "facade")]
#[inline(never)]
fn facade() -> usize {
    use waymaker_embassy as _;

    // Rung 0.4: drive one dispatcher step here.
    core::hint::black_box(0)
}

/// Nothing, in an image built without the façade.
#[cfg(not(feature = "facade"))]
#[inline(never)]
fn facade() -> usize {
    core::hint::black_box(0)
}

/// The one symbol the linker is told to keep.
///
/// `#[used]` puts it in `llvm.used`, which the linker treats as a root, so `--gc-sections`
/// keeps it and everything [`probe`] reaches. This is the whole reason the probe needs no
/// entry point and therefore no `unsafe`.
#[used]
static RETAIN: fn() -> usize = probe;
