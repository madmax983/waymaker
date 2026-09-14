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
against what step 3 committed. Only then does it call `Activities::perform`. A mismatch is
`InputMismatch`, refused before the world is asked anything.

**`Activities::perform` takes one `CheckedDispatch`, not an identity and bytes as two
separate arguments.** Both of `CheckedDispatch`'s fields — the `DurableIntent` and the
bytes — are private, and `Dispatchable::perform` is the only place that builds one; this
crate's own `compile_fail` doctest shows a caller cannot construct one elsewhere. So a
caller cannot reach `Activities::perform` with an identity and bytes that were never checked
against each other: there is no route to that pair except the one call that checks them
together, and no way for an adapter to recombine a stale identity from an earlier call with
the current call's bytes, because it would need two separate values to swap and there is
only the one. Codex found this in two rounds of review of this change: the first version
left `Activities::perform` taking `intent: DurableIntent` and `input: &[u8]` as two
arguments, so a caller with a `DurableIntent` from `Dispatchable::intent()` could call
`activities.perform(intent, wrong_bytes, out)` directly; wrapping the bytes alone in a
`CheckedInput` closed that route but left the identity a separate argument still, so an
adapter forwarding to another `Activities` implementor could still pair one effect's
`DurableIntent` with another effect's `CheckedInput` and produce a value that was, field by
field, individually valid. `CheckedDispatch` merges both into the one value
`Dispatchable::perform` builds atomically, closing that too.

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

**`effect-protocol` pins all four new methods.** `DurableIntent::kind`,
`Dispatchable::perform`, `CheckedDispatch::bytes` and `CheckedDispatch::durable_intent` join
`EFFECT_PROTOCOL_SURFACE` and `EFFECT_TYPE_METHODS`. None calls `.stage(`, `.payload_barrier(`
or `.commit(`. So none is one of §07's storage steps, and none needs a place in
`EFFECT_STEP_BODIES`.

**`effect-protocol` also pins `CheckedDispatch`'s one construction site.**
`CHECKED_DISPATCH_CONSTRUCTION` names `Dispatchable::perform` as the only body that may build
one, the way `EFFECT_CONSTRUCTIONS` already does for `DurableIntent` and `Dispatchable` — one
body rather than two, because `CheckedDispatch` has one legitimate origin rather than a
schedule and a redelivery. `EFFECT_NO_SELF_LITERAL` alone refuses a `Self` literal inside
`CheckedDispatch`'s own `impl` and a trait built for it; it says nothing about a sibling
`pub(crate)` function elsewhere in the file naming the type directly. Codex found that hole
on a third round of review of this change, and a fourth on the round after: the scan
resolved only `use` aliases, so `type Unchecked<'a> = CheckedDispatch<'a>;` followed by a
literal spelled `Unchecked { .. }` built the type under a name the pin never compared
against. `struct_literal_counts` now resolves `type` aliases the same way it already
resolved `use` aliases, chased through a chain of either kind — `type A = B; type B =
CheckedDispatch;` is two aliases, and a literal spelled `A { .. }` has to reach
`CheckedDispatch` through both. The fix lives in the shared scanner rather than in this rule
alone, so `EFFECT_CONSTRUCTIONS` and every other construction pin built on
`struct_literal_counts` closed the same gap at once.

A fifth round found the alias collector's own blind spot: it walked file items and inline
modules only, so a `type` alias declared *inside* a function body — legal Rust, and
invisible to a scan built for module-level declarations — evaded it just as the file-scoped
one had. `struct_literal_counts` now gives every block its own alias scope: entering a block
collects the `use` and `type` aliases declared directly in its own statements, resolves a
name against the innermost scope that declares it — the same rule a real compiler resolves
under, so a local alias correctly shadows a same-named one declared elsewhere in the file
rather than the scan picking whichever declaration happens to sort first — and pops that
scope on the way back out. The fix is still in the shared mechanism rather than bolted onto
this one pin: the same block-scoped resolution closes the gap for every construction pin
built on `struct_literal_counts`.

A sixth round found a hole in what counts as a `type` alias's right-hand side rather than in
where it is looked for: `type Unchecked<'a> = (CheckedDispatch<'a>);` is valid Rust —
`#[allow(unused_parens)]` lets it through `-D warnings` — and `syn` keeps the parens as their
own `Type::Paren` node rather than discarding them, so the `Type::Path` match that reads a
type alias's target saw nothing there and built no alias at all. `type_alias_target` now
unwraps `Type::Paren`, and `Type::Group` beside it for the same reason (a macro's own hygiene
grouping is the same shape), recursively — `((CheckedDispatch))` reaches the same target in
two hops.

**A seventh round found a gap in a different shape: rewriting a field in place, rather than
building a fresh value.** `CheckedDispatch`'s, `DurableIntent`'s and `Dispatchable`'s fields
are private to the *module* `effect.rs` declares, not to the type — Rust has no finer
grain — so any sibling function in that module can already write `dispatch.bytes = other;`
on an otherwise legitimate value, with no struct literal anywhere for a construction pin to
count. A `&mut` reference taken to the field is the same capability under a second spelling:
`core::mem::swap`, `core::mem::replace`, or passing the reference to an arbitrary
`&mut`-taking function all rewrite the field without an `=` in the source at all.
`source::EFFECT_PROOF_FIELDS` names the four fields this matters for — `id` and `request`
from `DurableIntent`, `intent` from `Dispatchable` and `CheckedDispatch` (spelled once, since
both name it the same way), and `bytes` from `CheckedDispatch` — and a new check,
`check_effect_proof_fields_are_not_rebound`, refuses both an assignment and a `&mut`
reference to any of them, anywhere in the file. `Effect`'s and `Dispatchable`'s `writer`
field is deliberately not on the list: it carries no identity, kind or byte binding, so
rewriting it is not the guarantee this list exists for. Nesting `CheckedDispatch` in a
private submodule of its own — the usual way Rust narrows field visibility below module
scope — was considered and rejected: `effect-protocol` already refuses a module declared
anywhere in this file, precisely so a construction site cannot hide from a scan that reads
`effect.rs` as one flat file, and a submodule added for this reason would open exactly the
hole that rule exists to close.

## Consequences

A caller cannot dispatch one effect's identity under another effect's kind, cannot dispatch
it under another effect's bytes, and cannot pair this identity with a different call's bytes
by holding one back and forwarding it later. All three are now facts the compiler checks, for
every caller there is or will be — including a caller who calls `Activities::perform`
directly, bypassing `Dispatchable::perform`, since there is still no value it could pass that
argument except one already checked. This is closer to absolute than most guarantees a trait
boundary between two crates can state. What is left is a caller inside `waymaker-drive`
itself reaching into `effect.rs`'s own module — a source change to this crate, not a misuse
of its public API. `CHECKED_DISPATCH_CONSTRUCTION` closes the first shape that takes:
building a `CheckedDispatch` by hand; `effect-protocol` fails a build in which any body but
`Dispatchable::perform` does it. `EFFECT_PROOF_FIELDS` closes the second shape, found on a
later round: rewriting a field of an already-legitimate value in place, by assignment or by a
`&mut` reference, rather than building a fresh one — a route no struct-literal count could
ever see, because it builds nothing.

`DurableIntent` doubles in size: an `EffectId` plus an `EffectRequest`. Both are `Copy` and
stack-passed. So nothing here touches a heap — this engine has none. `CheckedDispatch` costs
nothing beyond that: a two-field wrapper around a `DurableIntent` and a `&[u8]` has the same
layout as the pair passed separately, and the optimiser erases the wrapping.
`Dispatchable::perform` recomputes an input digest `decide` already computed once, to build
the request. That costs one redundant `C::frame_check` call per dispatch, on the shipped
path. In exchange, the same check applies unconditionally to every other caller. Measured,
not assumed: `cargo xtask size`'s gated `layers` figure moves from ADR 0036's 12820 B to
**12732 B** of the 13312 B gate — an 88 B drop, not a rise, and unmoved again by
`CheckedDispatch`. The wider `DurableIntent` and the new checked call cost this optimiser
less than the free `kind` argument they replace. No raise is asked for.

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

**A `CheckedInput` wrapping only the bytes, with the identity a separate argument.** This
change's own second version, until Codex's second round of review. Rejected: it closes the
route to unchecked bytes, but an adapter forwarding to another `Activities` implementor could
still hold a stale `DurableIntent` from an earlier call and pair it with the current call's
`CheckedInput`, producing a combination that is individually valid in each field and wrong as
a pair. Bundling both into one value removes the second value there would be to swap.

**A CRC comparison inside `Activities::perform` itself, left to each implementor.** Rejected.
It would ask every firmware author to reimplement the same check, and get the same answer.
Each author would also carry the cost of a mistake, in a trait method this crate cannot
review. One checked call site in `waymaker-drive` is the whole guarantee. An obligation on
the implementor is not a guarantee at all.
