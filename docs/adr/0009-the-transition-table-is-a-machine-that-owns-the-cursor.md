# ADR 0009: the transition table is a machine that owns the cursor, and divergence is terminal

- Status: accepted
- Date: 2026-09-02
- Issue: [#15](https://github.com/madmax983/waymaker/issues/15)
- Supersedes: nothing
- Related: [0004](0004-the-layering-contract-is-a-table-a-gate-reads.md),
  [0006](0006-effect-identity-is-newtypes-and-exhaustion-is-terminal.md),
  [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md),
  [0008](0008-the-replay-cursor-is-pumped-by-its-caller.md)

## Context

Design document §08 settles what happens at an effect boundary as a five-row table:

| Next history state | Workflow call | Engine action |
| --- | --- | --- |
| Matching schedule + completion | Same effect kind and input digest | Return the recorded result and advance the cursor |
| Matching schedule only | Same effect kind and input digest | Redeliver using the existing effect ID |
| End of history | New effect call | Append and commit a schedule record, then dispatch |
| Different kind, digest, or sequence | Replay divergence | Stop with `NondeterministicWorkflow`; never guess |
| Terminal workflow record | Further execution | Return stored completion/failure without polling further |

[ADR 0008](0008-the-replay-cursor-is-pumped-by-its-caller.md) built the cursor underneath
it, and drew the line deliberately: `ReplayCursor` validates history **against itself** —
an outcome with no schedule, a sequence that skips, anything after a terminal record — and
knows nothing about a workflow. Every row above except the third needs the other half of
the comparison: *what the workflow just asked for*. Nothing in the workspace held that
question, so `NondeterministicWorkflow` had no place it could come from.

Three constraints shape the answer.

**The kernel owns no CRC.** §05's must-not-own cell names it, and
[ADR 0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md)
put every checksum in `waymaker-flash`. §13 sketches the boundary as
`EffectRequest { kind, input: &'a [u8] }` — but comparing bytes against history means
digesting them, so a kernel that took the bytes would have to own the thing it must not.

**Replay is pumped by its caller.** ADR 0008's decision, and the reason one 512-byte page
is enough for a history of any length. Whatever implements §08 cannot read media either.

**Divergence is terminal and loud.** §08 says "never guess"; issue #15 adds "no
reinterpretation of history, no best-effort recovery". Both are statements about what must
*not* exist, which is the hardest kind of property to hold: a test cannot call a function
that is not there.

## Decision

**`waymaker_core::transition::ReplayMachine` owns a `ReplayCursor`, and one effect boundary
is two calls.**

1. **The machine owns the cursor rather than borrowing it.** The cursor is the kernel's only
   source of an `EffectId` (ADR 0006, ADR 0008), so putting it *inside* the machine puts the
   divergence check in front of the only door identity can leave by. "A diverging replay
   never dispatches an effect" then follows from the shape of the types rather than from a
   driver remembering to check a flag. The machine replaced the cursor's row in
   `kernel_state_types!` rather than joining it — the registry sums types that are
   independently live — and is pinned at `size_of::<ReplayCursor>() + align_of::<ReplayCursor>()`,
   which is the cursor plus one phase byte and its padding on both the host and
   `thumbv6m-none-eabi`.

2. **A boundary is `intent` then `outcome`, mirroring §07.** A durable intent is committed,
   and only then does an outcome exist to observe; replay reads them back in that order. So
   the machine consumes at most two records per boundary and asks for them one at a time,
   which is what keeps ADR 0008's single scratch page enough. Rows 1 and 2 differ only in
   whether the *second* record is there, which is exactly what the second call answers.
   `Next::EndOfHistory` is a named variant rather than an `Option::None`, because "history
   ended" is a row of the table rather than an absence.

3. **The request carries a digest, not the input.** `EffectRequest { kind, input_len,
   input_crc }`: the layer that owns the checksum computes it and passes the pair down. That
   is §13's sketch adapted to §05's layering rather than copied, and it also makes the
   request `Copy` and eight bytes with no lifetime — which is what lets a boundary be two
   calls without the machine holding a borrow between them.

4. **Sequence divergence is checked against the cursor, not against the request.** Nothing
   in a workflow call carries a sequence, so `EffectRequest::divergence_from` takes the
   identity the run would issue next and compares the recorded schedule's `EffectId` against
   it — whole, run id included. That catches a reordered call, a record pumped out of order,
   and a schedule from the previous generation, which two banks make a thing that can be
   read. The three flavours are reported in position order — sequence, then kind, then digest
   — because a digest compared against a record from another position is a comparison of two
   unrelated effects, and "different kind" for a reordered call sends an engineer to the
   wrong place.

5. **Divergence is refused before anything is consumed, and it is sticky by
   representation.** The private phase has a diverged state and no code path leaves it, the
   same discipline `EffectIdAllocator` applies to exhaustion (ADR 0006). Two properties fall
   out: no `EffectId` escapes, so nothing can be dispatched; and history stands where the
   divergence found it, so a diagnosis can name the record.

   **Every `intent` that answers `NondeterministicWorkflow` records one**, including the
   position gate's — the workflow reaching a boundary before its own `RunStarted`, while an
   effect is unresolved, or after a terminal record. Those get a fourth flavour,
   `Divergence::Boundary`, rather than being folded into `Sequence`: there is no recorded
   schedule at such a position to differ from, and reporting "the effect is not the one
   history recorded here" for a position where history recorded nothing would be the
   misleading log this record keeps arguing against. They are terminal for a stronger reason
   than driver error: a run that continues from one appends effects the journal cannot
   justify, so the *next* cold start could not replay it. Refusing is what keeps history
   replayable.

   The line is drawn at who made the claim. A refusal from `outcome` with no boundary open,
   or from `advance` with one still open, is the **driver** asking out of turn: the workflow
   claimed nothing, nothing is consumed, and the correct call still succeeds afterwards.
   Those are refusals, not divergence, and `diverged()` says so.

6. **A halted cursor's error outranks every refusal the machine has of its own.** Checked
   before the phase in all three entry points. Otherwise a record that halts the cursor
   mid-boundary leaves the phase set, and the next call answers `NondeterministicWorkflow` —
   reporting changed workflow code for a damaged journal, which is the one confusion
   `KernelError`'s own documentation is written to prevent. A halted cursor and a diverged
   phase cannot both be set, because `next_effect_id` refuses on a halted cursor before the
   divergence check is reached, so deferring costs no exactness.

7. **The absence is gated.** `transition-surface` pins the machine's public function
   surface against `xtask::source::TRANSITION_SURFACE`, in both directions. A `reset`, a
   `clear_divergence`, a `resume`, or a `force` argument on `intent` would break no layering
   rule, need no dependency and pass every other gate; the pin makes each of them a line a
   reviewer writes on purpose. The other direction matters more: a name the module no longer
   declares means the pin has stopped checking anything.

**Determinism itself stays a contract, and the documentation says so.** The module
documentation lists what workflow code must not read — hardware registers, ambient time,
randomness, mutable statics, network state, nondeterministic iteration order — and states
plainly what detection buys: a nondeterminism that never changes the kind, digest or order
of an effect is not detected and cannot be; every nondeterminism that would have made
replay return the wrong answer is. A lint for suspicious APIs is later tooling, as §08 says.

## Consequences

- `NondeterministicWorkflow` finally has a producer, and `MalformedHistory` keeps its own
  meaning. The two faults stay distinct: a damaged journal and changed code send an engineer
  to different places, and `diverged()` returns `None` for a machine halted by the former.
- A driver now talks to `ReplayMachine` rather than to `ReplayCursor`. The cursor stays
  public and stays pinned — it is still the thing that validates history against itself, and
  it is still what `replay-cursor-surface` guards.
- Two calls per boundary is more protocol than one. The machine refuses the misuses that
  makes possible — `outcome` with no boundary open, `intent` with one already open,
  `advance` between the two — so a driver bug is a refusal rather than a mislabelled result.
  It is still surface a façade has to get right, and rung 0.4's `Ctx` is where that stops
  being the firmware author's problem.
- The incremental code-flash delta rose from 5140 B to 7204 B of the 8192 B budget, and
  kernel state from 56 B to 64 B of 128 B. Both are measured by `cargo xtask size` against
  a probe that reaches every new public function and every row of the table, refusal
  included. **988 B of the code-flash budget are left**, which is the number to look at
  before the timer records are added: rung 0.1 is not finished, and the next change to this
  crate is the one that has to prove it still fits.
- `Divergence::Sequence` cannot be produced by any *workflow* behaviour, and its
  documentation now says so rather than implying a protection the machine does not give. A
  workflow call carries no sequence, so what reaches that check is an out-of-order journal
  or a driver feeding the wrong record. The same fault is `MalformedHistory` when the cursor
  meets it outside a boundary — two diagnoses for one fault, which §08's table asks for by
  putting "sequence" in the divergence row, and which is written down rather than left to be
  discovered in a log.
- The gate list grew to 31 rules. That is a cost — five places to edit for one rule, per
  CLAUDE.md's checklist — accepted for the reason the record keeps giving: an invariant with
  nothing that fails a build over it is a comment.

## Alternatives considered

- **A pure `decide(history, request) -> Resolve` function.** Simplest to test, and it was
  the first sketch. Rejected because a stateless function cannot make divergence terminal:
  each call is a fresh opinion, so "no best-effort recovery" would be a rule for the driver
  to keep. The pure part survived as `EffectRequest::divergence_from`, which is where each
  flavour is tested without a history to put it in.
- **Methods on `ReplayCursor` itself.** Rejected on two counts: it merges "history against
  itself" with "history against the workflow", which ADR 0008 separated on purpose, and it
  would have grown `REPLAY_SURFACE` — the pin that exists so §02 decision 2 cannot be
  weakened by accident.
- **A `HistorySource` trait the kernel pulls records from.** Rejected for ADR 0008's
  reasons, unchanged: a trait that yields borrowed records is a storage interface in the
  crate whose must-not-own cell names storage drivers, and the caller-pumped shape is what
  keeps the page out of the kernel.
- **One call per boundary, with the machine buffering the schedule record.** Rejected
  because the buffer would be a borrow of the caller's page held across calls, which is
  precisely the thing ADR 0008's "no lifetime parameter" rules out — and the payload bytes a
  row-1 result hands back have to be live at the moment they are returned.
- **A `const` table of `(state, input) -> action` rows with a lookup.** Rejected as the
  same table written twice: an exhaustive `match` *is* the table, and the compiler checks it.
- **Leaving the position gate's refusals non-terminal**, and narrowing `diverged()`'s
  documented postcondition to "divergence found by comparing a request against a record"
  instead. Considered seriously, because making them terminal ends a run over what may be a
  façade bug, and because a rung-0.4 async façade that re-polls a workflow future would hit
  the half-open case. Rejected: the two-call protocol already forces a façade to hold
  per-boundary state — it must keep the `EffectId` it dispatched under and the result bytes —
  so re-entering `intent` on one boundary is a façade defect rather than a shape the design
  needs. And the harm of continuing is not hypothetical: the run appends effects no replay
  could reproduce.
