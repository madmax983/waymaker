//! The integrity check, tested as the thing issue #17 asks it to be.
//!
//! Design document §16's first deferred question — "whether the default integrity check is
//! CRC32C or a smaller table-free CRC implementation" — is settled by
//! [ADR 0010](../../../docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md)
//! with measurements taken on `thumbv6m-none-eabi`. Issue
//! [#17](https://github.com/madmax983/waymaker/issues/17) asks for three things the ADR's
//! prose cannot supply on its own, and this file is those three.
//!
//! * **The rejected candidate, implemented rather than described.** ADR 0010 rejects
//!   CRC-32C on the strength of a measurement; a rejection with no runnable candidate
//!   behind it is an assertion. [`Crc32c`] is that candidate, anchored to its published
//!   check value so it is the real algorithm and not a plausible loop — and both
//!   *table-driven* forms are here too, [`crc32c_nibble`] and [`crc32c_byte`], because the
//!   64 B and 1024 B of `.rodata` issue #17 asks to be measured are properties of those two
//!   and of nothing else.
//! * **The failure modes confirmed.** §09 says what the checksum is for — "CRC detects
//!   accidental corruption and torn writes" — and the two that matter on NOR are a write
//!   torn at a program-unit boundary and a stale erased tail. Both are swept here over
//!   every alignment, every boundary, and every record variant.
//! * **The swap point exercised.** The choice is meant to stay swappable, so an
//!   implementation that is not the shipped one is driven through the entire codec:
//!   encode, decode, and the append scan.
//!
//! # What a CRC is not
//!
//! Authentication. Issue #17 puts it out of scope in those words and §09 agrees, so
//! [`a_resealed_forgery_is_accepted_because_a_crc_is_not_authentication`] states it as a
//! passing test rather than as a sentence a reader has to take on trust: a forged frame
//! resealed with the right algorithm decodes, and nothing here pretends otherwise.

use waymaker_core::{ActivityKind, DecodeError, EffectSeq, RecordRef};
use waymaker_flash::frame::{
    self, Decoded, ERASED_BYTE, FRAME_CRC_BYTES, FRAME_OVERHEAD_BYTES, HEADER_BYTES,
    HEADER_CRC_BYTES, ProgramAlign, Scan, TRAILER_BYTES,
};
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};

/// The alignments the sweeps below run at: one that pads nothing, the program sizes real
/// internal flash reports, and one page size.
const ALIGNMENTS: [u16; 6] = [1, 2, 4, 8, 16, 256];

/// Room for the longest frame these tests build, padded to the largest alignment above.
///
/// The largest sample is a 600-byte payload, so 616 bytes of frame and 768 once padded to
/// 256.
const SCRATCH: usize = 2_048;

/// The catalogue's check input: every CRC specification states its result for this.
const CHECK: &[u8] = b"123456789";

/// The candidate ADR 0010 rejected, implemented so that the rejection is a comparison.
///
/// CRC-32C (Castagnoli) over the frame and CRC-16/ARC over the header. Both differ from the
/// shipped pair, which is what makes this type useful twice over: it is §16's other
/// candidate, and it is a second implementation of [`IntegrityCheck`] that proves the swap
/// point is a real one rather than a trait nothing but the default has ever satisfied.
///
/// Both hooks are changed on purpose. An alternative that kept the shipped header check
/// would leave [`the_header_seal_is_the_header_check_and_the_frame_seal_is_the_frame_check`]
/// unable to tell whether the codec consults `header_check` at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Crc32c;

impl IntegrityCheck for Crc32c {
    fn header_check(bytes: &[u8]) -> u16 {
        crc16_arc(bytes)
    }

    fn frame_check(bytes: &[u8]) -> u32 {
        crc32c(bytes)
    }
}

/// CRC-32C (Castagnoli): reflected polynomial `0x82F6_3B78`, initial value and final xor
/// `0xFFFF_FFFF`. Published check value `0xE306_9283`.
const fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    let mut rest = bytes;
    while let Some((byte, tail)) = rest.split_first() {
        crc ^= *byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0x82F6_3B78
            };
            bit += 1;
        }
        rest = tail;
    }
    crc ^ 0xFFFF_FFFF
}

/// CRC-32C folded four bits at a time against a sixteen-entry table.
///
/// One of the two table-driven candidates ADR 0010 measured — 52 B of `.text` and **64 B of
/// `.rodata`**, 21 cycles per byte against the bitwise loop's 93. Present so that the
/// `.rodata` figure issue #17 asks for is a number about code in this repository rather than
/// a number in a paragraph, and so that ADR 0010's central claim — "a table is an
/// implementation of an algorithm and not a different one" — is checked by
/// [`every_candidate_computes_the_same_crc32c`] rather than asserted.
fn crc32c_nibble(bytes: &[u8]) -> u32 {
    // Generated from the bitwise loop rather than pasted, so the table cannot disagree with
    // the polynomial it is meant to be a folding of.
    let mut table = [0_u32; 16];
    for (index, slot) in table.iter_mut().enumerate() {
        let mut crc = u32::try_from(index).unwrap_or(0);
        for _ in 0..4 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0x82F6_3B78
            };
        }
        *slot = crc;
    }

    let mut crc: u32 = 0xFFFF_FFFF;
    for byte in bytes {
        for nibble in [u32::from(*byte) & 0xF, u32::from(*byte) >> 4] {
            let index = usize::try_from((crc ^ nibble) & 0xF).unwrap_or(0);
            // `get` rather than an index: a helper in an integration test is not exempt
            // from the workspace's `indexing_slicing` denial, and a fold that silently used
            // zero on an out-of-range index would be a candidate that agreed with nothing.
            crc = (crc >> 4) ^ table.get(index).copied().unwrap_or(0);
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// CRC-32C folded a byte at a time against a 256-entry table.
///
/// The other table-driven candidate: 44 B of `.text` and **1024 B of `.rodata`** — 12.5% of
/// design document §04's entire 8 KiB incremental code-flash budget — for 15 cycles per
/// byte. That ratio is the whole of ADR 0010's decision, and this is the implementation the
/// number was taken from.
fn crc32c_byte(bytes: &[u8]) -> u32 {
    let mut table = [0_u32; 256];
    for (index, slot) in table.iter_mut().enumerate() {
        let mut crc = u32::try_from(index).unwrap_or(0);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0x82F6_3B78
            };
        }
        *slot = crc;
    }

    let mut crc: u32 = 0xFFFF_FFFF;
    for byte in bytes {
        let index = usize::try_from((crc ^ u32::from(*byte)) & 0xFF).unwrap_or(0);
        crc = (crc >> 8) ^ table.get(index).copied().unwrap_or(0);
    }
    crc ^ 0xFFFF_FFFF
}

/// CRC-16/ARC: reflected polynomial `0xA001`, initial value zero, no final xor. Published
/// check value `0xBB3D`.
const fn crc16_arc(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    let mut rest = bytes;
    while let Some((byte, tail)) = rest.split_first() {
        crc ^= *byte as u16;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xA001
            };
            bit += 1;
        }
        rest = tail;
    }
    crc
}

/// The records the sweeps run over: one per [`RecordRef`] variant, plus one long enough to
/// span several program units at every alignment.
///
/// A fixed list rather than a generator: these tests are about byte-level damage, and a
/// failure that names a record by index is one a reader can rebuild by hand.
const fn samples() -> [RecordRef<'static>; 7] {
    [
        RecordRef::RunStarted {
            workflow_kind: 0xBEEF,
            workflow_version: 7,
            input: b"hi",
        },
        RecordRef::EffectScheduled {
            seq: EffectSeq(0x0102_0304),
            kind: ActivityKind(0x1234),
            input_len: 0x40,
            input_crc: 0xDEAD_BEEF,
        },
        RecordRef::EffectCompleted {
            seq: EffectSeq(9),
            result: &[1, 2, 3],
        },
        RecordRef::EffectFailed {
            seq: EffectSeq(0xFFFF_FFFE),
            error: &[0x00, 0xFF, 0x80],
        },
        RecordRef::RunCompleted { result: &[] },
        RecordRef::RunFailed { error: b"why" },
        // A record longer than one program unit at every alignment swept. Without it the
        // torn-write sweep was vacuous at its two largest alignments: every other sample is
        // 16-24 B, so at 16 and at 256 there is no interior boundary to tear at and the
        // loop ran once, at `cut = 0`, which is an erased page rather than a tear.
        RecordRef::EffectCompleted {
            seq: EffectSeq(11),
            result: &[0xA5; 600],
        },
    ]
}

/// An alignment, or the one that pads nothing when `bytes` is not a power of two.
///
/// A helper rather than an `unwrap`: `clippy.toml` exempts test *bodies* from the
/// workspace's `unwrap` denial and does not exempt a helper in an integration test.
const fn align_or_byte(bytes: u16) -> ProgramAlign {
    // `match` rather than `unwrap_or`: `Option::unwrap_or` is not a `const fn`, and a
    // helper in an integration test is not exempt from the workspace's `unwrap` denial.
    match ProgramAlign::new(bytes) {
        Some(align) => align,
        None => ProgramAlign::BYTE,
    }
}

/// The payload length a record encodes to, worked out from the record rather than from the
/// codec.
///
/// §09's field list, transcribed: four bytes of workflow identity plus the input for a
/// `RunStarted`, eight fixed bytes for an `EffectScheduled`, and the caller's bytes alone
/// for the other four. Written out here so that a test asking "where does the frame end and
/// the padding begin" is not asking the code under test.
const fn payload_len(record: &RecordRef<'_>) -> usize {
    match *record {
        RecordRef::RunStarted { input, .. } => 4 + input.len(),
        RecordRef::EffectScheduled { .. } => 8,
        RecordRef::EffectCompleted { result, .. } | RecordRef::RunCompleted { result } => {
            result.len()
        }
        RecordRef::EffectFailed { error, .. } | RecordRef::RunFailed { error } => error.len(),
    }
}

/// Whether `bytes` begins with a frame the codec will hand back as a record.
///
/// Total: a short buffer, a failed checksum and an unknown kind are all `false`, so a
/// caller asserts on the one thing these sweeps care about — whether damaged media
/// produced a record.
fn reads_as_a_record(bytes: &[u8]) -> bool {
    matches!(
        frame::decode(bytes),
        Ok(frame::Frame {
            decoded: Decoded::Record(_),
            ..
        })
    )
}

/// The records a scan yields before it stops, and where it stopped.
fn walk(journal: &[u8], align: ProgramAlign) -> (usize, Option<DecodeError>, usize) {
    let mut scan = Scan::new(journal, align);
    let mut yielded = 0;
    let mut failure = None;
    for step in &mut scan {
        match step {
            Ok(_) => yielded += 1,
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    (yielded, failure, scan.offset())
}

#[test]
fn the_checksum_widths_are_the_ones_the_frame_freezes() {
    // Issue #17's second "done when": the widths are settled *as a result* of the
    // algorithm choice. They are settled as the return types of `IntegrityCheck`, and
    // these constants are what say so in bytes on media. Compared with literals rather
    // than with each other, because both are on media for the life of a device.
    assert_eq!(HEADER_CRC_BYTES, 2);
    assert_eq!(FRAME_CRC_BYTES, 4);
    assert_eq!(HEADER_CRC_BYTES, size_of::<u16>());
    assert_eq!(FRAME_CRC_BYTES, size_of::<u32>());

    // And that they are the widths the frame layout actually spends: the header's last two
    // bytes and the whole trailer.
    assert_eq!(TRAILER_BYTES, FRAME_CRC_BYTES);
    assert_eq!(HEADER_BYTES, 10 + HEADER_CRC_BYTES);
    assert_eq!(FRAME_OVERHEAD_BYTES, HEADER_BYTES + FRAME_CRC_BYTES);
}

#[test]
fn the_shipped_check_is_the_catalogued_pair_adr_0010_settled() {
    // Two numbers from outside this repository. An implementation that agrees with them
    // agrees with every other implementation of the same algorithm, which is the whole
    // reason ADR 0010 chose a catalogued one — a journal pulled off a device is verifiable
    // with a tool nobody had to write.
    assert_eq!(Catalogued::header_check(CHECK), 0x29B1);
    assert_eq!(Catalogued::frame_check(CHECK), 0xCBF4_3926);
}

#[test]
fn the_rejected_candidate_reproduces_its_published_check_value() {
    // ADR 0010 rejects CRC-32C on a measurement. A rejected candidate that was never
    // implemented is a rejection nobody can check, so it is implemented here — and
    // anchored, so that it is the algorithm the ADR weighed rather than a loop that looks
    // like it.
    assert_eq!(Crc32c::frame_check(CHECK), 0xE306_9283);
    assert_eq!(Crc32c::header_check(CHECK), 0xBB3D);

    // And it really is a different answer over the same bytes, which is what makes every
    // swap test below mean something.
    assert_ne!(Crc32c::frame_check(CHECK), Catalogued::frame_check(CHECK));
    assert_ne!(Crc32c::header_check(CHECK), Catalogued::header_check(CHECK));
}

#[test]
fn the_default_codec_is_the_shipped_check_and_nothing_else() {
    // `encode` is documented as `encode_with::<Catalogued>`. The byte comparison cannot
    // fail while that is literally true, so this is a guard against the *rebinding* — a
    // wrapper pointed at some other implementation — rather than a proof of anything about
    // the shipped bytes. `frame.rs`'s golden-frame tests are what pin those.
    let mut wired = [0_u8; SCRATCH];
    let mut generic = [0_u8; SCRATCH];
    for record in samples() {
        for bytes in ALIGNMENTS {
            let align = align_or_byte(bytes);
            wired.fill(0);
            generic.fill(0);
            let a = frame::encode(&record, align, &mut wired).unwrap();
            let b = frame::encode_with::<Catalogued>(&record, align, &mut generic).unwrap();
            assert_eq!(a, b);
            assert_eq!(wired[..a], generic[..b], "{record:?} at {bytes}");

            assert_eq!(
                frame::decode(&wired[..a]).unwrap(),
                frame::decode_with::<Catalogued>(&generic[..b]).unwrap()
            );
        }
    }
}

#[test]
fn the_header_seal_is_the_header_check_and_the_frame_seal_is_the_frame_check() {
    // Which hook seals which range, pinned against both implementations. With one
    // implementation this could not be said at all: a codec that computed the header seal
    // with `frame_check` truncated would round-trip perfectly.
    let record = RecordRef::EffectCompleted {
        seq: EffectSeq(9),
        result: &[1, 2, 3],
    };
    let mut page = [0_u8; SCRATCH];

    let written = frame::encode(&record, ProgramAlign::BYTE, &mut page).unwrap();
    let covered = written - FRAME_CRC_BYTES;
    assert_eq!(
        page[HEADER_BYTES - HEADER_CRC_BYTES..HEADER_BYTES],
        Catalogued::header_check(&page[..HEADER_BYTES - HEADER_CRC_BYTES]).to_le_bytes()
    );
    assert_eq!(
        page[covered..written],
        Catalogued::frame_check(&page[..covered]).to_le_bytes()
    );

    let written = frame::encode_with::<Crc32c>(&record, ProgramAlign::BYTE, &mut page).unwrap();
    let covered = written - FRAME_CRC_BYTES;
    assert_eq!(
        page[HEADER_BYTES - HEADER_CRC_BYTES..HEADER_BYTES],
        Crc32c::header_check(&page[..HEADER_BYTES - HEADER_CRC_BYTES]).to_le_bytes()
    );
    assert_eq!(
        page[covered..written],
        Crc32c::frame_check(&page[..covered]).to_le_bytes()
    );
}

#[test]
fn an_input_digest_is_the_frame_check_of_whatever_seals_the_frame() {
    // ADR 0011 records a scheduled effect's input as a length and a digest, and ADR 0010
    // says there is exactly one definition of "the digest" — the frame's own CRC-32. That
    // coupling has to survive the swap, or a journal sealed with one check would carry
    // digests computed with another.
    for input in [&b""[..], b"in", b"a longer activity input"] {
        assert_eq!(frame::input_digest(input), Catalogued::frame_check(input));
        assert_eq!(
            frame::input_digest_with::<Catalogued>(input),
            frame::input_digest(input)
        );
        assert_eq!(
            frame::input_digest_with::<Crc32c>(input),
            Crc32c::frame_check(input)
        );
    }
}

#[test]
fn every_candidate_computes_the_same_crc32c() {
    // ADR 0010's decision rests on a claim it makes in one sentence and nowhere checks: "a
    // table is an implementation of an algorithm and not a different one". If that were
    // false, the table rows of its measurement table would be measuring something else and
    // the 64 B / 1024 B `.rodata` figures would not be prices for the same answer. All three
    // candidates therefore have to agree, byte for byte, over inputs that cross a nibble
    // boundary, a byte boundary and neither.
    assert_eq!(crc32c(CHECK), 0xE306_9283);
    assert_eq!(crc32c_nibble(CHECK), 0xE306_9283);
    assert_eq!(crc32c_byte(CHECK), 0xE306_9283);

    let mut state = 0x0123_4567_89AB_CDEF_u64;
    let mut sample = [0_u8; 137];
    for length in 0..sample.len() {
        for slot in &mut sample {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *slot = u8::try_from(state & 0xFF).unwrap_or(0);
        }
        let input = &sample[..length];
        assert_eq!(crc32c_nibble(input), crc32c(input), "length {length}");
        assert_eq!(crc32c_byte(input), crc32c(input), "length {length}");
    }
}

#[test]
fn a_swapped_integrity_check_round_trips_through_the_whole_codec() {
    // The swap point, driven end to end rather than at the checksum call: encode, decode,
    // and the append scan, at every alignment, for every record variant. Anything less
    // would leave a path that still reached the shipped check.
    let mut page = [0_u8; SCRATCH];
    for record in samples() {
        for bytes in ALIGNMENTS {
            let align = align_or_byte(bytes);
            page.fill(0);
            let written = frame::encode_with::<Crc32c>(&record, align, &mut page).unwrap();
            assert_eq!(written, frame::encoded_len(&record, align).unwrap());

            let decoded = frame::decode_with::<Crc32c>(&page[..written]).unwrap();
            assert_eq!(decoded.decoded, Decoded::Record(record));

            let mut scan = Scan::<Crc32c>::with_integrity(&page[..written], align);
            assert_eq!(scan.next(), Some(Ok(record)));
            assert_eq!(scan.next(), None);
            assert_eq!(scan.offset(), written);
        }
    }
}

#[test]
fn a_frame_sealed_by_one_check_is_refused_by_the_other() {
    // Swapping the integrity check is a wire-format change, and this is the test that says
    // so out loud. A device reflashed with a different check does not read its own
    // journal — it reports integrity failure at the first record, which is the loud
    // failure §14 asks for rather than a silent reinterpretation.
    let mut page = [0_u8; SCRATCH];
    let record = RecordRef::RunFailed { error: b"why" };
    let align = ProgramAlign::BYTE;

    let written = frame::encode_with::<Crc32c>(&record, align, &mut page).unwrap();
    assert_eq!(
        frame::decode(&page[..written]),
        Err(DecodeError::IntegrityFailed)
    );
    assert_eq!(
        walk(&page[..written], align),
        (0, Some(DecodeError::IntegrityFailed), 0)
    );

    let written = frame::encode(&record, align, &mut page).unwrap();
    assert_eq!(
        frame::decode_with::<Crc32c>(&page[..written]),
        Err(DecodeError::IntegrityFailed)
    );
}

#[test]
fn a_write_torn_at_a_program_unit_boundary_is_never_read_as_a_record() {
    // The first failure mode issue #17 names. A program unit is the smallest thing a
    // device writes atomically, so a power loss mid-append leaves whole units programmed
    // and the rest of the frame erased. Every prefix but the whole frame has to be refused
    // — accepting one would hand replay a record the writer never finished committing to.
    let mut page = [0_u8; SCRATCH];
    for bytes in ALIGNMENTS {
        let mut torn_at_an_interior_boundary = false;
        for record in samples() {
            let align = align_or_byte(bytes);
            page.fill(ERASED_BYTE);
            let written = frame::encode(&record, align, &mut page).unwrap();
            let unit = usize::from(align.get());

            let mut torn = [ERASED_BYTE; SCRATCH];
            let mut units = 0;
            while units * unit < written {
                let cut = units * unit;
                torn.fill(ERASED_BYTE);
                torn[..cut].copy_from_slice(&page[..cut]);

                assert!(
                    !reads_as_a_record(&torn[..written]),
                    "{record:?} at {bytes} torn after {cut} of {written} B decoded as a record"
                );
                // The refusal *kind* matters and is asserted rather than discarded. A tear
                // after nothing is an erased journal, which is a clean end of history; a
                // tear after a program unit leaves programmed bytes behind, and calling
                // that a clean end hands a caller an offset into cells a program cycle has
                // already cleared.
                let (yielded, failure, offset) = walk(&torn[..written], align);
                assert_eq!(yielded, 0, "{record:?} at {bytes} torn after {cut} B");
                assert_eq!(offset, 0);
                assert_eq!(
                    failure,
                    if cut == 0 {
                        None
                    } else {
                        Some(DecodeError::IntegrityFailed)
                    },
                    "{record:?} at {bytes} torn after {cut} B"
                );
                units += 1;
            }

            // The whole frame is the one prefix that is a record, which is what stops the
            // sweep above from passing for the wrong reason.
            assert!(reads_as_a_record(&page[..written]));
            // And at least one sample must tear somewhere other than at nothing, at every
            // alignment. Without this the sweep went vacuous at 16 and 256 the moment every
            // sample fitted inside one program unit, and nothing said so.
            torn_at_an_interior_boundary |= units > 1;
        }
        assert!(
            torn_at_an_interior_boundary,
            "no sample spans more than one program unit at alignment {bytes}, so this sweep \
             only ever tore a frame after nothing"
        );
    }
}

#[test]
fn a_write_torn_inside_a_program_unit_is_never_read_as_a_record() {
    // The same failure at byte granularity. A device that programs a unit in several bus
    // transactions, or a driver whose `program_size` is smaller than the writer believed,
    // tears somewhere that is not a unit boundary — and the answer has to be the same.
    let mut page = [0_u8; SCRATCH];
    for record in samples() {
        let align = ProgramAlign::BYTE;
        page.fill(ERASED_BYTE);
        let written = frame::encode(&record, align, &mut page).unwrap();

        let mut torn = [ERASED_BYTE; SCRATCH];
        for cut in 0..written {
            torn.fill(ERASED_BYTE);
            torn[..cut].copy_from_slice(&page[..cut]);
            assert!(
                !reads_as_a_record(&torn[..written]),
                "{record:?} torn after {cut} of {written} B decoded as a record"
            );
        }
    }
}

#[test]
fn a_torn_write_leaves_the_committed_prefix_of_earlier_records_intact() {
    // §14: "frame ignored; previous history prefix wins". A tear in the last record must
    // cost the journal that record and nothing else, and the scan must stop *at* it rather
    // than past it — an offset past a torn frame is an offset into cells a program cycle
    // has already cleared.
    let align = align_or_byte(8);
    let unit = usize::from(align.get());
    let mut journal = [ERASED_BYTE; SCRATCH];
    let committed = [
        RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"go",
        },
        RecordRef::EffectCompleted {
            seq: EffectSeq(1),
            result: b"ok",
        },
    ];

    let mut offset = 0;
    for record in committed {
        offset += frame::encode(&record, align, &mut journal[offset..]).unwrap();
    }
    let prefix_end = offset;
    let last = RecordRef::EffectCompleted {
        seq: EffectSeq(2),
        result: b"lost",
    };
    let last_len = frame::encode(&last, align, &mut journal[offset..]).unwrap();

    let mut torn = [ERASED_BYTE; SCRATCH];
    let mut cut = 0;
    while cut < last_len {
        torn.fill(ERASED_BYTE);
        torn[..prefix_end + cut].copy_from_slice(&journal[..prefix_end + cut]);

        let (yielded, failure, stopped) = walk(&torn[..prefix_end + last_len], align);
        assert_eq!(yielded, 2, "torn after {cut} B of the last record");
        assert_eq!(stopped, prefix_end, "torn after {cut} B of the last record");
        // Two records and then *nothing programmed* is a clean end of history; two records
        // and then a torn frame is not, and the difference is the one §14 turns on. Without
        // this assertion a scan that reported every tear as a clean end passed.
        assert_eq!(
            failure,
            if cut == 0 {
                None
            } else {
                Some(DecodeError::IntegrityFailed)
            },
            "torn after {cut} B of the last record"
        );
        cut += unit;
    }

    // Undamaged, all three are history — so the sweep above is losing the third record to
    // the tear rather than to a journal that never held it.
    let (yielded, failure, stopped) = walk(&journal[..prefix_end + last_len], align);
    assert_eq!((yielded, failure), (3, None));
    assert_eq!(stopped, prefix_end + last_len);
}

#[test]
fn a_stale_erased_tail_is_the_end_of_history_and_a_programmed_one_is_not() {
    // The second failure mode issue #17 names. An erased tail is how every journal ends,
    // so it cannot be an error; anything programmed after an erased run cannot have been
    // appended by a writer that never skips, so it cannot be a clean end. Reporting one
    // as the other is the worst failure this codec has, because everything downstream
    // believes an offset.
    let align = align_or_byte(4);
    let mut journal = [ERASED_BYTE; SCRATCH];
    let record = RecordRef::RunCompleted { result: b"done" };
    let written = frame::encode(&record, align, &mut journal).unwrap();

    let (yielded, failure, stopped) = walk(&journal[..256], align);
    assert_eq!((yielded, failure), (1, None));
    assert_eq!(stopped, written);

    // A second frame stranded past an erased gap: committed history cannot have a hole in
    // it, so this is damage rather than the end.
    let mut gapped = journal;
    let stranded = frame::encode(&record, align, &mut gapped[128..]).unwrap();
    assert!(stranded > 0);
    let (yielded, failure, stopped) = walk(&gapped[..256], align);
    assert_eq!(yielded, 1);
    assert_eq!(failure, Some(DecodeError::IntegrityFailed));
    assert_eq!(stopped, written);
}

#[test]
fn an_erased_page_and_a_zeroed_page_are_never_records() {
    // The two states a page reaches without a writer: erased, and programmed to zero by a
    // tear that cleared every bit. Neither is a record, and the magic refuses both before
    // a checksum is reached — which is why `MAGIC` is neither `0x0000` nor `0xFFFF`.
    let erased = [ERASED_BYTE; 64];
    let zeroed = [0_u8; 64];
    assert_eq!(frame::decode(&erased), Err(DecodeError::IntegrityFailed));
    assert_eq!(frame::decode(&zeroed), Err(DecodeError::IntegrityFailed));

    let align = ProgramAlign::BYTE;
    assert_eq!(walk(&erased, align), (0, None, 0));
    assert_eq!(
        walk(&zeroed, align),
        (0, Some(DecodeError::IntegrityFailed), 0)
    );
}

#[test]
fn every_bit_a_partial_program_could_clear_is_caught() {
    // NOR programming only clears bits: a torn write turns ones into zeroes and never the
    // other way. So the damage a device can actually do to an already-written frame is a
    // subset of arbitrary bit flips, and it is the subset worth sweeping exhaustively.
    let mut page = [0_u8; SCRATCH];
    for record in samples() {
        for bytes in ALIGNMENTS {
            let align = align_or_byte(bytes);
            page.fill(ERASED_BYTE);
            let padded = frame::encode(&record, align, &mut page).unwrap();
            // The *frame*, not its padding. Padding is a run of `ERASED_BYTE` no writer
            // programs and no reader interprets — see
            // `damage_in_a_frames_padding_is_not_caught`, which states that limitation
            // rather than leaving this sweep to imply it does not exist.
            let written = FRAME_OVERHEAD_BYTES + payload_len(&record);
            assert!(written <= padded);
            for index in 0..written {
                for bit in 0..8_u8 {
                    if page[index] & (1 << bit) == 0 {
                        // Already zero: a program cycle cannot change it, so there is no
                        // failure here to detect.
                        continue;
                    }
                    let mut damaged = page;
                    damaged[index] &= !(1 << bit);
                    assert!(
                        !reads_as_a_record(&damaged[..padded]),
                        "{record:?} at {bytes}, byte {index} bit {bit} cleared and still \
                         decoded"
                    );
                    let (yielded, _, offset) = walk(&damaged[..padded], align);
                    assert_eq!(yielded, 0);
                    assert_eq!(offset, 0);
                }
            }
        }
    }
}

#[test]
fn damage_in_a_frames_padding_is_not_caught() {
    // Stated on purpose, in the style `frame.rs` uses for the alignment mismatch it cannot
    // see. Padding is outside both seals — `frame_crc` covers the header and the payload,
    // and `Scan` strides over the pad without reading it — so a bit cleared there is
    // invisible. That is sound as long as nothing writes the padding: `encode` fills it with
    // `ERASED_BYTE`, which programs no cells at all. The limitation is bounded and asserted
    // rather than left for somebody to discover from a device.
    let align = align_or_byte(16);
    let record = RecordRef::EffectCompleted {
        seq: EffectSeq(1),
        result: b"ok",
    };
    let mut page = [ERASED_BYTE; SCRATCH];
    let padded = frame::encode(&record, align, &mut page).unwrap();
    let frame_len = FRAME_OVERHEAD_BYTES + payload_len(&record);
    assert!(
        padded > frame_len,
        "this record must be padded to be a test"
    );

    for index in frame_len..padded {
        let mut damaged = page;
        damaged[index] &= !1;
        assert!(
            reads_as_a_record(&damaged[..padded]),
            "padding byte {index} is inside a seal after all, which would be an improvement"
        );
        assert_eq!(walk(&damaged[..padded], align), (1, None, padded));
    }
}

#[test]
fn only_the_header_seal_can_catch_a_header_edit_that_was_resealed_at_the_frame() {
    // The header seal is otherwise untestable through damage, because `frame_crc` covers the
    // header too: every corruption sweep in this file is refused by the frame seal whether
    // or not the header seal exists. Mutation review proved the gap — deleting the header
    // CRC check from `decode_with` left all sixteen tests here green. Resealing the frame
    // over the edited header is what leaves the header seal as the only check that can
    // refuse, which is also §09's reason for having two: `payload_len` has to be trusted
    // before the frame's end can be located, so it cannot be the frame seal that vouches
    // for it.
    let record = RecordRef::EffectCompleted {
        seq: EffectSeq(9),
        result: &[1, 2, 3],
    };
    let mut page = [0_u8; SCRATCH];
    let written = frame::encode(&record, ProgramAlign::BYTE, &mut page).unwrap();
    let covered = written - FRAME_CRC_BYTES;

    for index in 0..HEADER_BYTES - HEADER_CRC_BYTES {
        for bit in 0..8_u8 {
            let mut damaged = page;
            damaged[index] ^= 1 << bit;
            let reseal = Catalogued::frame_check(&damaged[..covered]).to_le_bytes();
            damaged[covered..written].copy_from_slice(&reseal);

            // `payload_len`'s two bytes move the frame's end, so a reseal over `covered`
            // bytes is a frame whose length field disagrees with its own extent; that is
            // `Truncated` or `IntegrityFailed` depending on which way the length moved.
            // Every other byte must be the header seal's refusal, and none of them may
            // decode.
            assert!(
                !reads_as_a_record(&damaged[..written]),
                "header byte {index} bit {bit} was resealed into a record"
            );
            if !(8..10).contains(&index) {
                assert_eq!(
                    frame::decode(&damaged[..written]),
                    Err(DecodeError::IntegrityFailed),
                    "header byte {index} bit {bit} was not refused by the header seal"
                );
            }
        }
    }
}

#[test]
fn a_wrong_magic_is_refused_even_when_both_seals_hold() {
    // `MAGIC` is neither `0x0000` nor `0xFFFF` so that a zeroed page and an erased one are
    // refused by the first check rather than by a checksum further in. That claim cannot be
    // tested with a zeroed page — its seals fail too — so the magic is edited and *both*
    // seals recomputed over the result. Only the magic check can refuse this frame.
    let record = RecordRef::EffectCompleted {
        seq: EffectSeq(9),
        result: &[1, 2, 3],
    };
    let mut page = [0_u8; SCRATCH];
    let written = frame::encode(&record, ProgramAlign::BYTE, &mut page).unwrap();
    let covered = written - FRAME_CRC_BYTES;
    let sealed_header = HEADER_BYTES - HEADER_CRC_BYTES;

    page[0] ^= 0x01;
    let header_seal = Catalogued::header_check(&page[..sealed_header]).to_le_bytes();
    page[sealed_header..HEADER_BYTES].copy_from_slice(&header_seal);
    let frame_seal = Catalogued::frame_check(&page[..covered]).to_le_bytes();
    page[covered..written].copy_from_slice(&frame_seal);

    assert_eq!(
        frame::decode(&page[..written]),
        Err(DecodeError::IntegrityFailed)
    );
    assert_eq!(
        walk(&page[..written], ProgramAlign::BYTE),
        (0, Some(DecodeError::IntegrityFailed), 0)
    );
}

#[test]
fn burst_errors_are_caught_exhaustively_to_nine_bits_and_sampled_to_thirty_two() {
    // The property a CRC is chosen for, and the reason the widths in
    // `the_checksum_widths_are_the_ones_the_frame_freezes` are the ones they are: a CRC
    // with a degree-n generator detects every error burst of n bits or fewer. A burst is
    // exactly what a flash device produces when a program pulse dies partway through a
    // word.
    //
    // The two seals are 16 and 32 bits, so the property runs to 32 — and exhausting a
    // 32-bit burst means 2^30 interiors per position, which is not a test anybody runs. So:
    // exhaustive to nine bits, where every interior is reachable, and pseudo-random
    // interiors from ten to thirty-two, which is where the guarantee is tightest and a
    // counterexample would be most interesting. The name says exactly that, because
    // "up to the check width" would promise the sweep the exhaustive half does not do.
    let record = RecordRef::EffectScheduled {
        seq: EffectSeq(0x0102_0304),
        kind: ActivityKind(0x1234),
        input_len: 0x40,
        input_crc: 0xDEAD_BEEF,
    };
    let mut page = [0_u8; SCRATCH];
    let written = frame::encode(&record, ProgramAlign::BYTE, &mut page).unwrap();
    let frame_bits = written * 8;

    // Every pattern of every burst up to nine bits: the first and last bit of a burst are
    // set by definition, so a burst of length `len` has `2^(len - 2)` interiors.
    for start in 0..frame_bits {
        for len in 1..=9_usize {
            if start + len > frame_bits {
                break;
            }
            let interiors = 1_u32 << len.saturating_sub(2).min(31);
            for interior in 0..if len < 3 { 1 } else { interiors } {
                let mut pattern = 1_u32 | (1 << (len - 1));
                if len >= 3 {
                    pattern |= interior << 1;
                }
                let mut damaged = page;
                for offset in 0..len {
                    if pattern & (1 << offset) == 0 {
                        continue;
                    }
                    let bit = start + offset;
                    damaged[bit / 8] ^= 1 << (bit % 8);
                }
                assert!(
                    !reads_as_a_record(&damaged[..written]),
                    "burst of {len} bits at {start} pattern {pattern:#x} survived"
                );
            }
        }
    }

    // Ten to thirty-two bits, sampled. xorshift64 rather than a dependency: no layer of this
    // workspace may take one, so a test of a layer does not reach for `proptest` either.
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut draw = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for start in 0..frame_bits {
        for len in 10..=32_usize {
            if start + len > frame_bits {
                break;
            }
            for _ in 0..4 {
                let interior = u32::try_from(draw() & 0xFFFF_FFFF).unwrap_or(0);
                let mut pattern = 1_u32 | (1 << (len - 1));
                pattern |= (interior << 1) & ((1 << (len - 1)) - 1);
                let mut damaged = page;
                for offset in 0..len {
                    if pattern & (1 << offset) == 0 {
                        continue;
                    }
                    let bit = start + offset;
                    damaged[bit / 8] ^= 1 << (bit % 8);
                }
                assert!(
                    !reads_as_a_record(&damaged[..written]),
                    "burst of {len} bits at {start} pattern {pattern:#x} survived"
                );
            }
        }
    }
}

#[test]
fn a_resealed_forgery_is_accepted_because_a_crc_is_not_authentication() {
    // Issue #17 puts authentication out of scope and §09 says the same, so the limit is
    // stated here as a passing test. Anyone who can write to the media can rewrite a
    // record and reseal it, and the codec will hand the forgery back as history. A
    // signature would change that; a CRC does not, and no documentation in this repository
    // may imply otherwise.
    let mut page = [0_u8; SCRATCH];
    let honest = RecordRef::EffectCompleted {
        seq: EffectSeq(1),
        result: b"paid",
    };
    let written = frame::encode(&honest, ProgramAlign::BYTE, &mut page).unwrap();
    assert!(reads_as_a_record(&page[..written]));

    let forged = RecordRef::EffectCompleted {
        seq: EffectSeq(1),
        result: b"free",
    };
    let rewritten = frame::encode(&forged, ProgramAlign::BYTE, &mut page).unwrap();
    assert_eq!(rewritten, written);
    assert_eq!(
        frame::decode(&page[..rewritten]).unwrap().decoded,
        Decoded::Record(forged)
    );
}
