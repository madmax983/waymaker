//! The two-bank layout, the generation seal, and the selection rule.
//!
//! Design document §10 Two-bank lifecycle and issue
//! [#22](https://github.com/madmax983/waymaker/issues/22). Three things are held here, and
//! they fail in different ways:
//!
//! * the **layout** is derived from a [`Geometry`] rather than written down, so a device
//!   with three erase blocks and a device with two hundred both get two banks;
//! * the **header** is a self-delimiting, twice-checksummed frame, so a reader knows where
//!   the journal begins before it trusts a length it read out of media;
//! * the **selection** is a total function of two optional generations, so "highest valid
//!   seal wins" is a thing that can be enumerated rather than a sentence.
//!
//! Golden bytes are pinned here rather than only round-tripped. A round trip agrees with
//! itself whatever the layout is; the arrays below are what a device already in the field
//! has on it.

use waymaker_core::RecordRef;
use waymaker_core::{DecodeError, RunId};
use waymaker_flash::bank::{
    self, Authority, BankHeader, BankId, BankLayout, Generation, LayoutError, Seal,
};
use waymaker_flash::frame::{self, ERASED_BYTE, FORMAT_VERSION, ProgramAlign, Scan};
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};
use waymaker_flash::storage::Geometry;

/// The run every header below belongs to.
const RUN: RunId = RunId(0x0123_4567_89AB_CDEF);

fn align(bytes: u16) -> ProgramAlign {
    let Some(align) = ProgramAlign::new(bytes) else {
        unreachable!("{bytes} is a power of two within the program-size range")
    };
    align
}

fn geometry(capacity: u32, erase: u32, program: u32, read: u32) -> Geometry {
    let Ok(geometry) = Geometry::new(capacity, erase, program, read) else {
        unreachable!("{capacity}/{erase}/{program}/{read} describes a device")
    };
    geometry
}

fn layout(capacity: u32, erase: u32, program: u32, read: u32) -> BankLayout {
    let Ok(layout) = BankLayout::new(geometry(capacity, erase, program, read)) else {
        unreachable!("{capacity}/{erase} is at least two erase blocks of two banks")
    };
    layout
}

/// [`bank::SEAL_BYTES`] as a `u32`, for comparisons against a region's offsets.
fn seal_width() -> u32 {
    let Ok(width) = u32::try_from(bank::SEAL_BYTES) else {
        unreachable!("twelve fits a u32")
    };
    width
}

fn header(input: &[u8]) -> BankHeader<'_> {
    BankHeader {
        run: RUN,
        align: align(4),
        workflow_kind: 0x0807,
        workflow_version: 0x0A09,
        input_schema: 0x0C0B,
        input,
    }
}

// ---------------------------------------------------------------------------------------
// The layout is derived from the geometry
// ---------------------------------------------------------------------------------------

#[test]
fn two_banks_of_one_erase_block_each_is_the_typical_device() {
    // §04: "Two erase blocks minimum. Typically 2 x 4 KiB, but entirely geometry-dependent."
    let layout = layout(8192, 4096, 256, 1);
    assert_eq!(layout.bank_bytes(), 4096);

    let a = layout.bank(BankId::A);
    let b = layout.bank(BankId::B);
    assert_eq!((a.base(), a.bytes()), (0, 4096));
    assert_eq!((b.base(), b.bytes()), (4096, 4096));
    // The banks do not overlap, and the seal of each sits inside its own bank.
    assert_eq!(a.base() + a.bytes(), b.base());
    assert!(a.seal_offset() >= a.base());
    assert!(a.seal_offset() + a.seal_bytes() <= a.base() + a.bytes());
    assert!(b.seal_offset() >= b.base());
    assert!(b.seal_offset() + b.seal_bytes() <= b.base() + b.bytes());
}

#[test]
fn the_seal_is_a_whole_number_of_program_units_at_the_end_of_its_bank() {
    // Programming below a program unit is not a thing a device does, so a seal that is not
    // unit-aligned is a seal that cannot be written. It is at the *end* of the bank so that
    // its offset is a function of the geometry alone — a reader finds it before it has
    // decoded anything, and the header may grow with the run input without it moving.
    //
    // Deliberately not "so that a front-to-back erase clears it first". ADR 0017 rejects that
    // reasoning: nothing in §12 says which direction a driver erases in, so the property that
    // carries the crash windows is the seal's header digest, not its position.
    for program in [1_u32, 2, 4, 16, 256] {
        let layout = layout(8192, 4096, program, 1);
        for id in BankId::ALL {
            let region = layout.bank(id);
            assert_eq!(region.seal_bytes() % program, 0, "program unit {program}");
            assert!(region.seal_bytes() >= seal_width());
            assert!(region.seal_bytes() < seal_width() + program);
            assert_eq!(region.seal_offset() % program, 0);
            assert_eq!(
                region.seal_offset() + region.seal_bytes(),
                region.base() + region.bytes()
            );
            assert_eq!(
                region.payload_bytes(),
                region.bytes() - region.seal_bytes(),
                "the payload is everything the seal does not occupy"
            );
        }
    }
}

#[test]
fn an_odd_number_of_erase_blocks_still_gives_two_equal_banks() {
    // A three-block device is an ordinary thing. Two equal banks of one block each is the
    // answer; the third block is not addressed by the layout, and saying so is the point of
    // this test — a layout that silently gave one bank two blocks would make a swap between
    // them impossible.
    let three = layout(3 * 4096, 4096, 4, 1);
    assert_eq!(three.bank_bytes(), 4096);
    assert_eq!(three.bank(BankId::A).base(), 0);
    assert_eq!(three.bank(BankId::B).base(), 4096);
    assert!(three.bank(BankId::B).base() + three.bank_bytes() <= three.geometry().capacity());

    let five = layout(5 * 1024, 1024, 4, 1);
    assert_eq!(five.bank_bytes(), 2048);
    assert_eq!(five.bank(BankId::B).base(), 2048);
}

#[test]
fn a_device_of_one_erase_block_cannot_hold_two_banks() {
    assert_eq!(
        BankLayout::new(geometry(4096, 4096, 4, 1)),
        Err(LayoutError::TooFewEraseBlocks)
    );
    assert!(!LayoutError::TooFewEraseBlocks.message().is_empty());
}

#[test]
fn a_bank_too_small_for_a_header_a_seal_and_a_record_is_refused() {
    // The other end of the same rule: two erase blocks is not enough when a program unit is
    // most of one. A layout that reported these banks would hand a writer offsets that
    // overlap.
    assert_eq!(
        BankLayout::new(geometry(2 * 32, 32, 32, 1)),
        Err(LayoutError::BankTooSmall)
    );

    // A bank that clears the header and the seal and has nothing left. Everything is padded
    // to the program unit, so a 26-byte header occupies 32 and the journal is zero bytes
    // long — a bank on which no record can ever be written, which an earlier version of this
    // guard reported as the smallest device that works.
    assert_eq!(
        BankLayout::new(geometry(2 * 64, 64, 32, 1)),
        Err(LayoutError::BankTooSmall)
    );

    // The arm that is not a subtraction. Two 8-byte erase blocks with an 8-byte program unit
    // gives an 8-byte bank and a 16-byte seal, so `bank_bytes - seal_bytes` wraps — and a
    // release build has no overflow checks, so it would wrap into an enormous, legal-looking
    // layout rather than panicking.
    assert_eq!(
        BankLayout::new(geometry(16, 8, 8, 1)),
        Err(LayoutError::BankTooSmall)
    );

    // Three blocks per bank used to be the smallest device that worked, and it is not: since
    // issue #24 a record is its frame body *and* a commit seal one program unit wide, so a
    // bank whose journal is one program unit long can hold a frame that can never be
    // committed. Review of issue #25 found the guard reserving a frame body and calling it a
    // record.
    assert_eq!(
        BankLayout::new(geometry(6 * 32, 32, 32, 1)),
        Err(LayoutError::BankTooSmall)
    );

    // Four blocks per bank is: one for the header, two for a record and its seal, one for the
    // generation seal.
    let smallest = layout(8 * 32, 32, 32, 1);
    assert_eq!(smallest.bank_bytes(), 128);
    assert_eq!(smallest.bank(BankId::A).payload_bytes(), 96);
    let header = BankHeader {
        align: align(32),
        ..header(b"")
    };
    let journal = header.journal_offset().expect("it rounds");
    assert_eq!(journal, 32, "the header occupies one whole program unit");
    let record = waymaker_flash::frame::encoded_len_for(0, align(32)).expect("it encodes");
    assert_eq!(
        record, 64,
        "a frame body and its commit seal, one unit each"
    );
    assert!(
        smallest.bank(BankId::A).payload_bytes() as usize - journal >= record,
        "the smallest accepted bank must still hold one committed record after its header"
    );
}

#[test]
fn the_layout_error_messages_say_different_things() {
    // Asserting each is non-empty is vacuous for a `&'static str` literal. What can actually
    // regress is two arms returning the same text, which makes a diagnosis useless without
    // making anything red.
    let messages = [
        LayoutError::TooFewEraseBlocks.message(),
        LayoutError::BankTooSmall.message(),
        LayoutError::ProgramUnitTooLarge.message(),
    ];
    for (index, message) in messages.iter().enumerate() {
        assert!(!message.is_empty());
        for other in messages.iter().skip(index + 1) {
            assert_ne!(message, other);
        }
    }
    // And `Display` really is the message, rather than a derive nobody reads.
    assert_eq!(
        format!("{}", LayoutError::BankTooSmall),
        LayoutError::BankTooSmall.message()
    );
}

#[test]
fn a_layout_hands_back_the_granularity_its_device_programs_at() {
    // The conversion every caller would otherwise write for itself, with an arm it can never
    // reach. `new` has already proved the narrowing succeeds.
    for program in [1_u32, 2, 4, 16, 256, 4096, 32_768] {
        let erase = program.max(65_536);
        let layout = layout(erase * 6, erase, program, 1);
        assert_eq!(u32::from(layout.align().get()), program);
        assert_eq!(
            u32::from(layout.align().get()),
            layout.geometry().program_size(),
            "the layout's granularity is its device's, not one it chose"
        );
    }
}

#[test]
fn a_device_that_programs_in_units_no_header_can_record_is_refused() {
    // `Geometry` describes a program unit with a `u32` and `ProgramAlign` with a `u16`, so
    // the two disagree above 32 KiB. A layout that accepted the difference would hand a
    // writer offsets it could only record at a *smaller* granularity than it programs at,
    // and a reader striding at that number lands inside a frame's padding and calls it the
    // end of history.
    assert_eq!(
        BankLayout::new(geometry(1024 * 1024, 256 * 1024, 64 * 1024, 1)),
        Err(LayoutError::ProgramUnitTooLarge)
    );
    assert!(!LayoutError::ProgramUnitTooLarge.message().is_empty());

    // The largest unit that *is* recordable is accepted, and round-trips through a header.
    let largest = layout(4 * 65_536, 65_536, 32_768, 1);
    assert_eq!(largest.bank_bytes(), 2 * 65_536);
    assert_eq!(largest.bank(BankId::A).seal_bytes(), 32_768);

    let mut media = vec![0_u8; 65_536];
    let written = bank::encode_header(
        &BankHeader {
            align: align(32_768),
            ..header(b"at the ceiling")
        },
        &mut media,
    )
    .expect("it fits");
    assert_eq!(written, 32_768);
    let decoded = bank::decode_header(&media).expect("what was just written");
    assert_eq!(decoded.align, align(32_768));
    assert_eq!(decoded.journal_offset(), Some(32_768));
}

#[test]
fn every_geometry_a_layout_accepts_has_a_recordable_program_unit() {
    // The invariant the refusal above establishes, swept rather than sampled: over every
    // power-of-two program unit a `Geometry` admits, a layout either refuses the device or
    // its program unit is one a header can record.
    let mut accepted = 0_usize;
    let mut refused = 0_usize;
    for shift in 0..=20_u32 {
        let program = 1_u32 << shift;
        let erase = program.max(65_536);
        let Ok(geometry) = Geometry::new(erase * 4, erase, program, 1) else {
            continue;
        };
        match BankLayout::new(geometry) {
            Ok(layout) => {
                accepted += 1;
                assert!(
                    u16::try_from(layout.geometry().program_size())
                        .ok()
                        .and_then(ProgramAlign::new)
                        .is_some(),
                    "a layout accepted a {program}-byte program unit no header can record"
                );
            }
            Err(LayoutError::ProgramUnitTooLarge) => {
                refused += 1;
                assert!(program > 32_768, "{program} is recordable and was refused");
            }
            Err(other) => unreachable!("{other} for a {program}-byte program unit"),
        }
    }
    assert!(
        accepted > 0 && refused > 0,
        "{accepted} accepted and {refused} refused, so the sweep saw only one side"
    );
}

#[test]
fn a_bank_bounds_the_run_input_its_header_may_carry() {
    // `MAX_RUN_INPUT_BYTES` is what the length field can describe; this is what the bank can
    // hold, and on every device §04 targets it is the smaller of the two by orders of
    // magnitude.
    let layout = layout(8192, 4096, 256, 1);
    let region = layout.bank(BankId::A);
    let ceiling = region.max_run_input_bytes(layout.align());
    assert!(ceiling < bank::MAX_RUN_INPUT_BYTES);

    // An input at the ceiling fits the bank *and* leaves a whole committed record — a frame
    // body and its commit seal — of journal behind it.
    let record = waymaker_flash::frame::encoded_len_for(0, layout.align()).expect("it encodes");
    let mut image = vec![ERASED_BYTE; region.payload_bytes() as usize];
    let input = vec![0x5A_u8; ceiling];
    let written = bank::encode_header(
        &BankHeader {
            align: layout.align(),
            input: &input,
            ..header(b"")
        },
        &mut image,
    )
    .expect("an input at the ceiling fits its bank");
    assert!(
        region.payload_bytes() as usize - written >= record,
        "the ceiling must leave room for a committed record"
    );

    // One byte more still *fits* — it is inside the same program unit — and that is the
    // point of the ceiling being about the journal rather than about the buffer: it is the
    // largest input after which a record can still be appended. A caller that ignores it and
    // uses the whole payload gets a bank with nowhere to write history, which no length check
    // would ever have told it.
    let over = vec![0x5A_u8; ceiling + 1];
    let overrun = bank::encode_header(
        &BankHeader {
            align: layout.align(),
            input: &over,
            ..header(b"")
        },
        &mut image,
    )
    .expect("one byte past the ceiling is still inside the payload region");
    assert!(
        region.payload_bytes() as usize - overrun < record,
        "past the ceiling there must be no room left for a committed record"
    );

    // And an input that genuinely cannot fit the payload region is refused by the buffer.
    let huge = vec![0x5A_u8; region.payload_bytes() as usize];
    assert_eq!(
        bank::encode_header(
            &BankHeader {
                align: layout.align(),
                input: &huge,
                ..header(b"")
            },
            &mut image
        ),
        Err(DecodeError::LengthOutOfBounds)
    );
}

#[test]
fn the_layout_reports_the_geometry_it_was_derived_from() {
    let geometry = geometry(8192, 4096, 4, 1);
    let layout = layout(8192, 4096, 4, 1);
    assert_eq!(layout.geometry(), geometry);
}

#[test]
fn a_bank_id_has_exactly_one_other() {
    assert_eq!(BankId::A.other(), BankId::B);
    assert_eq!(BankId::B.other(), BankId::A);
    assert_eq!(BankId::A.other().other(), BankId::A);
    assert_eq!(BankId::ALL.len(), bank::BANKS);
    assert_eq!(BankId::A.index(), 0);
    assert_eq!(BankId::B.index(), 1);
}

// ---------------------------------------------------------------------------------------
// The header frame
// ---------------------------------------------------------------------------------------

#[test]
fn a_header_round_trips_every_field() {
    let mut media = [0_u8; 64];
    let written = bank::encode_header(&header(b"run input"), &mut media).expect("it fits");
    assert_eq!(written % 4, 0, "the header is padded to its program unit");

    // Nothing past what it reported was touched. The storage contract's
    // `operations-act-on-what-they-name` clause, for the encoder's own buffer: a `split_at_mut`
    // dropped for a bare length check makes the padding loop fill the caller's whole buffer
    // with `0xFF` while still reporting 36 bytes — which, handed a bank, program-patterns the
    // journal region.
    assert!(
        media[written..].iter().all(|byte| *byte == 0),
        "the encoder wrote past the {written} bytes it reported"
    );

    let decoded = bank::decode_header(&media).expect("what was just written");
    assert_eq!(decoded.run, RUN);
    assert_eq!(decoded.workflow_kind, 0x0807);
    assert_eq!(decoded.workflow_version, 0x0A09);
    assert_eq!(decoded.input_schema, 0x0C0B);
    assert_eq!(decoded.input, b"run input");
    assert_eq!(decoded.align, align(4));
    assert_eq!(decoded.frame_len(), bank::HEADER_OVERHEAD_BYTES + 9);
    assert_eq!(decoded.journal_offset(), Some(written));
}

#[test]
fn an_empty_run_input_is_an_ordinary_header() {
    let mut media = [0_u8; 64];
    let written = bank::encode_header(&header(b""), &mut media).expect("it fits");
    let decoded = bank::decode_header(&media).expect("what was just written");
    assert!(decoded.input.is_empty());
    assert_eq!(decoded.frame_len(), bank::HEADER_OVERHEAD_BYTES);
    assert_eq!(
        written,
        align(4).round_up(bank::HEADER_OVERHEAD_BYTES).unwrap()
    );
    assert_eq!(decoded.journal_offset(), Some(written));
}

#[test]
fn the_header_is_the_bytes_this_test_says_it_is() {
    // Golden. A round trip agrees with itself whatever the layout is; this is what a device
    // in the field already has on it, so a field that moved fails here rather than in
    // somebody's recovery.
    let mut media = [0_u8; 32];
    let written = bank::encode_header(
        &BankHeader {
            run: RunId(0x0807_0605_0403_0201),
            align: align(4),
            workflow_kind: 0x0A09,
            workflow_version: 0x0C0B,
            input_schema: 0x0E0D,
            input: b"ab",
        },
        &mut media,
    )
    .expect("it fits");
    assert_eq!(written, 28);

    let sealed = &media[..20];
    let header_crc = Catalogued::header_check(sealed).to_le_bytes();
    let frame_crc = Catalogued::frame_check(&media[..24]).to_le_bytes();
    assert_eq!(
        &media[..28],
        &[
            0x42,
            0x4B, // magic, "BK"
            FORMAT_VERSION,
            2, // program_shift: 1 << 2 == 4
            0x01,
            0x02,
            0x03,
            0x04,
            0x05,
            0x06,
            0x07,
            0x08, // run id
            0x09,
            0x0A, // workflow kind
            0x0B,
            0x0C, // workflow version
            0x0D,
            0x0E, // input schema
            0x02,
            0x00, // input len
            header_crc[0],
            header_crc[1],
            b'a',
            b'b',
            frame_crc[0],
            frame_crc[1],
            frame_crc[2],
            frame_crc[3],
        ][..]
    );
}

#[test]
fn a_header_is_padded_with_erased_bytes() {
    // Programming `0xFF` over an erased cell changes no bits, so padding costs the device
    // nothing beyond the program cycle the alignment already required.
    let mut media = [0_u8; 64];
    let written = bank::encode_header(
        &BankHeader {
            align: align(16),
            ..header(b"x")
        },
        &mut media,
    )
    .expect("it fits");
    assert_eq!(written, 32);
    assert!(
        media[bank::HEADER_OVERHEAD_BYTES + 1..written]
            .iter()
            .all(|byte| *byte == ERASED_BYTE)
    );
}

#[test]
fn a_header_that_does_not_fit_writes_nothing() {
    let mut media = [0_u8; 27];
    assert_eq!(
        bank::encode_header(&header(b"ab"), &mut media),
        Err(DecodeError::LengthOutOfBounds)
    );
    assert!(
        media.iter().all(|byte| *byte == 0),
        "a partial header in a staging buffer is a header a later flush could program"
    );
}

#[test]
fn an_erased_bank_has_no_header() {
    let media = [ERASED_BYTE; 64];
    assert_eq!(
        bank::decode_header(&media),
        Err(DecodeError::IntegrityFailed)
    );
}

#[test]
fn a_record_frame_is_not_a_bank_header_and_a_bank_header_is_not_a_seal() {
    // What the magic is *for*, as a test rather than as a comment. Every other refusal in
    // this file also fails its checksum, so deleting both magic comparisons passes the whole
    // crate — the constants are held apart by a `const` assertion and the checks were held by
    // nothing.
    let mut record = [ERASED_BYTE; 64];
    let written = frame::encode(
        &RecordRef::RunCompleted { result: b"done" },
        align(4),
        &mut record,
    )
    .expect("it fits");
    assert!(written > 0);
    assert_eq!(
        bank::decode_header(&record),
        Err(DecodeError::IntegrityFailed),
        "a record frame decoded as a bank header is a mistake worth catching at the magic"
    );
    assert_eq!(
        bank::decode_seal(&record),
        Err(DecodeError::IntegrityFailed)
    );

    let mut bank_header = [ERASED_BYTE; 64];
    bank::encode_header(&header(b"in"), &mut bank_header).expect("it fits");
    assert_eq!(
        frame::decode(&bank_header),
        Err(DecodeError::IntegrityFailed),
        "a bank header decoded as a record frame is the same mistake the other way round"
    );
    assert_eq!(
        bank::decode_seal(&bank_header),
        Err(DecodeError::IntegrityFailed)
    );

    // And a seal is not a header.
    let mut seal = [0_u8; 12];
    bank::encode_seal(
        &Seal {
            generation: Generation::FIRST,
            header_check: 1,
        },
        align(1),
        &mut seal,
    )
    .expect("it fits");
    assert_eq!(bank::decode_header(&seal), Err(DecodeError::Truncated));
}

#[test]
fn a_truncated_header_is_truncated_rather_than_corrupt() {
    let mut media = [0_u8; 64];
    bank::encode_header(&header(b"run input"), &mut media).expect("it fits");
    for short in 0..bank::HEADER_OVERHEAD_BYTES + 9 {
        // Everything below the full frame is a truncation. The last four bytes are the
        // trailer, so cutting into them leaves a header whose length is known and whose
        // seal is not there.
        let cut = &media[..short];
        let outcome = bank::decode_header(cut);
        assert!(
            matches!(outcome, Err(DecodeError::Truncated)),
            "{short} bytes gave {outcome:?}"
        );
    }
}

#[test]
fn every_single_byte_mutation_of_a_header_is_caught() {
    let mut media = [0_u8; 64];
    let written = bank::encode_header(&header(b"input"), &mut media).expect("it fits");
    let original = bank::decode_header(&media).expect("the clean header");

    for index in 0..written {
        for bit in 0..8_u32 {
            let mut damaged = media;
            damaged[index] ^= 1 << bit;
            match bank::decode_header(&damaged) {
                Err(_) => {}
                Ok(decoded) => assert_eq!(
                    (
                        decoded.run,
                        decoded.workflow_kind,
                        decoded.workflow_version,
                        decoded.input_schema,
                        decoded.input,
                        decoded.align
                    ),
                    (
                        original.run,
                        original.workflow_kind,
                        original.workflow_version,
                        original.input_schema,
                        original.input,
                        original.align
                    ),
                    "byte {index} bit {bit} changed a header that still decoded"
                ),
            }
        }
    }
}

#[test]
fn a_header_from_another_format_version_is_refused_before_its_body_is_read() {
    let mut media = [0_u8; 64];
    bank::encode_header(&header(b"in"), &mut media).expect("it fits");
    media[2] = FORMAT_VERSION.wrapping_add(1);
    // The header checksum covers the version, so a version bump alone fails integrity — and
    // that is asserted rather than narrated. Without this line the checksum comparison could
    // move *after* the version check with nothing red, and a header whose length field was
    // then read before its seal held would be a length nobody wrote.
    assert_eq!(
        bank::decode_header(&media),
        Err(DecodeError::IntegrityFailed),
        "the header checksum must be checked before the version"
    );
    // Re-seal it and the version refusal is what is left.
    let resealed = Catalogued::header_check(&media[..20]).to_le_bytes();
    media[20] = resealed[0];
    media[21] = resealed[1];
    let frame_crc = Catalogued::frame_check(&media[..24]).to_le_bytes();
    media[24..28].copy_from_slice(&frame_crc);
    assert_eq!(
        bank::decode_header(&media),
        Err(DecodeError::UnsupportedFormatVersion)
    );
}

#[test]
fn a_program_shift_no_device_has_is_refused() {
    // `ProgramAlign` is a `u16`, so the largest granularity is `1 << 15`. A shift of 16 or
    // more describes a device that cannot exist, and a reader that accepted it would stride
    // by a number it computed by overflowing.
    let mut media = [0_u8; 64];
    bank::encode_header(&header(b"in"), &mut media).expect("it fits");
    media[3] = 16;
    let resealed = Catalogued::header_check(&media[..20]).to_le_bytes();
    media[20] = resealed[0];
    media[21] = resealed[1];
    let frame_crc = Catalogued::frame_check(&media[..24]).to_le_bytes();
    media[24..28].copy_from_slice(&frame_crc);
    assert_eq!(
        bank::decode_header(&media),
        Err(DecodeError::MalformedRecord)
    );
}

#[test]
fn a_length_that_reaches_past_the_bank_is_a_truncation_rather_than_a_read() {
    // `input_len` is read out of the bytes being validated, which is why the header
    // checksum comes first: the length is known to be the number the writer wrote before it
    // is used to say where the frame ends.
    let mut media = [0_u8; 64];
    bank::encode_header(&header(b"in"), &mut media).expect("it fits");
    media[18] = 0xFF;
    media[19] = 0xFF;
    let resealed = Catalogued::header_check(&media[..20]).to_le_bytes();
    media[20] = resealed[0];
    media[21] = resealed[1];
    assert_eq!(bank::decode_header(&media), Err(DecodeError::Truncated));
}

#[test]
fn a_run_input_of_exactly_the_length_field_round_trips() {
    // The boundary `u16::try_from` exists for. Tested only at `MAX + 1` before, so `>` could
    // become `>=` and refuse the largest legal input with nothing red.
    let input = vec![0xA5_u8; bank::MAX_RUN_INPUT_BYTES];
    let mut media = vec![0_u8; bank::MAX_RUN_INPUT_BYTES + 64];
    let written = bank::encode_header(
        &BankHeader {
            input: &input,
            ..header(b"")
        },
        &mut media,
    )
    .expect("the largest input the field can describe");
    assert_eq!(written % 4, 0);
    let decoded = bank::decode_header(&media).expect("what was just written");
    assert_eq!(decoded.input.len(), bank::MAX_RUN_INPUT_BYTES);
    assert_eq!(decoded.input, &input[..]);
}

#[test]
fn a_run_input_longer_than_the_length_field_cannot_be_encoded() {
    let input = vec![0_u8; bank::MAX_RUN_INPUT_BYTES + 1];
    let mut media = vec![0_u8; bank::MAX_RUN_INPUT_BYTES + 64];
    assert_eq!(
        bank::encode_header(
            &BankHeader {
                input: &input,
                ..header(b"")
            },
            &mut media
        ),
        Err(DecodeError::LengthOutOfBounds)
    );
}

// ---------------------------------------------------------------------------------------
// The generation seal
// ---------------------------------------------------------------------------------------

#[test]
fn a_seal_round_trips() {
    let seal = Seal {
        generation: Generation(0x0403_0201),
        header_check: 0x0807_0605,
    };
    let mut media = [0_u8; 16];
    let written = bank::encode_seal(&seal, align(4), &mut media).expect("it fits");
    assert_eq!(written, bank::SEAL_BYTES);
    assert_eq!(bank::decode_seal(&media), Ok(seal));
}

#[test]
fn a_seal_is_padded_to_its_program_unit_with_erased_bytes() {
    // The configuration §04 calls typical: a 256-byte program unit, where a bank's seal
    // region is 256 bytes of which 244 are padding. Every other seal test in this file uses
    // a 1- or 4-byte unit, where `round_up(12, unit) == 12` and this path never runs at all —
    // so the branch that executes on the representative part was the one nothing reached.
    let region = layout(8192, 4096, 256, 1).bank(BankId::A);
    assert_eq!(region.seal_bytes(), 256);

    let seal = Seal {
        generation: Generation(0x1234_5678),
        header_check: 0x9ABC_DEF0,
    };
    let mut media = [0_u8; 512];
    let written = bank::encode_seal(&seal, align(256), &mut media).expect("it fits");
    assert_eq!(written, region.seal_bytes() as usize);

    assert!(
        media[bank::SEAL_BYTES..written]
            .iter()
            .all(|byte| *byte == ERASED_BYTE),
        "padding must be erased bytes: programming 0xFF over an erased cell clears no bits, \
         and 0x00 clears every one of them"
    );
    assert!(
        media[written..].iter().all(|byte| *byte == 0),
        "the encoder wrote past the {written} bytes it reported"
    );
    // And a padded seal still reads back.
    assert_eq!(bank::decode_seal(&media), Ok(seal));
}

#[test]
fn a_seal_names_the_digest_the_header_stores_for_itself() {
    // The wire-format identity `seal_for` documents, pinned against the header's own trailer
    // rather than against another call to `seal_for`. Every other test in this file writes a
    // seal with `seal_for` and verifies it with `seal_for`, so the two cancel and the digest
    // could be anything — `C::frame_check(frame) ^ 0xA5A5_A5A5` passes them all.
    let mut media = [0_u8; 64];
    bank::encode_header(&header(b"input"), &mut media).expect("it fits");
    let frame_len = bank::HEADER_OVERHEAD_BYTES + 5;

    let stored = u32::from_le_bytes(
        media[frame_len - 4..frame_len]
            .try_into()
            .expect("four bytes"),
    );
    let seal = bank::seal_for(&media, Generation(1)).expect("the header decodes");
    assert_eq!(
        seal.header_check, stored,
        "the seal must quote the header's own frame_crc"
    );
    assert_eq!(seal.generation, Generation(1));
}

#[test]
fn the_seal_is_the_bytes_this_test_says_it_is() {
    let mut media = [0_u8; 12];
    bank::encode_seal(
        &Seal {
            generation: Generation(0x0605_0403),
            header_check: 0x0A09_0807,
        },
        align(1),
        &mut media,
    )
    .expect("it fits");
    let check = Catalogued::header_check(&media[..10]).to_le_bytes();
    assert_eq!(
        media,
        [
            0x47, 0x53, // magic, "GS"
            0x03, 0x04, 0x05, 0x06, // generation
            0x07, 0x08, 0x09, 0x0A, // the header's frame check
            check[0], check[1],
        ]
    );
}

#[test]
fn an_erased_seal_region_is_not_a_seal() {
    let media = [ERASED_BYTE; 16];
    assert_eq!(bank::decode_seal(&media), Err(DecodeError::IntegrityFailed));
    // Nor is a zeroed one: a page that was programmed to zero is not a seal either.
    assert_eq!(
        bank::decode_seal(&[0_u8; 16]),
        Err(DecodeError::IntegrityFailed)
    );
}

#[test]
fn every_single_byte_mutation_of_a_seal_is_caught() {
    let seal = Seal {
        generation: Generation(7),
        header_check: 0xDEAD_BEEF,
    };
    let mut media = [0_u8; 12];
    bank::encode_seal(&seal, align(1), &mut media).expect("it fits");
    for index in 0..bank::SEAL_BYTES {
        for bit in 0..8_u32 {
            let mut damaged = media;
            damaged[index] ^= 1 << bit;
            match bank::decode_seal(&damaged) {
                Err(_) => {}
                Ok(decoded) => assert_eq!(
                    decoded, seal,
                    "byte {index} bit {bit} changed a seal that still decoded"
                ),
            }
        }
    }
}

#[test]
fn a_seal_shorter_than_its_own_width_is_truncated() {
    let mut media = [0_u8; 12];
    bank::encode_seal(
        &Seal {
            generation: Generation::FIRST,
            header_check: 1,
        },
        align(1),
        &mut media,
    )
    .expect("it fits");
    for short in 0..bank::SEAL_BYTES {
        assert_eq!(
            bank::decode_seal(&media[..short]),
            Err(DecodeError::Truncated)
        );
    }
}

#[test]
fn a_seal_that_does_not_fit_writes_nothing() {
    let mut media = [0_u8; 11];
    assert_eq!(
        bank::encode_seal(
            &Seal {
                generation: Generation::FIRST,
                header_check: 1
            },
            align(1),
            &mut media
        ),
        Err(DecodeError::LengthOutOfBounds)
    );
    assert!(media.iter().all(|byte| *byte == 0));
}

// ---------------------------------------------------------------------------------------
// What makes a bank a candidate
// ---------------------------------------------------------------------------------------

/// A whole bank: the header at the front, the seal at the back.
fn sealed_bank(input: &[u8], generation: Generation) -> ([u8; 64], [u8; 12]) {
    let mut header_bytes = [ERASED_BYTE; 64];
    let Ok(_written) = bank::encode_header(&header(input), &mut header_bytes) else {
        unreachable!("a bank header of this shape fits 64 bytes")
    };
    let Ok(seal) = bank::seal_for(&header_bytes, generation) else {
        unreachable!("the header this helper just encoded decodes")
    };
    let mut seal_bytes = [ERASED_BYTE; 12];
    let Ok(_sealed) = bank::encode_seal(&seal, align(1), &mut seal_bytes) else {
        unreachable!("a seal fits twelve bytes at a one-byte program unit")
    };
    (header_bytes, seal_bytes)
}

#[test]
fn a_bank_whose_header_and_seal_agree_is_a_candidate() {
    let (header_bytes, seal_bytes) = sealed_bank(b"input", Generation(41));
    assert_eq!(
        bank::sealed_generation(&header_bytes, &seal_bytes),
        Some(Generation(41))
    );
}

#[test]
fn a_seal_that_does_not_name_this_header_is_not_a_candidate_at_any_generation() {
    // The stale-tail hazard, structurally. A seal that survived an erase which took its
    // header with it names a digest nothing on media computes to, so the bank has no
    // generation — whichever way round the driver erases.
    let (header_bytes, seal_bytes) = sealed_bank(b"input", Generation(41));
    let (other_header, _) = sealed_bank(b"a different run", Generation(41));
    assert_eq!(bank::sealed_generation(&other_header, &seal_bytes), None);

    let erased_header = [ERASED_BYTE; 64];
    assert_eq!(bank::sealed_generation(&erased_header, &seal_bytes), None);

    let erased_seal = [ERASED_BYTE; 12];
    assert_eq!(bank::sealed_generation(&header_bytes, &erased_seal), None);
}

#[test]
fn a_torn_seal_is_not_a_candidate() {
    let (header_bytes, seal_bytes) = sealed_bank(b"input", Generation(41));
    for cut in 0..bank::SEAL_BYTES {
        let mut torn = seal_bytes;
        torn[cut..].fill(ERASED_BYTE);
        if torn == seal_bytes {
            // The suffix was already erased, so nothing was torn and there is nothing to
            // assert about. Skipped rather than asserted, so the test cannot pass by having
            // changed no bytes.
            continue;
        }
        assert_eq!(
            bank::sealed_generation(&header_bytes, &torn),
            None,
            "a seal torn at byte {cut} was accepted"
        );
    }
}

#[test]
fn a_whole_bank_round_trips_through_the_layout_the_header_and_the_scan() {
    // The path §10 actually describes, end to end, with nothing hand-computed: a layout says
    // where the bank is, a header says where its journal starts *and at what granularity*,
    // the real `Scan` walks it, and the seal makes it authoritative.
    //
    // Written because `program_shift` had no reader. ADR 0017 puts the writer's program unit
    // on media so "the stride a reader uses is a fact it read rather than one it assumed",
    // and until this test nothing in the workspace read it and strode with it — the claim had
    // no consumer, which is a claim held by nothing.
    let layout = layout(8192, 4096, 16, 1);
    let region = layout.bank(BankId::B);
    let mut image = vec![ERASED_BYTE; layout.geometry().capacity() as usize];

    let head = BankHeader {
        align: layout.align(),
        ..header(b"the run input")
    };
    assert!(head.input.len() <= region.max_run_input_bytes(layout.align()));

    let base = region.base() as usize;
    let payload_end = base + region.payload_bytes() as usize;
    let written = bank::encode_header(&head, &mut image[base..payload_end]).expect("it fits");

    // Append two records at the offset the header says the journal starts at, striding at the
    // granularity the header recorded.
    let records = [
        RecordRef::RunStarted {
            workflow_kind: 7,
            workflow_version: 1,
            input: b"the run input",
        },
        RecordRef::RunCompleted { result: b"done" },
    ];
    let mut at = base + written;
    for record in &records {
        let wrote = frame::encode(record, layout.align(), &mut image[at..payload_end])
            .expect("the journal has room");
        at += wrote;
    }

    let seal_at = region.seal_offset() as usize;
    let seal_end = seal_at + region.seal_bytes() as usize;
    let seal = bank::seal_for(&image[base..payload_end], Generation(3)).expect("it decodes");
    bank::encode_seal(&seal, layout.align(), &mut image[seal_at..seal_end]).expect("it fits");

    // Now recover it the way a cold boot would, taking every number off media.
    assert_eq!(
        bank::select(BankId::ALL.map(|id| {
            let region = layout.bank(id);
            let base = region.base() as usize;
            bank::sealed_generation(
                &image[base..base + region.payload_bytes() as usize],
                &image[region.seal_offset() as usize
                    ..region.seal_offset() as usize + region.seal_bytes() as usize],
            )
        })),
        Authority::Bank {
            id: BankId::B,
            generation: Generation(3)
        }
    );

    let decoded = bank::decode_header(&image[base..payload_end]).expect("the header");
    let journal_at = decoded.journal_offset().expect("it rounds");
    assert_eq!(journal_at, written);
    assert!(journal_at <= region.payload_bytes() as usize);

    // The stride is the one the header recorded, not one this test chose.
    let walked: Vec<RecordRef<'_>> =
        Scan::new(&image[base + journal_at..payload_end], decoded.align)
            .map(|step| step.expect("every frame this test wrote verifies"))
            .collect();
    assert_eq!(walked, records);
}

// ---------------------------------------------------------------------------------------
// The selection rule
// ---------------------------------------------------------------------------------------

#[test]
fn the_highest_valid_seal_wins() {
    assert_eq!(
        bank::select([Some(Generation(41)), Some(Generation(42))]),
        Authority::Bank {
            id: BankId::B,
            generation: Generation(42)
        }
    );
    assert_eq!(
        bank::select([Some(Generation(42)), Some(Generation(41))]),
        Authority::Bank {
            id: BankId::A,
            generation: Generation(42)
        }
    );
}

#[test]
fn a_bank_whose_seal_fails_validation_is_not_a_candidate_at_any_generation() {
    // Issue #22, in as many words. An invalid seal does not lose to the other bank — it is
    // not in the comparison at all, so the other bank wins however low its generation is.
    assert_eq!(
        bank::select([None, Some(Generation::FIRST)]),
        Authority::Bank {
            id: BankId::B,
            generation: Generation::FIRST
        }
    );
    assert_eq!(
        bank::select([Some(Generation::MAX), None]),
        Authority::Bank {
            id: BankId::A,
            generation: Generation::MAX
        }
    );
}

#[test]
fn a_device_with_no_valid_seal_has_nothing_to_boot_from() {
    assert_eq!(bank::select([None, None]), Authority::Unsealed);
}

#[test]
fn two_banks_at_one_generation_are_ambiguous_rather_than_resolved() {
    // A tie is the state no protocol may reach, so a selection that resolved it would hide
    // the bug the count exists to find.
    assert_eq!(
        bank::select([Some(Generation(9)), Some(Generation(9))]),
        Authority::Ambiguous {
            generation: Generation(9)
        }
    );
}

#[test]
fn the_generation_ceiling_is_explicit_rather_than_a_wraparound() {
    // Issue #22: "Generation wraparound is handled explicitly rather than by unsigned
    // comparison luck." It is handled by making it unreachable: a writer cannot mint a
    // generation after `MAX`, so the `u32` order *is* the order of the swaps and there is
    // no wrap for a comparison to get wrong.
    assert_eq!(Generation::FIRST.successor(), Some(Generation(1)));
    assert_eq!(Generation(41).successor(), Some(Generation(42)));
    assert_eq!(Generation::MAX.successor(), None);
    assert_eq!(Generation::MAX, Generation(u32::MAX));
    assert_eq!(Generation::FIRST, Generation(0));

    // And the comparison that would have been wrong if it had wrapped: generation zero
    // against the ceiling. Zero can only ever precede the ceiling, never follow it.
    assert_eq!(
        bank::select([Some(Generation::FIRST), Some(Generation::MAX)]),
        Authority::Bank {
            id: BankId::B,
            generation: Generation::MAX
        }
    );
    assert!(Generation::FIRST < Generation::MAX);
}

#[test]
fn selection_is_total_over_a_bounded_sweep_of_generations() {
    // Every pair, so the rule is enumerated rather than sampled at the interesting points.
    let candidates = [
        None,
        Some(Generation::FIRST),
        Some(Generation(1)),
        Some(Generation(0x8000_0000)),
        Some(Generation::MAX),
    ];
    for a in candidates {
        for b in candidates {
            let authority = bank::select([a, b]);
            match (a, b) {
                (None, None) => assert_eq!(authority, Authority::Unsealed),
                (Some(g), None) => assert_eq!(
                    authority,
                    Authority::Bank {
                        id: BankId::A,
                        generation: g
                    }
                ),
                (None, Some(g)) => assert_eq!(
                    authority,
                    Authority::Bank {
                        id: BankId::B,
                        generation: g
                    }
                ),
                (Some(x), Some(y)) if x == y => {
                    assert_eq!(authority, Authority::Ambiguous { generation: x });
                }
                (Some(x), Some(y)) => assert_eq!(
                    authority,
                    Authority::Bank {
                        id: if x > y { BankId::A } else { BankId::B },
                        generation: if x > y { x } else { y }
                    }
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// The integrity check is a parameter, not a fact
// ---------------------------------------------------------------------------------------

/// An integrity check that is not the shipped one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Other;

impl IntegrityCheck for Other {
    fn header_check(bytes: &[u8]) -> u16 {
        !Catalogued::header_check(bytes)
    }

    fn frame_check(bytes: &[u8]) -> u32 {
        !Catalogued::frame_check(bytes)
    }
}

#[test]
fn a_header_sealed_with_one_check_is_refused_by_another() {
    let mut media = [0_u8; 64];
    bank::encode_header_with::<Other>(&header(b"in"), &mut media).expect("it fits");
    assert_eq!(
        bank::decode_header(&media),
        Err(DecodeError::IntegrityFailed)
    );
    assert!(bank::decode_header_with::<Other>(&media).is_ok());
}

#[test]
fn a_seal_sealed_with_one_check_is_refused_by_another() {
    let seal = Seal {
        generation: Generation(3),
        header_check: 5,
    };
    let mut media = [0_u8; 12];
    bank::encode_seal_with::<Other>(&seal, align(1), &mut media).expect("it fits");
    assert_eq!(bank::decode_seal(&media), Err(DecodeError::IntegrityFailed));
    assert_eq!(bank::decode_seal_with::<Other>(&media), Ok(seal));
}

#[test]
fn a_bank_written_under_one_check_is_no_candidate_under_another() {
    let mut header_bytes = [ERASED_BYTE; 64];
    bank::encode_header_with::<Other>(&header(b"in"), &mut header_bytes).expect("it fits");
    let seal =
        bank::seal_for_with::<Other>(&header_bytes, Generation(4)).expect("what was written");
    let mut seal_bytes = [ERASED_BYTE; 12];
    bank::encode_seal_with::<Other>(&seal, align(1), &mut seal_bytes).expect("it fits");

    assert_eq!(
        bank::sealed_generation_with::<Other>(&header_bytes, &seal_bytes),
        Some(Generation(4))
    );
    assert_eq!(bank::sealed_generation(&header_bytes, &seal_bytes), None);
}
