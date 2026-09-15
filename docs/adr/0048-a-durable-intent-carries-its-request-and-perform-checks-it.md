# ADR 0048: A durable intent carries its request, and `perform` checks it

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

**An eighth round found the third route the first two left open: a method call.**
`dispatch.bytes.clone_from(&other)` reassigns `bytes` through an *implicit* `&mut self`
autoref — nothing in the source spells `=` or `&mut`, so neither the assignment check nor
the reference check sees it. Which method is called, and whether it really takes `&mut
self`, is a question `syn` cannot answer without type inference. So `mutated_field_names`
refuses every method call whose receiver is a guarded field, not only the ones a reviewer
could confirm mutate — over-broad by the same standing every other scanner in this
workspace accepts, and the one that costs nothing here: no method is ever legitimately
called directly on one of these fields anywhere in `effect.rs` today, only on the whole
value through its own accessor (`dispatch.bytes()`, whose receiver is a plain path, not a
field access, and stays unaffected).

**A ninth round found two more gaps, in two different mechanisms.** The first is a fourth
route to the field-rebinding problem the seventh and eighth rounds closed:
`let CheckedDispatch { bytes: ref mut slot, .. } = dispatch;` borrows `bytes` mutably through
the pattern itself, with no assignment, no `&mut` expression and no method call anywhere for
the first three routes to see. `mutated_field_names` now also refuses a `ref mut` binding on
a guarded field in any struct pattern, walked with a nested visitor so a binding nested
arbitrarily deep (behind a second guarded field, say) is found the same way regardless of
depth. A field bound `mut slot` with no `ref` is deliberately left alone: it moves or copies
the value into a fresh local, which is a read, and reconstructing `CheckedDispatch` from that
local afterward is a struct literal the construction pins already cover.

The second is in the type-alias resolution itself: `type Unchecked = <Via as Alias>::Dispatch;`
is a qualified associated-type projection, and `type_alias_target` explicitly skips every
`Type::Path` with a `qself` — deliberately, since resolving what a trait's `impl` names as its
associated type needs type inference this scanner does not have. Skipping was silently
permissive: the alias built nothing, so it counted as nothing, while the projection itself
could name `CheckedDispatch`. Unlike a tuple, a reference or a trait object — none of which
can ever appear where `Name { .. }` construction syntax is legal — a projection genuinely can
resolve to a struct usable that way, so "cannot resolve" cannot mean "therefore safe" here the
way it does for those other shapes. `qself_type_alias_names` reports every such alias instead,
and a new check refuses the file outright over it — a hard refusal of the construct, in the
same spirit as `effect-protocol`'s ban on a module declared anywhere in this file, rather than
an attempt at the type resolution neither `syn` nor this scanner can safely do.

**A tenth round found a gap in `mutated_field_names` itself: it checked only the outermost
field of a chain.** `dispatch.intent.request.kind = x;` assigns to `kind`, which is not a
guarded name — but `intent` and `request` are both guarded *ancestors* in the same chain,
and rewriting through either reaches the identity or the kind the whole family of checks
exists to protect, whether or not the leaf field itself is named. `note` now walks the full
chain of field accesses back to its root, checking every segment rather than only the last
one, for all three routes (assignment, `&mut` reference, method call) at once, since all
three share the one helper.

Two further findings from this round — that Rust's match ergonomics can bind a struct
pattern's field to a mutable alias with *no* `ref`, `mut` or `&mut` written anywhere, purely
from the scrutinee's own reference-ness, which `syn` cannot see; and that a generic type
alias with a trait bound (`type Unchecked<T: Alias> = T::Dispatch;`) is an associated-type
projection with no `qself` for the existing check to key on — are real and are not fixed
here. Ten review rounds deep, both would need genuinely new detection machinery rather than
a completion of what already exists, and this project's own review-depth guidance is to stop
iterating past two or three rounds and open an issue once a fourth still finds real bugs
rather than continue an unbounded loop. They are tracked in issue
[#171](https://github.com/madmax983/waymaker/issues/171) instead.

**An eleventh round, on the merge of this branch with a concurrent one, found a gap in the
tenth's own fix: a compound assignment.** `dispatch.intent.request.kind ^= 1;` rewrites
`kind` in place, and `note`'s chain walk covers it once reached — but `visit_expr_assign`
was the only route that called `note` at all, and `syn` does not parse `+=`, `^=` or the
other eight compound-assignment operators as an `Expr::Assign`. Each is a `BinOp` on an
`Expr::Binary`, a different node the visitor never visited. `mutated_field_names` now also
visits `Expr::Binary` and calls `note` on the left operand for any of the ten assignment
operators, leaving an ordinary binary expression (`x.field + 1`, which reads and rewrites
nothing) untouched.

**A twelfth round found a gap in the eleventh's own review, not its fix: destructuring
assignment.** `(dispatch.bytes,) = (replacement,);` is still an `Expr::Assign` — `note` is
still called on its left side — but that left side is `Expr::Tuple`, not `Expr::Field`, and
`note`'s chain walk starts by checking for `Expr::Field` and does nothing at all otherwise.
A field buried inside a tuple, array or struct-literal destructuring target was invisible
the same way a compound-assignment target had been. `note` now recurses into each element
of a tuple or array and each field's value in a struct literal — arbitrarily nested, since a
tuple can hold another tuple — before falling back to the field-chain walk, so
`(x, (dispatch.bytes,)) = (x, (replacement,));` is still caught two levels down.

**A thirteenth round found a gap in the tenth round's own chain walk, not in the eleventh's
or twelfth's fixes: a parenthesized ancestor.** `(dispatch.intent.request).kind = x;` is a
plain field assignment, and its outermost field is `kind` — not a guarded name — but
`intent` and `request` are guarded ancestors in the same chain, which is exactly what the
tenth round's walk exists to catch. The walk is `while let Expr::Field(field) = current {
.. current = &field.base; }`, and here `field.base` is an `Expr::Paren` rather than another
`Expr::Field`, so the loop stopped there and never saw either ancestor. The walk now
unwraps `Expr::Paren` and `Expr::Group` as it descends, the same two wrappers
`type_alias_target` already unwraps for the sixth round's reason, so a doubly parenthesized
ancestor (`((dispatch.intent).request).kind = x;`) still resolves in two hops. This is the
third round of Codex findings since this branch's merge with a concurrent one, and this
project's own review-depth guidance is to stop past two or three rounds and open an issue
once a fourth still finds real bugs — so a fourteenth finding of this shape goes to issue
[#171](https://github.com/madmax983/waymaker/issues/171) rather than a fourteenth round here.

**Review of the merge itself found a separate bug in code the merge introduced, not in
`mutated_field_names`'s own chain above: a nested module's alias leaking into a block's
lookup.** `resolve_local_alias_chain` was written to keep issue #92's function-local
type-alias resolution working after issue #169's rewrite of `struct_literal_counts` onto a
stack of raw `&[syn::Item]` slices rather than precomputed alias lists. It resolved a
block's own aliases by calling `collect_item_aliases`, which recurses into any `mod` the
block declares — the right behaviour for a whole-file alias index, wrong for a block-local
one. A block declaring both `type S = Foo;` directly and `mod hidden { type S = Bar; }`
alongside it had `hidden`'s own `S` collected into the same flat list as the block's own, so
a bare `S {}` outside `hidden` could resolve through the nested module's private alias
rather than the block's real one — exactly the leak `own_aliases`'s own doc comment already
states a module-level lookup must not have. `own_aliases` is generalized to take any
`&syn::Item` iterator instead of only a `&[syn::Item]` slice, and `resolve_local_alias_chain`
now calls it in place of `collect_item_aliases` — the same non-recursive, own-level-only
collection a module lookup already gets, reused rather than reimplemented.
`a_nested_modules_alias_does_not_leak_into_the_enclosing_blocks_lookup` is the regression,
confirmed RED against the unpatched lookup.

**Review of that fix found the inverse leak in the same round: a block's own alias leaking
into a nested module's lookup.** `visit_item_mod`, the `Literals` visitor's own traversal of
a nested `mod`, pushed the module's items onto `self.stack` and popped them on the way out
for module-level scoping, but never touched `self.block_items` — so a block's own local
`type`/`use` aliases stayed visible while the visitor descended into a `mod` declared
directly inside that block, even though real Rust never lets a nested module inherit an
enclosing function body's local items, the mirror image of the leak above. A block declaring
`type S = Foo;` directly and, alongside it, `mod hidden { pub struct S; fn make() -> S { S
{} } }` had `hidden::make`'s own `S {}` resolve through the outer block's alias to `Foo`,
when `hidden` should never see that alias at all. `visit_item_mod` now sets `block_items`
aside with `core::mem::take` before descending into the module and restores it once the
descent returns, the same discipline `self.stack`'s own push/pop already has.
`a_blocks_local_alias_does_not_leak_into_a_nested_module` is the regression, confirmed RED
against the unpatched visitor.

**Codex then found a gap in a different mechanism, not in the alias scanner's scope
discipline at all.** `syn::Visit` never descends into a `macro_rules!` body — to a
syntax-only scan it is an opaque token stream — so a local macro defined and invoked inside
`effect.rs` and expanding to `CheckedDispatch { intent, bytes }` builds the pinned type at a
construction site none of `struct_literal_counts`'s callers, nor any check built on it, can
see. Expanding or inspecting a macro body was rejected for the reason resolving a qualified
associated-type projection already was earlier in this same file's history: it needs
machinery — real macro expansion — this scanner does not have. `check_effect_types` now
refuses `effect.rs` outright over a bare `macro_rules` identifier instead, the same
construct `ctx-facade` already refuses in its own two pinned files for the identical reason:
a scanner cannot expand a macro, so it refuses the construct rather than trying to see
through it. `a_macro_rules_in_the_effect_protocol_file_is_reported` is the regression,
confirmed RED against the unpatched rule.

**Codex found the gap that ban left open in the same round.** Reading the `macro_rules`
identifier catches a *definition*, not an *invocation* of a macro defined anywhere else in
the crate — `emit!(CheckedDispatch { intent, bytes })` spells no such identifier at all, and
its token body is exactly as opaque to `syn::Visit` as a local definition's. `syn::Macro` is
the one type every invocation site shares — `ItemMacro`, `StmtMacro`, `ExprMacro`,
`TypeMacro` and `PatMacro` each carry one — so `crate::parse::invokes_any_macro` overrides
`visit_macro` once instead, which catches all five invocation shapes, `macro_rules!`
included, without naming any of them individually; the identifier-only check in
`check_effect_types` is retired in its favour. `effect.rs` now refuses the file outright over
any macro use at all, outside `#[cfg(test)]`. `a_macro_invocation_in_the_effect_protocol_file_is_reported`
is the regression, confirmed RED against the identifier-only check.

**A third round in the same family found the shape neither of the first two catches.** An
attribute macro or a custom derive is a `syn::Attribute`, not a `syn::Macro` invocation, so
`invokes_any_macro`'s `visit_macro` override — however exhaustive over every invocation shape
— never sees `#[forge]` on a method or `#[derive(Forge)]` on a struct: each expands in its
own defining crate with nothing here able to read what comes out. `crate::parse::unaudited_attributes`
closes it the same way as the two before it — a hard refusal rather than an attempt to
resolve what an unfamiliar name expands to — requiring every attribute in `effect.rs` to be
one of a fixed set the compiler itself interprets with no macro behind it
(`source::EFFECT_ALLOWED_ATTRIBUTES`), and a `#[derive(..)]` to name only the compiler's own
derives (`source::EFFECT_ALLOWED_DERIVES`), each name in the list checked on its own since
one attribute can mix an inert compiler derive with a custom one. `cfg_attr` is refused
outright rather than classified recursively, since it can emit an arbitrary attribute and
`effect.rs` has no legitimate use for one today. Three rounds deep in this macro-opacity
family — this project's own review-depth guidance is to stop past two or three rounds and
open an issue once a fourth still finds real bugs — so a fourth finding of this shape goes to
a new issue rather than a fourth round here.
`a_procedural_attribute_in_the_effect_protocol_file_is_reported` and
`a_custom_derive_in_the_effect_protocol_file_is_reported` are the regressions, confirmed RED
against the unpatched rule.

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
