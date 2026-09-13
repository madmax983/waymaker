//! How much stack one run of the boot used.
//!
//! [`paint`] fills the unused stack with a known byte before the rig runs. [`high_water_mark`]
//! reads back how far that byte was disturbed. This is the third and last reason this crate
//! writes the `unsafe` keyword at all — after the two macro expansions `main.rs` names — and
//! it is a raw fill and a raw read, both confined to the region the linker reserves for the
//! stack. See [ADR 0041].
//!
//! # What this measures, and what it does not
//!
//! It measures how deep this one boot's own call chain reached — the rig, the conformance
//! suite, and everything under them, on this image, on this core. It does not measure the
//! engine alone: `waymaker-rig` and `waymaker-conformance` are linked here too, and their
//! stack use is counted along with the layers'. `CLAUDE.md`'s [budgets] section still gates a
//! narrower, composed figure that excludes call-chain depth — the two numbers answer
//! different questions and neither substitutes for the other.
//!
//! It is also not an exact figure, and this section says so plainly rather than let a doc
//! comment claim otherwise. Painting the stack and reading back a high-water mark is a
//! *lower bound* on real usage, not an exact reading. A frame can reserve bytes it never
//! writes — alignment padding, a buffer only partly filled — and a byte like that still
//! reads as the paint pattern afterwards. So the figure can understate how deep the stack
//! pointer actually went. [`GUARD_BYTES`] still guarantees a *floor*: never painted, so the
//! figure can never read below it. Nothing here turns the technique into a proven ceiling,
//! though. That is a property of stack painting in general, not a defect of this
//! implementation, and no rule in this workspace has ever asked it to be more.
//!
//! [ADR 0041]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0041-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md
//! [budgets]: https://github.com/madmax983/waymaker/blob/main/CLAUDE.md#budgets

/// The byte [`paint`] fills unused stack with.
///
/// Not `0x00` or `0xFF`. A stack holds both often: locals are frequently zeroed, and `0xFF`
/// bytes come from erased media read into a buffer or from a `-1` sentinel. A run that wrote
/// either would look untouched. `0xA5` alternates its bits, so it is unlikely to appear by
/// chance, and embedded stack-painting conventionally uses it.
const POISON: u8 = 0xA5;

/// Bytes left unpainted just below the caller's `depth_from`.
///
/// A margin, not a proof. Fat LTO usually inlines [`paint`] and [`high_water_mark`] into
/// their one caller, so neither reliably owns a frame of its own for this margin to protect.
/// It guards two things this reasoning cannot rule out instead: an exception stacked below
/// the stack pointer while either runs, and a future rebuild that stops inlining them. Never
/// painted and never scanned, so [`high_water_mark`] can never read below it: a run that used
/// fewer bytes than this margin still reports at least this many.
pub const GUARD_BYTES: usize = 128;

unsafe extern "C" {
    /// The end of the stack region: the lowest address this image's stack may reach.
    ///
    /// A symbol `cortex-m-rt`'s linker script defines — `_stack_end`, provided after the
    /// `.uninit` section, is the bottom of the region the stack may use, where `.bss`'s own
    /// end is not: this workspace declares nothing in `.uninit` today, so the two symbols
    /// happen to coincide, but `_stack_end` is the one that stays correct if that changes.
    /// Naming it only takes its address — nothing here reads or writes through it directly,
    /// only through the bounds [`paint`] and [`high_water_mark`] compute from it.
    static _stack_end: u8;
}

/// The lowest address this image's stack may reach.
fn stack_floor() -> usize {
    core::ptr::addr_of!(_stack_end) as usize
}

/// The stack pointer, read from the core rather than inferred from a local's address.
///
/// A local's address depends on where a compiler chooses to place it within its frame, which
/// this workspace's own layering rules do not promise and a future rebuild could change. The
/// stack pointer is the one number that is always exactly where the hardware says it is.
#[must_use]
pub fn current_stack_pointer() -> usize {
    cortex_m::register::msp::read() as usize
}

/// Fills unused stack with [`POISON`], from the linker's `_stack_end` up to `depth_from`.
///
/// `depth_from` must be a stack pointer reading taken before the caller does anything else —
/// [`current_stack_pointer`], called first — so everything below it is unused at the moment
/// of the call. Painting less than the true free region is safe; painting more is not, which
/// is why the caller must read `depth_from` first and use nothing below it afterwards until
/// the run this call is measuring has finished.
///
/// Call this once per boot. A second call would paint over whatever the first call's own
/// caller had already done, and report on the wrong run.
pub fn paint(depth_from: usize) {
    let bottom = stack_floor();
    let Some(top) = depth_from.checked_sub(GUARD_BYTES) else {
        return;
    };
    if top <= bottom {
        return;
    }
    // SAFETY: `bottom` is the linker's own `_stack_end`, the lowest address this image's
    // stack ever reaches. `top` is `depth_from` less a margin this function never touches,
    // and `depth_from` is a stack pointer reading taken before the caller used any of the
    // memory below it — the caller's obligation, stated above, not this function's. Every
    // byte in `[bottom, top)` is therefore stack this image owns and has not yet used. The
    // fill goes through a volatile write so it cannot be optimised away as dead.
    unsafe {
        let mut at = bottom as *mut u8;
        let end = top as *mut u8;
        while (at as usize) < (end as usize) {
            at.write_volatile(POISON);
            at = at.add(1);
        }
    }
}

/// How many bytes below `depth_from` a run since [`paint`] disturbed.
///
/// `depth_from` must be the same reading [`paint`] was called with. Scans from `_stack_end`
/// upward for the first byte that is no longer [`POISON`]; everything below that byte was
/// never touched, so the run reached no deeper. The scan stops at the same ceiling `paint`
/// stopped filling at — `depth_from` less [`GUARD_BYTES`] — and never reads the guard margin
/// itself: that memory was never painted, so a byte that happened to already read as
/// [`POISON`] there would be indistinguishable from one `paint` wrote, and the figure would
/// under-report rather than over-report. Bounding the scan the same way `paint` bounded the
/// fill closes that off.
#[must_use]
pub fn high_water_mark(depth_from: usize) -> u32 {
    let bottom = stack_floor();
    let Some(ceiling) = depth_from.checked_sub(GUARD_BYTES) else {
        return 0;
    };
    if ceiling <= bottom {
        return 0;
    }
    // SAFETY: reads only bytes in `[bottom, ceiling)`, exactly the range `paint` was called
    // with this same `depth_from` to fill. Every byte read here is therefore either still
    // `POISON` or was legitimately written by the run being measured.
    let deepest = unsafe {
        let mut at = bottom as *const u8;
        let end = ceiling as *const u8;
        while (at as usize) < (end as usize) && at.read_volatile() == POISON {
            at = at.add(1);
        }
        at as usize
    };
    used_bytes(bottom, depth_from, deepest)
}

/// The arithmetic [`high_water_mark`] reports, apart from the read that feeds it.
///
/// `deepest` is the first address, scanning up from `bottom`, that is no longer [`POISON`].
/// Every address below it stayed untouched, so the run reached no deeper than `deepest`, and
/// the bytes it used are the rest of the region up to `top`.
fn used_bytes(bottom: usize, top: usize, deepest: usize) -> u32 {
    if top <= bottom || deepest < bottom || deepest > top {
        return 0;
    }
    // `top - deepest` never exceeds `top - bottom`, and both fit in the 16 KiB memory map —
    // well inside `u32`. `try_from` fails only where `usize` is wider than 32 bits, which is
    // a host build; the figure there is `Unmeasurable` in any case.
    u32::try_from(top - deepest).unwrap_or(u32::MAX)
}

/// The size of the region [`paint`] fills and [`high_water_mark`] scans.
#[must_use]
pub fn available_bytes(depth_from: usize) -> u32 {
    let bottom = stack_floor();
    if depth_from <= bottom {
        0
    } else {
        u32::try_from(depth_from - bottom).unwrap_or(u32::MAX)
    }
}
