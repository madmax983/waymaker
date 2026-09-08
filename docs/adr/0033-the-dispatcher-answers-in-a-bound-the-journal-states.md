# ADR 0033: the dispatcher answers in a bound the journal states, and a row is a number

- Status: accepted
- Date: 2026-09-08
- Issue: [#36](https://github.com/madmax983/waymaker/issues/36)
- Supersedes: nothing
- Related: [0025](0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md),
  [0026](0026-redelivery-is-the-kernels-answer-and-at-least-once-is-the-contract.md),
  [0032](0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md)

## Context

Design document §13 sketches the activity dispatcher as an `async fn`. Issue
[#35](https://github.com/madmax983/waymaker/issues/35) needed one to build `Ctx`, so
`ActivityDispatcher` landed there in poll form. Three things it left owed are issue #36's,
and the crate's own documentation named each.

**The bound.** §10 prices a bank against `Bounds`, and `effect_result_bytes` is how wide an
activity answer may be. `Ctx::new` asks for a buffer as wide as the *wider* of the run's two
bounds, because a terminal payload goes through the same buffer. So the buffer is not the
bound. The façade handed the dispatcher all of it and checked the reported length against
`out.len()`, which is a different figure — and `Answer::Completed`'s own documentation says
"within the run's declared bound". `waymaker-drive` re-checked and refused, so no truncated
record ever reached media through *this* driver; a `Journal` implementor that believed the
documentation would have written one. Issue #36 asks for the length to be "validated against
the bound before any record is written", and validated against `out.len()` is not that.

**A typed failure payload.** §09 gives `EffectFailed` a bounded payload. The trait had no
route to one: `Ok(len)` was recorded as an `EffectCompleted` and `Err(E)` as a failure with
no payload, so an activity could report bytes or failure and never both. `dispatch.rs` said
so and named this issue.

**Names, and the two non-goals.** Issue #36 says "numeric `ActivityKind` on the dispatch
path. Activity names are compile-time metadata for logs and diagnostics and are never stored
in records", and "no dynamic workflow loading and no string-addressed activity registry —
that is an explicit non-goal". Nothing in the workspace held any of that, because nothing
had a name at all.

## Decision

**The journal states the bound with the identity.** `Handoff::Dispatch` carries
`result_bytes` beside `id`, and `waymaker-drive` fills it from `reserve.bounds()` — the same
figure `Boundary::resolve` refuses an answer against, read from the reserve that priced the
bank. `ActivityFuture` then hands the dispatcher `out[..min(result_bytes, out.len())]`, so a
dispatcher **cannot write past the bound**, and a reported length over it is
`Answer::Exhausted`. That is the difference between a check and an impossibility, and it is
what issue #36's second work item asks for. A caller that undersized its buffer gets the
smaller of the two figures, so the undersized case is `Exhausted` and never a short record.

**`Produced` is what a dispatch answers.** `poll_dispatch` returns
`Poll<Result<Produced, Self::Error>>`, where `Produced` is `Completed(usize)` or
`Failed(usize)`. `Produced::Failed` is §09's bounded failure payload, recorded and replayed;
`Err(E)` stays "failed with nothing recordable", for the implementor's log. Both shapes are
bounded by one figure, so a failure payload cannot smuggle bytes past a result bound.

**`wiring::Table` is the ergonomic form, and a row is a number.** A table is a `const` slice
of `Activity` rows, each a `(kind, name, fn)` triple, and `Table::over(world, rows)`
implements `ActivityDispatcher`. Selection is `rows.iter().find(|row| row.kind == kind)`.
The name is reachable only through `Table::name_of` and `Activity::name`, and never on the
dispatch path.

The `dispatch-wiring` rule is what holds the two non-goals: it pins both modules' public
surfaces in both directions, pins `Activity`'s and `Table`'s methods at every visibility,
refuses a public field on either, and refuses a selection body that names a row's label. A
`Table::by_name`, a `Table::register` or a `pub rows` field would each break no layering
rule and pass every test, because the run still completes.

`Unhandled::NoSuchActivity` is what a kind no row declares answers with. It is recorded as a
failure with no payload, which is a **permanent** decision for that run — the consequences
section says what that costs.

`§13`'s `async fn dispatch` is not delivered and cannot be here. The future an `async fn` in
a trait returns borrows the dispatcher, `ActivityFuture` already holds that borrow, and
storing it across polls needs a self-referential value or an allocation. The poll form
stores nothing, and `Table` is the ergonomics instead: a row per activity, and no state
machine to write.

## Consequences

An unknown kind is recorded as a failure, and replay answers the same way on every later
boot — including a boot whose firmware has the row. That is worse than it sounds and better
than the two alternatives. A refusal strands the run, because §08 has no edge from an
unresolved effect to a terminal record; a `Poll::Pending` spins for ever with nothing to say
why. A route for "this firmware cannot service this kind" — the mirror of `TimerSpec`'s
`IncompatibleWorkflow` for a recorded clock kind — has no place in this trait, and is issue
[#111](https://github.com/madmax983/waymaker/issues/111).

`Handoff::Dispatch` is a struct variant in two crates now, so a caller that split §07's two
halves reads `Handoff::Dispatch { id, result_bytes }`. That is churn for a figure most
callers ignore, and it is the price of the bound being the journal's word rather than the
caller's.

The bound is the journal's word, and nothing verifies that the journal states the figure the
run was priced against. `waymaker-drive` reads it from the reserve, so the two cannot
disagree there; another `Journal` implementor could state anything. It is a precondition on
the implementor, the same standing as `Swap::beginning`'s two unverified arguments.

`ctx-facade`'s `static` ban had to be narrowed. It fired on the identifier anywhere on a
line, so `&'static str` — which is what compile-time metadata is spelled as — failed the
gate. The lifetime is set aside now and the item forms are unchanged;
`a_static_item_beside_a_static_lifetime_is_still_reported` is what keeps the narrowing from
becoming a hole.

The code-flash figure is worth recording, because the interesting half of it did not move.
The *gated* row is §04's "core + flash adapter", and it reads **12222 B** of the 12288 B
gate before this change and after it: none of this is in the two crates the budget is stated
over. The `facade` row goes from 12516 B to **12618 B** — the table, the bound and the two
`Produced` shapes cost **102 B**, driven by the size probe rather than inferred, because
`size-probe-reach` makes the probe name every public function the wiring declares. That row
is printed rather than gated, and issue
[#39](https://github.com/madmax983/waymaker/issues/39) is where it becomes a gate.

## Alternatives considered

**A fifth `Journal` method for the bound.** `CTX_JOURNAL_SURFACE` says four, and says why: a
fifth is a question the façade would be answering for itself. A bound is not a question about
history, so the objection is weaker than usual — but the figure is only ever wanted at the
moment an intent is committed, and putting it on the handoff says exactly that.

**`Ctx::new` taking the bound.** It is the caller's word rather than the journal's, and the
caller is who sized the buffer that is not the bound. The disagreement this change exists to
close would have moved rather than gone.

**Checking rather than narrowing.** A `len > result_bytes` test alone leaves a dispatcher
free to write into the part of the buffer the run's terminal payload will use. Narrowing the
slice makes the overwrite impossible and the check redundant, and both are kept because a
dispatcher may still *report* a length over the bound.

**A `Produced::Exhausted`.** The trait could let a dispatcher say "the answer does not fit",
as `Performed::Exhausted` does on the synchronous path. It is unnecessary here: `out` is
exactly the bound, so a length over it says the same thing, and one figure is easier to be
right about than two.

**A blanket `impl ActivityDispatcher` for a simpler trait.** A blanket impl conflicts with
every hand-written dispatcher, so it would have to be a newtype either way — and a newtype
over one function is less useful than a newtype over a table, which is the shape a firmware
with several activities actually has.
