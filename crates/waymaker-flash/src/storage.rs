//! The storage contract: what Waymaker requires of the media beneath it.
//!
//! Design document §12. Four operations and one promise:
//!
//! * `program` and `erase` may fail or be interrupted at any supported unit.
//! * After `barrier` returns, all earlier successful mutations survive reset.
//! * No later mutation may become durable before mutations ordered by a completed barrier.
//! * The adapter validates erase/program alignment before touching media.
//! * Flash-specific one-way bit programming rules remain the driver's responsibility.
//!
//! # What this module owns
//!
//! The shape of the contract and the arithmetic that guards it: [`Geometry`], which is the
//! four units a device is described by and the only thing that decides whether an offset
//! and a length are legal, and [`StableStorage`], which is the contract itself.
//!
//! # What this module must not own
//!
//! A driver. There is no implementation here, and there is deliberately no in-memory one
//! either: a model of the media that lies on purpose is what issue
//! [#18](https://github.com/madmax983/waymaker/issues/18) builds, and it lives in
//! `waymaker-fault`, one level above this crate — see
//! [ADR 0013](https://github.com/madmax983/waymaker/blob/main/docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md).
//! Keeping it out of here is what stops an exhaustive host-side crash enumerator from being
//! charged against an 8 KiB code-flash budget.
//!
//! # Why the validators are on the geometry rather than on the trait
//!
//! Because §12 puts the obligation on *the adapter*, and an adapter is a different type in
//! every port. A default method on [`StableStorage`] would be overridable, which is the one
//! thing a validation step must not be; a free function would have to be handed all four
//! units on every call. [`Geometry`] is `Copy`, is the value the driver already holds, and
//! cannot be constructed in an inconsistent state — [`Geometry::new`] is the only way in and
//! it rejects units that do not nest.
//!
//! # Why alignment is reported before bounds
//!
//! A misaligned offset is a caller bug; an out-of-bounds one may be a legitimate caller
//! meeting the end of a bank. Reporting the alignment first means the more specific
//! diagnosis wins when an operation is both, which is what a driver author needs to see
//! first.

use core::fmt;

/// The four units a storage device is described by.
///
/// # Invariants
///
/// Every value of this type nests: `capacity` is whole erase blocks, an erase block is
/// whole program units, and a program unit is whole read units. None of the four is zero.
/// [`Geometry::new`] is the only constructor, so no other value of this type exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Geometry {
    capacity: u32,
    erase_size: u32,
    program_size: u32,
    read_size: u32,
}

impl Geometry {
    /// Describes a device, or refuses to.
    ///
    /// # Errors
    ///
    /// [`GeometryError::ZeroUnit`] if any of the four is zero, and
    /// [`GeometryError::UnitsDoNotNest`] if the units are not whole multiples of one
    /// another. A device that fails either check has bytes no legal operation could name.
    pub const fn new(
        capacity: u32,
        erase_size: u32,
        program_size: u32,
        read_size: u32,
    ) -> Result<Self, GeometryError> {
        if capacity == 0 || erase_size == 0 || program_size == 0 || read_size == 0 {
            return Err(GeometryError::ZeroUnit);
        }
        if capacity % erase_size != 0
            || erase_size % program_size != 0
            || program_size % read_size != 0
        {
            return Err(GeometryError::UnitsDoNotNest);
        }
        Ok(Self {
            capacity,
            erase_size,
            program_size,
            read_size,
        })
    }

    /// Total addressable bytes.
    #[must_use]
    pub const fn capacity(self) -> u32 {
        self.capacity
    }

    /// The smallest region `erase` can act on.
    #[must_use]
    pub const fn erase_size(self) -> u32 {
        self.erase_size
    }

    /// The smallest region `program` can act on.
    #[must_use]
    pub const fn program_size(self) -> u32 {
        self.program_size
    }

    /// The smallest region `read` can act on.
    #[must_use]
    pub const fn read_size(self) -> u32 {
        self.read_size
    }

    /// How many erase blocks the device has.
    ///
    /// # Postconditions
    ///
    /// At least one, because [`Geometry::new`] rejects a capacity that is not whole erase
    /// blocks and rejects a capacity of zero.
    #[must_use]
    pub const fn erase_blocks(self) -> u32 {
        self.capacity / self.erase_size
    }

    /// Whether `offset..offset + len` is a legal read.
    ///
    /// # Errors
    ///
    /// [`GeometryError::MisalignedOffset`] or [`GeometryError::MisalignedLength`] if either
    /// is not a multiple of [`read_size`](Self::read_size), and
    /// [`GeometryError::OutOfBounds`] if the region reaches past the capacity — including
    /// when `offset + len` would overflow, which is checked rather than wrapped.
    pub const fn validate_read(self, offset: u32, len: u32) -> Result<(), GeometryError> {
        self.validate(offset, len, self.read_size)
    }

    /// Whether `offset..offset + len` is a legal program.
    ///
    /// # Errors
    ///
    /// As [`validate_read`](Self::validate_read), against
    /// [`program_size`](Self::program_size).
    pub const fn validate_program(self, offset: u32, len: u32) -> Result<(), GeometryError> {
        self.validate(offset, len, self.program_size)
    }

    /// Whether `offset..offset + len` is a legal erase.
    ///
    /// # Errors
    ///
    /// As [`validate_read`](Self::validate_read), against [`erase_size`](Self::erase_size).
    pub const fn validate_erase(self, offset: u32, len: u32) -> Result<(), GeometryError> {
        self.validate(offset, len, self.erase_size)
    }

    /// The one arithmetic the three validators share.
    ///
    /// A zero length is legal at any aligned offset up to and including the capacity: a
    /// caller that has nothing to write is not a caller with a bug, and a driver that
    /// rejected it would push the empty case into every call site.
    const fn validate(self, offset: u32, len: u32, unit: u32) -> Result<(), GeometryError> {
        if offset % unit != 0 {
            return Err(GeometryError::MisalignedOffset);
        }
        if len % unit != 0 {
            return Err(GeometryError::MisalignedLength);
        }
        match offset.checked_add(len) {
            Some(end) if end <= self.capacity => Ok(()),
            _ => Err(GeometryError::OutOfBounds),
        }
    }
}

/// A geometry that cannot describe a device, or an operation that geometry forbids.
///
/// Not `#[non_exhaustive]`, for the reason [`waymaker_core::DecodeError`] is not: every
/// match on it is in this workspace, and an exhaustive match is how the compiler tells
/// whoever adds a variant which call sites now have a case to think about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GeometryError {
    /// A capacity or a unit was zero.
    ZeroUnit,
    /// The units are not whole multiples of one another.
    UnitsDoNotNest,
    /// The offset is not a multiple of the unit the operation acts on.
    MisalignedOffset,
    /// The length is not a multiple of the unit the operation acts on.
    MisalignedLength,
    /// The region reaches past the end of the device, or its end would overflow.
    OutOfBounds,
}

impl GeometryError {
    /// A short static description of this failure.
    ///
    /// Static text written straight through the formatter, for the reason
    /// `waymaker_core`'s error messages are: a single `write!` with an argument links
    /// `core::fmt::write` into an image with an 8 KiB budget, to say something no device
    /// with no console will ever print.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::ZeroUnit => "a storage capacity or unit was zero",
            Self::UnitsDoNotNest => "storage units are not whole multiples of one another",
            Self::MisalignedOffset => "offset is not a multiple of the operation's unit",
            Self::MisalignedLength => "length is not a multiple of the operation's unit",
            Self::OutOfBounds => "the region reaches past the end of the device",
        }
    }
}

impl fmt::Display for GeometryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl core::error::Error for GeometryError {}

/// Design document §12's required storage contract.
///
/// # Invariants a driver must uphold
///
/// * After [`barrier`](Self::barrier) returns, every earlier successful mutation survives
///   reset.
/// * No mutation issued after a completed barrier may become durable before one the
///   barrier ordered.
/// * [`program`](Self::program) and [`erase`](Self::erase) may fail or be interrupted at
///   any supported unit; a caller may not assume either is atomic.
/// * Alignment and bounds are validated against [`geometry`](Self::geometry) *before* media
///   is touched. [`Geometry`]'s three validators are how.
///
/// Disabling interrupts is not part of the durability proof. A blocking internal-flash
/// driver may need to for hardware reasons; the protocol above it is expressed entirely in
/// durable ordering and recoverable bytes.
///
/// # What this trait must not grow
///
/// Host conveniences. §05 is explicit that a host or browser adapter "must not expand the
/// firmware traits to accommodate host conveniences", and this file's public surface is
/// pinned by the `storage-contract` rule of `cargo xtask check-layering` so that a
/// `read_all`, a `capacity()` shortcut or a `flush` is a line a reviewer writes on purpose.
pub trait StableStorage {
    /// How this driver reports a refusal.
    type Error;

    /// The device's four units.
    fn geometry(&self) -> Geometry;

    /// Fills `dst` from `offset`.
    ///
    /// # Errors
    ///
    /// If the region is misaligned, out of bounds, or the media cannot be read.
    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error>;

    /// Writes `src` at `offset`.
    ///
    /// # Errors
    ///
    /// If the region is misaligned, out of bounds, or the program fails or is interrupted.
    /// A failed program may still have changed media.
    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error>;

    /// Returns `offset..offset + len` to the erased state.
    ///
    /// # Errors
    ///
    /// If the region is misaligned, out of bounds, or the erase fails or is interrupted.
    /// A failed erase may still have changed media.
    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error>;

    /// Orders everything before it against everything after it, durably.
    ///
    /// # Errors
    ///
    /// If durability cannot be established. A caller that meets an error here has learned
    /// nothing about what is on media and must treat every mutation since the last
    /// successful barrier as merely attempted.
    fn barrier(&mut self) -> Result<(), Self::Error>;
}
