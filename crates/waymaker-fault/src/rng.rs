//! A generator with a seed in it, and the geometries it draws.
//!
//! Issue [#19](https://github.com/madmax983/waymaker/issues/19) asks the recovery oracle to
//! be swept over "random record sequences across random storage geometries". Random there
//! means *drawn* and never *unrepeatable*: a crash-testing suite whose failures cannot be
//! re-run is a suite nobody can fix.
//!
//! # Why this is thirty lines rather than a dependency
//!
//! Because `waymaker-fault` has no third-party dependencies and is not about to grow one
//! for a property suite — see
//! [ADR 0013](https://github.com/madmax983/waymaker/blob/main/docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md).
//! What the sweep needs from a generator is that it be deterministic, that one seed be one
//! stream for ever, and that two seeds differ; `SplitMix64` is all three in a handful of
//! arithmetic, is published, and has no state to get wrong. What it is not is a source of
//! cryptographic randomness, and nothing here should ever be used as one.
//!
//! # Why the geometry generator is here and not in the tests
//!
//! Because a geometry is the harness's own vocabulary — it is what a [`Device`] is
//! described by — and "a legal device, drawn" is a thing rung 0.2's bank tests and rung
//! 0.3's effect-protocol tests will each want. A generator that lived in one integration
//! test would be copied into the next one, and the copy is where the invariants go wrong.
//!
//! [`Device`]: crate::Device

use waymaker_flash::storage::Geometry;

/// `SplitMix64`: one seed, one stream, for ever.
///
/// # Postconditions
///
/// Every method is a pure function of the state it advances, so a sequence is reproducible
/// from its seed on every target and in every build. Nothing here consults a clock, an
/// environment variable or an allocation address — a "random" test that cannot be re-run
/// from its failure message is a test that reports bugs nobody can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// The stream `seed` names.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next value in the stream.
    ///
    /// # Postconditions
    ///
    /// Advances the state exactly once, so a caller counting draws is counting the same
    /// thing the seed does.
    pub const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^ (mixed >> 31)
    }

    /// A value in `0..bound`, or zero when `bound` is zero.
    ///
    /// Zero rather than a panic or a division by it: a generator asked for a choice among
    /// nothing has one honest answer, and a harness that panicked there would turn a
    /// degenerate draw into a failure of the suite rather than a case in it.
    ///
    /// # Postconditions
    ///
    /// Strictly below `bound` whenever `bound` is non-zero. The modulo bias is real and is
    /// accepted: the bounds here are small constants against a 64-bit draw, and a sweep
    /// that leaned one part in 2^60 towards a shorter history would still be a sweep.
    pub fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        let drawn = self.next_u64() % u64::from(bound);
        // Strictly below `bound`, which is a `u32`, so the conversion cannot lose anything
        // — written as a fallible one anyway, because a cast that is only correct because
        // of the line above it is a cast that stops being correct when that line moves.
        u32::try_from(drawn).unwrap_or(0)
    }

    /// A coin.
    pub const fn flip(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

/// A legal geometry drawn from `rng`, no larger than `max_capacity`.
///
/// # Postconditions
///
/// Total: every draw is a geometry [`Geometry::new`] accepts, and its capacity is at most
/// `max_capacity` — or exactly one byte when `max_capacity` is zero, because a device with
/// no bytes in it is not something `Geometry` can describe and a generator has to answer
/// with something legal. The three units are powers of two ordered `erase >= program >=
/// read`, and the capacity is a whole number of erase blocks, because those are the
/// invariants the constructor enforces and this function clamps rather than gambles.
///
/// The *units* drawn are small on purpose — at most a 128-byte erase block of 16-byte
/// program units of 4-byte reads — because those are the shapes real NOR parts have and a
/// sweep gains nothing from a page size no device reports. The *capacity* is not: it is
/// whatever `max_capacity` allows, and a [`Device`] over it is that many bytes of host
/// memory and roughly twice that many crash points. `max_capacity` is therefore a runtime
/// dial the caller owns, and a caller that passes [`u32::MAX`] has asked for a four-gigabyte
/// device and will get one.
///
/// [`Device`]: crate::Device
#[must_use]
pub fn random_geometry(rng: &mut Rng, max_capacity: u32) -> Geometry {
    let budget = max_capacity.max(1);

    // Drawn from the read unit upwards, so that the nesting `erase >= program >= read` is
    // established by construction rather than checked afterwards.
    let read = 1_u32 << rng.below(3);
    let program = read << rng.below(3);
    let mut erase = program << rng.below(4);

    // A block bigger than the whole budget cannot be a whole number of blocks within it.
    // Halving keeps it a power of two and keeps it at or above `read`, which is one.
    while erase > budget {
        erase >>= 1;
    }
    let program = program.min(erase);
    let read = read.min(program);

    // `erase` is now at most `budget`, so this is at least one block.
    let blocks = 1 + rng.below(budget / erase);
    let capacity = erase.saturating_mul(blocks);

    match Geometry::new(capacity, erase, program, read) {
        Ok(geometry) => geometry,
        // Unreachable by construction: none of the four is zero, the three units are powers
        // of two drawn by shifting one left and clamped only by halving, they nest by the
        // `min` chain above, and the capacity is `blocks` whole erase blocks.
        Err(error) => unreachable!("a drawn geometry was refused: {}", error.message()),
    }
}
