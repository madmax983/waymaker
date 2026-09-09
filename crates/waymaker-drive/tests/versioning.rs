//! Workflow versioning driven end to end: an upgrade across a reboot, with and without a
//! recorded gate.
//!
//! Issue [#40](https://github.com/madmax983/waymaker/issues/40)'s third work item, against
//! the real driver, the real codec and `waymaker-fault`'s model of NOR. Design document
//! §08 states four rules; each has a section below.
//!
//! The workflow is [`Upgradable`]: prepare, then — from [`V2`] — verify, then finish. One
//! effect added in the middle is the smallest change §08's third rule is about, because
//! every effect after it shifts by one sequence.

use waymaker_core::version::{GateId, VersionRange};
use waymaker_core::{KernelError, RecordKind, RecordRef, RunId};
use waymaker_drive::demo::{
    Branching, FINISH, PREPARE, UPGRADABLE_BOUNDS, UPGRADE_GATE, Upgradable, V1, V2, VERIFY, World,
};
use waymaker_drive::{Conclusion, DriveError, Driver, Identity, Progress, Scratch, Workflow};
use waymaker_fault::{Device, FaultError, Harness, Session};
use waymaker_flash::append::Journal;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};

/// The run these journals belong to.
const RUN: RunId = RunId(0x00C0_FFEE_0000_0040);

/// What one boot answered.
type Booted = Result<Progress, DriveError<<Device as StableStorage>::Error>>;

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
    let Ok(reserve) = Reserve::for_layout(UPGRADABLE_BOUNDS, layout) else {
        unreachable!("the upgradable workflow's bounds fit this layout")
    };
    reserve
}

/// One boot of `workflow` against `device` and `world`.
fn boot(device: &mut Device, world: &mut World, workflow: &mut Upgradable) -> Booted {
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve()).boot(
        device,
        world,
        workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

/// One boot of a fresh image at `versions`, branching by `branching`.
fn boot_image(device: &mut Device, world: &mut World, workflow: Upgradable) -> Booted {
    let mut owned = workflow;
    boot(device, world, &mut owned)
}

/// The kind byte of every record the journal holds.
fn kinds(device: &mut Device) -> Vec<RecordKind> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut out = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journals these tests write are legal")
        };
        out.push(record.kind());
    }
    out
}

/// Every `VersionMarker` the journal holds, as `(gate, version)`.
fn markers(device: &mut Device) -> Vec<(GateId, u16)> {
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut out = Vec::new();
    while let Some(step) = recovery.next(device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journals these tests write are legal")
        };
        if let RecordRef::VersionMarker { gate, version, .. } = record {
            out.push((gate, version));
        }
    }
    out
}

/// A world that answers every activity and reads a clock nobody consults.
const fn fresh() -> World {
    World::new()
}

/// A world that stops answering at the `nth` dispatch of this boot, so the run suspends.
const fn world_pending_at(nth: usize) -> World {
    World::pending_at(nth)
}

// ---------------------------------------------------------------------------------------
// §08 rule 1: existing runs continue under compatible code for their recorded version
// ---------------------------------------------------------------------------------------

#[test]
fn a_run_recorded_at_v1_is_replayed_by_an_image_that_writes_v2() {
    // The rule this issue exists for. Before it, the driver compared the recorded version
    // for equality, so every run in flight was refused the moment the binary changed.
    let mut device = Device::new(geometry());
    let mut world = world_pending_at(0);

    // The v1 image starts the run and stops at its first effect.
    let first = boot_image(
        &mut device,
        &mut world,
        Upgradable::v1(Branching::RecordedVersion),
    );
    assert!(matches!(first, Ok(Progress::Waiting { .. })), "{first:?}");
    assert_eq!(kinds(&mut device)[0], RecordKind::RUN_STARTED);

    // The v2 image picks it up and carries it to the end.
    let second = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::v2(Branching::RecordedVersion),
    );
    assert!(
        matches!(
            second,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "{second:?}"
    );
}

#[test]
fn a_run_recorded_at_v1_takes_the_v1_path_under_the_v2_image() {
    // "Compatible code for their *recorded* version": the run began before `VERIFY`
    // existed, so it must not acquire one part way through.
    let mut device = Device::new(geometry());

    let _ = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::v1(Branching::RecordedVersion),
    );
    let replayed = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::v2(Branching::RecordedVersion),
    );

    assert!(
        matches!(replayed, Ok(Progress::Finished { .. })),
        "{replayed:?}"
    );
    // Two effects, not three: `PREPARE` and `FINISH`.
    assert_eq!(
        kinds(&mut device),
        vec![
            RecordKind::RUN_STARTED,
            RecordKind::EFFECT_SCHEDULED,
            RecordKind::EFFECT_COMPLETED,
            RecordKind::EFFECT_SCHEDULED,
            RecordKind::EFFECT_COMPLETED,
            RecordKind::RUN_COMPLETED,
        ]
    );
}

#[test]
fn a_fresh_run_records_the_version_the_image_writes() {
    let mut device = Device::new(geometry());

    let _ = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::v2(Branching::RecordedVersion),
    );

    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let first = recovery
        .next(&mut device, &mut page)
        .expect("the journal is not empty")
        .expect("the first record is legal");
    assert!(
        matches!(first, RecordRef::RunStarted { workflow_version, .. } if workflow_version == V2),
        "{first:?}"
    );
}

// ---------------------------------------------------------------------------------------
// §08 rule 2: a firmware image that cannot replay the recorded version returns
// `IncompatibleWorkflow`
// ---------------------------------------------------------------------------------------

#[test]
fn an_image_that_retired_v1_refuses_a_run_recorded_at_v1() {
    // The rollback's twin: this image *is* newer, but it dropped the old branch on purpose.
    // §08 says it returns `IncompatibleWorkflow` rather than replaying under whatever code
    // is left.
    let mut device = Device::new(geometry());
    let _ = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::v1(Branching::RecordedVersion),
    );

    let refused = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::new(VersionRange::exact(V2), Branching::RecordedVersion),
    );

    assert_eq!(
        refused,
        Err(DriveError::Kernel(KernelError::IncompatibleWorkflow))
    );
}

#[test]
fn an_image_older_than_the_run_refuses_it() {
    // The rollback. A v1 image meets a run a v2 image wrote, and replaying it means
    // executing branches this binary never had.
    let mut device = Device::new(geometry());
    let _ = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::v2(Branching::RecordedVersion),
    );

    let refused = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::v1(Branching::RecordedVersion),
    );

    assert_eq!(
        refused,
        Err(DriveError::Kernel(KernelError::IncompatibleWorkflow))
    );
}

#[test]
fn an_incompatible_version_is_not_a_different_workflow() {
    // Two refusals with two different causes. `NotThisWorkflow` says the journal belongs to
    // some other workflow; `IncompatibleWorkflow` says it belongs to this one, at a release
    // this image cannot run. A log that could not tell them apart would send an engineer to
    // look for the wrong bug.
    let mut device = Device::new(geometry());
    let _ = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::v1(Branching::RecordedVersion),
    );

    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let mut other = OtherWorkflow;
    let refused = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut fresh(),
        &mut other,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert_eq!(refused, Err(DriveError::NotThisWorkflow));
}

/// A workflow with another kind, for the refusal above.
struct OtherWorkflow;

impl Workflow for OtherWorkflow {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: 0xFFFF,
            versions: VersionRange::exact(V1),
            input: b"seed",
        }
    }

    fn run(
        &mut self,
        _boundary: &mut dyn waymaker_drive::Boundary,
    ) -> Result<waymaker_core::Outcome<'_>, waymaker_drive::Suspended> {
        unreachable!("the driver refuses this run before the workflow is called")
    }
}

// ---------------------------------------------------------------------------------------
// §08 rule 3: added, removed or reordered effects need a new version or a recorded gate
// ---------------------------------------------------------------------------------------

#[test]
fn a_recorded_gate_replays_the_branch_the_run_took() {
    // Issue #40's first work item, end to end: a run whose gate recorded `1` takes the v1
    // path for ever, on an image that would have chosen `2`.
    let mut device = Device::new(geometry());

    let first = boot_image(&mut device, &mut fresh(), Upgradable::v1(Branching::Gate));
    assert!(
        matches!(
            first,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "{first:?}"
    );
    assert_eq!(markers(&mut device), vec![(UPGRADE_GATE, V1)]);

    // The v2 image replays it. Nothing is dispatched and nothing is written: the run is
    // over, and the journal it reads is the one the v1 image wrote.
    let before = kinds(&mut device);
    let mut replaying = fresh();
    let replayed = boot_image(&mut device, &mut replaying, Upgradable::v2(Branching::Gate));
    assert!(
        matches!(
            replayed,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "{replayed:?}"
    );
    assert_eq!(replaying.dispatched(), &[]);
    assert_eq!(kinds(&mut device), before);
}

#[test]
fn a_gate_first_reached_after_an_upgrade_records_the_new_branch() {
    // What a gate can do that `recorded_version` cannot. The run began under v1 and
    // suspended before the gate; the upgrade lands; the first execution to reach the gate
    // is the v2 image, so `2` is recorded and the new step runs — in a run whose
    // `RunStarted` still says `1`.
    let mut device = Device::new(geometry());

    // v1 starts the run and stops at `PREPARE`, before the gate.
    let mut world = world_pending_at(0);
    let first = boot_image(&mut device, &mut world, Upgradable::v1(Branching::Gate));
    assert!(matches!(first, Ok(Progress::Waiting { .. })), "{first:?}");
    assert_eq!(markers(&mut device), vec![]);

    // v2 picks it up, reaches the gate for the first time, and records its own version.
    let second = boot_image(&mut device, &mut fresh(), Upgradable::v2(Branching::Gate));
    assert!(
        matches!(
            second,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "{second:?}"
    );
    assert_eq!(markers(&mut device), vec![(UPGRADE_GATE, V2)]);
    // Three effects: `PREPARE`, `VERIFY`, `FINISH`.
    assert_eq!(
        kinds(&mut device),
        vec![
            RecordKind::RUN_STARTED,
            RecordKind::EFFECT_SCHEDULED,
            RecordKind::EFFECT_COMPLETED,
            RecordKind::VERSION_MARKER,
            RecordKind::EFFECT_SCHEDULED,
            RecordKind::EFFECT_COMPLETED,
            RecordKind::EFFECT_SCHEDULED,
            RecordKind::EFFECT_COMPLETED,
            RecordKind::RUN_COMPLETED,
        ]
    );
}

#[test]
fn a_recorded_branch_a_rollback_cannot_replay_is_refused_by_the_gate() {
    // §08's second rule met at the *marker* rather than at the run's own record, which is
    // the case only a gate can produce: a run whose `RunStarted` says V1 — so `begin`
    // admits it — and whose marker says V2, which this image never had.
    //
    // The journal is built the way `a_gate_first_reached_after_an_upgrade_records_the_new_branch`
    // builds it, and that is load-bearing. Starting the run under the v2 image instead
    // would put V2 in `RunStarted` and `begin` would refuse before the gate was reached,
    // so the test would pass with `recorded_gate`'s `admits` deleted. Review of this change
    // ran that version and watched all nineteen tests here stay green.
    let mut device = Device::new(geometry());
    let mut world = world_pending_at(0);
    let _ = boot_image(&mut device, &mut world, Upgradable::v1(Branching::Gate));
    let _ = boot_image(&mut device, &mut fresh(), Upgradable::v2(Branching::Gate));
    assert_eq!(markers(&mut device), vec![(UPGRADE_GATE, V2)]);

    // The rollback. Its range is exactly V1, so the run's own record is fine and the
    // marker is not.
    let refused = boot_image(&mut device, &mut fresh(), Upgradable::v1(Branching::Gate));

    assert_eq!(
        refused,
        Err(DriveError::Kernel(KernelError::IncompatibleWorkflow))
    );
    // The refusal came from the gate, so the run's own record was accepted first: a
    // `NotThisWorkflow` here would mean `begin` had refused and the gate was never reached.
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let first = recovery
        .next(&mut device, &mut page)
        .expect("the journal is not empty")
        .expect("the first record is legal");
    assert!(
        matches!(first, RecordRef::RunStarted { workflow_version, .. } if workflow_version == V1),
        "the run must be one this image admits: {first:?}"
    );
}

#[test]
fn an_added_effect_with_no_gate_is_a_divergence() {
    // The tooth. Issue #40's third rule says an added effect needs a new version or a
    // recorded gate; `Branching::ImageVersion` has neither, and this is what that costs.
    // The v2 image replays a v1 run, calls `VERIFY` where history recorded `FINISH`, and
    // §08 stops the run rather than guessing.
    let mut device = Device::new(geometry());
    let _ = boot_image(
        &mut device,
        &mut fresh(),
        Upgradable::v1(Branching::ImageVersion),
    );
    let before = kinds(&mut device);

    let mut upgraded = fresh();
    let diverged = boot_image(
        &mut device,
        &mut upgraded,
        Upgradable::v2(Branching::ImageVersion),
    );

    assert_eq!(
        diverged,
        Err(DriveError::Kernel(KernelError::NondeterministicWorkflow))
    );
    // Nothing was written: history stands where the divergence found it. `PREPARE` is
    // replayed from the journal, so the only dispatch a correct replay could make is the
    // one the divergence stopped.
    assert_eq!(upgraded.dispatched(), &[]);
    assert_eq!(kinds(&mut device), before);
}

#[test]
fn a_gate_moved_ahead_of_the_first_effect_is_a_boundary_kind_divergence() {
    // §08's fourth rule as a failure: call-order sequencing is authoritative, so a gate
    // that moved is caught by where it is rather than by what it is called.
    //
    // Named for the flavour it really reaches. History holds `EffectScheduled` at the
    // position the moved gate asks about, so `version_intent` takes its `BoundaryKind` arm
    // and `recorded_gate` is never entered — review of this change proved it by deleting
    // the sequence check and watching every test here stay green. `recorded_gate`'s own
    // `Divergence::Sequence` needs a *marker* at the wrong position, which is
    // `a_marker_recorded_at_another_position_is_refused` below.
    let mut device = Device::new(geometry());
    let _ = boot_image(&mut device, &mut fresh(), Upgradable::v1(Branching::Gate));

    // The same gate, reached before `PREPARE` rather than after it.
    let mut moved = MovedGate;
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let diverged = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut fresh(),
        &mut moved,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert_eq!(
        diverged,
        Err(DriveError::Kernel(KernelError::NondeterministicWorkflow))
    );
}

/// [`Upgradable`] with the gate moved ahead of the first effect.
struct MovedGate;

impl Workflow for MovedGate {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: waymaker_drive::demo::UPGRADABLE_KIND,
            versions: VersionRange::exact(V1),
            input: b"seed",
        }
    }

    fn run(
        &mut self,
        boundary: &mut dyn waymaker_drive::Boundary,
    ) -> Result<waymaker_core::Outcome<'_>, waymaker_drive::Suspended> {
        let _ = boundary.gate(UPGRADE_GATE)?;
        let _ = boundary.call(PREPARE, b"go")?;
        let _ = boundary.call(FINISH, b"end")?;
        Ok(waymaker_core::Outcome::Completed(b""))
    }
}

#[test]
fn a_gate_renumbered_in_place_is_a_gate_divergence() {
    // Two gates that swapped places keep every sequence, so the sequence check cannot see
    // them. The gate id is what does — which is why it is on media.
    let mut device = Device::new(geometry());
    let _ = boot_image(&mut device, &mut fresh(), Upgradable::v1(Branching::Gate));

    let mut renumbered = RenumberedGate;
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let diverged = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut fresh(),
        &mut renumbered,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert_eq!(
        diverged,
        Err(DriveError::Kernel(KernelError::NondeterministicWorkflow))
    );
}

/// [`Upgradable`] with the gate at another number, in the same position.
struct RenumberedGate;

impl Workflow for RenumberedGate {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: waymaker_drive::demo::UPGRADABLE_KIND,
            versions: VersionRange::exact(V1),
            input: b"seed",
        }
    }

    fn run(
        &mut self,
        boundary: &mut dyn waymaker_drive::Boundary,
    ) -> Result<waymaker_core::Outcome<'_>, waymaker_drive::Suspended> {
        let _ = boundary.call(PREPARE, b"go")?;
        let _ = boundary.gate(GateId(0x00FF))?;
        let _ = boundary.call(FINISH, b"end")?;
        Ok(waymaker_core::Outcome::Completed(b""))
    }
}

// ---------------------------------------------------------------------------------------
// §08 rule 4: call-order sequencing is authoritative
// ---------------------------------------------------------------------------------------

#[test]
fn a_marker_spends_a_sequence_that_the_effects_after_it_are_numbered_past() {
    // A gate is a boundary in the one ordered history, not a note beside it. The effect
    // after the marker carries sequence 2, so an added or removed gate moves every effect
    // after it — which is what makes §08's third rule enforceable at all.
    let mut device = Device::new(geometry());
    let mut world = world_pending_at(1);

    let _ = boot_image(&mut device, &mut world, Upgradable::v2(Branching::Gate));

    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut sequences = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journals these tests write are legal")
        };
        match record {
            RecordRef::EffectScheduled { seq, .. } | RecordRef::VersionMarker { seq, .. } => {
                sequences.push(seq.0);
            }
            _ => {}
        }
    }
    // `PREPARE` at 0, the marker at 1, `VERIFY` at 2 — one space, three boundaries.
    assert_eq!(sequences, vec![0, 1, 2]);
}

#[test]
fn the_marker_reaches_media_before_the_effect_its_branch_caused() {
    // §02 decision 3's shape at a boundary whose effect is a branch inside the workflow,
    // read off the journal at the moment the run suspends *inside* the branch.
    //
    // What this holds is the *order on media*, and no more. That the marker crossed both of
    // §07's barriers before `gate` returned is `write`'s, shared with every other record,
    // and the window a crash leaves is
    // `a_recovered_marker_is_whole_or_absent_at_every_crash_point` below — an ordering
    // assertion cannot see it, because an append-only journal and a workflow that calls
    // `gate` before `call` already entail this order.
    let mut device = Device::new(geometry());
    // Stop at the second dispatch of the boot, which is `VERIFY` — the first effect the
    // v2 branch adds.
    let mut world = world_pending_at(1);

    let progress = boot_image(&mut device, &mut world, Upgradable::v2(Branching::Gate));

    assert!(
        matches!(progress, Ok(Progress::Waiting { .. })),
        "{progress:?}"
    );
    let recorded = kinds(&mut device);
    let marker_at = recorded
        .iter()
        .position(|kind| *kind == RecordKind::VERSION_MARKER)
        .expect("the gate recorded a marker");
    let verify_at = recorded
        .iter()
        .rposition(|kind| *kind == RecordKind::EFFECT_SCHEDULED)
        .expect("the branch scheduled an effect");
    assert!(
        marker_at < verify_at,
        "the marker must precede the effect its branch caused: {recorded:?}"
    );
}

#[test]
fn a_replayed_gate_records_no_second_marker_and_rewrites_no_prefix() {
    // A gate is not re-decided on the boot that replays it, exactly as a fired timer is not
    // re-armed. The record is the decision.
    //
    // This boot legitimately writes and dispatches, because the first one suspended part
    // way through: what it may not do is decide the gate again. "Writes nothing and
    // dispatches nothing" is the *completed* run's property, and
    // `a_recorded_gate_replays_the_branch_the_run_took` is where it is asserted.
    let mut device = Device::new(geometry());
    let mut world = world_pending_at(1);
    let _ = boot_image(&mut device, &mut world, Upgradable::v2(Branching::Gate));
    let after_first = kinds(&mut device);

    // The same image again, with the world answering. It must reach the same branch and
    // add exactly the records the rest of the run needs — never a second marker.
    let mut answering = fresh();
    let second = boot_image(&mut device, &mut answering, Upgradable::v2(Branching::Gate));

    assert!(
        matches!(
            second,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "{second:?}"
    );
    assert_eq!(markers(&mut device), vec![(UPGRADE_GATE, V2)]);
    let after_second = kinds(&mut device);
    assert_eq!(
        after_second.get(..after_first.len()).unwrap_or_default(),
        after_first.as_slice(),
        "the replayed prefix must be untouched"
    );
}

#[test]
fn the_recorded_version_a_workflow_reads_is_the_runs_and_not_the_images() {
    // The property that makes `recorded_version` deterministic: it is a fact about history,
    // so every boot of one run sees the same number however many times the binary changes.
    let mut device = Device::new(geometry());
    let mut world = world_pending_at(0);
    let _ = boot_image(
        &mut device,
        &mut world,
        Upgradable::v1(Branching::RecordedVersion),
    );

    let mut seen = SeenVersion(None);
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let _ = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut fresh(),
        &mut seen,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert_eq!(seen.0, Some(V1));
}

/// A workflow at v2 that records what `recorded_version` answered.
struct SeenVersion(Option<u16>);

impl Workflow for SeenVersion {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: waymaker_drive::demo::UPGRADABLE_KIND,
            versions: VersionRange::new(V1, V2).unwrap_or(VersionRange::exact(V2)),
            input: b"seed",
        }
    }

    fn run(
        &mut self,
        boundary: &mut dyn waymaker_drive::Boundary,
    ) -> Result<waymaker_core::Outcome<'_>, waymaker_drive::Suspended> {
        self.0 = Some(boundary.recorded_version());
        let _ = boundary.call(PREPARE, b"go")?;
        let _ = boundary.call(FINISH, b"end")?;
        Ok(waymaker_core::Outcome::Completed(b""))
    }
}

#[test]
fn a_fresh_run_reads_the_version_its_own_record_is_about_to_hold() {
    // The other side of the same call: on the boot that *writes* `RunStarted`, the recorded
    // version is the one this image just wrote, so a workflow's first execution and its
    // replays agree.
    let mut device = Device::new(geometry());
    let mut seen = SeenVersion(None);
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let _ = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut fresh(),
        &mut seen,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert_eq!(seen.0, Some(V2));
}

#[test]
fn a_gate_reached_with_an_effect_outstanding_is_refused() {
    // The driver's own guard, met at the third boundary. A caller that split §07 in two
    // holds the writer inside the outstanding effect, so a marker here would report
    // `NoAppendPoint` — named for what it is instead.
    let mut device = Device::new(geometry());
    let mut split = SplitThenGate;
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let refused = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut fresh(),
        &mut split,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert_eq!(refused, Err(DriveError::EffectOutstanding));
}

/// A workflow that takes §07's identity and then reaches a gate instead of resolving it.
struct SplitThenGate;

impl Workflow for SplitThenGate {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: waymaker_drive::demo::UPGRADABLE_KIND,
            versions: VersionRange::exact(V1),
            input: b"seed",
        }
    }

    fn run(
        &mut self,
        boundary: &mut dyn waymaker_drive::Boundary,
    ) -> Result<waymaker_core::Outcome<'_>, waymaker_drive::Suspended> {
        let _ = boundary.schedule(PREPARE, b"go")?;
        let _ = boundary.gate(UPGRADE_GATE)?;
        Ok(waymaker_core::Outcome::Completed(b""))
    }
}

#[test]
fn the_gates_activity_vocabulary_is_the_one_the_journal_records() {
    // A cheap guard against the reference workflow drifting: the three activity numbers are
    // what a journal read back names, so a test that asserted on record kinds alone would
    // pass with the wrong steps in it.
    let mut device = Device::new(geometry());
    let _ = boot_image(&mut device, &mut fresh(), Upgradable::v2(Branching::Gate));

    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut called = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journals these tests write are legal")
        };
        if let RecordRef::EffectScheduled { kind, .. } = record {
            called.push(kind);
        }
    }
    assert_eq!(called, vec![PREPARE, VERIFY, FINISH]);
}

/// Appends `record` to the journal as it stands, whatever follows it.
///
/// The only way to reach `recorded_gate`'s sequence check from the driver: a correct driver
/// never writes a marker at a sequence the run would not issue, so history that holds one
/// has to be written by something that is not this driver.
fn append_past_the_end(device: &mut Device, record: &RecordRef<'_>) {
    let mut page = [0_u8; 256];
    let mut scan = Recovery::new(region());
    while scan.next(device, &mut page).is_some() {}
    let Some(mut journal) = Journal::after(scan) else {
        unreachable!("the fixtures here end in erased media")
    };
    let Ok(staged) = journal.stage(device, record, &mut page) else {
        unreachable!("the record fits the region")
    };
    let Ok(sealable) = staged.payload_barrier(device) else {
        unreachable!("the model's barrier cannot fail")
    };
    let Ok(_) = sealable.commit(device) else {
        unreachable!("the model's program cannot fail here")
    };
}

#[test]
fn a_marker_recorded_at_another_position_is_refused() {
    // `recorded_gate`'s own sequence check, reached from the driver. The run has committed
    // nothing, so the sequence it would issue next is 0; the marker on media says 3.
    //
    // Review of this change deleted that check and watched every other test in this file
    // stay green, because the moved-gate test above reaches the `BoundaryKind` arm instead
    // and never enters `recorded_gate` at all.
    // A run whose first record is `RunStarted` and whose second is a marker three
    // sequences ahead of anything the run has issued.
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    let mut ahead = Device::new(geometry());
    append_past_the_end(
        &mut ahead,
        &RecordRef::RunStarted {
            workflow_kind: waymaker_drive::demo::UPGRADABLE_KIND,
            workflow_version: V1,
            input: b"seed",
        },
    );
    append_past_the_end(
        &mut ahead,
        &RecordRef::VersionMarker {
            seq: waymaker_core::EffectSeq(3),
            gate: UPGRADE_GATE,
            version: V1,
        },
    );

    let mut workflow = OnlyAGate;
    let refused = Driver::new(region(), RUN, reserve()).boot(
        &mut ahead,
        &mut fresh(),
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert_eq!(
        refused,
        Err(DriveError::Kernel(KernelError::NondeterministicWorkflow))
    );
}

/// A workflow whose first boundary is the gate, so nothing precedes the marker.
struct OnlyAGate;

impl Workflow for OnlyAGate {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: waymaker_drive::demo::UPGRADABLE_KIND,
            versions: VersionRange::exact(V1),
            input: b"seed",
        }
    }

    fn run(
        &mut self,
        boundary: &mut dyn waymaker_drive::Boundary,
    ) -> Result<waymaker_core::Outcome<'_>, waymaker_drive::Suspended> {
        let _ = boundary.gate(UPGRADE_GATE)?;
        Ok(waymaker_core::Outcome::Completed(b""))
    }
}

// ---------------------------------------------------------------------------------------
// The marker's own crash window, at every point the injector lists
// ---------------------------------------------------------------------------------------

/// One boot of the v2 image over `session`.
fn drive_gated(session: &mut Session) -> Result<(), DriveError<FaultError>> {
    let mut workflow = Upgradable::v2(Branching::Gate);
    let mut world = fresh();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve())
        .boot(
            session,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        )
        .map(|_| ())
}

/// Every record kind, and every marker, a crash image recovers to.
fn recovered(image: &[u8]) -> (Vec<RecordKind>, Vec<(GateId, u16)>) {
    let Some(mut device) = Device::restored(geometry(), image.to_vec()) else {
        unreachable!("the image is device-sized")
    };
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut seen = Vec::new();
    let mut found = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let Ok(record) = step else {
            break;
        };
        seen.push(record.kind());
        if let RecordRef::VersionMarker { gate, version, .. } = record {
            found.push((gate, version));
        }
    }
    (seen, found)
}

#[test]
fn a_recovered_marker_is_whole_or_absent_at_every_crash_point() {
    // A gate's whole promise is that the branch a run took is on media. A torn marker must
    // therefore be no marker at all — §09's commit seal is what makes that true — and a
    // recovered one must be the decision the writer meant, never a half-programmed body
    // that decodes as some other branch.
    //
    // Every other boundary is swept this way: the effect boundary in `tests/crash.rs`, and
    // both §06 examples in `tests/ota.rs` and `tests/provisioning.rs`. Until this, the gate
    // was the one that was not.
    let harness = Harness::new(geometry());
    let runs = harness
        .run(drive_gated)
        .expect("the fault-free run completes");
    assert!(
        runs.len() > 1,
        "the enumeration found crash points to sweep"
    );

    let reference = {
        let mut device = Device::new(geometry());
        let _ = boot_image(&mut device, &mut fresh(), Upgradable::v2(Branching::Gate));
        assert_eq!(markers(&mut device), vec![(UPGRADE_GATE, V2)]);
        kinds(&mut device)
    };

    let mut whole = 0_usize;
    let mut absent = 0_usize;
    for run in &runs {
        let (seen, found) = recovered(run.image());
        assert!(
            reference.starts_with(&seen),
            "recovery is not a prefix of the fault-free run, at {:?}: {seen:?}",
            run.injection()
        );
        match found.as_slice() {
            [] => absent += 1,
            [one] => {
                assert_eq!(
                    *one,
                    (UPGRADE_GATE, V2),
                    "a recovered marker is not the one the writer meant, at {:?}",
                    run.injection()
                );
                whole += 1;
            }
            many => panic!(
                "one gate recorded {} markers, at {:?}",
                many.len(),
                run.injection()
            ),
        }
    }
    // Both halves have to be reached, or the sweep is asserting about one of them.
    assert!(whole > 0, "no crash point left the marker whole");
    assert!(absent > 0, "no crash point landed before the marker");
}

#[test]
fn no_effect_the_branch_caused_survives_a_crash_the_marker_did_not() {
    // §02 decision 3's shape at a version boundary, swept. `VERIFY` exists only on the
    // branch the marker records, so an image holding its schedule record and no marker
    // would be a run whose branch the journal cannot account for — the same fault as an
    // effect with no committed intent.
    let harness = Harness::new(geometry());
    let runs = harness
        .run(drive_gated)
        .expect("the fault-free run completes");

    let mut before_the_gate = 0_usize;
    for run in &runs {
        let (seen, found) = recovered(run.image());
        if !found.is_empty() {
            continue;
        }
        before_the_gate += 1;
        let schedules = seen
            .iter()
            .filter(|kind| **kind == RecordKind::EFFECT_SCHEDULED)
            .count();
        assert!(
            schedules <= 1,
            "an effect from the branch survived a crash the marker did not, at {:?}: {seen:?}",
            run.injection()
        );
    }
    assert!(
        before_the_gate > 0,
        "no crash point landed before the marker, so this asserts about nothing"
    );
}
