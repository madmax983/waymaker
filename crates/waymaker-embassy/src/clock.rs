//! The persistent-clock capability, and the route to a persistent deadline that a clock
//! witnesses.
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
//! constructor of a [`PersistentTimer`]. So a firmware with no clock cannot write that
//! call. That is issue [#32](https://github.com/madmax983/waymaker/issues/32)'s
//! compile-time half, and a `compile_fail` doctest on `arm` is what states it.
//!
//! It is *this* type's only route, not the workspace's:
//! `Timer::arm(TimerSpec::AtPersistentTime { .. }, ClockCapability::Persistent, now)` is
//! public and takes no clock. There the firmware's declaration is its own word, and the
//! runtime half — [`ClockCapability::admits`](waymaker_core::timer::ClockCapability::admits),
//! refusing with a named error — is what holds a firmware that declares honestly. Obliging
//! the witness is rung 0.4's dispatcher.
//!
//! Nothing here downgrades. This module names exactly one spec, the persistent one, and
//! the `timer-capability` rule fails a build in which it names another — under any
//! spelling, an associated constant included. Reaching for the boot spec substitutes one
//! clock policy for the other, which is what §11 forbids.
//!
//! # What the clock type closes, and what it does not
//!
//! [`PersistentTimer`] carries the clock *type* that armed it, so a firmware with two
//! drivers cannot poll one timer with the other: two epochs are both `u64`, and comparing
//! across them fires a durable deadline early or late. What the type cannot tell apart is
//! two *instances* of one driver — two RTCs on one board, or one driver re-created with a
//! different epoch — which is the same limit `waymaker-flash` records for a `Geometry`.
//! Binding an instance needs a borrow the timer would have to hold across the wait, and a
//! deadline that borrows its clock cannot be recorded. See CLAUDE.md.

use core::marker::PhantomData;

use waymaker_core::KernelError;
use waymaker_core::timer::{ClockCapability, Deadline, Timer, TimerSpec};

/// A clock whose readings survive power loss.
///
/// Design document §11, verbatim. An implementation is a board driver: an RTC held up by a
/// battery or a supercapacitor, or an epoch a network restored into retained storage.
///
/// # Invariants a driver must uphold
///
/// * A reading is in the implementation's own unit, and the same unit across reboots.
///   [`Timer`] compares readings; it never converts them.
/// * Readings must not go backwards. If the hardware can move back — a battery change, an
///   epoch re-synchronisation — return an [`Err`]. [`PersistentTimer::poll`] refuses a
///   reading below the highest it has seen, so a driver that says nothing is still caught.
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

/// A deadline on a clock that survives power loss, tied to the clock that armed it.
///
/// # Invariants
///
/// * Its [`timer`](Self::timer) always holds a `TimerSpec::AtPersistentTime`. There is no
///   other constructor, and the one there is builds that spec itself.
/// * Building one needs a [`PersistentClock`]. The capability is the witness.
/// * `C` is the clock that armed it, and [`poll`](Self::poll) accepts no other type. A
///   reading is only meaningful against readings from the same source: two clocks with
///   different epochs or different tick units are both `u64`, and comparing across them
///   fires a durable deadline early or late. `C` makes that a compile error rather than a
///   timer that looks armed and is not.
///
/// # No derives
///
/// A `PhantomData<C>` would make every derived `impl` require the same bound of the clock
/// type, so a driver that is not `Clone` or not `Debug` would make the timer neither. The
/// type is a handle a caller holds, so it needs none of them; a firmware that wants one
/// writes it, and the `timer-capability` pin makes that a line somebody wrote on purpose.
pub struct PersistentTimer<C> {
    /// The armed deadline.
    timer: Timer,
    /// The highest reading this timer has been shown.
    ///
    /// [`Timer::armed_at`] alone is not enough. It is a floor at the *arming* reading, so a
    /// clock that moved back after a poll — but stayed above the arming reading — was
    /// believed, and a deadline that had already reported `Elapsed` reported `Remaining` on
    /// the next look. A timer that un-fires is worse than one that never fired.
    seen: u64,
    /// The clock that armed it, held as an identity and never as a value.
    ///
    /// `fn(&C)` rather than `C`: the timer owns no clock, so it must not inherit the
    /// clock's drop behaviour, and it stays [`Send`] and [`Sync`] whatever the driver is —
    /// which matters where a deadline is armed in a task and read in an interrupt.
    clock: PhantomData<fn(&C)>,
}

impl<C: PersistentClock> PersistentTimer<C> {
    /// Arms a deadline at persistent-clock reading `instant`.
    ///
    /// Reads `clock` once, for the arming reading that [`poll`](Self::poll) compares
    /// against.
    ///
    /// A caller with no clock cannot write this call:
    ///
    /// ```compile_fail,E0277
    /// use waymaker_embassy::clock::PersistentTimer;
    ///
    /// // `()` implements no `PersistentClock`, and there is no other constructor.
    /// let _ = PersistentTimer::arm(&mut (), 2_000);
    /// ```
    ///
    /// A caller with one can:
    ///
    /// ```
    /// use waymaker_embassy::clock::{PersistentClock, PersistentTimer};
    ///
    /// struct Rtc;
    ///
    /// impl PersistentClock for Rtc {
    ///     type Error = ();
    ///     fn now(&mut self) -> Result<u64, ()> { Ok(1_000) }
    /// }
    ///
    /// let _ = PersistentTimer::arm(&mut Rtc, 2_000);
    /// ```
    ///
    /// # Errors
    ///
    /// [`ClockError::Unavailable`] when the clock cannot be read. The kernel admits the
    /// spec by construction, because the clock in hand is the capability.
    pub fn arm(clock: &mut C, instant: u64) -> Result<Self, ClockError<C::Error>> {
        let now = clock.now().map_err(ClockError::Unavailable)?;
        Timer::arm(
            TimerSpec::AtPersistentTime { instant },
            ClockCapability::Persistent,
            now,
        )
        .map(|timer| Self {
            timer,
            seen: now,
            clock: PhantomData,
        })
        .map_err(ClockError::Refused)
    }

    /// The armed timer, for a caller that records it or reads its spec.
    #[must_use]
    pub const fn timer(&self) -> &Timer {
        &self.timer
    }

    /// Reads `clock` and says what the reading means for this deadline.
    ///
    /// `clock` is the same type that armed this timer. A second driver with its own epoch
    /// does not compile here, which is what stops one clock's reading being measured
    /// against another's arming reading.
    ///
    /// The wrong clock is rejected by the compiler:
    ///
    /// ```compile_fail,E0308
    /// use waymaker_embassy::clock::{PersistentClock, PersistentTimer};
    ///
    /// struct Rtc;
    /// struct NetworkEpoch;
    ///
    /// impl PersistentClock for Rtc {
    ///     type Error = ();
    ///     fn now(&mut self) -> Result<u64, ()> { Ok(1_000_000) }
    /// }
    /// impl PersistentClock for NetworkEpoch {
    ///     type Error = ();
    ///     fn now(&mut self) -> Result<u64, ()> { Ok(1_000) }
    /// }
    ///
    /// let mut rtc = Rtc;
    /// let mut epoch = NetworkEpoch;
    /// let mut armed = match PersistentTimer::arm(&mut epoch, 1_500) {
    ///     Ok(timer) => timer,
    ///     Err(_) => return,
    /// };
    /// // The RTC reads 1_000_000, which is past the instant 1_500 in a unit that is not
    /// // the epoch's. Without the clock type this compiled and answered `Elapsed`.
    /// let _ = armed.poll(&mut rtc);
    /// ```
    ///
    /// The same call with the clock that armed it compiles:
    ///
    /// ```
    /// use waymaker_embassy::clock::{PersistentClock, PersistentTimer};
    ///
    /// struct NetworkEpoch;
    ///
    /// impl PersistentClock for NetworkEpoch {
    ///     type Error = ();
    ///     fn now(&mut self) -> Result<u64, ()> { Ok(1_000) }
    /// }
    ///
    /// let mut epoch = NetworkEpoch;
    /// let mut armed = match PersistentTimer::arm(&mut epoch, 1_500) {
    ///     Ok(timer) => timer,
    ///     Err(_) => return,
    /// };
    /// let _ = armed.poll(&mut epoch);
    /// ```
    ///
    /// # Errors
    ///
    /// [`ClockError::Unavailable`] when the clock cannot be read, and
    /// [`ClockError::Refused`] when it read below the highest reading this timer has seen —
    /// which is not the same as below the arming reading, and is why
    /// [`KernelError::ClockWentBackwards`]'s message says "a reading already accepted". The
    /// high-water mark is what makes an elapsed deadline stay elapsed: `armed_at` alone
    /// believed any backwards move that stayed above it.
    pub fn poll(&mut self, clock: &mut C) -> Result<Deadline, ClockError<C::Error>> {
        let reading = clock.now().map_err(ClockError::Unavailable)?;
        if reading < self.seen {
            return Err(ClockError::Refused(KernelError::ClockWentBackwards));
        }
        self.seen = reading;
        self.timer.evaluate(reading).map_err(ClockError::Refused)
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
        let mut armed = PersistentTimer::arm(&mut clock, 200).expect("the clock answered");
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
        assert!(matches!(
            PersistentTimer::arm(&mut clock, 200),
            Err(ClockError::Unavailable(()))
        ));
    }

    #[test]
    fn a_backwards_reading_is_refused() {
        let mut clock = Fixed {
            readings: &[100, 99],
            taken: 0,
        };
        let mut armed = PersistentTimer::arm(&mut clock, 200).expect("the clock answered");
        assert_eq!(
            armed.poll(&mut clock),
            Err(ClockError::Refused(KernelError::ClockWentBackwards))
        );
    }

    #[test]
    fn an_elapsed_deadline_does_not_un_fire() {
        // The arming reading is not enough of a floor on its own: 250 is above it, so a
        // timer that only remembered `armed_at` reported `Remaining` again after reporting
        // `Elapsed`. A caller that polled twice got two different answers about one event.
        let mut clock = Fixed {
            readings: &[100, 300, 250],
            taken: 0,
        };
        let mut armed = PersistentTimer::arm(&mut clock, 200).expect("the clock answered");
        assert_eq!(armed.poll(&mut clock), Ok(Deadline::Elapsed));
        assert_eq!(
            armed.poll(&mut clock),
            Err(ClockError::Refused(KernelError::ClockWentBackwards)),
            "250 is above the arming reading and below what this timer has seen"
        );
    }
}
