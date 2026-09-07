# ADR 0025: The effect protocol is a typestate, and an exhausted answer is a record

- Status: accepted
- Date: 2026-09-06
- Issue: [#29](https://github.com/madmax983/waymaker/issues/29)
- Supersedes: nothing
- Related: [0024](0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md),
  [0020](0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md),
  [0019](0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md)

## Context

Design document §07 states the first execution of an activity as seven ordered steps. Steps 1
to 3 make the schedule record durable, step 4 dispatches the activity with a stable
`(RunId, EffectSeq)`, and steps 5 to 7 make the outcome replayable. §02 decision 3 is the
reason: a physical effect never precedes its committed intent.

Issue [#28](https://github.com/madmax983/waymaker/issues/28) put those seven steps on media in
the right order. It did not make the order the *only* order. `waymaker-drive`'s boundary
committed the schedule record and then dispatched, one statement after the other, in one
function. Nothing stopped a later change from swapping them. Issue #29 asks for the
difference: "make step 4 unreachable without step 3 having completed — structurally, not by
review".

Issue #29 also asks for "bounded result and error payloads with an explicit exhaustion
behaviour when an activity returns more than the bound". The behaviour at the time was a boot
error. That is worse than it looks. The schedule record is already committed, so the next boot
redelivers the effect, the activity answers with the same over-long result, and the boot fails
again. §08 has no edge from an unresolved effect to a terminal record, so the run can never
end. That is the failure mode issue #25's capacity reserve exists to prevent, in a second
place.

Two bounds also disagreed. The activity wrote into the caller's result buffer, and the record
was priced against `Bounds::effect_result_bytes`. An answer could fit one and not the other.

## Decision

**§07 is three types, in `waymaker-drive`.** `Effect` holds §10's gated writer between
effects. `Effect::schedule` takes steps 1, 2 and 3 and returns a `Dispatchable`.
`Dispatchable::intent` is the only source of a `DurableIntent`, and `Activities::perform` —
step 4 — accepts no other proof. `Dispatchable::resolve` takes steps 5, 6 and 7 and returns a
`Resolved`, which carries the only `Outcome` a caller can reach. So the workflow observes the
result after step 7's barrier and at no earlier point, because there is no earlier value to
observe.

`DurableIntent`'s field is private. This crate builds one in two bodies. `schedule` builds one
after step 3's commit barrier returned, which is a proof. `redelivering` builds one for a
schedule record an earlier boot committed, and that is *not* a proof: the evidence is the
kernel's `Resolve::Redeliver`, which the driver reads and this module cannot see. So
`redelivering` is `pub(crate)`, and the trust is confined to the one caller beside it. A
`compile_fail` doctest shows that a third route is not available to a caller.

**The protocol is above the layers, not in `waymaker-flash`.** Steps 1 to 3 and 5 to 7 are
`waymaker-flash`'s two-barrier writer, but step 4 is an activity, and `waymaker-flash`'s
must-not-own cell names activities. A protocol that contained step 4 would put activities in
the storage layer. `waymaker-drive` already owns the loop that joins the two, so §07 lives
beside it. ADR 0024's standing holds: nothing obliges a dispatcher to use it, and rung 0.4's
does.

**`effect-protocol` is the gate.** It pins the public surface in both directions, the two
methods the dispatch state may declare, the private fields of the three types that carry the
proof, the two bodies a `DurableIntent` may be built in, and that each of the two step bodies
names the frame, the payload barrier and the seal exactly once and in that order. It also
refuses a `redelivering` that writes anything, because the schedule record it names committed
in an earlier boot.

**An answer that overruns the bound is a record, not a refusal.** `Resolution::Exhausted` is
written as an `EffectFailed` with no payload. The run continues, the workflow takes its
failure branch, and no part of the answer reaches it. `Performed::Exhausted` is how an
activity says so, and an activity that instead *reports* a length wider than the buffer it
was handed is recorded the same way: on media the two are the same statement, and a refusal
there strands the run exactly as the old behaviour did.

**There is one bound.** The driver hands an activity a buffer of exactly
`Bounds::effect_result_bytes`, taken from the reserve that priced the bank —
`Reserve::bounds()` is new for this. A result buffer narrower than that bound is
`DriveError::ResultBufferTooSmall`, refused at the start of a boot before the run's own record
is written. A payload wider than the bound is refused by §10's reserve before any media call.

## Consequences

The order of §07's steps is now a property of which type a step hands back, and the gate holds
the construction sites. A driver cannot name the identity of an effect it has not committed.

An exhausted effect and an effect that failed with no detail are the same record. That is a
real loss of diagnosis, and it is deliberate: an empty payload is the only payload that fits
every bound, `effect_result_bytes == 0` included. A marker payload would fit some runs and not
others, which is worse than a stated ambiguity.

`Resolution::Exhausted` turns a success into a failure. A workflow that ignores
`Outcome::Failed` will treat an over-long answer as a normal failure. The alternative is a run
that can never end, and §08 leaves no third option.

The result-buffer rule is stricter than it was. A caller that brought a small buffer and a
large declared bound is now refused at boot rather than at the first effect. Two tests in
`crates/waymaker-drive/tests/boundary.rs` moved to reserves of their own to keep testing what
they were about.

The bound applies on the reading side too, which review found it did not: `store` copies a
replayed outcome under the narrower of the caller's buffer and the declared bound, so a
firmware whose bounds shrank refuses a journal written under the old ones rather than handing
back a payload it would decline to write.

`Decision` grew from an `EffectId` to a `Dispatchable`, and the writer is now moved through
`schedule` and `resolve` rather than staying inside `Source`. Code flash moved 4 B; the stack
did not move measurably, because nothing measures it — §04's runtime RAM figure is statics
only, and CLAUDE.md says so. It is recorded here because this change is what made the value
large enough to be worth recording.

`Reserve::bounds()` is a new public function in a layer, so the size probe reaches it and
§04's code-flash budget charges for it: 18098 B to 18102 B, against the same 18432 B gate. No
raise is asked for, which is what ADR 0020 said the next change had to manage. §07's own three
types cost nothing there, because they are in `waymaker-drive` and no budget covers it — which
is a real gap rather than a saving, and it is the same one ADR 0024 records for the driver.

What is still owed is ADR 0024's list, unchanged: this driver does not swap banks, nothing
obliges a dispatcher to use it, and bank selection stays `waymaker-flash`'s. One thing is
added to it. `Effect::redelivering` takes the kernel's word that committed history holds a
schedule record with no outcome — the driver reads `Resolve::Redeliver` and calls it — and
this module cannot check that. It is a precondition on the caller, of the same standing as
`Swap::beginning`'s, and [what is not
checked](../../CLAUDE.md#what-is-not-checked) says so.

## Alternatives considered

**The protocol in `waymaker-flash`.** It would be reachable by `waymaker-embassy` at rung 0.4
without going through a test-support crate. It was not taken because step 4 is an activity and
`waymaker-flash` must not own activities; a `DurableIntent` minted in the storage layer and
consumed above it would be a protocol split across a boundary it is meant to define. The
code-flash budget agrees — §04's incremental gate has 330 B of headroom after this change —
but that is corroboration rather than the reason.

**A closure: `schedule_then_dispatch(|intent| ...)`.** The order would then be a fact about
one function's body rather than about a type, which is the thing issue #29 asks to be
replaced. It also fixes the shape of the caller: a driver that wants to record something
between steps 4 and 5, or to suspend at `Performed::Pending` without resolving, has to
express it as a return value from the closure instead of as ordinary control flow.

**A truncated result.** Forbidden by issue #29's second "done when": no partial result bytes
are ever exposed to the workflow. A truncation also becomes history and replays for ever.

**A new record kind, `EffectExhausted`.** It would remove the ambiguity with an empty
`EffectFailed`. It is a wire-format change, §09's record vocabulary is a design-document
table, and the ambiguity costs a diagnosis rather than a guarantee.

**A marker payload, such as `b"exhausted"`.** It does not fit a run whose declared bound is
shorter than the marker, which is the one case the marker exists for.

**Taking the bound from the caller's buffer.** It is `Reserve::for_bytes(tail)` again: a bound
a caller supplied is a bound nothing vouched for, and it can be narrower than the bound §10
priced the bank against.
