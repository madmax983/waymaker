//! The persistent-clock capability, tested through the surface a board driver sees.
//!
//! Design document §11 puts `PersistentClock` in the adapter rather than in the kernel,
//! for the reason §05 puts `StableStorage` there: the kernel's must-not-own cell names a
//! clock, and a trait whose one method reads hardware is a driver interface.
//!
//! What these tests are about is the *capability*, not the arithmetic. Whether a deadline
//! is elapsed is `waymaker-core`'s and is tested there; what is here is that an
//! `AtPersistentTime` timer cannot exist without a clock, that a clock which fails is a
//! failure rather than a reading, and that a clock which goes backwards is refused.

use waymaker_core::KernelError;
use waymaker_core::timer::{ClockCapability, Deadline, Timer, TimerSpec};
use waymaker_embassy::clock::{ClockError, PersistentClock, PersistentTimer};

/// An RTC that answers with the readings the test supplied, in order.
///
/// A clock with no reading left fails, which is what a real RTC does when its bus does not
/// answer. So [`Rtc::failing`] is an empty list rather than a second mechanism, and the
/// helper never panics: `expect` in a helper of an integration test is denied here, and a
/// stand-in driver that panicked would report a test bug as a driver fault anyway.
struct Rtc {
    readings: Vec<u64>,
    taken: usize,
}

/// What this stand-in RTC fails with. A real driver's error type is its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fault;

impl Rtc {
    fn reading(values: &[u64]) -> Self {
        Self {
            readings: values.to_vec(),
            taken: 0,
        }
    }

    fn failing() -> Self {
        Self::reading(&[])
    }
}

impl PersistentClock for Rtc {
    type Error = Fault;

    fn now(&mut self) -> Result<u64, Fault> {
        let answer = self.readings.get(self.taken).copied().ok_or(Fault);
        self.taken = self.taken.saturating_add(1);
        answer
    }
}

#[test]
fn a_persistent_timer_can_only_be_armed_with_a_clock_in_hand() {
    // The compile-time half of issue #32's first work item. There is no other constructor:
    // reaching `TimerSpec::AtPersistentTime` through `PersistentTimer` requires a
    // `&mut C: PersistentClock`, and a firmware with no such type cannot write the call.
    let mut rtc = Rtc::reading(&[1_000, 1_500]);
    let mut armed = PersistentTimer::arm(&mut rtc, 2_000).expect("the clock answered");

    assert_eq!(
        armed.timer().spec(),
        TimerSpec::AtPersistentTime { instant: 2_000 }
    );
    assert_eq!(armed.timer().armed_at(), 1_000);
    assert_eq!(armed.poll(&mut rtc), Ok(Deadline::Remaining { ticks: 500 }));
}

#[test]
fn a_restored_epoch_past_the_instant_is_elapsed_on_the_first_poll() {
    // The across-power-loss case, driven through the capability rather than through the
    // arithmetic: the run re-arms on the new boot and the RTC has kept counting.
    let mut rtc = Rtc::reading(&[5_000, 5_000]);
    let mut armed = PersistentTimer::arm(&mut rtc, 2_000).expect("the clock answered");

    assert_eq!(armed.poll(&mut rtc), Ok(Deadline::Elapsed));
}

#[test]
fn a_clock_that_fails_is_a_failure_rather_than_a_reading() {
    // Issue #32's third work item, first half. There is no default reading: a zero here
    // would fire every persistent timer at once, and a `u64::MAX` would fire none of them.
    let mut arming = Rtc::failing();
    assert!(matches!(
        PersistentTimer::arm(&mut arming, 2_000),
        Err(ClockError::Unavailable(Fault))
    ));

    let mut polling = Rtc::reading(&[1_000]);
    let mut armed = PersistentTimer::arm(&mut polling, 2_000).expect("the clock answered once");
    let mut broken = Rtc::failing();
    assert_eq!(armed.poll(&mut broken), Err(ClockError::Unavailable(Fault)));
}

#[test]
fn a_clock_that_goes_backwards_is_refused_rather_than_believed() {
    // Issue #32's third work item, second half. A battery swap or an epoch re-sync can
    // move an RTC back; believing it would un-fire a timer that had already elapsed.
    let mut rtc = Rtc::reading(&[1_000, 999]);
    let mut armed = PersistentTimer::arm(&mut rtc, 2_000).expect("the clock answered");

    assert_eq!(
        armed.poll(&mut rtc),
        Err(ClockError::Refused(KernelError::ClockWentBackwards))
    );
}

/// A second driver, with its own epoch. Its readings are `u64` and mean something else.
struct NetworkEpoch {
    seconds: u64,
}

impl PersistentClock for NetworkEpoch {
    type Error = Fault;

    fn now(&mut self) -> Result<u64, Fault> {
        Ok(self.seconds)
    }
}

#[test]
fn a_timer_armed_by_one_clock_is_polled_by_that_clock() {
    // Codex found this on the first review round, and it is the failure this module exists
    // to prevent wearing a different hat: two drivers, two epochs, both `u64`. An RTC
    // counting milliseconds since 1970 and a network epoch counting seconds since boot
    // produce readings that are meaningless against each other, and a deadline measured
    // across them fires early or late with nothing to say so.
    //
    // The clock type is now part of the timer, so the mix-up is a compile error rather
    // than a wrong verdict. What this test can state at runtime is the other half: the two
    // timers are different types, and each is polled by the clock that armed it.
    let mut rtc = Rtc::reading(&[1_000_000, 1_000_500]);
    let mut armed_by_rtc: PersistentTimer<Rtc> =
        PersistentTimer::arm(&mut rtc, 1_002_000).expect("the RTC answered");

    let mut epoch = NetworkEpoch { seconds: 1_000 };
    let mut armed_by_epoch: PersistentTimer<NetworkEpoch> =
        PersistentTimer::arm(&mut epoch, 1_500).expect("the network epoch answered");

    // Each against its own clock. The RTC is 1 500 of its units short of its deadline; the
    // epoch is 500 of its units short of its own.
    assert_eq!(
        armed_by_rtc.poll(&mut rtc),
        Ok(Deadline::Remaining { ticks: 1_500 })
    );
    assert_eq!(
        armed_by_epoch.poll(&mut epoch),
        Ok(Deadline::Remaining { ticks: 500 })
    );

    // Had the clock type not been part of the timer, `armed_by_epoch.poll(&mut rtc)` would
    // have compiled and answered `Elapsed`: 1 000 000 is past the instant 1 500, so a
    // deadline 500 seconds away would have fired at once. That call is now rejected by the
    // compiler, and `the_wrong_clock_does_not_compile` is the doctest that proves it.
    let would_have_been_elapsed = armed_by_epoch.timer().evaluate(1_000_000);
    assert_eq!(would_have_been_elapsed, Ok(Deadline::Elapsed));
}

#[test]
fn the_facade_offers_no_route_from_a_boot_only_firmware_to_a_persistent_timer() {
    // The absence this module exists for, stated where a reader looks for it. The kernel's
    // own refusal is the runtime half; the façade's is that the persistent path takes a
    // clock and nothing else can produce the spec.
    assert_eq!(
        Timer::arm(
            TimerSpec::AtPersistentTime { instant: 2_000 },
            ClockCapability::BootOnly,
            0
        ),
        Err(KernelError::NoPersistentClock)
    );

    // And an ordinary in-boot delay is still an ordinary in-boot delay: the façade does
    // not turn it into a persistent one either, in the direction nobody looks at.
    let boot = Timer::arm(
        TimerSpec::AfterBoot { ticks: 50 },
        ClockCapability::BootOnly,
        0,
    )
    .expect("a boot-only firmware arms an after-boot timer");
    assert_eq!(boot.spec(), TimerSpec::AfterBoot { ticks: 50 });
}
