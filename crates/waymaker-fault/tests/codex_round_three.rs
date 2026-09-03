//! The two defects Codex's third review of pull request #63 found.
//!
//! Both are about a crash point a caller builds by hand and hands to
//! [`Harness::run_one`](waymaker_fault::Harness::run_one). [`injections`] never produces
//! either shape, so both were invisible to a sweep — and a targeted reproduction is exactly
//! where a wrong answer costs the most, because it is the run somebody is staring at.
//!
//! [`injections`]: waymaker_fault::injections

use std::cell::Cell;

use waymaker_fault::{
    Durability, FaultError, Harness, Injection, Interruption, Progress, RecordId, Run, Session,
};
use waymaker_flash::storage::{Geometry, StableStorage};

/// Two 32-byte erase blocks, so that an erase has an interior to be rounded into.
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

/// Programs both blocks, then erases the whole device.
fn fill_then_erase(session: &mut Session) -> Result<(), FaultError> {
    session.begin_record(RecordId(0));
    session.program(0, &[0x00; 4])?;
    session.program(32, &[0x00; 4])?;
    session.erase(0, 64)
}

#[test]
fn an_erase_torn_inside_a_block_is_rounded_down_to_the_block_boundary() {
    // No device erases byte by byte, which is why `injections` offers erase tear points at
    // erase blocks and nowhere else. A hand-built `Bytes(1)` that reached the media anyway
    // would put a reset image in front of a contributor that no modelled device can
    // produce — one byte of a block back to `0xFF` and the rest of it programmed.
    let run = run_one(
        Injection {
            op: 2,
            progress: Progress::Bytes(1),
            interruption: Interruption::PowerLoss,
        },
        fill_then_erase,
    );
    assert_eq!(
        run.image().first(),
        Some(&0x00),
        "one byte is less than an erase block, so nothing came back"
    );
    assert_eq!(run.image().get(32), Some(&0x00));
}

#[test]
fn an_erase_torn_past_a_block_boundary_is_rounded_down_to_it() {
    let run = run_one(
        Injection {
            op: 2,
            progress: Progress::Bytes(33),
            interruption: Interruption::PowerLoss,
        },
        fill_then_erase,
    );
    assert_eq!(
        run.image().first(),
        Some(&0xFF),
        "the first whole block came back"
    );
    assert_eq!(
        run.image().get(32),
        Some(&0x00),
        "and the second did not, because 33 bytes is one block and a bit"
    );
}

#[test]
fn byte_progress_at_the_length_completes_the_operation_and_returns() {
    // `Progress::Bytes` documents that anything at or past the length means `Whole`, and
    // `Whole` under power loss returns `Ok(())` before the power goes. Deciding that from
    // the variant rather than from how much landed made the two disagree, so a hand-built
    // `Bytes(len)` skipped everything the writer does after the call returns.
    let reached = Cell::new(false);
    let run = run_one(
        Injection {
            op: 0,
            progress: Progress::Bytes(4),
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

    assert!(
        reached.get(),
        "every byte landed, so the call completed and returned"
    );
    assert_eq!(run.image().get(..4), Some(&[0xA0; 4][..]));
    assert_eq!(run.ledger().torn(RecordId(0)), Some(false));
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable),
        "the barrier met a device with no power, so nothing is acknowledged"
    );
}

#[test]
fn byte_progress_past_the_length_completes_the_operation_too() {
    let reached = Cell::new(false);
    let run = run_one(
        Injection {
            op: 2,
            progress: Progress::Bytes(4096),
            interruption: Interruption::PowerLoss,
        },
        |session| {
            reached.set(false);
            fill_then_erase(session)?;
            reached.set(true);
            session.barrier()
        },
    );
    assert!(reached.get(), "the erase completed and returned");
    assert_eq!(run.image(), &[0xFF; 64][..]);
}
