//! The harness: one run per crash point, and the three record states §15 asks for.
//!
//! Design document §15: "the model distinguishes records that were merely attempted,
//! records that may have become durable before acknowledgment, and records whose barrier
//! returned. Recovery may include an unacknowledged complete record, but it may never lose
//! an acknowledged one."

use waymaker_fault::{
    Breach, Durability, Effect, FaultError, Harness, Injection, OneWayBits, Op, Progress, RecordId,
    Run, Session, verify_recovery,
};
use waymaker_flash::storage::{Geometry, StableStorage};

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
    Harness::new(geometry()).run(two_records)
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
        effect: Effect::PowerLoss,
    });
    assert_eq!(run.ledger().state(RecordId(0)), Some(Durability::Attempted));
    assert_eq!(run.image(), &[0xFF; 64][..]);
}

#[test]
fn a_record_torn_mid_write_is_possibly_durable_and_never_acknowledged() {
    let run = one_run(Injection {
        op: 0,
        progress: Progress::Bytes(2),
        effect: Effect::PowerLoss,
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
        effect: Effect::PowerLoss,
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
        effect: Effect::PowerLoss,
    });
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::Acknowledged)
    );
    // The second record never began.
    assert_eq!(run.ledger().state(RecordId(1)), None);
}

#[test]
fn a_barrier_that_returns_an_error_acknowledges_nothing() {
    // The writer sees an error from `barrier` and stops. Whether the ordering was really
    // established is unknowable to it, so the model must not promise recovery.
    let run = one_run(Injection {
        op: 1,
        progress: Progress::None,
        effect: Effect::Failure,
    });
    assert_eq!(
        run.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
    );
}

#[test]
fn after_power_loss_nothing_else_reaches_media_however_hard_the_writer_tries() {
    let harness = Harness::new(geometry());
    let stubborn = harness.run(|session| {
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
                    effect: Effect::PowerLoss,
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
    let harness = Harness::new(geometry());
    let retrying = harness.run(|session| {
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
                    effect: Effect::Failure,
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
    let harness = Harness::new(geometry());
    let prepared = harness.run(|session| {
        session.erase(0, 32)?;
        session.begin_record(RecordId(7));
        session.program(0, &[0x11; 4])?;
        session.barrier()
    });
    let clean = prepared.first().expect("a fault-free run");
    assert_eq!(clean.ledger().order(), [RecordId(7)]);
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
        effect: Effect::PowerLoss,
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
        effect: Effect::PowerLoss,
    });
    assert_eq!(verify_recovery(run.ledger(), &[]), Ok(()));
    assert_eq!(verify_recovery(run.ledger(), &[RecordId(0)]), Ok(()));
}

#[test]
fn the_oracle_fails_closed_on_a_ledger_that_names_one_record_twice() {
    let harness = Harness::new(geometry());
    let confused = harness.run(|session| {
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
    let harness = Harness::with_bit_rule(geometry(), OneWayBits::Nor);
    assert_eq!(harness.geometry(), geometry());

    let runs = harness.run(|session| {
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
    let runs = Harness::new(geometry()).run(|session| {
        session.begin_record(RecordId(0));
        let written = session.program(0, &[0x00; 4]);
        let mut page = [0xAA_u8; 4];
        let seen = session.read(0, &mut page);
        if written == Err(FaultError::PowerLoss) {
            assert_eq!(seen, Err(FaultError::PowerLoss));
        }
        match (written, seen) {
            (_, Err(error)) | (Err(error), _) => Err(error),
            _ => Ok(()),
        }
    });

    let torn = runs
        .iter()
        .find(|run| {
            run.injection()
                == Some(Injection {
                    op: 0,
                    progress: Progress::Bytes(2),
                    effect: Effect::PowerLoss,
                })
        })
        .expect("a torn first write is among the crash points");
    // The read is not recorded as an operation, because it never happened.
    assert_eq!(torn.ops(), [Op::Program { offset: 0, len: 4 }]);
}

#[test]
fn a_harness_can_be_told_to_report_one_way_bit_violations() {
    // The strict rule is the one a driver bug shows up under: hardware absorbs a program
    // that asks for a bit only an erase can restore, and a `Vec<u8>` model that assigned
    // instead of masking would absorb it too, silently.
    let strict = Harness::with_bit_rule(geometry(), OneWayBits::Rejected);
    let runs = strict.run(|session| {
        session.begin_record(RecordId(0));
        session.program(0, &[0x00; 4])?;
        session.program(0, &[0xFF; 4])
    });

    let clean = runs.first().expect("the fault-free run");
    // The second program was refused, so it is not an operation and has no crash points.
    assert_eq!(clean.ops(), [Op::Program { offset: 0, len: 4 }]);
    assert_eq!(
        clean.ledger().state(RecordId(0)),
        Some(Durability::PossiblyDurable)
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
    let Some(run) = Harness::new(geometry())
        .run(erase_both_blocks)
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
        effect: Effect::PowerLoss,
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
        effect: Effect::Failure,
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
    let runs = Harness::new(geometry()).run(|session| {
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
                    effect: Effect::PowerLoss,
                })
        })
        .expect("a tear at the program-unit boundary is among the crash points");
    assert_eq!(
        torn.image().get(..8),
        Some(&[0x11, 0x22, 0x33, 0x44, 0xFF, 0xFF, 0xFF, 0xFF][..])
    );
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
