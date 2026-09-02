//! The transition table of design document §08: does history answer this, or does the world?
//!
//! At every effect boundary a workflow reaches, exactly one of five things is true, and
//! §08 states them as a table. This module is that table, as an explicit state machine
//! with one arm per row:
//!
//! | Next history state | Workflow call | Engine action |
//! | --- | --- | --- |
//! | Matching schedule + completion | Same effect kind and input digest | [`Resolve::Replayed`] |
//! | Matching schedule only | Same effect kind and input digest | [`Resolve::Redeliver`] |
//! | End of history | New effect call | [`Intent::Schedule`] |
//! | Different kind, digest, or sequence | Replay divergence | [`KernelError::NondeterministicWorkflow`] |
//! | Terminal workflow record | Further execution | [`Intent::Finished`] |
//!
//! # What this module owns
//!
//! [`ReplayMachine`], which is a [`ReplayCursor`] plus the one thing the cursor cannot
//! know — what the workflow just asked for. [`EffectRequest`] is that question,
//! [`Divergence`] is the three ways it can disagree with history, and [`Intent`] and
//! [`Resolve`] are the engine actions the table prescribes.
//!
//! # What this module must not own
//!
//! The digest, and the dispatch. [`EffectRequest`] carries a length and a checksum that
//! the layer above computed, because the kernel's must-not-own cell names CRC; and no
//! function here calls an activity, writes a record, or reaches a device. The machine
//! hands out an [`EffectId`] and says what has to happen to it. Design document §13
//! sketches this boundary as `EffectRequest { kind, input: &[u8] }`; a kernel that took
//! the bytes would have to digest them, so the bytes stop one layer up and their digest
//! comes down.
//!
//! # Why an effect boundary is two calls
//!
//! [`intent`](ReplayMachine::intent) then [`outcome`](ReplayMachine::outcome), because
//! that is the shape of §07's protocol: a durable intent is committed, and only then does
//! an outcome exist to observe. Replay reads them back in the same order, so the machine
//! consumes at most two records per boundary and asks for them one at a time — which is
//! what keeps a 512-byte page enough for a history of any length. Rows 1 and 2 differ
//! only in whether the *second* record is there, which is precisely why the second call
//! exists.
//!
//! # The loop a driver writes
//!
//! The five rows read as a table; this is what they are to the code that uses them. `next`
//! is whatever the driver's scan of the committed prefix produced at the cursor's position.
//!
//! ```text
//! loop {
//!     let request = EffectRequest { kind, input_len, input_crc };   // the workflow asked
//!     match machine.intent(request, next)? {
//!         Intent::Finished { outcome } => break outcome,            // row 5
//!         Intent::Schedule { id } => {                              // row 3
//!             append_and_seal(schedule_record(id, request));        // §07 steps 1-3
//!             machine.advance(schedule_record(id, request))?;
//!             let result = dispatch(id, input);                     // §07 step 4
//!             append_and_seal(outcome_record(id, result));          // §07 steps 5-7
//!             machine.advance(outcome_record(id, result))?;
//!         }
//!         Intent::Recorded { id } => match machine.outcome(next)? {
//!             Resolve::Replayed { outcome, .. } => hand_back(outcome),   // row 1
//!             Resolve::Redeliver { id } => {                            // row 2
//!                 let result = dispatch(id, input);       // the intent is already durable
//!                 append_and_seal(outcome_record(id, result));
//!                 machine.advance(outcome_record(id, result))?;
//!             }
//!         },
//!     }
//! }
//! ```
//!
//! Three vocabularies for one boundary — [`Step`], [`Intent`], [`Resolve`] — because the
//! three answer different questions: what a record meant, what history says about a call,
//! and what history holds for a call it recognises. Row 4 is the `?`.
//!
//! # Determinism is a contract
//!
//! §08 states it and nothing in Rust can enforce it. Workflow code must not read:
//!
//! * hardware registers,
//! * ambient time — a clock, a tick counter, an uptime,
//! * randomness,
//! * mutable statics,
//! * network state,
//! * or anything with a nondeterministic iteration order.
//!
//! Every one of those values enters a workflow through a *recorded effect*, so that the
//! replay of a run sees what the original execution saw rather than what the device
//! happens to hold now.
//!
//! The type system cannot prove this property for arbitrary Rust: a workflow that reads a
//! register is an ordinary function, and no signature distinguishes it from one that does
//! not. Waymaker therefore detects divergence where it becomes observable — at effect
//! boundaries, by comparing the request against the schedule history recorded — and a lint
//! for suspicious APIs is later tooling rather than a promise made here. What that buys is
//! narrow and worth stating plainly: a workflow whose nondeterminism never changes the
//! kind, digest or order of its effects is not detected, and cannot be. What is detected
//! is every nondeterminism that would have made replay return the wrong answer.
//!
//! # Divergence is terminal
//!
//! §08: "stop with `NondeterministicWorkflow`; never guess." So a divergent request is
//! refused *before* the cursor consumes the record it disagreed with, which leaves two
//! properties a driver can rely on: no [`EffectId`] escapes, so nothing can be dispatched;
//! and history stands where the divergence found it, so a diagnosis can name the record.
//! The refusal is sticky by representation — the machine's private phase has a diverged
//! state and no code path leaves it — for the reason
//! [`EffectIdAllocator`](crate::EffectIdAllocator) makes exhaustion sticky: a refusal that
//! can be forgotten is a refusal that will be.

use crate::activity::ActivityKind;
use crate::error::KernelError;
use crate::id::EffectId;
use crate::record::RecordRef;
use crate::replay::{PendingEffect, Position, ReplayCursor, Step};

/// What the driver found at the cursor's position.
///
/// Not `Option<RecordRef<'_>>`. `None` reads as "no record", and the thing being said is
/// "history ended" — which is a *row of the table* rather than an absence, and the row
/// whose engine action is to write a new record. A named variant makes the two rows that
/// depend on it, [`Intent::Schedule`] and [`Resolve::Redeliver`], impossible to reach by
/// accident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next<'a> {
    /// One committed record, decoded into the caller's scratch page.
    Record(RecordRef<'a>),
    /// Nothing further is committed.
    EndOfHistory,
}

/// A recorded outcome, borrowed from the record it was read out of.
///
/// One type for both an effect's outcome and the run's, because they are the same shape
/// and a driver's `match` should not have to learn two. Which one it is is decided by
/// where it appears: [`Resolve::Replayed`] carries an effect's, [`Intent::Finished`] the
/// run's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome<'a> {
    /// Success, with the bytes the workflow observes. Opaque to the kernel.
    Completed(&'a [u8]),
    /// Failure, with a bounded failure payload. Opaque to the kernel.
    Failed(&'a [u8]),
}

/// What history said about the *intent* half of an effect boundary.
///
/// The first of the two calls §07's protocol implies. Every variant is an engine action
/// from §08's table, and the fourth row — divergence — is the [`Err`] this is returned
/// beside rather than a variant, because a divergent boundary has no action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent<'a> {
    /// History already holds this effect's committed intent, under `id`.
    ///
    /// Rows 1 and 2 both arrive here: whether the recorded outcome exists is what
    /// [`ReplayMachine::outcome`] answers, and it is the only legal next call.
    Recorded {
        /// The identity history committed, paired with the machine's run.
        id: EffectId,
    },
    /// Row 3. History ended and the workflow asked for something new: commit a schedule
    /// record for `id`, then dispatch.
    ///
    /// Nothing has been spent. §07 spends a sequence when its schedule record is
    /// *committed*, so asking twice before that answers the same effect — which is what
    /// lets a writer retry a torn frame without skipping an identity.
    Schedule {
        /// The identity the schedule record must carry.
        id: EffectId,
    },
    /// Row 5. History holds a terminal run record: return this and poll no further.
    Finished {
        /// The run's recorded outcome.
        outcome: Outcome<'a>,
    },
}

/// What history said about the *outcome* half of an effect boundary.
///
/// Reached only after [`Intent::Recorded`]. Two variants, because at that point history
/// either holds the outcome or does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolve<'a> {
    /// Row 1. The recorded outcome, to hand the workflow back.
    Replayed {
        /// The effect this outcome resolves.
        id: EffectId,
        /// What the activity recorded.
        outcome: Outcome<'a>,
    },
    /// Row 2. Intent is committed and no outcome is: redeliver under `id`, writing
    /// nothing.
    ///
    /// §14's redelivery contract: `id` is the identity the dispatcher was given before the
    /// reset, so a downstream system that deduplicates on it sees a repeat rather than a
    /// second effect. There is no exactly-once promise here and §07 says so.
    Redeliver {
        /// The identity to redeliver under.
        id: EffectId,
    },
}

/// The way a workflow disagreed with history.
///
/// §08 names three — "different kind, digest, or sequence" — and there is a fourth here
/// that §08 implies rather than lists. Every one of them is the same refusal,
/// [`KernelError::NondeterministicWorkflow`]. This is the diagnosis beside it, kept because
/// the four have four different causes: a reordered call, a renamed activity, a changed
/// input and a workflow running ahead of its own history are four different things for an
/// engineer to go and look at.
///
/// Not [`Ord`]: four causes, not four magnitudes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Divergence {
    /// The recorded schedule's sequence is not the one the run would issue next.
    ///
    /// # What this actually diagnoses
    ///
    /// Not changed workflow code, despite the error it produces. A workflow call carries no
    /// sequence — the engine assigns it — so a workflow cannot ask for the wrong one. What
    /// reaches this is history out of order, or a driver that fed the record after the one
    /// it should have. §08's table nonetheless puts "sequence" in the divergence row, so
    /// that is the refusal a boundary reports, and this flavour is what tells a reader the
    /// refusal was about *position* rather than about the call.
    ///
    /// One consequence is worth knowing before it is met in a log: the same out-of-order
    /// journal is diagnosed twice over, depending on where it is noticed. Met at an effect
    /// boundary it is this, and the cursor is left where it stood; met by
    /// [`ReplayMachine::advance`] outside one it is [`KernelError::MalformedHistory`], and
    /// the cursor halts. Both are terminal. §09's "recovery stops at the first
    /// out-of-sequence frame" is the second; this row of §08's table is the first.
    ///
    /// # The run half of the comparison
    ///
    /// [`EffectRequest::divergence_from`] compares the whole [`EffectId`], run id included,
    /// so a [`PendingEffect`] built from another run is caught. Through
    /// [`ReplayMachine::intent`] that half cannot fire, and saying so is better than leaving
    /// an implied protection: §07 keeps the run id in the bank header rather than in every
    /// record, so a schedule record carries only a sequence and the machine can only pair it
    /// with its own run. Noticing that a *bank* belongs to another generation is the seal
    /// check in `waymaker-flash`, not this. The run-id clause guards a caller that reaches
    /// for the pure function with a [`PendingEffect`] it got from somewhere else.
    Sequence,
    /// The activity kind differs from the one history recorded.
    Kind,
    /// The input digest differs: a different length, a different checksum, or both.
    Digest,
    /// The workflow reached an effect boundary where history says none can come next.
    ///
    /// Not one of §08's three, because there is no recorded schedule here to differ from —
    /// which is exactly why it needs a flavour of its own. Reporting
    /// [`Sequence`](Self::Sequence) would put "the effect is not the one history recorded
    /// here" in a log for a position where history recorded nothing at all.
    ///
    /// Three positions produce it, and each is the workflow running ahead of what the
    /// journal can account for: before the run's own `RunStarted` was consumed, so the
    /// workflow is executing without its recorded input; while an effect is unresolved, so
    /// the workflow passed an `.await` without a result; and after a terminal record, which
    /// is §08 row 5's "without polling further" enforced rather than advised.
    ///
    /// Each is terminal for the same reason the other three are, and it is a stronger reason
    /// than "a driver made a mistake": a run that continues from here appends effects the
    /// journal cannot justify, so the *next* cold start could not replay it. Refusing is
    /// what keeps history replayable.
    Boundary,
}

impl Divergence {
    /// A short static description of this divergence.
    ///
    /// # Postconditions
    ///
    /// Non-empty, ASCII, distinct from every other variant's, and shorter than a firmware
    /// log line — the same contract [`KernelError::message`] holds, and for the same
    /// reason: two causes sharing a string is a log that cannot say which happened.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Sequence => "the effect is not the one history recorded here",
            Self::Kind => "a different activity kind than history recorded",
            Self::Digest => "a different activity input than history recorded",
            Self::Boundary => "an effect boundary history cannot account for",
        }
    }
}

/// What the workflow asked for at an effect boundary.
///
/// # Why this is a digest rather than the input
///
/// Design document §13 sketches `EffectRequest { kind, input: &'a [u8] }`. The kernel
/// cannot take the bytes: comparing them against history means digesting them, and the
/// kernel's must-not-own cell in §05 names CRC. So the layer that owns the checksum
/// computes it — `waymaker_flash::frame::input_digest` — and passes the pair down. That
/// also makes this type `Copy` and eight bytes with no lifetime, which is what lets a
/// boundary be two calls without the machine holding a borrow between them.
///
/// # Invariants
///
/// None this type enforces; the fields are public so a façade can build one. `input_len`
/// and `input_crc` must describe the *same* bytes the workflow passed, or the divergence
/// check compares a request against a digest of something else. The pair is compared
/// whole: §09 records both, and a length change with a colliding checksum is exactly the
/// case a checksum alone would wave through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EffectRequest {
    /// Which activity the workflow called.
    pub kind: ActivityKind,
    /// How many bytes of input it passed.
    pub input_len: u16,
    /// The digest of those bytes, computed one layer up.
    pub input_crc: u32,
}

impl EffectRequest {
    /// How this request disagrees with `recorded`, if it does.
    ///
    /// The fourth row of §08's table as a pure function, so that each flavour can be
    /// tested without a history to put it in. `expected` is the identity the run would
    /// issue next, which is what makes "sequence" a check rather than a hope: nothing in a
    /// workflow call carries a sequence, so the only way to catch a schedule from the
    /// wrong position — or from the wrong run — is against what the cursor says comes next.
    ///
    /// # Postconditions
    ///
    /// [`None`] exactly when `recorded.id == expected` and the kind, length and checksum
    /// all agree. Otherwise the *first* disagreement in this order:
    ///
    /// 1. [`Sequence`](Divergence::Sequence) — because a kind or digest compared against a
    ///    record from another position is a comparison of two unrelated effects, and
    ///    reporting "different kind" for a reordered call sends an engineer to the wrong
    ///    place;
    /// 2. [`Kind`](Divergence::Kind) — because a digest comparison between two different
    ///    activities says nothing a reader can use;
    /// 3. [`Digest`](Divergence::Digest).
    ///
    /// Pure: it reads nothing but its arguments and decides nothing about what happens
    /// next.
    #[must_use]
    pub const fn divergence_from(
        &self,
        recorded: &PendingEffect,
        expected: EffectId,
    ) -> Option<Divergence> {
        // The integers rather than the newtypes: derived `PartialEq` is not `const`.
        if recorded.id.run.0 != expected.run.0 || recorded.id.seq.0 != expected.seq.0 {
            return Some(Divergence::Sequence);
        }
        if recorded.kind.0 != self.kind.0 {
            return Some(Divergence::Kind);
        }
        if recorded.input_len != self.input_len || recorded.input_crc != self.input_crc {
            return Some(Divergence::Digest);
        }
        None
    }
}

/// How much of an effect boundary is open, and whether the workflow still agrees with
/// history.
///
/// Deliberately small and deliberately not a projection of the cursor's [`Position`]: the
/// cursor knows whether an *effect* is unresolved, and this knows whether the *workflow*
/// is between the two halves of one boundary. A redelivered effect is both — unresolved on
/// media, and settled here, because the driver has been told what to do and owes the
/// machine nothing until the outcome record exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No effect boundary is half-open.
    Settled,
    /// [`ReplayMachine::intent`] matched a committed schedule.
    /// [`ReplayMachine::outcome`] is the only legal next call.
    AwaitingOutcome,
    /// The workflow disagreed with history. Terminal, and there is no path out.
    Diverged(Divergence),
}

/// Design document §08's transition table, driven one record at a time.
///
/// This is the kernel's live state for the whole of a run, so it is registered in
/// [`kernel_state_types!`](crate::budget) and charged against the 128-byte budget in §04.
/// It *replaced* [`ReplayCursor`] in that registry rather than joining it, for the reason
/// the registry's comment gives: it contains the cursor, and two rows would spend the same
/// bytes twice.
///
/// # Invariants
///
/// * The run never changes.
/// * An [`EffectId`] is produced only by a call that got past the divergence check, so a
///   diverging replay cannot dispatch. That is structural rather than a discipline: the
///   machine owns the cursor, and the cursor is the only source of identity.
/// * A refused request consumes no record. History stands where the divergence found it.
/// * A halted cursor's error outranks every refusal the machine has of its own, so the
///   diagnosis a driver reports is always the record that stopped recovery.
/// * A divergence is sticky by representation: the private phase has a diverged state and
///   no code path leaves it. Every [`intent`](Self::intent) that answers
///   [`KernelError::NondeterministicWorkflow`] records one, so the refusal cannot be
///   forgotten by a later call that happens to be answerable.
/// * The machine holds no borrow of anything. It has no lifetime parameter, so one 512-byte
///   scratch page is enough however long history is.
///
/// # Why it is not `Clone`, `Copy` or `Default`
///
/// For [`ReplayCursor`]'s reasons, which it inherits by containing one: a copied machine is
/// two readers of one journal that each believe they are alone, and a defaulted one is a
/// run whose identity nobody chose. [`Debug`], [`PartialEq`] and [`Eq`] are derived, so a
/// test can compare two machines rather than compare their behaviour.
#[derive(Debug, PartialEq, Eq)]
pub struct ReplayMachine {
    cursor: ReplayCursor,
    phase: Phase,
}

impl ReplayMachine {
    /// A machine for `run`, standing before the first record.
    ///
    /// # Postconditions
    ///
    /// `position() == Position::BeforeRun`, `run() == run`, `pending() == None` and
    /// `diverged() == None`.
    #[must_use]
    pub const fn new(run: crate::id::RunId) -> Self {
        Self {
            cursor: ReplayCursor::new(run),
            phase: Phase::Settled,
        }
    }

    /// The run this machine replays.
    ///
    /// # Postconditions
    ///
    /// The same value for the life of the machine, diverged or not.
    #[must_use]
    pub const fn run(&self) -> crate::id::RunId {
        self.cursor.run()
    }

    /// Where the cursor stands in history.
    ///
    /// # Postconditions
    ///
    /// A pure projection: reading it advances nothing. It reports what *history* has done,
    /// which is why a diverged machine still reports [`Position::Replaying`] — the record
    /// the workflow disagreed with was never consumed, and saying otherwise would hide the
    /// place a diagnosis has to start from. [`diverged`](Self::diverged) is the other half
    /// of the answer.
    #[must_use]
    pub const fn position(&self) -> Position {
        self.cursor.position()
    }

    /// The run's first unresolved effect, when history left one open.
    ///
    /// # Postconditions
    ///
    /// The same as [`ReplayCursor::pending`]: `Some` exactly at
    /// [`Position::AwaitingOutcome`].
    #[must_use]
    pub const fn pending(&self) -> Option<PendingEffect> {
        self.cursor.pending()
    }

    /// The divergence that stopped this machine, if one did.
    ///
    /// # Postconditions
    ///
    /// `Some` from the first [`intent`](Self::intent) that returned
    /// [`KernelError::NondeterministicWorkflow`] onwards, and always the *first* one: the
    /// diagnosis a driver reports is the disagreement that stopped replay rather than
    /// whatever the workflow asked for next. Every such refusal is recorded, so a driver
    /// that consults this to decide whether §08's "stop, never guess" applies is never told
    /// `None` after one.
    ///
    /// [`None`] in two cases that are deliberately not divergence. A machine halted by
    /// [`KernelError::MalformedHistory`] is a fault in the *journal* rather than in the
    /// workflow — §08's divergence and §09's recovery stop are two different faults, and a
    /// log line that could not tell them apart would send an engineer to the wrong place.
    /// And a refusal from [`outcome`](Self::outcome) with no boundary open, or from
    /// [`advance`](Self::advance) with one still open, is the *driver* asking out of turn:
    /// the workflow made no claim, nothing is consumed, and a correct call still succeeds
    /// afterwards. Only a claim the workflow made can be a divergence.
    #[must_use]
    pub const fn diverged(&self) -> Option<Divergence> {
        match self.phase {
            Phase::Diverged(divergence) => Some(divergence),
            Phase::Settled | Phase::AwaitingOutcome => None,
        }
    }

    /// The refusal that outranks the machine's own, when history has already stopped.
    ///
    /// §09 stops recovery at the first record it cannot account for, and the diagnosis a
    /// driver reports has to be *that* record's. The machine has refusals of its own — a
    /// divergence, a boundary already open, a boundary not open — and every one of them
    /// would otherwise overwrite the cursor's, turning a damaged journal into a report of
    /// changed workflow code. `KernelError`'s own documentation calls those two different
    /// faults with two different causes, and this is what keeps them apart at the point a
    /// driver reads them.
    ///
    /// Checked before the phase in every entry point, which costs nothing in exactness: a
    /// halted cursor and a diverged phase cannot both be set, because
    /// [`ReplayCursor::next_effect_id`] refuses on a halted cursor before the divergence
    /// check is ever reached.
    const fn halted(&self) -> Option<KernelError> {
        match self.cursor.position() {
            Position::Halted(error) => Some(error),
            _ => None,
        }
    }

    /// Records `divergence` and hands back §08's refusal.
    ///
    /// One place, so that "a divergence is sticky" cannot be true of the arms that remember
    /// to set the phase and false of the ones that only return the error — which is exactly
    /// the shape the first version of this module got wrong.
    const fn diverge(&mut self, divergence: Divergence) -> KernelError {
        self.phase = Phase::Diverged(divergence);
        KernelError::NondeterministicWorkflow
    }

    /// Consume a committed record outside an effect boundary.
    ///
    /// The pump for everything the table does not decide: the `RunStarted` record that
    /// begins recovery, and the records a driver commits itself once the table has told it
    /// to — the schedule record of [`Intent::Schedule`], and the outcome record that
    /// finally resolves a [`Resolve::Redeliver`].
    ///
    /// # Errors
    ///
    /// * [`KernelError::NondeterministicWorkflow`] once the machine has diverged, and while
    ///   an effect boundary is half-open — between [`intent`](Self::intent) and
    ///   [`outcome`](Self::outcome) the next record is the one that answers the boundary,
    ///   and consuming it here would resolve an effect the workflow is still waiting on.
    ///   Neither case consumes the record.
    /// * Whatever [`ReplayCursor::advance`] refuses: [`KernelError::MalformedHistory`] for
    ///   a record that could not legally follow, [`KernelError::IdExhausted`] at the end of
    ///   the sequence space, and on any later call the failure that stopped the cursor.
    pub const fn advance<'a>(&mut self, record: RecordRef<'a>) -> Result<Step<'a>, KernelError> {
        if let Some(error) = self.halted() {
            return Err(error);
        }
        match self.phase {
            Phase::Diverged(_) | Phase::AwaitingOutcome => {
                Err(KernelError::NondeterministicWorkflow)
            }
            Phase::Settled => self.cursor.advance(record),
        }
    }

    /// The workflow reached an effect boundary: rows 3, 4 and 5, and the first half of 1
    /// and 2.
    ///
    /// `next` is the record the driver read at the cursor's position, or
    /// [`Next::EndOfHistory`].
    ///
    /// # Postconditions
    ///
    /// * [`Intent::Schedule`] consumes nothing and mints nothing.
    /// * [`Intent::Recorded`] consumed exactly the schedule record, and
    ///   [`outcome`](Self::outcome) is the only legal next call.
    /// * [`Intent::Finished`] consumed the terminal record, and the machine refuses every
    ///   later request — which is §08's "without polling further" as a refusal rather than
    ///   as advice.
    /// * A refused request consumes nothing.
    ///
    /// One rule covers all four, and a driver that keeps its scan in step with the cursor
    /// needs only this one: [`Next::EndOfHistory`] consumes nothing, an [`Ok`] consumed the
    /// record, and an [`Err`] did not. [`outcome`](Self::outcome) obeys the same rule.
    ///
    /// # Errors
    ///
    /// * [`KernelError::NondeterministicWorkflow`] when the request disagrees with the
    ///   schedule history recorded — [`diverged`](Self::diverged) says how — and, for the
    ///   same reason [`ReplayCursor::next_effect_id`] gives, when no effect can come next
    ///   at all: before the run starts, while an effect is unresolved, after a terminal
    ///   record, once the machine has diverged, and while a boundary is already open.
    /// * [`KernelError::MalformedHistory`] when `next` is a record that could not legally
    ///   follow. That is history being impossible rather than the workflow disagreeing, and
    ///   it halts the cursor.
    /// * [`KernelError::IdExhausted`] when the run's sequence space is spent.
    pub const fn intent<'a>(
        &mut self,
        request: EffectRequest,
        next: Next<'a>,
    ) -> Result<Intent<'a>, KernelError> {
        if let Some(error) = self.halted() {
            return Err(error);
        }
        match self.phase {
            // Already diverged. Checked before anything else and *not* re-recorded, so the
            // diagnosis a driver reports stays the disagreement that stopped replay.
            Phase::Diverged(_) => return Err(KernelError::NondeterministicWorkflow),
            // A boundary already open is *not* an arm here, deliberately. Setting the phase
            // to `AwaitingOutcome` requires having advanced the cursor over a schedule
            // record, so the cursor is at `Position::AwaitingOutcome` too and its own gate
            // below refuses — with the same recorded `Boundary` divergence. A second check
            // here would be a branch no test could distinguish from the one that does the
            // work, which is how a guard ends up believed in and not exercised.
            Phase::Settled | Phase::AwaitingOutcome => {}
        }

        // The one source of identity in the kernel, and the position gate with it.
        //
        // An `Option`, because a spent sequence space stops a *new* effect and nothing
        // else. The cursor accepts a terminal record at `Replaying` without allocating
        // anything, so a run that legally committed `EffectSeq::MAX` effects and then
        // finished must still be able to report its recorded outcome: row 5 needs no
        // identity, and taking one from it would lose a completion the journal holds. Rows
        // 2 and 3 do need one, and refuse below when there is none.
        //
        // That history needs 2^32 committed schedules — 64 GiB at the frame's sixteen-byte
        // floor — so no test here walks to it, and the `None` arms below are uncovered for
        // the same reason `EffectSeq::MAX` is exercised in `EffectIdAllocator` rather than
        // through a cursor. The ceiling is real; the walk is not.
        let expected = match self.cursor.next_effect_id() {
            Ok(id) => Some(id),
            // History says no effect can come next: before the run starts, while an effect
            // is unresolved, or after a terminal record. The workflow is running ahead of
            // what the journal can account for, which is terminal for the same reason a
            // mismatched schedule is.
            Err(KernelError::NondeterministicWorkflow) => {
                return Err(self.diverge(Divergence::Boundary));
            }
            // Not a workflow fault, so not recorded as one, and already sticky: the
            // allocator has no path back from a spent sequence space and §07 makes that
            // terminal for the run.
            Err(KernelError::IdExhausted) => None,
            Err(error) => return Err(error),
        };

        match next {
            // Row 3. A new effect, so it is the one row that cannot proceed without an
            // identity to give it.
            Next::EndOfHistory => match expected {
                Some(id) => Ok(Intent::Schedule { id }),
                None => Err(KernelError::IdExhausted),
            },
            Next::Record(record) => match record {
                // Rows 1, 2 and 4: history holds a schedule, and whether it is *this*
                // effect's is the whole of the divergence check.
                RecordRef::EffectScheduled {
                    seq,
                    kind,
                    input_len,
                    input_crc,
                } => {
                    // The recorded schedule has to be compared against the identity the run
                    // would issue next, so this row needs one too.
                    let Some(expected) = expected else {
                        return Err(KernelError::IdExhausted);
                    };
                    let recorded = PendingEffect {
                        id: EffectId {
                            run: expected.run,
                            seq,
                        },
                        kind,
                        input_len,
                        input_crc,
                    };
                    // Row 4, and it comes first: the cursor is advanced only past this
                    // check, so a divergent boundary consumes no record and produces no
                    // identity. That is what "never dispatches" is made of.
                    if let Some(divergence) = request.divergence_from(&recorded, expected) {
                        self.phase = Phase::Diverged(divergence);
                        return Err(KernelError::NondeterministicWorkflow);
                    }
                    match self.cursor.advance(record) {
                        Ok(Step::EffectScheduled(pending)) => {
                            self.phase = Phase::AwaitingOutcome;
                            Ok(Intent::Recorded { id: pending.id })
                        }
                        // The cursor answers a schedule record with a schedule step, so
                        // these arms are unreachable. Written out rather than left to a
                        // wildcard because a `Step` added later must be a compile error
                        // here, and refused rather than panicked because this workspace
                        // denies `panic!` in production code.
                        Ok(
                            Step::RunStarted { .. }
                            | Step::EffectCompleted { .. }
                            | Step::EffectFailed { .. }
                            | Step::RunCompleted { .. }
                            | Step::RunFailed { .. },
                        ) => Err(KernelError::MalformedHistory),
                        Err(error) => Err(error),
                    }
                }
                // Row 5.
                RecordRef::RunCompleted { .. } | RecordRef::RunFailed { .. } => {
                    match self.cursor.advance(record) {
                        Ok(Step::RunCompleted { result }) => Ok(Intent::Finished {
                            outcome: Outcome::Completed(result),
                        }),
                        Ok(Step::RunFailed { error }) => Ok(Intent::Finished {
                            outcome: Outcome::Failed(error),
                        }),
                        Ok(
                            Step::RunStarted { .. }
                            | Step::EffectScheduled(_)
                            | Step::EffectCompleted { .. }
                            | Step::EffectFailed { .. },
                        ) => Err(KernelError::MalformedHistory),
                        Err(error) => Err(error),
                    }
                }
                // No row: a run cannot start twice, and an outcome cannot precede its
                // schedule. Handed to the cursor rather than refused here so that one type
                // owns "what may follow what" and so that the refusal is sticky — recovery
                // stops at the first record it cannot account for, and stays stopped.
                RecordRef::RunStarted { .. }
                | RecordRef::EffectCompleted { .. }
                | RecordRef::EffectFailed { .. } => match self.cursor.advance(record) {
                    Ok(
                        Step::RunStarted { .. }
                        | Step::EffectScheduled(_)
                        | Step::EffectCompleted { .. }
                        | Step::EffectFailed { .. }
                        | Step::RunCompleted { .. }
                        | Step::RunFailed { .. },
                    ) => Err(KernelError::MalformedHistory),
                    Err(error) => Err(error),
                },
            },
        }
    }

    /// The outcome half of an effect boundary: rows 1 and 2.
    ///
    /// Legal only after [`Intent::Recorded`]. `next` is the record that follows the
    /// schedule, or [`Next::EndOfHistory`].
    ///
    /// # Postconditions
    ///
    /// * [`Resolve::Replayed`] consumed the outcome record; the effect is resolved and the
    ///   run carries on at the next sequence.
    /// * [`Resolve::Redeliver`] consumed nothing; the effect stays unresolved and
    ///   [`pending`](Self::pending) keeps naming it, so a driver that resets again
    ///   redelivers the same identity.
    /// * Either way the boundary is closed, and the borrowed outcome is valid for exactly
    ///   as long as the page holding the record it came from.
    ///
    /// # Errors
    ///
    /// * [`KernelError::NondeterministicWorkflow`] with no boundary open, and once the
    ///   machine has diverged.
    /// * [`KernelError::MalformedHistory`] when `next` is a record that could not resolve
    ///   the open effect — an outcome naming another sequence, a second schedule, a
    ///   terminal record while an effect is unresolved. That halts the cursor.
    /// * On a halted cursor, the failure that stopped it.
    pub const fn outcome<'a>(&mut self, next: Next<'a>) -> Result<Resolve<'a>, KernelError> {
        if let Some(error) = self.halted() {
            return Err(error);
        }
        match self.phase {
            // No boundary is open, or the machine has diverged.
            Phase::Diverged(_) | Phase::Settled => {
                return Err(KernelError::NondeterministicWorkflow);
            }
            Phase::AwaitingOutcome => {}
        }

        let Some(open) = self.cursor.pending() else {
            // Unreachable: `Phase::AwaitingOutcome` is set only by the arm that just
            // advanced the cursor over a schedule record. Refused rather than panicked.
            return Err(KernelError::MalformedHistory);
        };

        match next {
            // Row 2.
            Next::EndOfHistory => {
                self.phase = Phase::Settled;
                Ok(Resolve::Redeliver { id: open.id })
            }
            // Row 1.
            Next::Record(record) => match self.cursor.advance(record) {
                Ok(Step::EffectCompleted { id, result }) => {
                    self.phase = Phase::Settled;
                    Ok(Resolve::Replayed {
                        id,
                        outcome: Outcome::Completed(result),
                    })
                }
                Ok(Step::EffectFailed { id, error }) => {
                    self.phase = Phase::Settled;
                    Ok(Resolve::Replayed {
                        id,
                        outcome: Outcome::Failed(error),
                    })
                }
                // The cursor accepts only those two while an effect is unresolved, so
                // these arms are unreachable; they are written out for the reason the ones
                // in `intent` are.
                Ok(
                    Step::RunStarted { .. }
                    | Step::EffectScheduled(_)
                    | Step::RunCompleted { .. }
                    | Step::RunFailed { .. },
                ) => Err(KernelError::MalformedHistory),
                Err(error) => Err(error),
            },
        }
    }
}

// The machine is the cursor and a phase, and nothing else.
//
// Pinned rather than bounded, for the reason `ReplayCursor`'s own assertion gives: the
// kernel-state budget leaves enough headroom that a machine which grew an inline scratch
// buffer would satisfy every inequality in this crate. Stated against the cursor rather
// than as a literal because `RunId` aligns to 8 on the host and to 4 on
// `thumbv6m-none-eabi`: the phase is one byte of payload, so it costs exactly one
// alignment unit of padding on either.
const _: () = assert!(size_of::<Phase>() <= 2);
const _: () =
    assert!(size_of::<ReplayMachine>() == size_of::<ReplayCursor>() + align_of::<ReplayCursor>());

#[cfg(test)]
mod tests {
    use super::{Divergence, EffectRequest, Next, Phase, ReplayMachine};
    use crate::activity::ActivityKind;
    use crate::error::KernelError;
    use crate::id::{EffectId, EffectSeq, RunId};
    use crate::record::RecordRef;
    use crate::replay::PendingEffect;

    const RUN: RunId = RunId(5);

    /// One of each phase, so a test can be total over them.
    const EVERY_PHASE: [Phase; 3] = [
        Phase::Settled,
        Phase::AwaitingOutcome,
        Phase::Diverged(Divergence::Kind),
    ];

    /// Every `Divergence`, in declaration order.
    const EVERY_DIVERGENCE: [Divergence; 4] = [
        Divergence::Sequence,
        Divergence::Kind,
        Divergence::Digest,
        Divergence::Boundary,
    ];

    /// The position of each flavour in [`EVERY_DIVERGENCE`], by exhaustive `match`.
    ///
    /// Adding a variant without extending the array forces a new arm here, and the only
    /// index it can be given is one the array does not have. The same guard `error.rs` puts
    /// on its two enums, and for the same reason: a fixed-length array cannot notice a
    /// variant that was never put in it.
    const fn divergence_index(divergence: Divergence) -> usize {
        match divergence {
            Divergence::Sequence => 0,
            Divergence::Kind => 1,
            Divergence::Digest => 2,
            Divergence::Boundary => 3,
        }
    }

    #[test]
    fn the_divergence_list_is_complete() {
        assert!(
            EVERY_DIVERGENCE
                .iter()
                .copied()
                .map(divergence_index)
                .eq(0..EVERY_DIVERGENCE.len())
        );
    }

    #[test]
    fn only_a_diverged_phase_reports_a_divergence() {
        let reported = EVERY_PHASE.map(|phase| {
            ReplayMachine {
                cursor: crate::replay::ReplayCursor::new(RUN),
                phase,
            }
            .diverged()
        });
        assert_eq!(reported, [None, None, Some(Divergence::Kind)]);
    }

    #[test]
    fn a_machine_is_usable_in_a_const_context() {
        // `const` so a driver can hold one in a static, and so nothing here needs an
        // allocator to exist. The whole forward path is `const fn`.
        const MACHINE: ReplayMachine = ReplayMachine::new(RunId(9));
        assert_eq!(MACHINE.run(), RunId(9));
        assert_eq!(MACHINE.diverged(), None);
    }

    #[test]
    fn the_divergence_check_is_available_in_a_const_context() {
        const REQUEST: EffectRequest = EffectRequest {
            kind: ActivityKind(1),
            input_len: 2,
            input_crc: 3,
        };
        const RECORDED: PendingEffect = PendingEffect {
            id: EffectId {
                run: RUN,
                seq: EffectSeq::FIRST,
            },
            kind: ActivityKind(1),
            input_len: 2,
            input_crc: 3,
        };
        const AGREED: Option<Divergence> = REQUEST.divergence_from(
            &RECORDED,
            EffectId {
                run: RUN,
                seq: EffectSeq::FIRST,
            },
        );
        assert_eq!(AGREED, None);
    }

    #[test]
    fn a_diverged_machine_refuses_every_later_call() {
        let mut machine = ReplayMachine::new(RUN);
        machine.phase = Phase::Diverged(Divergence::Digest);

        assert_eq!(
            machine.advance(RecordRef::RunStarted {
                workflow_kind: 1,
                workflow_version: 1,
                input: &[],
            }),
            Err(KernelError::NondeterministicWorkflow)
        );
        assert_eq!(
            machine.outcome(Next::EndOfHistory),
            Err(KernelError::NondeterministicWorkflow)
        );
        assert_eq!(machine.diverged(), Some(Divergence::Digest));
    }
}
