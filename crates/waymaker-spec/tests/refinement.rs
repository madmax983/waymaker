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
//! The first ten tests drive record-only writers, over the record dimension alone: each
//! passes `[Bank::Erased; BANKS]` into its [`Observation`] and asks nothing of the fourth
//! guarantee. The bank-swap tests near the end of the file are issue
//! [#73](https://github.com/madmax983/waymaker/issues/73)'s answer to that gap: they drive
//! `waymaker_flash::bank`'s real writer and ask question 1 of *its* real output.
//! [`waymaker_spec::obligation`] says what is still owed after that.

use std::cell::RefCell;
use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, EffectSeq, RecordRef, RunId};
use waymaker_fault::{Durability, FaultError, Harness, RecordId, Run, Session, verify_recovery};
use waymaker_flash::bank::{
    self, BankHeader, BankId as FlashBankId, BankLayout, BankRegion, Generation,
};
use waymaker_flash::frame::{self, ProgramAlign, Scan};
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_spec::explore::{BankShape, explore};
use waymaker_spec::model::{BANKS, Bank, BankId, Bound, Guards, Journal, Role, Transition};
use waymaker_spec::reader::{Mutant, Reader, Specified};
use waymaker_spec::refine::{
    Observation, abstraction, bank_after_erase, bank_after_seal, call_touched,
};

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
        // The writers this file drives schedule and complete effects and nothing else. A
        // timer is a schedule and a firing is its outcome, so the model has dimensions for
        // both; what it has no numbering for is a journal that mixes them, which no writer
        // here produces. Named rather than left to a wildcard, so a record kind added later
        // is a decision here.
        RecordRef::RunStarted { .. }
        | RecordRef::EffectFailed { .. }
        | RecordRef::TimerScheduled { .. }
        | RecordRef::TimerFired { .. }
        | RecordRef::VersionMarker { .. }
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
///
/// Scoped to states where every record is in `BankId::A` — matching [`abstraction`]'s own
/// convention and this file's own claim that "no writer here touches a bank" — rather than
/// every explored state. `explore` does not know that claim: nothing here stops the model's
/// *first* seal landing on bank B while records already sit in A, so `REFINEMENT`'s reachable
/// set includes states whose records are split across both banks, which no writer this file
/// drives (all single-bank) could ever produce. Including one here would make assertion 1
/// below accept a real crash against an observation no run of these writers could match,
/// since `abstraction` always tags every record `BankId::A`. Codex found the reachability gap
/// this filter closes on review of issue #67's pull request, which is what gave the model a
/// bank dimension to split across in the first place; a later round found that
/// `Observation`'s per-record tuple carried no bank identity at all, which this filter's own
/// correctness happened to hide since it always compared same-bank observations — see
/// `Observation::records`'s doc comment.
fn reachable_observations() -> BTreeSet<Observation> {
    let explored = match explore(REFINEMENT, Guards::ENFORCED, CEILING) {
        Ok(explored) => explored,
        Err(error) => unreachable!("{error}"),
    };
    explored
        .states()
        .iter()
        .filter(|state| {
            state
                .records()
                .iter()
                .all(|record| record.bank == BankId::A)
        })
        .map(Journal::observation)
        .collect()
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
                if observed
                    .records
                    .iter()
                    .any(|(_, _, _, torn_here, _)| *torn_here)
                {
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
    // reachable state has anything behind a gap for it to find. `Mutant::BootsTheRetiredBank`
    // is excluded for the parallel reason this file's own module doc gives: no writer here
    // drives the two-bank adapter, so every reconstructed state has never sealed and the
    // mutant's "boot the other bank" branch never triggers — it falls back to `Specified` and
    // agrees with it everywhere, which is a gap in what this file exercises rather than in the
    // mutant.
    let runs = drive(journal);
    for mutant in Mutant::ALL {
        if matches!(mutant, Mutant::SkipsGaps | Mutant::BootsTheRetiredBank) {
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
    // invention. A caller with real banks to abstract — issue #22's `waymaker_flash::bank`
    // is one, and abstracting it is still owed — gets three guarantees judged and the fourth
    // answered for free, and this is the assertion that says so out loud.
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
        records: vec![(
            RecordId(0),
            Role::Schedule,
            Durability::Acknowledged,
            true,
            BankId::A,
        )],
        dispatched: Vec::new(),
        ..Observation::default()
    };
    let error = Journal::reconstructed(&impossible).expect_err("torn and acknowledged");
    assert!(
        error.to_string().contains("torn and acknowledged"),
        "{error}"
    );

    let also_impossible = Observation {
        records: vec![(
            RecordId(0),
            Role::Schedule,
            Durability::Attempted,
            true,
            BankId::A,
        )],
        dispatched: Vec::new(),
        ..Observation::default()
    };
    let error = Journal::reconstructed(&also_impossible).expect_err("torn and absent");
    assert!(error.to_string().contains("never reached media"), "{error}");
}

#[test]
fn reconstruction_never_reissues_an_id_an_erase_already_spent() {
    // Codex, PR #135's merge round: `next_id` used to be inferred from `records.max_id + 1`,
    // which cannot see an id `BeginErase` dropped along with its record. A caller that
    // erased anything and then reported an observation with no surviving record at all would
    // reconstruct a state that believed no id had ever been issued, and the very next
    // `Declare` would reissue one a real device's counter had already moved past —
    // `Observation::next_id` exists so the caller reports the real counter instead of
    // leaving it to be inferred.
    let no_records_but_two_ids_spent = Observation {
        records: Vec::new(),
        dispatched: Vec::new(),
        next_id: Some(2),
        ..Observation::default()
    };
    let state = Journal::reconstructed(&no_records_but_two_ids_spent)
        .expect("an empty, unpowered observation is never impossible");
    let state = state
        .step(Transition::Reboot, Guards::ENFORCED, Bound::PROOF)
        .expect("reboot is legal from an unpowered state");
    let declared = state
        .step(
            Transition::Declare(Role::Schedule),
            Guards::ENFORCED,
            Bound::PROOF,
        )
        .expect("declaring from a fresh bank is legal");
    assert_eq!(
        declared.records().last().map(|record| record.id),
        Some(RecordId(2)),
        "reconstruction reissued an id the real device's counter had already spent"
    );
}

#[test]
fn reconstruction_refuses_a_next_id_that_reissues_a_resident() {
    // Codex, PR #135's next round: the erase-only version of this check let a `next_id`
    // collide with a record named in the *same* observation, rather than only with one an
    // earlier erase had dropped. Records 0 and 1 with `next_id: Some(1)` used to reconstruct
    // successfully; a `Reboot` then a `Declare(Schedule)` minted a second `RecordId(1)`, and
    // the `Program` after it found the older record already whole and refused with
    // `RecordAlreadyWritten` — stranding the new declaration on the identity collision issue
    // #67's whole counter scheme exists to forbid.
    let colliding = Observation {
        records: vec![
            (
                RecordId(0),
                Role::Schedule,
                Durability::Acknowledged,
                false,
                BankId::A,
            ),
            (
                RecordId(1),
                Role::Outcome,
                Durability::Acknowledged,
                false,
                BankId::A,
            ),
        ],
        dispatched: Vec::new(),
        next_id: Some(1),
        ..Observation::default()
    };
    let error =
        Journal::reconstructed(&colliding).expect_err("next_id collides with resident record 1");
    assert!(error.to_string().contains("resident record 1"), "{error}");

    // The floor is exact, not merely "somewhere higher": one past the highest resident id is
    // accepted, and the next legal declaration gets that id rather than reissuing 0 or 1.
    let just_past = Observation {
        next_id: Some(2),
        ..colliding
    };
    let state = Journal::reconstructed(&just_past).expect("next_id past every resident id");
    let declared = state
        .step(Transition::Reboot, Guards::ENFORCED, Bound::PROOF)
        .expect("reboot is legal from an unpowered state")
        .step(
            Transition::Declare(Role::Schedule),
            Guards::ENFORCED,
            Bound::PROOF,
        )
        .expect("record 1 is the Outcome that resolves record 0's Schedule, so a fresh Schedule is legal here");
    assert_eq!(
        declared.records().last().map(|record| record.id),
        Some(RecordId(2)),
        "next_id: Some(2) should hand out RecordId(2), not reissue 0 or 1"
    );
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
        for (id, _, state, torn, _) in &observed.records {
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

#[test]
fn observation_and_reconstruction_agree_on_a_state_with_records_in_two_banks() {
    // Codex, PR #135's round on commit 76dab02: `Observation`'s per-record tuple carried no
    // bank identity at all, so `Journal::reconstructed` hardcoded every record to `BankId::A`
    // regardless of which bank `observation()` actually read it from. A device that retires a
    // record in bank A behind its very first seal (landing on B), then declares a fresh
    // record in B, recovers `[1]` directly — but round-tripped through `observation()` and
    // `reconstructed()`, both records land in `BankId::A`, `recovering_bank()` stays `B`, and
    // neither record's bank matches it, so the reconstructed state recovers `[]` instead.
    let bound = Bound {
        records: 4,
        generations: 2,
    };
    let state = Journal::new()
        .step(Transition::Declare(Role::Schedule), Guards::ENFORCED, bound)
        .expect("declare record 0 in bank A")
        .step(Transition::Program(RecordId(0)), Guards::ENFORCED, bound)
        .expect("program record 0")
        .step(Transition::Barrier, Guards::ENFORCED, bound)
        .expect("barrier over record 0")
        .step(Transition::BeginSeal(BankId::B), Guards::ENFORCED, bound)
        .expect("the device's very first seal, on bank B")
        .step(Transition::CommitSeal(BankId::B), Guards::ENFORCED, bound)
        .expect("commit the seal; B is now sole authority")
        .step(Transition::Declare(Role::Schedule), Guards::ENFORCED, bound)
        .expect("declare record 1 in bank B, now current")
        .step(Transition::Program(RecordId(1)), Guards::ENFORCED, bound)
        .expect("program record 1")
        .step(Transition::Barrier, Guards::ENFORCED, bound)
        .expect("barrier over record 1");

    let direct = Specified.recover(&state);
    assert_eq!(
        direct,
        vec![RecordId(1)],
        "record 0 sits in a bank that lost authority, so only record 1 should recover"
    );

    let observed = state.observation();
    assert_eq!(
        observed
            .records
            .iter()
            .map(|(id, .., bank)| (*id, *bank))
            .collect::<Vec<_>>(),
        vec![(RecordId(0), BankId::A), (RecordId(1), BankId::B)],
        "observation() must report each record's real bank"
    );

    let reconstructed =
        Journal::reconstructed(&observed).expect("a real state is never impossible");
    assert_eq!(
        Specified.recover(&reconstructed),
        direct,
        "round-tripping through observation()/reconstructed() changed what recovery returns"
    );
}

#[test]
fn reconstruction_refuses_the_same_id_declared_in_two_banks() {
    // Codex, PR #135's next round: `Journal::bank_of` — which `single_authority` and
    // `durable_intent` both use to ask "which bank is this id's record really in" — answers
    // with the *first* matching record it finds, ignoring bank. A hand-built `Observation`
    // naming `RecordId(0)` once in a retired bank and again in the sole authoritative bank
    // made `single_authority` misreport a legitimately recovered record (from the
    // authoritative bank) as belonging to the retired one, breaching a guarantee that in fact
    // held. This id scheme has a single, device-wide counter — no legal transition sequence
    // ever declares the same id twice, in one bank or two — so the fix is to refuse the
    // observation rather than to make every by-id lookup bank-aware.
    let observed = Observation {
        records: vec![
            (
                RecordId(0),
                Role::Schedule,
                Durability::Acknowledged,
                false,
                BankId::A,
            ),
            (
                RecordId(0),
                Role::Schedule,
                Durability::Acknowledged,
                false,
                BankId::B,
            ),
        ],
        dispatched: vec![RecordId(0)],
        banks: [Bank::Erased, Bank::Sealed(1)],
        sealed_once: true,
        next_id: Some(1),
    };
    let error =
        Journal::reconstructed(&observed).expect_err("the same id names two different records");
    assert!(error.to_string().contains("named twice"), "{error}");
}

#[test]
fn no_reachable_observation_is_a_shape_no_single_bank_writer_could_leave() {
    // Codex, PR #135 round 6: nothing stops `REFINEMENT`'s exploration reaching a state whose
    // first-ever seal lands on bank B while records already sit in A — `Observation` carries
    // no bank identity, so flattening one of those in declaration order can produce a `Whole`
    // record following a gap, a shape `Guard::AppendOnly` forbids within a single bank and no
    // writer this file drives (all single-bank) could ever leave. `reachable_observations`
    // filters those states out before projecting; this is the check that it actually works,
    // over every observation the filtered set contains rather than over one example.
    for observation in reachable_observations() {
        let mut saw_gap = false;
        for (id, _, state, torn, _) in &observation.records {
            let whole = matches!(
                state,
                Durability::PossiblyDurable | Durability::Acknowledged
            ) && !torn;
            if whole {
                assert!(
                    !saw_gap,
                    "record {} is whole after a gap in {observation:?}, which no single-bank \
                     writer could have left",
                    id.0
                );
            } else {
                saw_gap = true;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Bank refinement: issue #73
// ---------------------------------------------------------------------------------------

/// The run the bank-swap writer below records.
const BANK_RUN: RunId = RunId(0x0000_0000_0000_00B7);

/// The generation the device's stale bank carries when the sweep starts.
const STALE: Generation = Generation(0);

/// The generation the device is booting from when the swap starts.
const CURRENT: Generation = Generation(1);

/// The generation the swap installs.
const NEW: Generation = match CURRENT.successor() {
    Some(next) => next,
    None => unreachable!(),
};

/// The bound the bank sweep is checked against.
///
/// No records: this writer declares none. Three generations, because the swap mints one
/// (`NEW`) past `CURRENT`, and the model's own numbering — see [`bank_after_seal`]'s docs —
/// starts one higher than the firmware's, so the highest model generation this run reaches is
/// three.
const BANK_REFINEMENT: Bound = Bound {
    records: 0,
    generations: 3,
};

/// Eight erase blocks: two banks of four, so an erase interrupted at a block boundary can
/// leave a bank half-erased. Styled on `crates/waymaker-fault/tests/banks.rs`'s own geometry,
/// since that file's `swap` is what this one abstracts.
fn bank_geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 32, 4, 1) else {
        unreachable!("256 is eight whole 32-byte blocks of 4-byte units of single bytes")
    };
    geometry
}

fn bank_layout() -> BankLayout {
    let Ok(layout) = BankLayout::new(bank_geometry()) else {
        unreachable!("eight erase blocks is four per bank")
    };
    layout
}

fn bank_align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    align
}

/// The model's generation number for a real one. See [`bank_after_seal`]'s docs.
///
/// Both sides number generations with a `u32`, so the `+1` shift cannot represent
/// `Generation::MAX` — there is no model number left for it. Refuses rather than clamping and
/// silently colliding it with `Generation::MAX - 1`'s: this test's own generations never
/// approach the boundary, so reaching it here would be this shift breaking, not the firmware.
fn model_generation(generation: Generation) -> u32 {
    let Some(shifted) = generation.0.checked_add(1) else {
        unreachable!("the +1 shift cannot represent Generation::MAX")
    };
    shifted
}

fn header_of(generation: Generation) -> BankHeader<'static> {
    BankHeader {
        run: BANK_RUN,
        align: bank_align(),
        workflow_kind: 7,
        workflow_version: 1,
        input_schema: 1,
        input: match generation.0 {
            0 => b"stale",
            1 => b"current",
            _ => b"next",
        },
    }
}

/// Programs `id`'s bank header, padded to the program unit.
fn program_header(
    session: &mut Session,
    id: FlashBankId,
    generation: Generation,
) -> Result<(), FaultError> {
    let region = bank_layout().bank(id);
    let mut page = [0_u8; 64];
    let Ok(written) = bank::encode_header(&header_of(generation), &mut page) else {
        unreachable!("a bank header of this shape fits 64 bytes")
    };
    let Some(bytes) = page.get(..written) else {
        unreachable!("`encode_header` reports what it wrote")
    };
    session.program(region.base(), bytes)
}

/// Programs `id`'s generation seal, naming the header already on media.
fn program_seal(
    session: &mut Session,
    id: FlashBankId,
    generation: Generation,
) -> Result<(), FaultError> {
    let region = bank_layout().bank(id);
    let mut page = [0_u8; 64];
    let Some(read_back) = page.get_mut(..region.payload_bytes().min(64) as usize) else {
        unreachable!("64 bytes is within a bank's payload")
    };
    session.read(region.base(), read_back)?;
    let Ok(seal) = bank::seal_for(read_back, generation) else {
        // The header on media does not decode, so there is no seal to write.
        return Ok(());
    };
    let mut sealed = [0_u8; 16];
    let Ok(written) = bank::encode_seal(&seal, bank_align(), &mut sealed) else {
        unreachable!("a seal fits 16 bytes at a 4-byte program unit")
    };
    let Some(bytes) = sealed.get(..written) else {
        unreachable!("`encode_seal` reports what it wrote")
    };
    session.program(region.seal_offset(), bytes)
}

/// Installs a whole bank: the header, its barrier, the seal, its barrier. §10 steps 3 to 6.
fn install(
    session: &mut Session,
    id: FlashBankId,
    generation: Generation,
) -> Result<(), FaultError> {
    program_header(session, id, generation)?;
    session.barrier()?;
    program_seal(session, id, generation)?;
    session.barrier()
}

/// The device as a previous life left it: a stale bank, then the one in use.
fn previous_life(session: &mut Session) -> Result<(), FaultError> {
    install(session, FlashBankId::B, STALE)?;
    install(session, FlashBankId::A, CURRENT)
}

/// The honest swap: never erase the bank you are booting from. Mirrors
/// `crates/waymaker-fault/tests/banks.rs`'s `swap`.
fn swap_writer(session: &mut Session) -> Result<(), FaultError> {
    previous_life(session)?;

    let spare = bank_layout().bank(FlashBankId::B);
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;

    program_header(session, FlashBankId::B, NEW)?;
    session.barrier()?;

    program_seal(session, FlashBankId::B, NEW)?;
    session.barrier()
}

/// The op index of each mutation [`reconstruct_banks`] reads, in the order `swap_writer`
/// issues them: the stale bank's seal, the current bank's seal, the spare bank's erase, and
/// the new seal. Named rather than searched for, so a shape change to `install` or
/// `swap_writer` is caught by [`check_bank_shape`] rather than by a silent misclassification.
const OP_SEAL_B_STALE: usize = 2;
const OP_SEAL_A_CURRENT: usize = 6;
const OP_ERASE_B: usize = 8;
const OP_SEAL_B_NEW: usize = 12;

/// Asserts the op indices above still name what they say — the *offset* as well as the
/// *kind*, so a reordering of `program_header` and `program_seal` inside `install` (which
/// would leave every op's kind unchanged) fails here rather than downstream.
fn check_bank_shape(clean: &Run) {
    use waymaker_fault::Op;
    let ops = clean.ops();
    let bank_a = bank_layout().bank(FlashBankId::A);
    let bank_b = bank_layout().bank(FlashBankId::B);
    let assert_seal_op = |op: usize, region: BankRegion, label: &str| {
        assert_eq!(
            ops.get(op),
            Some(&Op::Program {
                offset: region.seal_offset(),
                len: region.seal_bytes()
            }),
            "op {op} is no longer {label}'s seal write: {ops:?}"
        );
    };
    assert_seal_op(OP_SEAL_B_STALE, bank_b, "the stale bank");
    assert_seal_op(OP_SEAL_A_CURRENT, bank_a, "the current bank");
    assert_eq!(
        ops.get(OP_ERASE_B),
        Some(&Op::Erase {
            offset: bank_b.base(),
            len: bank_b.bytes()
        }),
        "op {OP_ERASE_B} is no longer the spare bank's erase: {ops:?}"
    );
    assert_seal_op(OP_SEAL_B_NEW, bank_b, "the new bank");
    assert_eq!(ops.len(), 14, "the writer's shape changed: {ops:?}");
}

/// One bank's header and seal regions of `image`.
fn regions(image: &[u8], id: FlashBankId) -> (&[u8], &[u8]) {
    let region = bank_layout().bank(id);
    let header = image
        .get(region.base() as usize..(region.base() + region.payload_bytes()) as usize)
        .unwrap_or_default();
    let seal = image
        .get(region.seal_offset() as usize..(region.seal_offset() + region.seal_bytes()) as usize)
        .unwrap_or_default();
    (header, seal)
}

/// Whether `id`'s region of `image` decodes as sealed at exactly `generation`.
fn decodes_sealed_at(image: &[u8], id: FlashBankId, generation: Generation) -> bool {
    let (header, seal) = regions(image, id);
    bank::sealed_generation(header, seal) == Some(generation)
}

/// Refines `known` against what `id`'s region shows right now, for a fold that may end up
/// reading it back as `prior`.
///
/// `known` is [`Bank::Erased`] at two points: before anything has happened to `id` at all,
/// and right after [`bank_after_erase`] reports its erase committed. Both are followed by a
/// header write with no checkpoint of its own — `install`'s and `swap_writer`'s header writes
/// are not modelled, which is `obligation.rs`'s `single-authority` row's "banks hold no
/// records" gap — so a crash that tears the header before the matching seal write is even
/// reached leaves the *next* fold reading `known` as `prior`, unrefined. Claiming
/// [`Bank::Erased`] regardless would be wrong the moment that header write left anything
/// behind: [`Bank::Erased`] promises "nothing half-gone", and a torn header is exactly
/// half-gone. This reads the region back and downgrades to [`Bank::Erasing`] when it no
/// longer reads erased — not an erase, but the closest the model has: the same "not
/// bootable, not safely writable" it already uses [`Bank::Erasing`] for.
///
/// Any other `known` is returned unchanged: [`Bank::Erasing`], [`Bank::Sealing`] and
/// [`Bank::Sealed`] each already mean something a header write cannot take back by itself,
/// and `swap_writer` never writes a header over a bank it has not just erased.
fn ground_prior(image: &[u8], id: FlashBankId, known: Bank) -> Bank {
    match known {
        Bank::Erased if !is_erased(image, id) => Bank::Erasing,
        other => other,
    }
}

/// Whether `id`'s whole bank — header and seal both — is fully erased in `image`.
///
/// Both regions, not the header alone: an erase interrupted after clearing the header but
/// before reaching the seal's own block leaves an old seal standing over an erased header,
/// which is a bank still *in flight*, not one this run has finished erasing. Checking the
/// header alone would call that `erased` too, since a cleared header cannot decode either
/// way — and [`bank_after_erase`] would then report [`Bank::Erased`] for a bank a later crash
/// could still boot from its stale seal.
fn is_erased(image: &[u8], id: FlashBankId) -> bool {
    let region = bank_layout().bank(id);
    let whole = image
        .get(region.base() as usize..(region.base() + region.bytes()) as usize)
        .unwrap_or_default();
    whole.iter().all(|byte| *byte == 0xFF)
}

/// Asserts that `bank_after_erase`'s answer agrees with `erased` — the same ground truth it
/// was handed — whenever the erase actually touched media.
///
/// Why this earns its keep: [`Bank::Erased`] and [`Bank::Erasing`] are both non-authoritative,
/// so neither the reachability check nor the real-selection cross-check in the test below can
/// tell one from the other — a build with the two branches of `bank_after_erase` swapped
/// passes both unchanged. This is the check that actually pins the branch.
fn assert_erase_matches_ground_truth(run: &Run, op: usize, bank: Bank, erased: bool) {
    if !call_touched(run, op, bank_geometry()) {
        return;
    }
    assert_eq!(
        matches!(bank, Bank::Erased),
        erased,
        "at {:?}: bank_after_erase reported {bank:?}, but the region reads erased: {erased}",
        run.injection()
    );
}

/// Asserts that `bank_after_seal`'s answer agrees with `sealed` — the same ground truth it
/// was handed — whenever the seal write actually touched media. See
/// [`assert_erase_matches_ground_truth`] for why this, and not the checks below, is what
/// pins [`Bank::Sealed`] against [`Bank::Sealing`].
fn assert_seal_matches_ground_truth(run: &Run, op: usize, bank: Bank, sealed: bool) {
    if !call_touched(run, op, bank_geometry()) {
        return;
    }
    assert_eq!(
        matches!(bank, Bank::Sealed(_)),
        sealed,
        "at {:?}: bank_after_seal reported {bank:?}, but the region reads sealed: {sealed}",
        run.injection()
    );
}

/// Folds one crashed run's final image into `[Bank; BANKS]` and whether either bank has ever
/// sealed.
///
/// One fold per phase, in the order `swap_writer` issues them: the stale install, the current
/// install, then the swap's erase and reseal of the spare bank. `erased`/`sealed` are read off
/// the *final* image — safe here because whenever a later phase touches a bank again, that
/// phase's own fold overrides this one regardless of what it decided; a phase's own read of
/// the final image is accurate exactly when nothing later touches that bank, which is the one
/// case where it matters.
fn reconstruct_banks(run: &Run) -> ([Bank; BANKS], bool) {
    let mut sealed_once = false;

    let stale_sealed = decodes_sealed_at(run.image(), FlashBankId::B, STALE);
    let mut b = bank_after_seal(
        ground_prior(run.image(), FlashBankId::B, Bank::Erased),
        run,
        OP_SEAL_B_STALE,
        bank_geometry(),
        model_generation(STALE),
        stale_sealed,
    );
    assert_seal_matches_ground_truth(run, OP_SEAL_B_STALE, b, stale_sealed);
    sealed_once |= matches!(b, Bank::Sealed(_));

    let current_sealed = decodes_sealed_at(run.image(), FlashBankId::A, CURRENT);
    let a_seal = bank_after_seal(
        ground_prior(run.image(), FlashBankId::A, Bank::Erased),
        run,
        OP_SEAL_A_CURRENT,
        bank_geometry(),
        model_generation(CURRENT),
        current_sealed,
    );
    assert_seal_matches_ground_truth(run, OP_SEAL_A_CURRENT, a_seal, current_sealed);
    sealed_once |= matches!(a_seal, Bank::Sealed(_));

    let erased = is_erased(run.image(), FlashBankId::B);
    b = bank_after_erase(b, run, OP_ERASE_B, bank_geometry(), erased);
    assert_erase_matches_ground_truth(run, OP_ERASE_B, b, erased);

    let new_sealed = decodes_sealed_at(run.image(), FlashBankId::B, NEW);
    b = bank_after_seal(
        ground_prior(run.image(), FlashBankId::B, b),
        run,
        OP_SEAL_B_NEW,
        bank_geometry(),
        model_generation(NEW),
        new_sealed,
    );
    assert_seal_matches_ground_truth(run, OP_SEAL_B_NEW, b, new_sealed);
    sealed_once |= matches!(b, Bank::Sealed(_));

    ([a_seal, b], sealed_once)
}

/// Which bank a reader boots, in a form comparable across the model and the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RealAuthority {
    /// Neither bank carries a valid seal.
    None,
    /// Exactly one bank does.
    One(FlashBankId, Generation),
    /// Both do, at the same generation.
    Ambiguous(Generation),
}

/// Which bank the real selection boots, read straight off `image`.
fn real_authority_of(image: &[u8]) -> RealAuthority {
    let generations = FlashBankId::ALL.map(|id| {
        let (header, seal) = regions(image, id);
        bank::sealed_generation(header, seal)
    });
    match bank::select(generations) {
        bank::Authority::Unsealed => RealAuthority::None,
        bank::Authority::Bank { id, generation } => RealAuthority::One(id, generation),
        bank::Authority::Ambiguous { generation } => RealAuthority::Ambiguous(generation),
    }
}

/// Which bank the reconstructed model state says a reader boots.
fn model_authority(state: &Journal) -> RealAuthority {
    let flash_id = |id: BankId| match id {
        BankId::A => FlashBankId::A,
        BankId::B => FlashBankId::B,
    };
    let real_generation = |id: BankId| {
        // Only ever called with an `id` from `state.authoritative()`, which answers `Some`
        // for exactly the ids it names — see `Journal::authoritative`.
        let Some(model) = state.bank(id).authoritative_generation() else {
            unreachable!("an authoritative bank names its own generation")
        };
        let Some(real) = model.checked_sub(1) else {
            unreachable!("model_generation never produces zero")
        };
        Generation(real)
    };
    match state.authoritative().as_slice() {
        [] => RealAuthority::None,
        [only] => RealAuthority::One(flash_id(*only), real_generation(*only)),
        [first, ..] => RealAuthority::Ambiguous(real_generation(*first)),
    }
}

#[test]
fn the_bank_swap_refines_the_specification_at_every_crash_point() {
    let runs = drive_on(bank_geometry(), swap_writer);
    let Some(clean) = runs.first() else {
        unreachable!("the fault-free run is always first")
    };
    check_bank_shape(clean);
    assert!(runs.len() > 100, "only {} runs", runs.len());
    assert_eq!(
        real_authority_of(clean.image()),
        RealAuthority::One(FlashBankId::B, NEW)
    );

    let reachable = {
        let explored = match explore(BANK_REFINEMENT, Guards::ENFORCED, CEILING) {
            Ok(explored) => explored,
            Err(error) => unreachable!("{error}"),
        };
        explored
            .states()
            .iter()
            .map(Journal::observation)
            .collect::<BTreeSet<_>>()
    };

    let mut shapes = BTreeSet::new();
    let mut installed = 0_usize;
    let mut still_current = 0_usize;
    let mut still_stale = 0_usize;
    let mut neither = 0_usize;
    let mut erasing_seen = 0_usize;
    let mut sealing_seen = 0_usize;

    for run in &runs {
        let (banks, sealed_once) = reconstruct_banks(run);
        let observed = Observation {
            banks,
            sealed_once,
            ..Observation::default()
        };
        assert!(
            reachable.contains(&observed),
            "at {:?}: {banks:?} (sealed_once: {sealed_once}) is not a state the model reaches",
            run.injection()
        );

        let Ok(state) = Journal::reconstructed(&observed) else {
            unreachable!("a bank-only observation is never torn")
        };
        assert!(
            waymaker_spec::invariant::holds(
                waymaker_spec::invariant::Invariant::SingleAuthority,
                &state,
                &[],
            )
            .is_ok(),
            "at {:?}: {banks:?} breaks single authority",
            run.injection()
        );

        // Two independent judgements of the same crash: `waymaker_flash::bank::select` over
        // the real bytes, and `Journal::authoritative` over the state this abstraction built.
        // A wrong fold could still land on a reachable state and pass the check above; it
        // could not also agree with the real selection by accident on every crash point.
        let real_authority = real_authority_of(run.image());
        assert_eq!(
            model_authority(&state),
            real_authority,
            "at {:?}: the reconstructed state and the real selection disagree",
            run.injection()
        );

        match real_authority {
            RealAuthority::One(FlashBankId::B, generation) if generation == NEW => installed += 1,
            RealAuthority::One(FlashBankId::A, generation) if generation == CURRENT => {
                still_current += 1;
            }
            RealAuthority::One(FlashBankId::B, generation) if generation == STALE => {
                still_stale += 1;
            }
            RealAuthority::None => neither += 1,
            other => unreachable!(
                "at {:?}: a device in state {other:?} was never written",
                run.injection()
            ),
        }
        erasing_seen += banks.iter().filter(|bank| **bank == Bank::Erasing).count();
        sealing_seen += banks
            .iter()
            .filter(|bank| matches!(bank, Bank::Sealing(_)))
            .count();
        shapes.insert(banks.map(BankShape::of));
    }

    assert!(
        installed > 0 && still_current > 0 && still_stale > 0 && neither > 0,
        "{installed} installed, {still_current} kept current, {still_stale} kept stale, \
         {neither} had no authority at all"
    );
    assert!(
        shapes.len() >= 4,
        "only {} distinct bank-shape combinations were reached, which is too few for \
         question 1 to be a check rather than a formality",
        shapes.len()
    );
    // `Bank::Erasing` and `Bank::Sealing` are the two shapes the authority cross-check above
    // cannot see either side of — a bank in either is as non-authoritative as one that is
    // `Erased` or was never touched. Without this, a fold that never actually produced one of
    // them (a mistake with the same shape as `bank_after_erase`/`bank_after_seal` always
    // taking the "committed" branch) would still pass every assertion above.
    assert!(erasing_seen > 0, "no crash point ever left a bank Erasing");
    assert!(sealing_seen > 0, "no crash point ever left a bank Sealing");
}
