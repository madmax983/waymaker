# ADR 0023: a watchdog reset is modelled, and its difference is one return

- Status: accepted
- Date: 2026-09-06
- Issue: [#27](https://github.com/madmax983/waymaker/issues/27)
- Supersedes: [0021](0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md)'s
  decision that "a watchdog reset is a cause the rig carries, not one a host can perform".
  Nothing else in 0021 is changed.
- Related: [0013](0013-the-fault-harness-is-a-crate-above-the-layers.md),
  [0014](0014-the-oracle-is-four-lines-and-the-sweep-is-seeded.md)

## Context

Issue [#27](https://github.com/madmax983/waymaker/issues/27) asks for two things about
resets, and is explicit that they are two: supply cuts "at randomised points during schedule,
dispatch, and completion writes", and "watchdog-reset tests at the same three points — a
watchdog reset is not identical to a brownout and both must be covered".

The first was discharged when `waymaker-rig` landed. The second was not discharged at all.
`waymaker_rig::census` declared the six cells, the plan armed the cause and the log recorded
it, and every injection the harness performed was a power loss. Three cells were filled,
three were empty, and a test asserted the emptiness.

The reason given was ADR 0021's, in a bolded decision of its own: "a watchdog reset is a cause
the rig carries, not one a host can perform". That reason was too broad.
A host cannot perform a *power cut* either: `waymaker-fault` models one, and the model's
power-cut cells are counted while the physical attestation stays owed to the boards in
`xtask::docs::HARDWARE_TARGETS`. Applying one standard to a brownout and a stricter one to a
watchdog reset left an acceptance criterion with no coverage and no plan to get any.

What made the first attempt wrong was different, and Codex was right to reject it: it
partitioned the existing power-loss enumeration by `Progress` and called half of it watchdog
coverage. That groups power cuts. It does not perform a reset the supply survives.

## Decision

`waymaker_fault::Interruption` gains a third variant, `Watchdog`, and it is a fault of its
own rather than a label on an existing one. Three things differ from a brownout, and each is
placed where it can be checked.

**The unit in flight completes.** The supply holds, so the flash controller finishes the
program unit or the erase block the core stopped believing in. A `Progress::Bytes` armed on a
watchdog reset is rounded *up* to the unit, so media after one always holds a whole number of
units. A brownout can stop inside one.

**The writer is never told.** The call returns `FaultError::WatchdogReset` at every
`Progress`, including `Whole` — where a power cut returns `Ok(())` first. Design document §02
decision 3 is about that state: a writer that does `barrier()?` and then dispatches
dispatches after a power cut and does not after a watchdog reset.

**RAM survives.** Nothing in `waymaker-fault` models RAM. `waymaker-rig` owns that half,
because a durable witness is exactly what RAM retention would let a reader skip.

Only `(i, Whole, Watchdog)` is enumerated. Every other watchdog world is a power-cut world the
list already has: rounding up maps a reset inside a unit onto the power cut at the boundary
above it, the writer is dead in both, and an exhaustive list that counts one crash point twice
is no longer a count of anything. `tests/watchdog.rs` measures both halves of that claim —
`a_watchdog_reset_inside_a_unit_is_a_power_cut_at_the_boundary_above_it` for the worlds left
out, and `a_watchdog_reset_at_a_whole_operation_is_not_a_power_cut_at_one` for the one kept.

On media a watchdog reset is therefore *weaker* than a brownout, and that is stated as a
theorem rather than left as a silence:
`every_watchdog_image_is_one_a_power_cut_also_produces` proves the inclusion over the real
journal writer. What rules out a relabelling is not that inclusion — it is proper for a
structural reason, since a power cut is enumerated at every interior byte and a watchdog reset
only at whole operations, so the count would separate them however they behaved. It is
`a_watchdog_reset_at_a_whole_operation_is_not_a_power_cut_at_one`, where the two answer the
caller differently.

`waymaker-rig`'s census credits a cell from the injector's own cause. `cause_of` reads
`injection.interruption` and nothing else, and
`a_watchdog_cell_is_credited_only_by_a_watchdog_reset` is what keeps it reading the cause
rather than the progress. That is the rule the rejected first attempt broke.

## Consequences

**Five of the six census cells are now filled on a host, and the sixth is named.** The
schedule and completion write points are reached under both causes. The *dispatch window* is
reached under a power cut only, and the reason is the second difference above: being in that
window needs the dispatch mark's commit barrier to have returned, and a watchdog reset is the
reset that does not return. No host run is ever in the window under one. A board's watchdog
fires on a timer rather than at a call boundary, so it can land inside the window; this
injector, whose every crash point is a storage operation, cannot.

**And the two cells that are filled buy less than a reader would assume.** The same sentence,
read forwards: the causes can only diverge where something other than another storage call
follows a completed operation, so at a schedule or a completion write they do not diverge at
all — the media, the ledger and the dispatch are the same as the power-cut twin's, and only
the cause the injector armed differs.
`the_two_causes_part_company_only_where_an_effect_follows_a_completed_call` measures all three
of those and requires a divergence to exist somewhere, so the claim is checked in both
directions. Those cells therefore record that the cause was performed and that recovery
survived it. They are not new coverage of media, and saying so is the difference between a
census and a tally. It is also the strongest objection to this change, which is why it is a
test rather than a paragraph.

`the_sweep_covers_five_of_the_six_census_cells_and_names_the_sixth` asserts the five and
requires `Coverage::verdict` to refuse the run, naming `(Dispatch, Watchdog)`. A census that
reported six would be reporting coverage nothing produced.

**The third difference is measured as a cost rather than modelled as a state.** RAM retention
makes two shortcuts available on a board that a brownout forbids, and the rig's teeth take one
each.

Skip the journal scan and judge from the history still in RAM:
`a_rig_that_skipped_the_journal_scan_would_notice_no_loss_at_all` asserts that such a reading
returns `Ok` for *every* run, the ones the media-reading rig breaches included. That is a
tautology and is written as one — a reading that never touches the journal cannot disagree
with the history the writer believed it wrote — so what carries the content is the contrast
beside it, which is asserted: on the same runs the real rig is not a constant either way.

Keep the *witness* in RAM instead of on media, and it holds every mark the writer issued,
including the ones a reset took off media before they landed. Those marks over-claim, so the
audit reports a loss that never happened:
`a_rig_that_kept_its_marks_in_ram_would_invent_a_breach_on_a_healthy_part` requires that to
happen on the correct writer, and requires it not to happen everywhere — a tooth that accused
every run would measure the fixture. That is the direction that can fail, and it is
`Breach::LostAcknowledgedRecord`'s own documented hazard: "it invents a breach on a healthy
device".

**Every sweep in the workspace grew by one point per mutating operation and per barrier.**
That is cheap by construction — the interior points are the expensive ones and none are added
— and it moved five pinned counts, each of which is a census doing its job:
`waymaker-conformance`'s `SWEEP`, `waymaker-fault`'s enumeration arithmetic, and
`tests/swap.rs`'s three per-operation point counts. It adds one point per operation, barriers
included — a barrier is not a no-op here, because "after it returned" is a different world
from "before it ran".

**Two enums gained a variant, and four matches gained an arm.** `Interruption` and
`FaultError` are deliberately not `#[non_exhaustive]`, so the compiler listed every call site
that had a case to think about: `FaultError::message`, `Session::completed`,
`Session::interrupt` and the rig's `cause_of`. The last was the only one outside
`waymaker-fault`, which is the layering working.

**One pre-existing defect surfaced beside it.** `Session::barrier` decided "the barrier ran"
by matching the `Progress` *variant*, so a hand-built `Progress::Bytes(0)` — which
`Progress::Bytes` documents as meaning `Progress::None` — was read as a completed barrier and
acknowledged the record before it. That obliges recovery to produce a record from a barrier
the caller said did nothing, which is a check failing in the direction a check must not. Fixed
here, with `a_barrier_stopped_at_zero_bytes_is_a_barrier_that_did_nothing`, because the new
cause adds a second way to reach it.

**The boards still owe both causes, and now owe one more thing.** A real part may abort the
unit in flight rather than finish it, may leave a bit at neither level, and has a reset-cause
register no model has. `HARDWARE_TARGETS` carries both rows and both stay `Not run`. The
dispatch-window watchdog cell is inside them.

## Alternatives considered

**Enumerate a watchdog reset at every byte, like a brownout.** Rejected: with the rounding,
those runs are the power-cut runs at the unit boundary above, so the sweep would have doubled
in the places it is largest to re-verify images it had already verified. The crate's own rule
against counting a crash point twice decides it.

**Enumerate a watchdog reset at unit boundaries.** The first version of this change did, and
it is still a duplicate: a reset at `Bytes(k * unit)` leaves what a power cut there leaves,
and the writer is dead in both. It survived one round of thinking because "unit-aligned" reads
like new information; it is not, and `a_watchdog_reset_inside_a_unit_is_a_power_cut_at_the_boundary_above_it`
is that realisation kept as a test.

**Model the reset-cause register.** A durable byte the rig reads back after a reset. It would
be true on a board and vacuous here: nothing in the model would read it that does not already
know which injection it armed.

**Model retained RAM in `waymaker-fault`.** A session that survives its own reset with the
caller's values intact. Rejected for the reason ADR 0013 keeps the harness ignorant of
records: RAM is the writer's, not the media's, and a storage model that carried writer state
would stop being reusable by any writer. The rig is where the writer lives, so the rig is
where the cost is measured.

**Leave the three cells to hardware.** The status quo, and the thing this ADR argues against.
It is defensible for the dispatch cell, where the model genuinely cannot produce the world,
and it was not defensible for the other two, where it produced no coverage for an acceptance
criterion that had none.
