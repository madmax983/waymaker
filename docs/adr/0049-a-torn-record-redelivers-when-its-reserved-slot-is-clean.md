# ADR 0049: a torn record redelivers when its reserved slot is clean

- Status: accepted
- Date: 2026-09-14
- Issue: [#95](https://github.com/madmax983/waymaker/issues/95)
- Supersedes: nothing
- Related: [0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md),
  [0019](0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md),
  [0026](0026-redelivery-is-the-kernels-answer-and-at-least-once-is-the-contract.md),
  [0027](0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md)

## Context

Design document §14, row 5: a torn completion write is ignored and the run redelivers the
effect. Recovery could do the first half — ADR 0019's commit seal already tells "the power
went during an append" from "this bank is damaged" — but not the second: `Ending::Unsealed`
carried no append point, so `waymaker-drive` and `waymaker-rig` both refused the bank rather
than write into it. The run's only way on was §10's `continue_as_new`, a new run under a new
`RunId`. Issue #95's own words: "an effect that was performed before the crash would be
performed again under a different `(RunId, EffectSeq)`, which is the duplicate
`stable-redelivery` exists to forbid." ADR 0027 recorded the gap rather than closing it,
because closing it needed a decision about what recovery may trust, not a test.

The effect a torn completion belongs to has already run: §07 dispatches at step 4, before the
outcome frame is even staged at step 5. So the record's own loss costs nothing on its own —
what costs something is losing the *bank*, which forces `continue_as_new` for a reason that
has nothing to do with the record itself being unrecoverable.

## Decision

**An unsealed frame is ignored, not merely reported.** No writer starts a record before the
one ahead of it has sealed — `waymaker-flash`'s append discipline, `commit-discipline` — so an
interrupted attempt never touches anything past its own reserved slot: the frame padded to the
journal's granularity, plus one program unit of commit seal. If every byte from the frame's
own unpadded length to the end of that slot is erased, nothing else was ever written there.
The record is dropped — never yielded — and the slot's own end becomes the append point, so
the *same* run keeps going and redelivers the effect under the identity its schedule record
already committed.

**The check is bounded to the slot, not run to the end of the region.** The first version of
this fix checked "is everything from here to the end of the journal erased", modelled on the
existing erased-tail walk. It was wrong twice over, and both failures are why the bound moved
to the slot alone.

First, it reopened exactly the hazard [ADR 0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)
closed: a reader walked at a granularity *wider* than the one a journal was written at
computes a padded body that runs past several real, committed records without ever decoding
them, looks for a seal in erased padding that belongs to none of them, and — under the
"walk to the end" version — found the *real* records further out and, since they are not
erased, correctly refused; but the moment the miscomputed slot happened to land in genuinely
erased media beyond everything, it silently accepted a slot that was never real. The fix
checks only `[frame_len, stride)`, which are exactly the bytes a real writer would have left
behind for *this* record: a wrong granularity almost always makes that span disagree with
"erased", and where it does not, the record is genuinely alone in the journal and skipping it
is correct regardless. `a_scan_at_a_larger_alignment_than_the_writer_used_is_caught_by_the_seal`
is the regression that found the wide check wrong.

Second, the "walk to the end" version answered `Ending::Clean` immediately, the same way an
erased header does. That is right for the *scan that found it*, and wrong for the *next* one:
if this run's own later boot redelivers successfully and commits real history starting right
after the ignored slot, a fresh scan from the top of the journal meets the same unsealed frame
at the same offset — nothing on NOR ever un-programs it — and this time the bytes beyond are
not erased, because the later boot's own history is sitting there. A scan that trusted its
first verdict would now refuse a bank holding a run that completed. The fix does not stop at
the slot: it advances past it and *keeps scanning*, the same way it would past a yielded
record, so the later boot's history is found the ordinary way rather than hidden behind a slot
no scan ever gets past twice.

**Both readers, in lockstep.** `waymaker-flash` holds two readers of one format —
`Recovery`, over real storage, and `Scan`, over a slice already in RAM — to each other by a
property test that drives the same bytes through both and requires the same records, the same
stopping offset and the same verdict. Fixing one without the other is exactly the drift that
test exists to catch, and it did: the first attempt at this change fixed only `Recovery`, and
`a_recovery_reads_what_a_scan_reads` failed within the hour.

**Only an outcome may be ignored this way.** `frame::redeliverable_kind` reads the record the
same, already-verified decode produced and answers `true` for exactly `EffectCompleted`,
`EffectFailed` and `TimerFired` — the three kinds whose retry §10's capacity reserve prices,
below. Every other kind — `RunStarted`, a schedule, a version marker, a terminal record —
still ends the scan as `Ending::Unsealed` when its seal does not hold, exactly as it always
did before this issue. See [Consequences](#consequences) for why the scope stops there.

**`Recovery::next`'s one call to the codec stays one call.** `RECOVERY_ROUTING_STEPS` pins
`next` to exactly one call to `frame::decode_with::<C>`, so that a recovery always verifies
with the check its caller chose. Answering "is this frame sealed" needs a decode, and
`next` already needs a *second*, `'page`-lifetime-tied decode to hand a caller a borrowed
record — the two cannot share one borrow, because the short-lived check has to be able to
hand the page back for another read when the frame turns out to be unsealed, and a borrow
tied to the page's own lifetime parameter cannot be given back once taken. The second decode
is real work, not a workaround: `sealed`, a private method with its own single call to
`frame::decode_with::<C>`, carries it, so `next`'s own body keeps the one call the pin reads
and the routing guarantee holds exactly as before.

## Consequences

**§14 row 5 holds, except where recovery cannot tell an interrupted append from damage.** A
tear inside the commit seal itself — some but not all of the seal's bytes landed — leaves
bytes that are neither erased nor a valid seal, and no writer ever leaves that shape on
purpose. Recovery still cannot tell that from corruption, and still refuses: `Ending::Unsealed`
stands, unchanged, for that case. `waymaker-drive`'s and `waymaker-rig`'s row-5 tests now sweep
both outcomes and require each to occur, rather than asserting only the refusal ADR 0027
recorded.

**The fix is scoped to outcomes, not generic over the record kind.** The first version of
this ADR let `Recovery` and `Scan` ignore an unsealed frame of *any* kind, on the reasoning
that they decode bytes, not kinds, and that losing an unsealed schedule, marker or terminal
record costs nothing because nothing downstream depended on it yet. That reasoning is right
for a schedule, a marker and a terminal record's own *replay effect* — but it is silent about
*capacity*, and capacity is exactly where it broke, twice, on this same pull request's own
review. `frame::redeliverable_kind` now answers `true` for exactly three kinds —
`EffectCompleted`, `EffectFailed`, `TimerFired` — and `false` for everything else
(`RunStarted`, a schedule, a version marker, a terminal record), which reverts every other
kind to the pre-issue-#95 refusal: an unsealed one of those still ends the scan as
`Ending::Unsealed`, no append point, exactly as it always did. `Recovery::sealed` decodes the
frame once (the same decode the seal check already needed) and hands `past_the_seal_slot` a
`Seal::Torn { redeliverable }` rather than a bare `bool`; `Scan::next` gates its own `clean`
check on the same function, since it already holds the decoded frame in scope. Neither reader
gained a second call to `frame::decode_with` — the kind was already being computed and
discarded.

**Why capacity is the line, and why it falls in different places for different kinds.** An
outcome is the one case where the *effect already ran* — §07 dispatches at step 4, before the
outcome frame is even staged at step 5 — so losing the record and keeping the bank is a pure
win: the alternative was `continue_as_new`, and this ADR's whole point is that redelivering in
place is strictly better than that. Losing a *schedule* is free by the same argument row 5
already makes: an unsealed schedule's effect was never dispatched, so there is nothing to
protect. But the outcome's redelivery is not free to the *bank*: recovery ignoring a torn
outcome attempt still costs `outcome_bytes` of media that cannot be reclaimed, and — this is
where the first version of this ADR stopped looking — dispatch happens on `Ending::Clean`,
*before* capacity is ever checked, so the activity gets redelivered whether or not there is
still room to record its result. §10's reserve prices exactly this: `redelivery_slack`, below.
A torn *terminal* record redelivering in place is a *second*, independent instance of the
same shape, and it is worse in one respect — the terminal is the run's only exit, so failing
to ever record it strands the run for good, not merely refuses one more effect. Codex found
both, in two separate review rounds of the same mechanism, and the second finding is what
settled the scope: rather than widen the reserve a second time for a kind (and prove it sound
for every kind after that — `RunStarted`'s own retry safety turns out to depend on the
*relative* sizes of `run_input_bytes` against everything else in `Bounds`, which no single
constant term fixes in general), the fix is scoped to the one kind whose retry-safety has an
actual, checked, static reservation behind it.

**The capacity reserve widens for the outcome, and only the outcome.** A schedule admitted at
§10's reserve boundary used to leave room for exactly one outcome and the terminal record;
after one torn-and-ignored attempt that is down to the terminal record alone, and the retry
that has to record the *real* outcome would refuse with `Refusal::NearCapacity` — on every
later boot, forever, after the activity has already run again. `Reserve::exit_bytes_after`'s
`EffectScheduled`/`TimerScheduled` arm and `Reserve::for_layout`'s floor both now add one more
`outcome_bytes` — `redelivery_slack` — so one wasted attempt is always affordable. A second
tear on the retry itself is outside what this covers, the same standing this codebase gives
its other single-crash guarantees rather than an unbounded one.
`a_torn_outcome_at_the_reserve_boundary_still_leaves_room_for_the_retry` drives the exact
shape: a schedule at the boundary, a torn outcome attempt, a fresh recovery, and a retry that
now fits — verified to fail without `redelivery_slack` before it existed. No such term exists
for any other kind, which is exactly why no other kind is in `redeliverable_kind`'s set.

**Numbers.** `cargo xtask size`: the `default` row moves from 12820 B to 12964 B of the 13312 B
gate ADR 0036 set — 92 B for the recovery fix's two new methods and the loop, 8 B for
`redelivery_slack`, and 44 B for `redeliverable_kind` and the two call sites that read it — no
raise asked for and 348 B left. `cargo xtask profile`: zero heap blocks on all four workloads,
unchanged. Runtime RAM and kernel state are unmoved — `Recovery`'s own fields did not change,
only a local, frame-only struct (`Staged`) gained one field, two private methods joined it,
`sealed`'s own return type widened from a `bool` to a two-variant `Seal`, and `Reserve`
gained no field at all.

**`waymaker-rig`'s own row classification needed a signal of its own.** The rig judges only
what a board could: it has no access to which byte of a program call a crash landed on, so it
had inferred "nothing of the completion frame landed" from `Ending::Clean` — which this issue
makes ambiguous, since a clean ending now also means "something landed and was safely
ignored". The rig's test-only `Evidence` gained a `landed` field, read by comparing where a
scan stopped against where the attempted record's own slot starts, both readable off the same
media a board has. `waymaker_rig::matrix::Row`'s own vocabulary, and the model's per-row
counts, did not move: `waymaker-drive`'s classification already used the injected operation
and its progress, not `Ending`, so it was never resting on the ambiguity this issue removes.
The rig's own resume-sweep census (`a_reset_at_any_point_of_a_resume_leaves_a_part_the_rig_judges_healthy`)
moved twice: first from 46 797 to 48 534 cuts and 15 990 to 16 536 inside a mark, when every
kind was redeliverable and more crash points reached a healthy, resumable part than
`rig.verify` used to filter out before a resume was ever attempted; then back down to
47 276 cuts and 16 133 inside a mark once the scope narrowed to outcomes, because a torn
`RunStarted`, schedule, marker or terminal record is once again a part `rig.verify` filters
out before resuming, exactly as it always was outside the three redeliverable kinds.

**What is still owed.** `waymaker-spec`'s ghost model is untouched, on purpose: it already
models the two-barrier write more coarsely than this, with no transition for the state a
payload barrier creates on its own — see [what is not checked](../../CLAUDE.md#what-is-not-checked) —
so this issue is a fact about bytes the model was never built to see change, and
`stable-redelivery`'s own proof, over the identity allocator, is unaffected either way. Two
things this ADR does not attempt: it does not widen `EffectScheduled`'s wire-format fields, and
it settles no part of issue [#16](https://github.com/madmax983/waymaker/issues/16)'s
`retry-policy-placement`, which stays open at rung 0.4.

`redelivery_slack` tolerates exactly one wasted outcome attempt per scheduled effect, not an
unbounded number. Two independent tears — the first consuming the slack, a second landing on
the retry itself — are outside what this reserve prices, and would strand the run exactly as
described above: the activity redelivered on every later boot with no way to ever record its
outcome. Closing that fully needs either an unbounded reserve, which no finite bank can pay
for, or `waymaker-drive` falling back to `continue_as_new` on a `NearCapacity` refusal it
meets after redelivery — which is rung 0.4's dispatcher, the same standing as every other
"nothing obliges a future dispatcher to..." limitation this codebase already records.

## Alternatives considered

**Carry the outstanding `(RunId, EffectSeq)` into the next run's header, and let
`continue_as_new` redeliver under the old identity.** Issue #95's other proposed repair. Keeps
the identity across a swap rather than never losing the bank, which is a wire-format or
bank-header change to the run a swap installs, and belongs with rung 0.4's dispatcher — the
thing that would actually perform the swap on a torn tail. Rejected here because it is strictly
more machinery for the same guarantee this ADR gets by construction, with no swap and no new
field.

**Trust `Ending::Clean` unchanged, and have the driver retry the whole boot once.** Would have
worked by accident on the very first re-scan and failed the moment a later boot's own history
sat past the ignored slot, which is exactly the "walk to the end" defect above. Rejected: a
fix whose correctness depends on how many times a caller happens to retry is not a fix.

**Widen `Ending::Unsealed` to carry an optional append point, rather than reusing `Clean`.**
Considered so a caller could still tell "an interrupted append that turned out to be safe"
from an ordinary clean end. Rejected: `Ending::Clean`'s own postcondition — an offset from
which every byte to the end of the region is erased at the moment the scan concludes — is
exactly what this case satisfies, and every caller's next action is identical to the ordinary
clean case. A distinct shape whose only difference is a fact no caller needs would be state
carried for its own sake, which is the thing this module's own four-shapes-and-no-more
documentation argues against.
