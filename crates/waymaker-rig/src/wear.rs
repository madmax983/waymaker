//! What a run cost the part, counted at the device rather than at the journal.
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27) asks the rig to "record erase
//! counts and per-effect write amplification across the run".
//! [`WriteAmplification`] already counts payload bytes, programmed
//! bytes, program calls and barriers — but it counts them for a *journal*, and a journal
//! never erases. §10's bank lifecycle is where the erases are, and a run's erase count is a
//! figure about the part rather than about the writer.
//!
//! So this module meters the storage instead. [`Metered`] is a
//! [`StableStorage`] that wraps another one and
//! counts every mutation the layer beneath was asked for.
//!
//! # Why it is here and not in `waymaker-flash`
//!
//! Because an 18 KiB code-flash budget should not be charged for an instrument. A fifth field
//! on `WriteAmplification` would be reached by the size probe, measured by `cargo xtask size`
//! and paid for by every shipped image, in exchange for a number only a rig reads. The
//! decorator costs the firmware nothing because the firmware never links it.
//!
//! # Why what was *asked* for
//!
//! Design document §12 is explicit that "a failed program may still have changed media", and
//! says the same of erase. A wear figure that only counted the calls that returned `Ok` would
//! understate exactly the runs that wore the part hardest — the ones full of interrupted
//! writes, which is every run this rig performs. `WriteAmplification` counts on the same
//! principle, and the two agree because of it.
//!
//! # Why the rig's own traffic is counted separately
//!
//! The rig writes a witness ([`crate::witness`]) so that what it knew survives the cut. Those
//! are programs and barriers the engine never issued. Reporting them inside the engine's
//! write amplification would publish the instrument's cost as the subject's — a figure that
//! gets worse the more carefully you measure. [`Traffic`] is the switch, and
//! [`Metered::wear`] is the engine's alone.
//!
//! # Why a per-effect figure is a fraction and not a number
//!
//! `waymaker-flash` refuses to divide because a divider is not free on `thumbv6m` and the
//! figure is the host's business. The rig is on the same target and takes the same care: a
//! per-effect figure is computed once at the end of a run rather than per record, and it is
//! `None` rather than zero when no effect ran, because a report that printed `0 B per effect`
//! would be reporting a division nobody performed.
//!
//! It is also a [`PerEffect`] — a numerator and a denominator — rather than a quotient. An
//! eight-effect run issues thirty-eight program calls, and `38 / 8` is `4`: an integer
//! quotient publishes **less wear than was measured**, every time the division is not exact.
//! That is the one direction a wear figure must not be wrong in, and it is the direction
//! truncation always fails in. The totals stay exact, the rendering carries two decimal
//! places, and [`PerEffect::is_exact`] says whether they were needed.

use waymaker_flash::append::WriteAmplification;
use waymaker_flash::storage::{Geometry, StableStorage};

/// Whose write a mutation is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Traffic {
    /// The engine under test: journal records, bank headers, generation seals.
    #[default]
    Engine,
    /// The rig's own bookkeeping: witness marks, and anything else the instrument writes.
    Rig,
}

/// What a run cost the media.
///
/// # Invariants
///
/// Every counter saturates, and for the six *numerators* — the erase and program counts, the
/// bytes, the barriers — that is the safe direction: clamping at `u32::MAX` over-reports wear,
/// and a figure that wrapped would report a part as almost unused after four billion bytes.
///
/// [`effects`](Self::effects) is a *denominator*, and saturating it upward biases every
/// per-effect figure downward, which is the flattering direction. It is unreachable — a run
/// of four billion effects is not one this rig performs, and
/// [`Rig::new`](crate::run::Rig::new) refuses a run whose records a `u16` cannot index — but
/// the invariant is worth stating as it actually is rather than one notch stronger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Wear {
    erase_operations: u32,
    erased_bytes: u32,
    erase_blocks: u32,
    program_operations: u32,
    programmed_bytes: u32,
    barriers: u32,
    effects: u32,
    payload_bytes: u32,
}

impl Wear {
    /// A run that has asked for nothing.
    pub const NONE: Self = Self {
        erase_operations: 0,
        erased_bytes: 0,
        erase_blocks: 0,
        program_operations: 0,
        programmed_bytes: 0,
        barriers: 0,
        effects: 0,
        payload_bytes: 0,
    };

    /// Every counter at its ceiling, so saturation is reachable in a test rather than only
    /// in a four-billion-iteration run.
    pub const SATURATED: Self = Self {
        erase_operations: u32::MAX,
        erased_bytes: u32::MAX,
        erase_blocks: u32::MAX,
        program_operations: u32::MAX,
        programmed_bytes: u32::MAX,
        barriers: u32::MAX,
        effects: u32::MAX,
        payload_bytes: u32::MAX,
    };

    /// How many `erase` calls the device was asked for.
    #[must_use]
    pub const fn erase_operations(self) -> u32 {
        self.erase_operations
    }

    /// How many bytes those calls named.
    #[must_use]
    pub const fn erased_bytes(self) -> u32 {
        self.erased_bytes
    }

    /// How many erase blocks those bytes covered.
    ///
    /// The figure a part's endurance is quoted in. Derived from the geometry at the moment
    /// of the call rather than at the end of the run, because a rig may meter more than one
    /// device.
    #[must_use]
    pub const fn erase_blocks(self) -> u32 {
        self.erase_blocks
    }

    /// How many `program` calls the device was asked for.
    #[must_use]
    pub const fn program_operations(self) -> u32 {
        self.program_operations
    }

    /// How many bytes those calls carried.
    #[must_use]
    pub const fn programmed_bytes(self) -> u32 {
        self.programmed_bytes
    }

    /// How many `barrier` calls the device was asked for.
    #[must_use]
    pub const fn barriers(self) -> u32 {
        self.barriers
    }

    /// How many effects the run completed, as the denominator of the per-effect figures.
    #[must_use]
    pub const fn effects(self) -> u32 {
        self.effects
    }

    /// Payload bytes the records carried.
    ///
    /// The one figure the meter cannot see for itself: a device is asked to program a frame,
    /// and how much of that frame was the caller's payload is the journal's knowledge. It is
    /// taken from [`WriteAmplification::payload_bytes`] rather than recomputed, and
    /// [`agrees_with`](Self::agrees_with) is what holds the two counters to each other.
    #[must_use]
    pub const fn payload_bytes(self) -> u32 {
        self.payload_bytes
    }

    /// How many bytes [`encode`](Self::encode) writes.
    ///
    /// Eight counters of four bytes each. A constant rather than a `size_of`, because it is a
    /// wire format: [`crate::log`] carries a run's wear in its line so that a violation found
    /// on a board is re-checkable on a host with the same figures in front of it, and a
    /// length that moved with the struct's layout would make an old log line unreadable.
    pub const ENCODED_BYTES: usize = 32;

    /// Every counter, little-endian, in a fixed order.
    ///
    /// # Errors
    ///
    /// `None` when `out` is shorter than [`ENCODED_BYTES`](Self::ENCODED_BYTES).
    #[must_use]
    pub fn encode(self, out: &mut [u8]) -> Option<usize> {
        let slot = out.get_mut(..Self::ENCODED_BYTES)?;
        let counters = [
            self.erase_operations,
            self.erased_bytes,
            self.erase_blocks,
            self.program_operations,
            self.programmed_bytes,
            self.barriers,
            self.effects,
            self.payload_bytes,
        ];
        for (word, counter) in slot.chunks_exact_mut(4).zip(counters) {
            word.copy_from_slice(&counter.to_le_bytes());
        }
        Some(Self::ENCODED_BYTES)
    }

    /// The wear [`encode`](Self::encode) wrote.
    ///
    /// The only way back into a [`Wear`] from numbers, and deliberately the only one: a
    /// caller building a wear from figures it made up is reporting a measurement that did not
    /// happen, and the sole caller here is a decoder handing back numbers a meter produced.
    ///
    /// # Errors
    ///
    /// `None` when `bytes` is shorter than [`ENCODED_BYTES`](Self::ENCODED_BYTES).
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let slot = bytes.get(..Self::ENCODED_BYTES)?;
        let mut counters = [0_u32; 8];
        for (counter, word) in counters.iter_mut().zip(slot.chunks_exact(4)) {
            *counter = u32::from_le_bytes(<[u8; 4]>::try_from(word).ok()?);
        }
        let [
            erase_operations,
            erased_bytes,
            erase_blocks,
            program_operations,
            programmed_bytes,
            barriers,
            effects,
            payload_bytes,
        ] = counters;
        Some(Self {
            erase_operations,
            erased_bytes,
            erase_blocks,
            program_operations,
            programmed_bytes,
            barriers,
            effects,
            payload_bytes,
        })
    }

    /// This wear carrying the payload total from the journal's own counters.
    #[must_use]
    pub const fn with_amplification(self, amplification: WriteAmplification) -> Self {
        Self {
            payload_bytes: amplification.payload_bytes(),
            ..self
        }
    }

    /// Whether the journal's counters and the meter's agree about the same traffic.
    ///
    /// They are the same measurement taken from opposite ends — the journal counts what it
    /// asked the device for, the meter counts what the device was asked for — so they must
    /// agree whenever nothing else is writing. A rig that consulted only one of them would
    /// not notice a writer that had started issuing programs it did not count.
    #[must_use]
    pub const fn agrees_with(self, amplification: WriteAmplification) -> bool {
        self.program_operations == amplification.program_operations()
            && self.programmed_bytes == amplification.programmed_bytes()
            && self.barriers == amplification.barriers()
            && self.payload_bytes == amplification.payload_bytes()
    }

    /// This wear with one more completed effect credited.
    #[must_use]
    pub const fn crediting_effect(self) -> Self {
        Self {
            effects: self.effects.saturating_add(1),
            ..self
        }
    }

    /// The two totals added, saturating in every field.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self {
            erase_operations: self.erase_operations.saturating_add(other.erase_operations),
            erased_bytes: self.erased_bytes.saturating_add(other.erased_bytes),
            erase_blocks: self.erase_blocks.saturating_add(other.erase_blocks),
            program_operations: self
                .program_operations
                .saturating_add(other.program_operations),
            programmed_bytes: self.programmed_bytes.saturating_add(other.programmed_bytes),
            barriers: self.barriers.saturating_add(other.barriers),
            effects: self.effects.saturating_add(other.effects),
            payload_bytes: self.payload_bytes.saturating_add(other.payload_bytes),
        }
    }

    /// `total` over the completed effects, or `None` when none completed.
    const fn per_effect(self, total: u32) -> Option<PerEffect> {
        if self.effects == 0 {
            None
        } else {
            Some(PerEffect {
                total,
                effects: self.effects,
            })
        }
    }

    /// Programmed bytes per completed effect.
    #[must_use]
    pub const fn programmed_bytes_per_effect(self) -> Option<PerEffect> {
        self.per_effect(self.programmed_bytes)
    }

    /// Payload bytes per completed effect.
    #[must_use]
    pub const fn payload_bytes_per_effect(self) -> Option<PerEffect> {
        self.per_effect(self.payload_bytes)
    }

    /// `program` calls per completed effect.
    #[must_use]
    pub const fn program_operations_per_effect(self) -> Option<PerEffect> {
        self.per_effect(self.program_operations)
    }

    /// `barrier` calls per completed effect.
    #[must_use]
    pub const fn barriers_per_effect(self) -> Option<PerEffect> {
        self.per_effect(self.barriers)
    }

    /// `erase` calls per completed effect.
    #[must_use]
    pub const fn erase_operations_per_effect(self) -> Option<PerEffect> {
        self.per_effect(self.erase_operations)
    }

    /// This wear with one erase of `len` bytes on a device with `erase_size`-byte blocks.
    const fn erasing(self, len: u32, erase_size: u32) -> Self {
        // `Geometry::new` refuses a zero erase size, so the guard is unreachable through a
        // real device; it is here because a division is not a place to be nearly sure.
        let blocks = if erase_size == 0 {
            0
        } else {
            len.div_ceil(erase_size)
        };
        Self {
            erase_operations: self.erase_operations.saturating_add(1),
            erased_bytes: self.erased_bytes.saturating_add(len),
            erase_blocks: self.erase_blocks.saturating_add(blocks),
            ..self
        }
    }

    /// This wear with one program of `len` bytes.
    const fn programming(self, len: u32) -> Self {
        Self {
            program_operations: self.program_operations.saturating_add(1),
            programmed_bytes: self.programmed_bytes.saturating_add(len),
            ..self
        }
    }

    /// This wear with one barrier.
    const fn barriering(self) -> Self {
        Self {
            barriers: self.barriers.saturating_add(1),
            ..self
        }
    }
}

/// A measured total over the effects that produced it.
///
/// A fraction rather than a quotient, because a quotient is wrong in the one direction a wear
/// figure must not be wrong in. An eight-effect run issues thirty-eight program calls, and
/// `38 / 8` is `4` — a published artifact reporting less wear than was measured, silently,
/// for every division that is not exact.
///
/// The numerator and the denominator are both kept, so a reader can check the arithmetic and a
/// consumer can re-derive it at whatever precision it wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PerEffect {
    total: u32,
    effects: u32,
}

impl PerEffect {
    /// The measured total.
    #[must_use]
    pub const fn total(self) -> u32 {
        self.total
    }

    /// How many effects it is over.
    ///
    /// Never zero: a run with no completed effects has no per-effect figure at all, and
    /// [`Wear::programmed_bytes_per_effect`] and its siblings answer `None` rather than
    /// dividing.
    #[must_use]
    pub const fn effects(self) -> u32 {
        self.effects
    }

    /// The integer part.
    #[must_use]
    pub const fn whole(self) -> u32 {
        match self.total.checked_div(self.effects) {
            Some(whole) => whole,
            // Unreachable: the constructor refuses a zero denominator.
            None => 0,
        }
    }

    /// The figure in hundredths, so a renderer can show two decimal places without a float.
    ///
    /// Saturating in the multiply rather than wrapping: a total that large is a reading of
    /// media rather than a count, and a wrapped figure would understate it — which is the
    /// failure this whole type exists to prevent.
    #[must_use]
    pub const fn hundredths(self) -> u32 {
        match self.total.saturating_mul(100).checked_div(self.effects) {
            Some(hundredths) => hundredths,
            None => 0,
        }
    }

    /// Whether the division is exact, so a report can say when a decimal is a rounding.
    #[must_use]
    pub const fn is_exact(self) -> bool {
        match self.total.checked_rem(self.effects) {
            Some(remainder) => remainder == 0,
            None => true,
        }
    }
}

impl core::fmt::Display for PerEffect {
    /// Two decimal places, computed in integers.
    ///
    /// Truncated at the hundredth rather than rounded, and that is deliberate in the same
    /// direction as everything else here: a report that rounded 4.995 up to 5.00 would be
    /// publishing a figure nobody measured, and one that rounded down understates by at most a
    /// hundredth of a byte. [`is_exact`](Self::is_exact) is how a caller tells the two apart.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let hundredths = self.hundredths();
        write!(formatter, "{}.{:02}", hundredths / 100, hundredths % 100)
    }
}

/// A [`StableStorage`] that counts what the one beneath it was asked for.
///
/// # Invariants
///
/// Every mutation is counted before it is issued, so a call that fails — or that never
/// returns, because the supply went — is counted exactly as §12 says it must be: a program
/// that failed may still have changed media.
///
/// A borrow rather than an owner, for [`crate::window::Window`]'s reason: a meter is a view
/// held for the length of a run, and the part is wanted back afterwards to be read by a
/// verifier that must not be metered.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Metered<'a, S> {
    storage: &'a mut S,
    engine: Wear,
    rig: Wear,
    traffic: Traffic,
}

impl<'a, S> Metered<'a, S> {
    /// Meters `storage`, attributing everything to the engine until told otherwise.
    pub const fn new(storage: &'a mut S) -> Self {
        Self {
            storage,
            engine: Wear::NONE,
            rig: Wear::NONE,
            traffic: Traffic::Engine,
        }
    }

    /// What the engine under test cost the part.
    #[must_use]
    pub const fn wear(&self) -> Wear {
        self.engine
    }

    /// What the rig's own bookkeeping cost it.
    #[must_use]
    pub const fn rig_wear(&self) -> Wear {
        self.rig
    }

    /// Both together — what the part actually endured.
    #[must_use]
    pub const fn total_wear(&self) -> Wear {
        self.engine.plus(self.rig)
    }

    /// Whose writes the following mutations are.
    pub const fn set_traffic(&mut self, traffic: Traffic) {
        self.traffic = traffic;
    }

    /// Whose writes the meter is currently attributing.
    #[must_use]
    pub const fn traffic(&self) -> Traffic {
        self.traffic
    }

    /// Credits one completed effect to the engine's denominator.
    pub const fn credit_effect(&mut self) {
        self.engine = self.engine.crediting_effect();
    }

    /// Attaches the journal's own counters to the engine's wear.
    pub const fn set_amplification(&mut self, amplification: WriteAmplification) {
        self.engine = self.engine.with_amplification(amplification);
    }

    /// The storage beneath, for a caller that needs it back.
    #[must_use]
    pub const fn inner(&self) -> &S {
        self.storage
    }

    /// The storage beneath, mutably.
    ///
    /// Mutations issued through this are *not* counted, which is the point: a rig that has to
    /// reach past its own meter is doing something the report should not claim to have seen.
    #[must_use]
    pub const fn inner_mut(&mut self) -> &mut S {
        self.storage
    }

    /// Applies `step` to whichever wear the current traffic names.
    fn charge(&mut self, step: impl FnOnce(Wear) -> Wear) {
        match self.traffic {
            Traffic::Engine => self.engine = step(self.engine),
            Traffic::Rig => self.rig = step(self.rig),
        }
    }
}

impl<S: StableStorage> StableStorage for Metered<'_, S> {
    type Error = S::Error;

    fn geometry(&self) -> Geometry {
        self.storage.geometry()
    }

    fn read(&mut self, offset: u32, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.storage.read(offset, dst)
    }

    fn program(&mut self, offset: u32, src: &[u8]) -> Result<(), Self::Error> {
        let len = u32::try_from(src.len()).unwrap_or(u32::MAX);
        self.charge(|wear| wear.programming(len));
        self.storage.program(offset, src)
    }

    fn erase(&mut self, offset: u32, len: u32) -> Result<(), Self::Error> {
        let erase_size = self.storage.geometry().erase_size();
        self.charge(|wear| wear.erasing(len, erase_size));
        self.storage.erase(offset, len)
    }

    fn barrier(&mut self) -> Result<(), Self::Error> {
        self.charge(Wear::barriering);
        self.storage.barrier()
    }
}
