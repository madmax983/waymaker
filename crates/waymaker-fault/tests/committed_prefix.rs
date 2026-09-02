//! Two unrelated writers, driven through the same unmodified harness.
//!
//! Issue [#18](https://github.com/madmax983/waymaker/issues/18)'s second exit criterion is
//! that the harness "is reusable by `waymaker-flash` and the effect-protocol tests without
//! modification". That is not a claim a doc comment can make, so this file makes it twice:
//!
//! * [`journal`] is `waymaker-flash`'s real record codec — §09 frames, encoded by
//!   `frame::encode` and recovered by `frame::Scan` — appended one record per barrier.
//! * [`effect_protocol`] is design document §11's durable-intent shape: a schedule record
//!   crosses a barrier, *then* the effect is dispatched, and only afterwards is a
//!   completion recorded.
//!
//! Neither the harness nor the injector knows that either exists. `waymaker-fault` names no
//! record type, no frame constant and no activity kind; it sees offsets, lengths and
//! barriers. `tests/harness.rs` adds a third writer with a byte layout of its own, so the
//! generality is three shapes rather than a family resemblance.
//!
//! [`journal`]: the_flash_journal_recovers_a_committed_prefix_at_every_crash_point
//! [`effect_protocol`]: no_effect_is_dispatched_without_its_intent_in_recovered_history

use std::cell::RefCell;
use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, EffectSeq, RecordRef};
use waymaker_fault::{
    Breach, Durability, FaultError, Harness, RecordId, Run, Session, verify_recovery,
};
use waymaker_flash::frame::{self, ProgramAlign, Scan};
use waymaker_flash::storage::{Geometry, StableStorage};

/// One erase block, which is the whole journal region: `Scan`'s erased-tail rule is stated
/// over the journal and nothing else, so the device and the journal are the same bytes.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 256, 4, 1) else {
        unreachable!("256 is one whole 256-byte block of 4-byte units of single bytes")
    };
    geometry
}

fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    align
}

/// The activity every schedule record below names.
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// How many effects each writer records.
const EFFECTS: u32 = 3;

/// Appends one record, padded to the program granularity.
fn append(session: &mut Session, at: &mut u32, record: &RecordRef<'_>) -> Result<(), FaultError> {
    let mut buffer = [0_u8; 64];
    let Ok(written) = frame::encode(record, align(), &mut buffer) else {
        unreachable!("64 bytes is more than any record this file writes")
    };
    let Some(bytes) = buffer.get(..written) else {
        unreachable!("`encode` reports what it wrote")
    };
    session.program(*at, bytes)?;
    *at = at.wrapping_add(u32::try_from(written).unwrap_or(u32::MAX));
    Ok(())
}

/// Every run of `writer`, or a loud failure.
///
/// A writer that gives up with no faults armed enumerates almost nothing, and every
/// assertion below would then pass over a journal that was never written.
fn drive<W, E>(writer: W) -> Vec<Run>
where
    W: FnMut(&mut Session) -> Result<(), E>,
    E: std::fmt::Debug,
{
    match Harness::new(geometry()).run(writer) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

/// Every record `Scan` can recover from `image`, as the ids the writers assigned.
///
/// A record is identified by what is *in* it — the effect sequence it names — rather than
/// by where it sits in the scan, so "recovery produced a prefix" is a statement about
/// content and not a tautology about counting.
fn recovered(image: &[u8]) -> Vec<RecordId> {
    Scan::new(image, align())
        .take_while(Result::is_ok)
        .flatten()
        .filter_map(|record| match record {
            RecordRef::RunStarted { .. } => Some(RecordId(0)),
            RecordRef::EffectScheduled { seq, .. } => {
                Some(RecordId(seq.0.wrapping_mul(2).wrapping_sub(1)))
            }
            RecordRef::EffectCompleted { seq, .. } => Some(RecordId(seq.0.wrapping_mul(2))),
            RecordRef::EffectFailed { .. }
            | RecordRef::RunCompleted { .. }
            | RecordRef::RunFailed { .. } => None,
        })
        .collect()
}

#[test]
fn the_flash_journal_recovers_a_committed_prefix_at_every_crash_point() {
    let runs = drive(|session| {
        let mut at = 0;
        for seq in 1..=EFFECTS {
            session.begin_record(RecordId(seq.wrapping_mul(2) - 1));
            append(
                session,
                &mut at,
                &RecordRef::EffectScheduled {
                    seq: EffectSeq(seq),
                    kind: DOWNLOAD,
                    input_len: 4,
                    input_crc: frame::input_digest(b"blob"),
                },
            )?;
            session.barrier()?;
        }
        Ok::<(), FaultError>(())
    });

    // The enumeration is doing real work: a three-record journal has hundreds of crash
    // points, not a handful.
    assert!(runs.len() > 100, "only {} runs", runs.len());
    assert_verdicts_hold(&runs);

    // And the fault-free run really did write all three, so the loop above is not
    // vacuously true over an empty journal.
    let clean = runs.first().expect("the fault-free run");
    assert_eq!(
        recovered(clean.image()),
        [RecordId(1), RecordId(3), RecordId(5)]
    );

    // Every prefix length actually occurs. Without this the loop above could be passing
    // because every crash point happened to recover the same thing.
    let lengths: BTreeSet<usize> = runs
        .iter()
        .map(|run| recovered(run.image()).len())
        .collect();
    assert_eq!(lengths, BTreeSet::from([0, 1, 2, 3]));

    // And acknowledgment is a real obligation somewhere: some run had to keep a record it
    // had promised while losing one it had not.
    assert!(runs.iter().any(|run| {
        let history = recovered(run.image());
        run.ledger().acknowledged().count() > 0 && history.len() < 3
    }));
}

#[test]
fn a_recovery_that_drops_its_last_record_is_caught_wherever_that_record_was_acknowledged() {
    // The harness has to be able to fail on the property it exists to check, not only on
    // the ones a unit test can construct by hand. This drives the real journal writer and
    // then lies about what recovery found, by one record, and asserts that at least one
    // crash point calls it.
    let runs = drive(|session| {
        let mut at = 0;
        for seq in 1..=EFFECTS {
            session.begin_record(RecordId(seq.wrapping_mul(2) - 1));
            append(
                session,
                &mut at,
                &RecordRef::EffectScheduled {
                    seq: EffectSeq(seq),
                    kind: DOWNLOAD,
                    input_len: 4,
                    input_crc: frame::input_digest(b"blob"),
                },
            )?;
            session.barrier()?;
        }
        Ok::<(), FaultError>(())
    });

    let caught: Vec<Breach> = runs
        .iter()
        .filter_map(|run| {
            let mut history = recovered(run.image());
            history.pop();
            verify_recovery(run.ledger(), &history).err()
        })
        .collect();

    assert!(
        caught
            .iter()
            .any(|breach| matches!(breach, Breach::LostAnAcknowledgedRecord { .. })),
        "a recovery short by one record was accepted at every crash point: {caught:?}"
    );
}

#[test]
fn no_effect_is_dispatched_without_its_intent_in_recovered_history() {
    // Design document §02 decision 3: "the schedule record crosses a durability barrier
    // before dispatch. A physical effect never precedes its committed intent." The writer
    // below dispatches only after `barrier` returned, and this asserts that at every crash
    // point the recovered history still has the schedule record for everything dispatched.
    let dispatches: RefCell<Vec<Vec<u32>>> = RefCell::new(Vec::new());

    let runs = drive(|session| {
        dispatches.borrow_mut().push(Vec::new());
        let mut at = 0;

        session.begin_record(RecordId(0));
        append(
            session,
            &mut at,
            &RecordRef::RunStarted {
                workflow_kind: 7,
                workflow_version: 1,
                input: b"go",
            },
        )?;
        session.barrier()?;

        for seq in 1..=EFFECTS {
            session.begin_record(RecordId(seq.wrapping_mul(2) - 1));
            append(
                session,
                &mut at,
                &RecordRef::EffectScheduled {
                    seq: EffectSeq(seq),
                    kind: DOWNLOAD,
                    input_len: 4,
                    input_crc: frame::input_digest(b"blob"),
                },
            )?;
            session.barrier()?;

            // The barrier returned, so the intent is durable and the effect may happen.
            if let Some(run) = dispatches.borrow_mut().last_mut() {
                run.push(seq);
            }

            session.begin_record(RecordId(seq.wrapping_mul(2)));
            append(
                session,
                &mut at,
                &RecordRef::EffectCompleted {
                    seq: EffectSeq(seq),
                    result: b"ok",
                },
            )?;
            session.barrier()?;
        }
        Ok::<(), FaultError>(())
    });

    assert_verdicts_hold(&runs);

    let log = dispatches.borrow();
    assert_eq!(log.len(), runs.len(), "one dispatch log per run");
    let mut dispatched_at_all = 0;
    for (run, dispatched) in runs.iter().zip(log.iter()) {
        let history = recovered(run.image());
        for seq in dispatched {
            dispatched_at_all += 1;
            let intent = RecordId(seq.wrapping_mul(2) - 1);
            assert!(
                history.contains(&intent),
                "effect {seq} was dispatched but {intent:?} is not in {history:?} after \
                 {:?}",
                run.injection()
            );
        }
    }
    assert!(
        dispatched_at_all > 0,
        "no run dispatched anything, so the invariant held vacuously"
    );
}

#[test]
fn a_writer_that_forgets_its_barrier_is_caught_by_the_oracle() {
    // The harness has to be able to fail. This writer records two records and never
    // orders the first, so at the crash point that tears the second write the first is
    // only possibly durable — and a *claim* that it was acknowledged is a breach the
    // oracle names.
    let runs = drive(|session| {
        let mut at = 0;
        session.begin_record(RecordId(1));
        append(
            session,
            &mut at,
            &RecordRef::EffectScheduled {
                seq: EffectSeq(1),
                kind: DOWNLOAD,
                input_len: 0,
                input_crc: 0,
            },
        )?;
        session.begin_record(RecordId(3));
        append(
            session,
            &mut at,
            &RecordRef::EffectScheduled {
                seq: EffectSeq(2),
                kind: DOWNLOAD,
                input_len: 0,
                input_crc: 0,
            },
        )?;
        Ok::<(), FaultError>(())
    });

    let clean = runs.first().expect("the fault-free run");
    assert_eq!(
        clean.ledger().state(RecordId(1)),
        Some(Durability::PossiblyDurable),
        "no barrier was ever issued, so nothing can be acknowledged"
    );
    // Recovery losing an unacknowledged record is legal; inventing one is not.
    assert_eq!(verify_recovery(clean.ledger(), &[]), Ok(()));

    let never_started = runs
        .iter()
        .find(|run| run.ledger().state(RecordId(3)) == Some(Durability::Attempted))
        .expect("some crash point stops before the second record starts");
    assert_eq!(
        verify_recovery(never_started.ledger(), &[RecordId(1), RecordId(3)]),
        Err(Breach::RecoveredWhatWasNeverAttempted {
            record: RecordId(3)
        })
    );
}

/// Every run of `runs` recovers a legal prefix of its own committed history.
fn assert_verdicts_hold(runs: &[Run]) {
    for run in runs {
        let history = recovered(run.image());
        assert_eq!(
            verify_recovery(run.ledger(), &history),
            Ok(()),
            "{:?} recovered {history:?} from a ledger of {:?}",
            run.injection(),
            run.ledger().records().collect::<Vec<_>>()
        );
    }
}
