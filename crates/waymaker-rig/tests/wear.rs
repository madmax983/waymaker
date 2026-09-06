//! Erase counts and per-effect write amplification, measured rather than asserted.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27) asks the rig to "record erase
//! counts and per-effect write amplification across the run", and
//! [`waymaker_flash::append::WriteAmplification`] counts neither erases nor effects: it is a
//! journal's figure, and a journal does not erase. The rig meters the device instead, which
//! is also the only way to see traffic the journal did not issue.

use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::wear::{Metered, PerEffect, Traffic, Wear};

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 256, 4, 1) else {
        unreachable!("1 | 4 | 256 | 1024 is a legal geometry")
    };
    geometry
}

fn device() -> waymaker_fault::Device {
    waymaker_fault::Device::new(geometry())
}

/// The meter under test, over a device the caller owns.
///
/// A meter borrows its storage, so every case names the device it is metering rather than
/// letting a temporary live only as long as the call — which is the same discipline
/// `Window` imposes, and for the same reason.
macro_rules! metered {
    ($device:ident, $meter:ident) => {
        let mut $device = device();
        #[allow(
            unused_mut,
            reason = "a case that only reads the meter needs no mutation"
        )]
        let mut $meter = Metered::new(&mut $device);
    };
}

#[test]
fn a_fresh_meter_has_counted_nothing() {
    metered!(part, meter);
    assert_eq!(meter.wear(), Wear::NONE);
    assert_eq!(meter.wear().erase_operations(), 0);
    assert_eq!(meter.wear().erased_bytes(), 0);
    assert_eq!(meter.wear().erase_blocks(), 0);
    assert_eq!(meter.wear().effects(), 0);
}

#[test]
fn an_erase_is_counted_in_operations_bytes_and_blocks() {
    // Three figures rather than one: a part's endurance is quoted per block, a rig's cost is
    // per operation, and the bytes are what tells the two apart when an erase covers more
    // than one block.
    metered!(part, meter);
    meter.erase(0, 512).expect("two blocks of a legal region");
    assert_eq!(meter.wear().erase_operations(), 1);
    assert_eq!(meter.wear().erased_bytes(), 512);
    assert_eq!(meter.wear().erase_blocks(), 2);
}

#[test]
fn a_failed_erase_is_still_wear() {
    // Design document §12: "a failed erase may still have changed media". A counter that
    // only counted successes would understate exactly the runs that wore the part.
    metered!(part, meter);
    let refused = meter.erase(1, 256);
    assert!(refused.is_err(), "a misaligned erase is refused");
    assert_eq!(meter.wear().erase_operations(), 1);
}

#[test]
fn programs_and_barriers_are_counted_with_the_bytes_they_carried() {
    metered!(part, meter);
    meter.erase(0, 256).expect("one block");
    meter.program(0, b"\x01\x02\x03\x04").expect("four bytes");
    meter.barrier().expect("a barrier");
    meter
        .program(4, b"\x05\x06\x07\x08")
        .expect("four more bytes");
    meter.barrier().expect("a barrier");
    assert_eq!(meter.wear().program_operations(), 2);
    assert_eq!(meter.wear().programmed_bytes(), 8);
    assert_eq!(meter.wear().barriers(), 2);
}

#[test]
fn a_read_is_not_wear() {
    metered!(part, meter);
    let mut page = [0_u8; 4];
    meter.read(0, &mut page).expect("a legal read");
    assert_eq!(meter.wear(), Wear::NONE);
}

#[test]
fn the_meter_hands_the_geometry_through_unchanged() {
    metered!(part, meter);
    assert_eq!(meter.geometry(), geometry());
}

#[test]
fn traffic_the_rig_causes_is_not_charged_to_the_engine() {
    // The rig's own witness marks are programs the engine never issued. Publishing them as
    // the engine's write amplification would report the instrument's cost as the subject's.
    metered!(part, meter);
    meter.erase(0, 256).expect("one block");
    meter.program(0, b"\x01\x02\x03\x04").expect("the engine");
    meter.set_traffic(Traffic::Rig);
    meter.program(4, b"\x05\x06\x07\x08").expect("the rig");
    meter.set_traffic(Traffic::Engine);
    meter
        .program(8, b"\x09\x0A\x0B\x0C")
        .expect("the engine again");

    assert_eq!(meter.wear().programmed_bytes(), 8);
    assert_eq!(meter.wear().program_operations(), 2);
    assert_eq!(meter.rig_wear().programmed_bytes(), 4);
    assert_eq!(meter.rig_wear().program_operations(), 1);
    assert_eq!(meter.total_wear().programmed_bytes(), 12);
}

#[test]
fn an_effect_is_credited_once_and_the_figures_are_per_effect() {
    metered!(part, meter);
    meter.erase(0, 256).expect("one block");
    for effect in 0..4_u32 {
        meter
            .program(effect * 8, b"\x01\x02\x03\x04")
            .expect("a schedule");
        meter.barrier().expect("a barrier");
        meter
            .program(effect * 8 + 4, b"\x05\x06\x07\x08")
            .expect("a completion");
        meter.barrier().expect("a barrier");
        meter.credit_effect();
    }
    let wear = meter.wear();
    assert_eq!(wear.effects(), 4);
    assert_eq!(wear.programmed_bytes(), 32);
    assert_eq!(wear.program_operations(), 8);
    assert_eq!(wear.barriers(), 8);
    // Exact here — four effects, thirty-two bytes — and asserted as exact rather than as a
    // number, so a truncating quotient could not satisfy it by accident.
    let bytes = wear.programmed_bytes_per_effect().expect("effects ran");
    assert_eq!(bytes.whole(), 8);
    assert_eq!(bytes.hundredths(), 800);
    assert!(bytes.is_exact());
    assert_eq!(bytes.to_string(), "8.00");
    assert_eq!(
        wear.program_operations_per_effect().map(PerEffect::whole),
        Some(2)
    );
    assert_eq!(wear.barriers_per_effect().map(PerEffect::whole), Some(2));
}

#[test]
fn a_run_with_no_effects_has_no_per_effect_figure() {
    // Not zero. Zero is a measurement; "no effects ran" is the absence of one, and a report
    // that printed 0 B per effect would be reporting a division nobody performed.
    let wear = Wear::NONE;
    assert_eq!(wear.programmed_bytes_per_effect(), None);
    assert_eq!(wear.program_operations_per_effect(), None);
    assert_eq!(wear.barriers_per_effect(), None);
    assert_eq!(wear.erase_operations_per_effect(), None);
    assert_eq!(wear.payload_bytes_per_effect(), None);
}

#[test]
fn a_meter_that_saw_traffic_the_journal_did_not_count_does_not_agree_with_it() {
    use waymaker_flash::append::WriteAmplification;
    metered!(part, meter);
    meter.erase(0, 256).expect("one block");
    meter.program(0, b"\x01\x02\x03\x04").expect("a program");
    assert!(
        !meter.wear().agrees_with(WriteAmplification::NONE),
        "a meter that counted a program agreed with a journal that counted none"
    );
}

#[test]
fn a_per_effect_figure_that_does_not_divide_is_not_rounded_down_to_a_whole() {
    // The defect Codex found: an eight-effect run's thirty-eight program calls are 4.75 each,
    // and an integer quotient publishes 4 — less wear than was measured, silently.
    metered!(part, meter);
    meter.erase(0, 256).expect("one block");
    for step in 0..3_u32 {
        meter
            .program(step * 4, b"\x01\x02\x03\x04")
            .expect("a program");
    }
    for _ in 0..2 {
        meter.credit_effect();
    }
    let figure = meter
        .wear()
        .program_operations_per_effect()
        .expect("effects ran");
    assert_eq!(figure.total(), 3);
    assert_eq!(figure.effects(), 2);
    assert_eq!(figure.whole(), 1, "the integer part alone understates");
    assert_eq!(figure.hundredths(), 150);
    assert!(!figure.is_exact());
    assert_eq!(figure.to_string(), "1.50");
}

#[test]
fn a_run_with_no_effects_has_no_fraction_at_all() {
    // `None`, not a zero denominator: a division nobody performed is not a figure.
    assert_eq!(Wear::NONE.program_operations_per_effect(), None);
}

#[test]
fn two_wears_add() {
    let left = Wear::NONE;
    metered!(part, meter);
    meter.erase(0, 256).expect("one block");
    meter.credit_effect();
    let right = meter.wear();
    assert_eq!(left.plus(right), right);
    assert_eq!(right.plus(right).erase_operations(), 2);
    assert_eq!(right.plus(right).effects(), 2);
}

#[test]
fn a_wear_total_saturates_rather_than_wrapping() {
    let full = Wear::NONE.plus(Wear::SATURATED);
    assert_eq!(full.erased_bytes(), u32::MAX);
    assert_eq!(full.plus(Wear::SATURATED).erased_bytes(), u32::MAX);
    assert_eq!(full.plus(Wear::SATURATED).effects(), u32::MAX);
}

#[test]
fn the_journals_own_amplification_is_carried_alongside_the_meters() {
    // The two are the same measurement taken from opposite ends: the journal counts what it
    // asked for, the meter counts what the device was asked for. They agree when nothing
    // else is writing, and the rig checks that rather than assuming it.
    use waymaker_flash::append::WriteAmplification;
    let wear = Wear::NONE.with_amplification(WriteAmplification::NONE);
    assert_eq!(wear.payload_bytes(), 0);
    assert!(
        wear.agrees_with(WriteAmplification::NONE),
        "two counters of nothing disagreed"
    );
}
