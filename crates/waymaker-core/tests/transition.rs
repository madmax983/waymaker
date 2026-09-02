//! Design document §08's transition table, row by row, through the surface a driver sees.
//!
//! The table has five rows and this file has a section per row, plus one section for each
//! flavour of divergence and one for the property the whole module exists to hold: a
//! diverging replay never hands out an effect identity, so it can never dispatch.
//!
//! Written against the machine rather than against the cursor. The cursor validates
//! history against itself (issue #14); this is history against what the workflow asked
//! for, which is the only place §08's `NondeterministicWorkflow` can come from.

use waymaker_core::budget::SCRATCH_PAGE_BYTES;
use waymaker_core::replay::{PendingEffect, Position};
use waymaker_core::transition::{
    Divergence, EffectRequest, Intent, Next, Outcome, ReplayMachine, Resolve,
};
use waymaker_core::{ActivityKind, EffectId, EffectSeq, KernelError, RecordRef, RunId};

/// The run every test below replays, unless it says otherwise.
const RUN: RunId = RunId(0x0102_0304_0506_0708);

/// The activity kind the synthetic histories schedule.
const KIND: ActivityKind = ActivityKind(7);

/// The digest the synthetic histories record for that activity's input.
const INPUT_LEN: u16 = 4;
/// The checksum of those same four bytes, as the layer above would have computed it.
const INPUT_CRC: u32 = 0xDEAD_BEEF;

/// What the workflow asks for at every boundary below, unless a test changes one field.
const REQUEST: EffectRequest = EffectRequest {
    kind: KIND,
    input_len: INPUT_LEN,
    input_crc: INPUT_CRC,
};

/// A machine that has consumed `RunStarted` and nothing else.
fn started() -> ReplayMachine {
    let mut machine = ReplayMachine::new(RUN);
    let step = machine.advance(RecordRef::RunStarted {
        workflow_kind: 1,
        workflow_version: 2,
        input: b"input",
    });
    assert!(step.is_ok(), "a fresh machine must accept `RunStarted`");
    machine
}

/// `RecordRef::EffectScheduled` for `seq`, matching [`REQUEST`].
const fn schedule(seq: u32) -> RecordRef<'static> {
    RecordRef::EffectScheduled {
        seq: EffectSeq(seq),
        kind: KIND,
        input_len: INPUT_LEN,
        input_crc: INPUT_CRC,
    }
}

/// The identity `schedule(seq)` commits.
const fn effect(seq: u32) -> EffectId {
    EffectId {
        run: RUN,
        seq: EffectSeq(seq),
    }
}

// ---------------------------------------------------------------------------
// Row 1: matching schedule + completion → return the recorded result.
// ---------------------------------------------------------------------------

#[test]
fn a_matching_schedule_and_completion_returns_the_recorded_result() {
    let mut machine = started();

    assert_eq!(
        machine.intent(REQUEST, Next::Record(schedule(0))),
        Ok(Intent::Recorded { id: effect(0) })
    );
    assert_eq!(
        machine.outcome(Next::Record(RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"out",
        })),
        Ok(Resolve::Replayed {
            id: effect(0),
            outcome: Outcome::Completed(b"out"),
        })
    );
    // The effect is resolved, so the next boundary is a new effect rather than a
    // redelivery of this one.
    assert_eq!(machine.position(), Position::Replaying);
    assert_eq!(machine.pending(), None);
}

#[test]
fn a_matching_schedule_and_failure_returns_the_recorded_failure() {
    let mut machine = started();

    assert!(machine.intent(REQUEST, Next::Record(schedule(0))).is_ok());
    assert_eq!(
        machine.outcome(Next::Record(RecordRef::EffectFailed {
            seq: EffectSeq(0),
            error: b"nope",
        })),
        Ok(Resolve::Replayed {
            id: effect(0),
            outcome: Outcome::Failed(b"nope"),
        })
    );
    assert_eq!(machine.position(), Position::Replaying);
}

#[test]
fn a_replayed_result_borrows_the_page_and_nothing_else() {
    // One 512 B page for a history of any length: the machine has no lifetime parameter,
    // so it cannot be holding what the page held. Proved by overwriting the page between
    // every record, which is what a real driver does.
    let mut page = [0_u8; SCRATCH_PAGE_BYTES];
    let mut machine = started();

    for seq in 0..8_u32 {
        page.fill(0xAA);
        assert_eq!(
            machine.intent(REQUEST, Next::Record(schedule(seq))),
            Ok(Intent::Recorded { id: effect(seq) })
        );

        page.fill(u8::try_from(seq).unwrap_or(0));
        let result = page.get(..3).unwrap_or_default();
        let resolved = machine.outcome(Next::Record(RecordRef::EffectCompleted {
            seq: EffectSeq(seq),
            result,
        }));
        assert_eq!(
            resolved,
            Ok(Resolve::Replayed {
                id: effect(seq),
                outcome: Outcome::Completed(result),
            })
        );
    }
}

// ---------------------------------------------------------------------------
// Row 2: matching schedule only → redeliver under the existing effect ID.
// ---------------------------------------------------------------------------

#[test]
fn a_matching_schedule_with_no_outcome_redelivers_the_existing_effect_id() {
    let mut machine = started();

    assert_eq!(
        machine.intent(REQUEST, Next::Record(schedule(0))),
        Ok(Intent::Recorded { id: effect(0) })
    );
    assert_eq!(
        machine.outcome(Next::EndOfHistory),
        Ok(Resolve::Redeliver { id: effect(0) })
    );
    // Still unresolved: §14's redelivery contract is that the dispatcher sees the same
    // identity it was given before the reset.
    assert_eq!(machine.position(), Position::AwaitingOutcome);
    assert_eq!(machine.pending().map(|effect| effect.id), Some(effect(0)));
}

#[test]
fn a_redelivered_effect_resolves_when_its_outcome_is_finally_committed() {
    let mut machine = started();
    assert!(machine.intent(REQUEST, Next::Record(schedule(0))).is_ok());
    assert!(machine.outcome(Next::EndOfHistory).is_ok());

    // The driver dispatched, the activity answered, and the outcome record was committed.
    assert!(
        machine
            .advance(RecordRef::EffectCompleted {
                seq: EffectSeq(0),
                result: b"live",
            })
            .is_ok()
    );
    assert_eq!(machine.position(), Position::Replaying);

    // And the run carries on from there, at the next sequence.
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Ok(Intent::Schedule { id: effect(1) })
    );
}

// ---------------------------------------------------------------------------
// Row 3: end of history + a new effect call → commit a schedule, then dispatch.
// ---------------------------------------------------------------------------

#[test]
fn end_of_history_asks_for_a_schedule_record_at_the_next_sequence() {
    let mut machine = started();
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Ok(Intent::Schedule { id: effect(0) })
    );
    // Nothing was consumed and nothing was minted: §07 spends a sequence when its schedule
    // record is committed, so asking twice answers the same effect.
    assert_eq!(machine.position(), Position::Replaying);
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Ok(Intent::Schedule { id: effect(0) })
    );
}

#[test]
fn a_committed_schedule_moves_the_run_on_to_the_next_sequence() {
    let mut machine = started();
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Ok(Intent::Schedule { id: effect(0) })
    );
    assert!(machine.advance(schedule(0)).is_ok());
    assert!(
        machine
            .advance(RecordRef::EffectCompleted {
                seq: EffectSeq(0),
                result: b"ok",
            })
            .is_ok()
    );
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Ok(Intent::Schedule { id: effect(1) })
    );
}

// ---------------------------------------------------------------------------
// Row 4: a different kind, digest or sequence → stop, never guess.
// ---------------------------------------------------------------------------

#[test]
fn a_different_activity_kind_is_divergence() {
    let mut machine = started();
    let request = EffectRequest {
        kind: ActivityKind(KIND.0.wrapping_add(1)),
        ..REQUEST
    };
    assert_eq!(
        machine.intent(request, Next::Record(schedule(0))),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(machine.diverged(), Some(Divergence::Kind));
}

#[test]
fn a_different_input_length_is_divergence() {
    let mut machine = started();
    let request = EffectRequest {
        input_len: INPUT_LEN.wrapping_add(1),
        ..REQUEST
    };
    assert_eq!(
        machine.intent(request, Next::Record(schedule(0))),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(machine.diverged(), Some(Divergence::Digest));
}

#[test]
fn a_different_input_checksum_is_divergence() {
    // The length alone is not the digest: two calls of the same size with different bytes
    // are exactly what a changed workflow produces.
    let mut machine = started();
    let request = EffectRequest {
        input_crc: INPUT_CRC ^ 1,
        ..REQUEST
    };
    assert_eq!(
        machine.intent(request, Next::Record(schedule(0))),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(machine.diverged(), Some(Divergence::Digest));
}

#[test]
fn a_schedule_at_the_wrong_sequence_is_divergence() {
    let mut machine = started();
    assert_eq!(
        machine.intent(REQUEST, Next::Record(schedule(1))),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(machine.diverged(), Some(Divergence::Sequence));
}

#[test]
fn a_schedule_from_another_run_is_divergence() {
    // Two banks means a record from the previous generation is a thing that can be read.
    let other = EffectId {
        run: RunId(RUN.0.wrapping_add(1)),
        seq: EffectSeq(0),
    };
    assert_eq!(
        REQUEST.divergence_from(
            &PendingEffect {
                id: other,
                kind: KIND,
                input_len: INPUT_LEN,
                input_crc: INPUT_CRC,
            },
            effect(0)
        ),
        Some(Divergence::Sequence)
    );
}

#[test]
fn divergence_is_reported_in_position_order() {
    // A record that differs in every field reports the sequence, because a comparison of
    // kind or digest against a record from another position is a comparison of two
    // unrelated effects.
    let request = EffectRequest {
        kind: ActivityKind(1),
        input_len: 1,
        input_crc: 1,
    };
    let recorded = PendingEffect {
        id: effect(3),
        kind: ActivityKind(2),
        input_len: 2,
        input_crc: 2,
    };
    assert_eq!(
        request.divergence_from(&recorded, effect(0)),
        Some(Divergence::Sequence)
    );
    // At the right position, the kind is what a reader is told about first: a digest
    // comparison between two different activities says nothing useful.
    assert_eq!(
        request.divergence_from(
            &PendingEffect {
                id: effect(0),
                ..recorded
            },
            effect(0)
        ),
        Some(Divergence::Kind)
    );
}

#[test]
fn a_matching_request_diverges_from_nothing() {
    assert_eq!(
        REQUEST.divergence_from(
            &PendingEffect {
                id: effect(0),
                kind: KIND,
                input_len: INPUT_LEN,
                input_crc: INPUT_CRC,
            },
            effect(0)
        ),
        None
    );
}

#[test]
fn every_divergence_has_its_own_message() {
    let messages = [
        Divergence::Sequence.message(),
        Divergence::Kind.message(),
        Divergence::Digest.message(),
    ];
    for (left_index, left) in messages.iter().enumerate() {
        assert!(!left.is_empty());
        for (right_index, right) in messages.iter().enumerate() {
            assert_eq!(left_index == right_index, left == right, "{left}");
        }
    }
}

// ---------------------------------------------------------------------------
// Divergence is terminal: no reinterpretation, no best-effort recovery.
// ---------------------------------------------------------------------------

#[test]
fn divergence_is_sticky_and_keeps_its_first_diagnosis() {
    let mut machine = started();
    let wrong_kind = EffectRequest {
        kind: ActivityKind(99),
        ..REQUEST
    };
    assert_eq!(
        machine.intent(wrong_kind, Next::Record(schedule(0))),
        Err(KernelError::NondeterministicWorkflow)
    );

    // A second, *matching* request does not talk the machine round.
    assert_eq!(
        machine.intent(REQUEST, Next::Record(schedule(0))),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(
        machine.outcome(Next::Record(RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"out",
        })),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(
        machine.advance(schedule(0)),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(machine.diverged(), Some(Divergence::Kind));
}

#[test]
fn a_diverging_replay_never_dispatches_an_effect() {
    // The property the whole module exists to hold, stated the way a driver would observe
    // it: every identity the engine ever hands out is counted, and a divergent history
    // produces none.
    let mut dispatched: Vec<EffectId> = Vec::new();
    let mut machine = started();

    // Two effects replay cleanly and dispatch nothing: history already answered them.
    for seq in 0..2_u32 {
        match machine.intent(REQUEST, Next::Record(schedule(seq))) {
            Ok(Intent::Recorded { .. }) => {}
            other => panic!("expected a recorded intent, got {other:?}"),
        }
        match machine.outcome(Next::Record(RecordRef::EffectCompleted {
            seq: EffectSeq(seq),
            result: b"out",
        })) {
            Ok(Resolve::Replayed { .. }) => {}
            other => panic!("expected a replayed outcome, got {other:?}"),
        }
    }

    // The third boundary is where the workflow changed. Anything the engine returns that
    // carries an identity is a dispatch.
    let changed = EffectRequest {
        kind: ActivityKind(123),
        ..REQUEST
    };
    for _ in 0..4 {
        match machine.intent(changed, Next::Record(schedule(2))) {
            Ok(Intent::Schedule { id } | Intent::Recorded { id }) => dispatched.push(id),
            Ok(Intent::Finished { .. }) | Err(_) => {}
        }
        match machine.outcome(Next::EndOfHistory) {
            Ok(Resolve::Redeliver { id } | Resolve::Replayed { id, .. }) => dispatched.push(id),
            Err(_) => {}
        }
    }

    assert!(
        dispatched.is_empty(),
        "dispatched after divergence: {dispatched:?}"
    );
    assert_eq!(machine.diverged(), Some(Divergence::Kind));
    // And history was not consumed: the cursor stands exactly where the divergence found
    // it, so a diagnosis can name the record that disagreed.
    assert_eq!(machine.position(), Position::Replaying);
}

// ---------------------------------------------------------------------------
// Row 5: a terminal workflow record → return the stored outcome, poll no further.
// ---------------------------------------------------------------------------

#[test]
fn a_terminal_completion_ends_the_run_at_an_effect_boundary() {
    let mut machine = started();
    assert_eq!(
        machine.intent(
            REQUEST,
            Next::Record(RecordRef::RunCompleted { result: b"done" })
        ),
        Ok(Intent::Finished {
            outcome: Outcome::Completed(b"done"),
        })
    );
    assert_eq!(machine.position(), Position::RunCompleted);
    assert!(machine.position().is_terminal());
}

#[test]
fn a_terminal_failure_ends_the_run_at_an_effect_boundary() {
    let mut machine = started();
    assert_eq!(
        machine.intent(
            REQUEST,
            Next::Record(RecordRef::RunFailed { error: b"bad" })
        ),
        Ok(Intent::Finished {
            outcome: Outcome::Failed(b"bad"),
        })
    );
    assert_eq!(machine.position(), Position::RunFailed);
}

#[test]
fn a_finished_run_is_never_polled_again() {
    let mut machine = started();
    assert!(
        machine
            .intent(
                REQUEST,
                Next::Record(RecordRef::RunCompleted { result: b"done" })
            )
            .is_ok()
    );
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(
        machine.intent(REQUEST, Next::Record(schedule(0))),
        Err(KernelError::NondeterministicWorkflow)
    );
}

// ---------------------------------------------------------------------------
// The protocol around the table: what a driver may not do.
// ---------------------------------------------------------------------------

#[test]
fn an_effect_boundary_cannot_be_opened_before_the_run_starts() {
    let mut machine = ReplayMachine::new(RUN);
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(machine.position(), Position::BeforeRun);
}

#[test]
fn an_outcome_cannot_be_taken_without_an_open_boundary() {
    let mut machine = started();
    assert_eq!(
        machine.outcome(Next::EndOfHistory),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn a_second_boundary_cannot_open_while_one_is_unresolved() {
    let mut machine = started();
    assert!(machine.intent(REQUEST, Next::Record(schedule(0))).is_ok());
    assert_eq!(
        machine.intent(REQUEST, Next::Record(schedule(1))),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn history_cannot_be_pumped_while_a_boundary_is_open() {
    let mut machine = started();
    assert!(machine.intent(REQUEST, Next::Record(schedule(0))).is_ok());
    assert_eq!(
        machine.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"out",
        }),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn a_record_that_could_not_follow_halts_the_machine() {
    let mut machine = started();
    // An outcome with no schedule before it is malformed history, not divergence: every
    // frame decoded, and it is how they sit beside each other that is impossible.
    assert_eq!(
        machine.intent(
            REQUEST,
            Next::Record(RecordRef::EffectCompleted {
                seq: EffectSeq(0),
                result: b"out",
            })
        ),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        machine.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
    assert_eq!(machine.diverged(), None);
    // Sticky, and the diagnosis a driver reports is the one that stopped it.
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        machine.outcome(Next::EndOfHistory),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_malformed_outcome_keeps_its_diagnosis_through_the_half_open_boundary() {
    // A completion naming an effect that is not the open one. Every frame decoded; it is
    // how they sit beside each other that is impossible, so this is `MalformedHistory` and
    // not divergence.
    //
    // The half-open boundary is what makes this worth a test of its own. The machine has
    // refusals of its own for a boundary left open, and if one of them answered here a
    // driver would be told the *workflow* changed when the *journal* is damaged — two
    // faults with two causes, and a log line that confused them sends an engineer to the
    // wrong place.
    let mut machine = started();
    assert!(machine.intent(REQUEST, Next::Record(schedule(0))).is_ok());
    assert_eq!(
        machine.outcome(Next::Record(RecordRef::EffectCompleted {
            seq: EffectSeq(1),
            result: b"out",
        })),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        machine.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
    assert_eq!(machine.diverged(), None);

    // Every entry point, because each has a refusal of its own that could have won.
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        machine.outcome(Next::EndOfHistory),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        machine.advance(schedule(1)),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_second_schedule_while_an_effect_is_unresolved_halts_the_machine() {
    // The other way an open boundary meets an impossible record: history holding two
    // schedules with no outcome between them.
    let mut machine = started();
    assert!(machine.intent(REQUEST, Next::Record(schedule(0))).is_ok());
    assert_eq!(
        machine.outcome(Next::Record(schedule(1))),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        machine.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
    assert_eq!(machine.diverged(), None);
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_terminal_record_while_an_effect_is_unresolved_halts_the_machine() {
    let mut machine = started();
    assert!(machine.intent(REQUEST, Next::Record(schedule(0))).is_ok());
    assert_eq!(
        machine.outcome(Next::Record(RecordRef::RunCompleted { result: b"done" })),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        machine.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
    assert_eq!(
        machine.advance(RecordRef::RunFailed { error: b"x" }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn the_machine_reports_the_run_it_replays() {
    let machine = ReplayMachine::new(RUN);
    assert_eq!(machine.run(), RUN);
    assert_eq!(machine.position(), Position::BeforeRun);
    assert_eq!(machine.pending(), None);
    assert_eq!(machine.diverged(), None);
}
