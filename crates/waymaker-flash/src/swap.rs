//! `continue_as_new`: the seven-step bank swap, as a typestate.
//!
//! Design document §10 Two-bank lifecycle, and issue
//! [#26](https://github.com/madmax983/waymaker/issues/26). A normal async workflow cannot
//! serialise its hidden suspension state — §02 decision 6 — so Waymaker does not disguise
//! storage maintenance as a snapshot. The workflow supplies the bounded input for its next
//! run, and this module installs that run in the other bank:
//!
//! 1. **Stop accepting new effects for the current run.** [`Swap::beginning`] consumes the
//!    [`Retired`] reader or writer the run was using, so there is no value left to append
//!    with. That is the step being a type rather than a comment.
//! 2. **Erase the inactive bank**, and barrier. [`Swap::prepare`].
//! 3. **Write the new bank header** — the new [`RunId`], the workflow version and the next
//!    run's input. [`Prepared::stage`].
//! 4. **Barrier**: the new bank's payload becomes durable. [`Staged::payload_barrier`].
//! 5. **Write the higher-generation seal**, and
//! 6. **barrier**: the new bank becomes authoritative. [`Sealable::commit`].
//! 7. **Lazily erase the old bank.** [`Installed::reclaim`].
//!
//! # Why this is types and not a function
//!
//! For the reason [`crate::append`] is. §10's recovery rules are statements about *where*
//! the barriers are — "a crash before step 5 recovers the old run, a crash after step 6
//! recovers the new run" — and a `continue_as_new()` that did all seven steps in one body
//! would make that ordering a convention which the next patch is free to re-order. Nothing
//! about such a mistake is visible until a power loss on somebody's device.
//!
//! So the states are separate types and each has exactly one thing to do. [`Prepared`] has
//! no `commit`, [`Staged`] has no `program`, and [`Sealable`] is reachable only from
//! [`Staged::payload_barrier`]:
//!
//! ```
//! # use waymaker_flash::swap::Sealable;
//! # use waymaker_flash::storage::StableStorage;
//! fn commit_after_the_barrier<S: StableStorage>(sealable: Sealable<'_>, storage: &mut S) {
//!     let _ = sealable.commit(storage);
//! }
//! ```
//!
//! ```compile_fail,E0599
//! # use waymaker_flash::swap::Staged;
//! # use waymaker_flash::storage::StableStorage;
//! fn commit_without_the_barrier<S: StableStorage>(staged: Staged<'_>, storage: &mut S) {
//!     let _ = staged.commit(storage);
//! }
//! ```
//!
//! The two differ in one word, and the compiling twin is what stops the second from passing
//! because [`Staged`] was deleted rather than because it has no `commit`.
//!
//! # The erase, and the barrier after it
//!
//! Step 2 ends with a barrier, and it is not caution. §12 orders only what a *completed*
//! barrier ordered, so without one the header of step 3 may become durable before the erase
//! that precedes it — and a bank whose erase lands after its header is a bank with no header
//! and a seal that names one. The whole point of §10's crash windows is that no such state
//! is reachable.
//!
//! # A seal is never programmed over a header that did not land
//!
//! `waymaker-fault`'s `swap_that_seals_whatever_landed` is a writer whose device can carry a
//! torn header under a perfectly valid highest-generation seal, and it has two bugs rather
//! than one: it seals the header it *intended* to write, and it carries on past a failed
//! program. What separates [`Prepared::stage`] from it is the second — the `?` on the
//! program call — and it is worth being exact about that, because the first would not have
//! helped. The seal is computed from the buffer the header was encoded into, before the
//! program, so sealing the [`BankHeader`] argument directly would produce the same digest
//! byte for byte. Reading the header back after programming it would be a third answer, and
//! it is not taken: a successful `program` is §12 saying those bytes are on media, and a
//! read-back would cost a second pass over the caller's page to learn what the contract
//! already says.
//!
//! What the digest *does* buy is that a seal names one header. A frame torn by a power loss
//! part-way through the program cannot match the seal computed for the whole one, so a
//! half-written bank is not a candidate at any generation — which is [`crate::bank`]'s
//! guarantee, reached here rather than re-argued.
//!
//! # What the types cannot make impossible
//!
//! Two things, stated because the rest of this module argues that step order is a fact about
//! types rather than a convention, and a reader is owed the edges of that claim.
//!
//! `booted` is a [`bank::select`] answer the caller supplies, and nothing here reads media to
//! confirm it. A caller that supplies a *stale* one — naming a bank that lost a swap since —
//! makes [`Swap::prepare`] erase the bank that is actually authoritative, and every check in
//! this module passes, because the retired reader really is over the bank `booted` named.
//! The refusal that would close it is a read of the spare bank's seal, and it cannot be made
//! fail-closed: the header it would have to decode is as long as the previous run's input,
//! which is bounded by the bank rather than by the caller's page, so a small page would turn
//! it into a guard that silently allows what it exists to refuse. It is stated as a
//! precondition on [`Swap::beginning`] instead, and closing it by construction is the
//! dispatcher's — rung 0.4's — because a dispatcher that selects and swaps in one place
//! cannot hold a `booted` older than the swap it is planning.
//!
//! And a [`Sealable`] is linear within one chain but not across two. A caller that reaches
//! [`Sealable`], leaves it, runs a second swap to completion with a second page, and *then*
//! commits the first would program a stale seal over a live one — on NOR the two `AND`
//! together into a seal that decodes as neither, and if the second swap also reclaimed, the
//! device has no authoritative bank. It needs two pages and a deliberate second swap, so it
//! is a line somebody writes rather than a mistake, which is the same standing
//! [`crate::append`] gives two writers over one journal.
//!
//! # Why the retired reader is consumed
//!
//! §10 step 1 is "stop accepting new effects for the current run", and the only way to make
//! that a fact rather than a discipline is to take away the thing that appends.
//! [`Retired::Journal`] is the ordinary path — a run that met
//! [`Refusal::NearCapacity`](crate::capacity::Refusal::NearCapacity) is a run holding a
//! writer — and [`Retired::Recovery`] is the other one: a bank whose scan ended
//! [`Damaged`](crate::recovery::Ending::Damaged) or
//! [`Unsealed`](crate::recovery::Ending::Unsealed) has no writer at all, and it is exactly
//! the bank §10 says to recycle. Requiring a [`Journal`] would have made the swap
//! unavailable in the case it is most needed.
//!
//! It cannot make a second writer *impossible*: a caller that runs a second scan of the same
//! region gets one, exactly as [`Journal::after`] documents of itself. What it buys is that
//! appending to a retired run is a line somebody wrote on purpose.
//!
//! # What this module must not own
//!
//! Policy, and records. Whether a run *should* roll over is §10's capacity reserve in
//! [`crate::capacity`]; what the next run's input means is the workflow's; and a swap writes
//! no journal record at all — §10's two exits are a terminal record **or** a
//! `continue_as_new`, and this is the second one.

use core::fmt;
use core::marker::PhantomData;

use waymaker_core::{DecodeError, EffectIdAllocator, RunId};

use crate::append::Journal;
use crate::bank::{self, Authority, BankHeader, BankId, BankLayout, BankRegion, Generation};
use crate::frame::{self, RUN_STARTED_PREFIX_BYTES};
use crate::integrity::{Catalogued, IntegrityCheck};
use crate::recovery::{JournalRegion, Recovery, RegionError};
use crate::storage::StableStorage;

/// The reader or writer the retiring run was using, given up so that it cannot be used.
///
/// §10 step 1. See the module documentation for why both shapes are accepted and why the
/// value is taken rather than borrowed.
///
/// An enum rather than two constructors on [`Swap`], because a variant costs no public
/// function: the linear discipline is in the `self` of [`Swap::beginning`], and a second
/// entry point would be a second thing `swap-discipline` has to pin.
#[derive(Debug)]
pub enum Retired<C: IntegrityCheck = Catalogued> {
    /// The writer the run was appending with.
    Journal(Journal<C>),
    /// A scan of the retiring bank, for a run that has no writer — a journal that ended
    /// damaged or unsealed, which is the bank §10 recycles.
    ///
    /// Any [`Recovery`] over the retiring bank is accepted, finished or not: what §10 step 1
    /// asks for is that the reader be given up, and a scan half-way through one is still a
    /// reader. The wording above is what this variant is *for* rather than what it requires,
    /// and the probe and the crash sweep both hand it an unscanned one, because a scan reads
    /// and step 1 is about what can write.
    Recovery(Recovery<C>),
}

impl<C: IntegrityCheck> Retired<C> {
    /// The journal region this reader or writer was over, taking the value with it.
    ///
    /// Consuming rather than borrowing, because that is §10 step 1: the reader or writer the
    /// retiring run was using is destroyed here, in the only call that looks at it, and
    /// nothing that can append to that run survives the expression.
    const fn into_region(self) -> JournalRegion {
        match self {
            Self::Journal(journal) => journal.region(),
            Self::Recovery(recovery) => recovery.region(),
        }
    }
}

/// Why a swap could not be planned.
///
/// Configuration and device state, all of it decided before a byte moves — the same split
/// [`CapacityError`](crate::capacity::CapacityError) makes against
/// [`ReservedError`](crate::capacity::ReservedError), and for the same reason: a caller
/// acts differently on "this device cannot roll over" than on "the media refused a write".
///
/// Not `#[non_exhaustive]`, for the reason [`waymaker_core::DecodeError`] is not: every
/// match on it is in this workspace, and an exhaustive match is how the compiler tells
/// whoever adds a variant which call sites now have a case to think about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SwapError {
    /// The device does not have exactly one authoritative bank.
    ///
    /// [`Authority::Unsealed`] is a device with no run to continue, and
    /// [`Authority::Ambiguous`] is one with no single run to continue *from*. Neither is a
    /// swap, and neither is resolved here: a selection that picked one would hide the bug
    /// `Ambiguous` exists to report.
    NoAuthority,
    /// The authoritative bank is already at [`Generation::MAX`].
    ///
    /// Generations do not wrap, which is what makes the plain `u32` comparison in
    /// [`bank::select`] the order of the swaps. A device at the ceiling refuses to roll over
    /// rather than minting a generation that sorts below the bank it is replacing — this
    /// workspace's treatment of every bounded counter, and `waymaker_core`'s of the effect
    /// sequence.
    GenerationExhausted,
    /// The next run carries the run id of the run being retired.
    ///
    /// An effect is identified by the pair `(RunId, EffectSeq)` and the sequence restarts at
    /// a swap, so two runs sharing a run id share every effect identity they ever mint.
    /// Refused here because there is no later point at which it could be: once the header is
    /// on media the collision is durable.
    ///
    /// This catches the adjacent case and **is not a uniqueness check**. A run id belonging
    /// to any *earlier* run of the device passes it, and the collision is the same one — a
    /// dispatcher or an external service holding deduplication state would read the new run's
    /// first effects as redeliveries of that older run's. `RunId` is documented as unique on
    /// a device and nothing on the device remembers the ids it has retired, so global
    /// freshness is the caller's obligation; this is the cheap refusal of the mistake most
    /// likely to be made, not the guarantee. Codex found the overclaim on the second review
    /// round.
    RunReused,
    /// The retired reader or writer is not over the bank being retired.
    ///
    /// A caller holding a writer over the *inactive* bank would keep it across a swap that
    /// erases that bank underneath it. Compared against the bank
    /// [`Authority::Bank`] named, so the check is against what the device really booted from
    /// rather than against an argument.
    NotTheActiveBank,
    /// The retired reader was validated against a different device than the layout.
    ///
    /// The same refusal [`AppendError::WrongDevice`](crate::append::AppendError::WrongDevice)
    /// makes, taken one step earlier: every offset below is derived from the layout, and a
    /// region proved legal on another device says nothing about this one.
    WrongDevice,
    /// The next run's input leaves the installed bank no room for the record that run must
    /// write first.
    ///
    /// §10's roll-over is only an exit if the run it installs can *write* something, and two
    /// weaker tests are available here that both look like this one.
    /// [`Region`](Self::Region) below refuses only a journal of zero bytes.
    /// [`BankRegion::max_run_input_bytes`] reserves one *empty* record — and §09's
    /// `RunStarted` repeats the whole run input and adds four bytes of workflow identity in
    /// front of it, so an input at that ceiling installs a bank whose journal is twenty-four
    /// bytes and whose mandatory first record is four thousand. Durably, with the swap
    /// reporting success and no way out but another swap. Codex found that on this change's
    /// first review round; `an_installed_run_can_write_the_opening_record_it_must_write`
    /// drives it.
    ///
    /// So the bound is the header **and** a `RunStarted` carrying this input, both padded to
    /// the granularity, inside the bank's payload. What it deliberately does not price is the
    /// rest of a run — an effect scheduled, its outcome, a terminal record — which is
    /// [`Reserve::for_layout`]'s floor and a policy rather than a fact about whether the
    /// installed run can start.
    ///
    /// [`Reserve::for_layout`]: crate::capacity::Reserve::for_layout
    InputTooLong,
    /// The next run's header leaves no usable journal in the bank it would be installed in.
    ///
    /// The residual refusal, after [`InputTooLong`](Self::InputTooLong) has taken the case
    /// that matters. Carries [`JournalRegion::of`]'s own answer, so the caller learns whether
    /// the geometry or the granularity was what disagreed.
    ///
    /// [`RegionError::NoJournalRoom`] is not reachable through it on any layout
    /// [`BankLayout::new`] accepts — an input that would fill the bank exceeds
    /// [`InputTooLong`](Self::InputTooLong)'s ceiling first, by a whole record — so what a
    /// caller really meets here is [`RegionError::AlignDisagreesWithBank`]. The variant keeps
    /// the other shapes rather than flattening them, because a refusal that named the wrong
    /// cause would send an operator to the wrong place.
    Region(RegionError),
}

impl SwapError {
    /// A short static description of this refusal.
    ///
    /// # Postconditions
    ///
    /// Non-empty, ASCII, distinct from every other variant's, and shorter than a firmware
    /// log line — the contract every error in this workspace keeps, because a device with no
    /// debugger attached still has to be able to say which refusal it met.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NoAuthority => "no single bank is authoritative",
            Self::GenerationExhausted => "the generation space is spent",
            Self::RunReused => "the next run repeats the retired run",
            Self::NotTheActiveBank => "that reader is not the active bank",
            Self::WrongDevice => "that reader is on another device",
            Self::InputTooLong => "the next run cannot write its first record",
            Self::Region(inner) => inner.message(),
        }
    }
}

impl fmt::Display for SwapError {
    /// Writes [`SwapError::message`] and nothing else.
    ///
    /// [`fmt::Formatter::write_str`] rather than `write!`, for the reason every other error
    /// in this crate uses it: an argument would pull `core::fmt`'s formatting machinery into
    /// a firmware image that has a static string to hand.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for SwapError {}

/// Why a step of a planned swap did not happen.
///
/// Generic over the driver's error, for the reason
/// [`AppendError`](crate::append::AppendError) is: §12 lets every port name its own, and
/// flattening them throws away the only thing a driver author can act on. Deliberately no
/// [`fmt::Display`] either, and for the same reason — the bound would spread to every
/// signature this type appears in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SwapStepError<E> {
    /// The media refused an erase, a program or a barrier.
    ///
    /// What is on media afterwards is deliberately not guessed at. §12 says a failed
    /// mutation may still have changed it, and the swap is abandoned rather than retried:
    /// the retiring bank is untouched until step 7, so the device still boots the old run,
    /// which is §10's own answer for every crash before step 5.
    Storage(E),
    /// The next run's header could not be encoded, or the caller's page could not hold it.
    ///
    /// [`DecodeError::LengthOutOfBounds`] in both cases. The header's *fit in the bank* was
    /// settled at [`Swap::beginning`]; what is left here is the caller's buffer.
    Encode(DecodeError),
    /// The storage handed to a step is not the device the swap was planned against.
    ///
    /// Compared at **every** step and not only at the first, which is the lesson issue #24's
    /// review left: a barrier taken on some other device orders nothing on this one, so the
    /// new bank would be sealed without its payload ever having been made durable, and an
    /// erase aimed at an offset another device does not have is a write outside every bank
    /// it does.
    ///
    /// # What "device" means here, exactly
    ///
    /// A [`Geometry`](crate::storage::Geometry), and therefore not an *instance*. Two parts
    /// of the same model have the same geometry, so a caller holding two of them can
    /// [`prepare`](Swap::prepare) on one and [`commit`](Sealable::commit) on the other and
    /// this refusal will not fire — sealing a bank whose erase happened on the other chip, or
    /// erasing an unrelated device's active bank. Codex found that on the second review
    /// round.
    ///
    /// It is stated rather than closed because it is not this module's contract to change:
    /// [`AppendError::WrongDevice`](crate::append::AppendError::WrongDevice),
    /// [`RecoveryError::WrongDevice`](crate::recovery::RecoveryError::WrongDevice) and
    /// [`CapacityError::WrongDevice`](crate::capacity::CapacityError::WrongDevice) are the
    /// same comparison, and a swap that bound an instance while the writer beside it did not
    /// would be the one module in this crate whose `WrongDevice` meant something different.
    /// Binding storage identity across all four — by holding the `&mut S` through a
    /// protocol rather than accepting one per step — is issue
    /// [#84](https://github.com/madmax983/waymaker/issues/84).
    WrongDevice,
}

/// Everything about a swap that is decided before any media is touched.
///
/// Private, and carried by each state of the typestate rather than recomputed: the offsets
/// below were validated once, against one layout, and a step that re-derived them from a
/// caller's argument would be a step proving its bounds against something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plan {
    /// The journal the next run will write into.
    ///
    /// Also where the device comes from: a [`JournalRegion`] keeps the geometry it was
    /// validated against, so a second copy of it here would be sixteen bytes carried through
    /// five states to say the same thing twice.
    region: JournalRegion,
    /// The bank being installed into.
    installing: BankRegion,
    /// The bank being retired, which step 7 erases.
    retiring: BankRegion,
    /// Which bank [`installing`](Self::installing) is.
    installed: BankId,
    /// The generation the new bank is sealed at.
    generation: Generation,
    /// The run the new bank's header names.
    run: RunId,
}

impl Plan {
    /// Refuses `storage` unless it is the device this plan's offsets were proved against.
    ///
    /// Every step calls it, not only the first. A barrier taken on some other device orders
    /// nothing on this one, and an erase or a program aimed at an offset another device does
    /// not have is a write outside every bank it does.
    ///
    /// By reference, and answering `()`: a plan is eighty-odd bytes and there are five
    /// steps, so a check that handed the plan back would copy it five times for a comparison
    /// that reads one field.
    fn on<S: StableStorage>(&self, storage: &S) -> Result<(), SwapStepError<S::Error>> {
        if storage.geometry() == self.region.geometry() {
            Ok(())
        } else {
            Err(SwapStepError::WrongDevice)
        }
    }
}

/// A planned swap: step 1 has happened, and nothing has reached media.
///
/// Holding one means the retiring run's reader or writer is gone. Dropping one is legal and
/// changes nothing — the device still boots the run it was booting.
#[must_use = "a planned swap that is dropped installs nothing"]
#[derive(Debug)]
pub struct Swap<'next, C: IntegrityCheck = Catalogued> {
    plan: Plan,
    next: BankHeader<'next>,
    /// The check this swap seals with. Zero-sized: [`IntegrityCheck`]'s methods take no
    /// `self`, so there is nothing to carry and the field costs no bytes.
    check: PhantomData<C>,
}

impl<'next, C: IntegrityCheck> Swap<'next, C> {
    /// Plans the swap that replaces the run `retired` was reading or writing with `next`.
    ///
    /// §10 step 1. The only constructor, and it takes the whole of the next run up front
    /// because that is what §02 decision 6 means by "the workflow explicitly supplies the
    /// bounded input for its next run": the roll-over is declared, in full, at the moment
    /// the current run stops.
    ///
    /// # Preconditions
    ///
    /// `booted` is [`bank::select`]'s answer for this device **now**, and `run` is the run id
    /// the header of the bank it named carries. Neither is checked here — this call reads no
    /// media — and both are what the caller already decoded to get this far.
    ///
    /// `next.run` must also be fresh for the *device*, not merely different from `run`:
    /// [`SwapError::RunReused`] compares the two it is given and cannot see the run ids this
    /// device has already retired.
    ///
    /// Each is load-bearing in a different way, and neither failure has a symptom. A wrong
    /// `run` weakens exactly one thing, the [`SwapError::RunReused`] refusal, and the result
    /// is two runs whose `(RunId, EffectSeq)` pairs collide for ever. A *stale* `booted` —
    /// one naming a bank that has since lost a swap — is worse: [`prepare`](Self::prepare)
    /// would erase the bank that is really authoritative, and every check below passes,
    /// because the retired reader genuinely is over the bank `booted` named. Only a `booted`
    /// from another *device* is caught, by [`SwapError::NotTheActiveBank`]. See the module
    /// documentation for why the refusal that would close the stale case cannot be made
    /// fail-closed here, and whose it is.
    ///
    /// # Postconditions
    ///
    /// On success the bank to install into is the one the device did **not** boot from, the
    /// generation is strictly greater than the one it did, the next run's id differs from
    /// the retired one, and [`Installed::region`] is a journal that bank really has room
    /// for. Nothing has been read, programmed, erased or barriered.
    ///
    /// # Errors
    ///
    /// See [`SwapError`]: [`NoAuthority`](SwapError::NoAuthority),
    /// [`GenerationExhausted`](SwapError::GenerationExhausted),
    /// [`RunReused`](SwapError::RunReused),
    /// [`NotTheActiveBank`](SwapError::NotTheActiveBank),
    /// [`WrongDevice`](SwapError::WrongDevice) and [`Region`](SwapError::Region).
    pub fn beginning(
        layout: BankLayout,
        booted: Authority,
        run: RunId,
        retired: Retired<C>,
        next: BankHeader<'next>,
    ) -> Result<Self, SwapError> {
        let Authority::Bank { id, generation } = booted else {
            return Err(SwapError::NoAuthority);
        };
        let Some(generation) = generation.successor() else {
            return Err(SwapError::GenerationExhausted);
        };
        if next.run == run {
            return Err(SwapError::RunReused);
        }

        // §10 step 1, and the only place the retired reader is looked at: it is consumed
        // here, so no value that can append to the run being replaced outlives this line.
        let retiring = layout.bank(id);
        let region = retired.into_region();
        if region.geometry() != layout.geometry() {
            return Err(SwapError::WrongDevice);
        }
        if !within(retiring, region) {
            return Err(SwapError::NotTheActiveBank);
        }

        // Never a parameter: the bank installed into is the one the device did *not* boot.
        let installed = id.other();
        let installing = layout.bank(installed);
        // §10 step 3's header has to leave a journal the installed run can be *used* in,
        // which is two tests stronger than one that leaves a journal at all.
        //
        // `JournalRegion::of` below refuses only a journal of *zero* bytes.
        // `BankRegion::max_run_input_bytes` is stronger and still not enough, which is what
        // Codex found on the first review round: it reserves `encoded_len_for(0)`, an *empty*
        // record, and the record §08 obliges this run to write first is a `RunStarted` that
        // repeats the whole input and adds four bytes of workflow identity in front of it. So
        // the bound is the header *and* that record, both padded, inside the bank's payload —
        // and an input at the old ceiling installed a bank with a 24-byte journal and a
        // 4064-byte first record, durably, with the swap reporting success.
        let (Some(header), Ok(opening)) = (
            bank::header_len_for(next.input.len(), layout.align()),
            frame::encoded_len_for(
                RUN_STARTED_PREFIX_BYTES.saturating_add(next.input.len()),
                layout.align(),
            ),
        ) else {
            return Err(SwapError::InputTooLong);
        };
        if header.saturating_add(opening) > installing.payload_bytes() as usize {
            return Err(SwapError::InputTooLong);
        }
        // Both are decided before the erase of step 2, because a swap that erased the spare
        // bank and *then* found the header would not fit has destroyed the only other copy
        // of anything this device holds.
        let region = JournalRegion::of(layout, installed, &next).map_err(SwapError::Region)?;

        Ok(Self {
            plan: Plan {
                region,
                installing,
                retiring,
                installed,
                generation,
                run: next.run,
            },
            next,
            check: PhantomData,
        })
    }

    /// §10 step 2: erases the inactive bank, and waits for the erase to become durable.
    ///
    /// # Postconditions
    ///
    /// On success every byte of the bank being installed into is erased media, and the
    /// barrier has returned — so the header of step 3 cannot become durable ahead of the
    /// erase that would take it. See the module documentation for why that barrier is the
    /// protocol rather than caution.
    ///
    /// On failure the swap is over. The retiring bank has not been touched, so the device
    /// still boots the run it was booting, and the inactive bank is at worst partly erased —
    /// which is a bank that is not a candidate at any generation rather than a bank that
    /// competes.
    ///
    /// # Errors
    ///
    /// [`SwapStepError::WrongDevice`] when `storage` is not the device the swap was planned
    /// against, and [`SwapStepError::Storage`] when the erase or the barrier fails.
    pub fn prepare<S: StableStorage>(
        self,
        storage: &mut S,
    ) -> Result<Prepared<'next, C>, SwapStepError<S::Error>> {
        self.plan.on(storage)?;
        storage
            .erase(self.plan.installing.base(), self.plan.installing.bytes())
            .map_err(SwapStepError::Storage)?;
        storage.barrier().map_err(SwapStepError::Storage)?;
        Ok(Prepared {
            plan: self.plan,
            next: self.next,
            check: PhantomData,
        })
    }
}

/// An erased bank, waiting for the run that will live in it.
///
/// The only thing to do with one is [`stage`](Self::stage). It has no `commit` and no way to
/// program a seal, which is half of what makes §10's step order unrepresentable rather than
/// documented.
#[must_use = "an erased bank with no header in it is not a run"]
#[derive(Debug)]
pub struct Prepared<'next, C: IntegrityCheck = Catalogued> {
    plan: Plan,
    next: BankHeader<'next>,
    check: PhantomData<C>,
}

impl<C: IntegrityCheck> Prepared<'_, C> {
    /// §10 step 3: writes the new bank header — the new run id, workflow version and input.
    ///
    /// The header is encoded into `page` and programmed from it; the seal that will make the
    /// bank authoritative is then encoded into the same `page`, and the returned [`Staged`]
    /// borrows it. The borrow is what stops a caller reusing the page between the two, which
    /// would program a seal over bytes that are no longer the seal — the same discipline
    /// [`crate::append`] keeps, and the reason no buffer is copied into this type.
    ///
    /// # Postconditions
    ///
    /// On success the padded header frame is on media at the bank's base and the seal in
    /// `page` carries *that* frame's digest, so no other header can ever be committed under
    /// it. The bank is not yet authoritative and will not be until step 6: nothing before
    /// that barrier changes which run a reader boots.
    ///
    /// On failure the bank is at worst written and unsealed, which is not a candidate at any
    /// generation — but "at worst" is doing real work here, and this is the one step whose
    /// failure can leave media changed. A refusal before the program leaves the erased bank
    /// of step 2; a failed program leaves whatever §12 says a failed program leaves; and a
    /// seal that cannot be *encoded* leaves a whole header on media with no seal coming. All
    /// three boot the run the device was already running, and all three are recycled by the
    /// next swap's step 2 rather than repaired.
    ///
    /// # Errors
    ///
    /// [`SwapStepError::WrongDevice`] when `storage` is not the device the swap was planned
    /// against, [`SwapStepError::Encode`] when `page` cannot hold the header or the seal, and
    /// [`SwapStepError::Storage`] when the program fails.
    pub fn stage<'page, S: StableStorage>(
        self,
        storage: &mut S,
        page: &'page mut [u8],
    ) -> Result<Staged<'page, C>, SwapStepError<S::Error>> {
        self.plan.on(storage)?;

        let written =
            bank::encode_header_with::<C>(&self.next, page).map_err(SwapStepError::Encode)?;
        let Some(frame) = page.get(..written) else {
            return Err(SwapStepError::Encode(DecodeError::LengthOutOfBounds));
        };
        // Taken before the page is reused for the seal. This is the *encoded* header rather
        // than a read-back, and deliberately: what stops a seal reaching a header that never
        // landed is the `?` on the program below, not where the digest came from. See the
        // module documentation.
        let seal =
            bank::seal_for_with::<C>(frame, self.plan.generation).map_err(SwapStepError::Encode)?;
        storage
            .program(self.plan.installing.base(), frame)
            .map_err(SwapStepError::Storage)?;

        let align = self.plan.region.align();
        let sealed =
            bank::encode_seal_with::<C>(&seal, align, page).map_err(SwapStepError::Encode)?;
        // The page is frozen from here: the seal rides on the caller's buffer rather than
        // being copied into this type.
        let frozen: &'page [u8] = &*page;
        let Some(bytes) = frozen.get(..sealed) else {
            return Err(SwapStepError::Encode(DecodeError::LengthOutOfBounds));
        };
        Ok(Staged {
            plan: self.plan,
            seal: bytes,
            check: PhantomData,
        })
    }
}

/// A bank header on media, waiting for the barrier that lets it be sealed.
///
/// The whole of §10's "a crash before step 5 recovers the old run" is that this type cannot
/// program a seal. The only thing to do with one is
/// [`payload_barrier`](Self::payload_barrier), and the only thing that produces a
/// [`Sealable`] is that call.
///
/// Dropping one is legal and leaves a written, unsealed bank on media. That is not a leak
/// and not a silent failure: an unsealed bank is not a candidate at any generation, the
/// device still boots the run it was booting, and the next swap erases it again.
#[must_use = "a staged bank is not authoritative until its payload barrier and commit have \
              returned"]
#[derive(Debug)]
pub struct Staged<'page, C: IntegrityCheck = Catalogued> {
    plan: Plan,
    /// The generation seal, still in the caller's page.
    seal: &'page [u8],
    check: PhantomData<C>,
}

impl<'page, C: IntegrityCheck> Staged<'page, C> {
    /// §10 step 4: waits for the new bank's payload to become durable.
    ///
    /// # Postconditions
    ///
    /// On success the header frame survives reset and a [`Sealable`] exists, which is the
    /// only value in this crate that can program a generation seal.
    ///
    /// On failure there is none. §12: a caller that met an error at a barrier has learned
    /// nothing about what is on media, so the staged bank is consumed and cannot be sealed
    /// at all — the failure closing in the direction that cannot make an unwritten bank
    /// authoritative.
    ///
    /// # Errors
    ///
    /// [`SwapStepError::WrongDevice`] when `storage` is not the device the swap was planned
    /// against, and [`SwapStepError::Storage`] if the barrier fails.
    pub fn payload_barrier<S: StableStorage>(
        self,
        storage: &mut S,
    ) -> Result<Sealable<'page, C>, SwapStepError<S::Error>> {
        self.plan.on(storage)?;
        storage.barrier().map_err(SwapStepError::Storage)?;
        Ok(Sealable {
            plan: self.plan,
            seal: self.seal,
            check: PhantomData,
        })
    }
}

/// A durable bank payload, and the seal that will make it authoritative.
///
/// Reachable only from [`Staged::payload_barrier`]. There is no constructor and no other
/// method that returns one, which is what makes [`commit`](Self::commit) safe to be the one
/// place a generation seal is programmed.
#[must_use = "a sealable bank is not authoritative until `commit` has returned"]
#[derive(Debug)]
pub struct Sealable<'page, C: IntegrityCheck = Catalogued> {
    plan: Plan,
    seal: &'page [u8],
    check: PhantomData<C>,
}

impl<C: IntegrityCheck> Sealable<'_, C> {
    /// §10 steps 5 and 6: programs the generation seal and waits for it to become durable.
    ///
    /// # Postconditions
    ///
    /// On success the new bank is authoritative — it carries the highest valid generation
    /// seal on the device — and §10's "a crash after step 6 recovers the new run" is true
    /// from the moment this returns. The retiring bank is still intact and still sealed, at
    /// a strictly lower generation, until [`Installed::reclaim`].
    ///
    /// On failure the seal may or may not be on media, and this deliberately does not guess:
    /// both answers a device that lost power here can give — the new run authoritative, or
    /// the old one still — are legal, and they are §10's two recovery rules. What is *not*
    /// possible is a valid seal over a payload that never became durable, because
    /// [`Staged::payload_barrier`] returned before this call existed.
    ///
    /// # Errors
    ///
    /// [`SwapStepError::WrongDevice`] when `storage` is not the device the swap was planned
    /// against, and [`SwapStepError::Storage`] if the program or the barrier fails.
    pub fn commit<S: StableStorage>(
        self,
        storage: &mut S,
    ) -> Result<Installed, SwapStepError<S::Error>> {
        self.plan.on(storage)?;
        storage
            .program(self.plan.installing.seal_offset(), self.seal)
            .map_err(SwapStepError::Storage)?;
        storage.barrier().map_err(SwapStepError::Storage)?;
        Ok(Installed { plan: self.plan })
    }
}

/// A run that is on media and authoritative, and the bank the swap replaced.
///
/// §10 step 7's other half: what a caller does *after* a successful swap. It is not generic
/// over the integrity check, because nothing left to do reads or writes a seal.
///
/// # Why it is not `Copy`
///
/// [`reclaim`](Self::reclaim) takes `self` so that §10's lazy erase happens once, and a
/// `Copy` type would make that consumption a fiction: a caller holding two would erase the
/// retired bank twice, which is a second erase cycle on a part that has a countable number
/// of them. `Journal` is not `Copy` for the same shape of reason, one layer down.
#[must_use = "a completed swap reports the journal the new run writes into"]
#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    plan: Plan,
}

impl Installed {
    /// What [`bank::select`] would now say, and what the next swap begins from.
    ///
    /// Not read back from media: it is what this swap installed, and a device that
    /// disagreed with it would be a device whose commit barrier did not hold — which is
    /// §12's contract and `waymaker-conformance`'s across-reset witness rather than
    /// something a return value can establish.
    #[must_use]
    pub const fn authority(&self) -> Authority {
        Authority::Bank {
            id: self.plan.installed,
            generation: self.plan.generation,
        }
    }

    /// The journal the new run writes into.
    ///
    /// Validated at [`Swap::beginning`], before the erase, so this costs the caller no
    /// second chance to get §10's chain wrong. Every byte of it is erased media: step 2
    /// erased the whole bank and step 3 programmed only the header in front of this region,
    /// so a [`Recovery`] over it ends [`Clean`](crate::recovery::Ending::Clean) at zero.
    ///
    /// A [`Journal`] is deliberately *not* handed back. [`Journal::after`] taking a finished
    /// [`Recovery`] and nothing else is what makes issue #23's anti-bricking rule structural,
    /// and a second constructor for the writer — even one this module could prove correct —
    /// is a second way to reach an append offset that no scan vouched for.
    #[must_use]
    pub const fn region(&self) -> JournalRegion {
        self.plan.region
    }

    /// An effect id allocator for the run this swap installed.
    ///
    /// Issue #26's third "done when": the sequence restarts at [`EffectSeq::FIRST`] under a
    /// run id that is not the retired one, so the two runs' effect ids stay distinguishable
    /// even though their sequences are the same numbers. `(RunId, EffectSeq)` is the pair
    /// §07 identifies an effect by, and [`SwapError::RunReused`] is what stops the pair from
    /// collapsing.
    ///
    /// A constructor rather than a handle: this is
    /// [`EffectIdAllocator::for_run`] with a run id the caller cannot get wrong, and calling
    /// it twice is what calling `for_run` twice already is. What it removes is the one
    /// mistake with no symptom — building the next run's allocator from the *retired* run's
    /// id, which mints identities history already holds.
    ///
    /// [`EffectSeq::FIRST`]: waymaker_core::EffectSeq::FIRST
    #[must_use]
    pub const fn allocator(&self) -> EffectIdAllocator {
        EffectIdAllocator::for_run(self.plan.run)
    }

    /// §10 step 7: erases the bank the swap replaced.
    ///
    /// Lazy, and crash-safe by construction rather than by care. The new bank already
    /// carries a strictly higher generation, so the retiring bank loses [`bank::select`]
    /// whether it is whole, half erased or gone — an interrupted erase can only *remove* a
    /// candidate, never promote one. And it cannot reach the wrong bank: which one is
    /// retiring was decided at [`Swap::beginning`] from the authority the device booted, and
    /// there is no parameter here to get it wrong with.
    ///
    /// # Postconditions
    ///
    /// On success the retiring bank is erased media and the device has exactly one
    /// authoritative bank. On failure the device has one authoritative bank as well: the new
    /// one, which is the only thing this step could have changed and does not.
    ///
    /// # Errors
    ///
    /// [`SwapStepError::WrongDevice`] when `storage` is not the device the swap was planned
    /// against, and [`SwapStepError::Storage`] when the erase or the barrier fails. Neither is
    /// fatal to the run that was installed — a bank that is still there is a bank the next
    /// swap erases again.
    pub fn reclaim<S: StableStorage>(self, storage: &mut S) -> Result<(), SwapStepError<S::Error>> {
        self.plan.on(storage)?;
        storage
            .erase(self.plan.retiring.base(), self.plan.retiring.bytes())
            .map_err(SwapStepError::Storage)?;
        // Spelled with the `?` its sibling in `prepare` uses rather than as a tail
        // expression, so that one pinned spelling means the same thing in both bodies.
        storage.barrier().map_err(SwapStepError::Storage)?;
        Ok(())
    }
}

/// Whether `region` lies inside `bank`'s payload.
///
/// A journal is the bytes between a bank's header and its seal, so a region of the bank it
/// names is inside [`BankRegion::payload_bytes`] — and a region of the *other* bank, or of
/// somewhere else entirely, is not. Written with checked arithmetic because the two ends are
/// caller-supplied and a wrap here would accept the region it exists to refuse.
const fn within(bank: BankRegion, region: JournalRegion) -> bool {
    let (Some(end), Some(limit)) = (
        region.base().checked_add(region.bytes()),
        bank.base().checked_add(bank.payload_bytes()),
    ) else {
        return false;
    };
    region.base() >= bank.base() && end <= limit
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Geometry;

    #[test]
    fn a_region_is_inside_the_bank_it_belongs_to_and_no_other() {
        let Ok(geometry) = Geometry::new(8192, 4096, 8, 1) else {
            unreachable!("8192 is two whole 4096-byte blocks")
        };
        let Ok(layout) = BankLayout::new(geometry) else {
            unreachable!("two erase blocks are two banks")
        };
        let (first, second) = (layout.bank(BankId::A), layout.bank(BankId::B));
        let align = layout.align();
        let Ok(inside) = JournalRegion::spanning(geometry, first.base(), 64, align) else {
            unreachable!("a 64-byte journal at a bank's base is a legal region")
        };
        let Ok(elsewhere) = JournalRegion::spanning(geometry, second.base(), 64, align) else {
            unreachable!("the same is true of the other bank")
        };

        assert!(within(first, inside));
        assert!(!within(second, inside));
        assert!(within(second, elsewhere));
        assert!(!within(first, elsewhere));
    }

    #[test]
    fn a_region_running_into_the_seal_is_not_inside_the_bank() {
        // The payload is what a journal may be in: a region that reached the generation
        // seal would be a journal whose last record overwrites the thing that makes the
        // bank authoritative.
        let Ok(geometry) = Geometry::new(8192, 4096, 8, 1) else {
            unreachable!("8192 is two whole 4096-byte blocks")
        };
        let Ok(layout) = BankLayout::new(geometry) else {
            unreachable!("two erase blocks are two banks")
        };
        let bank = layout.bank(BankId::A);
        let Ok(whole) =
            JournalRegion::spanning(geometry, bank.base(), bank.bytes(), layout.align())
        else {
            unreachable!("a whole bank is a legal program range")
        };

        assert!(!within(bank, whole));
    }
}
