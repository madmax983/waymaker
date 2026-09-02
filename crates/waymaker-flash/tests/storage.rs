//! The storage contract of design document §12, held to what §12 actually requires.
//!
//! Every assertion here is a sentence from "Required storage contract" turned into
//! something a build can fail over. What this file cannot check — that a *real* driver
//! honours the ordering the barrier promises — is what issue
//! [#18](https://github.com/madmax983/waymaker/issues/18)'s fault harness is for; this is
//! the contract those tests are written against.

use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// A geometry a Cortex-M0+ internal flash could plausibly have.
fn nor() -> Geometry {
    let Ok(geometry) = Geometry::new(8192, 4096, 8, 1) else {
        unreachable!("8192 is whole 4096-byte blocks of whole 8-byte units of single bytes")
    };
    geometry
}

#[test]
fn a_geometry_reports_the_four_units_it_was_built_from() {
    let geometry = nor();
    assert_eq!(geometry.capacity(), 8192);
    assert_eq!(geometry.erase_size(), 4096);
    assert_eq!(geometry.program_size(), 8);
    assert_eq!(geometry.read_size(), 1);
    assert_eq!(geometry.erase_blocks(), 2);
}

#[test]
fn a_zero_unit_is_not_a_geometry() {
    for candidate in [
        Geometry::new(0, 4096, 8, 1),
        Geometry::new(8192, 0, 8, 1),
        Geometry::new(8192, 4096, 0, 1),
        Geometry::new(8192, 4096, 8, 0),
    ] {
        assert_eq!(candidate, Err(GeometryError::ZeroUnit));
    }
}

#[test]
fn a_unit_that_is_not_a_power_of_two_is_not_a_geometry() {
    // Not a taste: every alignment check in this module is `offset & (unit - 1)`, and that
    // identity holds only for powers of two. `thumbv6m-none-eabi` has no divider, so a
    // geometry that permitted a 12-byte program unit would link `__aeabi_uidivmod` and
    // cost 408 B of an 8 KiB budget to describe a device nobody sells.
    for candidate in [
        Geometry::new(8192, 4096, 12, 8),
        Geometry::new(8192, 4096, 8, 3),
        Geometry::new(8192, 3000, 8, 1),
    ] {
        assert_eq!(candidate, Err(GeometryError::UnitIsNotAPowerOfTwo));
    }
}

#[test]
fn units_that_do_not_nest_are_not_a_geometry() {
    // A capacity that is not whole erase blocks, an erase block smaller than a program
    // unit, and a program unit smaller than a read unit. Each would leave a byte of media
    // no legal operation could name.
    for candidate in [
        Geometry::new(8000, 4096, 8, 1),
        Geometry::new(8192, 4, 8, 1),
        Geometry::new(8192, 4096, 1, 4),
    ] {
        assert_eq!(candidate, Err(GeometryError::UnitsDoNotNest));
    }
}

#[test]
fn a_capacity_need_not_be_a_power_of_two_as_long_as_it_is_whole_blocks() {
    let Ok(geometry) = Geometry::new(12288, 4096, 8, 1) else {
        unreachable!("12288 is three whole 4096-byte blocks")
    };
    assert_eq!(geometry.erase_blocks(), 3);
    assert_eq!(geometry.validate_erase(8192, 4096), Ok(()));
    assert_eq!(
        geometry.validate_erase(8192, 8192),
        Err(GeometryError::OutOfBounds)
    );
}

#[test]
fn the_smallest_legal_geometry_is_one_block_of_one_byte() {
    let Ok(geometry) = Geometry::new(1, 1, 1, 1) else {
        unreachable!("one byte nests inside itself")
    };
    assert_eq!(geometry.erase_blocks(), 1);
    assert_eq!(geometry.validate_program(0, 1), Ok(()));
    assert_eq!(geometry.validate_program(1, 0), Ok(()));
}

#[test]
fn a_program_is_validated_against_the_program_unit_and_the_capacity() {
    let geometry = nor();
    assert_eq!(geometry.validate_program(0, 8), Ok(()));
    assert_eq!(geometry.validate_program(8184, 8), Ok(()));
    assert_eq!(
        geometry.validate_program(1, 8),
        Err(GeometryError::MisalignedOffset)
    );
    assert_eq!(
        geometry.validate_program(0, 9),
        Err(GeometryError::MisalignedLength)
    );
    assert_eq!(
        geometry.validate_program(8192, 8),
        Err(GeometryError::OutOfBounds)
    );
    assert_eq!(
        geometry.validate_program(8188, 8),
        Err(GeometryError::MisalignedOffset)
    );
}

#[test]
fn an_erase_is_validated_against_the_erase_block_and_the_capacity() {
    let geometry = nor();
    assert_eq!(geometry.validate_erase(0, 4096), Ok(()));
    assert_eq!(geometry.validate_erase(4096, 4096), Ok(()));
    assert_eq!(geometry.validate_erase(0, 8192), Ok(()));
    assert_eq!(
        geometry.validate_erase(8, 4096),
        Err(GeometryError::MisalignedOffset)
    );
    assert_eq!(
        geometry.validate_erase(0, 8),
        Err(GeometryError::MisalignedLength)
    );
    assert_eq!(
        geometry.validate_erase(4096, 8192),
        Err(GeometryError::OutOfBounds)
    );
}

#[test]
fn a_read_is_validated_against_the_read_unit_and_the_capacity() {
    let Ok(geometry) = Geometry::new(8192, 4096, 8, 4) else {
        unreachable!("an 8-byte program unit is whole 4-byte read units")
    };
    assert_eq!(geometry.validate_read(0, 4), Ok(()));
    assert_eq!(
        geometry.validate_read(2, 4),
        Err(GeometryError::MisalignedOffset)
    );
    assert_eq!(
        geometry.validate_read(0, 5),
        Err(GeometryError::MisalignedLength)
    );
    assert_eq!(
        geometry.validate_read(8188, 8),
        Err(GeometryError::OutOfBounds)
    );
}

#[test]
fn an_offset_and_a_length_that_overflow_are_out_of_bounds_rather_than_wrapping() {
    // `offset + len` in `u32` wraps to a small number, and a validator that computed it
    // that way would let a program run off the end of every device in the world.
    let geometry = nor();
    assert_eq!(
        geometry.validate_program(u32::MAX - 7, 8),
        Err(GeometryError::OutOfBounds)
    );
    assert_eq!(
        geometry.validate_read(u32::MAX, 1),
        Err(GeometryError::OutOfBounds)
    );
}

#[test]
fn every_geometry_error_carries_a_message_and_they_are_all_different() {
    let all = [
        GeometryError::ZeroUnit,
        GeometryError::UnitIsNotAPowerOfTwo,
        GeometryError::UnitsDoNotNest,
        GeometryError::MisalignedOffset,
        GeometryError::MisalignedLength,
        GeometryError::OutOfBounds,
    ];
    for error in all {
        assert!(!error.message().is_empty());
        assert_eq!(error.message(), std::format!("{error}"));
    }
    for (index, error) in all.iter().enumerate() {
        for other in all.iter().skip(index + 1) {
            assert_ne!(error.message(), other.message());
        }
    }
}

/// A storage that answers the contract and remembers whether the barrier was reached.
///
/// Not a fault model — that is `waymaker-fault`'s job. This exists to prove the trait can
/// be implemented at all from outside the crate that declares it, which is the property
/// design document §12 is really about.
struct Bench {
    geometry: Geometry,
    media: Vec<u8>,
    barriers: usize,
}

/// `media[offset..offset + len]`, or `None` rather than a panic.
fn span(media: &[u8], offset: u32, len: u32) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    media.get(start..end)
}

/// The same span, to write through.
fn span_mut(media: &mut [u8], offset: u32, len: u32) -> Option<&mut [u8]> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    media.get_mut(start..end)
}

impl StableStorage for Bench {
    type Error = GeometryError;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(dst.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_read(offset, len)?;
        let source = span(&self.media, offset, len).ok_or(GeometryError::OutOfBounds)?;
        dst.copy_from_slice(source);
        Ok(())
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(src.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_program(offset, len)?;
        let target = span_mut(&mut self.media, offset, len).ok_or(GeometryError::OutOfBounds)?;
        for (cell, wanted) in target.iter_mut().zip(src) {
            *cell &= *wanted;
        }
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.geometry.validate_erase(offset, len)?;
        let target = span_mut(&mut self.media, offset, len).ok_or(GeometryError::OutOfBounds)?;
        target.fill(0xFF);
        Ok(())
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.barriers += 1;
        Ok(())
    }
}

#[test]
fn the_contract_can_be_implemented_from_outside_the_crate_that_declares_it() {
    let geometry = nor();
    let mut bench = Bench {
        geometry,
        media: std::vec![0xFF; 8192],
        barriers: 0,
    };

    assert_eq!(bench.geometry(), geometry);
    assert_eq!(bench.program(0, &[0x00; 8]), Ok(()));
    bench.barrier().expect("the bench barrier cannot fail");

    let mut read = [0xAA; 8];
    assert_eq!(bench.read(0, &mut read), Ok(()));
    assert_eq!(read, [0x00; 8]);
    assert_eq!(bench.barriers, 1);

    assert_eq!(
        bench.program(3, &[0x00; 8]),
        Err(GeometryError::MisalignedOffset)
    );
}
