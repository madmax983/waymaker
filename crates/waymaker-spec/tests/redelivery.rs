//! Design document §14's fourth guarantee: "retries and reboot redelivery reuse the original
//! effect identity."
//!
//! Not a statement about a ghost state, so it is not proved over the model: it is a statement
//! about `waymaker_core::EffectIdAllocator`, and it is discharged against the shipped
//! allocator over every resume point a bounded run can have.
//!
//! The claim, stated precisely: the identity an effect gets is a function of the run and the
//! effect's position in history, and of nothing else. Not of when the power went away, not of
//! how many times it went away, and not of how much of history had been committed when it
//! did. That is what makes redelivery after a reboot the *same* effect rather than a new one,
//! and it is what lets a downstream service deduplicate.

use waymaker_core::{EffectId, EffectIdAllocator, EffectSeq, KernelError, RunId};

/// How many effects a run allocates in these proofs.
const EFFECTS: u32 = 6;

/// Runs to check the claim over. Distinct, and including the boundary values, because the
/// identity is a pair and a bug that dropped the run half would be invisible over one run.
const RUNS: [RunId; 4] = [
    RunId(0),
    RunId(1),
    RunId(u64::MAX),
    RunId(0x0123_4567_89AB_CDEF),
];

/// The ids a fresh run allocates, in order.
fn fresh(run: RunId) -> Vec<EffectId> {
    let mut allocator = EffectIdAllocator::for_run(run);
    (0..EFFECTS)
        .map(|_| match allocator.allocate() {
            Ok(id) => id,
            Err(error) => unreachable!("six effects is not exhaustion: {error}"),
        })
        .collect()
}

#[test]
fn a_reboot_at_every_point_in_history_redelivers_the_same_identity() {
    for run in RUNS {
        let original = fresh(run);
        // A reboot after `committed` effects were durable. Every point, including none at
        // all and all of them.
        for committed in 0_usize..=original.len() {
            let last = committed
                .checked_sub(1)
                .and_then(|index| original.get(index))
                .map(|id| id.seq);
            let mut resumed = EffectIdAllocator::resume(run, last);
            for (position, expected) in original.iter().enumerate().skip(committed) {
                let allocated = resumed
                    .allocate()
                    .expect("resuming inside a six-effect run is not exhaustion");
                assert_eq!(
                    allocated, *expected,
                    "a reboot after {committed} committed effects gave position \
                     {position} a different identity"
                );
            }
        }
    }
}

#[test]
fn any_number_of_reboots_at_one_point_gives_one_identity() {
    // Retry, not just reboot: an effect redelivered five times is the same effect five
    // times. A per-boot counter or a clock in the identity would fail here and pass the
    // test above.
    for run in RUNS {
        let original = fresh(run);
        let third = original
            .get(2)
            .copied()
            .expect("six effects were allocated");
        let after_second = original.get(1).map(|id| id.seq);
        for _ in 0..5 {
            let mut resumed = EffectIdAllocator::resume(run, after_second);
            assert_eq!(
                resumed.allocate().expect("not exhaustion"),
                third,
                "redelivery gave the same effect a different identity on a later attempt"
            );
        }
    }
}

#[test]
fn two_runs_never_share_an_identity() {
    // The other half of "the original identity": an identity that were a function of the
    // position alone would be stable and useless, because two runs would deduplicate against
    // each other.
    let mut seen = std::collections::BTreeSet::new();
    for run in RUNS {
        for id in fresh(run) {
            assert!(
                seen.insert(id),
                "two runs allocated the same effect identity"
            );
        }
    }
    assert_eq!(seen.len(), RUNS.len() * EFFECTS as usize);
}

/// Ways an allocator could be wrong, as functions of the same shape as the real one.
///
/// The falsifier for this clause. The other five proofs here run against the shipped
/// allocator and nothing else, and a suite that only ever sees the right answer is a suite
/// whose assertions nobody has watched fail. Each of these is a plausible mistake — a
/// per-boot counter, an identity that forgets the run, an allocator that restarts after a
/// crash — and each has to be caught by a *named* claim above rather than by any claim at
/// all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WrongAllocator {
    /// Restarts at the first sequence after every reboot. The redelivery-after-reset bug.
    RestartsAfterACrash,
    /// Numbers effects by position and forgets the run. Two runs then deduplicate against
    /// each other.
    ForgetsTheRun,
    /// Continues one past where the run left off. The fencepost.
    SkipsOne,
}

impl WrongAllocator {
    const ALL: [Self; 3] = [
        Self::RestartsAfterACrash,
        Self::ForgetsTheRun,
        Self::SkipsOne,
    ];

    /// The ids this allocator hands out after a reboot with `last` committed.
    fn resume(self, run: RunId, last: Option<EffectSeq>, count: u32) -> Vec<EffectId> {
        let (run, first) = match self {
            Self::RestartsAfterACrash => (run, EffectSeq::FIRST),
            Self::ForgetsTheRun => (
                RunId(0),
                last.map_or(EffectSeq::FIRST, |seq| EffectSeq(seq.0.saturating_add(1))),
            ),
            Self::SkipsOne => (
                run,
                last.map_or(EffectSeq(2), |seq| EffectSeq(seq.0.saturating_add(2))),
            ),
        };
        (0..count)
            .map(|offset| EffectId {
                run,
                seq: EffectSeq(first.0.saturating_add(offset)),
            })
            .collect()
    }
}

#[test]
fn every_wrong_allocator_is_caught_by_a_claim_this_file_makes() {
    for wrong in WrongAllocator::ALL {
        let caught = RUNS.iter().any(|run| {
            let original = fresh(*run);
            // The claim of `a_reboot_at_every_point_in_history_redelivers_the_same_identity`,
            // asked of the wrong allocator instead of the real one.
            (0_usize..=original.len()).any(|committed| {
                let last = committed
                    .checked_sub(1)
                    .and_then(|index| original.get(index))
                    .map(|id| id.seq);
                let produced = wrong.resume(
                    *run,
                    last,
                    u32::try_from(original.len().saturating_sub(committed)).unwrap_or(0),
                );
                let expected: Vec<EffectId> = original.iter().skip(committed).copied().collect();
                produced != expected
            })
        });
        assert!(
            caught,
            "an allocator that {wrong:?} redelivers the same identities as the real one, so \
             this file cannot tell them apart"
        );
    }
}

#[test]
fn an_allocator_that_forgets_the_run_is_caught_by_the_uniqueness_claim_too() {
    // The second claim, separately: `two_runs_never_share_an_identity` has to be what
    // catches this one, not a coincidence of the first.
    let mut seen = std::collections::BTreeSet::new();
    let mut collided = false;
    for run in RUNS {
        for id in WrongAllocator::ForgetsTheRun.resume(run, None, EFFECTS) {
            if !seen.insert(id) {
                collided = true;
            }
        }
    }
    assert!(
        collided,
        "an allocator that forgets the run does not collide, so the uniqueness claim is not \
         what rules it out"
    );
}

#[test]
fn resuming_past_the_last_sequence_is_refused_rather_than_wrapped() {
    // The boundary, and the one place a stable identity could stop being stable: an
    // allocator that wrapped would hand a fresh effect an identity history already used.
    // ADR 0006 makes exhaustion terminal instead.
    for run in RUNS {
        let mut exhausted = EffectIdAllocator::resume(run, Some(EffectSeq(u32::MAX)));
        assert_eq!(exhausted.peek(), None);
        assert_eq!(exhausted.allocate(), Err(KernelError::IdExhausted));
        // Terminal: asking again does not recover.
        assert_eq!(exhausted.allocate(), Err(KernelError::IdExhausted));
    }
}

#[test]
fn a_fresh_allocator_and_a_resume_from_nothing_are_the_same_allocator() {
    // `resume(run, None)` is what a device that crashed before committing anything does, and
    // it has to be indistinguishable from a first boot — otherwise the first effect of a run
    // has two possible identities depending on a crash nobody can observe afterwards.
    for run in RUNS {
        let mut first_boot = EffectIdAllocator::for_run(run);
        let mut after_a_crash = EffectIdAllocator::resume(run, None);
        assert_eq!(first_boot.peek(), after_a_crash.peek());
        for _ in 0..EFFECTS {
            assert_eq!(first_boot.allocate(), after_a_crash.allocate());
        }
    }
}

#[test]
fn every_sequence_boundary_resumes_where_the_run_left_off() {
    // Exhaustive over the boundaries a `u32` sequence has, rather than over the six-effect
    // runs above: the value before exhaustion, and the values a fencepost error would reach
    // for.
    for run in RUNS {
        for last in [0_u32, 1, 2, u32::MAX - 2, u32::MAX - 1] {
            let mut resumed = EffectIdAllocator::resume(run, Some(EffectSeq(last)));
            let next = resumed.allocate().expect("one more sequence is available");
            assert_eq!(
                next.seq,
                EffectSeq(last + 1),
                "resuming after sequence {last} did not continue at {}",
                last + 1
            );
            assert_eq!(next.run, run);
        }
    }
}
