//! The harness: one run per crash point, and the three record states §15 asks for.
//!
//! Design document §15: "the model distinguishes records that were merely attempted,
//! records that may have become durable before acknowledgment, and records whose barrier
//! returned. Recovery may include an unacknowledged complete record, but it may never lose
//! an acknowledged one."

use std::cell::{Cell, RefCell};

use waymaker_fault::{
    Breach, Durability, FaultError, Harness, HarnessError, Injection, Interruption, Ledger,
    OneWayBits, Op, Progress, RecordId, Run, Session, verify_recovery,
};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(64, 32, 4, 1) else {
        unreachable!("64 is two 32-byte blocks of eight 4-byte units of single bytes")
    };
    geometry
}

/// Two records, each one program followed by a barrier.
fn two_records(session: &mut Session) -> Result<(), FaultError> {
    session.begin_record(RecordId(0));
    session.program(0, &[0xA0, 0xA1, 0xA2, 0xA3])?;
    session.barrier()?;
    session.begin_record(RecordId(1));
    session.program(4, &[0xB0, 0xB1, 0xB2, 0xB3])?;
    session.barrier()
}

fn runs() -> Vec<Run> {
    drive(two_records)
}

/// Every run of `writer`, or a loud failure.
///
/// A writer that gives up with no faults armed enumerates almost nothing, and every
/// assertion over the result then passes for the wrong reason — which is what
/// [`HarnessError`] exists to stop, and why these helpers refuse rather than default.
fn drive<W, E>(writer: W) -> Vec<Run>
where
    W: FnMut(&mut Session) -> Result<(), E>,
    E: std::fmt::Debug,
{
    drive_with(OneWayBits::Absorbed, writer)
}

fn drive_with<W, E>(rule: OneWayBits, writer: W) -> Vec<Run>
where
    W: FnMut(&mut Session) -> Result<(), E>,
    E: std::fmt::Debug,
{
    match Harness::with_bit_rule(geometry(), rule).run(writer) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

#[test]
fn the_first_run_is_the_fault_free_one_and_every_other_run_carries_one_injection() {
    let runs = runs();
    assert_eq!(runs.first().and_then(Run::injection), None);
    assert!(runs.iter().skip(1).all(|run| run.injection().is_some()));

    // Exactly one run per crash point, plus the fault-free run.
    let expected =
        waymaker_fault::injections(runs.first().expect("a fault-free run").ops(), geometry());
    assert_eq!(runs.len(), expected.len() + 1);
    let carried: Vec<Injection> = runs.iter().skip(1).filter_map(Run::injection).collect();
    assert_eq!(carried, expected);
}

#[test]
fn the_fault_free_run_writes_everything_and_acknowledges_both_records() {
    let runs = runs();
    let clean = runs.first().expect("a fault-free run");
    assert_eq!(
        clean.image().get(..8),
        Some(&[0xA0, 0xA1, 0xA2, 0xA3, 0xB0, 0xB1, 0xB2, 0xB3][..])
    );
    assert_eq!(
        clean.ledger().state(RecordId(0)),
        Some(Durability::Acknowledged)
    );
    assert_eq!(
        clean.ledger().state(RecordId(1)),
        Some(Durability::Acknowledged)
    );
    assert_eq!(
        clean.ops(),
        [
            Op::Program { offset: 0, len: 4 },
            Op::Barrier,
            Op::Program { offset: 4, len: 4 },
            Op::Barrier,
        ]
    );
}

#[test]
fn a_record_whose_first_byte_never_landed_is_merely_attempted() {
    let run = one_run(Injection {
        op: 0,
        progress: Progress::None,
        interruption: Interruption::PowerLoss,
    });
    assert_eq!(run.ledger().state(RecordId(0)), Some(Durability::Attempted));
    assert_eq!(run.image(), &[0xFF; 64][..]);
}

#[test]
fn a_record_torn_mid_write_is_possibly_durable_and_never_acknowledged() {
    let run = one_run(Injection {
        op: 0,
        progress: Progress::Bytes(2),
        interruption: Interruption::PowerLoss,
    });
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
    );
    assert_eq!(run.image().get(..4), Some(&[0xA0, 0xA1, 0xFF, 0xFF][..]));
}

#[test]
fn a_record_written_whole_but_not_yet_ordered_is_possibly_durable() {
    let run = one_run(Injection {
        op: 0,
        progress: Progress::Whole,
        interruption: Interruption::PowerLoss,
    });
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
    );
    assert_eq!(run.image().get(..4), Some(&[0xA0, 0xA1, 0xA2, 0xA3][..]));
}

#[test]
fn power_lost_after_a_barrier_returned_leaves_that_record_acknowledged() {
    let run = one_run(Injection {
        op: 1,
        progress: Progress::Whole,
        interruption: Interruption::PowerLoss,
    });
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::Acknowledged)
    );
    // The barrier returned, so the writer went on and declared the second record. Its write
    // met a device with no power left, so none of it reached media.
    assert_eq!(run.ledger().state(RecordId(1)), Some(Durability::Attempted));
    assert_eq!(run.ops(), [Op::Program { offset: 0, len: 4 }, Op::Barrier]);
    assert_eq!(
        verify_recovery(run.ledger(), &[RecordId(0)]),
        Ok(()),
        "the acknowledged record is required and the attempted one is not"
    );
}

#[test]
fn a_barrier_that_returns_an_error_acknowledges_nothing() {
    // The writer sees an error from `barrier` and stops. Whether the ordering was really
    // established is unknowable to it, so the model must not promise recovery.
    let run = one_run(Injection {
        op: 1,
        progress: Progress::None,
        interruption: Interruption::Failure,
    });
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
    );
}

#[test]
fn after_power_loss_nothing_else_reaches_media_however_hard_the_writer_tries() {
    let stubborn = drive(|session| {
        session.begin_record(RecordId(0));
        // Ignore the first failure entirely and keep writing, as a retry loop would.
        let first = session.program(0, &[0x00; 4]);
        let second = session.program(4, &[0x00; 4]);
        let third = session.barrier();
        match (first, second, third) {
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
            _ => Ok(()),
        }
    });

    let lost = stubborn
        .iter()
        .find(|run| {
            run.injection()
                == Some(Injection {
                    op: 0,
                    progress: Progress::Bytes(1),
                    interruption: Interruption::PowerLoss,
                })
        })
        .expect("a torn first write is among the crash points");
    assert_eq!(
        lost.image().get(..8),
        Some(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF][..])
    );
    assert_eq!(lost.ops(), [Op::Program { offset: 0, len: 4 }]);
}

#[test]
fn an_injected_failure_lets_the_writer_carry_on_and_the_next_op_is_recorded() {
    let retrying = drive(|session| {
        session.begin_record(RecordId(0));
        if session.program(0, &[0xC0; 4]).is_err() {
            session.program(0, &[0xC0; 4])?;
        }
        session.barrier()
    });

    let retried = retrying
        .iter()
        .find(|run| {
            run.injection()
                == Some(Injection {
                    op: 0,
                    progress: Progress::None,
                    interruption: Interruption::Failure,
                })
        })
        .expect("a failed first program is among the crash points");
    assert_eq!(retried.ops().len(), 3, "{:?}", retried.ops());
    assert_eq!(retried.image().get(..4), Some(&[0xC0; 4][..]));
    assert_eq!(
        retried.ledger().state(RecordId(0)),
        Some(Durability::Acknowledged)
    );
}

#[test]
fn operations_issued_before_the_first_record_belong_to_no_record() {
    let prepared = drive(|session| {
        session.erase(0, 32)?;
        session.begin_record(RecordId(7));
        session.program(0, &[0x11; 4])?;
        session.barrier()
    });
    let clean = prepared.first().expect("a fault-free run");
    assert_eq!(clean.ledger().order().collect::<Vec<_>>(), [RecordId(7)]);
    assert_eq!(
        clean.ledger().state(RecordId(7)),
        Some(Durability::Acknowledged)
    );
}

#[test]
fn the_oracle_accepts_a_prefix_and_rejects_a_lost_acknowledgment() {
    let runs = runs();
    let clean = runs.first().expect("a fault-free run");

    assert_eq!(
        verify_recovery(clean.ledger(), &[]),
        Err(Breach::LostAnAcknowledgedRecord {
            record: RecordId(0)
        })
    );
    assert_eq!(
        verify_recovery(clean.ledger(), &[RecordId(0), RecordId(1)]),
        Ok(())
    );
    assert_eq!(
        verify_recovery(clean.ledger(), &[RecordId(1), RecordId(0)]),
        Err(Breach::NotAPrefix {
            position: 0,
            expected: Some(RecordId(0)),
            found: RecordId(1),
        })
    );
    assert_eq!(
        verify_recovery(clean.ledger(), &[RecordId(0), RecordId(1), RecordId(2)]),
        Err(Breach::NotAPrefix {
            position: 2,
            expected: None,
            found: RecordId(2),
        })
    );
}

#[test]
fn the_oracle_rejects_a_record_that_was_never_attempted() {
    let run = one_run(Injection {
        op: 0,
        progress: Progress::None,
        interruption: Interruption::PowerLoss,
    });
    assert_eq!(
        verify_recovery(run.ledger(), &[RecordId(0)]),
        Err(Breach::RecoveredWhatWasNeverAttempted {
            record: RecordId(0)
        })
    );
    assert_eq!(verify_recovery(run.ledger(), &[]), Ok(()));
}

#[test]
fn the_oracle_accepts_an_unacknowledged_record_either_way() {
    // §15: "recovery may include an unacknowledged complete record, but it may never lose
    // an acknowledged one". Both answers are legal for a possibly-durable record.
    let run = one_run(Injection {
        op: 0,
        progress: Progress::Whole,
        interruption: Interruption::PowerLoss,
    });
    assert_eq!(verify_recovery(run.ledger(), &[]), Ok(()));
    assert_eq!(verify_recovery(run.ledger(), &[RecordId(0)]), Ok(()));
}

#[test]
fn the_oracle_fails_closed_on_a_ledger_that_names_one_record_twice() {
    let confused = drive(|session| {
        session.begin_record(RecordId(3));
        session.program(0, &[0x01; 4])?;
        session.begin_record(RecordId(3));
        session.program(4, &[0x02; 4])?;
        session.barrier()
    });
    let clean = confused.first().expect("a fault-free run");
    assert_eq!(
        verify_recovery(clean.ledger(), &[RecordId(3)]),
        Err(Breach::DuplicateRecordId {
            record: RecordId(3)
        })
    );
}

#[test]
fn every_breach_prints_something_and_no_two_print_the_same_thing() {
    let all = [
        Breach::DuplicateRecordId {
            record: RecordId(1),
        },
        Breach::NotAPrefix {
            position: 0,
            expected: None,
            found: RecordId(1),
        },
        Breach::RecoveredWhatWasNeverAttempted {
            record: RecordId(1),
        },
        Breach::LostAnAcknowledgedRecord {
            record: RecordId(1),
        },
    ];
    for breach in &all {
        assert!(!breach.to_string().is_empty());
    }
    for (index, breach) in all.iter().enumerate() {
        for other in all.iter().skip(index + 1) {
            assert_ne!(breach.to_string(), other.to_string());
        }
    }
}

#[test]
fn every_run_of_the_two_record_writer_satisfies_the_oracle_against_what_landed() {
    // The end-to-end shape: for every crash point, the records whose bytes are wholly on
    // media are a legal recovery, and the oracle agrees.
    for run in runs() {
        let recovered: Vec<RecordId> = [(RecordId(0), 0_usize), (RecordId(1), 4)]
            .into_iter()
            .take_while(|(_, at)| {
                run.image()
                    .get(*at..at.saturating_add(4))
                    .is_some_and(|bytes| bytes.iter().all(|byte| *byte != 0xFF))
            })
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            verify_recovery(run.ledger(), &recovered),
            Ok(()),
            "{:?} recovered {recovered:?}",
            run.injection()
        );
    }
}

#[test]
fn a_session_answers_for_the_device_it_is_driving() {
    // The three observers a writer under test has while it is running, as opposed to the
    // `Run` it leaves behind: the geometry it must align against, the bytes as they stand,
    // and which crash point — if any — is armed.
    assert_eq!(
        Harness::with_bit_rule(geometry(), OneWayBits::Absorbed).geometry(),
        geometry()
    );

    let runs = drive_with(OneWayBits::Absorbed, |session| {
        assert_eq!(session.geometry(), geometry());
        session.begin_record(RecordId(0));
        session.program(0, &[0x0F; 4])?;

        // With nothing armed, a writer can read back exactly what it wrote and see it in
        // the image. Asserted only in that case, because every other run is by definition
        // one in which something went wrong.
        if session.injection().is_none() {
            let mut page = [0_u8; 4];
            assert_eq!(session.read(0, &mut page), Ok(()));
            assert_eq!(page, [0x0F; 4]);
            assert_eq!(session.image().first(), Some(&0x0F));
        }
        session.barrier()
    });

    let clean = runs.first().expect("the fault-free run");
    assert_eq!(clean.ledger().len(), 1);
    assert!(!clean.ledger().is_empty());
    assert_eq!(
        clean.ledger().records().collect::<Vec<_>>(),
        [(RecordId(0), Durability::Acknowledged)]
    );
    assert_eq!(clean.ledger().acknowledged().count(), 1);
}

#[test]
fn a_session_read_after_power_loss_reports_power_loss_rather_than_bytes() {
    // A reader that could still see the device has power. Without this, a writer that
    // verified its own writes would read the pre-crash image and conclude the write landed.
    //
    // Asserted on one named crash point rather than under an `if` on the value being
    // tested: an assertion guarded by its own subject is one a wrong answer switches off.
    let seen = RefCell::new(Vec::new());
    let read_back = Cell::new([0_u8; 4]);
    let torn = Harness::new(geometry())
        .run_one(
            Injection {
                op: 0,
                progress: Progress::Bytes(2),
                interruption: Interruption::PowerLoss,
            },
            |session| {
                seen.borrow_mut().clear();
                session.begin_record(RecordId(0));
                seen.borrow_mut().push(session.program(0, &[0x00; 4]));
                let mut page = [0xAA_u8; 4];
                seen.borrow_mut().push(session.read(0, &mut page));
                seen.borrow_mut().push(session.barrier());
                read_back.set(page);
                Ok::<(), FaultError>(())
            },
        )
        .expect("the writer succeeds with no faults armed");

    assert_eq!(
        seen.into_inner(),
        [
            Err(FaultError::PowerLoss),
            Err(FaultError::PowerLoss),
            Err(FaultError::PowerLoss),
        ]
    );
    // The caller's buffer was left exactly as it was handed over.
    assert_eq!(read_back.get(), [0xAA; 4]);
    // Only the program is an operation: nothing after the power loss happened.
    assert_eq!(torn.ops(), [Op::Program { offset: 0, len: 4 }]);
    assert_eq!(torn.image().get(..4), Some(&[0x00, 0x00, 0xFF, 0xFF][..]));
}

#[test]
fn a_session_refuses_an_operation_the_geometry_forbids_without_recording_it() {
    // A refused call never reached media, so it is not an operation a crash point can be
    // inside — and if it were recorded, every injection index after it would be aimed one
    // operation late.
    let refusals = RefCell::new(Vec::new());
    let runs = drive(|session| {
        refusals.borrow_mut().clear();
        session.begin_record(RecordId(0));
        refusals.borrow_mut().push(session.program(1, &[0x00; 4]));
        refusals.borrow_mut().push(session.program(0, &[0x00; 3]));
        refusals.borrow_mut().push(session.erase(0, 4));
        refusals.borrow_mut().push(session.erase(64, 32));
        session.program(0, &[0x00; 4])?;
        session.barrier()
    });

    assert_eq!(
        refusals.into_inner(),
        [
            Err(FaultError::Geometry(GeometryError::MisalignedOffset)),
            Err(FaultError::Geometry(GeometryError::MisalignedLength)),
            Err(FaultError::Geometry(GeometryError::MisalignedLength)),
            Err(FaultError::Geometry(GeometryError::OutOfBounds)),
        ]
    );
    let clean = runs.first().expect("the fault-free run");
    assert_eq!(
        clean.ops(),
        [Op::Program { offset: 0, len: 4 }, Op::Barrier],
        "four refused calls are not four operations"
    );
    // And none of them touched media: only the one legal write is there.
    assert_eq!(
        clean.image().get(..8),
        Some(&[0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF][..])
    );
}

#[test]
fn a_barrier_that_did_not_complete_acknowledges_nothing_even_under_power_loss() {
    // `injections` only ever offers a barrier's `Whole` under power loss, because "before
    // the barrier" is the previous operation's `Whole`. `run_one` can still be handed the
    // other one, and the answer has to be the conservative one: a barrier the power cut
    // short established no ordering.
    let run = Harness::new(geometry())
        .run_one(
            Injection {
                op: 1,
                progress: Progress::None,
                interruption: Interruption::PowerLoss,
            },
            |session| {
                session.begin_record(RecordId(0));
                session.program(0, &[0x00; 4])?;
                session.barrier()
            },
        )
        .expect("the writer succeeds with no faults armed");
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
    );
    assert_eq!(verify_recovery(run.ledger(), &[]), Ok(()));
}

#[test]
fn a_writer_that_is_not_a_function_of_its_storage_is_refused() {
    // The enumeration is taken from the fault-free run, so a writer carrying state between
    // runs aims its crash points at operations that are not there. It is the failure that
    // would quietly hollow out every suite built on this one, so it is an error rather
    // than a result.
    let calls = Cell::new(0_u32);
    let outcome = Harness::new(geometry()).run(|session| {
        let call = calls.get();
        calls.set(call.wrapping_add(1));
        session.begin_record(RecordId(0));
        if call % 2 == 0 {
            session.program(0, &[0x00; 4])?;
        }
        session.program(4, &[0x00; 4])?;
        session.barrier()
    });
    assert!(
        matches!(outcome, Err(HarnessError::WriterIsNotDeterministic { .. })),
        "{:?}",
        outcome.map(|runs| runs.len())
    );
}

#[test]
fn an_erase_is_recorded_at_the_offset_it_was_issued_at() {
    let runs = drive(|session| {
        session.begin_record(RecordId(0));
        session.erase(32, 32)?;
        session.barrier()
    });
    let clean = runs.first().expect("the fault-free run");
    assert_eq!(
        clean.ops(),
        [
            Op::Erase {
                offset: 32,
                len: 32
            },
            Op::Barrier
        ]
    );
}

#[test]
fn a_session_reports_which_crash_point_is_armed() {
    let armed = RefCell::new(Vec::new());
    let runs = drive(|session| {
        armed.borrow_mut().push(session.injection());
        session.begin_record(RecordId(0));
        session.program(0, &[0x00; 4])?;
        session.barrier()
    });
    let reported = armed.into_inner();
    let carried: Vec<Option<Injection>> = runs.iter().map(Run::injection).collect();
    assert_eq!(reported, carried);
    assert_eq!(reported.first(), Some(&None));
    assert!(reported.iter().skip(1).all(Option::is_some));
}

#[test]
fn a_harness_can_be_told_to_report_one_way_bit_violations() {
    // The strict rule is the one a driver bug shows up under: hardware absorbs a program
    // that asks for a bit only an erase can restore, and a `Vec<u8>` model that assigned
    // instead of masking would absorb it too, silently.
    // A writer that trips the rule with nothing armed cannot be enumerated at all, and the
    // harness says so rather than reporting a sweep of two runs.
    let refused = Harness::with_bit_rule(geometry(), OneWayBits::Rejected).run(|session| {
        session.begin_record(RecordId(0));
        session.program(0, &[0x00; 4])?;
        session.program(0, &[0xFF; 4])
    });
    assert_eq!(
        refused.err(),
        Some(HarnessError::WriterFailedWithNoFaultsArmed(
            "BitSetWithoutErase".to_owned()
        ))
    );

    // And the rule is live in the *injected* runs, not only in the fault-free one. This
    // writer is legal with nothing armed — the erase restores the bits the last program
    // needs — and illegal exactly when that erase is the one that fails.
    let runs = drive_with(OneWayBits::Rejected, |session| {
        session.begin_record(RecordId(0));
        session.program(0, &[0x0F; 4])?;
        session.erase(0, 32)?;
        session.program(0, &[0xF0; 4])
    });
    assert_eq!(runs.first().map(|run| run.ops().len()), Some(3));

    let unerased = runs
        .iter()
        .find(|run| {
            run.injection()
                == Some(Injection {
                    op: 1,
                    progress: Progress::None,
                    interruption: Interruption::Failure,
                })
        })
        .expect("an erase that failed having done nothing is among the crash points");
    assert_eq!(
        unerased.ops(),
        [
            Op::Program { offset: 0, len: 4 },
            Op::Erase { offset: 0, len: 32 },
        ],
        "the last program asked for bits only an erase restores, so it never became an op"
    );
    assert_eq!(unerased.image().first(), Some(&0x0F));
}

#[test]
fn a_record_torn_by_a_failed_write_is_never_acknowledged_by_a_later_barrier() {
    // `Interruption::Failure` exists so the writer carries on past an error. If it then
    // reaches a barrier, the barrier orders half a record — and a ledger that called that
    // acknowledged would *require* recovery to produce a half-written frame, so a correct
    // recovery that drops it would be reported as a breach.
    let run = one_run(Injection {
        op: 0,
        progress: Progress::Bytes(2),
        interruption: Interruption::Failure,
    });
    assert_eq!(run.image().get(..4), Some(&[0xA0, 0xA1, 0xFF, 0xFF][..]));
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
    );
    assert_eq!(run.ledger().torn(RecordId(0)), Some(true));
    assert_eq!(verify_recovery(run.ledger(), &[]), Ok(()));
}

#[test]
fn the_oracle_refuses_a_recovery_that_resurrects_a_torn_record() {
    // Design document §15: "recovery may include an unacknowledged **complete** record".
    // Complete is the load-bearing word, and half a record is not it.
    let run = one_run(Injection {
        op: 0,
        progress: Progress::Bytes(2),
        interruption: Interruption::Failure,
    });
    assert_eq!(
        verify_recovery(run.ledger(), &[RecordId(0)]),
        Err(Breach::RecoveredATornRecord {
            record: RecordId(0)
        })
    );
}

#[test]
fn a_record_that_wrote_nothing_the_media_could_keep_is_not_acknowledged() {
    // Two shapes that change no cell at all: programming `0xFF` over erased media, which
    // AND-masking makes the identity, and erasing a block that is already erased — the
    // bank-prepare shape, which is not exotic. A ledger that counted the *call* rather than
    // the *change* would acknowledge a record with nothing on media to recover.
    for runs in [
        drive(|session| {
            session.begin_record(RecordId(0));
            session.program(0, &[0xFF; 4])?;
            session.barrier()
        }),
        drive(|session| {
            session.begin_record(RecordId(0));
            session.erase(0, 32)?;
            session.barrier()
        }),
    ] {
        let clean = runs.first().expect("the fault-free run");
        assert_eq!(
            clean.ledger().state(RecordId(0)),
            Some(Durability::Attempted)
        );
        assert_eq!(verify_recovery(clean.ledger(), &[]), Ok(()));
    }
}

#[test]
fn a_record_that_never_reached_media_does_not_occupy_a_position_in_history() {
    // An `Attempted` record contributes nothing to media, so it cannot sit between two
    // records recovery *did* find. A prefix check that counted it would leave a correct
    // recovery — the two records that are really there — with no accepting answer.
    let runs = drive(|session| {
        // The middle record's write is allowed to fail without stopping the writer, which
        // is the only way a hole appears between two records that both landed.
        for record in 0..3_u32 {
            session.begin_record(RecordId(record));
            let landed = session.program(record.wrapping_mul(4), &[0xA0; 4]);
            session.end_record();
            if record != 1 {
                landed?;
            }
        }
        session.barrier()
    });

    let hole = runs
        .iter()
        .find(|run| {
            run.ledger().state(RecordId(1)) == Some(Durability::Attempted)
                && run.ledger().state(RecordId(2)) == Some(Durability::Acknowledged)
        })
        .expect("a failed middle write leaves a hole between two records that landed");
    assert_eq!(
        hole.ledger().order().collect::<Vec<_>>(),
        [RecordId(0), RecordId(1), RecordId(2)]
    );
    assert_eq!(
        hole.ledger().committed().collect::<Vec<_>>(),
        [RecordId(0), RecordId(2)],
        "a record that reached no media is not part of committed history"
    );
    assert_eq!(
        verify_recovery(hole.ledger(), &[RecordId(0), RecordId(2)]),
        Ok(())
    );
}

#[test]
fn housekeeping_after_a_record_is_closed_does_not_take_its_acknowledgment_away() {
    // Without `end_record`, a writer cannot say a record is finished, and any operation it
    // issues afterwards falls inside that record's span. An unrelated erase after the
    // barrier would then leave the record with an unordered mutation in it and downgrade
    // it from acknowledged — which weakens the oracle silently, in the direction that
    // stops it catching a real loss.
    let runs = drive(|session| {
        session.begin_record(RecordId(0));
        session.program(0, &[0xA0; 4])?;
        session.barrier()?;
        session.end_record();
        // Housekeeping that belongs to no record.
        session.erase(32, 32)
    });

    let clean = runs.first().expect("the fault-free run");
    assert_eq!(
        clean.ledger().state(RecordId(0)),
        Some(Durability::Acknowledged)
    );
    assert_eq!(
        verify_recovery(clean.ledger(), &[]),
        Err(Breach::LostAnAcknowledgedRecord {
            record: RecordId(0)
        })
    );
    assert_eq!(clean.ledger().order().collect::<Vec<_>>(), [RecordId(0)]);
}

#[test]
fn without_end_record_a_trailing_mutation_belongs_to_the_record_that_was_open() {
    // The other side of the same rule, asserted so that the attribution is a documented
    // choice rather than an accident: operations belong to the record that was open when
    // they were issued, and a writer that does not close one is saying they are its.
    let runs = drive(|session| {
        session.begin_record(RecordId(0));
        session.program(0, &[0xA0; 4])?;
        session.barrier()?;
        session.erase(32, 32)
    });
    let clean = runs.first().expect("the fault-free run");
    assert_eq!(
        clean.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
    );
}

#[test]
fn end_record_before_any_record_is_open_changes_nothing() {
    let runs = drive(|session| {
        session.end_record();
        session.program(0, &[0xA0; 4])?;
        session.end_record();
        session.begin_record(RecordId(9));
        session.program(4, &[0xB0; 4])?;
        session.barrier()
    });
    let clean = runs.first().expect("the fault-free run");
    assert_eq!(clean.ledger().order().collect::<Vec<_>>(), [RecordId(9)]);
    assert_eq!(
        clean.ledger().state(RecordId(9)),
        Some(Durability::Acknowledged)
    );
}

/// Two blocks programmed, then erased in one call — so an erase has an interior.
fn erase_both_blocks(session: &mut Session) -> Result<(), FaultError> {
    session.begin_record(RecordId(0));
    session.program(0, &[0x00; 4])?;
    session.program(32, &[0x00; 4])?;
    session.barrier()?;
    session.begin_record(RecordId(1));
    session.erase(0, 64)?;
    session.barrier()
}

fn erase_run(injection: Injection) -> Run {
    let Some(run) = drive(erase_both_blocks)
        .into_iter()
        .find(|run| run.injection() == Some(injection))
    else {
        unreachable!("{injection:?} is not among the crash points")
    };
    run
}

#[test]
fn an_interrupted_erase_leaves_whole_blocks_erased_and_the_rest_untouched() {
    // Design document §12: an erase "may fail or be interrupted at any supported unit",
    // and the supported unit is the erase block. One of two blocks came back; the other
    // still holds what was programmed into it.
    let run = erase_run(Injection {
        op: 3,
        progress: Progress::Bytes(32),
        interruption: Interruption::PowerLoss,
    });
    assert_eq!(run.image().first(), Some(&0xFF));
    assert_eq!(run.image().get(32), Some(&0x00));
    // The erase touched media, so the record that issued it is not merely attempted — and
    // its barrier never returned, so it is not acknowledged either.
    assert_eq!(
        run.ledger().state(RecordId(1)),
        Some(Durability::PossiblyDurable)
    );
}

#[test]
fn a_failed_erase_reports_an_error_after_the_media_has_already_changed() {
    // The other half of "may fail **or** be interrupted": the call returns an error, the
    // erase happened anyway, and the writer carries on none the wiser.
    let run = erase_run(Injection {
        op: 3,
        progress: Progress::Whole,
        interruption: Interruption::Failure,
    });
    assert_eq!(run.image(), &[0xFF; 64][..]);
    // The writer stopped at the error, so the barrier after the erase was never issued.
    assert_eq!(
        run.ops(),
        [
            Op::Program { offset: 0, len: 4 },
            Op::Program { offset: 32, len: 4 },
            Op::Barrier,
            Op::Erase { offset: 0, len: 64 },
        ]
    );
    assert_eq!(
        run.ledger().state(RecordId(1)),
        Some(Durability::PossiblyDurable)
    );
}

#[test]
fn a_write_torn_at_a_program_unit_boundary_lands_exactly_that_many_bytes() {
    // "Torn writes at every byte and every program unit" — this is the program-unit half,
    // asserted on the media rather than only on the enumeration.
    let unit = geometry().program_size();
    let runs = drive(|session| {
        session.begin_record(RecordId(0));
        session.program(0, &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88])?;
        session.barrier()
    });
    let torn = runs
        .iter()
        .find(|run| {
            run.injection()
                == Some(Injection {
                    op: 0,
                    progress: Progress::Bytes(unit),
                    interruption: Interruption::PowerLoss,
                })
        })
        .expect("a tear at the program-unit boundary is among the crash points");
    assert_eq!(
        torn.image().get(..8),
        Some(&[0x11, 0x22, 0x33, 0x44, 0xFF, 0xFF, 0xFF, 0xFF][..])
    );
}

#[test]
fn the_oracle_reports_the_most_specific_diagnosis_when_two_are_true_at_once() {
    // The doc promises "the first the checks below find, in the order they are written".
    // A ledger with an unrecoverable record recovered *and* an acknowledged one lost trips
    // two checks; the useful sentence is the one about the record that was invented.
    let ledger = Ledger::new(vec![
        (RecordId(0), Durability::Acknowledged, false),
        (RecordId(1), Durability::Attempted, false),
    ]);
    assert_eq!(
        verify_recovery(&ledger, &[RecordId(1)]),
        Err(Breach::RecoveredWhatWasNeverAttempted {
            record: RecordId(1)
        })
    );
}

#[test]
fn a_ledger_reports_the_first_of_two_records_that_share_an_id() {
    let ledger = Ledger::new(vec![
        (RecordId(4), Durability::Attempted, false),
        (RecordId(4), Durability::Acknowledged, false),
    ]);
    assert_eq!(ledger.state(RecordId(4)), Some(Durability::Attempted));
    assert_eq!(ledger.torn(RecordId(4)), Some(false));
    assert_eq!(ledger.len(), 2);
    assert_eq!(
        verify_recovery(&ledger, &[]),
        Err(Breach::DuplicateRecordId {
            record: RecordId(4)
        })
    );
}

#[test]
fn a_prefix_breach_says_what_history_has_where_recovery_disagreed() {
    // The `Some(expected)` arm of `NotAPrefix`'s message, which the round-trip through a
    // harness run never reaches: it needs a recovery that is wrong rather than short.
    let ledger = Ledger::new(vec![
        (RecordId(0), Durability::Acknowledged, false),
        (RecordId(1), Durability::PossiblyDurable, false),
    ]);
    let breach = verify_recovery(&ledger, &[RecordId(1)]);
    assert_eq!(
        breach,
        Err(Breach::NotAPrefix {
            position: 0,
            expected: Some(RecordId(0)),
            found: RecordId(1),
        })
    );
    let Err(breach) = breach else {
        unreachable!("just asserted to be an error")
    };
    assert!(
        breach.to_string().contains("where history has record 0"),
        "{breach}"
    );
}

#[test]
fn an_empty_ledger_is_empty_and_requires_nothing() {
    let ledger = Ledger::default();
    assert!(ledger.is_empty());
    assert_eq!(ledger.len(), 0);
    assert_eq!(ledger.state(RecordId(0)), None);
    assert_eq!(ledger.torn(RecordId(0)), None);
    assert_eq!(verify_recovery(&ledger, &[]), Ok(()));
}

fn one_run(injection: Injection) -> Run {
    let Some(run) = runs()
        .into_iter()
        .find(|run| run.injection() == Some(injection))
    else {
        unreachable!("{injection:?} is not among the crash points")
    };
    run
}
