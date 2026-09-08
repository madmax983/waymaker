//! A persistent clock over an epoch a network restores, for a board with no RTC.
//!
//! Design document §11 and issue [#34](https://github.com/madmax983/waymaker/issues/34).
//! §11 names two sources for a durable reading. [`crate::rtc`] is the one in hardware. This
//! is the other: the device is told the time, and a monotonic clock carries that answer
//! forward until the next power cycle.
//!
//! # What this module owns
//!
//! [`Monotonic`], the boot clock a board brings; [`RestoredEpoch`], the driver; and
//! [`EpochFault`], how a reading fails.
//!
//! # What this module must not own
//!
//! A deadline, for [`crate::rtc`]'s reason, and the network. Fetching the time is the
//! firmware's; [`RestoredEpoch::restore`] takes the answer.
//!
//! # How this path differs from an RTC, and why that is the whole documentation
//!
//! An RTC counter is in a backup domain. It crosses a power cut. A restored epoch is in
//! RAM, and a power cut takes RAM. So:
//!
//! * After a cut, a device on this path knows nothing. [`RestoredEpoch::now`] returns
//!   [`EpochFault::NotRestored`] until the network answers again.
//! * A driver reports that refusal, and the run suspends. It does not fire the deadline and
//!   it does not discard it. The record on media still holds the instant, so the boot that
//!   is told the time judges the same deadline.
//! * A device that never reaches the network never fires an `AtPersistentTime` deadline.
//!   That is the honest outcome. §02 decision 8 is that timer semantics match the
//!   hardware's clock, and a device with no clock and no network has no time.
//!
//! # The absence this module defends
//!
//! A reading invented from a zero. An unrestored epoch is a fault, never `0` and never the
//! boot clock's own reading. Both would be a persistent deadline served by a clock that
//! restarts on every reset, which is the downgrade §11 forbids.

use waymaker_embassy::clock::PersistentClock;

/// A monotonic clock that runs while the device has power.
///
/// It restarts at zero after a reset. That is what makes it a boot clock and not a
/// persistent one, and this module never treats it as more than a way to advance an answer
/// somebody else supplied.
///
/// # What an implementor must uphold
///
/// * Readings do not go backwards within one power cycle.
/// * A read that fails is an [`Err`]. An implementor must not substitute a value.
pub trait Monotonic {
    /// How a failed read reports. The board's own type; this crate never inspects it.
    type Error;

    /// Ticks since this power cycle began.
    ///
    /// # Errors
    ///
    /// The board's error, when the clock cannot be read.
    fn ticks(&mut self) -> Result<u64, Self::Error>;
}

/// A reading from a restored epoch failed.
///
/// Four causes, kept apart because a caller acts on them differently. A device that has not
/// been told the time can ask the network. A device whose monotonic clock regressed has
/// lost the anchor and must be told again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EpochFault<E> {
    /// The monotonic clock could not be read.
    Monotonic(E),
    /// No epoch has been restored into this power cycle.
    NotRestored,
    /// The monotonic clock read below the reading the epoch was anchored to, or a re-sync
    /// would move the clock backwards.
    Regressed,
    /// The epoch plus the ticks since it was restored does not fit a reading.
    Unrepresentable,
}

/// Where a restored epoch was anchored.
///
/// Two numbers taken in the same call: the time the network gave, and the monotonic reading
/// at that moment. Every later reading is one plus the difference of the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Anchor {
    /// The reading the network supplied.
    epoch: u64,
    /// The monotonic reading it was supplied at.
    ticks: u64,
}

/// A persistent clock over an epoch the firmware restores.
///
/// # Invariants
///
/// * A reading is `epoch + (ticks now - ticks at the restore)`. Neither term is guessed and
///   neither operation wraps: an underflow is [`EpochFault::Regressed`] and an overflow is
///   [`EpochFault::Unrepresentable`].
/// * Before the first [`restore`](Self::restore), there is no reading at all.
/// * [`restore`](Self::restore) never moves the answer backwards.
///
/// # No derives
///
/// For [`crate::rtc::Rtc`]'s reason: a derive would put the same bound on `M`.
pub struct RestoredEpoch<M> {
    /// The board's boot clock.
    monotonic: M,
    /// Where the epoch was anchored, once it has been.
    anchor: Option<Anchor>,
}

impl<M> RestoredEpoch<M> {
    /// A clock over `monotonic` with no epoch in it.
    ///
    /// The only constructor, and it names the state a device is in after a power cut: it
    /// has a boot clock and it does not know the time.
    #[must_use]
    pub const fn awaiting(monotonic: M) -> Self {
        Self {
            monotonic,
            anchor: None,
        }
    }
}

impl<M: Monotonic> RestoredEpoch<M> {
    /// Anchors this clock at `reading`.
    ///
    /// The firmware calls this with what the network said. A later call re-synchronises,
    /// and a re-synchronisation that would move the clock backwards is refused rather than
    /// applied: §11 requires a persistent clock's readings not to go backwards, and a
    /// per-timer high-water mark catches such a move for one deadline and misses it for the
    /// next.
    ///
    /// # Errors
    ///
    /// [`EpochFault::Monotonic`] when the boot clock cannot be read,
    /// [`EpochFault::Regressed`] when the boot clock read below the current anchor or when
    /// `reading` is behind what this clock already reports, and
    /// [`EpochFault::Unrepresentable`] when the current reading does not fit.
    pub fn restore(&mut self, reading: u64) -> Result<(), EpochFault<M::Error>> {
        let ticks = self.monotonic.ticks().map_err(EpochFault::Monotonic)?;
        if self.anchor.is_some() && reading < self.reading_at(ticks)? {
            return Err(EpochFault::Regressed);
        }
        self.anchor = Some(Anchor {
            epoch: reading,
            ticks,
        });
        Ok(())
    }

    /// What this clock reads when the boot clock reads `ticks`.
    ///
    /// # Errors
    ///
    /// [`EpochFault::NotRestored`], [`EpochFault::Regressed`] or
    /// [`EpochFault::Unrepresentable`], as [`restore`](Self::restore) describes.
    fn reading_at(&self, ticks: u64) -> Result<u64, EpochFault<M::Error>> {
        let anchor = self.anchor.ok_or(EpochFault::NotRestored)?;
        let elapsed = ticks
            .checked_sub(anchor.ticks)
            .ok_or(EpochFault::Regressed)?;
        anchor
            .epoch
            .checked_add(elapsed)
            .ok_or(EpochFault::Unrepresentable)
    }
}

impl<M: Monotonic> PersistentClock for RestoredEpoch<M> {
    type Error = EpochFault<M::Error>;

    /// The restored epoch, advanced by the boot clock.
    ///
    /// # Errors
    ///
    /// [`EpochFault::NotRestored`] before the first [`restore`](Self::restore) of this power
    /// cycle, and the three faults [`restore`](Self::restore) describes.
    fn now(&mut self) -> Result<u64, Self::Error> {
        let ticks = self.monotonic.ticks().map_err(EpochFault::Monotonic)?;
        self.reading_at(ticks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boot clock that answers one number.
    struct Fixed(u64);

    impl Monotonic for Fixed {
        type Error = ();

        fn ticks(&mut self) -> Result<u64, ()> {
            Ok(self.0)
        }
    }

    #[test]
    fn an_unrestored_epoch_is_a_fault() {
        let mut clock = RestoredEpoch::awaiting(Fixed(0));
        assert_eq!(clock.now(), Err(EpochFault::NotRestored));
    }

    #[test]
    fn a_restored_epoch_advances_with_the_boot_clock() {
        let mut clock = RestoredEpoch::awaiting(Fixed(10));
        assert_eq!(clock.restore(1_000), Ok(()));
        clock.monotonic = Fixed(60);
        assert_eq!(clock.now(), Ok(1_050));
    }
}
