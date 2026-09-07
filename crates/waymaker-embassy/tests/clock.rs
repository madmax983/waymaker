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
    let armed = PersistentTimer::arm(&mut rtc, 2_000).expect("the clock answered");

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
    let armed = PersistentTimer::arm(&mut rtc, 2_000).expect("the clock answered");

    assert_eq!(armed.poll(&mut rtc), Ok(Deadline::Elapsed));
}

#[test]
fn a_clock_that_fails_is_a_failure_rather_than_a_reading() {
    // Issue #32's third work item, first half. There is no default reading: a zero here
    // would fire every persistent timer at once, and a `u64::MAX` would fire none of them.
    let mut arming = Rtc::failing();
    assert_eq!(
        PersistentTimer::arm(&mut arming, 2_000),
        Err(ClockError::Unavailable(Fault))
    );

    let mut polling = Rtc::reading(&[1_000]);
    let armed = PersistentTimer::arm(&mut polling, 2_000).expect("the clock answered once");
    let mut broken = Rtc::failing();
    assert_eq!(armed.poll(&mut broken), Err(ClockError::Unavailable(Fault)));
}

#[test]
fn a_clock_that_goes_backwards_is_refused_rather_than_believed() {
    // Issue #32's third work item, second half. A battery swap or an epoch re-sync can
    // move an RTC back; believing it would un-fire a timer that had already elapsed.
    let mut rtc = Rtc::reading(&[1_000, 999]);
    let armed = PersistentTimer::arm(&mut rtc, 2_000).expect("the clock answered");

    assert_eq!(
        armed.poll(&mut rtc),
        Err(ClockError::Refused(KernelError::ClockWentBackwards))
    );
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
