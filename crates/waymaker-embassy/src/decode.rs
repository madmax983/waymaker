//! Turning recorded bytes into a workflow's own value.
//!
//! Design document §02 decision 4: a codec is a convenience, never a wire-format
//! requirement. The kernel records opaque bytes. This trait is how a workflow reads them,
//! and it names no codec.

/// A value a workflow decodes from an activity's recorded answer.
///
/// # What it must be
///
/// Small, and owned. A Rust future holds every local that survives an `.await`, so a value
/// kept across an effect boundary is workflow RAM for the life of the run. Cross a boundary
/// with a handle, not with the bytes the handle names.
///
/// # Allocation
///
/// None. `bytes` is borrowed from the caller's buffer and is overwritten by the next
/// boundary, so an implementor copies what it keeps into its own fixed-size storage.
pub trait Decode: Sized {
    /// Why the bytes are not a value of this type.
    type Error;

    /// Read `bytes` as a value.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the bytes are not one. A decode failure is a workflow fault,
    /// not an effect failure: the effect's outcome is already committed and replay hands
    /// back the same bytes, so the same call fails the same way on every boot.
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error>;
}

/// An answer a workflow does not read.
///
/// `ctx.activity::<()>(kind, input)` is the activity whose result is only that it happened.
/// It accepts any bytes, which is right: history holds them and this workflow never asked
/// what they were.
impl Decode for () {
    type Error = core::convert::Infallible;

    fn decode(_bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(())
    }
}

/// Serde itself, for a [`Format`] implementor's own use.
#[cfg(feature = "serde")]
pub use serde;
/// Serde's owned-value bound, re-exported so an implementor need not name the version.
///
/// [`Format`] is written against it, so a firmware writing its own format would otherwise
/// have to declare a `serde` dependency matching this crate's.
#[cfg(feature = "serde")]
pub use serde::de::DeserializeOwned;

/// A data format a workflow's recorded bytes are in.
///
/// Waymaker names serde's data model and no format. A firmware with its own codec
/// implements this and gets [`Decode`] for every type that codec reads.
///
/// # Allocation
///
/// None. An implementor reads `bytes` and returns an owned value.
#[cfg(feature = "serde")]
pub trait Format {
    /// Why the bytes are not a value.
    type Error;

    /// Read `bytes` as a `T`.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the bytes are not a `T`.
    fn read<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Self::Error>;
}

/// A `T` that [`Format`] `F` reads: the [`Decode`] a workflow names.
///
/// `T` is owned. `bytes` is the caller's buffer and the next boundary overwrites it, so a
/// value that borrowed it would be a dangling read one `.await` later.
#[cfg(feature = "serde")]
pub struct Coded<F, T> {
    value: T,
    /// Which format read the value. `fn() -> F` so that `F` constrains nothing else.
    format: core::marker::PhantomData<fn() -> F>,
}

#[cfg(feature = "serde")]
impl<F, T> Coded<F, T> {
    /// The value the format read.
    pub fn into_inner(self) -> T {
        self.value
    }
}

#[cfg(feature = "serde")]
impl<F: Format, T: DeserializeOwned> Decode for Coded<F, T> {
    type Error = F::Error;

    fn decode(bytes: &[u8]) -> Result<Self, F::Error> {
        Ok(Self {
            value: F::read(bytes)?,
            format: core::marker::PhantomData,
        })
    }
}

/// The postcard wire format.
#[cfg(feature = "postcard")]
pub struct Postcard;

#[cfg(feature = "postcard")]
impl Format for Postcard {
    type Error = postcard::Error;

    fn read<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// A `T` postcard reads.
///
/// `ctx.activity::<FromPostcard<Report>>(kind, input)`, then `into_inner`.
#[cfg(feature = "postcard")]
pub type FromPostcard<T> = Coded<Postcard, T>;
