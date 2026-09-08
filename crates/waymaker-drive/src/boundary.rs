//! The workflow's half of design document §06's explicit kernel boundary.
//!
//! Two methods, and they are the whole vocabulary a workflow has: ask for an effect, or
//! wait for a deadline, and get either the answer or an instruction to stop. There is no
//! `Future` here and no executor — a synchronous workflow suspends by propagating
//! [`Suspended`] with `?`, which is what `.await` does in the façade one layer up.

use waymaker_core::timer::TimerSpec;
use waymaker_core::{ActivityKind, Outcome};

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
}
