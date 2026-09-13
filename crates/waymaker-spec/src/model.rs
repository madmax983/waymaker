//! The ghost model: the math shadow of one run's on-media state.
//!
//! Design document §15 names three record states — merely attempted, possibly durable
//! before acknowledgment, and barrier-returned — and §02 decision 7 names two banks whose
//! generation seal decides which run is authoritative. This module is those two state
//! machines and nothing else: no bytes, no offsets, no geometry, no time.
//!
//! # What a state is
//!
//! A [`Journal`] is what is *true about the world* at one instant, in the only three
//! dimensions §15's oracle asks about: what reached media, what was handed to the world,
//! and which bank a reader would boot from. It is deliberately not a picture of the media —
//! two of §15's three record states are the same bytes, and the difference between them is
//! when the power went away relative to a barrier, which no image can be asked.
//!
//! # What a transition is
//!
//! A [`Transition`] is one thing a writer, or the power supply, can do. [`Journal::step`]
//! is total: every transition is either legal from a state and yields the successor, or is
//! refused with the [`Illegal`] reason. There is no third answer — an illegal move is never
//! silently ignored, because a state machine whose guards can be stepped over is a state
//! machine whose guards are decoration.
//!
//! One transition is genuinely idempotent and may leave the state unchanged: a barrier over
//! a device with nothing new to acknowledge. It is named, and `tests/machine.rs` requires
//! every *other* legal transition to change something — a no-op that is not on that list is
//! a guard that stopped guarding.
//!
//! # The guards are the design
//!
//! Six preconditions carry the whole specification, and each is separately removable
//! through [`Guards`]. That is not a convenience: `tests/necessity.rs` removes them one at
//! a time and requires that each removal makes some §14 guarantee reachable-false. A guard
//! that can be deleted with every proof still passing is a guard that was never load-bearing,
//! and this is the only way to find out which those are.

use std::collections::BTreeSet;

use waymaker_fault::{Durability, Ledger, RecordId};

/// How many banks §02 decision 7 gives a device.
pub const BANKS: usize = 2;

/// Which of the two banks a transition names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BankId {
    /// The first bank.
    A,
    /// The second bank.
    B,
}

impl BankId {
    /// Both banks, in a fixed order.
    pub const ALL: [Self; BANKS] = [Self::A, Self::B];

    /// The bank this one is not.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    /// Position in [`Journal::banks`].
    const fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }
}

/// How much of a record's bytes reached media.
///
/// Three values rather than [`Durability`]'s three, because these are the *media* facts and
/// [`Durability`] is what recovery owes. The mapping is [`Record::durability`], and it is
/// one way: `Whole` is either possibly durable or acknowledged depending on whether a
/// barrier has returned since, and no amount of looking at bytes decides which.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OnMedia {
    /// Not one cell changed. Recovery must not produce it.
    Absent,
    /// Some cells changed and some did not. Recovery must not produce it either: §15 permits
    /// recovery to include "an unacknowledged **complete** record", and complete is the
    /// load-bearing word.
    Partial,
    /// Every byte is there.
    Whole,
}

/// What a record is *for*.
///
/// The model is otherwise deliberately incurious about record content — that is what makes
/// it a model of the protocol rather than of the codec — but durable intent cannot be
/// stated without this one distinction. §02 decision 3 is "no *dispatched effect* lacks a
/// recoverable *schedule* record", and a model whose records are interchangeable would let
/// an effect be accounted for by an acknowledged completion, which schedules nothing and
/// says an effect happened that history has no intent for.
///
/// Two roles rather than [`waymaker_core::RecordKind`]'s six: the run records are outside a
/// bounded model of effect identity, and a failure is an outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// A durable intent: the record §02 decision 3 requires to cross a barrier before its
    /// effect reaches the world.
    Schedule,
    /// The outcome of the schedule before it — a completion or a failure, which the kernel
    /// tells apart and this model does not need to.
    Outcome,
}

impl Role {
    /// Both roles, in a fixed order.
    pub const ALL: [Self; 2] = [Self::Schedule, Self::Outcome];
}

/// One record of the ghost history.
///
/// # `acknowledged` and `media` are independent on purpose
///
/// Design document §15's "a torn record is never acknowledged" could have been made a fact
/// about this type, and deliberately is not: [`Guard::BarrierNeedsWhole`] is what makes it
/// true, `tests/necessity.rs` removes that guard and requires acknowledged durability to
/// fail, and a representation that could not hold the counter-example would have no way to
/// show the guard was load-bearing. `tests/machine.rs` proves the implication instead, over
/// every state the enforced machine reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Record {
    /// The writer's own numbering, allocated once and never reused — issue #67's scheme for
    /// identity across a bank swap and across a reboot. Not a position: `Journal::declare`
    /// takes it from a counter that only grows, so a record dropped by
    /// `Journal::begin_erase` can never be reissued to something else.
    pub id: RecordId,
    /// Whether this record is a durable intent or the outcome of one.
    pub role: Role,
    /// How much of it reached media.
    pub media: OnMedia,
    /// Whether a barrier has returned since the last of its writes.
    pub acknowledged: bool,
    /// Which bank holds these bytes.
    ///
    /// Issue [#67](https://github.com/madmax983/waymaker/issues/67)'s first gap: without
    /// this, erasing a bank left every record untouched, and "never recover the old run as
    /// current" was a sentence the model had no way to state. `Journal::begin_erase` drops
    /// every record with a matching `bank`, and recovery reads only the records whose `bank`
    /// is the one bank a reader would boot from.
    pub bank: BankId,
}

impl Record {
    /// What recovery is allowed, and required, to do with this record.
    #[must_use]
    pub const fn durability(&self) -> Durability {
        if self.acknowledged {
            return Durability::Acknowledged;
        }
        match self.media {
            OnMedia::Absent => Durability::Attempted,
            OnMedia::Partial | OnMedia::Whole => Durability::PossiblyDurable,
        }
    }

    /// Whether recovery is permitted to produce this record.
    ///
    /// Absent and torn records are both refused, for different reasons that reach the same
    /// answer: nothing of the first is there to read, and half of the second is.
    #[must_use]
    pub const fn is_recoverable(&self) -> bool {
        matches!(self.media, OnMedia::Whole)
    }
}

/// A bank's generation seal, as §02 decision 7 stages it.
///
/// The middle value is the whole decision: "a new run becomes authoritative only after its
/// payload and generation seal are durable". A seal whose barrier has not returned is
/// [`Bank::Sealing`], and [`Journal::authoritative`] does not count it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bank {
    /// No seal, and nothing half-gone. A reader will not boot from it, and a new run may be
    /// written into it.
    Erased,
    /// An erase was begun and has not returned. Design document §15 enumerates an erase
    /// interrupted at an erase block, so this is a state a real device is in — and one the
    /// swap's atomicity rests on being unbootable, which is a fact worth proving rather than
    /// assuming. Not bootable, and not writable either: a partly-erased bank is not a blank
    /// one.
    Erasing,
    /// A seal was written and no barrier has returned. Still not bootable.
    Sealing(u32),
    /// The seal is durable at this generation.
    Sealed(u32),
}

impl Bank {
    /// The generation a reader would read off this bank, or `None` if it would not boot it.
    #[must_use]
    pub const fn authoritative_generation(self) -> Option<u32> {
        match self {
            Self::Sealed(generation) => Some(generation),
            Self::Erased | Self::Erasing | Self::Sealing(_) => None,
        }
    }
}

/// One thing a writer, or the power supply, can do.
///
/// [`Transition::Declare`] carries no id: ids are allocated in declaration order, the way
/// [`waymaker_core::EffectIdAllocator`] allocates a sequence, so a model that let a writer
/// choose would be modelling a freedom the kernel does not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Transition {
    /// Begin a record in this role. Nothing reaches media yet.
    Declare(Role),
    /// Write a declared record's bytes, all of them.
    Program(RecordId),
    /// Write a declared record's bytes and have the call fail part-way through.
    ///
    /// Design document §12: "program and erase may fail". The difference from
    /// [`Transition::Tear`] is the power, and it is the whole reason both exist: a failed
    /// call leaves a half-written record on a device that is still running, which is the only
    /// state in which a writer can go on to ask for a barrier over a torn record.
    FailedProgram(RecordId),
    /// Wait for the durability barrier. Every whole record becomes acknowledged.
    Barrier,
    /// Hand the effect this record schedules to the world. Physical, and irreversible.
    Dispatch(RecordId),
    /// Begin erasing a bank. Its seal is gone; the bank is not yet blank.
    BeginErase(BankId),
    /// The erase returned. The bank is blank and a new run may be written into it.
    CommitErase(BankId),
    /// Write a bank's generation seal without waiting for it.
    BeginSeal(BankId),
    /// Wait for the seal's barrier. The bank becomes authoritative.
    CommitSeal(BankId),
    /// The power goes away part-way through the open record's program.
    Tear,
    /// The power goes away between operations.
    PowerLoss,
    /// The device powers back on, resuming from the recovered prefix.
    ///
    /// Issue [#67](https://github.com/madmax983/waymaker/issues/67)'s second gap: the only
    /// transition legal while [`Journal::powered`] is `false`, and it is legal only then. A
    /// fresh run seeded straight from a crash rather than from [`Journal::new`] had no
    /// representation before this, so a device on its second or third boot was a state this
    /// machine could not reach.
    Reboot,
}

/// Why a transition was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Illegal {
    /// The power is already gone. Nothing happens after that, including another power loss.
    PowerIsGone,
    /// There is no record waiting to be written.
    NoOpenRecord,
    /// An earlier record is not wholly on media, so this write would leave a gap behind it.
    EarlierRecordIncomplete,
    /// The record already reached media, wholly or in part; programming it again would be a
    /// second write to cells only an erase can restore.
    RecordAlreadyWritten,
    /// The other bank has a seal in flight, and a writer seals one bank at a time.
    SealAlreadyInFlight,
    /// The model's record bound is reached.
    CapacityReached,
    /// The transition names a record this run never declared.
    UndeclaredRecord,
    /// The transition dispatches against a record that schedules nothing.
    ///
    /// §02 decision 3 is about a *schedule* record. An outcome is not an intent, and a model
    /// that let one stand in for the other would report durable intent satisfied by a record
    /// that says an effect finished rather than that one was ordered.
    NotAScheduleRecord,
    /// A schedule may not be declared while an earlier one has no outcome, and an outcome
    /// may not be declared with no schedule waiting for it.
    ///
    /// Design document §11's order, and the same rule
    /// [`waymaker_core::ReplayCursor`](waymaker_core::replay::ReplayCursor) enforces when it
    /// refuses "a schedule while one is unresolved" as malformed history. A model that
    /// admitted three schedules in a row would be a model of a journal the kernel refuses to
    /// replay.
    OutOfProtocolOrder,
    /// The schedule record has not crossed a barrier, so §02 decision 3 forbids the dispatch.
    IntentNotDurable,
    /// The effect was already handed to the world; doing it twice is a different property.
    AlreadyDispatched,
    /// The bank is not erased, so a seal would be written over cells only an erase restores.
    BankNotErased,
    /// The bank has no erase in flight to complete.
    BankNotErasing,
    /// The bank already has an erase in flight.
    ///
    /// Re-issuing one is not modelled, for the same reason re-programming a half-written
    /// record is not: a writer that reacts to a failure by retrying is rung 0.2's
    /// compaction, and a model that admitted the retry without describing what it retries
    /// *into* would be admitting a transition with no content.
    EraseAlreadyInFlight,
    /// The bank has no seal in flight to commit.
    BankNotSealing,
    /// This is the bank a reader would boot from, so erasing it would either strand the
    /// device or hand it back an older run.
    WouldEraseTheAuthority,
    /// The model's generation bound is reached.
    GenerationExhausted,
    /// The device is already powered; there is nothing to reboot from.
    AlreadyPowered,
}

impl Illegal {
    /// One line naming the precondition that refused the transition.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::PowerIsGone => "the power is gone and nothing happens after that",
            Self::NoOpenRecord => "no record is open",
            Self::EarlierRecordIncomplete => "an earlier record is not wholly on media",
            Self::RecordAlreadyWritten => "that record already reached media",
            Self::SealAlreadyInFlight => "the other bank has a seal in flight",
            Self::CapacityReached => "the model's record bound is reached",
            Self::UndeclaredRecord => "this run never declared that record",
            Self::NotAScheduleRecord => "that record schedules nothing",
            Self::OutOfProtocolOrder => "a schedule is unresolved, or no schedule is waiting",
            Self::IntentNotDurable => "the schedule record has not crossed a barrier",
            Self::AlreadyDispatched => "that effect was already handed to the world",
            Self::BankNotErased => "the bank is not erased",
            Self::BankNotErasing => "the bank has no erase in flight",
            Self::EraseAlreadyInFlight => "the bank already has an erase in flight",
            Self::BankNotSealing => "the bank has no seal in flight",
            Self::WouldEraseTheAuthority => "this is the bank a reader would boot from",
            Self::GenerationExhausted => "the model's generation bound is reached",
            Self::AlreadyPowered => "the device is already powered",
        }
    }
}

impl core::fmt::Display for Illegal {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for Illegal {}

/// One precondition, named so that it can be removed on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Guard {
    /// A record's bytes may not be written while an earlier declared record is not whole.
    ///
    /// Without it, `Declare, Declare, Program(1), Barrier` acknowledges the second record
    /// behind an absent first one, and every prefix-honest recovery loses it. This is the
    /// guard the proof of acknowledged durability *needs*, and finding that out — rather
    /// than assuming append-only writing and proving the assumption — is the whole reason
    /// the guards are named and separately removable.
    AppendOnly,
    /// A barrier acknowledges only records that are wholly on media.
    ///
    /// Not a refusal — a barrier over a device with a half-written record on it returns
    /// perfectly well. What it must not do is claim that record. Without this guard the
    /// barrier acknowledges everything that reached media at all, and a prefix-honest reader
    /// then stops at the torn record it was told is durable.
    BarrierNeedsWhole,
    /// An effect may not be dispatched before its schedule record is acknowledged.
    ///
    /// §02 decision 3, as a precondition rather than a hope.
    DurableIntent,
    /// An effect's durable intent must be a *schedule* record.
    ///
    /// Without it, a dispatch can be accounted for by an acknowledged completion — a record
    /// that says an effect finished, standing in for the one that says it was ordered. The
    /// guarantee would then read "some record about this effect is recoverable", which is
    /// not §02 decision 3 and is not worth anything: the completion is written *after* the
    /// world was changed, so a recovery holding it has already lost the ordering the
    /// decision exists to create.
    DispatchNeedsASchedule,
    /// The bank a reader would boot from may not have its erase begun.
    ///
    /// Design document §14's failure table, on the two-bank swap: "never recover the old run
    /// as current". Erasing the authoritative bank does exactly that — authority falls back
    /// to whatever older generation the other bank still carries, or to nothing at all. The
    /// swap recycles the *inactive* bank, which is what makes it atomic.
    NeverEraseTheAuthority,
    /// A new seal's generation must be strictly greater than the other bank's.
    StrictGeneration,
}

impl Guard {
    /// Every guard, in a fixed order.
    pub const ALL: [Self; 6] = [
        Self::AppendOnly,
        Self::BarrierNeedsWhole,
        Self::DurableIntent,
        Self::DispatchNeedsASchedule,
        Self::NeverEraseTheAuthority,
        Self::StrictGeneration,
    ];

    /// This guard's place in [`Guards`]'s bitmask.
    const fn bit(self) -> u8 {
        match self {
            Self::AppendOnly => 1,
            Self::BarrierNeedsWhole => 1 << 1,
            Self::DurableIntent => 1 << 2,
            Self::DispatchNeedsASchedule => 1 << 3,
            Self::NeverEraseTheAuthority => 1 << 4,
            Self::StrictGeneration => 1 << 5,
        }
    }
}

/// Which preconditions [`Journal::step`] enforces.
///
/// A bitmask over [`Guard`] rather than five fields, so that adding a sixth precondition is
/// one arm of a private `Guard::bit` rather than a field every constructor has to remember.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Guards(u8);

impl Guards {
    /// The specification: every precondition enforced.
    ///
    /// Folded from [`Guard::ALL`] rather than written as `u8::MAX`, so the representation
    /// has no bit no guard owns. With unowned bits set, `ENFORCED.without(..every guard..)`
    /// and a hypothetical `Guards(0)` would enforce the same nothing and compare unequal —
    /// and [`crate::explore::Explored`] derives equality through this field.
    pub const ENFORCED: Self = {
        let mut bits = 0;
        let mut remaining: &[Guard] = &Guard::ALL;
        // A `while let` over a shrinking slice rather than an index, because the workspace
        // denies indexing and a `const fn` has no iterator.
        while let [guard, rest @ ..] = remaining {
            bits |= guard.bit();
            remaining = rest;
        }
        Self(bits)
    };

    /// The same machine with one precondition removed.
    #[must_use]
    pub const fn without(self, guard: Guard) -> Self {
        Self(self.0 & !guard.bit())
    }

    /// Whether `guard` is enforced.
    #[must_use]
    pub const fn enforces(self, guard: Guard) -> bool {
        self.0 & guard.bit() != 0
    }
}

impl Default for Guards {
    fn default() -> Self {
        Self::ENFORCED
    }
}

/// How far the state space is explored.
///
/// The bound is part of every claim this crate makes, and it is carried in the result rather
/// than left in a comment: a proof over three records is a proof over three records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bound {
    /// How many records a run may declare.
    pub records: usize,
    /// The highest generation a bank may be sealed at.
    pub generations: u32,
}

impl Bound {
    /// The bound the spine proofs run at.
    pub const PROOF: Self = Self {
        records: 3,
        generations: 3,
    };
}

/// The ghost state of one run.
///
/// Ordered and hashable so that the explorer can put it in a set; a state is exactly its
/// records, its banks, what it dispatched, and whether the power is still on.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Journal {
    records: Vec<Record>,
    banks: [Bank; BANKS],
    dispatched: Vec<RecordId>,
    powered: bool,
    sealed_once: bool,
    /// The next id [`declare`](Self::declare) will hand out.
    ///
    /// Issue [#67](https://github.com/madmax983/waymaker/issues/67)'s identity scheme: a
    /// counter that only grows, rather than `records.len()`. Once [`begin_erase`](Self::begin_erase)
    /// can drop records from the middle of `records`, length is no longer a record's position
    /// in history, and reusing it would let a record dropped by an erase be reissued to a
    /// record that means something else — the one thing a reboot's redelivery proof cannot
    /// survive.
    next_id: u32,
}

impl Default for Journal {
    fn default() -> Self {
        Self::new()
    }
}

impl Journal {
    /// A device that has done nothing: no records, both banks erased, power on.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
            banks: [Bank::Erased; BANKS],
            dispatched: Vec::new(),
            powered: true,
            sealed_once: false,
            next_id: 0,
        }
    }

    /// Which bank a new record declared right now would belong to.
    ///
    /// [`recovering_bank`](Self::recovering_bank)'s answer, or [`BankId::A`] when that is
    /// `None`: a live writer always has somewhere to write, even under a relaxed [`Guards`]
    /// where authority is momentarily absent or ambiguous, which is what makes this total
    /// rather than an `Option`.
    fn current_bank(&self) -> BankId {
        self.recovering_bank().unwrap_or(BankId::A)
    }

    /// Which bank a reader would recover from right now, or `None` if there is nothing safe
    /// to boot.
    ///
    /// [`BankId::A`] by convention before the device's first seal — the same convention
    /// [`Journal::new`] and `from_parts` already carry, since a rung-0.1
    /// device has exactly one implicit bank; the sole authoritative bank once one exists;
    /// `None` when authority is absent or ambiguous, matching
    /// [`Invariant::SingleAuthority`](crate::invariant::Invariant::SingleAuthority)'s own
    /// refusal to pick one.
    #[must_use]
    pub fn recovering_bank(&self) -> Option<BankId> {
        if !self.sealed_once {
            return Some(BankId::A);
        }
        match self.authoritative().as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }

    /// Which bank holds `id`'s bytes, or `None` if no declared record has that id.
    #[must_use]
    pub fn bank_of(&self, id: RecordId) -> Option<BankId> {
        self.records
            .iter()
            .find(|record| record.id == id)
            .map(|record| record.bank)
    }

    /// The records the bank a reader would boot from has declared, in declaration order.
    ///
    /// [`Invariant::PrefixSafety`](crate::invariant::Invariant::PrefixSafety)'s first clause
    /// against: "this run declared" means the run recovery would boot into, not every record
    /// any bank has ever held. Empty when [`recovering_bank`](Self::recovering_bank) is
    /// `None`, which is what lets a reader that boots the wrong (retired) bank fail this
    /// clause instead of being compared against a run it does not belong to.
    #[must_use]
    pub fn declared(&self) -> Vec<RecordId> {
        let bank = self.recovering_bank();
        self.records
            .iter()
            .filter(|record| Some(record.bank) == bank)
            .map(|record| record.id)
            .collect()
    }

    /// The records this run declared, in declaration order.
    #[must_use]
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Both banks, in [`BankId::ALL`] order.
    #[must_use]
    pub const fn banks(&self) -> &[Bank; BANKS] {
        &self.banks
    }

    /// One bank.
    #[must_use]
    pub fn bank(&self, id: BankId) -> Bank {
        self.banks.get(id.index()).copied().unwrap_or(Bank::Erased)
    }

    /// The schedule records of effects this run really handed to the world.
    #[must_use]
    pub fn dispatched(&self) -> &[RecordId] {
        &self.dispatched
    }

    /// Whether the power is still on.
    #[must_use]
    pub const fn powered(&self) -> bool {
        self.powered
    }

    /// Whether any record is on media in part but not in whole.
    #[must_use]
    pub fn has_torn_record(&self) -> bool {
        self.records
            .iter()
            .any(|record| record.media == OnMedia::Partial)
    }

    /// The records that reached media at all, in declaration order, in the bank a reader
    /// would boot from.
    ///
    /// *Committed history* as design document §15's oracle means it — scoped to
    /// [`recovering_bank`](Self::recovering_bank) for the reason [`declared`](Self::declared)
    /// is: a record a retired bank still holds reached media once, but not in the run
    /// recovery is about.
    pub fn committed(&self) -> impl Iterator<Item = RecordId> + use<'_> {
        let bank = self.recovering_bank();
        self.records
            .iter()
            .filter(move |record| Some(record.bank) == bank && record.media != OnMedia::Absent)
            .map(|record| record.id)
    }

    /// The records recovery is required to produce, in the bank a reader would boot from.
    ///
    /// Scoped to [`recovering_bank`](Self::recovering_bank) for the reason
    /// [`declared`](Self::declared) is: a record acknowledged in a bank a later swap
    /// superseded is a promise the run recovery boots into was never asked to keep, and
    /// `crate::invariant::acknowledged_durability` and [`legal_recoveries`](Self::legal_recoveries)
    /// both rest on this being true rather than filtering it out twice.
    pub fn acknowledged(&self) -> impl Iterator<Item = RecordId> + use<'_> {
        let bank = self.recovering_bank();
        self.records
            .iter()
            .filter(move |record| Some(record.bank) == bank && record.acknowledged)
            .map(|record| record.id)
    }

    /// What the specified reader produces from this state's media.
    ///
    /// The longest prefix of declaration order, *in the bank a reader would boot from*, whose
    /// records are wholly on media. A reader walks an append-only journal from the start and
    /// stops at the first frame it cannot accept, so the stopping rule is the specification of
    /// recovery, not a convenience: everything after a gap is unreachable whether or not its
    /// bytes are there — and everything in a bank that is not the one authority is
    /// unreachable full stop, which is issue [#67](https://github.com/madmax983/waymaker/issues/67)'s
    /// "never recover the old run as current" as a fact about this function rather than a
    /// hope about its caller.
    ///
    /// Empty when [`recovering_bank`](Self::recovering_bank) is `None`: a device with no
    /// authority, or with two banks claiming it, has nothing safe to boot.
    ///
    /// This is the one definition; [`crate::reader::Specified`] delegates to it, so the
    /// reader the proofs quantify over and the reader this type describes cannot drift.
    #[must_use]
    pub fn recover(&self) -> Vec<RecordId> {
        let bank = self.recovering_bank();
        self.records
            .iter()
            .filter(|record| Some(record.bank) == bank)
            .take_while(|record| record.is_recoverable())
            .map(|record| record.id)
            .collect()
    }

    /// Every recovery design document §15 permits from this state.
    ///
    /// "Recovery may include an unacknowledged **complete** record, but it may never lose an
    /// acknowledged one": every prefix of [`recover`](Self::recover) that still holds every
    /// acknowledged record — membership, exactly as
    /// [`waymaker_fault::verify_oracle`] states it. Stated as a set rather than as a function because recovery is a
    /// relation — a reader that stops one record early is correct, and a specification that
    /// insisted on the longest answer would fail a correct reader.
    #[must_use]
    pub fn legal_recoveries(&self) -> BTreeSet<Vec<RecordId>> {
        let full = self.recover();
        (0..=full.len())
            .filter_map(|length| full.get(..length))
            // Membership, not a count. Under the specified machine the acknowledged records
            // are exactly the first few of `recover()`, so "holds every acknowledged record"
            // and "is at least as long as there are acknowledged records" agree — but that
            // is a *theorem* about this machine, proved in `tests/machine.rs`, and stating
            // the rule as a length here would make this function and
            // [`crate::invariant::holds`] disagree about the same history the moment a guard
            // is relaxed. Two judgements that only agree via a lemma proved elsewhere are
            // not two judgements.
            .filter(|prefix| self.acknowledged().all(|id| prefix.contains(&id)))
            .map(<[RecordId]>::to_vec)
            .collect()
    }

    /// The banks a reader would boot from. Design document §15's fourth oracle line counts
    /// this, and §02 decision 7 says the count is one.
    #[must_use]
    pub fn authoritative(&self) -> Vec<BankId> {
        let highest = BankId::ALL
            .into_iter()
            .filter_map(|id| self.bank(id).authoritative_generation())
            .max();
        let Some(highest) = highest else {
            return Vec::new();
        };
        BankId::ALL
            .into_iter()
            .filter(|id| self.bank(*id).authoritative_generation() == Some(highest))
            .collect()
    }

    /// Whether any bank has *ever* carried a durable seal.
    ///
    /// History, not current state, and the difference is the whole point: a device that
    /// never sealed is not a device that lost its authority, and a device that sealed once
    /// and has no authority now is exactly
    /// [`waymaker_fault::Breach::NoAuthoritativeBank`]. Reading the current banks instead
    /// would make the fourth guarantee unfalsifiable — nothing to boot from would answer
    /// "this device never had authority" and pass.
    #[must_use]
    pub const fn has_sealed(&self) -> bool {
        self.sealed_once
    }

    /// This state as the ledger design document §15's oracle judges a recovery against.
    ///
    /// The abstraction runs this way on purpose: the ghost model is the specification and
    /// [`waymaker_fault::verify_recovery`] is the implementation of the judgement, so the
    /// model produces the oracle's input rather than the other way round.
    /// Scoped to [`recovering_bank`](Self::recovering_bank): `waymaker_fault`'s oracle judges
    /// one run at a time, and a retired bank's leftover bytes are a different run's, not a
    /// second copy of this one's history to be confused with it.
    #[must_use]
    pub fn ledger(&self) -> Ledger {
        let bank = self.recovering_bank();
        Ledger::new(
            self.records
                .iter()
                .filter(|record| Some(record.bank) == bank)
                .map(|record| {
                    (
                        record.id,
                        record.durability(),
                        record.media == OnMedia::Partial,
                    )
                })
                .collect(),
        )
    }

    /// Every transition worth trying from this state, legal or not.
    ///
    /// The explorer asks [`step`](Self::step) which of them are legal; this is the alphabet,
    /// and it is bounded by `bound` so that the candidate set is finite.
    #[must_use]
    pub fn alphabet(bound: Bound) -> Vec<Transition> {
        let mut alphabet = vec![
            Transition::Declare(Role::Schedule),
            Transition::Declare(Role::Outcome),
            Transition::Barrier,
            Transition::Tear,
            Transition::PowerLoss,
            Transition::Reboot,
        ];
        for index in 0..bound.records {
            let id = RecordId(u32::try_from(index).unwrap_or(u32::MAX));
            alphabet.push(Transition::Program(id));
            alphabet.push(Transition::FailedProgram(id));
            alphabet.push(Transition::Dispatch(id));
        }
        for bank in BankId::ALL {
            alphabet.push(Transition::BeginErase(bank));
            alphabet.push(Transition::CommitErase(bank));
            alphabet.push(Transition::BeginSeal(bank));
            alphabet.push(Transition::CommitSeal(bank));
        }
        alphabet
    }

    /// Applies `transition`, or says which precondition refused it.
    ///
    /// Total: every transition from every state has exactly one of two answers. The only
    /// transitions that may leave the state unchanged are the two genuinely idempotent ones,
    /// [`Transition::Barrier`]; `tests/machine.rs` refuses a no-op from any other.
    ///
    /// # Errors
    ///
    /// [`Illegal`], naming the precondition that refused it.
    pub fn step(
        &self,
        transition: Transition,
        guards: Guards,
        bound: Bound,
    ) -> Result<Self, Illegal> {
        // `Reboot` is the one exception to "the power is gone and nothing happens after
        // that": it is the only transition legal while unpowered, and it is legal only then.
        if transition == Transition::Reboot {
            if self.powered {
                return Err(Illegal::AlreadyPowered);
            }
            let mut next = self.clone();
            next.reboot();
            return Ok(next);
        }
        if !self.powered {
            return Err(Illegal::PowerIsGone);
        }
        let mut next = self.clone();
        match transition {
            Transition::Declare(role) => next.declare(role, bound)?,
            Transition::Program(id) | Transition::FailedProgram(id) => {
                let whole = matches!(transition, Transition::Program(_));
                next.write(id, whole, guards)?;
            }
            Transition::Barrier => next.barrier(guards),
            Transition::Dispatch(id) => next.dispatch(id, guards)?,
            Transition::BeginErase(bank) => next.begin_erase(bank, guards)?,
            Transition::CommitErase(bank) => next.commit_erase(bank)?,
            Transition::BeginSeal(bank) => next.begin_seal(bank, guards, bound)?,
            Transition::CommitSeal(bank) => next.commit_seal(bank)?,
            Transition::Tear => next.tear(guards)?,
            Transition::PowerLoss => next.powered = false,
            Transition::Reboot => unreachable!("handled above"),
        }
        Ok(next)
    }

    /// Gives the next record in declaration order an identity, in the current bank. Nothing
    /// reaches media.
    ///
    /// # Postconditions
    ///
    /// One record longer, and identical in every other dimension. The new record is
    /// [`OnMedia::Absent`] and unacknowledged, its id comes from a counter that only grows —
    /// issue [#67](https://github.com/madmax983/waymaker/issues/67)'s identity scheme, rather
    /// than a position an erase could later reuse — and no earlier record changes. Held over
    /// every edge by `tests/machine.rs`'s `a_declared_record_is_never_renumbered_or_removed`
    /// and by the census's requirement that a record arrive [`Durability::Attempted`] and in
    /// no other state. `bound.records` caps how many records this run may ever declare in
    /// total, not how many are resident at once, so an erased bank does not reopen capacity a
    /// swap already spent.
    fn declare(&mut self, role: Role, bound: Bound) -> Result<(), Illegal> {
        if self.next_id as usize >= bound.records {
            return Err(Illegal::CapacityReached);
        }
        let bank = self.current_bank();
        let unresolved = self.unresolved_schedule_in(bank).is_some();
        if (role == Role::Schedule) == unresolved {
            return Err(Illegal::OutOfProtocolOrder);
        }
        let id = RecordId(self.next_id);
        self.next_id += 1;
        self.records.push(Record {
            id,
            role,
            media: OnMedia::Absent,
            acknowledged: false,
            bank,
        });
        Ok(())
    }

    /// The schedule record `bank` has declared and not yet declared an outcome for.
    ///
    /// At most one per bank, which is the whole of §11's order: history is sequential, so an
    /// effect is scheduled only after the previous one's outcome is committed. Scoped to one
    /// bank rather than to the whole run: a bank swap starts a fresh run (§10's
    /// `continue_as_new`) that owes nothing to whatever the retired bank left unresolved —
    /// issue [#95](https://github.com/madmax983/waymaker/issues/95) is the accepted cost of
    /// that, not something this function should pretend does not happen.
    fn unresolved_schedule_in(&self, bank: BankId) -> Option<&Record> {
        let mut open = None;
        for record in self.records.iter().filter(|record| record.bank == bank) {
            match record.role {
                Role::Schedule => open = Some(record),
                Role::Outcome => open = None,
            }
        }
        open
    }

    /// Puts `id`'s bytes on media, wholly or in part.
    ///
    /// # Postconditions
    ///
    /// Exactly one record changes, from [`OnMedia::Absent`] to [`OnMedia::Whole`] or
    /// [`OnMedia::Partial`]. No record's acknowledgment changes, and no record's bytes are
    /// taken back — `tests/machine.rs`'s `bytes_on_media_are_never_taken_back_within_a_run`
    /// is that second half over every edge. With [`Guard::AppendOnly`] enforced, every
    /// earlier record is already whole, which is what makes the written record extend a
    /// prefix rather than land behind a gap.
    fn write(&mut self, id: RecordId, whole: bool, guards: Guards) -> Result<(), Illegal> {
        let position = self.position_of(id).ok_or(Illegal::UndeclaredRecord)?;
        let target = self
            .records
            .get(position)
            .ok_or(Illegal::UndeclaredRecord)?;
        if target.media != OnMedia::Absent {
            return Err(Illegal::RecordAlreadyWritten);
        }
        let bank = target.bank;
        if guards.enforces(Guard::AppendOnly) && !self.whole_before(bank, position) {
            return Err(Illegal::EarlierRecordIncomplete);
        }
        let target = self
            .records
            .get_mut(position)
            .ok_or(Illegal::UndeclaredRecord)?;
        target.media = if whole {
            OnMedia::Whole
        } else {
            OnMedia::Partial
        };
        Ok(())
    }

    /// Waits for the durability barrier. Infallible: a barrier over a device with a
    /// half-written record on it returns perfectly well — what it must not do is claim that
    /// record, which is [`Guard::BarrierNeedsWhole`].
    ///
    /// # Postconditions
    ///
    /// Every [`OnMedia::Whole`] record is acknowledged and no other record is; nothing else
    /// changes, and no acknowledgment is taken back. The second is
    /// `an_acknowledged_record_is_never_un_acknowledged` over every edge, and the first is
    /// `under_the_specification_an_acknowledged_record_is_wholly_on_media` over every state.
    /// This is the one transition permitted to leave the state unchanged, and
    /// `no_legal_transition_leaves_the_state_unchanged_except_where_it_is_meant_to` is what
    /// keeps that permission to this one.
    fn barrier(&mut self, guards: Guards) {
        let claims = if guards.enforces(Guard::BarrierNeedsWhole) {
            OnMedia::Whole
        } else {
            OnMedia::Partial
        };
        for record in &mut self.records {
            if record.media == OnMedia::Whole || record.media == claims {
                record.acknowledged = true;
            }
        }
    }

    /// Hands the effect `id` schedules to the world.
    ///
    /// # Postconditions
    ///
    /// `id` is in [`dispatched`](Self::dispatched), which stays sorted and free of
    /// duplicates; no record and no bank changes. With [`Guard::DurableIntent`] enforced,
    /// `id` was already acknowledged when this ran — §02 decision 3, which is what makes
    /// [`crate::invariant::Invariant::DurableIntent`] hold rather than a thing to check
    /// afterwards.
    ///
    /// Nothing here refuses a dispatch whose record's bank a later swap has since retired:
    /// design document §10's swap consumes the old run's writer in the real firmware, but
    /// nothing here rests on that being true, because
    /// [`crate::invariant::holds`]'s `DurableIntent` clause already treats such a dispatch as
    /// moot rather than as a breach — the old run is superseded either way, and a precondition
    /// guarding against it would be a guard `tests/necessity.rs` could delete without breaking
    /// a single proof.
    fn dispatch(&mut self, id: RecordId, guards: Guards) -> Result<(), Illegal> {
        let record = self
            .records
            .iter()
            .find(|record| record.id == id)
            .ok_or(Illegal::UndeclaredRecord)?;
        let is_schedule = record.role == Role::Schedule;
        if guards.enforces(Guard::DispatchNeedsASchedule) && !is_schedule {
            return Err(Illegal::NotAScheduleRecord);
        }
        // §11's order: the effect goes to the world after its intent is durable and before
        // its outcome is recorded. A dispatch against a schedule history has already
        // resolved is an effect happening after the record that says it finished. Checked
        // only for a schedule, so that removing the guard above reaches the state it exists
        // to forbid rather than being stopped here instead.
        if is_schedule && self.unresolved_schedule_in(record.bank).map(|open| open.id) != Some(id) {
            return Err(Illegal::OutOfProtocolOrder);
        }
        if self.dispatched.contains(&id) {
            return Err(Illegal::AlreadyDispatched);
        }
        if guards.enforces(Guard::DurableIntent) && !record.acknowledged {
            return Err(Illegal::IntentNotDurable);
        }
        self.dispatched.push(id);
        self.dispatched.sort_unstable();
        Ok(())
    }

    /// Begins erasing a bank, clearing its seal before the erase has returned and destroying
    /// every record it held.
    ///
    /// # Postconditions
    ///
    /// The named bank is [`Bank::Erasing`] and is not bootable; the other bank is untouched.
    /// Every record whose `bank` was the named one is gone — issue
    /// [#67](https://github.com/madmax983/waymaker/issues/67)'s first gap, "erasing a bank
    /// destroys the journal in it" — and so is every dispatched id that pointed at one of
    /// them, so [`dispatched`](Self::dispatched) never outlives the record it names. With
    /// [`Guard::NeverEraseTheAuthority`] enforced the named bank was not the one a reader
    /// would boot from, so the authoritative generation does not fall —
    /// `the_authoritative_generation_never_goes_backwards`, over every edge.
    fn begin_erase(&mut self, bank: BankId, guards: Guards) -> Result<(), Illegal> {
        if self.bank(bank) == Bank::Erasing {
            return Err(Illegal::EraseAlreadyInFlight);
        }
        if guards.enforces(Guard::NeverEraseTheAuthority) && self.authoritative().contains(&bank) {
            return Err(Illegal::WouldEraseTheAuthority);
        }
        self.records.retain(|record| record.bank != bank);
        let remaining: BTreeSet<RecordId> = self.records.iter().map(|record| record.id).collect();
        self.dispatched.retain(|id| remaining.contains(id));
        self.set_bank(bank, Bank::Erasing);
        Ok(())
    }

    /// The erase returned, so the bank is blank.
    ///
    /// # Postconditions
    ///
    /// The named bank is [`Bank::Erased`] and may now be sealed; nothing else changes.
    fn commit_erase(&mut self, bank: BankId) -> Result<(), Illegal> {
        if self.bank(bank) != Bank::Erasing {
            return Err(Illegal::BankNotErasing);
        }
        self.set_bank(bank, Bank::Erased);
        Ok(())
    }

    /// Writes a bank's generation seal without waiting for it.
    ///
    /// # Postconditions
    ///
    /// The named bank is [`Bank::Sealing`] and still not bootable, so the authoritative bank
    /// is whichever it was — §02 decision 7's "only after its payload and generation seal
    /// are durable". With [`Guard::StrictGeneration`] enforced the pending generation
    /// strictly outranks the other bank's, which `a_new_seal_is_strictly_newer_than_the_bank_it_replaces`
    /// holds over every edge and which is what makes the two never tie.
    fn begin_seal(&mut self, bank: BankId, guards: Guards, bound: Bound) -> Result<(), Illegal> {
        if self.bank(bank) != Bank::Erased {
            return Err(Illegal::BankNotErased);
        }
        if matches!(self.bank(bank.other()), Bank::Sealing(_)) {
            return Err(Illegal::SealAlreadyInFlight);
        }
        let other = self.bank(bank.other()).authoritative_generation();
        let generation = if guards.enforces(Guard::StrictGeneration) {
            match other {
                None => 1,
                // Issue #67's smaller item: `seen.saturating_add(1)` used to answer `seen`
                // again once `seen` was `u32::MAX`, so a device at the ceiling could seal a
                // *second* bank at the same generation — the tie `Guard::StrictGeneration`
                // exists to make impossible, reached only because nothing here could ever
                // explore that far before a bank carried a hand-built generation. Refusing
                // instead matches `Generation::successor`'s real behaviour (ADR 0017): the
                // firmware treats the ceiling as exhausted, not as a value worth repeating.
                Some(seen) => seen.checked_add(1).ok_or(Illegal::GenerationExhausted)?,
            }
        } else {
            other.unwrap_or(1)
        };
        if generation > bound.generations {
            return Err(Illegal::GenerationExhausted);
        }
        self.set_bank(bank, Bank::Sealing(generation));
        Ok(())
    }

    /// Waits for the seal's barrier. §02 decision 7's "only after ... are durable".
    ///
    /// # Postconditions
    ///
    /// The named bank is [`Bank::Sealed`] at the generation its seal was *written* at — not
    /// one this transition chooses, which `committing_a_seal_keeps_the_generation_the_seal_was_written_at`
    /// holds over every edge — and the device has now sealed at least once, so
    /// [`crate::invariant::Invariant::SingleAuthority`] applies to it from here on.
    fn commit_seal(&mut self, bank: BankId) -> Result<(), Illegal> {
        let Bank::Sealing(generation) = self.bank(bank) else {
            return Err(Illegal::BankNotSealing);
        };
        self.set_bank(bank, Bank::Sealed(generation));
        self.sealed_once = true;
        Ok(())
    }

    /// The power goes away part-way through the open record's program.
    ///
    /// # Postconditions
    ///
    /// Exactly one record moves from [`OnMedia::Absent`] to [`OnMedia::Partial`], and the
    /// power is off — so nothing else happens in this run, which
    /// `the_power_going_away_is_the_end_of_the_run` holds over every state. The torn record
    /// is the last one on media and nothing behind it is acknowledged, which is the lemma
    /// acknowledged durability rests on and which `tests/spine.rs` proves rather than
    /// assumes. The record has to be in [`current_bank`](Self::current_bank): nothing is
    /// physically being programmed into a bank a swap has already retired, so a dangling
    /// declared-but-unwritten record left behind there is not a live write a power loss can
    /// tear.
    fn tear(&mut self, guards: Guards) -> Result<(), Illegal> {
        let bank = self.current_bank();
        let position = self
            .records
            .iter()
            .position(|record| record.bank == bank && record.media == OnMedia::Absent)
            .ok_or(Illegal::NoOpenRecord)?;
        if guards.enforces(Guard::AppendOnly) && !self.whole_before(bank, position) {
            return Err(Illegal::EarlierRecordIncomplete);
        }
        let target = self
            .records
            .get_mut(position)
            .ok_or(Illegal::NoOpenRecord)?;
        target.media = OnMedia::Partial;
        self.powered = false;
        Ok(())
    }

    /// The device powers back on. History is exactly what recovery would produce; anything
    /// declared but not fully durable at the crash is gone, which is what makes reboot a
    /// genuine restart rather than a resumed write.
    ///
    /// # Postconditions
    ///
    /// [`powered`](Self::powered) is `true`; [`records`](Self::records) is
    /// [`recover`](Self::recover)'s answer, unchanged, so every id it carries keeps the
    /// acknowledgment and media state it crossed the crash with; [`dispatched`](Self::dispatched)
    /// keeps only ids that still have a record. `next_id` and every bank's seal survive
    /// untouched — a reboot learns nothing new about either, and a record dropped here can
    /// never be reissued, which is issue [#67](https://github.com/madmax83/waymaker/issues/67)'s
    /// identity scheme doing its job.
    fn reboot(&mut self) {
        let kept = self.recover();
        self.records.retain(|record| kept.contains(&record.id));
        self.dispatched.retain(|id| kept.contains(id));
        self.powered = true;
    }

    /// The record and dispatch fields of a state, built without going through
    /// [`step`](Self::step).
    ///
    /// The abstraction function's codomain, and the only reason it is allowed to exist is
    /// that `tests/refinement.rs` requires every state it builds to match a state the search
    /// really reached. Crate-private so that no test can reach for it as a shortcut past the
    /// preconditions.
    ///
    /// Every record is assumed to be in [`BankId::A`]: `waymaker-fault`'s harness drives one
    /// writer over one bank's region, so `records` never names a second one, and both banks
    /// start [`Bank::Erased`] here regardless — the reconstructed state has never sealed, so
    /// [`recovering_bank`](Self::recovering_bank) already answers `Some(BankId::A)` by the
    /// same convention. `next_id` is set past every id `records` carries so a caller that went
    /// on to call [`step`](Self::step) — nothing in this crate does — could not reissue one.
    pub(crate) fn from_parts(records: Vec<Record>, dispatched: Vec<RecordId>) -> Self {
        let mut sorted = dispatched;
        sorted.sort_unstable();
        sorted.dedup();
        let next_id = records
            .iter()
            .map(|record| record.id.0.saturating_add(1))
            .max()
            .unwrap_or(0);
        Self {
            records,
            banks: [Bank::Erased; BANKS],
            dispatched: sorted,
            powered: false,
            sealed_once: false,
            next_id,
        }
    }

    /// Where `id` sits in declaration order.
    fn position_of(&self, id: RecordId) -> Option<usize> {
        self.records.iter().position(|record| record.id == id)
    }

    /// Whether every record of `bank` declared before `position` is wholly on media.
    ///
    /// Scoped to `bank` rather than to every record before `position`: a record a retired
    /// bank left torn or absent must not block append-only writes to the bank that
    /// superseded it, which is exactly what makes a live swap while a torn record sits behind
    /// it — issue [#67](https://github.com/madmax983/waymaker/issues/67)'s third gap,
    /// compaction — representable at all.
    fn whole_before(&self, bank: BankId, position: usize) -> bool {
        self.records.get(..position).is_some_and(|earlier| {
            earlier
                .iter()
                .filter(|record| record.bank == bank)
                .all(|record| record.media == OnMedia::Whole)
        })
    }

    fn set_bank(&mut self, id: BankId, bank: Bank) {
        if let Some(slot) = self.banks.get_mut(id.index()) {
            *slot = bank;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue [#67](https://github.com/madmax983/waymaker/issues/67)'s smaller item: no bound
    /// small enough for the exhaustive search to finish ever reaches a generation near
    /// [`u32::MAX`], so a hand-built state is the only way to reach the ceiling at all. A unit
    /// test rather than an integration one, because [`Journal`]'s fields are private and
    /// nothing outside this crate has a legitimate reason to build a bank already sealed nine
    /// hundred billion generations in.
    #[test]
    fn a_generation_at_the_ceiling_is_refused_rather_than_tied_with_the_other_bank() {
        let near_ceiling = Journal {
            records: Vec::new(),
            banks: [Bank::Sealed(u32::MAX - 1), Bank::Erased],
            dispatched: Vec::new(),
            powered: true,
            sealed_once: true,
            next_id: 0,
        };
        let bound = Bound {
            records: 3,
            generations: u32::MAX,
        };

        // One more generation is still short of the ceiling, so it succeeds.
        let sealing = near_ceiling
            .step(Transition::BeginSeal(BankId::B), Guards::ENFORCED, bound)
            .expect("u32::MAX - 1 + 1 does not overflow");
        assert_eq!(sealing.bank(BankId::B), Bank::Sealing(u32::MAX));
        let sealed = sealing
            .step(Transition::CommitSeal(BankId::B), Guards::ENFORCED, bound)
            .expect("committing a seal that was legal to begin");
        assert_eq!(sealed.bank(BankId::B), Bank::Sealed(u32::MAX));

        // Reclaiming the now-retired bank and sealing it again would need generation
        // `u32::MAX + 1`. `seen.saturating_add(1)` used to answer `u32::MAX` again here —
        // the same generation bank B already holds — which is the tie
        // `Guard::StrictGeneration` exists to make impossible. It is refused instead.
        let erased = sealed
            .step(Transition::BeginErase(BankId::A), Guards::ENFORCED, bound)
            .and_then(|erasing| {
                erasing.step(Transition::CommitErase(BankId::A), Guards::ENFORCED, bound)
            })
            .expect("reclaiming the bank the swap retired");
        assert_eq!(
            erased.step(Transition::BeginSeal(BankId::A), Guards::ENFORCED, bound),
            Err(Illegal::GenerationExhausted)
        );
    }
}
