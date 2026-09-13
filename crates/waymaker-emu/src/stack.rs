//! How much stack one run of the boot used.
//!
//! [`paint`] fills the unused stack with a known byte before the rig runs. [`high_water_mark`]
//! reads back how far that byte was disturbed. This is the third and last reason this crate
//! carries `#![allow(unsafe_code)]`: a raw fill and a raw read, both confined to the region
//! the linker reserves for the stack. See [ADR 0041].
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
//! [ADR 0041]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0041-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md
//! [budgets]: https://github.com/madmax983/waymaker/blob/main/CLAUDE.md#budgets

/// The byte [`paint`] fills unused stack with.
///
/// Not `0x00` or `0xFF`: both are common in real records — an erased NOR cell reads `0xFF`,
/// and a struct's padding is often zeroed — and a run that legitimately wrote either would
/// look untouched. `0xA5` is the byte the same technique uses elsewhere in embedded work,
/// for the same reason: one byte, alternating bits, unlikely to reappear by chance.
const POISON: u8 = 0xA5;

/// Bytes left unpainted just below the caller's marker.
///
/// [`paint`] and [`high_water_mark`] are themselves calls, and a call's own frame sits below
/// its caller's — below the marker `main` takes before either is called. This margin is
/// never painted and never scanned, so neither function can read or write through the
/// pattern the memory it is itself running on. The reported figure can only read *high* for
/// it, never low: a run that used less than this margin still reports at least this many
/// bytes, never fewer than the run actually used.
const GUARD_BYTES: usize = 128;

unsafe extern "C" {
    /// The end of `.bss`: the lowest address this image ever asks the stack to reach.
    ///
    /// A symbol `cortex-m-rt`'s linker script defines, not a Rust `static` this crate owns.
    /// Naming it only takes its address — nothing here reads or writes through it directly,
    /// only through the bounds [`paint`] and [`high_water_mark`] compute from it.
    static __ebss: u8;
}

/// The lowest address this image's stack may reach.
fn stack_floor() -> usize {
    core::ptr::addr_of!(__ebss) as usize
}

/// Fills unused stack with [`POISON`], from the linker's `__ebss` up to `depth_from`.
///
/// `depth_from` must be the address of a local still on the caller's stack, taken before the
/// caller does anything else — so everything below it is unused at the moment of the call.
/// Painting less than the true free region is safe; painting more is not, which is why the
/// caller must take `depth_from` first and hold nothing below it worth keeping.
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
    // SAFETY: `bottom` is the linker's own `__ebss`, the lowest address this image's stack
    // ever reaches. `top` is `depth_from` less a margin this function never touches, and
    // `depth_from` is an address on the caller's stack that nothing has used yet — the
    // caller's obligation, stated above, not this function's. Every byte in `[bottom, top)`
    // is therefore stack this image owns and has not yet used. The fill goes through a
    // volatile write so it cannot be optimised away as dead.
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
/// `depth_from` must be the same address [`paint`] was called with. Scans from `__ebss`
/// upward for the first byte that is no longer [`POISON`]; everything below that byte was
/// never touched, so the run reached no deeper. The scan stops at the same ceiling `paint`
/// stopped filling at — `depth_from` less [`GUARD_BYTES`] — and never reads the guard margin
/// itself: that memory was never painted, so a byte that happened to already read as
/// [`POISON`] there would be indistinguishable from one `paint` wrote, and the figure would
/// under-report rather than over-report. Bounding the scan the same way `paint` bounded the
/// fill closes that off; the figure this returns can only be too high, never too low.
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
    // well inside `u32`. `try_from` only fails on a host build wider than that, where the
    // figure would be `Unmeasurable` regardless.
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
