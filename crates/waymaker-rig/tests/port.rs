//! The rig, driven through the `embedded-storage` port rather than through the model.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27)'s first work item is a
//! "concrete `StableStorage` adapter over real NOR flash (via `embedded-storage`, adapted
//! rather than depended upon by the kernel)", and that adapter already exists:
//! [`NorFlashStorage`], landed for issue #21 and argued in
//! [ADR 0016](https://github.com/madmax983/waymaker/blob/main/docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md).
//!
//! What did not exist is this: a *workflow* run over it. Everything else in this crate drives
//! `waymaker_fault::Device` directly, and an adapter nothing has ever run a journal through is
//! an adapter whose geometry derivation, whose validation and whose no-op barrier nobody has
//! actually leaned on. The port and the rig are two halves of one work item, and this is
//! where they meet.
//!
//! The `NorFlash` beneath is backed by [`waymaker_fault::Device`], so the media still behaves
//! the way NOR behaves — erased is `0xFF`, programming only clears bits — and the port is
//! doing real translation rather than sitting over a `Vec` that agrees with everything.

use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use waymaker_conformance::nor::NorFlashStorage;
use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::log::Outcome;
use waymaker_rig::plan::Plan;
use waymaker_rig::run::{Dispatcher, NeverCut, Rig, Stop};
use waymaker_rig::wear::Metered;

const CAPACITY: u32 = 6 * 256;
const ERASE: u32 = 256;
const PROGRAM: u32 = 4;

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(CAPACITY, ERASE, PROGRAM, 1) else {
        unreachable!("a legal geometry")
    };
    geometry
}

/// An `embedded-storage` NOR flash whose media is the crash injector's model.
///
/// Not a `Vec<u8>`: a double that accepted every write would let the port through on
/// arithmetic that a real part refuses. This one clears bits and nothing else, and validates
/// through `waymaker_fault::Device` before it does.
struct ModelledNor {
    device: waymaker_fault::Device,
}

impl ModelledNor {
    fn new() -> Self {
        Self {
            device: waymaker_fault::Device::new(geometry()),
        }
    }
}

impl ErrorType for ModelledNor {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for ModelledNor {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.device
            .read(offset, bytes)
            .map_err(|_| NorFlashErrorKind::OutOfBounds)
    }

    fn capacity(&self) -> usize {
        usize::try_from(CAPACITY).unwrap_or(0)
    }
}

impl NorFlash for ModelledNor {
    const WRITE_SIZE: usize = PROGRAM as usize;
    const ERASE_SIZE: usize = ERASE as usize;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let len = to.checked_sub(from).ok_or(NorFlashErrorKind::OutOfBounds)?;
        self.device
            .erase(from, len)
            .map_err(|_| NorFlashErrorKind::NotAligned)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.device
            .program(offset, bytes)
            .map_err(|_| NorFlashErrorKind::NotAligned)
    }
}

struct Counting {
    dispatched: Vec<u16>,
}

impl Dispatcher for Counting {
    type Error = core::convert::Infallible;

    fn dispatch(&mut self, effect: u16, _input: &[u8]) -> Result<(), Self::Error> {
        self.dispatched.push(effect);
        Ok(())
    }
}

#[test]
fn the_port_derives_the_geometry_the_rig_was_laid_out_for() {
    // The port's own job, and the rig's precondition: every step of `waymaker-flash` compares
    // the geometry a region was validated against with the geometry of the storage it is
    // handed, so a port that described this part differently would fail before it wrote a byte.
    let storage = NorFlashStorage::new(ModelledNor::new()).expect("a describable part");
    assert_eq!(storage.geometry(), geometry());
}

#[test]
fn a_whole_run_writes_and_recovers_through_the_embedded_storage_port() {
    let mut storage = NorFlashStorage::new(ModelledNor::new()).expect("a describable part");
    let rig = Rig::new::<<NorFlashStorage<ModelledNor> as StableStorage>::Error>(
        geometry(),
        Plan::new(0x2027),
        3,
    )
    .expect("six erase blocks hold two banks and a witness");
    let mut page = [0_u8; Rig::PAGE_BYTES];

    let (stop, dispatched, wear) = {
        let mut metered = Metered::new(&mut storage);
        rig.prepare(&mut metered, 0, &mut page)
            .expect("the port accepts the layout the rig erases and installs");
        let mut dispatcher = Counting {
            dispatched: Vec::new(),
        };
        let stop = rig
            .iterate(0, &mut metered, &mut dispatcher, &mut NeverCut, &mut page)
            .expect("a run with no cut in it");
        (stop, dispatcher.dispatched, metered.wear())
    };

    assert_eq!(stop, Stop::Completed);
    assert_eq!(dispatched, vec![0, 1, 2]);
    assert_eq!(wear.effects(), 3);
    assert!(wear.erase_operations() > 0, "the bank install erases");
    assert!(
        wear.programmed_bytes_per_effect().is_some(),
        "a run with effects in it has a per-effect figure"
    );

    // And the half that matters: what the port wrote is what a recovery reads back, judged by
    // the rig's own oracle rather than by a comparison this test wrote.
    let outcome = rig
        .verify(0, &mut storage, &mut page)
        .expect("a verdict from the port");
    assert_eq!(outcome, Outcome::Passed);
}

#[test]
fn the_port_costs_the_same_traffic_the_model_does() {
    // Two adapters over the same media model, asked for the same run. A port that silently
    // split or merged the calls beneath it would show up here as a different figure — which is
    // the only way this test can tell the port apart from the thing it wraps.
    let rig = Rig::new::<waymaker_fault::FaultError>(geometry(), Plan::new(0x2027), 3)
        .expect("a legal layout");
    let mut page = [0_u8; Rig::PAGE_BYTES];

    let mut direct = waymaker_fault::Device::new(geometry());
    let direct_wear = {
        let mut metered = Metered::new(&mut direct);
        rig.prepare(&mut metered, 0, &mut page).expect("prepared");
        rig.iterate(
            0,
            &mut metered,
            &mut Counting {
                dispatched: Vec::new(),
            },
            &mut NeverCut,
            &mut page,
        )
        .expect("a clean run");
        metered.wear()
    };

    let mut ported = NorFlashStorage::new(ModelledNor::new()).expect("a describable part");
    let ported_wear = {
        let mut metered = Metered::new(&mut ported);
        rig.prepare(&mut metered, 0, &mut page).expect("prepared");
        rig.iterate(
            0,
            &mut metered,
            &mut Counting {
                dispatched: Vec::new(),
            },
            &mut NeverCut,
            &mut page,
        )
        .expect("a clean run");
        metered.wear()
    };

    assert_eq!(direct_wear, ported_wear);
}

#[test]
fn the_media_the_port_wrote_is_the_media_the_model_would_have() {
    // Byte for byte. The port translates offsets and validates; it must not reorder, pad or
    // rewrite anything, and an image comparison is the bluntest way to say so.
    let rig = Rig::new::<waymaker_fault::FaultError>(geometry(), Plan::new(0x2027), 2)
        .expect("a legal layout");
    let mut page = [0_u8; Rig::PAGE_BYTES];

    let mut direct = waymaker_fault::Device::new(geometry());
    {
        let mut metered = Metered::new(&mut direct);
        rig.prepare(&mut metered, 0, &mut page).expect("prepared");
        rig.iterate(
            0,
            &mut metered,
            &mut Counting {
                dispatched: Vec::new(),
            },
            &mut NeverCut,
            &mut page,
        )
        .expect("a clean run");
    }

    let mut ported = NorFlashStorage::new(ModelledNor::new()).expect("a describable part");
    {
        let mut metered = Metered::new(&mut ported);
        rig.prepare(&mut metered, 0, &mut page).expect("prepared");
        rig.iterate(
            0,
            &mut metered,
            &mut Counting {
                dispatched: Vec::new(),
            },
            &mut NeverCut,
            &mut page,
        )
        .expect("a clean run");
    }

    let mut through_port = vec![0_u8; usize::try_from(CAPACITY).expect("a small capacity")];
    ported
        .read(0, &mut through_port)
        .expect("the whole part is readable");
    assert_eq!(direct.image(), through_port.as_slice());
}
