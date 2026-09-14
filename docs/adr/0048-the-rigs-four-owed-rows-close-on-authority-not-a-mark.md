# ADR 0047: the rig's four owed rows close on authority, not a mark

- Status: accepted
- Date: 2026-09-14
- Issue: [#96](https://github.com/madmax983/waymaker/issues/96)
- Supersedes: nothing
- Related: [0027](0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md),
  [0022](0022-the-bank-swap-is-a-typestate-and-step-one-is-a-value-being-consumed.md),
  [0020](0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md)

## Context

ADR 0027 gave the rig six of the failure matrix's ten rows and named the other four
`Owed`, in the table `xtask::docs::FAILURE_ROWS` still calls the gate over. Issue #96 states
what each one needed. Rows `during-inactive-bank-erase-or-write` and
`after-new-bank-seal-barrier` needed a workload that rolls over: `Rig::judge` and
`Rig::resume` walked `Rig::BANK` by name, and nothing had ever asked this rig to install a
second run, so the question "which bank does a boot choose" and "which bank is `Rig::BANK`"
had always had the same answer. Row `history-capacity-reached` needed a bank the workload
could fill, not a probe asking for a record wider than the bound. Row `replay-divergence`
needed the rig to replay a recovered journal against a workload that disagreed with it at
one point, and require `NondeterministicWorkflow`-shaped refusal before any dispatch.

`crates/waymaker-rig/tests/matrix.rs`'s own module documentation already named the risk of
doing this carelessly: the six rows already swept are pinned by exact crash-point counts —
86, 84, 2, 42, 84, 138 — and a change to `Rig::judge` or `Rig::resume` that moved any of them
would be indistinguishable, from the test output alone, from a change that closed the four
owed rows correctly.

## Decision

**`Rig::authority` replaces the count `Rig::authoritative_banks` used to compute directly.**
`bank::select`'s answer was always thrown away down to a `usize` the moment it left the
media read, because nothing needed more than "how many". A swap workload needs to know
*which* bank, so `authoritative_banks` is now a two-line wrapper over `authority`, and every
existing caller keeps asking the question it always asked — `authority_count(self.authority
(..)?)` is the same number `authoritative_banks` returned before. This is why the six swept
rows do not move: the only place `authority` and `authoritative_banks` can answer
differently is a device with a second bank actually installed, which none of those six
crash points ever produces.

**`installed_journal` now checks authority, not only a run id.** Before this change it read
`Rig::BANK`'s header, compared its run id against the workload's, and answered `Some` on a
match — regardless of whether `Rig::BANK` was still the bank a boot would choose. A swap
does not touch the bank it retires until an optional, lazy step 7, so a retired bank's
header still names the run that used to be current, and the old check would still find it. The
fix is one added arm: `installed_journal` refuses immediately unless the `bank::Authority` it
is handed names `Rig::BANK` specifically. This is the whole of what makes row 8 provable
rather than merely constructed — reverting the arm and rerunning
`after_new_bank_seal_barrier_the_new_bank_is_authoritative_and_the_old_run_is_never_current_again_on_the_rig`
reproduces the defect on a real crash point: `Rig::resume` answers `Ok(Completed { recovered:
3, redelivered: None })` from the retiring bank's own three records, on a device whose
authority had already moved to the bank the swap installed.

**`Rig::iterate_until_rollover` is the workload rows 7 and 8 needed.** It writes a run's
`RunStarted` and as many schedule/completion pairs as it is asked for, then stops — no
`RunCompleted`, because a rolled-over run's continuation is whichever bank a swap leaves
authoritative, not this bank's own end. The seven-step swap itself is not a new `Rig` method:
`crates/waymaker-rig/tests/matrix.rs`'s `perform_rollover_swap` drives `waymaker_flash::swap`
directly against the bank the partial run left off in, the way
`crates/waymaker-fault/tests/swap.rs` already drives it for the model, rather than adding a
swap API surface to `Rig` that only a test would call. The whole combined sequence — the
partial run, the swap, and a small complete run written into the installed bank — runs
through the crash injector once, in `rollover_sweep`, and every point is classified by
`bank::select`'s answer alone: a swap declares no journal record and marks no witness, so
there is nothing else to read a row from. A point before the swap's own operations began is
one of rows 1 to 6 already; the boundary is the operation count of a separate fault-free run
of just the partial sequence, so the two counts are told apart without guessing at an
operation index.

**Row 9 gates the writer instead of searching for a smaller bank.** `Rig::iterate_reserved`
and `Rig::resume_reserved` are `iterate` and `resume` with `waymaker_flash::capacity::Reserved`
in place of the ungated `Journal`. `Rig::new`'s own construction-time check
(`RigError::BankTooSmall`) already prices every record of the whole run at its worst case, so
a bank that construction accepts always has enough *physical* room for the run it was sized
for — searching for a smaller *geometry* the way `waymaker-drive`'s own row 9 does finds
nothing, because geometry sizes are powers of two and the two thresholds move together. What
does not track construction's own accounting is a *reserve* built from bounds wider than the
workload's true maximum: `Reserve::admits` prices a record's tail from the declared bound,
not from what a real payload turns out to need, so a bound declared wider than
`Workload::MAX_PAYLOAD_BYTES` reserves more than the run will ever use and refuses before
construction's own worst case would have. `near_capacity` searches declared tail widths on
the rig's own shared fixture rather than searching geometries, and the explicit exit past the
refusal is the same swap rows 7 and 8 drive, run once by hand.

**Row 10 is one changed record, not a shortened or corrupted run.** `Workload::diverging`
adds one field, `divergent: Option<u16>`, and changes exactly one thing: the schedule record
at that index declares a different activity kind. The run identity (`Workload::run`, which
depends only on the seed and the iteration), the record count, and every other record's bytes
are untouched. `Rig::resume_declaring` is `resume` with the declared workload taken as an
argument instead of derived from the iteration number, so a caller can audit history against
a workload other than the one that wrote it. `Audit::saw` — unchanged — is what actually
refuses: a declared record that disagrees with what recovery reads back is
`Breach::RecordDiffers`, the same breach a torn or rewritten record produces, at the first
point of disagreement and before anything is written. The test drives this at every crash
point that leaves the second effect's schedule recovered and its completion outstanding — the
sharpest case, an open effect whose request has changed — twice in a row, and requires no
mutation and no dispatch each time.

**The census is renamed and pins all ten counts.**
`the_rig_fills_six_rows_and_names_the_seventh_as_its_gap` — a name that asserted a gap this
issue closes — is now `every_row_of_the_table_is_reached_and_the_sweeps_have_not_thinned`,
matching the model half's own name for the same idea. `xtask::docs::FAILURE_ROWS` moves all
four rows off `RigStanding::Owed`, two — the bank rows — to `RigStanding::Swept` and two —
history-capacity-reached and replay-divergence, driven once each rather than swept — to a
`RigStanding::Driven` added by review so a hand-driven row is never rendered as one the
injector's own census covers, and `CLAUDE.md`'s table follows.

## Consequences

**The numbers.** `rollover_sweep` classifies 236 crash points of the combined sequence into
row 7 or row 8: 61 in `during-inactive-bank-erase-or-write`, 175 in
`after-new-bank-seal-barrier` — the second figure moved from its original 167 once review
found the sweep never reclaimed the retiring bank at all; see below. Rows 9 and 10 are
driven once each, as they are on the model.
All ten rows are now pinned in one table by
`every_row_of_the_table_is_reached_and_the_sweeps_have_not_thinned`, and the six pre-existing
counts — 86, 84, 2, 42, 84, 138 — did not move.

**`RigError` gained two variants.** `Capacity(Refusal)`, for a reserve's own refusal, and
`Reserve(CapacityError)`, for a reserve that does not describe the journal it is applied to.
Both are additive to an already-`#[non_exhaustive]`-shaped match everywhere `RigError` is
consumed in this workspace by pattern rather than by an exhaustive `match`.

**`Rig` gained five public functions**: `iterate_until_rollover`, `iterate_reserved`,
`resume_reserved`, and `resume_declaring`, plus the private `authority` this ADR's fix
depends on. None of the four public additions changes what `iterate` or `resume` do; each is
a new entry point rather than a new branch in an existing one, which is what keeps the six
swept rows' pinned counts exact.

**Review found two more defects of the same shape as this ADR's own fix: an instrument
answering a question issue #96 made reachable for the first time that it had never been
asked before.** `iterate_reserved` and `resume_reserved` wrote a record's `Attempted`
witness mark *before* asking `Reserve::admits` whether that record would fit, so row 9's own
no-mutation claim was false on the first encounter with a refusal — invisible in the
original test because a replay's witness continuation skips a mark the first attempt already
wrote. A free `admits` call, made before any mark, closes it. And `Rig::judge` gating its
audit on *current* authority — the same gate `Rig::resume` correctly needs — made a
healthy row-8 rollover's own retired bank read as though its acknowledged records had been
lost, because `uninstalled` assumes a bank with no current authority has nothing to say
about this run rather than that it said something and was superseded. `installed_journal`
stays authority-gated for `resume` and `recover_prefix`; `judge` moves to a new
`own_bank_journal`, which audits `Rig::BANK`'s own header by run id alone, regardless of
which bank is authoritative now — a retired bank's own history does not change when a swap
moves authority away from it. Both were verified failing against the prior code before their
fixes landed.

**A later round found two more of the same shape.** First, `resume_declaring`'s `declared`
can share this rig's seed and iteration — so it matches the recovered prefix exactly — while
naming more effects than `Rig::new` provisioned the bank and the witness for. Left
ungated, review found `resume_as` running past that provisioning until an unrelated capacity
error stopped it, rather than refusing before any write or dispatch as `resume`'s own
postcondition promises; `resume_as` now refuses a workload wider than `self.effects` before
touching the device. Second, `rollover_sweep`'s combined run never called
`Installed::reclaim`, because `Installed::recovery` and `Installed::reclaim` both consume
the value `commit` returns and the sweep took the former to keep writing into the bank it
installed — so the crash injector never produced a point during the retiring bank's own
erase or its barrier, and row 8 was published as fully swept regardless. `drive_rollover_swap`
now reclaims first and re-derives the installed bank's journal region by hand, which is
where row 8's count above moved from 167 to 175; row 7's count did not move, because
reclaiming the retiring bank can only lose it `bank::select`'s vote, never regain one ahead
of the bank already installed. Both were verified failing against the prior code before
their fixes landed.

**What is still owed.** The two bank rows are swept at one `effects_before_swap` value and
one declared next-run input; unlike `waymaker-fault`'s own swap sweep, this one does not vary
the geometry or drive a spare bank holding a stale sealed run. Row 9's search is over
declared bounds on the rig's shared fixture, so it says nothing about a bank sized
differently. Issue #96's board half is exactly as unmet as ADR 0027 left it: rows 2, 3 and 4
are told apart by whether the dispatcher was entered and returned, which the harness sees and
a reset takes with the RAM, so a board needs a durable record of the world rather than this
rig's own log. [What the boards still owe](../../CLAUDE.md#what-the-boards-still-owe) is
unchanged by this issue.

## Alternatives considered

**A `Rig::swap` method, so the test does not drive `waymaker_flash::swap` directly.** Rejected
for the reason `crates/waymaker-fault/tests/swap.rs` already drives the model's own swap
sweep directly rather than through a `Journal`-level convenience: a swap API on `Rig` would
be surface nothing but a test calls, and `Rig`'s must-not-own cell for on-media authority is
the driver's, not the façade's — adding one API for one caller is the shape `capacity-reserve`
and the other surface-pinning rules exist to make a reviewer notice.

**Generalizing `judge` and `resume` to take an explicit bank argument on every call.**
Considered and rejected: it would change the signature of two already-widely-used public
functions for a case — a rolled-over run — that only the new rows exercise. Keeping
`Rig::BANK` as the implicit bank for `judge`/`resume`, and fixing `installed_journal` to
verify it is still the *authoritative* one, closes the actual defect without touching either
signature.

**Pricing the capacity row from a smaller geometry.** Tried first, matching
`waymaker-drive`'s own row 9. Rejected once the search showed why it cannot work here:
`Rig::new`'s construction-time check and `Reserve`'s per-record admission price the same
worst case, so a geometry accepted by the first always has room for the second under real
payloads. A declared bound wider than the workload's true maximum is what creates the gap
between them.

**Deriving row 10 from two different seeds.** Two workloads sharing nothing but a run id
would disagree at the very first record, which is a truncated-or-garbled-run shape this
matrix already covers elsewhere, not the "one boundary's request changed" shape design
document §14 and `waymaker-drive`'s own `Divergent` workflow mean by divergence.
