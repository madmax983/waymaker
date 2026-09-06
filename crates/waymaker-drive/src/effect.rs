//! Design document §07's durable effect protocol, as three types.
//!
//! Seven steps. Steps 1 to 3 make the intent durable, step 4 dispatches the activity, and
//! steps 5 to 7 make the result replayable. The order is the guarantee: §02 decision 3 says
//! a physical effect never precedes its committed intent.
//!
//! # What makes step 4 unreachable
//!
//! [`DurableIntent`] is the only value a dispatch accepts, its field is private, and this
//! module builds one in two bodies only — [`Effect::schedule`], after step 3's commit
//! barrier returned, and [`Effect::redelivering`], for a schedule record that committed
//! before this boot. A driver cannot name the identity of an effect it has not committed.
//!
//! # Why this crate
//!
//! `waymaker-flash` owns steps 1 to 3 and 5 to 7, and must not own activities. Step 4 is an
//! activity. The protocol that joins them therefore sits above the layers, beside the
//! driver that runs it.

use waymaker_core::{EffectId, EffectRequest, EffectSeq, Outcome, RecordRef, RunId};
use waymaker_flash::capacity::{Reserved, ReservedError};
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};
use waymaker_flash::storage::StableStorage;

use crate::drive::DriveError;

/// Proof that §07 step 3 completed for one effect.
///
/// Step 4 takes this and nothing else.
///
/// # Why the field is private
///
/// So that a dispatch before a durable intent does not compile:
///
/// ```compile_fail,E0451
/// use waymaker_core::{EffectId, EffectSeq, RunId};
/// use waymaker_drive::DurableIntent;
///
/// let forged = DurableIntent {
///     id: EffectId { run: RunId(1), seq: EffectSeq(0) },
/// };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DurableIntent {
    id: EffectId,
}

impl DurableIntent {
    /// The stable `(RunId, EffectSeq)` step 4 dispatches under.
    ///
    /// A reset redelivers the same pair, so a downstream service that deduplicates on it
    /// sees a repeat rather than a second effect.
    #[must_use]
    pub const fn id(self) -> EffectId {
        self.id
    }
}

/// What an activity produced.
///
/// The payload bound is the one the run declared in `Bounds::effect_result_bytes`. There is
/// no second bound: the driver hands an activity a buffer of exactly that width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Resolution<'a> {
    /// Success, within the bound.
    Completed(&'a [u8]),
    /// Failure, within the bound.
    Failed(&'a [u8]),
    /// The answer is longer than the bound.
    ///
    /// Recorded as a failure with no payload. The run continues and the workflow sees no
    /// part of the answer. The two alternatives are both worse: a truncation records a
    /// short result and replays it for ever, and a refusal strands the run, because §08 has
    /// no edge from an unresolved effect to a terminal record.
    ///
    /// An empty payload is the only payload that fits every bound, `0` included. So an
    /// exhausted effect and an effect that failed with no detail are the same record, which
    /// is stated rather than hidden.
    Exhausted,
}

impl<'a> Resolution<'a> {
    /// What the workflow observes, after step 7's barrier.
    ///
    /// The bytes are the bytes the record holds. That is what keeps a partial answer out of
    /// a workflow: [`Exhausted`](Self::Exhausted) records nothing and shows nothing.
    ///
    /// Private, because §07 says the workflow may observe the result only after step 7's
    /// barrier returns. [`Resolved`] carries the only copy a caller can reach, and only
    /// [`Dispatchable::resolve`] makes one.
    const fn outcome(self) -> Outcome<'a> {
        match self {
            Self::Completed(result) => Outcome::Completed(result),
            Self::Failed(error) => Outcome::Failed(error),
            Self::Exhausted => Outcome::Failed(&[]),
        }
    }

    /// The record step 5 writes.
    const fn record(self, seq: EffectSeq) -> RecordRef<'a> {
        match self {
            Self::Completed(result) => RecordRef::EffectCompleted { seq, result },
            Self::Failed(error) => RecordRef::EffectFailed { seq, error },
            Self::Exhausted => RecordRef::EffectFailed { seq, error: &[] },
        }
    }
}

/// What §07 step 3 produced.
///
/// The record travels beside the dispatchable effect because the caller has to tell the
/// kernel what was written.
#[derive(Debug, PartialEq, Eq)]
pub struct Scheduled<C: IntegrityCheck = Catalogued> {
    /// The effect, now that step 4 is legal.
    pub dispatch: Dispatchable<C>,
    /// The schedule record on media.
    pub record: RecordRef<'static>,
}

/// What §07 step 7 produced.
///
/// The outcome is here and nowhere else, which is §07's last sentence as a type: the
/// workflow may observe the result only after step 7's barrier returns.
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved<'a, C: IntegrityCheck = Catalogued> {
    /// The protocol, ready for the next effect.
    pub next: Effect<C>,
    /// The outcome record on media.
    pub record: RecordRef<'a>,
    /// What the workflow may now observe.
    pub outcome: Outcome<'a>,
}

/// §07 over one run's journal, between effects.
///
/// It holds §10's gated writer, so every record this protocol appends is admitted by the
/// reserve first.
#[derive(Debug, PartialEq, Eq)]
pub struct Effect<C: IntegrityCheck = Catalogued> {
    run: RunId,
    writer: Reserved<C>,
}

impl<C: IntegrityCheck> Effect<C> {
    /// The protocol over `writer`, for `run`.
    ///
    /// The run is taken here because §09 keeps the run id in the bank header rather than in
    /// every record, so the journal cannot say which run it is.
    #[must_use]
    pub const fn over(run: RunId, writer: Reserved<C>) -> Self {
        Self { run, writer }
    }

    /// The writer back, for a record that is not an effect.
    #[must_use]
    pub const fn into_writer(self) -> Reserved<C> {
        self.writer
    }

    /// §07 steps 1, 2 and 3: the schedule frame, the payload barrier, and the seal.
    ///
    /// Nothing physical has happened when this returns. The record is returned beside the
    /// dispatchable effect because the caller has to tell the kernel what was written.
    ///
    /// # Postconditions
    ///
    /// On [`Ok`] the schedule record survives a reset, and the returned value is the only
    /// thing step 4 accepts.
    ///
    /// # Errors
    ///
    /// [`DriveError::Capacity`] when §10 refuses the record, before the device is touched.
    /// [`DriveError::Append`] when the device refuses it.
    pub fn schedule<S: StableStorage>(
        mut self,
        storage: &mut S,
        seq: EffectSeq,
        request: EffectRequest,
        page: &mut [u8],
    ) -> Result<Scheduled<C>, DriveError<S::Error>> {
        let record = RecordRef::EffectScheduled {
            seq,
            kind: request.kind,
            input_len: request.input_len,
            input_crc: request.input_crc,
        };
        self.writer
            .stage(storage, &record, page)
            .map_err(refusal)?
            .payload_barrier(storage)
            .map_err(DriveError::Append)?
            .commit(storage)
            .map_err(DriveError::Append)?;
        Ok(Scheduled {
            dispatch: Dispatchable {
                intent: DurableIntent {
                    id: EffectId { run: self.run, seq },
                },
                writer: self.writer,
            },
            record,
        })
    }

    /// An intent that committed before this boot.
    ///
    /// §08's redelivery row: committed history holds the schedule record and no outcome, so
    /// steps 1 to 3 are already behind us and this writes nothing. The kernel is what says
    /// so — it answers `Resolve::Redeliver` — and this takes its word. See
    /// [what is not checked](https://github.com/madmax983/waymaker/blob/main/CLAUDE.md#what-is-not-checked).
    #[must_use]
    pub const fn redelivering(self, seq: EffectSeq) -> Dispatchable<C> {
        Dispatchable {
            intent: DurableIntent {
                id: EffectId { run: self.run, seq },
            },
            writer: self.writer,
        }
    }
}

/// An effect whose intent is durable: step 4 is legal, and steps 5 to 7 end it.
#[derive(Debug, PartialEq, Eq)]
pub struct Dispatchable<C: IntegrityCheck = Catalogued> {
    intent: DurableIntent,
    writer: Reserved<C>,
}

impl<C: IntegrityCheck> Dispatchable<C> {
    /// What step 4 dispatches under.
    #[must_use]
    pub const fn intent(&self) -> DurableIntent {
        self.intent
    }

    /// §07 steps 5, 6 and 7: the outcome frame, the payload barrier, and the seal.
    ///
    /// # Postconditions
    ///
    /// On [`Ok`] the result is replayable, and not before. The [`Resolved`] carries the only
    /// outcome a caller can reach, so nothing observes the answer earlier.
    ///
    /// # Errors
    ///
    /// [`DriveError::Capacity`] when §10 refuses the record — a payload over the run's
    /// declared bound included — before the device is touched. [`DriveError::Append`] when
    /// the device refuses it.
    pub fn resolve<'a, S: StableStorage>(
        mut self,
        storage: &mut S,
        resolution: Resolution<'a>,
        page: &mut [u8],
    ) -> Result<Resolved<'a, C>, DriveError<S::Error>> {
        let record = resolution.record(self.intent.id.seq);
        self.writer
            .stage(storage, &record, page)
            .map_err(refusal)?
            .payload_barrier(storage)
            .map_err(DriveError::Append)?
            .commit(storage)
            .map_err(DriveError::Append)?;
        Ok(Resolved {
            next: Effect {
                run: self.intent.id.run,
                writer: self.writer,
            },
            record,
            outcome: resolution.outcome(),
        })
    }
}

/// §10's two refusals, as the driver reports them.
fn refusal<E>(error: ReservedError<E>) -> DriveError<E> {
    match error {
        ReservedError::Capacity(refused) => DriveError::Capacity(refused),
        ReservedError::Append(error) => DriveError::Append(error),
    }
}
