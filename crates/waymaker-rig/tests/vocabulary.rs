//! Every refusal the rig can report, and the accessors a caller reads it through.
//!
//! A device with no console still has to say what happened, so each error here carries a
//! short static message rather than a formatted string. This file is what stops one of them
//! being unreachable, empty, or the same sentence as its neighbour — the failure mode
//! `waymaker-core`'s `tests/errors.rs` exists for, in the crate whose whole job is reporting.

use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::audit::{Audit, Breach};
use waymaker_rig::census::{Coverage, Gap};
use waymaker_rig::log::{Entry, LogError, Outcome};
use waymaker_rig::phase::{Phase, ResetCause};
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Cutter, NeverCut, PlannedCut, Rig, Stop};
use waymaker_rig::wear::{Metered, Traffic, Wear};
use waymaker_rig::window::{Window, WindowError, WindowFault};
use waymaker_rig::witness::{Mark, Progress, Stage, Witness, WitnessError, WitnessRegion};
use waymaker_rig::workload::{Role, Workload};

fn part() -> Geometry {
    let Ok(geometry) = Geometry::new(6 * 256, 256, 4, 1) else {
        unreachable!("a legal geometry")
    };
    geometry
}

#[test]
fn every_window_refusal_says_something_of_its_own() {
    let messages: Vec<&str> = [
        WindowError::Unaligned,
        WindowError::PastTheEnd,
        WindowError::Empty,
    ]
    .into_iter()
    .map(WindowError::message)
    .collect();
    for message in &messages {
        assert!(!message.is_empty());
    }
    let mut unique = messages.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), messages.len(), "two refusals read the same");
    assert_eq!(WindowError::Empty.to_string(), WindowError::Empty.message());
}

#[test]
fn a_window_fault_reports_whichever_half_refused() {
    let mut device = waymaker_fault::Device::new(part());
    let mut window = Window::new(&mut device, 0, 512).expect("a legal window");
    // The window's own bound, which the part beneath would have allowed.
    let refusal = window
        .program(512, b"\x00\x00\x00\x00")
        .expect_err("past the window's end");
    assert_eq!(refusal, WindowFault::Window(WindowError::PastTheEnd));
    assert!(!refusal.to_string().is_empty());

    // And the part's, which the window passes through unchanged.
    let refusal = window
        .program(1, b"\x00\x00\x00\x00")
        .expect_err("misaligned against the program unit");
    assert!(matches!(refusal, WindowFault::Part(_)));
    assert!(!refusal.to_string().is_empty());
}

#[test]
fn a_window_hands_the_part_back_both_ways() {
    let mut device = waymaker_fault::Device::new(part());
    let mut window = Window::new(&mut device, 256, 256).expect("a legal window");
    assert_eq!(window.inner().geometry(), part());
    assert_eq!(window.inner_mut().geometry(), part());
    assert_eq!(window.base(), 256);
}

#[test]
fn every_witness_refusal_says_something_of_its_own() {
    let refusals: Vec<WitnessError<core::convert::Infallible>> = vec![
        WitnessError::ShortBuffer,
        WitnessError::NotAMark,
        WitnessError::Hole,
        WitnessError::MixedIterations,
        WitnessError::OutOfOrder,
        WitnessError::Full,
        WitnessError::Region,
        WitnessError::WrongGeometry,
    ];
    let mut messages: Vec<&str> = refusals.iter().map(WitnessError::message).collect();
    for message in &messages {
        assert!(!message.is_empty());
    }
    let count = messages.len();
    messages.sort_unstable();
    messages.dedup();
    assert_eq!(messages.len(), count, "two refusals read the same");
    let hole: WitnessError<core::convert::Infallible> = WitnessError::Hole;
    assert_eq!(hole.to_string(), hole.message());
    // The driver arm's message is the driver's, so it is checked separately rather than
    // required to be unique against the eight above.
    let driver: WitnessError<&str> = WitnessError::Driver("the part said no");
    assert_eq!(driver.to_string(), "the part said no");
    assert!(!driver.message().is_empty());
}

#[test]
fn a_stage_names_itself_and_the_three_names_differ() {
    let names: Vec<&str> = [Stage::Attempted, Stage::Acknowledged, Stage::Dispatched]
        .into_iter()
        .map(Stage::name)
        .collect();
    assert_eq!(names, ["attempted", "acknowledged", "dispatched"]);
}

#[test]
fn a_witness_reports_the_region_and_the_slot_it_is_at() {
    let Ok(region) = WitnessRegion::of(part(), 0, 256) else {
        unreachable!("the first erase block is a legal witness region")
    };
    let witness = Witness::new(region);
    assert_eq!(witness.region(), region);
    assert_eq!(witness.next_slot(), 0);
    assert_eq!(region.geometry(), part());
    assert_eq!(region.base(), 0);
    assert_eq!(region.bytes(), 256);
    assert!(region.capacity() > 1);
}

#[test]
fn a_mark_reports_what_it_carries() {
    let mark = Mark::new(9, 4, Stage::Dispatched);
    assert_eq!(mark.iteration(), 9);
    assert_eq!(mark.index(), 4);
    assert_eq!(mark.stage(), Stage::Dispatched);
}

#[test]
fn a_gap_names_the_cell_it_is_about() {
    let gap: Gap = Coverage::EMPTY
        .record(Phase::Schedule, ResetCause::PowerCut)
        .verdict()
        .expect_err("five cells are still empty");
    assert_eq!(gap.phase(), Phase::Schedule);
    assert_eq!(gap.cause(), ResetCause::Watchdog);
    let rendered = gap.to_string();
    assert!(rendered.contains("schedule"), "got {rendered}");
    assert!(rendered.contains("watchdog"), "got {rendered}");
}

#[test]
fn every_log_refusal_says_something_of_its_own() {
    let refusals = [
        LogError::ShortBuffer,
        LogError::NotAnEntry,
        LogError::UnknownVersion { version: 9 },
    ];
    for refusal in refusals {
        assert!(!refusal.message().is_empty());
        assert_eq!(refusal.to_string(), refusal.message());
    }
}

#[test]
fn an_outcome_names_itself_and_a_pass_is_code_zero() {
    assert_eq!(Outcome::Passed.code(), 0);
    assert_eq!(Outcome::Passed.name(), "passed");
    assert_eq!(Outcome::Passed.detail(), 0);
    assert_eq!(Outcome::default(), Outcome::Passed);
    let breached = Outcome::Breached(Breach::RecordDiffers { index: 7 });
    assert_eq!(breached.code(), Breach::RecordDiffers { index: 0 }.code());
    assert_eq!(breached.detail(), 7);
    assert_eq!(breached.name(), "record-differs");
}

#[test]
fn a_breach_renders_as_its_name() {
    let breach = Breach::WitnessUnreadable;
    assert_eq!(breach.to_string(), breach.name());
}

#[test]
fn an_audit_reports_the_progress_it_was_given() {
    let workload = Workload::new(1, 0, 2);
    let progress = Progress::default().raising(Stage::Acknowledged, 3);
    let audit = Audit::new(workload, progress);
    assert_eq!(audit.progress(), progress);
    assert_eq!(audit.recovered(), 0);
}

#[test]
fn a_workload_reports_what_it_was_built_from() {
    let workload = Workload::new(0xABCD, 12, 5);
    assert_eq!(workload.seed(), 0xABCD);
    assert_eq!(workload.iteration(), 12);
    assert_eq!(workload.effects(), 5);
    assert_ne!(workload.run(), Workload::new(0xABCD, 13, 5).run());
    assert_eq!(workload.role(0), Some(Role::Start));
}

#[test]
fn a_meter_reports_the_traffic_it_is_attributing() {
    let mut device = waymaker_fault::Device::new(part());
    let mut meter = Metered::new(&mut device);
    assert_eq!(meter.traffic(), Traffic::Engine);
    assert_eq!(Traffic::default(), Traffic::Engine);
    meter.set_traffic(Traffic::Rig);
    assert_eq!(meter.traffic(), Traffic::Rig);
    assert_eq!(meter.inner().geometry(), part());
    assert_eq!(meter.inner_mut().geometry(), part());
    assert_eq!(meter.total_wear(), Wear::NONE);
}

#[test]
fn a_wear_round_trips_through_its_encoding() {
    let mut device = waymaker_fault::Device::new(part());
    let mut meter = Metered::new(&mut device);
    meter.erase(0, 256).expect("one block");
    meter.program(0, b"\x01\x02\x03\x04").expect("a program");
    meter.barrier().expect("a barrier");
    meter.credit_effect();
    let wear = meter.wear();

    let mut bytes = [0_u8; Wear::ENCODED_BYTES];
    assert_eq!(wear.encode(&mut bytes), Some(Wear::ENCODED_BYTES));
    assert_eq!(Wear::decode(&bytes), Some(wear));

    let mut short = [0_u8; Wear::ENCODED_BYTES - 1];
    assert_eq!(wear.encode(&mut short), None);
    assert_eq!(Wear::decode(&short), None);
}

#[test]
fn a_rig_reports_the_layout_it_derived() {
    let rig = Rig::new::<waymaker_fault::FaultError>(part(), Plan::new(3), 2)
        .expect("six erase blocks hold two banks and a witness");
    assert_eq!(rig.part(), part());
    assert_eq!(rig.effects(), 2);
    assert_eq!(rig.plan().seed(), 3);
    assert_eq!(rig.workload(0).effects(), 2);
    assert_eq!(rig.cut(0), Plan::new(3).cut(0));
    assert_eq!(
        rig.instrument_base(),
        part().capacity() - part().erase_size()
    );
    assert_eq!(rig.witness_region().bytes(), part().erase_size());
    assert!(rig.layout().geometry().capacity() < part().capacity());
}

#[test]
fn a_part_too_small_for_two_banks_and_a_witness_is_refused() {
    // One erase block: the instrument takes it and the engine has nothing left.
    let Ok(tiny) = Geometry::new(256, 256, 4, 1) else {
        unreachable!("a legal geometry")
    };
    assert!(Rig::new::<waymaker_fault::FaultError>(tiny, Plan::new(0), 1).is_err());

    // Two erase blocks: the engine has one, which cannot hold §10's two banks.
    let Ok(small) = Geometry::new(512, 256, 4, 1) else {
        unreachable!("a legal geometry")
    };
    assert!(Rig::new::<waymaker_fault::FaultError>(small, Plan::new(0), 1).is_err());
}

#[test]
fn a_page_shorter_than_the_rig_needs_is_refused_by_every_entry_point() {
    let rig =
        Rig::new::<waymaker_fault::FaultError>(part(), Plan::new(0), 1).expect("a legal layout");
    let mut device = waymaker_fault::Device::new(part());
    let mut page = [0_u8; Rig::PAGE_BYTES - 1];
    let mut metered = Metered::new(&mut device);
    assert!(rig.prepare(&mut metered, 0, &mut page).is_err());
    drop(metered);
    assert!(rig.verify(0, &mut device, &mut page).is_err());
    assert!(
        rig.judge(0, &mut device, Progress::default(), &mut page)
            .is_err()
    );
}

#[test]
fn a_never_cutter_never_cuts_and_a_planned_one_cuts_once() {
    let mut never = NeverCut;
    assert!(!never.cut(Phase::Schedule, ResetCause::PowerCut, 0));
    assert_eq!(NeverCut, NeverCut::default());

    let cut = Plan::new(0).cut(0);
    let mut planned = PlannedCut::new(cut, 4);
    assert!(!planned.fired());
    let effect = planned.effect();
    assert!(effect < 4);
    // A phase that is not the armed one never fires it.
    let other = match cut.phase() {
        Phase::Schedule => Phase::Completion,
        Phase::Dispatch | Phase::Completion => Phase::Schedule,
    };
    assert!(!planned.cut(other, cut.cause(), effect));
    assert!(planned.cut(cut.phase(), cut.cause(), effect));
    assert!(planned.fired());
    // And only once: a cutter that fired twice would stop a resumed iteration immediately.
    assert!(!planned.cut(cut.phase(), cut.cause(), effect));
}

#[test]
fn a_rig_builds_a_log_entry_from_an_outcome_and_a_wear() {
    let rig =
        Rig::new::<waymaker_fault::FaultError>(part(), Plan::new(77), 3).expect("a legal layout");
    let entry = rig.entry(5, Outcome::Passed, Wear::NONE);
    assert_eq!(entry.seed(), 77);
    assert_eq!(entry.iteration(), 5);
    assert_eq!(entry.effects(), 3);
    assert_eq!(entry.geometry(), Ok(part()));
    assert_eq!(entry.outcome(), Outcome::Passed);
    assert_eq!(entry.wear(), Wear::NONE);
    let mut bytes = [0_u8; waymaker_rig::log::ENTRY_BYTES];
    entry.encode(&mut bytes).expect("an entry fits");
    assert_eq!(Entry::decode(&bytes), Ok(entry));
}

#[test]
fn a_stop_says_where_the_iteration_ended() {
    assert_ne!(
        Stop::Completed,
        Stop::Cut {
            phase: Phase::Dispatch,
            effect: 0,
        }
    );
}
