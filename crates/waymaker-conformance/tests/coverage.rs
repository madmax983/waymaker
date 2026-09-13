//! Whether the suite reads back everything it legally mutates.
//!
//! Issue #70's first check: record the byte ranges a case touches, and make sure a *later*
//! read covers every one of them. This catches the exact family of bug the issue found — a
//! byte a program or erase call changes, that no read after it ever looks at again.
//!
//! Order matters here and is tracked explicitly: a read before a mutation says nothing about
//! what the mutation left behind, so it does not count as covering it. Without that, an
//! early case that reads the whole region — `barrier_changes_no_media` does, by design —
//! would silently mask every later case's own verification, which a Codex review of this
//! file caught: with ordering ignored, stripping every check out of
//! `multi_unit_program_is_legal` still passed this test, because that earlier full-region
//! read looked like coverage for bytes the later case had not yet mutated.
//!
//! # What this does not prove
//!
//! That one case's own verification is complete. It proves only that the run as a whole
//! does not leave a mutated byte unobserved after it was last changed — a case with a short
//! check can still pass here if a *later*, unrelated case happens to read the same bytes
//! afterwards. Proving each case's own check is complete needs the other two parts of issue
//! #70, filed as issue #130.

use waymaker_conformance::{Region, run};
use waymaker_flash::storage::{Geometry, StableStorage};

/// One mutation or read, in the order it happened.
#[derive(Clone, Copy)]
struct Touch {
    offset: u32,
    len: u32,
    sequence: u64,
}

/// Wraps a [`StableStorage`] and records which bytes were mutated and read, in order.
struct TouchRecorder<S> {
    inner: S,
    mutated: Vec<Touch>,
    read: Vec<Touch>,
    sequence: u64,
}

impl<S: StableStorage> TouchRecorder<S> {
    const fn new(inner: S) -> Self {
        Self {
            inner,
            mutated: Vec::new(),
            read: Vec::new(),
            sequence: 0,
        }
    }

    /// The next sequence number, advancing the counter.
    const fn next_sequence(&mut self) -> u64 {
        let sequence = self.sequence;
        self.sequence += 1;
        sequence
    }

    /// Every byte some successful program or erase named that no *later* read covers.
    fn unread_after_mutation(&self) -> Vec<u32> {
        let mut gaps = Vec::new();
        for mutation in &self.mutated {
            for byte in mutation.offset..mutation.offset.saturating_add(mutation.len) {
                let covered = self.read.iter().any(|read| {
                    read.sequence > mutation.sequence
                        && byte >= read.offset
                        && byte < read.offset.saturating_add(read.len)
                });
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
        let sequence = self.next_sequence();
        if result.is_ok() {
            if let Ok(len) = u32::try_from(dst.len()) {
                if len > 0 {
                    self.read.push(Touch {
                        offset,
                        len,
                        sequence,
                    });
                }
            }
        }
        result
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let result = self.inner.program(offset, src);
        let sequence = self.next_sequence();
        if result.is_ok() {
            if let Ok(len) = u32::try_from(src.len()) {
                if len > 0 {
                    self.mutated.push(Touch {
                        offset,
                        len,
                        sequence,
                    });
                }
            }
        }
        result
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        let result = self.inner.erase(offset, len);
        let sequence = self.next_sequence();
        if result.is_ok() && len > 0 {
            self.mutated.push(Touch {
                offset,
                len,
                sequence,
            });
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
        "{} byte(s) were mutated and never read back afterwards; first at offset {}",
        gaps.len(),
        gaps.first().copied().unwrap_or_default()
    );
}
