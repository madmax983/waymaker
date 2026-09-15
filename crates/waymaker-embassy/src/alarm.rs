//! The hardware-alarm capability, and the route to an in-boot sleep.
//!
//! Design document §11's in-boot sleep, issue
//! [#110](https://github.com/madmax983/waymaker/issues/110).
//! [`TimerFuture`](crate::ctx::TimerFuture) asks [`Journal::wait`](crate::journal::Journal::wait)
//! on every poll; when the answer is "not yet", it arms an [`Alarm`] with the ticks still
//! owed and the task's own waker, instead of asking again straight away. A firmware with no
//! such peripheral uses [`NoAlarm`], and nothing here changes: the deadline is asked again
//! on the next poll, exactly as it was before this capability existed.

use core::task::Waker;

use waymaker_core::timer::ClockKind;

/// A hardware alarm this firmware can arm.
///
/// An implementation owns one countdown peripheral. [`TimerFuture`](crate::ctx::TimerFuture)
/// calls [`wake_after`](Self::wake_after) once a deadline has not passed, instead of asking
/// again straight away, so the executor can suspend the core until the interrupt wakes it.
///
/// # A missed or an early wake costs a poll, never correctness
///
/// The deadline is asked again on the next poll regardless of why it fired. So a firmware
/// with no interrupt for `kind`, or one that wakes early, only busy-polls once more — it
/// does not read history wrong. What must not happen is the opposite: an executor that
/// never polls again because nothing woke it.
///
/// `Send`, because [`wake_after`](Self::wake_after) is called from one context and its
/// `waker` is typically woken from another — an interrupt handler, on a real board.
pub trait Alarm: Send {
    /// Wake `waker` once `remaining` ticks of `kind` have passed.
    ///
    /// `remaining` is in `kind`'s own unit. The two clocks a firmware may have need not
    /// share a unit, so a caller handed a bare number could not tell which alarm to set or
    /// by how much to scale it — `kind` travels beside the figure precisely so it can.
    fn wake_after(&mut self, kind: ClockKind, remaining: u64, waker: &Waker);
}

/// No hardware alarm. [`wake_after`](Alarm::wake_after) does nothing.
///
/// A firmware with no countdown peripheral for [`TimerFuture`](crate::ctx::TimerFuture) to
/// arm uses this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoAlarm;

impl Alarm for NoAlarm {
    fn wake_after(&mut self, _kind: ClockKind, _remaining: u64, _waker: &Waker) {}
}
