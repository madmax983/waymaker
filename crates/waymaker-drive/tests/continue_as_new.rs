//! Issue [#110](https://github.com/madmax983/waymaker/issues/110)'s `continue_as_new` join:
//! a [`Driver::at_bank`] performs design document §10's real swap, over real media.
//!
//! `crates/waymaker-drive/tests/ota.rs`'s
//! `continue_as_new_is_refused_by_a_driver_that_cannot_name_a_bank` is the other half —
//! [`Driver::new`], pointed at a fixed region, still refuses. This file is the driver that
//! can.

use waymaker_core::version::VersionRange;
use waymaker_core::{ActivityKind, Outcome, RunId};
use waymaker_drive::{
    Boundary, DriveError, Driver, Identity, Progress, Scratch, Suspended, Workflow,
};
use waymaker_fault::Device;
use waymaker_flash::bank::{self, BankHeader, BankId, BankLayout, Generation};
use waymaker_flash::capacity::{Bounds, Reserve};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::recovery::JournalRegion;
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
