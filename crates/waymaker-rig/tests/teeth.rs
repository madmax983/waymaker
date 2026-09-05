//! Writers that are wrong in one way each, and the guarantee each one must break.
//!
//! A rig whose oracle has never been seen to fail is a rig nobody should believe. Every
//! guarantee in [`waymaker_rig::audit`] is a claim that some real mistake would be caught,
//! and these are the mistakes: not injected faults, but *writers* — the same primitives in
//! the wrong order, which is how the bug would actually arrive.
//!
//! Each writer here is a plausible simplification of [`Rig::iterate`]. That is the point: a
//! tooth that could only be produced by a deliberate act of sabotage says nothing about the
//! code a tired contributor writes at the end of a long day.

use waymaker_fault::{Device, Harness, Session};
use waymaker_flash::append::Journal;
use waymaker_flash::recovery::Recovery;
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::audit::Breach;
use waymaker_rig::cutter::{Dispatcher, NeverCut};
use waymaker_rig::log::Outcome;
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Rig, Verdict};
use waymaker_rig::wear::{Metered, Traffic};
use waymaker_rig::window::Window;
use waymaker_rig::witness::{Mark, Stage, Witness};
use waymaker_rig::workload::{Role, Workload};

const SEED: u64 = 0x0BAD_1DEA_0BAD_1DEA;
const EFFECTS: u16 = 2;

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(6 * 256, 256, 4, 1) else {
        unreachable!("a legal geometry")
    };
    geometry
}

fn rig() -> Rig {
    let Ok(rig) = Rig::new::<waymaker_fault::FaultError>(geometry(), Plan::new(SEED), EFFECTS)
    else {
        unreachable!("the geometry above holds two banks and a witness")
    };
    rig
}

struct Silent;

impl Dispatcher for Silent {
    type Error = core::convert::Infallible;

    fn dispatch(&mut self, _effect: u16, _input: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Which step of a record the witness's acknowledgment is written before.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flaw {
    /// The acknowledgment mark goes down before the commit seal, so a crash between them
    /// leaves the witness claiming a record that is not committed history.
    AcknowledgeBeforeCommit,
    /// The dispatch mark goes down before the commit seal, so a crash between them leaves an
    /// effect claimed as dispatched with no recoverable schedule record. Design document §07
    /// step 4 is exactly this order, the right way round.
    DispatchBeforeCommit,
}

/// One iteration written by a rig with `flaw` in it.
///
/// The same primitives `Rig::iterate` uses, in one wrong order. Deliberately not a flag on
/// the real writer: a defect switch inside the code under test is a defect switch somebody
/// eventually flips in production.
fn wrong_writer(flaw: Flaw, session: &mut Session) -> Result<(), ()> {
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut part = Metered::new(session);
    rig.prepare(&mut part, 0, &mut page).map_err(|_| ())?;

    let workload = rig.workload(0);
    let region = {
        let mut engine =
            Window::new(&mut part, 0, rig.layout().geometry().capacity()).map_err(|_| ())?;
        let mut header = [0_u8; Rig::PAGE_BYTES];
        let region = rig.layout().bank(Rig::BANK);
        let want = (region.payload_bytes() as usize).min(header.len());
        let slot = header.get_mut(..want).ok_or(())?;
        engine.read(region.base(), slot).map_err(|_| ())?;
        let decoded = waymaker_flash::bank::decode_header(slot).map_err(|_| ())?;
        waymaker_flash::recovery::JournalRegion::of(rig.layout(), Rig::BANK, &decoded)
            .map_err(|_| ())?
    };

    let mut journal = {
        let mut engine =
            Window::new(&mut part, 0, rig.layout().geometry().capacity()).map_err(|_| ())?;
        let mut recovery = Recovery::new(region);
        while let Some(step) = recovery.next(&mut engine, &mut page) {
            step.map_err(|_| ())?;
        }
        Journal::after(recovery).ok_or(())?
    };

    // A second buffer, and not an incidental one: `Sealable` borrows the page between the
    // payload barrier and the commit seal, so `waymaker-flash`'s own typestate refuses to let
    // this writer reuse `page` for a mark in the window it is trying to open. The flaw is
    // still writable — it just costs the wrong writer a buffer of its own, which is a fair
    // description of how it would arrive in real code.
    let mut mark_page = [0_u8; Rig::PAGE_BYTES];
    let mut witness = Witness::new(rig.witness_region());
    let mut record_page = [0_u8; Workload::MAX_PAYLOAD_BYTES];
    let instrument_base = rig.instrument_base();
    let instrument_bytes = geometry().erase_size();

    let mut mark = |part: &mut Metered<'_, Session>, mark: Mark, page: &mut [u8]| {
        part.set_traffic(Traffic::Rig);
        let outcome = {
            let mut instrument =
                Window::new(part, instrument_base, instrument_bytes).map_err(|_| ())?;
            witness.mark(&mut instrument, mark, page).map_err(|_| ())
        };
        part.set_traffic(Traffic::Engine);
        outcome
    };

    for index in 0..workload.records().ok_or(())? {
        let role = workload.role(index).ok_or(())?;
        mark(
            &mut part,
            Mark::new(0, index, Stage::Attempted),
            &mut mark_page,
        )?;

        let mut engine =
            Window::new(&mut part, 0, rig.layout().geometry().capacity()).map_err(|_| ())?;
        let record = workload.record(index, &mut record_page).ok_or(())?;
        let staged = journal
            .stage(&mut engine, &record, &mut page)
            .map_err(|_| ())?;
        let sealable = staged.payload_barrier(&mut engine).map_err(|_| ())?;
        // The window is a borrow of `part`, and the marks below need it back. Ended with a
        // scope rather than a `drop`, because a `Window` has no destructor and dropping one
        // only extends the borrow it holds.

        // The flaw: a mark that belongs after the commit seal, written before it.
        match (flaw, role) {
            (Flaw::AcknowledgeBeforeCommit, _) => {
                mark(
                    &mut part,
                    Mark::new(0, index, Stage::Acknowledged),
                    &mut mark_page,
                )?;
            }
            (Flaw::DispatchBeforeCommit, Role::Schedule(_)) => {
                mark(
                    &mut part,
                    Mark::new(0, index, Stage::Dispatched),
                    &mut mark_page,
                )?;
            }
            (Flaw::DispatchBeforeCommit, _) => {}
        }

        {
            let mut engine =
                Window::new(&mut part, 0, rig.layout().geometry().capacity()).map_err(|_| ())?;
            sealable.commit(&mut engine).map_err(|_| ())?;
        }

        if flaw == Flaw::DispatchBeforeCommit {
            mark(
                &mut part,
                Mark::new(0, index, Stage::Acknowledged),
                &mut mark_page,
            )?;
        }
    }
    Ok(())
}

/// Every breach `flaw` produced across the whole crash sweep.
fn breaches(flaw: Flaw) -> Vec<Breach> {
    let harness = Harness::new(geometry());
    let Ok(runs) = harness.run(|session| wrong_writer(flaw, session)) else {
        unreachable!("the fault-free run of a wrong writer still succeeds")
    };
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    let mut found = Vec::new();
    for run in &runs {
        let Some(mut device) = Device::restored(geometry(), run.image().to_vec()) else {
            continue;
        };
        if let Ok(verdict) = rig.verify(0, &mut device, &mut page)
            && let Outcome::Breached(breach) = verdict.outcome()
        {
            found.push(breach);
        }
    }
    found
}

#[test]
fn the_control_writer_is_caught_by_nothing() {
    // Without this the tests below prove only that the oracle can fail, not that it fails
    // for the reason they name.
    let harness = Harness::new(geometry());
    let runs = harness
        .run(|session| {
            let rig = rig();
            let mut page = [0_u8; Rig::PAGE_BYTES];
            let mut part = Metered::new(session);
            rig.prepare(&mut part, 0, &mut page).map_err(|_| ())?;
            rig.iterate(0, &mut part, &mut Silent, &mut NeverCut, &mut page)
                .map(|_| ())
                .map_err(|_| ())
        })
        .expect("the fault-free run succeeds");
    let rig = rig();
    let mut page = [0_u8; Rig::PAGE_BYTES];
    for run in &runs {
        let Some(mut device) = Device::restored(geometry(), run.image().to_vec()) else {
            continue;
        };
        assert_eq!(
            rig.verify(0, &mut device, &mut page).map(Verdict::outcome),
            Ok(Outcome::Passed),
            "the real writer was caught at {:?}",
            run.injection()
        );
    }
}

/// The guarantee a flaw must break, and the ones it must not.
///
/// An exhaustive `match`, so a flaw added to [`Flaw`] and left out of this does not compile —
/// which is the standard `waymaker-conformance`'s teeth are held to, and the standard the
/// first version of this file only appeared to meet.
const fn owed(flaw: Flaw) -> (u8, &'static str) {
    match flaw {
        // §14 `acknowledged-durability`. The witness says the barrier returned; recovery does
        // not have the record.
        Flaw::AcknowledgeBeforeCommit => (
            Breach::LostAcknowledgedRecord { index: 0 }.code(),
            "acknowledged-durability",
        ),
        // §14 `durable-intent`, and §02 decision 3: "the schedule record crosses a durability
        // barrier before dispatch".
        Flaw::DispatchBeforeCommit => (
            Breach::DispatchedEffectWithoutSchedule { index: 0 }.code(),
            "durable-intent",
        ),
    }
}

#[test]
fn each_flaw_is_caught_by_its_own_guarantee_and_by_no_other() {
    // The half the first version of this file left out. `any(matches!(..))` over the whole
    // sweep says a flaw produced *some* breach somewhere; it does not say the breach was the
    // one the flaw opens, and a rig whose every flaw produced `RecordDiffers` would have
    // passed it while proving nothing about which guarantee is doing the work.
    for flaw in [Flaw::AcknowledgeBeforeCommit, Flaw::DispatchBeforeCommit] {
        let (expected, guarantee) = owed(flaw);
        let found = breaches(flaw);
        assert!(
            found.iter().any(|breach| breach.code() == expected),
            "the {guarantee} flaw was never caught by {guarantee}; got {found:?}"
        );
        let others: Vec<Breach> = found
            .iter()
            .copied()
            .filter(|breach| breach.code() != expected)
            .collect();
        assert!(
            others.is_empty(),
            "the {guarantee} flaw was also caught by {others:?}, so the sweep does not \
             distinguish which guarantee it breaks"
        );
    }
}

#[test]
fn a_flaw_is_wrong_in_its_own_window_rather_than_everywhere() {
    // A tooth that breached at *every* crash point would be a tooth that says nothing about
    // the specific window it opens — it would be caught by a rig that reported a violation
    // unconditionally.
    for flaw in [Flaw::AcknowledgeBeforeCommit, Flaw::DispatchBeforeCommit] {
        let found = breaches(flaw);
        let harness = Harness::new(geometry());
        let Ok(runs) = harness.run(|session| wrong_writer(flaw, session)) else {
            unreachable!("the fault-free run of a wrong writer still succeeds")
        };
        assert!(!found.is_empty(), "a flaw produced no breach at all");
        assert!(
            found.len() < runs.len() / 2,
            "{} of {} crash points breached, so the tooth is not about its own window",
            found.len(),
            runs.len()
        );
    }
}
