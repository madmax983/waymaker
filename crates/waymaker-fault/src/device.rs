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
/// [`Nor`](OneWayBits::Absorbed) is what the hardware does and is the default, and
/// [`Rejected`](OneWayBits::Rejected) is a strictness knob for a test that wants the bug
/// reported rather than absorbed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OneWayBits {
    /// Clear the bits that can be cleared and silently ignore the rest, as hardware does.
    #[default]
    Absorbed,
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
    ///
    /// Named for the *failure* rather than for the injection, because
    /// [`PowerLoss`](Self::PowerLoss) is injected too: what tells them apart is that this
    /// one leaves the device alive.
    InjectedFailure,
}

impl FaultError {
    /// A short static description of this failure.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Geometry(error) => error.message(),
            Self::BitSetWithoutErase => "a program would set a bit that only an erase restores",
            Self::PowerLoss => "power was lost; nothing after this point happened",
            Self::InjectedFailure => "the injected failure of a program or an erase",
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
        Self::with_bit_rule(geometry, OneWayBits::Absorbed)
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
    /// Returns whether any cell actually changed, which is not the same as whether bytes
    /// were offered: AND-masking `0xFF` over erased media is the identity, and a record
    /// whose only write was that has nothing on media for recovery to find.
    pub(crate) fn apply_program(&mut self, offset: u32, src: &[u8]) -> bool {
        let Some(target) = usize::try_from(offset)
            .ok()
            .and_then(|start| start.checked_add(src.len()).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get_mut(start..end))
        else {
            return false;
        };
        let mut changed = false;
        for (cell, wanted) in target.iter_mut().zip(src) {
            let after = *cell & *wanted;
            changed |= after != *cell;
            *cell = after;
        }
        changed
    }

    /// Erases `offset..offset + len` with no geometry check, as an interrupted erase does.
    ///
    /// Returns whether any cell actually changed. Erasing an already-erased block — the
    /// bank-prepare shape, which is not exotic — changes nothing, and a record whose only
    /// mutation was that has nothing on media either.
    pub(crate) fn apply_erase(&mut self, offset: u32, len: u32) -> bool {
        let Some(target) = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(len).ok())
            .and_then(|(start, len)| start.checked_add(len).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get_mut(start..end))
        else {
            return false;
        };
        let changed = target.iter().any(|cell| *cell != ERASED);
        target.fill(ERASED);
        changed
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

    /// Whether programming `src` at `offset` would change any cell.
    ///
    /// Not the same question as "does it write bytes". Programming `0xFF` over erased media
    /// is the identity, which is what makes a write torn inside a frame's padding
    /// indistinguishable from one that completed — the bytes that did not land were the
    /// bytes that would have changed nothing.
    pub(crate) fn program_would_change(&self, offset: u32, src: &[u8]) -> bool {
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
            .any(|(cell, wanted)| cell & wanted != *cell)
    }

    /// Whether erasing `offset..offset + len` would change any cell.
    pub(crate) fn erase_would_change(&self, offset: u32, len: u32) -> bool {
        let Some(target) = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(len).ok())
            .and_then(|(start, len)| start.checked_add(len).map(|end| (start, end)))
            .and_then(|(start, end)| self.media.get(start..end))
        else {
            return false;
        };
        target.iter().any(|cell| *cell != ERASED)
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
        let _changed = self.apply_program(offset, src);
        Ok(())
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.geometry.validate_erase(offset, len)?;
        let _changed = self.apply_erase(offset, len);
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
