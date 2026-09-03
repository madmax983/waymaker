//! The bytes the suite is allowed to destroy.
//!
//! A conformance suite has to erase and program to prove anything, and the device it is
//! handed is usually the one the firmware is running from. Naming the region explicitly is
//! what makes "run the suite against your driver" an instruction somebody can follow on
//! real hardware rather than one they follow once.

use core::fmt;

use waymaker_flash::storage::Geometry;

/// How many erase blocks a region must have.
///
/// Three, and each one is load-bearing. The suite proves an erase is confined to the block
/// it names, which takes a neighbour to watch; and the across-reset witness of
/// [`crate::durability`] puts its acknowledged witness, its seal and its unacknowledged
/// witness in three *different* blocks, because two of them sharing a block would make a
/// single interrupted erase look like a barrier that failed to order.
pub const REQUIRED_ERASE_BLOCKS: u32 = 3;

/// A geometry-checked window of a device that a conformance run may destroy.
///
/// # Invariants
///
/// Erase-block aligned at both ends, inside the capacity, and at least
/// [`REQUIRED_ERASE_BLOCKS`] blocks long. [`Region::new`] is the only constructor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Region {
    geometry: Geometry,
    offset: u32,
    len: u32,
}

impl Region {
    /// Names `offset..offset + len` as expendable, or refuses to.
    ///
    /// # Errors
    ///
    /// [`RegionError::NotEraseAligned`] if either end is not on an erase block,
    /// [`RegionError::OutOfBounds`] if the window leaves the device, and
    /// [`RegionError::TooFewEraseBlocks`] if it is shorter than
    /// [`REQUIRED_ERASE_BLOCKS`] blocks.
    pub const fn new(geometry: Geometry, offset: u32, len: u32) -> Result<Self, RegionError> {
        let block = geometry.erase_size();
        if offset & (block - 1) != 0 || len & (block - 1) != 0 {
            return Err(RegionError::NotEraseAligned);
        }
        match offset.checked_add(len) {
            Some(end) if end <= geometry.capacity() => {}
            _ => return Err(RegionError::OutOfBounds),
        }
        if len >> block.trailing_zeros() < REQUIRED_ERASE_BLOCKS {
            return Err(RegionError::TooFewEraseBlocks);
        }
        Ok(Self {
            geometry,
            offset,
            len,
        })
    }

    /// The whole device as an expendable region.
    ///
    /// # Errors
    ///
    /// As [`Region::new`]. A device with fewer than [`REQUIRED_ERASE_BLOCKS`] erase blocks
    /// cannot be conformance-tested at all, which is a thing to be told rather than a thing
    /// to discover from a suite that passed.
    pub const fn whole_device(geometry: Geometry) -> Result<Self, RegionError> {
        Self::new(geometry, 0, geometry.capacity())
    }

    /// The geometry this region was checked against.
    #[must_use]
    pub const fn geometry(self) -> Geometry {
        self.geometry
    }

    /// The first byte of the region.
    #[must_use]
    pub const fn offset(self) -> u32 {
        self.offset
    }

    /// How many bytes the region covers.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.len
    }

    /// Whether the region covers nothing.
    ///
    /// Always `false` — [`Region::new`] rejects a region shorter than
    /// [`REQUIRED_ERASE_BLOCKS`] erase blocks, and an erase block is never zero bytes. It
    /// exists because a type with a `len` and no `is_empty` is a clippy lint, and because a
    /// reader who reaches for it should be told the answer is settled rather than left to
    /// wonder.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// One past the last byte of the region.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.offset + self.len
    }

    /// The first byte of the `index`-th erase block of the region.
    ///
    /// `None` past the end of the region, which is what keeps a case that walks blocks from
    /// walking off one.
    #[must_use]
    pub const fn block(self, index: u32) -> Option<u32> {
        let Some(step) = self.geometry.erase_size().checked_mul(index) else {
            return None;
        };
        if step >= self.len {
            return None;
        }
        Some(self.offset + step)
    }
}

/// Why a window is not a region a conformance run may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionError {
    /// One of the two ends is not on an erase block boundary.
    NotEraseAligned,
    /// The window leaves the device, or its end would overflow.
    OutOfBounds,
    /// Fewer than [`REQUIRED_ERASE_BLOCKS`] erase blocks.
    TooFewEraseBlocks,
}

impl RegionError {
    /// A short static description of this refusal.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotEraseAligned => "region is not erase-block aligned",
            Self::OutOfBounds => "region is out of bounds",
            Self::TooFewEraseBlocks => "region is shorter than three erase blocks",
        }
    }
}

impl fmt::Display for RegionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for RegionError {}
