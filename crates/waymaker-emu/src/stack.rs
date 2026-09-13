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
/// It guards three things this reasoning cannot rule out instead: an exception stacked below
/// the stack pointer while either runs, a future rebuild that stops inlining them, and the
/// two functions' own call frames costing slightly different amounts — [`clamp_to_stack_region`]
/// reads a fresh stack pointer inside each, so [`high_water_mark`]'s live reading can sit a
/// handful of bytes above [`paint`]'s own without this margin covering the gap. Never painted
/// and never scanned, so [`high_water_mark`] can never read below it: a run that used fewer
/// bytes than this margin still reports at least this many.
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
    /// The top of the stack region: the highest address this image's stack may reach.
    ///
    /// The other linker-provided bound, named so [`clamp_to_stack_region`] can hold a caller's
    /// reading to memory this image actually owns regardless of what the reading says. Naming
    /// it only takes its address, exactly as `_stack_end` above.
    static _stack_start: u8;
}

/// The lowest address this image's stack may reach.
fn stack_floor() -> usize {
    core::ptr::addr_of!(_stack_end) as usize
}

/// The highest address this image's stack may reach.
fn stack_ceiling() -> usize {
    core::ptr::addr_of!(_stack_start) as usize
}

/// Holds `depth_from` to `[stack_floor(), stack_ceiling()]`, then to the stack pointer's
/// live reading.
///
/// [`paint`] and [`high_water_mark`] are `pub fn`, not `unsafe fn`: a safe function must stay
/// sound for every input, not only the one reading `main` actually passes. The region clamp
/// alone is not enough — a stale `depth_from` that is still a legal *stack* address, or
/// `usize::MAX` clamped down to `_stack_start`, both pass it, and either would let [`paint`]
/// fill memory the stack is genuinely using right now, above the true stack pointer. So this
/// also takes the lower of the region-clamped value and a *fresh* [`current_stack_pointer`]
/// reading, taken at the moment either function is called rather than trusted from the
/// argument. A caller's `depth_from` can therefore only ever narrow what gets painted or
/// scanned, never widen it past where the stack pointer genuinely is. The cost of a wrong
/// reading is a wrong *measurement*, never an out-of-bounds access.
fn clamp_to_stack_region(depth_from: usize) -> usize {
    depth_from
        .clamp(stack_floor(), stack_ceiling())
        .min(current_stack_pointer())
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
/// Returns the resolved bound this call actually used.
///
/// `depth_from` should be a stack pointer reading taken before the caller does anything else
/// — [`current_stack_pointer`], called first — so everything below it is unused at the
/// moment of the call. Painting less than the true free region is safe; painting more is
/// not, which is why the caller should read `depth_from` first and use nothing below it
/// afterwards until the run this call is measuring has finished. A wrong or stale reading
/// still cannot reach memory this image does not own, or memory the stack is genuinely using
/// right now: [`clamp_to_stack_region`] holds `depth_from` inside the stack region first, then
/// takes the lower of that and a fresh stack pointer reading of its own, so the cost of a
/// wrong reading is a wrong measurement, never an out-of-bounds write.
///
/// The returned bound is what the caller should pass to [`high_water_mark`] and to a later
/// [`available_bytes`] call, rather than the original `depth_from` reading. Each of those two
/// would otherwise re-derive its own live stack pointer independently, at a different point
/// in the boot, and the two readings can differ enough to report `used` a few bytes short of
/// `available` even when a run truly disturbed every painted byte — which would defeat
/// `StackUsage::shortfall`'s fail-closed check on exactly the case it exists to catch. Reusing
/// the one bound this call already resolved is what keeps the two figures comparable.
///
/// Call this once per boot. A second call would paint over whatever the first call's own
/// caller had already done, and report on the wrong run.
#[must_use]
pub fn paint(depth_from: usize) -> usize {
    let depth_from = clamp_to_stack_region(depth_from);
    let bottom = stack_floor();
    let Some(top) = depth_from.checked_sub(GUARD_BYTES) else {
        return depth_from;
    };
    if top <= bottom {
        return depth_from;
    }
    // SAFETY: `bottom` is the linker's own `_stack_end`, the lowest address this image's
    // stack ever reaches. `top` is `depth_from` less a margin this function never touches,
    // and `clamp_to_stack_region` has already held `depth_from` to no more than the lower of
    // `_stack_start` and a stack pointer reading it took itself, moments before this loop
    // runs — real memory this image owns, and never above where the stack genuinely is right
    // now, whatever the caller passed in. Every byte in `[bottom, top)` is therefore stack
    // this image owns and is not currently using. The fill goes through a volatile write so
    // it cannot be optimised away as dead.
    unsafe {
        let mut at = bottom as *mut u8;
        let end = top as *mut u8;
        while (at as usize) < (end as usize) {
            at.write_volatile(POISON);
            at = at.add(1);
        }
    }
    depth_from
}

/// How many bytes below `depth_from` a run since [`paint`] disturbed.
///
/// `depth_from` must be the bound [`paint`] returned, not the original stack pointer reading
/// — passing the same already-resolved value is what keeps this call's clamp a no-op rather
/// than an independent live reading of its own, which is what makes `used` comparable to a
/// later [`available_bytes`] call over the same bound. Scans from `_stack_end` upward for the
/// first byte that is no longer [`POISON`]; everything below that byte was never touched, so
/// the run reached no deeper. The scan stops at the same ceiling `paint` stopped filling at —
/// `depth_from` less [`GUARD_BYTES`] — and never reads the guard margin itself: that memory
/// was never painted, so a byte that happened to already read as [`POISON`] there would be
/// indistinguishable from one `paint` wrote, and the figure would under-report rather than
/// over-report. Bounding the scan the same way `paint` bounded the fill closes that off.
#[must_use]
pub fn high_water_mark(depth_from: usize) -> u32 {
    let depth_from = clamp_to_stack_region(depth_from);
    let bottom = stack_floor();
    let Some(ceiling) = depth_from.checked_sub(GUARD_BYTES) else {
        return 0;
    };
    if ceiling <= bottom {
        return 0;
    }
    // SAFETY: `clamp_to_stack_region` holds `depth_from` to no more than the stack pointer's
    // own live reading, taken at this call, so `[bottom, ceiling)` is real memory this image
    // owns and is at or below where the stack genuinely is right now — true regardless of what
    // the caller passed in. Every byte in it is therefore either still `POISON` — untouched by
    // the run — or was legitimately written by `paint`: the caller passes the bound `paint`
    // itself returned, so this call's own clamp is a no-op in the ordinary case, and even where
    // it is not, `GUARD_BYTES` is wide enough to absorb the few bytes this call's own frame and
    // `paint`'s own frame can differ by.
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
///
/// Called twice in the one real boot: once on the original stack pointer reading, before
/// [`paint`] runs, to decide whether there is room to attempt a measurement at all; and once
/// more on the bound [`paint`] returned, after the run, so the figure this second call reports
/// is measured against the same bound [`high_water_mark`] used rather than an earlier,
/// independent live reading that a genuinely exhausted run could otherwise slip past.
#[must_use]
pub fn available_bytes(depth_from: usize) -> u32 {
    let depth_from = clamp_to_stack_region(depth_from);
    let bottom = stack_floor();
    if depth_from <= bottom {
        0
    } else {
        u32::try_from(depth_from - bottom).unwrap_or(u32::MAX)
    }
}
