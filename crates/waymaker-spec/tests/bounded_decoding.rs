//! Design document §14's fifth guarantee: "malformed storage cannot cause out-of-bounds
//! reads or allocation."
//!
//! This is the one guarantee that is not a statement about a state, so it is not proved over
//! the ghost model. It is a statement about a *decoder*, and it is discharged against the
//! shipped one — `waymaker_flash::frame` — over a domain this file states rather than
//! describes.
//!
//! # The three claims, and what discharges each
//!
//! * **Total.** Every input in the domain returns `Ok` or `Err`. A panic is an out-of-bounds
//!   read that Rust caught for you, and on `panic = "abort"` firmware it is a reset.
//! * **In bounds.** Every borrow a decoded record hands back lies inside the caller's slice.
//!   Checked by pointer range, so a decoder that returned a subslice of some other buffer
//!   would be caught rather than assumed impossible.
//! * **Allocation-free.** Structural, not measured: `waymaker-core` and `waymaker-flash` are
//!   `no_std`, have no dependencies, and are refused an `extern crate alloc` by the
//!   `crate-attributes` and `kernel-zero-dependencies` gate rules. Measuring it instead would
//!   need a global allocator, and a global allocator needs the `unsafe` this workspace
//!   denies. [`waymaker_spec::obligation`] carries that as the clause's `owed` note rather
//!   than letting the gap go unsaid.
//!
//! # The domain
//!
//! Exhaustive over every byte string up to three bytes, every truncation of a valid frame,
//! every single-byte mutation of one, every *pair* of positions over an eight-value
//! corruption alphabet, every declared payload length a header can carry, and every
//! erased-tail and stale-tail shape a scan can meet. §15 names four of those — "CRC
//! corruption, stale tails, and malformed lengths" — and the exhaustive byte-string sweep is
//! what makes the word *exhaustive* mean something at the small sizes where a hand-written
//! case would have been guesswork.
//!
//! What the domain is **not** is every malformed input. Three bytes is a quarter of a header,
//! so no unstructured input here is ever a whole frame, and the structured half is mutations
//! of frames this firmware wrote. A bug needing three coordinated corrupt fields, or two
//! outside the corruption alphabet, is outside it — and the scan half has a domain of its
//! own, listed at [`scan_layouts`]. Both restrictions are rows in
//! [`waymaker_spec::obligation`]'s `owed` column, not footnotes here, because a domain a
//! reader has to infer from a passing test is a domain nobody knows the edges of.

use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, DecodeError, EffectSeq, RecordRef};
use waymaker_flash::frame::{
    self, ERASED_BYTE, FRAME_OVERHEAD_BYTES, HEADER_BYTES, HEADER_CRC_BYTES, ProgramAlign, Scan,
};
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};

/// Where §09's twelve-byte header carries `payload_len`.
///
/// The header seal is the last field, and `payload_len` is the two bytes before it, so both
/// offsets are derived from the constants `waymaker-flash` exports rather than written as
/// literals a frame-layout change could leave behind.
const HEADER_CRC_AT: usize = HEADER_BYTES - HEADER_CRC_BYTES;
const PAYLOAD_LEN_AT: usize = HEADER_CRC_AT - 2;

fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    align
}

/// A frame this firmware writes, and the bytes it occupies.
fn valid_frame() -> Vec<u8> {
    let mut buffer = [0_u8; 64];
    let Ok(written) = frame::encode(
        &RecordRef::EffectCompleted {
            seq: EffectSeq(1),
            result: b"result",
        },
        align(),
        &mut buffer,
    ) else {
        unreachable!("64 bytes is more than this record needs")
    };
    let Some(bytes) = buffer.get(..written) else {
        unreachable!("`encode` reports what it wrote")
    };
    bytes.to_vec()
}

/// A frame whose record borrows the whole of its payload, and a long one.
///
/// The in-bounds claim is only checked on records that *have* a borrow, so the corpus needs
/// more than one shape that does: `valid_schedule` carries four scalars and no bytes at all,
/// so a pointer check run over it alone would be a check over nothing.
fn valid_run_started() -> Vec<u8> {
    let mut buffer = [0_u8; 96];
    let Ok(written) = frame::encode(
        &RecordRef::RunStarted {
            workflow_kind: 9,
            workflow_version: 2,
            input: b"a rather longer input than the others",
        },
        align(),
        &mut buffer,
    ) else {
        unreachable!("96 bytes is more than this record needs")
    };
    let Some(bytes) = buffer.get(..written) else {
        unreachable!("`encode` reports what it wrote")
    };
    bytes.to_vec()
}

/// A second frame with a different shape, so the corpus is not one layout.
fn valid_schedule() -> Vec<u8> {
    let mut buffer = [0_u8; 64];
    let Ok(written) = frame::encode(
        &RecordRef::EffectScheduled {
            seq: EffectSeq(7),
            kind: ActivityKind(3),
            input_len: 4,
            input_crc: frame::input_digest(b"blob"),
        },
        align(),
        &mut buffer,
    ) else {
        unreachable!("64 bytes is more than this record needs")
    };
    let Some(bytes) = buffer.get(..written) else {
        unreachable!("`encode` reports what it wrote")
    };
    bytes.to_vec()
}

/// Every borrow a decoded record hands back.
fn borrows<'a>(record: &RecordRef<'a>) -> Vec<&'a [u8]> {
    match record {
        RecordRef::RunStarted { input, .. } => vec![input],
        RecordRef::EffectCompleted { result, .. } | RecordRef::RunCompleted { result } => {
            vec![result]
        }
        RecordRef::EffectFailed { error, .. } | RecordRef::RunFailed { error } => vec![error],
        RecordRef::EffectScheduled { .. } => Vec::new(),
    }
}

/// Decodes `bytes` and checks the two claims that hold for any input at all.
///
/// Returns whether the decode succeeded, so a caller can assert its sweep found both answers
/// rather than one of them.
fn decodes_within_its_input(bytes: &[u8]) -> bool {
    let Ok(frame) = frame::decode(bytes) else {
        return false;
    };
    assert!(
        frame.frame_len <= bytes.len(),
        "a frame {} bytes long decoded out of {} bytes",
        frame.frame_len,
        bytes.len()
    );
    if let frame::Decoded::Record(record) = frame.decoded {
        let outer = bytes.as_ptr_range();
        for borrow in borrows(&record) {
            let inner = borrow.as_ptr_range();
            assert!(
                inner.start >= outer.start && inner.end <= outer.end,
                "a decoded record borrows bytes outside the input it was decoded from"
            );
        }
    }
    true
}

#[test]
fn every_byte_string_up_to_three_bytes_decodes_or_is_refused() {
    // 1 + 256 + 65_536 + 16_777_216 inputs, exhaustively. Three bytes is a quarter of a
    // header, so nothing here can be a whole frame — which is the point: every one of these
    // is malformed storage, and the decoder has to have an answer for all of them.
    let mut buffer = Vec::with_capacity(3);
    assert!(!decodes_within_its_input(&buffer));
    for first in 0..=u8::MAX {
        buffer.clear();
        buffer.push(first);
        assert!(!decodes_within_its_input(&buffer));
        for second in 0..=u8::MAX {
            buffer.truncate(1);
            buffer.push(second);
            assert!(!decodes_within_its_input(&buffer));
            for third in 0..=u8::MAX {
                buffer.truncate(2);
                buffer.push(third);
                assert!(!decodes_within_its_input(&buffer));
            }
        }
    }
}

#[test]
fn every_truncation_of_a_valid_frame_is_refused_rather_than_half_decoded() {
    // The boundary is the frame's own length, not the buffer's: `encode` pads to the
    // device's program granularity, and the pad belongs to the device rather than to the
    // frame. So a buffer cut back to exactly `frame_len` still holds a whole frame and has
    // to decode, and one byte less has to be refused. Getting that wrong in either
    // direction is the failure this test is for — a decoder that refused a frame with its
    // pad trimmed would reject the last record of every journal.
    let corpora = [valid_frame(), valid_schedule(), valid_run_started()];
    // One of the two has to carry padding, or the interesting half of the claim below —
    // that a frame with its pad trimmed still decodes — is about nothing.
    assert!(
        corpora.iter().any(|corpus| {
            frame::decode(corpus).is_ok_and(|frame| frame.frame_len < corpus.len())
        }),
        "neither corpus carries padding, so the trimmed-pad case is never exercised"
    );
    for corpus in corpora {
        let frame_len = frame::decode(&corpus)
            .expect("the corpus is a frame this firmware wrote")
            .frame_len;
        for length in 0..frame_len {
            let truncated = corpus.get(..length).expect("a prefix of its own length");
            assert!(
                !decodes_within_its_input(truncated),
                "{length} bytes of a {frame_len} byte frame decoded"
            );
        }
        for length in frame_len..=corpus.len() {
            let padded = corpus.get(..length).expect("a prefix of its own length");
            assert!(
                decodes_within_its_input(padded),
                "a whole {frame_len} byte frame in {length} bytes did not decode, so this \
                 sweep proves nothing"
            );
        }
    }
}

#[test]
fn every_single_byte_mutation_of_a_valid_frame_is_answered() {
    // §15's "CRC corruption", exhaustively rather than by example: every byte of every valid
    // frame, set to every other value. A mutation that still decodes is legitimate — the
    // payload bytes of a record are not what the seal is *for* — so the claim is that the
    // decoder answers within its input, never that it refuses.
    let mut accepted = 0_usize;
    let mut refused = 0_usize;
    let mut borrowed = 0_usize;
    for corpus in [valid_frame(), valid_schedule(), valid_run_started()] {
        for position in 0..corpus.len() {
            for replacement in 0..=u8::MAX {
                let mut mutated = corpus.clone();
                let Some(cell) = mutated.get_mut(position) else {
                    unreachable!("position is an index into this vector")
                };
                if *cell == replacement {
                    continue;
                }
                *cell = replacement;
                if decodes_within_its_input(&mutated) {
                    accepted += 1;
                    if frame::decode(&mutated).is_ok_and(|frame| match frame.decoded {
                        frame::Decoded::Record(record) => {
                            borrows(&record).iter().any(|slice| !slice.is_empty())
                        }
                        frame::Decoded::UnknownKind(_) => false,
                    }) {
                        borrowed += 1;
                    }
                } else {
                    refused += 1;
                }
            }
        }
    }
    assert!(
        accepted > 0 && refused > 0,
        "the mutation sweep produced only one answer ({accepted} accepted, {refused} \
         refused), so it is not exercising the seal"
    );
    // The in-bounds half of `decodes_within_its_input` only runs on a record that has a
    // borrow, so a sweep in which nothing decoded to one would be checking totality alone.
    assert!(
        borrowed > 0,
        "no mutated frame decoded to a record with a non-empty borrow, so the pointer check \
         never ran"
    );
}

#[test]
fn every_declared_payload_length_a_header_can_carry_is_answered() {
    // §15's "malformed lengths", exhaustively: the length field is a `u16`, so every value
    // it can hold is 65_536 cases and there is no reason to sample them.
    //
    // The header seal has to be recomputed after the length is changed, and that is the
    // whole point of this test rather than an incidental detail. §09 decodes the header
    // checksum *before* it reads `payload_len` — that is what makes the length trustworthy —
    // so a sweep that only overwrote the length would be refused at the seal every time and
    // the bounds check would never see a single one of the 65_536 values. That is exactly
    // what the first version of this test did, and it passed.
    let corpus = valid_frame();
    let mut reached = 0_usize;
    let mut accepted = 0_usize;
    for declared in 0..=u16::MAX {
        let mut mutated = corpus.clone();
        let Some(length_field) = mutated.get_mut(PAYLOAD_LEN_AT..PAYLOAD_LEN_AT + 2) else {
            unreachable!("the corpus is a whole frame, so it has a header")
        };
        length_field.copy_from_slice(&declared.to_le_bytes());
        let Some(sealed) = mutated.get(..HEADER_CRC_AT) else {
            unreachable!("the corpus is a whole frame, so it has a header")
        };
        let seal = Catalogued::header_check(sealed).to_le_bytes();
        let Some(seal_field) = mutated.get_mut(HEADER_CRC_AT..HEADER_BYTES) else {
            unreachable!("the corpus is a whole frame, so it has a header")
        };
        seal_field.copy_from_slice(&seal);
        reached = reached.saturating_add(1);
        if decodes_within_its_input(&mutated) {
            accepted = accepted.saturating_add(1);
        }
    }
    assert_eq!(
        reached,
        usize::from(u16::MAX) + 1,
        "every declared length is swept, or this is not the exhaustive claim it says it is"
    );
    // Exactly one length describes the payload that is really there. Every other one is
    // refused — and the assertion is that the decoder *reaches* that decision, which the
    // in-bounds check inside `decodes_within_its_input` is what proves.
    assert_eq!(
        accepted, 1,
        "{accepted} of 65536 declared lengths decoded; only the frame's own should"
    );

    // And the same sweep without resealing, kept because it is a different claim: arbitrary
    // bytes anywhere in the header, refused at the seal rather than at the bounds check.
    let mut unsealed = 0_usize;
    for declared in 0..=u16::MAX {
        for offset in 0..HEADER_BYTES.saturating_sub(1) {
            let mut mutated = corpus.clone();
            let Some(window) = mutated.get_mut(offset..offset.saturating_add(2)) else {
                continue;
            };
            window.copy_from_slice(&declared.to_le_bytes());
            decodes_within_its_input(&mutated);
            unsealed = unsealed.saturating_add(1);
        }
    }
    assert_eq!(unsealed, (usize::from(u16::MAX) + 1) * (HEADER_BYTES - 1));
}

#[test]
fn every_coordinated_pair_of_corrupt_bytes_in_a_valid_frame_is_answered() {
    // Codex, PR #66 round 1. The exhaustive byte-string sweep stops before a complete header,
    // and everything longer was a truncation or a *single*-byte mutation — so a decoder bug
    // that needs two fields corrupted together was outside the domain entirely. The obvious
    // one is a length changed and a seal changed to match, which the sweep above now covers;
    // this is the general case.
    //
    // Every pair of positions, over an alphabet of the values a corruption actually reaches:
    // the erased byte, the all-clear byte, the bit patterns a half-programmed unit leaves,
    // and the frame's own magic. Exhaustive over *that* alphabet rather than over all 65_536
    // byte pairs, and the restriction is stated here and in `obligation.rs`'s owed note
    // rather than left for a reader to infer from a passing test.
    const ALPHABET: [u8; 8] = [0x00, 0x01, 0x0F, 0x7F, 0x80, 0xF0, 0xFE, 0xFF];
    let mut accepted = 0_usize;
    let mut refused = 0_usize;
    for corpus in [valid_frame(), valid_run_started()] {
        for first in 0..corpus.len() {
            for second in first.saturating_add(1)..corpus.len() {
                for left in ALPHABET {
                    for right in ALPHABET {
                        let mut mutated = corpus.clone();
                        let (Some(a), Some(b)) = (mutated.get(first), mutated.get(second)) else {
                            unreachable!("both indices are inside the corpus")
                        };
                        if *a == left && *b == right {
                            continue;
                        }
                        if let Some(cell) = mutated.get_mut(first) {
                            *cell = left;
                        }
                        if let Some(cell) = mutated.get_mut(second) {
                            *cell = right;
                        }
                        if decodes_within_its_input(&mutated) {
                            accepted = accepted.saturating_add(1);
                        } else {
                            refused = refused.saturating_add(1);
                        }
                    }
                }
            }
        }
    }
    assert!(
        accepted > 0 && refused > 0,
        "the coordinated-pair sweep produced only one answer ({accepted} accepted, \
         {refused} refused), so it is not exercising the seal"
    );
}

#[test]
fn a_scan_over_arbitrary_bytes_terminates_and_never_goes_backwards() {
    // The other half of bounded decoding: a decoder that always answers is still unbounded
    // if the reader looping over it never finishes. The scan's offset has to strictly
    // increase while it is producing records, and it has to stop.
    //
    // Codex, PR #66 round 2: the first version of this used one 16-byte stale-tail gap and
    // single-byte mutations of one two-frame journal, so a `Scan` offset or termination bug
    // that needed a different gap length, an off-alignment boundary or a coordinated
    // corruption would have passed. The layouts are generated systematically now — every
    // gap length from nothing to two frames, on and off the program granularity, at every
    // position a gap can sit in a three-frame journal — and the residual restriction is a
    // row in `obligation.rs`'s owed column rather than an unstated one.
    let mut produced = 0_usize;
    let mut refused = 0_usize;
    let mut stopped_clean = 0_usize;
    for image in scan_layouts() {
        let mut scan = Scan::new(&image, align());
        let mut previous = scan.offset();
        let mut steps = 0_usize;
        let mut saw_error = false;
        while let Some(item) = scan.next() {
            steps = steps.saturating_add(1);
            assert!(
                steps <= image.len().saturating_add(1),
                "the scan produced more items than the journal has bytes"
            );
            if item.is_ok() {
                produced = produced.saturating_add(1);
                assert!(
                    scan.offset() > previous,
                    "the scan produced a record without advancing"
                );
                assert!(
                    scan.offset() <= image.len(),
                    "the scan advanced past the end of the journal"
                );
                previous = scan.offset();
            } else {
                refused = refused.saturating_add(1);
                saw_error = true;
            }
        }
        if !saw_error {
            stopped_clean = stopped_clean.saturating_add(1);
        }
        // Fused: once it is done it stays done, so a caller that keeps asking gets nothing
        // rather than a second pass over the same bytes.
        assert!(scan.next().is_none());
        assert!(scan.next().is_none());
    }
    assert!(
        produced > 0 && refused > 0 && stopped_clean > 0,
        "the scan sweep is one-sided: {produced} records, {refused} refusals, \
         {stopped_clean} clean ends"
    );
}

/// Journal layouts a scan has to terminate over.
///
/// Generated rather than hand-picked, which is the difference between a claim about arbitrary
/// storage and a claim about the four images somebody thought of.
fn scan_layouts() -> Vec<Vec<u8>> {
    let one = valid_frame();
    let two = valid_schedule();
    let stride = one.len();
    let mut images = vec![Vec::new(), vec![ERASED_BYTE; 64], one.clone()];

    // A frame, a gap of every length up to two frames, then a frame behind the hole — the
    // stale tail §09 and §15 both name. On the program granularity and off it, because a
    // reader handed the wrong stride lands inside a frame's padding, which is a run of
    // erased bytes and reads as a clean end of history with committed records still ahead.
    for gap in 0..stride.saturating_mul(2) {
        let mut stale = one.clone();
        stale.extend(std::iter::repeat_n(ERASED_BYTE, gap));
        stale.extend(two.iter().copied());
        images.push(stale);
    }

    // Every truncation of a three-frame journal, so a frame is cut at every byte of its
    // header, its payload and its trailer.
    let mut three = one.clone();
    three.extend(two.iter().copied());
    three.extend(one.iter().copied());
    for length in 0..three.len() {
        if let Some(prefix) = three.get(..length) {
            images.push(prefix.to_vec());
        }
    }

    // And coordinated corruption: every pair of positions in a two-frame journal over the
    // corruption alphabet, which is what catches an offset bug that needs two fields wrong
    // at once rather than one.
    let mut pair = one.clone();
    pair.extend(two.iter().copied());
    for first in (0..pair.len()).step_by(3) {
        for second in (first..pair.len()).step_by(5) {
            for value in [0x00, 0x01, ERASED_BYTE] {
                let mut mutated = pair.clone();
                if let Some(cell) = mutated.get_mut(first) {
                    *cell = value;
                }
                if let Some(cell) = mutated.get_mut(second) {
                    *cell = value ^ 0xFF;
                }
                images.push(mutated);
            }
        }
    }
    images
}

#[test]
fn the_scan_sweep_covers_the_layouts_it_says_it_does() {
    // The domain asserted rather than described, for the reason every other sweep here does
    // it: an enumeration nobody counted is an enumeration that can quietly shrink.
    let layouts = scan_layouts();
    let stride = valid_frame().len();
    assert!(layouts.contains(&Vec::new()), "the empty journal");
    assert!(
        layouts
            .iter()
            .any(|image| image.iter().all(|byte| *byte == ERASED_BYTE) && !image.is_empty()),
        "a wholly erased device"
    );
    // Every stale-tail gap length, on and off the program granularity.
    let align = usize::from(align().get());
    let gaps: BTreeSet<usize> = (0..stride.saturating_mul(2)).collect();
    assert!(gaps.iter().any(|gap| gap % align == 0), "an aligned gap");
    assert!(gaps.iter().any(|gap| gap % align != 0), "an unaligned gap");
    assert!(
        layouts.len() > 200,
        "only {} layouts, which is a sample rather than a sweep",
        layouts.len()
    );
}

#[test]
fn a_frame_shorter_than_its_own_overhead_is_refused() {
    // The boundary the two claims meet at: a length field that describes fewer bytes than a
    // frame has structure for. Named rather than left to the exhaustive sweep, because it is
    // the case a reviewer will look for.
    for length in 0..FRAME_OVERHEAD_BYTES {
        let bytes = vec![0_u8; length];
        assert!(matches!(
            frame::decode(&bytes),
            Err(DecodeError::Truncated | DecodeError::IntegrityFailed)
        ));
    }
}
