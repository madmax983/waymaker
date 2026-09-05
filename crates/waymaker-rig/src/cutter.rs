//! The board's half of the rig: the thing that takes the power away, and the thing that
//! performs the effect.
//!
//! # Why these are a module of their own
//!
//! Two reasons, and the second is the one that decided it.
//!
//! They are the *interface*, and everything else in this crate is the implementation. A board
//! brings a `Cutter` that drives a MOSFET or a watchdog and a `Dispatcher` that toggles a pin;
//! it brings nothing else, because everything else is the same code the host runs.
//!
//! And [`Rig`](crate::run::Rig)'s public surface is pinned by the `rig-oracle` rule of
//! `cargo xtask check-layering`, which is a list of *names*. A trait with two implementations
//! declares its method three times in one file, and a pin that compares names cannot tell
//! those three apart from a fourth somebody added — which is the hole the pin exists to close.
//! So the traits live here and the runner lives next door.

use crate::phase::{Phase, ResetCause};
use crate::plan::Cut;

/// The physical effect a scheduled record stands for.
///
/// On a board this toggles a pin, drives a UART, or whatever the rig's operator can observe
/// afterwards. On a host it counts. What matters to §14's `durable-intent` is only that it
/// happens *after* the schedule record's commit barrier, which is [`Rig::iterate`](crate::run::Rig::iterate)'s job
/// rather than the implementer's.
pub trait Dispatcher {
    /// How this dispatcher reports a refusal.
    type Error;

    /// Performs the effect `effect` of the current iteration, given the input its schedule
    /// record described.
    ///
    /// # Errors
    ///
    /// Whatever the physical effect can fail with. A rig that meets one stops the iteration.
    fn dispatch(&mut self, effect: u16, input: &[u8]) -> Result<(), Self::Error>;
}

/// The thing that takes the power away.
pub trait Cutter {
    /// The rig has reached `phase` for `effect` and is about to do the work of it.
    ///
    /// # Postconditions
    ///
    /// On a board this arms the cut and returns `false`, or cuts and never returns. On a host
    /// it returns `true` to stop the iteration where it stands. A `Cutter` that always
    /// returned `false` turns the rig into a plain writer, which is what
    /// [`NeverCut`] is for.
    fn cut(&mut self, phase: Phase, cause: ResetCause, effect: u16) -> bool;
}

/// A cutter that never cuts.
///
/// The fault-free run, and the baseline every sweep needs: a rig whose clean run does not
/// pass has nothing to say about the runs that were interrupted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NeverCut;

impl Cutter for NeverCut {
    fn cut(&mut self, _phase: Phase, _cause: ResetCause, _effect: u16) -> bool {
        false
    }
}

/// A cutter that stops at the first moment the iteration's own [`Cut`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlannedCut {
    cut: Cut,
    effect: u16,
    fired: bool,
}

impl PlannedCut {
    /// Stops at `cut`, on a run of `effects` effects.
    ///
    /// Named `at` rather than `new` for the reason this module exists at all: the
    /// `rig-oracle` pin is a list of *names*, and `Rig` next door already has a `new`.
    #[must_use]
    pub fn at(cut: Cut, effects: u16) -> Self {
        Self {
            cut,
            effect: cut.effect_index(effects),
            fired: false,
        }
    }

    /// Whether the cut has been reached.
    #[must_use]
    pub const fn fired(self) -> bool {
        self.fired
    }

    /// Which effect it is armed against.
    #[must_use]
    pub const fn effect(self) -> u16 {
        self.effect
    }
}

impl Cutter for PlannedCut {
    fn cut(&mut self, phase: Phase, _cause: ResetCause, effect: u16) -> bool {
        if !self.fired && phase == self.cut.phase() && effect == self.effect {
            self.fired = true;
            return true;
        }
        false
    }
}
