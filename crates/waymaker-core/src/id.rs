//! Effect identity: which run, and which effect within it.
//!
//! Design document §07. An effect is identified on a device by the pair `(RunId,
//! EffectSeq)`, and only the sequence is repeated in every record — the run id lives once,
//! in the bank header. The types here are newtypes rather than a `u64` and a `u32` so that
//! a run cannot be passed where a sequence is expected, and their fields are public so
//! that the wire encoder in `waymaker-flash` can reach the integer without the kernel
//! growing an accessor for it.
//!
//! # What this module owns
//!
//! The identity types, their pinned layout, and the single allocator that mints
//! sequences. [`EffectIdAllocator`] is the only thing in the kernel that produces an
//! [`EffectId`], which is what makes "every sequence is issued at most once per run" a
//! property of one type rather than a discipline spread over call sites.
//!
//! # What this module must not own
//!
//! Encoding. Nothing here writes a byte: the in-memory layout below is free to be whatever
//! the target prefers precisely because `waymaker-flash` writes `effect_seq` field by
//! field. It also owns no clock, no storage and no allocation — see the crate
//! documentation.
//!
//! # The boundary this module exists to defend
//!
//! A 32-bit sequence space is large, not infinite, and the failure at the end of it is
//! silent: `u32::MAX + 1` is `0` in a release build, so the next effect would claim the
//! identity of the run's first effect and replay would resolve it against a recorded
//! result belonging to something else. §07 settles that as terminal: "Wraparound is a
//! terminal `IdExhausted` condition, not silent reuse." Here that is a fact about the
//! representation — an exhausted allocator holds [`None`], and no code path puts a value
//! back — rather than a comment saying it will not happen. See
//! [ADR 0006](https://github.com/madmax983/waymaker/blob/main/docs/adr/0006-effect-identity-is-newtypes-and-exhaustion-is-terminal.md).

use crate::error::KernelError;

/// A run of a workflow, unique on a device.
///
/// Stored once per run, in the bank header, rather than in every record: design document
/// §07. The field is public because a wire encoder needs the integer and a `pub fn`
/// accessor is public surface with an enforced cost — every one of them has to be reached
/// by the size probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct RunId(pub u64);

/// The position of an effect within its run's history, counting from zero.
///
/// # Invariants
///
/// Within one run, sequences are issued in strictly increasing order, each at most once,
/// and they never reset. Both facts are properties of [`EffectIdAllocator`], which is the
/// only thing that issues them; this type is the value, and the only arithmetic it offers
/// is [`successor`](Self::successor), which cannot wrap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct EffectSeq(pub u32);

impl EffectSeq {
    /// The first sequence of every run.
    ///
    /// A run that has issued nothing yet has this as its next sequence, so
    /// `EffectIdAllocator::for_run(run).peek() == Some(EffectSeq::FIRST)`.
    pub const FIRST: Self = Self(0);

    /// The last sequence a run can ever issue, and it *is* allocatable.
    ///
    /// Exhaustion means "nothing left to issue", not "the last one is off limits": an
    /// implementation that refused `MAX` would quietly shorten every run by one effect.
    pub const MAX: Self = Self(u32::MAX);

    /// The sequence one after this one, or [`None`] at [`MAX`](Self::MAX).
    ///
    /// # Postconditions
    ///
    /// `Some(Self(self.0 + 1))` when `self < MAX`, and `None` when `self == MAX`. The
    /// result is strictly greater than `self` whenever it is `Some`, so this never wraps
    /// and never repeats.
    ///
    /// Implemented with [`u32::checked_add`] and nothing else. Not `+`, which panics in a
    /// debug firmware build and wraps in a release one; not `wrapping_add`, which is the
    /// silent reuse §07 forbids; and not `saturating_add`, which would reissue `MAX` for
    /// ever while looking, at every individual call site, entirely fine.
    ///
    /// This mints nothing. It is pure, it returns a value, and issuing identity goes
    /// through [`EffectIdAllocator`] — so a holder of a sequence is not a source of new
    /// ones.
    #[must_use]
    pub const fn successor(self) -> Option<Self> {
        // Written as a `match` rather than `.map()` because `Option::map` is not `const`,
        // and this has to be usable from the `const fn` allocator below.
        match self.0.checked_add(1) {
            Some(next) => Some(Self(next)),
            None => None,
        }
    }
}

/// One effect, identified by its run and its position within that run.
///
/// # Invariants
///
/// Field order is load-bearing: the derived [`Ord`] is lexicographic in declaration order,
/// so `run` comes first and two ids from the same run compare by their sequences. A later
/// run always sorts after an earlier one, whatever sequences either reached.
///
/// This is 16 bytes for 12 bytes of data — [`RunId`]'s alignment of 8 pulls in four bytes
/// of tail padding. That is accepted rather than fought: the wire format is written field
/// by field in `waymaker-flash`, so nothing on media pays for the padding, and
/// `#[repr(packed)]` would need `unsafe` to read a field back out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EffectId {
    /// The run that minted this effect.
    pub run: RunId,
    /// Where the effect falls in that run's history.
    pub seq: EffectSeq,
}

/// The one thing in the kernel that mints effect identity for a run.
///
/// This is live kernel state for the whole of a run, so it is registered in
/// [`kernel_state_types!`](crate::budget) and charged against the 128 B budget in design
/// document §04.
///
/// # Invariants
///
/// * `next` is the sequence the following [`allocate`](Self::allocate) will issue, or
///   [`None`] once the space is spent.
/// * Every sequence is issued at most once per run, and issued values strictly increase.
/// * Exhaustion is sticky *by representation*: there is no path from `None` back to
///   `Some`, so there is no flag to forget and no state where the allocator is spent and
///   still holds something to hand out. [`peek`](Self::peek) never returns
///   `Some(EffectSeq::FIRST)` after a refusal, which is what wraparound looks like from
///   the outside.
/// * The run never changes.
///
/// # Why it is not `Clone`, `Copy` or `Default`
///
/// A copied allocator is two allocators minting the same sequence twice; a defaulted one
/// is a run silently restarting at zero. Both are the failure this type exists to prevent,
/// so neither trait is derived. [`Debug`], [`PartialEq`] and [`Eq`] are, so that tests can
/// compare two allocators rather than compare their behaviour.
#[derive(Debug, PartialEq, Eq)]
pub struct EffectIdAllocator {
    run: RunId,
    next: Option<EffectSeq>,
}

impl EffectIdAllocator {
    /// An allocator for a run that has issued nothing.
    ///
    /// # Postconditions
    ///
    /// `peek() == Some(EffectSeq::FIRST)` and `run() == run`.
    #[must_use]
    pub const fn for_run(run: RunId) -> Self {
        Self {
            run,
            next: Some(EffectSeq::FIRST),
        }
    }

    /// An allocator that continues a run after its highest committed sequence.
    ///
    /// This is the replay path, not a test hook: recovery re-creates a run from its
    /// committed prefix, reads the highest sequence that reached durable storage, and
    /// continues *after* it. Re-issuing a committed sequence would make a new effect
    /// indistinguishable from the recorded one.
    ///
    /// # Preconditions
    ///
    /// `last_committed` is the highest sequence this run durably committed, or [`None`] if
    /// it committed nothing.
    ///
    /// # Postconditions
    ///
    /// `resume(run, None) == for_run(run)` — exactly equal, not merely alike. Otherwise
    /// `peek() == last_committed.successor()`, so `resume(run, Some(EffectSeq::MAX))` comes
    /// back already exhausted: the ceiling survives a reboot.
    #[must_use]
    pub const fn resume(run: RunId, last_committed: Option<EffectSeq>) -> Self {
        let next = match last_committed {
            Some(committed) => committed.successor(),
            None => Some(EffectSeq::FIRST),
        };
        Self { run, next }
    }

    /// The run this allocator mints for.
    ///
    /// # Postconditions
    ///
    /// The same value for the life of the allocator, exhausted or not.
    #[must_use]
    pub const fn run(&self) -> RunId {
        self.run
    }

    /// The sequence the next [`allocate`](Self::allocate) will issue, or [`None`] once the
    /// space is spent.
    ///
    /// # Postconditions
    ///
    /// Pure: inspecting costs nothing and issues nothing, so a caller may check capacity
    /// before deciding to schedule. `peek()` agrees with what the next `allocate` returns,
    /// and once it is `None` it stays `None`.
    #[must_use]
    pub const fn peek(&self) -> Option<EffectSeq> {
        self.next
    }

    /// Issues the next effect id for this run and advances.
    ///
    /// # Postconditions
    ///
    /// On success the returned id is `EffectId { run: self.run(), seq }` where `seq` was
    /// [`peek`](Self::peek) before the call, and `peek()` afterwards is `seq.successor()`.
    /// Issued sequences strictly increase and no sequence is issued twice.
    ///
    /// Once [`EffectSeq::MAX`] has been issued the allocator is exhausted for ever: this
    /// returns [`KernelError::IdExhausted`] and keeps doing so, and `peek()` stays
    /// [`None`]. It never wraps back to [`EffectSeq::FIRST`].
    ///
    /// # Errors
    ///
    /// [`KernelError::IdExhausted`] once the run's sequence space is spent. That is
    /// terminal for the run: the way out is `continue_as_new`, which is also the only
    /// point at which history is reclaimed.
    pub const fn allocate(&mut self) -> Result<EffectId, KernelError> {
        match self.next {
            Some(seq) => {
                self.next = seq.successor();
                Ok(EffectId { run: self.run, seq })
            }
            None => Err(KernelError::IdExhausted),
        }
    }
}

// Size *and* alignment are pinned, not size alone: `size_of` is what the kernel-state
// budget charges for, but alignment is what decides whether that size is what a containing
// struct actually costs. A `const` block rather than a test, because the size of a type is
// a compile-time fact and a firmware budget that only fails after a test run is a budget
// somebody has to remember to look at.
const _: () = assert!(core::mem::size_of::<RunId>() == 8);
const _: () = assert!(core::mem::align_of::<RunId>() == 8);
const _: () = assert!(core::mem::size_of::<EffectSeq>() == 4);
const _: () = assert!(core::mem::align_of::<EffectSeq>() == 4);
const _: () = assert!(core::mem::size_of::<EffectId>() == 16);
const _: () = assert!(core::mem::align_of::<EffectId>() == 8);
const _: () = assert!(core::mem::size_of::<EffectIdAllocator>() == 16);
const _: () = assert!(core::mem::align_of::<EffectIdAllocator>() == 8);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::KernelError;

    /// A run id with a distinctive bit pattern, so a test that loses it says so.
    const RUN: RunId = RunId(0x0BAD_F00D_DEAD_BEEF);

    #[test]
    fn a_fresh_allocator_peeks_the_first_sequence() {
        let allocator = EffectIdAllocator::for_run(RUN);

        assert_eq!(allocator.run(), RUN);
        assert_eq!(allocator.peek(), Some(EffectSeq::FIRST));
        assert_eq!(EffectSeq::FIRST, EffectSeq(0));
    }

    #[test]
    fn allocate_issues_first_then_increments() {
        let mut allocator = EffectIdAllocator::for_run(RUN);

        // Sequences are a run's history order, so the first effect of a run is sequence
        // zero and every later one is exactly one more. A gap would make a replay cursor
        // that counts disagree with a journal that does not.
        for expected in 0..8_u32 {
            assert_eq!(allocator.peek(), Some(EffectSeq(expected)));
            assert_eq!(
                allocator.allocate(),
                Ok(EffectId {
                    run: RUN,
                    seq: EffectSeq(expected),
                })
            );
        }
        assert_eq!(allocator.peek(), Some(EffectSeq(8)));
    }

    #[test]
    fn issued_ids_carry_the_run() {
        // §07: the dispatcher dedups on the pair, so an id is only useful if it knows
        // which run minted it — even though only the sequence is written per record.
        let mut allocator = EffectIdAllocator::for_run(RunId(42));

        for _ in 0..4 {
            assert_eq!(allocator.allocate().unwrap().run, RunId(42));
        }
        assert_eq!(allocator.run(), RunId(42));
    }

    #[test]
    fn resume_after_none_equals_for_run() {
        // A run with nothing committed is a run that has not started issuing, so recovery
        // and a cold start have to produce the same allocator rather than two that merely
        // behave alike.
        assert_eq!(
            EffectIdAllocator::resume(RUN, None),
            EffectIdAllocator::for_run(RUN)
        );
    }

    #[test]
    fn resume_after_a_committed_sequence_issues_the_one_after_it() {
        // Replay hands back the highest sequence that reached durable storage. The next
        // effect is the successor of that, never a repeat of it: re-issuing a committed
        // sequence would make the new effect indistinguishable from the recorded one.
        let mut allocator = EffectIdAllocator::resume(RUN, Some(EffectSeq(17)));

        assert_eq!(allocator.peek(), Some(EffectSeq(18)));
        assert_eq!(
            allocator.allocate(),
            Ok(EffectId {
                run: RUN,
                seq: EffectSeq(18),
            })
        );
        assert_eq!(allocator.peek(), Some(EffectSeq(19)));
    }

    #[test]
    fn resume_after_max_is_already_exhausted() {
        // The ceiling survives a reboot. An allocator resumed from a run that spent its
        // last sequence must come back spent, not come back fresh.
        let mut allocator = EffectIdAllocator::resume(RUN, Some(EffectSeq::MAX));

        assert_eq!(allocator.peek(), None);
        assert_eq!(allocator.allocate(), Err(KernelError::IdExhausted));
        assert_eq!(allocator.run(), RUN);
    }

    #[test]
    fn the_last_sequence_is_issued_before_exhaustion() {
        // `MAX` is allocatable: exhaustion is "nothing left to issue", not "the last one
        // is off limits". An implementation that refused `MAX` would quietly shorten
        // every run by one effect.
        let mut allocator = EffectIdAllocator::resume(RUN, Some(EffectSeq(u32::MAX - 1)));

        assert_eq!(allocator.peek(), Some(EffectSeq::MAX));
        assert_eq!(
            allocator.allocate(),
            Ok(EffectId {
                run: RUN,
                seq: EffectSeq::MAX,
            })
        );
        assert_eq!(allocator.peek(), None);
        assert_eq!(allocator.allocate(), Err(KernelError::IdExhausted));
    }

    #[test]
    fn exhaustion_is_sticky() {
        // §07: wraparound is terminal, not silent reuse. The observable difference is
        // exactly this — a wrapping allocator hands out `FIRST` again on some later call,
        // so every call after the first refusal is checked rather than just the next one.
        let mut allocator = EffectIdAllocator::resume(RUN, Some(EffectSeq::MAX));

        for attempt in 0..6 {
            assert_eq!(
                allocator.allocate(),
                Err(KernelError::IdExhausted),
                "attempt {attempt}"
            );
            assert_eq!(allocator.peek(), None, "attempt {attempt}");
            assert_ne!(
                allocator.peek(),
                Some(EffectSeq::FIRST),
                "attempt {attempt} wrapped to the first sequence"
            );
            assert_eq!(allocator.run(), RUN, "attempt {attempt}");
        }
    }

    #[test]
    fn peek_agrees_with_allocate() {
        // `peek` is what the caller inspects before deciding to schedule; if it could
        // disagree with what `allocate` then issues, a capacity check would be checking
        // the wrong sequence.
        let mut allocator = EffectIdAllocator::for_run(RUN);
        for _ in 0..8 {
            let peeked = allocator.peek();
            assert_eq!(peeked, Some(allocator.allocate().unwrap().seq));
        }

        let mut spent = EffectIdAllocator::resume(RUN, Some(EffectSeq::MAX));
        assert!(spent.peek().is_none());
        assert!(spent.allocate().is_err());
    }

    #[test]
    fn no_reachable_state_reports_exhaustion_while_sequences_remain() {
        // The one property that would need 2^32 resumes to prove exhaustively: an
        // allocator is exhausted if and only if `MAX` is already committed. Swept at both
        // ends, where an off-by-one lives, and across the middle on a stride, where a
        // stray mask or truncation would show.
        const PROBES: [u32; 9] = [
            0,
            1,
            2,
            4_095,
            65_535,
            16_777_216,
            u32::MAX - 2,
            u32::MAX - 1,
            u32::MAX,
        ];

        for committed in PROBES {
            let allocator = EffectIdAllocator::resume(RUN, Some(EffectSeq(committed)));
            assert_eq!(
                allocator.peek().is_some(),
                committed != u32::MAX,
                "committed {committed}"
            );
            assert_eq!(
                allocator.peek(),
                EffectSeq(committed).successor(),
                "committed {committed}"
            );
        }
    }

    #[test]
    fn successor_of_max_is_none() {
        // `checked_add`, never `+`, `wrapping_add` or `saturating_add`: the first panics
        // in a debug firmware build and the other two are the silent reuse §07 forbids.
        assert_eq!(EffectSeq::MAX, EffectSeq(u32::MAX));
        assert_eq!(EffectSeq::MAX.successor(), None);
    }

    #[test]
    fn successor_increments_by_one() {
        assert_eq!(EffectSeq::FIRST.successor(), Some(EffectSeq(1)));
        assert_eq!(EffectSeq(41).successor(), Some(EffectSeq(42)));
        assert_eq!(EffectSeq(u32::MAX - 1).successor(), Some(EffectSeq::MAX));
    }

    #[test]
    fn effect_ids_order_by_run_then_seq() {
        // The derive reads fields in declaration order, so `run` must stay first. A later
        // run always sorts after an earlier one, whatever sequences either reached.
        let early_run_last_seq = EffectId {
            run: RunId(1),
            seq: EffectSeq::MAX,
        };
        let late_run_first_seq = EffectId {
            run: RunId(2),
            seq: EffectSeq::FIRST,
        };
        let early_run_first_seq = EffectId {
            run: RunId(1),
            seq: EffectSeq::FIRST,
        };

        assert!(early_run_first_seq < early_run_last_seq);
        assert!(early_run_last_seq < late_run_first_seq);
        assert!(early_run_first_seq < late_run_first_seq);
    }

    #[test]
    fn monotonic_over_a_few_thousand_allocations() {
        // A few thousand rather than the whole space: enough to cross the byte and
        // half-word boundaries where a truncating increment would fold back on itself,
        // and still well under a second.
        const ALLOCATIONS: u32 = 4_096;

        let mut allocator = EffectIdAllocator::for_run(RUN);
        let mut previous = None;

        for expected in 0..ALLOCATIONS {
            let issued = allocator.allocate().unwrap();
            assert_eq!(issued.run, RUN);
            assert_eq!(issued.seq, EffectSeq(expected));
            if let Some(before) = previous {
                assert!(before < issued.seq, "sequence {expected} repeated");
            }
            previous = Some(issued.seq);
        }

        assert_eq!(previous, Some(EffectSeq(ALLOCATIONS - 1)));
        assert_eq!(allocator.peek(), Some(EffectSeq(ALLOCATIONS)));
    }
}
