//! §10's capacity reserve: the tail that keeps a run able to end or roll over.
//!
//! Design document §10 "Capacity is explicit" is three sentences, and issue
//! [#25](https://github.com/madmax983/waymaker/issues/25) asks for each of them:
//!
//! > Waymaker reserves enough tail space for a terminal record or `continue_as_new`.
//! > Ordinary effect scheduling fails early with `HistoryNearCapacity`; the runtime never
//! > overwrites committed history to make room.
//!
//! An append-only journal in a fixed bank runs out. What happens then is a design decision
//! rather than an accident, and §02 decision 2 has already made it: history is a committed
//! prefix, so nothing may be evicted to make room. That leaves exactly one way for a full
//! journal to stay useful, which is to never become full by surprise — to stop admitting
//! *ordinary* records while the two ways out are still affordable.
//!
//! # What the reserve is
//!
//! The two exits, priced for the bounds a run declares. [`Bounds`] is those bounds and
//! [`Reserve`] is the price list.
//!
//! * **A terminal record.** `RunCompleted` or `RunFailed`, at
//!   [`Bounds::terminal_bytes`], padded and sealed like any other record.
//! * **An effect outcome.** `EffectCompleted` or `EffectFailed`, at
//!   [`Bounds::effect_result_bytes`]. This one is the reserve's least obvious term and the
//!   one that makes it correct — see below.
//! * **A `continue_as_new` header.** §10 step 3, at [`Bounds::run_input_bytes`]. It is
//!   priced ([`Reserve::swap_bytes`]) and it is *not* part of the tail, because the swap
//!   writes it into the **inactive** bank. What it constrains instead is the configuration:
//!   [`Reserve::for_layout`] refuses bounds under which a bank could not hold the header the
//!   swap would write, or under which the run that swap installs would have no exit of its
//!   own.
//!
//! # Why an outcome is in the tail
//!
//! Because a schedule record creates an obligation. `waymaker_core::ReplayCursor` implements
//! design document §08's transition table, and that table has **no edge from an unresolved
//! effect to a terminal record**: `RunCompleted` and `RunFailed` follow a `Replaying`
//! position, and an outstanding `EffectScheduled` is not one. So a run with an effect
//! outstanding cannot end until the outcome is written, and a reserve that kept room for a
//! terminal record alone would admit a schedule and then strand the run — an effect whose
//! outcome does not fit, and a terminal record §08 refuses to let follow it.
//!
//! That is the tempting arithmetic, and it is wrong. It is driven and watched failing in
//! `crates/waymaker-flash/tests/capacity.rs`, in
//! `a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding`, because a reserve
//! whose necessity is only argued is a reserve somebody will shrink.
//!
//! # What a record still owes
//!
//! [`Reserve::exit_bytes_after`] is that, as a total function of the record's kind — an
//! exhaustive `match`, so a record kind added to §09's table is a compile error here rather
//! than an exit nothing budgeted for:
//!
//! | After | Still owed |
//! | --- | --- |
//! | `EffectScheduled` | an outcome, then a terminal record |
//! | `RunStarted`, `EffectCompleted`, `EffectFailed` | a terminal record |
//! | `RunCompleted`, `RunFailed` | nothing |
//!
//! # Where the gate is
//!
//! [`Reserved`] wraps [`Journal`] and is the only thing in this crate that applies a policy
//! to an append. It **consumes** the writer it gates, for the reason [`Journal::after`]
//! consumes a [`Recovery`](crate::recovery::Recovery): a caller holding both would have an
//! ungated path to the same offset, and the point of a linear discipline is that the second
//! route is a line somebody writes on purpose.
//!
//! The refusal happens before [`Journal::stage`] is called at all, which is what "fails
//! **early**" means on media: no encode, no program, no wear counter moved. §12 says a
//! failed program may still have changed media, so the only refusal that changes nothing is
//! one that never reaches the device.
//!
//! # What this module must not own
//!
//! The swap. §10's seven steps — stop scheduling, erase the inactive bank, write the header,
//! barrier, seal, barrier, lazily erase the old bank — are issue
//! [#26](https://github.com/madmax983/waymaker/issues/26)'s. What is here is the *price* of
//! the header that swap writes, because the price is what the reserve is computed from and
//! a reserve that learned it at the swap would learn it too late.

use core::fmt;

use waymaker_core::{DecodeError, KernelError, RecordRef};

use crate::append::{AppendError, Journal, Staged};
use crate::bank::{BankId, BankLayout, HEADER_OVERHEAD_BYTES};
use crate::frame::{self, ProgramAlign};
use crate::integrity::{Catalogued, IntegrityCheck};
use crate::storage::StableStorage;

/// Bytes of §09 payload a `RunStarted` record spends before its input.
///
/// The workflow kind and version, two bytes each. Written down here rather than derived,
/// for the reason `append`'s `payload_of` writes it down: a payload is a property of the
/// record's kind, and `a_reserve_is_the_worst_case_outcome_and_the_worst_case_terminal_record`
/// pins every figure in this module against [`frame::encoded_len`] of a real record, so a
/// constant that drifted from the codec fails a build rather than shrinking a reserve.
const RUN_STARTED_IDENTITY_BYTES: usize = 4;

/// Bytes of §09 payload an `EffectScheduled` record occupies, in full.
///
/// [ADR 0011](https://github.com/madmax983/waymaker/blob/main/docs/adr/0011-a-scheduled-effect-records-a-length-and-a-digest.md)
/// fixes it at a sequence, a kind, a length and a digest, and the `effect-scheduled-fields`
/// rule fails a build over a fifth field.
const EFFECT_SCHEDULED_BODY_BYTES: usize = 8;

/// `value` as a `u32`, saturating rather than truncating.
///
/// Every length this module narrows is bounded by [`frame::MAX_FRAME_BYTES`] plus a program
/// unit, so the saturation is unreachable. It is here because the alternative is a cast that
/// silently wraps a length into a small number, and a *small* reserve is the one failure
/// this module exists to prevent: `u32::MAX` fits in no journal, so the degenerate answer is
/// "nothing is admitted" rather than "everything is".
#[allow(
    clippy::cast_possible_truncation,
    reason = "the branch is the bound that makes the cast exact"
)]
const fn narrow(value: usize) -> u32 {
    if value < u32::MAX as usize {
        value as u32
    } else {
        u32::MAX
    }
}

/// The worst case a run declares for each thing it may write.
///
/// # Invariants
///
/// These are *ceilings*, not sizes: [`Reserve::admits`] refuses a record longer than the
/// bound for its kind, because a record the reserve never priced is a record that makes the
/// reserve's promise false for whatever comes after it.
///
/// Fields rather than a constructor because there is nothing to validate here on its own —
/// every one of these is legal in isolation and only a *layout* can say whether the three
/// together fit a bank. That is [`Reserve::for_layout`], which is the only way to turn these
/// into a reserve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Bounds {
    /// The longest run input this workflow uses, in a `RunStarted` record and in the bank
    /// header a `continue_as_new` writes.
    pub run_input_bytes: u16,
    /// The longest result or error an effect outcome carries.
    pub effect_result_bytes: u16,
    /// The longest result or error a terminal record carries.
    pub terminal_bytes: u16,
}

/// Why a reserve could not be built, or could not be applied to a journal.
///
/// Distinct from the *admission* answers, which are [`KernelError`]s: these are all
/// configuration, reported where a device is set up rather than where a record is written.
/// A run that meets one of these at its first append would have met it at every append.
///
/// Not `#[non_exhaustive]`, for the reason [`waymaker_core::DecodeError`] is not: every match
/// on it is in this workspace, and an exhaustive match is how the compiler tells whoever adds
/// a variant which call sites now have a case to think about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CapacityError {
    /// The declared run input is longer than a bank header on this layout can carry.
    ///
    /// §10's `continue_as_new` writes the next run's input into the inactive bank's header.
    /// A bound larger than that header can hold is a run that could never roll over, which
    /// is half of what §10 promises — refused where the bounds are declared, because the
    /// swap is much too late to learn it.
    SwapDoesNotFit,
    /// A bank on this layout could not hold both that header and the run behind it.
    ///
    /// The reserve is not only about *this* run. A `continue_as_new` installs another one in
    /// a bank of the same size, and bounds under which that run would have no room for its
    /// own `RunStarted` record and its own exit are bounds that buy one roll-over and then
    /// strand the device.
    ReserveDoesNotFit,
    /// The reserve was computed at a different program granularity than the journal's.
    ///
    /// Every figure in a reserve is padded to a program unit, so a reserve computed at one
    /// byte and applied to an eight-byte part under-states every record it prices. Refused
    /// rather than re-derived: a reserve is built from a [`BankLayout`], and a journal that
    /// disagrees with it belongs to a different device.
    WrongGranularity,
}

impl CapacityError {
    /// A short static description of this failure.
    ///
    /// # Postconditions
    ///
    /// Non-empty, ASCII, distinct from every other variant's, and shorter than a firmware
    /// log line — the same contract [`waymaker_core::KernelError::message`] keeps, and for
    /// the same reason: a device with no debugger attached still has to be able to say which
    /// of three refusals it met.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::SwapDoesNotFit => "a bank header cannot carry the declared run input",
            Self::ReserveDoesNotFit => "a bank cannot hold the declared run and its exit",
            Self::WrongGranularity => "the reserve was priced at another program unit",
        }
    }
}

impl fmt::Display for CapacityError {
    /// Writes [`CapacityError::message`] and nothing else.
    ///
    /// [`fmt::Formatter::write_str`] rather than `write!`: an argument would pull
    /// `core::fmt::write` into an image with a 16 KiB incremental budget.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for CapacityError {}

/// What each of §10's exits costs on this layout, and therefore what the tail must hold.
///
/// # Invariants
///
/// * Every figure is [`frame::encoded_len_for`] of the worst-case payload for its kind, at
///   the layout's program granularity — padded and sealed, which is what a record really
///   occupies.
/// * [`tail_bytes`](Self::tail_bytes) is what an outstanding effect still owes: an outcome
///   and then a terminal record. It is the largest [`exit_bytes_after`](Self::exit_bytes_after)
///   can answer.
/// * A reserve exists only for a layout that can hold it. [`for_layout`](Self::for_layout)
///   refuses the rest, so a `Reserve` in hand is a promise that has already been checked
///   against a bank.
///
/// Pure and stateless. Nothing here depends on what has been written, which is what lets a
/// device compute the same reserve on every boot from the bounds and the geometry alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Reserve {
    align: ProgramAlign,
    /// The worst-case `RunStarted` record, seal included.
    start_bytes: u32,
    /// An `EffectScheduled` record, which has one size. ADR 0011.
    schedule_bytes: u32,
    /// The worst-case `EffectCompleted` or `EffectFailed` record, seal included.
    outcome_bytes: u32,
    /// The worst-case `RunCompleted` or `RunFailed` record, seal included.
    terminal_bytes: u32,
    /// The worst-case `continue_as_new` bank header, padded to the granularity it records.
    swap_bytes: u32,
}

impl Reserve {
    /// Prices §10's exits for `bounds` on `layout`.
    ///
    /// The only constructor. A reserve is meaningless without the device it is a reserve
    /// *on* — the padding, the seal width and the bank's size all come from the layout — and
    /// a constructor taking the figures directly would let a caller supply a set that never
    /// described any device.
    ///
    /// # Postconditions
    ///
    /// On success: [`tail_bytes`](Self::tail_bytes) and
    /// [`swap_bytes`](Self::swap_bytes) both fit inside one bank of `layout`, together, and
    /// with the worst-case `RunStarted` record of the run a `continue_as_new` would install.
    /// So a device that accepts these bounds can end the run it is running *and* roll over
    /// into another one that can do the same — which is the whole of §10's promise, checked
    /// once at configuration rather than hoped for at each append.
    ///
    /// The two banks of a [`BankLayout`] are equal in size by construction, so pricing
    /// against either is pricing against both.
    ///
    /// # Errors
    ///
    /// [`CapacityError::SwapDoesNotFit`] when `bounds.run_input_bytes` is longer than a bank
    /// header on this layout can carry, and [`CapacityError::ReserveDoesNotFit`] when a bank
    /// could not hold that header and the run behind it.
    pub const fn for_layout(bounds: Bounds, layout: BankLayout) -> Result<Self, CapacityError> {
        let align = layout.align();
        // The two banks are equal in size, so either one prices both.
        let region = layout.bank(BankId::A);
        let input = bounds.run_input_bytes as usize;

        // §10 step 3, priced first: a run whose input a header cannot carry can never roll
        // over, whatever else fits.
        if input > region.max_run_input_bytes(align) {
            return Err(CapacityError::SwapDoesNotFit);
        }
        let Some(swap) = align.round_up(HEADER_OVERHEAD_BYTES.saturating_add(input)) else {
            return Err(CapacityError::SwapDoesNotFit);
        };

        let (Ok(start), Ok(schedule), Ok(outcome), Ok(terminal)) = (
            // A `RunStarted` spends four payload bytes on the workflow identity before its
            // input.
            frame::encoded_len_for(RUN_STARTED_IDENTITY_BYTES.saturating_add(input), align),
            frame::encoded_len_for(EFFECT_SCHEDULED_BODY_BYTES, align),
            // §09 puts the effect sequence in the frame *header*, beside the kind and the
            // length, so an outcome's payload is its result or its error and nothing else.
            frame::encoded_len_for(bounds.effect_result_bytes as usize, align),
            frame::encoded_len_for(bounds.terminal_bytes as usize, align),
        ) else {
            // A bound the frame format cannot express is a bound no bank can hold.
            return Err(CapacityError::ReserveDoesNotFit);
        };

        let reserve = Self {
            align,
            start_bytes: narrow(start),
            schedule_bytes: narrow(schedule),
            outcome_bytes: narrow(outcome),
            terminal_bytes: narrow(terminal),
            swap_bytes: narrow(swap),
        };

        // What the run *after* the next swap needs: its own header, its own `RunStarted`
        // record if it writes one, and its own exit. Checked against a bank's payload — the
        // header and journal together, which is everything the generation seal does not
        // occupy — because that is what a swap has to fit a whole run into.
        let opening = reserve.start_bytes.saturating_add(reserve.terminal_bytes);
        let owed = if opening > reserve.tail_bytes() {
            opening
        } else {
            reserve.tail_bytes()
        };
        let Some(needed) = reserve.swap_bytes.checked_add(owed) else {
            return Err(CapacityError::ReserveDoesNotFit);
        };
        if needed > region.payload_bytes() {
            return Err(CapacityError::ReserveDoesNotFit);
        }

        Ok(reserve)
    }

    /// Bytes of journal tail this reserve keeps free while an ordinary record is admitted.
    ///
    /// The worst-case outcome and the worst-case terminal record, which is what a run with
    /// an effect outstanding still owes. See the module documentation for why the outcome is
    /// in it.
    #[must_use]
    pub const fn tail_bytes(&self) -> u32 {
        self.outcome_bytes.saturating_add(self.terminal_bytes)
    }

    /// Bytes a `continue_as_new` costs the bank it rolls into.
    ///
    /// §10 step 3's header, padded to the granularity it records. Not part of
    /// [`tail_bytes`](Self::tail_bytes): the swap writes into the *inactive* bank, so a
    /// journal at its reserve boundary has already paid for it — what this figure buys is
    /// [`for_layout`](Self::for_layout)'s refusal of bounds a bank could not roll over into.
    #[must_use]
    pub const fn swap_bytes(&self) -> u32 {
        self.swap_bytes
    }

    /// Bytes that must still be free once `record` is committed, for the run to be able to
    /// end.
    ///
    /// A total function of the record's kind, by exhaustive `match`: see the table in the
    /// module documentation.
    #[must_use]
    pub const fn exit_bytes_after(&self, record: &RecordRef<'_>) -> u32 {
        match record {
            // §08 has no edge from an unresolved effect to a terminal record, so a schedule
            // owes the outcome that resolves it *and* the terminal record after that.
            RecordRef::EffectScheduled { .. } => self.tail_bytes(),
            RecordRef::RunStarted { .. }
            | RecordRef::EffectCompleted { .. }
            | RecordRef::EffectFailed { .. } => self.terminal_bytes,
            // A terminal record is the exit. Nothing may follow it.
            RecordRef::RunCompleted { .. } | RecordRef::RunFailed { .. } => 0,
        }
    }

    /// Whether `record` may be appended to a journal with `room` bytes left in it.
    ///
    /// §10's whole decision, as a pure predicate over a length: no media, no state, and the
    /// same answer on every boot.
    ///
    /// # Postconditions
    ///
    /// [`Ok`] implies both that the record fits *and* that what it still owes fits after it,
    /// so a record this admits is never one [`Journal::stage`] refuses with
    /// [`AppendError::NoRoom`]. `crates/waymaker-flash/tests/capacity.rs` sweeps that over
    /// every room a small journal can have.
    ///
    /// # Errors
    ///
    /// [`KernelError::HistoryNearCapacity`] when the record and what it owes do not both fit
    /// — §10's "ordinary effect scheduling fails early", which for a terminal record is
    /// instead the honest report that not even the exit fits any more.
    ///
    /// [`KernelError::Decode`] carrying [`DecodeError::LengthOutOfBounds`] when the record is
    /// longer than the bound its kind was priced at, or longer than the frame format can
    /// express. That is a different failure and says so: the reserve is a promise about
    /// records of a declared size, and admitting a larger one would make the promise false
    /// for everything after it rather than for this record.
    pub fn admits(&self, record: &RecordRef<'_>, room: u32) -> Result<(), KernelError> {
        let Ok(encoded) = frame::encoded_len(record, self.align) else {
            return Err(KernelError::Decode(DecodeError::LengthOutOfBounds));
        };
        let needed = narrow(encoded);
        if needed > self.ceiling_for(record) {
            return Err(KernelError::Decode(DecodeError::LengthOutOfBounds));
        }
        let Some(total) = needed.checked_add(self.exit_bytes_after(record)) else {
            return Err(KernelError::HistoryNearCapacity);
        };
        if total > room {
            return Err(KernelError::HistoryNearCapacity);
        }
        Ok(())
    }

    /// The largest `record` of its kind this reserve priced.
    const fn ceiling_for(&self, record: &RecordRef<'_>) -> u32 {
        match record {
            RecordRef::RunStarted { .. } => self.start_bytes,
            RecordRef::EffectScheduled { .. } => self.schedule_bytes,
            RecordRef::EffectCompleted { .. } | RecordRef::EffectFailed { .. } => {
                self.outcome_bytes
            }
            RecordRef::RunCompleted { .. } | RecordRef::RunFailed { .. } => self.terminal_bytes,
        }
    }

    /// The granularity every figure here was padded to.
    pub(crate) const fn align(&self) -> ProgramAlign {
        self.align
    }
}

/// Why a reserved append did not happen.
///
/// Two failures with two different causes, and a caller acts on them differently: a capacity
/// refusal is the run being told to end or roll over, and an append failure is the media or
/// the caller's own arguments. Flattening them would leave a dispatcher deciding whether to
/// call `continue_as_new` on the strength of a string.
///
/// Generic over the driver's error and deliberately without [`fmt::Display`], for the reasons
/// [`AppendError`] is both: §12 lets every port name its own error, and a `Display` bound
/// would spread to every signature this type appears in.
///
/// Not `#[non_exhaustive]`, for the reason [`CapacityError`] is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReservedError<E> {
    /// §10 refused the record. Always before the device was asked for anything.
    ///
    /// [`KernelError::HistoryNearCapacity`] is the refusal §10 names, and it is the answer a
    /// caller acts on: stop scheduling, and either end the run or `continue_as_new`. The
    /// other value this can carry is [`KernelError::Decode`], which is a record longer than
    /// the bounds the reserve was built from — a configuration mistake rather than a full
    /// journal, and one that will not be fixed by ending the run.
    Capacity(KernelError),
    /// The append itself failed. See [`AppendError`] for what is on media afterwards.
    Append(AppendError<E>),
}

/// A [`Journal`] that applies §10's reserve before it programs anything.
///
/// # Invariants
///
/// * The reserve and the journal describe one device: [`over`](Self::over) refuses a reserve
///   priced at another program granularity, and a journal is only ever positioned by a
///   recovery of a real region.
/// * Nothing reaches media before the reserve has answered. [`stage`](Self::stage) calls
///   [`Reserve::admits`] first, and a refusal returns before [`Journal::stage`] encodes,
///   bounds-checks or programs anything — so §25's "the failure produces no mutation at all"
///   is a property of the call order rather than of an undo.
/// * The gated writer is the only one: [`over`](Self::over) takes the [`Journal`] by value.
///
/// # Why it is not `Copy` or `Clone`
///
/// For the reason [`Journal`] is neither. A copied writer is a second appender at one
/// offset, and a copied *reserve*-carrying writer is a second appender that also believes
/// the tail is still free.
#[derive(Debug, PartialEq, Eq)]
pub struct Reserved<C: IntegrityCheck = Catalogued> {
    journal: Journal<C>,
    reserve: Reserve,
}

impl<C: IntegrityCheck> Reserved<C> {
    /// Gates `journal` with `reserve`.
    ///
    /// The only constructor, and it consumes the writer: see the module documentation for
    /// why an ungated writer beside a gated one is a line somebody has to write on purpose.
    ///
    /// # Errors
    ///
    /// [`CapacityError::WrongGranularity`] when the reserve was priced at a different program
    /// unit than the journal's region was written at, and
    /// [`CapacityError::ReserveDoesNotFit`] when the region is smaller than the tail the
    /// reserve keeps — a journal in which nothing could ever be admitted.
    pub const fn over(journal: Journal<C>, reserve: Reserve) -> Result<Self, CapacityError> {
        let region = journal.region();
        if region.align().get() != reserve.align().get() {
            return Err(CapacityError::WrongGranularity);
        }
        if reserve.tail_bytes() > region.bytes() {
            return Err(CapacityError::ReserveDoesNotFit);
        }
        Ok(Self { journal, reserve })
    }

    /// The writer underneath, for its offset, its room and its write amplification.
    ///
    /// A shared borrow: [`Journal::stage`] needs a unique one, so this cannot be used to
    /// append around the reserve.
    #[must_use]
    pub const fn journal(&self) -> &Journal<C> {
        &self.journal
    }

    /// [`Journal::stage`], once §10's reserve has admitted the record.
    ///
    /// # Postconditions
    ///
    /// On [`ReservedError::Capacity`] the device was not called: not read, not programmed,
    /// not barriered, and the journal's offset and
    /// [`amplification`](Journal::amplification) are exactly what they were. On every other
    /// answer this is [`Journal::stage`], whose postconditions are unchanged — the reserve is
    /// a gate rather than a second writer, and §07's two barriers still follow.
    ///
    /// # Errors
    ///
    /// [`ReservedError::Capacity`] for §10's refusals — see [`Reserve::admits`] — and
    /// [`ReservedError::Append`] for everything [`Journal::stage`] can refuse.
    pub fn stage<'journal, 'page, S: StableStorage>(
        &'journal mut self,
        storage: &mut S,
        record: &RecordRef<'_>,
        page: &'page mut [u8],
    ) -> Result<Staged<'journal, 'page, C>, ReservedError<S::Error>> {
        // First, and before anything that could touch media. §12: a failed program may still
        // have changed media, so the only refusal that changes nothing is one taken before
        // the device is called at all.
        self.reserve
            .admits(record, self.journal.room())
            .map_err(ReservedError::Capacity)?;
        self.journal
            .stage(storage, record, page)
            .map_err(ReservedError::Append)
    }
}

// A reserve is a price list and nothing that grows with history, and a gated writer is a
// writer plus that list. Checked where a mistake is a compile error, the way `Journal`'s
// size is.
const _: () = assert!(size_of::<Reserve>() == 24);
const _: () = assert!(size_of::<Reserved>() == size_of::<Journal>() + size_of::<Reserve>());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Geometry;

    fn layout() -> BankLayout {
        let Ok(geometry) = Geometry::new(8192, 4096, 8, 1) else {
            unreachable!("8192 is two whole 4096-byte blocks of whole 8-byte units")
        };
        let Ok(layout) = BankLayout::new(geometry) else {
            unreachable!("two erase blocks are two banks")
        };
        layout
    }

    const BOUNDS: Bounds = Bounds {
        run_input_bytes: 32,
        effect_result_bytes: 16,
        terminal_bytes: 16,
    };

    #[test]
    fn narrowing_saturates_rather_than_wrapping() {
        // The one direction a reserve must not fail in: a wrapped length is a small reserve,
        // and a small reserve is a full journal nobody saw coming.
        assert_eq!(narrow(0), 0);
        assert_eq!(narrow(64), 64);
        assert_eq!(narrow(usize::MAX), u32::MAX);
    }

    #[test]
    fn every_refusal_has_a_message_of_its_own() {
        let messages = [
            CapacityError::SwapDoesNotFit.message(),
            CapacityError::ReserveDoesNotFit.message(),
            CapacityError::WrongGranularity.message(),
        ];
        for (left, one) in messages.iter().enumerate() {
            assert!(one.is_ascii() && !one.is_empty());
            for (right, other) in messages.iter().enumerate() {
                assert_eq!(left == right, one == other, "two refusals share a message");
            }
        }
    }

    #[test]
    fn a_reserve_is_the_same_reserve_on_every_boot() {
        // Pure: nothing here reads media or a clock, so a device that recomputes the reserve
        // after a reset gets the number it refused a record with before it.
        let Ok(first) = Reserve::for_layout(BOUNDS, layout()) else {
            unreachable!("these bounds fit a 4 KiB bank")
        };
        let Ok(second) = Reserve::for_layout(BOUNDS, layout()) else {
            unreachable!("these bounds fit a 4 KiB bank")
        };
        assert_eq!(first, second);
    }

    #[test]
    fn the_tail_is_the_largest_thing_a_record_can_owe() {
        let Ok(reserve) = Reserve::for_layout(BOUNDS, layout()) else {
            unreachable!("these bounds fit a 4 KiB bank")
        };
        for record in [
            RecordRef::RunStarted {
                workflow_kind: 1,
                workflow_version: 1,
                input: b"in",
            },
            RecordRef::EffectScheduled {
                seq: waymaker_core::EffectSeq(0),
                kind: waymaker_core::ActivityKind(0),
                input_len: 0,
                input_crc: 0,
            },
            RecordRef::EffectCompleted {
                seq: waymaker_core::EffectSeq(0),
                result: b"",
            },
            RecordRef::EffectFailed {
                seq: waymaker_core::EffectSeq(0),
                error: b"",
            },
            RecordRef::RunCompleted { result: b"" },
            RecordRef::RunFailed { error: b"" },
        ] {
            assert!(reserve.exit_bytes_after(&record) <= reserve.tail_bytes());
        }
    }

    #[test]
    fn a_zero_length_bound_is_still_a_whole_frame() {
        // A run that returns nothing still writes a record, and a reserve that priced it at
        // zero would admit a schedule with no room for the terminal record that follows it.
        let Ok(reserve) = Reserve::for_layout(
            Bounds {
                run_input_bytes: 0,
                effect_result_bytes: 0,
                terminal_bytes: 0,
            },
            layout(),
        ) else {
            unreachable!("empty bounds fit any bank")
        };
        assert!(reserve.tail_bytes() >= 2 * narrow(frame::FRAME_OVERHEAD_BYTES));
        assert!(reserve.swap_bytes() >= narrow(HEADER_OVERHEAD_BYTES));
    }
}
