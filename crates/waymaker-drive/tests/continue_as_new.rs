//! Issue [#110](https://github.com/madmax983/waymaker/issues/110)'s `continue_as_new` join:
//! a [`Driver::at_bank`] performs design document §10's real swap, over real media.
//!
//! `crates/waymaker-drive/tests/ota.rs`'s
//! `continue_as_new_is_refused_by_a_driver_that_cannot_name_a_bank` is the other half —
//! [`Driver::new`], pointed at a fixed region, still refuses. This file is the driver that
//! can.

use waymaker_core::timer::TimerSpec;
use waymaker_core::version::VersionRange;
use waymaker_core::{ActivityKind, Outcome, RunId};
use waymaker_drive::{
    Boundary, DriveError, Driver, Identity, Progress, Scratch, Suspended, Workflow,
};
use waymaker_fault::Device;
use waymaker_flash::bank::{self, BankHeader, BankId, BankLayout, Generation};
use waymaker_flash::capacity::{Bounds, Reserve};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::{JournalRegion, RecoveryError};
use waymaker_flash::storage::{Geometry, StableStorage};

const WORKFLOW_KIND: u16 = 0x00A1;
const WORKFLOW_VERSION: u16 = 1;
const INPUT_SCHEMA: u16 = 1;
const RUN: RunId = RunId(0x0000_0000_0000_002A);
const FIRST_INPUT: &[u8] = b"the-run-in-progress";
const NEXT_INPUT: &[u8] = b"what-continue-as-new-asked-for";

const BOUNDS: Bounds = Bounds {
    run_input_bytes: 64,
    effect_result_bytes: 16,
    terminal_bytes: 16,
};

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(8192, 4096, 8, 1) else {
        unreachable!("8192/4096/8/1 is a legal geometry of two whole erase blocks")
    };
    geometry
}

fn layout() -> BankLayout {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("two erase blocks are two banks")
    };
    layout
}

fn align() -> ProgramAlign {
    layout().align()
}

fn reserve() -> Reserve {
    let Ok(reserve) = Reserve::for_layout(BOUNDS, layout()) else {
        unreachable!("these bounds fit this layout")
    };
    reserve
}

fn first_header() -> BankHeader<'static> {
    BankHeader {
        run: RUN,
        align: align(),
        workflow_kind: WORKFLOW_KIND,
        workflow_version: WORKFLOW_VERSION,
        input_schema: INPUT_SCHEMA,
        input: FIRST_INPUT,
    }
}

/// A geometry like [`geometry`]'s, but with a chosen read unit. [`geometry`] always uses
/// one, which is why the seal-alignment bug Codex found needed a fixture of its own: every
/// other test in this file reads a bank's seal on a device where a misaligned read cannot
/// be told apart from an aligned one.
fn geometry_with_read_size(read_size: u32) -> Geometry {
    let Ok(geometry) = Geometry::new(8192, 4096, 8, read_size) else {
        unreachable!("8192/4096/8/{read_size} is a legal geometry of two whole erase blocks")
    };
    geometry
}

fn layout_with_read_size(read_size: u32) -> BankLayout {
    let Ok(layout) = BankLayout::new(geometry_with_read_size(read_size)) else {
        unreachable!("two erase blocks are two banks")
    };
    layout
}

fn reserve_for(layout: BankLayout) -> Reserve {
    let Ok(reserve) = Reserve::for_layout(BOUNDS, layout) else {
        unreachable!("these bounds fit this layout")
    };
    reserve
}

/// [`install`], against a caller-chosen layout rather than [`layout`]'s own.
fn install_on(
    device: &mut Device,
    layout: BankLayout,
    id: BankId,
    generation: Generation,
    header: &BankHeader<'_>,
) {
    let region = layout.bank(id);
    let mut staging = [0_u8; 512];
    let Ok(header_len) = bank::encode_header(header, &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    let Some(header_frame) = staging.get(..header_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    let (Ok(()), Ok(())) = (
        device.program(region.base(), header_frame),
        device.barrier(),
    ) else {
        unreachable!("a bank header is a legal program")
    };
    let Ok(seal) = bank::seal_for(header_frame, generation) else {
        unreachable!("a header frame can be sealed")
    };
    let mut seal_bytes = [0_u8; 64];
    let Ok(seal_len) = bank::encode_seal(&seal, layout.align(), &mut seal_bytes) else {
        unreachable!("a seal fits its own region")
    };
    let Some(sealed) = seal_bytes.get(..seal_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    let (Ok(()), Ok(())) = (
        device.program(region.seal_offset(), sealed),
        device.barrier(),
    ) else {
        unreachable!("a generation seal is a legal program")
    };
}

/// Installs a bank the way a previous life left it: header, barrier, seal, barrier.
///
/// Not the swap under test — this is the device's history, and a test that built its
/// starting state with the writer it is testing would be a test of nothing. Ported from
/// `crates/waymaker-flash/tests/swap.rs`'s helper of the same shape.
fn install(device: &mut Device, id: BankId, generation: Generation, header: &BankHeader<'_>) {
    let region = layout().bank(id);
    let mut staging = [0_u8; 512];
    let Ok(header_len) = bank::encode_header(header, &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    let Some(header_frame) = staging.get(..header_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    let (Ok(()), Ok(())) = (
        device.program(region.base(), header_frame),
        device.barrier(),
    ) else {
        unreachable!("a bank header is a legal program")
    };
    let Ok(seal) = bank::seal_for(header_frame, generation) else {
        unreachable!("a header frame can be sealed")
    };
    let mut seal_bytes = [0_u8; 64];
    let Ok(seal_len) = bank::encode_seal(&seal, align(), &mut seal_bytes) else {
        unreachable!("a seal fits its own region")
    };
    let Some(sealed) = seal_bytes.get(..seal_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    let (Ok(()), Ok(())) = (
        device.program(region.seal_offset(), sealed),
        device.barrier(),
    ) else {
        unreachable!("a generation seal is a legal program")
    };
}

/// Writes a bank's header and nothing else, leaving its seal region erased.
///
/// The shape a device left mid-swap — staged, never sealed — rather than the finished
/// article [`install`] and [`install_on`] both write. Used to prove that an unsealed bank's
/// header, whatever it declares, can never cost a boot anything: [`bank::sealed_generation`]
/// already says such a bank is not a candidate at any generation, and `read_bank` has to
/// establish that *before* it ever holds the header's own declared length against `page`.
fn install_header_only(
    device: &mut Device,
    layout: BankLayout,
    id: BankId,
    header: &BankHeader<'_>,
) {
    let region = layout.bank(id);
    let mut staging = [0_u8; 512];
    let Ok(header_len) = bank::encode_header(header, &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    let Some(header_frame) = staging.get(..header_len) else {
        unreachable!("the encoder wrote inside the buffer it was given")
    };
    let (Ok(()), Ok(())) = (
        device.program(region.base(), header_frame),
        device.barrier(),
    ) else {
        unreachable!("a bank header is a legal program")
    };
}

/// A device booted from bank A, generation zero, and nothing on bank B.
fn booted() -> Device {
    let mut device = Device::new(geometry());
    install(&mut device, BankId::A, Generation::FIRST, &first_header());
    device
}

/// The header a bank on media carries, decoded the way a cold boot has to.
fn header_on(device: &mut Device, id: BankId) -> Option<(RunId, u16, u16, Vec<u8>)> {
    let region = layout().bank(id);
    let mut page = [0_u8; 512];
    let Ok(()) = device.read(region.base(), &mut page) else {
        unreachable!("a bank's header is inside the device")
    };
    let decoded = bank::decode_header(&page).ok()?;
    Some((
        decoded.run,
        decoded.workflow_version,
        decoded.input_schema,
        decoded.input.to_vec(),
    ))
}

/// A workflow that immediately retires itself over [`NEXT_INPUT`].
struct ContinueOnce;

impl Workflow for ContinueOnce {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            versions: VersionRange::exact(WORKFLOW_VERSION),
            input: FIRST_INPUT,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        Err(boundary.continue_as_new(NEXT_INPUT))
    }
}

/// A workflow over whatever input it was built with. Used for the run `continue_as_new`
/// installs, which this file never asks to do anything but exist.
struct JustStarted<'a> {
    input: &'a [u8],
}

impl Workflow for JustStarted<'_> {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            versions: VersionRange::exact(WORKFLOW_VERSION),
            input: self.input,
        }
    }

    fn run(&mut self, _boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        Ok(Outcome::Completed(&[]))
    }
}

/// A workflow that leaves an effect outstanding and then asks to migrate anyway.
///
/// §07's schedule record is already durable in bank A's journal when `continue_as_new`
/// runs — the effect is committed, not merely asked for — so a swap that went ahead would
/// forfeit an identity a crash never took.
struct ScheduleThenContinue;

impl Workflow for ScheduleThenContinue {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            versions: VersionRange::exact(WORKFLOW_VERSION),
            input: FIRST_INPUT,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        let _ = boundary.schedule(ActivityKind(1), b"an-effect-in-flight")?;
        Err(boundary.continue_as_new(NEXT_INPUT))
    }
}

/// Wider than [`BOUNDS`]'s `run_input_bytes`.
const OVERSIZED_INPUT: [u8; 65] = [b'x'; 65];

/// A workflow that asks `continue_as_new` for more than the run's own bound allows.
struct ContinueWithOversizedInput;

impl Workflow for ContinueWithOversizedInput {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            versions: VersionRange::exact(WORKFLOW_VERSION),
            input: FIRST_INPUT,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        Err(boundary.continue_as_new(&OVERSIZED_INPUT))
    }
}

/// A workflow admitting both [`WORKFLOW_VERSION`] and its successor — an image that still
/// replays the version the retiring bank recorded, standing in for a firmware upgrade
/// arriving mid-run.
struct UpgradingContinueOnce;

impl Workflow for UpgradingContinueOnce {
    fn identity(&self) -> Identity<'_> {
        let Some(versions) = VersionRange::new(WORKFLOW_VERSION, WORKFLOW_VERSION + 1) else {
            unreachable!("a lower bound below the current version is a legal range")
        };
        Identity {
            kind: WORKFLOW_KIND,
            versions,
            input: FIRST_INPUT,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        Err(boundary.continue_as_new(NEXT_INPUT))
    }
}

/// [`JustStarted`] over [`NEXT_INPUT`], admitting only the version after
/// [`WORKFLOW_VERSION`] — an image that has since dropped the retired version entirely.
struct JustStartedAfterUpgrade;

impl Workflow for JustStartedAfterUpgrade {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            versions: VersionRange::exact(WORKFLOW_VERSION + 1),
            input: NEXT_INPUT,
        }
    }

    fn run(&mut self, _boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        Ok(Outcome::Completed(&[]))
    }
}

/// A workflow over [`FIRST_INPUT`], with a version range this test picks, that waits for a
/// persistent deadline nothing here ever reaches — so the run stays suspended, recorded but
/// unfinished, across as many boots as a test wants to drive it through.
struct WaitingAtVersion {
    versions: VersionRange,
}

impl WaitingAtVersion {
    fn new(oldest: u16, current: u16) -> Self {
        let Some(versions) = VersionRange::new(oldest, current) else {
            unreachable!("oldest <= current is a legal range")
        };
        Self { versions }
    }
}

impl Workflow for WaitingAtVersion {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            versions: self.versions,
            input: FIRST_INPUT,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        boundary.wait(TimerSpec::AtPersistentTime { instant: u64::MAX })?;
        Ok(Outcome::Completed(&[]))
    }
}

const fn scratch<'a>(page: &'a mut [u8; 512], result: &'a mut [u8; 16]) -> Scratch<'a> {
    Scratch { page, result }
}

#[test]
fn a_driver_at_a_bank_performs_a_real_swap_and_installs_the_next_run_in_the_other_bank() {
    let mut device = booted();
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        scratch(&mut page, &mut result),
    );

    let Ok(Progress::Migrated { run }) = progress else {
        unreachable!("a bank-pointed driver over a sealed bank performs the swap: {progress:?}")
    };
    assert_eq!(
        run,
        RunId(RUN.0 + 1),
        "the next run id is this device's first successor"
    );

    // This driver performs all seven steps in one call, reclaim included: the retiring
    // bank, A, is erased rather than left holding a stale sealed run.
    assert_eq!(header_on(&mut device, BankId::A), None);

    // The installed bank, B, carries the new run and the input `continue_as_new` was asked
    // for — read back the way a cold boot has to, not assumed from the call that wrote it.
    let (b_run, b_version, b_schema, b_input) =
        header_on(&mut device, BankId::B).expect("the swap installed bank B");
    assert_eq!(b_run, run);
    assert_eq!(b_version, WORKFLOW_VERSION);
    assert_eq!(b_schema, INPUT_SCHEMA);
    assert_eq!(b_input, NEXT_INPUT);

    // And the seal that made B authoritative names the generation after the one this
    // device booted from — not merely *a* valid seal, which an understated generation
    // would still decode as.
    assert_eq!(
        generation_on(&mut device, BankId::B),
        Generation::FIRST.successor()
    );
}

#[test]
fn continue_as_new_stamps_the_new_bank_with_the_images_current_version_not_the_retired_ones() {
    // Codex found this. `bank.workflow_version` is the *retiring* bank's own recorded
    // version, read back from its header rather than from this image — stamping the next
    // bank with it silently downgrades a run this same call is meant to carry forward. A v2
    // image continuing a v1 run has to record v2, the version `begin` would also choose for
    // a freshly erased journal, not the v1 the old header happened to carry.
    let mut device = booted();
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut UpgradingContinueOnce,
        scratch(&mut page, &mut result),
    );
    assert!(
        matches!(progress, Ok(Progress::Migrated { .. })),
        "{progress:?}"
    );

    let (_, b_version, ..) = header_on(&mut device, BankId::B).expect("the swap installed bank B");
    assert_eq!(b_version, WORKFLOW_VERSION + 1);

    // The sharper proof: an image that has since dropped v1 entirely still replays what the
    // swap installed, which it could not if the header had been stamped with the retired
    // v1 rather than the v2 this call was actually made under.
    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut JustStartedAfterUpgrade,
        scratch(&mut page, &mut result),
    );
    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: waymaker_drive::Conclusion::Completed,
                ..
            })
        ),
        "{progress:?}"
    );
}

#[test]
fn a_later_image_that_dropped_the_headers_own_version_still_resumes_the_runs_recorded_one() {
    // Codex found this. `booted()`'s bank header names `WORKFLOW_VERSION` and never changes
    // again — it is a fact about whatever wrote the swap, not about the run. A v2 image that
    // still admits v1 boots this bank for the first time and records its *own* current
    // version into `RunStarted`, per `begin`'s existing choice for an erased journal. Every
    // boot after that has a real recorded version to read, and `verify_header_identity` must
    // not go on checking the header's own stale one once it does — a v3 image that has since
    // dropped v1 entirely, but still admits the v2 this run is actually recorded at, has to
    // resume it rather than being refused over a field nothing here depends on any more.
    let mut device = booted();
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    // v2, still admitting the header's own v1, boots this bank for the first time.
    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut WaitingAtVersion::new(WORKFLOW_VERSION, WORKFLOW_VERSION + 1),
        scratch(&mut page, &mut result),
    );
    assert!(
        matches!(progress, Ok(Progress::WaitingUntil { .. })),
        "{progress:?}"
    );

    // v3, admitting only the version the journal actually recorded and nothing below it.
    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut WaitingAtVersion::new(WORKFLOW_VERSION + 1, WORKFLOW_VERSION + 1),
        scratch(&mut page, &mut result),
    );
    assert!(
        matches!(progress, Ok(Progress::WaitingUntil { .. })),
        "a v3 image admitting only the journal's own recorded version must still resume \
         this run rather than being refused over the header's stale v1: {progress:?}"
    );
}

#[test]
fn the_next_boot_of_the_same_layout_replays_the_bank_the_swap_installed() {
    let mut device = booted();
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];
    let Ok(Progress::Migrated { run: next_run }) = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        scratch(&mut page, &mut result),
    ) else {
        unreachable!("the first boot performs the swap")
    };

    // A fresh driver, built exactly as the first one was: nothing about which bank is
    // authoritative was carried over from the first boot.
    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut JustStarted { input: NEXT_INPUT },
        scratch(&mut page, &mut result),
    );

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: waymaker_drive::Conclusion::Completed,
                ..
            })
        ),
        "{progress:?}"
    );
    let _ = next_run;

    // The proof that boot 2 replayed the bank the swap installed, rather than a coincidence
    // of layout: boot 2 wrote a real `RunStarted` into bank B over `NEXT_INPUT`, so a third
    // boot declaring a *different* input over the same layout can only be refused by reading
    // that same record back. A stale fallback to bank A's own `RunStarted` — over
    // `FIRST_INPUT` — would refuse this boot too, for the wrong reason, so the input below is
    // chosen to differ from both.
    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut JustStarted {
            input: b"neither-run-ever-declared-this",
        },
        scratch(&mut page, &mut result),
    );
    assert_eq!(progress, Err(DriveError::NotThisWorkflow));
}

#[test]
fn a_committed_effect_is_not_forfeited_by_a_live_continue_as_new() {
    let mut device = booted();
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ScheduleThenContinue,
        scratch(&mut page, &mut result),
    );

    assert_eq!(progress, Err(DriveError::EffectOutstanding));

    // The swap never started: bank A is still the only sealed bank, carrying the run it
    // always did, and bank B holds nothing a swap would have installed.
    let (a_run, ..) = header_on(&mut device, BankId::A).expect("bank A is untouched");
    assert_eq!(a_run, RUN);
    assert_eq!(header_on(&mut device, BankId::B), None);
}

#[test]
fn continue_as_new_refuses_when_this_replay_never_consumed_committed_history() {
    // Codex found this. A previous boot's committed schedule record — durable, and never
    // resolved — sits unread on this replay, because `ContinueOnce`'s own logic asks for
    // nothing before migrating. Swapping over it would reclaim the only copy of the
    // history that proves this image diverges from whatever recorded it: exactly the
    // nondeterminism `nothing_follows` already refuses at a normal ending, and it has to
    // run before the swap here rather than after, since `swap_in` is what would destroy
    // the evidence.
    let mut device = booted();
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    // A previous boot leaves a durable, unresolved schedule record behind.
    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ScheduleThenContinue,
        scratch(&mut page, &mut result),
    );
    assert_eq!(progress, Err(DriveError::EffectOutstanding));

    // This replay's own logic never asks for it before trying to migrate.
    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        scratch(&mut page, &mut result),
    );
    assert_eq!(progress, Err(DriveError::HistoryContinues));

    // Refused before anything moved: bank A is untouched and bank B still empty.
    let (a_run, ..) = header_on(&mut device, BankId::A).expect("bank A is untouched");
    assert_eq!(a_run, RUN);
    assert_eq!(header_on(&mut device, BankId::B), None);
}

#[test]
fn an_oversized_next_run_input_is_refused_before_the_device_is_touched() {
    let mut device = booted();
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueWithOversizedInput,
        scratch(&mut page, &mut result),
    );

    assert_eq!(
        progress,
        Err(DriveError::NextRunInputTooLong {
            bytes: OVERSIZED_INPUT.len(),
            bound: BOUNDS.run_input_bytes,
        })
    );

    // Refused before the device was asked for anything: bank A still boots the run it
    // always did, and bank B was never erased.
    let (a_run, ..) = header_on(&mut device, BankId::A).expect("bank A is untouched");
    assert_eq!(a_run, RUN);
    assert_eq!(header_on(&mut device, BankId::B), None);
}

#[test]
fn a_run_id_at_the_ceiling_is_refused_rather_than_reissued() {
    let mut device = Device::new(geometry());
    install(
        &mut device,
        BankId::A,
        Generation::FIRST,
        &BankHeader {
            run: RunId(u64::MAX),
            ..first_header()
        },
    );
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        scratch(&mut page, &mut result),
    );

    assert_eq!(progress, Err(DriveError::RunIdExhausted));
    // No successor id exists to install, so nothing was written: bank B stays empty.
    assert_eq!(header_on(&mut device, BankId::B), None);
}

#[test]
fn a_driver_at_a_bank_with_no_sealed_bank_refuses_to_boot() {
    let mut device = Device::new(geometry());
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        scratch(&mut page, &mut result),
    );

    assert_eq!(progress, Err(DriveError::NoAuthoritativeBank));
}

#[test]
fn a_driver_at_a_bank_with_both_banks_sealed_at_one_generation_refuses_ambiguously() {
    let mut device = Device::new(geometry());
    install(&mut device, BankId::A, Generation::FIRST, &first_header());
    install(
        &mut device,
        BankId::B,
        Generation::FIRST,
        &BankHeader {
            run: RunId(0x9999_9999_9999_9999),
            ..first_header()
        },
    );
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        scratch(&mut page, &mut result),
    );

    assert_eq!(progress, Err(DriveError::AmbiguousAuthority));
}

#[test]
fn a_stale_bank_is_never_a_candidate_once_a_later_generation_exists() {
    // `select`'s own rule, read through a real boot: bank B two generations behind bank A
    // is not weighed against it at all, so a boot from a device with a very old spare bank
    // still starts the run bank A names.
    let mut device = Device::new(geometry());
    install(
        &mut device,
        BankId::B,
        Generation(0),
        &BankHeader {
            run: RunId(0x1111_1111_1111_1111),
            input: b"stale",
            ..first_header()
        },
    );
    install(&mut device, BankId::A, Generation(9), &first_header());
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        scratch(&mut page, &mut result),
    );

    let Ok(Progress::Migrated { run }) = progress else {
        unreachable!("bank A, the higher generation, is what this boot starts from: {progress:?}")
    };
    assert_eq!(run, RunId(RUN.0 + 1));
    // And the swap installed into B — the bank this device did *not* boot — recycling the
    // stale bank rather than the one just replayed.
    let (b_run, ..) = header_on(&mut device, BankId::B).expect("the swap installed bank B");
    assert_eq!(b_run, run);
}

/// `RunId::successor()` itself, directly — the unit this crate's own `a_run_id_at_the_ceiling_is_refused_rather_than_reissued`
/// exercises through a real boot, above.
#[test]
fn run_id_successor_refuses_only_at_the_ceiling() {
    assert_eq!(RunId(u64::MAX).successor(), None);
    assert_eq!(RunId(0).successor(), Some(RunId(1)));
}

/// Bank A's journal region, as a cold boot pointed at a fixed region would have to be
/// handed it.
fn first_region() -> JournalRegion {
    let Ok(region) = JournalRegion::of(layout(), BankId::A, &first_header()) else {
        unreachable!("bank A holds a journal behind its header")
    };
    region
}

/// The generation a bank's seal names, read back the way a cold boot has to: header and
/// seal, decoded together rather than assumed from the call that wrote them.
fn generation_on(device: &mut Device, id: BankId) -> Option<Generation> {
    let region = layout().bank(id);
    let mut header = [0_u8; 512];
    let Ok(()) = device.read(region.base(), &mut header) else {
        unreachable!("a bank's header is inside the device")
    };
    let mut seal = [0_u8; bank::SEAL_BYTES];
    let Ok(()) = device.read(region.seal_offset(), &mut seal) else {
        unreachable!("a bank's seal is inside the device")
    };
    bank::sealed_generation(&header, &seal)
}

#[test]
fn a_driver_pointed_at_a_region_still_refuses_to_swap() {
    // The other half of the join: this file's driver performs a real swap, and
    // `Driver::new` genuinely still cannot — `ota.rs`'s
    // `continue_as_new_is_refused_by_a_driver_that_cannot_name_a_bank` is the same claim
    // over the reference OTA workflow. This is the same claim, minimal.
    let mut device = booted();
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::new(first_region(), RUN, reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        scratch(&mut page, &mut result),
    );

    assert_eq!(progress, Err(DriveError::ContinueUnsupported));
}

/// Fails every erase aimed at one bank; every other call reaches the real device unchanged.
///
/// Stands in for §10 step 7's own erase failing — `Installed::reclaim`'s documented
/// postcondition is that the new run stays authoritative either way, and this is what lets
/// a test hold `swap_in` to that promise rather than to the media this device happens to
/// model.
struct EraseFails<'a> {
    device: &'a mut Device,
    failing: BankId,
}

impl StableStorage for EraseFails<'_> {
    type Error = <Device as StableStorage>::Error;

    fn geometry(&self) -> Geometry {
        self.device.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.device.read(offset, dst)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        self.device.program(offset, src)
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        let region = layout().bank(self.failing);
        if offset >= region.base() && offset < region.base() + region.bytes() {
            return Err(waymaker_fault::FaultError::PowerLoss);
        }
        self.device.erase(offset, len)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.device.barrier()
    }
}

#[test]
fn a_failed_reclaim_does_not_turn_a_successful_migration_into_a_failure() {
    // Codex found this. `commit()` is the swap's own point of no return: once it returns,
    // bank B is durably sealed and authoritative, and reclaiming bank A is cleanup rather
    // than part of the migration — `Installed::reclaim`'s own documentation says the device
    // has one authoritative bank, the new one, whether or not the erase lands. Reporting a
    // failed reclaim as a failed `continue_as_new` would tell a caller the migration it just
    // performed had not happened, when a fresh boot of this same layout would show that it
    // had.
    let mut device = booted();
    let mut storage = EraseFails {
        device: &mut device,
        failing: BankId::A,
    };
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut storage,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        scratch(&mut page, &mut result),
    );

    let Ok(Progress::Migrated { run }) = progress else {
        unreachable!(
            "a failed reclaim of the retiring bank must not read back as a failed swap: \
             {progress:?}"
        )
    };
    assert_eq!(run, RunId(RUN.0 + 1));

    // Bank A never actually erased — the injected failure is real, not merely reported —
    // and bank B is authoritative anyway: the next boot of this layout replays it.
    assert!(header_on(&mut device, BankId::A).is_some());
    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut JustStarted { input: NEXT_INPUT },
        scratch(&mut page, &mut result),
    );
    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: waymaker_drive::Conclusion::Completed,
                ..
            })
        ),
        "{progress:?}"
    );
}

#[test]
fn a_seal_read_is_aligned_to_the_devices_own_read_unit_not_a_bare_constant() {
    // Codex found this. `bank::SEAL_BYTES` is 12, which is not a multiple of every real
    // geometry's read unit — a device reading in units of 8, for instance — and a
    // conforming `StableStorage` refuses a read whose length is not a multiple of it,
    // before this driver ever gets to compare generations. `waymaker_fault::Device` is
    // exactly such a conforming adapter, so a `Driver::at_bank` boot over one with a read
    // unit `SEAL_BYTES` does not divide is the regression: every boot used to refuse
    // outright, over a read this driver made rather than one the device was asked to do.
    let layout = layout_with_read_size(8);
    let mut device = Device::new(geometry_with_read_size(8));
    install_on(
        &mut device,
        layout,
        BankId::A,
        Generation::FIRST,
        &BankHeader {
            align: layout.align(),
            ..first_header()
        },
    );
    let mut page = [0_u8; 512];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout, reserve_for(layout)).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut JustStarted { input: FIRST_INPUT },
        scratch(&mut page, &mut result),
    );

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: waymaker_drive::Conclusion::Completed,
                ..
            })
        ),
        "{progress:?}"
    );
}

#[test]
fn an_undersized_page_refuses_rather_than_reviving_a_retired_bank() {
    // Codex found this. Bank B is authoritative — its generation is higher — but a swap
    // once installed a longer input into it than bank A ever carried, so its header is the
    // wider of the two. A page too small to read bank B's header back has to refuse
    // outright rather than fall back to bank A: bank A is a real, validly sealed candidate
    // too, just the *retired* one, and nothing below `select_bank` can tell "damaged" from
    // "did not fit" unless `read_bank` says which.
    let mut device = Device::new(geometry());
    let long_input = &[b'x'; 60][..];
    let long_header = BankHeader {
        input: long_input,
        ..first_header()
    };
    install(&mut device, BankId::A, Generation::FIRST, &first_header());
    let Some(later) = Generation::FIRST.successor() else {
        unreachable!("FIRST has a successor")
    };
    install(&mut device, BankId::B, later, &long_header);

    let mut staging = [0_u8; 512];
    let Ok(short_len) = bank::encode_header(&first_header(), &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    let Ok(long_len) = bank::encode_header(&long_header, &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    // Room for bank A's header and the seal, deliberately short of bank B's header.
    let mut page = vec![0_u8; short_len + bank::SEAL_BYTES + 8];
    assert!(
        page.len() < long_len + bank::SEAL_BYTES,
        "the fixture needs bank B's header to overflow this page"
    );
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout(), reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut ContinueOnce,
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert!(
        matches!(
            progress,
            Err(DriveError::Recovery(RecoveryError::PageTooSmall { .. }))
        ),
        "an undersized page must refuse outright rather than silently booting bank A's \
         retired run: {progress:?}"
    );

    // Nothing moved: bank B, the real authority, is untouched.
    let (b_run, ..) = header_on(&mut device, BankId::B).expect("bank B is untouched");
    assert_eq!(b_run, RUN);
}

#[test]
fn read_bank_uses_the_whole_page_for_the_header_once_the_seal_is_a_scalar() {
    // Codex found this on round 4. The old `read_bank` reserved `seal_len` bytes out of
    // `page` before it ever read the header, so a page sized to hold the header — and
    // nothing besides it, not even that bank's own seal — could still be refused. The header
    // and the seal never need to be in `page` at the same time: the seal decodes to a
    // scalar `Seal` with no borrow of the buffer, so once it is read the whole page is free
    // for the header. With an 8-byte program unit and a 64-byte input, the padded header
    // needs exactly as many bytes as `page` holds; the old reservation left 16 bytes short.
    let layout = layout_with_read_size(8);
    let mut device = Device::new(geometry_with_read_size(8));
    let wide_input = [b'x'; 64];
    let header = BankHeader {
        align: layout.align(),
        input: &wide_input,
        ..first_header()
    };
    install_on(&mut device, layout, BankId::A, Generation::FIRST, &header);

    let mut staging = [0_u8; 512];
    let Ok(header_needed) = bank::encode_header(&header, &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    // Exactly the padded header's own length — no room for the bank's seal at all.
    let mut page = vec![0_u8; header_needed];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout, reserve_for(layout)).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut JustStarted { input: &wide_input },
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: waymaker_drive::Conclusion::Completed,
                ..
            })
        ),
        "a page sized to exactly the header's own padded length must still boot, even though \
         it has no room left over for that bank's seal: {progress:?}"
    );
}

#[test]
fn an_unsealed_banks_oversized_header_never_blocks_the_smaller_sealed_authority() {
    // Codex found this on round 4, in the fix for the previous round's truncation check: it
    // ran before the seal was ever read, so an *unsealed* bank whose stale header declares
    // more input than this boot's page can hold produced a hard refusal — even though an
    // unsealed bank is never a candidate at any generation and reading its header at all
    // should have cost this boot nothing. Bank A is small and genuinely sealed, the real
    // authority; bank B carries an oversized header with no seal behind it at all, the shape
    // a device left mid-swap — staged, never sealed — would have.
    let layout = layout();
    let mut device = Device::new(geometry());
    install(&mut device, BankId::A, Generation::FIRST, &first_header());

    let wide_input = [b'x'; 60];
    let wide_header = BankHeader {
        input: &wide_input,
        ..first_header()
    };
    install_header_only(&mut device, layout, BankId::B, &wide_header);

    let mut staging = [0_u8; 512];
    let Ok(short_len) = bank::encode_header(&first_header(), &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    let Ok(long_len) = bank::encode_header(&wide_header, &mut staging) else {
        unreachable!("a bank holds its own header")
    };
    // Room for bank A's own header and its seal, deliberately short of bank B's declared
    // (and never-sealed) length.
    let mut page = vec![0_u8; short_len + bank::SEAL_BYTES + 8];
    assert!(
        page.len() < long_len + bank::SEAL_BYTES,
        "the fixture needs bank B's declared length to overflow this page"
    );
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout, reserve()).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut JustStarted { input: FIRST_INPUT },
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: waymaker_drive::Conclusion::Completed,
                ..
            })
        ),
        "an unsealed bank's oversized header must never block booting the smaller, sealed \
         authority: {progress:?}"
    );
}

#[test]
fn header_read_length_is_rounded_down_to_a_whole_read_unit() {
    // Codex found this on round 4. Even with the seal decoded to scalar state first, a
    // header read sized to whatever `page` happened to leave — with no rounding — can ask a
    // conforming `StableStorage` for a length it refuses outright, even when `page` had
    // ample room for the header several times over. 113 is not a multiple of this device's
    // 8-byte read unit.
    let layout = layout_with_read_size(8);
    let mut device = Device::new(geometry_with_read_size(8));
    install_on(
        &mut device,
        layout,
        BankId::A,
        Generation::FIRST,
        &BankHeader {
            align: layout.align(),
            ..first_header()
        },
    );
    let mut page = vec![0_u8; 113];
    let mut result = [0_u8; 16];

    let progress = Driver::at_bank(layout, reserve_for(layout)).boot(
        &mut device,
        &mut waymaker_drive::demo::World::new(),
        &mut JustStarted { input: FIRST_INPUT },
        Scratch {
            page: &mut page,
            result: &mut result,
        },
    );

    assert!(
        matches!(
            progress,
            Ok(Progress::Finished {
                conclusion: waymaker_drive::Conclusion::Completed,
                ..
            })
        ),
        "an ample but misaligned page must still be readable, at a length the device's own \
         read unit accepts: {progress:?}"
    );
}
