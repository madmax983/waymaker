//! The four crash-point defects Codex's first review of pull request #63 found.
//!
//! Kept in a file of their own so that the regression is legible: each of these passed
//! every other test in this crate, and each is a way the sweep quietly stopped covering
//! what it says it covers.

use std::cell::Cell;

use waymaker_fault::{
    Durability, FaultError, Harness, HarnessError, Injection, Interruption, Op, Progress, RecordId,
    Run, Session, verify_recovery,
};
use waymaker_flash::storage::{Geometry, StableStorage};

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(64, 32, 4, 1) else {
        unreachable!("64 is two 32-byte blocks of eight 4-byte units of single bytes")
    };
    geometry
}

fn run_one(
    injection: Injection,
    writer: impl FnMut(&mut Session) -> Result<(), FaultError>,
) -> Run {
    match Harness::new(geometry()).run_one(injection, writer) {
        Ok(run) => run,
        Err(error) => unreachable!("{error}"),
    }
}

#[test]
fn power_lost_after_a_barrier_returned_lets_the_writer_act_on_that_return() {
    // A barrier whose `Whole` crash point returned `Err` to its caller is not "power lost
    // after the barrier returned" — it is a barrier that failed. The difference is the
    // whole of design document §02 decision 3: a writer does `barrier()?` and *then*
    // dispatches, so a barrier that hands back an error means the dispatch never happens,
    // and the one crash point where an effect is in flight when the power goes is missing
    // from every sweep.
    let dispatched = Cell::new(false);
    let run = run_one(
        Injection {
            op: 1,
            progress: Progress::Whole,
            interruption: Interruption::PowerLoss,
        },
        |session| {
            dispatched.set(false);
            session.begin_record(RecordId(0));
            session.program(0, &[0xA0; 4])?;
            session.barrier()?;
            // The intent is durable, so the effect may happen. The power goes away here.
            dispatched.set(true);
            session.program(4, &[0xB0; 4])
        },
    );

    assert!(
        dispatched.get(),
        "the barrier returned, so the writer must have gone on to dispatch"
    );
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::Acknowledged)
    );
    // The write after the dispatch never reached media: the power was already gone.
    assert_eq!(
        run.image().get(..8),
        Some(&[0xA0, 0xA0, 0xA0, 0xA0, 0xFF, 0xFF, 0xFF, 0xFF][..])
    );
    assert_eq!(run.ops(), [Op::Program { offset: 0, len: 4 }, Op::Barrier]);
}

#[test]
fn a_completed_program_followed_by_power_loss_also_returns_before_it_dies() {
    let reached = Cell::new(false);
    let run = run_one(
        Injection {
            op: 0,
            progress: Progress::Whole,
            interruption: Interruption::PowerLoss,
        },
        |session| {
            reached.set(false);
            session.begin_record(RecordId(0));
            session.program(0, &[0xA0; 4])?;
            reached.set(true);
            session.barrier()
        },
    );
    assert!(reached.get(), "the program completed, so it returned `Ok`");
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable),
        "the barrier after it never ran, so nothing is acknowledged"
    );
}

#[test]
fn a_record_missing_a_whole_write_is_torn_even_though_that_write_never_started() {
    // Two mutations in one record. The second fails having written nothing, the writer
    // ignores it, and a barrier follows. The record is on media in part and missing the
    // rest, so acknowledging it would oblige recovery to produce something incomplete.
    let run = run_one(
        Injection {
            op: 1,
            progress: Progress::None,
            interruption: Interruption::Failure,
        },
        |session| {
            session.begin_record(RecordId(0));
            session.program(0, &[0xA0; 4])?;
            let _ignored = session.program(4, &[0xB0; 4]);
            session.barrier()
        },
    );

    assert_eq!(run.image().get(4..8), Some(&[0xFF; 4][..]));
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
    );
    assert_eq!(run.ledger().torn(RecordId(0)), Some(true));
}

#[test]
fn a_write_a_retry_completed_is_not_missing_from_the_record() {
    // The other side of the same rule, and the reason tornness is judged against the media
    // at the end of the run rather than at the moment of each operation: what a failed
    // write did not put on media, a retry did.
    let run = run_one(
        Injection {
            op: 0,
            progress: Progress::None,
            interruption: Interruption::Failure,
        },
        |session| {
            session.begin_record(RecordId(0));
            if session.program(0, &[0xC0; 4]).is_err() {
                session.program(0, &[0xC0; 4])?;
            }
            session.barrier()
        },
    );
    assert_eq!(run.image().get(..4), Some(&[0xC0; 4][..]));
    assert_eq!(run.ledger().torn(RecordId(0)), Some(false));
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::Acknowledged)
    );
    assert_eq!(verify_recovery(run.ledger(), &[RecordId(0)]), Ok(()));
}

#[test]
fn a_writer_that_changes_its_payload_between_runs_is_refused() {
    // `Op` carries offsets, lengths and barriers — not the bytes. A writer that keeps the
    // shape and changes the contents is evaluating every crash point against a different
    // history from the one they were enumerated from, and the shape check alone says
    // nothing about it.
    let call = Cell::new(0_u8);
    let outcome = Harness::new(geometry()).run(|session| {
        let byte = call.get();
        call.set(byte.wrapping_add(1));
        session.begin_record(RecordId(0));
        session.program(0, &[byte; 4])?;
        session.barrier()
    });
    assert!(
        matches!(outcome, Err(HarnessError::WriterIsNotDeterministic { .. })),
        "{:?}",
        outcome.map(|runs| runs.len())
    );
}

#[test]
fn a_writer_that_renumbers_its_records_between_runs_is_refused() {
    let call = Cell::new(0_u32);
    let outcome = Harness::new(geometry()).run(|session| {
        let nth = call.get();
        call.set(nth.wrapping_add(1));
        session.begin_record(RecordId(nth));
        session.program(0, &[0xA0; 4])?;
        session.barrier()
    });
    assert!(
        matches!(outcome, Err(HarnessError::WriterIsNotDeterministic { .. })),
        "{:?}",
        outcome.map(|runs| runs.len())
    );
}

#[test]
fn a_writer_that_issues_no_operations_still_sweeps() {
    // `injections(&[], geometry)` returns the one crash point that precedes everything, and
    // an empty sequence is explicitly supported. A sweep that refused it would report a
    // determinism failure for a writer that is perfectly deterministic.
    let runs = match Harness::new(geometry()).run(|_session| Ok::<(), FaultError>(())) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    };
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run.ops().is_empty()));
    assert!(runs.iter().all(|run| run.ledger().is_empty()));
    assert!(runs.iter().all(|run| run.image() == [0xFF; 64]));
}

#[test]
fn a_writer_that_only_declares_a_record_still_sweeps() {
    let runs = match Harness::new(geometry()).run(|session| {
        session.begin_record(RecordId(0));
        Ok::<(), FaultError>(())
    }) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    };
    assert_eq!(runs.len(), 2);
    for run in &runs {
        assert_eq!(run.ledger().state(RecordId(0)), Some(Durability::Attempted));
        assert_eq!(verify_recovery(run.ledger(), &[]), Ok(()));
    }
}
