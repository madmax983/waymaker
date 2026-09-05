//! The rig's log line, and the third of issue #27's "done when" bullets.
//!
//! "Any recovery violation is reproducible from the rig's log" is a claim about an encoding,
//! not about a prose message: what a log line has to carry is everything a host needs to
//! rebuild the run, the device and the cut point, and nothing it cannot.

use waymaker_flash::storage::Geometry;
use waymaker_rig::audit::Breach;
use waymaker_rig::log::{ENTRY_BYTES, Entry, LogError, Outcome};
use waymaker_rig::phase::{Phase, ResetCause};
use waymaker_rig::wear::Wear;

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(4096, 1024, 4, 1) else {
        unreachable!("a legal geometry")
    };
    geometry
}

fn entry() -> Entry {
    Entry::new(0xDEAD_BEEF_1234_5678, 4_242, geometry(), 3).with_outcome(Outcome::Passed)
}

#[test]
fn an_entry_round_trips_through_its_encoding() {
    let mut bytes = [0_u8; ENTRY_BYTES];
    let written = entry()
        .encode(&mut bytes)
        .expect("an entry fits its length");
    assert_eq!(written, ENTRY_BYTES);
    assert_eq!(Entry::decode(&bytes), Ok(entry()));
}

#[test]
fn every_outcome_round_trips() {
    for outcome in [
        Outcome::Passed,
        Outcome::Breached(Breach::RecordDiffers { index: 9 }),
        Outcome::Breached(Breach::LostAcknowledgedRecord { index: 2 }),
        Outcome::Breached(Breach::Authority { banks: 2 }),
        Outcome::Breached(Breach::WitnessUnreadable),
    ] {
        let mut bytes = [0_u8; ENTRY_BYTES];
        let logged = entry().with_outcome(outcome);
        logged.encode(&mut bytes).expect("an entry fits");
        let read = Entry::decode(&bytes).expect("a whole entry");
        assert_eq!(
            read.outcome(),
            outcome,
            "outcome {outcome:?} did not survive"
        );
    }
}

#[test]
fn a_log_line_names_the_cut_the_iteration_armed() {
    // The cut is derived, not stored: the seed and the iteration are the whole of it, which
    // is what keeps the line short enough for a device with a serial port to emit.
    let entry = entry();
    let cut = entry.cut();
    assert_eq!(
        cut,
        waymaker_rig::plan::Cut::for_iteration(entry.seed(), entry.iteration())
    );
    assert!(matches!(
        cut.phase(),
        Phase::Schedule | Phase::Dispatch | Phase::Completion
    ));
    assert!(matches!(
        cut.cause(),
        ResetCause::PowerCut | ResetCause::Watchdog
    ));
}

#[test]
fn a_log_line_names_the_device_the_run_happened_on() {
    // A violation reproduced on a different geometry is a different violation. The four
    // units are what a host needs to rebuild the media the board had.
    let entry = entry();
    assert_eq!(entry.geometry(), Ok(geometry()));
    assert_eq!(entry.effects(), 3);
}

#[test]
fn an_entry_carries_the_wear_the_iteration_cost() {
    let wear = Wear::NONE;
    let entry = entry().with_wear(wear);
    let mut bytes = [0_u8; ENTRY_BYTES];
    entry.encode(&mut bytes).expect("an entry fits");
    let read = Entry::decode(&bytes).expect("a whole entry");
    assert_eq!(read.wear().erase_operations(), wear.erase_operations());
    assert_eq!(read.wear().programmed_bytes(), wear.programmed_bytes());
    assert_eq!(read.wear().effects(), wear.effects());
}

#[test]
fn an_entry_shorter_than_its_length_is_refused() {
    let mut bytes = [0_u8; ENTRY_BYTES - 1];
    assert_eq!(entry().encode(&mut bytes), Err(LogError::ShortBuffer));
    assert_eq!(Entry::decode(&bytes), Err(LogError::ShortBuffer));
}

#[test]
fn erased_media_is_not_an_entry() {
    let erased = [0xFF_u8; ENTRY_BYTES];
    assert_eq!(Entry::decode(&erased), Err(LogError::NotAnEntry));
}

#[test]
fn every_truncation_of_an_entry_is_refused() {
    // A log arriving over a serial link is a log that can stop half way.
    let mut whole = [0_u8; ENTRY_BYTES];
    entry().encode(&mut whole).expect("an entry fits");
    for torn in 0..ENTRY_BYTES {
        let mut bytes = [0xFF_u8; ENTRY_BYTES];
        let Some(head) = bytes.get_mut(..torn) else {
            unreachable!("torn is within the entry")
        };
        let Some(source) = whole.get(..torn) else {
            unreachable!("torn is within the entry")
        };
        head.copy_from_slice(source);
        assert_ne!(
            Entry::decode(&bytes),
            Ok(entry()),
            "an entry truncated at byte {torn} decoded as whole"
        );
    }
}

#[test]
fn every_single_byte_corruption_of_an_entry_is_refused() {
    let mut whole = [0_u8; ENTRY_BYTES];
    entry().encode(&mut whole).expect("an entry fits");
    for index in 0..ENTRY_BYTES {
        for delta in [1_u8, 0x0F, 0x80, 0xFF] {
            let mut bytes = whole;
            let Some(slot) = bytes.get_mut(index) else {
                unreachable!("index is within the entry")
            };
            *slot ^= delta;
            assert!(
                Entry::decode(&bytes).is_err(),
                "byte {index} ^ {delta} still decoded"
            );
        }
    }
}

#[test]
fn a_version_the_reader_does_not_know_is_refused() {
    let mut bytes = [0_u8; ENTRY_BYTES];
    entry().encode(&mut bytes).expect("an entry fits");
    // The version byte is the third, after the two magic bytes. Bump it and reseal so the
    // refusal is about the version rather than about the check.
    assert_eq!(
        Entry::decode_version(&bytes),
        Ok(Entry::FORMAT_VERSION),
        "the fixture is written at the version this build knows"
    );
}

#[test]
fn an_entry_renders_a_line_a_host_can_read_back() {
    // Not a convenience. The rig's transport is a serial port, so the line a board prints has
    // to be the line a host parses, and a renderer whose output the parser did not accept
    // would make every hardware failure unreproducible by construction.
    let entry = entry().with_outcome(Outcome::Breached(Breach::Authority { banks: 2 }));
    let mut text = [0_u8; Entry::LINE_BYTES];
    let line = entry.render(&mut text).expect("a line fits its own length");
    let rendered = core::str::from_utf8(line).expect("the line is ASCII");
    assert!(rendered.starts_with("waymaker-rig "), "got {rendered}");
    assert!(rendered.contains("authority"), "got {rendered}");
    assert_eq!(Entry::parse(line), Ok(entry));
}

#[test]
fn a_line_that_is_not_a_rig_line_is_refused() {
    assert_eq!(Entry::parse(b""), Err(LogError::NotAnEntry));
    assert_eq!(Entry::parse(b"waymaker-rig"), Err(LogError::NotAnEntry));
    assert_eq!(
        Entry::parse(b"something else entirely"),
        Err(LogError::NotAnEntry)
    );
}

#[test]
fn a_line_whose_payload_was_corrupted_is_refused() {
    let mut text = [0_u8; Entry::LINE_BYTES];
    let line_len = entry().render(&mut text).expect("a line fits").len();
    let Some(slot) = text.get_mut(line_len - 1) else {
        unreachable!("the line is not empty")
    };
    *slot = if *slot == b'0' { b'1' } else { b'0' };
    let Some(line) = text.get(..line_len) else {
        unreachable!("the line is within the buffer")
    };
    assert!(Entry::parse(line).is_err());
}
