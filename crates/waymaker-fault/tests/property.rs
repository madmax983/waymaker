//! Design document §15's core property oracle, over random histories and every crash point.
//!
//! Issue [#19](https://github.com/madmax983/waymaker/issues/19) is rung 0.1's exit
//! criterion, and it states the oracle in four lines:
//!
//! ```text
//! recovered_history.is_prefix_of(committed_history)
//!     && acknowledged_records.all(|r| recovered_history.contains(r))
//!     && dispatched_effects.all(|e| recovered_history.has_schedule(e.id))
//!     && recovered_banks.count_authoritative() == 1
//! ```
//!
//! The first two are swept here, over histories drawn from a seed rather than written by
//! hand.
//!
//! The third is *reached* by the sweep and cannot fail in it, and that is worth stating
//! plainly rather than letting the loop imply otherwise. Every writer here dispatches after
//! its barrier returns, as §02 decision 3 requires — and a record whose barrier returned is
//! `Acknowledged`, so the second line already demands it and would be the line that fired.
//! The third line is only load-bearing for an intent recovery is *permitted* to drop, which
//! is what dispatching before the barrier produces:
//! `tests/teeth.rs`'s `an_effect_dispatched_before_its_intent_is_durable_is_caught` drives a
//! writer that commits exactly that inversion, and is where the line has teeth. The fourth needs two banks and a generation seal, and it is
//! [`tests/banks.rs`](https://github.com/madmax983/waymaker/tree/main/crates/waymaker-fault/tests/banks.rs).
//! [`tests/teeth.rs`](https://github.com/madmax983/waymaker/tree/main/crates/waymaker-fault/tests/teeth.rs)
//! is the other half of "done when": a deliberately broken cursor or codec has to make this
//! fail, or the sweep is decoration.
//!
//! # What is being tested, and what is only being used
//!
//! Everything on the write side and everything on the read side is the real thing.
//! Records are §09 frames from `waymaker-flash`'s [`frame::encode`]; recovery is
//! [`Scan`] — the append scan — feeding [`ReplayCursor`], which is `waymaker-core`'s
//! ordering authority. Nothing here re-implements either. What this file owns is the
//! *generator*, the writer that appends what it drew, and the census that says the sweep
//! covered what issue #19 asked it to.
//!
//! # Why a record is identified by its content
//!
//! [`History::identify`] maps a recovered record back to the planned one by what is *in*
//! it — the effect sequence it names, or its position in the run's grammar — never by where
//! the scan found it. Identifying by position would make "recovery produced a prefix" a
//! statement about counting rather than about history, and it would pass against a reader
//! that returned the right number of the wrong records.
//!
//! # Where the oracle deliberately is not applied
//!
//! To a run that erases committed data. A stale tail is a real hazard and §09's scan
//! refuses one — `tests/committed_prefix.rs` holds that — but an erase of a block holding
//! an acknowledged record destroys it on purpose, and an oracle asked whether recovery lost
//! an acknowledged record can only answer "yes". The property that survives there is about
//! the *scan*, not about the journal's guarantees, so it is asserted as such. Rung 0.2's
//! two banks are what make an erase safe, and `tests/banks.rs` is where the oracle meets
//! one.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use waymaker_core::{
    ActivityKind, DecodeError, EffectSeq, KernelError, RecordKind, RecordRef, ReplayCursor, RunId,
};
use waymaker_fault::{
    Breach, Durability, FaultError, Harness, Injection, Interruption, Op, Progress, RecordId,
    Recovery, Rng, Run, Session, injections, random_geometry, verify_oracle,
};
use waymaker_flash::frame::{self, ProgramAlign, Scan};
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};
use waymaker_flash::storage::{Geometry, StableStorage};

/// How many seeds the sweep draws.
const SEEDS: u64 = 32;

/// The smallest device the sweep will draw.
///
/// A device with no room for a single frame is a legal thing to model — the writer stops
/// with nothing written, and the oracle holds trivially — but it contributes two runs and
/// no history, and `a_run_that_overflows_its_journal_recovers_what_fitted_and_nothing_more`
/// covers the boundary properly. A floor keeps every seed carrying its weight.
const MIN_CAPACITY: u32 = 64;

/// The largest device the sweep will draw.
///
/// Every byte of journal is two crash points and every crash point is one more run of the
/// writer, so this is a runtime dial and not a semantic one. Two hundred and fifty-six
/// bytes is a journal of several records at every program granularity the generator draws,
/// which is what the sweep needs; a megabyte would be the same test, slower.
const MAX_CAPACITY: u32 = 256;

/// A device drawn from `rng` that is worth sweeping: at least [`MIN_CAPACITY`] bytes.
///
/// Redrawing rather than clamping, so that every shape in the result is one
/// [`random_geometry`] really produces.
fn device(rng: &mut Rng) -> Geometry {
    loop {
        let geometry = random_geometry(rng, MAX_CAPACITY);
        if geometry.capacity() >= MIN_CAPACITY {
            return geometry;
        }
    }
}

/// The run every generated history belongs to.
const RUN: RunId = RunId(0x5741_594D_524B_0001);

/// The record id no plan ever has, handed back for a record recovery invented.
///
/// A ledger never declares it, so the prefix check reports it at the position it appeared
/// rather than silently accepting it — which is what a sentinel in a test has to do.
const UNPLANNED: RecordId = RecordId(u32::MAX);

// ---------------------------------------------------------------------------------------
// The generator
// ---------------------------------------------------------------------------------------

/// One record a drawn history means to append.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Planned {
    Started,
    Scheduled { seq: EffectSeq, kind: ActivityKind },
    Completed { seq: EffectSeq },
    Failed { seq: EffectSeq },
    RunCompleted,
    RunFailed,
}

/// A history to write, and the payloads its records carry.
///
/// Legal by construction in `waymaker-core`'s grammar: a run starts once, an outcome
/// follows its own schedule, nothing follows a terminal record. That is deliberate. A
/// generator that drew illegal histories would be testing the cursor's refusals, which
/// `waymaker-core`'s own suite already does; what the oracle needs is a history that *could*
/// have happened, so that everything a crash point does to it is the crash point's doing.
#[derive(Clone, Debug)]
struct History {
    plans: Vec<Planned>,
    /// Whether the writer issues a barrier after record `i`. Index-aligned with `plans`.
    barriers: Vec<bool>,
    /// The payload record `i` carries. Index-aligned with `plans`.
    payloads: Vec<Vec<u8>>,
}

impl History {
    /// A history drawn from `rng`.
    fn draw(rng: &mut Rng) -> Self {
        let mut history = Self {
            plans: Vec::new(),
            barriers: Vec::new(),
            payloads: Vec::new(),
        };
        history.push(rng, Planned::Started);

        let effects = rng.below(5);
        let mut seq = EffectSeq::FIRST;
        let mut unresolved = false;
        for effect in 0..effects {
            let kind = ActivityKind(1 + u16::try_from(rng.below(4)).unwrap_or(0));
            history.push(rng, Planned::Scheduled { seq, kind });
            // The last effect may be left unresolved, which is the state a run is in when
            // the power goes while an effect is in flight. Every earlier one is resolved,
            // because a schedule with an unresolved schedule before it is not a history the
            // cursor would ever have written.
            if effect + 1 == effects && rng.flip() {
                unresolved = true;
                break;
            }
            let outcome = if rng.flip() {
                Planned::Completed { seq }
            } else {
                Planned::Failed { seq }
            };
            history.push(rng, outcome);
            let Some(next) = seq.successor() else { break };
            seq = next;
        }

        // A run does not end while an effect is in flight: §08's transition table refuses a
        // terminal record at `AwaitingOutcome`, so a history with one would be a history no
        // workflow could have written — and the sweep would then be measuring the cursor's
        // refusals rather than what a crash point did.
        let terminal = rng.below(4);
        if !unresolved {
            match terminal {
                0 => history.push(rng, Planned::RunCompleted),
                1 => history.push(rng, Planned::RunFailed),
                _ => {}
            }
        }
        history
    }

    /// Appends `plan`, drawing its payload and whether a barrier follows it.
    fn push(&mut self, rng: &mut Rng, plan: Planned) {
        let index = self.plans.len();
        let len = usize::try_from(rng.below(9)).unwrap_or(0);
        self.plans.push(plan);
        // Three quarters, not all: a record with no barrier after it is only ever
        // *possibly* durable, and a sweep in which every record is acknowledged would never
        // exercise the half of the oracle that permits recovery to drop one.
        self.barriers.push(rng.below(4) != 0);
        self.payloads
            .push((0..len).map(|at| filler(index, at)).collect());
    }

    /// The record view for plan `index`, borrowing its payload.
    fn record(&self, index: usize) -> Option<RecordRef<'_>> {
        let payload = self.payloads.get(index)?.as_slice();
        Some(match *self.plans.get(index)? {
            Planned::Started => RecordRef::RunStarted {
                workflow_kind: 7,
                workflow_version: 1,
                input: payload,
            },
            Planned::Scheduled { seq, kind } => RecordRef::EffectScheduled {
                seq,
                kind,
                input_len: u16::try_from(payload.len()).unwrap_or(u16::MAX),
                input_crc: frame::input_digest(payload),
            },
            Planned::Completed { seq } => RecordRef::EffectCompleted {
                seq,
                result: payload,
            },
            Planned::Failed { seq } => RecordRef::EffectFailed {
                seq,
                error: payload,
            },
            Planned::RunCompleted => RecordRef::RunCompleted { result: payload },
            Planned::RunFailed => RecordRef::RunFailed { error: payload },
        })
    }

    /// Which planned record `record` is, by what is in it.
    ///
    /// Every plan is distinguishable from every other by content alone: one run start, one
    /// terminal record, and at most one schedule, completion or failure per sequence. So
    /// this is a lookup and not a guess, and a reader that returned records in the wrong
    /// order gets the ids it really produced rather than the ids of the positions it
    /// filled.
    fn identify(&self, record: &RecordRef<'_>) -> RecordId {
        let wanted = |plan: &Planned| match (plan, record) {
            (Planned::Started, RecordRef::RunStarted { .. })
            | (Planned::RunCompleted, RecordRef::RunCompleted { .. })
            | (Planned::RunFailed, RecordRef::RunFailed { .. }) => true,
            (Planned::Scheduled { seq: planned, .. }, RecordRef::EffectScheduled { seq, .. })
            | (Planned::Completed { seq: planned }, RecordRef::EffectCompleted { seq, .. })
            | (Planned::Failed { seq: planned }, RecordRef::EffectFailed { seq, .. }) => {
                planned == seq
            }
            _ => false,
        };
        self.plans
            .iter()
            .position(wanted)
            .and_then(|index| u32::try_from(index).ok())
            .map_or(UNPLANNED, RecordId)
    }

    /// The record ids of every schedule in this history, in order.
    fn schedules(&self) -> Vec<RecordId> {
        self.plans
            .iter()
            .enumerate()
            .filter(|(_, plan)| matches!(plan, Planned::Scheduled { .. }))
            .filter_map(|(index, _)| u32::try_from(index).ok().map(RecordId))
            .collect()
    }
}

/// A byte for position `at` of record `index`, so that no two records share a payload.
fn filler(index: usize, at: usize) -> u8 {
    u8::try_from((index.wrapping_mul(31).wrapping_add(at).wrapping_add(1)) & 0xFF).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------
// The writer, and recovery
// ---------------------------------------------------------------------------------------

/// The program granularity for `geometry`, as the frame encoder wants it.
fn align_of(geometry: Geometry) -> ProgramAlign {
    let Ok(bytes) = u16::try_from(geometry.program_size()) else {
        unreachable!("no geometry in this file has a program unit wider than a `u16`")
    };
    let Some(align) = ProgramAlign::new(bytes) else {
        unreachable!("a geometry's program size is a power of two, which is the whole rule")
    };
    align
}

/// Appends `history` to `session`, one record at a time, stopping when the journal is full.
///
/// Stopping rather than failing, because a full journal is a boundary the sweep is *for*:
/// a writer that returned an error there would fail the fault-free run, and the harness
/// would refuse the whole enumeration rather than test the boundary. Rung 0.2's bank swap
/// is what a real writer does here; at 0.1 there is nowhere to go.
///
/// `dispatched` collects the schedules whose barrier returned, which is the only point at
/// which §02 decision 3 permits an effect to happen.
fn append_all(
    session: &mut Session,
    history: &History,
    geometry: Geometry,
    dispatched: &mut Vec<RecordId>,
) -> Result<(), FaultError> {
    let align = align_of(geometry);
    let mut buffer = [0_u8; 128];
    let mut at = 0_u32;

    for index in 0..history.plans.len() {
        let Some(record) = history.record(index) else {
            break;
        };
        let Ok(written) = frame::encode(&record, align, &mut buffer) else {
            break;
        };
        let Some(bytes) = buffer.get(..written) else {
            break;
        };
        let len = u32::try_from(written).unwrap_or(u32::MAX);
        if at
            .checked_add(len)
            .is_none_or(|end| end > geometry.capacity())
        {
            break;
        }

        let id = u32::try_from(index).unwrap_or(u32::MAX);
        session.begin_record(RecordId(id));
        session.program(at, bytes)?;
        at = at.wrapping_add(len);
        if history.barriers.get(index) == Some(&true) {
            session.barrier()?;
            // The intent is durable, so the effect may now happen.
            if matches!(history.plans.get(index), Some(Planned::Scheduled { .. })) {
                dispatched.push(RecordId(id));
            }
        }
    }
    session.end_record();
    Ok(())
}

/// Every record recovery can produce from `image`, through the real reader.
///
/// [`Scan`] is §09's append scan and [`ReplayCursor`] is §06's ordering authority, and both
/// are here because recovery is both: a frame that checks out but could not follow the
/// records before it is not history, and a reader that stopped at only one of the two would
/// be a different reader from the one the firmware runs.
fn recover(image: &[u8], history: &History, align: ProgramAlign) -> Vec<RecordId> {
    let mut cursor = ReplayCursor::new(RUN);
    let mut recovered = Vec::new();
    for step in Scan::new(image, align) {
        let Ok(record) = step else { break };
        if cursor.advance(record).is_err() {
            break;
        }
        recovered.push(history.identify(&record));
    }
    recovered
}

/// Every run of `history` on `geometry`, and the dispatch log each run produced.
fn sweep(history: &History, geometry: Geometry) -> (Vec<Run>, Vec<Vec<RecordId>>) {
    let log: RefCell<Vec<Vec<RecordId>>> = RefCell::new(Vec::new());
    let runs = Harness::new(geometry).run(|session| {
        let mut dispatched = Vec::new();
        let result = append_all(session, history, geometry, &mut dispatched);
        log.borrow_mut().push(dispatched);
        result
    });
    match runs {
        Ok(runs) => (runs, log.into_inner()),
        Err(error) => unreachable!("{error}"),
    }
}

// ---------------------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------------------

/// What one seed's sweep observed, so that the suite can be held to covering something.
#[derive(Clone, Debug, Default)]
struct Census {
    runs: usize,
    geometries: BTreeSet<(u32, u32, u32, u32)>,
    histories: BTreeSet<Vec<(&'static str, usize)>>,
    prefix_lengths: BTreeSet<usize>,
    acknowledged_kept_while_others_were_lost: usize,
    dispatched: usize,
    dispatched_then_power_lost: usize,
    scans_that_refused: usize,
    torn_records: usize,
    /// Runs where a record is torn and the scan does *not* refuse, or the other way round.
    torn_and_refused_disagreed: usize,
    empty_recoveries: usize,
    full_recoveries: usize,
    /// Which of §09's six record kinds the generator actually drew.
    kinds: BTreeSet<&'static str>,
    /// Records the writer appended and never ordered with a barrier.
    unordered_records: usize,
    /// Seeds whose history was longer than the device it was drawn beside.
    histories_that_did_not_fit: usize,
}

impl Planned {
    /// The §09 record kind this plan writes, for the census.
    const fn kind(self) -> &'static str {
        match self {
            Self::Started => "RunStarted",
            Self::Scheduled { .. } => "EffectScheduled",
            Self::Completed { .. } => "EffectCompleted",
            Self::Failed { .. } => "EffectFailed",
            Self::RunCompleted => "RunCompleted",
            Self::RunFailed => "RunFailed",
        }
    }
}

#[test]
fn the_oracle_holds_over_random_histories_on_random_geometries_at_every_crash_point() {
    let mut census = Census::default();

    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let geometry = device(&mut rng);
        let history = History::draw(&mut rng);
        let align = align_of(geometry);
        let (runs, log) = sweep(&history, geometry);

        assert_eq!(
            log.len(),
            runs.len(),
            "seed {seed}: one dispatch log per run"
        );
        census.runs += runs.len();
        census.geometries.insert((
            geometry.capacity(),
            geometry.erase_size(),
            geometry.program_size(),
            geometry.read_size(),
        ));

        let clean = runs
            .first()
            .unwrap_or_else(|| unreachable!("the fault-free run"));
        assert!(
            !clean.ops().is_empty(),
            "seed {seed}: the writer issued no operations at all on {geometry:?}, so this \
             seed contributes two runs and no history"
        );
        let whole = recover(clean.image(), &history, align);
        census.histories.insert(
            history
                .plans
                .iter()
                .zip(history.payloads.iter())
                .map(|(plan, payload)| (plan.kind(), payload.len()))
                .collect(),
        );
        census
            .kinds
            .extend(history.plans.iter().map(|plan| plan.kind()));
        census.unordered_records += history.barriers.iter().filter(|kept| !**kept).count();
        if clean.ledger().len() < history.plans.len() {
            census.histories_that_did_not_fit += 1;
        }

        for (run, dispatched) in runs.iter().zip(log.iter()) {
            let recovered = recover(run.image(), &history, align);
            let verdict = verify_oracle(
                run.ledger(),
                &Recovery::new(&recovered).dispatched(dispatched),
            );
            assert_eq!(
                verdict,
                Ok(()),
                "seed {seed} at {:?}: recovered {recovered:?} from a ledger of {:?} on \
                 {geometry:?}",
                run.injection(),
                run.ledger().records().collect::<Vec<_>>()
            );

            census.prefix_lengths.insert(recovered.len());
            census.dispatched += dispatched.len();
            if recovered.is_empty() {
                census.empty_recoveries += 1;
            }
            if recovered == whole && !whole.is_empty() {
                census.full_recoveries += 1;
            }
            if run.ledger().acknowledged().count() > 0 && recovered.len() < whole.len() {
                census.acknowledged_kept_while_others_were_lost += 1;
            }
            if !dispatched.is_empty()
                && run.injection().is_some_and(|injection| {
                    injection.interruption == Interruption::PowerLoss
                        && injection.progress == Progress::Whole
                })
            {
                census.dispatched_then_power_lost += 1;
            }
            let refused = Scan::new(run.image(), align).any(|step| step.is_err());
            let torn = run
                .ledger()
                .order()
                .any(|id| run.ledger().torn(id) == Some(true));
            if refused {
                census.scans_that_refused += 1;
            }
            if torn {
                census.torn_records += 1;
            }
            if refused != torn {
                census.torn_and_refused_disagreed += 1;
            }
        }
    }

    assert_the_sweep_covered_something(&census);
}

/// The difference between a sweep and a loop that ran.
///
/// A suite that asserted only the oracle would pass just as loudly over one empty journal
/// on one geometry, so each of these names a way the sweep above could have been vacuous
/// and refuses it.
fn assert_the_sweep_covered_something(census: &Census) {
    assert!(
        census.runs > 5_000,
        "the whole sweep was only {} runs",
        census.runs
    );
    assert!(
        census.geometries.len() >= 8,
        "only {} distinct geometries: {:?}",
        census.geometries.len(),
        census.geometries
    );
    assert!(
        census.histories.len() >= 8,
        "only {} distinct histories drawn across {SEEDS} seeds",
        census.histories.len()
    );
    assert!(
        census.prefix_lengths.len() >= 4,
        "recovery only ever produced {:?} records",
        census.prefix_lengths
    );
    assert!(
        census.empty_recoveries > 0 && census.full_recoveries > 0,
        "the sweep never saw both ends: {} empty, {} whole",
        census.empty_recoveries,
        census.full_recoveries
    );
    assert!(
        census.acknowledged_kept_while_others_were_lost > 0,
        "no run had to keep a record it had promised while losing one it had not, so the \
         acknowledgment half of the oracle held vacuously"
    );
    assert!(
        census.dispatched > 0 && census.dispatched_then_power_lost > 0,
        "no run both dispatched an effect and lost power at the end of an operation, so the \
         sweep never reached the state §02 decision 3 is about — an effect in flight when \
         the power goes: {} dispatches, {} runs that also lost power on a completed \
         operation",
        census.dispatched,
        census.dispatched_then_power_lost
    );
    assert!(
        census.torn_records > 0,
        "no crash point tore a record in half across the whole sweep"
    );
    // The two counters above are the same measurement taken from opposite ends — the ledger
    // says a record is half on media, the scan says a frame does not verify — and over the
    // whole sweep they agree exactly. That equivalence is the interesting property, and
    // asserting it is what stops these being one check written twice: a codec that accepted
    // a torn frame, or a harness that stopped noticing one, breaks it from its own side.
    assert_eq!(
        census.torn_and_refused_disagreed, 0,
        "a torn record and a journal the scan refuses stopped being the same thing in {} \
         runs",
        census.torn_and_refused_disagreed
    );
    assert!(
        census.scans_that_refused > 0,
        "no crash point produced a journal the scan refused, so integrity failure was never \
         reached from a real torn write"
    );
    assert_eq!(
        census.kinds.len(),
        6,
        "the generator drew only {:?} of §09's six record kinds",
        census.kinds
    );
    assert!(
        census.unordered_records > 0,
        "every record the generator drew was followed by a barrier, so the half of the \
         oracle that *permits* recovery to drop a record was never exercised"
    );
    assert!(
        census.histories_that_did_not_fit > 0,
        "no drawn history was longer than the device beside it, so the sweep never met the \
         capacity boundary"
    );
}

#[test]
fn every_byte_and_every_program_unit_of_every_write_is_a_crash_point() {
    // Issue #19's "torn writes at every byte and program unit", as an assertion over the
    // enumeration rather than as a claim in a doc comment. `injections` is a pure function
    // of the recorded sequence, so this is checkable exactly rather than by sampling.
    let mut units = 0_usize;
    let mut interior = 0_usize;

    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let geometry = device(&mut rng);
        let history = History::draw(&mut rng);
        let (runs, _) = sweep(&history, geometry);

        let clean = runs
            .first()
            .unwrap_or_else(|| unreachable!("the fault-free run"));
        let enumerated = injections(clean.ops(), geometry);
        assert_eq!(
            runs.len(),
            enumerated.len() + 1,
            "seed {seed}: the harness ran a different number of crash points than it listed"
        );

        for (index, op) in clean.ops().iter().enumerate() {
            let Op::Program { len, .. } = *op else {
                continue;
            };
            for byte in 1..len {
                let tear = Injection {
                    op: index,
                    progress: Progress::Bytes(byte),
                    interruption: Interruption::PowerLoss,
                };
                assert!(
                    enumerated.contains(&tear),
                    "seed {seed}: no crash point tears operation {index} at byte {byte}"
                );
                if byte % geometry.program_size() == 0 {
                    units += 1;
                } else {
                    interior += 1;
                }
            }
        }
    }

    assert!(units > 0, "no tear ever landed on a program-unit boundary");
    assert!(interior > 0, "no tear ever landed inside a program unit");
}

#[test]
fn the_power_goes_before_and_after_every_barrier() {
    // The other half of issue #19's fault list. "After barrier `b`" is `(b, Whole,
    // PowerLoss)`; "before" it is the previous operation's `Whole` entry, or the one crash
    // point that precedes the whole sequence when the barrier is first.
    let mut barriers = 0_usize;

    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let geometry = device(&mut rng);
        let history = History::draw(&mut rng);
        let (runs, _) = sweep(&history, geometry);

        let clean = runs
            .first()
            .unwrap_or_else(|| unreachable!("the fault-free run"));
        let enumerated = injections(clean.ops(), geometry);

        for (index, op) in clean.ops().iter().enumerate() {
            if *op != Op::Barrier {
                continue;
            }
            barriers += 1;
            let after = Injection {
                op: index,
                progress: Progress::Whole,
                interruption: Interruption::PowerLoss,
            };
            assert!(
                enumerated.contains(&after),
                "seed {seed}: the power never goes after the barrier at {index}"
            );
            let before = index.checked_sub(1).map_or(
                Injection {
                    op: 0,
                    progress: Progress::None,
                    interruption: Interruption::PowerLoss,
                },
                |previous| Injection {
                    op: previous,
                    progress: Progress::Whole,
                    interruption: Interruption::PowerLoss,
                },
            );
            assert!(
                enumerated.contains(&before),
                "seed {seed}: the power never goes before the barrier at {index}"
            );
        }
    }

    assert!(
        barriers > 32,
        "only {barriers} barriers across the whole sweep"
    );
}

// ---------------------------------------------------------------------------------------
// The named fault families
// ---------------------------------------------------------------------------------------

/// A geometry whose journal is one erase block, for the hand-built cases below.
fn fixed_geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 256, 4, 1) else {
        unreachable!("256 is one whole 256-byte block of 4-byte units of single bytes")
    };
    geometry
}

#[test]
fn a_single_bit_flipped_anywhere_loses_only_what_lies_at_or_beyond_the_damage() {
    // CRC corruption, swept exhaustively rather than sampled: every bit of every byte of a
    // complete journal, one at a time.
    //
    // The oracle is asked, and it is asked in the only form that can be true here. Damage
    // inflicted on media *after* the writer finished is outside the harness's fault model —
    // the ledger records what the writer achieved, and a bit somebody flipped afterwards is
    // not a crash point — so a journal whose records are all acknowledged will lose one for
    // every flip that lands in a frame, and `LostAnAcknowledgedRecord` is the correct
    // verdict rather than a bug. What must be true, and is what a mis-striding reader would
    // break, is *which* record it is permitted to lose: never one whose frame ends before
    // the damage begins. So the assertion is that the only breach is that one, and that the
    // record it names still reaches the damaged byte.
    let geometry = fixed_geometry();
    let align = align_of(geometry);
    let mut rng = Rng::new(101);
    let history = History::draw(&mut rng);
    let (runs, _) = sweep(&history, geometry);
    let clean = runs
        .first()
        .unwrap_or_else(|| unreachable!("the fault-free run"));
    let whole = recover(clean.image(), &history, align);
    assert!(whole.len() >= 2, "the fixture recovered only {whole:?}");

    // Where each record's frame ends, taken from the write sequence the writer really
    // issued rather than recomputed here: `append_all` issues exactly one program per
    // record, so operation `i` *is* record `i`, and a layout worked out twice is a layout
    // that can disagree with itself.
    let ends: Vec<u32> = clean
        .ops()
        .iter()
        .filter_map(|op| match *op {
            Op::Program { offset, len } => Some(offset.wrapping_add(len)),
            Op::Erase { .. } | Op::Barrier => None,
        })
        .collect();
    assert_eq!(ends.len(), whole.len(), "one program per recovered record");

    let written_bytes = ends
        .last()
        .copied()
        .unwrap_or_else(|| unreachable!("the fixture wrote at least two records"));
    let mut refusals = 0_usize;
    let mut shortened = 0_usize;
    let mut breached = 0_usize;
    for byte in 0..clean.image().len() {
        for bit in 0..8_u32 {
            let mut damaged = clean.image().to_vec();
            let Some(cell) = damaged.get_mut(byte) else {
                unreachable!("the index came from the image's own length")
            };
            *cell ^= 1_u8 << bit;

            let recovered = recover(&damaged, &history, align);
            assert!(
                whole.starts_with(&recovered),
                "flipping bit {bit} of byte {byte} made recovery produce {recovered:?}, \
                 which is not a prefix of {whole:?}"
            );

            let at = u32::try_from(byte).unwrap_or(u32::MAX);
            match verify_oracle(clean.ledger(), &Recovery::new(&recovered)) {
                Ok(()) => {}
                Err(Breach::LostAnAcknowledgedRecord { record }) => {
                    let end = ends
                        .get(record.0 as usize)
                        .copied()
                        .unwrap_or_else(|| unreachable!("every lost record was written"));
                    assert!(
                        end > at,
                        "flipping bit {bit} of byte {byte} lost {record:?}, whose frame \
                         already ended at {end}"
                    );
                    breached += 1;
                }
                Err(other) => unreachable!(
                    "flipping bit {bit} of byte {byte} produced {other}, which no amount of \
                     damage to a finished journal should be able to do"
                ),
            }

            if recovered.len() < whole.len() {
                shortened += 1;
            }
            // Only damage inside the frames counts. A flip in the erased tail also makes
            // the scan refuse — the tail stops being erased, which is the stale-tail rule
            // doing its job — and counting those would make this number say "the scan
            // noticed something" when almost every flip lands there.
            if at < written_bytes && Scan::new(&damaged, align).any(|step| step.is_err()) {
                refusals += 1;
            }
        }
    }

    assert!(
        shortened > 0 && refusals > 0 && breached > 0,
        "no single-bit flip was noticed at all: {shortened} shortened, {refusals} refused, \
         {breached} breached"
    );
    // Every flip that shortened history is a flip the oracle was asked about and answered
    // the same way, so the two counts are the same measurement seen twice.
    assert_eq!(shortened, breached);
}

#[test]
fn a_payload_length_that_lies_is_refused_by_the_seal_or_by_the_bounds() {
    // "Malformed lengths", first half: a frame whose *header* seal is correct and whose
    // length is a lie. That is the case §09's two checksums exist for — `payload_len` is
    // read out of the bytes being validated, so the header seal is what makes it a number
    // the writer wrote rather than a number that was found.
    //
    // What each lie is caught *by* is asserted rather than left as "some error", because
    // the two mechanisms are different and only one of them is about the length: a short
    // lie moves the trailer inside the payload, so the frame seal fails; a long one puts
    // the trailer past the end of the journal, so the bounds check fails.
    // `a_self_consistently_sealed_frame_whose_length_is_wrong_for_its_kind_is_refused` is
    // the half that reaches the record's own length rule, which neither of these does.
    let geometry = fixed_geometry();
    let align = align_of(geometry);
    let mut buffer = [0_u8; 64];
    let record = RecordRef::EffectCompleted {
        seq: EffectSeq::FIRST,
        result: b"result",
    };
    let Ok(written) = frame::encode(&record, align, &mut buffer) else {
        unreachable!("64 bytes holds a six-byte completion")
    };

    for lie in [0_u16, 1, 5, 7, 64, 4096, u16::MAX] {
        let mut journal = buffer.get(..written).unwrap_or_default().to_vec();
        let Some(length_field) = journal.get_mut(8..10) else {
            unreachable!("a frame is at least twelve bytes")
        };
        length_field.copy_from_slice(&lie.to_le_bytes());
        // Reseal the header, so that the only thing wrong with this frame is its length.
        let seal = {
            let Some(header) = journal.get(..10) else {
                unreachable!("a frame is at least twelve bytes")
            };
            Catalogued::header_check(header)
        };
        let Some(header_crc) = journal.get_mut(10..12) else {
            unreachable!("a frame is at least twelve bytes")
        };
        header_crc.copy_from_slice(&seal.to_le_bytes());

        let claimed = frame::HEADER_BYTES
            .saturating_add(usize::from(lie))
            .saturating_add(frame::TRAILER_BYTES);
        let expected = if claimed <= journal.len() {
            DecodeError::IntegrityFailed
        } else {
            DecodeError::Truncated
        };
        let mut scan = Scan::new(&journal, align);
        assert_eq!(
            scan.next(),
            Some(Err(expected)),
            "a frame claiming a {lie}-byte payload was not refused the way it should be"
        );
        assert_eq!(
            scan.offset(),
            0,
            "the scan advanced past a frame it refused"
        );
    }
}

/// A frame that is self-consistent at every level the checksums can see.
///
/// Both seals are computed over what is really there, so nothing about the *bytes* is
/// wrong. Whether the record is legal is then the only question left, which is the point:
/// it is the one way to reach `decode`'s per-kind length rule, and a test that stopped at a
/// broken seal would never get there.
fn sealed_frame(kind: RecordKind, seq: EffectSeq, payload: &[u8]) -> Vec<u8> {
    let mut journal = Vec::new();
    journal.extend_from_slice(&frame::MAGIC.to_le_bytes());
    journal.push(frame::FORMAT_VERSION);
    journal.push(kind.0);
    journal.extend_from_slice(&seq.0.to_le_bytes());
    journal.extend_from_slice(
        &u16::try_from(payload.len())
            .unwrap_or(u16::MAX)
            .to_le_bytes(),
    );
    let header_seal = Catalogued::header_check(&journal);
    journal.extend_from_slice(&header_seal.to_le_bytes());
    journal.extend_from_slice(payload);
    let frame_seal = Catalogued::frame_check(&journal);
    journal.extend_from_slice(&frame_seal.to_le_bytes());
    journal
}

#[test]
fn a_self_consistently_sealed_frame_whose_length_is_wrong_for_its_kind_is_refused() {
    // "Malformed lengths", second half, and the half that is actually about the length. A
    // frame whose header seal, frame seal and trailer position all agree with the length it
    // claims passes everything the checksums can decide — so what refuses it can only be
    // the record's own rule about how long its payload is. Nothing in the first half
    // reaches that code at all.
    let scheduled = [
        1_u8, 0, // activity kind
        4, 0, // input length
        0xEF, 0xBE, 0xAD, 0xDE, // input digest
    ];

    // The control. Eight bytes is what a schedule's body is, so this one decodes.
    let good = sealed_frame(RecordKind::EFFECT_SCHEDULED, EffectSeq(3), &scheduled);
    assert_eq!(
        frame::decode(&good).map(|frame| frame.decoded),
        Ok(frame::Decoded::Record(RecordRef::EffectScheduled {
            seq: EffectSeq(3),
            kind: ActivityKind(1),
            input_len: 4,
            input_crc: 0xDEAD_BEEF,
        })),
        "the fixture frame is not well formed, so the refusals below prove nothing"
    );

    // Every length but the right one, sealed to agree with itself.
    for wrong in [0_usize, 1, 4, 7, 9, 12, 40] {
        let mut payload = scheduled.to_vec();
        payload.resize(wrong, 0);
        let journal = sealed_frame(RecordKind::EFFECT_SCHEDULED, EffectSeq(3), &payload);
        assert_eq!(
            frame::decode(&journal).err(),
            Some(DecodeError::MalformedRecord),
            "a schedule record with a {wrong}-byte body was accepted"
        );
    }

    // A run start needs four bytes of workflow identity before its input begins, and it is
    // a *minimum* rather than an exact length — so the boundary is on the other side.
    for wrong in [0_usize, 1, 3] {
        let journal = sealed_frame(RecordKind::RUN_STARTED, EffectSeq::FIRST, &vec![0; wrong]);
        assert_eq!(
            frame::decode(&journal).err(),
            Some(DecodeError::MalformedRecord),
            "a run start with a {wrong}-byte body was accepted"
        );
    }
    assert!(
        frame::decode(&sealed_frame(
            RecordKind::RUN_STARTED,
            EffectSeq::FIRST,
            &[0; 4]
        ))
        .is_ok(),
        "four bytes is exactly the workflow identity, so a run start with no input is legal"
    );

    // And the other rule `decode_body` owns: a run-scoped record carries no effect number.
    let journal = sealed_frame(RecordKind::RUN_COMPLETED, EffectSeq(1), b"done");
    assert_eq!(
        frame::decode(&journal).err(),
        Some(DecodeError::MalformedRecord),
        "a run-scoped record numbered as an effect was accepted"
    );
}

#[test]
fn a_journal_filled_to_its_last_byte_still_ends_where_it_ends() {
    // Capacity boundary. The scan's ordinary end of history is an erased tail, and a
    // journal with no tail at all has none — so this is the one shape where "the journal
    // ended" has to be decided by the slice running out rather than by a byte on media.
    let geometry = fixed_geometry();
    let align = align_of(geometry);
    let mut buffer = [0_u8; 64];
    let record = RecordRef::EffectScheduled {
        seq: EffectSeq::FIRST,
        kind: ActivityKind(1),
        input_len: 4,
        input_crc: frame::input_digest(b"blob"),
    };
    let Ok(written) = frame::encode(&record, align, &mut buffer) else {
        unreachable!("64 bytes holds a schedule record")
    };
    let frame_bytes = buffer.get(..written).unwrap_or_default();

    let mut journal = Vec::new();
    while journal.len() + written <= geometry.capacity() as usize {
        journal.extend_from_slice(frame_bytes);
    }
    let exact = journal.len();
    journal.resize(geometry.capacity() as usize, 0xFF);

    // Filled to the last byte the frames fit in: every frame is history, and the scan ends
    // because the slice does.
    let Some(flush) = journal.get(..exact) else {
        unreachable!("`exact` came from the vector's own length")
    };
    let flush_records = Scan::new(flush, align).count();
    assert_eq!(flush_records, exact / written);
    assert!(Scan::new(flush, align).all(|step| step.is_ok()));

    // One program unit short of a whole frame at the end: a truncation, not a record, and
    // not a clean end either — the bytes are programmed.
    let Some(short) = journal.get(..exact - align.get() as usize) else {
        unreachable!("a frame is longer than one program unit")
    };
    let short_records = Scan::new(short, align).take_while(Result::is_ok).count();
    assert_eq!(short_records, flush_records - 1);
    assert!(
        Scan::new(short, align).any(|step| step.is_err()),
        "a journal cut inside its last frame reported a clean end of history"
    );

    // And with the erased tail restored, the same frames and an ordinary end.
    assert_eq!(Scan::new(&journal, align).count(), flush_records);
    assert!(Scan::new(&journal, align).all(|step| step.is_ok()));
}

#[test]
fn a_sequence_at_the_top_of_the_space_survives_the_frame_and_stops_the_run() {
    // Issue #19's "sequence wrap". §07 makes exhaustion terminal rather than wrapping —
    // `EffectSeq::successor` returns `None` at `MAX` and the allocator never returns to
    // `FIRST` — so the boundary to test is that the *frame* carries the top of the space
    // unharmed and that nothing invents a sequence past it.
    //
    // Walking a cursor to 2^32 records is 64 GiB of journal at the frame's sixteen-byte
    // floor, so it is not walked here; `waymaker-core`'s allocator suite reaches the
    // ceiling directly, and this is the wire-format half.
    let geometry = fixed_geometry();
    let align = align_of(geometry);
    let mut buffer = [0_u8; 64];

    for seq in [EffectSeq(u32::MAX - 1), EffectSeq::MAX] {
        let record = RecordRef::EffectScheduled {
            seq,
            kind: ActivityKind(9),
            input_len: 0,
            input_crc: 0,
        };
        let Ok(written) = frame::encode(&record, align, &mut buffer) else {
            unreachable!("64 bytes holds a schedule record")
        };
        let journal = buffer.get(..written).unwrap_or_default();
        let mut scan = Scan::new(journal, align);
        assert_eq!(
            scan.next(),
            Some(Ok(record)),
            "a sequence of {seq:?} did not survive"
        );
    }

    assert_eq!(
        EffectSeq::MAX.successor(),
        None,
        "the sequence space wrapped"
    );

    // And a journal whose sequences skip is refused by the cursor rather than replayed:
    // "out of sequence" is a fact about the run, so the scan does not own it and the cursor
    // does.
    //
    // The prefix in front of the gap is the whole point. A journal that opened with a
    // schedule would be refused at its *first* record — a fresh cursor accepts only
    // `RunStarted` — and the assertion below would then pass with sequence checking removed
    // entirely. So the run starts, one effect is scheduled and resolved, and only the
    // fourth record jumps to the top of the space.
    let records = [
        RecordRef::RunStarted {
            workflow_kind: 7,
            workflow_version: 1,
            input: b"",
        },
        RecordRef::EffectScheduled {
            seq: EffectSeq::FIRST,
            kind: ActivityKind(9),
            input_len: 0,
            input_crc: 0,
        },
        RecordRef::EffectCompleted {
            seq: EffectSeq::FIRST,
            result: b"",
        },
        RecordRef::EffectScheduled {
            seq: EffectSeq::MAX,
            kind: ActivityKind(9),
            input_len: 0,
            input_crc: 0,
        },
    ];
    let mut journal = Vec::new();
    for record in &records {
        let Ok(written) = frame::encode(record, align, &mut buffer) else {
            unreachable!("64 bytes holds any record in this array")
        };
        journal.extend_from_slice(buffer.get(..written).unwrap_or_default());
    }

    let mut cursor = ReplayCursor::new(RUN);
    let mut accepted = 0_usize;
    let mut refusal = None;
    for step in Scan::new(&journal, align) {
        let Ok(record) = step else {
            unreachable!("every frame in this journal was just encoded")
        };
        match cursor.advance(record) {
            Ok(_) => accepted += 1,
            Err(error) => {
                refusal = Some(error);
                break;
            }
        }
    }
    assert_eq!(
        accepted,
        records.len() - 1,
        "the cursor refused a record before the one that jumps to the top of the space"
    );
    assert_eq!(
        refusal,
        Some(KernelError::MalformedHistory),
        "the cursor replayed a schedule whose sequence jumps to the top of the space"
    );
    // And it stays refused: §08's divergence is terminal, so the run cannot be resumed by
    // handing it the record it should have had.
    assert_eq!(
        cursor.advance(RecordRef::EffectScheduled {
            seq: EffectSeq(1),
            kind: ActivityKind(9),
            input_len: 0,
            input_crc: 0,
        }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_run_that_overflows_its_journal_recovers_what_fitted_and_nothing_more() {
    // The other capacity boundary: a history longer than the device. The writer stops, so
    // the records that did not fit were never attempted — and recovery producing one of
    // them would be an invention, which the oracle names.
    let Ok(tiny) = Geometry::new(64, 64, 4, 1) else {
        unreachable!("64 is one whole 64-byte block of 4-byte units")
    };
    let align = align_of(tiny);
    let history = History {
        plans: (0..12)
            .map(|index| {
                if index == 0 {
                    Planned::Started
                } else if index % 2 == 1 {
                    Planned::Scheduled {
                        seq: EffectSeq(u32::try_from(index / 2).unwrap_or(0)),
                        kind: ActivityKind(1),
                    }
                } else {
                    Planned::Completed {
                        seq: EffectSeq(u32::try_from(index / 2 - 1).unwrap_or(0)),
                    }
                }
            })
            .collect(),
        barriers: vec![true; 12],
        payloads: vec![Vec::new(); 12],
    };

    let (runs, log) = sweep(&history, tiny);
    let clean = runs
        .first()
        .unwrap_or_else(|| unreachable!("the fault-free run"));
    let whole = recover(clean.image(), &history, align);
    assert!(
        !whole.is_empty() && whole.len() < history.plans.len(),
        "the journal was meant to overflow; it recovered {whole:?} of {} records",
        history.plans.len()
    );
    assert_eq!(
        clean.ledger().len(),
        whole.len(),
        "the writer declared records it never wrote"
    );
    for id in history
        .schedules()
        .into_iter()
        .filter(|id| !whole.contains(id))
    {
        assert_eq!(
            clean.ledger().state(id),
            None,
            "{id:?} did not fit and yet the ledger has a state for it"
        );
    }

    for (run, dispatched) in runs.iter().zip(log.iter()) {
        let recovered = recover(run.image(), &history, align);
        assert_eq!(
            verify_oracle(
                run.ledger(),
                &Recovery::new(&recovered).dispatched(dispatched)
            ),
            Ok(()),
            "at {:?}: recovered {recovered:?}",
            run.injection()
        );
    }
}

#[test]
fn a_record_the_writer_never_started_is_not_one_recovery_may_produce() {
    // The oracle's fail-closed direction, driven by the real writer rather than by a
    // hand-built ledger: at the crash point that stops before a record's first byte, that
    // record is merely attempted, and claiming to have recovered it is a breach.
    let geometry = fixed_geometry();
    let align = align_of(geometry);
    let mut rng = Rng::new(303);
    let history = History::draw(&mut rng);
    let (runs, _) = sweep(&history, geometry);

    let mut caught = 0_usize;
    for run in &runs {
        let mut recovered = recover(run.image(), &history, align);
        let Some(next) = history
            .plans
            .iter()
            .enumerate()
            .filter_map(|(index, _)| u32::try_from(index).ok().map(RecordId))
            .find(|id| !recovered.contains(id))
        else {
            continue;
        };
        if run.ledger().state(next) != Some(Durability::Attempted) {
            continue;
        }
        recovered.push(next);
        // The exact breach, not merely "some breach". Appending anything past a complete
        // prefix is also `NotAPrefix`, so an `is_err()` assertion here would survive
        // deleting the check it is named after — which is the failure this whole file
        // exists to make impossible.
        assert_eq!(
            verify_oracle(run.ledger(), &Recovery::new(&recovered)),
            Err(Breach::RecoveredWhatWasNeverAttempted { record: next }),
            "the oracle accepted {next:?}, none of whose bytes ever reached media"
        );
        caught += 1;
    }
    assert!(
        caught > 0,
        "no crash point stopped before a record's first byte"
    );
}

#[test]
fn a_stale_tail_left_by_an_interrupted_erase_is_never_read_as_a_clean_end() {
    // Issue #19's "stale tails". The oracle is deliberately not applied: this writer erases
    // a block that holds acknowledged records, which destroys them on purpose, and an
    // oracle asked whether recovery lost an acknowledged record can only say yes. What
    // survives is the property that matters — a reader must not report a *clean* end of
    // history with committed frames still behind the hole.
    // 64-byte blocks, so the 128-byte erase below spans two of them and the enumeration
    // really contains an *interrupted* erase. An erase is interrupted at erase blocks and
    // nowhere else, so a block as large as the erase would leave this test asserting a
    // property of a completed one — which is a different, weaker thing than the name says.
    let Ok(blocks) = Geometry::new(256, 64, 4, 1) else {
        unreachable!("256 is four whole 64-byte blocks of 4-byte units")
    };
    let align = align_of(blocks);
    let history = History {
        plans: (0..8)
            .map(|index| Planned::Scheduled {
                seq: EffectSeq(u32::try_from(index).unwrap_or(0)),
                kind: ActivityKind(1),
            })
            .collect(),
        barriers: vec![false; 8],
        payloads: vec![Vec::new(); 8],
    };

    let runs = match Harness::new(blocks).run(|session| {
        let mut dispatched = Vec::new();
        append_all(session, &history, blocks, &mut dispatched)?;
        session.barrier()?;
        session.erase(0, 128)
    }) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    };

    let mut stale = 0_usize;
    let mut half_erased = 0_usize;
    for run in &runs {
        let erased_head = run.image().first() == Some(&0xFF);
        let data_behind = run.image().iter().any(|byte| *byte != 0xFF);
        if !erased_head || !data_behind {
            continue;
        }
        stale += 1;
        if matches!(
            run.injection(),
            Some(Injection {
                progress: Progress::Bytes(_),
                ..
            })
        ) {
            half_erased += 1;
        }
        let mut scan = Scan::new(run.image(), align);
        assert_eq!(
            scan.next(),
            Some(Err(DecodeError::IntegrityFailed)),
            "an erased header with frames behind it read as a clean end of history"
        );
        assert_eq!(scan.offset(), 0);
        assert!(recover(run.image(), &history, align).is_empty());
    }
    assert!(stale > 0, "no crash point left a stale tail");
    assert!(
        half_erased > 0,
        "every stale tail here came from an erase that *finished*, so no crash point was \
         inside the erase and the test is not the one its name claims"
    );
}

#[test]
fn a_writer_that_retries_a_failed_write_is_a_world_the_sweep_would_not_otherwise_reach() {
    // Half the enumerated crash set is `Interruption::Failure`, and for a writer that
    // propagates every error with `?` — which is every other writer here, and every writer
    // in `tests/committed_prefix.rs` — a failure produces byte-identical media to the power
    // loss armed at the same point. `Harness` documents that it takes a *closure* rather
    // than a recorded write log precisely so that "what does the writer do when this call
    // returns an error" is a real question, and nothing was asking it.
    //
    // This writer retries a failed program once. On NOR that repairs the write: the bytes
    // that landed are already correct and the rest are still erased, so programming the same
    // frame again completes it. Two things follow that no other test here reaches — the
    // failure worlds stop being duplicates of the power-loss worlds, and `Missing`'s "what
    // one write failed to put on media a retry may have put there since" is exercised rather
    // than described.
    let geometry = fixed_geometry();
    let align = align_of(geometry);
    let mut rng = Rng::new(404);
    let history = History::draw(&mut rng);

    let runs = match Harness::new(geometry).run(|session| {
        let mut buffer = [0_u8; 128];
        let mut at = 0_u32;
        for index in 0..history.plans.len() {
            let Some(record) = history.record(index) else {
                break;
            };
            let Ok(written) = frame::encode(&record, align, &mut buffer) else {
                break;
            };
            let Some(bytes) = buffer.get(..written) else {
                break;
            };
            let len = u32::try_from(written).unwrap_or(u32::MAX);
            if at
                .checked_add(len)
                .is_none_or(|end| end > geometry.capacity())
            {
                break;
            }
            session.begin_record(RecordId(u32::try_from(index).unwrap_or(u32::MAX)));
            // One retry, and one only. After a power loss the session is dead and every
            // call returns `PowerLoss`, so a loop here would be a writer that cannot stop.
            if session.program(at, bytes).is_err() {
                session.program(at, bytes)?;
            }
            at = at.wrapping_add(len);
            session.barrier()?;
        }
        session.end_record();
        Ok::<(), FaultError>(())
    }) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    };

    // The oracle holds for a writer that reacts to failure, which is the point of asking.
    for run in &runs {
        let recovered = recover(run.image(), &history, align);
        assert_eq!(
            verify_oracle(run.ledger(), &Recovery::new(&recovered)),
            Ok(()),
            "at {:?}: recovered {recovered:?}",
            run.injection()
        );
    }

    // A record whose write was torn by a failure and put right by the retry: not torn at the
    // end of the run, and acknowledged by the barrier that followed. This is the branch that
    // judges tornness when the run *ends* rather than when the operation ran.
    let repaired = runs
        .iter()
        .filter(|run| {
            matches!(
                run.injection(),
                Some(Injection {
                    progress: Progress::Bytes(_),
                    interruption: Interruption::Failure,
                    ..
                })
            ) && run.ledger().order().any(|id| {
                run.ledger().torn(id) == Some(false)
                    && run.ledger().state(id) == Some(Durability::Acknowledged)
            })
        })
        .count();
    assert!(
        repaired > 0,
        "no failed write was ever repaired by the retry, so the reacting-writer case is \
         still untested"
    );

    // And the two interruptions really do lead to different media now.
    let mut worlds: BTreeMap<(usize, Progress, Interruption), &[u8]> = BTreeMap::new();
    for run in &runs {
        if let Some(injection) = run.injection() {
            worlds.insert(
                (injection.op, injection.progress, injection.interruption),
                run.image(),
            );
        }
    }
    let diverged = worlds
        .iter()
        .filter(|((op, progress, interruption), image)| {
            *interruption == Interruption::Failure
                && worlds
                    .get(&(*op, *progress, Interruption::PowerLoss))
                    .is_some_and(|lost| lost != *image)
        })
        .count();
    assert!(
        diverged > 0,
        "every failure produced the same media as the power loss armed at the same point, \
         so half the enumerated crash set is still a duplicate of the other half"
    );
}
