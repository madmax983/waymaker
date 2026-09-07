# ADR 0027: the failure matrix is ten named tests, and a rig that resumes

- Status: accepted
- Date: 2026-09-07
- Issue: [#31](https://github.com/madmax983/waymaker/issues/31)
- Supersedes: nothing
- Related: [0015](0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md),
  [0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md),
  [0021](0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md),
  [0023](0023-a-watchdog-reset-is-modelled-and-its-difference-is-one-return.md),
  [0026](0026-redelivery-is-the-kernels-answer-and-at-least-once-is-the-contract.md)

## Context

Design document §14 states failure semantics as a table: ten failure points, what recovery
must produce at each, and what the engine must then do. Issue #31 is rung 0.3's exit
criterion, and it asks for two things. Each row has a named test, "and the test name matches
the row so a failure reads as a spec violation". And the matrix runs in CI "on the in-memory
model and on the hardware rig".

Before this change every row was covered somewhere and no row was covered by name.
`crates/waymaker-drive/tests/crash.rs` held rows 1, 2, 4, 5 and 6 as three blended
assertions; `crates/waymaker-fault/tests/swap.rs` held rows 7 and 8 by generation number;
`crates/waymaker-flash/tests/capacity.rs` held row 9 and `crates/waymaker-core/tests/transition.rs`
row 10. A failure in any of them read as the test that failed, not as the row it broke.

The rig was further away. `waymaker-rig` cuts during three write phases and judges what a
reset left; it never carries a cut iteration on. Six of §14's rows state a *required
behaviour* — redeliver the stable id, never execute a completed activity again — that only a
resume can observe. A rig that only judged media could classify a crash point into a row and
could not say whether the row's behaviour held.

## Decision

**The rows are a vocabulary, and it lives in the rig.** `waymaker_rig::matrix::Row` is the
ten rows in the table's order, each with a stable id: `during-schedule-frame-write`,
`after-schedule-barrier-before-dispatch`, `during-physical-activity`,
`after-activity-before-completion-barrier`, `during-completion-write`,
`after-completion-barrier`, `during-inactive-bank-erase-or-write`,
`after-new-bank-seal-barrier`, `history-capacity-reached` and `replay-divergence`. It is in
the rig rather than in the driver because the rig is the crate a board links, and a vocabulary
the board cannot name is a vocabulary the model and the rig would eventually spell
differently. `Matrix` is the census over it, and `Matrix::verdict` fails closed at the first
row nothing reached. Both are pinned by `rig-oracle`, for `Coverage`'s reason.

**One named test per row, on the model.** `crates/waymaker-drive/tests/matrix.rs` has ten
tests whose names are the failure point followed by the required behaviour. Seven rows are
crash sweeps: every crash point `waymaker-fault` lists is put in a row from the operation it
interrupted, cross-checked against the media it left, and the reboot is held to the row's
behaviour. Row 3 is not a storage crash point and is modelled as a world that is entered and
does not return. Rows 7 and 8 drive the real `Swap` at every crash point and then *boot the
driver* on whichever bank `select` names, so "continue the old run" is a run continuing rather
than a generation comparing. Row 9 counts mutations during the refused boot and then performs
the swap from that state. Row 10 reboots every crash image whose history reaches the divergent
effect with a divergent workflow, twice.

The classification is read off the recorded operation sequence: the reference run is six
records of four operations each, pinned by
`the_reference_run_is_six_records_of_four_operations_each`. A power cut after a commit
barrier returned is normalised to the next record's frame program with nothing landed, which
is where the writer meets it. Every class is then cross-checked: a row whose media disagrees
with its operation is a panic, not a row.

**The rig resumes.** `Rig::resume` recovers the installed bank, audits the prefix record by
record against the workload, redelivers the effect whose schedule has no completion under the
same index, and writes what the run still owes. It takes no cut and writes no witness mark: it
is judged by what it returns and by what the dispatcher saw. `crates/waymaker-rig/tests/matrix.rs`
classifies every crash point from the witness, the recovered count, the journal's ending and
the dispatcher's own log, resumes it, and holds it to its row.

**The rig fills six rows and says so.** Rows 7 to 10 need a swap workload, a capacity refusal
and a divergent replay, and this rig has none. `the_rig_fills_six_rows_and_names_the_seventh_as_its_gap`
requires `Matrix::verdict` to refuse at `during-inactive-bank-erase-or-write`. That is the
same shape ADR 0021 chose for the watchdog cells: a census that fails naming the gap, rather
than a table shortened to what passes.

**A gate holds the five places together.** `xtask::docs::FAILURE_ROWS` is the table, and the
`failure-matrix` rule fails a build in which a row is missing from the rig's `Row::id`, has
no `#[test]` of its name in the model file, is marked swept and never named in the rig file,
has no `CLAUDE.md` row carrying its failure point, its test and its rig standing, or is absent
from this ADR. The vocabulary check runs both ways, so a row added to the enum and not to the
table fails too.

## Consequences

**The numbers.** On the model, 541 crash points are classified into a row and every one is
held to its row's behaviour: 138 in row 1, 12 in row 2, 6 in row 4, 159 in row 5, 79 in
row 6, 122 in row 7 and 25 in row 8, with rows 3, 9 and 10 driven directly. Every one of the
seven swept rows is reached by a power cut and by a watchdog reset. On the rig, 434 crash
points are classified and resumed, plus two for row 3: 86, 84, 42, 84 and 138 in rows 1, 2, 4,
5 and 6.

**Row 5 does not hold as written, and this records it.** §14 says "redeliver". A torn
completion leaves a journal with no append point — ADR 0018's anti-bricking rule, restated by
ADR 0026 — so neither the driver nor the rig can redeliver into that bank. Both refuse. The
test asserts what does hold: the torn completion is ignored, no partial bytes reach the
workflow, and nothing is dispatched. The run's continuation is §10's `continue_as_new`, which
is a new run under a new id. A dispatcher that swaps on a torn tail is rung 0.4's.

**Row 2 is reachable on the model after all.** "After schedule barrier, before dispatch" has
no power-cut instance in a writer that dispatches as soon as the barrier returns. It has a
watchdog instance — the barrier that does not return — and a power-cut instance at the seal
that landed whole with its barrier refused. Twelve crash points, six of them watchdog resets.

**The driver gained a dev-dependency on the rig.** A test-support crate depending on another
is the standing `waymaker-rig` already has on `waymaker-conformance`. No layer is touched and
no budget moves.

**`Rig::resume` is a fifth public function the runner pin lists**, and `RigError` gained a
`Breach` variant: a resume over a prefix that is not this run's refuses before writing.

**What is owed** is the four rig rows, written in the table as `Owed`. A swap workload in the
rig is what fills 7 and 8, and it is the change that makes `Rig::judge` walk the bank `select`
names rather than bank A. A capacity refusal and a divergent replay on the rig are cheaper and
are still absent, because a probe that asks for a record wider than the bound is not "history
capacity reached". The boards are still owed both causes, exactly as
[what the boards still owe](../../CLAUDE.md#what-the-boards-still-owe) says.

## Alternatives considered

**Ten tests in `crash.rs`.** The cheapest reading of the issue. Rejected because rows 7 to 10
are not crash points of the driver, and because nothing would hold the ten to the table:
a renamed test is a row with no name.

**The vocabulary in `waymaker-core`.** The kernel's `Resolve` is where §08's rows already
live. Rejected because a public item in a layer is charged against §04's code-flash budget and
must be reached by the size probe, for a type only tests read.

**A rig census over six rows.** Honest about what the rig reaches, and wrong in the way ADR
0021 refused: a table sized to what passes is a table that stops naming what is owed.

**Classifying crash points from the media alone.** The recovery-result column can be read
off media; the failure-point column cannot, because a torn frame and a frame never begun
leave the same erased tail. The operation index is the evidence, and the media is the check.
