//! The two persistent clocks a board can bring, tested through the capability.
//!
//! Issue [#34](https://github.com/madmax983/waymaker/issues/34). Design document §11 names
//! two sources for a reading that survives power loss: an RTC held up by a battery or a
//! supercapacitor, and an epoch a network restores. Both are drivers, so both answer
//! `PersistentClock` and neither decides policy.
//!
//! What is tested here is the *refusal*. A deadline's arithmetic is `waymaker-core`'s and
//! the end-to-end power cut is `waymaker-drive/tests/power_loss.rs`'s. What a driver owes
//! is that a reading it cannot vouch for is an error and never a number: a zero fires every
//! persistent deadline at once, and a maximum fires none of them.
//!
//! The stand-in registers below are held in `Cell`s the test owns, so that moving a counter
//! is what it is on a part — the hardware changing under a driver that is still holding it.

use core::cell::{Cell, RefCell};

use waymaker_core::KernelError;
use waymaker_core::timer::Deadline;
use waymaker_embassy::clock::{ClockError, PersistentClock, PersistentTimer};
use waymaker_rig::epoch::{EpochFault, Monotonic, RestoredEpoch};
use waymaker_rig::rtc::{BackedRtc, Continuity, Rtc, RtcFault};

/// What a stand-in register read fails with. A real driver's error type is its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bus;

/// The state a board RTC keeps in its backup domain, and the two ways reading it can fail.
///
/// The two faults are separate flags rather than one, because the registers fail
/// independently on a real part: an I2C RTC can answer its status byte and then lose the
/// bus before the counter is clocked out.
struct Domain {
    counter: Cell<u64>,
    continuity: Cell<Continuity>,
    counter_faults: Cell<bool>,
    continuity_faults: Cell<bool>,
    reads: RefCell<Vec<&'static str>>,
}

impl Domain {
    const fn holding(counter: u64) -> Self {
        Self {
            counter: Cell::new(counter),
            continuity: Cell::new(Continuity::Held),
            counter_faults: Cell::new(false),
            continuity_faults: Cell::new(false),
            reads: RefCell::new(Vec::new()),
        }
    }

    fn lost(counter: u64) -> Self {
        let domain = Self::holding(counter);
        domain.continuity.set(Continuity::Lost);
        domain
    }
}

/// The board's half: two register reads and nothing else.
struct Registers<'a> {
    domain: &'a Domain,
}

impl BackedRtc for Registers<'_> {
    type Error = Bus;

    fn counter(&mut self) -> Result<u64, Bus> {
        self.domain.reads.borrow_mut().push("counter");
        if self.domain.counter_faults.get() {
            return Err(Bus);
        }
        Ok(self.domain.counter.get())
    }

    fn continuity(&mut self) -> Result<Continuity, Bus> {
        self.domain.reads.borrow_mut().push("continuity");
        if self.domain.continuity_faults.get() {
            return Err(Bus);
        }
        Ok(self.domain.continuity.get())
    }
}

/// An RTC driver over `domain`.
const fn rtc(domain: &Domain) -> Rtc<Registers<'_>> {
    Rtc::over(Registers { domain })
}

#[test]
fn a_counter_from_a_domain_that_held_is_a_reading() {
    let domain = Domain::holding(1_700_000_000);
    let mut clock = rtc(&domain);

    assert_eq!(clock.now(), Ok(1_700_000_000));
}

#[test]
fn a_domain_that_lost_power_is_a_fault_rather_than_a_zero() {
    // The failure this driver exists for. A backup domain that did not hold leaves the
    // counter at whatever the reset value is, and on most parts that is zero — which is
    // below every instant a workflow ever waits for, so every persistent deadline fires at
    // once. The register that says the domain broke is the only thing between the two.
    let domain = Domain::lost(0);
    let mut clock = rtc(&domain);

    assert_eq!(clock.now(), Err(RtcFault::ContinuityLost));
}

#[test]
fn a_counter_that_cannot_be_read_is_a_fault() {
    let domain = Domain::holding(1_000);
    domain.counter_faults.set(true);
    let mut clock = rtc(&domain);

    assert_eq!(clock.now(), Err(RtcFault::Register(Bus)));
}

#[test]
fn a_continuity_register_that_cannot_be_read_is_a_fault() {
    // A counter with no statement about the domain behind it is a number this driver
    // cannot vouch for, so it is refused rather than handed on.
    let domain = Domain::holding(1_000);
    domain.continuity_faults.set(true);
    let mut clock = rtc(&domain);

    assert_eq!(clock.now(), Err(RtcFault::Register(Bus)));
}

#[test]
fn continuity_is_read_after_the_counter() {
    // The order is the safe direction. A supercapacitor that browns out *during* the read
    // latches the flag, so asking afterwards catches a break the counter read already met;
    // asking first cannot. Pinned here because it otherwise reads as arbitrary.
    let domain = Domain::holding(1_000);
    let mut clock = rtc(&domain);
    let _ = clock.now();

    assert_eq!(*domain.reads.borrow(), vec!["counter", "continuity"]);
}

#[test]
fn an_rtc_that_kept_counting_reaches_the_deadline_it_was_armed_for() {
    let domain = Domain::holding(1_700_000_000);
    let mut clock = rtc(&domain);
    let mut armed =
        PersistentTimer::arm(&mut clock, 1_700_000_600).expect("the RTC answered its arming read");

    assert_eq!(
        armed.poll(&mut clock),
        Ok(Deadline::Remaining { ticks: 600 })
    );

    domain.counter.set(1_700_000_600);
    assert_eq!(armed.poll(&mut clock), Ok(Deadline::Elapsed));
}

#[test]
fn a_counter_that_rolled_over_is_refused_rather_than_credited() {
    // A part with a 32-bit seconds counter wraps, and this driver reports the register
    // rather than inventing an epoch to widen it with — it has none, because RAM did not
    // survive. A wrapped counter therefore reads below the arming reading, and the kernel
    // refuses the interval it cannot measure. Documented semantics, not an accident.
    let domain = Domain::holding(u64::from(u32::MAX) - 10);
    let mut clock = rtc(&domain);
    let mut armed =
        PersistentTimer::arm(&mut clock, u64::from(u32::MAX)).expect("the RTC answered");

    domain.counter.set(5);
    assert_eq!(
        armed.poll(&mut clock),
        Err(ClockError::Refused(KernelError::ClockWentBackwards))
    );
}

/// A boot clock the test moves. It restarts at zero on every power cycle, which is the
/// whole difference between the two drivers in this file.
struct BootClock<'a> {
    ticks: &'a Cell<u64>,
    faults: &'a Cell<bool>,
}

/// Everything a monotonic boot clock has, held where a test can move it.
struct Uptime {
    ticks: Cell<u64>,
    faults: Cell<bool>,
}

impl Uptime {
    const fn at(ticks: u64) -> Self {
        Self {
            ticks: Cell::new(ticks),
            faults: Cell::new(false),
        }
    }
}

impl Monotonic for BootClock<'_> {
    type Error = Bus;

    fn ticks(&mut self) -> Result<u64, Bus> {
        if self.faults.get() {
            return Err(Bus);
        }
        Ok(self.ticks.get())
    }
}

/// A network-time clock over `uptime`, with no epoch restored into it yet.
const fn epoch(uptime: &Uptime) -> RestoredEpoch<BootClock<'_>> {
    RestoredEpoch::awaiting(BootClock {
        ticks: &uptime.ticks,
        faults: &uptime.faults,
    })
}

#[test]
fn an_epoch_nobody_restored_is_a_fault_rather_than_a_zero() {
    // The externally-restored-epoch path's whole semantics, in one assertion. A device that
    // gets its time from a network comes back from a power cut knowing nothing, and the
    // honest answer to "what time is it" is that it cannot say yet.
    let uptime = Uptime::at(0);
    let mut clock = epoch(&uptime);

    assert_eq!(clock.now(), Err(EpochFault::NotRestored));
}

#[test]
fn a_restored_epoch_advances_with_the_monotonic_clock() {
    let uptime = Uptime::at(40);
    let mut clock = epoch(&uptime);
    clock
        .restore(1_700_000_000)
        .expect("the boot clock answered");

    assert_eq!(clock.now(), Ok(1_700_000_000));

    uptime.ticks.set(340);
    assert_eq!(clock.now(), Ok(1_700_000_300));
}

#[test]
fn a_re_sync_that_would_move_the_clock_backwards_is_refused() {
    // §11's invariant for a persistent clock is that its readings do not go backwards. A
    // network answer behind what this device has already reported is refused here rather
    // than left for a per-timer high-water mark to catch for one deadline and miss for the
    // next.
    let uptime = Uptime::at(0);
    let mut clock = epoch(&uptime);
    clock
        .restore(1_700_000_000)
        .expect("the boot clock answered");
    uptime.ticks.set(100);

    assert_eq!(clock.restore(1_700_000_050), Err(EpochFault::Regressed));
    assert_eq!(clock.now(), Ok(1_700_000_100));
}

#[test]
fn a_forward_re_sync_is_accepted() {
    let uptime = Uptime::at(0);
    let mut clock = epoch(&uptime);
    clock
        .restore(1_700_000_000)
        .expect("the boot clock answered");
    uptime.ticks.set(100);

    clock
        .restore(1_700_000_500)
        .expect("a later answer moves the clock forwards");
    assert_eq!(clock.now(), Ok(1_700_000_500));
}

#[test]
fn a_monotonic_clock_that_regressed_since_the_restore_is_refused() {
    // The restore anchored the epoch to a boot-clock reading. A later reading below it is a
    // clock that wrapped or was reset, and the time since the anchor is unknowable — so the
    // driver refuses instead of wrapping the subtraction into a distant future.
    let uptime = Uptime::at(500);
    let mut clock = epoch(&uptime);
    clock
        .restore(1_700_000_000)
        .expect("the boot clock answered");

    uptime.ticks.set(10);
    assert_eq!(clock.now(), Err(EpochFault::Regressed));
}

#[test]
fn an_epoch_that_would_not_fit_a_reading_is_refused() {
    let uptime = Uptime::at(0);
    let mut clock = epoch(&uptime);
    clock.restore(u64::MAX).expect("the boot clock answered");

    uptime.ticks.set(1);
    assert_eq!(clock.now(), Err(EpochFault::Unrepresentable));
}

#[test]
fn a_monotonic_clock_that_cannot_be_read_is_a_fault() {
    let uptime = Uptime::at(0);
    let mut clock = epoch(&uptime);
    clock
        .restore(1_700_000_000)
        .expect("the boot clock answered");

    uptime.faults.set(true);
    assert_eq!(clock.now(), Err(EpochFault::Monotonic(Bus)));
    assert_eq!(
        clock.restore(1_700_000_001),
        Err(EpochFault::Monotonic(Bus))
    );
}

#[test]
fn a_re_sync_after_a_boot_clock_reset_is_accepted() {
    // The lockout the review of this change found. A boot clock that reset is exactly the
    // state a re-sync is the remedy for, so refusing every `restore` while it holds leaves
    // the device unable to tell the time for the rest of the power cycle.
    let uptime = Uptime::at(500);
    let mut clock = epoch(&uptime);
    clock
        .restore(1_700_000_000)
        .expect("the boot clock answered");

    uptime.ticks.set(10);
    assert_eq!(clock.now(), Err(EpochFault::Regressed));

    clock
        .restore(1_700_000_400)
        .expect("the network answered while the boot clock was below its anchor");
    assert_eq!(clock.now(), Ok(1_700_000_400));
}

#[test]
fn a_re_sync_below_a_reading_already_given_is_refused_after_a_reset_too() {
    // The other half of the same rule. The anchor is gone, so the only floor left is the
    // highest reading this clock ever gave — and it still refuses a network answer behind it.
    let uptime = Uptime::at(500);
    let mut clock = epoch(&uptime);
    clock
        .restore(1_700_000_000)
        .expect("the boot clock answered");
    uptime.ticks.set(900);
    assert_eq!(clock.now(), Ok(1_700_000_400));

    uptime.ticks.set(10);
    assert_eq!(clock.restore(1_700_000_200), Err(EpochFault::Regressed));
    clock
        .restore(1_700_000_400)
        .expect("an answer level with the highest reading given is not a move backwards");
}

#[test]
fn a_clock_that_climbed_back_past_its_anchor_does_not_resume_below_a_reading_it_gave() {
    // The reading the anchor alone believes. At tick 600 the anchor's own arithmetic works
    // again and answers 1_700_000_100 — below the 1_700_000_400 this clock already gave, and
    // `Ok` without the floor. A deadline armed under that reading fires late rather than
    // being refused, which is worse than either.
    let uptime = Uptime::at(500);
    let mut clock = epoch(&uptime);
    clock
        .restore(1_700_000_000)
        .expect("the boot clock answered");
    uptime.ticks.set(900);
    assert_eq!(clock.now(), Ok(1_700_000_400));

    uptime.ticks.set(10);
    assert_eq!(clock.now(), Err(EpochFault::Regressed));

    uptime.ticks.set(600);
    assert_eq!(clock.now(), Err(EpochFault::Regressed));
}

#[test]
fn an_unrepresentable_reading_does_not_lock_out_a_re_sync() {
    // The same lockout wearing the other fault's hat: an anchor whose sum has overflowed is
    // an anchor no reading can come from, and a re-sync is the only way out of it.
    let uptime = Uptime::at(0);
    let mut clock = epoch(&uptime);
    clock.restore(u64::MAX).expect("the boot clock answered");

    uptime.ticks.set(1);
    assert_eq!(clock.now(), Err(EpochFault::Unrepresentable));

    clock
        .restore(u64::MAX)
        .expect("re-anchoring at the same reading moves nothing backwards");
    assert_eq!(clock.now(), Ok(u64::MAX));
}

#[test]
fn a_restored_epoch_does_not_survive_the_power_cut_an_rtc_does() {
    // The two drivers side by side, which is what the documentation of this path has to
    // say. The RTC's counter is in the backup domain and crosses the cut; the epoch is in
    // RAM and does not. So a network device answers `NotRestored` until it has been told
    // the time again, and never fires a durable deadline early on the strength of a zero.
    let domain = Domain::holding(1_700_000_000);
    let uptime = Uptime::at(400);
    // The power cycle before the cut. Both drivers live in this block and neither leaves it.
    {
        let mut before_rtc = rtc(&domain);
        let mut before_epoch = epoch(&uptime);
        before_epoch
            .restore(1_700_000_000)
            .expect("the boot clock answered");
        assert_eq!(before_rtc.now(), Ok(1_700_000_000));
        // The boot clock runs, and the restored epoch runs with it.
        uptime.ticks.set(700);
        assert_eq!(before_epoch.now(), Ok(1_700_000_300));
    }

    // The cut. The backup domain kept counting; RAM did not survive at all, so the epoch
    // driver is built again with nothing in it and the boot clock really is back at zero.
    domain.counter.set(1_700_000_900);
    uptime.ticks.set(0);
    let mut after_rtc = rtc(&domain);
    let mut after_epoch = epoch(&uptime);

    assert_eq!(after_rtc.now(), Ok(1_700_000_900));
    assert_eq!(after_epoch.now(), Err(EpochFault::NotRestored));
}
