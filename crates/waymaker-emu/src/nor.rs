//! The part the emulated boot runs on: NOR flash modelled in the emulator's RAM.
//!
//! # Why there is a model here at all
//!
//! Neither QEMU machine this image is started on has a flash part a driver can program.
//! `-machine microbit` models an nRF51822's CPU and a handful of its peripherals; its NVMC
//! is not a device this code can write a journal through. So the media is an array, and the
//! honest reading of that is in [ADR 0040]: what the emulator adds is *execution* — the
//! rig's arithmetic, its branches and its `u64` work running on ARMv6-M and ARMv7E-M rather
//! than being compiled for them — and not a part.
//!
//! # Why it is not a `[u8; N]` that accepts everything
//!
//! Because a double that agreed with every write would let the rig through on arithmetic a
//! real part refuses, and the run would look like a pass. This one behaves the way
//! `waymaker-fault`'s host model behaves and the way NOR behaves: erased is [`ERASED`],
//! programming only clears bits, and an operation the geometry forbids never reaches a cell.
//!
//! That claim is not left as a comment either. The boot runs `waymaker-conformance`'s suite
//! over this model *before* it runs the rig over it, which is the crate that exists to ask an
//! adapter whether it obeys design document §12 — so the emulated run reports on a media
//! model that was interrogated in the same boot rather than on one that was described in a
//! doc comment.
//!
//! # Why it is presented through the `embedded-storage` port
//!
//! [`waymaker_conformance::nor::NorFlashStorage`] is the adapter issue #21 landed and issue
//! #27 asked the rig to be driven through. Writing a second [`StableStorage`] here would put
//! an adapter in the boot path that nothing else in the workspace ships, and the emulated run
//! would be reporting on it. So this type implements `embedded-storage`'s traits and the port
//! does the translation, exactly as `waymaker-rig/tests/port.rs` does on the host.
//!
//! [`StableStorage`]: waymaker_flash::storage::StableStorage
//! [ADR 0040]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md

use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
use waymaker_flash::storage::{Geometry, GeometryError};

/// The value every cell of an erased part reads as.
///
/// A constant rather than something learned from the model, for the reason
/// `waymaker-conformance` states about the device under test: a model that taught its reader
/// what "erased" meant could teach it anything.
pub const ERASED: u8 = 0xFF;

/// How many bytes the modelled part holds.
///
/// Small on purpose. The image is linked against 16 KiB of RAM — the micro:bit's — and this
/// array is a local of the boot rather than a `static`, so it is spent out of that. Sixteen
/// erase blocks is more than the rig's window and the conformance region need between them,
/// and every byte beyond that is stack the tightest of the three machines does not have.
pub const CAPACITY: u32 = 4096;

/// The erase block, in bytes.
pub const ERASE_BYTES: u32 = 256;

/// The program unit, in bytes.
pub const PROGRAM_BYTES: u32 = 4;

/// The read unit, in bytes.
pub const READ_BYTES: u32 = 1;

/// The four units, as design document §12's [`Geometry`].
///
/// # Errors
///
/// [`GeometryError`] if the four constants above stop nesting — which is a compile-time
/// mistake reported at run time, because `Geometry::new` is the one validator and this crate
/// does not get to have a second opinion about what a legal part is.
pub const fn geometry() -> Result<Geometry, GeometryError> {
    Geometry::new(CAPACITY, ERASE_BYTES, PROGRAM_BYTES, READ_BYTES)
}

/// NOR flash modelled in RAM: erased is [`ERASED`], and programming only clears bits.
pub struct Nor {
    cells: [u8; CAPACITY as usize],
}

impl Default for Nor {
    fn default() -> Self {
        Self::new()
    }
}

impl Nor {
    /// A part with every cell erased.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cells: [ERASED; CAPACITY as usize],
        }
    }

    /// Returns the part to the state [`new`](Self::new) leaves it in.
    ///
    /// The boot runs two things over one part — the conformance suite, then the rig — and a
    /// second `Nor` would be a second four-kilobyte local on a stack that has sixteen.
    pub const fn reset(&mut self) {
        self.cells = [ERASED; CAPACITY as usize];
    }

    /// The bytes at `offset`, or `None` if the request leaves the part.
    fn span(&self, offset: u32, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(len)?;
        self.cells.get(start..end)
    }

    /// The bytes at `offset`, mutably, or `None` if the request leaves the part.
    fn span_mut(&mut self, offset: u32, len: usize) -> Option<&mut [u8]> {
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(len)?;
        self.cells.get_mut(start..end)
    }

    /// Copies `bytes.len()` bytes from `offset`.
    ///
    /// # Errors
    ///
    /// [`NorFlashErrorKind::OutOfBounds`] if the request leaves the part.
    pub fn read_at(&self, offset: u32, bytes: &mut [u8]) -> Result<(), NorFlashErrorKind> {
        let source = self
            .span(offset, bytes.len())
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        bytes.copy_from_slice(source);
        Ok(())
    }

    /// Programs `bytes` at `offset`, clearing bits and never setting one.
    ///
    /// # Errors
    ///
    /// [`NorFlashErrorKind::OutOfBounds`] if the request leaves the part, and
    /// [`NorFlashErrorKind::NotAligned`] if the offset or the length is not a whole number of
    /// program units.
    pub fn program_at(&mut self, offset: u32, bytes: &[u8]) -> Result<(), NorFlashErrorKind> {
        let len = u32::try_from(bytes.len()).map_err(|_| NorFlashErrorKind::OutOfBounds)?;
        if offset % PROGRAM_BYTES != 0 || len % PROGRAM_BYTES != 0 {
            return Err(NorFlashErrorKind::NotAligned);
        }
        let target = self
            .span_mut(offset, bytes.len())
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        for (cell, byte) in target.iter_mut().zip(bytes) {
            // The whole of what makes this NOR rather than RAM: a program clears bits and
            // never sets one, so a cell already programmed to 0x00 stays there whatever the
            // caller asked for, and a writer that expected otherwise is caught here rather
            // than on a part.
            *cell &= *byte;
        }
        Ok(())
    }

    /// Erases `len` bytes from `offset`, returning them to [`ERASED`].
    ///
    /// # Errors
    ///
    /// [`NorFlashErrorKind::OutOfBounds`] if the request leaves the part, and
    /// [`NorFlashErrorKind::NotAligned`] if the offset or the length is not a whole number of
    /// erase blocks.
    pub fn erase_at(&mut self, offset: u32, len: u32) -> Result<(), NorFlashErrorKind> {
        if offset % ERASE_BYTES != 0 || len % ERASE_BYTES != 0 {
            return Err(NorFlashErrorKind::NotAligned);
        }
        let len = usize::try_from(len).map_err(|_| NorFlashErrorKind::OutOfBounds)?;
        let target = self
            .span_mut(offset, len)
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        target.fill(ERASED);
        Ok(())
    }
}

// The `embedded-storage` traits are implemented for `&mut Nor` rather than for `Nor`,
// because `NorFlashStorage` owns the flash it wraps and the boot wraps this part twice —
// once for the conformance suite and once for the rig, with a [`Nor::reset`] between them.
// Implementing them on the borrow is what lets one four-kilobyte part serve both without a
// second one on a stack that has sixteen.
impl ErrorType for &mut Nor {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for &mut Nor {
    const READ_SIZE: usize = READ_BYTES as usize;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        (**self).read_at(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.cells.len()
    }
}

impl NorFlash for &mut Nor {
    const WRITE_SIZE: usize = PROGRAM_BYTES as usize;
    const ERASE_SIZE: usize = ERASE_BYTES as usize;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let len = to.checked_sub(from).ok_or(NorFlashErrorKind::OutOfBounds)?;
        (**self).erase_at(from, len)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        (**self).program_at(offset, bytes)
    }
}
