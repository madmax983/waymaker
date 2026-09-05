//! Where the supply is cut, and why that is a pure function.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27) asks for two things that
//! sound contradictory: the rig "cuts supply at randomised points", and "any recovery
//! violation is reproducible from the rig's log". They are only contradictory if the
//! randomness has state. Here it does not: a [`Cut`] is a pure function of a seed and an
//! iteration number, so a log line carrying those two numbers carries the whole of the cut
//! point, and a violation found on a board at 3 a.m. is re-derivable on a host at 9.
//!
//! # Why a generator rather than the hardware's entropy
//!
//! A board with a true random source could pick its cut points from it, and the rig would
//! then have to *log* each point rather than derive it — every field of the cut, on a link
//! that may itself be interrupted by the cut. Deriving instead means the durable state a
//! resumed rig needs is one iteration counter, and the log line that reproduces a failure is
//! sixteen bytes rather than a transcript.
//!
//! # Why `SplitMix64`
//!
//! It is a fixed sequence of shifts and multiplies with no table and no branch, its outputs
//! are pinned against the published reference in `tests/plan.rs`, and — the property this
//! module is built on — its `n`th output is computable without producing the `n - 1` before
//! it, because the state is the seed plus `n` gammas. A resumed rig computes iteration 900's
//! cut without replaying 900 iterations, which is what makes a log line meaningful on its
//! own.

use crate::phase::{Phase, ResetCause};

/// The odd increment `SplitMix64` walks its state by.
const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// `SplitMix64`, as published.
///
/// # Invariants
///
/// [`next`](Self::next) is a pure function of the state it advances, and
/// [`at`](Self::at) is the same sequence indexed rather than iterated:
/// `SplitMix64::new(seed).at(n)` is the `n + 1`th output of
/// `SplitMix64::new(seed)`, for every `n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// A generator seeded with `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Mixes one state word into an output word.
    const fn mix(mut z: u64) -> u64 {
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Advances the state and returns the next word.
    pub const fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(GAMMA);
        Self::mix(self.state)
    }

    /// The `index`th output of this generator, without producing the ones before it.
    ///
    /// Wrapping in `index`, which is what makes this total: a rig that has been running for
    /// more than `u64::MAX` gammas has bigger problems than a repeated cut point.
    #[must_use]
    pub const fn at(self, index: u64) -> u64 {
        Self::mix(
            self.state
                .wrapping_add(GAMMA.wrapping_mul(index.wrapping_add(1))),
        )
    }
}

/// How many words of the generator one cut consumes.
const WORDS_PER_CUT: u64 = 2;

/// Where one iteration cuts the supply.
///
/// # Invariants
///
/// Every field is derived from the seed and the iteration number, and nothing else. Two rigs
/// given the same two numbers arm the same cut, which is what
/// [`crate::log`] rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cut {
    phase: Phase,
    cause: ResetCause,
    placement: u64,
}

impl Cut {
    /// The cut iteration `iteration` of the run seeded with `seed` arms.
    ///
    /// # Postconditions
    ///
    /// A pure function of its two arguments. `Cut::for_iteration(s, n)` is
    /// `Plan::new(s).cut(n)`, and both are stable for the life of the format.
    #[must_use]
    pub const fn for_iteration(seed: u64, iteration: u32) -> Self {
        let generator = SplitMix64::new(seed);
        let base = (iteration as u64).wrapping_mul(WORDS_PER_CUT);
        let selector = generator.at(base);
        let placement = generator.at(base.wrapping_add(1));

        // Three phases and two causes taken from opposite ends of one word: the low bits of
        // a SplitMix64 output are as good as the high ones, and taking both from the same
        // end would correlate the phase with the cause.
        //
        // Through `from_index` rather than a hand-written match, so the modulus and the
        // mapping cannot drift: a fourth phase added to `Phase::ALL` would otherwise leave a
        // residue folding onto `Completion`, no cut would ever reach the new phase, and
        // `census::Coverage::verdict` would demand a cell nothing can fill — failing closed,
        // but for a reason that takes a long evening to find.
        let residue = selector % (Phase::ALL.len() as u64);
        // Walked rather than matched, and walked through `Phase::from_index`, so the modulus
        // and the mapping cannot drift: adding a phase changes `Phase::ALL` and every
        // exhaustive `match` in that file, and this picks up the new residue for free.
        // A hand-written `_ => Completion` arm would instead fold the new residue onto an old
        // phase — no cut would ever reach the new one, and `census::Coverage::verdict` would
        // demand a cell nothing can fill.
        let mut index = 0_usize;
        let mut found = None;
        while index < Phase::ALL.len() {
            if index as u64 == residue {
                found = Phase::from_index(index);
            }
            index += 1;
        }
        let Some(phase) = found else {
            // Unreachable: a residue of `ALL.len()` is an index into `ALL`.
            return Self {
                phase: Phase::Schedule,
                cause: ResetCause::PowerCut,
                placement,
            };
        };
        let cause = if (selector >> 63) == 0 {
            ResetCause::PowerCut
        } else {
            ResetCause::Watchdog
        };

        Self {
            phase,
            cause,
            placement,
        }
    }

    /// Which write the cut lands in.
    #[must_use]
    pub const fn phase(self) -> Phase {
        self.phase
    }

    /// How the device comes back afterwards.
    #[must_use]
    pub const fn cause(self) -> ResetCause {
        self.cause
    }

    /// The raw placement word, for a log line that carries the cut whole.
    #[must_use]
    pub const fn placement(self) -> u64 {
        self.placement
    }

    /// The byte of a `len`-byte write the cut falls inside, or `None` if it has no inside.
    ///
    /// # Postconditions
    ///
    /// `Some(n)` implies `0 < n < len`. A cut at byte zero is "before the write" and one at
    /// `len` is "after it": both are worlds the rig reaches through its phase and its reset
    /// cause, and returning them here would count one crash point twice.
    /// `waymaker_fault::injections` draws the same line for the same reason.
    #[must_use]
    pub fn tear_offset(self, len: u32) -> Option<u32> {
        let interior = len.checked_sub(1)?;
        if interior == 0 {
            return None;
        }
        // `placement % interior` is in `0..interior`, so the narrowing always succeeds and
        // the result is in `1..len`. Written as a conversion rather than a cast because a
        // cast that "always succeeds" is a claim nothing checks.
        let inside = u32::try_from(self.placement % u64::from(interior)).ok()?;
        inside.checked_add(1)
    }

    /// Which effect of a run with `effects` of them the cut lands on.
    ///
    /// # Postconditions
    ///
    /// Strictly less than `effects`, and zero when `effects` is zero — a run with no effects
    /// has no effect for a cut to land on, and refusing here would push the same arithmetic
    /// into every caller. `u16` because that is what a run counts its effects in, so no
    /// caller has to narrow the answer and no narrowing can be got wrong.
    #[must_use]
    pub fn effect_index(self, effects: u16) -> u16 {
        if effects == 0 {
            return 0;
        }
        // The placement word is reused rather than drawn again: a cut lands on one effect at
        // one byte, and two draws would let the two disagree about which iteration they came
        // from after a resume.
        u16::try_from((self.placement >> 32) % u64::from(effects)).unwrap_or(0)
    }
}

/// A seeded sequence of cut points.
///
/// # Invariants
///
/// [`cut`](Self::cut) has no side effects and holds no state that advances: calling it for
/// iteration 900 gives the same answer whether or not iterations 0 to 899 were ever asked
/// for. That is what lets a rig resumed after a reset carry one counter rather than a
/// generator state that a power cut could have torn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Plan {
    seed: u64,
}

impl Plan {
    /// The plan for `seed`.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// The seed, for the log line that reproduces this run.
    #[must_use]
    pub const fn seed(self) -> u64 {
        self.seed
    }

    /// The cut iteration `iteration` arms.
    #[must_use]
    pub const fn cut(self, iteration: u32) -> Cut {
        Cut::for_iteration(self.seed, iteration)
    }
}
