//! The persistent-clock capability, and the only route to a timer that needs one.
//!
//! Design document §11. A persistent deadline needs a clock that survives power loss: an
//! RTC, or an epoch a network restores. This module is where that hardware is named.
//!
//! # What this module owns
//!
//! [`PersistentClock`], the driver capability; [`PersistentTimer`], an armed persistent
//! deadline; and [`ClockError`], what a persistent timer can fail with.
//!
//! # Why the capability is here and not in the kernel
//!
//! `waymaker-core`'s must-not-own cell names a clock. [`PersistentClock::now`] reads
//! hardware, so it is a driver interface, and it sits one layer above the kernel for the
//! reason `StableStorage` does. `waymaker-flash` is not the home either: its own
//! must-not-own cell names timers.
//!
//! # The absence this module defends
//!
//! [`PersistentTimer::arm`] takes `&mut C` where `C: PersistentClock`, and it is the only
//! constructor. So a firmware with no clock cannot write the call at all. That is issue
//! [#32](https://github.com/madmax983/waymaker/issues/32)'s compile-time half; the runtime
//! half is [`ClockCapability::admits`](waymaker_core::timer::ClockCapability::admits),
//! which refuses with a named error.
//!
//! Nothing here downgrades. This module never names `TimerSpec::AfterBoot`, and the
//! `timer-capability` gate rule fails a build in which it starts to: a persistent-clock
//! module that reaches for the boot spec is either substituting one policy for the other
//! or fabricating a reading, and §11 forbids both.

use waymaker_core::KernelError;
use waymaker_core::timer::{ClockCapability, Deadline, Timer, TimerSpec};

/// A clock whose readings survive power loss.
///
/// Design document §11, verbatim. An implementation is a board driver: an RTC held up by a
/// battery or a supercapacitor, or an epoch a network restored into retained storage.
///
/// # Contract
///
/// * A reading is in the implementation's own unit, and the same unit across reboots.
///   [`Timer`] compares readings; it never converts them.
/// * Readings must not go backwards. Where the hardware can — a battery change, an epoch
///   re-synchronisation — the driver may report the fact, and
///   [`PersistentTimer::poll`] catches it either way.
/// * A read that cannot be trusted is an [`Err`]. A driver must not substitute a value:
///   a zero fires every persistent timer at once, and a maximum fires none of them.
pub trait PersistentClock {
    /// What a failed read reports. The driver's own type; this crate never inspects it.
    type Error;

    /// The current persistent reading.
    ///
    /// # Errors
    ///
    /// The driver's error, when the clock cannot be read.
    fn now(&mut self) -> Result<u64, Self::Error>;
}

/// A persistent timer failed.
///
/// Two causes, kept apart because a caller acts on them differently. A clock that cannot
/// be read may answer later. A clock that went backwards has already broken the assumption
/// the deadline rests on.
///
/// Generic over the driver's error rather than flattening it to a string: this crate has
/// no allocator, and a firmware with no console has nothing to print it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClockError<E> {
    /// The clock could not be read.
    Unavailable(E),
    /// The kernel refused the reading. Always
    /// [`waymaker_core::KernelError::ClockWentBackwards`] today; the variant carries the
    /// error so a second refusal needs no new shape here.
    Refused(KernelError),
}

/// A deadline on a clock that survives power loss.
///
/// # Invariants
///
/// * Its [`timer`](Self::timer) always holds a `TimerSpec::AtPersistentTime`. There is no
///   other constructor, and the one there is builds that spec itself.
/// * Building one needs a [`PersistentClock`]. The capability is the witness.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PersistentTimer(Timer);

impl PersistentTimer {
    /// Arms a deadline at persistent-clock reading `instant`.
    ///
    /// Reads `clock` once, for the arming reading that [`poll`](Self::poll) compares
    /// against.
    ///
    /// # Errors
    ///
    /// [`ClockError::Unavailable`] when the clock cannot be read. The kernel admits the
    /// spec by construction, because the clock in hand is the capability.
    pub fn arm<C: PersistentClock>(
        clock: &mut C,
        instant: u64,
    ) -> Result<Self, ClockError<C::Error>> {
        let now = clock.now().map_err(ClockError::Unavailable)?;
        Timer::arm(
            TimerSpec::AtPersistentTime { instant },
            ClockCapability::Persistent,
            now,
        )
        .map(Self)
        .map_err(ClockError::Refused)
    }

    /// The armed timer, for a caller that records it or reads its spec.
    #[must_use]
    pub const fn timer(&self) -> &Timer {
        &self.0
    }

    /// Reads `clock` and says what the reading means for this deadline.
    ///
    /// # Errors
    ///
    /// [`ClockError::Unavailable`] when the clock cannot be read, and
    /// [`ClockError::Refused`] when it read below the arming reading.
    pub fn poll<C: PersistentClock>(
        &self,
        clock: &mut C,
    ) -> Result<Deadline, ClockError<C::Error>> {
        let reading = clock.now().map_err(ClockError::Unavailable)?;
        self.0.evaluate(reading).map_err(ClockError::Refused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock that answers with a fixed sequence, and fails once it runs out.
    struct Fixed {
        readings: &'static [u64],
        taken: usize,
    }

    impl PersistentClock for Fixed {
        type Error = ();

        fn now(&mut self) -> Result<u64, ()> {
            let answer = self.readings.get(self.taken).copied().ok_or(());
            self.taken = self.taken.saturating_add(1);
            answer
        }
    }

    #[test]
    fn arming_records_the_reading_it_read() {
        let mut clock = Fixed {
            readings: &[100, 150],
            taken: 0,
        };
        let armed = PersistentTimer::arm(&mut clock, 200).expect("the clock answered");
        assert_eq!(armed.timer().armed_at(), 100);
        assert_eq!(
            armed.timer().spec(),
            TimerSpec::AtPersistentTime { instant: 200 }
        );
        assert_eq!(
            armed.poll(&mut clock),
            Ok(Deadline::Remaining { ticks: 50 })
        );
    }

    #[test]
    fn a_clock_that_cannot_be_read_is_not_a_reading() {
        let mut clock = Fixed {
            readings: &[],
            taken: 0,
        };
        assert_eq!(
            PersistentTimer::arm(&mut clock, 200),
            Err(ClockError::Unavailable(()))
        );
    }

    #[test]
    fn a_backwards_reading_is_refused() {
        let mut clock = Fixed {
            readings: &[100, 99],
            taken: 0,
        };
        let armed = PersistentTimer::arm(&mut clock, 200).expect("the clock answered");
        assert_eq!(
            armed.poll(&mut clock),
            Err(ClockError::Refused(KernelError::ClockWentBackwards))
        );
    }
}
