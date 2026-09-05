//! The durable witness: what the rig had done when the supply went.
//!
//! A host crash sweep keeps its ledger in RAM. A power cut takes RAM, so a rig that did the
//! same would come back knowing only what is on media — and "what is on media" is the thing
//! under test, not the thing that judges it. Two of design document §14's guarantees are
//! statements about what the *writer* had done: `acknowledged-durability` is about records
//! whose barrier returned, and `durable-intent` is about effects that were dispatched.
//! Neither is checkable from the journal alone.

use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::witness::{MARK_BYTES, Mark, Stage, Witness, WitnessError, WitnessRegion};

const ITERATION: u32 = 0x0123_4567;

fn geometry(program: u32) -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 256, program, 1) else {
        unreachable!("a legal geometry")
    };
    geometry
}

fn region(program: u32) -> WitnessRegion {
    let Ok(region) = WitnessRegion::of(geometry(program), 0, 256) else {
        unreachable!("the first erase block is a legal witness region")
    };
    region
}

fn device(program: u32) -> waymaker_fault::Device {
    let mut device = waymaker_fault::Device::new(geometry(program));
    // A helper in an integration test is not a test body, and the workspace denies `expect`
    // outside one — see CLAUDE.md. A refusal here would be a broken fixture, not a finding.
    let Ok(()) = device.erase(0, 256) else {
        unreachable!("the first erase block of a legal geometry")
    };
    device
}

#[test]
fn a_mark_round_trips_through_its_encoding() {
    for stage in [Stage::Attempted, Stage::Acknowledged, Stage::Dispatched] {
        let mark = Mark::new(ITERATION, 7, stage);
        let mut bytes = [0_u8; MARK_BYTES];
        let written = mark.encode(&mut bytes).expect("a mark fits its own length");
        assert_eq!(written, MARK_BYTES);
        assert_eq!(Mark::decode(&bytes), Ok(mark));
    }
}

#[test]
fn an_erased_slot_is_not_a_mark() {
    // Erased is 0xFF. A witness that read one as a mark would report progress the rig never
    // made, which is the direction that turns a passing run into a false green.
    let erased = [0xFF_u8; MARK_BYTES];
    assert_eq!(Mark::decode(&erased), Err(WitnessError::NotAMark));
}

#[test]
fn a_partially_programmed_mark_is_refused() {
    // A NOR program clears bits in order, so a torn mark is a prefix of the real bytes with
    // an erased tail. Every truncation of every mark must be refused.
    let mark = Mark::new(ITERATION, 3, Stage::Acknowledged);
    let mut whole = [0_u8; MARK_BYTES];
    mark.encode(&mut whole).expect("a mark fits");
    for torn in 0..MARK_BYTES {
        let mut bytes = [0xFF_u8; MARK_BYTES];
        let Some(head) = bytes.get_mut(..torn) else {
            unreachable!("torn is within the mark")
        };
        let Some(source) = whole.get(..torn) else {
            unreachable!("torn is within the mark")
        };
        head.copy_from_slice(source);
        assert_ne!(
            Mark::decode(&bytes),
            Ok(mark),
            "a mark torn at byte {torn} decoded as whole"
        );
    }
}

#[test]
fn every_single_byte_mutation_of_a_mark_is_refused_or_is_a_different_mark() {
    let mark = Mark::new(ITERATION, 3, Stage::Acknowledged);
    let mut whole = [0_u8; MARK_BYTES];
    mark.encode(&mut whole).expect("a mark fits");
    for index in 0..MARK_BYTES {
        for delta in 1..=255_u8 {
            let mut bytes = whole;
            let Some(slot) = bytes.get_mut(index) else {
                unreachable!("index is within the mark")
            };
            *slot ^= delta;
            assert_ne!(
                Mark::decode(&bytes),
                Ok(mark),
                "byte {index} ^ {delta} still decoded as the original mark"
            );
        }
    }
}

#[test]
fn a_short_buffer_is_refused_rather_than_truncated() {
    let mark = Mark::new(ITERATION, 0, Stage::Attempted);
    let mut bytes = [0_u8; MARK_BYTES - 1];
    assert_eq!(mark.encode(&mut bytes), Err(WitnessError::ShortBuffer));
    assert_eq!(Mark::decode(&bytes), Err(WitnessError::ShortBuffer));
}

#[test]
fn a_witness_region_rounds_its_slot_up_to_the_program_unit() {
    // A mark must land in whole program units or a torn one could straddle two slots.
    let mark_bytes = u32::try_from(MARK_BYTES).expect("a mark is twelve bytes");
    assert_eq!(region(1).slot_bytes(), mark_bytes);
    assert_eq!(region(4).slot_bytes(), mark_bytes);
    assert_eq!(region(8).slot_bytes(), 16);
    assert_eq!(region(16).slot_bytes(), 16);
}

#[test]
fn a_region_that_cannot_hold_a_mark_is_refused() {
    assert!(WitnessRegion::of(geometry(4), 0, 8).is_err());
    assert!(WitnessRegion::of(geometry(4), 1, 256).is_err());
    assert!(WitnessRegion::of(geometry(4), 0, 2048).is_err());
}

#[test]
fn a_scan_of_an_erased_region_reports_no_progress() {
    let mut storage = device(4);
    let mut page = [0_u8; 64];
    let progress = Witness::new(region(4))
        .scan(&mut storage, &mut page)
        .expect("an erased region is a legal empty witness");
    assert_eq!(progress.iteration(), None);
    assert_eq!(progress.attempted(), None);
    assert_eq!(progress.acknowledged(), None);
    assert_eq!(progress.dispatched(), None);
    assert_eq!(progress.marks(), 0);
    assert!(!progress.torn());
}

#[test]
fn what_was_marked_is_what_a_scan_reports() {
    let mut storage = device(4);
    let mut page = [0_u8; 64];
    let mut witness = Witness::new(region(4));
    for index in 0..3_u16 {
        witness
            .mark(
                &mut storage,
                Mark::new(ITERATION, index, Stage::Attempted),
                &mut page,
            )
            .expect("a mark fits the region");
        witness
            .mark(
                &mut storage,
                Mark::new(ITERATION, index, Stage::Acknowledged),
                &mut page,
            )
            .expect("a mark fits the region");
    }
    witness
        .mark(
            &mut storage,
            Mark::new(ITERATION, 1, Stage::Dispatched),
            &mut page,
        )
        .expect("a mark fits the region");

    let progress = Witness::new(region(4))
        .scan(&mut storage, &mut page)
        .expect("the marks this test wrote are legal");
    assert_eq!(progress.iteration(), Some(ITERATION));
    assert_eq!(progress.attempted(), Some(2));
    assert_eq!(progress.acknowledged(), Some(2));
    assert_eq!(progress.dispatched(), Some(1));
    assert_eq!(progress.marks(), 7);
}

#[test]
fn a_torn_final_mark_is_reported_and_does_not_raise_the_high_water() {
    // The one direction that matters: a torn acknowledgment must *under*-claim. Claiming a
    // record was acknowledged when the mark never landed would make the rig demand a record
    // recovery is not obliged to have.
    let mut storage = device(4);
    let mut page = [0_u8; 64];
    let mut witness = Witness::new(region(4));
    witness
        .mark(
            &mut storage,
            Mark::new(ITERATION, 0, Stage::Attempted),
            &mut page,
        )
        .expect("a mark fits");
    witness
        .mark(
            &mut storage,
            Mark::new(ITERATION, 0, Stage::Acknowledged),
            &mut page,
        )
        .expect("a mark fits");

    // The third slot is torn: the first four bytes of an `Attempted(1)` mark landed.
    let mut whole = [0_u8; MARK_BYTES];
    Mark::new(ITERATION, 1, Stage::Attempted)
        .encode(&mut whole)
        .expect("a mark fits");
    let Some(head) = whole.get(..4) else {
        unreachable!("a mark is longer than four bytes")
    };
    storage.program(24, head).expect("a torn mark");

    let progress = Witness::new(region(4))
        .scan(&mut storage, &mut page)
        .expect("a torn tail ends a witness rather than breaking it");
    assert_eq!(progress.attempted(), Some(0));
    assert_eq!(progress.acknowledged(), Some(0));
    assert_eq!(progress.marks(), 2);
    assert!(progress.torn());
}

#[test]
fn a_mark_after_a_hole_is_a_refusal_rather_than_a_reading() {
    // Marks are appended, so a valid mark past a gap is media the rig did not produce. The
    // witness is the instrument: an instrument that guesses is worse than one that stops.
    let mut storage = device(4);
    let mut page = [0_u8; 64];
    let mut whole = [0_u8; MARK_BYTES];
    Mark::new(ITERATION, 0, Stage::Attempted)
        .encode(&mut whole)
        .expect("a mark fits");
    storage
        .program(24, &whole)
        .expect("a mark in the third slot");

    assert_eq!(
        Witness::new(region(4)).scan(&mut storage, &mut page),
        Err(WitnessError::Hole)
    );
}

#[test]
fn a_witness_carrying_two_iterations_is_a_refusal() {
    let mut storage = device(4);
    let mut page = [0_u8; 64];
    let mut witness = Witness::new(region(4));
    witness
        .mark(
            &mut storage,
            Mark::new(ITERATION, 0, Stage::Attempted),
            &mut page,
        )
        .expect("a mark fits");
    witness
        .mark(
            &mut storage,
            Mark::new(ITERATION + 1, 0, Stage::Attempted),
            &mut page,
        )
        .expect("a mark fits");
    assert_eq!(
        Witness::new(region(4)).scan(&mut storage, &mut page),
        Err(WitnessError::MixedIterations)
    );
}

#[test]
fn a_stage_that_went_backwards_is_a_refusal() {
    let mut storage = device(4);
    let mut page = [0_u8; 64];
    let mut witness = Witness::new(region(4));
    witness
        .mark(
            &mut storage,
            Mark::new(ITERATION, 3, Stage::Attempted),
            &mut page,
        )
        .expect("a mark fits");
    witness
        .mark(
            &mut storage,
            Mark::new(ITERATION, 1, Stage::Attempted),
            &mut page,
        )
        .expect("a mark fits");
    assert_eq!(
        Witness::new(region(4)).scan(&mut storage, &mut page),
        Err(WitnessError::OutOfOrder)
    );
}

#[test]
fn a_full_witness_refuses_rather_than_wrapping() {
    let mut storage = device(4);
    let mut page = [0_u8; 64];
    let mut witness = Witness::new(region(4));
    let capacity = region(4).capacity();
    for index in 0..capacity {
        let index = u16::try_from(index).expect("the capacity fits a mark index");
        witness
            .mark(
                &mut storage,
                Mark::new(ITERATION, index, Stage::Attempted),
                &mut page,
            )
            .expect("a mark within the capacity");
    }
    let overflow = witness.mark(
        &mut storage,
        Mark::new(ITERATION, 0, Stage::Attempted),
        &mut page,
    );
    assert_eq!(overflow, Err(WitnessError::Full));
}

#[test]
fn a_page_shorter_than_a_slot_is_refused() {
    let mut storage = device(8);
    let mut page = [0_u8; 12];
    assert_eq!(
        Witness::new(region(8)).scan(&mut storage, &mut page),
        Err(WitnessError::ShortBuffer)
    );
}

#[test]
fn a_witness_scanned_on_a_different_geometry_is_refused() {
    // The region was validated against one device. Bounds proved on one say nothing about
    // another — the same rule `waymaker_flash::recovery::JournalRegion` holds itself to.
    let mut storage = device(8);
    let mut page = [0_u8; 64];
    assert_eq!(
        Witness::new(region(4)).scan(&mut storage, &mut page),
        Err(WitnessError::WrongGeometry)
    );
}
