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
//! The first three are swept here, over histories drawn from a seed rather than written by
//! hand. The fourth needs two banks and a generation seal, and it is
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
use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, DecodeError, EffectSeq, RecordRef, ReplayCursor, RunId};
use waymaker_fault::{
    Durability, FaultError, Harness, Injection, Interruption, Op, Progress, RecordId, Recovery,
    Rng, Run, Session, injections, random_geometry, verify_oracle,
};
use waymaker_flash::frame::{self, ProgramAlign, Scan};
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};
use waymaker_flash::storage::{Geometry, StableStorage};

/// How many seeds the sweep draws.
const SEEDS: u64 = 32;

/// The largest device the sweep will draw.
///
/// Every byte of journal is two crash points and every crash point is one more run of the
/// writer, so this is a runtime dial and not a semantic one. Two hundred and fifty-six
/// bytes is a journal of several records at every program granularity the generator draws,
/// which is what the sweep needs; a megabyte would be the same test, slower.
const MAX_CAPACITY: u32 = 256;

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
    let bytes = u16::try_from(geometry.program_size()).unwrap_or(1);
    let Some(align) = ProgramAlign::new(bytes) else {
        unreachable!("a drawn geometry's program size is a power of two the frame permits")
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
    shapes: BTreeSet<Vec<RecordId>>,
    prefix_lengths: BTreeSet<usize>,
    acknowledged_kept_while_others_were_lost: usize,
    dispatched: usize,
    dispatched_then_power_lost: usize,
    scans_that_refused: usize,
    torn_records: usize,
    empty_recoveries: usize,
    full_recoveries: usize,
}

#[test]
fn the_oracle_holds_over_random_histories_on_random_geometries_at_every_crash_point() {
    let mut census = Census::default();

    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let geometry = random_geometry(&mut rng, MAX_CAPACITY);
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
        let whole = recover(clean.image(), &history, align);
        census.shapes.insert(whole.clone());

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
            if Scan::new(run.image(), align).any(|step| step.is_err()) {
                census.scans_that_refused += 1;
            }
            if run
                .ledger()
                .order()
                .any(|id| run.ledger().torn(id) == Some(true))
            {
                census.torn_records += 1;
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
        census.shapes.len() >= 8,
        "only {} distinct histories",
        census.shapes.len()
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
        "no effect was dispatched and then lost to a power cut, so §02 decision 3 was never \
         put to the test: {} dispatches, {} of them followed by a power loss",
        census.dispatched,
        census.dispatched_then_power_lost
    );
    assert!(
        census.torn_records > 0,
        "no crash point tore a record in half across the whole sweep"
    );
    assert!(
        census.scans_that_refused > 0,
        "no crash point produced a journal the scan refused, so integrity failure was never \
         reached from a real torn write"
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
        let geometry = random_geometry(&mut rng, MAX_CAPACITY);
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
        let geometry = random_geometry(&mut rng, MAX_CAPACITY);
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
fn a_single_bit_flipped_anywhere_never_lengthens_recovered_history() {
    // CRC corruption, swept exhaustively rather than sampled: every bit of a complete
    // journal, one at a time. A checksum's job is to make damage stop the scan, and the
    // failure that matters is the silent one — damage that makes recovery produce *more*
    // than it should, or something other than a prefix of what is really there.
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

    let mut refusals = 0_usize;
    let mut shortened = 0_usize;
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
            if recovered.len() < whole.len() {
                shortened += 1;
            }
            if Scan::new(&damaged, align).any(|step| step.is_err()) {
                refusals += 1;
            }
        }
    }

    assert!(
        shortened > 0 && refusals > 0,
        "no single-bit flip was noticed at all: {shortened} shortened, {refusals} refused"
    );
}

#[test]
fn a_payload_length_that_lies_is_refused_even_with_a_header_that_checks_out() {
    // "Malformed lengths", from issue #19's coverage list, at the one place it is
    // dangerous: `payload_len` is read out of the bytes being validated, so a frame whose
    // *header* seal is correct and whose length is a lie is the case §09's two checksums
    // exist for. Resealing the header is what makes this a real test rather than a
    // corrupted-header test wearing a different name.
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

        let mut scan = Scan::new(&journal, align);
        assert!(
            matches!(scan.next(), Some(Err(_))),
            "a frame claiming a {lie}-byte payload was accepted"
        );
        assert_eq!(
            scan.offset(),
            0,
            "the scan advanced past a frame it refused"
        );
    }
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
    // "out of sequence" is a fact about the run, so the scan does not own it and the
    // cursor does.
    let mut journal = Vec::new();
    for seq in [EffectSeq::FIRST, EffectSeq::MAX] {
        let record = RecordRef::EffectScheduled {
            seq,
            kind: ActivityKind(9),
            input_len: 0,
            input_crc: 0,
        };
        let Ok(written) = frame::encode(&record, align, &mut buffer) else {
            unreachable!("64 bytes holds a schedule record")
        };
        journal.extend_from_slice(buffer.get(..written).unwrap_or_default());
    }
    let mut cursor = ReplayCursor::new(RUN);
    let mut accepted = 0_usize;
    for step in Scan::new(&journal, align) {
        let Ok(record) = step else { break };
        if cursor.advance(record).is_err() {
            break;
        }
        accepted += 1;
    }
    assert_eq!(
        accepted, 0,
        "the cursor replayed a journal whose sequences jump to the top of the space"
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
        assert!(
            verify_oracle(run.ledger(), &Recovery::new(&recovered)).is_err(),
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
    let Ok(two_blocks) = Geometry::new(256, 128, 4, 1) else {
        unreachable!("256 is two whole 128-byte blocks of 4-byte units")
    };
    let align = align_of(two_blocks);
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

    let runs = match Harness::new(two_blocks).run(|session| {
        let mut dispatched = Vec::new();
        append_all(session, &history, two_blocks, &mut dispatched)?;
        session.barrier()?;
        session.erase(0, 128)
    }) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    };

    let mut stale = 0_usize;
    for run in &runs {
        let erased_head = run.image().first() == Some(&0xFF);
        let data_behind = run.image().iter().any(|byte| *byte != 0xFF);
        if !erased_head || !data_behind {
            continue;
        }
        stale += 1;
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
}
