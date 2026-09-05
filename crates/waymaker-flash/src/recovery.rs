//! Recovery: the forward scan that turns an authoritative bank into a committed prefix.
//!
//! Design document §09 Journal and wire format, §10 Two-bank lifecycle, §14 Failure
//! semantics, and issue [#23](https://github.com/madmax983/waymaker/issues/23). Once
//! [`crate::bank::select`] has said which bank is authoritative, its journal is scanned
//! forward to the last valid committed record. **Where the scan stops is the whole of prefix
//! safety.**
//!
//! # What this module owns
//!
//! [`JournalRegion`], which is the bytes between a bank's header and its seal, validated
//! once as a legal read; [`Recovery`], which is a position in that region and nothing else;
//! and [`Ending`], which is what a finished scan learned — including, in exactly one of its
//! three shapes, where the next record may be written.
//!
//! # Why this is not [`Scan`](crate::frame::Scan)
//!
//! [`Scan`](crate::frame::Scan) walks a journal that is already in RAM, and a journal that
//! is already in RAM is a journal on a host. §04 gives a device **768 B** of runtime RAM
//! stated with a **512 B** scratch page, so a 4 KiB bank cannot be staged and a 64 KiB one
//! is not worth discussing. This module reads through §12's storage contract instead, one
//! frame at a time, into a page the caller owns and this type never keeps.
//!
//! The two are held to each other rather than merely intended to agree:
//! `tests/recovery.rs`'s `a_recovery_reads_what_a_scan_reads` walks the same bytes both
//! ways and requires the same records, the same stopping offset and the same verdict. Two
//! readers of one format drift; that test is what fails when they start to.
//!
//! # The stop conditions, and where each lives
//!
//! §09: "recovery stops at the first unsealed, malformed, out-of-sequence, or
//! integrity-failed frame". Those four are not all in one place, and pretending otherwise
//! would be worse than saying so:
//!
//! * **malformed** and **integrity-failed** are here, through
//!   [`decode_with`](crate::frame::decode_with) — a frame whose magic, header seal, frame
//!   seal, version or body does not hold ends the scan at its own first byte.
//! * **out-of-sequence** is `waymaker_core::ReplayCursor`'s, because ordering is a fact
//!   about the run rather than about the bytes — see
//!   [ADR 0008](https://github.com/madmax983/waymaker/blob/main/docs/adr/0008-the-replay-cursor-is-pumped-by-its-caller.md).
//!   A caller pairs the two, and a cursor that refuses a record simply stops pumping this
//!   type; because [`append_offset`](Recovery::append_offset) answers [`None`] for a scan
//!   that has not finished, the refusal withholds the append point as well.
//!   `tests/cold_start.rs` is that pairing, driven end to end.
//! * **unsealed** is here too, since issue
//!   [#24](https://github.com/madmax983/waymaker/issues/24). §09's frame ends with a commit
//!   seal one program unit wide, written only after a payload barrier, so a frame body with
//!   no valid seal over it is a frame whose writer never reached §07 step 3. It stops the
//!   scan at its own first byte with [`Ending::Unsealed`], which is what lets a caller tell
//!   "the power went during an append" from "this bank is damaged" — the distinction this
//!   module was written without, and the one [`Ending`] said it would grow a variant for.
//!
//! # The append offset
//!
//! Issue #23 asks for the append point as a by-product of the scan, and the by-product is
//! deliberately hard to get at: only [`Ending::Clean`] carries one. A scan that stopped at
//! damage has **no** safe append point, and this is not conservatism.
//! [`Scan`](crate::frame::Scan)'s own documentation states the failure: without the commit
//! seal a scan cannot tell a torn write from corruption, so it may have stopped at a frame
//! whose header was half-programmed — and on NOR a programmed bit cannot be returned to one
//! without erasing the block. A writer that appended there would produce a frame that fails
//! its own header checksum on every boot, for ever. Appending *past* it is worse: the next
//! boot's scan stops at the tear again and never reaches what was written, so the records
//! are lost while the device reports success.
//!
//! So the invariant is: **whenever an append offset comes back, every byte from it to the
//! end of the region is erased, and the absolute offset it names is one this device can
//! program at.** `tests/recovery.rs` asserts exactly that, over a journal
//! at every length from empty upwards, and `waymaker-fault`'s crash sweep asserts it at
//! every point a power loss can land.
//!
//! # Whether the next record *fits* is a separate question
//!
//! An append offset says where a record may go, not that one will. [`Ending::Clean`] is
//! reported for a journal that is full to its last byte and for one whose tail is shorter
//! than a header, and both are honest: everything from the offset to the end of the region
//! is erased, there is just not much of it. The room is
//! `region.bytes() - append_at`, and the size of the record that has to fit in it is
//! [`frame::encoded_len`] at the region's granularity.
//!
//! Those two are deliberately not folded into a third accessor. §10 reserves tail space for
//! a terminal record or a `continue_as_new` and fails ordinary scheduling early with
//! `HistoryNearCapacity`, so "does this fit" is a *policy* question with a reserve in it,
//! and the reserve is not this module's. What is this module's is that the offset is safe,
//! and that is what [`append_offset`](Recovery::append_offset) answers.
//!
//! # Cost
//!
//! Per record: at most two reads — one of a header, one of the whole record — and at most
//! one padded frame plus its commit seal of bytes. The page therefore has to hold a *record*
//! rather than a frame, which is one program unit more than it was before issue #24, and
//! [`RecoveryError::PageTooSmall`] reports that number. Nothing in [`Recovery`] grows with
//! history; it is a region, an offset and a verdict, and a `const` assertion below says so.
//!
//! Two reads rather than one is a deliberate trade and worth stating, because the obvious
//! alternative looks cheaper and is not. Staging `min(page, remaining)` bytes in one read
//! would halve the transactions and multiply the *bytes* by twenty: a 512 B page fetched for
//! a 24 B record. On a QSPI part transfer time is the cost that dominates, so twelve wasted
//! header bytes per record beats four hundred and eighty wasted payload ones. The header
//! seal is also verified twice per record, once by
//! [`frame::frame_len_of`] and once inside
//! [`frame::decode`] — ten bytes of CRC-16, against the alternative of
//! a second public decoder entry point taking an already-verified header.
//!
//! The one cost that is not per record is the erased-tail walk: when the scan meets an
//! erased header it reads the rest of the region to be sure the whole tail is erased, in
//! page-sized chunks. That is bounded by the region rather than by history, is paid once,
//! and is what stops a hole in a journal from reading as the end of one. It is also the
//! cost that will dominate boot latency on the first real board — a 64 KiB bank with a
//! 512 B page is 128 reads on *every* boot, however short its history — and the commit seal
//! of issue [#24](https://github.com/madmax983/waymaker/issues/24) does **not** remove it.
//! The seal says a record is committed; it does not say that no record follows. Stopping at
//! an erased header without walking the tail would need a marker saying "history ends
//! here", which is a record kind rather than a seal, and none of §09's vocabulary is one.
//! What the seal removes is the *ambiguity* at the stopping point rather than the walk, and
//! the walk is still owed a cheaper answer.

use core::fmt;
use core::marker::PhantomData;

use waymaker_core::{DecodeError, RecordRef};

use crate::bank::{BankHeader, BankId, BankLayout};
use crate::frame::{self, Decoded, ERASED_BYTE, HEADER_BYTES, ProgramAlign};
use crate::integrity::{Catalogued, IntegrityCheck};
use crate::storage::{Geometry, GeometryError, StableStorage};

/// [`HEADER_BYTES`] as the width it is compared against device geometry as.
const HEADER_WIDTH: u32 = 12;

// The two spellings of the same number, checked where a mistake is a compile error.
const _: () = assert!(HEADER_WIDTH as usize == HEADER_BYTES);

/// A journal region that cannot be described, or cannot be read.
///
/// Not `#[non_exhaustive]`, for the reason [`GeometryError`] is not: every match on it is in
/// this workspace, and an exhaustive match is how the compiler tells whoever adds a variant
/// which call sites now have a case to think about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionError {
    /// The bank's header leaves no bytes behind it for a journal.
    ///
    /// A refusal rather than a zero-length region, because a zero-length region is one every
    /// caller would read as an empty history it may append to.
    NoJournalRoom,
    /// The region has no bytes in it.
    EmptyRegion,
    /// The bank records a granularity this layout does not use.
    ///
    /// [`BankLayout::align`](crate::bank::BankLayout::align) is documented as "the value a
    /// header written for a bank of this layout must carry", and until now nothing enforced
    /// it. The two can only differ across devices, and the failure is quiet in both
    /// directions. A writer with a *coarser* program unit reserved more room for its
    /// generation seal than this reader subtracts, so the journal region runs into the
    /// writer's seal and a sound bank reads as damaged on every boot. A *finer* one shortens
    /// the region, and history past its end is dropped under a clean ending — which is the
    /// silent truncation [`Scan::new`](crate::frame::Scan::new) calls the worst failure it
    /// has. Neither is something a scan can notice, so the mismatch is refused where it is
    /// visible: the one call that welds the writer's granularity to the reader's.
    AlignDisagreesWithBank,
    /// The journal was written at a granularity finer than this device programs at.
    ///
    /// Frame boundaries are a whole number of the *writer's* program units, so a journal
    /// written more finely than this device programs has frame starts it cannot write at —
    /// and the offset a recovery hands back is one of them. Refusing here is a cold boot
    /// that stops; discovering it at the first append would be a driver told to program a
    /// misaligned offset, and discovering it mid-scan would be a recovery that halted
    /// somewhere arbitrary and called it the end of history.
    ///
    /// One check rather than two, because a geometry nests: `erase >= program >= read`, so
    /// a granularity at least the program unit is at least the read unit as well.
    AlignBelowProgramUnit,
    /// The region is not something [`Geometry::validate_program`] permits.
    Geometry(GeometryError),
}

impl RegionError {
    /// A short static description of this failure.
    ///
    /// Static text written straight through the formatter, for the reason
    /// [`GeometryError::message`] is: a single `write!` with an argument links
    /// `core::fmt::write` into an image with a code-flash budget, to say something no device
    /// with no console will ever print.
    ///
    /// # Postconditions
    ///
    /// Non-empty, ASCII, and distinct from every other variant's — including from every
    /// [`GeometryError`] it wraps, so a refusal is diagnosable without a debugger.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NoJournalRoom => "the bank header leaves no room for a journal",
            Self::EmptyRegion => "the journal region is empty",
            Self::AlignDisagreesWithBank => "the bank's granularity is not this layout's",
            Self::AlignBelowProgramUnit => {
                "the journal's granularity is below the device's program unit"
            }
            Self::Geometry(error) => error.message(),
        }
    }
}

impl fmt::Display for RegionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for RegionError {}

/// Where one bank's journal is, and what granularity it was written at.
///
/// # Invariants
///
/// Every value of this type is a legal *program* on the device it was built against: the base
/// and the length are whole program units, the region is inside the device, and it is not
/// empty. [`spanning`](Self::spanning) and [`of`](Self::of) are the only constructors, and
/// [`of`](Self::of) goes through [`spanning`](Self::spanning). So a region that exists is one
/// every read this module performs inside it is legal for — a geometry nests, so a program
/// unit is whole read units — which is why the scan validates once here rather than on every
/// frame.
///
/// And the recorded granularity is at least the device's program unit, so **every frame
/// boundary inside the region is an offset this device can both read and write at**. The
/// second half is what makes an append offset a place a driver can really program: validating
/// the region only as a read would admit a base of 1 on a device that programs eight bytes at
/// a time, and a recovery of it would hand back an offset every conforming
/// [`StableStorage`] must refuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct JournalRegion {
    /// The device this region was validated against, kept rather than borrowed from the
    /// storage a scan is handed.
    ///
    /// Every bound in this module — that a read is aligned, that it stays inside the region,
    /// that a frame's padded stride is a legal offset — was proved against *this* geometry
    /// at construction. Reading the units back off the `StableStorage` a caller passes to
    /// [`Recovery::next`] would prove them against a different one: a region built at
    /// granularity 4 and walked on a device that reads sixteen bytes at a time would round a
    /// 24-byte frame up to a 32-byte read and run eight bytes past the region's end, into the
    /// generation seal or the neighbouring bank. Carrying the geometry makes that
    /// unrepresentable rather than guarded, and a caller that hands over a device the region
    /// does not describe gets a refusal from the driver's own validation instead — which is
    /// the failure closing in the right direction.
    geometry: Geometry,
    base: u32,
    bytes: u32,
    align: ProgramAlign,
}

impl JournalRegion {
    /// A journal of `bytes` bytes at `base`, written at `align`, on a device of `geometry`.
    ///
    /// The primitive constructor. A port that lays its journals out some other way builds
    /// one here; a caller using this crate's two-bank layout uses [`of`](Self::of), which
    /// computes the two numbers from §10's chain rather than asking a caller to.
    ///
    /// Named `spanning` rather than `new` because [`Recovery::new`] is in this file too, and
    /// the `recovery-surface` pin is a list of names: a second `new` would be a second
    /// public function invisible to it and to `size-probe-reach`, which is the shape both
    /// rules exist to catch.
    ///
    /// # Errors
    ///
    /// [`RegionError::EmptyRegion`] for a region of no bytes,
    /// [`RegionError::AlignBelowProgramUnit`] when `align` is finer than the device's program
    /// unit — see that variant for why that is refused rather than tolerated — and
    /// [`RegionError::Geometry`] when the region is misaligned against the program unit or
    /// reaches past the device.
    ///
    /// Validated as a *program* rather than as a read, which is stronger in both places it
    /// has to be: a geometry nests, so whatever is legal to program is legal to read, and a
    /// region that is readable but not programmable is one whose append offset no driver
    /// would accept.
    pub fn spanning(
        geometry: Geometry,
        base: u32,
        bytes: u32,
        align: ProgramAlign,
    ) -> Result<Self, RegionError> {
        if bytes == 0 {
            return Err(RegionError::EmptyRegion);
        }
        if u32::from(align.get()) < geometry.program_size() {
            return Err(RegionError::AlignBelowProgramUnit);
        }
        match geometry.validate_program(base, bytes) {
            Ok(()) => Ok(Self {
                geometry,
                base,
                bytes,
                align,
            }),
            Err(error) => Err(RegionError::Geometry(error)),
        }
    }
    /// The journal of `bank`, as `header` describes it.
    ///
    /// §10's chain in one call: `layout` says where the bank is and where its seal ends,
    /// `header` says where its journal starts and what granularity its frames were written
    /// at, and what is left between them is the journal. A caller that did this arithmetic
    /// itself would have four chances to get it wrong, and one of them — taking the
    /// granularity from the device rather than from the header — is the mismatch
    /// [`Scan::new`](crate::frame::Scan::new) documents as the worst failure it has.
    ///
    /// # Errors
    ///
    /// [`RegionError::NoJournalRoom`] when the header's padded frame fills the bank's
    /// payload region, [`RegionError::AlignDisagreesWithBank`] when the header records a
    /// granularity this layout does not use — see that variant for the two silent failures
    /// that closes — and otherwise as [`spanning`](Self::spanning).
    pub fn of(
        layout: BankLayout,
        bank: BankId,
        header: &BankHeader<'_>,
    ) -> Result<Self, RegionError> {
        // Before any offset is computed from either. `journal_offset` is a fact about the
        // granularity the *writer* used and `payload_bytes` is a fact about the one the
        // *reader* uses, and welding those two together is this function's whole job — so it
        // is also the only place that can refuse to. Once they agree, "the offset comes from
        // the header rather than from the device" is a distinction without a difference,
        // which is the point: the mismatch is closed by refusal rather than by getting the
        // field right.
        if header.align.get() != layout.align().get() {
            return Err(RegionError::AlignDisagreesWithBank);
        }
        let region = layout.bank(bank);
        let Some(offset) = header.journal_offset() else {
            return Err(RegionError::NoJournalRoom);
        };
        let Ok(offset) = u32::try_from(offset) else {
            return Err(RegionError::NoJournalRoom);
        };
        let (Some(base), Some(bytes)) = (
            region.base().checked_add(offset),
            region.payload_bytes().checked_sub(offset),
        ) else {
            return Err(RegionError::NoJournalRoom);
        };
        if bytes == 0 {
            return Err(RegionError::NoJournalRoom);
        }
        Self::spanning(layout.geometry(), base, bytes, header.align)
    }

    /// The device offset the journal starts at.
    #[must_use]
    pub const fn base(self) -> u32 {
        self.base
    }

    /// How many bytes the journal occupies.
    ///
    /// Named `bytes` rather than `len` for the reason
    /// [`BankRegion::bytes`](crate::bank::BankRegion::bytes) is: a region is never empty, so
    /// the `is_empty` a `len` obliges a type to grow would always answer `false`.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        self.bytes
    }

    /// The program granularity the journal's frames were written at.
    #[must_use]
    pub const fn align(self) -> ProgramAlign {
        self.align
    }

    /// The device this region was validated against.
    ///
    /// `pub(crate)` rather than `pub`, deliberately. [`crate::append`] needs it to refuse a
    /// writer handed a device the region does not describe, which is the same check
    /// [`Recovery::next`] makes; a caller outside this crate has the geometry already,
    /// because it is the one it built the region with. The `recovery-surface` rule pins this
    /// file's *public* names, and a fifth accessor on the reader's surface is not the price
    /// of an internal comparison.
    pub(crate) const fn geometry(self) -> Geometry {
        self.geometry
    }
}

/// How a recovery ended, and — in one case only — where the next record may be written.
///
/// The three are not degrees of the same thing; they are three different things a caller
/// must do next.
///
/// # The fourth shape, and why it is not `Damaged`
///
/// Issue [#24](https://github.com/madmax983/waymaker/issues/24) added
/// [`Unsealed`](Self::Unsealed), which is what this type's earlier documentation said it
/// would grow rather than overloading [`Damaged`](Self::Damaged) — and the reason is what a
/// caller does next. A damaged bank is a bank to suspect: the media returned bytes no writer
/// could have written, and §10's swap is what recycles it. An unsealed tail is the ordinary
/// shape of a device that lost power while appending, and the only thing wrong with it is
/// that the record was never committed. Both are final and neither is appendable — every
/// byte of them is programmed — but a firmware that raised an alarm on the second would
/// raise it on every unlucky reboot.
///
/// Every `match` on this enum in this workspace is exhaustive, so a fifth shape would be a
/// list of call sites rather than a silent reinterpretation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ending {
    /// The journal ended in erased media, and `append_at` is its first erased byte.
    ///
    /// The prefix is the whole of history, and appending at `append_at` is safe in both
    /// senses: every byte from it to the end of the region is erased, and
    /// `region.base() + append_at` is a whole number of the device's program units, because
    /// [`JournalRegion`] validated the region as a program and every stride is a multiple of
    /// its granularity. That is the invariant the whole module exists to keep — see the note
    /// on the append offset in the module documentation.
    ///
    /// `append_at` may equal [`JournalRegion::bytes`], which is a full journal rather than a
    /// failure; whether the next record fits is the caller's arithmetic.
    Clean {
        /// The first erased byte, relative to [`JournalRegion::base`].
        append_at: u32,
    },
    /// The scan stopped at a frame it could not accept, at `at`.
    ///
    /// The prefix is final — §14's "frame ignored; previous history prefix wins" — and it is
    /// complete, so a caller may replay it. Nothing may be appended: the region has to be
    /// recycled, which is §10's bank swap.
    Damaged {
        /// The first byte of the frame the scan refused, relative to
        /// [`JournalRegion::base`].
        at: u32,
    },
    /// The scan could not continue, and what lies past `at` is unknown.
    ///
    /// A read that failed, or a frame longer than the caller's page. Distinct from
    /// [`Damaged`](Self::Damaged) in the one way that matters: the prefix may be *short*, so
    /// a caller must not replay it as though it were all of history, and must not append.
    Incomplete {
        /// The offset the scan gave up at, relative to [`JournalRegion::base`].
        at: u32,
    },
    /// The scan met a frame body with no commit seal over it, at `at`.
    ///
    /// §09's first stop condition. The prefix before it is final and complete, so a caller
    /// may replay it — that is what separates this from [`Incomplete`](Self::Incomplete),
    /// where the prefix may be short. Nothing may be appended: `at` is the first byte of a
    /// frame whose cells a program cycle has already cleared.
    ///
    /// This is what a power loss during §07's steps 1 to 3 leaves behind, and it is the
    /// only ending that says so. A record that reaches it was never acknowledged, was never
    /// dispatched — §07 dispatches at step 4, after the commit barrier — and is therefore
    /// history the device is right to have no trace of.
    Unsealed {
        /// The first byte of the unsealed frame, relative to [`JournalRegion::base`].
        at: u32,
    },
}

/// Why a recovery could not produce the next record.
///
/// Generic over the driver's error, because §12 lets every port name its own and a recovery
/// that flattened them would be throwing away the only thing a driver author can act on.
///
/// Deliberately no [`Display`](fmt::Display) and no `message` accessor of its own, which is
/// the one place this type breaks with every other error in the workspace. `Display` would
/// need `E: Display`, and a bound like that spreads to every signature the type appears in.
/// A `message` would be a *second* one in this file — [`RegionError`] has the name — and the
/// `recovery-surface` pin is a list of names, so the second would be invisible to it.
///
/// Nothing is lost. Every variant already carries something better than a string: the
/// driver's own error, [`DecodeError::message`] behind a one-line `match`, and a byte count
/// that says exactly how much larger the page would have to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecoveryError<E> {
    /// The media could not be read. Nothing was learned about what is on it.
    Storage(E),
    /// A frame was not the record it claimed to be.
    Decode(DecodeError),
    /// The storage handed to a step is not the device the region was validated against.
    ///
    /// Every bound this module keeps — that a read is aligned and inside the region, that a
    /// frame's padded stride lands where a record may be written — was established against
    /// one [`Geometry`], at construction. A step given a different device would be answering
    /// about that device with arithmetic proved about another, and the failure is silent in
    /// the direction that matters: a region built where the program unit is one byte and
    /// walked on a device that programs eight reads perfectly well, reports a clean end, and
    /// hands back an append offset the second device must refuse.
    ///
    /// So the two are compared rather than assumed equal, on every step. It is four integer
    /// comparisons against an anti-bricking guarantee.
    WrongDevice,
    /// The caller's page cannot hold what the next step has to stage.
    ///
    /// Not damage: the journal may be perfectly sound and this device simply cannot read a
    /// record that long. `needed` is how many bytes the page would have had to hold,
    /// rounded up to the device's read unit.
    PageTooSmall {
        /// Bytes the page would have had to hold.
        needed: usize,
    },
}

/// A position in one journal, walked forward through §12's storage contract.
///
/// # Invariants
///
/// * [`offset`](Self::offset) is inside the region, is a whole number of the region's
///   program units, and only ever moves forward.
/// * Once [`ending`](Self::ending) answers [`Some`], it never changes and
///   [`next`](Self::next) answers [`None`] for ever. A record yielded after the first
///   failure would be a record outside the committed prefix.
/// * No borrow of the caller's page is retained. The lifetime on
///   [`next`](Self::next)'s answer rides on the page rather than on `&mut self`, which is
///   what makes one page enough for a history of any length — the same decision
///   [ADR 0008](https://github.com/madmax983/waymaker/blob/main/docs/adr/0008-the-replay-cursor-is-pumped-by-its-caller.md)
///   made for the replay cursor, for the same reason.
/// * [`append_offset`](Self::append_offset) answers [`Some`] only for a scan that ended in
///   erased media.
///
/// # Why it is not `Copy`
///
/// A position, and a copied position is two readers of one journal that each believe they
/// are the only one. `Clone` stays, because forking a scan deliberately is a thing a caller
/// may want to write down.
///
/// # Why the integrity check is a type parameter
///
/// So that a recovery cannot verify with a different check from the one that sealed what it
/// is walking. It defaults to [`Catalogued`], so `Recovery` is the shipped check and a
/// caller that wants another writes it down at the type, where it is visible in every
/// signature the value passes through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery<C: IntegrityCheck = Catalogued> {
    region: JournalRegion,
    offset: u32,
    ending: Option<Ending>,
    /// The check this recovery verifies with. Zero-sized: [`IntegrityCheck`]'s methods take
    /// no `self`, so there is nothing to carry and the field costs no bytes.
    check: PhantomData<C>,
}

/// What one step staged, once it is known to be a record this page can hold.
#[derive(Clone, Copy)]
struct Staged {
    /// Bytes of the page the record occupies: its padded frame body and its commit seal.
    need: usize,
    /// Where the commit seal starts inside those bytes, which is the padded body's length.
    seal_at: usize,
    /// Bytes the offset advances by, which is the whole record.
    stride: u32,
}

impl Recovery<Catalogued> {
    /// A recovery of `region`, verifying with the shipped integrity check.
    #[must_use]
    #[inline]
    pub const fn new(region: JournalRegion) -> Self {
        Self::with_integrity(region)
    }
}

impl<C: IntegrityCheck> Recovery<C> {
    /// A recovery of `region`, verifying with `C`.
    ///
    /// [`new`](Recovery::new) is this at `C = Catalogued`. A recovery with the wrong `C`
    /// does not misread a journal: it stops at the first frame with
    /// [`DecodeError::IntegrityFailed`], because a seal computed by one algorithm is
    /// overwhelmingly unlikely to verify under another.
    #[must_use]
    pub const fn with_integrity(region: JournalRegion) -> Self {
        Self {
            region,
            offset: 0,
            ending: None,
            check: PhantomData,
        }
    }

    /// The region this recovery walks.
    ///
    /// The one way the region leaves this type, and it exists so that
    /// [`crate::append::Journal`] can be built from a finished recovery and nothing else: a
    /// writer that took a region and an offset separately could be handed two that do not
    /// belong together, which is the mismatch [`JournalRegion::of`] exists to refuse one
    /// level down.
    #[must_use]
    pub const fn region(&self) -> JournalRegion {
        self.region
    }

    /// The byte at which the committed prefix ends, relative to the region's base.
    ///
    /// # Postconditions
    ///
    /// Zero before the first step; after a step that yielded a record, past that record and
    /// its padding; after a step that failed, still at the *start* of the frame that failed
    /// — §14's "frame ignored; previous history prefix wins" is exactly that offset. Always
    /// a whole number of the region's program units, and never greater than
    /// [`JournalRegion::bytes`].
    ///
    /// This is where history *ended*. It is not where the next record may be written unless
    /// [`append_offset`](Self::append_offset) says so.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// How the scan ended, or [`None`] while it is still running.
    ///
    /// [`None`] is also the answer for a scan a caller abandoned part-way — after its replay
    /// cursor refused an out-of-sequence record, say — which is why
    /// [`append_offset`](Self::append_offset) is derived from this rather than from
    /// [`offset`](Self::offset): an unfinished scan has no append point either.
    #[must_use]
    pub const fn ending(&self) -> Option<Ending> {
        self.ending
    }

    /// Where the next record may be written, relative to the region's base.
    ///
    /// Issue #23's "establish the append offset for the next record as a by-product of the
    /// scan", with the by-product made hard to misuse.
    ///
    /// # Postconditions
    ///
    /// [`Some`] exactly when the scan ran to an erased tail, and then **every byte from it
    /// to the end of the region is erased**. [`None`] for a scan that stopped at damage,
    /// gave up, or has not finished — see the module documentation for why appending after
    /// any of those bricks a bank rather than merely risking one.
    #[must_use]
    pub const fn append_offset(&self) -> Option<u32> {
        match self.ending {
            Some(Ending::Clean { append_at }) => Some(append_at),
            _ => None,
        }
    }

    /// The next committed record, staged into `page`.
    ///
    /// The caller pumps: it owns the page, it may overwrite it the moment it has dealt with
    /// the record, and nothing here holds a borrow of it between calls.
    ///
    /// # Postconditions
    ///
    /// [`None`] once the scan has ended, for ever. A record borrows `page` and is valid
    /// until the next call. At most two reads are issued per record, plus — once, at an
    /// erased header — a walk of the rest of the region in page-sized chunks.
    ///
    /// # Errors
    ///
    /// [`RecoveryError::Decode`] for a frame that is malformed, integrity-failed, truncated
    /// against the region, or wearing a record kind this firmware does not know;
    /// [`RecoveryError::Storage`] when a read fails; and
    /// [`RecoveryError::PageTooSmall`] when `page` cannot hold the next frame. Every one of
    /// them ends the scan.
    pub fn next<'page, S: StableStorage>(
        &mut self,
        storage: &mut S,
        page: &'page mut [u8],
    ) -> Option<Result<RecordRef<'page>, RecoveryError<S::Error>>> {
        // Split in two so that every mutable use of the page is behind us before the record
        // borrows it: `stage` fills the page, and nothing below writes to it.
        let staged = match self.stage(storage, &mut *page)? {
            Ok(staged) => staged,
            Err(error) => return Some(Err(error)),
        };
        let frozen: &'page [u8] = &*page;
        let Some(bytes) = frozen.get(..staged.need) else {
            // Unreachable: `stage` refused a `need` the page could not hold. Spelled as a
            // refusal rather than an `unwrap` because the workspace denies both, and a
            // reader walking bytes off a damaged device is the last place to make an
            // exception.
            self.ending = Some(Ending::Incomplete { at: self.offset });
            return Some(Err(RecoveryError::PageTooSmall {
                needed: staged.need,
            }));
        };
        match frame::decode_with::<C>(bytes) {
            Ok(frame) => {
                // §09's first stop condition, before the record kind: what an uncommitted
                // frame *says* is not a question worth asking. `stage` read the whole
                // record, so the seal is already in the page.
                let Some(seal) = bytes.get(staged.seal_at..) else {
                    // Unreachable: `stage` staged `need > seal_at` bytes.
                    self.ending = Some(Ending::Incomplete { at: self.offset });
                    return Some(Err(RecoveryError::PageTooSmall {
                        needed: staged.need,
                    }));
                };
                if !frame::commit_seal_holds(frame.frame_crc, seal) {
                    self.ending = Some(Ending::Unsealed { at: self.offset });
                    return Some(Err(RecoveryError::Decode(DecodeError::Unsealed)));
                }
                match frame.decoded {
                    Decoded::Record(record) => {
                        // `stride >= FRAME_OVERHEAD_BYTES > 0`, so the offset always moves and a
                        // scan over a finite region is finite. Checked rather than saturating,
                        // because this is the one arithmetic here whose degenerate answer is an
                        // *affirmative* one: an offset that saturated would make `remaining` zero,
                        // and the next step would report a clean end at `u32::MAX` — "safe to
                        // append", at an offset outside the device. `stride <= remaining` is
                        // checked before the read, so the `else` is unreachable and is spelled as
                        // the refusal it would have to be.
                        let Some(next) = self.offset.checked_add(staged.stride) else {
                            self.ending = Some(Ending::Incomplete { at: self.offset });
                            return Some(Err(RecoveryError::Decode(
                                DecodeError::LengthOutOfBounds,
                            )));
                        };
                        self.offset = next;
                        Some(Ok(record))
                    }
                    Decoded::UnknownKind(_) => {
                        // §09 makes skipping a property of the format version, and
                        // `permits_unknown_record_skip` answers `false` for every one of the 256
                        // a version byte can hold. There is deliberately no second arm: a branch
                        // no test can reach is a branch whose first execution is recovery after
                        // a power loss on somebody's device.
                        self.ending = Some(Ending::Damaged { at: self.offset });
                        Some(Err(RecoveryError::Decode(DecodeError::UnknownRecordKind)))
                    }
                }
            }
            Err(error) => {
                self.ending = Some(Ending::Damaged { at: self.offset });
                Some(Err(RecoveryError::Decode(error)))
            }
        }
    }

    /// Reads the next frame into `page`, or ends the scan.
    ///
    /// [`None`] means the journal ended cleanly and [`Recovery::ending`] now says so;
    /// `Some(Err(..))` means it ended some other way, and the ending says which.
    #[allow(
        clippy::too_many_lines,
        reason = "one frame's worth of stop conditions, each of which is a different \
                  ending; splitting them would hide which branch sets which"
    )]
    fn stage<S: StableStorage>(
        &mut self,
        storage: &mut S,
        page: &mut [u8],
    ) -> Option<Result<Staged, RecoveryError<S::Error>>> {
        if self.ending.is_some() {
            return None;
        }
        // The region carries the device it was validated against, and this is what makes that
        // more than bookkeeping: a step on any other device is refused before a byte is read.
        // Codex found the half that survived carrying the geometry — reads that all succeed,
        // a clean ending, and an append offset the *caller's* device cannot program at.
        if storage.geometry() != self.region.geometry {
            return Some(Err(self.incomplete(RecoveryError::WrongDevice)));
        }
        let read_unit = self.region.geometry.read_size();
        let remaining = self.region.bytes.saturating_sub(self.offset);
        if remaining == 0 {
            // The region is full to its last byte. That is a clean end: the prefix is whole,
            // and whether a record fits at the offset it reports is the caller's arithmetic.
            self.ending = Some(Ending::Clean {
                append_at: self.offset,
            });
            return None;
        }

        // Whole read units of the page, because §12 validates a read's length as well as its
        // offset and a partial unit is a read no adapter may accept.
        let page_bytes = u32::try_from(page.len()).unwrap_or(u32::MAX);
        let capacity = page_bytes & !read_unit.wrapping_sub(1);
        let Some(header_need) = round_up(HEADER_WIDTH, read_unit) else {
            return Some(Err(self.incomplete(RecoveryError::PageTooSmall {
                needed: HEADER_BYTES,
            })));
        };
        // A whole number of read units either way: `header_need` is one by construction and
        // `remaining` is one because the region's length and every stride are.
        //
        // The page is sized against `want` rather than against `header_need`, and that is the
        // difference between agreeing with `frame::Scan` and disagreeing with it: a region
        // whose tail is shorter than a header needs only that tail read, and refusing because
        // the page could not have held a *whole* header would report `Incomplete` — the prefix
        // may be short — where the scan reports a clean end of history.
        let want = header_need.min(remaining);
        if capacity < want {
            return Some(Err(self.incomplete(RecoveryError::PageTooSmall {
                needed: usize::try_from(want).unwrap_or(usize::MAX),
            })));
        }
        let Ok(want_bytes) = usize::try_from(want) else {
            return Some(Err(
                self.incomplete(RecoveryError::PageTooSmall { needed: usize::MAX })
            ));
        };
        if let Err(error) = self.read(storage, page, self.offset, want_bytes) {
            return Some(Err(self.incomplete(error)));
        }

        let declared = {
            let Some(head) = page.get(..want_bytes) else {
                return Some(Err(
                    self.incomplete(RecoveryError::PageTooSmall { needed: want_bytes })
                ));
            };
            if want_bytes < HEADER_BYTES {
                // Fewer bytes left in the region than a header. Erased is the ordinary end
                // of a journal; programmed bytes are a torn header, and reporting an end of
                // history there hands a caller an offset pointing into cells a program cycle
                // has already cleared — which on NOR cannot be written again without erasing
                // the block.
                if is_erased(head) {
                    self.ending = Some(Ending::Clean {
                        append_at: self.offset,
                    });
                    return None;
                }
                return Some(Err(
                    self.damaged(RecoveryError::Decode(DecodeError::Truncated))
                ));
            }
            match head.get(..HEADER_BYTES) {
                Some(header) if is_erased(header) => None,
                Some(header) => Some(frame::frame_len_of_with::<C>(header)),
                None => Some(Err(DecodeError::Truncated)),
            }
        };

        let Some(declared) = declared else {
            // An erased header ends history only if everything after it is erased too.
            // Otherwise it is a hole — an erased run with records on the far side of it —
            // and calling that the end of history would hand back a prefix missing records
            // the device still holds, and an append offset pointing at cells a later frame
            // already occupies.
            //
            // The walk needs `capacity >= read_unit` to advance, and it has it. This branch
            // is reachable only when `want_bytes >= HEADER_BYTES`, and no region can leave a
            // remainder strictly between `HEADER_BYTES` and `header_need`: `remaining` is a
            // whole number of program units and a program unit is whole read units. So
            // `want == header_need >= read_unit` here, and `capacity >= want`.
            return match self.erased_to_end(storage, page, capacity) {
                Ok(true) => {
                    self.ending = Some(Ending::Clean {
                        append_at: self.offset,
                    });
                    None
                }
                Ok(false) => Some(Err(
                    self.damaged(RecoveryError::Decode(DecodeError::IntegrityFailed))
                )),
                Err(error) => Some(Err(self.incomplete(error))),
            };
        };

        let frame_len = match declared {
            Ok(frame_len) => frame_len,
            Err(error) => return Some(Err(self.damaged(RecoveryError::Decode(error)))),
        };
        // The frame body padded to the granularity the journal was written at, and the
        // commit seal that follows it. A record whose two parts run past the end of the
        // region could not have been written into it, so the region is shorter than the
        // record it appears to hold — a truncation and not a record.
        let (Some(body), Some(seal)) = (
            self.region
                .align
                .round_up(frame_len)
                .and_then(|body| u32::try_from(body).ok()),
            u32::try_from(frame::seal_bytes(self.region.align)).ok(),
        ) else {
            return Some(Err(
                self.damaged(RecoveryError::Decode(DecodeError::LengthOutOfBounds))
            ));
        };
        let Some(stride) = body.checked_add(seal) else {
            return Some(Err(
                self.damaged(RecoveryError::Decode(DecodeError::LengthOutOfBounds))
            ));
        };
        if stride > remaining {
            return Some(Err(
                self.damaged(RecoveryError::Decode(DecodeError::Truncated))
            ));
        }
        // The whole record is staged, seal included, and no rounding is needed to do it: a
        // padded body and a seal are both whole multiples of the region's granularity, which
        // is at least the device's program unit, which — a geometry nests — is whole read
        // units. `JournalRegion::spanning` is what guarantees that, and `stride <= remaining`
        // is what keeps the read inside the region.
        let need_bytes = usize::try_from(stride).unwrap_or(usize::MAX);
        if stride > capacity {
            return Some(Err(
                self.incomplete(RecoveryError::PageTooSmall { needed: need_bytes })
            ));
        }
        if let Err(error) = self.read(storage, page, self.offset, need_bytes) {
            return Some(Err(self.incomplete(error)));
        }
        Some(Ok(Staged {
            need: need_bytes,
            seal_at: usize::try_from(body).unwrap_or(usize::MAX),
            stride,
        }))
    }

    /// Fills `page[..len]` from `offset` bytes into the region.
    ///
    /// # Errors
    ///
    /// The driver's own, or [`RecoveryError::PageTooSmall`] for a `len` the page cannot hold
    /// — which every caller has already ruled out, and which is still a refusal rather than a
    /// silent success. Answering `Ok(())` without filling the page would leave the *previous*
    /// frame staged: a whole frame, with both seals holding, which the caller would decode
    /// and yield as a duplicate record. "Never a record invented out of stale bytes" has to
    /// include stale bytes this module put there itself.
    fn read<S: StableStorage>(
        &self,
        storage: &mut S,
        page: &mut [u8],
        offset: u32,
        len: usize,
    ) -> Result<(), RecoveryError<S::Error>> {
        let Some(target) = page.get_mut(..len) else {
            return Err(RecoveryError::PageTooSmall { needed: len });
        };
        storage
            .read(self.region.base.saturating_add(offset), target)
            .map_err(RecoveryError::Storage)
    }

    /// Whether the region is erased from [`offset`](Self::offset) to its end.
    ///
    /// Read in `capacity`-sized chunks, so the RAM this costs is the caller's page and the
    /// reads this costs are bounded by the region rather than by history. It is paid once,
    /// at the end of a scan, and it is what stops a hole from reading as the end of a
    /// journal.
    fn erased_to_end<S: StableStorage>(
        &self,
        storage: &mut S,
        page: &mut [u8],
        capacity: u32,
    ) -> Result<bool, RecoveryError<S::Error>> {
        let mut at = self.offset;
        while at < self.region.bytes {
            let want = capacity.min(self.region.bytes.saturating_sub(at));
            let Ok(want_bytes) = usize::try_from(want) else {
                // Unreachable on any target this crate builds for, and false is the safe
                // answer: it reports damage rather than a clean end, so nothing is appended.
                return Ok(false);
            };
            self.read(storage, page, at, want_bytes)?;
            let Some(chunk) = page.get(..want_bytes) else {
                return Ok(false);
            };
            if !is_erased(chunk) {
                return Ok(false);
            }
            // `capacity` is at least one read unit, so this always advances — and checked
            // rather than saturating for the reason the offset advance is: `false` is the
            // safe answer here, and an `at` that stopped moving would spin.
            let Some(next) = at.checked_add(want) else {
                return Ok(false);
            };
            at = next;
        }
        Ok(true)
    }

    /// Ends the scan at the current offset with a final prefix, and answers `error`.
    const fn damaged<E>(&mut self, error: RecoveryError<E>) -> RecoveryError<E> {
        self.ending = Some(Ending::Damaged { at: self.offset });
        error
    }

    /// Ends the scan at the current offset with a prefix that may be short, and answers
    /// `error`.
    const fn incomplete<E>(&mut self, error: RecoveryError<E>) -> RecoveryError<E> {
        self.ending = Some(Ending::Incomplete { at: self.offset });
        error
    }
}

/// Whether every byte of `bytes` reads as an erased NOR cell.
///
/// One definition rather than three copies of `iter().all(..)`. Erased is [`ERASED_BYTE`], a
/// constant, and never something learned from the device: an adapter whose `erase` does
/// nothing on media reading `0x00` would teach a learning reader that nothing is programmable
/// and that it had no questions to ask.
fn is_erased(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte == ERASED_BYTE)
}

/// `len` rounded up to a whole number of `unit`s, or [`None`] on overflow.
///
/// `unit` is a power of two on every path that reaches here — [`Geometry`] refuses anything
/// else — so this is an add and a mask rather than a division. See
/// [`crate::storage`]'s note on why that matters on `thumbv6m-none-eabi`.
const fn round_up(len: u32, unit: u32) -> Option<u32> {
    let mask = unit.wrapping_sub(1);
    match len.checked_add(mask) {
        Some(sum) => Some(sum & !mask),
        None => None,
    }
}

// A recovery that grew an inline page would be a type whose size tracked the caller's
// buffer, and §04's 768 B runtime budget — stated *with* a 512 B page rather than with two —
// has no room for a second one. Checked where a mistake is a compile error, the way
// `ReplayCursor`'s size is: the equality is the point, because a `<=` leaves room to hide a
// buffer in.
const _: () = assert!(size_of::<JournalRegion>() == 28);
const _: () = assert!(size_of::<Recovery>() == 40);
// And the whole of it is the region, the offset and the verdict: no fourth field, and in
// particular no page. A `<=` here would leave room to hide one in, which is why the two above
// are equalities and why this restates the sum rather than trusting them separately.
const _: () = assert!(size_of::<Recovery>() == size_of::<JournalRegion>() + 4 + 8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_up_lands_on_the_next_unit() {
        assert_eq!(round_up(0, 4), Some(0));
        assert_eq!(round_up(1, 4), Some(4));
        assert_eq!(round_up(4, 4), Some(4));
        assert_eq!(round_up(5, 1), Some(5));
        assert_eq!(round_up(u32::MAX, 4), None);
    }

    #[test]
    fn every_ending_but_a_clean_one_withholds_the_append_offset() {
        // The invariant stated over the type rather than over a journal: there is no way to
        // get an offset out of a recovery that did not run to erased media.
        let geometry = Geometry::new(64, 32, 1, 1).expect("two whole blocks");
        let region = JournalRegion::spanning(geometry, 0, 32, ProgramAlign::BYTE)
            .expect("this region is a legal program");
        for (ending, expected) in [
            (Ending::Clean { append_at: 8 }, Some(8)),
            (Ending::Damaged { at: 8 }, None),
            (Ending::Incomplete { at: 8 }, None),
        ] {
            let recovery = Recovery::<Catalogued> {
                region,
                offset: 8,
                ending: Some(ending),
                check: PhantomData,
            };
            assert_eq!(recovery.append_offset(), expected);
        }
        let running = Recovery::new(region);
        assert_eq!(running.ending(), None);
        assert_eq!(running.append_offset(), None);
    }
}
