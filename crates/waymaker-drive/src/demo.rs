//! A reference workflow and a reference world.
//!
//! Here rather than in `tests/` on purpose. The claim this crate exists to make is that a
//! workflow runs to completion with no `Future`, no Embassy and no allocation, and a
//! fixture that only ever compiled for the host would leave the last third of that
//! unchecked. These types are `#![no_std]` and allocation-free like the driver, and the
//! `drive-firmware` pipeline stage builds them for `thumbv6m-none-eabi`.
//!
//! [`Pipeline`] is also the shape a workflow has to have against a synchronous boundary:
//! every result is copied into the workflow's own storage before the next call, because the
//! borrow it arrived in ends there.

use waymaker_core::{ActivityKind, EffectId, Outcome};

use crate::activity::{Activities, Performed};
use crate::boundary::{Boundary, Suspended};
use crate::workflow::{Identity, Workflow};

/// Fetch bytes from somewhere outside the device.
pub const DOWNLOAD: ActivityKind = ActivityKind(1);
/// Reduce those bytes to a digest.
pub const HASH: ActivityKind = ActivityKind(2);
/// The workflow's kind, as its `RunStarted` record records it.
pub const WORKFLOW_KIND: u16 = 7;
/// The workflow's version.
pub const WORKFLOW_VERSION: u16 = 1;
/// What [`World`] answers a [`DOWNLOAD`] with.
pub const DOWNLOADED: &[u8] = b"contents-of-the-thing";
/// What [`World`] answers a [`HASH`] with.
pub const HASHED: &[u8] = b"\x01\x02\x03\x04";

/// Copies as much of `src` into `dst` as fits, and says how much that was.
fn copy(src: &[u8], dst: &mut [u8]) -> usize {
    let taken = src.len().min(dst.len());
    let (Some(from), Some(into)) = (src.get(..taken), dst.get_mut(..taken)) else {
        return 0;
    };
    into.copy_from_slice(from);
    taken
}

/// Download something, hash it, return the hash.
///
/// Two effects, so that replay has an order to get wrong, and a result derived from the
/// first effect's output, so that a workflow which lost a replayed result cannot reach the
/// end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pipeline {
    input: [u8; 4],
    downloaded: [u8; 32],
    downloaded_len: usize,
    hashed: [u8; 8],
    hashed_len: usize,
}

impl Pipeline {
    /// A workflow that has run nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            input: *b"seed",
            downloaded: [0; 32],
            downloaded_len: 0,
            hashed: [0; 8],
            hashed_len: 0,
        }
    }

    /// What [`DOWNLOAD`] answered, as this run observed it.
    #[must_use]
    pub fn downloaded(&self) -> &[u8] {
        self.downloaded
            .get(..self.downloaded_len)
            .unwrap_or_default()
    }

    /// What [`HASH`] answered, as this run observed it.
    #[must_use]
    pub fn hashed(&self) -> &[u8] {
        self.hashed.get(..self.hashed_len).unwrap_or_default()
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Workflow for Pipeline {
    fn identity(&self) -> Identity<'_> {
        Identity {
            kind: WORKFLOW_KIND,
            version: WORKFLOW_VERSION,
            input: &self.input,
        }
    }

    fn run(&mut self, boundary: &mut dyn Boundary) -> Result<Outcome<'_>, Suspended> {
        let downloaded = match boundary.call(DOWNLOAD, b"url")? {
            Outcome::Completed(bytes) => bytes,
            Outcome::Failed(_) => return Ok(Outcome::Failed(b"download")),
        };
        // Copied before the next call, because the borrow ends there.
        self.downloaded_len = copy(downloaded, &mut self.downloaded);

        let source = self
            .downloaded
            .get(..self.downloaded_len)
            .unwrap_or_default();
        let hashed = match boundary.call(HASH, source)? {
            Outcome::Completed(bytes) => bytes,
            Outcome::Failed(_) => return Ok(Outcome::Failed(b"hash")),
        };
        let taken = copy(hashed, &mut self.hashed);
        self.hashed_len = taken;

        Ok(Outcome::Completed(
            self.hashed.get(..taken).unwrap_or_default(),
        ))
    }
}

/// One effect the world was asked to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Dispatch {
    /// The identity the schedule record committed.
    pub id: EffectId,
    /// Which activity was asked for.
    pub kind: ActivityKind,
}

/// How many dispatches a [`World`] remembers.
///
/// Bounded rather than growable: this crate has no allocator, and a world that could grow
/// its log would be the one place in it that did.
pub const DISPATCH_LOG: usize = 8;

/// The world, and a record of every effect it was asked to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct World {
    log: [Dispatch; DISPATCH_LOG],
    count: usize,
    pending_at: Option<usize>,
    failing_at: Option<usize>,
}

impl World {
    /// The identity a log slot holds before anything is dispatched into it.
    const UNUSED: Dispatch = Dispatch {
        id: EffectId {
            run: waymaker_core::RunId(0),
            seq: waymaker_core::EffectSeq(0),
        },
        kind: ActivityKind(0),
    };

    /// A world that performs everything it is asked.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            log: [Self::UNUSED; DISPATCH_LOG],
            count: 0,
            pending_at: None,
            failing_at: None,
        }
    }

    /// A world that answers [`Performed::Pending`] at the `nth` dispatch, counting from
    /// zero, and performs everything else.
    #[must_use]
    pub const fn pending_at(nth: usize) -> Self {
        Self {
            pending_at: Some(nth),
            ..Self::new()
        }
    }

    /// A world that answers [`Performed::Failed`] at the `nth` dispatch.
    #[must_use]
    pub const fn failing_at(nth: usize) -> Self {
        Self {
            failing_at: Some(nth),
            ..Self::new()
        }
    }

    /// Every effect this world was asked to perform, in order.
    #[must_use]
    pub fn dispatched(&self) -> &[Dispatch] {
        self.log.get(..self.count).unwrap_or_default()
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl Activities for World {
    fn perform(
        &mut self,
        id: EffectId,
        kind: ActivityKind,
        _input: &[u8],
        out: &mut [u8],
    ) -> Performed {
        let nth = self.count;
        if self.pending_at == Some(nth) {
            return Performed::Pending;
        }
        if let Some(slot) = self.log.get_mut(nth) {
            *slot = Dispatch { id, kind };
            self.count = nth.saturating_add(1);
        }
        let answer = if kind == DOWNLOAD { DOWNLOADED } else { HASHED };
        let taken = copy(answer, out);
        if self.failing_at == Some(nth) {
            Performed::Failed(taken)
        } else {
            Performed::Completed(taken)
        }
    }
}
