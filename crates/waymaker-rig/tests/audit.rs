//! The rig's oracle: design document §15's four lines, checked against what the witness
//! durably knew rather than against a ledger a power cut would have taken.

use waymaker_rig::audit::{Audit, Breach};
use waymaker_rig::witness::{Progress, Stage};
use waymaker_rig::workload::Workload;

const SEED: u64 = 0xFEED_FACE_CAFE_BEEF;
const EFFECTS: u16 = 3;

const fn workload() -> Workload {
    Workload::new(SEED, 0, EFFECTS)
}

/// A witness that saw `attempted` records begun, `acknowledged` records committed and
/// `dispatched` effects sent, expressed as the high-water marks a scan reports.
fn progress(
    attempted: Option<u16>,
    acknowledged: Option<u16>,
    dispatched: Option<u16>,
) -> Progress {
    let mut progress = Progress::default();
    if let Some(index) = attempted {
        progress = progress.raising(Stage::Attempted, index);
    }
    if let Some(index) = acknowledged {
        progress = progress.raising(Stage::Acknowledged, index);
    }
    if let Some(index) = dispatched {
        progress = progress.raising(Stage::Dispatched, index);
    }
    progress
}

/// Replays `recovered` records of the workload through an audit and returns its verdict.
fn audit(recovered: u16, progress: Progress, banks: usize) -> Result<(), Breach> {
    let workload = workload();
    let mut audit = Audit::new(workload, progress);
    let mut expected = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    let mut page = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    for index in 0..recovered {
        let Some(record) = workload.record(index, &mut page) else {
            unreachable!("the fixture asks only for records the run has")
        };
        audit.saw(&record, &mut expected)?;
    }
    audit.finish(banks)
}

#[test]
fn a_clean_run_recovered_whole_is_no_breach() {
    let whole = workload().records();
    audit(
        whole,
        progress(Some(whole - 1), Some(whole - 1), Some(5)),
        1,
    )
    .expect("everything acknowledged and everything recovered");
}

#[test]
fn a_run_that_never_started_is_no_breach() {
    audit(0, Progress::default(), 1).expect("a witness with nothing in it demands nothing");
}

#[test]
fn losing_an_acknowledged_record_is_a_breach() {
    // §14 `acknowledged-durability`. The witness says record 2's barrier returned; recovery
    // handed back two records, so record 2 is gone.
    let breach = audit(2, progress(Some(2), Some(2), None), 1)
        .expect_err("an acknowledged record was not recovered");
    assert_eq!(breach, Breach::LostAcknowledgedRecord { index: 2 });
}

#[test]
fn recovering_more_than_was_ever_attempted_is_a_breach() {
    // §14 `prefix-safety` from the other side: a record the writer never began cannot be a
    // legal prefix of committed history.
    // Reported at the first record past the high water rather than at the end of the scan:
    // the writer began records 0 and 1, so record 2 is already one too many and the rig must
    // stop there rather than keep reading a journal it has proved it cannot trust.
    let breach =
        audit(4, progress(Some(1), Some(1), None), 1).expect_err("recovery invented a record");
    assert_eq!(
        breach,
        Breach::RecoveredPastWhatWasAttempted {
            recovered: 3,
            attempted: 1,
        }
    );
}

#[test]
fn a_record_that_is_not_the_one_declared_is_a_breach() {
    // §14 `prefix-safety`. Not "some legal history" — *this* run's history, in order.
    let workload = workload();
    let mut audit = Audit::new(workload, progress(Some(3), Some(3), None));
    let mut expected = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    let mut page = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    let first = workload.record(0, &mut page).expect("a start record");
    audit
        .saw(&first, &mut expected)
        .expect("record zero is right");

    let mut other = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    let wrong = workload.record(3, &mut other).expect("a later record");
    let breach = audit
        .saw(&wrong, &mut expected)
        .expect_err("record one is not record three");
    assert_eq!(breach, Breach::RecordDiffers { index: 1 });
}

#[test]
fn a_dispatched_effect_whose_schedule_is_gone_is_a_breach() {
    // §14 `durable-intent`. The witness says the effect scheduled by record 1 was dispatched;
    // recovery handed back one record, so record 1 is gone and a physical effect happened
    // that no committed intent accounts for.
    let breach = audit(1, progress(Some(1), None, Some(1)), 1)
        .expect_err("a dispatched effect lost its schedule record");
    assert_eq!(breach, Breach::DispatchedEffectWithoutSchedule { index: 1 });
}

#[test]
fn a_dispatch_mark_that_names_a_record_which_is_not_a_schedule_is_a_breach() {
    // The witness is the instrument. An instrument reporting a dispatch against a completion
    // record is broken, and a rig that shrugged would be reading its own bug as a pass.
    let whole = workload().records();
    let breach = audit(
        whole,
        progress(Some(whole - 1), Some(whole - 1), Some(2)),
        1,
    )
    .expect_err("record two is a completion, not a schedule");
    assert_eq!(breach, Breach::DispatchMarkIsNotASchedule { index: 2 });
}

#[test]
fn a_device_that_was_never_installed_has_no_authority_to_lose() {
    // A crash during preparation leaves a part with no sealed bank, which is the state every
    // part starts in. Reporting it as a §14 violation would fail every crash point in the
    // install — and the witness is empty there, which is how the two are told apart.
    audit(0, Progress::default(), 0).expect("a part that was never installed");
}

#[test]
fn two_authorities_are_a_breach_even_before_the_rig_began() {
    // No crash makes a second authority out of nothing, so this one needs no witness.
    let breach = audit(0, Progress::default(), 2).expect_err("two banks claimed authority");
    assert_eq!(breach, Breach::Authority { banks: 2 });
}

#[test]
fn anything_but_exactly_one_authoritative_bank_is_a_breach() {
    // §14 `single-authority`.
    let whole = workload().records();
    let full = progress(Some(whole - 1), Some(whole - 1), Some(5));
    for banks in [0_usize, 2, 3] {
        let breach = audit(whole, full, banks).expect_err("only one bank may be authoritative");
        assert_eq!(breach, Breach::Authority { banks });
    }
}

#[test]
fn the_audit_reports_the_first_breach_and_stops() {
    // A record that differs is reported where it differs rather than at the end, because the
    // rig must not carry on writing after it has seen recovery diverge.
    let workload = workload();
    let mut audit = Audit::new(workload, progress(Some(0), Some(0), None));
    let mut expected = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    let mut page = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    let wrong = workload.record(2, &mut page).expect("a later record");
    assert_eq!(
        audit.saw(&wrong, &mut expected),
        Err(Breach::RecordDiffers { index: 0 })
    );
    assert_eq!(audit.recovered(), 0);
}

#[test]
fn a_torn_acknowledgment_demands_less_rather_than_more() {
    // The whole reason the acknowledgment mark is written *after* the barrier: a mark that
    // did not land under-claims, and an under-claim can only fail to catch a bug. Claiming a
    // record was acknowledged when it was not would invent violations.
    audit(2, progress(Some(2), Some(1), None), 1)
        .expect("record two was written but never acknowledged, so recovery owes nothing");
}

#[test]
fn an_over_claiming_dispatch_mark_is_still_satisfiable() {
    // Dispatch is marked *before* the effect, so the rig may claim a dispatch that never
    // happened. That is deliberate — it demands more of recovery, never less — and it must
    // still pass when recovery has the schedule record.
    audit(2, progress(Some(1), Some(1), Some(1)), 1)
        .expect("the schedule record is there, so the obligation is met");
}

#[test]
fn an_audit_reports_how_many_records_it_saw() {
    let workload = workload();
    let mut audit = Audit::new(workload, progress(Some(3), Some(3), None));
    let mut expected = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    let mut page = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    for index in 0..4 {
        let record = workload.record(index, &mut page).expect("a record");
        audit.saw(&record, &mut expected).expect("in order");
    }
    assert_eq!(audit.recovered(), 4);
}

#[test]
fn a_breach_names_itself() {
    // The log carries a code rather than a message, and the two must agree or a violation
    // read back from a log line is a different violation.
    for breach in [
        Breach::RecordDiffers { index: 0 },
        Breach::LostAcknowledgedRecord { index: 0 },
        Breach::RecoveredPastWhatWasAttempted {
            recovered: 1,
            attempted: 0,
        },
        Breach::DispatchedEffectWithoutSchedule { index: 0 },
        Breach::DispatchMarkIsNotASchedule { index: 0 },
        Breach::Authority { banks: 0 },
        Breach::WitnessUnreadable,
    ] {
        assert!(!breach.name().is_empty());
        assert_ne!(breach.code(), 0, "zero is reserved for 'no breach'");
        assert_eq!(Breach::name_of_code(breach.code()), Some(breach.name()));
    }
    assert_eq!(Breach::name_of_code(0), None);
}
