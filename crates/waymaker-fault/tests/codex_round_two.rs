//! The defect Codex's second review of pull request #63 found.
//!
//! Tornness was decided by asking "would programming the withheld bytes change anything".
//! That is the wrong question: programming can only clear bits, so a region that has since
//! had *more* bits cleared answers "no" — the withheld bytes cannot be told apart from
//! bytes that arrived. The right question is whether the media holds what completing the
//! write would have left, and that is an equality.

use waymaker_fault::{
    Breach, Durability, FaultError, Harness, Injection, Interruption, Progress, RecordId, Run,
    Session, verify_recovery,
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
fn a_withheld_byte_overwritten_with_fewer_bits_is_still_missing() {
    // The first write is torn after two bytes; the writer ignores the error and programs
    // `0x00` over the whole region, which clears every bit the withheld `0xF0` would have
    // left standing. Asking "would programming `0xF0` change `0x00`" answers no — and the
    // record would be acknowledged holding bytes it never wrote.
    let run = run_one(
        Injection {
            op: 0,
            progress: Progress::Bytes(2),
            interruption: Interruption::Failure,
        },
        |session| {
            session.begin_record(RecordId(0));
            let _torn = session.program(0, &[0xF0; 4]);
            session.program(0, &[0x00; 4])?;
            session.barrier()
        },
    );

    assert_eq!(run.image().get(..4), Some(&[0x00; 4][..]));
    assert_eq!(run.ledger().torn(RecordId(0)), Some(true));
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
    );
    assert_eq!(
        verify_recovery(run.ledger(), &[RecordId(0)]),
        Err(Breach::RecoveredATornRecord {
            record: RecordId(0)
        })
    );
}

#[test]
fn a_withheld_byte_written_exactly_by_a_later_write_is_not_missing() {
    // The boundary on the other side, so the fix is an equality rather than a blanket
    // "anything that touched this region tore the record".
    let run = run_one(
        Injection {
            op: 0,
            progress: Progress::Bytes(2),
            interruption: Interruption::Failure,
        },
        |session| {
            session.begin_record(RecordId(0));
            let _torn = session.program(0, &[0xF0; 4]);
            session.program(0, &[0xF0; 4])?;
            session.barrier()
        },
    );

    assert_eq!(run.image().get(..4), Some(&[0xF0; 4][..]));
    assert_eq!(run.ledger().torn(RecordId(0)), Some(false));
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::Acknowledged)
    );
    assert_eq!(verify_recovery(run.ledger(), &[RecordId(0)]), Ok(()));
}

#[test]
fn what_a_write_would_have_left_is_the_preimage_masked_by_the_bytes_it_carried() {
    // A withheld byte's expected value is not the byte the writer passed: programming ANDs,
    // so over media already holding `0x3C` a withheld `0xF0` would have left `0x30`. A model
    // that stored the argument instead would call an untouched region torn.
    let run = run_one(
        Injection {
            op: 1,
            progress: Progress::Bytes(2),
            interruption: Interruption::Failure,
        },
        |session| {
            session.program(0, &[0x3C; 4])?;
            session.begin_record(RecordId(0));
            let _torn = session.program(0, &[0xF0; 4]);
            // Finish the job by hand, leaving exactly what the torn write would have.
            session.program(0, &[0x30; 4])?;
            session.barrier()
        },
    );

    assert_eq!(run.image().get(..4), Some(&[0x30; 4][..]));
    assert_eq!(run.ledger().torn(RecordId(0)), Some(false));
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::Acknowledged)
    );
}
