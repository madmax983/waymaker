# ADR 0045: A durable intent carries its request, and `perform` checks it

- Status: accepted
- Date: 2026-09-14
- Issue: [#92](https://github.com/madmax983/waymaker/issues/92)
- Supersedes: nothing
- Related: [0025](0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md),
  [0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md)

## Context

ADR 0025 made step 4 of design document §07 unreachable without a `DurableIntent` — a value
only `Effect::schedule`, after the commit barrier, or `Effect::redelivering`, for a record an
earlier boot committed, can build. That value proved *an* effect was committed. It did not
prove *which* one.

`DurableIntent` held only an `EffectId`. `Activities::perform` took it beside two free
arguments, `kind: ActivityKind` and `input: &[u8]`. A caller could take the `DurableIntent`
`schedule` handed out for one request and call `perform` with a different kind or different
bytes. That compiled, dispatched the wrong effect in the world, and left a recoverable
schedule record describing the effect that did not run. A reboot would then redeliver the
record's own request — the wrong one, under an identity a downstream deduplicator accepts
without complaint.

Issue #92, found on review of a related change, states why nothing has broken: the shipped
driver builds its `EffectRequest` and its dispatch call from the same `kind` and `input`
locals, so the two cannot disagree in `waymaker-drive`'s own path. That is true by reading
one function, not by anything that would catch a second caller getting it wrong — and rung
0.4's dispatcher is exactly that second caller. §07 step 4 says "replay reconstructs input
and verifies its recorded digest"; the type did not carry a digest to verify against.

## Decision

**`DurableIntent` carries the `EffectRequest` it was scheduled with.** Not only the
`EffectId`. `DurableIntent::kind` reads the activity from that request. There is no second
argument anywhere that could name a different one — the kind cannot be separated from the
identity, in any caller's hands.

**`Dispatchable::perform` is the one route from a proof and raw bytes to a dispatch.** It
takes the `Activities` implementor and the input, checks the input's length and digest
against what step 3 committed, and only then calls `Activities::perform` — whose signature
drops the free `kind` argument, since `DurableIntent::kind` is now the only source of one.
A mismatch is `InputMismatch`, refused before the world is asked anything.

**`Effect::redelivering` threads the request through, with no new read of media and no
kernel-boundary change.** `waymaker-drive`'s own `decide` already builds an `EffectRequest`
from the workflow's *current* call and checks it against history with
`ReplayMachine::intent` before the redelivery row is even reached — a mismatch stops there,
as `Divergence::Kind` or `Divergence::Digest`. The `Half::Recorded` arm is only reached when
that check agreed, so the same, already-checked `request` value is what `redelivering`
binds into the redelivered `DurableIntent`. Threading the *replayed* `EffectScheduled`
record's own fields through `Resolve::Redeliver` — which issue #92 raised as the heavier
alternative, and which would touch `kernel-boundary`'s pinned types — turned out not to be
needed: the kernel has already vouched for the request the driver already holds.

**`effect-protocol` pins the two new methods.** `DurableIntent::kind` and
`Dispatchable::perform` join `EFFECT_PROTOCOL_SURFACE` and `EFFECT_TYPE_METHODS`. Nothing
about the step order, the construction sites, or the redelivery vocabulary changes: `perform`
calls none of `.stage(`, `.payload_barrier(`, `.commit(`, so it is not one of §07's storage
steps and needs no place in `EFFECT_STEP_BODIES`.

## Consequences

A caller cannot dispatch one effect's identity under another effect's kind — that is now a
fact about the argument list, checked by the compiler, for every caller there is or will be.
A caller cannot dispatch it under another effect's input either, without going out of their
way to call `Activities::perform` directly rather than through `Dispatchable::perform` — Rust
gives a trait method implemented by one crate and invoked by another no way to forbid that
entirely, so this is the strongest guarantee the language admits, not an unconditional one.
`Dispatchable::perform` is the sanctioned, ergonomic path, and it is the only one that costs
nothing to get right.

`DurableIntent` doubles in size — an `EffectId` plus an `EffectRequest`, both `Copy` and
stack-passed, so nothing here touches a heap that does not exist. `Dispatchable::perform`
recomputes the input digest that `waymaker-drive`'s own `decide` already computed once to
build the request in the first place: one redundant `C::frame_check` call per dispatch on
the shipped path, in exchange for the same check applying unconditionally to every other
caller. Measured rather than assumed: `cargo xtask size`'s gated `layers` figure moves from
ADR 0036's 12820 B to **12732 B** of the 13312 B gate — down, not up, because the wider
`DurableIntent` and the new checked call cost this optimiser less than the free `kind`
argument they replace. No raise is asked for.

`DriveError` gains `EffectInputMismatch`. Unreachable through `waymaker-drive`'s own
`Context::dispatch`, for the reason above; named anyway, because a `Result` this driver
cannot construct today is still a `Result` a future caller of `Dispatchable::perform` can.

What issue #92 asked to be written down if this were *not* fixed — a new entry in CLAUDE.md's
[What is not checked](../../CLAUDE.md#what-is-not-checked) — is not needed, because the gap
it would have named no longer exists.

## Alternatives considered

**Threading the replayed record through `Resolve::Redeliver`.** Issue #92's own "stronger"
reading of the redelivery half. Rejected once it became clear the driver already holds a
request the kernel has verified, at the one call site that matters — adding a second copy of
the same three fields to a kernel-boundary type pinned by `kernel-boundary` would duplicate
data already in scope, for no additional check.

**`Dispatchable` exposing the `EffectRequest` and leaving verification to the caller.**
Issue #92's "first, weaker" option. Rejected because it closes the kind half (there being no
second argument to disagree) but leaves the input half exactly where it was: a caller could
still read `dispatchable.intent()` and call `activities.perform(intent, wrong_bytes, out)`
with nothing to stop it, since nothing forces the caller through a checked path at all.

**A CRC comparison inside `Activities::perform` itself, left to each implementor.** Rejected:
it would ask every firmware author to reimplement the same check, get the same answer, and
carry the cost of a mistake in a trait method this crate cannot review. One checked call site
in `waymaker-drive` is the whole of the guarantee; an implementor obligation is not a
guarantee at all.
