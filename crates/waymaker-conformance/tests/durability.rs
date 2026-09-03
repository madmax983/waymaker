//! The two clauses no in-process suite can observe.
//!
//! §12 says "after `barrier` returns, all earlier successful mutations survive reset" and
//! "no later mutation may become durable before mutations ordered by a completed barrier".
//! Both are statements about what is on media *after the power came back*, and a suite
//! running in one process never sees that moment. So the witness is two calls with a reset
//! in the middle: [`arm`] writes, the caller cuts the power at any point it likes, and
//! [`verify`] reads the answer.
//!
//! On real hardware the caller is a person with a bench supply. Here it is
//! `waymaker_fault`, which cuts the power at *every* point the write sequence can be
//! interrupted, and rebuilds the device from the image that survived.

use waymaker_conformance::durability::{Breach, Verdict, WitnessError, arm, verify};
use waymaker_conformance::region::Region;
use waymaker_conformance::suite::SuiteError;
use waymaker_fault::{Device, Harness};
use waymaker_flash::storage::{Geometry, StableStorage};

/// A device that accepts every program and keeps none of them.
///
/// The one shape `arm` has to refuse rather than report as "nothing was durable": if a
/// witness never reaches media, every answer `verify` could give afterwards is about a
/// device that was never armed.
struct Amnesiac(Device);

impl StableStorage for Amnesiac {
    type Error = waymaker_fault::FaultError;

    fn geometry(&self) -> Geometry {
        self.0.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, dst)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let _ = (offset, src);
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.0.erase(offset, len)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.0.barrier()
    }
}

/// A device whose reads refuse, so that `verify` has a driver error to carry.
struct Blind(Device);

impl StableStorage for Blind {
    type Error = waymaker_fault::FaultError;

    fn geometry(&self) -> Geometry {
        self.0.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let _ = (offset, dst);
        Err(waymaker_fault::FaultError::InjectedFailure)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        self.0.program(offset, src)
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.0.erase(offset, len)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.0.barrier()
    }
}

fn nested() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 64, 4, 2) else {
        unreachable!("1024 is whole 64-byte blocks of whole 4-byte units of 2-byte reads")
    };
    geometry
}

fn whole(geometry: Geometry) -> Region {
    let Ok(region) = Region::whole_device(geometry) else {
        unreachable!("sixteen erase blocks is more than three")
    };
    region
}

/// The device a reset would leave behind, given the bytes that survived.
fn after_reset(geometry: Geometry, image: &[u8]) -> Device {
    let Some(device) = Device::restored(geometry, image.to_vec()) else {
        unreachable!("the image is exactly the capacity it came from")
    };
    device
}

#[test]
fn a_witness_that_was_never_interrupted_holds() {
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 64];
    let mut device = Device::new(geometry);

    assert_eq!(arm(&mut device, region, &mut buffer), Ok(()));

    let image = device.image().to_vec();
    let mut after = after_reset(geometry, &image);
    assert_eq!(
        verify(&mut after, region, &mut buffer),
        Ok(Verdict::Held),
        "an uninterrupted run acknowledged everything it barriered"
    );
}

#[test]
fn a_witness_holds_at_every_point_the_power_can_go() {
    // The whole point of the two-phase shape. `waymaker_fault::injections` lists every byte
    // of every program, every erase block of every erase, and both sides of every barrier;
    // a correct device cannot be caught out at any of them.
    let geometry = nested();
    let region = whole(geometry);
    let harness = Harness::new(geometry);

    let runs = harness
        .run(|session| {
            let mut buffer = [0_u8; 64];
            arm(session, region, &mut buffer)
        })
        .expect("the writer succeeds with no faults armed");

    assert!(
        runs.len() > 40,
        "the sweep thinned out to {} runs; a witness with no crash points in it proves nothing",
        runs.len()
    );

    let mut buffer = [0_u8; 64];
    for (index, run) in runs.iter().enumerate() {
        let mut after = after_reset(geometry, run.image());
        assert_eq!(
            verify(&mut after, region, &mut buffer),
            Ok(Verdict::Held),
            "run {index} ({:?}) was judged a breach on a correct device",
            run.injection()
        );
    }
}

#[test]
fn losing_a_mutation_a_barrier_acknowledged_is_a_breach() {
    // The seal is programmed after the witness's barrier returned. A reset that finds the
    // seal and not the witness has found a barrier that did not mean what it said — which
    // is exactly the failure §12's second sentence forbids, and exactly the one a suite
    // running in one process cannot see.
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 64];
    let mut device = Device::new(geometry);
    assert_eq!(arm(&mut device, region, &mut buffer), Ok(()));

    let mut image = device.into_image();
    erase_block(&mut image, region.offset(), geometry.erase_size());

    let mut after = after_reset(geometry, &image);
    assert_eq!(
        verify(&mut after, region, &mut buffer),
        Ok(Verdict::Breached(Breach::AcknowledgedMutationLost))
    );
}

#[test]
fn a_mutation_overtaking_a_completed_barrier_is_a_breach() {
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 64];
    let mut device = Device::new(geometry);
    assert_eq!(arm(&mut device, region, &mut buffer), Ok(()));

    let mut image = device.into_image();
    let Some(seal) = region.block(1) else {
        unreachable!("a region has three blocks")
    };
    erase_block(&mut image, seal, geometry.erase_size());

    let mut after = after_reset(geometry, &image);
    assert_eq!(
        verify(&mut after, region, &mut buffer),
        Ok(Verdict::Breached(Breach::LaterMutationOvertookABarrier))
    );
}

#[test]
fn a_device_that_lost_everything_is_not_a_breach() {
    // Power cut before any barrier returned. Nothing was acknowledged, so nothing is owed,
    // and a witness that called this a breach would be a witness nobody could trust.
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 64];
    let mut device = Device::new(geometry);
    assert_eq!(arm(&mut device, region, &mut buffer), Ok(()));

    let mut image = device.into_image();
    for index in 0..3 {
        let Some(block) = region.block(index) else {
            unreachable!("a region has three blocks")
        };
        erase_block(&mut image, block, geometry.erase_size());
    }

    let mut after = after_reset(geometry, &image);
    assert_eq!(verify(&mut after, region, &mut buffer), Ok(Verdict::Held));
}

#[test]
fn a_witness_needs_a_region_that_belongs_to_the_device() {
    let geometry = nested();
    let Ok(other) = Geometry::new(2048, 64, 4, 2) else {
        unreachable!("2048 is whole 64-byte blocks")
    };
    let mut buffer = [0_u8; 64];
    let mut device = Device::new(geometry);

    assert_eq!(
        arm(&mut device, whole(other), &mut buffer),
        Err(WitnessError::Suite(SuiteError::RegionIsNotForThisDevice))
    );
    assert_eq!(
        verify(&mut device, whole(other), &mut buffer),
        Err(WitnessError::Suite(SuiteError::RegionIsNotForThisDevice))
    );
}

#[test]
fn a_witness_needs_a_buffer_it_can_work_in() {
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 2];
    let mut device = Device::new(geometry);

    assert_eq!(
        arm(&mut device, region, &mut buffer),
        Err(WitnessError::Suite(SuiteError::BufferTooSmall))
    );
}

#[test]
fn a_witness_that_does_not_reach_media_is_refused_at_arm_time() {
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 64];
    let mut device = Amnesiac(Device::new(geometry));

    assert_eq!(
        arm(&mut device, region, &mut buffer),
        Err(WitnessError::WitnessDidNotTake),
        "a device that keeps no witness must be told so before any barrier claims one"
    );
}

#[test]
fn a_read_the_driver_refuses_reaches_the_caller() {
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 64];
    let mut device = Blind(Device::new(geometry));

    assert_eq!(
        verify(&mut device, region, &mut buffer),
        Err(WitnessError::Driver(
            waymaker_fault::FaultError::InjectedFailure
        ))
    );
}

#[test]
fn verifying_needs_a_buffer_it_can_work_in() {
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 2];
    let mut device = Device::new(geometry);

    assert_eq!(
        verify(&mut device, region, &mut buffer),
        Err(WitnessError::Suite(SuiteError::BufferTooSmall))
    );
}

/// Clears one erase block of an image, as a reset that lost it would leave it.
fn erase_block(image: &mut [u8], offset: u32, len: u32) {
    let (Ok(start), Ok(len)) = (usize::try_from(offset), usize::try_from(len)) else {
        unreachable!("a geometry's offsets fit in a usize on every host this runs on")
    };
    let Some(end) = start.checked_add(len) else {
        unreachable!("an erase block inside the capacity cannot overflow")
    };
    let Some(target) = image.get_mut(start..end) else {
        unreachable!("the block is inside the image it came from")
    };
    target.fill(0xFF);
}
