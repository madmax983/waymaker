//! The firmware refines the specification, at every crash point the injector lists.
//!
//! A ghost model nothing is compared against is a second implementation with no tests. This
//! file is the comparison: `waymaker-flash`'s real record codec is driven through
//! `waymaker-fault`'s crash injector, and every run is abstracted into the model and asked
//! three questions.
//!
//! 1. **Is this a state the model says is reachable?** A crash the firmware can be in and
//!    the specification cannot describe means the specification is wrong about the firmware,
//!    and every proof about it is about something else.
//! 2. **Does the real reader produce what the specified reader produces?** `Scan` over the
//!    media the crash left, against
//!    [`Specified`](waymaker_spec::reader::Specified) over the abstracted state. This is the
//!    claim that makes the model load-bearing rather than decorative.
//! 3. **Does design document §15's oracle agree?** Three independent judgements of one run.
//!
//! What this does not cover is banks: no writer in this file drives the two-bank adapter issue #22 added, so the
//! fourth guarantee is discharged against the model alone and
//! [`waymaker_spec::obligation`] says so in a row rather than leaving it to be noticed.

use std::cell::RefCell;
use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, EffectSeq, RecordRef};
use waymaker_fault::{Durability, FaultError, Harness, RecordId, Run, Session, verify_recovery};
use waymaker_flash::frame::{self, ProgramAlign, Scan};
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_spec::explore::explore;
use waymaker_spec::model::{Bound, Guards, Journal, Role};
use waymaker_spec::reader::{Mutant, Reader, Specified};
use waymaker_spec::refine::{Observation, abstraction};

/// The activity every schedule record below names.
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// How many effects each writer records. Two records each, per design document §11.
const EFFECTS: u32 = 2;

/// The bound the refinement is checked against.
///
/// Four records, because every writer declares two per effect and the model has to be able
/// to describe a two-effect run. One generation, because no writer here touches a bank.
const REFINEMENT: Bound = Bound {
    records: 4,
    generations: 1,
};

const CEILING: usize = 400_000;

/// One erase block, which is the whole journal region: `Scan`'s erased-tail rule is stated
/// over the journal and nothing else.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 256, 4, 1) else {
        unreachable!("256 is one whole 256-byte block of 4-byte units of single bytes")
    };
    geometry
}

/// The same capacity in four erase blocks, so a journal crosses a block boundary.
///
/// A second geometry because §15 asks for "random record sequences and storage geometries",
/// and one geometry is a sample of size one: a device whose journal never crosses a block is
/// a device on which a whole class of offset arithmetic is never exercised.
fn blocks() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 64, 4, 1) else {
        unreachable!("256 is four whole 64-byte blocks of 4-byte units of single bytes")
    };
    geometry
}

fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    align
}

/// Appends one record, padded to the program granularity.
fn append(session: &mut Session, at: &mut u32, record: &RecordRef<'_>) -> Result<(), FaultError> {
    let mut buffer = [0_u8; 64];
    let Ok(written) = frame::encode(record, align(), &mut buffer) else {
        unreachable!("64 bytes is more than any record this file writes")
    };
    let Some(bytes) = buffer.get(..written) else {
        unreachable!("`encode` reports what it wrote")
    };
    session.program(*at, bytes)?;
    *at = at.wrapping_add(u32::try_from(written).unwrap_or(u32::MAX));
    Ok(())
}

/// Every record `Scan` can recover from `image`, as the ids the writers assigned.
///
/// Ids are declaration indices — 0, 1, 2 — because that is how the model's
/// [`Transition::Declare`](waymaker_spec::model::Transition::Declare) allocates them, and an
/// abstraction that had to renumber would be an abstraction with a translation nobody
/// checks. A record is identified by what is *in* it, so "recovery produced a prefix" stays
/// a statement about content rather than about counting.
fn recovered(image: &[u8]) -> Vec<RecordId> {
    let read: Vec<RecordRef<'_>> = Scan::new(image, align())
        .take_while(Result::is_ok)
        .flatten()
        .collect();
    let named: Vec<RecordId> = read.iter().filter_map(record_id).collect();
    // A record the reader produced and the numbering cannot name would be dropped here, and
    // the refinement check would then compare a *shorter* history against the model and pass.
    // Refused loudly instead: the writers under test emit two record kinds, and a third
    // arriving is a change to the fixture rather than something to quietly ignore.
    assert_eq!(
        named.len(),
        read.len(),
        "the scan recovered a record kind this file does not name, so the comparison below \
         would be against a history with a hole in it"
    );
    named
}

/// Design document §11's numbering: effect `n` is records `2n` and `2n + 1`.
///
/// One scheme rather than one per writer, because there is only one legal shape. A run that
/// wrote three schedules in a row is not a journal `waymaker_core::ReplayCursor` will
/// replay — it refuses "a schedule while one is unresolved" as malformed history — and the
/// model refuses it too, with `Illegal::OutOfProtocolOrder`. Every writer here therefore
/// alternates, which is what makes these runs journals rather than byte sequences.
const fn record_id(record: &RecordRef<'_>) -> Option<RecordId> {
    match record {
        RecordRef::EffectScheduled { seq, .. } => Some(RecordId(seq.0.wrapping_mul(2))),
        RecordRef::EffectCompleted { seq, .. } => {
            Some(RecordId(seq.0.wrapping_mul(2).wrapping_add(1)))
        }
        RecordRef::RunStarted { .. }
        | RecordRef::EffectFailed { .. }
        | RecordRef::RunCompleted { .. }
        | RecordRef::RunFailed { .. } => None,
    }
}

/// What each record is for, derived from the same numbering.
const fn role_of(id: RecordId) -> Role {
    if id.0 % 2 == 0 {
        Role::Schedule
    } else {
        Role::Outcome
    }
}

/// A schedule and its completion per effect, each across a barrier.
fn journal(session: &mut Session) -> Result<(), FaultError> {
    let mut at = 0;
    for effect in 0..EFFECTS {
        session.begin_record(RecordId(effect.wrapping_mul(2)));
        append(session, &mut at, &schedule(effect))?;
        session.barrier()?;

        session.begin_record(RecordId(effect.wrapping_mul(2).wrapping_add(1)));
        append(session, &mut at, &completion(effect))?;
        session.barrier()?;
    }
    Ok(())
}

/// The schedule record for effect `n`, counting from [`EffectSeq::FIRST`].
///
/// Zero-based, because that is what `waymaker_core::ReplayCursor` will replay: a journal
/// whose first effect is numbered one is a sequence that skips, which it refuses as
/// malformed history. A refinement driven over a journal the kernel would not accept is a
/// refinement of something else.
const fn schedule(effect: u32) -> RecordRef<'static> {
    RecordRef::EffectScheduled {
        seq: EffectSeq(effect),
        kind: DOWNLOAD,
        input_len: 4,
        input_crc: frame::input_digest(b"blob"),
    }
}

/// The completion record for effect `n`.
const fn completion(effect: u32) -> RecordRef<'static> {
    RecordRef::EffectCompleted {
        seq: EffectSeq(effect),
        result: b"ok",
    }
}

/// A writer that does not give up when a program call fails.
///
/// Design document §12: "program and erase may fail". Every other writer here propagates the
/// error with `?` and the run ends, which means no refined run ever reaches *a live device
/// with a half-written record on it* — and that is the one state
/// [`Guard::BarrierNeedsWhole`](waymaker_spec::model::Guard::BarrierNeedsWhole) exists to
/// constrain, and the only firmware evidence
/// [`Transition::FailedProgram`](waymaker_spec::model::Transition::FailedProgram) can have.
/// Without this writer both are proved against the model and against nothing else.
///
/// It carries on exactly as far as the specification says it may: a barrier, which must not
/// claim the torn record, and then it stops. It does not retry at a new offset — an
/// append-only journal with a half-written record in it cannot advance, which is what
/// `Illegal::EarlierRecordIncomplete` says, and rung 0.2's compaction is where that is
/// answered.
fn journal_that_survives_a_failed_program(session: &mut Session) -> Result<(), FaultError> {
    let mut at = 0;
    for effect in 0..EFFECTS {
        session.begin_record(RecordId(effect.wrapping_mul(2)));
        match append(session, &mut at, &schedule(effect)) {
            Ok(()) => session.barrier()?,
            // The device is alive and a record is half on media. One barrier, which the
            // specification says must not acknowledge it, and then the run is over.
            Err(FaultError::InjectedFailure) => {
                let _ = session.barrier();
                return Ok(());
            }
            Err(other) => return Err(other),
        }

        session.begin_record(RecordId(effect.wrapping_mul(2).wrapping_add(1)));
        match append(session, &mut at, &completion(effect)) {
            Ok(()) => session.barrier()?,
            Err(FaultError::InjectedFailure) => {
                let _ = session.barrier();
                return Ok(());
            }
            Err(other) => return Err(other),
        }
    }
    Ok(())
}

/// Design document §11's shape: a schedule record crosses a barrier, *then* the effect is
/// dispatched, and only afterwards is a completion recorded.
fn effect_protocol(
    session: &mut Session,
    dispatched: &RefCell<Vec<RecordId>>,
) -> Result<(), FaultError> {
    let mut at = 0;
    for effect in 0..EFFECTS {
        let intent = RecordId(effect.wrapping_mul(2));
        session.begin_record(intent);
        append(session, &mut at, &schedule(effect))?;
        session.barrier()?;

        // §02 decision 3: the intent is durable, so the world may now be changed.
        dispatched.borrow_mut().push(intent);

        session.begin_record(RecordId(intent.0.wrapping_add(1)));
        append(session, &mut at, &completion(effect))?;
        session.barrier()?;
    }
    Ok(())
}

fn drive<W, E>(writer: W) -> Vec<Run>
where
    W: FnMut(&mut Session) -> Result<(), E>,
    E: std::fmt::Debug,
{
    drive_on(geometry(), writer)
}

fn drive_on<W, E>(geometry: Geometry, writer: W) -> Vec<Run>
where
    W: FnMut(&mut Session) -> Result<(), E>,
    E: std::fmt::Debug,
{
    match Harness::new(geometry).run(writer) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

/// Every observation the model says a run can end in.
fn reachable_observations() -> BTreeSet<Observation> {
    let explored = match explore(REFINEMENT, Guards::ENFORCED, CEILING) {
        Ok(explored) => explored,
        Err(error) => unreachable!("{error}"),
    };
    explored.states().iter().map(Journal::observation).collect()
}

/// Runs the three refinement questions over `runs`, and reports what it saw.
fn check(runs: &[Run], dispatched: &[Vec<RecordId>]) -> BTreeSet<Vec<RecordId>> {
    let reachable = reachable_observations();
    let mut histories = BTreeSet::new();

    for (index, run) in runs.iter().enumerate() {
        let effects = dispatched.get(index).cloned().unwrap_or_default();
        let observed = abstraction(run.ledger(), &effects, role_of);

        // 1. The model describes this crash.
        assert!(
            reachable.contains(&observed),
            "run {index} ended in {observed:?}, which the model says is unreachable"
        );

        // 2. The real reader and the specified reader produce the same history.
        let Ok(state) = Journal::reconstructed(&observed) else {
            unreachable!("the harness never builds a torn acknowledged record")
        };
        let real = recovered(run.image());
        assert_eq!(
            real,
            Specified.recover(&state),
            "run {index} ({:?}) recovered {real:?}, and the specification says {:?}",
            run.injection(),
            Specified.recover(&state)
        );

        // 3. §15's oracle agrees.
        assert!(
            verify_recovery(run.ledger(), &real).is_ok(),
            "run {index} ({:?}) recovered {real:?}, which the oracle refuses",
            run.injection()
        );

        histories.insert(real);
    }
    histories
}

#[test]
fn the_real_journal_refines_the_specification_at_every_crash_point() {
    let runs = drive(journal);
    assert!(runs.len() > 100, "only {} crash points", runs.len());
    let histories = check(&runs, &[]);

    // Every prefix length really occurs — nothing recovered through every record of a
    // two-effect run. Without this the refinement could be holding because every crash point
    // recovered the same thing.
    let lengths: BTreeSet<usize> = histories.iter().map(Vec::len).collect();
    let expected: BTreeSet<usize> = (0..=(EFFECTS as usize).saturating_mul(2)).collect();
    assert_eq!(lengths, expected);
}

#[test]
fn a_writer_that_survives_a_failed_program_refines_the_specification_too() {
    // The run the other writers never reach: a live device with a half-written record on it,
    // and a barrier issued over it. This is the only firmware evidence there is for
    // `Transition::FailedProgram` and for `Guard::BarrierNeedsWhole`, both of which are
    // otherwise proved against the model alone.
    for geometry in [geometry(), blocks()] {
        let runs = drive_on(geometry, journal_that_survives_a_failed_program);
        let histories = check(&runs, &[]);
        assert!(!histories.is_empty());

        // And the state it exists for is really reached: a torn record on a device that then
        // went on to ask for a barrier, with the torn record still unacknowledged.
        let survived = runs
            .iter()
            .filter(|run| {
                run.ledger().records().any(|(id, state)| {
                    run.ledger().torn(id) == Some(true) && state == Durability::PossiblyDurable
                })
            })
            .count();
        assert!(
            survived > 0,
            "no run left a torn record on a live device, so the barrier precondition has no \
             firmware evidence"
        );
    }
}

#[test]
fn the_refinement_reaches_the_dimensions_the_guarantees_are_about() {
    // Question 1 is a containment check, and a containment check passes for a sweep that
    // reaches nothing. So what the sweep reaches is asserted: torn records, acknowledged
    // records lost, dispatched effects, and every prefix length. Without this the refinement
    // could hold because the firmware never got anywhere interesting.
    let mut torn = 0_usize;
    let mut acknowledged_and_short = 0_usize;
    let mut observations = BTreeSet::new();
    for writer in [
        &journal as &dyn Fn(&mut Session) -> Result<(), FaultError>,
        &journal_that_survives_a_failed_program,
    ] {
        for geometry in [geometry(), blocks()] {
            for run in drive_on(geometry, |session| writer(session)) {
                let observed = abstraction(run.ledger(), &[], role_of);
                if observed.records.iter().any(|(.., torn_here)| *torn_here) {
                    torn += 1;
                }
                let history = recovered(run.image());
                if run.ledger().acknowledged().count() > 0
                    && history.len() < (EFFECTS as usize).saturating_mul(2)
                {
                    acknowledged_and_short += 1;
                }
                observations.insert(observed);
            }
        }
    }
    assert!(torn > 0, "no run ever tore a record");
    assert!(
        acknowledged_and_short > 0,
        "no run had to keep a record it had promised while losing one it had not"
    );
    assert!(
        observations.len() >= 12,
        "only {} distinct model states are refined, which is too few for question 1 to be \
         a check rather than a formality",
        observations.len()
    );
}

#[test]
fn the_real_effect_protocol_refines_the_specification_at_every_crash_point() {
    // The dispatch log is per run, so it is captured per run: the harness re-runs the writer
    // once per crash point, and an effect dispatched in one run did not happen in another.
    //
    // The alignment relies on `Harness::run` invoking the writer in the order it returns the
    // runs — the fault-free run first, then one per injection, which is what its
    // implementation does. `Run` carries no dispatch log of its own, so there is nothing to
    // key on; the length assertion below catches a count that drifted and would not catch a
    // reordering, which is the reason this comment names the assumption rather than leaving
    // it to be inferred.
    let log = RefCell::new(Vec::new());
    let per_run = RefCell::new(Vec::new());
    let runs = drive(|session| {
        log.borrow_mut().clear();
        let result = effect_protocol(session, &log);
        per_run.borrow_mut().push(log.borrow().clone());
        result
    });
    let dispatched = per_run.borrow().clone();
    assert_eq!(
        dispatched.len(),
        runs.len(),
        "one dispatch log per run, or the logs are lined up against the wrong runs"
    );
    assert!(
        dispatched.iter().any(|effects| !effects.is_empty()),
        "no run ever dispatched an effect, so durable intent is refined about nothing"
    );

    check(&runs, &dispatched);
}

#[test]
fn the_refinement_check_can_tell_the_specified_reader_from_a_wrong_one() {
    // The falsifier for question 2. `check` asserts that the real reader produces exactly
    // what the specification says, and that assertion is only worth something if some other
    // answer would have failed it. Every wrong reader in the catalogue is required to
    // disagree with the firmware on at least one real crash point.
    //
    // `Mutant::SkipsGaps` is excluded, and `tests/teeth.rs` is where that is established:
    // under the append-only precondition it is not a wrong reader at all, because no
    // reachable state has anything behind a gap for it to find.
    let runs = drive(journal);
    for mutant in Mutant::ALL {
        if mutant == Mutant::SkipsGaps {
            continue;
        }
        let disagreements = runs
            .iter()
            .filter(|run| {
                let observed = abstraction(run.ledger(), &[], role_of);
                let Ok(state) = Journal::reconstructed(&observed) else {
                    return false;
                };
                recovered(run.image()) != mutant.recover(&state)
            })
            .count();
        assert!(
            disagreements > 0,
            "a reader that {mutant} agrees with the firmware at every crash point, so the \
             refinement check cannot tell it from the specified one"
        );
    }
}

#[test]
fn a_reconstructed_state_cannot_falsify_the_fourth_guarantee() {
    // Written down as a test rather than left to be discovered. `Observation` carries no
    // banks, so `reconstructed` builds a state that has never sealed, and `SingleAuthority`
    // returns `Ok` for it whatever history it is handed — including one that is pure
    // invention. A caller with real banks to abstract — issue #22's `waymaker_flash::bank` is one, and abstracting it is still owed — gets three
    // guarantees judged and the fourth answered for free, and this is the assertion that
    // says so out loud.
    let nonsense = [RecordId(99), RecordId(7)];
    for run in drive(journal) {
        let observed = abstraction(run.ledger(), &[], role_of);
        let Ok(state) = Journal::reconstructed(&observed) else {
            unreachable!("the harness never builds a torn acknowledged record")
        };
        assert!(!state.has_sealed());
        assert!(state.authoritative().is_empty());
        assert!(
            waymaker_spec::invariant::holds(
                waymaker_spec::invariant::Invariant::SingleAuthority,
                &state,
                &nonsense,
            )
            .is_ok(),
            "the fourth guarantee has become falsifiable on a reconstructed state, which \
             would be an improvement — update this test and obligation.rs's owed note"
        );
    }
}

#[test]
fn the_abstraction_refuses_an_observation_no_run_could_have_produced() {
    // The glue is unverified, so its refusals are tested rather than assumed. A ledger that
    // claims a barrier returned for a half-written record describes nothing, and the state
    // builder says so instead of quietly repairing it.
    let impossible = Observation {
        records: vec![(RecordId(0), Role::Schedule, Durability::Acknowledged, true)],
        dispatched: Vec::new(),
    };
    let error = Journal::reconstructed(&impossible).expect_err("torn and acknowledged");
    assert!(
        error.to_string().contains("torn and acknowledged"),
        "{error}"
    );

    let also_impossible = Observation {
        records: vec![(RecordId(0), Role::Schedule, Durability::Attempted, true)],
        dispatched: Vec::new(),
    };
    let error = Journal::reconstructed(&also_impossible).expect_err("torn and absent");
    assert!(error.to_string().contains("never reached media"), "{error}");
}

#[test]
fn the_abstraction_reports_what_the_ledger_says_and_nothing_else() {
    let runs = drive(journal);
    for run in &runs {
        let observed = abstraction(run.ledger(), &[RecordId(0), RecordId(0)], role_of);
        assert_eq!(
            observed.records.len(),
            run.ledger().len(),
            "the abstraction invented or dropped a record"
        );
        for (id, _, state, torn) in &observed.records {
            assert_eq!(run.ledger().state(*id), Some(*state));
            assert_eq!(run.ledger().torn(*id), Some(*torn));
        }
        assert_eq!(
            observed.dispatched,
            vec![RecordId(0)],
            "the abstraction did not deduplicate the dispatch log"
        );
    }
}
