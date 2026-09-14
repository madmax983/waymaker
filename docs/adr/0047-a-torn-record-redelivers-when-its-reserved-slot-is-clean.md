# ADR 0047: a torn record redelivers when its reserved slot is clean

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

**The fix is generic over the record kind.** `Recovery` and `Scan` decode bytes, not kinds, so
the same rule applies to a schedule record, a timer record or a version marker left unsealed
the same way. None of them costs anything by being ignored: an unsealed schedule record's
effect was never dispatched — §07 dispatches after the commit barrier, at step 4 — so losing
it is what §14 already asked for, now without losing the bank to get there.

**Numbers.** `cargo xtask size`: the `default` row moves from 12820 B to 12912 B of the 13312 B
gate ADR 0036 set, 92 B for the two new methods and the loop, no raise asked for and 400 B
left. `cargo xtask profile`: zero heap blocks on all four workloads, unchanged. Runtime RAM
and kernel state are unmoved — `Recovery`'s own fields did not change, only a local, frame-only
struct (`Staged`) gained one field and two private methods joined it.

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
grew, from 46 797 to 48 534 cuts and 15 990 to 16 536 inside a mark, because crash points that
used to fail `rig.verify` before a resume was ever attempted are now healthy, resumable parts.

**What is still owed.** `waymaker-spec`'s ghost model is untouched, on purpose: it already
models the two-barrier write more coarsely than this, with no transition for the state a
payload barrier creates on its own — see [what is not checked](../../CLAUDE.md#what-is-not-checked) —
so this issue is a fact about bytes the model was never built to see change, and
`stable-redelivery`'s own proof, over the identity allocator, is unaffected either way. Two
things this ADR does not attempt: it does not widen `EffectScheduled`'s wire-format fields, and
it settles no part of issue [#16](https://github.com/madmax983/waymaker/issues/16)'s
`retry-policy-placement`, which stays open at rung 0.4.

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
