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
/// # Allocation, and what the caller owns
///
/// None. The caller owns the storage on both sides. `bytes` borrows the caller's buffer,
/// which the next boundary overwrites, so an implementor copies what it keeps into its own
/// fixed-size storage. The value is returned by move, into the caller's frame.
pub trait Decode: Sized {
    /// Why the bytes are not a value of this type.
    type Error;

    /// Read `bytes` as a value.
    ///
    /// # Errors
    ///
    /// [`Self::Error`] when the bytes are not a value of this type. A decode failure is a
    /// workflow fault, not an effect failure. The outcome is already committed. Replay
    /// gives the same bytes, so the call fails the same way on each boot.
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

/// The serde crate, at the version this façade uses.
///
/// A [`Format`] implementor needs more than a bound: `Deserializer`, `Visitor`, `de::Error`
/// and the `forward_to_deserialize_any!` macro. The whole crate is re-exported so that an
/// implementor writes against this version and not a second one. `derive` is on in every
/// configuration that has this module, so `#[serde(crate = "..::decode::serde")]` works
/// whichever codec feature is enabled.
///
/// The cost is that serde is in this crate's public API: a serde 2.0 would be a breaking
/// change here.
#[cfg(feature = "serde")]
pub use serde;
/// Serde's owned-value bound.
///
/// [`Format`] uses this bound. Re-exported here so that a firmware with its own format does
/// not declare a `serde` dependency at the version this crate uses.
#[cfg(feature = "serde")]
pub use serde::de::DeserializeOwned;

/// A data format a workflow's recorded bytes are in.
///
/// Waymaker names serde's data model and no format. A firmware with its own codec
/// implements this and gets [`Decode`] for every type that codec reads.
///
/// # Allocation
///
/// None, in this workspace. An implementor reads `bytes` and returns an owned value. The
/// bound does not carry that claim: a firmware whose graph enables `serde/alloc` can name a
/// `T` that allocates, and the allocation is inside a dependency where `crate-attributes`
/// cannot see it.
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
/// `T` is owned. `bytes` is the caller's buffer. The next boundary overwrites it, so a
/// value that borrows `bytes` becomes invalid after the next `.await`.
#[cfg(feature = "serde")]
pub struct Coded<F, T> {
    value: T,
    /// Which format read the value. `fn() -> F` keeps `F` out of the auto traits and the
    /// variance of `T`.
    format: core::marker::PhantomData<fn() -> F>,
}

#[cfg(feature = "serde")]
impl<F, T> Coded<F, T> {
    /// The value the format read.
    pub fn into_inner(self) -> T {
        self.value
    }
}

// Written out rather than derived. A derive bounds every parameter, so `#[derive(Clone)]`
// would ask `F: Clone` of a format that is only a type-level tag. These bound `T` alone.
//
// There is no `Debug`. It would forward to `T`'s, which pulls `core::fmt` into a firmware
// that only wanted to decode: 208 B to 3028 B on the `waymaker-embassy/postcard` row of
// `cargo xtask size`, measured. A caller that wants to print the value calls `into_inner`
// and prints the `T`, whose `Debug` is its own.
#[cfg(feature = "serde")]
impl<F, T: Clone> Clone for Coded<F, T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            format: core::marker::PhantomData,
        }
    }
}

#[cfg(feature = "serde")]
impl<F, T: Copy> Copy for Coded<F, T> {}

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
///
/// A type-level tag. The private field keeps it one: [`Format::read`] takes no `self`, so a
/// value of this type has nowhere to go.
#[cfg(feature = "postcard")]
pub struct Postcard(());

#[cfg(feature = "postcard")]
impl Format for Postcard {
    type Error = postcard::Error;

    /// Read `bytes` as one complete `T`.
    ///
    /// # Errors
    ///
    /// [`postcard::Error`] when the bytes are not a `T`, and
    /// [`postcard::Error::DeserializeBadEncoding`] when a `T` is followed by more bytes.
    ///
    /// `take_from_bytes` rather than `from_bytes`, because `from_bytes` reads a *prefix*:
    /// it stops at the end of the value and never asks what follows. A recorded answer is
    /// one value, so bytes after it mean the record and this type disagree — which is what
    /// a firmware that narrowed a result type meets on replay. Postcard has no variant for
    /// it, so this one says the encoding is not a `T`, which is what it is.
    fn read<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, postcard::Error> {
        match postcard::take_from_bytes(bytes)? {
            (value, []) => Ok(value),
            (_value, _trailing) => Err(postcard::Error::DeserializeBadEncoding),
        }
    }
}

/// A `T` postcard reads.
///
/// `ctx.activity::<FromPostcard<Report>>(kind, input)`, then `into_inner`.
///
/// `T` owns what it holds, so a type that borrows the buffer does not compile:
///
/// ```compile_fail
/// use waymaker_embassy::Decode;
/// use waymaker_embassy::decode::FromPostcard;
///
/// let _ = FromPostcard::<&'static str>::decode(&[1]);
/// ```
#[cfg(feature = "postcard")]
pub type FromPostcard<T> = Coded<Postcard, T>;
