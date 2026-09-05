//! The two-barrier write discipline: the only way a record becomes committed history.
//!
//! Design document §07 Durable effect protocol, §09 Journal and wire format, and issue
//! [#24](https://github.com/madmax983/waymaker/issues/24). Each committed record costs two
//! barriers, and the order is not advice:
//!
//! 1. **Write the frame body** — header, payload and frame check, padded to the program
//!    unit — and *not* its commit seal.
//! 2. **Payload barrier.** The complete frame body must precede any durable seal.
//! 3. **Write the commit seal, then the commit barrier.** The seal is durable before the
//!    next irreversible action, which for a schedule record is dispatching the effect.
//!
//! # Why this is a type and not a comment
//!
//! Because a seal that reaches media ahead of the frame it seals is a record that recovery
//! calls committed and cannot read, and nothing about the failure is visible until a power
//! loss on somebody's device. §07 calls the ordering a protocol; issue #24 asks for a writer
//! in which "it is not possible to program a seal without the intervening payload barrier
//! having returned", and the way to make something impossible in Rust is to make it not
//! compile.
//!
//! So there are three types and not one. [`Journal`] is a position; [`Journal::stage`]
//! programs a frame body and hands back a [`Staged`]; [`Staged::payload_barrier`] is the
//! only thing that produces a [`Sealable`]; and [`Sealable::commit`] is the only thing that
//! programs a seal. A caller that wants to skip the barrier has nowhere to write it:
//! [`Staged`] has no `commit`, and there is no other constructor for [`Sealable`].
//!
//! ```
//! # use waymaker_flash::append::Sealable;
//! # use waymaker_flash::storage::StableStorage;
//! fn commit_after_the_barrier<S: StableStorage>(sealable: Sealable<'_, '_>, storage: &mut S) {
//!     let _ = sealable.commit(storage);
//! }
//! ```
//!
//! ```compile_fail,E0599
//! # use waymaker_flash::append::Staged;
//! # use waymaker_flash::storage::StableStorage;
//! fn commit_without_the_barrier<S: StableStorage>(staged: Staged<'_, '_>, storage: &mut S) {
//!     let _ = staged.commit(storage);
//! }
//! ```
//!
//! The two differ in one word. The first compiles, which is what stops the second from
//! failing for some unrelated reason — a `compile_fail` doctest with no compiling twin is a
//! test that passes when the type it names is deleted — and the second names `E0599`, so it
//! fails for "no method named `commit`" specifically rather than for a typo.
//!
//! # Where a writer may start
//!
//! [`Journal::after`] is the only constructor, and it takes a finished [`Recovery`] whose
//! [`append_offset`](Recovery::append_offset) answered [`Some`]. That is not
//! conservatism: appending anywhere else programs cells a cycle has already cleared, and on
//! NOR the bank never boots again — [`crate::recovery`]'s module documentation argues it at
//! length. A constructor taking a region and an offset would let a caller supply two that do
//! not belong together, so there is not one.
//!
//! # Write amplification
//!
//! Issue #24 asks for it as "a measurable output of the writer", and
//! [`WriteAmplification`] is that output: the payload bytes a record carried, the bytes
//! programmed to put it on media, and the two counts §07 adds — program calls and barriers.
//! [`Sealable::commit`] returns one record's, and [`Journal::amplification`] accumulates.
//!
//! What it does *not* do is divide. A ratio would link software division on
//! `thumbv6m-none-eabi`, which [`crate::storage`] measures at 408 B of an 8 KiB budget, to
//! compute a number a device with no console cannot print. The counters are exact, the
//! division is the host's, and [`WriteAmplification::overhead_bytes`] is the one derived
//! figure that costs a subtraction.
//!
//! # What this module must not own
//!
//! A driver, a bank, or a policy. It programs what it is handed, through §12's contract, at
//! an offset a recovery vouched for. Whether a record *should* be written — §10's capacity
//! reserve, `continue_as_new`, the bank swap — is not here, and neither is dispatch: §07
//! step 4 happens after [`Sealable::commit`] returns, in the caller.

use core::marker::PhantomData;

use waymaker_core::{DecodeError, RecordRef};

use crate::frame::{self, ProgramAlign};
use crate::integrity::{Catalogued, IntegrityCheck};
use crate::recovery::{JournalRegion, Recovery};
use crate::storage::StableStorage;

/// What one or more records cost the media they were written to.
///
/// # Invariants
///
/// Every field counts what the device was *asked* for rather than what it acknowledged.
/// §12 is explicit that "a failed program may still have changed media", so a call that
/// returned an error is still a program cycle this journal spent — and a wear figure that
/// only counted successes would understate exactly the runs that wore the part.
///
/// # What "amplification" is
///
/// [`programmed_bytes`](Self::programmed_bytes) over
/// [`payload_bytes`](Self::payload_bytes). A record of eight payload bytes on a device with
/// an eight-byte program unit costs a twenty-four-byte frame and an eight-byte seal, which
/// is four times its payload; the same record on a byte-programmable part costs twenty-one,
/// which is a little over two and a half. Neither number is computed here — see the module
/// documentation for why a division is not free on the target this runs on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct WriteAmplification {
    payload_bytes: u32,
    programmed_bytes: u32,
    program_operations: u32,
    barriers: u32,
}

impl WriteAmplification {
    /// A writer that has not been asked for anything.
    pub const NONE: Self = Self {
        payload_bytes: 0,
        programmed_bytes: 0,
        program_operations: 0,
        barriers: 0,
    };

    /// Payload bytes the records carried — §09's `payload_len`, summed.
    #[must_use]
    pub const fn payload_bytes(self) -> u32 {
        self.payload_bytes
    }

    /// Bytes programmed to put them on media: frames, padding and commit seals.
    #[must_use]
    pub const fn programmed_bytes(self) -> u32 {
        self.programmed_bytes
    }

    /// How many `program` calls that took.
    ///
    /// Two per record, and structurally so: the frame body and the commit seal cannot share
    /// a call, because a barrier goes between them.
    #[must_use]
    pub const fn program_operations(self) -> u32 {
        self.program_operations
    }

    /// How many barriers that took.
    ///
    /// Two per record, which is §07's whole cost. On real NOR a barrier is what dominates:
    /// the bytes are a burst and the barrier is a round trip.
    #[must_use]
    pub const fn barriers(self) -> u32 {
        self.barriers
    }

    /// Programmed bytes that were not payload.
    ///
    /// Saturating, so the degenerate answer is zero rather than a wrap. It cannot be
    /// reached — a frame is never smaller than its payload — and a wrapped overhead would be
    /// a number four billion times too large in a report somebody trusts.
    #[must_use]
    pub const fn overhead_bytes(self) -> u32 {
        self.programmed_bytes.saturating_sub(self.payload_bytes)
    }

    /// The two totals added.
    ///
    /// Saturating in every field, for the reason [`overhead_bytes`](Self::overhead_bytes) is:
    /// a counter that wrapped would report a device as almost unused after four billion
    /// bytes, which is the one direction a wear figure must not fail in.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self {
            payload_bytes: self.payload_bytes.saturating_add(other.payload_bytes),
            programmed_bytes: self.programmed_bytes.saturating_add(other.programmed_bytes),
            program_operations: self
                .program_operations
                .saturating_add(other.program_operations),
            barriers: self.barriers.saturating_add(other.barriers),
        }
    }

    /// This total plus a record carrying `bytes` of payload.
    const fn carrying(self, bytes: u32) -> Self {
        Self {
            payload_bytes: self.payload_bytes.saturating_add(bytes),
            ..self
        }
    }

    /// This total plus one program of `bytes`.
    const fn programming(self, bytes: u32) -> Self {
        Self {
            programmed_bytes: self.programmed_bytes.saturating_add(bytes),
            program_operations: self.program_operations.saturating_add(1),
            ..self
        }
    }

    /// This total plus one barrier.
    const fn barriering(self) -> Self {
        Self {
            barriers: self.barriers.saturating_add(1),
            ..self
        }
    }
}

/// Why a record could not be appended.
///
/// Generic over the driver's error, for the reason
/// [`RecoveryError`](crate::recovery::RecoveryError) is: §12 lets every port name its own,
/// and flattening them throws away the only thing a driver author can act on. Deliberately
/// no [`Display`](core::fmt::Display) either, and for the same reason — the bound would
/// spread to every signature this type appears in, and every variant already carries
/// something better than a string.
///
/// Not `#[non_exhaustive]`, for the reason [`waymaker_core::DecodeError`] is not: every
/// match on it is in this workspace, and an exhaustive match is how the compiler tells
/// whoever adds a variant which call sites now have a case to think about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppendError<E> {
    /// The media refused a program or a barrier.
    ///
    /// What is on media afterwards is unknown, and deliberately not guessed at: §12 says a
    /// failed program may still have changed it, and a caller that met an error at a barrier
    /// has learned nothing about what preceded it. The journal is not advanced, so the next
    /// boot's recovery is what decides — which will find either an unsealed frame or nothing
    /// at all, and neither is history.
    Storage(E),
    /// The record could not be encoded, or the caller's page could not hold it.
    ///
    /// [`DecodeError::LengthOutOfBounds`] in both cases, and told apart the way
    /// [`frame::encode`] documents: a record that fails [`frame::encoded_len`] can never be
    /// written, and one that passes it and fails here needs a bigger page.
    Encode(DecodeError),
    /// The storage handed to a step is not the device the region was validated against.
    ///
    /// The same refusal [`RecoveryError::WrongDevice`](crate::recovery::RecoveryError::WrongDevice)
    /// makes, in the direction that programs rather than reads — which is the worse
    /// direction: an append at an offset proved legal on another device is a write outside
    /// the journal.
    WrongDevice,
    /// The record does not fit in what is left of the region.
    ///
    /// Refused before a byte moves. A record half-appended at the end of a bank is a bank
    /// that cannot be booted *or* grown: the tail is programmed, so nothing may be written
    /// there, and the frame is incomplete, so recovery stops at it.
    NoRoom {
        /// Bytes the record needs, seal included.
        needed: u32,
        /// Bytes left in the region.
        available: u32,
    },
}

/// A position in one journal that records may be appended at.
///
/// # Invariants
///
/// * [`offset`](Self::offset) only ever moves forward, and only when a record's commit
///   barrier has returned. A record that failed at any step is not history and does not
///   advance it.
/// * Every byte from [`offset`](Self::offset) to the end of the region is erased. That is
///   established once, by [`after`](Self::after), and preserved by only ever appending whole
///   records at the offset.
/// * No borrow of the caller's page is retained past the record it staged.
///
/// # Why it is not `Copy`
///
/// Two writers appending to one journal at one offset would each overwrite the other's
/// record. `Clone` is not derived either, for the same reason: unlike a reader, a copied
/// *writer* is not a way to look ahead, it is a second appender.
#[derive(Debug, PartialEq, Eq)]
pub struct Journal<C: IntegrityCheck = Catalogued> {
    region: JournalRegion,
    offset: u32,
    written: WriteAmplification,
    /// The check this writer seals with. Zero-sized: [`IntegrityCheck`]'s methods take no
    /// `self`, so there is nothing to carry and the field costs no bytes.
    check: PhantomData<C>,
}

impl<C: IntegrityCheck> Journal<C> {
    /// A writer at the append point `recovery` established, or [`None`] if it established
    /// none.
    ///
    /// The only constructor. See the module documentation for why a region and an offset are
    /// not accepted separately.
    ///
    /// # Postconditions
    ///
    /// [`Some`] exactly when [`Recovery::append_offset`] is [`Some`] — so exactly when the
    /// scan ran to erased media, which is the one ending after which appending is safe. The
    /// writer seals with the same `C` the recovery verified with, because it is the same
    /// parameter: a journal read with one algorithm and extended with another is a journal
    /// neither reader can walk to its end.
    #[must_use]
    pub fn after(recovery: &Recovery<C>) -> Option<Self> {
        let offset = recovery.append_offset()?;
        Some(Self {
            region: recovery.region(),
            offset,
            written: WriteAmplification::NONE,
            check: PhantomData,
        })
    }

    /// Where the next record goes, relative to the region's base.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// How many bytes are left in the region.
    ///
    /// What a record has to fit in, and nothing more: §10's reserved tail — the space a
    /// terminal record or a `continue_as_new` needs — is a policy above this type, so a
    /// caller that keeps one subtracts it here rather than finding it already subtracted.
    #[must_use]
    pub const fn room(&self) -> u32 {
        self.region.bytes().saturating_sub(self.offset)
    }

    /// What this writer has cost the media since it was opened.
    #[must_use]
    pub const fn amplification(&self) -> WriteAmplification {
        self.written
    }

    /// Programs `record`'s frame body, and hands back the state a payload barrier consumes.
    ///
    /// §07 step 1. The whole record — frame, padding and commit seal — is encoded into
    /// `page`, and only the frame body is programmed; the seal stays in the page, borrowed
    /// by the returned [`Staged`], until [`Sealable::commit`] programs it. The borrow is
    /// what stops a caller from reusing the page between the two, which would program a seal
    /// over bytes that are no longer the seal.
    ///
    /// # Postconditions
    ///
    /// On success, `record`'s frame body is on media at [`offset`](Self::offset) and
    /// [`amplification`](Self::amplification) has counted one program. The offset does
    /// **not** move: an unsealed frame is not history, and a writer that advanced here would
    /// hand the next record an offset the reader will never reach.
    ///
    /// On failure nothing is on media *unless the driver says otherwise* — the three
    /// refusals below all happen before the program call, and only
    /// [`AppendError::Storage`] can leave the media changed.
    ///
    /// # Errors
    ///
    /// [`AppendError::WrongDevice`] when `storage` is not the device the region was
    /// validated against; [`AppendError::Encode`] when the record cannot be encoded or
    /// `page` cannot hold it; [`AppendError::NoRoom`] when the region cannot; and
    /// [`AppendError::Storage`] when the program fails.
    pub fn stage<'journal, 'page, S: StableStorage>(
        &'journal mut self,
        storage: &mut S,
        record: &RecordRef<'_>,
        page: &'page mut [u8],
    ) -> Result<Staged<'journal, 'page, C>, AppendError<S::Error>> {
        // Before anything: the device this region's arithmetic was proved against. Every
        // bound below — that the offset is programmable, that the record stays inside the
        // region — was established at construction, and proving them about one device and
        // programming another is the failure that cannot be seen from the outside.
        if storage.geometry() != self.region.geometry() {
            return Err(AppendError::WrongDevice);
        }
        let align = self.region.align();
        let (Ok(total), Ok(body)) = (
            frame::encoded_len(record, align),
            frame::body_len(record, align),
        ) else {
            return Err(AppendError::Encode(DecodeError::LengthOutOfBounds));
        };
        let (Ok(record_bytes), Ok(body_bytes)) = (u32::try_from(total), u32::try_from(body)) else {
            return Err(AppendError::Encode(DecodeError::LengthOutOfBounds));
        };
        // §10: "the runtime never overwrites committed history to make room". Decided here,
        // before a byte moves, because the alternative is a frame that runs off the end of a
        // bank.
        let available = self.room();
        if record_bytes > available {
            return Err(AppendError::NoRoom {
                needed: record_bytes,
                available,
            });
        }

        let written = frame::encode_with::<C>(record, align, page).map_err(AppendError::Encode)?;
        // `encode` reports the whole record and `body_len` says where its seal starts, so a
        // disagreement here is a codec that lost track of its own arithmetic rather than a
        // caller's mistake. Refused rather than asserted, because the workspace denies both
        // `panic!` and `unwrap`.
        if written != total {
            return Err(AppendError::Encode(DecodeError::LengthOutOfBounds));
        }
        let at = self.region.base().saturating_add(self.offset);
        let Some(frame_bytes) = page.get(..body) else {
            return Err(AppendError::Encode(DecodeError::LengthOutOfBounds));
        };
        let payload = payload_of(record);
        self.written = self.written.carrying(payload).programming(body_bytes);
        storage
            .program(at, frame_bytes)
            .map_err(AppendError::Storage)?;

        // The page is frozen from here: the seal rides on the caller's buffer rather than
        // being copied into this type, which is what keeps a writer the size of a position.
        let frozen: &'page [u8] = &*page;
        let Some(seal) = frozen.get(body..total) else {
            return Err(AppendError::Encode(DecodeError::LengthOutOfBounds));
        };
        Ok(Staged {
            record: WriteAmplification {
                payload_bytes: payload,
                programmed_bytes: body_bytes,
                program_operations: 1,
                barriers: 0,
            },
            journal: self,
            seal,
            seal_at: at.saturating_add(body_bytes),
            stride: record_bytes,
        })
    }
}

/// Payload bytes a record carries, for the amplification figure.
///
/// Not [`frame::body_len`] minus the overhead: a record's payload is a property of the
/// record, and deriving it from a padded length would report the padding as payload.
fn payload_of(record: &RecordRef<'_>) -> u32 {
    match record {
        RecordRef::RunStarted { input, .. } => {
            // §09 puts the workflow identity in the payload alongside the input, and the
            // frame's `payload_len` counts both.
            u32::try_from(input.len())
                .unwrap_or(u32::MAX)
                .saturating_add(4)
        }
        RecordRef::EffectCompleted { result, .. } | RecordRef::RunCompleted { result } => {
            u32::try_from(result.len()).unwrap_or(u32::MAX)
        }
        RecordRef::EffectFailed { error, .. } | RecordRef::RunFailed { error } => {
            u32::try_from(error.len()).unwrap_or(u32::MAX)
        }
        // ADR 0011: a sequence, a kind, a length and a digest, and nothing else.
        RecordRef::EffectScheduled { .. } => 8,
    }
}

/// A frame body on media, waiting for the barrier that lets it be sealed.
///
/// The whole of issue #24's "it is not possible to program a seal without the intervening
/// payload barrier having returned" is that this type has no method that programs one. The
/// only thing to do with it is [`payload_barrier`](Self::payload_barrier), and the only
/// thing that produces a [`Sealable`] is that call.
///
/// Dropping one is legal and leaves an unsealed frame on media. That is not a leak and not a
/// silent failure: recovery reports [`Ending::Unsealed`](crate::recovery::Ending::Unsealed)
/// at that frame, the record is not history, and the bank is recycled rather than appended
/// to. It is `#[must_use]` all the same, because dropping one is almost never what a caller
/// meant.
#[must_use = "a staged frame is not committed until its payload barrier and commit barrier \
              have returned"]
#[derive(Debug)]
pub struct Staged<'journal, 'page, C: IntegrityCheck = Catalogued> {
    journal: &'journal mut Journal<C>,
    /// The record's commit seal, still in the caller's page.
    seal: &'page [u8],
    /// Where that seal goes, as a device offset.
    seal_at: u32,
    /// What the whole record occupies, which is what the journal advances by.
    stride: u32,
    /// What this record has cost so far.
    record: WriteAmplification,
}

impl<'journal, 'page, C: IntegrityCheck> Staged<'journal, 'page, C> {
    /// §07 step 2: waits for the frame body to become durable.
    ///
    /// # Postconditions
    ///
    /// On success the frame body survives reset and a [`Sealable`] exists, which is the only
    /// value in this crate that can program a commit seal.
    ///
    /// On failure there is none. §12: a caller that met an error at a barrier "has learned
    /// nothing about what is on media and must treat every mutation since the last
    /// successful barrier as merely attempted" — so the staged frame is consumed and the
    /// record cannot be sealed at all, which is the failure closing in the direction that
    /// cannot commit half a protocol.
    ///
    /// # Errors
    ///
    /// [`AppendError::Storage`] if the barrier fails.
    pub fn payload_barrier<S: StableStorage>(
        self,
        storage: &mut S,
    ) -> Result<Sealable<'journal, 'page, C>, AppendError<S::Error>> {
        self.journal.written = self.journal.written.barriering();
        storage.barrier().map_err(AppendError::Storage)?;
        Ok(Sealable {
            journal: self.journal,
            seal: self.seal,
            seal_at: self.seal_at,
            stride: self.stride,
            record: self.record.barriering(),
        })
    }
}

/// A durable frame body, and the seal that will make it a committed record.
///
/// Reachable only from [`Staged::payload_barrier`]. There is no constructor and no other
/// method that returns one, which is what makes [`commit`](Self::commit) safe to be the one
/// place a seal is programmed.
#[must_use = "a sealable frame is not committed until `commit` has returned"]
#[derive(Debug)]
pub struct Sealable<'journal, 'page, C: IntegrityCheck = Catalogued> {
    journal: &'journal mut Journal<C>,
    seal: &'page [u8],
    seal_at: u32,
    stride: u32,
    record: WriteAmplification,
}

impl<C: IntegrityCheck> Sealable<'_, '_, C> {
    /// §07 step 3: programs the commit seal and waits for it to become durable.
    ///
    /// # Postconditions
    ///
    /// On success the record is committed history — a reader will yield it and stop past it
    /// — the journal's offset has moved by the whole record, and the returned
    /// [`WriteAmplification`] is what this one record cost: two programs, two barriers, and
    /// the bytes of both.
    ///
    /// On failure the offset does not move. The seal may or may not be on media, and this
    /// type deliberately does not guess: the next boot's recovery is what decides, and both
    /// answers it can give — a committed record or an unsealed frame — are legal states for
    /// a device that lost power here. What is *not* possible is a valid seal over an
    /// incomplete frame, because the frame became durable at
    /// [`Staged::payload_barrier`] before this call existed.
    ///
    /// # Errors
    ///
    /// [`AppendError::Storage`] if the program or the barrier fails.
    pub fn commit<S: StableStorage>(
        self,
        storage: &mut S,
    ) -> Result<WriteAmplification, AppendError<S::Error>> {
        let seal_bytes = u32::try_from(self.seal.len()).unwrap_or(u32::MAX);
        self.journal.written = self.journal.written.programming(seal_bytes);
        storage
            .program(self.seal_at, self.seal)
            .map_err(AppendError::Storage)?;
        self.journal.written = self.journal.written.barriering();
        storage.barrier().map_err(AppendError::Storage)?;

        // And only now is it history. Everything above this line is recoverable as "nothing
        // happened"; below it, the record is in the committed prefix.
        self.journal.offset = self.journal.offset.saturating_add(self.stride);
        Ok(self.record.programming(seal_bytes).barriering())
    }
}

// A writer is a position and a tally, and nothing that grows with history. Checked where a
// mistake is a compile error, the way `Recovery`'s size is.
const _: () = assert!(size_of::<Journal>() == size_of::<JournalRegion>() + 4 + 16);
const _: () = assert!(size_of::<WriteAmplification>() == 16);

// The seal is one program unit, and a program unit is what a `ProgramAlign` holds. A seal
// wider than the granularity the journal was written at would land in the next record.
const _: () = assert!(frame::seal_bytes(ProgramAlign::BYTE) == 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_amplification_saturates_rather_than_wrapping() {
        let full = WriteAmplification {
            payload_bytes: u32::MAX,
            programmed_bytes: u32::MAX,
            program_operations: u32::MAX,
            barriers: u32::MAX,
        };
        assert_eq!(full.plus(full), full);
        assert_eq!(full.carrying(1), full);
        assert_eq!(full.programming(1), full);
        assert_eq!(full.barriering(), full);
        assert_eq!(full.overhead_bytes(), 0);
        assert_eq!(WriteAmplification::NONE.overhead_bytes(), 0);
    }

    #[test]
    fn a_payload_is_measured_from_the_record_rather_than_from_its_frame() {
        assert_eq!(
            payload_of(&RecordRef::RunStarted {
                workflow_kind: 1,
                workflow_version: 1,
                input: b"hi",
            }),
            6,
            "§09 puts four bytes of workflow identity in a RunStarted payload"
        );
        assert_eq!(
            payload_of(&RecordRef::EffectScheduled {
                seq: waymaker_core::EffectSeq(0),
                kind: waymaker_core::ActivityKind(0),
                input_len: 9,
                input_crc: 0,
            }),
            8,
            "ADR 0011 fixes a schedule record's body at eight bytes"
        );
        assert_eq!(payload_of(&RecordRef::RunFailed { error: b"why" }), 3);
    }
}
