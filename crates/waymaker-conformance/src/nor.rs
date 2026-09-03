//! The `embedded-storage` port.

use embedded_storage::nor_flash::NorFlash;
use waymaker_flash::storage::{Geometry, GeometryError, StableStorage};

/// Why a NOR flash cannot be described as a [`Geometry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortGeometryError {
    /// A unit or the capacity does not fit in a 32-bit word.
    UnitDoesNotFitInAWord,
    /// The units are not a geometry Waymaker can address.
    Geometry(GeometryError),
}

/// How a ported driver refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortError<E> {
    /// The operation is not one this geometry permits. The driver was never called.
    Geometry(GeometryError),
    /// The driver itself refused.
    Driver(E),
}

/// A [`NorFlash`] presented as design document §12's [`StableStorage`].
#[derive(Clone, Copy, Debug)]
pub struct NorFlashStorage<F> {
    flash: F,
    geometry: Geometry,
}

impl<F: NorFlash> NorFlashStorage<F> {
    /// Describes `flash` as a [`Geometry`] and wraps it, or refuses to.
    ///
    /// # Errors
    ///
    /// [`PortGeometryError`] if the driver's units cannot be a Waymaker geometry.
    pub fn new(flash: F) -> Result<Self, PortGeometryError> {
        let geometry = Geometry::new(0, 1, 1, 1).map_err(PortGeometryError::Geometry)?;
        Ok(Self { flash, geometry })
    }

    /// The geometry this port derived.
    #[must_use]
    pub const fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// The driver, borrowed.
    #[must_use]
    pub const fn flash(&self) -> &F {
        &self.flash
    }

    /// The driver, mutably.
    #[must_use]
    pub const fn flash_mut(&mut self) -> &mut F {
        &mut self.flash
    }

    /// The driver, back.
    #[must_use]
    pub fn into_flash(self) -> F {
        self.flash
    }
}

impl<F: NorFlash> StableStorage for NorFlashStorage<F> {
    type Error = PortError<F::Error>;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.read(offset, dst).map_err(PortError::Driver)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        self.flash.write(offset, src).map_err(PortError::Driver)
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        self.flash
            .erase(offset, offset + len)
            .map_err(PortError::Driver)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
