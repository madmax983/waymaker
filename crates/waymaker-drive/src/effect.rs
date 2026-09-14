//! Design document §07's durable effect protocol, as three types.
//!
//! Seven steps. Steps 1 to 3 make the intent durable, step 4 dispatches the activity, and
//! steps 5 to 7 make the result replayable. The order is the guarantee: §02 decision 3 says
//! a physical effect never precedes its committed intent.
//!
//! # What makes step 4 unreachable
//!
//! [`DurableIntent`] is the only value a dispatch accepts, and its fields are private. This
//! module builds one in two bodies only. [`Effect::schedule`] builds one after step 3's
//! commit barrier returned. `Effect::redelivering` builds one for a schedule record that an
//! earlier boot committed; it is `pub(crate)`, because its evidence is the kernel's
//! `Resolve::Redeliver` rather than anything this module can see.
//!
//! So no caller outside this crate can name the identity of an effect it has not committed,
//! and inside it the one exception is one function with one caller.
//!
//! # What makes step 4 unmistakable
//!
//! A [`DurableIntent`] carries the kind and the input digest, not only the sequence, so
//! [`Activities::perform`] reads the kind from `intent` and has no second argument to read a
//! different one from. [`Dispatchable::perform`] is the one route from a proof and raw bytes
//! to a dispatched effect. It checks the bytes against the digest, then wraps them in a
//! [`CheckedInput`] — a type with a private field, built nowhere else. So a caller cannot use
//! effect A's identity to dispatch effect B's kind, and cannot reach `Activities::perform`
//! with effect B's input either, because there is no way to build the value that argument
//! takes except by passing this check first. See
//! [issue #92](https://github.com/madmax983/waymaker/issues/92).
//!
//! # Why this crate
//!
//! `waymaker-flash` owns steps 1 to 3 and 5 to 7, and must not own activities. Step 4 is an
//! activity. The protocol that joins them therefore sits above the layers, beside the
//! driver that runs it.

use waymaker_core::{ActivityKind, EffectId, EffectRequest, EffectSeq, Outcome, RecordRef, RunId};
use waymaker_flash::capacity::{Reserved, ReservedError};
use waymaker_flash::frame;
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};
use waymaker_flash::storage::StableStorage;

use crate::activity::{Activities, Performed};
use crate::drive::DriveError;

/// Proof that §07 step 3 completed, for one effect and its request.
///
/// Step 4 accepts no other proof. It carries the [`EffectRequest`] step 3 committed. So the
/// kind an activity dispatches under cannot be some other kind. There is no second way to
/// name one. See [`Dispatchable::perform`] for the same guarantee over the input bytes.
///
/// # Why the field is private
///
/// So that a dispatch before a durable intent does not compile:
///
/// ```compile_fail,E0451
/// use waymaker_core::{ActivityKind, EffectId, EffectRequest, EffectSeq, RunId};
/// use waymaker_drive::DurableIntent;
///
/// let forged = DurableIntent {
///     id: EffectId { run: RunId(1), seq: EffectSeq(0) },
///     request: EffectRequest { kind: ActivityKind(1), input_len: 0, input_crc: 0 },
/// };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DurableIntent {
    id: EffectId,
    request: EffectRequest,
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

    /// Which activity step 3 scheduled.
    ///
    /// The one kind [`Dispatchable::perform`] may dispatch this identity under. There is no
    /// second argument an implementor or a caller can read a different kind from.
    #[must_use]
    pub const fn kind(self) -> ActivityKind {
        self.request.kind
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
            .payload_barrier()
            .map_err(DriveError::Append)?
            .commit()
            .map_err(DriveError::Append)?;
        Ok(Scheduled {
            dispatch: Dispatchable {
                intent: DurableIntent {
                    id: EffectId { run: self.run, seq },
                    request,
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
    /// `request` is the caller's *current* call. [`waymaker_core::ReplayMachine::intent`]
    /// already checked it against the schedule record — a replay that disagreed would have
    /// stopped there. Passing `request` through binds the redelivered identity to the kind
    /// and input the schedule record names. No second read of media is needed.
    ///
    /// # Why it is not public
    ///
    /// It mints a proof from a sequence number. The evidence that the sequence is real is
    /// the kernel's `Resolve::Redeliver`, which the driver reads and this function cannot
    /// see, so in any hand but that one it is a forge. `pub(crate)` confines the trust to
    /// the one caller. See
    /// [what is not checked](https://github.com/madmax983/waymaker/blob/main/CLAUDE.md#what-is-not-checked).
    #[must_use]
    pub(crate) const fn redelivering(
        self,
        seq: EffectSeq,
        request: EffectRequest,
    ) -> Dispatchable<C> {
        Dispatchable {
            intent: DurableIntent {
                id: EffectId { run: self.run, seq },
                request,
            },
            writer: self.writer,
        }
    }
}

/// Step 4 was asked for `input` that disagrees with what step 3 committed.
///
/// [`Dispatchable::perform`] refuses before the activity runs. A caller offered bytes other
/// than the ones the identity was scheduled with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputMismatch;

/// Bytes [`Dispatchable::perform`] has checked against the recorded digest.
///
/// The only way to build one is inside [`Dispatchable::perform`], after the check passes.
/// Its field is private, so no caller — inside this crate or outside it — can hand
/// [`Activities::perform`] bytes the digest never vouched for.
///
/// ```compile_fail,E0451
/// use waymaker_drive::CheckedInput;
///
/// let forged = CheckedInput { bytes: b"anything" };
/// ```
#[derive(Clone, Copy, Debug)]
pub struct CheckedInput<'a> {
    bytes: &'a [u8],
}

impl<'a> CheckedInput<'a> {
    /// The checked bytes.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
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

    /// §07 step 4: ask `activities` to perform this identity's effect.
    ///
    /// Checks `input` against what step 3 committed, before `activities` ever sees it. This
    /// is the whole guarantee [`DurableIntent`] states. The kind cannot be a different kind:
    /// there is no second argument to read one from. The input cannot be different bytes
    /// either: [`Activities::perform`] takes a [`CheckedInput`], and this is the only place
    /// that builds one. A caller cannot skip the check by calling `Activities::perform`
    /// directly, because it has no bytes to pass it that were not checked here first.
    ///
    /// # Errors
    ///
    /// [`InputMismatch`] when `input`'s length or digest disagrees with the request step 3
    /// recorded. `activities` is not called.
    pub fn perform<A: Activities>(
        &self,
        activities: &mut A,
        input: &[u8],
        out: &mut [u8],
    ) -> Result<Performed, InputMismatch> {
        let request = self.intent.request;
        if input.len() != usize::from(request.input_len)
            || frame::input_digest_with::<C>(input) != request.input_crc
        {
            return Err(InputMismatch);
        }
        Ok(activities.perform(self.intent, CheckedInput { bytes: input }, out))
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
            .payload_barrier()
            .map_err(DriveError::Append)?
            .commit()
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

    use super::{Effect, InputMismatch, Resolution};
    use crate::activity::{Activities, Performed};
    use crate::effect::{CheckedInput, DurableIntent};

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
        let mut recovery = Recovery::new(region(), storage);
        let mut page = [0_u8; 128];
        while recovery.next(&mut page).is_some() {}
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

        let redelivered = resolved.next.redelivering(EffectSeq(0), request);

        assert_eq!(redelivered.intent().id().run, RUN);
        assert_eq!(redelivered.intent().id().seq, EffectSeq(0));
        assert_eq!(redelivered.intent().kind(), request.kind);
        assert_eq!(device.image(), before.as_slice());
    }

    /// A world that records what it was asked and always completes with `b"ok"`.
    #[derive(Default)]
    struct Recording {
        asked: Option<(DurableIntent, [u8; 3])>,
    }

    impl Activities for Recording {
        fn perform(
            &mut self,
            intent: DurableIntent,
            input: CheckedInput<'_>,
            out: &mut [u8],
        ) -> Performed {
            let input = input.bytes();
            let mut bytes = [0_u8; 3];
            let taken = input.len().min(bytes.len());
            if let (Some(from), Some(into)) = (input.get(..taken), bytes.get_mut(..taken)) {
                into.copy_from_slice(from);
            }
            self.asked = Some((intent, bytes));
            let answer = b"ok";
            if let Some(dst) = out.get_mut(..answer.len()) {
                dst.copy_from_slice(answer);
            }
            Performed::Completed(answer.len())
        }
    }

    /// `Dispatchable::perform` is the guarantee issue #92 asks for. The kind travels with
    /// the identity: `Activities::perform` has no separate argument to read one from. The
    /// input is checked against what step 3 recorded, before the world ever sees it.
    #[test]
    fn perform_refuses_input_that_disagrees_with_what_was_scheduled() {
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

        let mut world = Recording::default();
        let mut out = [0_u8; 8];
        let refused = scheduled.dispatch.perform(&mut world, b"xyz", &mut out);

        assert_eq!(refused, Err(InputMismatch));
        assert_eq!(world.asked, None, "a refused input never reaches the world");
    }

    #[test]
    fn perform_binds_the_kind_and_forwards_matching_input() {
        let mut device = Device::new(geometry());
        let mut page = [0_u8; 128];
        let kind = ActivityKind(7);
        let input = b"abc";
        let request = waymaker_core::EffectRequest {
            kind,
            input_len: 3,
            input_crc: waymaker_flash::frame::input_digest_with::<
                waymaker_flash::integrity::Catalogued,
            >(input),
        };
        let scheduled = Effect::over(RUN, writer(&mut device))
            .schedule(&mut device, EffectSeq(0), request, &mut page)
            .expect("the schedule fits the journal");

        let mut world = Recording::default();
        let mut out = [0_u8; 8];
        let performed = scheduled.dispatch.perform(&mut world, input, &mut out);

        assert_eq!(performed, Ok(Performed::Completed(2)));
        let (asked_intent, asked_input) = world.asked.expect("the world was asked");
        assert_eq!(asked_intent.kind(), kind);
        assert_eq!(&asked_input, input);
    }
}
