# ADR 0023: a watchdog reset is modelled, and its difference is one return

- Status: accepted
- Date: 2026-09-06
- Issue: [#27](https://github.com/madmax983/waymaker/issues/27)
- Supersedes: nothing
- Related: [0013](0013-the-fault-harness-is-a-crate-above-the-layers.md),
  [0014](0014-the-oracle-is-four-lines-and-the-sweep-is-seeded.md),
  [0021](0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md)

## Context

Issue [#27](https://github.com/madmax983/waymaker/issues/27) asks for two things about
resets, and is explicit that they are two: supply cuts "at randomised points during schedule,
dispatch, and completion writes", and "watchdog-reset tests at the same three points — a
watchdog reset is not identical to a brownout and both must be covered".

The first was discharged when `waymaker-rig` landed. The second was not discharged at all.
`waymaker_rig::census` declared the six cells, the plan armed the cause and the log recorded
it, and every injection the harness performed was a power loss. Three cells were filled,
three were empty, and a test asserted the emptiness.

The reason given was that a host cannot perform a watchdog reset. That reason was too broad.
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
`every_watchdog_image_is_one_a_power_cut_also_produces` proves it over the real journal
writer, and requires the inclusion to be *proper*, so the two causes cannot become one model
wearing two names.

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
fires on a timer rather than at a call boundary, which is the thing no model supplies.

`the_sweep_covers_five_of_the_six_census_cells_and_names_the_sixth` asserts the five and
requires `Coverage::verdict` to refuse the run, naming `(Dispatch, Watchdog)`. A census that
reported six would be reporting coverage nothing produced.

**The third difference is measured as a cost rather than modelled as a state.** RAM retention
makes a shortcut available on a board that a brownout forbids: judge a run from the history
the rig still holds, rather than from a fresh scan of media.
`a_rig_that_judged_from_retained_ram_would_pass_a_loss_it_must_catch` drives the existing
acknowledge-before-commit tooth and requires the retained-RAM reading to excuse *every* loss
the media-reading rig catches, with `the_real_rig_reads_the_history_from_media` as the control
that makes the disagreement a property of the loss rather than of two code paths.

**Every sweep in the workspace grew by one point per mutating operation and per barrier.**
That is cheap by construction — the interior points are the expensive ones and none are added
— and it moved four pinned counts, each of which is a census doing its job:
`waymaker-conformance`'s `SWEEP`, `waymaker-fault`'s enumeration arithmetic, and
`tests/swap.rs`'s three per-operation point counts.

**Two exhaustive matches gained an arm.** `Interruption` and `FaultError` are deliberately not
`#[non_exhaustive]`, so the compiler listed every call site that had a case to think about.
The rig's `cause_of` was the only one outside `waymaker-fault`, which is the layering working.

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
