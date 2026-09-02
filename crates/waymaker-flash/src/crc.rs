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
//! Two functions over borrowed bytes. Nothing else: no table, no state, and no trait — the
//! trait that makes the *choice* of algorithm swappable is [`crate::integrity`]'s, and
//! keeping it one module away is what leaves this file as two loops a reader can check
//! against a catalogue.
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
//! # Why these two algorithms, and why there is no table
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
//! The table is not free: 64 B of rodata for a nibble table, 1 KiB for a byte table,
//! against an 8 KiB *total* incremental code-flash budget for the kernel and this adapter
//! together. What it buys is 93 cycles per byte down to 21 or 15. At 48 MHz that is ~60 µs
//! to seal a scheduled-effect record — 20 bytes under [`crc32`] and 10 under [`crc16`],
//! since neither seal covers itself — and ~1.0 ms for a full 512-byte page. So it is not,
//! as this comment once claimed, a cost nobody can measure; it is a cost against which §04
//! states no latency budget at all. Both stay bitwise until a profile of a real workload
//! says otherwise, and that would be a superseding ADR — the `integrity-check` gate rule
//! fails a build that adds a table here, or a local array, without one.
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
    // this workspace and a `const fn` has no `for` loop over a slice to reach for.
    while let Some((byte, tail)) = rest.split_first() {
        crc ^= (*byte as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x1021
            };
            bit += 1;
        }
        rest = tail;
    }
    crc
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

    while let Some((byte, tail)) = rest.split_first() {
        crc ^= *byte as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xEDB8_8320
            };
            bit += 1;
        }
        rest = tail;
    }
    crc ^ 0xFFFF_FFFF
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
