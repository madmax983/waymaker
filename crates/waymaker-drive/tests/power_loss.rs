//! A durable deadline across a total power cut, driven through a board's own clock.
//!
//! Issue [#34](https://github.com/madmax983/waymaker/issues/34), rung 0.5's exit criterion.
//! `tests/timer.rs` drives the same boundary against a world whose epoch a test sets; what
//! is here is the same workflow against the two drivers a board really brings —
//! `waymaker_rig::rtc::Rtc` over a battery-backed counter, and
//! `waymaker_rig::epoch::RestoredEpoch` over an epoch a network supplies.
//!
//! # What makes the cut total
//!
//! [`power_up`] takes the media and the backup domain and constructs everything else
//! itself: the board, the driver, the workflow, both buffers. So the only state that
//! crosses a cut in this file is state a part really keeps — bytes on NOR, and the counter
//! in a domain a battery or a supercapacitor holds up. RAM is gone because there is no
//! value left holding it. That is the split [`Rig::prepare`] and [`Rig::verify`] make at the
//! reset boundary, applied to a driver rather than to a rig.
//!
//! # What it is not
//!
//! A board run. Nothing here has been on hardware; `xtask::docs::HARDWARE_TARGETS` carries
//! the row that says so, and only an accepted ADR carrying the attestation marker moves it.
//!
//! [`Rig::prepare`]: waymaker_rig::run::Rig::prepare
//! [`Rig::verify`]: waymaker_rig::run::Rig::verify

use core::cell::Cell;

use waymaker_core::timer::{ClockCapability, ClockKind};
use waymaker_core::{ActivityKind, KernelError, RecordKind, RunId};
use waymaker_drive::demo::{DELAYED_BOUNDS, Delayed, World};
use waymaker_drive::{
    Activities, Clocks, Conclusion, DriveError, Driver, DurableIntent, Performed, Progress, Scratch,
};
use waymaker_embassy::clock::PersistentClock;
use waymaker_fault::Device;
use waymaker_flash::bank::BankLayout;
use waymaker_flash::capacity::Reserve;
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::epoch::{Monotonic, RestoredEpoch};
use waymaker_rig::rtc::{BackedRtc, Continuity, Rtc};

/// The run these journals belong to.
const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

/// The persistent instant [`Delayed`] waits for.
const DEADLINE: u64 = 1_700_000_000;

/// How far short of the deadline the first boot arms the timer.
const INTERVAL: u64 = 600;

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

/// How many `TimerScheduled` records the journal holds.
fn schedules(device: &mut Device) -> usize {
    kinds(device)
        .iter()
        .filter(|kind| **kind == RecordKind::TIMER_SCHEDULED)
        .count()
}

/// What a battery or a supercapacitor holds up while the supply is away.
///
/// The counter and the continuity flag, and nothing else. Everything a firmware knows lives
/// in RAM, and a cut takes RAM — so a test that let anything else cross a cut in this file
/// would be describing a part that does not exist.
struct BackupDomain {
    counter: Cell<u64>,
    continuity: Cell<Continuity>,
}

impl BackupDomain {
    /// A domain that has been counting since `counter`, with the supply never interrupted.
    const fn counting_from(counter: u64) -> Self {
        Self {
            counter: Cell::new(counter),
            continuity: Cell::new(Continuity::Held),
        }
    }

    /// The supply was away for `ticks` and the domain held.
    fn away_for(&self, ticks: u64) {
        self.counter.set(self.counter.get().saturating_add(ticks));
    }

    /// The supply was away and the battery did not last. The counter reset with it.
    fn battery_died(&self) {
        self.counter.set(0);
        self.continuity.set(Continuity::Lost);
    }
}

/// The board's half of the RTC: two register reads.
struct Registers<'a> {
    domain: &'a BackupDomain,
}

impl BackedRtc for Registers<'_> {
    type Error = ();

    fn counter(&mut self) -> Result<u64, ()> {
        Ok(self.domain.counter.get())
    }

    fn continuity(&mut self) -> Result<Continuity, ()> {
        Ok(self.domain.continuity.get())
    }
}

/// How far a boot clock moves between two reads.
///
/// Non-zero on purpose. A clock that never moves lets a restored epoch pass every test in
/// this file while ignoring the monotonic reading altogether, which is the arithmetic the
/// path exists for.
const TICKS_PER_READ: u64 = 10;

/// A monotonic boot clock, built for one power-up and dropped with it.
///
/// It starts at zero on every power cycle — which is what makes the epoch driver need the
/// network again after a cut — and it moves on every read, as a real one does.
struct BootClock {
    ticks: u64,
}

impl Monotonic for BootClock {
    type Error = ();

    fn ticks(&mut self) -> Result<u64, ()> {
        let reading = self.ticks;
        self.ticks = self.ticks.saturating_add(TICKS_PER_READ);
        Ok(reading)
    }
}

/// A firmware image: the persistent clock it was built with, or none, and its activities.
///
/// [`Clocks::capability`] is derived from whether a driver is present rather than set
/// beside it. A board with no clock cannot declare one here, which is the witness §11 asks
/// for — and it is what makes the boot-only case in this file a *different image* rather
/// than the same one lying about itself.
struct Board<C> {
    clock: Option<C>,
    boot_ticks: u64,
    world: World,
}

impl<C> Board<C> {
    /// A firmware built with `clock`.
    const fn with(clock: C) -> Self {
        Self {
            clock: Some(clock),
            boot_ticks: 0,
            world: World::new(),
        }
    }

    /// A firmware built without a persistent clock at all.
    const fn without_a_clock() -> Self {
        Self {
            clock: None,
            boot_ticks: 0,
            world: World::new(),
        }
    }
}

impl<C: PersistentClock> Clocks for Board<C> {
    fn capability(&self) -> ClockCapability {
        if self.clock.is_some() {
            ClockCapability::Persistent
        } else {
            ClockCapability::BootOnly
        }
    }

    fn now(&mut self, kind: ClockKind) -> Option<u64> {
        if kind == ClockKind::AT_PERSISTENT_TIME {
            // `ok()` discards which fault it was, because `Clocks::now` has one refusal.
            // The driver reports `ClockUnavailable` either way, and a driver that
            // substituted a number here is the substitution §02 decision 8 forbids.
            return self.clock.as_mut()?.now().ok();
        }
        if kind == ClockKind::AFTER_BOOT {
            // A counter rather than a constant. `Clocks` says an implementor must not
            // substitute a value, and a fixed number in this file would be one — even though
            // nothing here waits on a boot deadline.
            let reading = self.boot_ticks;
            self.boot_ticks = self.boot_ticks.saturating_add(TICKS_PER_READ);
            return Some(reading);
        }
        None
    }
}

impl<C> Activities for Board<C> {
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

/// One power-up of [`Delayed`] on `board`, against `media`.
///
/// Every value a boot needs but the two arguments is made here and dropped on return, which
/// is what makes the gap between two calls a total cut.
fn power_up<C: PersistentClock>(
    media: &mut Device,
    board: &mut Board<C>,
) -> Result<Progress, DriveError<<Device as StableStorage>::Error>> {
    let mut workflow = Delayed::new();
    let mut page = [0_u8; 256];
    let mut result = [0_u8; 64];
    Driver::new(region(), RUN, reserve()).boot(
        media,
        board,
        &mut workflow,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    )
}

/// A board whose RTC reads `domain`.
const fn on_rtc(domain: &BackupDomain) -> Board<Rtc<Registers<'_>>> {
    Board::with(Rtc::over(Registers { domain }))
}

/// One power-up of a device that gets its time from a network.
///
/// `restored` is what the network answered on this boot, or [`None`] where it has not
/// answered yet. The boot clock is built here too, not only the board: it starts at zero on
/// every power cycle, so a cut that left it running would be no cut. Media is the only thing
/// this call shares with the last one.
fn power_up_on_network_time(
    media: &mut Device,
    restored: Option<u64>,
) -> Result<Progress, DriveError<<Device as StableStorage>::Error>> {
    let mut board = Board::with(RestoredEpoch::awaiting(BootClock { ticks: 0 }));
    if let (Some(clock), Some(reading)) = (board.clock.as_mut(), restored) {
        let Ok(()) = clock.restore(reading) else {
            unreachable!("this board's boot clock always answers")
        };
    }
    power_up(media, &mut board)
}

#[test]
fn a_persistent_deadline_survives_a_total_power_cut_and_is_elapsed_on_the_first_replay() {
    // Issue #34's first "done when", against the driver a board brings. The supply is away
    // for longer than the interval, and the first look after it is back says the deadline
    // passed — from the record and the counter, because nothing else crossed the cut.
    let mut media = Device::new(geometry());
    let domain = BackupDomain::counting_from(DEADLINE - INTERVAL);

    let armed = power_up(&mut media, &mut on_rtc(&domain));
    assert!(
        matches!(armed, Ok(Progress::WaitingUntil { remaining, .. }) if remaining == INTERVAL),
        "{armed:?}"
    );
    assert_eq!(
        kinds(&mut media),
        vec![RecordKind::RUN_STARTED, RecordKind::TIMER_SCHEDULED],
        "§07's order: the deadline is on media before the run can observe it as passed"
    );

    // The cut. `media` and `domain` are the only values that reach the next line, and the
    // supply is away for longer than the interval the workflow asked for.
    domain.away_for(INTERVAL + 1);

    let replayed = power_up(&mut media, &mut on_rtc(&domain));
    assert!(
        matches!(
            replayed,
            Ok(Progress::Finished {
                conclusion: Conclusion::Completed,
                ..
            })
        ),
        "the first replay after the supply came back must recognise the deadline: {replayed:?}"
    );
    assert!(kinds(&mut media).contains(&RecordKind::TIMER_FIRED));
    assert_eq!(
        schedules(&mut media),
        1,
        "the deadline is committed once; a second schedule is a re-armed timer"
    );
}

#[test]
fn a_board_with_no_persistent_clock_refuses_the_same_workflow() {
    // Issue #34's second "done when" and its third work item. The same workflow, on an
    // image built without the hardware: refused, and refused *before* a record it could
    // never arm reaches media. §02 decision 8 — never an approximation.
    let mut media = Device::new(geometry());
    let mut board = Board::<Rtc<Registers<'_>>>::without_a_clock();

    let refused = power_up(&mut media, &mut board);

    assert_eq!(
        refused,
        Err(DriveError::Kernel(KernelError::NoPersistentClock))
    );
    assert_eq!(
        KernelError::NoPersistentClock.message(),
        "this firmware has no persistent clock",
        "issue #34 asks the refusal to name the missing capability"
    );
    assert_eq!(
        kinds(&mut media),
        vec![RecordKind::RUN_STARTED],
        "a committed deadline this image can never arm strands the run for ever"
    );
}

#[test]
fn a_board_whose_battery_died_refuses_rather_than_firing_the_deadline() {
    // The quiet failure the continuity register exists for. A backup domain that did not
    // hold leaves the counter at its reset value, which is below every instant a workflow
    // waits for — so a driver that reported the number would fire this deadline, and every
    // other one on the device, at once.
    let mut media = Device::new(geometry());
    let domain = BackupDomain::counting_from(DEADLINE - INTERVAL);
    let armed = power_up(&mut media, &mut on_rtc(&domain));
    assert!(
        matches!(armed, Ok(Progress::WaitingUntil { remaining, .. }) if remaining == INTERVAL),
        "the first boot has to arm the deadline for the second to be about anything: {armed:?}"
    );
    assert_eq!(schedules(&mut media), 1);

    domain.battery_died();

    assert_eq!(
        power_up(&mut media, &mut on_rtc(&domain)),
        Err(DriveError::ClockUnavailable)
    );
    assert!(
        !kinds(&mut media).contains(&RecordKind::TIMER_FIRED),
        "a deadline nothing can measure is refused, not fired"
    );
}

#[test]
fn a_counter_that_moved_backwards_across_the_cut_is_refused() {
    // A battery changed while the supply was away, or an epoch re-synchronised behind where
    // it was. The domain still says it held, so the driver hands the number on and the
    // kernel meets a reading below the one the record carries — an interval it cannot
    // measure, refused rather than credited or discarded.
    let mut media = Device::new(geometry());
    let domain = BackupDomain::counting_from(DEADLINE - INTERVAL);
    let armed = power_up(&mut media, &mut on_rtc(&domain));
    assert!(
        matches!(armed, Ok(Progress::WaitingUntil { remaining, .. }) if remaining == INTERVAL),
        "{armed:?}"
    );

    domain.counter.set(DEADLINE - INTERVAL - 400);

    assert_eq!(
        power_up(&mut media, &mut on_rtc(&domain)),
        Err(DriveError::Kernel(KernelError::ClockWentBackwards))
    );
}

#[test]
fn a_network_device_waits_until_its_epoch_is_restored() {
    // Issue #34's fourth work item, driven rather than described. An externally-restored
    // epoch lives in RAM, so a total cut takes it: the boot after the cut cannot say what
    // time it is, and the deadline is neither fired nor discarded. It fires on the boot that
    // has been told the time again.
    let mut media = Device::new(geometry());

    let armed = power_up_on_network_time(&mut media, Some(DEADLINE - INTERVAL));
    assert!(
        matches!(armed, Ok(Progress::WaitingUntil { remaining, .. }) if remaining < INTERVAL),
        "the first boot arms the deadline against the epoch it was told, and the epoch has \
         moved with the boot clock by the time the intent is committed: {armed:?}"
    );

    // The cut. The epoch and the boot clock were both in RAM, and both are gone.
    assert_eq!(
        power_up_on_network_time(&mut media, None),
        Err(DriveError::ClockUnavailable),
        "a device that has not been told the time does not guess at one"
    );
    assert!(!kinds(&mut media).contains(&RecordKind::TIMER_FIRED));

    // The network answers, past the instant the workflow waited for.
    let replayed = power_up_on_network_time(&mut media, Some(DEADLINE + 1));
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
    assert_eq!(
        schedules(&mut media),
        1,
        "three boots, one committed deadline"
    );
}

#[test]
fn a_restored_epoch_advances_while_the_boot_it_was_restored_in_runs() {
    // The claim the assertion above rests on, stated where it can fail on its own. The
    // driver reads the persistent clock to arm the deadline and again once the intent is
    // committed, and a restored epoch that ignored its boot clock would report the same
    // number both times — so `remaining` would be the whole interval rather than less.
    let mut media = Device::new(geometry());

    let armed = power_up_on_network_time(&mut media, Some(DEADLINE - INTERVAL));
    let Ok(Progress::WaitingUntil { remaining, .. }) = armed else {
        unreachable!("the deadline is in the future, so this boot suspends: {armed:?}")
    };
    assert!(
        remaining < INTERVAL,
        "a restored epoch that ignored the boot clock would owe the whole {INTERVAL}, and \
         this one owes {remaining}"
    );
    assert!(
        remaining >= INTERVAL.saturating_sub(TICKS_PER_READ * 8),
        "and it would owe far less than {remaining} if it were advancing by something other \
         than the boot clock"
    );
}
