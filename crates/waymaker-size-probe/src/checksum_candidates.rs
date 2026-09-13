//! ADR 0010's five checksum candidates, for issue #61.
//!
//! [ADR 0010](../../../docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md)
//! decides the integrity check on measured `.text` and `.rodata` sizes. Nothing in this
//! repository produced those numbers until now: they were typed into the ADR by hand, from
//! a build this repository does not run. This module gives `cargo xtask size` a real image
//! to read them from.
//!
//! Each function here is a direct copy of an already-tested body: the two shipped
//! algorithms from `waymaker-flash/src/crc.rs`, and the three rejected candidates from
//! `waymaker-flash/tests/integrity.rs`. Neither file exports its version — the first is
//! `pub(crate)` to that crate, the second lives in a test binary — so a copy is the only
//! way to link one here. This file changes names only, never logic.
//!
//! # Why five functions and not one call each
//!
//! Every function is `#[inline(never)]`, so each keeps its own symbol under
//! `lto = "fat"`. `cargo xtask size` reads each symbol's size on its own; it does not take
//! an image delta the way the rest of the report does, because all five link into one
//! image and a delta could not tell them apart.

/// Calls every candidate and folds the results, so none of them is dead code the linker
/// can remove.
///
/// The return value is never read for its answer, only kept alive through
/// [`core::hint::black_box`]. Correctness of the five algorithms is
/// `waymaker-flash`'s to test; this crate only measures what they compile to.
#[allow(
    clippy::redundant_pub_crate,
    reason = "this is a `[[bin]]` crate; nothing outside it can see a `pub` item either, so \
              `pub(crate)` states the real intent"
)]
pub(crate) fn probe() -> usize {
    let input = core::hint::black_box(b"123456789".as_slice());
    let mut kept = usize::try_from(crc32_iso_hdlc_bitwise_candidate(input)).unwrap_or(0);
    kept = kept.wrapping_add(usize::try_from(crc32c_bitwise_candidate(input)).unwrap_or(0));
    kept = kept.wrapping_add(usize::try_from(crc32c_nibble_table_candidate(input)).unwrap_or(0));
    kept = kept.wrapping_add(usize::try_from(crc32c_byte_table_candidate(input)).unwrap_or(0));
    kept = kept.wrapping_add(usize::from(crc16_ccitt_false_bitwise_candidate(input)));
    core::hint::black_box(kept)
}

/// CRC-32/ISO-HDLC, bitwise. The algorithm Waymaker ships.
///
/// A direct copy of `waymaker_flash::crc::crc32`. Check value `0xCBF4_3926`.
#[inline(never)]
const fn crc32_iso_hdlc_bitwise_candidate(bytes: &[u8]) -> u32 {
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

/// CRC-32C (Castagnoli), bitwise. ADR 0010 rejects it.
///
/// A direct copy of `crates/waymaker-flash/tests/integrity.rs`'s `crc32c`. Check value
/// `0xE306_9283`.
#[inline(never)]
const fn crc32c_bitwise_candidate(bytes: &[u8]) -> u32 {
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

/// CRC-32C folded four bits at a time against [`CRC32C_NIBBLE_TABLE_CANDIDATE`].
///
/// A direct copy of `crates/waymaker-flash/tests/integrity.rs`'s `crc32c_nibble`. This is
/// the 64 B `.rodata` candidate ADR 0010's table names.
#[inline(never)]
fn crc32c_nibble_table_candidate(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for byte in bytes {
        for nibble in [u32::from(*byte) & 0xF, u32::from(*byte) >> 4] {
            let index = usize::try_from((crc ^ nibble) & 0xF).unwrap_or(0);
            crc = (crc >> 4) ^ table_entry(&CRC32C_NIBBLE_TABLE_CANDIDATE, index);
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// CRC-32C folded a byte at a time against [`CRC32C_BYTE_TABLE_CANDIDATE`].
///
/// A direct copy of `crates/waymaker-flash/tests/integrity.rs`'s `crc32c_byte`. This is
/// the 1024 B `.rodata` candidate ADR 0010's table names.
#[inline(never)]
fn crc32c_byte_table_candidate(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for byte in bytes {
        let index = usize::try_from((crc ^ u32::from(*byte)) & 0xFF).unwrap_or(0);
        crc = (crc >> 8) ^ table_entry(&CRC32C_BYTE_TABLE_CANDIDATE, index);
    }
    crc ^ 0xFFFF_FFFF
}

/// `table[index]`, or zero — which cannot happen, since every caller masks the index to
/// the table's own length. A helper because plain indexing is denied workspace-wide.
fn table_entry<const N: usize>(table: &[u32; N], index: usize) -> u32 {
    table.get(index).copied().unwrap_or(0)
}

/// The nibble table, 16 entries. **64 B of `.rodata`.**
static CRC32C_NIBBLE_TABLE_CANDIDATE: [u32; 16] = crc32c_fold_table();

/// The byte table, 256 entries. **1024 B of `.rodata`.**
static CRC32C_BYTE_TABLE_CANDIDATE: [u32; 256] = crc32c_fold_table();

/// A CRC-32C fold table of `N` entries, generated from the bitwise loop rather than typed
/// in, so a table cannot disagree with the polynomial it folds. `N` picks the width: 16
/// entries folds four bits at a time, 256 folds a byte.
///
/// A direct copy of `crates/waymaker-flash/tests/integrity.rs`'s `crc32c_table`.
#[allow(
    clippy::indexing_slicing,
    reason = "the index is the loop counter, bounded by the array's own length"
)]
#[allow(
    clippy::cast_possible_truncation,
    reason = "`N` is 16 or 256, so the entry index cannot exceed a u32; `try_from` is not \
              available in a const fn"
)]
const fn crc32c_fold_table<const N: usize>() -> [u32; N] {
    let mut table = [0_u32; N];
    let bits = if N == 16 { 4 } else { 8 };
    let mut index = 0;
    while index < N {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < bits {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0x82F6_3B78
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

/// CRC-16/CCITT-FALSE, bitwise. The algorithm Waymaker ships.
///
/// A direct copy of `waymaker_flash::crc::crc16`. Check value `0x29B1`.
#[inline(never)]
const fn crc16_ccitt_false_bitwise_candidate(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    let mut rest = bytes;
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

/// The three bitwise candidates reproduce their published check values.
///
/// A `const` assertion rather than a `#[test]`: this crate is `#![no_std]` `#![no_main]`
/// and is never run, so this is the only check that can run at all — at compile time,
/// which is free and cannot be skipped.
const _: () = {
    assert!(crc32_iso_hdlc_bitwise_candidate(b"123456789") == 0xCBF4_3926);
    assert!(crc32c_bitwise_candidate(b"123456789") == 0xE306_9283);
    assert!(crc16_ccitt_false_bitwise_candidate(b"123456789") == 0x29B1);
};
