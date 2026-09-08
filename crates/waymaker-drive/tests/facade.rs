#![cfg(not(feature = "without-facade"))]
//! The two halves of design document §07, called out of order.
//!
//! [`Boundary::schedule`] hands the writer to the effect it committed, and
//! [`Boundary::resolve`] takes it back. A caller that splits §07 in two and then loses its
//! place must be refused rather than left with a run §08 can never end, so both misuses are
//! named errors and both are driven here.
//!
//! The ordinary path is `crates/waymaker-drive/tests/ota.rs`.

use waymaker_core::timer::{ClockCapability, ClockKind, TimerSpec};
use waymaker_core::{ActivityKind, Outcome};
use waymaker_drive::ota::{BOUNDS, DOWNLOAD, URL, WORKFLOW_KIND, WORKFLOW_VERSION};
use waymaker_drive::{
    Activities, Answered, Boundary, Bridge, Clocks, DriveError, Driver, DurableIntent, Handoff,
    Identity, Performed, Scratch, Suspended, Workflow,
};
use waymaker_embassy::journal::{Answer, Journal as _};
use waymaker_fault::{Device, FaultError};
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::JournalRegion;
use waymaker_flash::storage::Geometry;

const RUN: waymaker_core::RunId = waymaker_core::RunId(0x0BAD_F00D_1234_5678);

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(4096, 1024, 4, 1) else {
        unreachable!("4096/1024/4/1 is a legal geometry")
    };
    geometry
}

fn region() -> JournalRegion {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    let Ok(region) = JournalRegion::spanning(geometry(), 0, 1024, align) else {
        unreachable!("a 1024-byte region at offset 0 fits this geometry")
    };
    region
}

fn reserve() -> Reserve {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("this geometry holds two erase blocks")
    };
    let Ok(reserve) = Reserve::for_layout(BOUNDS, layout) else {
        unreachable!("the OTA bounds fit this layout")
    };
    reserve
}

/// A world nothing asks anything of.
struct Idle;

impl Activities for Idle {
    fn perform(
        &mut self,
        _intent: DurableIntent,
        _kind: ActivityKind,
        _input: &[u8],
        _out: &mut [u8],
    ) -> Performed {
        Performed::Pending
    }
}

impl Clocks for Idle {
    fn capability(&self) -> ClockCapability {
        ClockCapability::BootOnly
    }

    fn now(&mut self, _kind: ClockKind) -> Option<u64> {
        Some(0)
    }
}

/// A workflow that calls the two halves of §07 the way `misuse` says.
struct Misusing(Misuse);

/// Which order to call the halves in.
#[derive(Clone, Copy)]
enum Misuse {
    /// Resolve an effect that was never scheduled.
    ResolveFirst,
    /// Schedule a second effect while the first is outstanding.
    ScheduleTwice,
}

impl Workflow for Misusing {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: URL,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        match self.0 {
            Misuse::ResolveFirst => {
                boundary.resolve(Answered::Completed(b"ok"))?;
            }
            Misuse::ScheduleTwice => {
                let first = boundary.schedule(DOWNLOAD, b"one")?;
                assert!(matches!(first, Handoff::Dispatch(_)), "the first schedules");
                boundary.schedule(DOWNLOAD, b"two")?;
            }
        }
        Ok(Outcome::Completed(&[]))
    }
}

fn drive(misuse: Misuse) -> Result<waymaker_drive::Progress, DriveError<FaultError>> {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut Idle,
        &mut Misusing(misuse),
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

#[test]
fn resolving_an_effect_that_was_never_scheduled_is_refused() {
    // A caller that never committed an intent has no outcome to record. Writing one anyway
    // would put an `EffectCompleted` into history with no schedule record before it, which
    // is history no execution could have produced.
    assert_eq!(
        drive(Misuse::ResolveFirst),
        Err(DriveError::NoEffectOutstanding)
    );
}

#[test]
fn scheduling_while_an_effect_is_outstanding_is_refused() {
    // The first effect owns the writer. A second schedule would drop it, leaving the first
    // effect with no way to record its outcome — and §08 has no edge from an unresolved
    // effect to a terminal record, so that run could never end.
    assert_eq!(
        drive(Misuse::ScheduleTwice),
        Err(DriveError::EffectOutstanding)
    );
}

#[test]
fn a_refused_misuse_writes_no_effect_record() {
    // The refusal is a refusal, not a repair: the run's own record is on media because the
    // boot wrote it, and nothing else is.
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let _refused = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut Idle,
        &mut Misusing(Misuse::ResolveFirst),
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    let mut recovery = waymaker_flash::recovery::Recovery::new(region());
    let mut records = 0_usize;
    while let Some(step) = recovery.next(&mut device, &mut page) {
        assert!(step.is_ok(), "the journal this boot wrote is legal");
        records += 1;
    }
    assert_eq!(records, 1, "the `RunStarted` record and nothing else");
}

// A caller that schedules and then stops without resolving is the window the façade lives
// in, and `Driver` reports it as `Progress::Waiting`. It cannot be driven from here:
// `Suspended` has a private field, so only this crate can stop a run without going through a
// boundary. `crates/waymaker-drive/tests/ota.rs` drives it through the façade instead.

/// A workflow that drives the bridge itself, one call at a time.
///
/// `Bridge` is four renames over the boundary, and the only caller that reaches it in
/// anger is a `Ctx`. Driving it directly is what puts each rename under a test: the
/// façade's own sequencing is `crates/waymaker-embassy/tests/ctx.rs`, and what is here is
/// that each of the four calls lands where the driver expects it.
struct Bridging(Bridged);

/// Which of the bridge's four calls to make.
#[derive(Clone, Copy)]
enum Bridged {
    /// A deadline that has already passed, then a completion.
    Deadline,
    /// §10's swap, which this driver refuses.
    Restart,
    /// An effect the world answered with a failure payload.
    FailedEffect,
    /// An effect whose answer is wider than the run's bound.
    ExhaustedEffect,
}

impl Workflow for Bridging {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: URL,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        let mut bridge = Bridge::over(boundary);
        match self.0 {
            Bridged::Deadline => {
                // Zero ticks after this boot began, so the deadline has already passed. A
                // halt here is the driver's answer and the assertion is in the test.
                if bridge.wait(TimerSpec::AfterBoot { ticks: 0 }).is_err() {
                    return Ok(Outcome::Failed(&[]));
                }
            }
            Bridged::Restart => {
                // The driver has recorded its refusal, and a recorded stop outranks
                // whatever the workflow returns — so this may return anything at all.
                let _halted = bridge.continue_as_new(b"next");
            }
            Bridged::FailedEffect | Bridged::ExhaustedEffect => {
                // Each `else` is a way this could go wrong, named so the test that reads
                // the outcome says which. A panicking helper is denied here.
                let Ok(waymaker_embassy::Handoff::Dispatch(_)) = bridge.schedule(DOWNLOAD, b"one")
                else {
                    return Ok(Outcome::Failed(b"schedule"));
                };
                let answer = match self.0 {
                    Bridged::FailedEffect => Answer::Failed(b"nope"),
                    _ => Answer::Exhausted,
                };
                let Ok(Outcome::Failed(_)) = bridge.resolve(answer) else {
                    return Ok(Outcome::Failed(b"resolve"));
                };
            }
        }
        Ok(Outcome::Completed(&[]))
    }
}

fn bridged(case: Bridged) -> Result<waymaker_drive::Progress, DriveError<FaultError>> {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut Idle,
        &mut Bridging(case),
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

#[test]
fn a_deadline_reaches_the_driver_through_the_bridge() {
    // `Bridge::wait` is one rename, and this is what says it lands on §11's boundary rather
    // than somewhere else: a deadline of zero ticks has passed, so the run carries on.
    assert_eq!(
        bridged(Bridged::Deadline),
        Ok(waymaker_drive::Progress::Finished {
            conclusion: waymaker_drive::Conclusion::Completed,
            result_len: 0,
        })
    );
}

#[test]
fn continue_as_new_through_the_bridge_reaches_the_drivers_refusal() {
    // §10's swap works on a bank and this driver is pointed at a journal region. The façade
    // hands the ask down; the refusal is the driver's.
    assert_eq!(
        bridged(Bridged::Restart),
        Err(DriveError::ContinueUnsupported)
    );
}

#[test]
fn a_failed_answer_and_an_exhausted_one_both_reach_the_driver_as_failures() {
    // The two arms of `Bridge::resolve` a completion does not take. On media they are the
    // same statement — an `EffectFailed` — and the payload is what differs.
    for case in [Bridged::FailedEffect, Bridged::ExhaustedEffect] {
        let progress = bridged(case);
        assert!(
            matches!(
                progress,
                Ok(waymaker_drive::Progress::Finished {
                    conclusion: waymaker_drive::Conclusion::Completed,
                    ..
                })
            ),
            "{progress:?}"
        );
    }
}
