//! The run the rig cuts into: deterministic, so a log line names it whole.

use waymaker_core::{ActivityKind, EffectSeq, RecordRef};
use waymaker_rig::workload::{Role, Workload};

const SEED: u64 = 0x1234_5678_9ABC_DEF0;

#[test]
fn a_run_is_a_start_a_pair_per_effect_and_a_finish() {
    for effects in 0..8_u16 {
        let workload = Workload::new(SEED, 0, effects);
        assert_eq!(workload.records(), 2 + 2 * effects);
        assert_eq!(workload.role(0), Some(Role::Start));
        for effect in 0..effects {
            assert_eq!(workload.role(1 + 2 * effect), Some(Role::Schedule(effect)));
            assert_eq!(
                workload.role(2 + 2 * effect),
                Some(Role::Completion(effect))
            );
        }
        assert_eq!(workload.role(1 + 2 * effects), Some(Role::Finish));
        assert_eq!(workload.role(2 + 2 * effects), None);
    }
}

#[test]
fn the_same_seed_and_iteration_give_the_same_records() {
    let workload = Workload::new(SEED, 17, 3);
    let twin = Workload::new(SEED, 17, 3);
    let mut left = [0_u8; 64];
    let mut right = [0_u8; 64];
    for index in 0..workload.records() {
        let a = workload
            .record(index, &mut left)
            .expect("a record of the run");
        let b = twin.record(index, &mut right).expect("a record of the run");
        assert_eq!(
            a, b,
            "record {index} differed between two identical workloads"
        );
    }
}

#[test]
fn different_iterations_give_different_payloads() {
    // A run that wrote the same bytes every iteration would make a torn record from
    // iteration 4 indistinguishable from one from iteration 5.
    let mut left = [0_u8; 64];
    let mut right = [0_u8; 64];
    let a = Workload::new(SEED, 4, 2)
        .record(0, &mut left)
        .expect("a start record");
    let b = Workload::new(SEED, 5, 2)
        .record(0, &mut right)
        .expect("a start record");
    assert_ne!(a, b);
}

#[test]
fn a_schedule_record_carries_the_sequence_the_effect_index_names() {
    let workload = Workload::new(SEED, 0, 4);
    let mut page = [0_u8; 64];
    for effect in 0..4_u16 {
        let record = workload
            .record(1 + 2 * effect, &mut page)
            .expect("a schedule record");
        match record {
            RecordRef::EffectScheduled { seq, kind, .. } => {
                assert_eq!(seq, EffectSeq(u32::from(effect)));
                assert_eq!(kind, workload.activity());
            }
            other => panic!("record {} is {other:?}, not a schedule", 1 + 2 * effect),
        }
    }
}

#[test]
fn a_schedule_records_length_and_digest_describe_its_own_input() {
    // §16's third deferred question is settled at a length and a digest (ADR 0011). A
    // workload whose declared digest did not match its input would make the rig's own runs
    // divergent under §08, and the rig would be testing a bug it wrote.
    let workload = Workload::new(SEED, 9, 3);
    let mut page = [0_u8; 64];
    let mut input = [0_u8; 64];
    for effect in 0..3_u16 {
        let bytes = workload.effect_input(effect, &mut input).expect("an input");
        let record = workload
            .record(1 + 2 * effect, &mut page)
            .expect("a schedule record");
        match record {
            RecordRef::EffectScheduled {
                input_len,
                input_crc,
                ..
            } => {
                assert_eq!(usize::from(input_len), bytes.len());
                assert_eq!(input_crc, waymaker_flash::frame::input_digest(bytes));
            }
            other => panic!("expected a schedule record, got {other:?}"),
        }
    }
}

#[test]
fn a_completion_carries_the_sequence_of_the_schedule_before_it() {
    let workload = Workload::new(SEED, 0, 3);
    let mut page = [0_u8; 64];
    for effect in 0..3_u16 {
        match workload.record(2 + 2 * effect, &mut page) {
            Some(RecordRef::EffectCompleted { seq, .. }) => {
                assert_eq!(seq, EffectSeq(u32::from(effect)));
            }
            other => panic!("record {} is {other:?}", 2 + 2 * effect),
        }
    }
}

#[test]
fn the_first_record_is_a_run_start_and_the_last_a_run_completion() {
    let workload = Workload::new(SEED, 0, 2);
    let mut page = [0_u8; 64];
    assert!(matches!(
        workload.record(0, &mut page),
        Some(RecordRef::RunStarted { .. })
    ));
    assert!(matches!(
        workload.record(workload.records() - 1, &mut page),
        Some(RecordRef::RunCompleted { .. })
    ));
}

#[test]
fn a_record_past_the_end_is_none() {
    let workload = Workload::new(SEED, 0, 1);
    let mut page = [0_u8; 64];
    assert_eq!(workload.record(workload.records(), &mut page), None);
    assert_eq!(workload.record(u16::MAX, &mut page), None);
}

#[test]
fn a_buffer_too_small_for_the_payload_is_none_rather_than_a_truncated_record() {
    let workload = Workload::new(SEED, 0, 1);
    let mut page = [0_u8; 1];
    // The workload's payloads are longer than one byte, so this cannot be satisfied.
    assert_eq!(workload.record(0, &mut page), None);
}

#[test]
fn the_workload_reports_the_page_every_record_of_it_fits_in() {
    let workload = Workload::new(SEED, 0, 6);
    let mut page = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    for index in 0..workload.records() {
        assert!(
            workload.record(index, &mut page).is_some(),
            "record {index} did not fit the declared maximum payload"
        );
    }
}

#[test]
fn the_activity_kind_is_stable_and_not_zero() {
    // Zero is a legal `ActivityKind`, and a workload that used it would agree with a
    // zeroed page — which is what a partially programmed record reads back as.
    assert_eq!(Workload::new(SEED, 0, 1).activity(), ActivityKind(1));
}

#[test]
fn the_schedule_index_of_an_effect_is_the_role_that_names_it() {
    let workload = Workload::new(SEED, 0, 5);
    for effect in 0..5_u16 {
        let index = workload
            .schedule_index(effect)
            .expect("an effect of this run");
        assert_eq!(workload.role(index), Some(Role::Schedule(effect)));
        let completion = workload
            .completion_index(effect)
            .expect("an effect of this run");
        assert_eq!(workload.role(completion), Some(Role::Completion(effect)));
    }
    assert_eq!(workload.schedule_index(5), None);
    assert_eq!(workload.completion_index(5), None);
}
