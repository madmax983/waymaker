//! The workflow's half of design document §06's explicit kernel boundary.
//!
//! Two methods, and they are the whole vocabulary a workflow has: ask for an effect, or
//! wait for a deadline, and get either the answer or an instruction to stop. There is no
//! `Future` here and no executor — a synchronous workflow suspends by propagating
//! [`Suspended`] with `?`, which is what `.await` does in the façade one layer up.

use waymaker_core::timer::TimerSpec;
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
/// So that only the driver can make one. A workflow that could build a `Suspended` could
/// stop a run that nothing asked to stop, and the driver would then report a wait that no
/// activity is behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Suspended(());

impl Suspended {
    /// The one value, made by the driver only.
    pub(crate) const NEW: Self = Self(());
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
/// So that removing the façade removes nothing here. This crate names no `waymaker-embassy`
/// type, which is what makes "the protocol is fully usable through the synchronous driver"
/// a fact about the dependency graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff<'a> {
    /// History holds the outcome. Nothing may be dispatched.
    Replayed(Outcome<'a>),
    /// The intent is durable. Perform the effect under `id`, then call
    /// [`Boundary::resolve`].
    Dispatch(EffectId),
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

    /// §10's `continue_as_new`: retire this run and install a new one over `input`.
    ///
    /// It returns [`Suspended`] and nothing else. The run that asked is over either way.
    ///
    /// # What this driver does
    ///
    /// It refuses, with
    /// [`DriveError::ContinueUnsupported`](crate::DriveError::ContinueUnsupported). §10's
    /// swap is `waymaker-flash`'s `swap` module, and it works on a *bank*: it needs the
    /// two-bank layout, the authority the device booted, and the generation seal.
    /// [`Driver`](crate::Driver) is pointed at a
    /// [`JournalRegion`](waymaker_flash::recovery::JournalRegion) and knows none of them,
    /// so a swap here would be a swap of a bank this driver cannot name. Issue
    /// [#36](https://github.com/madmax983/waymaker/issues/36)'s dispatcher is where the two
    /// are joined.
    fn continue_as_new(&mut self, input: &[u8]) -> Suspended;
}
