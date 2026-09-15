//! The workflow's half of design document §06's explicit kernel boundary.
//!
//! Two methods, and they are the whole vocabulary a workflow has: ask for an effect, or
//! wait for a deadline, and get either the answer or an instruction to stop. There is no
//! `Future` here and no executor — a synchronous workflow suspends by propagating
//! [`Suspended`] with `?`, which is what `.await` does in the façade one layer up.

use waymaker_core::timer::{ClockKind, TimerSpec};
use waymaker_core::version::GateId;
use waymaker_core::{ActivityKind, EffectId, Outcome};

/// The run cannot continue now. Return it.
///
/// Four different things produce it, and a workflow may not tell them apart: the run is
/// waiting for an activity, the run is waiting for a deadline, history says the run already
/// ended, or the driver met an error.
/// Which one it was is [`Progress`](crate::Progress) or [`DriveError`](crate::DriveError),
/// and both are the *driver's* answer rather than the workflow's.
///
/// # Why the field is private
///
/// So that a workflow cannot build one and stop a run that nothing asked to stop, and have
/// the driver then report a wait that no activity is behind. `NEW` is the driver's own
/// construction, kept to `waymaker-drive`. [`awaiting_dispatch`](Self::awaiting_dispatch) is
/// the one sanctioned exception: `waymaker-facade-demo`'s `Workflow` impls call it, not the
/// driver, because it answers a stop the driver never sees — see its own doc for why that is
/// still narrow rather than a second `NEW`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Suspended(());

impl Suspended {
    /// The one value, made by the driver only.
    pub(crate) const NEW: Self = Self(());

    /// The run cannot continue because a caller's own dispatch has not answered yet.
    ///
    /// A synchronous workflow gets [`Suspended`] by propagating one a boundary call
    /// returned. A caller driving an opaque `Future` cannot: design document §07 step 4 is
    /// the world's, so a dispatcher that answers `Poll::Pending` stops the future with
    /// nothing recorded and nothing to propagate — no boundary call is pending, only the
    /// world's own answer. This is that stop, named for the one caller it is for and `pub`
    /// rather than `pub(crate)` because that caller is `waymaker-facade-demo`, one crate up.
    /// It carries no more state than `NEW` does, so the exception costs nothing
    /// a misused `Suspended` could not already cost — an idle boot, never a wrong record.
    #[must_use]
    pub const fn awaiting_dispatch() -> Self {
        Self(())
    }
}

/// What the driver says about one activity boundary, after design document §07 step 3.
///
/// [`Boundary::call`]'s two halves, for a caller that performs the effect itself. There is
/// no third shape for "not durable yet": a driver that cannot commit the intent answers
/// [`Suspended`], so the identity that reaches the world exists only after the schedule
/// record is durable.
///
/// # Why this vocabulary is the driver's own
///
/// So that removing the façade removes nothing here. This crate names no dependency on
/// `waymaker-embassy` at all — issue
/// [#106](https://github.com/madmax983/waymaker/issues/106) moved `facade`, `ota` and
/// `provisioning` into `waymaker-facade-demo`, above this crate — which is what makes "the
/// protocol is fully usable through the synchronous driver" a fact `cargo metadata` states
/// rather than a claim a feature flag argued for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff<'a> {
    /// History holds the outcome. Nothing may be dispatched.
    Replayed(Outcome<'a>),
    /// The intent is durable. Perform the effect under `id`, then call
    /// [`Boundary::resolve`].
    Dispatch {
        /// The stable `(RunId, EffectSeq)` the schedule record committed.
        id: EffectId,
        /// How wide an answer this run declared it can record.
        ///
        /// §10's `effect_result_bytes`, from the reserve that priced the bank. It travels
        /// with the identity because the caller that performs the effect is the one that
        /// must not produce a wider answer, and the driver is the only party that knows
        /// the figure. Issue
        /// [#36](https://github.com/madmax983/waymaker/issues/36).
        result_bytes: usize,
    },
}

/// What the world answered for the effect [`Boundary::schedule`] handed out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answered<'a> {
    /// Success, within the run's declared bound.
    Completed(&'a [u8]),
    /// Failure, within the run's declared bound.
    Failed(&'a [u8]),
    /// The answer is wider than the bound. It is recorded as a failure with no payload.
    Exhausted,
}

/// What a workflow may ask of the world.
///
/// Implemented by the driver and by nothing else in this crate. It is `dyn`-safe on
/// purpose: a workflow is written against this trait alone, so it knows nothing about the
/// storage, the integrity check, or the activities behind it.
pub trait Boundary {
    /// Run `kind` over `input`, or stop.
    ///
    /// # The lifetime discipline
    ///
    /// The returned bytes borrow the caller's **result** buffer — not the scratch page,
    /// which never reaches a workflow — and the *next* call overwrites it. A workflow that
    /// needs a result after its next boundary copies it into its own storage first. That is
    /// not a convention: the borrow is derived from `&mut self`, so holding one across the
    /// next call does not compile.
    ///
    /// ```compile_fail,E0499
    /// use waymaker_core::{ActivityKind, Outcome};
    /// use waymaker_drive::{Boundary, Suspended};
    ///
    /// fn hold_across(boundary: &mut dyn Boundary) -> Result<(), Suspended> {
    ///     let first = boundary.call(ActivityKind(1), b"a")?;
    ///     let second = boundary.call(ActivityKind(2), b"b")?;
    ///     let _ = (first, second);
    ///     Ok(())
    /// }
    /// ```
    ///
    /// The same workflow, with the first result copied out before the second call, is the
    /// shape that does compile:
    ///
    /// ```
    /// use waymaker_core::{ActivityKind, Outcome};
    /// use waymaker_drive::{Boundary, Suspended};
    ///
    /// fn copy_first(boundary: &mut dyn Boundary) -> Result<usize, Suspended> {
    ///     let mut kept = [0_u8; 8];
    ///     let taken = match boundary.call(ActivityKind(1), b"a")? {
    ///         Outcome::Completed(bytes) | Outcome::Failed(bytes) => {
    ///             let taken = bytes.len().min(kept.len());
    ///             if let (Some(from), Some(into)) = (bytes.get(..taken), kept.get_mut(..taken)) {
    ///                 into.copy_from_slice(from);
    ///             }
    ///             taken
    ///         }
    ///     };
    ///     let _ = boundary.call(ActivityKind(2), b"b")?;
    ///     Ok(taken)
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// [`Suspended`] whenever the run must stop here. Nothing about *why* travels in it.
    fn call(&mut self, kind: ActivityKind, input: &[u8]) -> Result<Outcome<'_>, Suspended>;

    /// Wait until `spec`'s deadline has passed, or stop.
    ///
    /// Design document §11 and issue
    /// [#33](https://github.com/madmax983/waymaker/issues/33). A deadline is a boundary like
    /// an effect: the intent is recorded before the wait begins, and a reboot re-arms the
    /// same deadline from what the record holds rather than starting it again.
    ///
    /// It returns no bytes. A deadline's whole result is that it passed, which is why
    /// `TimerFired` has no body and why this is a `Result<(), Suspended>`.
    ///
    /// # The clock is the world's, never the workflow's
    ///
    /// A workflow that read a clock of its own would be nondeterministic, and §08 would
    /// catch it only where the reading changed an effect. The deadline enters through this
    /// call, is recorded, and is replayed — so the second execution of a run waits for what
    /// the first one waited for.
    ///
    /// # Errors
    ///
    /// [`Suspended`] whenever the run must stop here — including the ordinary case that the
    /// deadline has not passed yet. Nothing about *why* travels in it;
    /// [`Progress`](crate::Progress) is the driver's answer.
    fn wait(&mut self, spec: TimerSpec) -> Result<(), Suspended>;

    /// [`call`](Self::call)'s first half: §07 steps 1 to 3, and no dispatch.
    ///
    /// For a caller that performs the effect itself — an async façade that must `.await`
    /// the world between the two durable halves. A caller that takes this route calls
    /// [`resolve`](Self::resolve) next, with what the world answered.
    ///
    /// # Postconditions
    ///
    /// On [`Handoff::Dispatch`] the schedule record survives a reset. On
    /// [`Handoff::Replayed`] nothing was written and nothing may be dispatched.
    ///
    /// # Errors
    ///
    /// [`Suspended`] whenever the run must stop here.
    fn schedule(&mut self, kind: ActivityKind, input: &[u8]) -> Result<Handoff<'_>, Suspended>;

    /// [`call`](Self::call)'s second half: §07 steps 5 to 7.
    ///
    /// The returned bytes borrow the caller's result buffer, under
    /// [`call`](Self::call)'s lifetime discipline.
    ///
    /// # Postconditions
    ///
    /// On [`Ok`] the outcome is replayable, and not before. An answer wider than the run's
    /// declared bound is recorded as [`Answered::Exhausted`] rather than refused: a refusal
    /// strands the run, because §08 has no edge from an unresolved effect to a terminal
    /// record.
    ///
    /// # Errors
    ///
    /// [`Suspended`] whenever the run must stop here, and whenever no effect is
    /// outstanding — a `resolve` with no `schedule` before it is a caller that never
    /// committed the intent.
    fn resolve(&mut self, answered: Answered<'_>) -> Result<Outcome<'_>, Suspended>;

    /// The workflow version this run's `RunStarted` record holds.
    ///
    /// Design document §08's first rule — "existing runs must continue under compatible
    /// code for their recorded version" — as the one call a workflow needs to obey it. A
    /// run started before an upgrade replays under the code its recorded version asks for,
    /// whatever version this image writes.
    ///
    /// It is history, not ambient state: the number comes from the run's own record, so
    /// every boot of one run sees the same value and a workflow that branches on it is
    /// deterministic. A workflow that branched on a *firmware* constant instead would take
    /// the new path on the boot after an upgrade and diverge at its next effect.
    ///
    /// # When this is not enough
    ///
    /// It answers "what did this run begin under", and no more. A branch chosen part way
    /// through a run — a new step that applies from here on, for runs that have not passed
    /// this point — is not derivable from the start version, and [`gate`](Self::gate) is
    /// what records it.
    fn recorded_version(&self) -> u16;

    /// Take the branch this run recorded at `gate`, recording one if it has none.
    ///
    /// Design document §08's third rule and issue
    /// [#40](https://github.com/madmax983/waymaker/issues/40). "Code changes that add,
    /// remove, or reorder effects require a new version or an explicit recorded version
    /// gate" — this is that gate.
    ///
    /// The first execution to reach it records
    /// [`VersionRange::current`](waymaker_core::version::VersionRange::current) in a
    /// `VersionMarker`; every later boot is handed that number back. So a branch a run took
    /// under one image is taken again under every later one, whatever branch the later one
    /// would have chosen.
    ///
    /// # Postconditions
    ///
    /// On [`Ok`] the branch survives a reset: the marker record crossed both of §07's
    /// barriers before the number was returned. There is no window in which a workflow has
    /// taken a branch that media does not hold.
    ///
    /// # Errors
    ///
    /// [`Suspended`] whenever the run must stop here — a recorded branch this image cannot
    /// replay, a gate history recorded under another number, or a journal with no room for
    /// the marker. Nothing about *why* travels in it;
    /// [`DriveError`](crate::DriveError) is the driver's answer.
    fn gate(&mut self, gate: GateId) -> Result<u16, Suspended>;

    /// §10's `continue_as_new`: retire this run and install a new one over `input`.
    ///
    /// It returns [`Suspended`] and nothing else. The run that asked is over either way.
    ///
    /// # What this driver does
    ///
    /// A [`Driver`](crate::Driver) built with
    /// [`Driver::new`](crate::Driver::new) is pointed at a
    /// [`JournalRegion`](waymaker_flash::recovery::JournalRegion), not a bank, and refuses
    /// with [`DriveError::ContinueUnsupported`](crate::DriveError::ContinueUnsupported):
    /// §10's swap is `waymaker-flash`'s `swap` module, and it works on a *bank* — the
    /// two-bank layout, the authority the device booted, and the generation seal — none of
    /// which this shape knows. One built with
    /// [`Driver::at_bank`](crate::Driver::at_bank) performs the swap for real, reading
    /// that authority fresh from the device on this same boot. Issue
    /// [#110](https://github.com/madmax983/waymaker/issues/110).
    fn continue_as_new(&mut self, input: &[u8]) -> Suspended;

    /// Why [`wait`](Self::wait) answered [`Suspended`], when it was this call and the
    /// deadline had not passed yet.
    ///
    /// [`None`] for every other reason, and after every other method. It exists for one
    /// caller: an async façade that arms a hardware alarm instead of asking again straight
    /// away. Issue [#110](https://github.com/madmax983/waymaker/issues/110).
    fn deadline_remaining(&self) -> Option<(ClockKind, u64)>;
}
