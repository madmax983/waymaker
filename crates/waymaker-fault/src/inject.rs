//! Every way a write sequence can be interrupted, listed.
//!
//! Design document §15 asks for "torn writes at every byte/program unit" and "power loss
//! before and after every barrier"; issue
//! [#18](https://github.com/madmax983/waymaker/issues/18) asks for enumeration rather than
//! sampling. [`injections`] is that list: a pure function of a recorded write sequence and
//! a geometry, with no randomness in it and nothing left to a seed.
//!
//! # Why a recorded sequence rather than the writer itself
//!
//! Because "every crash point" has to be a finite, countable thing before it can be a loop.
//! [`crate::Harness`] runs the writer once with no faults to learn its sequence, asks this
//! function for the list, and then re-runs the writer once per entry. A writer that reacts
//! to an injected [`Interruption::Failure`] by doing something different is not a problem for
//! that: only one injection is armed per run, and everything before it is identical by
//! construction.
//!
//! # The two effects are different questions
//!
//! [`Interruption::PowerLoss`] asks "what is on media if the world stops here" — nothing after it
//! runs, ever. [`Interruption::Failure`] asks "what does the writer do when this call returns an
//! error" — the media may already have changed, and the writer carries on. Design document
//! §12 requires both: `program` and `erase` "may fail **or** be interrupted".
//!
//! # None of these enums is `#[non_exhaustive]`
//!
//! For the reason [`waymaker_core::DecodeError`] is not, and this crate follows the
//! workspace: every match on them is in this workspace, and an exhaustive match is how the
//! compiler tells whoever adds a variant which call sites now have a case to think about.
//! `#[non_exhaustive]` would replace that list with a wildcard arm that silently absorbs it
//! — which, for a harness whose whole job is to enumerate, is the wrong default twice over.

use waymaker_flash::storage::Geometry;

/// One mutation of a write sequence, as the harness recorded it.
///
/// Offsets and lengths only. What was being written is the writer's business, and a
/// harness that knew would be a harness only one caller could use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Op {
    /// `program(offset, src)`, with `len == src.len()`.
    Program {
        /// Where the write started.
        offset: u32,
        /// How many bytes it carried.
        len: u32,
    },
    /// `erase(offset, len)`.
    Erase {
        /// Where the erase started.
        offset: u32,
        /// How many bytes it covered.
        len: u32,
    },
    /// `barrier()`.
    Barrier,
}

impl Op {
    /// The points strictly inside this operation at which it can be interrupted.
    ///
    /// A program tears at every byte, because a program *is* a byte-ordered write and §15
    /// asks for every byte and every program unit — the unit boundaries are a subset. An
    /// erase is interrupted at erase blocks and nowhere else, because no device erases byte
    /// by byte, and offering byte granularity there would invent failure modes rather than
    /// cover them. A barrier has no interior at all.
    fn tear_points(self, geometry: Geometry) -> Vec<u32> {
        match self {
            Self::Program { len, .. } => (1..len).collect(),
            Self::Erase { len, .. } => (1..)
                .map_while(|block: u32| block.checked_mul(geometry.erase_size()))
                .take_while(|boundary| *boundary < len)
                .collect(),
            Self::Barrier => Vec::new(),
        }
    }

    /// Whether this operation has a state between "did nothing" and "did everything" that a
    /// caller could tell apart.
    ///
    /// A barrier does not. Whether the ordering was established or not, a caller that saw
    /// an error learned nothing about it and must treat everything since the last
    /// successful barrier as merely attempted — so a barrier that "failed after succeeding"
    /// is the same world as one that failed outright, and enumerating both would be
    /// counting one crash point twice.
    const fn failure_is_observable_after_the_fact(self) -> bool {
        !matches!(self, Self::Barrier)
    }

    /// Whether this operation leaves media exactly as it found it, whatever happens.
    ///
    /// A zero-length program or erase is a legal call — `validate_program(offset, 0)` is
    /// `Ok`, and a writer with nothing to append is not a writer with a bug — and it moves
    /// no bytes. So "power loss after it" is the same world as "power loss before it", and
    /// "it failed having done everything" is the same world as "it failed having done
    /// nothing". Both are dropped, because an exhaustive list that counts one crash point
    /// twice is no longer a count of anything.
    ///
    /// A barrier is deliberately not in here. It moves no bytes either, and "after it
    /// returned" is a different world from "before it ran" — that difference is the whole
    /// of acknowledgment.
    const fn mutates_nothing(self) -> bool {
        matches!(
            self,
            Self::Program { len: 0, .. } | Self::Erase { len: 0, .. }
        )
    }
}

/// How much of an operation reached media.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Progress {
    /// None of it. The operation is interrupted before it touches anything.
    None,
    /// The first `n` bytes of it, where `0 < n < len`.
    ///
    /// [`injections`] never produces an `n` outside that range. A hand-built one passed to
    /// [`Harness::run_one`](crate::Harness::run_one) is clamped rather than refused: zero
    /// means [`None`](Self::None) and anything at or past the length means
    /// [`Whole`](Self::Whole), which are the worlds those values describe.
    Bytes(u32),
    /// All of it.
    Whole,
}

/// What the interruption looks like to the writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Interruption {
    /// The power went away. The call returns an error and so does every call after it;
    /// nothing more reaches media, ever.
    PowerLoss,
    /// The call returns an error and the writer carries on. Design document §12's "program
    /// and erase may fail".
    Failure,
}

/// One crash point: which operation, how far into it, and what the writer sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Injection {
    /// The operation this happens to, as an index into the recorded write sequence.
    ///
    /// For the one crash point that precedes the whole sequence — `(0, None, PowerLoss)` —
    /// this is zero and indexes nothing, because "before operation zero" and "before an
    /// empty sequence" are the same world.
    pub op: usize,
    /// How much of that operation reached media before the interruption.
    pub progress: Progress,
    /// Power loss, or a failed call the writer may react to.
    pub interruption: Interruption,
}

/// Every crash point in `ops`, in a fixed order, with no duplicates.
///
/// # Postconditions
///
/// The result is a function of `ops` and `geometry` alone: the same inputs give the same
/// list, in the same order, every time. It contains
///
/// * `(0, None, PowerLoss)` — the world stopping before the sequence began;
/// * `(i, Bytes(n), PowerLoss)` for every interior tear point of every operation;
/// * `(i, Whole, PowerLoss)` for every operation that can change media — which is also
///   "power loss *before* operation `i + 1`", so the two are one entry rather than two;
/// * `(i, None, Failure)`, `(i, Bytes(n), Failure)` and `(i, Whole, Failure)` for every
///   operation that can fail after the fact, and `(i, None, Failure)` alone for a barrier
///   or for an operation that moves no bytes.
///
/// "Power loss before and after every barrier" falls out of the third bullet: *after* the
/// barrier at `b` is `(b, Whole, PowerLoss)`, and *before* it is the previous operation's
/// `Whole` entry, or `(0, None, PowerLoss)` when the barrier is first.
#[must_use]
pub fn injections(ops: &[Op], geometry: Geometry) -> Vec<Injection> {
    let mut points = vec![Injection {
        op: 0,
        progress: Progress::None,
        interruption: Interruption::PowerLoss,
    }];

    for (index, op) in ops.iter().enumerate() {
        for bytes in op.tear_points(geometry) {
            points.push(Injection {
                op: index,
                progress: Progress::Bytes(bytes),
                interruption: Interruption::PowerLoss,
            });
        }
        if !op.mutates_nothing() {
            points.push(Injection {
                op: index,
                progress: Progress::Whole,
                interruption: Interruption::PowerLoss,
            });
        }
    }

    for (index, op) in ops.iter().enumerate() {
        points.push(Injection {
            op: index,
            progress: Progress::None,
            interruption: Interruption::Failure,
        });
        if !op.failure_is_observable_after_the_fact() || op.mutates_nothing() {
            continue;
        }
        for bytes in op.tear_points(geometry) {
            points.push(Injection {
                op: index,
                progress: Progress::Bytes(bytes),
                interruption: Interruption::Failure,
            });
        }
        points.push(Injection {
            op: index,
            progress: Progress::Whole,
            interruption: Interruption::Failure,
        });
    }

    points
}
