//! The commit seal: what makes a frame on media a *committed* record.
//!
//! Design document §09 gives every record frame a `commit_seal` one storage-program unit
//! wide, and §07 says when it may be written: after a payload barrier, and never before.
//! Issue [#24](https://github.com/madmax983/waymaker/issues/24) is what puts it on media.
//!
//! This file is about the seal as *bytes* — its shape, the properties that make an erased
//! program unit and a torn one both refusable, and the two readers now requiring one.
//! `tests/append.rs` is about the writer that programs it in the right order.

use waymaker_core::{DecodeError, EffectSeq, RecordRef};
use waymaker_flash::frame::{self, ERASED_BYTE, ProgramAlign, SEAL_PATTERN_BYTES, Scan};

/// A granularity every case below shares, unless it is about a different one.
fn align(bytes: u16) -> ProgramAlign {
    let Some(align) = ProgramAlign::new(bytes) else {
        unreachable!("{bytes} is a power of two within the program-size range")
    };
    align
}

/// A record with a payload, so a frame is not all overhead.
const fn record() -> RecordRef<'static> {
    RecordRef::EffectCompleted {
        seq: EffectSeq(3),
        result: b"seven",
    }
}

// ---------------------------------------------------------------------------------------
// The seal's own properties
// ---------------------------------------------------------------------------------------

#[test]
fn no_byte_of_a_commit_seal_is_ever_erased() {
    // The whole of "erased media is never a seal, and a seal torn part-way through its own
    // program is never a whole one", over every value a frame check can take in the low
    // bits that reach a seal byte. Exhaustive over the pattern's domain: each byte of the
    // seal is a byte of the check, so 256 values per position is the whole space.
    for byte in 0..=u8::MAX {
        let check = u32::from_le_bytes([byte, byte, byte, byte]);
        for slot in frame::commit_seal(check) {
            assert_ne!(
                slot, ERASED_BYTE,
                "a seal byte read as erased, so an erased program unit would seal a frame"
            );
        }
    }
}

#[test]
fn a_seal_holds_only_over_the_frame_check_it_was_computed_for() {
    let seal = frame::commit_seal(0x1234_5678);
    assert!(frame::commit_seal_holds(0x1234_5678, &seal));
    assert!(!frame::commit_seal_holds(0x1234_5679, &seal));
}

#[test]
fn every_torn_seal_is_refused() {
    // A program writes bytes in order, so an interrupted one leaves a prefix and erased
    // media behind it. Every one of those, at every width a seal can have.
    for width in [1_usize, 2, 4, 8, 16, 64] {
        let check = 0x89AB_CDEF;
        let pattern = frame::commit_seal(check);
        let whole: Vec<u8> = (0..width)
            .map(|at| pattern[at % SEAL_PATTERN_BYTES])
            .collect();
        assert!(
            frame::commit_seal_holds(check, &whole),
            "a seal of {width} bytes written in full must hold"
        );
        for landed in 0..width {
            let mut torn = whole.clone();
            for slot in torn.iter_mut().skip(landed) {
                *slot = ERASED_BYTE;
            }
            assert!(
                !frame::commit_seal_holds(check, &torn),
                "a {width}-byte seal with {landed} bytes on media was accepted"
            );
        }
    }
}

#[test]
fn a_seal_of_no_bytes_seals_nothing() {
    // There is no such seal — the width is a `ProgramAlign` and never zero — and the
    // answer still has to be `false` rather than `all()`'s vacuous `true`.
    assert!(!frame::commit_seal_holds(0, &[]));
}

// ---------------------------------------------------------------------------------------
// What a record now occupies
// ---------------------------------------------------------------------------------------

#[test]
fn a_record_is_its_padded_body_and_one_program_unit_of_seal() {
    for unit in [1_u16, 2, 4, 8, 32] {
        let align = align(unit);
        let Ok(body) = frame::body_len(&record(), align) else {
            unreachable!("this record encodes")
        };
        let Ok(total) = frame::encoded_len(&record(), align) else {
            unreachable!("this record encodes")
        };
        assert_eq!(
            total,
            body + frame::seal_bytes(align),
            "a record at a {unit}-byte program unit is its padded body plus one seal"
        );
        assert_eq!(frame::seal_bytes(align), usize::from(unit));
        assert_eq!(
            body % usize::from(unit),
            0,
            "the body ends on a program unit"
        );
    }
}

#[test]
fn encode_writes_the_body_and_then_the_seal() {
    let align = align(4);
    let mut page = [0_u8; 64];
    let Ok(written) = frame::encode(&record(), align, &mut page) else {
        unreachable!("64 bytes holds this record")
    };
    let Ok(body) = frame::body_len(&record(), align) else {
        unreachable!("this record encodes")
    };
    assert_eq!(written, body + frame::seal_bytes(align));

    // The body still decodes on its own: the seal is a fact about durability, not a field
    // the decoder reads.
    let Ok(frame) = frame::decode(&page[..body]) else {
        unreachable!("the body this encode wrote decodes")
    };
    assert_eq!(frame.frame_len, frame::FRAME_OVERHEAD_BYTES + 5);

    // And the bytes after it are the seal the body deserves.
    assert!(frame::commit_seal_holds(
        frame.frame_crc,
        &page[body..written]
    ));
}

// ---------------------------------------------------------------------------------------
// The readers now require one
// ---------------------------------------------------------------------------------------

/// A journal holding one record, and the offset its seal starts at.
fn one_record(align: ProgramAlign) -> (Vec<u8>, usize, usize) {
    let mut page = [0_u8; 64];
    let Ok(written) = frame::encode(&record(), align, &mut page) else {
        unreachable!("64 bytes holds this record")
    };
    let Ok(body) = frame::body_len(&record(), align) else {
        unreachable!("this record encodes")
    };
    let Some(bytes) = page.get(..written) else {
        unreachable!("`encode` reports what it wrote")
    };
    let mut journal = bytes.to_vec();
    journal.resize(written + 32, ERASED_BYTE);
    (journal, body, written)
}

#[test]
fn a_scan_walks_a_sealed_record_and_ends_cleanly() {
    let align = align(4);
    let (journal, _body, written) = one_record(align);
    let mut scan = Scan::new(&journal, align);
    assert_eq!(scan.next(), Some(Ok(record())));
    assert_eq!(scan.next(), None);
    assert_eq!(
        scan.offset(),
        written,
        "the committed prefix ends past the record's seal"
    );
}

#[test]
fn a_scan_refuses_a_frame_whose_seal_never_landed() {
    let align = align(4);
    let (mut journal, body, _written) = one_record(align);
    for slot in journal.iter_mut().skip(body).take(frame::seal_bytes(align)) {
        *slot = ERASED_BYTE;
    }
    let mut scan = Scan::new(&journal, align);
    assert_eq!(scan.next(), Some(Err(DecodeError::Unsealed)));
    assert_eq!(
        scan.offset(),
        0,
        "history ends before the frame that was never committed"
    );
    assert_eq!(scan.next(), None, "the scan is fused");
}

#[test]
fn a_scan_refuses_a_frame_sealed_for_a_different_frame() {
    let align = align(4);
    let (mut journal, body, _written) = one_record(align);
    // The seal a writer would have computed for some other record — the shape of a writer
    // that seals what it *meant* to write rather than what landed.
    let elsewhere = frame::commit_seal(0xDEAD_BEEF);
    for (at, slot) in journal
        .iter_mut()
        .skip(body)
        .take(frame::seal_bytes(align))
        .enumerate()
    {
        *slot = elsewhere[at % SEAL_PATTERN_BYTES];
    }
    let mut scan = Scan::new(&journal, align);
    assert_eq!(scan.next(), Some(Err(DecodeError::Unsealed)));
}

#[test]
fn a_journal_with_no_room_for_a_seal_is_a_truncation() {
    let align = align(4);
    let (journal, _body, written) = one_record(align);
    // Everything but the last byte of the seal. The frame body is whole and its seal is
    // not there to be read, which is a journal shorter than the record it appears to hold.
    let short = &journal[..written - 1];
    let mut scan = Scan::new(short, align);
    assert_eq!(scan.next(), Some(Err(DecodeError::Truncated)));
}
