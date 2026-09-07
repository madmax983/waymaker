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

use crate::effect::DurableIntent;
use waymaker_flash::capacity::Bounds;

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

/// What [`Pipeline`]'s records may be worth, for §10's reserve.
///
/// A workflow declares this, because §10 prices a run's two exits before the run starts and
/// only the workflow knows how big its records get. The numbers are this workflow's own:
/// four bytes of run input, a `DOWNLOAD` result of [`DOWNLOADED`]'s length, and a terminal
/// payload no longer than that.
pub const BOUNDS: Bounds = Bounds {
    run_input_bytes: 4,
    effect_result_bytes: 32,
    terminal_bytes: 32,
};

/// Copies as much of `src` into `dst` as fits, and says how much that was.
///
/// The answer is what *fit*, which is what a workflow keeping a result needs. A world
/// reporting what it *produced* is a different question, and [`World::perform`] answers that
/// one instead — see [`Performed`].
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
    offers: [Dispatch; DISPATCH_LOG],
    offered: usize,
    pending_at: Option<usize>,
    pending_once_seq: Option<u32>,
    failing_at: Option<usize>,
    exhausting_at: Option<usize>,
    exhausting_seq: Option<u32>,
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
            offers: [Self::UNUSED; DISPATCH_LOG],
            offered: 0,
            pending_at: None,
            pending_once_seq: None,
            failing_at: None,
            exhausting_at: None,
            exhausting_seq: None,
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

    /// A world that declines the effect whose sequence is `seq` once, and performs every
    /// later attempt at it.
    ///
    /// An activity that was not ready and then was. Keyed on the identity the schedule record
    /// committed rather than on a dispatch count, so the decline and the retry are the same
    /// effect however many boots separate them — the trick
    /// [`exhausting_seq`](Self::exhausting_seq) uses, for the same reason.
    #[must_use]
    pub const fn pending_once_at_seq(seq: u32) -> Self {
        Self {
            pending_once_seq: Some(seq),
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

    /// A world whose answer at the `nth` dispatch is wider than the bound it was handed.
    ///
    /// It writes what fits and then reports [`Performed::Exhausted`], which is the one
    /// answer a world with too much to say may give.
    #[must_use]
    pub const fn exhausting_at(nth: usize) -> Self {
        Self {
            exhausting_at: Some(nth),
            ..Self::new()
        }
    }

    /// A world that exhausts the effect whose sequence is `seq`, on every boot.
    ///
    /// [`exhausting_at`](Self::exhausting_at) counts dispatches within one boot, so after a
    /// crash that already resolved an effect it exhausts a different one. This keys off the
    /// identity the schedule record committed, which is the same on every boot.
    #[must_use]
    pub const fn exhausting_seq(seq: u32) -> Self {
        Self {
            exhausting_seq: Some(seq),
            ..Self::new()
        }
    }

    /// Every effect this world **performed**, in order, up to [`DISPATCH_LOG`].
    ///
    /// An effect answered [`Performed::Pending`] is not one of them: the world was asked and
    /// declined, and nothing reached the outside. `crates/waymaker-drive/tests/crash.rs`
    /// reads this as the list of effects that really happened, so counting a declined one
    /// would make its durable-intent sweep assert about an effect nobody performed.
    #[must_use]
    pub fn dispatched(&self) -> &[Dispatch] {
        self.log
            .get(..self.count.min(DISPATCH_LOG))
            .unwrap_or_default()
    }

    /// Every intent this world was **offered**, in order, up to [`DISPATCH_LOG`].
    ///
    /// [`dispatched`](Self::dispatched)'s counterpart, and the difference is the whole reason
    /// both exist: an effect answered [`Performed::Pending`] is offered and not dispatched.
    /// Design document §14's redelivery contract is a statement about what the world is
    /// *asked* — a declined attempt and the retry that follows it must carry one identity —
    /// and a log that only recorded what happened could not see that pair.
    #[must_use]
    pub fn offered(&self) -> &[Dispatch] {
        self.offers
            .get(..self.offered.min(DISPATCH_LOG))
            .unwrap_or_default()
    }

    /// How many effects this world was asked to perform, log or no log.
    #[must_use]
    pub const fn performed(&self) -> usize {
        self.count
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
        intent: DurableIntent,
        kind: ActivityKind,
        _input: &[u8],
        out: &mut [u8],
    ) -> Performed {
        // Recorded before anything is decided, so a declined attempt is an offer like any
        // other. Counted whether or not the log had room, for the reason `count` is.
        if let Some(slot) = self.offers.get_mut(self.offered) {
            *slot = Dispatch {
                id: intent.id(),
                kind,
            };
        }
        self.offered = self.offered.saturating_add(1);

        let nth = self.count;
        if self.pending_at == Some(nth) {
            return Performed::Pending;
        }
        // Cleared as it fires, so the next attempt at this effect is performed.
        if self.pending_once_seq == Some(intent.id().seq.0) {
            self.pending_once_seq = None;
            return Performed::Pending;
        }
        if let Some(slot) = self.log.get_mut(nth) {
            *slot = Dispatch {
                id: intent.id(),
                kind,
            };
        }
        // Counted whether or not the log had room, so a run longer than [`DISPATCH_LOG`]
        // stops *recording* dispatches rather than stops counting them. A `pending_at` that
        // silently stopped advancing would be an instrument that lies.
        self.count = nth.saturating_add(1);
        let answer = if kind == DOWNLOAD { DOWNLOADED } else { HASHED };
        let taken = copy(answer, out);
        // §07 step 5 takes bounded result bytes. An answer wider than the bound is reported
        // as exhausted rather than truncated, so no part of it reaches the workflow.
        if self.exhausting_at == Some(nth)
            || self.exhausting_seq == Some(intent.id().seq.0)
            || taken < answer.len()
        {
            return Performed::Exhausted;
        }
        if self.failing_at == Some(nth) {
            Performed::Failed(taken)
        } else {
            Performed::Completed(taken)
        }
    }
}
