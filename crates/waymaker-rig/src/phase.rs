//! The three write points the supply is cut during, the two ways a device comes back, and
//! the census over the six cells they make.
//!
//! # Why three write points
//!
//! Design document §07 puts a durability barrier between the intent and the effect, and §08
//! is the transition table that says what may follow what. Between them they make three
//! moments in an effect's life at which losing power means something different:
//!
//! * [`Schedule`](Phase::Schedule) — the `EffectScheduled` record is going to media. Either
//!   it committed, in which case the effect is owed a dispatch, or it did not, in which case
//!   nothing is owed and the run replays as though the call had never been made.
//! * [`Dispatch`](Phase::Dispatch) — the schedule record's barrier has returned and the
//!   physical effect is under way. Nothing is being written. This is the interval §02
//!   decision 3 exists to create, and the one a media-only crash sweep cannot see, because
//!   no media operation is in flight to interrupt.
//! * [`Completion`](Phase::Completion) — the `EffectCompleted` record is going to media.
//!   Either it committed, and replay hands the result back, or it did not, and the effect is
//!   redelivered under the identity §14's `stable-redelivery` pins.
//!
//! # Why two reset causes
//!
//! Issue [#27](https://github.com/madmax983/waymaker/issues/27) says it outright: "a watchdog
//! reset is not identical to a brownout and both must be covered". They differ in what the
//! flash controller was allowed to finish and in what the RAM holds afterwards:
//!
//! * [`PowerCut`](ResetCause::PowerCut) — the supply goes. A program in flight stops where it
//!   stopped, possibly inside a program unit and possibly leaving bits at neither level. RAM
//!   is gone.
//! * [`Watchdog`](ResetCause::Watchdog) — the core is reset while the supply holds. The flash
//!   controller is not reset with it on every part, so a program unit already handed to it
//!   may complete *after* the core has stopped believing in it; and RAM is not cleared, so
//!   stale state can be read back by firmware that assumes a cold start.
//!
//! Treating the second as the first is the comfortable mistake, and it is comfortable in the
//! direction that hides bugs: a rig that only ever tore programs would never produce the
//! world in which a write the writer never learned about is nevertheless whole on media.
//!
//! # Where the census is
//!
//! [`crate::census`], not here. The two enums have parallel accessors — an `index`, a
//! `from_index` and a `name` each — and the census's own surface is pinned by the
//! `rig-oracle` rule of `cargo xtask check-layering`, which is a list of *names*: a pin
//! covering this file could not tell `Phase::index` from `ResetCause::index`, and a pin that
//! cannot tell two things apart is a pin that can be slipped past. So the vocabulary lives
//! here and the obligation lives next door.

/// A moment in an effect's life at which the device can be reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// The `EffectScheduled` record is being written.
    Schedule,
    /// The schedule record has committed and the physical effect is under way.
    Dispatch,
    /// The `EffectCompleted` record is being written.
    Completion,
}

impl Phase {
    /// The three, in the order issue #27 names them.
    ///
    /// [`index`](Self::index) is a position in this array, so the census cannot credit one
    /// cell for another's coverage.
    pub const ALL: [Self; 3] = [Self::Schedule, Self::Dispatch, Self::Completion];

    /// Where this phase falls in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Schedule => 0,
            Self::Dispatch => 1,
            Self::Completion => 2,
        }
    }

    /// The phase at `index`, or `None` past the end.
    #[must_use]
    pub const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Schedule),
            1 => Some(Self::Dispatch),
            2 => Some(Self::Completion),
            _ => None,
        }
    }

    /// A short static name, for a log a device with no allocator writes.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Dispatch => "dispatch",
            Self::Completion => "completion",
        }
    }
}

/// How the device came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResetCause {
    /// The supply was removed.
    PowerCut,
    /// The core was reset while the supply held.
    Watchdog,
}

impl ResetCause {
    /// Both, in the order issue #27 names them.
    pub const ALL: [Self; 2] = [Self::PowerCut, Self::Watchdog];

    /// Where this cause falls in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::PowerCut => 0,
            Self::Watchdog => 1,
        }
    }

    /// The cause at `index`, or `None` past the end.
    #[must_use]
    pub const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::PowerCut),
            1 => Some(Self::Watchdog),
            _ => None,
        }
    }

    /// A short static name, for a log a device with no allocator writes.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::PowerCut => "power-cut",
            Self::Watchdog => "watchdog",
        }
    }
}
