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
//! The bytes, and only the bytes: the constants above, the two checksums (private, in the
//! crate's `crc` module), [`encode`], [`decode`] and the append [`Scan`] that walks a
//! journal.
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
//!   no offset arithmetic against a geometry this rung does not have. Bank selection and
//!   the generation seal arrive with rung 0.2.
//!
//! [`StableStorage`]: https://github.com/madmax983/waymaker/blob/main/docs/design/waymaker-design-v0.2.html

use waymaker_core::{ActivityKind, DecodeError, EffectSeq, RecordKind, RecordRef};

use crate::crc::{crc16, crc32};

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

/// Everything in a frame that is not payload.
pub const FRAME_OVERHEAD_BYTES: usize = HEADER_BYTES + TRAILER_BYTES;

/// The longest payload `payload_len` can describe.
///
/// A `RunStarted` input is capped four bytes lower, because the workflow identity is part
/// of its payload.
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
/// A future version that does grant it says so by appearing here, and [`Scan`] starts
/// skipping. The rule is a lookup rather than a paragraph.
const VERSIONS_PERMITTING_UNKNOWN_RECORD_SKIP: &[u8] = &[];

/// How far the payload of the two fixed-shape records extends.
const RUN_STARTED_PREFIX_BYTES: usize = 4;
const EFFECT_SCHEDULED_BODY_BYTES: usize = 8;

/// The device's program granularity, known to be non-zero.
///
/// §12 gives the storage adapter a `program_size: u16`, and §09 requires records padded to
/// it. Zero is rejected once here rather than guarded at every modulo: a zero alignment is
/// not "no padding", it is a division by zero, and [`ProgramAlign::BYTE`] is what "no
/// padding" is spelled as.
///
/// A power of two is *not* required. Real internal flash reports one, but nothing in the
/// arithmetic below needs it, and a type that refused a device's honest geometry would be
/// a type a driver had to lie to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ProgramAlign(u16);

impl ProgramAlign {
    /// One byte: every length is already aligned, so nothing is padded.
    pub const BYTE: Self = Self(1);

    /// A program granularity of `bytes`, or [`None`] when `bytes` is zero.
    ///
    /// # Postconditions
    ///
    /// `Some` exactly when `bytes != 0`, and `get()` returns what was passed.
    #[must_use]
    pub const fn new(bytes: u16) -> Option<Self> {
        if bytes == 0 { None } else { Some(Self(bytes)) }
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
    #[must_use]
    pub const fn round_up(self, len: usize) -> Option<usize> {
        let unit = self.0 as usize;
        // `unit` is non-zero by construction, so neither operator can trap.
        let remainder = len % unit;
        if remainder == 0 {
            Some(len)
        } else {
            len.checked_add(unit - remainder)
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
    pub encoded_len: usize,
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
pub fn encode(
    record: &RecordRef<'_>,
    align: ProgramAlign,
    out: &mut [u8],
) -> Result<usize, DecodeError> {
    let payload_len = payload_len(record)?;
    let frame_len = FRAME_OVERHEAD_BYTES.saturating_add(payload_len);
    let padded = align
        .round_up(frame_len)
        .ok_or(DecodeError::LengthOutOfBounds)?;

    let Some((frame, padding)) = out.split_at_mut_checked(padded) else {
        return Err(DecodeError::LengthOutOfBounds);
    };
    let _ = padding;

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
    let [crc_low, crc_high] = crc16(&sealed_header).to_le_bytes();
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
    let frame_crc = crc32(frame.get(..covered).unwrap_or_default());
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
/// rest of the bank and [`Frame::encoded_len`] is what says where this record stopped.
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
pub fn decode(bytes: &[u8]) -> Result<Frame<'_>, DecodeError> {
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
    if crc16(&sealed_header) != u16::from_le_bytes([crc_low, crc_high]) {
        return Err(DecodeError::IntegrityFailed);
    }
    // Only now is the length a number the writer wrote rather than a number that was
    // found, and only now is the version worth reading: the header layout is frozen across
    // format versions, so its checksum is meaningful before its version is known.
    if version != FORMAT_VERSION {
        return Err(DecodeError::UnsupportedFormatVersion);
    }

    let payload_len = usize::from(u16::from_le_bytes([len_low, len_high]));
    // Both sums are bounded by `MAX_FRAME_BYTES`, so neither can overflow a `usize` on any
    // target this crate builds for; `saturating_add` says so without depending on it.
    let covered = HEADER_BYTES.saturating_add(payload_len);
    let encoded_len = covered.saturating_add(TRAILER_BYTES);
    let (Some(sealed), Some(trailer)) = (
        bytes.get(..covered),
        bytes
            .get(covered..)
            .and_then(<[u8]>::first_chunk::<TRAILER_BYTES>),
    ) else {
        return Err(DecodeError::Truncated);
    };
    if crc32(sealed) != u32::from_le_bytes(*trailer) {
        return Err(DecodeError::IntegrityFailed);
    }

    let Some(payload) = sealed.get(HEADER_BYTES..) else {
        // Unreachable: `sealed` is `covered` bytes long and `covered >= HEADER_BYTES`.
        // Spelled as a refusal rather than an `unwrap` because the workspace denies both
        // `unwrap` and `panic`, and a decoder walking bytes off a damaged device is the
        // last place to make an exception.
        return Err(DecodeError::Truncated);
    };
    let decoded = decode_body(
        RecordKind(kind),
        EffectSeq(u32::from_le_bytes([seq0, seq1, seq2, seq3])),
        payload,
    )?;

    Ok(Frame {
        format_version: version,
        decoded,
        encoded_len,
    })
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
/// ends at, which is where the next append goes.
///
/// # Invariants
///
/// * Every step advances by at least [`FRAME_OVERHEAD_BYTES`], so a scan over any journal
///   terminates. A malformed journal is a scan that ends, never a scan that spins.
/// * An erased tail is not damage. A journal whose next header is all
///   [`ERASED_BYTE`], or which has fewer bytes left than a header, has simply ended, and
///   the scan yields [`None`] rather than an error — otherwise every first boot would look
///   like a corrupted one.
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scan<'a> {
    journal: &'a [u8],
    align: ProgramAlign,
    offset: usize,
    stopped: bool,
}

impl<'a> Scan<'a> {
    /// A scan of `journal`, whose frames were padded to `align`.
    ///
    /// `journal` is the bound: this rung has no bank geometry, and a slice is a bound the
    /// type system already checks.
    #[must_use]
    pub const fn new(journal: &'a [u8], align: ProgramAlign) -> Self {
        Self {
            journal,
            align,
            offset: 0,
            stopped: false,
        }
    }

    /// Where the committed prefix ends, and therefore where the next append goes.
    ///
    /// # Postconditions
    ///
    /// Zero before the first step; after a step that yielded a record, past that record
    /// and its padding; after a step that yielded a failure, still at the *start* of the
    /// frame that failed — §14's "frame ignored; previous history prefix wins" is exactly
    /// that offset. Never greater than the journal's length.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }
}

impl<'a> Iterator for Scan<'a> {
    type Item = Result<RecordRef<'a>, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.stopped {
                return None;
            }
            let rest = self.journal.get(self.offset..)?;
            let Some(header) = rest.get(..HEADER_BYTES) else {
                // Less than a header left: the ordinary end of a journal.
                self.stopped = true;
                return None;
            };
            if header.iter().all(|byte| *byte == ERASED_BYTE) {
                self.stopped = true;
                return None;
            }

            let frame = match decode(rest) {
                Ok(frame) => frame,
                Err(error) => {
                    self.stopped = true;
                    return Some(Err(error));
                }
            };
            let Some(stride) = self.align.round_up(frame.encoded_len) else {
                self.stopped = true;
                return Some(Err(DecodeError::LengthOutOfBounds));
            };
            // `stride >= FRAME_OVERHEAD_BYTES > 0`, so the offset always moves and the
            // loop below cannot spin on a run of unknown kinds.
            let next = self.offset.saturating_add(stride).min(self.journal.len());

            match frame.decoded {
                Decoded::Record(record) => {
                    self.offset = next;
                    return Some(Ok(record));
                }
                Decoded::UnknownKind(_) => {
                    if VERSIONS_PERMITTING_UNKNOWN_RECORD_SKIP.contains(&frame.format_version) {
                        self.offset = next;
                        continue;
                    }
                    self.stopped = true;
                    return Some(Err(DecodeError::UnknownRecordKind));
                }
            }
        }
    }
}

impl core::iter::FusedIterator for Scan<'_> {}

// The frame's fixed parts are arithmetic the rest of this module trusts, so they are
// checked where a mistake in them is a compile error rather than a test run.
const _: () = assert!(HEADER_BYTES == 12);
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
        assert!(!VERSIONS_PERMITTING_UNKNOWN_RECORD_SKIP.contains(&FORMAT_VERSION));
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
