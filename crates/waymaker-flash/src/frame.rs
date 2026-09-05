//! The record frame: handwritten, fixed-endian, self-delimiting, bounds-validated.
//!
//! Design document §09. This is the whole of the wire format — everything the kernel is
//! forbidden to own and everything a journal on media actually consists of.
//!
//! ```text
//! offset  size  field           notes
//!      0     2  magic           u16 LE, `MAGIC`, reads as b"WM" on media
//!      2     1  format_version  u8, `FORMAT_VERSION`
//!      3     1  record_kind     u8, a `RecordKind` number
//!      4     4  effect_seq      u32 LE, zero on a run-scoped record
//!      8     2  payload_len     u16 LE
//!     10     2  header_crc      u16 LE, CRC-16 over offsets 0..10
//!     12     N  payload         the record body, N = payload_len
//!   12+N     4  frame_crc       u32 LE, CRC-32 over offsets 0..12+N
//!   16+N     -  padding         to the device's program alignment, written 0xFF
//! ```
//!
//! # What this module owns
//!
//! The bytes, and only the bytes: the constants above, the seal widths, [`encode`],
//! [`decode`], [`input_digest`] and the append [`Scan`] that walks a journal, each with a
//! sibling generic over the integrity check.
//!
//! It does **not** own the checksums. Which algorithm seals a frame is
//! [`crate::integrity`]'s, behind the [`IntegrityCheck`] trait, and everything here takes
//! it as a type parameter defaulted to [`Catalogued`] — see
//! [ADR 0012](https://github.com/madmax983/waymaker/blob/main/docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md).
//! The *widths* are still this module's, because they are positions in the frame.
//! The meaning of a record — what a completion is, which number a kind wears — belongs to
//! `waymaker-core`, which is why [`RecordKind`] and [`RecordRef`] are imported rather than
//! declared here.
//!
//! # What this module must not own
//!
//! Activities, workflow types, timers, Embassy, and the commit seal. The seal is a
//! storage-program unit rather than a field, so it belongs with the barrier protocol at
//! rung 0.2 — see [Deferred](#deferred) below.
//!
//! # Why there are two checksums
//!
//! Because §09 requires that "all lengths are validated against caller buffers and bank
//! bounds *before* reading", and `payload_len` is itself read out of the bytes being
//! validated. `header_crc` covers the twelve-byte header alone, so `payload_len` is known
//! to be the number the writer wrote *before* it is used to work out where the frame ends.
//! A single checksum over the whole frame could not do that: finding it would mean
//! trusting the length first.
//!
//! `frame_crc` then covers the header *and* the payload rather than the payload alone.
//! That costs twelve extra bytes of checksumming per record and buys two things: a payload
//! cannot be transplanted onto another frame's header and still check out, and a record
//! with an empty payload gets a checksum that depends on which record it is — CRC-32 of
//! nothing is zero, and a field that is zero for a whole class of records is a field a
//! zeroed page satisfies. §09 names the field `payload_crc`; this is that field, covering
//! strictly more than its name implies.
//!
//! # Why the header layout is frozen across format versions
//!
//! [`decode`] checks the header's checksum *before* it checks `format_version`. That is
//! only sound if every version of this format puts the same twelve bytes at the front,
//! which is the commitment this module makes: the header is frozen, and a version bump may
//! change what a payload means but not where the frame ends. Without that, a reader
//! meeting a version it does not know could not even say how far to skip, and §09's
//! forward-compatibility rule would have nothing to stand on.
//!
//! # Deferred
//!
//! Two things §09 names are deliberately absent, and are absent visibly rather than
//! silently:
//!
//! * **The commit seal.** §09's frame ends with a `commit_seal` — a storage-program unit
//!   written after a barrier, which is what makes a frame *committed* rather than merely
//!   present. It needs the [`StableStorage`] barrier protocol, which arrives with rung
//!   0.2. Until then [`Scan`] treats a frame whose checksums hold as history, so a torn
//!   write at the tail of a journal is indistinguishable from corruption there. Both stop
//!   the scan in the same place, which is what §14 requires either way — "frame ignored;
//!   previous history prefix wins" — so the deferral costs correctness nothing today. What
//!   the seal will add is the ability to say *which* of the two happened.
//! * **Bank bounds.** [`Scan`] is handed a `&[u8]`, and that slice *is* the bound: there is
//!   no offset arithmetic against a geometry. That is still true and is now a division of
//!   labour rather than a deferral: [`crate::bank`] owns the layout, so a caller asks a
//!   [`BankLayout`](crate::bank::BankLayout) where a bank is and a
//!   [`BankHeader`](crate::bank::BankHeader) where its journal starts, and hands the scan
//!   the slice between them.
//!
//! [`StableStorage`]: https://github.com/madmax983/waymaker/blob/main/docs/design/waymaker-design-v0.2.html

use core::marker::PhantomData;

use waymaker_core::{ActivityKind, DecodeError, EffectSeq, RecordKind, RecordRef};

use crate::crc::crc32;
use crate::integrity::{Catalogued, IntegrityCheck};

/// The two bytes every frame begins with: `0x57 0x4D`, which reads as `WM` on media.
///
/// Neither `0x0000` nor `0xFFFF`, so that a zeroed page and an erased one are both
/// rejected by the very first check rather than by a checksum further in.
pub const MAGIC: u16 = 0x4D57;

/// The only format version this firmware writes, and the only one it reads.
pub const FORMAT_VERSION: u8 = 1;

/// Bytes before the payload: magic, version, kind, sequence, length and the header
/// checksum.
pub const HEADER_BYTES: usize = 12;

/// Bytes after the payload: the frame checksum.
pub const TRAILER_BYTES: usize = 4;

/// Width of `header_crc` on media, in bytes.
///
/// Issue [#17](https://github.com/madmax983/waymaker/issues/17) asks for the two seal
/// widths to be settled as a result of the algorithm choice, and this is the header's.
/// Sixteen bits rather than thirty-two because it is paid on every record and covers ten
/// bytes: [ADR 0010](https://github.com/madmax983/waymaker/blob/main/docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md)
/// gives the reasoning, and
/// [ADR 0012](https://github.com/madmax983/waymaker/blob/main/docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md)
/// is what freezes it.
///
/// It is the width of [`IntegrityCheck::header_check`]'s return type. The trait's signature
/// is what fixes that, the `integrity-check` gate rule is what holds the two together — the
/// return type is not an associated constant and `header_check` is not `const`, so there is
/// no compile-time link to be had — and the assertions at the foot of this module check that
/// the frame layout still spends exactly this many bytes on it.
pub const HEADER_CRC_BYTES: usize = size_of::<u16>();

/// Width of the frame checksum on media, in bytes — §09 names the field `payload_crc`.
///
/// Thirty-two bits, over the header *and* the payload. The width is what
/// [`IntegrityCheck::frame_check`] returns, and it is the whole of [`TRAILER_BYTES`]: a
/// frame's last four bytes are its seal and nothing else.
pub const FRAME_CRC_BYTES: usize = size_of::<u32>();

/// Everything in a frame that is not payload.
pub const FRAME_OVERHEAD_BYTES: usize = HEADER_BYTES + TRAILER_BYTES;

/// The longest payload `payload_len` can describe.
///
/// A `RunStarted` input is capped four bytes lower, because the workflow identity is part
/// of its payload.
///
/// # This is a format ceiling, not a firmware one
///
/// [`MAX_FRAME_BYTES`] is 65 551 B and design document §04 states the runtime RAM budget
/// with a **512 B** scratch page, so the frame a device can actually stage is roughly two
/// orders of magnitude smaller than the one `payload_len` can describe. Nothing here
/// asserts a relationship between the two, and deliberately so: the wire format is frozen
/// for every device Waymaker will ever run on, and the staging buffer is one device's
/// geometry. What a caller hits first is the buffer it passed to [`encode`], which refuses
/// with [`DecodeError::LengthOutOfBounds`] long before this number is in reach.
pub const MAX_PAYLOAD_BYTES: usize = u16::MAX as usize;

/// The longest frame there can be, before padding.
pub const MAX_FRAME_BYTES: usize = FRAME_OVERHEAD_BYTES + MAX_PAYLOAD_BYTES;

/// What an erased NOR cell reads back as, and therefore what padding is written with.
///
/// Programming `0xFF` over an erased cell changes no bits, so padding costs the device
/// nothing beyond the program cycle the alignment already required.
pub const ERASED_BYTE: u8 = 0xFF;

/// Format versions at which a reader may skip a record kind it does not know.
///
/// §09: "Unknown record kinds are skippable only when the format version permits forward
/// compatibility." Version 1 does not permit it, so this list is empty — and empty is the
/// rule rather than an oversight. Skipping a record means asserting that the rest of
/// history means the same thing without it, and at v0.1 that is false for every record in
/// §09's table: a skipped `TimerFired` is a timer replay believes never fired.
///
/// Three things have to change together for a version to grant skipping, and they are in
/// three different places: [`decode`] must accept the version at all — it refuses anything
/// but [`FORMAT_VERSION`], so a version named here that `decode` rejects can never be
/// reached — this list must name it, and [`Scan`] must grow the arm that advances past the
/// frame. The `const` assertion below is what stops one of the three happening on its own: a
/// version added here is a compile error that names the other two.
const VERSIONS_PERMITTING_UNKNOWN_RECORD_SKIP: &[u8] = &[];

// A rule that can be half-enabled is a rule that gets half-enabled. `Scan` deliberately has
// no skip arm — a branch that cannot run is a branch whose first execution is recovery after
// a power loss — so the day this list stops being empty is the day that arm, the test that
// reaches it, and `decode`'s acceptance of the version all have to arrive. This assertion is
// how that day announces itself, at compile time, rather than as a journal read wrong.
const _: () = assert!(
    VERSIONS_PERMITTING_UNKNOWN_RECORD_SKIP.is_empty(),
    "a format version now permits skipping an unknown record kind, so `Scan` needs the arm \
     that advances past one, a test that reaches it, and `decode` must accept that version",
);

/// Whether a reader of a journal written at `version` may skip a record kind it does not
/// know.
///
/// The rule §09 states, as a function rather than as a paragraph, so that it can be checked
/// over every value a version byte can hold rather than at the one value this firmware
/// writes.
///
/// [`Scan`] does not branch on it today, and that is the point: the answer is the same for
/// every version, so the scan stops on a kind it does not know and there is no untested arm
/// in it. This is where the rule *lives* — a reader deciding whether a journal is safe to
/// walk with older firmware asks here — and it is what a future format version changes,
/// alongside the arm in [`Scan`] and the test that reaches it.
///
/// # Postconditions
///
/// `false` for every `version` at rung 0.1, [`FORMAT_VERSION`] included. Skipping a record
/// asserts that the rest of history means the same thing without it, and at v0.1 that is
/// false for every record in §09's table: a skipped `TimerFired` is a timer replay believes
/// never fired.
#[must_use]
pub const fn permits_unknown_record_skip(version: u8) -> bool {
    // Walked with `split_first` rather than `<[u8]>::contains`, which is not `const`. The
    // list is empty today, so this returns `false` without looking at anything; when a
    // version grants skipping it is one entry and one iteration.
    let mut rest = VERSIONS_PERMITTING_UNKNOWN_RECORD_SKIP;
    while let Some((permitted, tail)) = rest.split_first() {
        if *permitted == version {
            return true;
        }
        rest = tail;
    }
    false
}

/// How far the payload of the two fixed-shape records extends.
const RUN_STARTED_PREFIX_BYTES: usize = 4;
const EFFECT_SCHEDULED_BODY_BYTES: usize = 8;

/// The device's program granularity: a power of two, and never zero.
///
/// §12 gives the storage adapter a `program_size: u16`, and §09 requires records padded to
/// it. Zero is rejected once here rather than guarded at every use — a zero alignment is not
/// "no padding", it is a division by zero, and [`BYTE`](Self::BYTE) is what "no padding" is
/// spelled as.
///
/// # Why a power of two is required
///
/// An earlier version of this type accepted any non-zero `u16`, on the reasoning that
/// nothing in the arithmetic needed the restriction and a type that refused a device's
/// honest geometry is a type a driver has to lie to. That reasoning was wrong about the
/// arithmetic, and the measurement says so: rounding up with `%` and `-` divides by a
/// runtime `u16`, and `thumbv6m-none-eabi` — Cortex-M0 and M0+ — has no divide instruction.
/// The linker pulls in `compiler_builtins`' software division, and it is not small: around
/// 430 B of an 8 KiB budget for the whole kernel and this adapter, spent on generality that
/// no flash device asks for. It also leaves a live divide-by-zero panic branch, because the
/// non-zero invariant is not visible to the optimiser across a call boundary.
///
/// A power of two rounds up by mask — `(len + unit - 1) & !(unit - 1)` — which is three
/// instructions, cannot trap, and has no panic path at all. Every program size real flash
/// reports is a power of two, so the restriction costs a driver nothing and the generality
/// cost 430 B and a panic branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ProgramAlign(u16);

impl ProgramAlign {
    /// One byte: every length is already aligned, so nothing is padded.
    pub const BYTE: Self = Self(1);

    /// A program granularity of `bytes`, or [`None`] unless `bytes` is a power of two.
    ///
    /// # Postconditions
    ///
    /// `Some` exactly when `bytes` is a power of two — which excludes zero — and `get()`
    /// returns what was passed. The largest accepted value is `32_768`, the largest power of
    /// two a `u16` holds.
    #[must_use]
    pub const fn new(bytes: u16) -> Option<Self> {
        // `is_power_of_two` is false for zero, so this is the whole invariant in one check.
        if bytes.is_power_of_two() {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// The granularity, in bytes. Never zero.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// `len` rounded up to a whole number of program units, or [`None`] on overflow.
    ///
    /// # Postconditions
    ///
    /// The result is a multiple of [`get`](Self::get), is greater than or equal to `len`,
    /// and is less than `len + get()`. [`None`] rather than a wrap: a padded length that
    /// came back *smaller* than the frame it pads would be a writer told it had room it
    /// does not have.
    ///
    /// Rounded by mask rather than by `%`, which is what the power-of-two invariant is for:
    /// an add, a not and an and, with no division to link and no divide-by-zero branch to
    /// leave lying in a firmware image.
    #[must_use]
    pub const fn round_up(self, len: usize) -> Option<usize> {
        // `self.0` is a power of two, so `mask` is the low bits below it and `!mask` clears
        // exactly them. Subtracting one cannot underflow because zero is not a power of two.
        let mask = (self.0 as usize).wrapping_sub(1);
        match len.checked_add(mask) {
            Some(sum) => Some(sum & !mask),
            None => None,
        }
    }
}

/// What a frame turned out to hold.
///
/// The distinction is §09's forward-compatibility rule made into a type. A frame whose
/// kind this firmware does not know is not the same thing as a frame that failed to
/// decode: its header checksum held, its frame checksum held, and its length is known, so
/// a reader can say exactly where it ends. Whether a reader may then *skip* it is a
/// property of the format version, and [`Scan`] is where that decision is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoded<'a> {
    /// A record this firmware understands.
    Record(RecordRef<'a>),
    /// A structurally sound frame wearing a record kind this firmware does not know.
    UnknownKind(RecordKind),
}

/// One decoded frame, and how much of the input it occupied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// The version byte the frame declared. Always [`FORMAT_VERSION`] today, because
    /// [`decode`] refuses anything else.
    pub format_version: u8,
    /// What the frame held.
    pub decoded: Decoded<'a>,
    /// Bytes the frame occupies, *before* padding: `FRAME_OVERHEAD_BYTES + payload_len`.
    ///
    /// Padding is a property of the device the journal was written on rather than of the
    /// frame, so it is not counted here — [`ProgramAlign::round_up`] is what turns this
    /// into a stride, and [`Scan`] is what applies it.
    ///
    /// # This is not a cursor advance
    ///
    /// [`encode`] returns the *padded* length and this is the *unpadded* one, so
    /// `offset += frame.frame_len` lands a reader inside the pad of the frame it just
    /// read. That is why the field is not called `encoded_len`, and why [`Scan`] exists:
    /// applying the stride needs the device's program granularity, which is not on media
    /// and is therefore not something a single frame can tell you.
    pub frame_len: usize,
}

/// The digest a schedule record carries for the activity input it was scheduled with.
///
/// [`waymaker_core::RecordRef::EffectScheduled`] records an
/// activity input as a length and a digest rather than as bytes — §07 orders a durable
/// intent before the effect, and §08 compares what replay asks for against what history
/// recorded, for which a length and a digest are enough. That only works if both sides
/// compute the digest the same way, so there is exactly one definition of it and this is
/// it: the same CRC-32/ISO-HDLC the frame is sealed with.
///
/// Without this the field would be a `u32` the codec faithfully moves from one side of the
/// journal to the other with nothing able to produce a right value for it, and a divergence
/// check that compares two numbers nobody computed is a check that passes.
///
/// # Postconditions
///
/// Pure and total. `input_digest(&[])` is the CRC of the empty input, which is a fixed
/// number — a scheduled activity with no input has a digest like any other, and comparing
/// it is still the §08 check.
#[must_use]
pub const fn input_digest(input: &[u8]) -> u32 {
    crc32(input)
}

/// The digest a schedule record carries, under a chosen integrity check.
///
/// [`input_digest`] is this at `C = Catalogued`, kept separate because it is a `const fn`
/// and a trait method cannot be one — a golden digest in a test or a table in firmware
/// still costs nothing at runtime.
///
/// The two must agree, and
/// [`an_input_digest_is_the_frame_check_of_whatever_seals_the_frame`] is the test that says
/// so: a build that sealed frames with one check and digested activity inputs with another
/// would record a digest no replay of it could reproduce, and §08's divergence comparison
/// would fail on every effect.
///
/// # Postconditions
///
/// Pure and total, exactly as [`IntegrityCheck::frame_check`] is.
///
/// [`an_input_digest_is_the_frame_check_of_whatever_seals_the_frame`]: https://github.com/madmax983/waymaker/blob/main/crates/waymaker-flash/tests/integrity.rs
#[must_use]
#[inline]
pub fn input_digest_with<C: IntegrityCheck>(input: &[u8]) -> u32 {
    C::frame_check(input)
}

/// Bytes `record` occupies once written and padded to `align`.
///
/// This is what a writer reserves before it commits to appending, so that §10's "the
/// runtime never overwrites committed history to make room" can be decided before any
/// bytes move.
///
/// # Errors
///
/// [`DecodeError::LengthOutOfBounds`] when the record's payload is longer than
/// [`MAX_PAYLOAD_BYTES`], which `payload_len` could not describe, or when rounding up to
/// `align` would overflow. A `DecodeError` on a path that does not decode anything is
/// deliberate: its `LengthOutOfBounds` is documented as "a length field points past the
/// buffer *or the caller-owned output*", which is this, and a second error enum for two
/// cases would be a second vocabulary for adapters to translate between.
pub fn encoded_len(record: &RecordRef<'_>, align: ProgramAlign) -> Result<usize, DecodeError> {
    let payload_len = payload_len(record)?;
    align
        .round_up(FRAME_OVERHEAD_BYTES.saturating_add(payload_len))
        .ok_or(DecodeError::LengthOutOfBounds)
}

/// Writes `record` into `out` as a frame padded to `align`, returning the bytes written.
///
/// # Postconditions
///
/// On success the return value is [`encoded_len`], every byte of it in `out` has been
/// written, and `decode` of those bytes yields `record` back. On failure **nothing is
/// written**: the length check happens before the first byte, because a partial frame left
/// in a staging buffer is a frame a later flush could program.
///
/// # Errors
///
/// [`DecodeError::LengthOutOfBounds`] when `out` is shorter than [`encoded_len`], or when
/// the record cannot be encoded at all — see [`encoded_len`].
///
/// Those two are one error on purpose, and they are told apart by asking [`encoded_len`]
/// first: a record that cannot be encoded fails there too, and a record that succeeds there
/// and fails here needs a bigger buffer. A caller that retries needs the difference — the
/// second case is worth retrying and the first never is — and one call already gives it,
/// so a second error variant would be a second vocabulary for adapters to translate between
/// and no more information.
#[inline]
pub fn encode(
    record: &RecordRef<'_>,
    align: ProgramAlign,
    out: &mut [u8],
) -> Result<usize, DecodeError> {
    encode_with::<Catalogued>(record, align, out)
}

/// Writes `record` into `out` as a frame sealed with `C`, returning the bytes written.
///
/// [`encode`] is this at `C = Catalogued`, which is the check ADR 0010 settled on and the
/// only one this firmware writes. A `C` that computes different seals writes a journal this
/// firmware refuses at its first record: swapping to one is a wire-format change, not a
/// configuration one. A `C` that computes the *same* seals by another route — ADR 0010's
/// predicted nibble table — writes byte-identical frames, which is why that one would not
/// be.
///
/// # Postconditions
///
/// As [`encode`], and additionally: the header's last [`HEADER_CRC_BYTES`] bytes are
/// `C::header_check` of the ten before them, and the frame's last [`FRAME_CRC_BYTES`] are
/// `C::frame_check` of everything before them. Nothing else in the frame depends on `C`.
///
/// # Errors
///
/// As [`encode`].
pub fn encode_with<C: IntegrityCheck>(
    record: &RecordRef<'_>,
    align: ProgramAlign,
    out: &mut [u8],
) -> Result<usize, DecodeError> {
    let payload_len = payload_len(record)?;
    let frame_len = FRAME_OVERHEAD_BYTES.saturating_add(payload_len);
    let padded = align
        .round_up(frame_len)
        .ok_or(DecodeError::LengthOutOfBounds)?;

    // Split once, before a byte moves: the whole length check for the caller's buffer, and
    // the reason a failed encode leaves `out` untouched rather than half a frame a later
    // flush could program. Everything past `padded` is the caller's and is not written.
    let Some((frame, _beyond)) = out.split_at_mut_checked(padded) else {
        return Err(DecodeError::LengthOutOfBounds);
    };

    let body = body(record);
    let sequence = match record {
        RecordRef::EffectScheduled { seq, .. }
        | RecordRef::EffectCompleted { seq, .. }
        | RecordRef::EffectFailed { seq, .. } => seq.0,
        // A run-scoped record has no effect to number, and the decoder insists on the
        // zero: two byte sequences decoding to one record is a format that cannot be
        // reasoned about by looking at it.
        RecordRef::RunStarted { .. }
        | RecordRef::RunCompleted { .. }
        | RecordRef::RunFailed { .. } => 0,
    };
    let length = u16::try_from(payload_len).map_err(|_| DecodeError::LengthOutOfBounds)?;

    // Written out field by field rather than assembled through a cursor. Every byte's
    // position is visible at a glance and comparable with the table at the top of this
    // module, and none of it can fail: an array literal has no length to get wrong and no
    // bounds check to answer for.
    let [magic_low, magic_high] = MAGIC.to_le_bytes();
    let [seq0, seq1, seq2, seq3] = sequence.to_le_bytes();
    let [len_low, len_high] = length.to_le_bytes();
    let sealed_header: [u8; HEADER_BYTES - 2] = [
        magic_low,
        magic_high,
        FORMAT_VERSION,
        record.kind().0,
        seq0,
        seq1,
        seq2,
        seq3,
        len_low,
        len_high,
    ];
    let [crc_low, crc_high] = C::header_check(&sealed_header).to_le_bytes();
    let header: [u8; HEADER_BYTES] = [
        magic_low,
        magic_high,
        FORMAT_VERSION,
        record.kind().0,
        seq0,
        seq1,
        seq2,
        seq3,
        len_low,
        len_high,
        crc_low,
        crc_high,
    ];

    // Header, then body, then the pad. The frame checksum is computed from the bytes that
    // landed rather than from the values that produced them, so a write that went astray
    // cannot be sealed as if it had not.
    for (slot, byte) in frame.iter_mut().zip(
        header
            .into_iter()
            .chain(body.prefix.into_iter().take(body.prefix_len))
            .chain(body.tail.iter().copied()),
    ) {
        *slot = byte;
    }
    let covered = HEADER_BYTES.saturating_add(payload_len);
    // `ok_or` rather than `unwrap_or_default`. The slice is `padded >= frame_len > covered`
    // bytes long so this cannot fail, but the fail-open spelling would seal the frame with
    // `crc32(&[])` — and `crc32` of nothing is a fixed number, so a bug that reached it
    // would produce a frame that looks checksummed rather than one that is refused.
    let sealed = frame.get(..covered).ok_or(DecodeError::LengthOutOfBounds)?;
    let frame_crc = C::frame_check(sealed);
    for (slot, byte) in frame.iter_mut().skip(covered).zip(frame_crc.to_le_bytes()) {
        *slot = byte;
    }
    for slot in frame.iter_mut().skip(frame_len) {
        *slot = ERASED_BYTE;
    }

    Ok(padded)
}

/// Reads the frame at the front of `bytes`.
///
/// Trailing bytes are ignored: a journal is a run of frames, so `bytes` is normally the
/// rest of the bank and [`Frame::frame_len`] is what says where this record stopped.
///
/// # Postconditions
///
/// Every length is validated against `bytes` before it is used to read anything, in this
/// order: the header must be present; the magic must match; the header checksum must hold,
/// which is what makes `payload_len` trustworthy; the format version must be one this
/// firmware reads; the frame the header describes must be present in full; the frame
/// checksum must hold; and only then is the body interpreted. A record view returned from
/// here borrows `bytes`, so no payload is copied and nothing is allocated.
///
/// # Errors
///
/// * [`DecodeError::Truncated`] — `bytes` is shorter than the header, or shorter than the
///   frame the header describes.
/// * [`DecodeError::IntegrityFailed`] — the magic, the header checksum or the frame
///   checksum did not match.
/// * [`DecodeError::UnsupportedFormatVersion`] — a version this firmware does not read.
/// * [`DecodeError::MalformedRecord`] — the frame is intact and its body does not fit the
///   kind in its header.
///
/// [`DecodeError::UnknownRecordKind`] is *not* among them: an unrecognised kind on an
/// otherwise sound frame is [`Decoded::UnknownKind`], because the frame is still
/// self-delimiting and the decision about skipping it belongs to [`Scan`].
#[inline]
pub fn decode(bytes: &[u8]) -> Result<Frame<'_>, DecodeError> {
    decode_with::<Catalogued>(bytes)
}

/// Reads the frame at the front of `bytes`, verifying it against `C`.
///
/// [`decode`] is this at `C = Catalogued`. A frame whose seals were computed by a different
/// algorithm is refused with [`DecodeError::IntegrityFailed`] — at the header when the
/// header check differs, and at the trailer otherwise — which is what makes a firmware
/// reflashed with another check refuse an old journal loudly rather than walk it wrong.
/// "Refused" is a CRC's kind of certainty rather than a proof: a wrong header seal collides
/// with the right one about once in 2^16, and the frame seal behind it about once in 2^32.
///
/// # Postconditions
///
/// As [`decode`], with `C::header_check` and `C::frame_check` in place of the shipped pair.
///
/// # Errors
///
/// As [`decode`].
pub fn decode_with<C: IntegrityCheck>(bytes: &[u8]) -> Result<Frame<'_>, DecodeError> {
    // The header first, and through the one function that verifies one. Everything below
    // this line reads a length that the writer is known to have written.
    let header = verify_header_with::<C>(bytes)?;
    // Both sums are bounded by `MAX_FRAME_BYTES`, so neither can overflow a `usize` on any
    // target this crate builds for; `saturating_add` says so without depending on it.
    let covered = HEADER_BYTES.saturating_add(header.payload_len);
    let frame_len = covered.saturating_add(TRAILER_BYTES);
    let (Some(sealed), Some(trailer)) = (
        bytes.get(..covered),
        bytes
            .get(covered..)
            .and_then(<[u8]>::first_chunk::<TRAILER_BYTES>),
    ) else {
        return Err(DecodeError::Truncated);
    };
    if C::frame_check(sealed) != u32::from_le_bytes(*trailer) {
        return Err(DecodeError::IntegrityFailed);
    }

    let Some(payload) = sealed.get(HEADER_BYTES..) else {
        // Unreachable: `sealed` is `covered` bytes long and `covered >= HEADER_BYTES`.
        // Spelled as a refusal rather than an `unwrap` because the workspace denies both
        // `unwrap` and `panic`, and a decoder walking bytes off a damaged device is the
        // last place to make an exception.
        return Err(DecodeError::Truncated);
    };
    let decoded = decode_body(header.kind, header.seq, payload)?;

    Ok(Frame {
        format_version: header.format_version,
        decoded,
        frame_len,
    })
}

/// A frame header whose own seal has held, with the three fields the rest of the frame is
/// read against.
///
/// Private on purpose: [`Frame`] is what a caller gets, and a partly decoded frame is a
/// value nothing outside this module has a use for. What *is* public is the one number a
/// reader with a page smaller than the journal needs — see [`frame_len_of`].
#[derive(Clone, Copy)]
struct VerifiedHeader {
    format_version: u8,
    kind: RecordKind,
    seq: EffectSeq,
    payload_len: usize,
}

/// Reads the twelve-byte header at the front of `bytes` and verifies it against `C`.
///
/// The one place a header's seal is computed. [`decode_with`] and [`frame_len_of_with`]
/// both come through here rather than each destructuring twelve bytes for themselves,
/// because two readers of one header is exactly the drift §09's frozen layout exists to
/// rule out.
///
/// # Errors
///
/// [`DecodeError::Truncated`] when `bytes` is shorter than a header,
/// [`DecodeError::IntegrityFailed`] when the magic or the header seal does not hold, and
/// [`DecodeError::UnsupportedFormatVersion`] for a version this firmware does not read —
/// in that order, which is the order §09 requires: the header layout is frozen across
/// format versions, so its checksum is meaningful before its version is known.
fn verify_header_with<C: IntegrityCheck>(bytes: &[u8]) -> Result<VerifiedHeader, DecodeError> {
    // `first_chunk` gives a `&[u8; 12]`, which destructures. Every header field is then
    // named rather than offset-counted, and the read cannot fail once the chunk is in
    // hand — so the only bounds check on this path is the one that decides whether a
    // header is there at all, which is the check §09 asks to come first.
    let Some(header) = bytes.first_chunk::<HEADER_BYTES>() else {
        return Err(DecodeError::Truncated);
    };
    let [
        magic_low,
        magic_high,
        version,
        kind,
        seq0,
        seq1,
        seq2,
        seq3,
        len_low,
        len_high,
        crc_low,
        crc_high,
    ] = *header;
    let sealed_header: [u8; HEADER_BYTES - 2] = [
        magic_low, magic_high, version, kind, seq0, seq1, seq2, seq3, len_low, len_high,
    ];

    if u16::from_le_bytes([magic_low, magic_high]) != MAGIC {
        return Err(DecodeError::IntegrityFailed);
    }
    if C::header_check(&sealed_header) != u16::from_le_bytes([crc_low, crc_high]) {
        return Err(DecodeError::IntegrityFailed);
    }
    // Only now is the length a number the writer wrote rather than a number that was
    // found, and only now is the version worth reading: the header layout is frozen across
    // format versions, so its checksum is meaningful before its version is known.
    if version != FORMAT_VERSION {
        return Err(DecodeError::UnsupportedFormatVersion);
    }

    Ok(VerifiedHeader {
        format_version: version,
        kind: RecordKind(kind),
        seq: EffectSeq(u32::from_le_bytes([seq0, seq1, seq2, seq3])),
        payload_len: usize::from(u16::from_le_bytes([len_low, len_high])),
    })
}

/// How long the frame at the front of `header` is, before padding, read from its header
/// alone.
///
/// This is what §09's two checksums are *for*, made usable. `header_crc` covers the twelve
/// bytes that include `payload_len`, so a reader can learn where a frame ends without
/// having the frame: the length it gets back is one the writer is known to have written,
/// rather than one that was found in whatever the media happens to hold.
///
/// A recovery whose scratch page is smaller than the journal needs exactly that. It has to
/// decide how many bytes to stage *before* it stages them, and a reader that trusted an
/// unverified `payload_len` would be one an erased page could send anywhere.
///
/// # Postconditions
///
/// [`FRAME_OVERHEAD_BYTES`] plus the payload length the header declares, which is
/// [`Frame::frame_len`] for the same bytes and never more than [`MAX_FRAME_BYTES`]. Reads
/// at most [`HEADER_BYTES`] bytes of `header` and nothing past them. Padding is not
/// included, for the reason [`Frame::frame_len`] does not include it: it is a property of
/// the device rather than of the frame.
///
/// # Errors
///
/// [`DecodeError::Truncated`] when `header` is shorter than [`HEADER_BYTES`],
/// [`DecodeError::IntegrityFailed`] when the magic or the header checksum does not hold,
/// and [`DecodeError::UnsupportedFormatVersion`] for a version this firmware cannot read.
#[inline]
pub fn frame_len_of(header: &[u8]) -> Result<usize, DecodeError> {
    frame_len_of_with::<Catalogued>(header)
}

/// How long the frame at the front of `header` is, verified against `C`.
///
/// [`frame_len_of`] is this at `C = Catalogued`. A header sealed by another algorithm is
/// refused with [`DecodeError::IntegrityFailed`] rather than yielding a length taken from
/// bytes nothing checked.
///
/// # Postconditions
///
/// As [`frame_len_of`], with `C::header_check` in place of the shipped one.
///
/// # Errors
///
/// As [`frame_len_of`].
pub fn frame_len_of_with<C: IntegrityCheck>(header: &[u8]) -> Result<usize, DecodeError> {
    let verified = verify_header_with::<C>(header)?;
    // Bounded by `MAX_FRAME_BYTES`, so this cannot overflow on any target this crate builds
    // for; `saturating_add` says so without depending on it.
    Ok(FRAME_OVERHEAD_BYTES.saturating_add(verified.payload_len))
}

/// Interprets a verified payload under the kind its header declared.
///
/// Reached only once both checksums have held, so every refusal here is about the record
/// rather than about the bytes.
fn decode_body(
    kind: RecordKind,
    seq: EffectSeq,
    payload: &[u8],
) -> Result<Decoded<'_>, DecodeError> {
    // A run-scoped record has no effect to number. Checked once, for the three kinds that
    // share the rule, rather than three times.
    let run_scoped = matches!(
        kind,
        RecordKind::RUN_STARTED | RecordKind::RUN_COMPLETED | RecordKind::RUN_FAILED
    );
    if run_scoped && seq != EffectSeq::FIRST {
        return Err(DecodeError::MalformedRecord);
    }

    let record = match kind {
        RecordKind::RUN_STARTED => {
            let mut reader = Reader::new(payload);
            let (Some(workflow_kind), Some(workflow_version)) = (reader.u16(), reader.u16()) else {
                return Err(DecodeError::MalformedRecord);
            };
            RecordRef::RunStarted {
                workflow_kind,
                workflow_version,
                input: reader.rest(),
            }
        }
        RecordKind::EFFECT_SCHEDULED => {
            if payload.len() != EFFECT_SCHEDULED_BODY_BYTES {
                return Err(DecodeError::MalformedRecord);
            }
            let mut reader = Reader::new(payload);
            let (Some(kind), Some(input_len), Some(input_crc)) =
                (reader.u16(), reader.u16(), reader.u32())
            else {
                return Err(DecodeError::MalformedRecord);
            };
            RecordRef::EffectScheduled {
                seq,
                kind: ActivityKind(kind),
                input_len,
                input_crc,
            }
        }
        RecordKind::EFFECT_COMPLETED => RecordRef::EffectCompleted {
            seq,
            result: payload,
        },
        RecordKind::EFFECT_FAILED => RecordRef::EffectFailed {
            seq,
            error: payload,
        },
        RecordKind::RUN_COMPLETED => RecordRef::RunCompleted { result: payload },
        RecordKind::RUN_FAILED => RecordRef::RunFailed { error: payload },
        unknown => return Ok(Decoded::UnknownKind(unknown)),
    };
    Ok(Decoded::Record(record))
}

/// The payload length §09's `payload_len` field holds for `record`.
///
/// # Errors
///
/// [`DecodeError::LengthOutOfBounds`] when the payload is longer than a `u16` can
/// describe. `RunStarted` spends four of those bytes on the workflow identity, so its
/// input ceiling is four lower — a distinction a check written against the input rather
/// than against the payload would get wrong.
fn payload_len(record: &RecordRef<'_>) -> Result<usize, DecodeError> {
    let body = body(record);
    let len = body.prefix_len.saturating_add(body.tail.len());
    if len > MAX_PAYLOAD_BYTES {
        return Err(DecodeError::LengthOutOfBounds);
    }
    Ok(len)
}

/// A record's payload, as a fixed prefix followed by the caller's borrowed bytes.
///
/// Every record in §09's v0.1 table is one or the other or both, and expressing them all
/// this way is what lets [`encode`] write any record through one path — so a variant
/// cannot acquire a second, subtly different encoder.
struct Body<'a> {
    prefix: [u8; EFFECT_SCHEDULED_BODY_BYTES],
    prefix_len: usize,
    tail: &'a [u8],
}

/// Splits `record` into the fixed prefix its kind defines and the bytes after it.
fn body<'a>(record: &RecordRef<'a>) -> Body<'a> {
    let mut prefix = [0_u8; EFFECT_SCHEDULED_BODY_BYTES];
    match *record {
        RecordRef::RunStarted {
            workflow_kind,
            workflow_version,
            input,
        } => {
            for (slot, byte) in prefix.iter_mut().zip(
                workflow_kind
                    .to_le_bytes()
                    .into_iter()
                    .chain(workflow_version.to_le_bytes()),
            ) {
                *slot = byte;
            }
            Body {
                prefix,
                prefix_len: RUN_STARTED_PREFIX_BYTES,
                tail: input,
            }
        }
        RecordRef::EffectScheduled {
            kind,
            input_len,
            input_crc,
            ..
        } => {
            for (slot, byte) in prefix.iter_mut().zip(
                kind.0
                    .to_le_bytes()
                    .into_iter()
                    .chain(input_len.to_le_bytes())
                    .chain(input_crc.to_le_bytes()),
            ) {
                *slot = byte;
            }
            Body {
                prefix,
                prefix_len: EFFECT_SCHEDULED_BODY_BYTES,
                tail: &[],
            }
        }
        RecordRef::EffectCompleted { result, .. } | RecordRef::RunCompleted { result } => Body {
            prefix,
            prefix_len: 0,
            tail: result,
        },
        RecordRef::EffectFailed { error, .. } | RecordRef::RunFailed { error } => Body {
            prefix,
            prefix_len: 0,
            tail: error,
        },
    }
}

/// A front-to-back reader over borrowed bytes that cannot read past its end.
///
/// Every accessor returns [`None`] rather than panicking, which is what makes the bounded
/// decoding guarantee a property of one small type instead of a discipline spread over
/// every field of every record.
struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    /// The next `count` bytes, or [`None`] when fewer remain.
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.rest.split_at_checked(count)?;
        self.rest = tail;
        Some(head)
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes: [u8; 2] = self.take(2)?.try_into().ok()?;
        Some(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    }

    /// Everything not yet read.
    const fn rest(&self) -> &'a [u8] {
        self.rest
    }
}

/// Walks a journal frame by frame, in the order they were appended.
///
/// This is recovery's reader. §09: "Recovery stops at the first unsealed, malformed,
/// out-of-sequence, or integrity-failed frame" — so the scan is *fused*. Once it has
/// stopped it stays stopped, and [`offset`](Self::offset) is the byte the committed prefix
/// ends at.
///
/// # That offset is not yet an append point
///
/// It is tempting to write "and therefore where the next record goes", and at rung 0.1 that
/// would be wrong in a way that bricks a device. Without the commit seal the scan cannot
/// tell a torn write from damage, so it may have stopped at a frame whose header was
/// half-programmed — and on NOR a programmed bit cannot be returned to one without erasing
/// the block. An appender that wrote there would produce a frame that fails its own header
/// checksum on every boot, for ever, with no way to move past it. Deciding where it is safe
/// to append needs the seal and the barrier protocol, which is rung 0.2.
///
/// # Invariants
///
/// * Every step advances by at least [`FRAME_OVERHEAD_BYTES`], so a scan over any journal
///   terminates. A malformed journal is a scan that ends, never a scan that spins.
/// * An erased tail is not damage, and only an erased one. A journal has *ended* when what
///   is left is erased to its end — whether that is a full header of [`ERASED_BYTE`] with
///   nothing but erased bytes after it, or fewer bytes left than a header — and the scan
///   yields [`None`] rather than an error, because otherwise every first boot would look
///   like a corrupted one.
///
///   Programmed bytes in either case are not an end. An erased run with anything but erased
///   bytes after it is [`DecodeError::IntegrityFailed`] — see [`new`](Self::new) for the
///   stride mismatch that case exists to catch — and a short remainder that is not erased is
///   a torn header, reported as [`DecodeError::Truncated`]. Calling either a clean end would
///   tell a caller history stopped where it did not, at the one boundary where being wrong
///   means programming over cells a cycle has already cleared.
/// * A record yielded after the first failure would be a record outside the committed
///   prefix, so there are none: the iterator is fused rather than skipping damage.
/// * Out-of-sequence is *not* checked here. §09 lists it beside malformed and
///   integrity-failed, but ordering is a fact about the run rather than about the bytes,
///   and the replay cursor in `waymaker-core` is what owns it.
///
/// # Why it is not `Copy`
///
/// A scan is a position, and a copied position is two readers of one journal that each
/// believe they are the only one. `Clone` stays, because forking a scan deliberately — to
/// look ahead without losing where you were — is a thing a caller may want to write down.
///
/// # Why the integrity check is a type parameter
///
/// So that a scan cannot verify with a different check from the one that sealed what it is
/// walking. The parameter defaults to [`Catalogued`], so `Scan<'_>` is the shipped check
/// and every existing caller keeps meaning what it meant; a caller that wants another
/// writes it down at the type, where it is visible in every signature the scan passes
/// through rather than at one call site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scan<'a, C: IntegrityCheck = Catalogued> {
    journal: &'a [u8],
    align: ProgramAlign,
    offset: usize,
    stopped: bool,
    /// The check this scan verifies with. Zero-sized: [`IntegrityCheck`]'s methods take no
    /// `self`, so there is nothing to carry and the field costs no bytes.
    check: PhantomData<C>,
}

impl<'a> Scan<'a, Catalogued> {
    /// A scan of `journal`, whose frames were padded to `align`.
    ///
    /// `journal` is the bound: this rung has no bank geometry, and a slice is a bound the
    /// type system already checks.
    ///
    /// # `journal` is the journal region and nothing else
    ///
    /// A precondition rather than a convenience. The erased-tail rule below is "an erased
    /// header *and* erased to the end of the slice", so any byte after the last frame that
    /// is not [`ERASED_BYTE`] is damage as far as this type is concerned. A caller whose
    /// erase block also holds a bank header or a generation seal — which is the shape rung
    /// 0.2 arrives in — passes the journal region, not the block, or sees a sound journal
    /// reported as corrupt on every boot.
    ///
    /// # `align` must be the granularity the journal was *written* at
    ///
    /// Nothing on media records it. A frame says where it ends; only the device says where
    /// the next one begins, so a reader given a smaller granularity than the writer used
    /// strides short and lands inside a frame's padding. That padding is a run of
    /// [`ERASED_BYTE`], which is why the erased-tail rule above is "erased header *and*
    /// erased to the end of the journal" rather than "erased header": without the second
    /// half, a mismatch would report a clean end of history with committed records still
    /// ahead of it, and everything downstream would believe it.
    ///
    /// The check turns that into [`DecodeError::IntegrityFailed`] at the offset the reader
    /// went wrong, which is diagnosable. It does not make the mismatch safe, and the other
    /// half is worse: a reader given a *larger* granularity strides *over* whole frames and
    /// lands on erased bytes, which is an ordinary end of history in every respect this type
    /// can see. Nothing on media contradicts it, so nothing here can catch it — the test
    /// `a_scan_at_a_larger_alignment_than_the_writer_used_is_not_caught` asserts the wrong
    /// answer on purpose, so the limitation is bounded rather than undiscovered. Rung 0.2
    /// puts the writer's program size in the bank header, which is where a fact about the
    /// media belongs — and rung 0.2 has: it is
    /// [`BankHeader::align`](crate::bank::BankHeader::align), and
    /// [`BankHeader::journal_offset`](crate::bank::BankHeader::journal_offset) is the offset
    /// computed from it. A caller that takes both from the header it just decoded cannot be
    /// given a granularity the writer did not use. This type still cannot check that its
    /// caller did, because a slice carries no such fact — which is why the limitation is
    /// stated here rather than deleted.
    #[must_use]
    #[inline]
    pub const fn new(journal: &'a [u8], align: ProgramAlign) -> Self {
        Self::with_integrity(journal, align)
    }
}

impl<'a, C: IntegrityCheck> Scan<'a, C> {
    /// A scan of `journal` whose frames were sealed with `C` and padded to `align`.
    ///
    /// [`new`](Scan::new) is this at `C = Catalogued`. Everything
    /// [`new`](Scan::new) documents about `journal` and `align` applies here unchanged —
    /// the slice is the bound, and the alignment must be the one the journal was *written*
    /// at.
    ///
    /// A scan with the wrong `C` does not misread a journal: it stops at the first frame
    /// with [`DecodeError::IntegrityFailed`], because a seal computed by one algorithm is
    /// overwhelmingly unlikely to verify under another — see [`decode_with`] for what
    /// "overwhelmingly" is worth here.
    #[must_use]
    pub const fn with_integrity(journal: &'a [u8], align: ProgramAlign) -> Self {
        Self {
            journal,
            align,
            offset: 0,
            stopped: false,
            check: PhantomData,
        }
    }

    /// The byte at which the committed prefix ends.
    ///
    /// # Postconditions
    ///
    /// Zero before the first step; after a step that yielded a record, past that record
    /// and its padding; after a step that yielded a failure, still at the *start* of the
    /// frame that failed — §14's "frame ignored; previous history prefix wins" is exactly
    /// that offset. Always a whole number of program units, and never greater than the
    /// journal's length: a frame whose padding does not fit in the journal is a truncation
    /// rather than a record, so no step can end anywhere but on a boundary.
    ///
    /// This is where history *ended*, which is not the same as where the next record may be
    /// written: see [the note on `Scan`](Self#that-offset-is-not-yet-an-append-point).
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }
}

impl<'a, C: IntegrityCheck> Iterator for Scan<'a, C> {
    type Item = Result<RecordRef<'a>, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.stopped {
            return None;
        }
        let rest = self.journal.get(self.offset..)?;
        let Some(header) = rest.get(..HEADER_BYTES) else {
            // Fewer bytes left than a header. Erased is the ordinary end of a journal;
            // programmed bytes are a torn header, and the same rule applies to them as to a
            // full one — reporting an end of history there hands a caller an offset that
            // points into cells a program cycle has already cleared, which on NOR cannot be
            // written again without erasing the block.
            self.stopped = true;
            return if rest.iter().all(|byte| *byte == ERASED_BYTE) {
                None
            } else {
                Some(Err(DecodeError::Truncated))
            };
        };
        if header.iter().all(|byte| *byte == ERASED_BYTE) {
            self.stopped = true;
            // An erased header ends history only if everything after it is erased too.
            // Nothing on media records the program granularity a journal was written at, so
            // a reader handed a smaller one strides short and lands inside a frame's
            // padding — which is a run of `ERASED_BYTE`, and which would otherwise read as
            // a clean end of history with committed records still ahead of it. Silently
            // returning a truncated prefix is the worst failure this type has, because
            // everything downstream believes it.
            return if rest.iter().all(|byte| *byte == ERASED_BYTE) {
                None
            } else {
                Some(Err(DecodeError::IntegrityFailed))
            };
        }

        let frame = match decode_with::<C>(rest) {
            Ok(frame) => frame,
            Err(error) => {
                self.stopped = true;
                return Some(Err(error));
            }
        };
        let Some(stride) = self.align.round_up(frame.frame_len) else {
            self.stopped = true;
            return Some(Err(DecodeError::LengthOutOfBounds));
        };
        // A frame whose padded stride runs past the end of the journal could not have been
        // written into it: `encode` reserves the whole padded length before it writes a
        // byte, and refuses when the buffer is one short. So the journal is shorter than
        // the frame it appears to hold, which is a truncation and not a record — and
        // accepting it would advance to an offset that is not on a program boundary, which
        // is not a place anything may be written.
        let Some(next) = self
            .offset
            .checked_add(stride)
            .filter(|next| *next <= self.journal.len())
        else {
            self.stopped = true;
            return Some(Err(DecodeError::Truncated));
        };

        match frame.decoded {
            Decoded::Record(record) => {
                // `stride >= FRAME_OVERHEAD_BYTES > 0`, so the offset always moves and a
                // scan over a finite journal is finite.
                self.offset = next;
                Some(Ok(record))
            }
            Decoded::UnknownKind(_) => {
                // §09 makes skipping a property of the format version, and
                // `permits_unknown_record_skip` answers `false` for every one of the 256 a
                // version byte can hold. So the scan stops, and there is deliberately no
                // second arm here.
                //
                // An `if permits_unknown_record_skip(..) { advance; continue }` would read
                // as the rule made mechanical, and it would be a branch no test can reach —
                // whose first execution, years from now, is recovery after a power loss on
                // somebody's device. The version that grants skipping adds the arm, the
                // loop it needs, and the test that reaches it, in one change.
                self.stopped = true;
                Some(Err(DecodeError::UnknownRecordKind))
            }
        }
    }
}

impl<C: IntegrityCheck> core::iter::FusedIterator for Scan<'_, C> {}

// The frame's fixed parts are arithmetic the rest of this module trusts, so they are
// checked where a mistake in them is a compile error rather than a test run.
const _: () = assert!(HEADER_BYTES == 12);
// The seal widths issue #17 asks to be settled, checked against the layout that spends
// them: the header's last two bytes and the whole of the trailer. A width changed in one
// place and not the other is a frame whose fields have moved.
const _: () = assert!(HEADER_CRC_BYTES == 2);
const _: () = assert!(FRAME_CRC_BYTES == 4);
const _: () = assert!(TRAILER_BYTES == FRAME_CRC_BYTES);
const _: () = assert!(HEADER_BYTES == 10 + HEADER_CRC_BYTES);
const _: () = assert!(FRAME_OVERHEAD_BYTES == 16);
const _: () = assert!(MAX_FRAME_BYTES == 65_551);
const _: () = assert!(MAGIC != 0x0000 && MAGIC != 0xFFFF);
const _: () = assert!(RUN_STARTED_PREFIX_BYTES <= EFFECT_SCHEDULED_BODY_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_stops_at_the_end_of_its_bytes() {
        // The type the bounded-decoding guarantee rests on, tested where its `None`
        // branches are reachable: `decode` reaches them only through inputs it has already
        // length-checked, so without this they would be uncovered claims.
        let mut reader = Reader::new(&[1, 2, 3]);
        assert_eq!(reader.u16(), Some(0x0201));
        assert_eq!(reader.u16(), None);
        assert_eq!(reader.u32(), None);
        assert_eq!(reader.take(2), None);
        assert_eq!(reader.rest(), &[3]);
        assert_eq!(reader.take(1), Some(&[3][..]));
        assert!(reader.rest().is_empty());

        let mut empty = Reader::new(&[]);
        assert_eq!(empty.u16(), None);
        assert_eq!(empty.take(0), Some(&[][..]));
    }

    #[test]
    fn a_reader_reads_little_endian() {
        // Fixed-endian, independent of the host: a test on a big-endian machine has to
        // fail if the decoder ever used native order.
        let mut reader = Reader::new(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        assert_eq!(reader.u16(), Some(0x0201));
        assert_eq!(reader.u32(), Some(0x0605_0403));
    }

    #[test]
    fn version_one_permits_no_skipping() {
        // The forward-compatibility rule, read straight off the list `Scan` consults.
        assert!(VERSIONS_PERMITTING_UNKNOWN_RECORD_SKIP.is_empty());
        assert!(!permits_unknown_record_skip(FORMAT_VERSION));
    }

    #[test]
    fn a_body_splits_into_a_prefix_and_the_callers_bytes() {
        // One path encodes every record, so this is where each variant's share of the
        // split is pinned.
        let started = body(&RecordRef::RunStarted {
            workflow_kind: 0x0201,
            workflow_version: 0x0403,
            input: b"tail",
        });
        assert_eq!(started.prefix_len, 4);
        assert_eq!(started.prefix.get(..4), Some(&[1, 2, 3, 4][..]));
        assert_eq!(started.tail, b"tail");

        let scheduled = body(&RecordRef::EffectScheduled {
            seq: EffectSeq(0),
            kind: ActivityKind(0x0201),
            input_len: 0x0403,
            input_crc: 0x0807_0605,
        });
        assert_eq!(scheduled.prefix_len, 8);
        assert_eq!(scheduled.prefix, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(scheduled.tail.is_empty());

        let failed = body(&RecordRef::RunFailed { error: b"why" });
        assert_eq!(failed.prefix_len, 0);
        assert_eq!(failed.tail, b"why");
    }
}
