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
//! What this does not cover is banks: rung 0.1 has no two-bank adapter to drive, so the
//! fourth guarantee is discharged against the model alone and
//! [`waymaker_spec::obligation`] says so in a row rather than leaving it to be noticed.

use std::cell::RefCell;
use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, EffectSeq, RecordRef};
use waymaker_fault::{Durability, FaultError, Harness, RecordId, Run, Session, verify_recovery};
use waymaker_flash::frame::{self, ProgramAlign, Scan};
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_spec::explore::explore;
use waymaker_spec::model::{Bound, Guards, Journal};
use waymaker_spec::reader::{Mutant, Reader, Specified};
use waymaker_spec::refine::{Observation, abstraction};

/// The activity every schedule record below names.
const DOWNLOAD: ActivityKind = ActivityKind(1);

/// How many records the journal writer declares.
const RECORDS: u32 = 3;

/// The bound the refinement is checked against.
///
/// Four records, because the effect-protocol writer declares two per effect and the model
/// has to be able to describe a two-effect run. One generation, because no writer here
/// touches a bank.
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
fn recovered(image: &[u8], numbering: Numbering) -> Vec<RecordId> {
    Scan::new(image, align())
        .take_while(Result::is_ok)
        .flatten()
        .filter_map(|record| numbering.id_of(&record))
        .collect()
}

/// How a writer numbers its records, so that a recovered frame can be named the way the
/// model named it.
///
/// The model allocates ids in declaration order — 0, 1, 2 — because that is what
/// [`Transition::Declare`](waymaker_spec::model::Transition::Declare) does, so each writer
/// has to say how its frames map onto that. Making it explicit is the point: an abstraction
/// with a numbering baked into it would fit exactly one writer.
#[derive(Clone, Copy, Debug)]
enum Numbering {
    /// One schedule record per declaration: effect `n` is record `n - 1`.
    OneRecordPerEffect,
    /// A schedule and a completion per effect: effect `n` is records `2(n-1)` and
    /// `2(n-1) + 1`.
    ScheduleThenCompletion,
}

impl Numbering {
    const fn id_of(self, record: &RecordRef<'_>) -> Option<RecordId> {
        match (self, record) {
            (Self::OneRecordPerEffect, RecordRef::EffectScheduled { seq, .. }) => {
                Some(RecordId(seq.0.wrapping_sub(1)))
            }
            (Self::ScheduleThenCompletion, RecordRef::EffectScheduled { seq, .. }) => {
                Some(RecordId(seq.0.wrapping_sub(1).wrapping_mul(2)))
            }
            (Self::ScheduleThenCompletion, RecordRef::EffectCompleted { seq, .. }) => Some(
                RecordId(seq.0.wrapping_sub(1).wrapping_mul(2).wrapping_add(1)),
            ),
            _ => None,
        }
    }
}

/// Three records, one per barrier, numbered 0, 1, 2 in declaration order.
fn journal(session: &mut Session) -> Result<(), FaultError> {
    let mut at = 0;
    for index in 0..RECORDS {
        session.begin_record(RecordId(index));
        append(
            session,
            &mut at,
            &RecordRef::EffectScheduled {
                seq: EffectSeq(index.wrapping_add(1)),
                kind: DOWNLOAD,
                input_len: 4,
                input_crc: frame::input_digest(b"blob"),
            },
        )?;
        session.barrier()?;
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
    for effect in 1..=2_u32 {
        let schedule = RecordId(effect.wrapping_sub(1).wrapping_mul(2));
        session.begin_record(schedule);
        append(
            session,
            &mut at,
            &RecordRef::EffectScheduled {
                seq: EffectSeq(effect),
                kind: DOWNLOAD,
                input_len: 4,
                input_crc: frame::input_digest(b"blob"),
            },
        )?;
        session.barrier()?;

        // §02 decision 3: the intent is durable, so the world may now be changed.
        dispatched.borrow_mut().push(schedule);

        session.begin_record(RecordId(schedule.0.wrapping_add(1)));
        append(
            session,
            &mut at,
            &RecordRef::EffectCompleted {
                seq: EffectSeq(effect),
                result: b"ok",
            },
        )?;
        session.barrier()?;
    }
    Ok(())
}

fn drive<W, E>(writer: W) -> Vec<Run>
where
    W: FnMut(&mut Session) -> Result<(), E>,
    E: std::fmt::Debug,
{
    match Harness::new(geometry()).run(writer) {
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
fn check(
    runs: &[Run],
    numbering: Numbering,
    dispatched: &[Vec<RecordId>],
) -> BTreeSet<Vec<RecordId>> {
    let reachable = reachable_observations();
    let mut histories = BTreeSet::new();

    for (index, run) in runs.iter().enumerate() {
        let effects = dispatched.get(index).cloned().unwrap_or_default();
        let observed = abstraction(run.ledger(), &effects);

        // 1. The model describes this crash.
        assert!(
            reachable.contains(&observed),
            "run {index} ended in {observed:?}, which the model says is unreachable"
        );

        // 2. The real reader and the specified reader produce the same history.
        let Ok(state) = Journal::reconstructed(&observed) else {
            unreachable!("the harness never builds a torn acknowledged record")
        };
        let real = recovered(run.image(), numbering);
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
    let histories = check(&runs, Numbering::OneRecordPerEffect, &[]);

    // Every prefix length really occurs. Without this the refinement could be holding
    // because every crash point recovered the same thing.
    let lengths: BTreeSet<usize> = histories.iter().map(Vec::len).collect();
    assert_eq!(lengths, BTreeSet::from([0, 1, 2, 3]));
}

#[test]
fn the_real_effect_protocol_refines_the_specification_at_every_crash_point() {
    // The dispatch log is per run, so it is captured per run: the harness re-runs the writer
    // once per crash point, and an effect dispatched in one run did not happen in another.
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

    check(&runs, Numbering::ScheduleThenCompletion, &dispatched);
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
                let observed = abstraction(run.ledger(), &[]);
                let Ok(state) = Journal::reconstructed(&observed) else {
                    return false;
                };
                recovered(run.image(), Numbering::OneRecordPerEffect) != mutant.recover(&state)
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
fn the_abstraction_refuses_an_observation_no_run_could_have_produced() {
    // The glue is unverified, so its refusals are tested rather than assumed. A ledger that
    // claims a barrier returned for a half-written record describes nothing, and the state
    // builder says so instead of quietly repairing it.
    let impossible = Observation {
        records: vec![(RecordId(0), Durability::Acknowledged, true)],
        dispatched: Vec::new(),
    };
    let error = Journal::reconstructed(&impossible).expect_err("torn and acknowledged");
    assert!(
        error.to_string().contains("torn and acknowledged"),
        "{error}"
    );

    let also_impossible = Observation {
        records: vec![(RecordId(0), Durability::Attempted, true)],
        dispatched: Vec::new(),
    };
    let error = Journal::reconstructed(&also_impossible).expect_err("torn and absent");
    assert!(error.to_string().contains("never reached media"), "{error}");
}

#[test]
fn the_abstraction_reports_what_the_ledger_says_and_nothing_else() {
    let runs = drive(journal);
    for run in &runs {
        let observed = abstraction(run.ledger(), &[RecordId(0), RecordId(0)]);
        assert_eq!(
            observed.records.len(),
            run.ledger().len(),
            "the abstraction invented or dropped a record"
        );
        for (id, state, torn) in &observed.records {
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
