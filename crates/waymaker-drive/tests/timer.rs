//! A durable deadline driven end to end: recorded, re-armed across a reset, and replayed.
//!
//! Issue [#33](https://github.com/madmax983/waymaker/issues/33)'s two "done when" clauses,
//! against the real driver, the real codec and `waymaker-fault`'s model of NOR.
//!
//! * **Replay of a fired timer re-arms no hardware.** The only thing this driver does to
//!   hardware is read a clock, so [`Clocks::now`] is counted and required to be untouched
//!   on the boot that replays a firing.
//! * **A flipped clock kind in a recorded frame is refused.** The byte is changed *on
//!   media* and the frame re-sealed with the real codec, so what recovery meets is a frame
//!   a writer could have written.

use waymaker_core::Outcome;
use waymaker_core::timer::{ClockCapability, ClockKind, TimerSpec};
use waymaker_core::version::VersionRange;
use waymaker_core::{ActivityKind, KernelError, RecordKind, RecordRef, RunId};
use waymaker_drive::demo::{DELAYED_BOUNDS, Delayed, World};
use waymaker_drive::{
    Activities, Boundary, Clocks, Conclusion, DriveError, Driver, DurableIntent, Identity,
    Performed, Progress, Scratch, Suspended, Workflow,
};
use waymaker_fault::Device;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::{self, ProgramAlign};
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};

/// The run these journals belong to.
const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// The persistent instant [`Delayed`] waits for.
const DEADLINE: u64 = 1_700_000_000;

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
    let Ok(reserve) = Reserve::for_layout(DELAYED_BOUNDS, layout) else {
        unreachable!("the delayed workflow's bounds fit this layout")
    };
    reserve
}

/// One boot of [`Delayed`] against `device` and `world`.
fn boot(
    device: &mut Device,
    world: &mut World,
) -> Result<Progress, DriveError<<Device as StableStorage>::Error>> {
    let mut workflow = Delayed::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve()).boot(
        device,
        world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
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

/// A world whose persistent clock reads `epoch`.
const fn world_at(epoch: u64) -> World {
    let mut world = World::new();
    world.set_epoch(epoch);
    world
}

#[test]
fn a_deadline_in_the_future_records_its_intent_and_suspends_the_run() {
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);

    let progress = boot(&mut device, &mut world);

    assert!(
        matches!(progress, Ok(Progress::WaitingUntil { remaining, .. }) if remaining == 500),
        "{progress:?}"
    );
    // §07's order: the deadline is on media before the run can observe it as passed.
    assert_eq!(
        kinds(&mut device),
        vec![RecordKind::RUN_STARTED, RecordKind::TIMER_SCHEDULED]
    );
}

#[test]
fn a_deadline_already_past_fires_in_the_same_boot() {
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE + 1);

    let progress = boot(&mut device, &mut world);

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "{progress:?}"
    );
    assert!(kinds(&mut device).contains(&RecordKind::TIMER_FIRED));
}

#[test]
fn a_deadline_is_re_armed_across_a_reset_from_the_reading_history_recorded() {
    // The reset takes the RAM the armed timer lived in. What comes back is the record.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    assert!(boot(&mut device, &mut world).is_ok());

    // A second boot with a fresh world: nothing but media survives.
    let mut rebooted = world_at(DEADLINE - 400);
    let progress = boot(&mut device, &mut rebooted);
    assert!(
        matches!(progress, Ok(Progress::WaitingUntil { remaining, .. }) if remaining == 400),
        "{progress:?}"
    );
    // The intent is committed once. A second boot must not schedule the timer again.
    assert_eq!(
        kinds(&mut device)
            .iter()
            .filter(|kind| **kind == RecordKind::TIMER_SCHEDULED)
            .count(),
        1
    );

    // And once the epoch passes the instant, the same identity fires.
    let mut later = world_at(DEADLINE);
    assert!(boot(&mut device, &mut later).is_ok());
    assert!(kinds(&mut device).contains(&RecordKind::TIMER_FIRED));
}

#[test]
fn replaying_a_fired_timer_reads_no_clock_at_all() {
    // Issue #33's first "done when". Reading the clock is the only thing this driver does
    // to hardware, so a boot that replays a firing and touches it is a boot that re-armed.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE + 1);
    assert!(boot(&mut device, &mut world).is_ok());
    assert!(world.clock_reads() > 0, "the first boot must arm the timer");

    let mut replaying = world_at(DEADLINE + 1);
    let progress = boot(&mut device, &mut replaying);

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "{progress:?}"
    );
    assert_eq!(
        replaying.clock_reads(),
        0,
        "a timer whose firing is in history must be answered from history"
    );
}

#[test]
fn a_clock_kind_flipped_on_media_is_refused_rather_than_reinterpreted() {
    // Issue #33's second "done when". The byte is changed on the device and the frame is
    // re-sealed with the real codec, so the refusal is about the *policy* rather than about
    // a checksum. The firmware here has the persistent clock, so nothing stops it reading
    // the record — only the fact that the record now names another deadline.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    assert!(boot(&mut device, &mut world).is_ok());

    flip_the_clock_kind(&mut device);

    let mut rebooted = world_at(DEADLINE - 400);
    assert_eq!(
        boot(&mut device, &mut rebooted),
        Err(DriveError::Kernel(KernelError::NondeterministicWorkflow))
    );
}

#[test]
fn a_persistent_deadline_a_firmware_cannot_measure_is_an_incompatible_workflow() {
    // The same flip, met by a firmware built without the clock: the record is intact and
    // this image cannot honour it. §02 decision 8 — a refusal, never a substitution.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    assert!(boot(&mut device, &mut world).is_ok());

    let mut boot_only = world_at(DEADLINE - 400);
    boot_only.set_capability(ClockCapability::BootOnly);
    assert_eq!(
        boot(&mut device, &mut boot_only),
        Err(DriveError::Kernel(KernelError::IncompatibleWorkflow))
    );
}

/// Rewrites the journal's `TimerScheduled` record with the other clock kind.
///
/// The whole record is re-encoded through the real codec, so what lands on media is a frame
/// whose header checksum, frame checksum and commit seal all hold. A test that changed one
/// byte and left the checksums alone would be testing the checksum.
fn flip_the_clock_kind(device: &mut Device) {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two")
    };
    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let at;
    let flipped = loop {
        let offset = recovery.offset();
        let Some(step) = recovery.next(device, &mut page) else {
            unreachable!("the journal holds a scheduled timer")
        };
        let Ok(record) = step else {
            unreachable!("the journal this test wrote is legal")
        };
        if let RecordRef::TimerScheduled {
            seq,
            deadline,
            armed_at,
            ..
        } = record
        {
            at = offset;
            break RecordRef::TimerScheduled {
                seq,
                clock_kind: ClockKind::AFTER_BOOT,
                deadline,
                armed_at,
            };
        }
    };

    let mut encoded = [0_u8; 128];
    let Ok(written) = frame::encode(&flipped, align, &mut encoded) else {
        unreachable!("128 bytes hold a scheduled timer")
    };
    // The model only clears bits, so the block is erased before the frame is put back.
    let Ok(()) = device.erase(0, 1024) else {
        unreachable!("the region is one whole erase block")
    };
    let Ok(()) = replay_prefix(device, at as usize) else {
        unreachable!("the prefix was read from this device")
    };
    let Some(bytes) = encoded.get(..written) else {
        unreachable!("the frame is `written` bytes long")
    };
    let Ok(()) = device.program(at, bytes) else {
        unreachable!("the record fits where it came from")
    };
    let Ok(()) = device.barrier() else {
        unreachable!("the model's barrier cannot fail")
    };
}

/// Re-programs the `RunStarted` record the erase above removed.
fn replay_prefix(device: &mut Device, upto: usize) -> Result<(), ()> {
    let Some(align) = ProgramAlign::new(4) else {
        return Err(());
    };
    let mut encoded = [0_u8; 128];
    let record = RecordRef::RunStarted {
        workflow_kind: waymaker_drive::demo::DELAYED_KIND,
        workflow_version: waymaker_drive::demo::DELAYED_VERSION,
        input: b"seed",
    };
    let Ok(written) = frame::encode(&record, align, &mut encoded) else {
        return Err(());
    };
    if written != upto {
        return Err(());
    }
    let Some(bytes) = encoded.get(..written) else {
        return Err(());
    };
    device.program(0, bytes).map_err(|_| ())?;
    device.barrier().map_err(|_| ())
}

#[test]
fn a_timer_and_an_activity_share_one_sequence_space() {
    // Issue #33: one ordered history. The timer takes sequence 0 and the download that
    // follows it takes sequence 1, so a downstream system deduplicating on
    // `(RunId, EffectSeq)` sees two boundaries and not one.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE + 1);
    assert!(boot(&mut device, &mut world).is_ok());

    let mut recovery = Recovery::new(region());
    let mut page = [0_u8; 256];
    let mut sequences = Vec::new();
    while let Some(step) = recovery.next(&mut device, &mut page) {
        let Ok(record) = step else {
            unreachable!("the journal this test wrote is legal")
        };
        match record {
            RecordRef::TimerScheduled { seq, .. } => sequences.push(("timer", seq.0)),
            RecordRef::EffectScheduled { seq, .. } => sequences.push(("effect", seq.0)),
            _ => {}
        }
    }
    assert_eq!(sequences, vec![("timer", 0), ("effect", 1)]);
}

#[test]
fn a_spec_the_firmware_cannot_service_is_refused_before_a_record_is_written() {
    // A fresh run on a firmware with no persistent clock. Refused at the boundary, so the
    // journal holds the run's own record and nothing else: a committed `TimerScheduled` the
    // firmware could never arm strands the run for ever.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    world.set_capability(ClockCapability::BootOnly);

    assert_eq!(
        boot(&mut device, &mut world),
        Err(DriveError::Kernel(KernelError::NoPersistentClock))
    );
    assert_eq!(kinds(&mut device), vec![RecordKind::RUN_STARTED]);
}

#[test]
fn the_delayed_workflow_waits_for_the_deadline_this_test_file_names() {
    // The fixture the tests above rest on: if `Delayed` stopped waiting for `DEADLINE`,
    // every assertion about remaining ticks would be about another number.
    assert_eq!(
        Delayed::SPEC,
        TimerSpec::AtPersistentTime { instant: DEADLINE }
    );
}

/// A workflow that waits on the boot clock, which no reset carries across.
///
/// `Delayed` waits on the persistent clock, so every test above exercises the kind for
/// which the recorded arming reading survives. This is the other one, and it is the case
/// that strands a run when the driver treats the two alike.
struct Napping {
    input: [u8; 4],
    spec: TimerSpec,
}

impl Napping {
    const SPEC: TimerSpec = TimerSpec::AfterBoot { ticks: 1_000 };
    /// Shorter than the latency of committing its own schedule record.
    const BRIEF: TimerSpec = TimerSpec::AfterBoot { ticks: 1 };

    const fn new() -> Self {
        Self {
            input: *b"seed",
            spec: Self::SPEC,
        }
    }

    /// The same workflow, waiting for `spec`.
    const fn waiting(spec: TimerSpec) -> Self {
        Self {
            input: *b"seed",
            spec,
        }
    }
}

impl Workflow for Napping {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: 9,
            versions: VersionRange::exact(1),
            input: &self.input,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        boundary.wait(self.spec)?;
        Ok(Outcome::Completed(b"woke"))
    }
}

/// One boot of [`Napping`].
fn nap(
    device: &mut Device,
    world: &mut World,
) -> Result<Progress, DriveError<<Device as StableStorage>::Error>> {
    let mut workflow = Napping::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve()).boot(
        device,
        world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

/// A boot-only world whose clock has been running for `ticks`.
const fn booted(ticks: u64) -> World {
    let mut world = World::new();
    world.set_capability(ClockCapability::BootOnly);
    world.advance(ticks);
    world
}

#[test]
fn a_boot_deadline_that_outlives_a_reset_owes_its_whole_interval_and_refuses_nothing() {
    // The boot clock restarts at zero, so the arming reading the record carries belongs to
    // a power cycle that is gone. Measuring against it refuses a healthy clock with
    // `ClockWentBackwards` — and §08 has no edge from an open boundary to a terminal
    // record, so the run could never end. That is a device stranded for ever on the
    // ordinary path, and this is the test that says it does not happen.
    let mut device = Device::new(geometry());
    let mut world = booted(5_000);
    assert!(
        matches!(
            nap(&mut device, &mut world),
            Ok(Progress::WaitingUntil { .. })
        ),
        "the first boot arms the deadline"
    );

    let mut rebooted = booted(200);
    assert!(
        matches!(
            nap(&mut device, &mut rebooted),
            Ok(Progress::WaitingUntil { remaining, .. }) if remaining == 1_000
        ),
        "a reset owes the whole interval again, and refuses nothing"
    );
}

#[test]
fn a_boot_deadline_accrues_within_one_power_cycle_and_fires() {
    // The other side of the same rule: while the clock is still above the reading the
    // record carries, no reset has happened and the interval accrues. Without this a boot
    // deadline would restart on every poll and could never elapse at all.
    let mut device = Device::new(geometry());
    let mut world = booted(5_000);
    assert!(matches!(
        nap(&mut device, &mut world),
        Ok(Progress::WaitingUntil { remaining, .. }) if remaining == 1_000
    ));

    let mut later = booted(5_400);
    assert!(matches!(
        nap(&mut device, &mut later),
        Ok(Progress::WaitingUntil { remaining, .. }) if remaining == 600
    ));

    let mut elapsed = booted(6_000);
    assert!(matches!(
        nap(&mut device, &mut elapsed),
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            ..
        })
    ));
}

#[test]
fn a_boot_deadline_carried_across_a_reset_waits_longer_than_it_asked_for() {
    // The imprecision a boot clock cannot avoid, measured rather than described. The
    // recorded arming reading is a high-water mark from a power cycle that is gone, and a
    // boot clock offers no evidence that a reset happened — so once the new cycle's clock
    // climbs back past that mark the interval accrues from it, and the deadline is reached
    // at 6000 ticks of the new cycle rather than at the 1000 it asked for.
    //
    // Design document §11 calls this deadline not power-loss durable, and this is the shape
    // that takes. What would close it is a reset-cause register or retained RAM; both are a
    // board's, and issue [#34](https://github.com/madmax983/waymaker/issues/34) is where a
    // real one is met.
    let mut device = Device::new(geometry());
    let mut world = booted(5_000);
    assert!(nap(&mut device, &mut world).is_ok());

    // The reset, and then the new cycle at the interval it actually asked for.
    let mut owed = booted(1_200);
    assert!(
        matches!(
            nap(&mut device, &mut owed),
            Ok(Progress::WaitingUntil { remaining, .. }) if remaining == 1_000
        ),
        "1200 ticks of a new cycle do not reach a deadline armed at 5000 of the old one"
    );

    let mut past = booted(6_000);
    assert!(matches!(
        nap(&mut device, &mut past),
        Ok(Progress::Finished {
            conclusion: Conclusion::Completed,
            ..
        })
    ));
}

#[test]
fn a_persistent_clock_that_moved_backwards_across_a_reset_is_still_refused() {
    // The other half of the same rule. A persistent floor does cross the reset, so a clock
    // that really moved back — a battery change, a re-synchronised epoch — is an interval
    // the kernel cannot measure, and it refuses rather than crediting or discarding one.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    assert!(boot(&mut device, &mut world).is_ok());

    let mut moved_back = world_at(DEADLINE - 900);
    assert_eq!(
        boot(&mut device, &mut moved_back),
        Err(DriveError::Kernel(KernelError::ClockWentBackwards))
    );
}

/// A world whose clock runs while the driver programs flash.
///
/// `World`'s clock only moves when a test moves it, so nothing above can see time pass
/// *during* a write. Programming a frame and crossing two barriers is not instant, and a
/// deadline shorter than that latency has already elapsed by the time its own intent is
/// committed. This clock advances on every read, which is what a real one does.
struct Ticking {
    world: World,
    per_read: u64,
}

impl Ticking {
    const fn new(per_read: u64) -> Self {
        let mut world = World::new();
        world.set_capability(ClockCapability::BootOnly);
        Self { world, per_read }
    }
}

impl Clocks for Ticking {
    fn capability(&self) -> ClockCapability {
        self.world.capability()
    }

    fn now(&mut self, kind: ClockKind) -> Option<u64> {
        let reading = self.world.now(kind);
        self.world.advance(self.per_read);
        reading
    }
}

impl Activities for Ticking {
    fn perform(
        &mut self,
        intent: DurableIntent,
        kind: ActivityKind,
        input: &[u8],
        out: &mut [u8],
    ) -> Performed {
        self.world.perform(intent, kind, input, out)
    }
}

#[test]
fn a_deadline_shorter_than_its_own_commit_latency_fires_in_the_boot_that_armed_it() {
    // Codex found this. The deadline is measured after the schedule record is committed, not
    // before it, so the ticks spent programming the frame and crossing its two barriers are
    // ticks the run really waited. Measuring against the pre-write reading discards them:
    // a deadline shorter than the write latency is reported as owing its whole interval and
    // suspends a run that has already waited long enough.
    //
    // The recorded arming reading is still the pre-write one — that is what went to media,
    // and it is the floor. Only the measurement moved.
    let mut device = Device::new(geometry());
    let mut world = Ticking::new(100);
    let mut workflow = Napping::waiting(Napping::BRIEF);
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    let progress = Driver::new(region(), RUN, reserve()).boot(
        &mut device,
        &mut world,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "a 1-tick deadline armed on a clock that moves 100 ticks per read has elapsed by the \
         time its own record is committed: {progress:?}"
    );
    assert!(kinds(&mut device).contains(&RecordKind::TIMER_FIRED));
}

#[test]
fn a_wait_says_which_clock_its_remaining_ticks_are_counted_in() {
    // `Clocks` lets the two clocks keep different units — "a reading is in that clock's own
    // unit", and the kernel never converts — so a firmware whose RTC counts seconds and
    // whose boot clock counts milliseconds is an ordinary one. A caller handed a bare
    // `remaining` could not tell which alarm to set it on, or by how much to scale it, which
    // makes a wait this driver documents as usable for sleeping unusable. Both kinds are
    // pinned here, because one arm reporting the other's kind is the mistake.
    let mut device = Device::new(geometry());
    let mut world = world_at(DEADLINE - 500);
    assert!(
        matches!(
            boot(&mut device, &mut world),
            Ok(Progress::WaitingUntil {
                clock_kind: ClockKind::AT_PERSISTENT_TIME,
                remaining: 500,
                ..
            })
        ),
        "a persistent deadline counts in the persistent clock's unit"
    );

    let mut boot_device = Device::new(geometry());
    let mut ticking = booted(5_000);
    assert!(
        matches!(
            nap(&mut boot_device, &mut ticking),
            Ok(Progress::WaitingUntil {
                clock_kind: ClockKind::AFTER_BOOT,
                remaining: 1_000,
                ..
            })
        ),
        "a boot deadline counts in the boot clock's unit"
    );
}

#[test]
fn a_boot_clock_that_regresses_while_the_intent_commits_is_refused() {
    // Both of the arming path's readings are taken in one boot, microseconds apart, so a
    // clock that goes backwards between them is a regressing or wrapped clock and not a
    // reset. `rearmed_at` exists for the reset case and answers the lower of the two, which
    // here would report zero elapsed time and hide the fault; the arming floor is the
    // recorded reading, so the kernel sees the regression and refuses.
    struct Regressing(u64, ClockCapability);

    impl Clocks for Regressing {
        fn capability(&self) -> ClockCapability {
            self.1
        }

        fn now(&mut self, _kind: ClockKind) -> Option<u64> {
            let reading = self.0;
            // The second read is below the first: the clock went backwards.
            self.0 = self.0.saturating_sub(500);
            Some(reading)
        }
    }

    impl Activities for Regressing {
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

    let mut device = Device::new(geometry());
    let mut world = Regressing(5_000, ClockCapability::BootOnly);
    let mut workflow = Napping::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];

    assert_eq!(
        Driver::new(region(), RUN, reserve()).boot(
            &mut device,
            &mut world,
            &mut workflow,
            Scratch {
                page: &mut page,
                result: &mut result,
            },
        ),
        Err(DriveError::Kernel(KernelError::ClockWentBackwards))
    );
}
