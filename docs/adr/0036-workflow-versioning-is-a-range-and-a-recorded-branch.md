# ADR 0036: Workflow versioning is a range, and an upgrade branch is a recorded record

- Status: accepted
- Date: 2026-09-09
- Issue: [#40](https://github.com/madmax983/waymaker/issues/40)
- Supersedes: nothing
- Related: [0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md),
  [0024](0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md),
  [0029](0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md),
  [0030](0030-a-timer-is-a-boundary-and-its-clock-kind-is-on-media.md)

## Context

Design document §08 states four rules about workflow versioning:

> - Existing runs must continue under compatible code for their recorded version.
> - A firmware image that cannot replay the recorded version returns `IncompatibleWorkflow`.
> - Code changes that add, remove, or reorder effects require a new version or an explicit
>   recorded version gate.
> - Source-location hashes are not stable identity; call-order sequencing remains
>   authoritative.

None of the four was enforced. `RunStarted` has carried a `workflow_version` since issue
#13, and `waymaker-drive`'s `begin` compared it for **equality**, answering
`DriveError::NotThisWorkflow` on a mismatch. That is the opposite of the first rule: every
run in flight was refused the moment the binary changed, which is exactly what a durable
workflow engine exists to avoid. §09 numbered `VersionMarker` at 9 and issue #13 spent the
number with no body behind it, so the third rule had no mechanism at all.

The failure this leaves is the quiet one. An upgrade that adds a step in the middle of a
workflow shifts every effect after it by one sequence. The boot after the upgrade takes the
new path, reaches an effect boundary where history recorded another activity, and §08's
divergence check stops the run — permanently, because divergence is terminal and §08 gives a
diverged run no way to end. A fleet upgrade would strand every run that was in flight.

## Decision

Two mechanisms, because §08's rules are about two different questions.

**A version is a range, not a number.** `waymaker_core::version::VersionRange` is what a
firmware image declares: `oldest`, the earliest recorded version whose branches this binary
still holds, and `current`, the version it writes into a new run. `VersionRange::admits`
is §08's second rule as a total function with two answers — yes, and
`KernelError::IncompatibleWorkflow`. Two causes reach that one refusal and both are the same
fault from the run's point of view: `recorded > current` is a rollback, and
`recorded < oldest` is a branch this image retired on purpose. `Identity::versions` replaces
`Identity::version`, and `waymaker-drive`'s `begin` admits rather than compares.

`NotThisWorkflow` stays, and it now means only what it says: the journal belongs to some
other workflow. A log that could not tell the two apart would send an engineer to look for
the wrong bug.

**A branch taken during a run is a record.** `RecordRef::VersionMarker { seq, gate, version }`
is §09's kind 9, four payload bytes. `Boundary::gate` is the API a workflow author uses: the
first execution to reach a gate records `VersionRange::current` in a marker, and every later
boot is handed that number back — whatever branch the image running it would have chosen.
That is the whole of "an upgrade branch taken during a run is recorded and replays
identically forever after".

Three properties make it more than a note beside history:

- **It spends a sequence.** A marker is a boundary in the run's one ordered history, the way
  a deadline is. So a gate added, removed or moved shifts every boundary after it, and §08's
  third rule is enforceable by the sequence check that already exists.
- **It resolves itself.** There is no world between the intent and the answer — the effect of
  a gate is a branch inside the workflow — so one record both commits the decision and holds
  it, and the machine is settled the moment it is consumed. `ReplayMachine::version_intent`
  is therefore one call where `intent`/`outcome` and `timer_intent`/`timer_outcome` are two.
  There is no `VersionResolve`, and no new cursor position: kernel state stays at 104 B.
- **The record is durable before the branch is observable.** `Boundary::gate` writes the
  marker through §07's two barriers and returns the number afterwards, so there is no state
  in which the workflow has branched and media has not. That is §02 decision 3's shape at a
  boundary whose effect is a branch.

`Boundary::recorded_version` is the smaller companion: the version the run's own `RunStarted`
holds. It answers "what did this run begin under", it writes nothing, and it is a fact about
history rather than about the image, so a workflow that branches on it is deterministic. What
it cannot express is a decision taken part way through a run — that is the gate's.

**§08's fourth rule is an absence, so it is a gate rule.** A new rule id, `version-gate`,
fails a build over four things: the `VersionMarker` field set in both directions; the
versioning vocabulary's public surface in both directions; `VersionRange`'s shape — no
public field, no associated constant, no method at any visibility the pin does not list, no
submodule, no alias, no free function beside the pinned `impl`, no macro inside it, and a
crate-root re-export naming it at the source — because `oldest <= current` is exactly the
invariant every one of those gives back; and, in the kernel's two versioning files, any
route to a source location, whether a macro, an aliased import of one, or
`core::panic::Location`. A gate keyed on `line!()` changes identity when a comment above it
moves, so a reformatting would be a divergence.

The shape half is nine checks rather than three, and the extra six were bought the way
`timer-capability`'s and `effect-protocol`'s were: review of this change ran each of them
against a three-check version and watched the gate print `ok`. Two are recorded elsewhere in
this repository as having already defeated another rule — an associated constant against
`timer-capability`, a module-scope `pub(crate) fn` against `dispatch-wiring` — and the
rename-plus-decoy-module is the one `timer-capability` grew a crate-root half for. Meeting
them here rather than after they are demonstrated again is the whole value of writing the
defeats down.

What the identity half does **not** cover is where §08's fourth rule actually bites.
`GateId`'s field is public and the number in it is a *workflow author's*, chosen in a
workflow module — this crate's three examples, or a user crate no rule here can reach. A
`GateId(line!() as u16)` in `demo.rs` passes the gate, and review of this change ran that
too. The ban buys that the *engine* never derives identity from a source location; the rest
is a review question, and [what is not checked](../../CLAUDE.md#what-is-not-checked) says so
rather than leaving the list looking exhaustive.

The budget moves, and it is the first raise argued from a corrected figure.
`INCREMENTAL_CODE_FLASH_BYTES` goes from 12 KiB to **13 KiB** and
`FACADE_CODE_FLASH_BYTES` follows it to 14 KiB. The split, measured the way ADR 0020 measured
its own:

| Measured | layers | probe |
| --- | --- | --- |
| Before this change | 12220 B | 8460 B |
| The library change, through the reach the probe already had | 12396 B | 8496 B |
| With the probe reaching the whole new surface | 12820 B | 9108 B |

So §08's versioning costs **600 B** of layers, of which **176 B** is the change itself and
**424 B** is what `size-probe-reach` demands once every public function has to be named and
both refusals reached. The layers measure 12820 B of 13312, with 492 B left, and the `facade`
row 13218 B of 14336.

The first figure is smaller than it was when this was written, and the reason is worth
recording rather than smoothing over: `intent` went over clippy's line budget when its
`BoundaryKind` arm grew a second record kind, so rows 1, 2 and 4 were split out into
`recorded_effect` — the twin `recorded_timer` already was. That refactor is 72 B, and it is
the only reason a raise of 1 KiB leaves 492 B rather than 420 B.

## Consequences

`Identity` is a breaking change for every workflow: `version: u16` becomes
`versions: VersionRange`. The three reference workflows and every test take
`VersionRange::exact(..)`, which is what an image that has never been upgraded declares.

A gate costs a record. §10's reserve prices a marker at a terminal record — it opens nothing,
so it owes only the run's exit — and `Reserve::for_layout`'s floor does not price gates at
all. That is safe rather than merely cheap, and the reason is arithmetic review supplied: a
marker is strictly cheaper than a schedule at every alignment, because its body is four bytes
against a schedule's eight and its exit is `terminal_bytes` against a schedule's
`outcome + terminal`. So the floor's `start + schedule + tail` term dominates, and a gate can
never leave a run unable to reach an exit. `crates/waymaker-flash/tests/capacity.rs` holds
that rather than leaving it argued.

**A rollback that meets a newer recorded branch refuses the run outright.** That is §08's
second rule doing what it says, and it is worth stating as a cost: a fleet that upgrades,
records branch 2 on some devices, and then rolls back to an image whose range stops at 1 has
bricked those runs until an image that can replay branch 2 is deployed again. The way to avoid
it is to widen `oldest` before narrowing `current`, and nothing here enforces that ordering.

**A workflow that branches on its own image version is still wrong, and still undetected
until the next effect.** `Branching::ImageVersion` in `waymaker-drive`'s reference workflow is
that mistake written down, and `an_added_effect_with_no_gate_is_a_divergence` watches it
fail. There is no signature that distinguishes an ambient read from a recorded one; §08 says
so about determinism generally, and this is the same limit met once more.

**A marker torn by a power cut leaves the branch undecided.** The next boot re-decides, and
under a *different* image it may decide differently. That window is unavoidable — the record
was never durable — and it is narrower than §07's effect window, because no physical effect
follows a gate. It is the reason the record is written before the number is returned rather
than after, and it is swept rather than argued:
`a_recovered_marker_is_whole_or_absent_at_every_crash_point` drives the gated workflow at
every point `waymaker-fault` enumerates and requires both halves — whole and absent — to be
reached, with `no_effect_the_branch_caused_survives_a_crash_the_marker_did_not` beside it.
Until that sweep existed the gate was the one boundary in this workspace with no crash
window of its own, which review found.

**Two kinds of "which version" now exist.** `recorded_version` and `gate` answer different
questions and a reader has to know which. Both are documented on `Boundary`, and the
reference workflow demonstrates all three ways of branching — two right and one wrong.

`waymaker-spec` does not model markers. Its ghost model carries one distinction, a schedule
or an outcome, and a self-resolving record is neither: it opens no boundary, so §14's six
guarantees are unchanged by it. `tests/refinement.rs` maps it to `None`, beside the timer
records. Nothing here weakens a guarantee; nothing here strengthens one either.

## Alternatives considered

**Keep the equality check and require `continue_as_new` across an upgrade.** A run would be
retired and restarted at the new version. It loses the run's history and its effect identity,
so every effect already performed would be performed again under a new `(RunId, EffectSeq)` —
which is the duplicate §14's fourth guarantee forbids. It is issue #95's problem arriving on
purpose.

**Record only the version, with no gate id.** Two bytes cheaper, and it cannot tell two gates
apart. Two gates that swapped places in a workflow keep every sequence, so the sequence check
does not see them and replay would take one gate's branch at the other's call site. The
`gate` field is what makes `Divergence::Gate` a thing the kernel can say;
`a_gate_renumbered_in_place_is_a_gate_divergence` is what makes it falsifiable.

**Give a marker no sequence, like the three run-scoped records.** It would be a note beside
history rather than a position in it, and §08's third rule would be unenforceable: a gate
could be added, removed or moved with every effect after it keeping its number, and no replay
would notice. The sequence is what makes "call-order sequencing remains authoritative" true
of gates.

**Let `VersionRequest` carry a proposed version separate from the range.** It would let one
boot record a branch its own image says it cannot replay, so every later boot of that run
would refuse it. The recorded branch is `VersionRange::current` and nothing else.

**Settle §16's `wire-format-migration` deferred question.** Its row says it settles "when the
version-marker record of §09 is implemented **and** a fleet with two format versions in it can
be described end to end". This is the first half. The second is about the frame's
`format_version` byte rather than about a workflow's version, and nothing here describes a
fleet running two of those — so the row stays `Open`, and this ADR deliberately claims none
of §16's questions.
