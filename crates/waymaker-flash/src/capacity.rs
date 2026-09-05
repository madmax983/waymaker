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
//!   priced ([`Reserve::rollover_bytes`]) and it is *not* part of the tail, because the swap
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
use crate::bank::{self, BankId, BankLayout};
use crate::frame::{self, EFFECT_SCHEDULED_BODY_BYTES, ProgramAlign, RUN_STARTED_PREFIX_BYTES};
use crate::integrity::{Catalogued, IntegrityCheck};
use crate::storage::StableStorage;

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
    ///
    /// From [`Reserve::for_layout`] only.
    SwapDoesNotFit,
    /// A bound is longer than the record format can describe at all.
    ///
    /// Distinct from [`ReserveDoesNotFit`](Self::ReserveDoesNotFit), and the distinction is
    /// what an operator acts on: this one is not a bank that is too small, so no bank will
    /// fix it. §09's `payload_len` is a `u16`, and a `RunStarted` spends four of those bytes
    /// on the workflow identity, so a run input a *header* could carry is not always one a
    /// record can.
    ///
    /// From [`Reserve::for_layout`] only.
    BoundUnencodable,
    /// A bank on this layout could not hold that header and a usable run behind it.
    ///
    /// The reserve is not only about *this* run. A `continue_as_new` installs another one in
    /// a bank of the same size, and bounds under which that run could not write its opening
    /// record, schedule one effect, resolve it and end are bounds that buy one roll-over and
    /// then strand the device.
    ///
    /// From [`Reserve::for_layout`] only.
    ReserveDoesNotFit,
    /// The journal handed to [`Reserved::over`] is smaller than the reserve was priced for.
    ///
    /// Distinct from [`ReserveDoesNotFit`](Self::ReserveDoesNotFit): the bounds and the
    /// layout may be perfectly compatible and *this* journal still be the wrong one — a
    /// region from a smaller device, or a bank whose header carried an input longer than the
    /// bounds declared, which shrinks the journal behind it. Without this,
    /// [`Reserve::for_layout`]'s postcondition would not survive [`Reserved::over`], and the
    /// first record of the run would be refused for ever.
    ///
    /// From [`Reserved::over`] only.
    RegionTooSmall,
    /// The reserve was computed at a different program granularity than the journal's.
    ///
    /// Every figure in a reserve is padded to a program unit, so a reserve computed at one
    /// byte and applied to an eight-byte part under-states every record it prices. Refused
    /// rather than re-derived: a reserve is built from a [`BankLayout`], and a journal that
    /// disagrees with it belongs to a different device.
    ///
    /// From [`Reserved::over`] only.
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
            Self::SwapDoesNotFit => "a bank header cannot carry the run input",
            Self::BoundUnencodable => "a bound is longer than a record may be",
            Self::ReserveDoesNotFit => "a bank cannot hold the run and its exit",
            Self::RegionTooSmall => "this journal is smaller than the reserve",
            Self::WrongGranularity => "the reserve was priced at another unit",
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

/// Why §10's reserve refused a record.
///
/// Three answers rather than one, because a caller acts on them differently and a firmware
/// log line that could not tell them apart would send an engineer to the wrong place — the
/// argument [`waymaker_core::KernelError::MalformedHistory`] makes for itself.
/// [`kernel_error`](Self::kernel_error) is the kernel's coarser word for each, for a caller
/// that speaks only that vocabulary.
///
/// Not `#[non_exhaustive]`, for the reason [`CapacityError`] is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// §10: the record and what it still owes do not both fit.
    ///
    /// The refusal §10 names, and the one a caller acts on: stop scheduling, and either end
    /// the run or `continue_as_new`. Both are still affordable when this is the answer to an
    /// *ordinary* record — which is what the reserve was kept for.
    ///
    /// It is also the answer when even a terminal record does not fit, which is reachable
    /// only on a journal this gate did not write to its boundary. `HistoryNearCapacity` is
    /// still the honest word there: the variant's own text is "only a terminal record **or
    /// `continue_as_new`** still fits", and `continue_as_new` writes into the other bank, so
    /// the exit it names is the one that remains.
    NearCapacity,
    /// The record is longer than the bound its kind was priced at.
    ///
    /// Not a full journal, and ending the run will not fix it: the reserve is a promise about
    /// records of a declared size, and admitting a larger one would make the promise false
    /// for everything after it. The remedy is a shorter record or a re-declared [`Bounds`].
    ///
    /// One state deserves naming, because it is the one place §10 leaves no exit at all: an
    /// *outcome* refused here while its effect is outstanding cannot be followed by a
    /// terminal record either, since §08 has no edge from an unresolved effect to one. A
    /// caller that meets it must shorten the payload; the run cannot be ended around it.
    OverDeclaredBound,
    /// The record cannot be encoded at this granularity at all.
    ///
    /// §09's `payload_len` is a `u16`, and padding it to a program unit must not overflow.
    /// Distinct from [`OverDeclaredBound`](Self::OverDeclaredBound) because no [`Bounds`]
    /// would have admitted it.
    Unencodable,
}

impl Refusal {
    /// The kernel's word for this refusal.
    ///
    /// §10 names [`KernelError::HistoryNearCapacity`] and this is where that name is kept.
    /// The two length refusals map to [`KernelError::Decode`] carrying
    /// [`DecodeError::LengthOutOfBounds`], which is that error's documented second meaning —
    /// "a length field points past the buffer *or the caller-owned output*" — read as the
    /// caller-owned output being the room a run declared for itself.
    #[must_use]
    pub const fn kernel_error(self) -> KernelError {
        match self {
            Self::NearCapacity => KernelError::HistoryNearCapacity,
            Self::OverDeclaredBound | Self::Unencodable => {
                KernelError::Decode(DecodeError::LengthOutOfBounds)
            }
        }
    }
}

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
    rollover_bytes: u32,
    /// The smallest journal a run priced by these bounds can be *used* in.
    ///
    /// Its opening record, one effect scheduled and resolved, and its exit. See
    /// [`for_layout`](Reserve::for_layout) for why a floor that stopped at "can end" is a
    /// bank that can start a run and never do any work in it.
    floor_bytes: u32,
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
    /// On success a bank of `layout` can hold the worst-case `continue_as_new` header
    /// ([`rollover_bytes`](Self::rollover_bytes)) *and*, behind it, a journal in which a run
    /// priced by these bounds can write its opening record, schedule one effect, resolve it
    /// and end. So a device that accepts these bounds can finish the run it is running and
    /// roll over into another one that can do the same — §10's whole promise, checked once
    /// at configuration rather than hoped for at each append.
    ///
    /// The floor is "can be *used*", not "can end", and the difference is not pedantic: a
    /// bank sized to a `RunStarted` and a terminal record accepts bounds under which the very
    /// first `EffectScheduled` is refused for ever, which is a run that can start and finish
    /// and never do any work. Review of issue #25 found 4004 such configurations under the
    /// weaker floor.
    ///
    /// The two banks of a [`BankLayout`] are equal in size by construction, so pricing
    /// against either is pricing against both.
    ///
    /// # Errors
    ///
    /// [`CapacityError::SwapDoesNotFit`] when `bounds.run_input_bytes` is longer than a bank
    /// header on this layout can carry; [`CapacityError::BoundUnencodable`] when a bound is
    /// longer than a record may be, whatever the bank; and
    /// [`CapacityError::ReserveDoesNotFit`] when a bank could not hold that header and a
    /// usable run behind it.
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
        let Some(rollover) = bank::header_len_for(input, align) else {
            return Err(CapacityError::SwapDoesNotFit);
        };

        let (Ok(start), Ok(schedule), Ok(outcome), Ok(terminal)) = (
            // A `RunStarted` spends four payload bytes on the workflow identity before its
            // input, so a run input a header can carry is not always one a record can.
            frame::encoded_len_for(RUN_STARTED_PREFIX_BYTES.saturating_add(input), align),
            frame::encoded_len_for(EFFECT_SCHEDULED_BODY_BYTES, align),
            // §09 puts the effect sequence in the frame *header*, beside the kind and the
            // length, so an outcome's payload is its result or its error and nothing else.
            frame::encoded_len_for(bounds.effect_result_bytes as usize, align),
            frame::encoded_len_for(bounds.terminal_bytes as usize, align),
        ) else {
            return Err(CapacityError::BoundUnencodable);
        };

        let tail = narrow(outcome).saturating_add(narrow(terminal));
        // The opening record, one effect scheduled and resolved, and the exit. Every other
        // combination is smaller: an effect's outcome and the terminal record after it *are*
        // the tail, so `start + schedule + tail` dominates both `start + terminal` and the
        // tail alone.
        let floor = narrow(start)
            .saturating_add(narrow(schedule))
            .saturating_add(tail);
        let Some(needed) = narrow(rollover).checked_add(floor) else {
            return Err(CapacityError::ReserveDoesNotFit);
        };
        // Against a bank's *payload* — the header and journal together, everything the
        // generation seal does not occupy — because that is what a swap has to fit a whole
        // run into.
        if needed > region.payload_bytes() {
            return Err(CapacityError::ReserveDoesNotFit);
        }

        Ok(Self {
            align,
            start_bytes: narrow(start),
            schedule_bytes: narrow(schedule),
            outcome_bytes: narrow(outcome),
            terminal_bytes: narrow(terminal),
            rollover_bytes: narrow(rollover),
            floor_bytes: floor,
        })
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
    ///
    /// Named for the roll-over rather than for the swap because `u32` has an inherent
    /// `swap_bytes`, and a `reserve.swap_bytes()` read in an arithmetic expression has a
    /// genuine reason to be misread as byte-order reversal.
    #[must_use]
    pub const fn rollover_bytes(&self) -> u32 {
        self.rollover_bytes
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
    /// [`AppendError::NoRoom`]. It is exact rather than merely sufficient: a record whose
    /// width plus [`exit_bytes_after`](Self::exit_bytes_after) is exactly `room` is admitted.
    /// `crates/waymaker-flash/tests/capacity.rs` sweeps both directions.
    ///
    /// What it says nothing about is the caller's staging page. On a part whose program unit
    /// is larger than §04's 512 B scratch page, a record this admits can still be refused by
    /// [`Journal::stage`] with [`AppendError::Encode`], because the page cannot hold the
    /// padded frame. That is a fact about the caller's buffer rather than about the journal,
    /// and the reserve deliberately does not model it.
    ///
    /// # Errors
    ///
    /// See [`Refusal`], whose three variants are the three different things a caller does
    /// next.
    pub fn admits(&self, record: &RecordRef<'_>, room: u32) -> Result<(), Refusal> {
        let Ok(encoded) = frame::encoded_len(record, self.align) else {
            return Err(Refusal::Unencodable);
        };
        let needed = narrow(encoded);
        if needed > self.ceiling_for(record) {
            return Err(Refusal::OverDeclaredBound);
        }
        let Some(total) = needed.checked_add(self.exit_bytes_after(record)) else {
            return Err(Refusal::NearCapacity);
        };
        if total > room {
            return Err(Refusal::NearCapacity);
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
    /// See [`Refusal`] for the three answers and what a caller does next with each;
    /// [`Refusal::kernel_error`] is the kernel's coarser word for them.
    Capacity(Refusal),
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
/// # What the caller still owes
///
/// **One outstanding effect, in §08's order.** [`Reserve::exit_bytes_after`] prices exactly
/// one unresolved effect, and releases the whole tail once a terminal record is committed.
/// Both are right for history `waymaker_core::ReplayCursor` would accept — it refuses a
/// schedule while one is unresolved, and refuses anything after a terminal record — but this
/// type holds no cursor and enforces neither. A caller that commits two schedules in a row,
/// or a terminal record while an effect is outstanding, is admitted here and produces history
/// the cursor halts on for ever. §08's order is the precondition the whole tail rests on, and
/// the thing that will discharge it is the dispatcher of rung 0.4, which holds both.
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
    /// [`CapacityError::RegionTooSmall`] when the journal is smaller than the one
    /// [`Reserve::for_layout`] priced.
    ///
    /// The second is what makes a `Reserved` mean what [`Reserve::for_layout`] promised.
    /// A reserve is priced against a bank's *worst-case* journal — the one left behind a
    /// header at the declared bound — and nothing about a `Reserve` value ties it to that
    /// bank afterwards. Review of issue #25 built one on a 4 KiB layout, handed it an
    /// 80-byte journal from a 256 B device of the same granularity, and watched every
    /// `RunStarted` be refused for ever with the journal empty. Checking the whole floor
    /// rather than the tail alone is what closes it: the tail is what an *outstanding
    /// effect* owes, and a journal that can hold only that cannot hold the run that reaches
    /// it.
    pub const fn over(journal: Journal<C>, reserve: Reserve) -> Result<Self, CapacityError> {
        let region = journal.region();
        if region.align().get() != reserve.align().get() {
            return Err(CapacityError::WrongGranularity);
        }
        if reserve.floor_bytes > region.bytes() {
            return Err(CapacityError::RegionTooSmall);
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

    /// The reserve this writer applies.
    ///
    /// `Reserve` is `Copy` and answers [`Reserve::admits`] over a length, so a dispatcher can
    /// ask "may I still schedule?" — §10 step 1 — before it has built a record and without
    /// keeping its own copy beside the writer. A caller that had to recompute
    /// [`Reserve::for_layout`] at every decision point would be doing fallible arithmetic to
    /// answer an infallible question.
    #[must_use]
    pub const fn reserve(&self) -> Reserve {
        self.reserve
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
const _: () = assert!(size_of::<Reserve>() == 28);
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
        effect_result_bytes: 32,
        terminal_bytes: 16,
    };

    fn reserve() -> Reserve {
        let Ok(reserve) = Reserve::for_layout(BOUNDS, layout()) else {
            unreachable!("these bounds fit a 4 KiB bank")
        };
        reserve
    }

    #[test]
    fn narrowing_a_value_past_u32_saturates_rather_than_truncating() {
        // The one direction a reserve must not fail in: a wrapped length is a *small*
        // reserve, and a small reserve is a full journal nobody saw coming. `usize::MAX`
        // alone cannot show it — on a 64-bit host `usize::MAX as u32` is `u32::MAX` too, so
        // truncation and saturation agree at exactly that value. The first value past the
        // ceiling is where they part: truncation gives zero.
        assert_eq!(narrow(0), 0);
        assert_eq!(narrow(64), 64);
        assert_eq!(narrow(usize::MAX), u32::MAX);
        if let Some(beyond) = (u32::MAX as usize).checked_add(1) {
            assert_eq!(narrow(beyond), u32::MAX);
        }
    }

    #[test]
    fn every_refusal_has_a_message_of_its_own() {
        let messages = [
            CapacityError::SwapDoesNotFit.message(),
            CapacityError::BoundUnencodable.message(),
            CapacityError::ReserveDoesNotFit.message(),
            CapacityError::RegionTooSmall.message(),
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
    fn a_refusal_carries_the_kernels_word_for_itself() {
        // §10 names `HistoryNearCapacity` and this is where that name is kept. The two
        // length refusals are a different failure and say so, which is the whole reason
        // `Refusal` has three variants rather than one.
        assert_eq!(
            Refusal::NearCapacity.kernel_error(),
            KernelError::HistoryNearCapacity
        );
        assert_eq!(
            Refusal::OverDeclaredBound.kernel_error(),
            KernelError::Decode(DecodeError::LengthOutOfBounds)
        );
        assert_eq!(
            Refusal::Unencodable.kernel_error(),
            KernelError::Decode(DecodeError::LengthOutOfBounds)
        );
    }

    #[test]
    fn what_a_record_owes_is_exactly_one_of_three_answers() {
        // An equality table rather than a `<= tail_bytes()` bound: the bound is satisfied by
        // an `exit_bytes_after` that answers zero for everything, which is precisely the
        // mutation that destroys the module's central claim.
        let reserve = reserve();
        let owes = |record: &RecordRef<'_>| reserve.exit_bytes_after(record);
        assert_eq!(
            owes(&RecordRef::EffectScheduled {
                seq: waymaker_core::EffectSeq(0),
                kind: waymaker_core::ActivityKind(0),
                input_len: 0,
                input_crc: 0,
            }),
            reserve.tail_bytes()
        );
        assert_eq!(
            owes(&RecordRef::RunStarted {
                workflow_kind: 1,
                workflow_version: 1,
                input: b"in",
            }),
            reserve.terminal_bytes
        );
        assert_eq!(
            owes(&RecordRef::EffectCompleted {
                seq: waymaker_core::EffectSeq(0),
                result: b"",
            }),
            reserve.terminal_bytes
        );
        assert_eq!(
            owes(&RecordRef::EffectFailed {
                seq: waymaker_core::EffectSeq(0),
                error: b"",
            }),
            reserve.terminal_bytes
        );
        assert_eq!(owes(&RecordRef::RunCompleted { result: b"" }), 0);
        assert_eq!(owes(&RecordRef::RunFailed { error: b"" }), 0);
        assert!(reserve.tail_bytes() > reserve.terminal_bytes);
    }

    #[test]
    fn a_zero_length_bound_is_still_a_whole_frame() {
        // A run that returns nothing still writes a record, and a reserve that priced it at
        // zero would admit a schedule with no room for the terminal record that follows it.
        let empty = Bounds {
            run_input_bytes: 0,
            effect_result_bytes: 0,
            terminal_bytes: 0,
        };
        let Ok(reserve) = Reserve::for_layout(empty, layout()) else {
            unreachable!("empty bounds fit any bank")
        };
        let Ok(smallest) = frame::encoded_len_for(0, layout().align()) else {
            unreachable!("an empty payload encodes")
        };
        assert_eq!(reserve.tail_bytes(), narrow(smallest).saturating_mul(2));
        let Some(header) = bank::header_len_for(0, layout().align()) else {
            unreachable!("an empty header pads")
        };
        assert_eq!(reserve.rollover_bytes(), narrow(header));
    }

    #[test]
    fn the_floor_is_an_opening_record_one_effect_and_an_exit() {
        // The number `Reserved::over` re-checks. Stated as the sum it is, so a floor that
        // stopped counting the schedule — the term that separates a usable bank from one
        // that can only start and end a run — is a failure here rather than 4004 silently
        // accepted configurations.
        let reserve = reserve();
        assert_eq!(
            reserve.floor_bytes,
            reserve.start_bytes + reserve.schedule_bytes + reserve.tail_bytes()
        );
        assert!(reserve.floor_bytes > reserve.tail_bytes());
    }
}
