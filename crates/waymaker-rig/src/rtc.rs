//! A persistent clock over a board RTC that a battery or a supercapacitor holds up.
//!
//! Design document §11 and issue [#34](https://github.com/madmax983/waymaker/issues/34). A
//! monotonic MCU timer returns to zero after a reset. An RTC in a backup domain does not,
//! so it can say how long the supply was away.
//!
//! # What this module owns
//!
//! [`BackedRtc`], the two registers a board brings; [`Continuity`], what the second one
//! says; [`Rtc`], the driver over them; and [`RtcFault`], how a read fails.
//!
//! # What this module must not own
//!
//! A deadline. A clock reports a reading. Which deadline that reading meets is
//! `waymaker-core`'s, and the `timer-capability` rule fails a build if this module names a
//! `TimerSpec` at all.
//!
//! # Why the driver is here
//!
//! Because it is board support, not engine. `waymaker-embassy` owns the *capability*, and
//! a layer pays for every public function it declares against design document §04's
//! code-flash budget. A concrete driver for hardware Waymaker does not ship belongs above
//! the layers, beside the rig that cuts the supply.
//!
//! # The absence this module defends
//!
//! A reading the driver cannot vouch for. A backup domain that lost power leaves the
//! counter at its reset value. On most parts that value is zero, which is below every
//! instant a workflow waits for — so a driver that reported the number would fire every
//! persistent deadline on the device at once. [`Rtc::now`] returns
//! [`RtcFault::ContinuityLost`] instead, and there is no second constructor and no accessor
//! that hands the raw counter out.
//!
//! # What this driver does not do, and where that is caught instead
//!
//! `PersistentClock` asks a driver whose hardware can move back to return an [`Err`]. This
//! one does not: it reports the counter, and a part with a 32-bit counter wraps. It has
//! nothing to detect the wrap with — a floor would have to survive the power cut, and RAM
//! does not, so the only floor it could keep is one from a power cycle that is gone.
//!
//! Two floors do survive, and both are outside this module. Within a boot it is
//! `PersistentTimer`'s high-water mark. Across a boot it is the arming reading issue
//! [#33](https://github.com/madmax983/waymaker/issues/33)'s `TimerScheduled` record carries,
//! which `Timer::evaluate` refuses a reading below. So a wrapped counter is
//! `KernelError::ClockWentBackwards` rather than a credited interval — a refusal, in the
//! layer that has the evidence for it. Whether a given part's counter can wrap inside a
//! given deadline is arithmetic about a board, and the board is what has to do it.

use waymaker_embassy::clock::PersistentClock;

/// Whether the backup domain held since the counter was last set.
///
/// Every part with an RTC has this bit. It is `OSF` on a DS3231, `INITS` and `RSF` on an
/// STM32 backup domain, and a "power fail" latch elsewhere. The name here is what it means
/// rather than what one vendor calls it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Continuity {
    /// The domain held. The counter has been running since it was set.
    Held,
    /// The domain broke. The counter says nothing about elapsed time.
    Lost,
}

/// The two registers a board RTC offers.
///
/// A board implements this and nothing else. Everything above it is code a host runs too,
/// which is what keeps the driver testable off the part.
///
/// # What an implementor must uphold
///
/// * The counter is in the part's own unit, and the same unit across power cycles.
///   `waymaker-core` compares readings; it never converts them.
/// * A register that cannot be read is an [`Err`]. An implementor must not substitute a
///   value.
/// * [`continuity`](Self::continuity) reports the hardware bit. It must not be inferred
///   from the counter: a counter that looks plausible after a battery change is the failure
///   this trait exists to report.
pub trait BackedRtc {
    /// How a failed register read reports. The board's own type; this crate never inspects
    /// it.
    type Error;

    /// The RTC counter, in the part's own unit.
    ///
    /// # Errors
    ///
    /// The board's error, when the register cannot be read.
    fn counter(&mut self) -> Result<u64, Self::Error>;

    /// Whether the backup domain held.
    ///
    /// # Errors
    ///
    /// The board's error, when the register cannot be read.
    fn continuity(&mut self) -> Result<Continuity, Self::Error>;
}

/// A persistent-clock read failed.
///
/// Two causes. A register that did not answer may answer later. A domain that broke has
/// already destroyed the interval the deadline rests on.
///
/// Generic over the board's error rather than flattened to a string: this crate has no
/// allocator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RtcFault<E> {
    /// A register could not be read.
    Register(E),
    /// The backup domain did not hold. The counter is not a reading.
    ContinuityLost,
}

/// A persistent clock over `R`.
///
/// # Invariants
///
/// * Every reading it returns comes from a domain that reported [`Continuity::Held`] on the
///   same call.
/// * It holds no state of its own. A power cut takes RAM, so a driver that cached a reading
///   would carry a number from a power cycle that is gone.
///
/// # No derives
///
/// The type is a handle a caller holds. A derive would put the same bound on `R`, so a
/// board register block that is not `Debug` would make the clock not `Debug` either.
pub struct Rtc<R> {
    /// The board's registers.
    registers: R,
}

impl<R> Rtc<R> {
    /// A clock over `registers`.
    ///
    /// The only constructor. There is no way to build one over a reading a caller chose.
    #[must_use]
    pub const fn over(registers: R) -> Self {
        Self { registers }
    }
}

impl<R: BackedRtc> PersistentClock for Rtc<R> {
    type Error = RtcFault<R::Error>;

    /// The counter, if the domain behind it held.
    ///
    /// The counter is read first and the continuity bit second. That order is the safe one:
    /// a supercapacitor that browns out *during* the counter read latches the bit, so a
    /// read that follows the counter catches a break the counter met and a read that
    /// precedes it cannot.
    ///
    /// # Errors
    ///
    /// [`RtcFault::Register`] when either read fails, and [`RtcFault::ContinuityLost`] when
    /// the domain broke.
    fn now(&mut self) -> Result<u64, Self::Error> {
        let counter = self.registers.counter().map_err(RtcFault::Register)?;
        match self.registers.continuity().map_err(RtcFault::Register)? {
            Continuity::Held => Ok(counter),
            Continuity::Lost => Err(RtcFault::ContinuityLost),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registers a test sets.
    struct Fixed {
        counter: u64,
        continuity: Continuity,
    }

    impl BackedRtc for Fixed {
        type Error = ();

        fn counter(&mut self) -> Result<u64, ()> {
            Ok(self.counter)
        }

        fn continuity(&mut self) -> Result<Continuity, ()> {
            Ok(self.continuity)
        }
    }

    #[test]
    fn a_held_domain_gives_its_counter() {
        let mut clock = Rtc::over(Fixed {
            counter: 1_700_000_000,
            continuity: Continuity::Held,
        });
        assert_eq!(clock.now(), Ok(1_700_000_000));
    }

    #[test]
    fn a_broken_domain_gives_a_fault() {
        // The zero is the point. It is a plausible counter and a lethal reading.
        let mut clock = Rtc::over(Fixed {
            counter: 0,
            continuity: Continuity::Lost,
        });
        assert_eq!(clock.now(), Err(RtcFault::ContinuityLost));
    }
}
