# ADR 0053: the capacity reserve formula is a fleet precondition

- Status: accepted
- Date: 2026-09-15
- Issue: [#188](https://github.com/madmax983/waymaker/issues/188)
- Supersedes: nothing
- Related: [0020](0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md),
  [0036](0036-workflow-versioning-is-a-range-and-a-recorded-branch.md),
  [0037](0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md),
  [0052](0052-a-torn-record-redelivers-when-its-reserved-slot-is-clean.md)

## Context

`waymaker-flash::capacity::Reserve::for_layout` computes §10's reserve fresh, on every boot,
from the firmware's own `Bounds` and `BankLayout`. Nothing about the reserve is on media.
`Recovery` and `Scan` decode bytes and a seal, never a reserve — `recovery-surface` pins that
surface narrow on purpose, so a bank does not carry a copy of the formula that admitted it.

Issue [#95](https://github.com/madmax983/waymaker/issues/95) added `redelivery_slack`: extra
room so a torn, ignored outcome attempt can still be redelivered. ADR 0052 records that fix.
Codex's review of that pull request found a third instance of the same shape, filed as issue
[#188](https://github.com/madmax983/waymaker/issues/188): the fix only holds for a schedule
admitted by firmware that already has it.

A device may run firmware A, whose reserve leaves less room than firmware B's reserve later
needs. Firmware A admits a schedule at its own boundary. The device is upgraded to firmware
B. That schedule's outcome write tears.

Firmware B's recovery reads bytes and a seal, not a formula. It cannot tell firmware A's
schedule from its own. So it ignores the torn attempt and redelivers, the same as ADR 0052
always does. But the room firmware A actually left is firmware A's own promise, not
firmware B's.

Suppose firmware B's reserve asks for more room. Then firmware B's own retry refuses with
`Refusal::NearCapacity`. The reserve is recomputed the same way on every boot, so the
refusal repeats on every later boot too.

Nothing here is specific to `redelivery_slack`. Any future change that makes `Reserve`
reserve more is exposed the same way, for the same reason: the formula is a policy, not a
record.

Closing this fully needs new infrastructure this ADR does not build — either a wire-format
field recording what an admission-time reserve guaranteed, or `Recovery` gaining a
dependency on `Reserve` it deliberately does not have today. Neither is warranted yet:
"Nothing in this repository has ever run on a board" (CLAUDE.md), so no device exists whose
journal a weaker firmware wrote. Building the machinery now, against a scenario nobody has
reached, is the wrong order — this project's own rule for a record change is "the model and
the invariants first, then the proofs, then the code", and there is no real device history
yet to model.

## Decision

**The capacity-reserve formula is a precondition on how a fleet is upgraded, not a
guarantee the device enforces.** A fleet must not run firmware whose reserve formula asks
for more room than the formula that admitted a still-outstanding schedule already on that
device. This is the same shape of precondition ADR 0036 already states for widening
`oldest`/`current` and ADR 0037 already states for the read-set: an ordering a binary cannot
check, stated so an operator can.

Two firmware versions are compatible on one fleet exactly when their reserve formulas ask
for the same room, or firmware B's formula asks for no more than firmware A's — down to the
reserve of every kind, not only the total. Widening the read set is safe because a later
format only adds record kinds; widening a reserve formula is not automatically safe the same
way, because it removes room retroactively from a schedule already admitted under the
smaller figure.

This is documented, not enforced, and the residual is real:
`a_schedule_admitted_by_a_weaker_reserve_can_strand_a_stricter_retry` in
`crates/waymaker-flash/tests/capacity.rs` drives exactly the scenario above — a schedule
admitted at the pre-issue-#95 boundary, a torn outcome, a reboot on firmware carrying
`redelivery_slack` — and shows two things: the current reserve really does refuse a schedule
the old rule admitted, at the exact room the old rule left; and the failure this causes is
bounded and safe. No byte moves on the refusal, and the refusal repeats identically on every
later boot. The run cannot progress past the outstanding effect, but nothing is corrupted,
no identity is reused, and no guarantee in [the guarantees table](../../CLAUDE.md#the-guarantees-and-what-holds-each-up)
is broken — the failure is a stall, not a violation.

## Consequences

**A device can be permanently stuck if this precondition is broken.** An effect redelivered
forever, with no way to ever record its outcome, is the same stranding ADR 0052 closed for a
single firmware version, reopened across two. There is no `continue_as_new` escape: the
effect is outstanding, and §08 has no edge from an unresolved effect to a terminal record.

**No code changes.** `Reserve::for_layout`, `Reserve::exit_bytes_after`, `Recovery` and
`Scan` are unchanged. The fix is the precondition, stated where an operator planning a fleet
upgrade reads it, plus a falsifier proving the risk is real and bounded rather than argued.

**What is still owed.** Closing this for real needs one of the two mechanisms named in
[Context](#context) — a wire-format field, or `Recovery` gaining a dependency on `Reserve` —
and both are new infrastructure belonging to rung 0.4's dispatcher or later, when a real
fleet-upgrade path exists to design against. Until then, a firmware change that makes any
term of `Reserve` larger is a fleet-ordering decision an operator must make, the same
standing ADR 0036 already gives version-range widening.

## Alternatives considered

**Record, on media, what the admitting firmware's reserve guaranteed.** A wire-format
change with ADR 0037's own rigor: a new field, a corpus case, a read-set argument. Rejected
for now because there is no device history to design the field against, and a field chosen
today would freeze a guess into a format ADR 0037 says is frozen at v1.

**Give `Recovery` a dependency on `Reserve`, and refuse a redelivery the current formula
cannot cover.** Would close the gap in code. Rejected for now because it widens
`recovery-surface`'s deliberately narrow surface for a scenario nobody has reached, and
because the right refusal shape — treat the slot as `Ending::Unsealed` rather than ignoring
it — needs the same model-first order this project already asks of a record change.

**Say nothing, and let the residual stay implicit in ADR 0052's own prose.** Rejected: this
project's own rule is that a guarantee is only worth the evidence it could have failed. A
residual with no falsifier and no fleet-facing precondition is a finding that rots exactly
the way CLAUDE.md warns a deferred question rots without a table holding it.
