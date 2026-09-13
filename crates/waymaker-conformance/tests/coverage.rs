//! Whether the suite reads back everything it legally mutates.
//!
//! Issue #70's first check: record the byte ranges a case touches, and make sure a later
//! read covers every one of them. This catches the exact family of bug the issue found — a
//! byte a program or erase call changes, that no read in the rest of the run ever looks at
//! again.
//!
//! # What this does not prove
//!
//! That one case's own verification is complete. It proves only that the run as a whole
//! does not leave a mutated byte unobserved — a case with a short check can still pass here
//! if a *later*, unrelated case happens to read the same bytes. Proving each case's own
//! check is complete needs the other two parts of issue #70, filed as issue #130.

use waymaker_conformance::{Region, run};
use waymaker_flash::storage::{Geometry, StableStorage};

/// Wraps a [`StableStorage`] and records which bytes were mutated and which were read.
struct TouchRecorder<S> {
    inner: S,
    mutated: Vec<(u32, u32)>,
    read: Vec<(u32, u32)>,
}

impl<S: StableStorage> TouchRecorder<S> {
    const fn new(inner: S) -> Self {
        Self {
            inner,
            mutated: Vec::new(),
            read: Vec::new(),
        }
    }

    /// Every byte some successful program or erase named that no read afterwards covers.
    fn unread_after_mutation(&self) -> Vec<u32> {
        let mut gaps = Vec::new();
        for &(offset, len) in &self.mutated {
            for byte in offset..offset.saturating_add(len) {
                let covered = self
                    .read
                    .iter()
                    .any(|&(start, span)| byte >= start && byte < start.saturating_add(span));
                if !covered {
                    gaps.push(byte);
                }
            }
        }
        gaps
    }
}

impl<S: StableStorage> StableStorage for TouchRecorder<S> {
    type Error = S::Error;

    fn geometry(&self) -> Geometry {
        self.inner.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let result = self.inner.read(offset, dst);
        if result.is_ok() {
            if let Ok(len) = u32::try_from(dst.len()) {
                if len > 0 {
                    self.read.push((offset, len));
                }
            }
        }
        result
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let result = self.inner.program(offset, src);
        if result.is_ok() {
            if let Ok(len) = u32::try_from(src.len()) {
                if len > 0 {
                    self.mutated.push((offset, len));
                }
            }
        }
        result
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        let result = self.inner.erase(offset, len);
        if result.is_ok() && len > 0 {
            self.mutated.push((offset, len));
        }
        result
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.inner.barrier()
    }
}

/// A geometry in which every unit is wider than the one below it, so no case is exempt.
fn nested() -> Geometry {
    let Ok(geometry) = Geometry::new(1024, 64, 4, 2) else {
        unreachable!("1024 is whole 64-byte blocks of whole 4-byte units of 2-byte reads")
    };
    geometry
}

#[test]
fn every_legally_mutated_byte_is_read_back_before_the_run_ends() {
    let geometry = nested();
    let mut storage = TouchRecorder::new(waymaker_fault::Device::new(geometry));
    let Ok(region) = Region::whole_device(geometry) else {
        unreachable!("sixteen erase blocks is more than four")
    };
    let mut buffer = [0_u8; 64];

    let report = run(&mut storage, region, &mut buffer).expect("the run starts");
    assert_eq!(report.verdict(), Ok(()), "{report:?}");

    let gaps = storage.unread_after_mutation();
    assert!(
        gaps.is_empty(),
        "{} byte(s) were mutated and never read back; first at offset {}",
        gaps.len(),
        gaps.first().copied().unwrap_or_default()
    );
}
