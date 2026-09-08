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
