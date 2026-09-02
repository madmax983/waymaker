//! The streaming replay cursor: history consumed forwards, one record at a time.
//!
//! Design document §02 decision 2, §06 Cold-start replay, §08 Replay and determinism. A
//! workflow is re-created from its beginning after reboot and deterministically replayed
//! through an ordered journal. The cursor is the position in that journal: it advances in
//! the order the workflow encounters effect boundaries, so replay is constant-memory and
//! needs no random reads.
//!
//! # What this module owns
//!
//! [`ReplayCursor`] — the position, the run's effect identity, and the rules that say
//! which record may legally follow which. [`Position`] is where the cursor stands,
//! [`PendingEffect`] is the first unresolved effect when there is one, and [`Step`] is what
//! a record meant once the cursor placed it in the run's order.
//!
//! # What this module must not own
//!
//! Bytes, and the workflow. It reads no media — the caller decodes a record into its own
//! scratch page and hands the borrowed view over — and it computes no digest, because a
//! kernel that hashed an activity input would be a kernel that owns a CRC. It also holds no
//! opinion about what a *workflow* asked for: whether a request matches what history
//! recorded is design document §08's transition table, which is issue #15's. This cursor
//! validates history against itself.
//!
//! # The boundary this module exists to defend
//!
//! Two of them.
//!
//! The first is `replay-is-sequential`. There is no `Journal::get(id)` here, and no method
//! that takes an [`EffectSeq`] or an [`EffectId`] as a
//! key to look something up by. The only way a record reaches the cursor is
//! [`advance`](ReplayCursor::advance), and the only order it can arrive in is the order it
//! was written. An index would be RAM proportional to history, on a device whose whole
//! runtime budget is 768 bytes.
//!
//! The second is the scratch page. [`ReplayCursor`] has no lifetime parameter: it *cannot*
//! hold a borrow of the caller's page, so one 512-byte page is enough however long history
//! is. `advance` returns a [`Step`] whose lifetime comes from the record it was handed and
//! not from `&mut self`, so the caller is free to overwrite the page the instant it is done
//! with the step — and the integration tests do exactly that, between every record.
//!
//! # Prefix safety
//!
//! §14 promises that "recovery exposes only a legal prefix of committed records". So the
//! cursor stops at the first record that could not legally follow the ones before it, and
//! it stays stopped: [`Position::Halted`] holds the failure that stopped it, and every
//! later call reports the same one. There is no path from `Halted` back to a running
//! state, which is the same discipline
//! [`EffectIdAllocator`] applies to exhaustion — a refusal that
//! can be forgotten is a refusal that will be.

use crate::activity::ActivityKind;
use crate::error::KernelError;
use crate::id::{EffectId, EffectIdAllocator, EffectSeq, RunId};
use crate::record::RecordRef;

/// An effect whose durable intent is committed and whose outcome is not.
///
/// This is what §06's cold-start sequence calls "the first unresolved effect", and it is
/// the only effect a recovered run can have one of: history is sequential, so an effect is
/// scheduled only after the previous one's outcome is committed.
///
/// # Invariants
///
/// * `id.run` is the cursor's run. The run id lives once, in the bank header (§07), so the
///   record on media carried only the sequence; pairing the two is the cursor's job and is
///   why a caller never has to remember which run it is replaying.
/// * `input_len` and `input_crc` are the digest the schedule record recorded, moved rather
///   than recomputed. §08's divergence check compares them against what replay asks for;
///   computing either here would put a CRC in the crate whose must-not-own cell names one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PendingEffect {
    /// The stable identity a dispatcher deduplicates on, and a redelivery reuses.
    pub id: EffectId,
    /// Which activity the schedule record committed.
    pub kind: ActivityKind,
    /// How many bytes of input the recorded call passed.
    pub input_len: u16,
    /// The digest of those bytes, as history recorded it.
    pub input_crc: u32,
}

/// Where the cursor stands in history.
///
/// Six positions, and the reachable transitions between them are the whole of what a legal
/// history is:
///
/// ```text
/// BeforeRun --RunStarted--> Replaying --EffectScheduled--> AwaitingOutcome
///                               ^                                |
///                               +---EffectCompleted/Failed-------+
///                               |
///                               +---RunCompleted--> RunCompleted (terminal)
///                               +---RunFailed-----> RunFailed    (terminal)
///
/// any position --an illegal record--> Halted(error)   (sticky)
/// ```
///
/// Not `Ord`: a position is a place, not a magnitude.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Position {
    /// Nothing has been consumed. The next record must be
    /// [`RunStarted`](RecordRef::RunStarted).
    ///
    /// A fresh run is here too: the driver appends `RunStarted` and advances over it, so a
    /// run that has never been recovered and one whose journal is empty are the same
    /// position, which is what makes cold start and first start one code path.
    BeforeRun,
    /// The run started and every effect the cursor has seen is resolved.
    Replaying,
    /// A schedule is committed with no outcome: [`ReplayCursor::pending`] names it.
    AwaitingOutcome,
    /// A [`RunCompleted`](RecordRef::RunCompleted) record was consumed. Terminal.
    RunCompleted,
    /// A [`RunFailed`](RecordRef::RunFailed) record was consumed. Terminal.
    RunFailed,
    /// A record could not legally follow the ones before it, and recovery stopped.
    ///
    /// Sticky. The error is the *first* one, kept so that a driver reporting the fault
    /// reports the cause rather than whatever the next record happened to be.
    Halted(KernelError),
}

impl Position {
    /// Whether history ended the run, so nothing may follow and nothing may be polled.
    ///
    /// # Postconditions
    ///
    /// True for exactly [`RunCompleted`](Self::RunCompleted) and
    /// [`RunFailed`](Self::RunFailed). [`Halted`](Self::Halted) is deliberately *not*
    /// terminal in this sense: a run that stopped being recoverable is not a run that
    /// finished, and a driver that treated the two alike would report a corrupt journal as
    /// a successful workflow.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::RunCompleted | Self::RunFailed)
    }
}

/// What one record meant, once the cursor placed it in the run's order.
///
/// # Why this is not [`RecordRef`]
///
/// A `RecordRef` is what a record *is*; a `Step` is what a record *did to the run*. Two
/// differences carry that:
///
/// * a `Step` carries an [`EffectId`] where the record carried a bare
///   [`EffectSeq`]. The run id is in the bank header rather than in every
///   record (§07), and pairing them is the cursor's job.
/// * a `Step` can only exist where the record was legal. There is no `Step` for a
///   completion with no schedule before it, so a driver matching on one has no arm to write
///   for a history that cannot happen.
///
/// # Lifetimes
///
/// Every payload borrows the caller's scratch page, exactly as [`RecordRef`] does, and the
/// lifetime comes from the record rather than from the cursor. So a step is valid for
/// precisely as long as the page holds the record it came from — which is until the caller
/// reads the next one, and not one byte longer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step<'a> {
    /// The run began. §06 step 2: decode `input` into caller-owned storage before
    /// advancing, because the page it borrows is about to hold the next record.
    RunStarted {
        /// The workflow this run executes.
        workflow_kind: u16,
        /// The version of that workflow, which a firmware image may refuse to replay.
        workflow_version: u16,
        /// The run's input, opaque to the kernel.
        input: &'a [u8],
    },
    /// An effect's durable intent. Until its outcome arrives this is the run's first
    /// unresolved effect, and [`ReplayCursor::pending`] keeps naming it.
    EffectScheduled(PendingEffect),
    /// The effect completed, and `result` is what replay hands the workflow back.
    EffectCompleted {
        /// The effect this outcome resolves.
        id: EffectId,
        /// The activity's recorded result, opaque to the kernel.
        result: &'a [u8],
    },
    /// The effect failed, and `error` is the recorded failure payload.
    EffectFailed {
        /// The effect this outcome resolves.
        id: EffectId,
        /// The recorded failure payload, opaque to the kernel.
        error: &'a [u8],
    },
    /// The run finished successfully. Terminal: nothing may follow.
    RunCompleted {
        /// The workflow's recorded result, opaque to the kernel.
        result: &'a [u8],
    },
    /// The run failed. Terminal: nothing may follow.
    RunFailed {
        /// The recorded failure payload, opaque to the kernel.
        error: &'a [u8],
    },
}

/// The schedule the cursor is holding an outcome open for.
///
/// Twelve bytes, and deliberately not a [`PendingEffect`]: the run id is already in the
/// allocator, and storing it twice would charge the 128-byte kernel-state budget for a
/// number the cursor cannot disagree with itself about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scheduled {
    seq: EffectSeq,
    kind: ActivityKind,
    input_len: u16,
    input_crc: u32,
}

/// The cursor's position, with what each position needs to remember.
///
/// The public [`Position`] is a projection of this. They are separate types because the
/// pending schedule is state the cursor keeps and not something a caller should have to
/// destructure a position to reach — and because `Position` stays small, `Copy` and
/// comparable, which is what a driver's `match` wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    BeforeRun,
    Replaying,
    AwaitingOutcome(Scheduled),
    RunCompleted,
    RunFailed,
    Halted(KernelError),
}

/// A position in one run's committed history, advanced one record at a time.
///
/// This is live kernel state for the whole of a run, so it is registered in
/// [`kernel_state_types!`](crate::budget) and charged against the 128-byte budget in design
/// document §04. It replaced [`EffectIdAllocator`] in that registry rather than joining it:
/// the cursor *contains* the allocator, and registering both would double-count the same
/// bytes.
///
/// # Invariants
///
/// * The run never changes.
/// * The contained allocator's next sequence is one past the highest sequence history has
///   committed, so a new effect continues the run rather than re-issuing identity history
///   already holds. That is not a discipline spread over call sites: the only thing that
///   moves the sequence on is consuming a schedule record, and it does so through
///   [`EffectIdAllocator::allocate`], which cannot repeat or wrap.
/// * At most one effect is unresolved at a time.
/// * [`Position::Halted`] is sticky by representation: no code path leaves it.
/// * The cursor holds no borrow of anything. It has no lifetime parameter, so this is a
///   fact about the type rather than a promise about the implementation.
///
/// # Why it is not `Clone`, `Copy` or `Default`
///
/// For the reasons [`EffectIdAllocator`] is none of them, plus one of its own: a copied
/// cursor is two readers of one journal that each believe they are the only one, and the
/// copy that falls behind would replay records the other has already consumed. A defaulted
/// one is a run whose identity nobody chose. [`Debug`], [`PartialEq`] and [`Eq`] are
/// derived, so a test can compare two cursors rather than compare their behaviour.
#[derive(Debug, PartialEq, Eq)]
pub struct ReplayCursor {
    ids: EffectIdAllocator,
    state: State,
}

impl ReplayCursor {
    /// A cursor for `run`, standing before the first record.
    ///
    /// # Postconditions
    ///
    /// `position() == Position::BeforeRun`, `run() == run`, `pending() == None`, and
    /// `next_seq() == Some(EffectSeq::FIRST)`.
    #[must_use]
    pub const fn new(run: RunId) -> Self {
        Self {
            ids: EffectIdAllocator::for_run(run),
            state: State::BeforeRun,
        }
    }

    /// The run this cursor replays.
    ///
    /// # Postconditions
    ///
    /// The same value for the life of the cursor, halted or not.
    #[must_use]
    pub const fn run(&self) -> RunId {
        self.ids.run()
    }

    /// Where the cursor stands.
    ///
    /// # Postconditions
    ///
    /// Total, and a pure projection of the cursor's state — reading it advances nothing.
    #[must_use]
    pub const fn position(&self) -> Position {
        match self.state {
            State::BeforeRun => Position::BeforeRun,
            State::Replaying => Position::Replaying,
            State::AwaitingOutcome(_) => Position::AwaitingOutcome,
            State::RunCompleted => Position::RunCompleted,
            State::RunFailed => Position::RunFailed,
            State::Halted(error) => Position::Halted(error),
        }
    }

    /// The run's first unresolved effect, when history left one open.
    ///
    /// This is §06's step 5 — "identifies the first unresolved effect" — and §14's
    /// redelivery contract: the [`EffectId`] it carries is the one the dispatcher was given
    /// before the reset, so a downstream system that deduplicates on it sees a repeat rather
    /// than a second effect.
    ///
    /// # Postconditions
    ///
    /// `Some` exactly when `position() == Position::AwaitingOutcome`, and `None`
    /// everywhere else — a halted cursor included, because a run whose history stopped
    /// being legal has no effect anyone should redeliver.
    #[must_use]
    pub const fn pending(&self) -> Option<PendingEffect> {
        match self.state {
            State::AwaitingOutcome(scheduled) => Some(PendingEffect {
                id: EffectId {
                    run: self.ids.run(),
                    seq: scheduled.seq,
                },
                kind: scheduled.kind,
                input_len: scheduled.input_len,
                input_crc: scheduled.input_crc,
            }),
            _ => None,
        }
    }

    /// The sequence the next effect would be issued, or [`None`] once the space is spent.
    ///
    /// A peek, not a lookup: it answers "what comes next", which is the only question a
    /// sequential cursor can be asked about identity. There is deliberately no operation
    /// that goes the other way and finds a record from a sequence.
    ///
    /// # Postconditions
    ///
    /// One past the highest sequence history has committed, or
    /// [`EffectSeq::FIRST`] when it has committed none. Never
    /// moves except through [`advance`](Self::advance) and [`allocate`](Self::allocate),
    /// and never moves backwards.
    #[must_use]
    pub const fn next_seq(&self) -> Option<EffectSeq> {
        self.ids.peek()
    }

    /// Mint the identity for an effect that history does not yet hold.
    ///
    /// §08's "end of history, new effect call" row needs a stable
    /// `(RunId, EffectSeq)` before a schedule record can be written, and this is where it
    /// comes from. It decides nothing: it does not write, dispatch, or judge whether the
    /// workflow was entitled to ask.
    ///
    /// # Errors
    ///
    /// * [`KernelError::NondeterministicWorkflow`] unless the cursor is at
    ///   [`Position::Replaying`]. Before the run starts there is no run to add an effect
    ///   to; while an effect is unresolved the next effect is that one, redelivered under
    ///   the identity [`pending`](Self::pending) names, and minting a fresh sequence would
    ///   abandon it; after a terminal record the run is over. Each is history saying this
    ///   effect cannot come next, which is §08's divergence — stop, never guess.
    /// * The failure that halted the cursor, if one did. Recovery stops at the first bad
    ///   record and stays stopped.
    /// * [`KernelError::IdExhausted`] when the run's 32-bit sequence space is spent. §07
    ///   makes that terminal for the run; the way out is `continue_as_new`.
    pub const fn allocate(&mut self) -> Result<EffectId, KernelError> {
        match self.state {
            State::Replaying => self.ids.allocate(),
            State::Halted(error) => Err(error),
            _ => Err(KernelError::NondeterministicWorkflow),
        }
    }

    /// Consume the next committed record.
    ///
    /// The whole of the cursor's forward motion. The caller reads one record into its
    /// scratch page, decodes it, and hands the borrowed view over; the cursor says what
    /// that record meant for the run and moves on. Nothing is retained: the returned
    /// [`Step`] borrows the *record*, not the cursor, so the page may be overwritten as
    /// soon as the step has been dealt with.
    ///
    /// # Postconditions
    ///
    /// On success the cursor has moved exactly one position along the transition diagram on
    /// [`Position`], and the returned step names what moved it. On failure the cursor is
    /// [`Position::Halted`] and stays there.
    ///
    /// # Errors
    ///
    /// * [`KernelError::MalformedHistory`] when this record could not legally follow the
    ///   ones before it: an outcome with no schedule, an outcome naming another effect, a
    ///   schedule while one is unresolved, a sequence that skips or repeats, a second
    ///   `RunStarted`, or any record after a terminal one.
    /// * [`KernelError::IdExhausted`] when history holds more effects than a 32-bit
    ///   sequence space has room for.
    /// * On any later call, the failure that stopped the cursor the first time.
    pub const fn advance<'a>(&mut self, record: RecordRef<'a>) -> Result<Step<'a>, KernelError> {
        if let State::Halted(error) = self.state {
            return Err(error);
        }
        match self.transition(record) {
            Ok(step) => Ok(step),
            Err(error) => {
                self.state = State::Halted(error);
                Err(error)
            }
        }
    }

    /// The transition itself, without the halting.
    ///
    /// Split out so that [`advance`](Self::advance) owns "a failure is sticky" in one place
    /// and this owns "what may follow what" in another. Every arm below is a legal
    /// transition; the catch-all is every combination that is not, which is most of the
    /// thirty-six. Listing those individually would be thirty lines that all say
    /// `MalformedHistory`, and — more to the point — a record kind added to
    /// [`RecordRef`] later would then need an arm per position before it compiled, when the
    /// right default for a record this rung does not understand is exactly the refusal §09
    /// asks for.
    const fn transition<'a>(&mut self, record: RecordRef<'a>) -> Result<Step<'a>, KernelError> {
        match (self.state, record) {
            (
                State::BeforeRun,
                RecordRef::RunStarted {
                    workflow_kind,
                    workflow_version,
                    input,
                },
            ) => {
                self.state = State::Replaying;
                Ok(Step::RunStarted {
                    workflow_kind,
                    workflow_version,
                    input,
                })
            }
            (
                State::Replaying,
                RecordRef::EffectScheduled {
                    seq,
                    kind,
                    input_len,
                    input_crc,
                },
            ) => {
                // Taken from the allocator rather than trusted from the record: the
                // allocator is the one thing that cannot issue a sequence twice or wrap,
                // so "history's sequences are the ones this run would have issued, in
                // order" is checked by the same type that guarantees it going forwards.
                // The identity is consumed before the comparison because a mismatch halts
                // the cursor for good, and a spent sequence on a cursor that will never
                // issue another is spent either way.
                let id = match self.ids.allocate() {
                    Ok(id) => id,
                    Err(error) => return Err(error),
                };
                if id.seq.0 != seq.0 {
                    return Err(KernelError::MalformedHistory);
                }
                self.state = State::AwaitingOutcome(Scheduled {
                    seq,
                    kind,
                    input_len,
                    input_crc,
                });
                Ok(Step::EffectScheduled(PendingEffect {
                    id,
                    kind,
                    input_len,
                    input_crc,
                }))
            }
            (State::AwaitingOutcome(scheduled), RecordRef::EffectCompleted { seq, result })
                if scheduled.seq.0 == seq.0 =>
            {
                self.state = State::Replaying;
                Ok(Step::EffectCompleted {
                    id: EffectId {
                        run: self.ids.run(),
                        seq,
                    },
                    result,
                })
            }
            (State::AwaitingOutcome(scheduled), RecordRef::EffectFailed { seq, error })
                if scheduled.seq.0 == seq.0 =>
            {
                self.state = State::Replaying;
                Ok(Step::EffectFailed {
                    id: EffectId {
                        run: self.ids.run(),
                        seq,
                    },
                    error,
                })
            }
            (State::Replaying, RecordRef::RunCompleted { result }) => {
                self.state = State::RunCompleted;
                Ok(Step::RunCompleted { result })
            }
            (State::Replaying, RecordRef::RunFailed { error }) => {
                self.state = State::RunFailed;
                Ok(Step::RunFailed { error })
            }
            _ => Err(KernelError::MalformedHistory),
        }
    }
}

// The cursor is the caller's, and the page is the caller's, and the point of the design is
// that those are two different things. A cursor that had grown a page buffer could not be
// smaller than one, so this is the cheapest possible check that it has not — and it fails
// the build rather than a report, on the target the budget is stated for.
const _: () = assert!(size_of::<ReplayCursor>() < crate::budget::SCRATCH_PAGE_BYTES);

#[cfg(test)]
mod tests {
    use super::{Position, ReplayCursor, State};
    use crate::error::KernelError;
    use crate::id::RunId;
    use crate::record::RecordRef;

    /// Every internal state, so the projection below has something to be total over.
    const EVERY_STATE: [State; 6] = [
        State::BeforeRun,
        State::Replaying,
        State::AwaitingOutcome(super::Scheduled {
            seq: crate::id::EffectSeq(3),
            kind: crate::activity::ActivityKind(1),
            input_len: 0,
            input_crc: 0,
        }),
        State::RunCompleted,
        State::RunFailed,
        State::Halted(KernelError::MalformedHistory),
    ];

    #[test]
    fn every_state_projects_to_its_own_position() {
        // Six states over six positions: an arm returning another state's position shows up
        // as a duplicate rather than as a value nobody looked at.
        let positions = EVERY_STATE.map(|state| {
            ReplayCursor {
                ids: crate::id::EffectIdAllocator::for_run(RunId(1)),
                state,
            }
            .position()
        });
        for (left_index, left) in positions.iter().enumerate() {
            for (right_index, right) in positions.iter().enumerate() {
                assert_eq!(
                    left_index == right_index,
                    left == right,
                    "two states project to {left:?}"
                );
            }
        }
    }

    #[test]
    fn only_the_two_terminal_positions_are_terminal() {
        let terminal = [
            Position::BeforeRun,
            Position::Replaying,
            Position::AwaitingOutcome,
            Position::RunCompleted,
            Position::RunFailed,
            Position::Halted(KernelError::MalformedHistory),
        ]
        .map(Position::is_terminal);
        assert_eq!(terminal, [false, false, false, true, true, false]);
    }

    #[test]
    fn a_cursor_is_usable_in_a_const_context() {
        // `const` so a driver can hold one in a static and so nothing here needs an
        // allocator to exist. The whole forward path is `const fn`.
        const CURSOR: ReplayCursor = ReplayCursor::new(RunId(9));
        assert_eq!(CURSOR.run(), RunId(9));
        assert_eq!(CURSOR.position(), Position::BeforeRun);
    }

    #[test]
    fn the_pending_effect_carries_the_cursors_run() {
        let mut cursor = ReplayCursor::new(RunId(0xFEED));
        assert!(
            cursor
                .advance(RecordRef::RunStarted {
                    workflow_kind: 1,
                    workflow_version: 1,
                    input: &[],
                })
                .is_ok()
        );
        assert!(
            cursor
                .advance(RecordRef::EffectScheduled {
                    seq: crate::id::EffectSeq::FIRST,
                    kind: crate::activity::ActivityKind(2),
                    input_len: 0,
                    input_crc: 0,
                })
                .is_ok()
        );
        let pending = cursor.pending();
        assert_eq!(pending.map(|effect| effect.id.run), Some(RunId(0xFEED)));
    }
}
