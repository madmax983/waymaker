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

use std::cell::RefCell;
use waymaker_conformance::durability::{Breach, Reset, WitnessError, WitnessVerdict, arm, verify};

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

/// How many runs the crash-point sweep has.
///
/// Pinned, for the reason `waymaker-spec`'s census is: a sweep that silently shrank is the
/// direction that turns a proof into a formality.
const SWEEP: usize = 48;

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
        verify(&mut after, region, &mut buffer, Reset::AfterACompletedArm),
        Ok(WitnessVerdict::Held),
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

    // Each run's `Reset` is what `arm` actually returned in that run, recorded as it goes.
    // Inferring it from `run.injection().is_some()` would be wrong: a crash point after the
    // last program interrupts nothing `arm` had left to do, and `arm` returns `Ok(())`.
    let completed: RefCell<Vec<bool>> = RefCell::new(Vec::new());
    let runs = harness
        .run(|session| {
            let mut buffer = [0_u8; 64];
            let outcome = arm(session, region, &mut buffer);
            completed.borrow_mut().push(outcome.is_ok());
            outcome
        })
        .expect("the writer succeeds with no faults armed");
    let completed = completed.into_inner();
    assert_eq!(
        completed.len(),
        runs.len(),
        "the harness calls the writer once per run, in run order"
    );

    // Pinned rather than bounded below. The dangerous direction is a sweep that silently
    // shrank, which `> 40` would have tolerated all the way down to 41.
    assert_eq!(
        runs.len(),
        SWEEP,
        "the crash-point sweep changed size; if that is intended, move the pin"
    );

    let mut buffer = [0_u8; 64];
    for (index, run) in runs.iter().enumerate() {
        let reset = match completed.get(index) {
            Some(true) => Reset::AfterACompletedArm,
            _ => Reset::DuringArm,
        };
        let mut after = after_reset(geometry, run.image());
        assert_eq!(
            verify(&mut after, region, &mut buffer, reset),
            Ok(WitnessVerdict::Held),
            "run {index} ({:?}, {reset:?}) was judged a breach on a correct device",
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
        verify(&mut after, region, &mut buffer, Reset::DuringArm),
        Ok(WitnessVerdict::Breached(Breach::AcknowledgedMutationLost)),
        "the seal is on media without the witness the barrier before it acknowledged"
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
        verify(&mut after, region, &mut buffer, Reset::DuringArm),
        Ok(WitnessVerdict::Breached(
            Breach::LaterMutationOvertookABarrier
        ))
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
    assert_eq!(
        verify(&mut after, region, &mut buffer, Reset::DuringArm),
        Ok(WitnessVerdict::Held),
        "a power cut before the first barrier returned owes nothing"
    );
    // The same media after a *completed* arm is the barrier bug that loses everything, and
    // a seal-relative rule alone would have called it `Held`.
    assert_eq!(
        verify(&mut after, region, &mut buffer, Reset::AfterACompletedArm),
        Ok(WitnessVerdict::Breached(Breach::AcknowledgedMutationLost)),
        "a completed arm owes the witness and the seal unconditionally"
    );
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
        verify(&mut device, whole(other), &mut buffer, Reset::DuringArm),
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
        verify(&mut device, region, &mut buffer, Reset::DuringArm),
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
        verify(&mut device, region, &mut buffer, Reset::DuringArm),
        Err(WitnessError::Suite(SuiteError::BufferTooSmall))
    );
}

/// A device with a write-back cache behind a `barrier` that returns before it settles.
///
/// The barrier bug, rather than a hand-edited image: programs land in the cache, `barrier()`
/// returns `Ok(())` without flushing, and the reset finds media as it was. Everything the
/// witness barriered is lost, which a seal-relative rule alone would have called `Held`.
struct Cached {
    device: Device,
    pending: Vec<(u32, Vec<u8>)>,
    flushes: bool,
}

impl Cached {
    /// A cache whose `barrier` flushes, or one whose `barrier` lies.
    fn new(geometry: Geometry, flushes: bool) -> Self {
        Self {
            device: Device::new(geometry),
            pending: Vec::new(),
            flushes,
        }
    }

    /// The bytes a reset would find: the media, without whatever is still in the cache.
    fn image(&self) -> &[u8] {
        self.device.image()
    }
}

impl StableStorage for Cached {
    type Error = waymaker_fault::FaultError;

    fn geometry(&self) -> Geometry {
        self.device.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        // A cache a reader can see through, which is what makes this the *interesting* bug:
        // `arm`'s own read-back of the witness succeeds, so nothing before the reset notices.
        self.device.read(offset, dst)?;
        for (at, bytes) in &self.pending {
            for (index, byte) in bytes.iter().enumerate() {
                let Ok(index) = u32::try_from(index) else {
                    continue;
                };
                let Some(position) = at.checked_add(index).and_then(|absolute| {
                    absolute
                        .checked_sub(offset)
                        .and_then(|relative| usize::try_from(relative).ok())
                }) else {
                    continue;
                };
                if let Some(cell) = dst.get_mut(position) {
                    *cell &= *byte;
                }
            }
        }
        Ok(())
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        self.device
            .geometry()
            .validate_program(offset, u32::try_from(src.len()).unwrap_or(u32::MAX))?;
        self.pending.push((offset, src.to_vec()));
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.device.erase(offset, len)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        if self.flushes {
            for (offset, bytes) in core::mem::take(&mut self.pending) {
                self.device.program(offset, &bytes)?;
            }
        }
        Ok(())
    }
}

#[test]
fn a_barrier_that_does_not_flush_is_caught_across_the_reset() {
    // The teeth of the witness. Not a hand-edited image: an adapter that implements
    // `StableStorage` and gets the one clause no in-process suite can observe wrong.
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 64];

    let mut lying = Cached::new(geometry, false);
    assert_eq!(
        arm(&mut lying, region, &mut buffer),
        Ok(()),
        "a cache a reader can see through passes every check `arm` makes"
    );
    let mut after = after_reset(geometry, lying.image());
    assert_eq!(
        verify(&mut after, region, &mut buffer, Reset::AfterACompletedArm),
        Ok(WitnessVerdict::Breached(Breach::AcknowledgedMutationLost))
    );

    // The same adapter with a barrier that means it, to show the witness is not simply
    // failing everything with a cache in it.
    let mut honest = Cached::new(geometry, true);
    assert_eq!(arm(&mut honest, region, &mut buffer), Ok(()));
    let mut after = after_reset(geometry, honest.image());
    assert_eq!(
        verify(&mut after, region, &mut buffer, Reset::AfterACompletedArm),
        Ok(WitnessVerdict::Held)
    );
}

#[test]
fn a_later_write_that_only_partly_survived_still_overtook_the_barrier() {
    // The power cut partway through the final program. A prefix of the unacknowledged
    // witness is on media and the seal ordered before it is not, which is the third clause
    // broken — and a reader that asked only "is the whole witness there?" would call the
    // prefix absent and report `Held`.
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 64];
    let mut device = Device::new(geometry);
    assert_eq!(arm(&mut device, region, &mut buffer), Ok(()));

    let mut image = device.into_image();
    let (Some(seal), Some(later)) = (region.block(1), region.block(2)) else {
        unreachable!("a region has three blocks")
    };
    erase_block(&mut image, seal, geometry.erase_size());
    // Keep the first byte of the later witness and erase the rest of its unit: a torn write.
    erase_block(&mut image, later + 1, geometry.program_size() - 1);

    let mut after = after_reset(geometry, &image);
    assert_eq!(
        verify(&mut after, region, &mut buffer, Reset::DuringArm),
        Ok(WitnessVerdict::Breached(
            Breach::LaterMutationOvertookABarrier
        ))
    );
}

#[test]
fn an_erase_a_barrier_acknowledged_and_the_reset_lost_is_a_breach() {
    // A device whose programs are durable across a barrier and whose erases are not. Both
    // witnesses survive, so a reader that looked only at the witness units would report
    // `Held` — while an erase that crossed a barrier of its own is gone.
    let geometry = nested();
    let region = whole(geometry);
    let mut buffer = [0_u8; 64];
    let mut device = Device::new(geometry);
    assert_eq!(arm(&mut device, region, &mut buffer), Ok(()));

    let mut image = device.into_image();
    let unit = geometry.program_size() as usize;
    let Some(start) = usize::try_from(region.offset()).ok().map(|at| at + unit) else {
        unreachable!("a region offset fits in a usize")
    };
    let Some(stale) = image.get_mut(start..start + unit) else {
        unreachable!("the block is longer than two program units")
    };
    stale.fill(0x5A);

    let mut after = after_reset(geometry, &image);
    assert_eq!(
        verify(&mut after, region, &mut buffer, Reset::DuringArm),
        Ok(WitnessVerdict::Breached(Breach::AcknowledgedMutationLost))
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
