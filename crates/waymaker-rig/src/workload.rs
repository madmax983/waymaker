//! The run the rig cuts into.
//!
//! # Why the workload is derived rather than configured
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27)'s third "done when" is that
//! "any recovery violation is reproducible from the rig's log". A log line can carry a seed
//! and an iteration number; it cannot carry the bytes of every record of a run that has been
//! going for six hours. So the run *is* the seed and the iteration: every payload, every
//! length and every digest is derived from them, and a host handed those two numbers can
//! rebuild the identical run.
//!
//! # The shape of a run
//!
//! `RunStarted`, then a `EffectScheduled`/`EffectCompleted` pair per effect, then
//! `RunCompleted`. That is the smallest history that reaches all three of the write points
//! issue #27 names and is legal under design document §08's transition table — in particular
//! it never declares a schedule while one is unresolved, which
//! [`ReplayCursor`](waymaker_core::replay::ReplayCursor) refuses as malformed history.
//!
//! # Why the payloads are never empty and never uniform
//!
//! A record whose payload is empty is a record whose frame is all header, and a run whose
//! records are byte-identical is a run in which a torn record from iteration 4 is
//! indistinguishable from a whole one from iteration 5. Both are ways for a rig to agree with
//! a bug. Every payload here depends on the seed, the iteration and the record index.

use waymaker_core::{ActivityKind, EffectSeq, RecordRef, RunId};
use waymaker_flash::frame::input_digest;

use crate::plan::SplitMix64;

/// What a record of the run is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// The opening `RunStarted`.
    Start,
    /// The `EffectScheduled` for this effect.
    Schedule(u16),
    /// The `EffectCompleted` for this effect.
    Completion(u16),
    /// The terminal `RunCompleted`.
    Finish,
}

/// A deterministic run, derived from a seed and an iteration number.
///
/// # Invariants
///
/// [`record`](Self::record) is a pure function of the workload and the index: two workloads
/// built from the same three numbers produce the same records, byte for byte, for ever. That
/// is what [`crate::log`] rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Workload {
    seed: u64,
    iteration: u32,
    effects: u16,
}

impl Workload {
    /// The longest payload any record of any workload carries.
    ///
    /// A caller sizes one page from this and every record of the run fits it, which is what
    /// keeps a rig's RAM a constant rather than a function of its seed.
    pub const MAX_PAYLOAD_BYTES: usize = 16;

    /// The workflow this run executes, as the bank header would pin it.
    pub const WORKFLOW_KIND: u16 = 0x27;

    /// Its version.
    pub const WORKFLOW_VERSION: u16 = 1;

    /// The run seeded with `seed`, at `iteration`, scheduling `effects` effects.
    #[must_use]
    pub const fn new(seed: u64, iteration: u32, effects: u16) -> Self {
        Self {
            seed,
            iteration,
            effects,
        }
    }

    /// The seed this run was derived from.
    #[must_use]
    pub const fn seed(self) -> u64 {
        self.seed
    }

    /// Which iteration of the rig this run belongs to.
    #[must_use]
    pub const fn iteration(self) -> u32 {
        self.iteration
    }

    /// How many effects it schedules.
    #[must_use]
    pub const fn effects(self) -> u16 {
        self.effects
    }

    /// The run identity, derived so that two iterations are two runs.
    #[must_use]
    pub const fn run(self) -> RunId {
        RunId(SplitMix64::new(self.seed).at(self.iteration as u64))
    }

    /// The activity every effect of this run dispatches.
    ///
    /// Not zero: a zeroed page is what a partially programmed record reads back as, and a
    /// workload that used the kind a torn record shows would agree with a tear.
    #[must_use]
    pub const fn activity(self) -> ActivityKind {
        ActivityKind(1)
    }

    /// The largest number of effects a run can have.
    ///
    /// A run is `2 * effects + 2` records and a record index is a `u16`, so this is where the
    /// arithmetic runs out. Stated as a constant and refused at the boundary rather than
    /// saturated: a saturating count shortens a run exactly as silently as a wrapping one, and
    /// at 32767 effects the run it produced lost its `RunCompleted` — an §08-legal history
    /// that can never terminate.
    pub const MAX_EFFECTS: u16 = (u16::MAX - 2) / 2;

    /// How many records the whole run writes, or `None` for a run too long to index.
    ///
    /// `Option` rather than a saturating number, because the only honest answers to "how many
    /// records does a run of 40000 effects have" are 80002 and "not a run this rig performs".
    /// [`Rig::new`](crate::run::Rig::new) refuses the second at construction, so every
    /// workload a rig actually holds answers `Some`.
    #[must_use]
    pub const fn records(self) -> Option<u16> {
        if self.effects > Self::MAX_EFFECTS {
            return None;
        }
        match self.effects.checked_mul(2) {
            Some(pairs) => pairs.checked_add(2),
            None => None,
        }
    }

    /// What record `index` is for, or `None` past the end of the run.
    #[must_use]
    pub const fn role(self, index: u16) -> Option<Role> {
        let Some(records) = self.records() else {
            return None;
        };
        if index >= records {
            return None;
        }
        if index == 0 {
            return Some(Role::Start);
        }
        let position = index - 1;
        let effect = position / 2;
        if effect >= self.effects {
            return Some(Role::Finish);
        }
        if position % 2 == 0 {
            Some(Role::Schedule(effect))
        } else {
            Some(Role::Completion(effect))
        }
    }

    /// Which record index carries `effect`'s schedule, or `None` if the run has no such
    /// effect.
    #[must_use]
    pub const fn schedule_index(self, effect: u16) -> Option<u16> {
        if effect >= self.effects {
            return None;
        }
        match effect.checked_mul(2) {
            Some(offset) => offset.checked_add(1),
            None => None,
        }
    }

    /// Which record index carries `effect`'s completion.
    #[must_use]
    pub const fn completion_index(self, effect: u16) -> Option<u16> {
        match self.schedule_index(effect) {
            Some(index) => index.checked_add(1),
            None => None,
        }
    }

    /// Fills `out` with a payload derived from this run and `salt`, and returns the prefix
    /// that was written.
    ///
    /// Between one and [`MAX_PAYLOAD_BYTES`](Self::MAX_PAYLOAD_BYTES) bytes: never empty,
    /// because a record with no payload is all header and the interesting tears are in the
    /// payload.
    fn payload(self, salt: u64, out: &mut [u8]) -> Option<&[u8]> {
        let word = SplitMix64::new(self.seed ^ salt).at(u64::from(self.iteration));
        // The modulus is at most `MAX_PAYLOAD_BYTES`, which is sixteen, so the narrowing is a
        // fact about this file rather than a conversion a caller could get wrong. Written as
        // one so that it is checked rather than asserted.
        let len = 1 + usize::try_from(word % Self::MAX_PAYLOAD_BYTES as u64).ok()?;
        let slot = out.get_mut(..len)?;
        let mut generator = SplitMix64::new(word);
        for byte in slot.iter_mut() {
            // The low byte of each output. A `u64` mixed down to eight bits is what a
            // deterministic payload needs, and truncation is the operation rather than a
            // hazard — spelled with a mask so that it says so.
            *byte = generator.next().to_le_bytes().first().copied()?;
        }
        out.get(..len)
    }

    /// The input `effect`'s schedule record describes, written into `out`.
    ///
    /// # Postconditions
    ///
    /// The same bytes the schedule record's `input_len` and `input_crc` were computed over,
    /// which is what makes the run non-divergent under §08.
    #[must_use]
    pub fn effect_input(self, effect: u16, out: &mut [u8]) -> Option<&[u8]> {
        if effect >= self.effects {
            return None;
        }
        self.payload(0x1111_1111_0000_0000 ^ u64::from(effect), out)
    }

    /// Record `index` of the run, borrowing its payload from `out`.
    ///
    /// # Postconditions
    ///
    /// `None` past the end of the run, and `None` rather than a truncated record when `out`
    /// is shorter than the payload — a rig that silently shortened a record would be
    /// measuring a run it did not declare.
    #[must_use]
    pub fn record(self, index: u16, out: &mut [u8]) -> Option<RecordRef<'_>> {
        match self.role(index)? {
            Role::Start => {
                let input = self.payload(0x5555_5555_5555_5555, out)?;
                Some(RecordRef::RunStarted {
                    workflow_kind: Self::WORKFLOW_KIND,
                    workflow_version: Self::WORKFLOW_VERSION,
                    input,
                })
            }
            Role::Schedule(effect) => {
                let input = self.effect_input(effect, out)?;
                let input_len = u16::try_from(input.len()).ok()?;
                let input_crc = input_digest(input);
                Some(RecordRef::EffectScheduled {
                    seq: EffectSeq(u32::from(effect)),
                    kind: self.activity(),
                    input_len,
                    input_crc,
                })
            }
            Role::Completion(effect) => {
                let result = self.payload(0x2222_2222_0000_0000 ^ u64::from(effect), out)?;
                Some(RecordRef::EffectCompleted {
                    seq: EffectSeq(u32::from(effect)),
                    result,
                })
            }
            Role::Finish => {
                let result = self.payload(0x9999_9999_9999_9999, out)?;
                Some(RecordRef::RunCompleted { result })
            }
        }
    }
}
