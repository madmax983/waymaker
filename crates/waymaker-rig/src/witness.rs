//! What the rig had done when the supply went, made durable.
//!
//! # Why a rig needs one at all
//!
//! `waymaker-fault` keeps a `Ledger` in RAM: which records were
//! merely attempted, which were acknowledged, which effects were dispatched. On a host that
//! is enough, because the "crash" is a return from a function. On a board it is not, because
//! a power cut takes RAM with it, and the only thing left is media — which is the thing under
//! test rather than the thing that judges it.
//!
//! Two of design document §14's guarantees are statements about the *writer* and cannot be
//! read off the journal:
//!
//! * `acknowledged-durability` — "any record acknowledged after its barrier is recovered
//!   after reset". Whether a barrier returned is not a fact about bytes.
//! * `durable-intent` — "no Waymaker-dispatched effect lacks a recoverable schedule record".
//!   Whether an effect was physically dispatched is not a fact about bytes either.
//!
//! So the rig writes what it knew, as it knew it, to a region of its own.
//!
//! # The order, and why it is that order
//!
//! For each record `i`:
//!
//! 1. [`Stage::Attempted`] `i` — programmed **before** the record's first program.
//! 2. the record itself, through §07's two barriers.
//! 3. [`Stage::Acknowledged`] `i` — programmed **after** the record's commit barrier returned.
//!
//! and, for a scheduled effect whose schedule record is `i`, [`Stage::Dispatched`] `i`
//! **before** the physical effect.
//!
//! Every one of those choices is about which way an interrupted mark is allowed to be wrong.
//! A torn `Acknowledged` under-claims — the rig demands less of recovery than it might have —
//! and a torn `Attempted` under-claims too, but harmlessly, because a mark that did not land
//! is a record whose first program had not begun. `Dispatched` is written *before* the effect
//! precisely so that it over-claims: an effect the rig may not have got round to performing
//! still has its schedule record demanded, and demanding more of recovery than happened is
//! the safe direction for an instrument.
//!
//! # Why a mark is sealed, and why the seal is masked
//!
//! Because "erased" and "torn" and "written" have to be three different answers. A mark
//! carries a magic that is neither `0xFFFF` nor `0x0000`, a stage byte that is neither
//! `0x00` nor `0xFF`, a reserved byte that must be zero, and a check over its own body
//! computed with the same [`IntegrityCheck`] the firmware seals frames with.
//!
//! The check is stored with **bit 7 of each byte cleared**, which is
//! [ADR 0019](https://github.com/madmax983/waymaker/blob/main/docs/adr/0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md)'s
//! trick for the commit seal, applied here for exactly the reason it was invented there: it
//! makes **no byte a mark writes ever equal `0xFF`**. Without it the seal is the one field
//! with no guard against erased media, and a tear that landed eleven of twelve bytes leaves
//! `check | 0xFF00` on media — which verifies whenever the real check's high byte is `0xFF`,
//! one mark in 256. That is not a corner: `waymaker_fault::injections` enumerates a tear at
//! every byte of every program, so it is an ordinary crash point, and a falsely-accepted torn
//! `Attempted` mark raises the high water for a record whose first program never began —
//! loosening `Audit::saw`'s only defence against recovery inventing a record.
//!
//! The masking costs two bits, so the check is fourteen wide rather than sixteen. That is the
//! same trade ADR 0019 made and is worth it for the same reason: a collision is one in 16384,
//! and a torn mark is now *impossible* rather than merely unlikely.
//!
//! # What this is not
//!
//! A journal. There is no recovery here, no replay, no compaction; a witness is erased
//! between iterations and holds one iteration's marks. It is an instrument, and it is metered
//! as [`Traffic::Rig`](crate::wear::Traffic::Rig) so that its cost never appears in the
//! engine's published write amplification.

use waymaker_flash::integrity::{Catalogued, IntegrityCheck};
use waymaker_flash::storage::{Geometry, StableStorage};

/// How many bytes a mark occupies, before the slot is rounded to a program unit.
pub const MARK_BYTES: usize = 12;

/// The magic a mark opens with. Neither erased nor all-zero, so neither erased media nor a
/// fully cleared slot reads as one.
pub const MARK_MAGIC: u16 = 0x5752;

/// How many bytes of a mark the check covers.
const MARK_BODY_BYTES: usize = MARK_BYTES - 2;

/// The flag bit that says the witness's last slot was torn.
const FLAG_TORN: u8 = 0b0000_0001;

/// The flag bit that says the witness names an iteration.
///
/// A flag rather than a reserved iteration value, because `u32::MAX` is a legal iteration and
/// a sentinel would make a real witness from it decode as an empty one — which
/// [`crate::audit::Audit`] reads as "the run never began", turning a recorded violation into a
/// clean run for anybody who investigates it from the log.
const FLAG_ITERATION: u8 = 0b0000_0010;

/// The bits of the check a seal byte keeps.
///
/// Bit 7 cleared, so a seal byte is never `0xFF` and erased media is never a seal. See the
/// module documentation for why two bits are worth that.
const SEAL_MASK: u8 = 0x7F;

/// How many bytes a mark occupies, as the offsets a geometry validates are counted.
///
/// Declared rather than narrowed from [`MARK_BYTES`]: `usize::try_from` is not `const`, and a
/// cast is exactly the operation this file refuses everywhere else. The two are held equal by
/// a compile-time assertion, so a mark that grew in one place and not the other does not
/// build.
const MARK_WORDS: u32 = 12;
// The assertion the doc above describes: it pins the *pair*, so a mark that grew in one place
// and not the other does not build. Pinning only `MARK_BYTES` would have left `MARK_WORDS`
// free to drift, and every `Witness::mark` would then have become a runtime `ShortBuffer`.
const _: () = assert!(MARK_BYTES == MARK_WORDS as usize);

/// What a mark says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// The record's first program is about to be issued.
    Attempted,
    /// The record's commit barrier returned.
    Acknowledged,
    /// The effect this record scheduled is about to be physically performed.
    Dispatched,
}

impl Stage {
    /// The byte a mark stores. Never `0x00` and never `0xFF`, so a cleared or erased slot
    /// cannot read as a stage.
    const fn code(self) -> u8 {
        match self {
            Self::Attempted => 0x11,
            Self::Acknowledged => 0x22,
            Self::Dispatched => 0x33,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0x11 => Some(Self::Attempted),
            0x22 => Some(Self::Acknowledged),
            0x33 => Some(Self::Dispatched),
            _ => None,
        }
    }

    /// A short static name, for a log a device with no allocator writes.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Attempted => "attempted",
            Self::Acknowledged => "acknowledged",
            Self::Dispatched => "dispatched",
        }
    }
}

/// Why a witness refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WitnessError<E = core::convert::Infallible> {
    /// The buffer is shorter than the bytes the operation needs.
    ShortBuffer,
    /// These bytes are not a mark: erased, torn, or never written.
    NotAMark,
    /// A valid mark sits past a slot that is not one. Marks are appended, so this is media
    /// the rig did not produce.
    Hole,
    /// Two marks name different iterations. A witness holds one iteration's marks.
    MixedIterations,
    /// A stage's indices did not increase. Marks are appended in order.
    OutOfOrder,
    /// The region has no slot left.
    Full,
    /// The region is not one this geometry permits.
    Region,
    /// The storage handed in is not the device the region was validated against.
    WrongGeometry,
    /// The driver refused.
    Driver(E),
}

impl<E> WitnessError<E> {
    /// A short static description of this refusal.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::ShortBuffer => "the buffer is shorter than the operation needs",
            Self::NotAMark => "these bytes are not a mark",
            Self::Hole => "a mark sits past a slot that is not one",
            Self::MixedIterations => "the witness holds marks from more than one iteration",
            Self::OutOfOrder => "a stage's mark indices did not increase",
            Self::Full => "the witness region has no slot left",
            Self::Region => "the region is not one this geometry permits",
            Self::WrongGeometry => "the storage is not the device the region was validated against",
            Self::Driver(_) => "the driver refused",
        }
    }
}

impl<E: core::fmt::Display> core::fmt::Display for WitnessError<E> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Driver(error) => error.fmt(formatter),
            other => formatter.write_str(other.message()),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> core::error::Error for WitnessError<E> {}

/// The seal `body` gets, with bit 7 of each byte cleared.
fn masked_seal<C: IntegrityCheck>(body: &[u8]) -> [u8; 2] {
    let [low, high] = C::header_check(body).to_le_bytes();
    [low & SEAL_MASK, high & SEAL_MASK]
}

/// One thing the rig durably knew.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mark {
    iteration: u32,
    index: u16,
    stage: Stage,
}

impl Mark {
    /// A mark saying `stage` happened to record `index` of `iteration`.
    #[must_use]
    pub const fn new(iteration: u32, index: u16, stage: Stage) -> Self {
        Self {
            iteration,
            index,
            stage,
        }
    }

    /// Which iteration of the rig wrote this.
    #[must_use]
    pub const fn iteration(self) -> u32 {
        self.iteration
    }

    /// Which record of that iteration it is about.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.index
    }

    /// What it says happened.
    #[must_use]
    pub const fn stage(self) -> Stage {
        self.stage
    }

    /// Writes this mark into `out`, sealed with the shipped integrity check.
    ///
    /// # Errors
    ///
    /// [`WitnessError::ShortBuffer`] if `out` is shorter than [`MARK_BYTES`].
    pub fn encode(self, out: &mut [u8]) -> Result<usize, WitnessError> {
        self.encode_with::<Catalogued>(out)
    }

    /// Writes this mark into `out`, sealed with `C`.
    ///
    /// # Errors
    ///
    /// [`WitnessError::ShortBuffer`] if `out` is shorter than [`MARK_BYTES`].
    pub fn encode_with<C: IntegrityCheck>(self, out: &mut [u8]) -> Result<usize, WitnessError> {
        let Some(slot) = out.get_mut(..MARK_BYTES) else {
            return Err(WitnessError::ShortBuffer);
        };
        let mut body = [0_u8; MARK_BODY_BYTES];
        let (magic, rest) = body.split_at_mut(2);
        magic.copy_from_slice(&MARK_MAGIC.to_le_bytes());
        let (iteration, rest) = rest.split_at_mut(4);
        iteration.copy_from_slice(&self.iteration.to_le_bytes());
        let (index, rest) = rest.split_at_mut(2);
        index.copy_from_slice(&self.index.to_le_bytes());
        let (stage, reserved) = rest.split_at_mut(1);
        let Some(code) = stage.first_mut() else {
            return Err(WitnessError::ShortBuffer);
        };
        *code = self.stage.code();
        reserved.fill(0);

        let (head, tail) = slot.split_at_mut(MARK_BODY_BYTES);
        head.copy_from_slice(&body);
        tail.copy_from_slice(&masked_seal::<C>(&body));
        Ok(MARK_BYTES)
    }

    /// Reads a mark from `bytes`, or says why these bytes are not one.
    ///
    /// # Errors
    ///
    /// [`WitnessError::ShortBuffer`] if `bytes` is shorter than [`MARK_BYTES`], and
    /// [`WitnessError::NotAMark`] for erased media, a torn program, an unknown stage, or a
    /// failed check.
    pub fn decode(bytes: &[u8]) -> Result<Self, WitnessError> {
        Self::decode_with::<Catalogued>(bytes)
    }

    /// Reads a mark from `bytes`, verified with `C`.
    ///
    /// # Errors
    ///
    /// As [`decode`](Self::decode).
    pub fn decode_with<C: IntegrityCheck>(bytes: &[u8]) -> Result<Self, WitnessError> {
        let Some(slot) = bytes.get(..MARK_BYTES) else {
            return Err(WitnessError::ShortBuffer);
        };
        let (body, seal) = slot.split_at(MARK_BODY_BYTES);
        let (magic, rest) = body.split_at(2);
        let (iteration, rest) = rest.split_at(4);
        let (index, rest) = rest.split_at(2);
        let (stage, reserved) = rest.split_at(1);

        let (Ok(magic), Ok(iteration), Ok(index), Ok(seal)) = (
            <[u8; 2]>::try_from(magic),
            <[u8; 4]>::try_from(iteration),
            <[u8; 2]>::try_from(index),
            <[u8; 2]>::try_from(seal),
        ) else {
            return Err(WitnessError::NotAMark);
        };
        if u16::from_le_bytes(magic) != MARK_MAGIC {
            return Err(WitnessError::NotAMark);
        }
        if reserved.iter().any(|byte| *byte != 0) {
            return Err(WitnessError::NotAMark);
        }
        let Some(stage) = stage.first().and_then(|code| Stage::from_code(*code)) else {
            return Err(WitnessError::NotAMark);
        };
        // Compared against the *masked* check, so a seal byte with bit 7 set — which no
        // encoder writes and erased media always has — cannot match whatever the body says.
        if masked_seal::<C>(body) != seal {
            return Err(WitnessError::NotAMark);
        }
        Ok(Self {
            iteration: u32::from_le_bytes(iteration),
            index: u16::from_le_bytes(index),
            stage,
        })
    }
}

/// Where the rig keeps its marks.
///
/// # Invariants
///
/// The region is validated as a *program* against a geometry: whatever may be programmed may
/// be read, and a witness that could be read but not written is one whose next mark has
/// nowhere to go. Every slot is a whole number of program units, so an interrupted mark can
/// only tear inside its own slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WitnessRegion {
    geometry: Geometry,
    base: u32,
    bytes: u32,
    slot_bytes: u32,
}

impl WitnessRegion {
    /// The `bytes` at `base` on `geometry`, as a witness region.
    ///
    /// # Errors
    ///
    /// [`WitnessError::Region`] if the region is not one `geometry` permits a program in, or
    /// if it cannot hold a single mark.
    pub const fn of(geometry: Geometry, base: u32, bytes: u32) -> Result<Self, WitnessError> {
        if geometry.validate_program(base, bytes).is_err() {
            return Err(WitnessError::Region);
        }
        let program = geometry.program_size();
        let Some(units) = MARK_WORDS.div_ceil(program).checked_mul(program) else {
            return Err(WitnessError::Region);
        };
        if units > bytes {
            return Err(WitnessError::Region);
        }
        Ok(Self {
            geometry,
            base,
            bytes,
            slot_bytes: units,
        })
    }

    /// The geometry this region was validated against.
    #[must_use]
    pub const fn geometry(self) -> Geometry {
        self.geometry
    }

    /// Where the region starts.
    #[must_use]
    pub const fn base(self) -> u32 {
        self.base
    }

    /// How long it is.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        self.bytes
    }

    /// How many bytes one mark occupies on this device.
    #[must_use]
    pub const fn slot_bytes(self) -> u32 {
        self.slot_bytes
    }

    /// How many marks the region holds.
    #[must_use]
    pub const fn capacity(self) -> u32 {
        self.bytes / self.slot_bytes
    }

    /// Where slot `index` starts, or `None` past the end.
    const fn slot_offset(self, index: u32) -> Option<u32> {
        if index >= self.capacity() {
            return None;
        }
        match index.checked_mul(self.slot_bytes) {
            Some(offset) => self.base.checked_add(offset),
            None => None,
        }
    }
}

/// What a scan of the witness found.
///
/// # Invariants
///
/// Every high-water mark is the *largest index a whole mark named*, so a mark that did not
/// land whole lowers nothing and raises nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Progress {
    iteration: Option<u32>,
    attempted: Option<u16>,
    acknowledged: Option<u16>,
    dispatched: Option<u16>,
    marks: u32,
    torn: bool,
}

impl Progress {
    /// A witness nothing has been read from.
    pub const EMPTY: Self = Self {
        iteration: None,
        attempted: None,
        acknowledged: None,
        dispatched: None,
        marks: 0,
        torn: false,
    };

    /// The iteration every mark named, or `None` for an empty witness.
    #[must_use]
    pub const fn iteration(self) -> Option<u32> {
        self.iteration
    }

    /// The highest record index whose first program was about to be issued.
    #[must_use]
    pub const fn attempted(self) -> Option<u16> {
        self.attempted
    }

    /// The highest record index whose commit barrier returned.
    #[must_use]
    pub const fn acknowledged(self) -> Option<u16> {
        self.acknowledged
    }

    /// The highest schedule-record index whose effect was dispatched.
    #[must_use]
    pub const fn dispatched(self) -> Option<u16> {
        self.dispatched
    }

    /// How many whole marks the scan read.
    #[must_use]
    pub const fn marks(self) -> u32 {
        self.marks
    }

    /// Whether the slot after the last whole mark held something other than erased media.
    ///
    /// The witness's own tear, and the direct evidence that the supply went during a mark
    /// rather than between two.
    #[must_use]
    pub const fn torn(self) -> bool {
        self.torn
    }

    /// This progress with `stage`'s high water raised to `index`.
    ///
    /// A [`Progress`] normally comes from a [`Witness::scan`] of real media. This builds one
    /// without media, and it is not a convenience: [`crate::log`] reconstructs a run's
    /// obligations from an encoded log line so that a violation found on a board is
    /// re-checkable on a host, and that reconstruction has no device to read.
    ///
    /// Raising only. A high water that could be lowered would let a caller *reduce* what the
    /// rig demands of recovery, which is the one direction an instrument must not move in.
    #[must_use]
    pub const fn raising(mut self, stage: Stage, index: u16) -> Self {
        let slot = match stage {
            Stage::Attempted => &mut self.attempted,
            Stage::Acknowledged => &mut self.acknowledged,
            Stage::Dispatched => &mut self.dispatched,
        };
        match *slot {
            Some(previous) if previous >= index => {}
            _ => *slot = Some(index),
        }
        self.marks = self.marks.saturating_add(1);
        self
    }

    /// How many bytes [`encode`](Self::encode) writes.
    pub const ENCODED_BYTES: usize = 12;

    /// A high water, as two bytes, with `0xFFFF` for "none".
    ///
    /// `u16::MAX` is not a reachable record index — `Workload::MAX_EFFECTS` refuses a run
    /// long before it — so it is free to stand for absence, and a reader that met it would
    /// demand nothing rather than demand everything. The *iteration* has no such spare value
    /// and does not use this trick; see [`FLAG_ITERATION`].
    const fn word(value: Option<u16>) -> [u8; 2] {
        match value {
            Some(index) => index.to_le_bytes(),
            None => u16::MAX.to_le_bytes(),
        }
    }

    /// The inverse of [`word`](Self::word).
    const fn unword(bytes: [u8; 2]) -> Option<u16> {
        match u16::from_le_bytes(bytes) {
            u16::MAX => None,
            index => Some(index),
        }
    }

    /// The three high waters and the iteration they belong to, little-endian.
    ///
    /// This is what makes issue [#27](https://github.com/madmax983/waymaker/issues/27)'s
    /// third "done when" true rather than nearly true. A log line carrying a seed, an
    /// iteration and a geometry can rebuild the *run*; it cannot rebuild what the rig
    /// **knew**, and the obligations §14 puts on a recovery are entirely statements about
    /// that. Without these twelve bytes a violation is reproducible only if the host still
    /// has the device.
    ///
    /// The mark count and the tear flag travel too, because `Audit::finish` reads both.
    ///
    /// # Errors
    ///
    /// `None` when `out` is shorter than [`ENCODED_BYTES`](Self::ENCODED_BYTES).
    #[must_use]
    pub fn encode(self, out: &mut [u8]) -> Option<usize> {
        let slot = out.get_mut(..Self::ENCODED_BYTES)?;
        let (iteration, rest) = slot.split_at_mut(4);
        iteration.copy_from_slice(&self.iteration.unwrap_or(u32::MAX).to_le_bytes());
        let (attempted, rest) = rest.split_at_mut(2);
        attempted.copy_from_slice(&Self::word(self.attempted));
        let (acknowledged, rest) = rest.split_at_mut(2);
        acknowledged.copy_from_slice(&Self::word(self.acknowledged));
        let (dispatched, rest) = rest.split_at_mut(2);
        dispatched.copy_from_slice(&Self::word(self.dispatched));
        let (marks, flags) = rest.split_at_mut(1);
        // Saturating: the count is a figure in a report, and a wrapped one would read as an
        // empty witness — which `Audit::finish` treats as "the run never began".
        let count = u8::try_from(self.marks.min(u32::from(u8::MAX))).unwrap_or(u8::MAX);
        marks.fill(count);
        // Presence is a flag rather than a reserved iteration number. `u32::MAX` is a legal
        // iteration — `Plan::cut` answers for it and a rig can be asked to run it — so a
        // sentinel would make a real witness from that iteration decode as *no* witness, and
        // the audit would then read it as the empty state it treats as "the run never began".
        // One bit costs nothing and the alternative silently loses a violation.
        let mut bits = 0_u8;
        if self.torn {
            bits |= FLAG_TORN;
        }
        if self.iteration.is_some() {
            bits |= FLAG_ITERATION;
        }
        flags.fill(bits);
        Some(Self::ENCODED_BYTES)
    }

    /// The progress [`encode`](Self::encode) wrote.
    ///
    /// # Errors
    ///
    /// `None` when `bytes` is shorter than [`ENCODED_BYTES`](Self::ENCODED_BYTES), or when
    /// the tear flag is neither zero nor one — a byte with no meaning is not a witness.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let slot = bytes.get(..Self::ENCODED_BYTES)?;
        let (iteration, rest) = slot.split_at(4);
        let (attempted, rest) = rest.split_at(2);
        let (acknowledged, rest) = rest.split_at(2);
        let (dispatched, rest) = rest.split_at(2);
        let (marks, flags) = rest.split_at(1);
        let iteration = u32::from_le_bytes(<[u8; 4]>::try_from(iteration).ok()?);
        let bits = flags.first().copied()?;
        if bits & !(FLAG_TORN | FLAG_ITERATION) != 0 {
            // A bit with no meaning is not a witness. The same rule the mark's reserved byte
            // is held to, for the same reason: an undefined bit nothing reads is an undefined
            // bit nothing can detect a change in.
            return None;
        }
        Some(Self {
            iteration: (bits & FLAG_ITERATION != 0).then_some(iteration),
            attempted: Self::unword(<[u8; 2]>::try_from(attempted).ok()?),
            acknowledged: Self::unword(<[u8; 2]>::try_from(acknowledged).ok()?),
            dispatched: Self::unword(<[u8; 2]>::try_from(dispatched).ok()?),
            marks: u32::from(marks.first().copied()?),
            torn: bits & FLAG_TORN != 0,
        })
    }

    /// Raises the high water for `stage`, refusing an index that did not increase.
    const fn accept(mut self, mark: Mark) -> Result<Self, WitnessError> {
        match self.iteration {
            None => self.iteration = Some(mark.iteration()),
            Some(iteration) if iteration == mark.iteration() => {}
            Some(_) => return Err(WitnessError::MixedIterations),
        }
        let slot = match mark.stage() {
            Stage::Attempted => &mut self.attempted,
            Stage::Acknowledged => &mut self.acknowledged,
            Stage::Dispatched => &mut self.dispatched,
        };
        match slot {
            Some(previous) if *previous >= mark.index() => return Err(WitnessError::OutOfOrder),
            _ => *slot = Some(mark.index()),
        }
        self.marks = self.marks.saturating_add(1);
        Ok(self)
    }
}

/// An append position in a [`WitnessRegion`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Witness {
    region: WitnessRegion,
    next: u32,
}

impl Witness {
    /// A witness positioned at the start of `region`.
    #[must_use]
    pub const fn new(region: WitnessRegion) -> Self {
        Self { region, next: 0 }
    }

    /// The region this witness appends to.
    #[must_use]
    pub const fn region(self) -> WitnessRegion {
        self.region
    }

    /// Which slot the next mark goes in.
    #[must_use]
    pub const fn next_slot(self) -> u32 {
        self.next
    }

    /// Checks `storage` is the device the region was validated against, and `page` is long
    /// enough for one slot.
    fn check<S: StableStorage>(
        self,
        storage: &S,
        page: &[u8],
    ) -> Result<usize, WitnessError<S::Error>> {
        if storage.geometry() != self.region.geometry() {
            return Err(WitnessError::WrongGeometry);
        }
        let Ok(slot) = usize::try_from(self.region.slot_bytes()) else {
            return Err(WitnessError::Region);
        };
        if page.len() < slot {
            return Err(WitnessError::ShortBuffer);
        }
        Ok(slot)
    }

    /// Programs `mark` into the next slot and barriers.
    ///
    /// The barrier is the point: a mark that is merely attempted says nothing, and the rig's
    /// obligations rest on marks that are durable *before* the thing they precede.
    ///
    /// # Errors
    ///
    /// [`WitnessError::Full`] when the region has no slot left, [`WitnessError::ShortBuffer`]
    /// when `page` is shorter than a slot, [`WitnessError::WrongGeometry`] when `storage` is
    /// not the device the region was validated against, and [`WitnessError::Driver`] for
    /// anything the device refused.
    pub fn mark<S: StableStorage>(
        &mut self,
        storage: &mut S,
        mark: Mark,
        page: &mut [u8],
    ) -> Result<(), WitnessError<S::Error>> {
        let slot_bytes = self.check(storage, page)?;
        let Some(offset) = self.region.slot_offset(self.next) else {
            return Err(WitnessError::Full);
        };
        let Some(slot) = page.get_mut(..slot_bytes) else {
            return Err(WitnessError::ShortBuffer);
        };
        // Erased, so the padding after a mark is media a later reader will not mistake for
        // one and a NOR program never has to clear.
        slot.fill(0xFF);
        if mark.encode(slot).is_err() {
            return Err(WitnessError::ShortBuffer);
        }
        storage
            .program(offset, slot)
            .map_err(WitnessError::Driver)?;
        storage.barrier().map_err(WitnessError::Driver)?;
        self.next = self.next.saturating_add(1);
        Ok(())
    }

    /// Reads every slot and reports what the rig durably knew.
    ///
    /// # Errors
    ///
    /// [`WitnessError::Hole`] for a mark past a slot that is not one, and the refusals
    /// [`mark`](Self::mark) lists.
    pub fn scan<S: StableStorage>(
        self,
        storage: &mut S,
        page: &mut [u8],
    ) -> Result<Progress, WitnessError<S::Error>> {
        let slot_bytes = self.check(storage, page)?;
        let mut progress = Progress::default();
        let mut ended = false;

        for index in 0..self.region.capacity() {
            let Some(offset) = self.region.slot_offset(index) else {
                break;
            };
            let Some(slot) = page.get_mut(..slot_bytes) else {
                return Err(WitnessError::ShortBuffer);
            };
            storage.read(offset, slot).map_err(WitnessError::Driver)?;

            match Mark::decode(slot) {
                Ok(mark) if !ended => {
                    progress = progress.accept(mark).map_err(promote)?;
                }
                Ok(_) => return Err(WitnessError::Hole),
                Err(_) if ended => {
                    // Everything past the end must be erased. Anything else is a second
                    // region of writing, which marks are never appended as.
                    if slot.iter().any(|byte| *byte != 0xFF) {
                        return Err(WitnessError::Hole);
                    }
                }
                Err(_) => {
                    ended = true;
                    progress.torn = slot.iter().any(|byte| *byte != 0xFF);
                }
            }
        }
        Ok(progress)
    }
}

/// Widens a driver-free refusal to one carrying a driver's error type.
const fn promote<E>(error: WitnessError) -> WitnessError<E> {
    match error {
        WitnessError::ShortBuffer => WitnessError::ShortBuffer,
        WitnessError::NotAMark => WitnessError::NotAMark,
        WitnessError::Hole => WitnessError::Hole,
        WitnessError::MixedIterations => WitnessError::MixedIterations,
        WitnessError::OutOfOrder => WitnessError::OutOfOrder,
        WitnessError::Full => WitnessError::Full,
        WitnessError::Region => WitnessError::Region,
        WitnessError::WrongGeometry => WitnessError::WrongGeometry,
        WitnessError::Driver(never) => match never {},
    }
}
