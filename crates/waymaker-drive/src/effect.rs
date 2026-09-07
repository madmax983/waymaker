//! Design document §07's durable effect protocol, as three types.
//!
//! Seven steps. Steps 1 to 3 make the intent durable, step 4 dispatches the activity, and
//! steps 5 to 7 make the result replayable. The order is the guarantee: §02 decision 3 says
//! a physical effect never precedes its committed intent.
//!
//! # What makes step 4 unreachable
//!
//! [`DurableIntent`] is the only value a dispatch accepts, and its field is private. This
//! module builds one in two bodies only. [`Effect::schedule`] builds one after step 3's
//! commit barrier returned. `Effect::redelivering` builds one for a schedule record that an
//! earlier boot committed; it is `pub(crate)`, because its evidence is the kernel's
//! `Resolve::Redeliver` rather than anything this module can see.
//!
//! So no caller outside this crate can name the identity of an effect it has not committed,
//! and inside it the one exception is one function with one caller.
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
/// Step 4 accepts no other proof.
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
    /// A retry and a reset both redeliver this pair, so a downstream service that
    /// deduplicates on it sees a repeat rather than a second effect. This pair is all that
    /// Waymaker promises here; [`Activities`](crate::Activities) states what it does not.
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
    /// Recorded as a failure with no payload. The run continues, and the workflow sees no
    /// part of the answer.
    ///
    /// The two alternatives are worse. A truncation records a short result and replays it
    /// for ever. A refusal strands the run: §08 has no edge from an unresolved effect to a
    /// terminal record, so the run can never end.
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

    /// An effect whose schedule record was committed in an earlier boot.
    ///
    /// §08's redelivery row. Committed history holds the schedule record and no outcome, so
    /// steps 1 to 3 already happened and this writes nothing.
    ///
    /// # Why it is not public
    ///
    /// It mints a proof from a sequence number. The evidence is the kernel's
    /// `Resolve::Redeliver`, which the driver reads and this function cannot see, so in any
    /// hand but that one it is a forge. `pub(crate)` confines the trust to the one caller.
    /// See
    /// [what is not checked](https://github.com/madmax983/waymaker/blob/main/CLAUDE.md#what-is-not-checked).
    #[must_use]
    pub(crate) const fn redelivering(self, seq: EffectSeq) -> Dispatchable<C> {
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

#[cfg(test)]
mod tests {
    use waymaker_core::{ActivityKind, EffectSeq, RunId};
    use waymaker_fault::Device;
    use waymaker_flash::append::Journal;
    use waymaker_flash::bank::BankLayout;
    use waymaker_flash::capacity::{Bounds, Reserve, Reserved};
    use waymaker_flash::frame::ProgramAlign;
    use waymaker_flash::recovery::{JournalRegion, Recovery};
    use waymaker_flash::storage::Geometry;

    use super::{Effect, Resolution};

    const RUN: RunId = RunId(0x0BAD_F00D_1234_5678);

    const BOUNDS: Bounds = Bounds {
        run_input_bytes: 4,
        effect_result_bytes: 8,
        terminal_bytes: 8,
    };

    fn geometry() -> Geometry {
        let Ok(geometry) = Geometry::new(1024, 512, 4, 1) else {
            unreachable!("1024 is two whole 512-byte blocks of 4-byte units")
        };
        geometry
    }

    fn region() -> JournalRegion {
        let Some(align) = ProgramAlign::new(4) else {
            unreachable!("4 is a power of two within the program-size range")
        };
        let Ok(region) = JournalRegion::spanning(geometry(), 0, 512, align) else {
            unreachable!("the region is the device's first erase block")
        };
        region
    }

    fn writer(storage: &mut Device) -> Reserved {
        let Ok(layout) = BankLayout::new(geometry()) else {
            unreachable!("this geometry holds two erase blocks")
        };
        let Ok(reserve) = Reserve::for_layout(BOUNDS, layout) else {
            unreachable!("these bounds fit this layout")
        };
        let mut recovery = Recovery::new(region());
        let mut page = [0_u8; 128];
        while recovery.next(storage, &mut page).is_some() {}
        let Some(journal) = Journal::after(recovery) else {
            unreachable!("an erased journal has an append point")
        };
        let Ok(reserved) = Reserved::over(journal, reserve) else {
            unreachable!("the reserve fits this journal")
        };
        reserved
    }

    /// `redelivering` is `pub(crate)`, so this is the only place it can be driven directly.
    /// The end-to-end path is `crates/waymaker-drive/tests/drive.rs`.
    #[test]
    fn a_redelivered_intent_writes_nothing_and_keeps_the_committed_identity() {
        let mut device = Device::new(geometry());
        let mut page = [0_u8; 128];
        let request = waymaker_core::EffectRequest {
            kind: ActivityKind(1),
            input_len: 3,
            input_crc: 0x0BAD_F00D,
        };
        let scheduled = Effect::over(RUN, writer(&mut device))
            .schedule(&mut device, EffectSeq(0), request, &mut page)
            .expect("the schedule fits the journal");
        let resolved = scheduled
            .dispatch
            .resolve(&mut device, Resolution::Completed(b"ok"), &mut page)
            .expect("the outcome fits");
        let before = device.image().to_vec();

        let redelivered = resolved.next.redelivering(EffectSeq(0));

        assert_eq!(redelivered.intent().id().run, RUN);
        assert_eq!(redelivered.intent().id().seq, EffectSeq(0));
        assert_eq!(device.image(), before.as_slice());
    }
}
