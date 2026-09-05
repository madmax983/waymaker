//! The two-bank lifecycle: the layout, the bank header, the generation seal, and which
//! bank a reader boots from.
//!
//! Design document §10. Waymaker owns two fixed storage banks, usually one erase block
//! each; the bank with the highest **valid** generation seal is authoritative, and its
//! journal is then scanned to the last valid committed record.
//!
//! ```text
//! bank                          notes
//!   base                        header frame, padded to the program unit
//!   base + journal_offset       the journal region `Scan` walks
//!   base + len - seal_bytes     the generation seal, one or more program units
//! ```
//!
//! # What this module owns
//!
//! The arithmetic that turns a [`Geometry`] into two banks — [`BankLayout`] and
//! [`BankRegion`] — and the two structures that live on media at fixed positions inside
//! one: [`BankHeader`] and [`Seal`]. Selection is [`select`], over the generations
//! [`sealed_generation`] found.
//!
//! # What this module must not own
//!
//! Media. There is no `read`, no `program` and no `barrier` here: this module says where
//! the bytes go and what they mean, and a driver implementing
//! [`StableStorage`](crate::storage::StableStorage) is what moves them. That is what keeps
//! the whole of §10's selection rule a pure function a host can enumerate.
//!
//! It does not own the checksums either. Which algorithm seals a header or a seal is
//! [`crate::integrity`]'s, behind [`IntegrityCheck`], and everything here takes it as a
//! type parameter defaulted to [`Catalogued`] — exactly as [`crate::frame`] does, and for
//! the reason [ADR 0012](https://github.com/madmax983/waymaker/blob/main/docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md)
//! gives.
//!
//! # The header frame
//!
//! ```text
//! offset  size  field             notes
//!      0     2  magic             u16 LE, `BANK_MAGIC`, reads as b"BK" on media
//!      2     1  format_version    u8, `crate::frame::FORMAT_VERSION`
//!      3     1  program_shift     u8, log2 of the program unit the bank was written at
//!      4     8  run_id            u64 LE
//!     12     2  workflow_kind     u16 LE
//!     14     2  workflow_version  u16 LE
//!     16     2  input_schema      u16 LE
//!     18     2  input_len         u16 LE
//!     20     2  header_crc        u16 LE, the header check over offsets 0..20
//!     22     N  input             the bounded run input, N = input_len
//!   22+N     4  frame_crc         u32 LE, the frame check over offsets 0..22+N
//!   26+N     -  padding           to the program unit, written 0xFF
//! ```
//!
//! Two checksums, for the reason [`crate::frame`] has two: `input_len` is read out of the
//! bytes being validated, so the header's own seal is what makes it the number the writer
//! wrote before it is used to say where the frame ends.
//!
//! ## Why `program_shift` is on media
//!
//! Because nothing else records it, and a reader that guesses wrong walks the journal
//! wrong. [`crate::frame::Scan`] documents the hazard and names this as its answer: a reader
//! handed the wrong granularity strides to the wrong place, and until issue #24's commit
//! seal a larger one landed on erased bytes and read as an ordinary end of history. The seal
//! now refuses both directions, which makes the mismatch loud rather than silent — but loud
//! is a diagnosis and not a fix. With the writer's program unit in the header, a caller
//! reads it rather than assumes it, and [`BankHeader::journal_offset`] is computed from the
//! same number.
//!
//! # The generation seal
//!
//! ```text
//! offset  size  field         notes
//!      0     2  magic         u16 LE, `SEAL_MAGIC`, reads as b"GS" on media
//!      2     4  generation    u32 LE
//!      6     4  header_check  u32 LE, the frame check over the header frame it seals
//!     10     2  seal_check    u16 LE, the header check over offsets 0..10
//! ```
//!
//! ## Why the seal names its header
//!
//! Because §10's crash windows are exactly the ones in which half a bank is on media. A
//! seal that survived an erase which took its header with it names a digest nothing on that
//! bank computes to, so the bank is not a candidate — whichever direction the driver's
//! erase ran in, and with no assumption about erase order anywhere in this module. That is
//! the structural half of "a bank whose seal fails validation is not a candidate at any
//! generation"; the seal's own `seal_check` is the other half, which catches a seal torn
//! part-way through its own program.
//!
//! ## Why the seal is at the end of the bank
//!
//! So that the header may grow with the run input without the seal moving. Its offset is a
//! function of the geometry alone, which means a reader can find it before it has decoded
//! anything.
//!
//! # Generations do not wrap
//!
//! [`Generation::successor`] returns [`None`] at [`Generation::MAX`], so no writer can mint
//! a generation that follows the ceiling. That is what makes the plain [`u32`] order the
//! order of the swaps: the comparison in [`select`] is not getting wraparound right by
//! luck, it is comparing numbers a writer cannot have wrapped. A device that reaches the
//! ceiling refuses to swap, which is [`crate::frame`]'s and `waymaker_core`'s treatment of
//! every other bounded counter — exhaustion is terminal, never silent reuse.

use core::fmt;

use waymaker_core::{DecodeError, RunId};

use crate::frame::{ERASED_BYTE, FORMAT_VERSION, ProgramAlign};
use crate::integrity::{Catalogued, IntegrityCheck};
use crate::storage::Geometry;

/// How many banks §02 decision 7 gives a device.
pub const BANKS: usize = 2;

/// The two bytes every bank header begins with: `0x42 0x4B`, which reads as `BK` on media.
///
/// Neither `0x0000` nor `0xFFFF`, so a zeroed page and an erased one are both rejected by
/// the first check rather than by a checksum further in, and not
/// [`crate::frame::MAGIC`] either — a bank header decoded as a record
/// frame, or the other way round, is a mistake worth catching at the magic.
pub const BANK_MAGIC: u16 = 0x4B42;

/// The two bytes every generation seal begins with: `0x47 0x53`, which reads as `GS`.
pub const SEAL_MAGIC: u16 = 0x5347;

/// Bytes of a bank header before the run input.
pub const HEADER_PREFIX_BYTES: usize = 22;

/// Bytes of a bank header after the run input: the frame checksum.
pub const HEADER_TRAILER_BYTES: usize = 4;

/// Everything in a bank header that is not the run input.
pub const HEADER_OVERHEAD_BYTES: usize = HEADER_PREFIX_BYTES + HEADER_TRAILER_BYTES;

/// The longest run input `input_len` can describe.
///
/// A format ceiling, and a long way above every firmware one. Two things bite first, and
/// neither is this number: the buffer a caller hands [`encode_header`], and the bank the
/// header has to fit in — which [`BankLayout`] derives from the device's geometry, and which
/// on §04's typical 4 KiB bank is sixteen times smaller than this. Verifying a header also
/// means checksumming it, and [`IntegrityCheck`] has no streaming form, so the whole frame
/// is resident while it is read: on a device with §04's 768 B of runtime RAM the real
/// ceiling on a run input is the caller's page, not the format's field.
pub const MAX_RUN_INPUT_BYTES: usize = u16::MAX as usize;

/// Width of a generation seal on media, before padding to the program unit.
pub const SEAL_BYTES: usize = 12;

/// Bytes of a generation seal its own checksum covers.
const SEALED_SEAL_BYTES: usize = SEAL_BYTES - 2;

/// [`SEAL_BYTES`] and [`HEADER_OVERHEAD_BYTES`] as `u32`s, for the layout arithmetic.
///
/// Written out rather than cast from the `usize` constants above. A `usize` cast is a
/// truncation lint on a 64-bit host, and an `#[allow]` on it would be an allow on every
/// other cast in the same expression; the `const` assertions below tie the two spellings
/// together more tightly than the cast would have.
const SEAL_WIDTH: u32 = 12;
const HEADER_OVERHEAD_WIDTH: u32 = 26;

/// Bytes of a bank header its header checksum covers.
const SEALED_HEADER_BYTES: usize = HEADER_PREFIX_BYTES - 2;

/// The largest `program_shift` a [`ProgramAlign`] can hold.
///
/// [`ProgramAlign`] is a `u16`, so the largest granularity is `1 << 15`. A header declaring
/// more describes a device that cannot exist, and a reader that accepted it would stride by
/// a number it computed by overflowing.
const MAX_PROGRAM_SHIFT: u8 = 15;

/// The largest program unit a bank header can record.
///
/// [`Geometry`] takes a `u32` program size and [`ProgramAlign`] is a `u16`, so the two do not
/// agree about what a device may be — and this is the number they disagree above.
const MAX_PROGRAM_UNIT: u32 = 1 << MAX_PROGRAM_SHIFT;

/// [`crate::frame::FRAME_OVERHEAD_BYTES`] as a `u32`, for the layout arithmetic.
///
/// A bank that cannot hold one record frame after its header is a bank with no journal, and
/// a journal is what a bank is for.
const FRAME_OVERHEAD_WIDTH: u32 = 16;

/// Which of the two banks something names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BankId {
    /// The bank at offset zero.
    A,
    /// The bank after it.
    B,
}

impl BankId {
    /// Both banks, in a fixed order.
    pub const ALL: [Self; BANKS] = [Self::A, Self::B];

    /// The bank this one is not.
    ///
    /// A swap recycles the *inactive* bank, so this is how a writer names it without
    /// deciding anything.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    /// Position in a two-element array indexed by bank.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
}

/// How many times a device has been handed a new run.
///
/// # Invariants
///
/// A generation never wraps. [`successor`](Self::successor) refuses at [`MAX`](Self::MAX)
/// rather than returning to zero, so the derived [`Ord`] is the order in which the seals
/// were written and [`select`] compares numbers that cannot have gone round.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(pub u32);

impl Generation {
    /// The generation a device's first sealed bank carries.
    ///
    /// Zero is an ordinary generation: a seal's validity comes from its own checksum and
    /// from the header it names, never from the value of this field, so nothing is gained
    /// by reserving one.
    pub const FIRST: Self = Self(0);

    /// The last generation a device can ever seal, and it *is* usable.
    pub const MAX: Self = Self(u32::MAX);

    /// The generation one after this one, or [`None`] at [`MAX`](Self::MAX).
    ///
    /// # Postconditions
    ///
    /// `Some(Generation(n + 1))` for every `n` below `u32::MAX`, and [`None`] at it. Never
    /// zero for a non-zero input, which is the whole of the no-wraparound invariant.
    #[must_use]
    pub const fn successor(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(next) => Some(Self(next)),
            None => None,
        }
    }
}

/// A geometry that cannot hold two banks.
///
/// Deliberately not [`Ord`]: these are three unrelated ways to be undescribable, and a type
/// that can be sorted is a type somebody will take the `max` of. [`crate::storage::GeometryError`]
/// derives neither for the same reason.
///
/// Not `#[non_exhaustive]`, for the reason [`waymaker_core::DecodeError`] is not: every
/// match on it is in this workspace, and an exhaustive match is how the compiler tells
/// whoever adds a variant which call sites now have a case to think about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LayoutError {
    /// The device has fewer than two erase blocks, so the two banks would share one.
    ///
    /// §04: "Two erase blocks minimum." A bank is recycled by erasing it, and an erase acts
    /// on whole blocks, so two banks in one block cannot be swapped at all.
    TooFewEraseBlocks,
    /// A bank is too small to hold a header, a seal, and one record frame after them.
    ///
    /// All three measured *padded* to the device's program unit, which is the smallest thing
    /// it can write. A bank that clears the header and the seal but has nothing left is a
    /// bank whose journal is zero bytes long, and a journal is what a bank is for.
    BankTooSmall,
    /// The device programs in units larger than a bank header can record.
    ///
    /// [`Geometry`] describes a program unit with a `u32` and [`ProgramAlign`] with a `u16`,
    /// so a device that programs in more than 32 KiB is one this format cannot describe —
    /// that being the largest power of two a `u16` holds, and therefore the largest
    /// granularity a header's `program_shift` can name. It is refused rather than
    /// approximated: the only granularity a
    /// writer *could* record for such a device is smaller than the one it actually programs
    /// at, and a reader striding at a smaller granularity than the writer used lands inside
    /// a frame's padding and reports a clean end of history with committed records still
    /// ahead of it — which [`crate::frame::Scan`] names as the worst failure it has, because
    /// everything downstream believes it.
    ProgramUnitTooLarge,
}

impl LayoutError {
    /// A short static description of this failure.
    ///
    /// Static text written straight through the formatter, for the reason
    /// [`crate::storage::GeometryError`]'s messages are: a single `write!` with an argument
    /// links `core::fmt::write` into an image with an 8 KiB budget, to say something no
    /// device with no console will ever print.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::TooFewEraseBlocks => "the device has fewer than two erase blocks",
            Self::BankTooSmall => "a bank cannot hold a header, a seal and a record",
            Self::ProgramUnitTooLarge => "the program unit is larger than a header can record",
        }
    }
}

impl fmt::Display for LayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for LayoutError {}

/// Where one bank is, and where its seal is inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BankRegion {
    base: u32,
    len: u32,
    seal_bytes: u32,
}

impl BankRegion {
    /// The bank's first byte, and the offset its header is written at.
    #[must_use]
    pub const fn base(self) -> u32 {
        self.base
    }

    /// How many bytes the bank occupies.
    ///
    /// Named `bytes` rather than `len` because a region is never empty — [`BankLayout::new`]
    /// refuses a geometry that would produce one — so the `is_empty` a `len` obliges a type
    /// to grow would be a method that always answers `false`.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        self.len
    }

    /// Where the generation seal starts.
    ///
    /// # Postconditions
    ///
    /// A whole number of program units from zero, inside this bank, and ending exactly at
    /// the bank's last byte.
    #[must_use]
    pub const fn seal_offset(self) -> u32 {
        self.base + self.len - self.seal_bytes
    }

    /// How many bytes the seal occupies: [`SEAL_BYTES`] rounded up to a program unit.
    #[must_use]
    pub const fn seal_bytes(self) -> u32 {
        self.seal_bytes
    }

    /// The header and the journal together: everything the seal does not occupy.
    #[must_use]
    pub const fn payload_bytes(self) -> u32 {
        self.len - self.seal_bytes
    }

    /// The longest run input a header for this bank may carry.
    ///
    /// [`MAX_RUN_INPUT_BYTES`] is what the length *field* can describe; this is what the
    /// bank can hold, and it is the smaller of the two on every device §04 targets. Without
    /// it a caller has to subtract [`HEADER_OVERHEAD_BYTES`] from
    /// [`payload_bytes`](Self::payload_bytes) itself, leave room for a journal itself, and
    /// get the padding right itself — and the one in-tree caller that did wrote the check as
    /// an `unreachable!`, which is what a bound in the wrong place looks like.
    ///
    /// # Postconditions
    ///
    /// A header whose input is this long, padded to the layout's granularity, leaves at
    /// least one record frame of journal behind it — so an input at this ceiling is one a
    /// bank can actually be used with, not merely one that fits.
    /// [`BankLayout::new`] refuses a geometry for which that would be zero, so this is never
    /// larger than [`MAX_RUN_INPUT_BYTES`] and is never negative.
    #[must_use]
    pub const fn max_run_input_bytes(self, align: ProgramAlign) -> usize {
        // A whole record, not a frame body: since issue #24 a record ends in a commit seal
        // one program unit wide, and a journal sized to the body alone can hold a frame that
        // can never be committed. `encoded_len_for` is the codec's own arithmetic for it.
        let Ok(frame) = crate::frame::encoded_len_for(0, align) else {
            return 0;
        };
        let payload = self.payload_bytes() as usize;
        let Some(for_header) = payload.checked_sub(frame) else {
            return 0;
        };
        // The header is padded too, so the ceiling is the largest input whose *padded* frame
        // still fits. Rounding down to a whole unit and then removing the fixed overhead is
        // that, without a search.
        let whole = for_header & !(align.get() as usize - 1);
        match whole.checked_sub(HEADER_OVERHEAD_BYTES) {
            Some(room) if room < MAX_RUN_INPUT_BYTES => room,
            Some(_) => MAX_RUN_INPUT_BYTES,
            None => 0,
        }
    }
}

/// Two banks derived from a device's geometry.
///
/// # Invariants
///
/// The two banks are equal in size, do not overlap, and lie inside the device. Each is a
/// whole number of erase blocks, because a bank is recycled by erasing it. A device with an
/// odd number of erase blocks gets two banks of `blocks / 2` each and the last block is not
/// addressed here — a layout that gave one bank the spare block would make the two banks
/// unequal, and a run that fits one would then not fit the other.
///
/// And the device's program unit is one a [`ProgramAlign`] can hold, so a header written for
/// a bank of this layout can record the granularity it was really written at. Every value of
/// this type therefore describes a device this format can describe, which is what lets
/// [`BankHeader::journal_offset`] be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BankLayout {
    geometry: Geometry,
    bank_bytes: u32,
    seal_bytes: u32,
    align: ProgramAlign,
}

impl BankLayout {
    /// Derives two banks from `geometry`, or says why it cannot.
    ///
    /// # Errors
    ///
    /// [`LayoutError::TooFewEraseBlocks`] for a device of fewer than two erase blocks,
    /// [`LayoutError::ProgramUnitTooLarge`] for a device that programs in units no bank
    /// header could record, and [`LayoutError::BankTooSmall`] when a bank could not hold a
    /// padded header, a padded seal and one padded record frame — which is a real device on
    /// a part whose program unit is a large fraction of its erase block.
    pub const fn new(geometry: Geometry) -> Result<Self, LayoutError> {
        let blocks = geometry.erase_blocks();
        if blocks < 2 {
            return Err(LayoutError::TooFewEraseBlocks);
        }
        // `blocks / 2` is a shift: `erase_blocks` is already a shift of the capacity and
        // two is a power of two, so no divider is linked. See `storage`'s note on why that
        // matters on `thumbv6m-none-eabi`.
        let bank_bytes = (blocks >> 1) * geometry.erase_size();
        let program = geometry.program_size();
        // Before any offset is computed from it. A layout is the first thing a caller asks
        // for, so a refusal it can act on beats an offset it cannot trust — and the offsets
        // below would be perfectly well-formed for a device no header can describe.
        if program > MAX_PROGRAM_UNIT {
            return Err(LayoutError::ProgramUnitTooLarge);
        }
        // The seal is a whole number of program units. `program` is a power of two and
        // `SEAL_BYTES` is 12, so this cannot overflow on any geometry `Geometry::new`
        // admits: the largest program unit is the largest erase block, and a capacity is a
        // `u32` of whole blocks.
        // Every one of the three is *padded*, because a program unit is the smallest thing a
        // device can write: a 26-byte header on a part with 256-byte pages occupies 256. An
        // earlier version of this guard compared the bank against the unpadded header and
        // reserved nothing for the journal, which admitted a two-erase-block device whose
        // header filled its whole payload and whose journal was zero bytes long — a bank
        // that can never hold a record, reported as a legal layout. Review of this change
        // found it.
        let (Some(seal_bytes), Some(header_bytes), Some(frame_body)) = (
            round_up_u32(SEAL_WIDTH, program),
            round_up_u32(HEADER_OVERHEAD_WIDTH, program),
            round_up_u32(FRAME_OVERHEAD_WIDTH, program),
        ) else {
            return Err(LayoutError::BankTooSmall);
        };
        // And a record is its frame body *plus its commit seal*, which issue #24 made one
        // more program unit. Reserving only the body admitted a bank whose journal could
        // hold a frame and not the seal that commits it — a journal in which no record can
        // ever become history, reported as a legal layout. Review of issue #25 found it.
        let Some(frame_bytes) = frame_body.checked_add(program) else {
            return Err(LayoutError::BankTooSmall);
        };
        // Summed and compared rather than subtracted from the bank. `seal_bytes` can exceed
        // `bank_bytes` on a legal geometry — two 8-byte erase blocks with an 8-byte program
        // unit gives a 8-byte bank and a 16-byte seal — and the subtraction that used to
        // stand here wraps in a release build, which has no overflow checks, into a layout
        // whose offsets are enormous and whose `Ok` is a lie.
        let (Some(used), ..) = (seal_bytes.checked_add(header_bytes), ()) else {
            return Err(LayoutError::BankTooSmall);
        };
        let Some(least) = used.checked_add(frame_bytes) else {
            return Err(LayoutError::BankTooSmall);
        };
        if bank_bytes < least {
            return Err(LayoutError::BankTooSmall);
        }
        // Resolved once, here, because this is the only place with a `Result` to refuse
        // through. `program` is a power of two at or below `MAX_PROGRAM_UNIT`, so the shift
        // is at most `MAX_PROGRAM_SHIFT` and `program_align` answers `Some` — the `else` is
        // unreachable and is spelled as the same refusal rather than as a fallback, because a
        // layout that quietly reported the wrong granularity is the failure this whole field
        // exists to prevent.
        let mut unit = program;
        let mut shift = 0_u8;
        while unit > 1 {
            unit >>= 1_u32;
            shift += 1;
        }
        let Some(align) = program_align(shift) else {
            return Err(LayoutError::ProgramUnitTooLarge);
        };
        Ok(Self {
            geometry,
            bank_bytes,
            seal_bytes,
            align,
        })
    }

    /// The geometry this layout was derived from.
    #[must_use]
    pub const fn geometry(self) -> Geometry {
        self.geometry
    }

    /// How many bytes each bank occupies. Both are the same size.
    #[must_use]
    pub const fn bank_bytes(self) -> u32 {
        self.bank_bytes
    }

    /// The granularity this device programs at, as a header records it.
    ///
    /// Infallible, and that is the whole point of it existing.
    /// [`new`](Self::new) refuses a device whose program unit a [`ProgramAlign`] could not
    /// hold, so by the time there is a [`BankLayout`] the narrowing has already been proved
    /// to succeed — and without this every caller writes
    /// `u16::try_from(..).ok().and_then(ProgramAlign::new)` with an arm it can never reach.
    /// A fallible conversion a caller cannot act on is a conversion the caller will get
    /// wrong, most cheaply by hardcoding the granularity it happens to be testing on.
    ///
    /// # Postconditions
    ///
    /// `align().get()` is [`Geometry::program_size`], and it is the value a header written
    /// for a bank of this layout must carry in [`BankHeader::align`].
    #[must_use]
    pub const fn align(self) -> ProgramAlign {
        self.align
    }

    /// Where one bank is.
    #[must_use]
    pub const fn bank(self, id: BankId) -> BankRegion {
        let base = match id {
            BankId::A => 0,
            BankId::B => self.bank_bytes,
        };
        BankRegion {
            base,
            len: self.bank_bytes,
            seal_bytes: self.seal_bytes,
        }
    }
}

/// `len` rounded up to a whole number of `unit`s, or [`None`] on overflow.
///
/// `unit` is a power of two on every path that reaches here — [`Geometry`] refuses anything
/// else — so this is an add and a mask rather than a division.
const fn round_up_u32(len: u32, unit: u32) -> Option<u32> {
    let mask = unit.wrapping_sub(1);
    match len.checked_add(mask) {
        Some(sum) => Some(sum & !mask),
        None => None,
    }
}

/// What a bank header declares: whose run this bank holds, and how to read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BankHeader<'a> {
    /// The run whose history this bank holds. On media it lives here, once.
    pub run: RunId,
    /// The program granularity the bank's frames were written at.
    ///
    /// A fact about the media the bank was written on rather than about the device reading
    /// it, which is why it is recorded — see the module documentation.
    pub align: ProgramAlign,
    /// Which workflow this run is of.
    pub workflow_kind: u16,
    /// Which version of that workflow.
    pub workflow_version: u16,
    /// Which schema [`input`](Self::input) is encoded under.
    ///
    /// The kernel never interprets it. §02 decision 4 keeps records numeric kinds and
    /// borrowed bytes, so what a schema number *means* belongs to the application, and
    /// recording it is what lets a workflow refuse an input it no longer understands rather
    /// than decode one it does.
    pub input_schema: u16,
    /// The bounded run input, borrowed from the bank.
    pub input: &'a [u8],
}

impl BankHeader<'_> {
    /// Bytes the encoded header occupies, *before* padding.
    ///
    /// # Postconditions
    ///
    /// [`HEADER_OVERHEAD_BYTES`] plus the input's length. Saturating rather than wrapping:
    /// an input longer than [`MAX_RUN_INPUT_BYTES`] cannot be encoded at all, and a length
    /// that came back smaller than the frame it describes would be a writer told it had
    /// room it does not have.
    #[must_use]
    pub const fn frame_len(&self) -> usize {
        HEADER_OVERHEAD_BYTES.saturating_add(self.input.len())
    }

    /// Where this bank's journal begins, relative to the bank's base.
    ///
    /// The header padded to the granularity the header itself records, which is why this is
    /// a method on the decoded header rather than on [`BankRegion`]: the offset is a fact
    /// about what was written, not about the device now reading it.
    ///
    /// # Postconditions
    ///
    /// A whole number of [`align`](Self::align) units, at least
    /// [`frame_len`](Self::frame_len), and [`None`] only on an overflow no encodable header
    /// can reach.
    ///
    /// It is *not* guaranteed to be within the bank. For a header this crate encoded into a
    /// bank's payload region it is, because [`encode_header`] refuses a buffer it does not
    /// fit and the region's length is a whole number of the device's program units. A header
    /// that records a granularity the device does not program at breaks that chain, which is
    /// why [`BankLayout::align`] exists — a caller that takes the granularity from the layout
    /// rather than from anywhere else cannot get into the state where this offset runs past
    /// [`BankRegion::payload_bytes`].
    #[must_use]
    pub const fn journal_offset(&self) -> Option<usize> {
        self.align.round_up(self.frame_len())
    }
}

/// Bytes a bank header carrying `input_bytes` of run input occupies, padded to `align`.
///
/// [`BankHeader::frame_len`] followed by [`BankHeader::journal_offset`], without a header in
/// hand — the same thing [`crate::frame::encoded_len_for`] is to [`crate::frame::encoded_len`],
/// and here for the same reason: §10's capacity reserve prices the *worst case* of a bound,
/// and the worst case is a length rather than a value. `pub(crate)` because the reserve is
/// the only caller and a header a caller has in hand can answer for itself.
///
/// [`None`] when rounding up would overflow, which no encodable header can reach.
pub(crate) const fn header_len_for(input_bytes: usize, align: ProgramAlign) -> Option<usize> {
    align.round_up(HEADER_OVERHEAD_BYTES.saturating_add(input_bytes))
}

/// A bank's generation seal: what makes a written bank an authoritative one.
///
/// §02 decision 7: "a new run becomes authoritative only after its payload and generation
/// seal are durable."
///
/// Deliberately not [`Ord`]. Two seals differing only in `header_check` would sort by a
/// digest, and a `max` over seals is never the question — [`select`] compares
/// [`Generation`]s, which are ordered on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Seal {
    /// How many times a device has been handed a new run.
    pub generation: Generation,
    /// The frame check of the header frame this seal makes authoritative.
    ///
    /// A seal that does not name the header beside it seals nothing: see the module
    /// documentation for the crash window that makes this the load-bearing field.
    pub header_check: u32,
}

/// Writes `header` into `out`, padded to the granularity it records.
///
/// # Postconditions
///
/// On success the return value is [`BankHeader::journal_offset`], every byte of it in `out`
/// has been written, and [`decode_header`] of those bytes yields `header` back. On failure
/// **nothing is written**: the length check happens before the first byte, because a
/// partial header left in a staging buffer is a header a later flush could program.
///
/// # Errors
///
/// [`DecodeError::LengthOutOfBounds`] when the run input is longer than
/// [`MAX_RUN_INPUT_BYTES`], when rounding up to the header's own granularity would
/// overflow, or when `out` is shorter than the padded header.
#[inline]
pub fn encode_header(header: &BankHeader<'_>, out: &mut [u8]) -> Result<usize, DecodeError> {
    encode_header_with::<Catalogued>(header, out)
}

/// Writes `header` into `out`, sealed with `C`.
///
/// [`encode_header`] is this at `C = Catalogued`, which is the check ADR 0010 settled on
/// and the only one this firmware writes.
///
/// # Postconditions
///
/// As [`encode_header`], and additionally: bytes 20 and 21 are `C::header_check` of the
/// twenty before them, and the four bytes after the run input are `C::frame_check` of
/// everything before them. Nothing else in the header depends on `C`.
///
/// # Errors
///
/// As [`encode_header`].
pub fn encode_header_with<C: IntegrityCheck>(
    header: &BankHeader<'_>,
    out: &mut [u8],
) -> Result<usize, DecodeError> {
    if header.input.len() > MAX_RUN_INPUT_BYTES {
        return Err(DecodeError::LengthOutOfBounds);
    }
    let input_len =
        u16::try_from(header.input.len()).map_err(|_| DecodeError::LengthOutOfBounds)?;
    let frame_len = header.frame_len();
    let padded = header
        .journal_offset()
        .ok_or(DecodeError::LengthOutOfBounds)?;

    // Split once, before a byte moves: the whole length check for the caller's buffer, and
    // the reason a failed encode leaves `out` untouched.
    let Some((frame, _beyond)) = out.split_at_mut_checked(padded) else {
        return Err(DecodeError::LengthOutOfBounds);
    };

    let [magic_low, magic_high] = BANK_MAGIC.to_le_bytes();
    let shift = program_shift(header.align);
    let run = header.run.0.to_le_bytes();
    let [kind_low, kind_high] = header.workflow_kind.to_le_bytes();
    let [version_low, version_high] = header.workflow_version.to_le_bytes();
    let [schema_low, schema_high] = header.input_schema.to_le_bytes();
    let [len_low, len_high] = input_len.to_le_bytes();
    // Written out field by field rather than assembled through a cursor: every byte's
    // position is visible at a glance and comparable with the table at the top of this
    // module, and an array literal has no length to get wrong.
    let sealed_prefix: [u8; SEALED_HEADER_BYTES] = [
        magic_low,
        magic_high,
        FORMAT_VERSION,
        shift,
        run[0],
        run[1],
        run[2],
        run[3],
        run[4],
        run[5],
        run[6],
        run[7],
        kind_low,
        kind_high,
        version_low,
        version_high,
        schema_low,
        schema_high,
        len_low,
        len_high,
    ];
    let [crc_low, crc_high] = C::header_check(&sealed_prefix).to_le_bytes();

    for (slot, byte) in frame.iter_mut().zip(
        sealed_prefix
            .into_iter()
            .chain([crc_low, crc_high])
            .chain(header.input.iter().copied()),
    ) {
        *slot = byte;
    }
    // Sealed from the bytes that landed rather than from the values that produced them, so
    // a write that went astray cannot be sealed as if it had not.
    let sealed = frame
        .get(..frame_len - HEADER_TRAILER_BYTES)
        .ok_or(DecodeError::LengthOutOfBounds)?;
    let frame_crc = C::frame_check(sealed).to_le_bytes();
    for (slot, byte) in frame
        .iter_mut()
        .skip(frame_len - HEADER_TRAILER_BYTES)
        .zip(frame_crc)
    {
        *slot = byte;
    }
    for slot in frame.iter_mut().skip(frame_len) {
        *slot = ERASED_BYTE;
    }

    Ok(padded)
}

/// Reads the bank header at the front of `bytes`.
///
/// Trailing bytes are ignored: `bytes` is normally the whole bank, and
/// [`BankHeader::journal_offset`] is what says where the journal starts.
///
/// # Postconditions
///
/// Every length is validated against `bytes` before it is used to read anything, in this
/// order: the prefix must be present; the magic must match; the header checksum must hold,
/// which is what makes `input_len` trustworthy; the format version must be one this
/// firmware reads; the frame the prefix describes must be present in full; the frame
/// checksum must hold; and only then is `program_shift` interpreted. The returned header
/// borrows `bytes`, so no input is copied and nothing is allocated.
///
/// # Errors
///
/// * [`DecodeError::Truncated`] — `bytes` is shorter than the prefix, or shorter than the
///   frame the prefix describes.
/// * [`DecodeError::IntegrityFailed`] — the magic, the header checksum or the frame
///   checksum did not match. An erased bank fails at the magic.
/// * [`DecodeError::UnsupportedFormatVersion`] — a version this firmware does not read.
/// * [`DecodeError::MalformedRecord`] — the frame is intact and declares a program
///   granularity no device has.
#[inline]
pub fn decode_header(bytes: &[u8]) -> Result<BankHeader<'_>, DecodeError> {
    decode_header_with::<Catalogued>(bytes)
}

/// Reads the bank header at the front of `bytes`, verifying it against `C`.
///
/// [`decode_header`] is this at `C = Catalogued`. A header whose seals were computed by a
/// different algorithm is refused with [`DecodeError::IntegrityFailed`], which is what makes
/// a firmware reflashed with another check refuse an old bank loudly rather than read it
/// wrong.
///
/// # Postconditions
///
/// As [`decode_header`], with `C` in place of the shipped pair.
///
/// # Errors
///
/// As [`decode_header`].
pub fn decode_header_with<C: IntegrityCheck>(bytes: &[u8]) -> Result<BankHeader<'_>, DecodeError> {
    let Some(prefix) = bytes.first_chunk::<HEADER_PREFIX_BYTES>() else {
        return Err(DecodeError::Truncated);
    };
    let [
        magic_low,
        magic_high,
        version,
        shift,
        run0,
        run1,
        run2,
        run3,
        run4,
        run5,
        run6,
        run7,
        kind_low,
        kind_high,
        version_low,
        version_high,
        schema_low,
        schema_high,
        len_low,
        len_high,
        crc_low,
        crc_high,
    ] = *prefix;
    let sealed_prefix: [u8; SEALED_HEADER_BYTES] = [
        magic_low,
        magic_high,
        version,
        shift,
        run0,
        run1,
        run2,
        run3,
        run4,
        run5,
        run6,
        run7,
        kind_low,
        kind_high,
        version_low,
        version_high,
        schema_low,
        schema_high,
        len_low,
        len_high,
    ];

    if u16::from_le_bytes([magic_low, magic_high]) != BANK_MAGIC {
        return Err(DecodeError::IntegrityFailed);
    }
    if C::header_check(&sealed_prefix) != u16::from_le_bytes([crc_low, crc_high]) {
        return Err(DecodeError::IntegrityFailed);
    }
    // Only now is the length a number the writer wrote rather than a number that was found,
    // and only now is the version worth reading: the prefix layout is frozen across format
    // versions, so its checksum is meaningful before its version is known.
    if version != FORMAT_VERSION {
        return Err(DecodeError::UnsupportedFormatVersion);
    }

    let input_len = usize::from(u16::from_le_bytes([len_low, len_high]));
    let covered = HEADER_PREFIX_BYTES.saturating_add(input_len);
    let (Some(sealed), Some(trailer)) = (
        bytes.get(..covered),
        bytes
            .get(covered..)
            .and_then(<[u8]>::first_chunk::<HEADER_TRAILER_BYTES>),
    ) else {
        return Err(DecodeError::Truncated);
    };
    if C::frame_check(sealed) != u32::from_le_bytes(*trailer) {
        return Err(DecodeError::IntegrityFailed);
    }

    let Some(align) = program_align(shift) else {
        return Err(DecodeError::MalformedRecord);
    };
    let Some(input) = sealed.get(HEADER_PREFIX_BYTES..) else {
        // Unreachable: `sealed` is `covered` bytes long and `covered >= HEADER_PREFIX_BYTES`.
        // Spelled as a refusal rather than an `unwrap` because the workspace denies both,
        // and a decoder walking bytes off a damaged device is the last place to make an
        // exception.
        return Err(DecodeError::Truncated);
    };

    Ok(BankHeader {
        run: RunId(u64::from_le_bytes([
            run0, run1, run2, run3, run4, run5, run6, run7,
        ])),
        align,
        workflow_kind: u16::from_le_bytes([kind_low, kind_high]),
        workflow_version: u16::from_le_bytes([version_low, version_high]),
        input_schema: u16::from_le_bytes([schema_low, schema_high]),
        input,
    })
}

/// The `program_shift` byte for `align`.
///
/// [`ProgramAlign`] refuses anything but a power of two, so this is exact and the round trip
/// through [`program_align`] is the identity.
///
/// Counted rather than taken from `trailing_zeros`, which returns a `u32` and would need a
/// narrowing cast to reach the byte this field is. The loop runs at most
/// [`MAX_PROGRAM_SHIFT`] times, and `thumbv6m-none-eabi` has no count-leading-zeros
/// instruction, so `trailing_zeros` would compile to a loop of its own anyway.
const fn program_shift(align: ProgramAlign) -> u8 {
    let mut unit = align.get();
    let mut shift = 0_u8;
    while unit > 1 {
        unit >>= 1_u32;
        shift += 1;
    }
    shift
}

/// The [`ProgramAlign`] a `program_shift` byte names, or [`None`] for one no device has.
const fn program_align(shift: u8) -> Option<ProgramAlign> {
    if shift > MAX_PROGRAM_SHIFT {
        return None;
    }
    ProgramAlign::new(1_u16 << shift)
}

/// Writes `seal` into `out`, padded to `align`.
///
/// # Postconditions
///
/// On success the return value is [`SEAL_BYTES`] rounded up to `align`, every byte of it in
/// `out` has been written, and [`decode_seal`] of those bytes yields `seal` back. On failure
/// nothing is written.
///
/// # Errors
///
/// [`DecodeError::LengthOutOfBounds`] when `out` is shorter than the padded seal, or when
/// rounding up to `align` would overflow.
#[inline]
pub fn encode_seal(seal: &Seal, align: ProgramAlign, out: &mut [u8]) -> Result<usize, DecodeError> {
    encode_seal_with::<Catalogued>(seal, align, out)
}

/// Writes `seal` into `out`, sealed with `C`.
///
/// [`encode_seal`] is this at `C = Catalogued`.
///
/// # Postconditions
///
/// As [`encode_seal`], and additionally: the last two bytes of the seal proper are
/// `C::header_check` of the ten before them. The `header_check` field is the caller's — it
/// is the digest of the bank header this seal makes authoritative, and computing it here
/// would mean this function had to be handed the header as well as the seal.
///
/// # Errors
///
/// As [`encode_seal`].
pub fn encode_seal_with<C: IntegrityCheck>(
    seal: &Seal,
    align: ProgramAlign,
    out: &mut [u8],
) -> Result<usize, DecodeError> {
    let padded = align
        .round_up(SEAL_BYTES)
        .ok_or(DecodeError::LengthOutOfBounds)?;
    let Some((frame, _beyond)) = out.split_at_mut_checked(padded) else {
        return Err(DecodeError::LengthOutOfBounds);
    };

    let [magic_low, magic_high] = SEAL_MAGIC.to_le_bytes();
    let generation = seal.generation.0.to_le_bytes();
    let header = seal.header_check.to_le_bytes();
    let sealed_prefix: [u8; SEALED_SEAL_BYTES] = [
        magic_low,
        magic_high,
        generation[0],
        generation[1],
        generation[2],
        generation[3],
        header[0],
        header[1],
        header[2],
        header[3],
    ];
    let [crc_low, crc_high] = C::header_check(&sealed_prefix).to_le_bytes();
    for (slot, byte) in frame
        .iter_mut()
        .zip(sealed_prefix.into_iter().chain([crc_low, crc_high]))
    {
        *slot = byte;
    }
    for slot in frame.iter_mut().skip(SEAL_BYTES) {
        *slot = ERASED_BYTE;
    }

    Ok(padded)
}

/// Reads the generation seal at the front of `bytes`.
///
/// # Errors
///
/// [`DecodeError::Truncated`] when fewer than [`SEAL_BYTES`] bytes are there, and
/// [`DecodeError::IntegrityFailed`] when the magic or the seal's own checksum did not
/// match. An erased seal region and a zeroed one both fail at the magic.
#[inline]
pub fn decode_seal(bytes: &[u8]) -> Result<Seal, DecodeError> {
    decode_seal_with::<Catalogued>(bytes)
}

/// Reads the generation seal at the front of `bytes`, verifying it against `C`.
///
/// [`decode_seal`] is this at `C = Catalogued`.
///
/// # Errors
///
/// As [`decode_seal`].
pub fn decode_seal_with<C: IntegrityCheck>(bytes: &[u8]) -> Result<Seal, DecodeError> {
    let Some(seal) = bytes.first_chunk::<SEAL_BYTES>() else {
        return Err(DecodeError::Truncated);
    };
    let [
        magic_low,
        magic_high,
        gen0,
        gen1,
        gen2,
        gen3,
        header0,
        header1,
        header2,
        header3,
        crc_low,
        crc_high,
    ] = *seal;
    let sealed_prefix: [u8; SEALED_SEAL_BYTES] = [
        magic_low, magic_high, gen0, gen1, gen2, gen3, header0, header1, header2, header3,
    ];

    if u16::from_le_bytes([magic_low, magic_high]) != SEAL_MAGIC {
        return Err(DecodeError::IntegrityFailed);
    }
    if C::header_check(&sealed_prefix) != u16::from_le_bytes([crc_low, crc_high]) {
        return Err(DecodeError::IntegrityFailed);
    }

    Ok(Seal {
        generation: Generation(u32::from_le_bytes([gen0, gen1, gen2, gen3])),
        header_check: u32::from_le_bytes([header0, header1, header2, header3]),
    })
}

/// The seal that makes the bank header at the front of `header_frame` authoritative.
///
/// This is the one definition of the digest a seal carries, so a writer cannot compute it
/// one way and a reader another. `header_frame` is the bank from its base; trailing bytes
/// are ignored, so the whole bank may be passed.
///
/// # Errors
///
/// Whatever [`decode_header`] refuses `header_frame` with. A seal for a header that cannot
/// be read is a seal nothing could ever validate, so it is a refusal rather than a value.
#[inline]
pub fn seal_for(header_frame: &[u8], generation: Generation) -> Result<Seal, DecodeError> {
    seal_for_with::<Catalogued>(header_frame, generation)
}

/// The seal, under a chosen integrity check.
///
/// [`seal_for`] is this at `C = Catalogued`.
///
/// # Postconditions
///
/// `header_check` is `C::frame_check` over exactly the bytes the header's own trailer
/// covers, which is what makes it equal to the header's stored `frame_crc` for a header
/// this `C` accepts. Computed rather than read back out of that trailer: recomputing is
/// what makes the answer depend on `C` rather than on four bytes a damaged bank carries.
///
/// # Errors
///
/// As [`seal_for`].
pub fn seal_for_with<C: IntegrityCheck>(
    header_frame: &[u8],
    generation: Generation,
) -> Result<Seal, DecodeError> {
    let decoded = decode_header_with::<C>(header_frame)?;
    let covered = decoded
        .frame_len()
        .checked_sub(HEADER_TRAILER_BYTES)
        .ok_or(DecodeError::Truncated)?;
    let frame = header_frame.get(..covered).ok_or(DecodeError::Truncated)?;
    Ok(Seal {
        generation,
        header_check: C::frame_check(frame),
    })
}

/// The generation a reader would boot this bank at, or [`None`] if it would not boot it.
///
/// # `header` and `seal` are one bank's regions and nothing else
///
/// A precondition rather than a convenience, and the same one [`crate::frame::Scan::new`]
/// states about its journal. `header` is `base..base + payload_bytes()` and `seal` is
/// `seal_offset()..seal_offset() + seal_bytes()`, both from [`BankRegion`]. Trailing bytes
/// inside those regions are ignored — a header is self-delimiting — but a caller that passes
/// the whole device image passes bank A a slice that runs into bank B, and a header whose
/// `input_len` reached past its own bank would then borrow bytes from the other one. That is
/// "recovery combines the footprints of two runs", which is the thing §10 forbids, so the
/// bound is the caller's to get right and it is stated here rather than assumed.
///
/// # Postconditions
///
/// `Some` only when all three hold: the header decodes, the seal decodes, and the seal names
/// *this* header. A bank failing any of them is not a candidate at any generation, which is
/// issue [#22](https://github.com/madmax983/waymaker/issues/22)'s selection rule in as many
/// words.
#[must_use]
#[inline]
pub fn sealed_generation(header: &[u8], seal: &[u8]) -> Option<Generation> {
    sealed_generation_with::<Catalogued>(header, seal)
}

/// The generation a reader verifying with `C` would boot this bank at.
///
/// [`sealed_generation`] is this at `C = Catalogued`.
///
/// # Postconditions
///
/// As [`sealed_generation`]. Written as an equality against [`seal_for_with`] rather than
/// as a comparison of digests, so that the seal a writer produces and the seal a reader
/// accepts are the same value computed by the same function — a reader with a rule of its
/// own is a reader that can disagree with the writer about a bank neither of them damaged.
#[must_use]
pub fn sealed_generation_with<C: IntegrityCheck>(header: &[u8], seal: &[u8]) -> Option<Generation> {
    let seal = decode_seal_with::<C>(seal).ok()?;
    let expected = seal_for_with::<C>(header, seal.generation).ok()?;
    (expected == seal).then_some(seal.generation)
}

/// Which bank a reader boots from.
///
/// Not `#[non_exhaustive]`, for the reason [`LayoutError`] is not, and not [`Ord`] either:
/// "unsealed is less than ambiguous" is a sentence with no meaning, and the ordering that
/// does matter is [`Generation`]'s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Authority {
    /// Neither bank carries a valid seal, so there is nothing to boot from.
    ///
    /// The ordinary state of a device that has never committed, and
    /// [`waymaker_fault::Breach::NoAuthoritativeBank`] for one that has. Telling those two
    /// apart is the caller's: it is a fact about the device's history rather than about its
    /// bytes.
    ///
    /// [`waymaker_fault::Breach::NoAuthoritativeBank`]: https://github.com/madmax983/waymaker/blob/main/crates/waymaker-fault/src/oracle.rs
    Unsealed,
    /// Exactly one bank is authoritative.
    Bank {
        /// The bank to boot from.
        id: BankId,
        /// The generation its seal carries.
        generation: Generation,
    },
    /// Both banks are validly sealed at the same generation.
    ///
    /// The state no protocol may produce: §02 decision 7 makes a new run authoritative on
    /// its *generation* seal, and a seal that repeats a generation seals nothing. Reported
    /// rather than resolved, because a selection that picked one would hide the bug this
    /// variant exists to find.
    Ambiguous {
        /// The generation both banks claim.
        generation: Generation,
    },
}

/// Which bank a reader boots from, given what each bank's seal was worth.
///
/// The array is indexed by [`BankId::index`]. [`None`] is a bank whose seal failed
/// validation, and it is not in the comparison at all — so the other bank wins however low
/// its generation is, which is issue #22's "a bank whose seal fails validation is not a
/// candidate at any generation".
///
/// # Postconditions
///
/// Total, and a pure function of its argument. The comparison is the derived [`Ord`] on
/// [`Generation`], which is sound precisely because generations do not wrap — see the module
/// documentation.
#[must_use]
pub const fn select(generations: [Option<Generation>; BANKS]) -> Authority {
    match generations {
        [None, None] => Authority::Unsealed,
        [Some(generation), None] => Authority::Bank {
            id: BankId::A,
            generation,
        },
        [None, Some(generation)] => Authority::Bank {
            id: BankId::B,
            generation,
        },
        [Some(left), Some(right)] => {
            if left.0 == right.0 {
                Authority::Ambiguous { generation: left }
            } else if left.0 > right.0 {
                Authority::Bank {
                    id: BankId::A,
                    generation: left,
                }
            } else {
                Authority::Bank {
                    id: BankId::B,
                    generation: right,
                }
            }
        }
    }
}

// The layout's fixed parts are arithmetic the rest of this module trusts, so they are
// checked where a mistake in them is a compile error rather than a test run.
const _: () = assert!(HEADER_PREFIX_BYTES == 22);
const _: () = assert!(HEADER_TRAILER_BYTES == 4);
const _: () = assert!(HEADER_OVERHEAD_BYTES == 26);
const _: () = assert!(SEALED_HEADER_BYTES == 20);
const _: () = assert!(SEAL_BYTES == 12);
const _: () = assert!(SEALED_SEAL_BYTES == 10);
const _: () = assert!(BANK_MAGIC != 0x0000 && BANK_MAGIC != 0xFFFF);
const _: () = assert!(SEAL_MAGIC != 0x0000 && SEAL_MAGIC != 0xFFFF);
const _: () = assert!(BANK_MAGIC != SEAL_MAGIC && BANK_MAGIC != crate::frame::MAGIC);
const _: () = assert!(SEAL_MAGIC != crate::frame::MAGIC);
const _: () = assert!(BANKS == 2);
const _: () = assert!(MAX_PROGRAM_SHIFT == 15);
// The two spellings of the same ceiling, checked against each other rather than trusted:
// `MAX_PROGRAM_UNIT` guards a `u32` from a `Geometry` and `MAX_PROGRAM_SHIFT` guards a byte
// read off media, and a build in which they disagree admits a device on one path that the
// other refuses.
const _: () = assert!(MAX_PROGRAM_UNIT == 32_768);
// The frame overhead this module reserves a journal for is the one `frame` really has.
const _: () = assert!(FRAME_OVERHEAD_WIDTH as usize == crate::frame::FRAME_OVERHEAD_BYTES);
const _: () = assert!(program_align(MAX_PROGRAM_SHIFT).is_some());
const _: () = assert!(SEAL_BYTES == 12 && SEAL_WIDTH == 12);
const _: () = assert!(HEADER_OVERHEAD_BYTES == 26 && HEADER_OVERHEAD_WIDTH == 26);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_shift_round_trips_every_granularity_a_device_can_have() {
        // The one conversion on this module's decode path that is not a checksum, tested
        // over its whole domain rather than at the granularity the tests happen to use.
        for shift in 0..=MAX_PROGRAM_SHIFT {
            let align = program_align(shift).expect("every shift to 15 names a granularity");
            assert_eq!(align.get(), 1_u16 << shift);
            assert_eq!(program_shift(align), shift);
        }
        for shift in (MAX_PROGRAM_SHIFT + 1)..=u8::MAX {
            assert_eq!(program_align(shift), None, "shift {shift}");
        }
    }

    #[test]
    fn rounding_up_saturates_nowhere_and_refuses_at_the_ceiling() {
        assert_eq!(round_up_u32(0, 4), Some(0));
        assert_eq!(round_up_u32(1, 4), Some(4));
        assert_eq!(round_up_u32(4, 4), Some(4));
        assert_eq!(round_up_u32(12, 1), Some(12));
        // A padded length that came back *smaller* than what it pads would be a writer told
        // it had room it does not have.
        assert_eq!(round_up_u32(u32::MAX, 4), None);
    }
}
