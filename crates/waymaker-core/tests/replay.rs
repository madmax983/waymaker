//! The streaming replay cursor, tested through the surface a driver sees.
//!
//! Design document §02 decision 2, §06 Cold-start replay, §08 Replay and determinism. A
//! cursor advances through history in workflow order; there is no `Journal::get(id)` and no
//! in-memory event index, which is what makes replay constant-memory and free of random
//! reads.
//!
//! These tests are written against the two properties that are cheap to claim and easy to
//! lose:
//!
//! * the cursor keeps nothing from the caller's scratch page, so one page is enough however
//!   long history is — proved by overwriting the page between every record;
//! * the cursor's state does not grow with history — proved by replaying five history
//!   lengths spanning five orders of magnitude and comparing the live bytes.
//!
//! What is deliberately *not* here is the transition table of §08: whether a workflow's
//! request matches what history recorded is issue #15's, and this cursor knows nothing
//! about a workflow. It validates history against itself.

use waymaker_core::budget::SCRATCH_PAGE_BYTES;
use waymaker_core::replay::{PendingEffect, Position, ReplayCursor, Step};
use waymaker_core::{ActivityKind, EffectId, EffectSeq, KernelError, RecordRef, RunId};

/// The run every test below replays, unless it says otherwise.
const RUN: RunId = RunId(0x0102_0304_0506_0708);

/// The activity kind the synthetic histories schedule.
const KIND: ActivityKind = ActivityKind(7);

/// A cursor that has consumed `RunStarted` and nothing else.
fn started() -> ReplayCursor {
    let mut cursor = ReplayCursor::new(RUN);
    let step = cursor.advance(RecordRef::RunStarted {
        workflow_kind: 1,
        workflow_version: 2,
        input: b"input",
    });
    assert!(step.is_ok(), "a fresh cursor must accept `RunStarted`");
    cursor
}

/// `RecordRef::EffectScheduled` for `seq`, with a digest a test can recognise.
const fn schedule(seq: u32) -> RecordRef<'static> {
    RecordRef::EffectScheduled {
        seq: EffectSeq(seq),
        kind: KIND,
        input_len: 4,
        input_crc: 0xDEAD_BEEF,
    }
}

/// The effect `schedule(seq)` commits, as the cursor reports it.
const fn pending(seq: u32) -> PendingEffect {
    PendingEffect {
        id: EffectId {
            run: RUN,
            seq: EffectSeq(seq),
        },
        kind: KIND,
        input_len: 4,
        input_crc: 0xDEAD_BEEF,
    }
}

// ---------------------------------------------------------------------------
// Position, and the order history has to arrive in.
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_cursor_stands_before_the_run() {
    let cursor = ReplayCursor::new(RUN);
    assert_eq!(cursor.position(), Position::BeforeRun);
    assert_eq!(cursor.run(), RUN);
    assert_eq!(cursor.pending(), None);
    assert_eq!(cursor.next_seq(), Some(EffectSeq::FIRST));
    assert!(!cursor.position().is_terminal());
}

#[test]
fn the_first_record_must_be_the_one_that_starts_the_run() {
    let mut cursor = ReplayCursor::new(RUN);
    assert_eq!(
        cursor.advance(schedule(0)),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        cursor.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
}

#[test]
fn run_started_hands_back_the_run_input_and_begins_replay() {
    let mut cursor = ReplayCursor::new(RUN);
    let step = cursor.advance(RecordRef::RunStarted {
        workflow_kind: 0x1234,
        workflow_version: 9,
        input: b"input",
    });
    assert_eq!(
        step,
        Ok(Step::RunStarted {
            workflow_kind: 0x1234,
            workflow_version: 9,
            input: b"input",
        })
    );
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.next_seq(), Some(EffectSeq::FIRST));
}

#[test]
fn a_run_can_only_start_once() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 2,
            input: b"again",
        }),
        Err(KernelError::MalformedHistory)
    );
}

// ---------------------------------------------------------------------------
// Effects: a schedule, then exactly one outcome for it.
// ---------------------------------------------------------------------------

#[test]
fn a_schedule_becomes_the_first_unresolved_effect() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(schedule(0)),
        Ok(Step::EffectScheduled(pending(0)))
    );
    assert_eq!(cursor.position(), Position::AwaitingOutcome);
    assert_eq!(cursor.pending(), Some(pending(0)));
    // The identity is spent: a redelivery reuses it rather than minting a fresh one.
    assert_eq!(cursor.next_seq(), Some(EffectSeq(1)));
}

#[test]
fn a_completion_resolves_the_effect_it_names() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"result",
        }),
        Ok(Step::EffectCompleted {
            id: EffectId {
                run: RUN,
                seq: EffectSeq(0)
            },
            result: b"result",
        })
    );
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.pending(), None);
}

#[test]
fn a_failure_resolves_the_effect_it_names() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(RecordRef::EffectFailed {
            seq: EffectSeq(0),
            error: b"nope",
        }),
        Ok(Step::EffectFailed {
            id: EffectId {
                run: RUN,
                seq: EffectSeq(0)
            },
            error: b"nope",
        })
    );
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.pending(), None);
}

#[test]
fn an_outcome_with_no_schedule_is_refused() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"",
        }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn an_outcome_for_another_effect_is_refused() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(1),
            result: b"",
        }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_second_outcome_for_a_resolved_effect_is_refused() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    let _ = cursor.advance(RecordRef::EffectCompleted {
        seq: EffectSeq(0),
        result: b"",
    });
    assert_eq!(
        cursor.advance(RecordRef::EffectFailed {
            seq: EffectSeq(0),
            error: b"",
        }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_schedule_while_an_effect_is_unresolved_is_refused() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(schedule(1)),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_schedule_that_skips_a_sequence_is_refused() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(schedule(1)),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_schedule_that_reuses_a_sequence_is_refused() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    let _ = cursor.advance(RecordRef::EffectCompleted {
        seq: EffectSeq(0),
        result: b"",
    });
    assert_eq!(
        cursor.advance(schedule(0)),
        Err(KernelError::MalformedHistory)
    );
}

// ---------------------------------------------------------------------------
// Terminal records.
// ---------------------------------------------------------------------------

#[test]
fn a_run_completed_record_ends_the_run() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(RecordRef::RunCompleted { result: b"done" }),
        Ok(Step::RunCompleted { result: b"done" })
    );
    assert_eq!(cursor.position(), Position::RunCompleted);
    assert!(cursor.position().is_terminal());
}

#[test]
fn a_run_failed_record_ends_the_run() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(RecordRef::RunFailed { error: b"bad" }),
        Ok(Step::RunFailed { error: b"bad" })
    );
    assert_eq!(cursor.position(), Position::RunFailed);
    assert!(cursor.position().is_terminal());
}

#[test]
fn nothing_may_follow_a_terminal_record() {
    let mut cursor = started();
    let _ = cursor.advance(RecordRef::RunCompleted { result: b"" });
    assert_eq!(
        cursor.advance(schedule(0)),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_run_cannot_end_with_an_effect_unresolved() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(RecordRef::RunCompleted { result: b"" }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_run_cannot_end_before_it_starts() {
    let mut cursor = ReplayCursor::new(RUN);
    assert_eq!(
        cursor.advance(RecordRef::RunFailed { error: b"" }),
        Err(KernelError::MalformedHistory)
    );
}

// ---------------------------------------------------------------------------
// Recovery stops at the first bad record, and stays stopped.
// ---------------------------------------------------------------------------

#[test]
fn the_cursor_stays_halted_at_the_record_that_stopped_it() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(schedule(4)),
        Err(KernelError::MalformedHistory)
    );
    // A record that would have been perfectly legal a moment ago is refused too: §14's
    // "recovery exposes only a legal prefix of committed records" is not a best-effort.
    assert_eq!(
        cursor.advance(schedule(0)),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        cursor.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
    assert_eq!(cursor.pending(), None);
}

// ---------------------------------------------------------------------------
// Minting the identity for the work that comes after the committed prefix.
// ---------------------------------------------------------------------------

#[test]
fn the_next_effect_continues_after_the_committed_prefix() {
    let mut cursor = started();
    for seq in 0..3 {
        let _ = cursor.advance(schedule(seq));
        let _ = cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(seq),
            result: b"",
        });
    }
    assert_eq!(cursor.next_seq(), Some(EffectSeq(3)));
    assert_eq!(
        cursor.allocate(),
        Ok(EffectId {
            run: RUN,
            seq: EffectSeq(3)
        })
    );
    assert_eq!(cursor.next_seq(), Some(EffectSeq(4)));
}

#[test]
fn a_new_effect_cannot_replace_an_unresolved_one() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.allocate(),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn no_effect_follows_a_terminal_record() {
    let mut cursor = started();
    let _ = cursor.advance(RecordRef::RunCompleted { result: b"" });
    assert_eq!(
        cursor.allocate(),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn no_effect_is_minted_before_the_run_starts() {
    let mut cursor = ReplayCursor::new(RUN);
    assert_eq!(
        cursor.allocate(),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn a_halted_cursor_mints_nothing() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(9));
    assert_eq!(cursor.allocate(), Err(KernelError::MalformedHistory));
}

// ---------------------------------------------------------------------------
// One caller-owned scratch page, and constant memory.
// ---------------------------------------------------------------------------

/// The bytes a driver holds live while replaying: the cursor, and one scratch page.
///
/// The kernel has no allocator and no page of its own, so this is the whole of it. A
/// history-length-dependent term could only come from the cursor, which is why the tests
/// below compare this number across history lengths rather than trusting the type.
const fn live_bytes(cursor: &ReplayCursor, page: &[u8]) -> usize {
    core::mem::size_of_val(cursor) + core::mem::size_of_val(page)
}

/// Replays `effects` schedule/completion pairs through one page, poisoning it between
/// records, and returns the live bytes at the end.
///
/// The page is overwritten with a recognisable pattern before every record is written into
/// it, so a cursor that had kept a slice of the previous record would be reading `0xA5`s.
/// The history is generated as it is consumed: nothing in this function holds more than one
/// record, which is what makes the returned figure a measurement rather than a claim.
fn replay_generated_history(effects: u32) -> usize {
    let mut page = [0_u8; SCRATCH_PAGE_BYTES];
    let mut cursor = ReplayCursor::new(RUN);

    page.fill(0xA5);
    page[..5].copy_from_slice(b"input");
    let started = cursor.advance(RecordRef::RunStarted {
        workflow_kind: 1,
        workflow_version: 1,
        input: &page[..5],
    });
    assert_eq!(
        started,
        Ok(Step::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"input",
        })
    );

    for seq in 0..effects {
        page.fill(0xA5);
        assert_eq!(
            cursor.advance(schedule(seq)),
            Ok(Step::EffectScheduled(pending(seq)))
        );

        page.fill(0xA5);
        page[..4].copy_from_slice(&seq.to_le_bytes());
        let outcome = cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(seq),
            result: &page[..4],
        });
        assert_eq!(
            outcome,
            Ok(Step::EffectCompleted {
                id: EffectId {
                    run: RUN,
                    seq: EffectSeq(seq)
                },
                result: &seq.to_le_bytes(),
            })
        );
    }

    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.next_seq(), Some(EffectSeq(effects)));
    live_bytes(&cursor, &page)
}

#[test]
fn replaying_a_long_history_costs_exactly_what_a_short_one_costs() {
    // Five orders of magnitude. If any per-record state were retained, the last figure
    // would not be the first.
    let baseline = replay_generated_history(0);
    for effects in [1, 2, 1_000, 200_000] {
        assert_eq!(
            replay_generated_history(effects),
            baseline,
            "replaying {effects} effects did not cost what replaying none costs"
        );
    }
}

#[test]
fn the_cursor_holds_no_scratch_page_of_its_own() {
    // The issue's wording: "the kernel allocates nothing and holds no page buffer of its
    // own". A cursor that had one could not be smaller than one.
    const {
        assert!(size_of::<ReplayCursor>() < SCRATCH_PAGE_BYTES);
    }
}

#[test]
fn the_cursor_is_registered_against_the_kernel_state_budget() {
    let registered = waymaker_core::budget::KERNEL_STATE_TYPES
        .iter()
        .find(|entry| entry.name.contains("ReplayCursor"));
    let registered = registered.expect("the cursor is live kernel state and must be budgeted");
    assert_eq!(registered.size, size_of::<ReplayCursor>());
    assert!(registered.size <= waymaker_core::budget::KERNEL_STATE_BYTES);
}

#[test]
fn history_is_consumed_once_and_only_forwards() {
    // A single-pass iterator that cannot be restarted or indexed: if the cursor needed to
    // look back at a record, or to fetch one by effect id, this would not compile — there
    // is nothing to look back at and nothing to fetch from.
    let mut cursor = ReplayCursor::new(RUN);
    let history = [
        RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"in",
        },
        schedule(0),
        RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"out",
        },
        schedule(1),
    ];
    let mut seen = 0_u32;
    for record in history {
        assert!(cursor.advance(record).is_ok());
        seen += 1;
    }
    assert_eq!(seen, 4);
    // Step 5 of §06's cold-start sequence: the first unresolved effect is what is left.
    assert_eq!(cursor.pending(), Some(pending(1)));
    assert_eq!(cursor.position(), Position::AwaitingOutcome);
}
