//! Timer semantics, tested through the surface an adapter sees.
//!
//! Design document §11: a monotonic MCU timer returns to zero after reset and cannot say
//! how long the device was unpowered. §02 decision 8 makes that a rule — timer semantics
//! match the hardware's actual clock, and never pretend. So the tests here are about what
//! the kernel *refuses* as much as about what it computes.
//!
//! Issue [#32](https://github.com/madmax983/waymaker/issues/32) states two of them
//! directly: an `AfterBoot` timer restarts its interval after a reset, and an
//! `AtPersistentTime` timer whose restored epoch is already past its instant is elapsed
//! at once.

use waymaker_core::KernelError;
use waymaker_core::timer::{ClockCapability, ClockKind, Deadline, Timer, TimerSpec};

/// Both specs, so a test that must hold for every one of them can say so.
const EVERY_SPEC: [TimerSpec; 2] = [
    TimerSpec::AfterBoot { ticks: 50 },
    TimerSpec::AtPersistentTime { instant: 2_000 },
];

/// The position of each spec in [`EVERY_SPEC`], by exhaustive `match`.
///
/// Without this the no-downgrade property below is stated over a hand-written array: a
/// third spec would be added, the array would stay at two, and the test that exists to
/// cover *every* spec would quietly cover two of three while still passing.
const fn position_of(spec: TimerSpec) -> usize {
    match spec {
        TimerSpec::AfterBoot { .. } => 0,
        TimerSpec::AtPersistentTime { .. } => 1,
    }
}

/// The same, for the capabilities.
const fn capability_position_of(capability: ClockCapability) -> usize {
    match capability {
        ClockCapability::BootOnly => 0,
        ClockCapability::Persistent => 1,
    }
}

#[test]
fn the_property_below_is_stated_over_every_spec_and_every_capability() {
    assert!(EVERY_SPEC.iter().map(|spec| position_of(*spec)).eq(0..2));
    assert!(
        EVERY_CAPABILITY
            .iter()
            .map(|capability| capability_position_of(*capability))
            .eq(0..2)
    );
}

/// Both capabilities, for the same reason.
const EVERY_CAPABILITY: [ClockCapability; 2] =
    [ClockCapability::BootOnly, ClockCapability::Persistent];

#[test]
fn an_after_boot_timer_restarts_its_interval_after_a_reset() {
    // Issue #32's first "done when". The boot clock counts from this boot's start, so a
    // reset takes both the reading and the armed timer with it.
    let before = Timer::arm(
        TimerSpec::AfterBoot { ticks: 50 },
        ClockCapability::BootOnly,
        1_000,
    )
    .expect("a boot-only firmware arms an after-boot timer");
    assert_eq!(
        before.evaluate(1_040),
        Ok(Deadline::Remaining { ticks: 10 }),
        "40 of 50 ticks elapsed before the reset"
    );

    // The reset. The boot clock returns to zero and the armed timer, which lived in RAM,
    // is gone; the workflow re-arms from the same spec on the new boot.
    let after = Timer::arm(
        TimerSpec::AfterBoot { ticks: 50 },
        ClockCapability::BootOnly,
        0,
    )
    .expect("the same spec arms again on the next boot");

    // The whole interval starts again. The device has now been powered for 1_040 ticks
    // in total across the two boots, which is more than twice the interval, and the timer
    // is still not elapsed: that is the honest answer §11 asks for.
    assert_eq!(
        after.evaluate(40),
        Ok(Deadline::Remaining { ticks: 10 }),
        "the interval starts again from the new boot rather than crediting the old one"
    );
    assert_eq!(after.evaluate(49), Ok(Deadline::Remaining { ticks: 1 }));
    assert_eq!(after.evaluate(50), Ok(Deadline::Elapsed));
}

#[test]
fn a_timer_armed_before_a_reset_refuses_a_reading_from_after_it() {
    // The other half of the same fact, and the one that would fail silently. If the armed
    // timer somehow survived the reset — kept in retained RAM, or reconstructed by a
    // reader that took the deadline and forgot the arming point — the new boot's reading
    // is below the arming reading, and crediting it would report an interval that never
    // ran. The reading is refused instead.
    let before = Timer::arm(
        TimerSpec::AfterBoot { ticks: 50 },
        ClockCapability::BootOnly,
        1_000,
    )
    .expect("a boot-only firmware arms an after-boot timer");

    assert_eq!(before.evaluate(0), Err(KernelError::ClockWentBackwards));
    assert_eq!(before.evaluate(999), Err(KernelError::ClockWentBackwards));
    assert_eq!(
        before.evaluate(1_000),
        Ok(Deadline::Remaining { ticks: 50 }),
        "the arming reading itself is not backwards"
    );
}

#[test]
fn an_at_persistent_time_timer_is_elapsed_when_the_restored_epoch_is_past_its_instant() {
    // Issue #32's second "done when". Power was removed for longer than the interval, and
    // the restored epoch says so.
    let armed = Timer::arm(
        TimerSpec::AtPersistentTime { instant: 2_000 },
        ClockCapability::Persistent,
        1_000,
    )
    .expect("a firmware with a persistent clock arms a persistent timer");
    assert_eq!(
        armed.evaluate(1_500),
        Ok(Deadline::Remaining { ticks: 500 })
    );

    // Power is removed and restored. The run replays and re-arms from the same spec, and
    // the clock has kept counting: 5_000 is past the instant the timer names.
    let restored = Timer::arm(
        TimerSpec::AtPersistentTime { instant: 2_000 },
        ClockCapability::Persistent,
        5_000,
    )
    .expect("the same spec arms again after the power came back");

    assert_eq!(
        restored.evaluate(5_000),
        Ok(Deadline::Elapsed),
        "an instant already in the past is elapsed on the first look"
    );
}

#[test]
fn a_persistent_timer_is_refused_rather_than_downgraded_without_a_clock() {
    // §11's whole point, and issue #32's first work item: never a silent downgrade.
    let refused = Timer::arm(
        TimerSpec::AtPersistentTime { instant: 2_000 },
        ClockCapability::BootOnly,
        0,
    );

    assert_eq!(refused, Err(KernelError::NoPersistentClock));
    assert_eq!(
        KernelError::NoPersistentClock.message(),
        "this firmware has no persistent clock",
        "issue #34 asks the refusal to name the missing capability"
    );
    assert_eq!(
        KernelError::ClockWentBackwards.message(),
        "a clock read below a reading already accepted",
        "the other refusal a timer can produce, pinned so the two cannot be swapped"
    );
}

#[test]
fn arming_never_changes_a_spec_into_another_one() {
    // The downgrade this design exists to prevent is not one call site: it is any call
    // site. So the property is stated over every spec and every capability rather than
    // over the one pair a downgrade would have used. An armed timer carries the spec it
    // was armed from, so the only two outcomes are a refusal and that same spec.
    for spec in EVERY_SPEC {
        for capability in EVERY_CAPABILITY {
            match Timer::arm(spec, capability, 7) {
                Ok(timer) => {
                    assert_eq!(timer.spec(), spec, "{spec:?} under {capability:?}");
                    assert_eq!(timer.armed_at(), 7);
                }
                Err(error) => {
                    assert_eq!(error, KernelError::NoPersistentClock);
                    assert_eq!(spec, TimerSpec::AtPersistentTime { instant: 2_000 });
                    assert_eq!(capability, ClockCapability::BootOnly);
                }
            }
        }
    }
}

#[test]
fn a_capability_admits_exactly_the_specs_it_can_service() {
    // The same table as above, read through the predicate rather than through the
    // constructor, so that a constructor that stopped consulting it is visible.
    assert_eq!(
        ClockCapability::BootOnly.admits(TimerSpec::AfterBoot { ticks: 1 }),
        Ok(())
    );
    assert_eq!(
        ClockCapability::BootOnly.admits(TimerSpec::AtPersistentTime { instant: 1 }),
        Err(KernelError::NoPersistentClock)
    );
    assert_eq!(
        ClockCapability::Persistent.admits(TimerSpec::AfterBoot { ticks: 1 }),
        Ok(()),
        "a device with an RTC still runs ordinary in-boot delays"
    );
    assert_eq!(
        ClockCapability::Persistent.admits(TimerSpec::AtPersistentTime { instant: 1 }),
        Ok(())
    );
}

#[test]
fn a_zero_interval_is_elapsed_at_the_arming_reading() {
    // The boundary a scheduler has to survive: a delay of nothing is a delay that is
    // already over, not one that never ends.
    let armed = Timer::arm(
        TimerSpec::AfterBoot { ticks: 0 },
        ClockCapability::BootOnly,
        1_000,
    )
    .expect("a zero interval is a legal interval");
    assert_eq!(armed.evaluate(1_000), Ok(Deadline::Elapsed));
}

#[test]
fn the_widest_readings_neither_wrap_nor_saturate() {
    // No addition happens anywhere in `evaluate`, which is what makes this hold: an
    // interval is compared against a difference rather than added to an arming reading.
    // A wrapped deadline is a timer that fires at once or never, and both are lies.
    let boot = Timer::arm(
        TimerSpec::AfterBoot { ticks: u64::MAX },
        ClockCapability::BootOnly,
        u64::MAX - 1,
    )
    .expect("the widest interval is a legal interval");
    assert_eq!(
        boot.evaluate(u64::MAX),
        Ok(Deadline::Remaining {
            ticks: u64::MAX - 1
        })
    );

    let persistent = Timer::arm(
        TimerSpec::AtPersistentTime { instant: u64::MAX },
        ClockCapability::Persistent,
        0,
    )
    .expect("the widest instant is a legal instant");
    assert_eq!(
        persistent.evaluate(u64::MAX - 1),
        Ok(Deadline::Remaining { ticks: 1 })
    );
    assert_eq!(persistent.evaluate(u64::MAX), Ok(Deadline::Elapsed));
}

#[test]
fn each_spec_reports_the_clock_kind_its_record_will_carry() {
    // §11: "A persistent timer record includes its clock kind so recovery cannot silently
    // reinterpret one policy as another." The numbers are spent here so that issue #33
    // adds a record body rather than a renumbering.
    assert_eq!(
        TimerSpec::AfterBoot { ticks: 1 }.clock_kind(),
        ClockKind::AFTER_BOOT
    );
    assert_eq!(
        TimerSpec::AtPersistentTime { instant: 1 }.clock_kind(),
        ClockKind::AT_PERSISTENT_TIME
    );
    assert_ne!(ClockKind::AFTER_BOOT, ClockKind::AT_PERSISTENT_TIME);
    assert_eq!(ClockKind::AFTER_BOOT.0, 1);
    assert_eq!(ClockKind::AT_PERSISTENT_TIME.0, 2);
}

#[test]
fn a_timer_is_kernel_state_of_a_pinned_size() {
    // Registered in `kernel_state_types!`, so it is charged against §04's 128 B budget
    // rather than being live state nothing counts.
    const {
        assert!(core::mem::size_of::<Timer>() == 24);
        assert!(core::mem::size_of::<ClockKind>() == 1);
    }
    assert!(
        waymaker_core::budget::KERNEL_STATE_TYPES
            .iter()
            .any(|entry| entry.name.contains("Timer")),
        "a pending timer is live for as long as the run waits on it"
    );
}

#[test]
fn the_deadline_and_the_specs_are_evaluated_in_a_const_context() {
    // `const` so that a firmware can decide a deadline without a runtime call, and so the
    // arithmetic below cannot quietly grow a panic: a `const` evaluation that overflowed
    // would fail the build rather than the device.
    const ARMED: Timer = match Timer::arm(
        TimerSpec::AfterBoot { ticks: 10 },
        ClockCapability::BootOnly,
        5,
    ) {
        Ok(timer) => timer,
        Err(_) => panic!("an after-boot timer is admitted by a boot-only clock"),
    };
    const VERDICT: Deadline = match ARMED.evaluate(12) {
        Ok(deadline) => deadline,
        Err(_) => panic!("a reading above the arming reading is not backwards"),
    };
    assert_eq!(VERDICT, Deadline::Remaining { ticks: 3 });
}

#[test]
fn a_spec_and_its_recorded_pair_are_the_same_deadline() {
    // Issue #33's `TimerScheduled` record stores a clock kind and a deadline, and replay
    // rebuilds the spec from them. A round trip that lost the kind would rebuild the other
    // policy, which is the silent reinterpretation §11 forbids.
    for spec in EVERY_SPEC {
        assert_eq!(
            TimerSpec::recorded(spec.clock_kind(), spec.deadline()),
            Some(spec),
            "{spec:?}"
        );
    }
}

#[test]
fn each_spec_reports_its_own_deadline() {
    assert_eq!(TimerSpec::AfterBoot { ticks: 50 }.deadline(), 50);
    assert_eq!(
        TimerSpec::AtPersistentTime { instant: 2_000 }.deadline(),
        2_000
    );
}

#[test]
fn a_clock_kind_number_no_firmware_wrote_is_no_spec_at_all() {
    // Zero is not a kind, and neither is an erased byte. A conversion with a wildcard arm
    // would read either as a policy — which is how a zeroed page becomes a timer.
    for number in [0_u8, 3, 0x7F, 0xFF] {
        assert_eq!(TimerSpec::recorded(ClockKind(number), 10), None, "{number}");
    }
}

#[test]
fn a_persistent_floor_crosses_a_reset_and_a_boot_floor_does_not() {
    // Issue #33 puts the arming reading on media, and what it means on the other side of a
    // reset depends on the clock. The persistent one survives, so the floor does. The boot
    // one restarts, so a reading below the recorded value is a reset rather than a clock
    // that went backwards — and taking the recorded value there refuses a healthy clock.
    let persistent = TimerSpec::AtPersistentTime { instant: 2_000 };
    assert_eq!(persistent.rearmed_at(1_500, 1_400), 1_500);
    assert_eq!(persistent.rearmed_at(1_500, 1_600), 1_500);

    let boot = TimerSpec::AfterBoot { ticks: 50 };
    assert_eq!(boot.rearmed_at(5_000, 20), 20);
    assert_eq!(boot.rearmed_at(5_000, 5_100), 5_000);
}

#[test]
fn a_boot_deadline_re_armed_after_a_reset_is_never_a_backwards_clock() {
    // The whole point of the rule above, stated against `evaluate`: the floor a reset
    // leaves must not be one the kernel refuses, because a run whose boundary is open has
    // no edge to a terminal record and so no way to end.
    let boot = TimerSpec::AfterBoot { ticks: 50 };
    let armed = Timer::arm(boot, ClockCapability::BootOnly, boot.rearmed_at(5_000, 20));
    assert_eq!(
        armed.and_then(|timer| timer.evaluate(20)),
        Ok(Deadline::Remaining { ticks: 50 })
    );

    // And the persistent twin still catches a clock that really did move backwards.
    let persistent = TimerSpec::AtPersistentTime { instant: 9_000 };
    let armed = Timer::arm(
        persistent,
        ClockCapability::Persistent,
        persistent.rearmed_at(5_000, 4_000),
    );
    assert_eq!(
        armed.and_then(|timer| timer.evaluate(4_000)),
        Err(KernelError::ClockWentBackwards)
    );
}
