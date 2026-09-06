//! The world's half of the boundary: what happens when the kernel says dispatch.
//!
//! Synchronous, and bounded by the caller's buffer. An activity that cannot answer now says
//! [`Performed::Pending`] rather than blocking, which is how a driver with no executor
//! still has a way to wait.

use waymaker_core::{ActivityKind, EffectId};

/// What an activity did.
///
/// The length is how many bytes of the caller's buffer the activity filled. A length past
/// the end of that buffer is a refusal rather than a truncation: a result silently cut
/// short would be recorded as history and replayed for ever.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Performed {
    /// Success. The first `len` bytes of the buffer are the result.
    Completed(usize),
    /// Failure. The first `len` bytes of the buffer are the failure payload.
    Failed(usize),
    /// Not now. The run suspends under the identity it was dispatched with, and the next
    /// boot redelivers it — design document §14's redelivery contract.
    Pending,
}

/// The activities a workflow can call.
pub trait Activities {
    /// Perform `id`'s effect and write its outcome into `out`.
    ///
    /// `id` is the identity the schedule record already committed, so an activity that
    /// deduplicates downstream sees a repeat rather than a second effect when a reset
    /// redelivers it.
    fn perform(
        &mut self,
        id: EffectId,
        kind: ActivityKind,
        input: &[u8],
        out: &mut [u8],
    ) -> Performed;
}
