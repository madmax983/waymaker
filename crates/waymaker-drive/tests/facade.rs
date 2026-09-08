//! The two halves of design document §07, called out of order.
//!
//! [`Boundary::schedule`] hands the writer to the effect it committed, and
//! [`Boundary::resolve`] takes it back. A caller that splits §07 in two and then loses its
//! place must be refused rather than left with a run §08 can never end, so both misuses are
//! named errors and both are driven here.
//!
//! The ordinary path is `crates/waymaker-drive/tests/ota.rs`.

use waymaker_core::timer::{ClockCapability, ClockKind};
use waymaker_core::{ActivityKind, Outcome};
use waymaker_drive::ota::{BOUNDS, DOWNLOAD, URL, WORKFLOW_KIND, WORKFLOW_VERSION};
use waymaker_drive::{
    Activities, Answered, Boundary, Clocks, DriveError, Driver, DurableIntent, Handoff, Identity,
    Performed, Scratch, Suspended, Workflow,
};
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
