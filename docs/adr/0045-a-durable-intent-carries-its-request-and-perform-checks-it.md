# ADR 0045: A durable intent carries its request, and `perform` checks it

- Status: accepted
- Date: 2026-09-14
- Issue: [#92](https://github.com/madmax983/waymaker/issues/92)
- Supersedes: nothing
- Related: [0025](0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md),
  [0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md)

## Context

ADR 0025 made step 4 of design document §07 unreachable without a `DurableIntent`. Only two
functions can build one: `Effect::schedule`, after the commit barrier, and
`Effect::redelivering`, for a record an earlier boot committed. That value proved *an* effect
was committed. It did not prove *which* one.

`DurableIntent` held only an `EffectId`. `Activities::perform` took it beside two free
arguments, `kind: ActivityKind` and `input: &[u8]`. A caller could take the `DurableIntent`
that `schedule` returned for one request. Then call `perform` with a different kind or
different bytes. That compiled, dispatched the wrong effect in the world, and left a
recoverable schedule record describing the effect that did not run. A reboot would then
redeliver the record's own, wrong request. A downstream deduplicator would accept it without
complaint, under the same identity.

Issue #92, found on review of a related change, explains why nothing has broken so far. The
shipped driver builds its `EffectRequest` and its dispatch call from the same `kind` and
`input` locals. So the two cannot disagree, on `waymaker-drive`'s own path. That is true only
by reading one function. Nothing catches a second caller getting it wrong — and rung 0.4's
dispatcher is exactly that second caller. §07 step 4 says "replay reconstructs input and
verifies its recorded digest"; the type did not carry a digest to verify against.

## Decision

**`DurableIntent` now carries the `EffectRequest` it was scheduled with, not only the
`EffectId`.** `DurableIntent::kind` reads the activity from that request. There is no second
argument anywhere that could name a different kind. The kind cannot be separated from the
identity, in any caller's hands.

**`Dispatchable::perform` is the one route from a proof and raw bytes to a dispatch.** It
takes the `Activities` implementor and the input. It checks the input's length and digest
against what step 3 committed. Only then does it call `Activities::perform`. That call's
signature drops the free `kind` argument, because `DurableIntent::kind` is now the only
source of one. A mismatch is `InputMismatch`, refused before the world is asked anything.

**`Activities::perform` takes a `CheckedInput`, not a `&[u8]`.** `CheckedInput`'s field is
private, and `Dispatchable::perform` is the only place that builds one — this crate's own
`compile_fail` doctest shows a caller cannot construct one elsewhere. So a caller cannot
reach `Activities::perform` with unchecked bytes by calling it directly either: there is no
bytes-to-`CheckedInput` route outside the checked one. Codex found this exact bypass on
review of this change's first version, which left `Activities::perform` taking `&[u8]` — a
caller with a `DurableIntent` from `Dispatchable::intent()` could still call
`activities.perform(intent, wrong_bytes, out)` directly. `CheckedInput` closes it.

**`Effect::redelivering` threads the request through, with no new read of media and no
kernel-boundary change.** `waymaker-drive`'s own `decide` already builds an `EffectRequest`
from the workflow's *current* call. It checks that request against history with
`ReplayMachine::intent`, before the redelivery row is even reached. A mismatch stops there,
as `Divergence::Kind` or `Divergence::Digest`. The `Half::Recorded` arm is reached only when
that check agreed. So `redelivering` binds the same, already-checked `request` value into the
redelivered `DurableIntent`. Issue #92 raised a heavier alternative: thread the *replayed*
`EffectScheduled` record's own fields through `Resolve::Redeliver`. That would touch
`kernel-boundary`'s pinned types. It turned out not to be needed. The kernel has already
vouched for the request the driver already holds.

**`effect-protocol` pins the two new methods.** `DurableIntent::kind` and
`Dispatchable::perform` join `EFFECT_PROTOCOL_SURFACE` and `EFFECT_TYPE_METHODS`. Nothing
changes about the step order, the construction sites, or the redelivery vocabulary. `perform`
calls none of `.stage(`, `.payload_barrier(`, `.commit(`. So it is not one of §07's storage
steps, and needs no place in `EFFECT_STEP_BODIES`.

## Consequences

A caller cannot dispatch one effect's identity under another effect's kind, and cannot
dispatch it under another effect's input either. Both are now facts the compiler checks, for
every caller there is or will be — including a caller who calls `Activities::perform`
directly, bypassing `Dispatchable::perform`, since there is still no bytes it could pass that
argument. This is closer to absolute than most guarantees a trait boundary between two crates
can state: the one gap left is a caller inside `waymaker-drive` itself reaching into
`effect.rs`'s own module to build a `CheckedInput` by hand, which is a source change to this
crate, not a misuse of its public API.

`DurableIntent` doubles in size: an `EffectId` plus an `EffectRequest`. Both are `Copy` and
stack-passed. So nothing here touches a heap — this engine has none. `CheckedInput` costs
nothing: a one-field wrapper around `&[u8]` has the same layout as `&[u8]`, and the optimiser
erases it. `Dispatchable::perform` recomputes an input digest `decide` already computed once,
to build the request. That costs one redundant `C::frame_check` call per dispatch, on the
shipped path. In exchange, the same check applies unconditionally to every other caller.
Measured, not assumed: `cargo xtask size`'s gated `layers` figure moves from ADR 0036's
12820 B to **12732 B** of the 13312 B gate — down, not up, and unchanged again by
`CheckedInput`. The wider `DurableIntent` and the new checked call cost this optimiser less
than the free `kind` argument they replace. No raise is asked for.

`DriveError` gains `EffectInputMismatch`. `waymaker-drive`'s own `Context::dispatch` cannot
reach it, for the reason above. It is named anyway: a `Result` this driver cannot construct
today is still one a future caller of `Dispatchable::perform` can construct.

If this gap were *not* fixed, issue #92 asked for a new entry in CLAUDE.md's
[What is not checked](../../CLAUDE.md#what-is-not-checked). That entry is not needed: the
gap it would have named no longer exists.

## Alternatives considered

**Threading the replayed record through `Resolve::Redeliver`.** Issue #92's own "stronger"
reading of the redelivery half. Rejected. The driver already holds a request the kernel has
verified, at the one call site that matters. Adding a second copy of the same three fields to
a kernel-boundary type — one `kernel-boundary` pins — would duplicate data already in scope,
for no added check.

**`Dispatchable` exposing the `EffectRequest` and leaving verification to the caller.**
Issue #92's "first, weaker" option. Rejected. It closes the kind half: there is no second
argument left to disagree. But it leaves the input half exactly where it was. A caller could
still read `dispatchable.intent()` and call `activities.perform(intent, wrong_bytes, out)`
directly. Nothing forces the caller through a checked path.

**A CRC comparison inside `Activities::perform` itself, left to each implementor.** Rejected.
It would ask every firmware author to reimplement the same check, and get the same answer.
Each author would also carry the cost of a mistake, in a trait method this crate cannot
review. One checked call site in `waymaker-drive` is the whole guarantee. An obligation on
the implementor is not a guarantee at all.
