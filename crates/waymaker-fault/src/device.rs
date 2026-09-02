//! Media that behaves like media.
//!
//! A [`Device`] is a byte array with three properties real NOR flash has and a `Vec<u8>`
//! does not: it starts erased rather than zeroed, programming can only clear bits, and an
//! operation the geometry forbids never reaches it. Everything a crash injector does is
//! layered on top of this; getting these three wrong would make every fault test above it
//! agree with a model rather than with hardware.

use core::fmt;

use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// The byte an erased cell reads as.
///
/// Public because a test that asserts what a stale tail looks like should not have to
/// write `0xFF` and hope.
pub const ERASED: u8 = 0xFF;

/// What the model does when a program asks for a bit the media has already cleared.
///
/// Real NOR silently drops it, which is why firmware bugs of this shape survive testing on
/// a `Vec<u8>` model that assigns instead of masking. Both behaviours are here on purpose:
/// [`Nor`](OneWayBits::Nor) is what the hardware does and is the default, and
/// [`Rejected`](OneWayBits::Rejected) is a strictness knob for a test that wants the bug
/// reported rather than absorbed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OneWayBits {
    /// Clear the bits that can be cleared and silently ignore the rest, as hardware does.
    #[default]
    Nor,
    /// Refuse the whole program, touching no media.
    Rejected,
}

/// Anything a modelled device can refuse to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaultError {
    /// The operation is not one this geometry permits.
    Geometry(GeometryError),
    /// A program asked for a bit that only an erase can restore.
    ///
    /// Only ever returned under [`OneWayBits::Rejected`].
    BitSetWithoutErase,
    /// The power went away. Nothing after this observation happened, or ever will.
    PowerLoss,
    /// The injected failure of a `program` or an `erase`: the call returns an error, media
    /// may already have changed, and the caller carries on.
    Injected,
}

impl FaultError {
    /// A short static description of this failure.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Geometry(error) => error.message(),
            Self::BitSetWithoutErase => "a program would set a bit that only an erase restores",
            Self::PowerLoss => "power was lost; nothing after this point happened",
            Self::Injected => "the injected failure of a program or an erase",
        }
    }
}

impl From<GeometryError> for FaultError {
    fn from(error: GeometryError) -> Self {
        Self::Geometry(error)
    }
}

impl fmt::Display for FaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for FaultError {}

/// An in-memory device with a geometry.
///
/// # Invariants
///
/// The image is always exactly `geometry.capacity()` bytes. Every byte is `ERASED` until
/// something programs it, and no operation this type performs can set a bit that was
/// cleared without an intervening erase.
#[derive(Clone, Debug)]
pub struct Device {
    geometry: Geometry,
    media: Vec<u8>,
    bits: OneWayBits,
}

impl Device {
    /// An erased device with the hardware bit rule.
    #[must_use]
    pub fn new(geometry: Geometry) -> Self {
        Self::with_bit_rule(geometry, OneWayBits::Nor)
    }

    /// An erased device that reports one-way bit violations the way `rule` says.
    #[must_use]
    pub fn with_bit_rule(geometry: Geometry, rule: OneWayBits) -> Self {
        Self {
            geometry,
            media: vec![ERASED; geometry.capacity() as usize],
            bits: rule,
        }
    }

    /// The bytes as they stand, which is what a reader after a reset would see.
    #[must_use]
    pub fn image(&self) -> &[u8] {
        &self.media
    }

    /// The bytes, taken.
    #[must_use]
    pub fn into_image(self) -> Vec<u8> {
        self.media
    }

    /// Programs `src` at `offset` with no geometry check, as a torn write does.
    ///
    /// Crate-internal because it is not an operation a device offers: a torn write is a
    /// prefix of a *validated* operation, so [`crate::Session`] validates the whole write
    /// first and only then applies the part of it that survived. Exposing this would let a
    /// caller program across an erase-block boundary at an offset the geometry forbids,
    /// which is the one thing [`Geometry`] exists to stop.
    pub(crate) fn apply_program(&mut self, offset: u32, src: &[u8]) {
        let Some(target) = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(src.len()).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get_mut(start..end))
        else {
            return;
        };
        for (cell, wanted) in target.iter_mut().zip(src) {
            *cell &= *wanted;
        }
    }

    /// Erases `offset..offset + len` with no geometry check, as an interrupted erase does.
    pub(crate) fn apply_erase(&mut self, offset: u32, len: u32) {
        let Some(target) = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(len).ok())
            .and_then(|(start, len)| start.checked_add(len).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get_mut(start..end))
        else {
            return;
        };
        target.fill(ERASED);
    }

    /// Whether programming `src` at `offset` would ask for a bit only an erase can restore.
    pub(crate) fn would_set_a_bit(&self, offset: u32, src: &[u8]) -> bool {
        let Some(target) = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(src.len()).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get(start..end))
        else {
            return false;
        };
        target
            .iter()
            .zip(src)
            .any(|(cell, wanted)| wanted & !cell != 0)
    }

    /// The bit rule this device was built with.
    pub(crate) const fn bit_rule(&self) -> OneWayBits {
        self.bits
    }
}

impl StableStorage for Device {
    type Error = FaultError;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(dst.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_read(offset, len)?;
        let source = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(dst.len()).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get(start..end))
            .ok_or(GeometryError::OutOfBounds)?;
        dst.copy_from_slice(source);
        Ok(())
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(src.len()).map_err(|_| GeometryError::OutOfBounds)?;
        self.geometry.validate_program(offset, len)?;
        if self.bits == OneWayBits::Rejected && self.would_set_a_bit(offset, src) {
            return Err(FaultError::BitSetWithoutErase);
        }
        self.apply_program(offset, src);
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.geometry.validate_erase(offset, len)?;
        self.apply_erase(offset, len);
        Ok(())
    }

    /// Always succeeds.
    ///
    /// A [`Device`] has no write-behind to flush and no reordering to settle: it *is* the
    /// durable state. Durability is modelled one level up, by [`crate::Session`], because
    /// what a barrier means is a property of when the power went away rather than of the
    /// bytes.
    fn barrier(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
