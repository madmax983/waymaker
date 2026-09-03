//! Temporary reviewer probes.
#![allow(missing_docs)]

use waymaker_conformance::region::Region;
use waymaker_conformance::suite::run;
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// Bounds are checked on the start offset only; the length is ignored.
    OffsetOnlyBounds,
    /// Erase is accepted and dropped.
    EraseDoesNothing,
}

struct Dev {
    geometry: Geometry,
    media: Vec<u8>,
    kind: Kind,
}

#[derive(Debug)]
struct Refused;

impl Dev {
    fn new(geometry: Geometry, kind: Kind, initial: u8) -> Self {
        Self { geometry, media: vec![initial; geometry.capacity() as usize], kind }
    }
    fn check(&self, offset: u32, len: u32, unit: u32, val: Result<(), GeometryError>) -> Result<(), Refused> {
        if self.kind == Kind::OffsetOnlyBounds {
            // alignment enforced properly, bounds only on the offset
            if offset & (unit - 1) != 0 || len & (unit - 1) != 0 {
                return Err(Refused);
            }
            if offset > self.geometry.capacity() {
                return Err(Refused);
            }
            return Ok(());
        }
        val.map_err(|_| Refused)
    }
}

impl StableStorage for Dev {
    type Error = Refused;
    fn geometry(&self) -> Geometry { self.geometry }
    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Refused> {
        let len = u32::try_from(dst.len()).map_err(|_| Refused)?;
        let v = self.geometry.validate_read(offset, len);
        self.check(offset, len, self.geometry.read_size(), v)?;
        let start = offset as usize;
        match self.media.get(start..start + dst.len()) {
            Some(s) => dst.copy_from_slice(s),
            None => dst.fill(0xFF),
        }
        Ok(())
    }
    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Refused> {
        let len = u32::try_from(src.len()).map_err(|_| Refused)?;
        let v = self.geometry.validate_program(offset, len);
        self.check(offset, len, self.geometry.program_size(), v)?;
        let start = offset as usize;
        if let Some(t) = self.media.get_mut(start..start + src.len()) {
            for (c, w) in t.iter_mut().zip(src) { *c &= *w; }
        }
        Ok(())
    }
    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Refused> {
        let v = self.geometry.validate_erase(offset, len);
        self.check(offset, len, self.geometry.erase_size(), v)?;
        if self.kind == Kind::EraseDoesNothing { return Ok(()); }
        let start = offset as usize;
        let end = start + len as usize;
        let cap = self.media.len();
        if let Some(t) = self.media.get_mut(start..end.min(cap)) { t.fill(0xFF); }
        Ok(())
    }
    fn barrier(&mut self) -> Result<(), Refused> { Ok(()) }
}

#[test]
fn probe_a_offset_only_bounds_passes_the_whole_suite() {
    let geometry = Geometry::new(1024, 64, 4, 2).unwrap();
    let mut dev = Dev::new(geometry, Kind::OffsetOnlyBounds, 0xFF);
    let region = Region::whole_device(geometry).unwrap();
    let mut buffer = [0_u8; 64];
    let report = run(&mut dev, region, &mut buffer).expect("starts");
    println!("PROBE A verdict = {:?}", report.verdict());
    // and the adapter is genuinely broken:
    let mut dst = [0_u8; 4];
    println!(
        "PROBE A straddling read accepted = {:?}",
        dev.read(1024 - 2, &mut dst).is_ok()
    );
}

#[test]
fn probe_b_noop_erase_on_zeroed_media_passes() {
    let geometry = Geometry::new(1024, 64, 64, 2).unwrap();
    let mut dev = Dev::new(geometry, Kind::EraseDoesNothing, 0x00);
    let region = Region::whole_device(geometry).unwrap();
    let mut buffer = [0_u8; 256];
    let report = run(&mut dev, region, &mut buffer).expect("starts");
    println!("PROBE B verdict = {:?}", report.verdict());
    for (case, outcome) in report.entries() {
        println!("   {:<55} {:?}", case.name, outcome);
    }
}

#[test]
fn probe_c_suite_erases_outside_the_region_on_a_clamping_adapter() {
    // A driver that validates the offset but clamps the length: the classic bug.
    let geometry = Geometry::new(1024, 64, 4, 2).unwrap();
    let mut dev = Dev::new(geometry, Kind::OffsetOnlyBounds, 0x00);
    // Region = blocks 0..3 only. Everything from 192 up is the caller's data.
    let region = Region::new(geometry, 0, 192).unwrap();
    let mut buffer = [0_u8; 64];
    let before = dev.media.clone();
    let _ = run(&mut dev, region, &mut buffer).expect("starts");
    let end = region.end() as usize;
    let changed: Vec<usize> = (end..1024).filter(|i| before[*i] != dev.media[*i]).collect();
    println!(
        "PROBE C bytes mutated outside the region: {} (first {:?}, last {:?})",
        changed.len(),
        changed.first(),
        changed.last()
    );
}
