//! The `embedded-storage` port.
//!
//! Issue [#21](https://github.com/madmax983/waymaker/issues/21) is done when "an
//! `embedded-storage` implementation can be adapted without `embedded-storage` becoming a
//! kernel dependency". Both halves of that sentence are real things:
//!
//! * the adapter is [`NorFlashStorage`], here, above the layers, where a third-party
//!   dependency is allowed to be;
//! * "without becoming a kernel dependency" is `policy::LAYERS`, in which every layer's
//!   `may_depend_on_external` list is empty — so `waymaker-core` growing this dependency
//!   fails `kernel-zero-dependencies` and `waymaker-flash` growing it fails
//!   `dependency-direction`, and neither is a thing a reviewer has to notice.
//!
//! # What the port has to do that the driver does not
//!
//! **Validate.** `embedded-storage`'s `check_read`, `check_write` and `check_erase` are
//! helpers a driver *may* call; design document §12 puts the obligation on the adapter, so
//! [`NorFlashStorage`] validates against its own [`Geometry`] before the driver is reached.
//! A driver that also validates is asked nothing it cannot answer; one that does not is
//! still safe behind this.
//!
//! **Describe the device.** `embedded-storage` states its units as associated constants and
//! its capacity as a `usize`; a [`Geometry`] is four `u32`s that nest and are powers of two.
//! Not every NOR flash is describable that way, and [`NorFlashStorage::new`] says so at
//! construction rather than at the first misaligned write.
//!
//! **Say what the barrier means.** `NorFlash::write` and `NorFlash::erase` are blocking and
//! complete when they return: there is no write-behind to flush and no reordering to settle,
//! so [`barrier`](StableStorage::barrier) is a no-op. That is a claim about *this* trait, and
//! it is why a driver with a cache in front of it needs a port of its own rather than this
//! one.
//!
//! # What the port does not promise
//!
//! `NorFlash::write` forbids writing the same word twice; only a
//! [`MultiwriteNorFlash`](embedded_storage::nor_flash::MultiwriteNorFlash) allows it.
//! Waymaker's journal is append-only and never rewrites a programmed word, so the plain
//! trait is enough — but a caller that does rewrite a word is outside what the driver
//! beneath this promises, and §12 leaves one-way bit rules to the driver for exactly that
//! reason.

use embedded_storage::nor_flash::NorFlash;
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// Why a NOR flash cannot be described as a [`Geometry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortGeometryError {
    /// A unit or the capacity does not fit in the 32-bit words §12's offsets are.
    UnitDoesNotFitInAWord,
    /// The units are not a geometry Waymaker can address.
    Geometry(GeometryError),
}

impl PortGeometryError {
    /// A short static description of this refusal.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::UnitDoesNotFitInAWord => "a unit or the capacity does not fit in 32 bits",
            Self::Geometry(error) => error.message(),
        }
    }
}

impl core::fmt::Display for PortGeometryError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for PortGeometryError {}

/// How a ported driver refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortError<E> {
    /// The operation is not one this geometry permits, and the driver was never called.
    Geometry(GeometryError),
    /// The driver itself refused.
    Driver(E),
}

impl<E: core::fmt::Display> core::fmt::Display for PortError<E> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Geometry(error) => formatter.write_str(error.message()),
            Self::Driver(error) => error.fmt(formatter),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> core::error::Error for PortError<E> {}

/// An `embedded-storage` [`NorFlash`] presented as design document §12's [`StableStorage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NorFlashStorage<F> {
    flash: F,
    geometry: Geometry,
}

impl<F: NorFlash> NorFlashStorage<F> {
    /// Describes `flash` as a [`Geometry`] and wraps it, or refuses to.
    ///
    /// The capacity comes from `ReadNorFlash::capacity`, and the three units from
    /// `READ_SIZE`, `WRITE_SIZE` and `ERASE_SIZE`. A part whose units do not nest, are not
    /// powers of two, or do not fit in 32 bits is refused here rather than at the first
    /// operation that would have gone somewhere unexpected.
    ///
    /// # Errors
    ///
    /// [`PortGeometryError::UnitDoesNotFitInAWord`] if a `usize` will not narrow, and
    /// [`PortGeometryError::Geometry`] carrying [`Geometry::new`]'s own refusal otherwise.
    pub fn new(flash: F) -> Result<Self, PortGeometryError> {
        let capacity = narrow(flash.capacity())?;
        let erase = narrow(F::ERASE_SIZE)?;
        let program = narrow(F::WRITE_SIZE)?;
        let read = narrow(F::READ_SIZE)?;
        let geometry =
            Geometry::new(capacity, erase, program, read).map_err(PortGeometryError::Geometry)?;
        Ok(Self { flash, geometry })
    }

    /// The geometry this port derived from the driver's own constants.
    #[must_use]
    pub const fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// The driver, borrowed.
    #[must_use]
    pub const fn flash(&self) -> &F {
        &self.flash
    }

    /// The driver, borrowed mutably.
    pub const fn flash_mut(&mut self) -> &mut F {
        &mut self.flash
    }

    /// The driver, back.
    #[must_use]
    pub fn into_flash(self) -> F {
        self.flash
    }
}

/// A `usize` as the `u32` §12's offsets and lengths are.
fn narrow(value: usize) -> Result<u32, PortGeometryError> {
    u32::try_from(value).map_err(|_| PortGeometryError::UnitDoesNotFitInAWord)
}

impl<F: NorFlash> StableStorage for NorFlashStorage<F> {
    type Error = PortError<F::Error>;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(dst.len())
            .map_err(|_| PortError::Geometry(GeometryError::OutOfBounds))?;
        self.geometry
            .validate_read(offset, len)
            .map_err(PortError::Geometry)?;
        self.flash.read(offset, dst).map_err(PortError::Driver)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(src.len())
            .map_err(|_| PortError::Geometry(GeometryError::OutOfBounds))?;
        self.geometry
            .validate_program(offset, len)
            .map_err(PortError::Geometry)?;
        self.flash.write(offset, src).map_err(PortError::Driver)
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.geometry
            .validate_erase(offset, len)
            .map_err(PortError::Geometry)?;
        // §12 names a length and `embedded-storage` names an end. The addition cannot
        // overflow: `validate_erase` has already refused anything whose end passes the
        // capacity, and a capacity is a `u32`.
        let end = offset
            .checked_add(len)
            .ok_or(PortError::Geometry(GeometryError::OutOfBounds))?;
        self.flash.erase(offset, end).map_err(PortError::Driver)
    }

    /// Nothing to do, and that is a claim rather than an omission.
    ///
    /// `NorFlash`'s `write` and `erase` are blocking: when they return, the media has been
    /// changed. There is no write-behind to flush and no reordering to settle, so every
    /// mutation before this call is already durable and every mutation after it is already
    /// ordered behind them.
    fn barrier(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
