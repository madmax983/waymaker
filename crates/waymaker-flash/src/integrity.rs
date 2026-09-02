//! The swap point for the record frame's integrity check.
//!
//! Design document §16's first deferred question is "whether the default integrity check is
//! CRC32C or a smaller table-free CRC implementation", and
//! [ADR 0010](https://github.com/madmax983/waymaker/blob/main/docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md)
//! settles it with measurements taken on `thumbv6m-none-eabi`: CRC-32/ISO-HDLC over the
//! header and payload, CRC-16/CCITT-FALSE over the header, both table-free. This module is
//! where that answer is *bound* rather than assumed — [`Catalogued`] is the binding, and
//! [`IntegrityCheck`] is what makes it one choice among possible ones.
//!
//! # Why there is a trait at all, when only one implementation ships
//!
//! Issue [#17](https://github.com/madmax983/waymaker/issues/17) requires the check to live
//! "behind a `waymaker-flash` trait or feature so the choice stays swappable", and ADR 0010
//! names the two things that would revisit its answer: a latency requirement §04 does not
//! yet state, and a record whose checksummed extent passes ~11.2 KiB. Both are answered by
//! a *different implementation of the same interface*. Behind a trait that is one type and
//! one `impl`; hard-wired to two free functions it is a change to the codec, and a codec
//! edited under deadline is a codec that stops sealing what it says it seals.
//!
//! A trait rather than a cargo feature on purpose. Cargo features are additive and unify
//! across a dependency graph, so two features naming two algorithms are two features that
//! can both be on — which for a wire format means a firmware that seals with one check and
//! verifies with another. A type parameter cannot be in two states at once.
//!
//! # What it costs
//!
//! Nothing, when it is not used. The methods take no `self` and every implementation is
//! expected to be a zero-sized marker, so [`crate::frame::encode`] and
//! [`crate::frame::decode`] are `#[inline]` wrappers over one monomorphisation and the
//! generated code is what it was before the trait existed. An unused second implementation
//! is not instantiated at all and costs no flash.
//!
//! # What may not be swapped
//!
//! The widths. `header_check` returns a [`u16`] and `frame_check` a [`u32`] because §09's
//! frame spends exactly two bytes and four bytes on them, and the frame layout is frozen
//! across format versions — see [`crate::frame::HEADER_CRC_BYTES`] and
//! [`crate::frame::FRAME_CRC_BYTES`]. That is the second half of what issue #17 asks to be
//! settled: the algorithm is swappable, the widths are not, and the signature is where the
//! difference is stated so that it cannot be got wrong by accident.
//!
//! # What an integrity check is not
//!
//! Authentication. §09: "CRC detects accidental corruption and torn writes; it is not
//! authentication." Anyone able to write the media can rewrite a record and reseal it, and
//! this crate will hand the result back as history — `waymaker-flash`'s
//! `a_resealed_forgery_is_accepted_because_a_crc_is_not_authentication` test says so as a
//! passing assertion. No implementation of this trait changes that, and none should be
//! documented as if it did: a signature scheme is a different mechanism with a different
//! key management problem, and it is out of scope for the whole of rung 0.

use crate::crc::{crc16, crc32};

/// The two checksums a record frame is sealed with.
///
/// One implementation ships — [`Catalogued`] — and it is the one ADR 0010 settled on. A
/// second is a superseding ADR, because a checksum is part of the wire format: a device
/// reflashed with a different implementation does not read journals its previous firmware
/// wrote, and refuses them at the first record rather than misreading them.
///
/// # Implementing it
///
/// Both methods must be pure and total: same bytes, same answer, for every `&[u8]`
/// including the empty one, on every target. A check that consulted a clock, a peripheral,
/// or any state at all would make a journal unverifiable by the device that wrote it.
///
/// Neither takes `self`, so an implementation is a marker type and never a value with
/// configuration in it. That is what keeps the swap free: there is nothing to store, nothing
/// to pass, and nothing to keep in sync between a writer and a reader.
///
/// [`crate::frame::Scan`] carries its implementation as a type parameter and derives
/// `Clone`, `Debug`, `PartialEq` and `Eq`, so a marker meant to be used with the scan
/// derives them too.
pub trait IntegrityCheck {
    /// The seal over the header's first ten bytes.
    ///
    /// Sixteen bits, because it is paid on every record and covers ten bytes — and because
    /// §09's frame has two bytes there and the header layout is frozen across format
    /// versions.
    ///
    /// # Postconditions
    ///
    /// Pure and total. Two headers differing in any field must be overwhelmingly unlikely
    /// to share an answer, and a run of leading zero bytes must change it: a partially
    /// programmed header reads back as zeroes, and a check that ignored them would accept
    /// one as a shorter header.
    fn header_check(bytes: &[u8]) -> u16;

    /// The seal over the header and the payload together.
    ///
    /// Thirty-two bits, and over both parts rather than the payload alone: a payload cannot
    /// then be transplanted onto another frame's header and still check out, and a record
    /// with an empty payload still gets a seal that depends on which record it is.
    ///
    /// # Postconditions
    ///
    /// Pure and total. This is also the definition of the digest a scheduled effect records
    /// for its input — see [`crate::frame::input_digest_with`] — so an implementation that
    /// seals frames one way and digests inputs another is not expressible here, which is
    /// the point.
    fn frame_check(bytes: &[u8]) -> u32;
}

/// The integrity check this firmware writes and reads: the one ADR 0010 settled on.
///
/// CRC-16/CCITT-FALSE over the header — polynomial `0x1021`, initial value `0xFFFF`, no
/// reflection, no final xor, published check value `0x29B1` — and CRC-32/ISO-HDLC over the
/// header and payload — reflected polynomial `0xEDB8_8320`, initial value and final xor
/// `0xFFFF_FFFF`, published check value `0xCBF4_3926`. Both computed bitwise, with no
/// lookup table.
///
/// Both are catalogued algorithms rather than something invented here, which is the
/// property ADR 0010 chose them for: a journal pulled off a device is verifiable with a
/// tool nobody had to write, and CRC-32/ISO-HDLC is the one zlib, gzip and PNG use. A
/// checksum verified only against the implementation that produced it is a checksum that
/// agrees with its own bugs.
///
/// The `integrity-check` gate rule fails a pull request that changes either polynomial or
/// initial value, that adds a lookup table to the checksum module, or that binds this type
/// to anything but those two functions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Catalogued;

impl IntegrityCheck for Catalogued {
    #[inline]
    fn header_check(bytes: &[u8]) -> u16 {
        crc16(bytes)
    }

    #[inline]
    fn frame_check(bytes: &[u8]) -> u32 {
        crc32(bytes)
    }
}
