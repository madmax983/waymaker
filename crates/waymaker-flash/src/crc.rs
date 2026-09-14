//! The two checksums the record frame carries.
//!
//! Design document §09: "CRC detects accidental corruption and torn writes; it is not
//! authentication." Both are standard, catalogued algorithms rather than something
//! invented here, so each can be checked against a published value that nothing in this
//! repository produced — see the tests at the foot of this file. A checksum verified only
//! against itself is a checksum that agrees with its own bugs.
//!
//! # What this module owns
//!
//! Two functions over borrowed bytes, and the per-nibble helpers each is built from.
//! Nothing else: no state and no trait — the trait that makes the *choice* of algorithm
//! swappable is [`crate::integrity`]'s, and keeping it one module away is what leaves this
//! file as two algorithms a reader can check against a catalogue.
//!
//! # Why they are `pub(crate)` rather than `pub`
//!
//! A public function of a layer has an enforced cost — `size-probe-reach` requires the
//! size probe to call every one of them by name, so public surface is charged for whether
//! or not anybody uses it. Nothing outside this crate needs to compute a frame's checksum,
//! so neither function is public. Both are reached through
//! [`Catalogued`](crate::integrity::Catalogued), which is the crate's one binding of a seal
//! to an algorithm; `crc32` additionally through [`crate::frame::input_digest`], which is a
//! `const fn` and so cannot go through a trait method.
//!
//! Clippy's `redundant_pub_crate` asks for `pub` here, on the reasoning that this module is
//! private so the two spellings mean the same thing. They do to the compiler and they do
//! not to the gate, which reads the source rather than the item graph: `pub fn` in any
//! layer file is a function the probe is required to call. So the lint is allowed on each,
//! with this as the reason.
//!
//! # Why these two algorithms, and what changed about the table
//!
//! Design document §16 left this open — "whether the default integrity check is CRC32C or a
//! smaller table-free CRC implementation" — and
//! [ADR 0010](https://github.com/madmax983/waymaker/blob/main/docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md)
//! settles it against measurements taken on `thumbv6m-none-eabi`. Two of them decide it.
//!
//! The polynomial is free. CRC-32C and CRC-32/ISO-HDLC compile to the same 52 bytes and the
//! same instruction stream apart from one literal-pool word, because this target has no CRC
//! instruction and the polynomial is an immediate either way. With cost equal, the choice
//! falls to which algorithm a host can check a device's journal against without
//! reimplementing anything, and CRC-32/ISO-HDLC is zlib's, gzip's and PNG's.
//!
//! ADR 0010 kept both bitwise "until a profile of a real workload says otherwise", naming
//! the nibble table as the most likely answer if one ever did.
//! [ADR 0044](https://github.com/madmax983/waymaker/blob/main/docs/adr/0044-a-nibble-table-is-a-superseding-adr-and-crc16-needed-none.md)
//! is that profile, and it splits in two rather than landing where ADR 0010 expected.
//! [`crc16_nibble`] needs no table at all: for this specific polynomial, four rounds over a
//! single nibble reduce to one multiply, with no rodata and no lookup — `crc16` stays
//! table-free in the fullest sense, just no longer bit-serial. [`crc32_nibble`] has no such
//! reduction — its sixteen values are compiled from [`crc32_nibble_table`]'s sixteen-armed
//! `match`, which is a 64 B lookup table in every way that costs, even though no `[u32; 16]`
//! appears in this file's source; the `integrity-check` gate's `INTEGRITY_CHECK_TABLES`
//! pins that shape specifically, so it is a decision with a name attached rather than a
//! `match` statement nobody looked at twice.
//!
//! One property the choice gives up, recorded because it is the only place the two
//! candidates genuinely differ: ISO-HDLC is primitive, so its Hamming distance falls from 4
//! to 3 past a dataword of about 11.2 KiB, where CRC-32C's does not. The largest extent
//! sealed here is a 512-byte page.

/// CRC-16/CCITT-FALSE over `bytes`.
///
/// Polynomial `0x1021`, initial value `0xFFFF`, no reflection, no final xor. The
/// catalogue's check value — the CRC of `b"123456789"` — is `0x29B1`, which is what the
/// tests below compare against.
///
/// # Postconditions
///
/// Pure and total: every `&[u8]`, empty included, has one. The empty input hashes to the
/// initial value `0xFFFF`, which is deliberate rather than incidental — an all-ones
/// initial value is what makes a run of leading zero bytes change the result, and a header
/// of zeroes is exactly what a partially programmed flash page can read back as.
#[allow(
    clippy::redundant_pub_crate,
    reason = "`pub` here would make `size-probe-reach` demand a probe call for a private helper"
)]
pub(crate) const fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    let mut rest = bytes;

    // `split_first` rather than an index or an iterator: `indexing_slicing` is denied in
    // this workspace and a `const fn` has no `for` loop over a slice to reach for. A byte
    // is two nibbles, high half first: standard nibble-at-a-time CRC folds the register
    // four bits at a time as `crc = (crc << 4) ^ table[(crc >> 12) ^ nibble]`, and
    // `crc16_nibble` is that table's one value per input, computed rather than looked up.
    while let Some((byte, tail)) = rest.split_first() {
        let hi = (((crc >> 12) as u8) ^ (*byte >> 4)) & 0xF;
        crc = (crc << 4) ^ crc16_nibble(hi);
        let lo = (((crc >> 12) as u8) ^ (*byte & 0xF)) & 0xF;
        crc = (crc << 4) ^ crc16_nibble(lo);
        rest = tail;
    }
    crc
}

/// What four rounds of CRC-16/CCITT-FALSE add for one nibble, starting from a register
/// with nothing else in it.
///
/// This is the value a nibble-at-a-time table would hold at index `nibble` — `crc16`'s own
/// doc comment states the fold this is used in. It is not looked up in one, because for
/// this specific polynomial it does not need to be: `crc16_nibble_matches_the_four_round_reduction`
/// checks it against the four-round definition (four rounds is `crc16` §09's own eight
/// unrolled the way an earlier revision of this file wrote them, halved for one nibble
/// rather than one byte) for every one of the sixteen inputs, and every one agrees. No
/// `[u16; 16]`, no `match`, and nothing for `integrity-check`'s table pin to police, because
/// there is no table — this is `crc16` staying exactly as table-free as ADR 0010 left it,
/// with a cheaper formula for the same bitwise answer.
///
/// # Postconditions
///
/// `nibble & 0xF == nibble` on every legal call, so the widest input is 15 and the widest
/// output `15 * 0x1021 = 0xF1EF`: comfortably inside `u16`, so this never wraps.
const fn crc16_nibble(nibble: u8) -> u16 {
    // Named once, the way the eight-round loop this replaced named it once and used it
    // eight times: `integrity-check` still counts occurrences of `0x1021` in the function
    // that owns it, retargeted here from `crc16` because this is where it now lives.
    const POLY: u16 = 0x1021;
    (nibble as u16) * POLY
}

/// CRC-32/ISO-HDLC over `bytes` — the one zlib, gzip and PNG use.
///
/// Reflected polynomial `0xEDB8_8320`, initial value `0xFFFF_FFFF`, reflected in and out,
/// final xor `0xFFFF_FFFF`. The catalogue's check value is `0xCBF4_3926`.
///
/// # Postconditions
///
/// Pure and total. The empty input hashes to `0`, which is why [`crate::frame`] never
/// checksums a payload on its own: the frame's checksum covers the header as well, so a
/// record with no payload still has a checksum that depends on which record it is.
#[allow(
    clippy::redundant_pub_crate,
    reason = "`pub` here would make `size-probe-reach` demand a probe call for a private helper"
)]
pub(crate) const fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    let mut rest = bytes;

    // Reflected, so the low nibble folds in before the high one — the mirror image of
    // `crc16`'s high-nibble-first fold, and the reason the two functions read the byte in
    // opposite halves. `crc32_nibble_table` is where each nibble's contribution comes from.
    while let Some((byte, tail)) = rest.split_first() {
        let lo = (crc ^ (*byte as u32)) & 0xF;
        crc = (crc >> 4) ^ crc32_nibble_table(lo as u8);
        let hi = (crc ^ ((*byte >> 4) as u32)) & 0xF;
        crc = (crc >> 4) ^ crc32_nibble_table(hi as u8);
        rest = tail;
    }
    crc ^ 0xFFFF_FFFF
}

/// What four rounds of the reflected CRC-32/ISO-HDLC update add for one nibble, starting
/// from a register with nothing else in it — the value a nibble-at-a-time table would hold
/// at index `nibble`, the same role [`crc16_nibble`] plays for the other checksum.
///
/// Unlike `crc16_nibble`, this does not reduce to a multiply —
/// `crc32_nibble_matches_the_four_round_reduction` checks it against the plain four-round
/// loop for every nibble and there is no simpler closed form underneath it, which is a
/// property of this reflected polynomial rather than of CRC in general.
#[inline(always)]
#[allow(
    clippy::inline_always,
    reason = "LLVM only builds crc32_nibble_table's table when this body is visible at each arm first; a soft #[inline] measured as a real call per nibble instead, see ADR 0044"
)]
const fn crc32_nibble(nibble: u8) -> u32 {
    // `crc16_nibble`'s reason: named once, in the function that now owns it.
    const POLY: u32 = 0xEDB8_8320;
    let mut crc = nibble as u32;
    let mut bit = 0;
    while bit < 4 {
        crc = if crc & 1 == 0 {
            crc >> 1
        } else {
            (crc >> 1) ^ POLY
        };
        bit += 1;
    }
    crc
}

/// [`crc32_nibble`]'s sixteen values, selected rather than recomputed per byte.
///
/// This is a lookup table in every way that costs, even though no `[u32; 16]` appears
/// anywhere in this file: sixteen arms, each a distinct compile-time constant computed by
/// `crc32_nibble` and nothing else, over the full masked range of a nibble with no gap and
/// no repeat. LLVM's own switch-to-lookup-table pass is what turns that shape into a single
/// indexed load from a table it builds in `.rodata` on every target this crate has been
/// disassembled for so far — [ADR 0044] is where that disassembly and the reproduction
/// steps for it live, the same way ADR 0010's cycle counts are a dated, by-hand measurement
/// rather than a thing CI re-derives on every run, and for the same reason: there is no
/// gate here that could tell "a compiler stopped applying this optimisation" apart from "a
/// compiler applied a different one that costs the same", so this stays a documented,
/// reproducible claim rather than a green check that would read as more than it is.
/// [ADR 0044] is the decision that this specific, sixteen-entry, `crc32`-only table is worth
/// 64 B of `.rodata`; the `integrity-check` gate's `INTEGRITY_CHECK_TABLES` pins its
/// *shape* — this function's name, `crc32_nibble`'s name, and the count sixteen — so a
/// seventeenth arm, a seventh helper, or a second table elsewhere in this file is still
/// exactly the surprise ADR 0010 wanted a decision attached to.
///
/// [ADR 0044]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0044-a-nibble-table-is-a-superseding-adr-and-crc16-needed-none.md
#[inline(always)]
#[allow(
    clippy::inline_always,
    reason = "a soft #[inline] left this match uninlined into crc32's loop, measured as a real call per nibble rather than a table load, see ADR 0044"
)]
const fn crc32_nibble_table(nibble: u8) -> u32 {
    match nibble & 0xF {
        0 => crc32_nibble(0),
        1 => crc32_nibble(1),
        2 => crc32_nibble(2),
        3 => crc32_nibble(3),
        4 => crc32_nibble(4),
        5 => crc32_nibble(5),
        6 => crc32_nibble(6),
        7 => crc32_nibble(7),
        8 => crc32_nibble(8),
        9 => crc32_nibble(9),
        10 => crc32_nibble(10),
        11 => crc32_nibble(11),
        12 => crc32_nibble(12),
        13 => crc32_nibble(13),
        14 => crc32_nibble(14),
        _ => crc32_nibble(15),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue's check input: every CRC specification states its result for this.
    const CHECK: &[u8] = b"123456789";

    #[test]
    fn each_algorithm_reproduces_its_published_check_value() {
        // The whole point of choosing catalogued algorithms. These two numbers come from
        // outside this repository, so an implementation that agrees with them agrees with
        // every other implementation of the same algorithm — which is what a wire format
        // meant to be frozen needs. A round-trip test cannot say this: an encoder and a
        // decoder sharing one wrong checksum round-trip perfectly.
        assert_eq!(crc16(CHECK), 0x29B1);
        assert_eq!(crc32(CHECK), 0xCBF4_3926);
    }

    /// A plain, unrolled four-round bitwise fold — one nibble, starting from an empty
    /// register — kept only here, as the independent reference
    /// [`crc16_nibble`]'s doc comment promises it is checked against. Written the same
    /// shape the eight-round byte loop used to be, halved, so a reader can compare the two
    /// side by side rather than trust that the halving was done correctly.
    fn crc16_nibble_reference(nibble: u8) -> u16 {
        const POLY: u16 = 0x1021;
        let mut crc = u16::from(nibble) << 12;
        for _ in 0..4 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ POLY
            };
        }
        crc
    }

    #[test]
    fn crc16_nibble_matches_the_four_round_reduction() {
        // `crc16_nibble` claims a multiply computes the same thing four rounds of bitwise
        // reduction would, for this specific polynomial. Checked over every nibble rather
        // than trusted algebraically: a poly this holds for is a property of `0x1021`, not
        // of CRC in general, and the one thing worth verifying here is that it still holds.
        for nibble in 0_u8..16 {
            assert_eq!(
                crc16_nibble(nibble),
                crc16_nibble_reference(nibble),
                "nibble {nibble}"
            );
        }
    }

    /// The same reference shape as [`crc16_nibble_reference`], for the reflected checksum:
    /// a nibble folded from the low bit rather than the high one, over four rounds.
    fn crc32_nibble_reference(nibble: u8) -> u32 {
        const POLY: u32 = 0xEDB8_8320;
        let mut crc = u32::from(nibble);
        for _ in 0..4 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ POLY
            };
        }
        crc
    }

    #[test]
    fn crc32_nibble_matches_the_four_round_reduction() {
        for nibble in 0_u8..16 {
            assert_eq!(
                crc32_nibble(nibble),
                crc32_nibble_reference(nibble),
                "nibble {nibble}"
            );
        }
    }

    #[test]
    fn crc32_nibble_table_is_every_nibble_with_no_gap_and_no_repeat() {
        // `crc32_nibble_table`'s doc comment claims its sixteen arms are exactly the full
        // masked range with nothing skipped and nothing doubled — checked directly, since
        // that shape is what makes it a table `integrity-check`'s pin can hold to a count
        // rather than a `match` a reviewer has to recount by eye.
        for nibble in 0_u8..16 {
            assert_eq!(
                crc32_nibble_table(nibble),
                crc32_nibble(nibble),
                "nibble {nibble}"
            );
        }
    }

    #[test]
    fn the_empty_input_hashes_to_the_initial_value() {
        // Stated rather than discovered: `crc32` of nothing is zero, which is why the
        // frame checksum covers the header too. A checksum that could be zero for a whole
        // class of records is a checksum a zeroed page can satisfy.
        assert_eq!(crc16(&[]), 0xFFFF);
        assert_eq!(crc32(&[]), 0);
    }

    #[test]
    fn a_leading_zero_byte_changes_the_result() {
        // This is what the all-ones initial value buys. With an initial value of zero,
        // `crc(&[0, 0, x]) == crc(&[x])`, so a header whose leading bytes were lost to a
        // partial program would check out as a shorter header.
        assert_ne!(crc16(&[0, 1]), crc16(&[1]));
        assert_ne!(crc32(&[0, 1]), crc32(&[1]));
        assert_ne!(crc16(&[0, 0, 1]), crc16(&[0, 1]));
        assert_ne!(crc32(&[0, 0, 1]), crc32(&[0, 1]));
    }

    #[test]
    fn every_single_bit_flip_changes_the_result() {
        // A CRC's reason to exist. Swept over a whole message rather than sampled: a
        // shift in the wrong direction, or a polynomial with a lost bit, shows up as some
        // position that does not change the answer.
        const MESSAGE: [u8; 12] = [0x57, 0x4D, 1, 3, 9, 0, 0, 0, 3, 0, 0xAB, 0xCD];

        let clean16 = crc16(&MESSAGE);
        let clean32 = crc32(&MESSAGE);

        for index in 0..MESSAGE.len() {
            for bit in 0..8 {
                let mut flipped = MESSAGE;
                flipped[index] ^= 1 << bit;
                assert_ne!(crc16(&flipped), clean16, "byte {index} bit {bit}");
                assert_ne!(crc32(&flipped), clean32, "byte {index} bit {bit}");
            }
        }
    }

    #[test]
    fn both_are_usable_in_a_const_context() {
        // `const` so that a golden frame in a test, or a table of expected checksums in
        // firmware, costs nothing at runtime — and so that neither can quietly acquire
        // state.
        const HEADER_CRC: u16 = crc16(b"123456789");
        const FRAME_CRC: u32 = crc32(b"123456789");
        assert_eq!(HEADER_CRC, 0x29B1);
        assert_eq!(FRAME_CRC, 0xCBF4_3926);
    }
}
