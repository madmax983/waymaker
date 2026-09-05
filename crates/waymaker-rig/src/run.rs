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
//! [`Cutter::cut`] is called at a *record boundary*: before the schedule write, before the
//! dispatch, before the completion write. A board's implementation arms a supply cut with a
//! short randomised delay and returns, so the cut lands somewhere inside the write that
//! follows — which is where issue #27's "randomised points" live. A host's implementation
//! stops the iteration where it stands, which is a crash point too, but only one of them.
//! Interior tears are `waymaker-fault`'s: `tests/sweep.rs` drives this same
//! [`Rig::iterate`] through the crash injector, which interrupts every byte of every program
//! and every block of every erase, exhaustively rather than at random.

use waymaker_core::RecordRef;
use waymaker_flash::append::{AppendError, Journal};
use waymaker_flash::bank::{
    self, BankHeader, BankId, BankLayout, Generation, LayoutError, SEAL_BYTES,
};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{Ending, JournalRegion, Recovery, RecoveryError, RegionError};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

use crate::audit::{Audit, Breach};
use crate::log::{Entry, Outcome};
use crate::phase::{Phase, ResetCause};
use crate::plan::{Cut, Plan};
use crate::wear::{Metered, Traffic, Wear};
use crate::window::{Window, WindowError};
use crate::witness::{Mark, Progress, Stage, Witness, WitnessError, WitnessRegion};
use crate::workload::{Role, Workload};

/// The physical effect a scheduled record stands for.
///
/// On a board this toggles a pin, drives a UART, or whatever the rig's operator can observe
/// afterwards. On a host it counts. What matters to §14's `durable-intent` is only that it
/// happens *after* the schedule record's commit barrier, which is [`Rig::iterate`]'s job
/// rather than the implementer's.
pub trait Dispatcher {
    /// How this dispatcher reports a refusal.
    type Error;

    /// Performs the effect `effect` of the current iteration, given the input its schedule
    /// record described.
    ///
    /// # Errors
    ///
    /// Whatever the physical effect can fail with. A rig that meets one stops the iteration.
    fn dispatch(&mut self, effect: u16, input: &[u8]) -> Result<(), Self::Error>;
}

/// The thing that takes the power away.
pub trait Cutter {
    /// The rig has reached `phase` for `effect` and is about to do the work of it.
    ///
    /// # Postconditions
    ///
    /// On a board this arms the cut and returns `false`, or cuts and never returns. On a host
    /// it returns `true` to stop the iteration where it stands. A `Cutter` that always
    /// returned `false` turns the rig into a plain writer, which is what
    /// [`NeverCut`] is for.
    fn cut(&mut self, phase: Phase, cause: ResetCause, effect: u16) -> bool;
}

/// A cutter that never cuts.
///
/// The fault-free run, and the baseline every sweep needs: a rig whose clean run does not
/// pass has nothing to say about the runs that were interrupted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NeverCut;

impl Cutter for NeverCut {
    fn cut(&mut self, _phase: Phase, _cause: ResetCause, _effect: u16) -> bool {
        false
    }
}

/// A cutter that stops at the first moment the iteration's own [`Cut`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlannedCut {
    cut: Cut,
    effect: u16,
    fired: bool,
}

impl PlannedCut {
    /// Stops at `cut`, on a run of `effects` effects.
    #[must_use]
    pub fn new(cut: Cut, effects: u16) -> Self {
        Self {
            cut,
            effect: cut.effect_index(effects),
            fired: false,
        }
    }

    /// Whether the cut has been reached.
    #[must_use]
    pub const fn fired(self) -> bool {
        self.fired
    }

    /// Which effect it is armed against.
    #[must_use]
    pub const fn effect(self) -> u16 {
        self.effect
    }
}

impl Cutter for PlannedCut {
    fn cut(&mut self, phase: Phase, _cause: ResetCause, effect: u16) -> bool {
        if !self.fired && phase == self.cut.phase() && effect == self.effect {
            self.fired = true;
            return true;
        }
        false
    }
}

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
    /// The workload asked for a record it does not have.
    Workload,
    /// The part refused.
    Storage(E),
    /// The physical effect refused.
    Dispatch(D),
    /// The four units are not a geometry.
    Geometry(GeometryError),
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
    /// enough for a bank header frame, a record frame and a witness slot.
    pub const PAGE_BYTES: usize = 512;

    /// Lays `part` out: the last erase block for the instrument, the rest for the engine.
    ///
    /// # Errors
    ///
    /// [`RigError::Window`] when the part cannot be split, [`RigError::Layout`] when the
    /// engine area cannot hold §10's two banks, [`RigError::Witness`] when the instrument
    /// area cannot hold a mark, and [`RigError::Geometry`] when either area is not a
    /// geometry.
    pub fn new<E>(part: Geometry, plan: Plan, effects: u16) -> Result<Self, RigError<E>> {
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
    #[must_use]
    pub const fn cut(&self, iteration: u32) -> Cut {
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

        let mut engine = self.engine(part)?;
        let bytes = engine.geometry().capacity();
        engine.erase(0, bytes).map_err(unwindow)?;
        engine.barrier().map_err(unwindow)?;

        let region = self.layout.bank(Self::BANK);
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
        let mut sealed = [0_u8; SEAL_BYTES * 4];
        let Ok(seal_len) = bank::encode_seal(&seal, self.layout.align(), &mut sealed) else {
            return Err(RigError::Bank);
        };
        let Some(seal_bytes) = sealed.get(..seal_len) else {
            return Err(RigError::Bank);
        };

        let padded = pad_to(written, self.layout.align());
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
        let want = payload.min(page.len());
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
        let cut = self.cut(iteration);
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

        for index in 0..workload.records() {
            let Some(role) = workload.role(index) else {
                return Err(RigError::Workload);
            };
            if let Some(phase) = phase_of(role)
                && phase == cut.phase()
                && let Some(effect) = effect_of(role)
                && phase != Phase::Dispatch
                && cutter.cut(phase, cause, effect)
            {
                return Ok(Stop::Cut { phase, effect });
            }

            self.mark(
                part,
                &mut witness,
                Mark::new(iteration, index, Stage::Attempted),
                page,
            )
            .map_err(widen)?;

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

            match role {
                Role::Schedule(effect) => {
                    // §07 step 4: the intent is committed and durable, so the effect may
                    // happen. The dispatch mark goes first, on purpose — see `crate::witness`.
                    self.mark(
                        part,
                        &mut witness,
                        Mark::new(iteration, index, Stage::Dispatched),
                        page,
                    )
                    .map_err(widen)?;
                    if cut.phase() == Phase::Dispatch
                        && effect == cut.effect_index(self.effects)
                        && cutter.cut(Phase::Dispatch, cause, effect)
                    {
                        return Ok(Stop::Cut {
                            phase: Phase::Dispatch,
                            effect,
                        });
                    }
                    let Some(input) = workload.effect_input(effect, &mut record_page) else {
                        return Err(RigError::Workload);
                    };
                    dispatcher
                        .dispatch(effect, input)
                        .map_err(RigError::Dispatch)?;
                }
                Role::Completion(_) => part.credit_effect(),
                Role::Start | Role::Finish => {}
            }
        }
        Ok(Stop::Completed)
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
            let mut seal = [0_u8; SEAL_BYTES * 4];
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
    ) -> Result<Outcome, RigError<S::Error>> {
        if page.len() < Self::PAGE_BYTES {
            return Err(RigError::ShortPage);
        }
        let progress = {
            let mut instrument = self.instrument(part)?;
            match Witness::new(self.witness).scan(&mut instrument, page) {
                Ok(progress) => progress,
                Err(_) => return Ok(Outcome::Breached(Breach::WitnessUnreadable)),
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
    /// # Errors
    ///
    /// As [`verify`](Self::verify).
    pub fn judge<S: StableStorage>(
        &self,
        iteration: u32,
        part: &mut S,
        progress: Progress,
        page: &mut [u8],
    ) -> Result<Outcome, RigError<S::Error>> {
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

        // A bank whose header did not survive is a bank with no journal to walk. The
        // authority count is still a verdict, so the audit is finished on it rather than
        // abandoned.
        let Ok(region) = self.journal_region(&mut engine, page) else {
            return Ok(finish(Audit::new(workload, progress), banks));
        };

        let mut audit = Audit::new(workload, progress);
        let mut recovery = Recovery::new(region);
        let mut expected = [0_u8; Workload::MAX_PAYLOAD_BYTES];
        while let Some(step) = recovery.next(&mut engine, page) {
            match step {
                Ok(record) => {
                    if let Err(breach) = audit.saw(&record, &mut expected) {
                        return Ok(Outcome::Breached(breach));
                    }
                }
                // The scan stopping is history ending, which every `Ending` below says is a
                // legal thing for a crash to have produced. The audit is what decides whether
                // the prefix it produced is a prefix the rig will accept.
                Err(_) => break,
            }
        }
        let _unused: Option<Ending> = recovery.ending();
        Ok(finish(audit, banks))
    }

    /// A log entry for `iteration`, with the outcome and the wear it cost.
    #[must_use]
    pub const fn entry(&self, iteration: u32, outcome: Outcome, wear: Wear) -> Entry {
        Entry::new(self.plan.seed(), iteration, self.part, self.effects)
            .with_outcome(outcome)
            .with_wear(wear)
    }
}

/// Turns an audit's closing verdict into an outcome.
const fn finish(audit: Audit, banks: usize) -> Outcome {
    match audit.finish(banks) {
        Ok(()) => Outcome::Passed,
        Err(breach) => Outcome::Breached(breach),
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

/// `len` rounded up to a whole number of program units.
const fn pad_to(len: usize, align: ProgramAlign) -> usize {
    let unit = align.get() as usize;
    len.div_ceil(unit) * unit
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
