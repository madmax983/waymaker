//! The workflow's half of design document §06's explicit kernel boundary.
//!
//! One method, and it is the whole vocabulary a workflow has: ask for an effect, and get
//! either its outcome or an instruction to stop. There is no `Future` here and no executor
//! — a synchronous workflow suspends by propagating [`Suspended`] with `?`, which is what
//! `.await` does in the façade one layer up.

use waymaker_core::{ActivityKind, Outcome};

/// The run cannot continue now. Return it.
///
/// Three different things produce it, and a workflow may not tell them apart: the run is
/// waiting for an activity, history says the run already ended, or the driver met an error.
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
    /// The returned bytes borrow the driver's scratch page, and the *next* call overwrites
    /// it. A workflow that needs a result after its next boundary copies it into its own
    /// storage first. That is not a convention: the borrow is derived from `&mut self`, so
    /// holding one across the next call does not compile.
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
}
