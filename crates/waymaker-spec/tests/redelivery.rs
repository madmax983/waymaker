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

use waymaker_core::replay::Position;
use waymaker_core::{
    ActivityKind, EffectId, EffectIdAllocator, EffectSeq, KernelError, RecordRef, ReplayCursor,
    RunId,
};
use waymaker_flash::frame::{self, ProgramAlign, Scan};

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

// ---------------------------------------------------------------------------------------
// The redelivery path itself, through the replay API a reboot really uses.
//
// Everything above is about `EffectIdAllocator`, and the allocator is only half of §14's
// fourth guarantee. Codex caught the other half on PR #66: a *pending* effect — one whose
// schedule is committed and whose outcome is not — is never re-allocated. The allocator
// advances past its sequence, and the identity a dispatcher is handed again comes from
// `ReplayCursor::pending`. A regression there leaves every test above green, which is
// exactly the shape of hole a proof is supposed to have none of.
//
// So these drive the real journal: records encoded by `waymaker-flash`, read back by `Scan`,
// replayed by `ReplayCursor`, at every prefix of history a reset could have left behind.
// ---------------------------------------------------------------------------------------

/// The run these journals belong to.
const REPLAYED: RunId = RunId(0x0BAD_F00D_DEAD_BEEF);

/// The activity every schedule below names.
const DOWNLOAD: ActivityKind = ActivityKind(4);

fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    align
}

/// A journal holding `RunStarted`, then `schedules` effects, the last of which has no
/// outcome — the shape a reset during an activity leaves behind.
fn journal(schedules: u32) -> Vec<u8> {
    let mut image = Vec::new();
    let mut append = |record: &RecordRef<'_>| {
        let mut buffer = [0_u8; 64];
        let Ok(written) = frame::encode(record, align(), &mut buffer) else {
            unreachable!("64 bytes is more than any record here needs")
        };
        let Some(bytes) = buffer.get(..written) else {
            unreachable!("`encode` reports what it wrote")
        };
        image.extend_from_slice(bytes);
    };
    append(&RecordRef::RunStarted {
        workflow_kind: 1,
        workflow_version: 1,
        input: b"input",
    });
    // Sequences count from `EffectSeq::FIRST`, which is zero: a journal whose first effect
    // is numbered one is history the cursor refuses as malformed, because a sequence that
    // skips is a sequence no execution could have produced.
    for effect in 0..schedules {
        append(&RecordRef::EffectScheduled {
            seq: EffectSeq(effect),
            kind: DOWNLOAD,
            input_len: 4,
            input_crc: frame::input_digest(b"blob"),
        });
        if effect.saturating_add(1) < schedules {
            append(&RecordRef::EffectCompleted {
                seq: EffectSeq(effect),
                result: b"ok",
            });
        }
    }
    image
}

/// Replays `image` and returns the cursor it left behind.
fn replay(image: &[u8]) -> ReplayCursor {
    let mut cursor = ReplayCursor::new(REPLAYED);
    for record in Scan::new(image, align())
        .take_while(Result::is_ok)
        .flatten()
    {
        match cursor.advance(record) {
            Ok(_) => {}
            Err(error) => unreachable!("this journal is a legal history: {error}"),
        }
    }
    cursor
}

#[test]
fn a_pending_effect_is_redelivered_under_the_identity_the_run_first_gave_it() {
    // The claim §14 actually makes, on the path a reboot actually takes. The schedule for
    // effect `n` is committed and its outcome is not; the cursor rebuilt from that journal
    // has to hand back the same `EffectId` the run allocated before the reset, not a fresh
    // one.
    for schedules in 1..=EFFECTS {
        let original = fresh(REPLAYED);
        let Some(expected) = original
            .get((schedules as usize).saturating_sub(1))
            .copied()
        else {
            unreachable!("the run allocated EFFECTS identities")
        };

        let cursor = replay(&journal(schedules));
        assert_eq!(cursor.position(), Position::AwaitingOutcome);
        let Some(pending) = cursor.pending() else {
            unreachable!("a schedule with no outcome leaves an effect pending")
        };
        assert_eq!(
            pending.id, expected,
            "replaying a journal with effect {schedules} unresolved redelivered a different \
             identity than the run first gave it"
        );
        assert_eq!(pending.kind, DOWNLOAD);
        assert_eq!(pending.input_crc, frame::input_digest(b"blob"));
    }
}

#[test]
fn a_pending_effect_is_not_replaced_by_a_fresh_identity() {
    // The other half, and the one a naive driver gets wrong: while an effect is unresolved,
    // asking for the *next* identity is refused. A driver that reached for one anyway would
    // abandon the effect the world has already seen and hand the same work a second id.
    for schedules in 1..=EFFECTS {
        let cursor = replay(&journal(schedules));
        assert_eq!(
            cursor.next_effect_id(),
            Err(KernelError::NondeterministicWorkflow)
        );
        // And the allocator has moved past the pending sequence, which is why it cannot be
        // the thing that redelivers: the pending effect's identity is behind the counter.
        let Some(pending) = cursor.pending() else {
            unreachable!("a schedule with no outcome leaves an effect pending")
        };
        assert_eq!(cursor.next_seq(), Some(EffectSeq(pending.id.seq.0 + 1)));
    }
}

#[test]
fn redelivery_is_the_same_answer_however_many_times_the_run_is_replayed() {
    // A reboot loop: the journal does not change, so neither may the identity.
    let image = journal(EFFECTS);
    let first = replay(&image).pending();
    for _ in 0..5 {
        assert_eq!(replay(&image).pending(), first);
    }
    assert!(first.is_some());
}

#[test]
fn a_run_whose_last_effect_completed_has_nothing_to_redeliver_and_continues() {
    // The complement, so the test above is not passing because `pending` is always `Some`.
    // With every effect resolved, there is nothing to redeliver and the next identity is the
    // one the allocator would have issued.
    let mut image = journal(EFFECTS);
    let mut buffer = [0_u8; 64];
    let Ok(written) = frame::encode(
        &RecordRef::EffectCompleted {
            seq: EffectSeq(EFFECTS.saturating_sub(1)),
            result: b"ok",
        },
        align(),
        &mut buffer,
    ) else {
        unreachable!("64 bytes is more than this record needs")
    };
    let Some(bytes) = buffer.get(..written) else {
        unreachable!("`encode` reports what it wrote")
    };
    image.extend_from_slice(bytes);

    let cursor = replay(&image);
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.pending(), None);
    // One past the last identity the run allocated, and the same one a fresh allocator
    // resumed from this history would issue — the two halves of §14's guarantee agreeing.
    let next = EffectId {
        run: REPLAYED,
        seq: EffectSeq(EFFECTS),
    };
    assert_eq!(cursor.next_effect_id(), Ok(next));
    let mut resumed = EffectIdAllocator::resume(REPLAYED, Some(EffectSeq(EFFECTS - 1)));
    assert_eq!(resumed.allocate(), Ok(next));
}

#[test]
fn a_driver_that_redelivers_from_the_allocator_instead_of_from_history_is_caught() {
    // The falsifier for the path itself. `next_seq` is one past the pending effect, so a
    // driver that redelivered from the counter would hand the world a second identity for
    // work it has already been given — and every assertion in the first half of this file
    // would still pass, because the allocator is behaving correctly. This is what makes the
    // tests above worth having.
    let mut caught = 0_usize;
    for schedules in 1..=EFFECTS {
        let cursor = replay(&journal(schedules));
        let Some(pending) = cursor.pending() else {
            unreachable!("a schedule with no outcome leaves an effect pending")
        };
        let from_the_counter = cursor.next_seq();
        if from_the_counter != Some(pending.id.seq) {
            caught = caught.saturating_add(1);
        }
    }
    assert_eq!(
        caught, EFFECTS as usize,
        "redelivering from the allocator agrees with redelivering from history, so nothing \
         here can tell the two apart"
    );
}
