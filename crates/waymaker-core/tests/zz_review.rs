//! Throwaway adversarial tests. Delete.

use waymaker_core::replay::Position;
use waymaker_core::transition::{
    Divergence, EffectRequest, Intent, Next, Outcome, ReplayMachine, Resolve,
};
use waymaker_core::{ActivityKind, EffectId, EffectSeq, KernelError, RecordRef, RunId};

const RUN: RunId = RunId(0x0102_0304_0506_0708);
const KIND: ActivityKind = ActivityKind(7);
const REQUEST: EffectRequest = EffectRequest {
    kind: KIND,
    input_len: 4,
    input_crc: 0xDEAD_BEEF,
};

fn started() -> ReplayMachine {
    let mut machine = ReplayMachine::new(RUN);
    assert!(machine
        .advance(RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 2,
            input: b"input",
        })
        .is_ok());
    machine
}

const fn schedule(seq: u32) -> RecordRef<'static> {
    RecordRef::EffectScheduled {
        seq: EffectSeq(seq),
        kind: KIND,
        input_len: 4,
        input_crc: 0xDEAD_BEEF,
    }
}

const fn effect(seq: u32) -> EffectId {
    EffectId { run: RUN, seq: EffectSeq(seq) }
}

/// DEFECT A: a malformed record handed to `outcome` halts the cursor but leaves the
/// machine's phase at `AwaitingOutcome` for ever.
#[test]
fn a_wedged_boundary_after_outcome_halts_the_cursor() {
    let mut machine = started();
    assert_eq!(
        machine.intent(REQUEST, Next::Record(schedule(0))),
        Ok(Intent::Recorded { id: effect(0) })
    );
    // History: RunStarted, Sched(0), RunCompleted -- terminal while an effect is
    // unresolved, i.e. a corrupt journal.
    assert_eq!(
        machine.outcome(Next::Record(RecordRef::RunCompleted { result: b"done" })),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(machine.position(), Position::Halted(KernelError::MalformedHistory));
    assert_eq!(machine.diverged(), None);
    // The cursor's diagnosis is now masked: every later call reports the wrong fault.
    assert_eq!(
        machine.advance(RecordRef::RunCompleted { result: b"done" }),
        Err(KernelError::NondeterministicWorkflow),
        "advance should report the halt (MalformedHistory), not a workflow divergence"
    );
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Err(KernelError::NondeterministicWorkflow),
        "intent should report the halt (MalformedHistory)"
    );
    // And the branch commented "Unreachable" in `outcome` is now reachable:
    assert_eq!(machine.pending(), None);
    assert_eq!(machine.outcome(Next::EndOfHistory), Err(KernelError::MalformedHistory));
}

/// Same wedge, reached by an outcome record naming the wrong sequence.
#[test]
fn a_wedged_boundary_from_a_mismatched_outcome_sequence() {
    let mut machine = started();
    assert_eq!(
        machine.intent(REQUEST, Next::Record(schedule(0))),
        Ok(Intent::Recorded { id: effect(0) })
    );
    assert_eq!(
        machine.outcome(Next::Record(RecordRef::EffectCompleted {
            seq: EffectSeq(9),
            result: b"out",
        })),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(machine.position(), Position::Halted(KernelError::MalformedHistory));
    assert_eq!(
        machine.advance(schedule(1)),
        Err(KernelError::NondeterministicWorkflow)
    );
}

/// DEFECT B: `NondeterministicWorkflow` from the position gate is not sticky, and an
/// EffectId is handed out after it.
#[test]
fn an_effect_id_is_handed_out_after_a_nondeterministic_refusal() {
    let mut machine = started();
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Ok(Intent::Schedule { id: effect(0) })
    );
    assert!(machine.advance(schedule(0)).is_ok());
    // The workflow opens a second boundary while effect 0 is unresolved.
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Err(KernelError::NondeterministicWorkflow)
    );
    // ...and the machine does not remember it.
    assert_eq!(machine.diverged(), None);
    assert!(machine
        .advance(RecordRef::EffectCompleted { seq: EffectSeq(0), result: b"r" })
        .is_ok());
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Ok(Intent::Schedule { id: effect(1) }),
        "an identity is minted after a NondeterministicWorkflow refusal"
    );
}

/// Redeliver -> live completion: is the allocator/cursor consistent afterwards?
#[test]
fn redeliver_then_live_completion_is_consistent() {
    let mut machine = started();
    assert_eq!(
        machine.intent(REQUEST, Next::Record(schedule(0))),
        Ok(Intent::Recorded { id: effect(0) })
    );
    assert_eq!(
        machine.outcome(Next::EndOfHistory),
        Ok(Resolve::Redeliver { id: effect(0) })
    );
    assert_eq!(machine.position(), Position::AwaitingOutcome);
    assert!(machine
        .advance(RecordRef::EffectCompleted { seq: EffectSeq(0), result: b"live" })
        .is_ok());
    assert_eq!(machine.position(), Position::Replaying);
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Ok(Intent::Schedule { id: effect(1) })
    );
}

/// A terminal record consumed at a boundary while the workflow asked for a new effect.
#[test]
fn a_terminal_record_swallows_a_divergent_request() {
    let mut machine = started();
    assert_eq!(
        machine.intent(
            EffectRequest { kind: ActivityKind(99), input_len: 0, input_crc: 0 },
            Next::Record(RecordRef::RunCompleted { result: b"done" })
        ),
        Ok(Intent::Finished { outcome: Outcome::Completed(b"done") })
    );
    assert_eq!(machine.diverged(), None);
}

/// Can a divergence be reached and then an id handed out? (expected: no)
#[test]
fn divergence_is_sticky() {
    let mut machine = started();
    assert_eq!(
        machine.intent(
            EffectRequest { kind: ActivityKind(99), input_len: 4, input_crc: 0xDEAD_BEEF },
            Next::Record(schedule(0))
        ),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(machine.diverged(), Some(Divergence::Kind));
    assert_eq!(machine.position(), Position::Replaying);
    assert_eq!(
        machine.intent(REQUEST, Next::EndOfHistory),
        Err(KernelError::NondeterministicWorkflow)
    );
    assert_eq!(machine.advance(schedule(0)), Err(KernelError::NondeterministicWorkflow));
    assert_eq!(machine.outcome(Next::EndOfHistory), Err(KernelError::NondeterministicWorkflow));
    assert_eq!(machine.diverged(), Some(Divergence::Kind));
}
