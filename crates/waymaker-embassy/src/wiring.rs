//! The dispatch wiring: a table of numeric kinds over one world.
//!
//! Issue [#36](https://github.com/madmax983/waymaker/issues/36). A firmware that writes
//! [`ActivityDispatcher`] by hand writes a `match` over numbers and a state machine around
//! it. [`Table`] is that `match`, once: a row per activity, and each row is an ordinary
//! function.
//!
//! # What a row is keyed by
//!
//! Its number. [`ActivityKind`] is what a schedule record holds, so the number is the only
//! thing replay can reproduce. A row also carries a **name**, and the name is compile-time
//! metadata for a log: [`Table::name_of`] and [`Activity::name`] are what read it, no lookup
//! takes one, and no record holds one. Design document §09's `EffectScheduled` carries a
//! sequence, a kind, a length and a digest, and the `effect-scheduled-fields` rule is what
//! keeps it that way.
//!
//! # What is deliberately absent
//!
//! Issue #36 names two non-goals: no dynamic workflow loading, and no string-addressed
//! activity registry. Both are *additions* — a `Table::by_name`, a `Table::register`, a
//! row read from media — and the `dispatch-wiring` rule is what makes each a line a
//! reviewer writes rather than a commit.

use core::task::{Context as Task, Poll};

use waymaker_core::{ActivityKind, EffectId};

use crate::dispatch::{ActivityDispatcher, Produced};

/// What one row does.
///
/// A plain function pointer, so a table is a `const` and costs no allocation. `W` is the
/// world every row shares — the peripherals, the buffers, the connection — and `E` is what
/// a row fails with.
///
/// The arguments are [`ActivityDispatcher::poll_dispatch`]'s, less the kind: selection has
/// already happened, and a row that served two numbers would be two rows.
pub type Perform<W, E> =
    fn(&mut W, &mut Task<'_>, EffectId, &[u8], &mut [u8]) -> Poll<Result<Produced, E>>;

/// One activity: a number, a name, and what performs it.
///
/// The fields are private and [`new`](Self::new) is the only constructor, so a row is
/// always all three.
#[derive(Clone, Copy, Debug)]
pub struct Activity<W, E> {
    kind: ActivityKind,
    name: &'static str,
    perform: Perform<W, E>,
}

impl<W, E> Activity<W, E> {
    /// The row `perform` answers `kind` with, known to a log as `name`.
    #[must_use]
    pub const fn new(kind: ActivityKind, name: &'static str, perform: Perform<W, E>) -> Self {
        Self {
            kind,
            name,
            perform,
        }
    }

    /// The number this row answers.
    #[must_use]
    pub const fn kind(&self) -> ActivityKind {
        self.kind
    }

    /// What a log calls this row.
    ///
    /// Diagnostics only. Nothing selects on it and no record holds it.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// Why a table could not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unhandled<E> {
    /// The row ran and failed. `E` is the world's own reason.
    Activity(E),
    /// No row declares this number.
    ///
    /// The workflow asked for an activity this build does not have. It is recorded as an
    /// `EffectFailed` with no payload, so the run makes progress: design document §08 has
    /// no edge from an unresolved effect to a terminal record, so a refusal would strand
    /// the run, and a [`Poll::Pending`] would spin for ever with nothing to say why.
    ///
    /// That decision is permanent — replay returns the recorded failure on every later
    /// boot, including one whose firmware has the row. A table must therefore declare
    /// every number its workflow asks for.
    NoSuchActivity(ActivityKind),
}

/// A world, and the table that says which number is which activity.
///
/// It borrows the rows and owns the world. The rows are a `const` a firmware writes once;
/// the world is what they act through.
#[derive(Clone, Copy, Debug)]
pub struct Table<'t, W, E> {
    world: W,
    rows: &'t [Activity<W, E>],
}

impl<'t, W, E> Table<'t, W, E> {
    /// A dispatcher that reaches `world` through `rows`.
    ///
    /// `rows` is read top to bottom, so where two rows declare one number the first
    /// answers. Nothing refuses the second, because a run-time refusal would fail the boot
    /// over a table a reviewer can check by eye.
    #[must_use]
    pub const fn over(world: W, rows: &'t [Activity<W, E>]) -> Self {
        Self { world, rows }
    }

    /// The world, for a caller that wants to look at what the rows did.
    #[must_use]
    pub const fn world(&self) -> &W {
        &self.world
    }

    /// The world, to set up before a boot or read after one.
    #[must_use]
    pub const fn world_mut(&mut self) -> &mut W {
        &mut self.world
    }

    /// What a log calls `kind`, or [`None`] if no row declares it.
    ///
    /// Diagnostics only. It is not on the dispatch path: `poll_dispatch` selects by number,
    /// so a firmware that dropped every name would behave identically.
    #[must_use]
    pub fn name_of(&self, kind: ActivityKind) -> Option<&'static str> {
        self.row(kind).map(|row| row.name)
    }

    /// The first row declaring `kind`.
    ///
    /// The whole of selection, and it reads a number. A row's label is not consulted here
    /// or anywhere on the dispatch path — `dispatch-wiring` is what says so.
    fn row(&self, kind: ActivityKind) -> Option<&Activity<W, E>> {
        self.rows.iter().find(|row| row.kind == kind)
    }
}

impl<W, E> ActivityDispatcher for Table<'_, W, E> {
    type Error = Unhandled<E>;

    fn poll_dispatch(
        &mut self,
        task: &mut Task<'_>,
        id: EffectId,
        kind: ActivityKind,
        input: &[u8],
        out: &mut [u8],
    ) -> Poll<Result<Produced, Unhandled<E>>> {
        // By number, and by nothing else. The function pointer is copied out first, so the
        // borrow of `self.rows` ends before the row runs against `self.world`.
        let Some(perform) = self.row(kind).map(|row| row.perform) else {
            return Poll::Ready(Err(Unhandled::NoSuchActivity(kind)));
        };
        // The task's waker travels to the row and no further: this crate registers none of
        // its own, exactly as `Ctx` does not.
        match perform(&mut self.world, task, id, input, out) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(produced)) => Poll::Ready(Ok(produced)),
            Poll::Ready(Err(failed)) => Poll::Ready(Err(Unhandled::Activity(failed))),
        }
    }
}
