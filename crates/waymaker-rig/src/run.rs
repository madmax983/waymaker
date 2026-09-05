//! The rig itself: prepare a part, run an iteration into a cut, and judge what comes back.
//!
//! # The three moments
//!
//! [`Rig::prepare`] lays a part out — §10's two banks in the engine area, an erased
//! [`WitnessRegion`] in the instrument area, bank A installed at generation one — and is what
//! a rig does once, before a run.
//!
//! [`Rig::iterate`] is the run. It writes the iteration's [`Workload`] through
//! [`Journal`]'s two barriers, marks the witness on either side of every record, dispatches
//! every scheduled effect through a [`Dispatcher`], and calls a [`Cutter`] when it reaches
//! the phase the iteration's [`Cut`] named. On a board the cutter arms the supply and the
//! call does not return; on a host it returns and the iteration stops where it stood.
//!
//! [`Rig::verify`] is what runs after the reset. It reads the witness, selects the
//! authoritative bank, walks the journal, and feeds both to an [`Audit`].
//!
//! # Why the three are separate calls
//!
//! Because on a board there is a reset between the second and the third, and no stack
//! survives it. Anything `verify` needs has to be on media or derivable from the seed, and
//! splitting the rig at the reset boundary is what makes that a compile-time fact rather than
//! a discipline.
//!
//! # What the cutter can and cannot do here
//!
//! [`Cutter`] and [`Dispatcher`] are [`crate::cutter`]'s, not this module's — they are the
//! board's half of the interface, and they live apart so that this file's public surface can
//! be pinned by name. `Cutter::cut` is called at a *record boundary*: before the schedule write, before the
//! dispatch, before the completion write. A board's implementation arms a supply cut with a
//! short randomised delay and returns, so the cut lands somewhere inside the write that
//! follows — which is where issue #27's "randomised points" live. A host's implementation
//! stops the iteration where it stands, which is a crash point too, but only one of them.
//! Interior tears are `waymaker-fault`'s: `tests/sweep.rs` drives this same
//! [`Rig::iterate`] through the crash injector, which interrupts every byte of every program
//! and every block of every erase, exhaustively rather than at random.

use waymaker_core::RecordRef;
use waymaker_flash::append::{AppendError, Journal};
use waymaker_flash::bank::{self, BankHeader, BankId, BankLayout, Generation, LayoutError};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery, RecoveryError, RegionError};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

use crate::audit::{Audit, Breach};
use crate::cutter::{Cutter, Dispatcher};
use crate::log::{Entry, Outcome};
use crate::phase::Phase;
use crate::plan::{Cut, Plan};
use crate::wear::{Metered, Traffic, Wear};
use crate::window::{Window, WindowError};
use crate::witness::{Mark, Progress, Stage, Witness, WitnessError, WitnessRegion};
use crate::workload::{Role, Workload};

/// Why a rig could not lay a part out, run an iteration, or judge one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RigError<E, D = core::convert::Infallible> {
    /// The part cannot hold an engine area and an instrument area.
    Window(WindowError),
    /// The engine area cannot hold §10's two banks.
    Layout(LayoutError),
    /// The instrument area cannot hold a witness.
    Witness(WitnessError<E>),
    /// The journal region is not one this geometry permits.
    Region(RegionError),
    /// The bank header could not be encoded or read.
    Bank,
    /// The journal could not be recovered.
    Recovery(RecoveryError<E>),
    /// A record could not be appended.
    Append(AppendError<E>),
    /// The caller's page is too small for the rig.
    ShortPage,
    /// The part programs in units wider than the rig's fixed buffers.
    ProgramUnitTooWide {
        /// The widest program unit the rig can serve.
        limit: u32,
    },
    /// The run schedules more effects than a record index can number.
    TooManyEffects {
        /// The most effects a run can have.
        limit: u16,
    },
    /// The instrument area cannot hold every mark a clean run writes.
    WitnessTooSmall {
        /// How many marks the run needs.
        needed: u32,
        /// How many the region holds.
        capacity: u32,
    },
    /// The workload asked for a record it does not have.
    Workload,
    /// The part refused.
    Storage(E),
    /// The physical effect refused.
    Dispatch(D),
    /// The four units are not a geometry.
    Geometry(GeometryError),
}

/// What a verification found, and the evidence it found it from.
///
/// # Why the evidence travels with the outcome
///
/// A breach code says *which* guarantee broke. It does not say what recovery produced, and
/// for two of the six that is the whole of the difference: a `RecordDiffers` is caused by the
/// bytes on the part, and an `Authority` by the state of two bank seals — neither of which a
/// log line can carry and neither of which survives rebuilding a part from the seed. So the
/// verdict carries what was *observed*: how many records the audit accepted, and how many
/// banks claimed authority.
///
/// That is what makes a logged breach investigable rather than merely named. Without it, a
/// line reading `record-differs` tells a reader that history diverged and not where, and the
/// only way to find out is to still have the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Verdict {
    outcome: Outcome,
    recovered: u16,
    banks: usize,
}

impl Verdict {
    /// What the guarantees said.
    #[must_use]
    pub const fn outcome(self) -> Outcome {
        self.outcome
    }

    /// How many records the audit accepted before it reached that outcome.
    #[must_use]
    pub const fn recovered(self) -> u16 {
        self.recovered
    }

    /// How many banks claimed authority.
    #[must_use]
    pub const fn banks(self) -> usize {
        self.banks
    }
}

/// Where an iteration stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stop {
    /// The run wrote every record and ended.
    Completed,
    /// The cutter fired.
    Cut {
        /// Which write it fired in front of.
        phase: Phase,
        /// Which effect of the run.
        effect: u16,
    },
}

/// A part laid out for the rig, and the run it performs on it.
///
/// # Invariants
///
/// The instrument area is the last erase block of the part and the engine area is everything
/// before it, so the two never overlap and neither can erase the other's blocks — which is
/// what [`Window`] refuses to allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rig {
    part: Geometry,
    engine_bytes: u32,
    witness_bytes: u32,
    layout: BankLayout,
    witness: WitnessRegion,
    plan: Plan,
    effects: u16,
}

impl Rig {
    /// How many witness marks a clean run of `effects` effects writes.
    ///
    /// Three for every schedule record — attempted, acknowledged, dispatched — and two for
    /// every other: the opening `RunStarted`, each `EffectCompleted`, and the terminal
    /// `RunCompleted`. That is `5 * effects + 4`.
    ///
    /// `None` only for a run too long to index, which [`new`](Self::new) refuses separately.
    #[must_use]
    pub const fn marks_per_run(effects: u16) -> Option<u32> {
        let effects = effects as u32;
        let Some(per_effect) = effects.checked_mul(5) else {
            return None;
        };
        per_effect.checked_add(4)
    }

    /// The bank the rig installs and writes into.
    ///
    /// One bank, at generation one, for the length of a run. §10's swap is issue
    /// [#26](https://github.com/madmax983/waymaker/issues/26)'s, and a rig that swapped banks
    /// before the swap exists would be testing a protocol nobody wrote.
    pub const BANK: BankId = BankId::A;

    /// The generation that bank is installed at.
    pub const GENERATION: Generation = Generation(1);

    /// How large a page every call here needs.
    ///
    /// One number rather than four, because a rig's RAM is a constant it declares once. Wide
    /// enough for a bank header frame, a record frame and a witness slot — every one of which
    /// is padded to a program unit, which is why [`MAX_PROGRAM_BYTES`](Self::MAX_PROGRAM_BYTES)
    /// exists and is checked.
    pub const PAGE_BYTES: usize = 512;

    /// The widest program unit the rig's fixed buffers can serve.
    ///
    /// A generation seal is `waymaker_flash::bank::SEAL_BYTES` rounded *up* to a program unit, and a witness slot
    /// is [`MARK_BYTES`](crate::witness::MARK_BYTES) rounded up the same way, so both grow
    /// with the part. A `#![no_std]`, allocation-free rig sizes those buffers once, and the
    /// number it sizes them to is a limit somebody has to state.
    ///
    /// 256 bytes is the standard SPI NOR page. A part above it is refused by
    /// [`new`](Self::new) with [`RigError::ProgramUnitTooWide`] — which is the point of the
    /// constant: without it a 256-byte-page part was accepted, and then failed in the middle
    /// of installing a bank with an opaque [`RigError::Bank`].
    pub const MAX_PROGRAM_BYTES: u32 = 256;

    /// Lays `part` out: the last erase block for the instrument, the rest for the engine.
    ///
    /// # Errors
    ///
    /// [`RigError::Window`] when the part cannot be split, [`RigError::Layout`] when the
    /// engine area cannot hold §10's two banks, [`RigError::Witness`] when the instrument
    /// area cannot hold a mark, and [`RigError::Geometry`] when either area is not a
    /// geometry.
    pub fn new<E>(part: Geometry, plan: Plan, effects: u16) -> Result<Self, RigError<E>> {
        // Before any layout arithmetic: a seal and a witness slot are both sized in program
        // units, and this rig's buffers are fixed. Refused here so that a caller learns it
        // from the constructor rather than from a bank half-installed.
        // A run is `2 * effects + 2` records and a record index is a `u16`. Refused here so
        // that every workload a rig holds has a length, rather than one that saturated.
        if effects > Workload::MAX_EFFECTS {
            return Err(RigError::TooManyEffects {
                limit: Workload::MAX_EFFECTS,
            });
        }
        if part.program_size() > Self::MAX_PROGRAM_BYTES {
            return Err(RigError::ProgramUnitTooWide {
                limit: Self::MAX_PROGRAM_BYTES,
            });
        }
        let witness_bytes = part.erase_size();
        let Some(engine_bytes) = part.capacity().checked_sub(witness_bytes) else {
            return Err(RigError::Window(WindowError::PastTheEnd));
        };
        if engine_bytes == 0 {
            return Err(RigError::Window(WindowError::Empty));
        }
        let engine = Geometry::new(
            engine_bytes,
            part.erase_size(),
            part.program_size(),
            part.read_size(),
        )
        .map_err(RigError::Geometry)?;
        let instrument = Geometry::new(
            witness_bytes,
            part.erase_size(),
            part.program_size(),
            part.read_size(),
        )
        .map_err(RigError::Geometry)?;
        let layout = BankLayout::new(engine).map_err(RigError::Layout)?;
        let witness = WitnessRegion::of(instrument, 0, witness_bytes)
            .map_err(|error| RigError::Witness(promote(error)))?;
        // `WitnessRegion::of` checks that *one* mark fits. A clean run writes rather more, and
        // a rig whose instrument runs out near the end of an iteration reports
        // `WitnessError::Full` — an instrument failure dressed up as a run. Refused here,
        // where a caller can make the region bigger or the run shorter.
        let Some(marks) = Self::marks_per_run(effects) else {
            return Err(RigError::TooManyEffects {
                limit: Workload::MAX_EFFECTS,
            });
        };
        if marks > witness.capacity() {
            return Err(RigError::WitnessTooSmall {
                needed: marks,
                capacity: witness.capacity(),
            });
        }
        Ok(Self {
            part,
            engine_bytes,
            witness_bytes,
            layout,
            witness,
            plan,
            effects,
        })
    }

    /// The part this rig was laid out for.
    #[must_use]
    pub const fn part(&self) -> Geometry {
        self.part
    }

    /// §10's two banks, in the engine area.
    #[must_use]
    pub const fn layout(&self) -> BankLayout {
        self.layout
    }

    /// The instrument area's witness region.
    #[must_use]
    pub const fn witness_region(&self) -> WitnessRegion {
        self.witness
    }

    /// The seeded plan of cut points.
    #[must_use]
    pub const fn plan(&self) -> Plan {
        self.plan
    }

    /// How many effects every run schedules.
    #[must_use]
    pub const fn effects(&self) -> u16 {
        self.effects
    }

    /// The run iteration `iteration` performs.
    #[must_use]
    pub const fn workload(&self, iteration: u32) -> Workload {
        Workload::new(self.plan.seed(), iteration, self.effects)
    }

    /// The cut iteration `iteration` arms.
    ///
    /// Named `cut_at` rather than `cut` because [`Cutter::cut`] is in this file too, and the
    /// `rig-oracle` pin is a list of *names*.
    #[must_use]
    pub const fn cut_at(&self, iteration: u32) -> Cut {
        self.plan.cut(iteration)
    }

    /// Where the instrument area starts on the part.
    #[must_use]
    pub const fn instrument_base(&self) -> u32 {
        self.engine_bytes
    }

    /// The engine window over `part`.
    fn engine<'a, S: StableStorage>(
        &self,
        part: &'a mut S,
    ) -> Result<Window<'a, S>, RigError<S::Error>> {
        Window::new(part, 0, self.engine_bytes).map_err(RigError::Window)
    }

    /// The instrument window over `part`.
    fn instrument<'a, S: StableStorage>(
        &self,
        part: &'a mut S,
    ) -> Result<Window<'a, S>, RigError<S::Error>> {
        Window::new(part, self.engine_bytes, self.witness_bytes).map_err(RigError::Window)
    }

    /// The bank header this rig installs for `iteration`.
    const fn header<'a>(&self, iteration: u32, input: &'a [u8]) -> BankHeader<'a> {
        BankHeader {
            run: self.workload(iteration).run(),
            align: self.layout.align(),
            workflow_kind: Workload::WORKFLOW_KIND,
            workflow_version: Workload::WORKFLOW_VERSION,
            input_schema: 0,
            input,
        }
    }

    /// Erases the part and installs bank A at generation one, ready for `iteration`.
    ///
    /// # Errors
    ///
    /// [`RigError::Storage`] for anything the part refused, and the layout refusals
    /// [`new`](Self::new) lists.
    pub fn prepare<S: StableStorage>(
        &self,
        part: &mut Metered<'_, S>,
        iteration: u32,
        page: &mut [u8],
    ) -> Result<(), RigError<S::Error>> {
        if page.len() < Self::PAGE_BYTES {
            return Err(RigError::ShortPage);
        }
        // The instrument is erased as the rig's own traffic; the engine as the engine's,
        // because §10's bank lifecycle is what erases a bank and the figure published for it
        // has to include that.
        part.set_traffic(Traffic::Rig);
        {
            let mut instrument = self.instrument(part)?;
            let bytes = instrument.geometry().capacity();
            instrument
                .erase(0, bytes)
                .map_err(|_| RigError::Witness(WitnessError::Region))?;
            instrument
                .barrier()
                .map_err(|_| RigError::Witness(WitnessError::Region))?;
        }
        part.set_traffic(Traffic::Engine);

        // The bank input is the opening record's payload: §10 puts the run input in the bank
        // header and §06 replays `RunStarted` from history, so a rig whose two disagreed
        // would be running a workflow on one input and replaying it on another.
        let mut input = [0_u8; Workload::MAX_PAYLOAD_BYTES];
        let workload = self.workload(iteration);
        let Some(RecordRef::RunStarted {
            input: run_input, ..
        }) = workload.record(0, &mut input)
        else {
            return Err(RigError::Workload);
        };
        let header = self.header(iteration, run_input);

        // The engine area holds both banks and, on a geometry whose block count is odd, a
        // spare. Wiping all of it is the *rig* preparing a part, not §10 installing a bank —
        // and charging it to the engine made the published figure report five erased blocks
        // for a lifecycle that erases two. So the part outside the bank is erased first, as
        // the instrument's own traffic, and the bank itself is erased below as the engine's.
        let region = self.layout.bank(Self::BANK);
        let Some(beyond) = engine_capacity(&self.layout).checked_sub(region.bytes()) else {
            return Err(RigError::Layout(LayoutError::TooFewEraseBlocks));
        };
        if beyond > 0 {
            part.set_traffic(Traffic::Rig);
            let outcome = {
                let mut engine = self.engine(part)?;
                engine
                    .erase(region.bytes(), beyond)
                    .and_then(|()| engine.barrier())
            };
            part.set_traffic(Traffic::Engine);
            outcome.map_err(unwindow)?;
        }

        let mut engine = self.engine(part)?;
        // §10's install, and the only erase this figure charges the engine for: one bank.
        engine
            .erase(region.base(), region.bytes())
            .map_err(unwindow)?;
        engine.barrier().map_err(unwindow)?;

        let Some(slot) = page.get_mut(..) else {
            return Err(RigError::ShortPage);
        };
        let Ok(written) = bank::encode_header(&header, slot) else {
            return Err(RigError::Bank);
        };
        let Some(frame) = page.get(..written) else {
            return Err(RigError::ShortPage);
        };
        let Ok(seal) = bank::seal_for(frame, Self::GENERATION) else {
            return Err(RigError::Bank);
        };
        // Sized to the widest program unit `new` accepts rather than to `SEAL_BYTES`: a
        // generation seal is padded up to a program unit, so on a 256-byte-page part it is
        // 256 bytes, not 12.
        let mut sealed = [0_u8; Self::MAX_PROGRAM_BYTES as usize];
        let Ok(seal_len) = bank::encode_seal(&seal, self.layout.align(), &mut sealed) else {
            return Err(RigError::Bank);
        };
        let Some(seal_bytes) = sealed.get(..seal_len) else {
            return Err(RigError::Bank);
        };

        let Some(padded) = pad_to(written, self.layout.align()) else {
            return Err(RigError::Bank);
        };
        let Some(header_bytes) = page.get(..padded) else {
            return Err(RigError::ShortPage);
        };
        engine
            .program(region.base(), header_bytes)
            .map_err(unwindow)?;
        engine.barrier().map_err(unwindow)?;
        engine
            .program(region.seal_offset(), seal_bytes)
            .map_err(unwindow)?;
        engine.barrier().map_err(unwindow)?;
        Ok(())
    }

    /// Reads bank `id`'s header frame into `page` and returns its length.
    fn read_header<S: StableStorage>(
        &self,
        engine: &mut Window<'_, S>,
        id: BankId,
        page: &mut [u8],
    ) -> Result<usize, RigError<S::Error>> {
        let region = self.layout.bank(id);
        let Ok(payload) = usize::try_from(region.payload_bytes()) else {
            return Err(RigError::Bank);
        };
        // Rounded *down* to the read unit. §12's `validate_read` refuses a length that is not
        // a multiple of it, and the caller's page length is the caller's business — a rig
        // that passed it through turned an odd-sized buffer into what reads like a media
        // fault, which is a refusal a driver author would go looking for in their driver.
        let Ok(unit) = usize::try_from(engine.geometry().read_size()) else {
            return Err(RigError::Bank);
        };
        let want = payload.min(page.len() - page.len() % unit.max(1));
        let Some(slot) = page.get_mut(..want) else {
            return Err(RigError::ShortPage);
        };
        engine.read(region.base(), slot).map_err(unwindow)?;
        Ok(want)
    }

    /// The journal region of the bank this rig writes into.
    fn journal_region<S: StableStorage>(
        &self,
        engine: &mut Window<'_, S>,
        page: &mut [u8],
    ) -> Result<JournalRegion, RigError<S::Error>> {
        let read = self.read_header(engine, Self::BANK, page)?;
        let Some(bytes) = page.get(..read) else {
            return Err(RigError::ShortPage);
        };
        let Ok(header) = bank::decode_header(bytes) else {
            return Err(RigError::Bank);
        };
        JournalRegion::of(self.layout, Self::BANK, &header).map_err(RigError::Region)
    }

    /// The journal of the bank *this run* was installed in, or `None` if it was never
    /// installed on this part.
    ///
    /// Three ways to answer `None`, and each of them is a part that has nothing to say about
    /// `workload`'s run rather than a part that lost something:
    ///
    /// * no bank is authoritative, or two are — preparation was cut before its generation
    ///   seal landed, which is the state every part is in before its first one;
    /// * the authoritative bank's header does not decode — there is no journal region to
    ///   derive, and §14's `frame ignored; previous history prefix wins` is about frames
    ///   inside a journal rather than about the header that names one;
    /// * the header decodes and names a **different run** — the part is still the previous
    ///   iteration's, which is exactly the window a reset during `prepare` opens.
    ///
    /// The run id is the comparison rather than the whole header because that is what makes a
    /// bank *belong* to a run: [`Workload::run`] draws it from the seed and the iteration, so
    /// two iterations of one plan never share one.
    fn installed_journal<S: StableStorage>(
        &self,
        engine: &mut Window<'_, S>,
        workload: Workload,
        banks: usize,
        page: &mut [u8],
    ) -> Result<Option<JournalRegion>, RigError<S::Error>> {
        if banks != 1 {
            return Ok(None);
        }
        let read = self.read_header(engine, Self::BANK, page)?;
        let Some(bytes) = page.get(..read) else {
            return Err(RigError::ShortPage);
        };
        let Ok(header) = bank::decode_header(bytes) else {
            return Ok(None);
        };
        if header.run != workload.run() {
            return Ok(None);
        }
        match JournalRegion::of(self.layout, Self::BANK, &header) {
            Ok(region) => Ok(Some(region)),
            Err(error) => Err(RigError::Region(error)),
        }
    }

    /// Runs one iteration, stopping where the cutter fires.
    ///
    /// # Errors
    ///
    /// Every refusal in [`RigError`]. A rig that meets one stops: the iteration has proved
    /// nothing, and carrying on would write records into a journal it can no longer account
    /// for.
    pub fn iterate<S: StableStorage, D: Dispatcher, C: Cutter>(
        &self,
        iteration: u32,
        part: &mut Metered<'_, S>,
        dispatcher: &mut D,
        cutter: &mut C,
        page: &mut [u8],
    ) -> Result<Stop, RigError<S::Error, D::Error>> {
        if page.len() < Self::PAGE_BYTES {
            return Err(RigError::ShortPage);
        }
        let workload = self.workload(iteration);
        let Some(records) = workload.records() else {
            return Err(RigError::Workload);
        };
        let cut = self.cut_at(iteration);
        let cause = cut.cause();

        // Before anything is written: exactly one bank must be authoritative. This is what
        // makes the witness mean what `Audit::finish` reads it as meaning — a mark implies a
        // device that had an authority when the mark was written — and it is also what a
        // boot does, so a rig that skipped it would be running a protocol no firmware runs.
        let region = {
            let mut engine = self.engine(part).map_err(widen)?;
            if self.authoritative_banks(&mut engine, page).map_err(widen)? != 1 {
                return Err(RigError::Bank);
            }
            self.journal_region(&mut engine, page).map_err(widen)?
        };
        let mut journal = {
            let mut engine = self.engine(part).map_err(widen)?;
            let mut recovery = Recovery::new(region);
            while let Some(step) = recovery.next(&mut engine, page) {
                if let Err(error) = step {
                    return Err(RigError::Recovery(unwindow_recovery(error)));
                }
            }
            match Journal::after(recovery) {
                Some(journal) => journal,
                None => return Err(RigError::Region(RegionError::EmptyRegion)),
            }
        };

        let mut witness = Witness::new(self.witness);
        let mut record_page = [0_u8; Workload::MAX_PAYLOAD_BYTES];

        for index in 0..records {
            let Some(role) = workload.role(index) else {
                return Err(RigError::Workload);
            };
            self.mark(
                part,
                &mut witness,
                Mark::new(iteration, index, Stage::Attempted),
                page,
            )
            .map_err(widen)?;

            // The cut goes here — after the `Attempted` mark and immediately before the
            // journal write it names — and both halves of that matter.
            //
            // *After the mark*, because a board's cutter arms a delayed reset and returns, so
            // the very next storage operation is what a short delay tears. Offered before the
            // mark, that operation is the rig's own witness program: the run would report a
            // schedule-phase cut having torn the instrument, and the write phase issue #27
            // asks about would never have been under way at all.
            //
            // *Gated on the effect*, because `Cutter::cut` says the rig "has reached `phase`
            // for `effect` and is about to do the work of it". Offered for every effect, a
            // board cutter that acts whenever it is called arms on the first one whatever the
            // seed said — and arms again on the next. `PlannedCut` filters internally, which
            // is exactly why it could not see this.
            //
            // `Phase::Dispatch` cannot be reached here: `phase_of` answers only `Schedule` and
            // `Completion`, because a dispatch happens between two records rather than at one,
            // and its cut point is taken in `after_schedule`, where the effect goes out.
            if let Some(phase) = phase_of(role)
                && phase == cut.phase()
                && let Some(effect) = effect_of(role)
                && effect == cut.effect_index(self.effects)
                && cutter.cut(phase, cause, effect)
            {
                return Ok(Stop::Cut { phase, effect });
            }

            {
                let mut engine = self.engine(part).map_err(widen)?;
                let Some(record) = workload.record(index, &mut record_page) else {
                    return Err(RigError::Workload);
                };
                let staged = journal
                    .stage(&mut engine, &record, page)
                    .map_err(|error| RigError::Append(unwindow_append(error)))?;
                let sealable = staged
                    .payload_barrier(&mut engine)
                    .map_err(|error| RigError::Append(unwindow_append(error)))?;
                sealable
                    .commit(&mut engine)
                    .map_err(|error| RigError::Append(unwindow_append(error)))?;
            }
            part.set_amplification(journal.amplification());

            self.mark(
                part,
                &mut witness,
                Mark::new(iteration, index, Stage::Acknowledged),
                page,
            )
            .map_err(widen)?;

            if let Role::Schedule(effect) = role
                && let Some(stop) = self.after_schedule(
                    iteration,
                    index,
                    effect,
                    DispatchStep {
                        part,
                        witness: &mut witness,
                        dispatcher,
                        cutter,
                    },
                    page,
                )?
            {
                return Ok(stop);
            }
            if matches!(role, Role::Completion(_)) {
                part.credit_effect();
            }
        }
        Ok(Stop::Completed)
    }

    /// §07 step 4, for the schedule record at `index`: mark the dispatch, take the cut point
    /// if this iteration armed one here, and perform the effect.
    ///
    /// A method of its own so that [`iterate`](Self::iterate) stays a loop over records rather
    /// than a loop with a second protocol inside it. The order is the whole of
    /// `durable-intent`: the record's commit barrier has already returned, the mark goes down
    /// *before* the effect so that it over-claims rather than under-claims, and the cutter is
    /// offered the one window in which nothing is being written.
    fn after_schedule<S: StableStorage, D: Dispatcher, C: Cutter>(
        &self,
        iteration: u32,
        index: u16,
        effect: u16,
        parts: DispatchStep<'_, '_, S, D, C>,
        page: &mut [u8],
    ) -> Result<Option<Stop>, RigError<S::Error, D::Error>> {
        let DispatchStep {
            part,
            witness,
            dispatcher,
            cutter,
        } = parts;
        self.mark(
            part,
            witness,
            Mark::new(iteration, index, Stage::Dispatched),
            page,
        )
        .map_err(widen)?;

        let cut = self.cut_at(iteration);
        if cut.phase() == Phase::Dispatch
            && effect == cut.effect_index(self.effects)
            && cutter.cut(Phase::Dispatch, cut.cause(), effect)
        {
            return Ok(Some(Stop::Cut {
                phase: Phase::Dispatch,
                effect,
            }));
        }

        let mut input = [0_u8; Workload::MAX_PAYLOAD_BYTES];
        let Some(bytes) = self.workload(iteration).effect_input(effect, &mut input) else {
            return Err(RigError::Workload);
        };
        dispatcher
            .dispatch(effect, bytes)
            .map_err(RigError::Dispatch)?;
        Ok(None)
    }

    /// Programs one witness mark, as the rig's own traffic.
    fn mark<S: StableStorage>(
        &self,
        part: &mut Metered<'_, S>,
        witness: &mut Witness,
        mark: Mark,
        page: &mut [u8],
    ) -> Result<(), RigError<S::Error>> {
        part.set_traffic(Traffic::Rig);
        let outcome = {
            let mut instrument = self.instrument(part)?;
            witness.mark(&mut instrument, mark, page)
        };
        part.set_traffic(Traffic::Engine);
        outcome.map_err(|error| RigError::Witness(unwindow_witness(error)))
    }

    /// How many banks are authoritative.
    ///
    /// §14's `single-authority`, read off media exactly as a boot would read it: each bank's
    /// header and seal, through [`bank::sealed_generation`], then [`bank::select`].
    fn authoritative_banks<S: StableStorage>(
        &self,
        engine: &mut Window<'_, S>,
        page: &mut [u8],
    ) -> Result<usize, RigError<S::Error>> {
        let mut generations = [None, None];
        for (slot, id) in generations.iter_mut().zip([BankId::A, BankId::B]) {
            let region = self.layout.bank(id);
            let read = self.read_header(engine, id, page)?;
            let Some(header) = page.get(..read) else {
                return Err(RigError::ShortPage);
            };
            let mut seal = [0_u8; Self::MAX_PROGRAM_BYTES as usize];
            let Ok(seal_len) = usize::try_from(region.seal_bytes()) else {
                return Err(RigError::Bank);
            };
            let Some(seal_slot) = seal.get_mut(..seal_len) else {
                return Err(RigError::Bank);
            };
            engine
                .read(region.seal_offset(), seal_slot)
                .map_err(unwindow)?;
            *slot = bank::sealed_generation(header, seal_slot);
        }
        Ok(match bank::select(generations) {
            bank::Authority::Unsealed => 0,
            bank::Authority::Bank { .. } => 1,
            bank::Authority::Ambiguous { .. } => 2,
        })
    }

    /// Judges what a reset left behind.
    ///
    /// # Errors
    ///
    /// [`RigError::Storage`] and the layout refusals. A witness that cannot be read is
    /// *not* an error: it is [`Breach::WitnessUnreadable`], because an iteration whose
    /// instrument broke proved nothing and must not be reported as a pass.
    pub fn verify<S: StableStorage>(
        &self,
        iteration: u32,
        part: &mut S,
        page: &mut [u8],
    ) -> Result<Verdict, RigError<S::Error>> {
        if page.len() < Self::PAGE_BYTES {
            return Err(RigError::ShortPage);
        }
        let progress = {
            let mut instrument = self.instrument(part)?;
            match Witness::new(self.witness).scan(&mut instrument, page) {
                Ok(progress) => progress,
                Err(_) => {
                    return Ok(Verdict {
                        outcome: Outcome::Breached(Breach::WitnessUnreadable),
                        recovered: 0,
                        banks: 0,
                    });
                }
            }
        };
        self.judge(iteration, part, progress, page)
    }

    /// The half of [`verify`](Self::verify) that takes the witness's answer as given.
    ///
    /// Separate so that [`crate::log`]'s reconstruction can drive it with a [`Progress`]
    /// rebuilt from a log line rather than read off a device — which is issue #27's third
    /// "done when" in one function.
    ///
    /// # What it does not check about `progress`
    ///
    /// That it is *consistent*: nothing here requires `acknowledged <= attempted`, or that a
    /// dispatched index was ever attempted. It does not need to. Every inconsistency makes
    /// the audit demand **more** of recovery, so an incoherent witness fails closed — which
    /// is why `tests/sweep.rs` can hand it a hand-built one and get a breach rather than a
    /// pass.
    ///
    /// # Which bank it walks
    ///
    /// [`Rig::BANK`], always. The authority count above is computed the way a boot computes
    /// it — both banks' headers and seals, through [`bank::select`] — and then only bank A's
    /// journal is read, because bank A is the only one this rig installs. §10's swap is issue
    /// [#26](https://github.com/madmax983/waymaker/issues/26)'s, and when it lands this is
    /// where the selected bank has to start being the one that is walked.
    ///
    /// # Errors
    ///
    /// As [`verify`](Self::verify).
    pub fn judge<S: StableStorage>(
        &self,
        iteration: u32,
        part: &mut S,
        progress: Progress,
        page: &mut [u8],
    ) -> Result<Verdict, RigError<S::Error>> {
        // Checked here as well as in `verify`, because this is a public entry point of its
        // own: `crate::log`'s reconstruction calls it directly, and a short page that reached
        // the scan would surface as a decode failure — which this function reads as "history
        // ended here" rather than as "the caller passed the wrong buffer".
        if page.len() < Self::PAGE_BYTES {
            return Err(RigError::ShortPage);
        }
        let workload = self.workload(iteration);
        let mut engine = self.engine(part)?;
        let banks = self.authoritative_banks(&mut engine, page)?;

        // Ambiguity is decided first and on its own. Two sealed banks means the rig cannot say
        // which journal is this run's history, so every obligation below — what recovery owed
        // and what it produced — is *unanswerable* rather than failed, and answering one of
        // them anyway sends a reader to the journal for a fault that is in the bank selection.
        // Watched happening: an ambiguous part carrying a whole witness reported
        // `LostAcknowledgedRecord` at record five, because the rig had refused to walk either
        // bank and the witness was owed records nothing had produced.
        //
        // No crash makes a second authority out of nothing, so ownership is not a question this
        // has to answer: it is a breach whichever run installed either bank.
        if banks > 1 {
            return Ok(Verdict {
                outcome: Outcome::Breached(Breach::Authority { banks }),
                recovered: 0,
                banks,
            });
        }

        // §10's authority is what a boot reads, and a bank belongs to the run whose header it
        // carries. Both halves are load-bearing here, and the second one was found by review
        // rather than by writing it down: a rig's loop is `prepare(n)` → `iterate(n)` → reset
        // → `verify(n)`, so from the second iteration onwards `verify(n)` meets a part that
        // *finished* `n - 1`. A reset during `prepare(n)` leaves that part's engine still
        // sealed at `n - 1`, and a `judge` that walked whatever bank A held read run `n - 1`'s
        // journal against run `n`'s declarations and reported a §14 violation on a healthy
        // board. An uninstalled part is not a verdict about recovery — see [`uninstalled`].
        let Some(region) = self.installed_journal(&mut engine, workload, banks, page)? else {
            return Ok(uninstalled(workload, progress, banks));
        };

        let mut audit = Audit::new(workload, progress);
        let mut recovery = Recovery::new(region);
        let mut expected = [0_u8; Workload::MAX_PAYLOAD_BYTES];
        while let Some(step) = recovery.next(&mut engine, page) {
            match step {
                Ok(record) => {
                    if let Err(breach) = audit.saw(&record, &mut expected) {
                        return Ok(Verdict {
                            outcome: Outcome::Breached(breach),
                            recovered: audit.recovered(),
                            banks,
                        });
                    }
                }
                // A *decode* failure is history ending: a torn frame, an unsealed one, erased
                // media. Every one of those is a legal thing for a crash to have produced, and
                // the audit is what decides whether the prefix before it is one the rig will
                // accept.
                Err(RecoveryError::Decode(_)) => break,
                // Everything else is the instrument failing rather than the subject. A part
                // that goes read-flaky at record `k`, a page the caller sized wrong, a region
                // proved against another device — none of those is a statement about §14, and
                // reading them as "history ended here" reports a driver fault as a clean run.
                // That is the same failure `Breach::WitnessUnreadable` exists to prevent on
                // the other half of this function.
                Err(error) => return Err(RigError::Recovery(unwindow_recovery(error))),
            }
        }
        // The ending is deliberately not consulted. `Damaged`, `Unsealed` and `Clean` are the
        // three shapes a crash leaves, and which one it is changes nothing the audit asks:
        // the obligations come from the witness, and the prefix is the prefix either way.
        let _unused: Option<Ending> = recovery.ending();
        Ok(finish(audit, banks))
    }

    /// A log entry for `iteration`: the verdict and its evidence, the wear it cost, and the
    /// witness the verdict was computed against.
    ///
    /// None of the three is decoration. A seed and an iteration rebuild the *run*; §14's
    /// guarantees are statements about what the rig **knew**, which is the witness; and the
    /// verdict's evidence is what recovery actually produced, which no amount of rebuilding
    /// recovers — see [`Verdict`].
    #[must_use]
    pub fn entry(&self, iteration: u32, verdict: Verdict, wear: Wear, progress: Progress) -> Entry {
        Entry::new(self.plan.seed(), iteration, self.part, self.effects)
            .with_outcome(verdict.outcome())
            .with_evidence(verdict.recovered(), verdict.banks())
            .with_wear(wear)
            .with_progress(progress)
    }
}

/// What [`Rig::after_schedule`] needs, grouped so the signature stays inside the workspace's
/// argument limit — four borrows that are one thing: the run in progress.
struct DispatchStep<'part, 'storage, S, D, C> {
    part: &'part mut Metered<'storage, S>,
    witness: &'part mut Witness,
    dispatcher: &'part mut D,
    cutter: &'part mut C,
}

/// The verdict for a part this run was never installed on.
///
/// Nothing has been claimed about the run, so there is nothing recovery can have lost — but
/// "nothing to audit" is not the same as "nothing to check", and the two things this does
/// check are what keep it from being a hole the rig passes through.
///
/// The witness is normalised rather than read. A [`Progress`] naming a *different* iteration
/// is the previous run's, still on media because the reset landed before `prepare` erased the
/// instrument, and on an uninstalled part that is the ordinary state rather than an
/// instrument fault: it says nothing about this run, so this run is owed nothing. On an
/// *installed* part the same witness is [`Breach::WitnessUnreadable`] and stays so, because
/// `prepare` erases the instrument before it installs the bank — an installed part carrying
/// the previous iteration's marks is an instrument that failed.
///
/// And the authority figure is zero rather than `banks`, which is the half that still bites.
/// §14's `single-authority` asks about *this* run's authority, and a bank carrying another
/// run's header is not it. So a rig that had begun marking a run — a witness with marks in it,
/// or one torn mid-mark — on a part with no bank of its own is [`Breach::Authority`], which is
/// what it is: records were being written into a journal this run does not own.
///
/// # Preconditions
///
/// `banks` is zero or one. An *ambiguous* part never reaches here: [`Rig::judge`] decides two
/// sealed banks on their own, ahead of every question about ownership, because zero would
/// report the one state [`Audit::finish`] exists to refuse as a pass whenever the witness is
/// empty — a part prepared and not yet run — and would name the wrong number whenever it is
/// not.
const fn uninstalled(workload: Workload, progress: Progress, banks: usize) -> Verdict {
    let progress = match progress.iteration() {
        Some(other) if other != workload.iteration() => Progress::EMPTY,
        _ => progress,
    };
    let outcome = match Audit::new(workload, progress).finish(0) {
        Ok(()) => Outcome::Passed,
        Err(breach) => Outcome::Breached(breach),
    };
    Verdict {
        outcome,
        recovered: 0,
        banks,
    }
}

/// Turns an audit's closing verdict into a verdict, evidence and all.
const fn finish(audit: Audit, banks: usize) -> Verdict {
    let recovered = audit.recovered();
    let outcome = match audit.finish(banks) {
        Ok(()) => Outcome::Passed,
        Err(breach) => Outcome::Breached(breach),
    };
    Verdict {
        outcome,
        recovered,
        banks,
    }
}

/// Which write point a role is the write for, if any.
const fn phase_of(role: Role) -> Option<Phase> {
    match role {
        Role::Schedule(_) => Some(Phase::Schedule),
        Role::Completion(_) => Some(Phase::Completion),
        Role::Start | Role::Finish => None,
    }
}

/// Which effect a role belongs to, if any.
const fn effect_of(role: Role) -> Option<u16> {
    match role {
        Role::Schedule(effect) | Role::Completion(effect) => Some(effect),
        Role::Start | Role::Finish => None,
    }
}

/// How many bytes the engine area of `layout` spans.
///
/// `BankLayout` reports a bank's size rather than the window's, and the window is the geometry
/// the layout was built on — which is exactly the engine area, because `Rig::new` builds it
/// from one.
const fn engine_capacity(layout: &BankLayout) -> u32 {
    layout.geometry().capacity()
}

/// `len` rounded up to a whole number of program units, or `None` on an overflow.
///
/// `checked_mul` rather than the bare multiply the structurally identical code in
/// `WitnessRegion::of` already uses: it is not reachable — `len` is a header frame length
/// bounded by the page — but an unchecked multiply is a panic, and this crate forbids those.
const fn pad_to(len: usize, align: ProgramAlign) -> Option<usize> {
    let unit = align.get() as usize;
    len.div_ceil(unit).checked_mul(unit)
}

/// Widens a rig error that cannot name a dispatcher's failure.
fn widen<E, D>(error: RigError<E>) -> RigError<E, D> {
    match error {
        RigError::Window(inner) => RigError::Window(inner),
        RigError::Layout(inner) => RigError::Layout(inner),
        RigError::Witness(inner) => RigError::Witness(inner),
        RigError::Region(inner) => RigError::Region(inner),
        RigError::Bank => RigError::Bank,
        RigError::Recovery(inner) => RigError::Recovery(inner),
        RigError::Append(inner) => RigError::Append(inner),
        RigError::ShortPage => RigError::ShortPage,
        RigError::ProgramUnitTooWide { limit } => RigError::ProgramUnitTooWide { limit },
        RigError::TooManyEffects { limit } => RigError::TooManyEffects { limit },
        RigError::WitnessTooSmall { needed, capacity } => {
            RigError::WitnessTooSmall { needed, capacity }
        }
        RigError::Workload => RigError::Workload,
        RigError::Storage(inner) => RigError::Storage(inner),
        RigError::Geometry(inner) => RigError::Geometry(inner),
        RigError::Dispatch(never) => match never {},
    }
}

/// Widens a driver-free witness refusal.
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

/// Unwraps a window's own refusal back to the part's error.
fn unwindow<E>(error: crate::window::WindowFault<E>) -> RigError<E> {
    match error {
        crate::window::WindowFault::Window(inner) => RigError::Window(inner),
        crate::window::WindowFault::Part(inner) => RigError::Storage(inner),
    }
}

/// The same, for a recovery's error.
fn unwindow_recovery<E>(error: RecoveryError<crate::window::WindowFault<E>>) -> RecoveryError<E> {
    match error {
        RecoveryError::Storage(crate::window::WindowFault::Part(inner)) => {
            RecoveryError::Storage(inner)
        }
        // A window's own refusal is a bound proved on one device and applied to another,
        // which is what `WrongDevice` already means.
        RecoveryError::Storage(crate::window::WindowFault::Window(_))
        | RecoveryError::WrongDevice => RecoveryError::WrongDevice,
        RecoveryError::Decode(inner) => RecoveryError::Decode(inner),
        RecoveryError::PageTooSmall { needed } => RecoveryError::PageTooSmall { needed },
    }
}

/// The same, for an append's error.
fn unwindow_append<E>(error: AppendError<crate::window::WindowFault<E>>) -> AppendError<E> {
    match error {
        AppendError::Storage(crate::window::WindowFault::Part(inner)) => {
            AppendError::Storage(inner)
        }
        // As above: a window's refusal is a bound applied to the wrong device.
        AppendError::Storage(crate::window::WindowFault::Window(_)) | AppendError::WrongDevice => {
            AppendError::WrongDevice
        }
        AppendError::Encode(inner) => AppendError::Encode(inner),
        AppendError::NoRoom { needed, available } => AppendError::NoRoom { needed, available },
        AppendError::Interrupted => AppendError::Interrupted,
    }
}

/// The same, for a witness's error.
fn unwindow_witness<E>(error: WitnessError<crate::window::WindowFault<E>>) -> WitnessError<E> {
    match error {
        WitnessError::Driver(crate::window::WindowFault::Part(inner)) => {
            WitnessError::Driver(inner)
        }
        WitnessError::Driver(crate::window::WindowFault::Window(_)) | WitnessError::Region => {
            WitnessError::Region
        }
        WitnessError::ShortBuffer => WitnessError::ShortBuffer,
        WitnessError::NotAMark => WitnessError::NotAMark,
        WitnessError::Hole => WitnessError::Hole,
        WitnessError::MixedIterations => WitnessError::MixedIterations,
        WitnessError::OutOfOrder => WitnessError::OutOfOrder,
        WitnessError::Full => WitnessError::Full,
        WitnessError::WrongGeometry => WitnessError::WrongGeometry,
    }
}
