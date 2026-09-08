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
/// * **A tick is one unit of the epoch the firmware restores.** This module adds the two
///   numbers and never converts them, for [`Timer`]'s reason: a conversion needs a rate, and
///   a rate nobody checked is a clock that runs fast. A board whose epoch is seconds and
///   whose timer counts 32 kHz must divide before it answers here. Nothing below can catch
///   this — every arithmetic guard in this module passes on a reading 32768 times too large.
/// * Readings do not go backwards within one power cycle.
/// * A read that fails is an [`Err`]. An implementor must not substitute a value.
///
/// [`Timer`]: waymaker_core::timer::Timer
pub trait Monotonic {
    /// How a failed read reports. The board's own type; this crate never inspects it.
    type Error;

    /// Ticks since this power cycle began, in the unit the restored epoch counts in.
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
    /// A reading would be below one this clock has already given, or below the reading it
    /// stands at now. Either way it is a clock going backwards, which this driver refuses.
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
/// * **No reading is below one already given.** That is what the `floor` field is for, and
///   the anchor alone does not supply it — see below.
///
/// # Why the anchor is not enough
///
/// Review of this change found the version without the floor. An anchor detects a boot clock
/// that regressed *below the anchor tick*, and stops detecting it the moment the clock climbs
/// back. Anchored at `(epoch 1000, ticks 500)`: a read at tick 900 answers 1400, a reset to
/// tick 10 answers [`EpochFault::Regressed`], and a read at tick 600 then answers **1100** —
/// below a reading already given, and `Ok`. A deadline is not refused there; it fires late,
/// because [`Timer::evaluate`] floors at the reading the record was armed at and 1100 clears
/// it.
///
/// So the floor is the highest reading this clock has produced or been anchored to, and it
/// stands whether or not the anchor still evaluates. That second half is what stops the floor
/// becoming a lock: a boot clock that regressed makes the current reading unknowable, which is
/// the one state [`restore`](Self::restore) exists for, so it must not be the state that
/// refuses every re-sync.
///
/// # No derives
///
/// For [`crate::rtc::Rtc`]'s reason: a derive would put the same bound on `M`.
///
/// [`Timer::evaluate`]: waymaker_core::timer::Timer::evaluate
pub struct RestoredEpoch<M> {
    /// The board's boot clock.
    monotonic: M,
    /// Where the epoch was anchored, once it has been.
    anchor: Option<Anchor>,
    /// The highest reading this clock has produced or been anchored to.
    floor: u64,
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
            floor: 0,
        }
    }
}

impl<M: Monotonic> RestoredEpoch<M> {
    /// Anchors this clock at `reading`.
    ///
    /// The firmware calls this with what the network said. A later call re-synchronises, and
    /// a re-synchronisation that would move the clock backwards is refused rather than
    /// applied. `PersistentClock` requires a driver whose hardware can move back to return an
    /// [`Err`], and a per-timer high-water mark is not that: it catches such a move for one
    /// deadline and misses it for the next.
    ///
    /// What it refuses `reading` against is the higher of the reading this clock stands at
    /// and the highest it has already given. Where the anchor no longer evaluates — a boot
    /// clock that reset, an epoch that overflowed — only the second is left, and it is used
    /// rather than the failure being propagated. Propagating it locks a device out of the
    /// re-sync that is the whole remedy for the state it is in.
    ///
    /// # Errors
    ///
    /// [`EpochFault::Monotonic`] when the boot clock cannot be read, and
    /// [`EpochFault::Regressed`] when `reading` is behind what this clock reads or has read.
    pub fn restore(&mut self, reading: u64) -> Result<(), EpochFault<M::Error>> {
        let ticks = self.monotonic.ticks().map_err(EpochFault::Monotonic)?;
        let floor = self.reading_at(ticks).map_or(self.floor, |standing| {
            if standing > self.floor {
                standing
            } else {
                self.floor
            }
        });
        if reading < floor {
            return Err(EpochFault::Regressed);
        }
        self.anchor = Some(Anchor {
            epoch: reading,
            ticks,
        });
        self.floor = reading;
        Ok(())
    }

    /// What this clock reads when the boot clock reads `ticks`.
    ///
    /// The anchor's arithmetic and nothing else: the floor is applied by the two callers,
    /// each in its own way, which is why it is not applied here.
    ///
    /// # Errors
    ///
    /// [`EpochFault::NotRestored`] with no anchor, [`EpochFault::Regressed`] when `ticks` is
    /// below the anchor's, and [`EpochFault::Unrepresentable`] when the sum does not fit.
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
    /// cycle, [`EpochFault::Monotonic`], [`EpochFault::Unrepresentable`], and
    /// [`EpochFault::Regressed`] for a reading below one this clock has already given.
    fn now(&mut self) -> Result<u64, Self::Error> {
        let ticks = self.monotonic.ticks().map_err(EpochFault::Monotonic)?;
        let reading = self.reading_at(ticks)?;
        if reading < self.floor {
            return Err(EpochFault::Regressed);
        }
        self.floor = reading;
        Ok(reading)
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

    #[test]
    fn a_clock_that_climbed_back_past_its_anchor_does_not_resume_below_a_reading_it_gave() {
        // The failure the floor exists for, in the order it happens on a part.
        let mut clock = RestoredEpoch::awaiting(Fixed(500));
        assert_eq!(clock.restore(1_000), Ok(()));
        clock.monotonic = Fixed(900);
        assert_eq!(clock.now(), Ok(1_400));

        clock.monotonic = Fixed(10);
        assert_eq!(clock.now(), Err(EpochFault::Regressed));

        // Back above the anchor tick, so the anchor's own arithmetic works again and answers
        // 1100. Without the floor that is `Ok`, and a deadline armed below it fires late.
        clock.monotonic = Fixed(600);
        assert_eq!(clock.now(), Err(EpochFault::Regressed));
    }
}
