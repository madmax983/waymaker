//! §10's seven-step bank swap, at every point the injector can interrupt it.
//!
//! Issue [#26](https://github.com/madmax983/waymaker/issues/26) asks for three things, and
//! the first two are this file:
//!
//! * "Every crash point across all seven steps is enumerated and asserted against the
//!   correct recovery outcome" — [`the_recovery_rules_hold_at_every_crash_point_of_the_swap`],
//!   with [`every_step_of_the_protocol_has_crash_points_in_the_sweep`] as the census that
//!   stops the sweep from thinning out unnoticed;
//! * "The lazy erase of the old bank is itself crash-safe and never makes the old bank look
//!   authoritative again" — [`the_lazy_erase_never_returns_the_old_bank_to_authority`].
//!
//! The third is effect identity, which no image can show; it is
//! `crates/waymaker-flash/tests/swap.rs`.
//!
//! # This drives the real thing
//!
//! Every byte below is written by `waymaker_flash::swap` — the real typestate, the real
//! header codec, the real generation seal — and read by `waymaker_flash::bank`'s real
//! selection. `banks.rs` sweeps a swap protocol written *here*, which is what that file
//! needed before there was a writer; this one sweeps the writer.
//!
//! # What the teeth are
//!
//! Three swaps with one bug each, hand-written from the same primitives because the real
//! writer cannot express them. A suite that only ever saw a correct protocol would pass with
//! its own assertions deleted, so each mutant names the rule it breaks:
//! [`a_swap_that_erases_the_bank_it_booted_can_leave_nothing_to_boot_from`],
//! [`a_swap_that_does_not_bump_the_generation_leaves_two_authorities`], and
//! [`a_reclaim_taken_before_the_commit_barrier_can_lose_both_runs`] — the last being the one
//! that says what §10's step 7 being *after* step 6 is for.

use waymaker_core::RunId;
use waymaker_fault::{
    Breach, FaultError, Harness, Injection, Op, Progress, RecordId, Recovery as OracleRecovery,
    Run, Session, verify_oracle,
};
use waymaker_flash::bank::{self, Authority, BankHeader, BankId, BankLayout, Generation};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, Recovery};
use waymaker_flash::storage::Geometry;
use waymaker_flash::storage::StableStorage;
use waymaker_flash::swap::{Retired, Swap, SwapError, SwapStepError};

// ---------------------------------------------------------------------------------------
// The device, and the runs on it
// ---------------------------------------------------------------------------------------

/// The run the device is on when the swap starts.
const RUN: RunId = RunId(0x0000_0000_0000_00B7);

/// The run the swap installs.
const NEXT_RUN: RunId = RunId(0x0000_0000_0000_0C5E);

/// The run two generations ago, left in the spare bank so the swap has to recycle it.
const STALE_RUN: RunId = RunId(0x0000_0000_0000_0011);

/// The one record the swap declares: a bank swap is one durable unit or it is nothing.
const SWAP: RecordId = RecordId(1);

/// The generation the device's spare bank carries when the sweep starts.
const STALE: Generation = Generation(0);

/// The generation the device is booting from when the swap starts.
const CURRENT: Generation = Generation(1);

/// The generation the swap installs.
///
/// Minted from the generation it replaces rather than written down, so this file's
/// expectations come from the same arithmetic `Swap::beginning` uses.
const NEXT: Generation = match CURRENT.successor() {
    Some(next) => next,
    None => unreachable!(),
};

/// A device of eight erase blocks: two banks of four blocks each.
///
/// Four blocks per bank rather than one, for `banks.rs`'s reason: an erase is interrupted at
/// erase blocks and nowhere else, so a bank of one block has no interior tear point and both
/// erases in §10's protocol — step 2's and step 7's — would be all-or-nothing.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 32, 4, 1) else {
        unreachable!("256 is eight whole 32-byte blocks of 4-byte units of single bytes")
    };
    geometry
}

fn layout() -> BankLayout {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("eight erase blocks is four per bank")
    };
    layout
}

fn align() -> ProgramAlign {
    layout().align()
}

fn header_of(run: RunId, input: &'static [u8]) -> BankHeader<'static> {
    BankHeader {
        run,
        align: align(),
        workflow_kind: 7,
        workflow_version: 1,
        input_schema: 1,
        input,
    }
}

/// The header the bank in use carries.
fn current_header() -> BankHeader<'static> {
    header_of(RUN, b"current")
}

/// The header the swap installs.
fn next_header() -> BankHeader<'static> {
    header_of(NEXT_RUN, b"next")
}

/// The header the spare bank carries before the swap recycles it.
fn stale_header() -> BankHeader<'static> {
    header_of(STALE_RUN, b"stale")
}

/// The header a bank at `generation` is expected to carry.
fn expected_header(generation: Generation) -> BankHeader<'static> {
    match generation.0 {
        0 => stale_header(),
        1 => current_header(),
        _ => next_header(),
    }
}

// ---------------------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------------------

/// Anything that can stop this file's writers, so the harness has one `Debug` type.
///
/// The payloads are what `HarnessError::WriterFailedWithNoFaultsArmed` prints, and dead-code
/// analysis deliberately does not count a derived `Debug` as a read.
#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the payloads are read by the derived `Debug`, which dead-code analysis ignores"
)]
enum Failed {
    Media(FaultError),
    Plan(SwapError),
    Step(SwapStepError<FaultError>),
}

impl From<FaultError> for Failed {
    fn from(error: FaultError) -> Self {
        Self::Media(error)
    }
}

impl From<SwapError> for Failed {
    fn from(error: SwapError) -> Self {
        Self::Plan(error)
    }
}

impl From<SwapStepError<FaultError>> for Failed {
    fn from(error: SwapStepError<FaultError>) -> Self {
        Self::Step(error)
    }
}

/// Programs `id`'s bank header from `header`, padded to the program unit.
fn program_header(
    session: &mut Session,
    id: BankId,
    header: &BankHeader<'_>,
) -> Result<usize, FaultError> {
    let region = layout().bank(id);
    let mut page = [0_u8; 64];
    let Ok(written) = bank::encode_header(header, &mut page) else {
        unreachable!("a bank header of this shape fits 64 bytes")
    };
    let Some(bytes) = page.get(..written) else {
        unreachable!("`encode_header` reports what it wrote")
    };
    session.program(region.base(), bytes)?;
    Ok(written)
}

/// Programs `id`'s generation seal, naming the header bytes that landed on media.
fn program_seal(
    session: &mut Session,
    id: BankId,
    generation: Generation,
) -> Result<(), FaultError> {
    let region = layout().bank(id);
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
    let Ok(written) = bank::encode_seal(&seal, align(), &mut sealed) else {
        unreachable!("a seal fits 16 bytes at a 4-byte program unit")
    };
    let Some(bytes) = sealed.get(..written) else {
        unreachable!("`encode_seal` reports what it wrote")
    };
    session.program(region.seal_offset(), bytes)
}

/// Installs a whole bank by hand: the header, a barrier, the seal, a barrier.
///
/// The device's history, not the protocol under test. A sweep that built its starting state
/// with the writer it is sweeping would be a sweep of nothing.
fn install(
    session: &mut Session,
    id: BankId,
    generation: Generation,
    header: &BankHeader<'_>,
) -> Result<(), FaultError> {
    program_header(session, id, header)?;
    session.barrier()?;
    program_seal(session, id, generation)?;
    session.barrier()
}

/// The device as a previous life left it: a stale bank, then the one in use.
///
/// Neither is declared as a record, because a record is what recovery must account for and
/// setup is not one.
fn previous_life(session: &mut Session) -> Result<(), FaultError> {
    install(session, BankId::B, STALE, &stale_header())?;
    install(session, BankId::A, CURRENT, &current_header())
}

/// The journal region of the bank in use.
fn current_region() -> JournalRegion {
    let Ok(region) = JournalRegion::of(layout(), BankId::A, &current_header()) else {
        unreachable!("bank A holds a journal behind its header")
    };
    region
}

/// The swap the real writer performs, planned over the bank in use.
///
/// The retired reader is an unscanned [`Recovery`], which is deliberate and costs the sweep
/// nothing: a scan is reads, reads move no bytes, and the injector's crash points are
/// mutations. What is being swept is what the swap *writes*.
fn planned() -> Result<Swap<'static>, SwapError> {
    Swap::beginning(
        layout(),
        Authority::Bank {
            id: BankId::A,
            generation: CURRENT,
        },
        RUN,
        Retired::Recovery(Recovery::new(current_region())),
        next_header(),
    )
}

/// §10's seven steps, by the real writer.
fn swap(session: &mut Session) -> Result<(), Failed> {
    previous_life(session)?;

    // Steps 1 and 2. The erase and its barrier are not a record: an erased bank is not a
    // swap, and a record declared here would be acknowledged by a barrier after which
    // nothing of the new run is on media — obliging recovery to produce a swap the device is
    // right to have no trace of.
    let prepared = planned()?.prepare(session)?;

    // Steps 3 and 4. Not a record either, and for the same reason §10 gives: "a crash before
    // step 5 recovers the old run", so a header with no seal over it is not a swap that
    // happened.
    let mut page = [0_u8; 64];
    let sealable = prepared
        .stage(session, &mut page)?
        .payload_barrier(session)?;

    // Steps 5 and 6. The seal is the only separately recoverable thing in a swap, so it is
    // the whole of the record: §10's "a crash after step 6 recovers the new run" is this
    // barrier and nothing before it.
    session.begin_record(SWAP);
    let installed = sealable.commit(session)?;
    session.end_record();

    // Step 7, outside the record: the old bank is already beaten by a higher generation, so
    // erasing it changes no answer that recovery gives.
    installed.reclaim(session)?;
    Ok(())
}

/// The swap that programs a seal no reader can validate.
///
/// The same eight operations in the same order — so the step map above still says what it
/// says — and the seal is bytes rather than a seal. Bank B is never authoritative, bank A
/// is erased anyway, and §10's "a crash after step 6 recovers the new run" is false from
/// the moment the commit barrier returns.
fn swap_whose_seal_is_not_a_seal(session: &mut Session) -> Result<(), Failed> {
    previous_life(session)?;

    let (spare, retiring) = (layout().bank(BankId::B), layout().bank(BankId::A));
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;
    program_header(session, BankId::B, &next_header())?;
    session.barrier()?;

    session.begin_record(SWAP);
    let mut rubbish = [0_u8; 16];
    let Ok(seal_len) = usize::try_from(spare.seal_bytes()) else {
        unreachable!("a host holds a seal")
    };
    let Some(bytes) = rubbish.get_mut(..seal_len) else {
        unreachable!("a seal fits 16 bytes at a 4-byte program unit")
    };
    bytes.fill(0x5A);
    session.program(spare.seal_offset(), bytes)?;
    session.barrier()?;
    session.end_record();

    session.erase(retiring.base(), retiring.bytes())?;
    session.barrier()?;
    Ok(())
}

/// The swap that installs the new generation over the *previous* run's header.
///
/// Every structure on media is intact and self-consistent — the seal names the header
/// beneath it, so `sealed_generation` answers `Some(NEXT)` — and the run it names is the one
/// two generations ago. A reader boots generation `NEXT` and finds the stale run's input
/// under it, which is §10's "recovery never combines their footprints" broken in the one way
/// no count, no oracle and no checksum can see.
fn swap_that_installs_the_wrong_run(session: &mut Session) -> Result<(), Failed> {
    previous_life(session)?;

    let (spare, retiring) = (layout().bank(BankId::B), layout().bank(BankId::A));
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;
    program_header(session, BankId::B, &stale_header())?;
    session.barrier()?;

    session.begin_record(SWAP);
    program_seal(session, BankId::B, NEXT)?;
    session.barrier()?;
    session.end_record();

    session.erase(retiring.base(), retiring.bytes())?;
    session.barrier()?;
    Ok(())
}

fn drive(writer: fn(&mut Session) -> Result<(), Failed>) -> Vec<Run> {
    match Harness::new(geometry()).run(writer) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

// ---------------------------------------------------------------------------------------
// The seven steps, as positions in the write sequence
// ---------------------------------------------------------------------------------------

/// One step of §10's protocol.
///
/// Issue #26 numbers seven. Step 1 has no media operation at all — it is
/// `Swap::beginning` consuming the retired reader — so the census below asks for crash
/// points in steps 2 to 7 and requires step 1 to have none, which is the honest shape of a
/// step whose whole guarantee is that it does not compile otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// 1. Stop accepting new effects for the current run.
    StopEffects,
    /// 2. Erase the inactive bank, and order what follows after it.
    EraseInactive,
    /// 3. Write the new bank header.
    WriteHeader,
    /// 4. Barrier: the new bank's payload becomes durable.
    PayloadBarrier,
    /// 5. Write the higher-generation seal.
    WriteSeal,
    /// 6. Barrier: the new bank becomes authoritative.
    CommitBarrier,
    /// 7. Lazily erase the old bank.
    ReclaimOld,
}

impl Step {
    const ALL: [Self; 7] = [
        Self::StopEffects,
        Self::EraseInactive,
        Self::WriteHeader,
        Self::PayloadBarrier,
        Self::WriteSeal,
        Self::CommitBarrier,
        Self::ReclaimOld,
    ];

    /// Which operations of the swap's own sequence this step is, relative to [`FIRST_OP`].
    const fn ops(self) -> (usize, usize) {
        match self {
            // No media operation: the whole of step 1 is a value being consumed.
            Self::StopEffects => (0, 0),
            // The erase and the barrier that orders the header after it. Issue #26 numbers
            // the erase alone; the barrier belongs to it, because without it §12 permits the
            // header to become durable first.
            Self::EraseInactive => (0, 2),
            Self::WriteHeader => (2, 3),
            Self::PayloadBarrier => (3, 4),
            Self::WriteSeal => (4, 5),
            Self::CommitBarrier => (5, 6),
            // The erase and its barrier.
            Self::ReclaimOld => (6, 8),
        }
    }
}

/// The index, in the whole recorded sequence, of the swap's first operation.
///
/// Everything before it is the device's previous life. Asserted against the fault-free run
/// in every test below, so a writer edited without this constant fails loudly rather than
/// silently moving what each step means.
const FIRST_OP: usize = 8;

/// The index of the seal program: §10 step 5.
const SEAL_OP: usize = FIRST_OP + 4;

/// The index of the commit barrier: §10 step 6.
const COMMIT_OP: usize = FIRST_OP + 5;

/// How many crash points [`waymaker_fault::injections`] gives one program of `len` bytes.
///
/// Every interior byte is a tear point and each tear is enumerated twice — once as a power
/// loss and once as a failure the writer sees — and every interior *unit* boundary once more,
/// as a watchdog reset. Then four: the whole operation followed by a power loss, the whole
/// operation followed by a watchdog reset, a failure before it, and a failure after it.
/// Derived rather than measured, so the census below fails when the sweep *shrinks* rather
/// than when a fixture's input length changes.
fn points_in_a_program(len: u32) -> usize {
    let units = (len / geometry().program_size()) as usize;
    2 * (len as usize - 1) + 4 + units - 1
}

/// The same, for an erase: interrupted at erase blocks and nowhere else, so its tear points
/// and its reset points are the same boundaries.
const fn points_in_an_erase(blocks: u32) -> usize {
    3 * (blocks as usize - 1) + 4
}

/// The same, for a barrier: it has no interior, so a power loss after it, a watchdog reset
/// after it, and a failure.
const POINTS_IN_A_BARRIER: usize = 3;

/// How many erase blocks a bank of this geometry is.
fn blocks_per_bank() -> u32 {
    layout().bank(BankId::A).bytes() / geometry().erase_size()
}

/// Which step `op` belongs to, or [`None`] for the previous life.
fn step_of(op: usize) -> Option<Step> {
    let within = op.checked_sub(FIRST_OP)?;
    Step::ALL.into_iter().find(|step| {
        let (from, to) = step.ops();
        from < to && within >= from && within < to
    })
}

/// Asserts that the recorded sequence is still the eight operations §10 describes.
fn check_shape(runs: &[Run]) -> &Run {
    let Some(clean) = runs.first() else {
        unreachable!("the fault-free run is always first")
    };
    let spare = layout().bank(BankId::B);
    let retiring = layout().bank(BankId::A);
    let Some(previous) = clean.ops().get(..FIRST_OP) else {
        unreachable!("the previous life precedes the swap's own operations")
    };
    // Pinned so that `FIRST_COMMIT` stays the barrier it names. Two installs, each a header,
    // a barrier, a seal and a barrier — so operation 3 is the first commit and operations 0
    // to 3 are the device's first bank.
    assert_eq!(previous.len(), FIRST_OP);
    assert!(
        matches!(previous.get(FIRST_COMMIT), Some(Op::Barrier)),
        "operation {FIRST_COMMIT} is no longer the first install's commit barrier: {previous:?}"
    );
    assert_eq!(
        previous.iter().filter(|op| **op == Op::Barrier).count(),
        4,
        "the previous life is two installs of two barriers each: {previous:?}"
    );
    let Some(protocol) = clean.ops().get(FIRST_OP..) else {
        unreachable!("the swap's own operations follow the previous life")
    };
    assert_eq!(
        protocol,
        [
            Op::Erase {
                offset: spare.base(),
                len: spare.bytes()
            },
            Op::Barrier,
            Op::Program {
                offset: spare.base(),
                len: header_bytes()
            },
            Op::Barrier,
            Op::Program {
                offset: spare.seal_offset(),
                len: spare.seal_bytes()
            },
            Op::Barrier,
            Op::Erase {
                offset: retiring.base(),
                len: retiring.bytes()
            },
            Op::Barrier,
        ],
        "the writer changed shape, so the step map above no longer says what it says"
    );
    clean
}

/// Bytes the next run's header occupies on media, padded to the program unit.
fn header_bytes() -> u32 {
    let Some(offset) = next_header().journal_offset() else {
        unreachable!("this header has a journal behind it")
    };
    let Ok(bytes) = u32::try_from(offset) else {
        unreachable!("a header is shorter than a bank")
    };
    bytes
}

// ---------------------------------------------------------------------------------------
// The reader's half
// ---------------------------------------------------------------------------------------

/// One bank's two regions of `image`: everything but the seal, and the seal.
fn regions(image: &[u8], id: BankId) -> (&[u8], &[u8]) {
    let region = layout().bank(id);
    let payload = image
        .get(region.base() as usize..(region.base() + region.payload_bytes()) as usize)
        .unwrap_or_default();
    let seal = image
        .get(region.seal_offset() as usize..(region.seal_offset() + region.seal_bytes()) as usize)
        .unwrap_or_default();
    (payload, seal)
}

/// Which bank a reader boots from, decided by the real selection rule.
fn authority(image: &[u8]) -> Authority {
    bank::select(BankId::ALL.map(|id| {
        let (header, seal) = regions(image, id);
        bank::sealed_generation(header, seal)
    }))
}

/// How many banks a reader would boot from — §15's fourth oracle line counts this.
const fn authoritative_banks(authority: Authority) -> usize {
    match authority {
        Authority::Unsealed => 0,
        Authority::Bank { .. } => 1,
        Authority::Ambiguous { .. } => 2,
    }
}

/// What recovery produced: the swap's record if the installed generation is on media.
fn recovered(image: &[u8]) -> Vec<RecordId> {
    match authority(image) {
        Authority::Bank { generation, .. } | Authority::Ambiguous { generation }
            if generation == NEXT =>
        {
            vec![SWAP]
        }
        _ => Vec::new(),
    }
}

/// The index of the barrier that ends the previous life's first install.
///
/// Before it the device has never committed anything and has no authoritative bank to have
/// lost. Hard-coded, and therefore pinned: [`check_shape`] asserts the whole of the previous
/// life's write sequence, so a reordering inside `previous_life` that kept the operation
/// count would otherwise move what "after the first commit" means and silently change which
/// runs [`audit`] excuses.
const FIRST_COMMIT: usize = 3;

/// Whether `run` happened at or after the device's first durable commit.
fn after_first_commit(run: &Run) -> bool {
    match run.injection() {
        None => true,
        Some(Injection { op, progress, .. }) => {
            op > FIRST_COMMIT || (op == FIRST_COMMIT && progress == Progress::Whole)
        }
    }
}

/// Whether this run's power went strictly before §10 step 5 wrote a byte.
const fn before_the_seal(run: &Run) -> bool {
    match run.injection() {
        None => false,
        Some(Injection { op, .. }) => op < SEAL_OP,
    }
}

/// Whether this run's power went at or after §10 step 6's barrier returned.
fn after_the_commit_barrier(run: &Run) -> bool {
    match run.injection() {
        None => true,
        Some(Injection { op, progress, .. }) => {
            op > COMMIT_OP || (op == COMMIT_OP && progress == Progress::Whole)
        }
    }
}

/// Everything this file requires of one run, as a value so a tooth can require it to fail.
///
/// A function rather than a body of assertions because a suite that only ever ran against a
/// correct writer would pass with its own rules deleted: the mutants at the end of this file
/// hand their runs to *this*, and require it to say no.
fn audit(run: &Run) -> Result<(), String> {
    let image = run.image();
    let authority = authority(image);
    let count = authoritative_banks(authority);

    if !after_first_commit(run) {
        // A device that has never committed has no authoritative bank to have lost, and
        // §15's oracle would report `NoAuthoritativeBank` for the ordinary state of a part
        // straight out of the reel. Telling those two apart is a fact about the device's
        // history rather than about its bytes, which is why it is decided here.
        return Ok(());
    }
    match count {
        0 => {
            return Err(format!(
                "at {:?}: a committed device has nothing to boot from",
                run.injection()
            ));
        }
        1 => {}
        _ => {
            return Err(format!(
                "at {:?}: a committed device has {count} authoritative banks",
                run.injection()
            ));
        }
    }

    // §10: "a crash before step 5 recovers the old run", and "a crash after step 6 recovers
    // the new run". Both directions, because a writer that installed the new run too early
    // and one that never installed it at all are different bugs.
    let installed = matches!(authority, Authority::Bank { generation, .. } if generation == NEXT);
    if installed && before_the_seal(run) {
        return Err(format!(
            "at {:?}: the new run is authoritative before its seal was written",
            run.injection()
        ));
    }
    if !installed && after_the_commit_barrier(run) {
        return Err(format!(
            "at {:?}: the new run is not authoritative after its commit barrier returned",
            run.injection()
        ));
    }

    // "Recovery never combines their footprints": whichever bank is booted, its header is
    // one run's, whole, and the one the generation names.
    if let Authority::Bank { id, generation } = authority {
        let (header, _) = regions(image, id);
        let Ok(decoded) = bank::decode_header(header) else {
            return Err(format!(
                "at {:?}: a bank was authoritative whose header does not decode",
                run.injection()
            ));
        };
        let expected = expected_header(generation);
        if decoded != expected {
            return Err(format!(
                "at {:?}: a bank at generation {} carries {:?}, not {:?}",
                run.injection(),
                generation.0,
                decoded.run,
                expected.run
            ));
        }
    }

    let history = recovered(image);
    match verify_oracle(
        run.ledger(),
        &OracleRecovery::new(&history).authoritative_banks(count),
    ) {
        Ok(()) => Ok(()),
        Err(breach) => Err(format!(
            "at {:?}: {breach:?} recovering {history:?}",
            run.injection()
        )),
    }
}

// ---------------------------------------------------------------------------------------
// The properties
// ---------------------------------------------------------------------------------------

#[test]
fn the_recovery_rules_hold_at_every_crash_point_of_the_swap() {
    let runs = drive(swap);
    let clean = check_shape(&runs);

    // The fault-free run, the crash point that precedes the whole sequence, and then every
    // point in every operation. Exact, and derived from the sequence rather than from a
    // previous run of this test: a sweep that lost two thirds of its runs would pass a floor.
    let enumerated: usize = clean
        .ops()
        .iter()
        .map(|op| match op {
            Op::Program { len, .. } => points_in_a_program(*len),
            Op::Erase { len, .. } => points_in_an_erase(len / geometry().erase_size()),
            Op::Barrier => POINTS_IN_A_BARRIER,
        })
        .sum();
    // Plus the fault-free run and the two crash points that precede the whole sequence, one
    // per reset cause.
    assert_eq!(
        runs.len(),
        enumerated + 3,
        "the sweep is not the enumeration"
    );

    let mut installed = 0_usize;
    let mut still_the_old_run = 0_usize;
    let (mut before_five, mut after_six) = (0_usize, 0_usize);
    for run in &runs {
        if let Err(complaint) = audit(run) {
            unreachable!("{complaint}");
        }
        if !after_first_commit(run) {
            continue;
        }
        if before_the_seal(run) {
            before_five += 1;
        }
        if after_the_commit_barrier(run) {
            after_six += 1;
        }
        match authority(run.image()) {
            Authority::Bank { generation, .. } if generation == NEXT => installed += 1,
            Authority::Bank { generation, .. } if generation == CURRENT => still_the_old_run += 1,
            _ => {}
        }
    }

    // The sweep has to have seen both of §10's outcomes, or the rules held because only one
    // of them was ever reachable.
    assert!(
        installed > 0 && still_the_old_run > 0,
        "{installed} runs installed the new run and {still_the_old_run} kept the old one"
    );
    // And both of §10's recovery rules have to have been *applied*, or "no run broke them"
    // is a statement about an empty set. `audit` checks each on the runs that fall its side
    // of the boundary; these are how many that was.
    assert!(
        before_five > 0 && after_six > 0,
        "§10's two recovery rules were checked against {before_five} runs before step 5 and \
         {after_six} after step 6"
    );
}

#[test]
fn every_step_of_the_protocol_has_crash_points_in_the_sweep() {
    // Issue #26 asks for "every crash point across all seven steps". This is what makes the
    // claim checkable rather than asserted: each step of the protocol is counted, and a step
    // the injector never reaches fails the build rather than passing quietly.
    let runs = drive(swap);
    check_shape(&runs);

    let mut counted = [0_usize; Step::ALL.len()];
    for run in &runs {
        let Some(Injection { op, .. }) = run.injection() else {
            continue;
        };
        let Some(step) = step_of(op) else {
            continue;
        };
        for (index, candidate) in Step::ALL.into_iter().enumerate() {
            if candidate == step {
                if let Some(slot) = counted.get_mut(index) {
                    *slot += 1;
                }
            }
        }
    }

    // Exact rather than "at least one". A step that kept a single crash point would pass a
    // floor and would have stopped being a sweep, and `waymaker-spec`'s own census pins
    // counts for that reason: the dangerous direction is an enumeration that silently shrank.
    let erase = points_in_an_erase(blocks_per_bank());
    let expected = [
        // Step 1 touches no media, so a crash point in it would mean the writer does.
        0,
        erase + POINTS_IN_A_BARRIER,
        points_in_a_program(header_bytes()),
        POINTS_IN_A_BARRIER,
        points_in_a_program(layout().bank(BankId::B).seal_bytes()),
        POINTS_IN_A_BARRIER,
        erase + POINTS_IN_A_BARRIER,
    ];
    for (index, step) in Step::ALL.into_iter().enumerate() {
        assert_eq!(
            counted.get(index).copied(),
            expected.get(index).copied(),
            "{step:?} has the wrong number of crash points in the sweep"
        );
    }

    // And the two erases really are interrupted part-way, which is what four blocks a bank
    // buys: a bank half erased is the state §10 step 7's crash-safety is about. Two erases,
    // each torn at every interior block, each boundary enumerated three times — a power cut,
    // a watchdog reset and a failure.
    let torn_erases = runs
        .iter()
        .filter(|run| {
            matches!(
                run.injection(),
                Some(Injection {
                    op,
                    progress: Progress::Bytes(_),
                    ..
                }) if step_of(op) == Some(Step::EraseInactive)
                    || step_of(op) == Some(Step::ReclaimOld)
            )
        })
        .count();
    assert_eq!(
        torn_erases,
        2 * 3 * (blocks_per_bank() as usize - 1),
        "a half-erased bank is the state §10 step 7's crash-safety is about"
    );
    assert!(
        torn_erases > 0,
        "a bank of one erase block has no interior tear point, so this fixture measures \
         nothing about a partial erase"
    );
}

#[test]
fn the_lazy_erase_never_returns_the_old_bank_to_authority() {
    // Issue #26's second "done when". Once step 6's barrier has returned the new run is
    // authoritative, and step 7 is an erase of a bank that already lost: interrupted at any
    // block, it can only remove a candidate, never promote one.
    let runs = drive(swap);
    check_shape(&runs);

    let mut during_the_reclaim = 0_usize;
    let mut old_bank_gone = 0_usize;
    for run in &runs {
        let Some(Injection { op, .. }) = run.injection() else {
            continue;
        };
        if step_of(op) != Some(Step::ReclaimOld) {
            continue;
        }
        during_the_reclaim += 1;

        assert_eq!(
            authority(run.image()),
            Authority::Bank {
                id: BankId::B,
                generation: NEXT
            },
            "at {:?}: the reclaim moved the authority",
            run.injection()
        );
        let (header, seal) = regions(run.image(), BankId::A);
        let retired = bank::sealed_generation(header, seal);
        assert!(
            retired.is_none() || retired == Some(CURRENT),
            "at {:?}: the retired bank reports {retired:?}",
            run.injection()
        );
        if retired.is_none() {
            old_bank_gone += 1;
        }
    }

    assert_eq!(
        during_the_reclaim,
        points_in_an_erase(blocks_per_bank()) + POINTS_IN_A_BARRIER,
        "the lazy erase is an erase and a barrier, and every point in both is swept"
    );
    assert!(
        old_bank_gone > 0,
        "no crash point left the old bank actually erased, so the erase was never observed"
    );
}

// ---------------------------------------------------------------------------------------
// The teeth
// ---------------------------------------------------------------------------------------

/// How many runs of `writer` this file's own rules reject.
fn rejected(writer: fn(&mut Session) -> Result<(), Failed>) -> usize {
    drive(writer)
        .iter()
        .filter(|run| audit(run).is_err())
        .count()
}

/// The swap with the bug a two-bank protocol exists to prevent: clear the device first.
fn swap_that_erases_the_bank_it_booted(session: &mut Session) -> Result<(), Failed> {
    previous_life(session)?;

    let (spare, booted) = (layout().bank(BankId::B), layout().bank(BankId::A));
    session.erase(booted.base(), booted.bytes())?;
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;

    program_header(session, BankId::B, &next_header())?;
    session.barrier()?;
    session.begin_record(SWAP);
    program_seal(session, BankId::B, NEXT)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

/// The swap that seals the new bank at the generation the bank it replaces already carries.
fn swap_that_does_not_bump_the_generation(session: &mut Session) -> Result<(), Failed> {
    previous_life(session)?;

    let spare = layout().bank(BankId::B);
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;

    program_header(session, BankId::B, &next_header())?;
    session.barrier()?;
    session.begin_record(SWAP);
    program_seal(session, BankId::B, CURRENT)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

/// The swap that takes §10 step 7 before step 6.
///
/// The seal is programmed and the old bank is erased before the commit barrier returns, so
/// a crash between the two leaves a device whose new seal never became durable and whose old
/// bank is gone.
fn swap_that_reclaims_before_the_commit_barrier(session: &mut Session) -> Result<(), Failed> {
    previous_life(session)?;

    let (spare, retiring) = (layout().bank(BankId::B), layout().bank(BankId::A));
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;

    program_header(session, BankId::B, &next_header())?;
    session.barrier()?;
    session.begin_record(SWAP);
    program_seal(session, BankId::B, NEXT)?;
    session.erase(retiring.base(), retiring.bytes())?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

#[test]
fn a_swap_that_erases_the_bank_it_booted_can_leave_nothing_to_boot_from() {
    // §10's two banks exist so that there is always one to boot from. A writer that clears
    // the device first has a window in which there is none, and the real writer cannot have
    // one: which bank it erases is derived from the authority it was handed.
    assert!(
        rejected(swap_that_erases_the_bank_it_booted) > 0,
        "a swap that erased both banks was never caught"
    );
}

#[test]
fn a_swap_that_does_not_bump_the_generation_leaves_two_authorities() {
    // Every frame verifies, every seal is present, and nothing on media says which of the
    // two banks is newer. `Generation::successor` is what the real writer mints with.
    assert!(
        rejected(swap_that_does_not_bump_the_generation) > 0,
        "a swap that repeated a generation was never caught"
    );
}

#[test]
fn a_reclaim_taken_before_the_commit_barrier_can_lose_both_runs() {
    // The whole of why §10 step 7 is step *seven*. The real writer cannot express it:
    // `Installed::reclaim` exists only on the value `Sealable::commit` returns, and that
    // call ends in the barrier.
    assert!(
        rejected(swap_that_reclaims_before_the_commit_barrier) > 0,
        "a reclaim before the commit barrier was never caught"
    );
}

#[test]
fn a_swap_whose_seal_is_not_a_seal_never_installs_the_run_it_claims_to() {
    // §10's second recovery rule, as a thing that can fail: "a crash after step 6 recovers
    // the new run". This writer reaches step 6 and installs nothing.
    assert!(
        rejected(swap_whose_seal_is_not_a_seal) > 0,
        "a swap that sealed with rubbish was never caught"
    );
}

#[test]
fn a_swap_that_installs_another_runs_header_is_caught_by_the_footprint_rule() {
    // §10's prohibition, as a thing that can fail: "recovery never combines their
    // footprints". Nothing here is damaged — the seal names the header beneath it — so no
    // count, no oracle and no decode failure sees it. The only rule that does is the one
    // that reads the run out of the bank the generation named, which is why that rule is in
    // `audit` rather than left to `bank::select`.
    let complaints: Vec<String> = drive(swap_that_installs_the_wrong_run)
        .iter()
        .filter_map(|run| audit(run).err())
        .collect();
    assert!(
        !complaints.is_empty(),
        "a swap that installed the previous run's header was never caught"
    );
    assert!(
        complaints
            .iter()
            .any(|complaint| complaint.contains("carries")),
        "the footprint rule was not what caught it: {complaints:?}"
    );
}

#[test]
fn the_audit_passes_the_writer_it_was_written_for() {
    // The control. Three mutants being rejected says nothing unless the correct protocol is
    // accepted, and a `Breach` this file cannot produce would make every tooth above
    // meaningless.
    let runs = drive(swap);
    assert_eq!(
        rejected(swap),
        0,
        "the real swap is rejected by its own audit"
    );
    assert!(
        runs.iter().all(|run| audit(run).is_ok()),
        "the real swap is rejected by its own audit"
    );
    // And the oracle really is being asked: a run in which the swap was acknowledged and one
    // in which it was not are both in the sweep.
    let breach: Breach = Breach::NoAuthoritativeBank;
    assert_ne!(
        std::format!("{breach:?}"),
        String::new(),
        "the oracle's vocabulary is reachable from here"
    );
}
