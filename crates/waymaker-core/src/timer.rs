//! Timer semantics: what a deadline means, and which clock can measure it.
//!
//! Design document §11. A monotonic MCU timer returns to zero after a reset. It cannot
//! say how long the device had no power. §02 decision 8 makes that a rule: timer semantics
//! match the hardware's clock.
//!
//! # What this module owns
//!
//! [`TimerSpec`], the two deadlines §11 offers; [`ClockKind`], the number a
//! `TimerScheduled` record will carry; [`ClockCapability`], what a firmware declares it
//! can service; [`Timer`], an armed deadline; and [`Deadline`], what a clock reading says
//! about one.
//!
//! # What this module must not own
//!
//! A clock. The kernel's must-not-own cell names one, so nothing here reads time. Every
//! function takes a reading as an argument. The `PersistentClock` capability is
//! `waymaker-embassy`'s, for the reason `StableStorage` is `waymaker-flash`'s: a trait
//! whose one method reads hardware is a driver interface.
//!
//! # The absence this module defends
//!
//! A persistent deadline is never downgraded to a boot deadline. A [`Timer`] holds the
//! [`TimerSpec`] it was armed from and no other, so [`Timer::arm`] gives a refusal or a
//! timer for that spec. [`ClockCapability::admits`] names every pair and uses no wildcard,
//! so a spec added later must be decided there rather than admitted by default. The
//! `timer-capability` rule pins the rest.

use crate::error::KernelError;

/// Which clock a timer is stated against, as the byte on media will say it.
///
/// `u8` and a newtype for [`RecordKind`](crate::RecordKind)'s reasons: the number is the
/// wire format, so an encoder reaches the integer directly.
///
/// §11: "A persistent timer record includes its clock kind so recovery cannot silently
/// reinterpret one policy as another." The numbers are spent here so that issue
/// [#33](https://github.com/madmax983/waymaker/issues/33) writes a record body rather than
/// a renumbering. Zero is not a kind: an erased or zeroed field must not decode as a
/// policy.
///
/// Not [`Ord`]: the numbers are positions in a table, so one kind is not less than
/// another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ClockKind(pub u8);

impl ClockKind {
    /// The boot clock. It returns to zero after a reset.
    pub const AFTER_BOOT: Self = Self(1);
    /// A clock that survives power loss: an RTC, or an externally restored epoch.
    pub const AT_PERSISTENT_TIME: Self = Self(2);
}

/// What a workflow asks a timer to wait for.
///
/// The two variants are §11's, field for field. The difference between them is a hardware
/// fact, not a preference, which is why they are two variants and not one with a flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimerSpec {
    /// Wait `ticks` of this boot's monotonic clock.
    ///
    /// **This is not a power-loss-durable delay.** The boot clock restarts at zero after
    /// a reset, and the armed timer lives in RAM, so a reboot starts the whole interval
    /// again. A device that resets every `ticks / 2` never reaches this deadline. Use
    /// [`AtPersistentTime`](Self::AtPersistentTime) when the delay must survive power
    /// loss.
    AfterBoot {
        /// How many ticks of this boot to wait.
        ticks: u64,
    },
    /// Wait until a persistent clock reads `instant`.
    ///
    /// Survives power loss, and only where a persistent clock exists. A firmware that
    /// declares [`ClockCapability::BootOnly`] is refused rather than downgraded.
    AtPersistentTime {
        /// The persistent-clock reading this timer waits for.
        instant: u64,
    },
}

impl TimerSpec {
    /// The clock this spec is stated against.
    ///
    /// # Postconditions
    ///
    /// Total, `const`, and one of [`ClockKind`]'s two constants. The record encoder writes
    /// what this returns, so the mapping lives once.
    #[must_use]
    pub const fn clock_kind(self) -> ClockKind {
        match self {
            Self::AfterBoot { .. } => ClockKind::AFTER_BOOT,
            Self::AtPersistentTime { .. } => ClockKind::AT_PERSISTENT_TIME,
        }
    }

    /// The deadline this spec names, in its own clock's unit.
    ///
    /// The other half of what issue
    /// [#33](https://github.com/madmax983/waymaker/issues/33)'s `TimerScheduled` record
    /// stores. A number alone means nothing: it is ticks of this boot for
    /// [`AfterBoot`](Self::AfterBoot) and a reading of the persistent clock for
    /// [`AtPersistentTime`](Self::AtPersistentTime), and
    /// [`clock_kind`](Self::clock_kind) is what says which.
    ///
    /// # Postconditions
    ///
    /// Total, `const`, and the inverse of [`recorded`](Self::recorded) at this spec's own
    /// kind.
    #[must_use]
    pub const fn deadline(self) -> u64 {
        match self {
            Self::AfterBoot { ticks } => ticks,
            Self::AtPersistentTime { instant } => instant,
        }
    }

    /// The reading a recorded deadline is re-armed at, given what its clock says now.
    ///
    /// Replay's other half of issue
    /// [#33](https://github.com/madmax983/waymaker/issues/33)'s record. The arming reading
    /// crosses a reset on media, and what it *means* on the other side depends on the clock:
    ///
    /// * [`AtPersistentTime`](Self::AtPersistentTime) → `recorded`. The clock survives power
    ///   loss, so the floor does too, and a reading below it is a clock that moved backwards
    ///   — which [`Timer::evaluate`] refuses rather than credits.
    /// * [`AfterBoot`](Self::AfterBoot) → the lower of the two. A boot clock reads below its
    ///   own arming reading only after a reset, and §11 says a boot interval restarts after
    ///   one. Taking `recorded` unconditionally makes every reset a permanent
    ///   [`ClockWentBackwards`](KernelError::ClockWentBackwards) on a run that has no way to
    ///   end, because §08 has no edge from an open boundary to a terminal record.
    ///
    /// # Postconditions
    ///
    /// Total, `const`, and never above `now` or above `recorded`. It reads no clock: both
    /// readings are arguments, as everywhere else in this module.
    ///
    /// What it cannot do is tell a reset from an in-boot re-drive when the boot clock has
    /// already run past the recorded reading. There it measures from `recorded` and credits
    /// the previous boot's uptime. A boot clock offers no reset evidence, which is §11's own
    /// reason for calling this deadline not power-loss durable; a reset-cause register is a
    /// board's, and issue [#34](https://github.com/madmax983/waymaker/issues/34) is where a
    /// real one is met.
    #[must_use]
    pub const fn rearmed_at(self, recorded: u64, now: u64) -> u64 {
        match self {
            Self::AfterBoot { .. } => {
                if now < recorded {
                    now
                } else {
                    recorded
                }
            }
            Self::AtPersistentTime { .. } => recorded,
        }
    }

    /// The spec a recorded `clock_kind` and `deadline` name, or [`None`].
    ///
    /// Replay's half of the record. `waymaker-flash` decodes the two fields and this turns
    /// them back into a policy.
    ///
    /// # Postconditions
    ///
    /// Total and `const`. [`None`] for a kind number this firmware does not know — an
    /// erased byte, a zeroed one, or a policy a later format adds. The refusal is what
    /// stops §11's reinterpretation: a wildcard arm here would read an unknown byte as one
    /// of the two policies, so a zeroed page would decode as a timer.
    ///
    /// It is not a downgrade route. The caller supplies the kind, and the one caller that
    /// must never choose it is `waymaker-embassy`'s clock module, where the
    /// `timer-capability` rule already refuses every `TimerSpec` name but the persistent
    /// one.
    #[must_use]
    pub const fn recorded(clock_kind: ClockKind, deadline: u64) -> Option<Self> {
        match clock_kind.0 {
            n if n == ClockKind::AFTER_BOOT.0 => Some(Self::AfterBoot { ticks: deadline }),
            n if n == ClockKind::AT_PERSISTENT_TIME.0 => {
                Some(Self::AtPersistentTime { instant: deadline })
            }
            _ => None,
        }
    }
}

/// Which clocks this firmware can service.
///
/// The firmware declares this. `waymaker-embassy` is where a declaration of
/// [`Persistent`](Self::Persistent) is witnessed by a clock the caller holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ClockCapability {
    /// A monotonic boot clock, and nothing that survives power loss.
    BootOnly,
    /// A boot clock and a persistent clock.
    Persistent,
}

impl ClockCapability {
    /// Whether this firmware can honour `spec`.
    ///
    /// # Errors
    ///
    /// [`KernelError::NoPersistentClock`] when `spec` needs a persistent clock and this
    /// firmware has none. The refusal names the missing capability, which is what issue
    /// [#34](https://github.com/madmax983/waymaker/issues/34) asks of it.
    ///
    /// # Postconditions
    ///
    /// Total and `const`. It never rewrites `spec`: the only two answers are "yes" and a
    /// named refusal.
    ///
    /// Every pair is named. The `Ok` cases share one body and so share one arm, but by
    /// or-pattern and never by `_`, which is the whole point: a wildcard would *admit* a
    /// spec added later. That is the silent downgrade this module exists to forbid, and it
    /// would arrive in the one place that decides policy while
    /// [`clock_kind`](TimerSpec::clock_kind) and [`Timer::evaluate`] failed to compile and
    /// named the two places that do not. The kernel's error vocabulary refuses
    /// `#[non_exhaustive]` for the same reason.
    pub const fn admits(self, spec: TimerSpec) -> Result<(), KernelError> {
        match (self, spec) {
            (Self::BootOnly, TimerSpec::AtPersistentTime { .. }) => {
                Err(KernelError::NoPersistentClock)
            }
            (Self::BootOnly | Self::Persistent, TimerSpec::AfterBoot { .. })
            | (Self::Persistent, TimerSpec::AtPersistentTime { .. }) => Ok(()),
        }
    }
}

/// An armed deadline: the spec it came from, and the reading it was armed at.
///
/// # Invariants
///
/// * [`spec`](Self::spec) is the spec [`arm`](Self::arm) was given. A timer cannot carry
///   another one, because it stores no other description of its deadline.
/// * [`armed_at`](Self::armed_at) is a floor on the clock. A later reading below it is a
///   clock that went backwards, which [`evaluate`](Self::evaluate) refuses.
///
/// # Live state
///
/// A pending timer is live for as long as the run waits on it, so it is registered in
/// `kernel_state_types!` and charged against §04's 128 B kernel-state budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Timer {
    /// What the workflow asked for.
    spec: TimerSpec,
    /// The clock reading this timer was armed at.
    armed_at: u64,
}

impl Timer {
    /// Arms `spec` at reading `now`, if `capability` can service it.
    ///
    /// `now` is a reading of the spec's own clock: the boot clock for
    /// [`TimerSpec::AfterBoot`], the persistent clock for
    /// [`TimerSpec::AtPersistentTime`]. The kernel reads neither.
    ///
    /// # Errors
    ///
    /// [`KernelError::NoPersistentClock`], from [`ClockCapability::admits`].
    ///
    /// # Postconditions
    ///
    /// Either a refusal, or a timer whose [`spec`](Self::spec) is `spec` and whose
    /// [`armed_at`](Self::armed_at) is `now`. There is no third answer.
    pub const fn arm(
        spec: TimerSpec,
        capability: ClockCapability,
        now: u64,
    ) -> Result<Self, KernelError> {
        match capability.admits(spec) {
            Err(error) => Err(error),
            Ok(()) => Ok(Self {
                spec,
                armed_at: now,
            }),
        }
    }

    /// What the workflow asked for.
    #[must_use]
    pub const fn spec(&self) -> TimerSpec {
        self.spec
    }

    /// The clock reading this timer was armed at.
    ///
    /// Issue #33's `TimerScheduled` record carries it, so the floor below survives a
    /// reboot.
    #[must_use]
    pub const fn armed_at(&self) -> u64 {
        self.armed_at
    }

    /// What `reading` says about this deadline.
    ///
    /// # Errors
    ///
    /// [`KernelError::ClockWentBackwards`] when `reading` is below
    /// [`armed_at`](Self::armed_at). An RTC moves back when its battery is changed or its
    /// epoch is re-synchronised, and a boot clock moves back when the device resets. Both
    /// make elapsed time unknowable, so the kernel refuses instead of guessing.
    ///
    /// # Postconditions
    ///
    /// Total and `const`. It adds nothing: it compares an interval against a difference,
    /// and takes the difference only after the guard above. No reading can wrap.
    pub const fn evaluate(&self, reading: u64) -> Result<Deadline, KernelError> {
        if reading < self.armed_at {
            return Err(KernelError::ClockWentBackwards);
        }
        match self.spec {
            TimerSpec::AfterBoot { ticks } => {
                let elapsed = reading - self.armed_at;
                if elapsed >= ticks {
                    Ok(Deadline::Elapsed)
                } else {
                    Ok(Deadline::Remaining {
                        ticks: ticks - elapsed,
                    })
                }
            }
            TimerSpec::AtPersistentTime { instant } => {
                if reading >= instant {
                    Ok(Deadline::Elapsed)
                } else {
                    Ok(Deadline::Remaining {
                        ticks: instant - reading,
                    })
                }
            }
        }
    }
}

/// What one clock reading says about an armed timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Deadline {
    /// The deadline has passed. The workflow may proceed.
    Elapsed,
    /// The deadline is in the future.
    Remaining {
        /// How many ticks of the timer's own clock are still owed.
        ticks: u64,
    },
}

// The kind travels in a record, so its width is pinned the way `RecordKind`'s is.
const _: () = assert!(core::mem::size_of::<ClockKind>() == 1);
const _: () = assert!(core::mem::align_of::<ClockKind>() == 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_boot_timer_owes_its_whole_interval_on_a_new_boot() {
        let first = Timer::arm(
            TimerSpec::AfterBoot { ticks: 40 },
            ClockCapability::BootOnly,
            900,
        );
        assert_eq!(
            first.and_then(|timer| timer.evaluate(930)),
            Ok(Deadline::Remaining { ticks: 10 })
        );

        // The reset takes the reading and the armed timer. The same spec, armed again.
        let second = Timer::arm(
            TimerSpec::AfterBoot { ticks: 40 },
            ClockCapability::BootOnly,
            0,
        );
        assert_eq!(
            second.and_then(|timer| timer.evaluate(30)),
            Ok(Deadline::Remaining { ticks: 10 })
        );
    }

    #[test]
    fn a_persistent_instant_already_past_is_elapsed() {
        let armed = Timer::arm(
            TimerSpec::AtPersistentTime { instant: 100 },
            ClockCapability::Persistent,
            5_000,
        );
        assert_eq!(
            armed.and_then(|timer| timer.evaluate(5_000)),
            Ok(Deadline::Elapsed)
        );
    }

    #[test]
    fn a_boot_only_firmware_refuses_a_persistent_spec() {
        assert_eq!(
            Timer::arm(
                TimerSpec::AtPersistentTime { instant: 1 },
                ClockCapability::BootOnly,
                0
            ),
            Err(KernelError::NoPersistentClock)
        );
    }

    #[test]
    fn a_reading_below_the_arming_reading_is_refused() {
        let armed = Timer::arm(
            TimerSpec::AfterBoot { ticks: 1 },
            ClockCapability::BootOnly,
            10,
        )
        .expect("a boot-only firmware admits an after-boot spec");
        assert_eq!(armed.evaluate(9), Err(KernelError::ClockWentBackwards));
    }

    #[test]
    fn each_spec_keeps_its_own_clock_kind() {
        // Two specs, two kinds, and no arm returning the other's: a `match` that reported
        // one kind for both would show up as a duplicate here.
        assert_ne!(
            TimerSpec::AfterBoot { ticks: 1 }.clock_kind(),
            TimerSpec::AtPersistentTime { instant: 1 }.clock_kind()
        );
    }
}
