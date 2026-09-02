//! The in-memory device, held to what NOR flash actually does.
//!
//! Design document §12's contract and §15's fault list. Every assertion here is a physical
//! property a model that got it wrong would let a firmware bug through: erased bytes read
//! as `0xFF`, programming can only clear bits, and an operation the geometry forbids never
//! reaches media.

use waymaker_fault::{Device, FaultError, OneWayBits};
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(64, 32, 4, 1) else {
        unreachable!("64 is two 32-byte blocks of eight 4-byte units of single bytes")
    };
    geometry
}

fn read_all(device: &mut Device) -> Vec<u8> {
    let mut buffer = vec![0; device.geometry().capacity() as usize];
    let Ok(()) = device.read(0, &mut buffer) else {
        unreachable!("a read of the whole device is aligned and in bounds")
    };
    buffer
}

#[test]
fn a_fresh_device_is_erased_rather_than_zeroed() {
    let mut device = Device::new(geometry());
    assert_eq!(read_all(&mut device), vec![0xFF; 64]);
    assert_eq!(device.image(), &[0xFF; 64][..]);
}

#[test]
fn programming_clears_bits_and_never_sets_them() {
    let mut device = Device::new(geometry());
    device.program(0, &[0b1111_0000, 0xFF, 0xFF, 0xFF]).unwrap();
    device.program(0, &[0b1010_1010, 0xFF, 0xFF, 0xFF]).unwrap();
    // 0xFF & 0xF0 & 0xAA: the second program can only take bits away.
    assert_eq!(device.image().first().copied(), Some(0b1010_0000));
}

#[test]
fn a_program_that_would_set_a_bit_can_be_made_to_say_so() {
    let mut device = Device::with_bit_rule(geometry(), OneWayBits::Rejected);
    device.program(0, &[0x00, 0xFF, 0xFF, 0xFF]).unwrap();
    assert_eq!(
        device.program(0, &[0xFF, 0xFF, 0xFF, 0xFF]),
        Err(FaultError::BitSetWithoutErase)
    );
    // Rejected before media is touched.
    assert_eq!(device.image().first().copied(), Some(0x00));
}

#[test]
fn the_permissive_rule_is_what_hardware_does_and_is_the_default() {
    let mut device = Device::new(geometry());
    device.program(0, &[0x00, 0xFF, 0xFF, 0xFF]).unwrap();
    device.program(0, &[0xFF, 0xFF, 0xFF, 0xFF]).unwrap();
    assert_eq!(device.image().first().copied(), Some(0x00));
}

#[test]
fn erasing_returns_a_block_to_the_erased_state_and_leaves_its_neighbour_alone() {
    let mut device = Device::new(geometry());
    device.program(0, &[0x00; 4]).unwrap();
    device.program(32, &[0x00; 4]).unwrap();
    device.erase(0, 32).unwrap();
    assert_eq!(device.image().first().copied(), Some(0xFF));
    assert_eq!(device.image().get(32).copied(), Some(0x00));
}

#[test]
fn every_operation_is_validated_against_the_geometry_before_media_is_touched() {
    let mut device = Device::new(geometry());
    assert_eq!(
        device.program(1, &[0x00; 4]),
        Err(FaultError::Geometry(GeometryError::MisalignedOffset))
    );
    assert_eq!(
        device.program(0, &[0x00; 3]),
        Err(FaultError::Geometry(GeometryError::MisalignedLength))
    );
    assert_eq!(
        device.erase(0, 4),
        Err(FaultError::Geometry(GeometryError::MisalignedLength))
    );
    assert_eq!(
        device.erase(64, 32),
        Err(FaultError::Geometry(GeometryError::OutOfBounds))
    );
    let mut one = [0_u8; 1];
    assert_eq!(
        device.read(64, &mut one),
        Err(FaultError::Geometry(GeometryError::OutOfBounds))
    );
    assert_eq!(read_all(&mut device), vec![0xFF; 64]);
}

#[test]
fn a_barrier_on_a_device_with_no_faults_always_succeeds() {
    let mut device = Device::new(geometry());
    assert_eq!(device.barrier(), Ok(()));
}

#[test]
fn a_device_reports_the_geometry_it_was_built_from() {
    let device = Device::new(geometry());
    assert_eq!(device.geometry(), geometry());
}

#[test]
fn a_stale_tail_is_erased_bytes_rather_than_absent_bytes() {
    // §15's "stale tails": a reader that stops at the end of what was written has to be
    // told apart from one that stops at the end of the device. The model gives it the
    // same thing hardware does — `0xFF` all the way to the capacity.
    let mut device = Device::new(geometry());
    device.program(0, b"WM\x01\x00").unwrap();
    let image = read_all(&mut device);
    assert_eq!(image.get(..4), Some(&b"WM\x01\x00"[..]));
    assert!(
        image
            .get(4..)
            .is_some_and(|tail| tail.iter().all(|b| *b == 0xFF))
    );
}

#[test]
fn every_fault_error_carries_a_distinct_message() {
    let all = [
        FaultError::Geometry(GeometryError::OutOfBounds),
        FaultError::BitSetWithoutErase,
        FaultError::PowerLoss,
        FaultError::InjectedFailure,
    ];
    for error in all {
        assert!(!error.to_string().is_empty());
    }
    for (index, error) in all.iter().enumerate() {
        for other in all.iter().skip(index + 1) {
            assert_ne!(error.to_string(), other.to_string());
        }
    }
}
