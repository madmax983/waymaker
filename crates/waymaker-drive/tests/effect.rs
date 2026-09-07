//! Design document §07's seven steps, driven directly.
//!
//! Issue [#29](https://github.com/madmax983/waymaker/issues/29). The driver's own tests
//! exercise the protocol through a workflow; these exercise it through the three types the
//! protocol is made of, so that the order of the steps is measured rather than inferred
//! from a journal that came out right.

use waymaker_core::{ActivityKind, EffectRequest, EffectSeq, Outcome, RecordRef, RunId};
use waymaker_drive::{DriveError, Effect, Resolution, Resolved, Scheduled};
use waymaker_fault::{Device, FaultError, Harness, Op, Session};
use waymaker_flash::append::Journal;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::{Bounds, Refusal, Reserve, Reserved};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};

const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// The device's program unit, which is also the width of a commit seal.
const ALIGN: u32 = 4;

/// Four bytes of result, so that a five-byte answer is over the bound.
const BOUNDS: Bounds = Bounds {
    run_input_bytes: 4,
    effect_result_bytes: 4,
    terminal_bytes: 8,
};

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 512, 4, 1) else {
        unreachable!("1024 is two whole 512-byte blocks of 4-byte units")
    };
    geometry
}

fn region() -> JournalRegion {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 512, align) else {
        unreachable!("the region is the device's first erase block")
    };
    region
}

fn reserve() -> Reserve {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("this geometry holds two erase blocks")
    };
    let Ok(reserve) = Reserve::for_layout(BOUNDS, layout) else {
        unreachable!("these bounds fit this layout")
    };
    reserve
}

/// A gated writer positioned at the start of an erased journal.
fn writer<S: StableStorage>(storage: &mut S) -> Reserved {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 128];
    while recovery.next(storage, &mut page).is_some() {}
    let Some(journal) = Journal::after(recovery) else {
        unreachable!("an erased journal has an append point")
    };
    let Ok(reserved) = Reserved::over(journal, reserve()) else {
        unreachable!("the reserve fits this journal")
    };
    reserved
}

/// The request the tests below schedule.
const fn request() -> EffectRequest {
    EffectRequest {
        kind: ActivityKind(1),
        input_len: 3,
        input_crc: 0x0BAD_F00D,
    }
}

/// Every record the journal recovers to.
fn recovered(storage: &mut Device) -> Vec<RecordKindOf> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 128];
    let mut history = Vec::new();
    while let Some(step) = recovery.next(storage, &mut page) {
        let Ok(record) = step else { break };
        history.push(RecordKindOf::of(&record));
    }
    history
}

/// One recovered record, by shape and payload length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordKindOf {
    Scheduled(u32),
    Completed(u32, usize),
    Failed(u32, usize),
    Other,
}

impl RecordKindOf {
    const fn of(record: &RecordRef<'_>) -> Self {
        match *record {
            RecordRef::EffectScheduled { seq, .. } => Self::Scheduled(seq.0),
            RecordRef::EffectCompleted { seq, result } => Self::Completed(seq.0, result.len()),
            RecordRef::EffectFailed { seq, error } => Self::Failed(seq.0, error.len()),
            _ => Self::Other,
        }
    }
}

#[test]
fn a_schedule_commits_its_record_and_returns_the_only_value_a_dispatch_accepts() {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 128];
    let effect = Effect::over(RUN, writer(&mut device));

    let Scheduled { dispatch, record } = effect
        .schedule(&mut device, EffectSeq(0), request(), &mut page)
        .expect("the schedule fits the journal");

    assert_eq!(dispatch.intent().id().run, RUN);
    assert_eq!(dispatch.intent().id().seq, EffectSeq(0));
    assert_eq!(RecordKindOf::of(&record), RecordKindOf::Scheduled(0));
    assert_eq!(recovered(&mut device), [RecordKindOf::Scheduled(0)]);
}

#[test]
fn each_record_is_a_frame_then_a_barrier_then_a_seal_then_a_barrier() {
    let harness = Harness::new(geometry());
    let runs = harness
        .run(
            |session: &mut Session| -> Result<(), DriveError<FaultError>> {
                let mut page = [0_u8; 128];
                let effect = Effect::over(RUN, writer(session));
                let scheduled = effect.schedule(session, EffectSeq(0), request(), &mut page)?;
                scheduled
                    .dispatch
                    .resolve(session, Resolution::Completed(b"ok"), &mut page)?;
                Ok(())
            },
        )
        .expect("the fault-free run completes");

    let ops: Vec<Op> = runs
        .first()
        .expect("the fault-free run is first")
        .ops()
        .iter()
        .copied()
        .filter(|op| !matches!(op, Op::Erase { .. }))
        .collect();

    // §07 steps 1, 2, 3 and then 5, 6, 7, twice over. The offsets are the assertion: a
    // seal-before-frame writer produces the same four-op *shape* — which is why the shape
    // alone would not have caught `waymaker-fault`'s own tooth — so each pair is required to
    // be a frame body followed by a seal one program unit wide, at the offset just past it.
    assert_eq!(ops.len(), 8, "{ops:?}");
    for record in 0..2_usize {
        let at = record.saturating_mul(4);
        let (Some(Op::Program { offset, len }), Some(Op::Barrier)) =
            (ops.get(at), ops.get(at.saturating_add(1)))
        else {
            unreachable!("a record is a frame body and then a payload barrier: {ops:?}")
        };
        let (
            Some(Op::Program {
                offset: seal,
                len: width,
            }),
            Some(Op::Barrier),
        ) = (ops.get(at.saturating_add(2)), ops.get(at.saturating_add(3)))
        else {
            unreachable!("and then a commit seal and its barrier: {ops:?}")
        };
        assert_eq!(*width, ALIGN, "the seal is one program unit wide: {ops:?}");
        assert_eq!(
            *seal,
            offset.saturating_add(*len),
            "the seal sits just past the frame it seals: {ops:?}"
        );
        assert!(
            *len > ALIGN,
            "the frame body is programmed first, and it is wider than a seal: {ops:?}"
        );
    }
}

#[test]
fn an_outcome_is_committed_before_the_workflow_may_observe_it() {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 128];
    let effect = Effect::over(RUN, writer(&mut device));
    let scheduled = effect
        .schedule(&mut device, EffectSeq(0), request(), &mut page)
        .expect("the schedule fits");

    let resolved = scheduled
        .dispatch
        .resolve(&mut device, Resolution::Completed(b"okay"), &mut page)
        .expect("the outcome fits");

    assert_eq!(
        RecordKindOf::of(&resolved.record),
        RecordKindOf::Completed(0, 4)
    );
    assert_eq!(resolved.outcome, Outcome::Completed(b"okay"));
    assert_eq!(
        recovered(&mut device),
        [RecordKindOf::Scheduled(0), RecordKindOf::Completed(0, 4)]
    );
}

#[test]
fn an_exhausted_answer_is_recorded_as_a_failure_with_no_payload() {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 128];
    let effect = Effect::over(RUN, writer(&mut device));
    let scheduled = effect
        .schedule(&mut device, EffectSeq(0), request(), &mut page)
        .expect("the schedule fits");

    let Resolved {
        record, outcome, ..
    } = scheduled
        .dispatch
        .resolve(&mut device, Resolution::Exhausted, &mut page)
        .expect("an empty failure always fits");

    assert_eq!(RecordKindOf::of(&record), RecordKindOf::Failed(0, 0));
    // Nothing of the answer reaches the caller, so nothing partial can.
    assert_eq!(outcome, Outcome::Failed(&[]));
}

#[test]
fn an_answer_over_the_declared_bound_is_refused_with_no_mutation() {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 128];
    let effect = Effect::over(RUN, writer(&mut device));
    let scheduled = effect
        .schedule(&mut device, EffectSeq(0), request(), &mut page)
        .expect("the schedule fits");
    let before = device.image().to_vec();

    let refused = scheduled
        .dispatch
        .resolve(&mut device, Resolution::Completed(b"toolong"), &mut page)
        .expect_err("seven bytes is over the four-byte bound");

    assert_eq!(refused, DriveError::Capacity(Refusal::OverDeclaredBound));
    assert_eq!(device.image(), before.as_slice());
}
