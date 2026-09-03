//! The `embedded-storage` port.
//!
//! Issue [#21](https://github.com/madmax983/waymaker/issues/21) is done when "an
//! `embedded-storage` implementation can be adapted without `embedded-storage` becoming a
//! kernel dependency". This file is the first half of that sentence — a real
//! [`NorFlash`] driven through [`NorFlashStorage`] and then through the whole conformance
//! suite — and `xtask`'s `dependency-direction` and `kernel-zero-dependencies` rules are
//! the second: every layer's `may_depend_on_external` list is empty, so a layer that grew
//! this dependency fails the gate. The adapter lives here, above the layers, which is the
//! only place it can.

use std::cell::Cell;

use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use waymaker_conformance::nor::{NorFlashStorage, PortError, PortGeometryError};
use waymaker_conformance::region::Region;
use waymaker_conformance::suite::run;
use waymaker_flash::storage::{GeometryError, StableStorage};

const ERASED: u8 = 0xFF;
const CAPACITY: usize = 1024;

/// An `embedded-storage` NOR flash, of the kind a HAL crate ships.
///
/// It knows nothing about Waymaker: it implements `embedded-storage`'s traits and uses
/// that crate's own `check_read`, `check_write` and `check_erase` helpers, which is what a
/// real driver does.
struct FakeNor {
    media: Vec<u8>,
    erases: Cell<u32>,
    last_erase: Cell<(u32, u32)>,
    reads: Cell<u32>,
    writes: Cell<u32>,
}

impl FakeNor {
    fn new() -> Self {
        Self {
            media: vec![ERASED; CAPACITY],
            erases: Cell::new(0),
            last_erase: Cell::new((0, 0)),
            reads: Cell::new(0),
            writes: Cell::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.reads.get() + self.writes.get() + self.erases.get()
    }

    fn last_erase(&self) -> (u32, u32) {
        self.last_erase.get()
    }
}

impl ErrorType for FakeNor {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for FakeNor {
    const READ_SIZE: usize = 2;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.reads.set(self.reads.get().wrapping_add(1));
        embedded_storage::nor_flash::check_read(self, offset, bytes.len())?;
        let Ok(start) = usize::try_from(offset) else {
            return Err(NorFlashErrorKind::OutOfBounds);
        };
        let Some(end) = start.checked_add(bytes.len()) else {
            return Err(NorFlashErrorKind::OutOfBounds);
        };
        let Some(source) = self.media.get(start..end) else {
            return Err(NorFlashErrorKind::OutOfBounds);
        };
        bytes.copy_from_slice(source);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.media.len()
    }
}

impl NorFlash for FakeNor {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 64;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.erases.set(self.erases.get().wrapping_add(1));
        self.last_erase.set((from, to));
        embedded_storage::nor_flash::check_erase(self, from, to)?;
        let (Ok(start), Ok(end)) = (usize::try_from(from), usize::try_from(to)) else {
            return Err(NorFlashErrorKind::OutOfBounds);
        };
        let Some(target) = self.media.get_mut(start..end) else {
            return Err(NorFlashErrorKind::OutOfBounds);
        };
        target.fill(ERASED);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.writes.set(self.writes.get().wrapping_add(1));
        embedded_storage::nor_flash::check_write(self, offset, bytes.len())?;
        let Ok(start) = usize::try_from(offset) else {
            return Err(NorFlashErrorKind::OutOfBounds);
        };
        let Some(end) = start.checked_add(bytes.len()) else {
            return Err(NorFlashErrorKind::OutOfBounds);
        };
        let Some(target) = self.media.get_mut(start..end) else {
            return Err(NorFlashErrorKind::OutOfBounds);
        };
        for (cell, wanted) in target.iter_mut().zip(bytes) {
            *cell &= *wanted;
        }
        Ok(())
    }
}

/// A driver whose erase unit is not a power of two, which no Waymaker geometry can be.
struct OddNor;

impl ErrorType for OddNor {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for OddNor {
    const READ_SIZE: usize = 1;

    fn read(&mut self, _offset: u32, _bytes: &mut [u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn capacity(&self) -> usize {
        192
    }
}

impl NorFlash for OddNor {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 3;

    fn erase(&mut self, _from: u32, _to: u32) -> Result<(), Self::Error> {
        Ok(())
    }

    fn write(&mut self, _offset: u32, _bytes: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A driver whose erase unit does not fit in a 32-bit word.
struct WideEraseNor;

impl ErrorType for WideEraseNor {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for WideEraseNor {
    const READ_SIZE: usize = 1;

    fn read(&mut self, _offset: u32, _bytes: &mut [u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn capacity(&self) -> usize {
        1024
    }
}

impl NorFlash for WideEraseNor {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = (u32::MAX as usize) + 1;

    fn erase(&mut self, _from: u32, _to: u32) -> Result<(), Self::Error> {
        Ok(())
    }

    fn write(&mut self, _offset: u32, _bytes: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A driver whose program unit does not fit in a 32-bit word.
struct WideWriteNor;

impl ErrorType for WideWriteNor {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for WideWriteNor {
    const READ_SIZE: usize = (u32::MAX as usize) + 1;

    fn read(&mut self, _offset: u32, _bytes: &mut [u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn capacity(&self) -> usize {
        1024
    }
}

impl NorFlash for WideWriteNor {
    const WRITE_SIZE: usize = (u32::MAX as usize) + 1;
    const ERASE_SIZE: usize = 64;

    fn erase(&mut self, _from: u32, _to: u32) -> Result<(), Self::Error> {
        Ok(())
    }

    fn write(&mut self, _offset: u32, _bytes: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A driver whose capacity does not fit in the 32-bit offsets §12's contract is written in.
struct HugeNor;

impl ErrorType for HugeNor {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for HugeNor {
    const READ_SIZE: usize = 1;

    fn read(&mut self, _offset: u32, _bytes: &mut [u8]) -> Result<(), Self::Error> {
        Ok(())
    }

    fn capacity(&self) -> usize {
        (u32::MAX as usize) + 1
    }
}

impl NorFlash for HugeNor {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;

    fn erase(&mut self, _from: u32, _to: u32) -> Result<(), Self::Error> {
        Ok(())
    }

    fn write(&mut self, _offset: u32, _bytes: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn ported() -> NorFlashStorage<FakeNor> {
    match NorFlashStorage::new(FakeNor::new()) {
        Ok(storage) => storage,
        Err(error) => unreachable!("a 1024/64/4/2 NOR is a geometry: {error:?}"),
    }
}

#[test]
fn the_port_derives_the_geometry_from_the_drivers_own_constants() {
    let storage = ported();
    let geometry = storage.geometry();
    assert_eq!(geometry.capacity(), 1024);
    assert_eq!(geometry.erase_size(), 64);
    assert_eq!(geometry.program_size(), 4);
    assert_eq!(geometry.read_size(), 2);
}

#[test]
fn an_embedded_storage_driver_passes_the_whole_conformance_suite() {
    // Issue #21's second "done when", against its first: the suite that any adapter can be
    // run against, run against an `embedded-storage` adapter.
    let mut storage = ported();
    let Ok(region) = Region::whole_device(storage.geometry()) else {
        unreachable!("sixteen erase blocks is more than three")
    };
    let mut buffer = [0_u8; 64];

    let report = run(&mut storage, region, &mut buffer).expect("the run starts");

    assert_eq!(report.verdict(), Ok(()), "{report:?}");
    assert_eq!(report.exemptions().count(), 0, "{report:?}");
}

#[test]
fn a_driver_whose_units_are_not_a_geometry_is_refused_at_construction() {
    assert_eq!(
        NorFlashStorage::new(OddNor).err(),
        Some(PortGeometryError::Geometry(
            GeometryError::UnitIsNotAPowerOfTwo
        ))
    );
}

#[test]
fn a_driver_wider_than_the_offsets_the_contract_uses_is_refused_at_construction() {
    assert_eq!(
        NorFlashStorage::new(HugeNor).err(),
        Some(PortGeometryError::UnitDoesNotFitInAWord)
    );
}

#[test]
fn an_illegal_operation_is_refused_before_the_driver_is_called() {
    // §12 puts the validation obligation on the adapter, and `embedded-storage` does not
    // promise it: `check_read` and friends are helpers a driver *may* call. The port
    // validates first, so a driver that forgot is still not asked to do the impossible.
    let mut storage = ported();
    let before = storage.flash().calls();

    assert_eq!(
        storage.read(1, &mut [0_u8; 2]),
        Err(PortError::Geometry(GeometryError::MisalignedOffset))
    );
    assert_eq!(
        storage.program(1, &[0_u8; 4]),
        Err(PortError::Geometry(GeometryError::MisalignedOffset))
    );
    assert_eq!(
        storage.erase(1, 64),
        Err(PortError::Geometry(GeometryError::MisalignedOffset))
    );
    assert_eq!(
        storage.erase(1024, 64),
        Err(PortError::Geometry(GeometryError::OutOfBounds))
    );

    assert_eq!(
        storage.flash().calls(),
        before,
        "the driver was called for an operation the geometry forbids"
    );
}

#[test]
fn an_erase_length_becomes_the_range_the_driver_expects() {
    // The one shape mismatch between the two contracts: §12 says `erase(offset, len)` and
    // `embedded-storage` says `erase(from, to)`. Getting this wrong erases the wrong thing
    // and every round-trip test still passes, so it is asserted rather than inspected.
    let mut storage = ported();
    assert_eq!(storage.erase(128, 128), Ok(()));
    assert_eq!(storage.flash().last_erase(), (128, 256));
}

#[test]
fn a_driver_error_reaches_the_caller_unchanged() {
    let mut storage = ported();
    // Legal against the Waymaker geometry and legal against the driver: nothing to report.
    assert_eq!(storage.program(0, &[0x0F_u8; 4]), Ok(()));
    let mut back = [0_u8; 4];
    assert_eq!(storage.read(0, &mut back), Ok(()));
    assert_eq!(back, [0x0F; 4]);
}

#[test]
fn every_unit_wider_than_a_word_is_refused_and_not_only_the_capacity() {
    // Four `usize`s narrow to `u32` here, and a `narrow` call left off one of them would be
    // invisible on a device small enough for the other three.
    assert_eq!(
        NorFlashStorage::new(WideEraseNor).err(),
        Some(PortGeometryError::UnitDoesNotFitInAWord)
    );
    assert_eq!(
        NorFlashStorage::new(WideWriteNor).err(),
        Some(PortGeometryError::UnitDoesNotFitInAWord)
    );
}

#[test]
fn the_driver_can_be_reached_through_the_port() {
    // A port that could not be borrowed mutably would force a caller wanting a driver-level
    // operation — a chip erase, a status register — to give up the port to get it back.
    let mut storage = ported();
    storage.flash_mut().media.fill(0x00);
    let mut back = [0_u8; 2];
    assert_eq!(storage.read(0, &mut back), Ok(()));
    assert_eq!(back, [0x00; 2]);
}

#[test]
fn the_driver_can_be_taken_back() {
    let storage = ported();
    let flash = storage.into_flash();
    assert_eq!(flash.capacity(), CAPACITY);
}

#[test]
fn a_barrier_on_a_blocking_nor_driver_is_nothing_to_do() {
    // `NorFlash::write` and `NorFlash::erase` are blocking and complete when they return —
    // there is no write-behind to flush and no reordering to settle — so the barrier is a
    // no-op *and says so*. A driver with a cache in front of it needs a different port.
    let mut storage = ported();
    let before = storage.flash().calls();
    assert_eq!(storage.barrier(), Ok(()));
    assert_eq!(storage.barrier(), Ok(()));
    assert_eq!(storage.flash().calls(), before);
}
