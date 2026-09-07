# ADR 0026: Redelivery is the kernel's answer, and at-least-once is the contract

- Status: accepted
- Date: 2026-09-07
- Issue: [#30](https://github.com/madmax983/waymaker/issues/30)
- Supersedes: nothing
- Related: [0024](0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md),
  [0025](0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md),
  [0015](0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md)

## Context

Design document §14's fourth guarantee: "retries and reboot redelivery reuse the original
effect identity". That pair is the only value a downstream system can deduplicate on, so a
driver that hands a redelivered effect a fresh sequence turns one effect into two — with
every test green, because the run still completes and the journal still reads correctly.

After issues [#28](https://github.com/madmax983/waymaker/issues/28) and
[#29](https://github.com/madmax983/waymaker/issues/29) the parts existed. The kernel answers
`Resolve::Redeliver { id }` with the identity the schedule record committed, and
`EffectRequest::divergence_from` compares §09's recorded `input_len` and `input_crc` against
what the workflow asked for. `waymaker-spec/tests/redelivery.rs` proves the guarantee about
`EffectIdAllocator`.

Three things were missing, and issue #30 names each.

**The driver's half was unproven.** The spec proves the *allocator* is a function of the run
and the position. It says nothing about what a driver dispatches under. No test drove a
retry with no reset at all, which is the case where the workflow value, the world and the
RAM all survive — a reboot test cannot tell an identity read from history from one held in a
counter.

**The digest check was unproven at the driver.** `crates/waymaker-drive/tests/teeth.rs`
covered a changed activity *kind*. A changed *input* had no test, and the sharpest case had
none either: history holding a schedule and no outcome, where §08's engine action is a
dispatch and the wrong answer is not a wrong record but a physical effect performed against
an input nobody recorded.

**The contract was not written down where it is read.** `Activities::perform` said an
activity that deduplicates downstream "sees a repeat". It never said that Waymaker cannot
promise exactly-once, and issue #30 is explicit: "the documentation must not soften this".

## Decision

**Redelivery is the kernel's answer, and the driver has no identity of its own.** Every
`(RunId, EffectSeq)` the driver dispatches under comes from `Intent::Schedule` or
`Resolve::Redeliver`. `EffectIdAllocator` joins `RecordKind` and `Step` in
`source::DRIVER_FORBIDDEN_VOCABULARY`, so `kernel-boundary` fails a build in which
`waymaker-drive/src/drive.rs` reaches for the one thing permitted to mint a sequence.

This is a floor and not a proof. `EffectId`'s fields are public, so a hand-written literal
evades it, and so does a sibling module — the same limit `capacity-reserve` and
`recovery-surface` each record about the file they pin. It is in
[what is not checked](../../CLAUDE.md#what-is-not-checked) rather than implied.

**An in-boot retry is another boot with no reset.** No retry policy is introduced. Design
document §16 leaves `retry-policy-placement` open, owned by rung 0.4, and a driver that
decided when to try again would settle that question by accident. What the driver owes is
that the attempt carries the original identity, whatever decides to make it.

**The at-least-once contract is stated on `Activities`.** The trait an implementer writes
against says that one effect can be performed more than once, names the two causes — a retry,
and a reset between §07 step 4 and step 7 — states that every attempt carries one identity,
and states plainly that exactly-once physical side effects are not on offer and no setting
changes that. The two ways to get exactly-once are named, and both are outside this engine.

**Both halves are measured, and both have been watched failing.**
`crates/waymaker-drive/tests/redelivery.rs` drives a retry and five retries; every case uses
the run's *second* effect, because the allocator starts at `EffectSeq(0)` and a run whose
outstanding effect is its first cannot tell redelivery from a fresh mint —
`a_fresh_mint_would_not_answer_what_redelivery_answers` is the tooth that says so rather than
a comment claiming it. `crates/waymaker-drive/tests/crash.rs` adds the window issue #30's
first "done when" names: crash points at which the crashed boot had *already performed* the
effect and its outcome record did not commit. Those are counted, and a sweep in which none
occurred fails.

The digest cases are two: an input of the recorded length with a different byte, and a
shorter one. A driver cannot vary one half of §09's digest alone, because a shorter input has
a different checksum too — that isolation is the kernel's, in
`crates/waymaker-core/tests/transition.rs`. What these two hold is the driver's half: the
digest it computes is a digest of the bytes the workflow passed.

## Consequences

The guarantee is now falsifiable at the layer that can break it. A driver that re-mints on
redelivery fails four tests; a driver that digests the wrong bytes fails two. Both mutations
were run before the tests were kept.

`demo::World` grows an `offered` log beside its `dispatched` one, and a
`pending_once_at_seq` constructor. The two logs are the difference between what the world was
*asked* and what it *did*, and §14's contract is a statement about the first: a declined
attempt and the retry after it must carry one identity, and a log of what happened cannot see
that pair. The constructor is keyed on the sequence rather than on a dispatch count, for the
reason `exhausting_seq` is: a count means something different on the second boot.

What is still owed is unchanged and is written down. Exactly-once is not on offer, and no
future work here makes it so; §14 says the same. The gate rule is a floor, so a driver that
built an `EffectId` by hand would pass it and fail the tests instead. And nothing obliges a
future dispatcher to use this driver — rung 0.4's, the same standing as the capacity
reserve's.

## Alternatives considered

**A retry count or an attempt number in the dispatched identity.** It would let an activity
tell a first attempt from a second, which is a real thing an implementer wants. It is
refused because it is the guarantee inverted: the pair a downstream system deduplicates on
would then differ between the attempt that changed the world and the attempt that repeats
it, which is precisely the failure §14 exists to forbid. An activity that needs to know it
is a repeat learns it downstream, from the identity.

**A retry loop in the driver.** `Performed::Pending` could suspend the effect and try again
without returning. That is retry *policy*, which §16's `retry-policy-placement` leaves open
until the dispatcher exists and the cost of a recorded retry representation can be measured.
Answering it here would settle a deferred question with an implementation nobody weighed —
which [the record](README.md) says a decision must never be.

**Verifying the digest in the driver rather than in the kernel.** The driver already computes
it, so comparing it against the schedule record there is one line. It is refused because §08's
table is the kernel's, and a digest compared in two places is a digest that will one day be
compared two ways. The driver computes and passes; `EffectRequest::divergence_from` decides.
