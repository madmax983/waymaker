//! The durable half of the protocol, as the façade needs it.
//!
//! Design document §06's explicit kernel boundary, with one call per step the façade must
//! sequence. Everything here is a *question* the façade asks. Nothing here writes: an
//! implementor owns the media, the transition table, and what a record means.
//!
//! # Why the façade needs the split
//!
//! §07 puts the world's work at step 4, between two durable halves. A trait with one
//! `call` method would have to perform the effect itself, and the façade could then only
//! wrap it. [`Journal::schedule`] stops at step 3 and [`Journal::resolve`] takes steps 5 to
//! 7, so the façade can `.await` the world in between. That is the whole reason [`Ctx`] is
//! generic over a dispatcher as well as a journal.
//!
//! [`Ctx`]: crate::ctx::Ctx

use waymaker_core::timer::TimerSpec;
use waymaker_core::{ActivityKind, EffectId, Outcome};

/// The run cannot go on in this boot.
///
/// It carries no reason. A workflow cannot tell a wait, a finished run and a refusal apart.
/// If it could, it would guess at history. The caller that drove the boot asks the journal
/// what happened instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Halted;

/// What the journal says about one activity boundary, after §07 step 3.
///
/// There is no third shape for "not durable yet". A journal that cannot commit the intent
/// answers [`Halted`], so the value that reaches the world exists only after the schedule
/// record is durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handoff<'a> {
    /// History holds the outcome. Nothing is dispatched.
    Replayed(Outcome<'a>),
    /// The intent is durable. Perform the effect under `id`, then call
    /// [`Journal::resolve`].
    ///
    /// The same identity arrives again after a retry or a reset. A downstream system that
    /// must not repeat the effect deduplicates on this pair.
    Dispatch(EffectId),
}

/// What the world answered for the effect [`Journal::schedule`] handed out.
///
/// The bytes point into the caller's buffer. The journal copies what it records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Answer<'a> {
    /// Success, within the run's declared bound.
    Completed(&'a [u8]),
    /// Failure, within the run's declared bound.
    Failed(&'a [u8]),
    /// The answer is wider than the buffer. It is recorded as a failure with no payload.
    ///
    /// The two alternatives are worse. A truncation records a short answer and replays it
    /// for ever. A refusal strands the run, because §08 has no edge from an unresolved
    /// effect to a terminal record.
    Exhausted,
}

/// The durable half of the protocol.
///
/// An implementor owns the media and the transition table. The façade owns neither.
pub trait Journal {
    /// §08's answer for one activity boundary, up to and including §07 step 3.
    ///
    /// # Postconditions
    ///
    /// On [`Handoff::Dispatch`] the schedule record survives a reset. On
    /// [`Handoff::Replayed`] nothing was written and nothing may be dispatched.
    ///
    /// # Errors
    ///
    /// [`Halted`] when the run must stop here.
    fn schedule(&mut self, kind: ActivityKind, input: &[u8]) -> Result<Handoff<'_>, Halted>;

    /// §07 steps 5 to 7 for the effect [`schedule`](Self::schedule) handed out.
    ///
    /// # Postconditions
    ///
    /// On [`Ok`] the outcome is replayable, and not before.
    ///
    /// # Errors
    ///
    /// [`Halted`] when the run must stop here.
    fn resolve(&mut self, answer: Answer<'_>) -> Result<Outcome<'_>, Halted>;

    /// §11's deadline boundary: record the intent, then say whether the deadline passed.
    ///
    /// # Errors
    ///
    /// [`Halted`] when the run must stop here, including the ordinary case that the
    /// deadline has not passed yet.
    fn wait(&mut self, spec: TimerSpec) -> Result<(), Halted>;

    /// §10's `continue_as_new`: retire this run and install a new one over `input`.
    ///
    /// It returns [`Halted`] and nothing else. The run that asked is over either way — the
    /// journal replaced it, or the journal refused — and a run that is over has no answer
    /// to receive.
    fn continue_as_new(&mut self, input: &[u8]) -> Halted;
}
