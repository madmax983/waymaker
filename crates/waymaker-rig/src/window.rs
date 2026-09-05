//! One part, carved into an engine area and an instrument area.
//!
//! # Why the rig needs this
//!
//! The rig's witness ([`crate::witness`]) has to be on the same supply as the journal it is
//! a witness to. If the two were separate parts on separate rails, a cut could take one and
//! not the other, and the instrument would then be recording things the engine never got to
//! do — which is not a bug the rig would find, it is a bug the rig would *invent*.
//!
//! But [`BankLayout::new`](waymaker_flash::bank::BankLayout::new) takes a whole
//! [`Geometry`] and gives §10's two banks the whole of it, and every step of
//! `waymaker-flash` compares the geometry a region was validated against with the geometry
//! of the storage it is handed — deliberately, because bounds proved on one device say
//! nothing about another. So the engine has to be handed a device whose capacity *is* the
//! bank area.
//!
//! A [`Window`] is that device: a [`StableStorage`] presenting `bytes` of another one,
//! starting at `base`, as a part of its own.
//!
//! # What it refuses
//!
//! A base or a length that is not a whole number of erase blocks. An erase is the coarsest
//! operation a part has, and a window whose end fell inside a block would be a window whose
//! last erase cleared media it does not own — which is exactly the mistake a carve-up exists
//! to prevent.
//!
//! # What it does not narrow
//!
//! The barrier. §12's barrier orders everything before it against everything after it on the
//! *device*, and a window that claimed to scope one to its own range would be claiming a
//! guarantee no part provides.

use waymaker_flash::storage::{Geometry, StableStorage};

/// Why a part cannot be carved that way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WindowError {
    /// The base or the length is not a whole number of erase blocks.
    Unaligned,
    /// The window reaches past the end of the part.
    PastTheEnd,
    /// A window of no bytes is not a device.
    Empty,
}

impl WindowError {
    /// A short static description of this refusal.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Unaligned => "a window must be a whole number of erase blocks",
            Self::PastTheEnd => "the window reaches past the end of the part",
            Self::Empty => "a window of no bytes is not a device",
        }
    }
}

impl core::fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for WindowError {}

/// Part of one device, presented as a device.
///
/// A borrow rather than an owner. A part is carved into an engine area and an instrument
/// area for the life of one call, not for the life of the program, and a window that took
/// ownership would make "hand the same part to the other window next" a move rather than a
/// reborrow.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Window<'a, S> {
    storage: &'a mut S,
    base: u32,
    geometry: Geometry,
}

impl<'a, S: StableStorage> Window<'a, S> {
    /// The `bytes` of `storage` starting at `base`, as a device of their own.
    ///
    /// # Errors
    ///
    /// [`WindowError::Empty`], [`WindowError::Unaligned`] and [`WindowError::PastTheEnd`].
    pub fn new(storage: &'a mut S, base: u32, bytes: u32) -> Result<Self, WindowError> {
        let part = storage.geometry();
        if bytes == 0 {
            return Err(WindowError::Empty);
        }
        let erase = part.erase_size();
        if base % erase != 0 || bytes % erase != 0 {
            return Err(WindowError::Unaligned);
        }
        let Some(end) = base.checked_add(bytes) else {
            return Err(WindowError::PastTheEnd);
        };
        if end > part.capacity() {
            return Err(WindowError::PastTheEnd);
        }
        let Ok(geometry) = Geometry::new(bytes, erase, part.program_size(), part.read_size())
        else {
            return Err(WindowError::Unaligned);
        };
        Ok(Self {
            storage,
            base,
            geometry,
        })
    }

    /// Where the window starts on the part.
    #[must_use]
    pub const fn base(&self) -> u32 {
        self.base
    }

    /// The part beneath, for a caller that needs it back.
    #[must_use]
    pub const fn inner(&self) -> &S {
        self.storage
    }

    /// The part beneath, mutably. Offsets through this are the part's, not the window's.
    #[must_use]
    pub const fn inner_mut(&mut self) -> &mut S {
        self.storage
    }

    /// The part offset `offset` names, or `None` if it is outside the window.
    ///
    /// The window's own bounds check. `StableStorage`'s implementations validate against a
    /// geometry, and the geometry a window reports is the window's — but an implementation
    /// that trusted the part's validation alone would let a caller reach past the window's
    /// end and hit media the window does not own.
    fn translate(&self, offset: u32, len: u32) -> Option<u32> {
        let end = offset.checked_add(len)?;
        if end > self.geometry.capacity() {
            return None;
        }
        self.base.checked_add(offset)
    }
}

/// How a window refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowFault<E> {
    /// The operation is outside the window.
    Window(WindowError),
    /// The part refused.
    Part(E),
}

impl<E: core::fmt::Display> core::fmt::Display for WindowFault<E> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Window(error) => error.fmt(formatter),
            Self::Part(error) => error.fmt(formatter),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> core::error::Error for WindowFault<E> {}

impl<S: StableStorage> StableStorage for Window<'_, S> {
    type Error = WindowFault<S::Error>;

    fn geometry(&self) -> Geometry {
        self.geometry
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(dst.len()).unwrap_or(u32::MAX);
        let Some(at) = self.translate(offset, len) else {
            return Err(WindowFault::Window(WindowError::PastTheEnd));
        };
        self.storage.read(at, dst).map_err(WindowFault::Part)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(src.len()).unwrap_or(u32::MAX);
        let Some(at) = self.translate(offset, len) else {
            return Err(WindowFault::Window(WindowError::PastTheEnd));
        };
        self.storage.program(at, src).map_err(WindowFault::Part)
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        let Some(at) = self.translate(offset, len) else {
            return Err(WindowFault::Window(WindowError::PastTheEnd));
        };
        self.storage.erase(at, len).map_err(WindowFault::Part)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.storage.barrier().map_err(WindowFault::Part)
    }
}
