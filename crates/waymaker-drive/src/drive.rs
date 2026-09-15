//! The loop: recovery, the kernel boundary, and the two-barrier writer.
//!
//! Design document §06's cold-start replay, as one function. Every decision it takes comes
//! from [`Intent`] or [`Resolve`] — the driver reads records to *feed* the kernel and
//! writes records the kernel asked for, and it never decides anything from a record itself.
//! That is what makes the façade one layer up a façade: there is nothing left for it to own
//! but `async` syntax.

use core::marker::PhantomData;
use core::mem;

use waymaker_core::timer::{ClockCapability, ClockKind, Deadline, Timer, TimerSpec};
use waymaker_core::version::{GateId, VersionRange};
use waymaker_core::{
    ActivityKind, DecodeError, EffectId, EffectRequest, Intent, KernelError, Next, Outcome,
    RecordRef, ReplayMachine, Resolve, RunId, TimerIntent, TimerRequest, TimerResolve,
    VersionIntent, VersionRequest,
};
use waymaker_flash::append::{AppendError, Journal};
use waymaker_flash::bank::{self, Authority, BankHeader, BankId, BankLayout};
use waymaker_flash::capacity::{CapacityError, Refusal, Reserve, Reserved, ReservedError};
use waymaker_flash::frame::{self, ProgramAlign};
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};
use waymaker_flash::recovery::{JournalRegion, Recovery, RecoveryError};
use waymaker_flash::storage::StableStorage;
use waymaker_flash::swap::{Retired, Swap, SwapError, SwapStepError};

use crate::activity::{Activities, Clocks, Performed};
use crate::boundary::{Answered, Boundary, Handoff, Suspended};
use crate::effect::{Dispatchable, Effect, Resolution, Resolved, Scheduled};
use crate::workflow::Workflow;

/// How a run ended, without the bytes it ended with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Conclusion {
    /// A `RunCompleted` record.
    Completed,
    /// A `RunFailed` record.
    Failed,
}

/// How far one boot got.
///
/// Four answers: the run reached a terminal record, an activity was not ready, a deadline
/// has not passed, or `continue_as_new` installed a new run. The middle two are both waits
/// under a committed identity, kept apart because a caller acts on them differently — an
/// activity may answer on the next pass, and a deadline will not answer before its own
/// clock says so. There is no fifth — an error is the [`Err`] this is returned beside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Progress {
    /// The run has a terminal record, and the first `result_len` bytes of the caller's
    /// result buffer are what it ended with.
    Finished {
        /// Which terminal record history holds.
        conclusion: Conclusion,
        /// How much of the caller's result buffer the terminal payload fills.
        result_len: usize,
    },
    /// An activity answered [`Performed::Pending`]. The schedule record for `id` is
    /// committed, so the next boot redelivers it under the same identity.
    Waiting {
        /// The effect the run is waiting on.
        id: EffectId,
    },
    /// A deadline has not passed. Its `TimerScheduled` record is committed, so the next
    /// boot arms the same deadline from the reading history recorded.
    ///
    /// `remaining` is in `clock_kind`'s unit, and the kind travels beside it because the two
    /// units need not be the same one. [`Clocks`] says a reading is "in that clock's own
    /// unit" and that the kernel never converts, so a firmware whose RTC counts seconds and
    /// whose boot clock counts milliseconds is an ordinary firmware — and a caller handed a
    /// bare number could not tell which alarm to set it on, or by how much to scale it. A
    /// wait this driver documents as usable for sleeping has to say what it is measured in.
    /// Codex found the version that did not.
    ///
    /// This driver has no sleep of its own: design document §11's in-boot sleep is rung
    /// 0.4's.
    WaitingUntil {
        /// The timer the run is waiting on.
        id: EffectId,
        /// Which clock `remaining` is counted in.
        clock_kind: ClockKind,
        /// Ticks of that clock still owed, as of this boot's last reading.
        remaining: u64,
    },
    /// [`Boundary::continue_as_new`] performed §10's swap. The run that asked is retired;
    /// `run` is the one now installed.
    ///
    /// Only a driver [`at_bank`](Driver::at_bank) ever answers this — one
    /// [`Driver::new`] cannot name the bank a swap would install into, and its
    /// `continue_as_new` always refuses instead. The next boot of the same layout picks up
    /// the newly authoritative bank on its own; nothing here schedules that boot.
    Migrated {
        /// The run [`continue_as_new`](Boundary::continue_as_new) installed.
        run: RunId,
    },
}

/// Why a boot could not go on.
///
/// Every variant is a refusal rather than a guess. Design document §08 says "stop with
/// `NondeterministicWorkflow`; never guess", and §14 says a damaged prefix is history that
/// stands rather than history to be repaired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DriveError<E> {
    /// The recovery scan refused a frame, or the media could not be read.
    Recovery(RecoveryError<E>),
    /// A record could not be appended.
    Append(AppendError<E>),
    /// The kernel refused: a divergent workflow, or history that could not be written.
    Kernel(KernelError),
    /// The scan established no append point, so there is nowhere safe to write.
    ///
    /// Design document §14's "frame ignored; previous history prefix wins", from the
    /// writer's side: a scan that stopped at damage has an offset that may already be
    /// programmed, and on NOR a bank appended to there never boots again.
    NoAppendPoint,
    /// The recorded `RunStarted` describes a different workflow, version or input.
    NotThisWorkflow,
    /// The workflow ended while committed history holds records it never asked for.
    ///
    /// Not [`KernelError::NondeterministicWorkflow`], because the kernel was never given
    /// the chance to say so: §08's divergence check runs at an effect boundary, and a
    /// workflow that ends early reaches none. The fault is the same one.
    HistoryContinues,
    /// The result buffer is narrower than the bound the run declared.
    ///
    /// Refused at the start of a boot, before a record is written. An activity is handed a
    /// buffer of exactly `Bounds::effect_result_bytes`, so a narrower one would make the
    /// buffer a second, smaller bound that §10 never priced.
    ResultBufferTooSmall {
        /// What the run declared its effect results may be worth.
        needed: usize,
        /// How many bytes the caller supplied.
        available: usize,
    },
    /// More bytes were offered than may be handed back.
    ///
    /// The limit is the narrower of the caller's result buffer and the bound the run
    /// declared for that payload — §10's `effect_result_bytes` for an effect outcome,
    /// `terminal_bytes` for a terminal record. A record already on media that exceeds the
    /// bound is history written under other bounds, and it is refused rather than truncated.
    ResultTooLong {
        /// How many bytes were offered.
        produced: usize,
        /// How many may be handed back.
        available: usize,
    },
    /// An activity input longer than a schedule record can describe.
    InputTooLong {
        /// How many bytes the workflow passed.
        bytes: usize,
    },
    /// §10's reserve does not fit the journal this driver was pointed at.
    ///
    /// Decided when the writer is opened, before a record is staged: a journal that cannot
    /// hold the reserve is a journal in which the run's exits were never affordable.
    Reserve(CapacityError),
    /// A clock could not be read.
    ///
    /// [`Clocks::now`] answered [`None`]: the hardware failed, or the firmware has no clock
    /// of that kind. Either way the deadline cannot be measured, and design document §02
    /// decision 8 is that a deadline nothing can measure is refused rather than guessed at.
    ClockUnavailable,
    /// §07's two halves were called out of order: a schedule while one was outstanding.
    ///
    /// [`Boundary::schedule`](crate::Boundary::schedule) hands the writer to the effect it
    /// committed, so a second schedule before
    /// [`Boundary::resolve`](crate::Boundary::resolve) would leave the first effect with no
    /// way to record its outcome, and §08 has no edge from an unresolved effect to a
    /// terminal record.
    EffectOutstanding,
    /// §07's two halves were called out of order: a resolve with no effect outstanding.
    ///
    /// A caller that never committed an intent has no outcome to record.
    NoEffectOutstanding,
    /// §10's `continue_as_new` was asked of a driver that cannot swap banks.
    ///
    /// See [`Boundary::continue_as_new`](crate::Boundary::continue_as_new): a driver built
    /// with [`Driver::new`] is pointed at a journal region rather than at a bank, so it
    /// cannot name the bank a swap would install into. A driver built with
    /// [`Driver::at_bank`] never answers this.
    ContinueUnsupported,
    /// §10 refused the record, before the device was asked for anything.
    ///
    /// The refusal that keeps this driver honest. Without it a schedule record is committed,
    /// the effect is dispatched, and the outcome record then does not fit — which strands
    /// the run for ever, because §08 has no edge from an unresolved effect to a terminal
    /// record, and re-performs the effect on every boot after it.
    Capacity(Refusal),
    /// A driver [`at_bank`](Driver::at_bank) found no bank a boot could start from.
    ///
    /// Neither bank carries a valid seal — the ordinary state of a device nothing has
    /// provisioned yet. Provisioning the first bank is outside this driver.
    NoAuthoritativeBank,
    /// A driver [`at_bank`](Driver::at_bank) found both banks validly sealed at the same
    /// generation.
    ///
    /// §02 decision 7 makes a new run authoritative on its own generation seal, and a seal
    /// that repeats a generation seals nothing. No protocol this driver runs can produce
    /// it; a reader that reports it anyway is reporting a device this driver did not write.
    AmbiguousAuthority,
    /// [`Boundary::continue_as_new`](crate::Boundary::continue_as_new)'s next run id would
    /// wrap.
    ///
    /// §07's identity space is a `u64`. A device that reaches the ceiling refuses rather
    /// than reissuing a run id it has already sealed a bank under.
    RunIdExhausted,
    /// §10's swap refused to begin.
    Swap(SwapError),
    /// §10's swap failed partway through a step.
    ///
    /// What is on media afterwards is deliberately not guessed at — see
    /// [`SwapStepError`]'s own documentation. The retiring bank is untouched until the last
    /// step, so the device still boots the old run either way.
    SwapStep(SwapStepError<E>),
    /// [`Boundary::continue_as_new`](crate::Boundary::continue_as_new)'s next run input is
    /// wider than the run's own declared bound.
    ///
    /// Refused before the device is touched. `Swap::beginning`'s own gate only asks that
    /// the header and one opening record fit the bank; it says nothing about the bound
    /// *this* run priced its exits against, and installing a header wider than that bound
    /// would price the new run's own journal below `Reserve::for_layout`'s floor —
    /// stranding it the moment it is booted, with `DriveError::Reserve` on every boot after.
    NextRunInputTooLong {
        /// How many bytes the workflow passed.
        bytes: usize,
        /// The declared bound it was measured against.
        bound: u16,
    },
}

/// The two buffers a boot borrows.
///
/// Named rather than two adjacent `&mut [u8]` parameters, because two adjacent slice
/// parameters of different meanings are swappable in silence: a caller that passed them the
/// other way round would get a run that succeeded, or a `PageTooSmall` that points at
/// neither buffer. They cannot alias — two `&mut [u8]` in safe Rust are disjoint by
/// construction — so the only mistake left is which is which, and this is what removes it.
#[derive(Debug)]
pub struct Scratch<'a> {
    /// Where one record at a time is staged. Never retained across a call.
    ///
    /// Design document §04 states the runtime RAM budget with a 512-byte page. It must hold
    /// the largest record this run writes or replays; a smaller one is refused with
    /// [`DriveError::Recovery`] carrying `PageTooSmall`, or with [`DriveError::Append`].
    pub page: &'a mut [u8],
    /// Where an activity writes its outcome, and where a terminal payload is left.
    ///
    /// Every borrowed byte a workflow sees points in here, and the next boundary overwrites
    /// it. It must be at least `Bounds::effect_result_bytes` wide, which
    /// [`boot`](Driver::boot) refuses before it writes anything.
    pub result: &'a mut [u8],
}

/// What a [`Driver`] is configured against.
///
/// Two shapes, kept apart rather than merged into one carrying an optional bank: a driver
/// built with [`Driver::new`] is pointed at a fixed region and knows no bank at all, so its
/// [`Boundary::continue_as_new`](crate::Boundary::continue_as_new) genuinely cannot swap.
/// One built with [`Driver::at_bank`] carries a [`BankLayout`] instead and discovers its
/// region afresh from the device at every [`boot`](Driver::boot) — which is what keeps
/// that discovery from ever being a value this driver could be carrying stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pointing {
    /// A fixed journal region, and the run it belongs to.
    Region(JournalRegion, RunId),
    /// A device's two-bank layout. [`boot`](Driver::boot) selects the authoritative bank
    /// itself, every time.
    Bank(BankLayout),
}

/// A synchronous driver for one run's journal.
///
/// Configuration only: what it is pointed at. Everything that changes lives in
/// [`boot`](Self::boot)'s stack frame, which is what makes two boots of one driver two
/// independent replays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Driver<C: IntegrityCheck = Catalogued> {
    pointing: Pointing,
    reserve: Reserve,
    check: PhantomData<C>,
}

impl Driver<Catalogued> {
    /// A driver over `region`, for `run`, sealing with the shipped integrity check.
    #[must_use]
    pub const fn new(region: JournalRegion, run: RunId, reserve: Reserve) -> Self {
        Self::with_integrity(region, run, reserve)
    }

    /// A driver over `layout`'s two banks, sealing with the shipped integrity check.
    #[must_use]
    pub const fn at_bank(layout: BankLayout, reserve: Reserve) -> Self {
        Self::at_bank_with_integrity(layout, reserve)
    }
}

impl<C: IntegrityCheck> Driver<C> {
    /// A driver that verifies and seals with `C`.
    ///
    /// The run is taken separately from the region because §07 keeps the run id in the bank
    /// header rather than in every record: the journal cannot say which run it is.
    #[must_use]
    pub const fn with_integrity(region: JournalRegion, run: RunId, reserve: Reserve) -> Self {
        Self {
            pointing: Pointing::Region(region, run),
            reserve,
            check: PhantomData,
        }
    }

    /// [`at_bank`](Driver::at_bank), verifying and sealing with `C`.
    ///
    /// Design document §10, issue [#110](https://github.com/madmax983/waymaker/issues/110).
    /// Every [`boot`](Self::boot) reads both banks' seals fresh and replays whichever is
    /// authoritative, so `booted` is never a value this driver could be carrying stale from
    /// an earlier boot — and this is the one shape whose
    /// [`Boundary::continue_as_new`](crate::Boundary::continue_as_new) performs a real
    /// swap rather than refusing.
    #[must_use]
    pub const fn at_bank_with_integrity(layout: BankLayout, reserve: Reserve) -> Self {
        Self {
            pointing: Pointing::Bank(layout),
            reserve,
            check: PhantomData,
        }
    }

    /// §10's reserve every append is gated by.
    #[must_use]
    pub const fn reserve(&self) -> Reserve {
        self.reserve
    }

    /// The journal this driver replays and extends, for a driver pointed at a fixed region.
    ///
    /// [`None`] for a driver built with [`at_bank`](Driver::at_bank): its region is not
    /// known until [`boot`](Self::boot) has read the device.
    #[must_use]
    pub const fn region(&self) -> Option<JournalRegion> {
        match self.pointing {
            Pointing::Region(region, _) => Some(region),
            Pointing::Bank(_) => None,
        }
    }

    /// The run this driver replays, for a driver pointed at a fixed region.
    ///
    /// [`None`] for a driver built with [`at_bank`](Driver::at_bank): its run is not known
    /// until [`boot`](Self::boot) has read the device.
    #[must_use]
    pub const fn run(&self) -> Option<RunId> {
        match self.pointing {
            Pointing::Region(_, run) => Some(run),
            Pointing::Bank(_) => None,
        }
    }

    /// One boot: recover, replay, and carry the run as far as it goes.
    ///
    /// `world` is the two halves of design document §06's outward boundary: the
    /// [`Activities`] a workflow calls and the [`Clocks`] its deadlines are measured
    /// against. They are one argument because a run needs both and neither is optional —
    /// a firmware with no timers still declares
    /// [`ClockCapability::BootOnly`](waymaker_core::timer::ClockCapability::BootOnly).
    ///
    /// `page` is the scratch page every record is staged through — one record at a time,
    /// never retained. `result` is where an activity writes its outcome and where a
    /// terminal payload is left for the caller; it is a second buffer because a record is
    /// encoded into `page` *from* bytes that have to be somewhere else while that happens.
    ///
    /// # Postconditions
    ///
    /// * A schedule record is durable before its effect is dispatched — §02 decision 3, as
    ///   the order of two calls rather than as a comment.
    /// * A replayed effect is answered from history and never dispatched.
    /// * A redelivered effect is dispatched under the identity its schedule record already
    ///   committed.
    /// * Nothing is written after the first refusal.
    ///
    /// # Errors
    ///
    /// Every variant of [`DriveError`]; see each for what it means.
    pub fn boot<S, A, W>(
        &self,
        storage: &mut S,
        world: &mut A,
        workflow: &mut W,
        scratch: Scratch<'_>,
    ) -> Result<Progress, DriveError<S::Error>>
    where
        S: StableStorage,
        A: Activities + Clocks,
        W: Workflow,
    {
        let Scratch { page, result } = scratch;
        // Before anything reaches media. A boot that discovered this at the first effect
        // would already have committed the run's opening record.
        //
        // The *wider* of the run's two bounds, not the effect one alone. A terminal payload
        // is bounded by `terminal_bytes`, and a buffer that held every effect result and
        // not every terminal payload would refuse a legal run at its last record — after
        // every effect had been performed.
        let bounds = self.reserve.bounds();
        let needed = usize::from(bounds.effect_result_bytes.max(bounds.terminal_bytes));
        if result.len() < needed {
            return Err(DriveError::ResultBufferTooSmall {
                needed,
                available: result.len(),
            });
        }
        let (region, run, bank) = match self.pointing {
            Pointing::Region(region, run) => (region, run, None),
            Pointing::Bank(layout) => {
                let facts = select_bank::<S, C>(layout, storage, page)?;
                // §10's `input` is durably recorded in the header for exactly this: an
                // erased journal writes `RunStarted` from `workflow.identity()` alone, with
                // nothing else to check it against, so a workflow booting the bank a swap
                // just installed would otherwise be trusted to claim any input it liked.
                verify_header_identity::<S, C, W>(layout, facts.id, storage, page, workflow)?;
                let bank = BankContext {
                    layout,
                    booted: Authority::Bank {
                        id: facts.id,
                        generation: facts.generation,
                    },
                    run: facts.run,
                    region: facts.region,
                    align: facts.align,
                    workflow_kind: facts.workflow_kind,
                    workflow_version: facts.workflow_version,
                    input_schema: facts.input_schema,
                };
                (facts.region, facts.run, Some(bank))
            }
        };
        let mut machine = ReplayMachine::new(run);
        // `storage` moves in here rather than staying a field of `Context`: see `Source`'s
        // own documentation for why one caller cannot hold it in both places at once.
        let mut source = Source::Scanning(Recovery::<_, C>::with_integrity(region, storage));

        // A bank a swap installed but nothing has yet booted carries its intended version
        // nowhere but its own immutable header — see `begin`'s own documentation of why an
        // empty journal alone is the one case that still needs it.
        let header_version = bank.as_ref().map(|context| context.workflow_version);
        let recorded_version = begin(
            &mut source,
            &mut machine,
            workflow,
            page,
            self.reserve,
            header_version,
        )?;

        let mut context = Context {
            activities: world,
            machine: &mut machine,
            page,
            result,
            source,
            reserve: self.reserve,
            bank,
            stop: None,
            pending: None,
            versions: workflow.identity().versions,
            recorded_version,
        };
        let ended = workflow.run(&mut context);
        context.conclude(ended)
    }
}

/// What a bank-pointed driver knows about the bank it booted, kept for
/// [`Boundary::continue_as_new`](crate::Boundary::continue_as_new).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BankContext {
    layout: BankLayout,
    booted: Authority,
    run: RunId,
    region: JournalRegion,
    align: ProgramAlign,
    workflow_kind: u16,
    workflow_version: u16,
    input_schema: u16,
}

/// One bank's generation and the scalar header facts a swap needs, once its seal has been
/// validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BankFacts {
    id: BankId,
    generation: bank::Generation,
    run: RunId,
    region: JournalRegion,
    align: ProgramAlign,
    workflow_kind: u16,
    workflow_version: u16,
    input_schema: u16,
}

/// What [`read_bank`] learned about one bank.
///
/// Codex round 5 found that folding the third case into an immediate `Err` — as the two
/// rounds before it both did, in two different ways — refuses a page too eagerly: a bank
/// whose *own* header does not fit can still be safely ignored, if the *other* bank turns
/// out to be a fully validated, higher-generation candidate. Whether that is so is not
/// knowable until both banks have been looked at, so this carries the undecided case as a
/// value for [`select_bank`] to weigh rather than deciding it here. Codex round 9 found the
/// same shape of eagerness one step earlier: a bank whose header fails to *read* at all —
/// a device fault confined to that bank, once its seal already answered with a claimed
/// generation — was still an immediate `Err`, for the same reason and with the same fix.
enum BankRead<E> {
    /// A fully validated candidate, at the generation its own seal names.
    Found(BankFacts),
    /// Not a candidate at any generation — unsealed, or a header that fails to validate for
    /// a reason no larger a page would ever fix.
    Absent,
    /// Genuinely sealed, at `claimed_generation`, but `page` was not large enough to read
    /// and validate this bank's header. `needed` is the best figure available, rounded up to
    /// a whole read unit so that a caller retrying with exactly `needed` bytes gets a `page`
    /// this same rounding cannot then read back down to something smaller: the header's own
    /// checksum-protected declared length, where enough of the header was readable to
    /// checksum it, or just enough to read that much of the next attempt otherwise.
    Oversized {
        claimed_generation: bank::Generation,
        needed: usize,
    },
    /// Genuinely sealed, at `claimed_generation`, but the device itself refused this bank's
    /// header read — damage or a fault confined to this bank — or this firmware does not
    /// read the wire format version this bank's header declares. Neither is fixable by a
    /// larger `page`, unlike [`BankRead::Oversized`], so `error` carries forward unchanged
    /// as what a caller sees if nothing rescues the boot, rather than being reported as a
    /// size to grow.
    Unreadable {
        claimed_generation: bank::Generation,
        error: DriveError<E>,
    },
}

/// `len` rounded up to a whole number of `unit`s, or [`None`] on overflow.
///
/// `unit` is a power of two on every path that reaches here — [`Geometry`](waymaker_flash::storage::Geometry)
/// refuses anything else — so this is an add and a mask rather than a division, the same
/// shape `waymaker_flash::recovery`'s own private `round_up` is.
fn round_up_to_unit(len: usize, unit: u32) -> Option<usize> {
    let mask = (unit as usize).wrapping_sub(1);
    len.checked_add(mask).map(|sum| sum & !mask)
}

/// Reads bank `id`'s header and seal, and says what it is worth.
///
/// A bank whose seal does not validate or whose header does not decode is not a candidate at
/// any generation, exactly as [`bank::sealed_generation_with`] documents. This function only
/// adds the two reads that answer is read *from* rather than handed in, and keeps the scalar
/// fields [`Boundary::continue_as_new`](crate::Boundary::continue_as_new) needs later.
///
/// # Errors
///
/// Only a real device error reading this bank's *seal* — there is no claimed generation yet
/// to weigh a seal read's own failure against — or [`RecoveryError::PageTooSmall`] when
/// `page` cannot even hold this bank's seal. Neither this bank's header failing to fit a
/// large enough page, nor a device error reading it once its seal has already answered,
/// ever fails this call outright — both are [`BankRead`] values for [`select_bank`] to weigh
/// against the other bank instead.
fn read_bank<S, C>(
    layout: BankLayout,
    id: BankId,
    storage: &mut S,
    page: &mut [u8],
) -> Result<BankRead<S::Error>, DriveError<S::Error>>
where
    S: StableStorage,
    C: IntegrityCheck,
{
    let region = layout.bank(id);
    let payload_bytes = region.payload_bytes() as usize;
    // §10's own writer programs the seal at [`ProgramAlign::round_up`] of `SEAL_BYTES`
    // rather than `SEAL_BYTES` itself, and reads it back the same way here — a fixed
    // `[u8; SEAL_BYTES]` local is not a multiple of every geometry's read unit, and a
    // conforming `StableStorage` may refuse a read that is not one. The seal shares the one
    // page this call was handed, the same way `swap`'s own writer shares it for the seal it
    // programs, because a device's program unit has no upper bound this driver could give a
    // fixed local of its own.
    //
    // It is read, and decoded to the scalar `Seal` it names, *before* the header ever
    // touches `page` — Codex found both halves of what goes wrong when it is not. Decoded
    // first, an invalid seal answers [`BankRead::Absent`] here without ever looking at this
    // bank's header at all, so an unsealed bank — one a swap started staging and never
    // finished sealing, say — can carry any header length whatsoever without costing this
    // call anything: it was never a candidate, whatever its header says. And decoded to a
    // value rather than kept as bytes, the seal's own share of `page` is free the moment
    // this call is done with it, so the header read below gets the *whole* page rather than
    // what a reservation for the seal left of it — which is what lets a page sized to hold a
    // header exactly (and nothing besides, not even that bank's own seal) still read it.
    let seal_len = region.seal_bytes() as usize;
    let Some(seal_buf) = page.get_mut(..seal_len) else {
        return Err(DriveError::Recovery(RecoveryError::PageTooSmall {
            needed: seal_len,
        }));
    };
    storage
        .read(region.seal_offset(), seal_buf)
        .map_err(|error| DriveError::Recovery(RecoveryError::Storage(error)))?;
    let Ok(seal) = bank::decode_seal_with::<C>(seal_buf) else {
        return Ok(BankRead::Absent);
    };

    // A whole number of the device's own read units, exactly as `recovery::Scan::stage`
    // already rounds its page down — Codex found that a header read sized to whatever `page`
    // happened to leave, with no such rounding, could ask a conforming `StableStorage` for a
    // length it refuses outright even when `page` had ample room for the header.
    let read_unit = storage.geometry().read_size();
    let page_bytes = u32::try_from(page.len()).unwrap_or(u32::MAX);
    let capacity = page_bytes & !read_unit.wrapping_sub(1);
    let header_len = (capacity.min(region.payload_bytes())) as usize;
    let Some(header_buf) = page.get_mut(..header_len) else {
        // Unreachable: `header_len <= page_bytes == page.len()` by construction above.
        return Ok(BankRead::Absent);
    };
    // Codex found that a device error here — damage or a fault confined to this bank's
    // header, with its seal already read and validated above — was still an immediate
    // `Err`, aborting the whole boot before the other bank could be looked at. This bank's
    // claimed generation is already in hand, so the same deferral `BankRead::Oversized`
    // gives a header that fails to *decode* applies here to one that fails to *read* at all.
    if let Err(error) = storage.read(region.base(), header_buf) {
        return Ok(BankRead::Unreadable {
            claimed_generation: seal.generation,
            error: DriveError::Recovery(RecoveryError::Storage(error)),
        });
    }
    // A header whose self-declared length reaches past what fit in `page` — as opposed to
    // one this bank's own layout could never have held either, which `seal_for_with` below
    // still catches on its own — is not refused outright here any more than treated as "not
    // a candidate" outright. See this function's own `Errors` section and
    // `BankRead::Oversized`. `header_len_of_with` peeks at only the header's own
    // checksum-protected prefix, so it can answer even when the rest of the header — the
    // input, the trailer — never fit at all.
    if header_len < payload_bytes
        && matches!(
            bank::decode_header_with::<C>(header_buf),
            Err(DecodeError::Truncated)
        )
    {
        return Ok(match bank::header_len_of_with::<C>(header_buf) {
            // The prefix itself checksums cleanly: this bank really is sealed, and only
            // failed to fit because `page` did not reach far enough. `header_len_of_with`
            // answers the *unpadded* frame length, which need not itself be a multiple of
            // the read unit — Codex found that reporting it verbatim let a caller converge
            // on a length this same rounding then read down again, forever. Rounded up, a
            // caller that retries with exactly `needed` bytes gets a `page` this function's
            // own rounding cannot shrink further.
            Ok(needed) => BankRead::Oversized {
                claimed_generation: seal.generation,
                needed: round_up_to_unit(needed, read_unit)
                    .unwrap_or(payload_bytes)
                    .min(payload_bytes),
            },
            // Not even the checksum-protected prefix fit. Codex found that this bank's own
            // ceiling was a poor answer here too: a caller with a page shorter than
            // `HEADER_PREFIX_BYTES` learns nothing from being told to try a page several KiB
            // wide when a page merely wide enough for the *prefix* would already answer this
            // bank's real length on its very next call — the read that got this far already
            // holds the header's own declared length ready to be reported, one retry away. A
            // real, larger header cannot be ruled out either, so this is undecided rather
            // than absent.
            Err(DecodeError::Truncated) => BankRead::Oversized {
                claimed_generation: seal.generation,
                needed: round_up_to_unit(bank::HEADER_PREFIX_BYTES, read_unit)
                    .unwrap_or(payload_bytes)
                    .min(payload_bytes),
            },
            // Unreachable: `decode_header_with` just failed with `Truncated` on this exact
            // buffer, which means its own prefix check already passed (a magic, a checksum
            // and a version this firmware reads) before it ran out of room — so this
            // buffer's prefix cannot fail those same checks a second time here.
            Err(_) => BankRead::Absent,
        });
    }
    // Codex round 10 found that `seal_for_with` decoding this header failed the same way
    // for two different reasons this call folded into one: a header a bigger page or a
    // healthy device could never rescue, and a header declaring a *wire* format version
    // this firmware does not read (`reads_format_version`) — which a firmware elsewhere in
    // the same fleet, at a different point in a rollout, both wrote and would read back.
    // The second is exactly as deferrable as `BankRead::Oversized` and `Unreadable`: the
    // seal above already named a claimed generation, and a bigger page cannot fix it, but
    // the other bank fully validating at a higher generation still can.
    let expected = match bank::seal_for_with::<C>(header_buf, seal.generation) {
        Ok(expected) => expected,
        Err(DecodeError::UnsupportedFormatVersion) => {
            return Ok(BankRead::Unreadable {
                claimed_generation: seal.generation,
                error: DriveError::Recovery(RecoveryError::Decode(
                    DecodeError::UnsupportedFormatVersion,
                )),
            });
        }
        Err(_) => return Ok(BankRead::Absent),
    };
    if expected != seal {
        return Ok(BankRead::Absent);
    }
    let generation = seal.generation;
    // The seal already names this exact header's digest, so this decode cannot fail.
    // Treated as "not a candidate" rather than trusted, because the workspace denies both
    // `unwrap` and `panic!` and a decoder walking bytes off a device is the last place to
    // make an exception.
    let Ok(header) = bank::decode_header_with::<C>(header_buf) else {
        return Ok(BankRead::Absent);
    };
    let Ok(journal) = JournalRegion::of(layout, id, &header) else {
        return Ok(BankRead::Absent);
    };
    Ok(BankRead::Found(BankFacts {
        id,
        generation,
        run: header.run,
        region: journal,
        align: header.align,
        workflow_kind: header.workflow_kind,
        workflow_version: header.workflow_version,
        input_schema: header.input_schema,
    }))
}

/// The error a bank not fully validated would answer with alone: [`BankRead::Oversized`]'s
/// own [`RecoveryError::PageTooSmall`], or [`BankRead::Unreadable`]'s own device error
/// unchanged. [`BankRead::Found`] and [`BankRead::Absent`] need no such reduction, since
/// [`resolve_bank_read`] never reaches for this once either bank is one of those two.
fn undecided_error<E>(needed_or_error: Result<usize, DriveError<E>>) -> DriveError<E> {
    match needed_or_error {
        Ok(needed) => DriveError::Recovery(RecoveryError::PageTooSmall { needed }),
        Err(error) => error,
    }
}

/// Between two banks that are each undecidable — too large for `page`, or a device error
/// reading their header — the one whose seal claimed the *higher* generation is the one
/// worth reporting: if it turns out genuine once given whatever it is missing, the guarded
/// arms of [`resolve_bank_read`] already say the other bank's own answer never matters
/// again. Codex found that reporting whichever bank needed *more* room, or an error from
/// whichever bank happened to be checked first, could point a caller at a retired bank's
/// own stale trouble instead of the real authority's.
fn higher_claim<E>(
    generation_a: bank::Generation,
    error_a: Result<usize, DriveError<E>>,
    generation_b: bank::Generation,
    error_b: Result<usize, DriveError<E>>,
) -> DriveError<E> {
    if generation_a >= generation_b {
        undecided_error(error_a)
    } else {
        undecided_error(error_b)
    }
}

/// Weighs what [`read_bank`] learned about both banks into design document §10's selection
/// rule.
///
/// Split out of [`select_bank`] so each of the shapes two [`BankRead`]s can take is a
/// `match` arm rather than a nest of conditionals — see this function's own tests.
fn resolve_bank_read<E>(a: BankRead<E>, b: BankRead<E>) -> Result<BankFacts, DriveError<E>> {
    match (a, b) {
        (BankRead::Found(a), BankRead::Found(b)) => {
            match bank::select([Some(a.generation), Some(b.generation)]) {
                Authority::Bank { id: BankId::A, .. } => Ok(a),
                Authority::Bank { id: BankId::B, .. } => Ok(b),
                Authority::Ambiguous { .. } => Err(DriveError::AmbiguousAuthority),
                // Unreachable: both generations are `Some`, and `select` answers `Unsealed`
                // only when neither is.
                Authority::Unsealed => Err(DriveError::NoAuthoritativeBank),
            }
        }
        (BankRead::Found(facts), BankRead::Absent) | (BankRead::Absent, BankRead::Found(facts)) => {
            Ok(facts)
        }
        (BankRead::Absent, BankRead::Absent) => Err(DriveError::NoAuthoritativeBank),
        // A fully validated bank against one this call could not decide, for either reason
        // `BankRead` has: too large for `page`, or a device error reading its header. Codex
        // round 9 found the second case was still an eager `Err` — a header that fails to
        // *read* is exactly as safe to ignore as one that fails to *decode*, once the other
        // bank fully validates at a strictly higher generation.
        (
            BankRead::Found(facts),
            BankRead::Oversized {
                claimed_generation, ..
            }
            | BankRead::Unreadable {
                claimed_generation, ..
            },
        )
        | (
            BankRead::Oversized {
                claimed_generation, ..
            }
            | BankRead::Unreadable {
                claimed_generation, ..
            },
            BankRead::Found(facts),
        ) if claimed_generation < facts.generation => Ok(facts),
        (
            BankRead::Oversized {
                claimed_generation: generation_a,
                needed: needed_a,
            },
            BankRead::Oversized {
                claimed_generation: generation_b,
                needed: needed_b,
            },
        ) => Err(higher_claim(
            generation_a,
            Ok(needed_a),
            generation_b,
            Ok(needed_b),
        )),
        (
            BankRead::Unreadable {
                claimed_generation: generation_a,
                error: error_a,
            },
            BankRead::Unreadable {
                claimed_generation: generation_b,
                error: error_b,
            },
        ) => Err(higher_claim(
            generation_a,
            Err(error_a),
            generation_b,
            Err(error_b),
        )),
        (
            BankRead::Oversized {
                claimed_generation: generation_a,
                needed,
            },
            BankRead::Unreadable {
                claimed_generation: generation_b,
                error,
            },
        )
        | (
            BankRead::Unreadable {
                claimed_generation: generation_b,
                error,
            },
            BankRead::Oversized {
                claimed_generation: generation_a,
                needed,
            },
        ) => Err(higher_claim(
            generation_a,
            Ok(needed),
            generation_b,
            Err(error),
        )),
        (BankRead::Oversized { needed, .. }, _) | (_, BankRead::Oversized { needed, .. }) => {
            Err(DriveError::Recovery(RecoveryError::PageTooSmall { needed }))
        }
        (BankRead::Unreadable { error, .. }, _) | (_, BankRead::Unreadable { error, .. }) => {
            Err(error)
        }
    }
}

/// Design document §10's selection rule, over a real device: which bank a
/// [`Driver::at_bank`] boots, and the facts it needs from it.
fn select_bank<S, C>(
    layout: BankLayout,
    storage: &mut S,
    page: &mut [u8],
) -> Result<BankFacts, DriveError<S::Error>>
where
    S: StableStorage,
    C: IntegrityCheck,
{
    let a = read_bank::<S, C>(layout, BankId::A, storage, page)?;
    let b = read_bank::<S, C>(layout, BankId::B, storage, page)?;
    resolve_bank_read(a, b)
}

/// Refuses a boot whose workflow does not match what bank `id`'s own header declares.
///
/// [`begin`] makes the same comparison against the *journal*'s `RunStarted` record, but an
/// erased journal has none — it writes one from `workflow.identity()` alone, with nothing
/// else to check it against. A bank a swap just installed is exactly that: its header
/// durably records the workflow and input `continue_as_new` was asked for, and until a
/// first boot writes the opening record, `begin` cannot enforce it. This is the same
/// comparison, against the header instead of the journal, so that recording stays enforced
/// rather than becoming unreachable the moment it is written.
///
/// Kind and input only — never the header's own `workflow_version`, which is `begin`'s to
/// admit instead, and differently depending on what it finds. This runs on *every* boot of a
/// bank-pointed driver, not only a bank's first, and a run already under way has its version
/// recorded where it matters: in the journal's own `RunStarted`, which `begin` admits
/// directly, ignoring the header entirely. The header's version is a fact about the swap
/// that installed the bank, not about the run, and it never changes again — an image whose
/// admitted range has moved on from it while still admitting the run's own recorded version
/// would otherwise be refused over a number nothing here still depends on. For the one case
/// this function exists for — an erased journal, nothing recorded yet — the header's version
/// is the *only* record of what this run was meant to be, and `begin` admits it before ever
/// writing `identity.versions.current()` over it: Codex found that skipping this let an
/// older, rolled-back image silently claim authorship of a run a newer image's swap had
/// already named, the moment before that newer image's own first boot would have recorded
/// it properly.
fn verify_header_identity<S, C, W>(
    layout: BankLayout,
    id: BankId,
    storage: &mut S,
    page: &mut [u8],
    workflow: &W,
) -> Result<(), DriveError<S::Error>>
where
    S: StableStorage,
    C: IntegrityCheck,
    W: Workflow + ?Sized,
{
    let region = layout.bank(id);
    // Rounded down to a whole read unit, exactly as `read_bank`'s own header read is —
    // the same fix, for the same reason: a length `read_bank` never needed to round
    // because `select_bank` already read this bank once, but this call reads it again
    // with its own arithmetic and a conforming `StableStorage` does not know the two
    // calls agree.
    let read_unit = storage.geometry().read_size();
    let page_bytes = u32::try_from(page.len()).unwrap_or(u32::MAX);
    let capacity = page_bytes & !read_unit.wrapping_sub(1);
    let header_len = (capacity.min(region.payload_bytes())) as usize;
    let Some(header_buf) = page.get_mut(..header_len) else {
        // Unreachable: `header_len <= page_bytes == page.len()` by construction above.
        return Err(DriveError::NoAppendPoint);
    };
    storage
        .read(region.base(), header_buf)
        .map_err(|error| DriveError::Recovery(RecoveryError::Storage(error)))?;
    let Ok(header) = bank::decode_header_with::<C>(header_buf) else {
        // Unreachable: `select_bank` already decoded this exact bank successfully.
        return Err(DriveError::NoAppendPoint);
    };
    let identity = workflow.identity();
    let (workflow_kind, input) = (header.workflow_kind, header.input);
    if workflow_kind != identity.kind || input != identity.input {
        return Err(DriveError::NotThisWorkflow);
    }
    Ok(())
}

/// Design document §06 steps 1 and 2: the run's own record, from history or newly written.
///
/// Answers the version the run is *recorded* at, which is what
/// [`Boundary::recorded_version`] hands the workflow: the recorded one where history held a
/// `RunStarted`, and the one this image writes where it did not.
fn begin<S, C, W>(
    source: &mut Source<'_, S, C>,
    machine: &mut ReplayMachine,
    workflow: &W,
    page: &mut [u8],
    reserve: Reserve,
    header_version: Option<u16>,
) -> Result<u16, DriveError<S::Error>>
where
    S: StableStorage,
    C: IntegrityCheck,
    W: Workflow + ?Sized,
{
    let identity = workflow.identity();
    {
        let Source::Scanning(recovery) = &mut *source else {
            return Err(DriveError::NoAppendPoint);
        };
        match recovery.next(&mut *page) {
            Some(Ok(record)) => {
                let RecordRef::RunStarted {
                    workflow_kind,
                    workflow_version,
                    input,
                } = record
                else {
                    // A journal whose first record is not a `RunStarted` is history no
                    // execution could have produced. The cursor would refuse it too; saying
                    // so here keeps the diagnosis at the record that caused it.
                    return Err(DriveError::Kernel(KernelError::MalformedHistory));
                };
                if workflow_kind != identity.kind || input != identity.input {
                    return Err(DriveError::NotThisWorkflow);
                }
                // §08's first two rules. The version is *admitted* rather than compared:
                // an image that demanded equality would refuse every run in flight the
                // moment the binary changed, which is the opposite of "existing runs must
                // continue under compatible code for their recorded version". A version
                // outside the range is `IncompatibleWorkflow` and never a best-effort
                // replay — and never `NotThisWorkflow`, which says the journal belongs to
                // another workflow rather than to another release of this one.
                identity
                    .versions
                    .admits(workflow_version)
                    .map_err(DriveError::Kernel)?;
                return machine
                    .advance(record)
                    .map(|_| ())
                    .map_err(DriveError::Kernel)
                    .map(|()| workflow_version);
            }
            Some(Err(error)) => return Err(DriveError::Recovery(error)),
            None => {}
        }
    }

    // An erased journal. The run has to be recorded before anything can be scheduled
    // against it, and this is the one record the driver writes without the kernel asking.
    //
    // `header_version` is `Some` only for a bank-pointed driver, and only there does a
    // still-empty journal have anything else durable to consult at all: the header a swap
    // wrote. Codex found that skipping this let an older, rolled-back image boot such a
    // bank on kind and input alone and silently record its own current version over one a
    // newer image had already named — the header admits itself once a `RunStarted` exists
    // to make that irreversible, but until then it is the one thing here that still depends
    // on it.
    if let Some(header_version) = header_version {
        identity
            .versions
            .admits(header_version)
            .map_err(DriveError::Kernel)?;
    }
    let version = identity.versions.current();
    let record = RecordRef::RunStarted {
        workflow_kind: identity.kind,
        workflow_version: version,
        input: identity.input,
    };
    open(source, reserve)?;
    write(source, &record, page)?;
    machine
        .advance(record)
        .map(|_| ())
        .map_err(DriveError::Kernel)
        .map(|()| version)
}

/// Where the next record comes from, and where the next one goes.
///
/// One value rather than two fields, because the two are exclusive by construction:
/// [`Journal::after`] consumes the [`Recovery`] that positioned it, so a driver cannot hold
/// a reader and a writer over one region at once.
///
/// # Why the device lives here rather than in a field of `Context`
///
/// Issue [#84](https://github.com/madmax983/waymaker/issues/84) has [`Recovery`] hold the
/// device for the whole of a scan rather than take it fresh at every call. That is right for
/// the scan and wrong for a `Context` that also wants the device for §07's writer: a struct
/// cannot have one field borrow another field of the same instance. So the device travels
/// with whichever state is using it — inside the [`Recovery`] while
/// [`Scanning`](Self::Scanning), inside this enum directly once there is a writer or none at
/// all — and [`Context`] never holds it on its own.
enum Source<'storage, S, C: IntegrityCheck> {
    /// Replaying committed history.
    Scanning(Recovery<'storage, S, C>),
    /// History is exhausted; the journal is being extended, through §10's gate.
    Writing(&'storage mut S, Reserved<C>),
    /// The scan ended somewhere a writer cannot be opened at, or the writer is out for one
    /// effect's protocol — see [`Context::dispatch`].
    Spent(&'storage mut S),
    /// A transient placeholder [`mem::replace`] needs while a state above is decided.
    ///
    /// Never observed by a caller: every function below that matches on this treats it the
    /// way [`Spent`](Self::Spent) is treated once its device has been taken for good.
    Taken,
}

/// Turns a finished scan into a gated writer, or refuses.
fn open<E, S: StableStorage, C: IntegrityCheck>(
    source: &mut Source<'_, S, C>,
    reserve: Reserve,
) -> Result<(), DriveError<E>> {
    match mem::replace(source, Source::Taken) {
        // `None` is the scan that stopped at damage, at an unsealed frame, or was
        // abandoned. §14: the recovered prefix stands, and nothing may be appended after it.
        Source::Scanning(recovery) => {
            let (storage, journal) = Journal::after_taking_storage(recovery);
            let Some(journal) = journal else {
                *source = Source::Spent(storage);
                return Err(DriveError::NoAppendPoint);
            };
            // §10's gate, taken here so that every append below goes through it. A journal
            // whose region cannot hold the reserve is refused before a record is staged.
            let reserved = match Reserved::over(journal, reserve) {
                Ok(reserved) => reserved,
                Err(error) => {
                    *source = Source::Spent(storage);
                    return Err(DriveError::Reserve(error));
                }
            };
            *source = Source::Writing(storage, reserved);
            Ok(())
        }
        Source::Writing(storage, reserved) => {
            *source = Source::Writing(storage, reserved);
            Ok(())
        }
        Source::Spent(storage) => {
            *source = Source::Spent(storage);
            Err(DriveError::NoAppendPoint)
        }
        Source::Taken => Err(DriveError::NoAppendPoint),
    }
}

/// Takes the writer and the device out of `source`, or refuses.
///
/// §07's protocol consumes the writer for the length of one effect, which is what makes a
/// second appender over one journal unrepresentable. `source` is left holding only the
/// device, in [`Source::Spent`], until [`Context::dispatch`] puts the writer back.
const fn take<'storage, E, S, C: IntegrityCheck>(
    source: &mut Source<'storage, S, C>,
) -> Result<(&'storage mut S, Reserved<C>), DriveError<E>> {
    match mem::replace(source, Source::Taken) {
        Source::Writing(storage, reserved) => Ok((storage, reserved)),
        Source::Scanning(recovery) => {
            let storage = recovery.into_storage();
            *source = Source::Spent(storage);
            Err(DriveError::NoAppendPoint)
        }
        Source::Spent(storage) => {
            *source = Source::Spent(storage);
            Err(DriveError::NoAppendPoint)
        }
        Source::Taken => Err(DriveError::NoAppendPoint),
    }
}

/// Takes the device out of `source`, which must be [`Source::Spent`] — the state §07's
/// protocol leaves it in while an effect is outstanding, whether dispatched immediately or
/// split across [`Boundary::schedule`](crate::Boundary::schedule) and
/// [`Boundary::resolve`](crate::Boundary::resolve). Refuses, restoring what was there,
/// otherwise.
const fn spent<'storage, E, S, C: IntegrityCheck>(
    source: &mut Source<'storage, S, C>,
) -> Result<&'storage mut S, DriveError<E>> {
    match mem::replace(source, Source::Taken) {
        Source::Spent(storage) => Ok(storage),
        other => {
            *source = other;
            Err(DriveError::NoAppendPoint)
        }
    }
}

/// The record at the cursor's position, or [`Next::EndOfHistory`].
///
/// The one place the scan and the kernel are kept in step: an exhausted scan becomes a
/// writer here, so the transition happens exactly once and at the moment history runs out.
fn peek<'page, S, C>(
    source: &mut Source<'_, S, C>,
    page: &'page mut [u8],
    reserve: Reserve,
) -> Result<Next<'page>, DriveError<S::Error>>
where
    S: StableStorage,
    C: IntegrityCheck,
{
    match &mut *source {
        // A writer is open, so history really has run out.
        Source::Writing(..) => return Ok(Next::EndOfHistory),
        // A scan that ended somewhere no writer could be opened at. `EndOfHistory` is the
        // input that produces `Intent::Schedule` and `Resolve::Redeliver` — the two rows
        // that lead to dispatch — so answering it here would offer the world an effect this
        // bank can never record. Unreachable today because every `open` failure sets `stop`
        // first; spelled as the refusal it has to be rather than left to the call graph.
        Source::Spent(_) | Source::Taken => return Err(DriveError::NoAppendPoint),
        Source::Scanning(recovery) => match recovery.next(page) {
            Some(Ok(record)) => return Ok(Next::Record(record)),
            Some(Err(error)) => return Err(DriveError::Recovery(error)),
            None => {}
        },
    }
    open(source, reserve)?;
    Ok(Next::EndOfHistory)
}

/// Refuses a committed record that follows a terminal one.
///
/// §08 row 5 is "return the recorded outcome and poll no further", and a driver that stopped
/// at the terminal record without asking would accept history no execution could have
/// produced: `ReplayCursor` refuses a record after a terminal one, and this is the only place
/// the driver can put that question to it — the machine is never handed the record, because
/// the boot is over.
///
/// # What it costs, and what it does not refuse
///
/// One read, and on a scan that reaches erased media the walk ADR 0018 names: a bank of 64
/// KiB with a 512-byte page is 128 reads. It is paid only when a *replayed* run turns out to
/// be finished — a run that ends in this boot has a writer open, and a writer knows the scan
/// is behind it.
///
/// A frame that fails to **decode** is not refused. §14 is explicit that a damaged frame is
/// ignored and the previous history prefix wins, and for a finished run that prefix is the
/// whole run. Only a *valid sealed* record after the end is impossible.
///
/// Every other recovery failure is. A read that failed, a device that is not the one the
/// region was validated against, a page too small for the next record: none of them says
/// "nothing follows", they say the driver could not find out. Reporting a clean finish on
/// one of those is the same mistake as reporting it on a record that does follow.
fn nothing_follows<S, C>(
    source: &mut Source<'_, S, C>,
    page: &mut [u8],
) -> Result<(), DriveError<S::Error>>
where
    S: StableStorage,
    C: IntegrityCheck,
{
    match source {
        // The scan ran to erased media before either of these existed, so nothing follows.
        Source::Writing(..) | Source::Spent(_) | Source::Taken => Ok(()),
        Source::Scanning(recovery) => match recovery.next(page) {
            Some(Ok(_)) => Err(DriveError::HistoryContinues),
            Some(Err(RecoveryError::Decode(_))) | None => Ok(()),
            Some(Err(error)) => Err(DriveError::Recovery(error)),
        },
    }
}

/// Design document §07's three steps, for one record.
fn write<S, C>(
    source: &mut Source<'_, S, C>,
    record: &RecordRef<'_>,
    page: &mut [u8],
) -> Result<(), DriveError<S::Error>>
where
    S: StableStorage,
    C: IntegrityCheck,
{
    let Source::Writing(storage, reserved) = source else {
        return Err(DriveError::NoAppendPoint);
    };
    reserved
        .stage(*storage, record, page)
        .map_err(|error| match error {
            ReservedError::Capacity(refusal) => DriveError::Capacity(refusal),
            ReservedError::Append(error) => DriveError::Append(error),
        })?
        .payload_barrier()
        .map_err(DriveError::Append)?
        .commit()
        .map_err(DriveError::Append)?;
    Ok(())
}

/// Copies `outcome`'s bytes into `into`, and says which outcome it was.
///
/// `bound` is what the run declared this kind of payload may be worth — §10's
/// `effect_result_bytes` for an effect outcome, `terminal_bytes` for a terminal record. The
/// limit is the narrower of that and the caller's buffer, and it is the whole of the
/// one-bound rule on the *reading* side: a roomy buffer must not let a record the run never
/// priced reach the workflow. Without it a firmware that lowered its bounds would replay a
/// journal written under the old ones and hand back a payload it would refuse to write.
fn store<E>(
    outcome: Outcome<'_>,
    into: &mut [u8],
    bound: u16,
) -> Result<(Conclusion, usize), DriveError<E>> {
    let (conclusion, bytes) = match outcome {
        Outcome::Completed(bytes) => (Conclusion::Completed, bytes),
        Outcome::Failed(bytes) => (Conclusion::Failed, bytes),
    };
    let available = into.len().min(usize::from(bound));
    let Some(target) = into.get_mut(..bytes.len()) else {
        return Err(DriveError::ResultTooLong {
            produced: bytes.len(),
            available,
        });
    };
    if bytes.len() > available {
        return Err(DriveError::ResultTooLong {
            produced: bytes.len(),
            available,
        });
    }
    target.copy_from_slice(bytes);
    Ok((conclusion, bytes.len()))
}

/// The outcome a terminal record holds, or [`None`] if it is not a terminal record.
const fn recorded(record: RecordRef<'_>) -> Option<Outcome<'_>> {
    match record {
        RecordRef::RunCompleted { result } => Some(Outcome::Completed(result)),
        RecordRef::RunFailed { error } => Some(Outcome::Failed(error)),
        RecordRef::RunStarted { .. }
        | RecordRef::EffectScheduled { .. }
        | RecordRef::EffectCompleted { .. }
        | RecordRef::EffectFailed { .. }
        | RecordRef::TimerScheduled { .. }
        | RecordRef::TimerFired { .. }
        | RecordRef::VersionMarker { .. } => None,
    }
}

/// The terminal record `outcome` is written as.
const fn terminal(outcome: Outcome<'_>) -> RecordRef<'_> {
    match outcome {
        Outcome::Completed(result) => RecordRef::RunCompleted { result },
        Outcome::Failed(error) => RecordRef::RunFailed { error },
    }
}

/// Why the driver stopped, recorded at the moment it decided.
///
/// Kept rather than returned, because the workflow is on the stack above and the only thing
/// [`Boundary::call`] may hand it is [`Suspended`].
enum Stop<E> {
    /// An activity was not ready.
    Waiting(EffectId),
    /// A deadline has not passed, with the clock its remaining ticks are counted in.
    WaitingUntil(EffectId, ClockKind, u64),
    /// History holds a terminal record.
    Finished {
        /// Which one.
        conclusion: Conclusion,
        /// How much of the result buffer it filled.
        result_len: usize,
    },
    /// Something was refused.
    Failed(DriveError<E>),
    /// `continue_as_new` installed a new run under this id.
    Migrated(RunId),
}

/// The driver, as the workflow sees it.
struct Context<'a, S: StableStorage, A: Activities + Clocks, C: IntegrityCheck> {
    activities: &'a mut A,
    machine: &'a mut ReplayMachine,
    page: &'a mut [u8],
    result: &'a mut [u8],
    source: Source<'a, S, C>,
    reserve: Reserve,
    /// [`Some`] only for a driver built with [`Driver::at_bank`]. [`None`] here is what
    /// makes [`Boundary::continue_as_new`](crate::Boundary::continue_as_new) refuse for a
    /// driver [`Driver::new`] pointed at a fixed region.
    bank: Option<BankContext>,
    stop: Option<Stop<S::Error>>,
    /// The effect §07 step 3 committed, while a caller performs step 4 for itself.
    ///
    /// [`Boundary::call`] never uses it: that path holds the value on its own stack for
    /// the length of one call. This is where it waits when the two halves are split, and
    /// it holds the writer, so a run with an effect in flight still has no appender.
    pending: Option<Dispatchable<C>>,
    /// The recorded versions this image can replay, from the workflow's own identity.
    versions: VersionRange,
    /// The version the run's `RunStarted` record holds. §08's first rule.
    recorded_version: u16,
}

impl<S: StableStorage, A: Activities + Clocks, C: IntegrityCheck> Context<'_, S, A, C> {
    /// What the boot amounts to, once the workflow has returned.
    ///
    /// The driver's own stop outranks whatever the workflow returned. A workflow that
    /// swallowed a [`Suspended`] and carried on has not made the run go further; it has
    /// only stopped saying that it stopped.
    fn conclude(
        self,
        ended: Result<Outcome<'_>, Suspended>,
    ) -> Result<Progress, DriveError<S::Error>> {
        let Self {
            machine,
            page,
            result,
            mut source,
            reserve,
            stop,
            pending,
            ..
        } = self;
        match stop {
            Some(Stop::Failed(error)) => return Err(error),
            Some(Stop::Waiting(id)) => return Ok(Progress::Waiting { id }),
            Some(Stop::WaitingUntil(id, clock_kind, remaining)) => {
                return Ok(Progress::WaitingUntil {
                    id,
                    clock_kind,
                    remaining,
                });
            }
            // `swap_in` already asked `nothing_follows` before it touched the device —
            // asking again here would be asking it of a bank this call just reclaimed.
            Some(Stop::Migrated(run)) => return Ok(Progress::Migrated { run }),
            Some(Stop::Finished {
                conclusion,
                result_len,
            }) => {
                // §08 row 5 was reached at an effect boundary, so the terminal record is
                // already consumed and nothing may follow it.
                nothing_follows(&mut source, page)?;
                return Ok(Progress::Finished {
                    conclusion,
                    result_len,
                });
            }
            None => {}
        }

        // A caller that split §07 in two, took the identity, and never resolved it. The
        // writer is inside the outstanding effect, so every path below reaches `peek`,
        // finds `Source::Spent`, and reports `NoAppendPoint` — the refusal reserved for a
        // bank that can never be appended to again. Read before `ended`, because what the
        // workflow returned changes which answer is right and neither of them is that one.
        if let Some(outstanding) = pending {
            let Ok(_ended) = ended else {
                // The caller stopped. The schedule record is committed, so the effect is
                // outstanding and the next boot redelivers it — which is what
                // `Performed::Pending` reports on the undivided path. The knowledge is here
                // because only the driver holds the identity.
                return Ok(Progress::Waiting {
                    id: outstanding.intent().id(),
                });
            };
            // The caller says the run is over with an effect outstanding. §08 has no edge
            // from an unresolved effect to a terminal record, so this is refused either
            // way; it is named for what it is rather than for the media.
            return Err(DriveError::EffectOutstanding);
        }

        let Ok(outcome) = ended else {
            // Unreachable: every `Suspended` this crate hands out is recorded above first,
            // and a caller that stopped with an effect outstanding is the arm above.
            // Refused rather than panicked, because the workspace denies both.
            return Err(DriveError::Kernel(KernelError::NondeterministicWorkflow));
        };

        // Bound and collapsed in one statement: a `Next` that stayed alive across the
        // match would hold the page borrow into the arm that has to write through it.
        let terminated = match peek(&mut source, &mut *page, reserve)? {
            // §08 row 5 reached outside an effect boundary: the workflow and history agree
            // that the run is over, and history is what the caller is told.
            Next::Record(record) => {
                let Some(recorded) = recorded(record) else {
                    // The workflow ended while history holds records it never asked for.
                    // §08's divergence, met at the one place the divergence check cannot
                    // run — there is no request to compare against.
                    return Err(DriveError::HistoryContinues);
                };
                machine.advance(record).map_err(DriveError::Kernel)?;
                Some(store(recorded, result, reserve.bounds().terminal_bytes)?)
            }
            Next::EndOfHistory => None,
        };

        let (conclusion, result_len) = if let Some(recorded) = terminated {
            recorded
        } else {
            // Copied first, and the record written only if it fit. The other order commits a
            // terminal record and then refuses the boot that wrote it, so a run that really
            // completed reports `ResultTooLong` on this boot and on every boot after it.
            // Nothing else in this crate is ordered that way: `dispatch` measures an
            // activity's answer before it records one, for the same reason.
            let recorded = store(outcome, result, reserve.bounds().terminal_bytes)?;
            let record = terminal(outcome);
            // Advanced before it is written. The schedule and outcome records were already
            // authorised — `intent()` and `outcome()` answered for them — and this one has
            // nothing that authorised it, so the kernel is asked before media is touched.
            // §08 has no edge from an unresolved effect to a terminal record, so this is
            // where a run that ended with one outstanding is refused rather than recorded.
            machine.advance(record).map_err(DriveError::Kernel)?;
            write(&mut source, &record, page)?;
            recorded
        };
        // Both branches above, in one place: the run that ended in this boot and the run
        // whose terminal record history already held. Free on the first — a writer is open,
        // so the scan is behind it — and one read on the second.
        nothing_follows(&mut source, page)?;
        Ok(Progress::Finished {
            conclusion,
            result_len,
        })
    }
}

/// What the intent half of design document §08's table decided.
///
/// A value with no lifetime, on purpose: it is what collapses the scratch-page borrow the
/// kernel's answer was read through, so the same page can be written through below.
enum Half<E> {
    /// Row 3. Nothing is committed yet.
    Schedule(EffectId),
    /// Rows 1 and 2. The intent is committed; the outcome half decides which.
    Recorded,
    /// Row 5. History holds a terminal record, already copied into the result buffer.
    Finished(Conclusion, usize),
    /// Row 4, and everything else the kernel or the media refused.
    Failed(DriveError<E>),
}

/// What the outcome half decided.
///
/// Named for the answer rather than for the step, because [`Resolved`](crate::Resolved) is
/// §07 step 7's own value and two `Resolved`s in one file is one too many.
enum Answer<E> {
    /// Row 1, already copied into the result buffer.
    Replayed(Conclusion, usize),
    /// Row 2. Dispatch again, under the identity history recorded.
    Redeliver(EffectId),
    /// Refused.
    Failed(DriveError<E>),
}

/// What one boundary needs next, once every borrow of the page has been collapsed.
enum Decision<C: IntegrityCheck> {
    /// History answered. The bytes are in the result buffer.
    Replayed(Conclusion, usize),
    /// The world has to answer. §07 step 4 takes the value this carries and nothing else.
    Dispatch(Dispatchable<C>),
    /// The run stops here; `Context::stop` says why.
    Stop,
}

/// §02 decision 3, as §07 steps 1, 2 and 3.
///
/// Split out of [`Context::decide`] for clippy's line budget, and because this is the half
/// §02 decision 3 is about: the writer leaves `source` here and comes back only in
/// [`Context::dispatch`], so a run with an effect in flight has no appender.
fn scheduling<S, C>(
    source: &mut Source<'_, S, C>,
    machine: &mut ReplayMachine,
    page: &mut [u8],
    stop: &mut Option<Stop<S::Error>>,
    id: EffectId,
    request: EffectRequest,
) -> Decision<C>
where
    S: StableStorage,
    C: IntegrityCheck,
{
    let (storage, writer) = match take(source) {
        Ok(pair) => pair,
        Err(error) => {
            *stop = Some(Stop::Failed(error));
            return Decision::Stop;
        }
    };
    let scheduled = Effect::over(id.run, writer).schedule(&mut *storage, id.seq, request, page);
    // The device goes back into `source` regardless: it is still there whether or not the
    // schedule succeeded, and `take` already left nowhere else for it to live.
    *source = Source::Spent(storage);
    let Scheduled { dispatch, record } = match scheduled {
        Ok(scheduled) => scheduled,
        Err(error) => {
            *stop = Some(Stop::Failed(error));
            return Decision::Stop;
        }
    };
    if let Err(error) = machine.advance(record) {
        *stop = Some(Stop::Failed(DriveError::Kernel(error)));
        return Decision::Stop;
    }
    Decision::Dispatch(dispatch)
}

impl<S: StableStorage, A: Activities + Clocks, C: IntegrityCheck> Context<'_, S, A, C> {
    /// The kernel's answer for one boundary, with nothing borrowed from it.
    ///
    /// Every arm is a row of design document §08's table. A refusal is recorded in `stop`
    /// rather than returned, because the only thing [`Boundary::call`] may hand a workflow
    /// is [`Suspended`].
    fn decide(&mut self, kind: ActivityKind, input: &[u8]) -> Decision<C> {
        let Self {
            machine,
            page,
            result,
            source,
            reserve,
            stop,
            pending,
            ..
        } = self;

        // A boundary the driver has already stopped at answers nothing and touches nothing.
        if stop.is_some() {
            return Decision::Stop;
        }
        // A caller that split §07 in two and then took another boundary. The writer is
        // inside the outstanding effect, so every path below would reach `peek` and report
        // `NoAppendPoint` — the one refusal reserved for a bank that can never be appended
        // to again. Named for what it is instead.
        if pending.is_some() {
            *stop = Some(Stop::Failed(DriveError::EffectOutstanding));
            return Decision::Stop;
        }

        let Ok(input_len) = u16::try_from(input.len()) else {
            *stop = Some(Stop::Failed(DriveError::InputTooLong {
                bytes: input.len(),
            }));
            return Decision::Stop;
        };
        // The check this driver seals with, not the shipped one. `waymaker-flash`'s own
        // documentation says why: a build that sealed frames with one check and digested
        // activity inputs with another records a digest no replay of it can reproduce, and
        // §08's divergence comparison then fails on every effect.
        let input_crc = frame::input_digest_with::<C>(input);
        let request = EffectRequest {
            kind,
            input_len,
            input_crc,
        };

        let half = match peek(source, page, *reserve) {
            Err(error) => Half::Failed(error),
            Ok(next) => match machine.intent(request, next) {
                Ok(Intent::Schedule { id }) => Half::Schedule(id),
                Ok(Intent::Recorded { .. }) => Half::Recorded,
                // A terminal record, so the bound is the terminal one.
                Ok(Intent::Finished { outcome }) => {
                    match store(outcome, result, reserve.bounds().terminal_bytes) {
                        Ok((conclusion, len)) => Half::Finished(conclusion, len),
                        Err(error) => Half::Failed(error),
                    }
                }
                Err(error) => Half::Failed(DriveError::Kernel(error)),
            },
        };

        match half {
            Half::Failed(error) => {
                *stop = Some(Stop::Failed(error));
                Decision::Stop
            }
            Half::Finished(conclusion, result_len) => {
                *stop = Some(Stop::Finished {
                    conclusion,
                    result_len,
                });
                Decision::Stop
            }
            // §02 decision 3, as §07 steps 1 to 3: the intent crosses two barriers before
            // the effect, and the value step 4 needs does not exist until they returned.
            Half::Schedule(id) => scheduling(source, machine, page, stop, id, request),
            Half::Recorded => {
                let answer = match peek(source, page, *reserve) {
                    Err(error) => Answer::Failed(error),
                    Ok(next) => match machine.outcome(next) {
                        Ok(Resolve::Replayed { outcome, .. }) => {
                            match store(outcome, result, reserve.bounds().effect_result_bytes) {
                                Ok((conclusion, len)) => Answer::Replayed(conclusion, len),
                                Err(error) => Answer::Failed(error),
                            }
                        }
                        Ok(Resolve::Redeliver { id }) => Answer::Redeliver(id),
                        Err(error) => Answer::Failed(DriveError::Kernel(error)),
                    },
                };
                match answer {
                    Answer::Replayed(conclusion, len) => Decision::Replayed(conclusion, len),
                    // §07 steps 1 to 3 completed in an earlier boot: committed history holds
                    // the schedule record and no outcome, which is what the kernel just said.
                    Answer::Redeliver(id) => match take(source) {
                        Ok((storage, writer)) => {
                            // Redelivery mints no new record, so nothing here writes — but
                            // the device is still with this run for as long as the effect
                            // it is about to dispatch is outstanding, exactly as the
                            // scheduled path leaves it in `Spent` for `Context::dispatch`.
                            *source = Source::Spent(storage);
                            Decision::Dispatch(Effect::over(id.run, writer).redelivering(id.seq))
                        }
                        Err(error) => {
                            *stop = Some(Stop::Failed(error));
                            Decision::Stop
                        }
                    },
                    Answer::Failed(error) => {
                        *stop = Some(Stop::Failed(error));
                        Decision::Stop
                    }
                }
            }
        }
    }

    /// §07 steps 4 to 7: the world performs the effect, and its answer becomes history
    /// before the workflow sees it.
    ///
    /// The [`Dispatchable`] is the whole of §02 decision 3 here. It is a value only a
    /// completed step 3 produces, so this function cannot be reached with an intent that is
    /// not durable.
    fn dispatch(
        &mut self,
        dispatchable: Dispatchable<C>,
        kind: ActivityKind,
        input: &[u8],
    ) -> Result<Outcome<'_>, Suspended> {
        let Self {
            activities,
            machine,
            page,
            result,
            source,
            reserve,
            stop,
            ..
        } = self;

        let intent = dispatchable.intent();
        let available = result.len();
        let bound = usize::from(reserve.bounds().effect_result_bytes);
        // Unreachable: `boot` refuses a narrower buffer before anything is written. Spelled
        // as the refusal it has to be, because the workspace denies a panic.
        let Some(out) = result.get_mut(..bound) else {
            *stop = Some(Stop::Failed(DriveError::ResultBufferTooSmall {
                needed: bound,
                available,
            }));
            return Err(Suspended::NEW);
        };

        // §07 step 4. Matched once. A second match would need an arm for `Pending`, which
        // cannot be reached here — and an unreachable arm that picks a record kind is a wrong
        // default waiting for the day it is reachable.
        let answered = match activities.perform(intent, kind, input, out) {
            Performed::Completed(produced) => Some((produced, false)),
            Performed::Failed(produced) => Some((produced, true)),
            Performed::Exhausted => None,
            Performed::Pending => {
                *stop = Some(Stop::Waiting(intent.id()));
                return Err(Suspended::NEW);
            }
        };

        let resolution = match answered {
            None => Resolution::Exhausted,
            // An activity that reported more than the buffer it was handed said the same
            // thing `Performed::Exhausted` says, in the wrong words: the answer does not fit.
            // Recorded as exhausted rather than refused, because a refusal here strands the
            // run for ever — the schedule record is already committed, §08 has no edge from
            // an unresolved effect to a terminal record, and every later boot meets the same
            // answer. That is the defect this whole change exists to remove, and it was
            // still reachable three lines from the fix.
            Some((produced, _)) if produced > bound => Resolution::Exhausted,
            Some((produced, failed)) => {
                let Some(bytes) = out.get(..produced) else {
                    // Unreachable: the arm above caught it. Spelled as the refusal it has to
                    // be, because the workspace denies a panic.
                    *stop = Some(Stop::Failed(DriveError::ResultTooLong {
                        produced,
                        available: bound,
                    }));
                    return Err(Suspended::NEW);
                };
                if failed {
                    Resolution::Failed(bytes)
                } else {
                    Resolution::Completed(bytes)
                }
            }
        };

        // §07 steps 5, 6 and 7. The writer comes back only here, so a run with an effect in
        // flight has no appender. The device comes back from the same place it was parked
        // when the effect was scheduled — see `Source`'s own documentation.
        let storage = match spent(source) {
            Ok(storage) => storage,
            Err(error) => {
                *stop = Some(Stop::Failed(error));
                return Err(Suspended::NEW);
            }
        };
        let Resolved {
            next,
            record,
            outcome,
        } = match dispatchable.resolve(&mut *storage, resolution, page) {
            Ok(resolved) => resolved,
            Err(error) => {
                *stop = Some(Stop::Failed(error));
                *source = Source::Spent(storage);
                return Err(Suspended::NEW);
            }
        };
        if let Err(error) = machine.advance(record) {
            *stop = Some(Stop::Failed(DriveError::Kernel(error)));
            *source = Source::Spent(storage);
            return Err(Suspended::NEW);
        }
        *source = Source::Writing(storage, next.into_writer());
        // The bytes the record holds, and no others. An exhausted answer shows nothing, and
        // this is the only outcome §07 lets a caller reach.
        Ok(outcome)
    }

    /// The bytes history answered with, as the workflow observes them.
    fn observed(&self, conclusion: Conclusion, len: usize) -> Outcome<'_> {
        let bytes = self.result.get(..len).unwrap_or_default();
        match conclusion {
            Conclusion::Completed => Outcome::Completed(bytes),
            Conclusion::Failed => Outcome::Failed(bytes),
        }
    }
}

/// What one timer boundary needs next, once every borrow of the page has been collapsed.
///
/// Two answers rather than three: a deadline yields no bytes, so there is nothing to hand
/// back and nothing to copy.
enum TimerDecision {
    /// The deadline has passed and its firing is committed. The workflow carries on.
    Passed,
    /// The run stops here; `Context::stop` says why.
    Stop,
}

/// What the intent half of a timer boundary decided.
enum TimerHalf<E> {
    /// Row 3. Nothing is committed yet: read the clock, record the deadline, then measure.
    Arm(EffectId),
    /// Rows 1 and 2. The intent is committed; the outcome half decides which.
    Recorded,
    /// Row 5. History holds a terminal record, already copied into the result buffer.
    Finished(Conclusion, usize),
    /// Row 4, and everything else the kernel or the media refused.
    Failed(DriveError<E>),
}

/// Design document §11's arming, as the first three of §07's steps applied to a deadline.
///
/// Split out of [`Context::decide_timer`] for clippy's line budget, and because this is the
/// half that touches the clock: the reading it takes is the one the record carries, so the
/// floor a persistent deadline is measured against is the floor that goes to media.
///
/// §07's typestate is deliberately not used here. Its purpose is that no *physical effect*
/// precedes its committed intent, and this driver performs none for a deadline: it records
/// the intent and then compares readings. A dispatcher that armed a hardware alarm would
/// have a physical act to order, and that is rung 0.4's.
fn arming<S, C, K>(
    source: &mut Source<'_, S, C>,
    machine: &mut ReplayMachine,
    clocks: &mut K,
    page: &mut [u8],
    stop: &mut Option<Stop<S::Error>>,
    id: EffectId,
    spec: TimerSpec,
) -> TimerDecision
where
    S: StableStorage,
    C: IntegrityCheck,
    K: Clocks,
{
    let capability = clocks.capability();
    let Some(now) = clocks.now(spec.clock_kind()) else {
        *stop = Some(Stop::Failed(DriveError::ClockUnavailable));
        return TimerDecision::Stop;
    };
    let record = RecordRef::TimerScheduled {
        seq: id.seq,
        clock_kind: spec.clock_kind(),
        deadline: spec.deadline(),
        armed_at: now,
    };
    // Through §10's gate, like every other append: a deadline committed into a journal with
    // no room for the firing that resolves it strands the run, because §08 has no edge from
    // an open boundary to a terminal record.
    if let Err(error) = write(source, &record, page) {
        *stop = Some(Stop::Failed(error));
        return TimerDecision::Stop;
    }
    if let Err(error) = machine.advance(record) {
        *stop = Some(Stop::Failed(DriveError::Kernel(error)));
        return TimerDecision::Stop;
    }
    // Read again, *after* the commit. Programming a frame and crossing two barriers is not
    // instant, and the ticks that go into it are ticks the run really waited: measuring
    // against the pre-write reading discards every one of them, so a deadline shorter than
    // its own commit latency is reported as owing its whole interval and suspends a run that
    // has already waited long enough.
    let Some(reading) = clocks.now(spec.clock_kind()) else {
        *stop = Some(Stop::Failed(DriveError::ClockUnavailable));
        return TimerDecision::Stop;
    };
    // `now` is the floor, not `rearmed_at(now, reading)`. Both readings were taken in this
    // boot, microseconds apart, so there is no reset here for `rearmed_at` to accommodate —
    // and on a boot clock it answers the *lower* of the two, which would take a clock that
    // regressed or wrapped between the two reads and report it as zero elapsed time instead
    // of `ClockWentBackwards`. Codex found that: the round-1 fix reached for `rearmed_at`
    // defensively and masked the fault it was meant to leave visible. Re-arming a *recorded*
    // deadline is the only place a reset can have intervened, and that path still uses it.
    measure(
        source, machine, page, stop, id, spec, capability, now, reading,
    )
}

/// Whether `spec` has elapsed at `reading`, and the firing record if it has.
///
/// The one place a deadline is judged. `armed_at` comes from the clock on a fresh arming
/// and from media on a re-arming, which is the whole of what issue #33's record carries
/// across a reset.
#[expect(
    clippy::too_many_arguments,
    reason = "every argument is one of the driver's fields, destructured by its caller"
)]
fn measure<S, C>(
    source: &mut Source<'_, S, C>,
    machine: &mut ReplayMachine,
    page: &mut [u8],
    stop: &mut Option<Stop<S::Error>>,
    id: EffectId,
    spec: TimerSpec,
    capability: ClockCapability,
    armed_at: u64,
    reading: u64,
) -> TimerDecision
where
    S: StableStorage,
    C: IntegrityCheck,
{
    // The firmware's own declaration, carried here rather than derived from the spec. The
    // boundary already admitted this spec against it, so this cannot refuse today — but a
    // capability computed from the spec is `admits` being handed the answer it exists to
    // compute, and it would arm a persistent deadline on boot-only firmware the day a
    // caller reached this function without going through the boundary first. Spelled as the
    // refusal it has to be, because the workspace denies a panic.
    let armed = match Timer::arm(spec, capability, armed_at) {
        Ok(armed) => armed,
        Err(error) => {
            *stop = Some(Stop::Failed(DriveError::Kernel(error)));
            return TimerDecision::Stop;
        }
    };
    let elapsed = match armed.evaluate(reading) {
        Ok(deadline) => deadline,
        Err(error) => {
            *stop = Some(Stop::Failed(DriveError::Kernel(error)));
            return TimerDecision::Stop;
        }
    };
    let Deadline::Elapsed = elapsed else {
        let Deadline::Remaining { ticks } = elapsed else {
            // Unreachable: `Deadline` has two shapes and the other is the arm above.
            *stop = Some(Stop::Failed(DriveError::Kernel(
                KernelError::NondeterministicWorkflow,
            )));
            return TimerDecision::Stop;
        };
        *stop = Some(Stop::WaitingUntil(id, spec.clock_kind(), ticks));
        return TimerDecision::Stop;
    };

    let record = RecordRef::TimerFired { seq: id.seq };
    if let Err(error) = write(source, &record, page) {
        *stop = Some(Stop::Failed(error));
        return TimerDecision::Stop;
    }
    if let Err(error) = machine.advance(record) {
        *stop = Some(Stop::Failed(DriveError::Kernel(error)));
        return TimerDecision::Stop;
    }
    TimerDecision::Passed
}

impl<S: StableStorage, A: Activities + Clocks, C: IntegrityCheck> Context<'_, S, A, C> {
    /// The kernel's answer for one timer boundary, with nothing borrowed from it.
    ///
    /// [`decide`](Self::decide)'s twin, row for row.
    fn decide_timer(&mut self, spec: TimerSpec) -> TimerDecision {
        let Self {
            activities,
            machine,
            page,
            result,
            source,
            reserve,
            stop,
            pending,
            ..
        } = self;

        if stop.is_some() {
            return TimerDecision::Stop;
        }
        // `decide`'s guard, for the same reason: the writer is inside the outstanding
        // effect, so a deadline here would report `NoAppendPoint`.
        if pending.is_some() {
            *stop = Some(Stop::Failed(DriveError::EffectOutstanding));
            return TimerDecision::Stop;
        }

        let request = TimerRequest {
            spec,
            capability: activities.capability(),
        };

        let half = match peek(source, page, *reserve) {
            Err(error) => TimerHalf::Failed(error),
            Ok(next) => match machine.timer_intent(request, next) {
                Ok(TimerIntent::Schedule { id }) => TimerHalf::Arm(id),
                Ok(TimerIntent::Recorded { .. }) => TimerHalf::Recorded,
                Ok(TimerIntent::Finished { outcome }) => {
                    match store(outcome, result, reserve.bounds().terminal_bytes) {
                        Ok((conclusion, len)) => TimerHalf::Finished(conclusion, len),
                        Err(error) => TimerHalf::Failed(error),
                    }
                }
                Err(error) => TimerHalf::Failed(DriveError::Kernel(error)),
            },
        };

        match half {
            TimerHalf::Failed(error) => {
                *stop = Some(Stop::Failed(error));
                TimerDecision::Stop
            }
            TimerHalf::Finished(conclusion, result_len) => {
                *stop = Some(Stop::Finished {
                    conclusion,
                    result_len,
                });
                TimerDecision::Stop
            }
            TimerHalf::Arm(id) => arming(source, machine, *activities, page, stop, id, spec),
            TimerHalf::Recorded => {
                let resolved = match peek(source, page, *reserve) {
                    Err(error) => Err(error),
                    Ok(next) => machine.timer_outcome(next).map_err(DriveError::Kernel),
                };
                match resolved {
                    // Row 1. History holds the firing, so the deadline passed in an earlier
                    // boot. Nothing is armed, no clock is read, and no record is written —
                    // which is issue #33's first "done when".
                    Ok(TimerResolve::Fired { .. }) => TimerDecision::Passed,
                    // Row 2. The intent is committed and the firing is not. The spec and the
                    // arming reading come back from media, because the reset took the RAM
                    // they were in.
                    Ok(TimerResolve::Rearm {
                        id,
                        spec: recorded,
                        armed_at,
                    }) => {
                        let Some(reading) = activities.now(recorded.clock_kind()) else {
                            *stop = Some(Stop::Failed(DriveError::ClockUnavailable));
                            return TimerDecision::Stop;
                        };
                        // Which reading the deadline is measured from is §11's, not this
                        // driver's: a persistent floor crosses the reset and a boot floor
                        // does not, because the clock that set it restarted. Taking the
                        // recorded reading for both is a permanent `ClockWentBackwards` on
                        // every boot deadline that outlives a reset, on a run §08 gives no
                        // way to end.
                        let floor = recorded.rearmed_at(armed_at, reading);
                        measure(
                            source,
                            machine,
                            page,
                            stop,
                            id,
                            recorded,
                            request.capability,
                            floor,
                            reading,
                        )
                    }
                    Err(error) => {
                        *stop = Some(Stop::Failed(error));
                        TimerDecision::Stop
                    }
                }
            }
        }
    }
}

impl<S: StableStorage, A: Activities + Clocks, C: IntegrityCheck> Context<'_, S, A, C> {
    /// §07 steps 5, 6 and 7 for the effect [`Boundary::schedule`] handed out.
    ///
    /// The stop is recorded rather than returned, for [`Context::decide`]'s reason: the
    /// only thing a [`Boundary`] method may hand a workflow is [`Suspended`].
    fn record_answer(&mut self, answered: Answered<'_>) -> Option<(Conclusion, usize)> {
        let Self {
            machine,
            page,
            result,
            source,
            reserve,
            stop,
            pending,
            ..
        } = self;

        if stop.is_some() {
            return None;
        }
        let Some(dispatchable) = pending.take() else {
            *stop = Some(Stop::Failed(DriveError::NoEffectOutstanding));
            return None;
        };

        let bound = usize::from(reserve.bounds().effect_result_bytes);
        // An answer over the bound is exhausted rather than refused, for
        // `Context::dispatch`'s reason: the schedule record is committed, and a refusal
        // strands the run for ever. This is the last line of defence for a caller that
        // splits §07 itself: the façade narrows before it dispatches, but `Boundary` is
        // public and another caller need not.
        // `the_driver_refuses_an_over_bound_answer_a_caller_offers_it_directly` is what
        // fails when this arm goes.
        let resolution = match answered {
            Answered::Exhausted => Resolution::Exhausted,
            Answered::Completed(bytes) | Answered::Failed(bytes) if bytes.len() > bound => {
                Resolution::Exhausted
            }
            Answered::Completed(bytes) => Resolution::Completed(bytes),
            Answered::Failed(bytes) => Resolution::Failed(bytes),
        };

        // The device comes back from where scheduling parked it — see `Source`'s own
        // documentation.
        let storage = match spent(source) {
            Ok(storage) => storage,
            Err(error) => {
                *stop = Some(Stop::Failed(error));
                return None;
            }
        };
        let Resolved {
            next,
            record,
            outcome,
        } = match dispatchable.resolve(&mut *storage, resolution, page) {
            Ok(resolved) => resolved,
            Err(error) => {
                *stop = Some(Stop::Failed(error));
                *source = Source::Spent(storage);
                return None;
            }
        };
        if let Err(error) = machine.advance(record) {
            *stop = Some(Stop::Failed(DriveError::Kernel(error)));
            *source = Source::Spent(storage);
            return None;
        }
        *source = Source::Writing(storage, next.into_writer());
        // Copied into the caller's result buffer, because the bytes the workflow observes
        // must outlive the answer they were handed in.
        match store(outcome, result, reserve.bounds().effect_result_bytes) {
            Ok(recorded) => Some(recorded),
            Err(error) => {
                *stop = Some(Stop::Failed(error));
                None
            }
        }
    }
}

/// What the version boundary decided, once every borrow of the page has been collapsed.
///
/// Two shapes rather than three: a gate has no world to wait for, so there is no
/// "dispatch". Either the branch is known and durable, or the run stops.
enum GateDecision {
    /// The branch to take. Its `VersionMarker` is on media before this value exists.
    Branch(u16),
    /// The run stops here; `Context::stop` says why.
    Stop,
}

/// What the version boundary's first half decided, with nothing borrowed from the page.
enum GateHalf<E> {
    /// Rows 1 and 2. History holds the marker.
    Recorded(u16),
    /// Row 3. History ended, so this image's branch is the one to record.
    Record(EffectId),
    /// Row 5. History holds a terminal record, already copied into the result buffer.
    Finished(Conclusion, usize),
    /// Row 4, and everything else the kernel or the media refused.
    Failed(DriveError<E>),
}

impl<S: StableStorage, A: Activities + Clocks, C: IntegrityCheck> Context<'_, S, A, C> {
    /// The kernel's answer for one version gate, with nothing borrowed from it.
    ///
    /// [`decide`](Self::decide)'s twin, row for row, minus the world.
    fn decide_gate(&mut self, gate: GateId) -> GateDecision {
        let Self {
            machine,
            page,
            result,
            source,
            reserve,
            stop,
            pending,
            versions,
            ..
        } = self;

        if stop.is_some() {
            return GateDecision::Stop;
        }
        // `decide`'s guard, for the same reason: the writer is inside the outstanding
        // effect, so a marker here would report `NoAppendPoint`.
        if pending.is_some() {
            *stop = Some(Stop::Failed(DriveError::EffectOutstanding));
            return GateDecision::Stop;
        }

        let request = VersionRequest {
            gate,
            supported: *versions,
        };

        let half = match peek(source, page, *reserve) {
            Err(error) => GateHalf::Failed(error),
            Ok(next) => match machine.version_intent(request, next) {
                Ok(VersionIntent::Recorded { version, .. }) => GateHalf::Recorded(version),
                Ok(VersionIntent::Record { id }) => GateHalf::Record(id),
                Ok(VersionIntent::Finished { outcome }) => {
                    match store(outcome, result, reserve.bounds().terminal_bytes) {
                        Ok((conclusion, len)) => GateHalf::Finished(conclusion, len),
                        Err(error) => GateHalf::Failed(error),
                    }
                }
                Err(error) => GateHalf::Failed(DriveError::Kernel(error)),
            },
        };

        match half {
            GateHalf::Failed(error) => {
                *stop = Some(Stop::Failed(error));
                GateDecision::Stop
            }
            GateHalf::Finished(conclusion, result_len) => {
                *stop = Some(Stop::Finished {
                    conclusion,
                    result_len,
                });
                GateDecision::Stop
            }
            // Row 1. The branch the run took, whatever branch this image would take.
            GateHalf::Recorded(version) => GateDecision::Branch(version),
            // Row 3. §02 decision 3's shape, at a boundary whose effect is a branch inside
            // the workflow: the record crosses both barriers before the number is returned,
            // so there is no state in which the workflow has branched and media has not.
            GateHalf::Record(id) => {
                let version = versions.current();
                let record = RecordRef::VersionMarker {
                    seq: id.seq,
                    gate,
                    version,
                };
                if let Err(error) = write(source, &record, page) {
                    *stop = Some(Stop::Failed(error));
                    return GateDecision::Stop;
                }
                if let Err(error) = machine.advance(record) {
                    *stop = Some(Stop::Failed(DriveError::Kernel(error)));
                    return GateDecision::Stop;
                }
                GateDecision::Branch(version)
            }
        }
    }
}

impl<S: StableStorage, A: Activities + Clocks, C: IntegrityCheck> Context<'_, S, A, C> {
    /// §10's swap, performed end to end: retire this bank and install `input` as a new run
    /// in the other one.
    ///
    /// [`DriveError::ContinueUnsupported`] for a driver not built with
    /// [`Driver::at_bank`]. Otherwise every field a swap needs comes from `self.bank` —
    /// read fresh from the device this same boot — or from `self.reserve`, so none of it
    /// can be a value a caller carried in stale. Issue
    /// [#110](https://github.com/madmax983/waymaker/issues/110).
    fn swap_in(&mut self, input: &[u8]) -> Result<RunId, DriveError<S::Error>> {
        let Some(bank) = self.bank else {
            return Err(DriveError::ContinueUnsupported);
        };
        // §07's two halves were left out of order: an effect scheduled and not yet
        // resolved has a durable schedule record in the bank this call is about to
        // reclaim, and §10's swap forfeits an effect's identity on purpose only when a
        // *crash* leaves one behind — never as the ordinary answer to a live call. Every
        // other boundary method refuses the same way; this is the same refusal.
        if self.pending.is_some() {
            return Err(DriveError::EffectOutstanding);
        }
        // §10's reserve, consulted before the device is touched rather than left to a
        // caller's discretion: the `Bounds` this run was priced against have to fit the
        // bank the swap installs into, and both banks of one layout are the same size.
        let recomputed =
            Reserve::for_layout(self.reserve.bounds(), bank.layout).map_err(DriveError::Reserve)?;
        // Codex found that stopping at the line above only proves `self.reserve`'s *bounds*
        // fit this layout in the abstract — it says nothing about whether `self.reserve`
        // itself was ever priced against it. A caller who built this driver with a reserve
        // computed for another layout passes that check every time, and the swap below
        // would go on to install a run whose very first boot then meets `Reserved::over`
        // on the empty new journal and fails with `CapacityError::WrongDevice` — after the
        // old run is already gone. Comparing the two values here, before anything is
        // touched, is the same refusal `Reserved::over` would reach one boot later, just
        // early enough to matter.
        if recomputed != self.reserve {
            return Err(DriveError::Reserve(CapacityError::WrongDevice));
        }
        // `Reserve::for_layout` prices the *bound* the run declared, not the bytes this
        // call was actually handed — a header wider than that bound still fits
        // `Swap::beginning`'s own, weaker gate, and installs a journal below
        // `Reserve::for_layout`'s own floor, stranding the run it just started.
        let bound = self.reserve.bounds().run_input_bytes;
        if input.len() > usize::from(bound) {
            return Err(DriveError::NextRunInputTooLong {
                bytes: input.len(),
                bound,
            });
        }
        // §08's own rule for a run that says it is over, applied to the other way a run
        // ends: a replay still short of the end of history has not earned the right to
        // decide anything, migrating included — and unlike `Stop::Finished`'s check, this
        // one has to run *before* the swap rather than after it, because `swap_in` is about
        // to reclaim the only copy of whatever comes next. An earlier image that continued
        // past this point and recorded more would be silently overwritten rather than
        // reported, which is the nondeterminism `nothing_follows` exists to catch.
        nothing_follows(&mut self.source, self.page)?;
        let next_run = bank.run.successor().ok_or(DriveError::RunIdExhausted)?;
        let Some(storage) = take_storage(&mut self.source) else {
            return Err(DriveError::NoAppendPoint);
        };
        // A throwaway scan: `Swap::beginning` only reads its region back out of `retired`
        // and then drops it, so this reborrow's life ends there and `storage` is free for
        // `prepare` below. See `waymaker-flash`'s `swap` module for why the later steps
        // take a fresh argument instead of keeping this one.
        let retired =
            Retired::Recovery(Recovery::<_, C>::with_integrity(bank.region, &mut *storage));
        // The version this image writes into a new run, not the retiring bank's recorded
        // one — `begin` makes the same choice for an erased journal. A next-run header
        // stamped with the *retired* version would still validate today, against this same
        // image's `verify_header_identity`; it only strands the run once a later image
        // narrows `oldest` past a version that header never actually held any history under.
        let next_header = BankHeader {
            run: next_run,
            align: bank.align,
            workflow_kind: bank.workflow_kind,
            workflow_version: self.versions.current(),
            input_schema: bank.input_schema,
            input,
        };
        let swap = Swap::beginning(bank.layout, bank.booted, bank.run, retired, next_header)
            .map_err(DriveError::Swap)?;
        let prepared = swap.prepare(storage).map_err(DriveError::SwapStep)?;
        let staged = prepared.stage(self.page).map_err(DriveError::SwapStep)?;
        let sealable = staged.payload_barrier().map_err(DriveError::SwapStep)?;
        let installed = sealable.commit().map_err(DriveError::SwapStep)?;
        // `commit` is the swap's own point of no return: `next_run` is durably sealed and
        // authoritative from here whatever happens next, per `Installed::reclaim`'s own
        // documented postcondition — on failure "the device has one authoritative bank as
        // well: the new one". Reclaim only ever erases the *retiring* bank, so a failure
        // here cannot put this migration in doubt; it is not this call's to report, and
        // folding it into `DriveError::SwapStep` would tell the caller a migration failed
        // that a fresh boot of this same layout would show had already happened. The stale
        // bank left behind is not stranded either — the next swap's own `prepare` erases the
        // bank it installs into unconditionally, before writing anything.
        let _ = installed.reclaim();
        Ok(next_run)
    }
}

/// Takes the device out of `source`, whatever state it is in, and leaves
/// [`Source::Taken`] behind.
///
/// [`None`] only for [`Source::Taken`] itself — every other state holds a device to give
/// back. Used by [`Context::swap_in`], which is the boot's last act either way: nothing
/// after it reads `source` again.
const fn take_storage<'storage, S, C: IntegrityCheck>(
    source: &mut Source<'storage, S, C>,
) -> Option<&'storage mut S> {
    match mem::replace(source, Source::Taken) {
        Source::Scanning(recovery) => Some(recovery.into_storage()),
        Source::Writing(storage, _reserved) => Some(storage),
        Source::Spent(storage) => Some(storage),
        Source::Taken => None,
    }
}

impl<S: StableStorage, A: Activities + Clocks, C: IntegrityCheck> Boundary
    for Context<'_, S, A, C>
{
    fn recorded_version(&self) -> u16 {
        self.recorded_version
    }

    fn gate(&mut self, gate: GateId) -> Result<u16, Suspended> {
        match self.decide_gate(gate) {
            GateDecision::Branch(version) => Ok(version),
            GateDecision::Stop => Err(Suspended::NEW),
        }
    }

    fn call(&mut self, kind: ActivityKind, input: &[u8]) -> Result<Outcome<'_>, Suspended> {
        match self.decide(kind, input) {
            Decision::Replayed(conclusion, len) => Ok(self.observed(conclusion, len)),
            Decision::Dispatch(dispatchable) => self.dispatch(dispatchable, kind, input),
            Decision::Stop => Err(Suspended::NEW),
        }
    }

    fn wait(&mut self, spec: TimerSpec) -> Result<(), Suspended> {
        match self.decide_timer(spec) {
            TimerDecision::Passed => Ok(()),
            TimerDecision::Stop => Err(Suspended::NEW),
        }
    }

    fn schedule(&mut self, kind: ActivityKind, input: &[u8]) -> Result<Handoff<'_>, Suspended> {
        if self.pending.is_some() {
            if self.stop.is_none() {
                self.stop = Some(Stop::Failed(DriveError::EffectOutstanding));
            }
            return Err(Suspended::NEW);
        }
        match self.decide(kind, input) {
            Decision::Replayed(conclusion, len) => {
                Ok(Handoff::Replayed(self.observed(conclusion, len)))
            }
            Decision::Dispatch(dispatchable) => {
                let id = dispatchable.intent().id();
                // The bound travels with the identity. `Boundary::resolve` refuses an
                // answer over it, and a caller that never learned the figure could only
                // discover that after the world had already produced one.
                let result_bytes = usize::from(self.reserve.bounds().effect_result_bytes);
                self.pending = Some(dispatchable);
                Ok(Handoff::Dispatch { id, result_bytes })
            }
            Decision::Stop => Err(Suspended::NEW),
        }
    }

    fn resolve(&mut self, answered: Answered<'_>) -> Result<Outcome<'_>, Suspended> {
        let Some((conclusion, len)) = self.record_answer(answered) else {
            return Err(Suspended::NEW);
        };
        Ok(self.observed(conclusion, len))
    }

    fn continue_as_new(&mut self, input: &[u8]) -> Suspended {
        // §10's reserve names its own remedy: "stop scheduling, and either end the run or
        // `continue_as_new`". A pre-mutation near-capacity refusal is that remedy offered
        // and not yet taken, so a workflow that reacts to it by asking to migrate is not
        // asking to override a stop this boot already committed to — it is exercising the
        // one exit §10 reserved capacity for. Every other stop — a wait, a finished run, an
        // unrelated failure, or a `continue_as_new` this same call already decided — still
        // wins, because those are not offers open for the taking.
        let reserved_rollover = matches!(
            self.stop,
            Some(Stop::Failed(DriveError::Capacity(Refusal::NearCapacity)))
        );
        if self.stop.is_none() || reserved_rollover {
            self.stop = Some(match self.swap_in(input) {
                Ok(run) => Stop::Migrated(run),
                Err(error) => Stop::Failed(error),
            });
        }
        Suspended::NEW
    }

    fn deadline_remaining(&self) -> Option<(ClockKind, u64)> {
        match self.stop {
            Some(Stop::WaitingUntil(_, clock_kind, remaining)) => Some((clock_kind, remaining)),
            _ => None,
        }
    }
}
