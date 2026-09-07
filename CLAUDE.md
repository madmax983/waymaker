# CLAUDE.md

Waymaker is a firmware-first durable workflow engine for Rust. A workflow is re-created from
its beginning after reboot and deterministically replayed through an ordered journal:
completed effects return their recorded results, and the first unresolved effect becomes the
next piece of work.

This file is what a contributor — human or agent — works to. It states the invariants, the
layering rules, and what each crate must not own.

Much of it is checked rather than remembered: the must-not-own cells, the permitted
dependency edges, the eight decision ids, the command list, the five deferred questions and
all 46 rule ids below are compared against the tables that own them, and `cargo xtask check-layering` fails a pull
request when this file and those tables stop agreeing. The rest is prose, and
[What is not checked](#what-is-not-checked) says which.

- The architecture, drawn: [`docs/architecture.md`](docs/architecture.md)
- Why things are the way they are: [`docs/adr`](docs/adr/README.md)
- The design document this is all taken from: [`docs/design/waymaker-design-v0.2.html`](docs/design/waymaker-design-v0.2.html)

## Run this before you claim anything works

Every command CI runs, in the order the stage table gives them. The `claude-md` rule
compares this list against `xtask::pipeline::STAGES`, so a stage added to the pipeline and
forgotten here fails the build:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo build --locked --workspace --no-default-features
cargo test --locked --workspace --no-default-features
cargo doc --locked --workspace --no-deps --no-default-features
cargo --locked xtask coverage
cargo build --locked --no-default-features --target thumbv6m-none-eabi
cargo build --locked -p waymaker-rig --no-default-features --lib --target thumbv6m-none-eabi
cargo build --locked -p waymaker-drive --no-default-features --lib --target thumbv6m-none-eabi
cargo clippy --locked -p waymaker-size-probe --target thumbv6m-none-eabi --features probe,facade --bins -- -D warnings
cargo --locked xtask size
cargo test --locked -p waymaker-spec --no-default-features
cargo --locked xtask check-layering
```

`cargo doc` needs `RUSTDOCFLAGS=-D warnings` to mean what it says — that is in the
workflow's `env:` block, and the `ci-pipeline` rule fails a build without it.

`cargo xtask install-hooks` points git at `.githooks`, which runs format, lint and test
before every commit — the three fast ones. The hook is *generated* from the same stage
table, so "the hook and CI run the same commands" is a fact about how the file is produced,
not a claim.

## The invariants

The eight decisions design document §02 settles. Each has a stable id, defined in
[ADR 0003](docs/adr/0003-the-eight-settled-design-decisions.md) and held as a table in
`xtask::docs::SETTLED_DECISIONS`. Cite the id when a change touches one.

| Id | Invariant |
| --- | --- |
| `kernel-is-dependency-free` | The kernel is `no_std`, `no_alloc`, and dependency-free. Serialization, logging, executors and drivers live above it. |
| `replay-is-sequential` | A cursor advances through history in workflow order. There is no `Journal::get(id)` and no in-memory event index. |
| `durable-intent-before-effect` | The schedule record crosses a durability barrier before dispatch. A physical effect never precedes its committed intent. |
| `numeric-kinds-and-borrowed-bytes` | Records are numeric kinds and borrowed bytes. Strings, `Vec`, Serde and Postcard are optional conveniences, never wire-format requirements. |
| `async-syntax-is-an-adapter` | `waymaker-embassy` supplies the ergonomic façade. The persistence protocol depends on neither Embassy nor `Future`. |
| `no-snapshotted-futures` | *Arbitrary* suspended futures are not snapshotted. History is reclaimed only at an explicit `continue_as_new` boundary. (§16 leaves a future explicit-state snapshot API open as a deferred question.) |
| `two-banks-for-atomic-replacement` | A new run becomes authoritative only after its payload and generation seal are durable. |
| `durable-timers-need-durable-time` | A resettable monotonic clock cannot claim that time elapsed while power was absent. Timer semantics match the hardware's actual clock. |

Two more that are not from §02 but hold everywhere:

- **No behavior ships without a test, and no invariant ships without something that fails a
  build over it.** A rule that can be broken silently is a comment.
- **A measurement that did not happen is not a measurement that passed.** Every gate fails
  closed: a missing tool, an unreadable report, a crate that contributed nothing, an
  unparseable input.

## What is still undecided

Design document §16 leaves five questions open, and issue
[#16](https://github.com/madmax983/waymaker/issues/16) is explicit about the deadline: "each
needs an answer before the wire format freezes at 1.0". They are held as a table in
`xtask::docs::DEFERRED_QUESTIONS`, so an open question is as checked as a settled one — the
`deferred-questions` rule compares that table against this section, and against the ADR
record in both directions.

All 5 deferred questions, with the id to cite when a change touches one:

| Id | Question | Where it stands |
| --- | --- | --- |
| `integrity-check-algorithm` | Whether the default integrity check is CRC32C or a smaller table-free CRC implementation. | [Settled by 0010-the-integrity-check-is-catalogued-and-table-free.md](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md): CRC-32/ISO-HDLC and CRC-16/CCITT-FALSE, both table-free. The polynomial turned out to be free on `thumbv6m` and the table not to be. |
| `retry-policy-placement` | Whether retry policy belongs in the Embassy façade or remains workflow code. | Open, owned by rung 0.4 · embassy. Settles when the dispatcher exists and the cost of a recorded retry representation can be measured against reimplementing backoff in every workflow. |
| `effect-scheduled-metadata` | How much input metadata an EffectScheduled record stores beyond length and digest. | [Settled by 0011-a-scheduled-effect-records-a-length-and-a-digest.md](docs/adr/0011-a-scheduled-effect-records-a-length-and-a-digest.md): `seq`, `kind`, `input_len`, `input_crc`, and nothing else. |
| `explicit-state-snapshots` | Whether a future explicit-state workflow API may support true storage snapshots. | Open, owned by after rung 1.0. Settles when a non-async, explicit-state API has been designed far enough that the snapshot it would take can be described in records, without relaxing the no-snapshotted-futures decision for the async façade. |
| `wire-format-migration` | How stable wire-format migration is performed after a deployed fleet outlives v1. | Open, owned by rung 1.0. Settles when the version-marker record of §09 is implemented and a fleet with two format versions in it can be described end to end. |

An ADR that answers one carries `Settles deferred question:` and the id; the row in
`DEFERRED_QUESTIONS` moves from `Open` to `Settled` in the same change. Writing the ADR
without moving the row, or moving the row without the ADR, each fail the build — which is
the point, because this is the list that normally rots in exactly those two ways.

Deciding one of these early is not a favour. An ADR written for a question whose
implementation does not exist is a snapshot of an opinion, which is the one thing
[the record](docs/adr/README.md) says a decision record must never be.

## The guarantees, and what holds each up

Design document §14 states five, and §02 decision 7 states the sixth. They are the reason
this project exists, so they are a table rather than a paragraph: `xtask::docs::SPEC_CLAUSES`
holds them, `crates/waymaker-spec` proves them, and the `recovery-spec` rule fails a build in
which this section, that table, the crate's own `obligation.rs` and
[ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md)
stop naming the same set.

All 6 recovery invariants, with the id to cite when a change touches one:

| Id | Guarantee | Discharged by |
| --- | --- | --- |
| `prefix-safety` | recovery exposes only a legal prefix of committed records | `tests/spine.rs`, exhaustively over every reachable state, and refined against the real `Scan` at every crash point |
| `acknowledged-durability` | any record acknowledged after its barrier is recovered after reset | `tests/spine.rs`; `tests/necessity.rs` shows which precondition it rests on |
| `durable-intent` | no Waymaker-dispatched effect lacks a recoverable schedule record | `tests/spine.rs`, with §02 decision 3 as a precondition rather than a hope |
| `single-authority` | exactly one bank is authoritative after any crash | `tests/spine.rs`, against the model alone — there is now a two-bank adapter to abstract (issue #22's `waymaker_flash::bank`) and `tests/refinement.rs` does not yet abstract it, so the refinement is owed against real code rather than against nothing |
| `stable-redelivery` | retries and reboot redelivery reuse the original effect identity | `tests/redelivery.rs`, over every resume point of a bounded run, against the real allocator |
| `bounded-decoding` | malformed storage cannot cause out-of-bounds reads or allocation | `tests/bounded_decoding.rs`, over a stated domain: every byte string to three bytes, every truncation, every single-byte mutation and coordinated pair of three real frames, and every payload length a header can declare |

Paths are relative to `crates/waymaker-spec`, and they are a CI stage of their own —
`cargo test --locked -p waymaker-spec --no-default-features`, in the `verification` job.
The workspace test stage runs them too; the separate job exists for the reason the
`layering` job does, which is that a §14 guarantee that stopped holding should be legible in
the checks list under a name that says which.

The proofs are bounded and say so: `Bound::PROOF`
travels in every result, reaching the state ceiling is an error rather than a truncation, and
`tests/census.rs` pins the reachable state count so that a machine which quietly shrank fails
a build rather than passing every proof about the part of it that is left.

A guarantee is only worth the evidence that it could have failed. Every one of these has a
falsifier: `tests/necessity.rs` removes each of the model's six preconditions in turn and
requires a named guarantee to break, and `tests/teeth.rs` runs a catalogue of readers that
are wrong in one way each and requires each to be caught by the guarantee it breaks.

Records carry one distinction and no more: a **schedule** or an **outcome**. The model is
otherwise incurious about content, which is what keeps it a model of the protocol rather than
of the codec — but §14's third guarantee says *schedule* record and means it, and without the
distinction an effect could be accounted for by an acknowledged completion, which is history
written after the world was changed. The same distinction carries §11's order: a schedule may
not be declared while an earlier one is unresolved, which is the rule
`waymaker_core::ReplayCursor` enforces when it refuses "a schedule while one is unresolved"
as malformed history.

## The storage contract, and what each sentence rests on

Design document §12 states a storage contract in five sentences, and issue
[#21](https://github.com/madmax983/waymaker/issues/21) asks for them to be "documented and
tested". `waymaker-flash` owns the contract — `Geometry` and `StableStorage`, with the trait's
public surface pinned by `storage-contract` — and `waymaker-conformance` is what any adapter is
run against. `xtask::docs::STORAGE_CONTRACT_CLAUSES` holds the table, the crate's own
`clause.rs` holds it again, and the `storage-conformance` rule fails a build in which this
section, that table, the crate and
[ADR 0016](docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md) stop
naming the same set.

The "Discharged by" column is the point. Three of §12's five sentences cannot be observed by a
suite running inside one process, and a suite that reported "all clauses covered" would be
reporting on the two it can.

All 6 storage-contract clauses, with the id to cite when a change touches one:

| Id | Sentence | Discharged by |
| --- | --- | --- |
| `interruptible-mutations` | `program` and `erase` may fail or be interrupted at any supported unit. | a crash injector, not a suite: `waymaker-fault` interrupts a write at every byte of every program and every block of every erase, and a driver that never fails satisfies "may fail" vacuously |
| `barrier-is-durable` | After `barrier` returns, all earlier successful mutations survive reset. | the across-reset witness: `durability::arm`, a reset the caller performs, then `durability::verify` |
| `barrier-orders-what-follows` | No later mutation may become durable before mutations ordered by a completed barrier. | the across-reset witness, by the same two calls — a write that is on media while the seal ordered before it is not |
| `validated-before-media` | The adapter validates erase/program alignment before touching media. | the in-process suite, in every case about what an adapter *refuses*, including the three that read the media back afterwards to see whether the refusal came first |
| `one-way-bits-are-the-drivers` | Flash-specific one-way bit programming rules remain the driver's responsibility. | the driver, not the protocol — named here so its absence from the suite is a decision rather than an oversight |
| `operations-act-on-what-they-name` | `read`, `program` and `erase` act on exactly the region they name, and `barrier` changes no media. | the in-process suite, in every case about what it does when it *agrees*. Not one of §12's five: it is `StableStorage`'s own documentation, and without it the suite would be a suite of refusals that never checked that a legal operation works |

The suite has been observed failing, which is the only thing that makes a passing run worth
anything: `crates/waymaker-conformance/tests/teeth.rs` runs adapters wrong in one way each and
requires the case that names each one to go red, with a control adapter required to pass. The
case each flaw must break is an exhaustive `match`, so a flaw added to the model and left out
of it does not compile. It is run against two real adapters — `waymaker_fault::Device`,
written for issue #18 and knowing nothing about this crate, and an `embedded-storage`
`NorFlash` through `NorFlashStorage` — which is issue #21's two "done when" bullets.

Two things the suite refuses to guess, because guessing is how a broken adapter talks a suite
out of testing it. Erased is `0xFF` — a constant, not something learned from the device under
test, since an adapter whose `erase` does nothing on media reading `0x00` would teach a
learning suite that nothing is programmable and that it had no questions to ask. And no case
names a byte outside the caller's region, not even in an operation it expects to be refused,
so an adapter that wrongly *accepted* one could only damage media the caller made expendable;
where no such operation exists the case says so rather than reaching somewhere unsafe.

"Without `embedded-storage` becoming a kernel dependency" is not a promise either. Every
layer's `may_depend_on_external` list in `xtask::policy::LAYERS` is empty, so the kernel
growing that dependency fails `kernel-zero-dependencies` and `waymaker-flash` growing it fails
`dependency-direction`.

## What the boards still owe

Design document §16's rung 0.2 exit criterion is issue
[#27](https://github.com/madmax983/waymaker/issues/27), and one bullet of it is not a thing
any amount of host-side work discharges: "power-cut loops pass on one Cortex-M0+ board and one
Cortex-M4 board". Nothing in this repository has ever run on a board.

That is a sentence a green CI would otherwise contradict, so it is a table.
`xtask::docs::HARDWARE_TARGETS` holds it and the `hardware-attestation` rule compares it
against this section in both directions — the same move
[the guarantees table](#the-guarantees-and-what-holds-each-up) makes for what
`waymaker-spec` still owes.

All 2 hardware targets, with the id to cite when a change touches one:

| Id | Target | Where it stands | What would discharge it |
| --- | --- | --- | --- |
| `cortex-m0plus` | power-cut and watchdog-reset loops on a Cortex-M0+ board | Not run | a rig log from a board, with the census complete and no breach. `waymaker-rig` is written to link on the target and has never been on one. The census completes on a host now, but against a model: no weak bits, no reset-cause register, no retained RAM, and a watchdog that lands at a call boundary rather than on a timer. |
| `cortex-m4` | power-cut and watchdog-reset loops on a Cortex-M4 board | Not run | the same log from a second core, because a rig that only ever ran on one part has measured that part rather than the protocol. |

Moving a row to `Passed` needs an accepted ADR carrying the attestation marker and the id, in
the same change; writing that line without moving the row fails the build too. Those are the
two ways this list would otherwise rot, and they are the two `deferred-questions` already
guards against for §16's open questions.

What *is* discharged is everything a host can discharge, and it is worth being exact about
which. `waymaker-rig` is driven at every crash point `waymaker-fault` enumerates — every byte
of every program, every block of every erase, before and after every barrier, and now a
watchdog reset before every operation, at every unit boundary and after every operation — and
its own oracle accepts every
one; **every cell of the census is filled**; and writers wrong
in one way each are required to be caught by the guarantee they break and by no other.
The dispatch cell is filled on evidence that execution entered the dispatcher rather than on
the mark that precedes it — a mark is not evidence of the thing it marks, and `Dispatched` is
deliberately written *before* the effect, so a power cut taking that mark's own commit barrier
leaves it whole on media with no effect behind it.
`a_dispatch_mark_is_not_evidence_that_the_dispatcher_ran` requires such runs to exist, so the
qualification cannot quietly stop qualifying.

The dispatch cell is the one worth explaining, because it does not fill where a reader would
look for it. A watchdog reset at the dispatch mark's own commit barrier never dispatches — that
barrier does not return under this cause — and a reset *inside* the next witness program is a
reset during that write rather than in the window, however much a torn mark makes it look like
one. The cell fills at the next operation's `Progress::None`: the barrier returned, the
dispatcher ran, and the core reset before the next program began, so nothing is half done and
the effect is out. That is the mirror of the power-cut cell, which fills at the barrier's own
`Whole`. `phase_of` earns both from the dispatcher having run *and* from nothing being in
flight, and `a_write_in_flight_is_not_the_dispatch_window` requires the misreadable runs to
exist so the second qualification cannot quietly stop qualifying.

At a *write* point, though, the watchdog cells are worth less than a reader would assume. At a
completed operation the causes can only diverge where something other than another storage call
follows, so at a schedule or a completion write they do not diverge at all: the media, the
ledger and the dispatch are the power-cut twin's, and only the cause the injector armed
differs. Those cells record that the cause was performed and that recovery survived it. That is
the strongest objection to this, so it is a test rather than a paragraph —
`the_two_causes_part_company_only_where_an_effect_follows_a_completed_call` measures the
coincidence at every operation and requires a divergence to exist somewhere.

What a host cannot supply is media that behaves like a part: the model starts erased and only
clears bits, its barrier is a no-op, a bit that programmed weakly is not a state it has, and a
real part may abort the unit in flight where this one finishes it. Nor a reset-cause register,
nor a watchdog that fires on a timer rather than at a call boundary. Nor retained RAM — a
watchdog reset really leaves it, and nothing here models it; what the rig measures instead is
the *cost* of trusting it, in
`a_witness_kept_in_ram_over_claims_by_one_mark_and_still_accuses_nobody` and the tooth beside
it, which is the half of it that can fail. §12's
`barrier-is-durable` and `barrier-orders-what-follows` are still `waymaker-conformance`'s
across-reset witness's, and still owed against a real driver.

## The failure matrix, row by row

Design document §14 states failure semantics as a table of ten rows, and issue
[#31](https://github.com/madmax983/waymaker/issues/31) is rung 0.3's exit criterion: each row
is a named test, and the matrix runs on the in-memory model and on the rig. The rows are
`waymaker_rig::matrix::Row`, `xtask::docs::FAILURE_ROWS` holds the table, and the
`failure-matrix` rule fails a build in which this section, that table, the rig's vocabulary,
the model's test file and
[ADR 0027](docs/adr/0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md)
stop naming the same set.

The model half is `crates/waymaker-drive/tests/matrix.rs`: one test per row, named after it,
and the rule reads the names out of the file. The rig half is
`crates/waymaker-rig/tests/matrix.rs`, which classifies every crash point the injector lists,
resumes the run with `Rig::resume`, and holds it to the row. The rig reaches six rows; the
"On the rig" column says which, and `the_rig_fills_six_rows_and_names_the_seventh_as_its_gap`
requires the rig's census to refuse rather than to stop at six.

All 10 failure rows, with the id to cite when a change touches one:

| Id | Failure point | Discharged on the model by | On the rig |
| --- | --- | --- | --- |
| `during-schedule-frame-write` | During schedule frame write | `during_schedule_frame_write_the_frame_is_ignored_and_the_activity_was_not_yet_dispatchable` | Swept |
| `after-schedule-barrier-before-dispatch` | After schedule barrier, before dispatch | `after_schedule_barrier_before_dispatch_the_stable_effect_id_is_redelivered` | Swept |
| `during-physical-activity` | During physical activity | `during_physical_activity_the_effect_is_redelivered_and_the_activity_tolerates_the_duplicate_attempt` | Swept |
| `after-activity-before-completion-barrier` | After physical activity, before completion barrier | `after_physical_activity_before_completion_barrier_the_same_id_is_redelivered` | Swept |
| `during-completion-write` | During completion write | `during_completion_write_the_torn_completion_is_ignored_and_no_partial_result_bytes_are_exposed` | Swept |
| `after-completion-barrier` | After completion barrier | `after_completion_barrier_the_completion_is_replayed_and_the_activity_never_runs_again` | Swept |
| `during-inactive-bank-erase-or-write` | During inactive-bank erase/write | `during_inactive_bank_erase_or_write_the_old_bank_remains_authoritative_and_the_old_run_continues` | Owed |
| `after-new-bank-seal-barrier` | After new bank seal barrier | `after_new_bank_seal_barrier_the_new_bank_is_authoritative_and_the_old_run_is_never_current` | Owed |
| `history-capacity-reached` | History capacity reached | `history_capacity_reached_is_a_capacity_error_with_no_mutation_or_an_explicit_continue_as_new` | Owed |
| `replay-divergence` | Replay divergence | `replay_divergence_is_a_deterministic_fault_with_no_further_execution_and_history_untouched` | Owed |

One row does not hold as §14 writes it, and the table says so rather than the test pretending.
Row 5 says "redeliver": a torn completion leaves no append point
([ADR 0018](docs/adr/0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)),
so the driver and the rig both refuse the bank, and the test asserts the refusal beside the two
halves that do hold — the torn completion is ignored and no partial bytes reach the workflow.
The run's continuation is §10's `continue_as_new`, a new run.

The four `Owed` rows are the rig's, not the model's: a swap workload, a capacity refusal and a
divergent replay are things this rig does not do. Moving one to `Swept` means the rig test
naming the row, which the rule reads, and the rig's census reaching it, which the pinned gap
test then has to be moved to match.

## The layering

`waymaker-embassy` → `waymaker-flash` → `waymaker-core`, and never the other way. The table
is `xtask::policy::LAYERS`; the diagram is
[here](docs/architecture.md#crate-dependency-flow). Adding a crate to the workspace means
adding a row to that table — a member no rule covers fails `workspace-membership`.

The "May depend on" column is `may_depend_on` in `policy::LAYERS`, rendered the way the
gate renders it; the `claude-md` rule compares the two.

| Crate | Owns | May depend on |
| --- | --- | --- |
| `waymaker-core` | Borrowed record views, effect identity, replay cursor, transition rules, capacity errors | nothing |
| `waymaker-flash` | Stable wire encoding, the integrity-check trait and its shipped binding, the storage contract and its geometry, CRC and seals, the commit seal and the two-barrier write discipline, the two-bank layout, bank selection, append scanning, storage-backed recovery and the append offset, the capacity reserve, the seven-step bank swap and `continue_as_new`, compaction transition | waymaker-core |
| `waymaker-embassy` | `Ctx`, activity futures, dispatcher, wakeups, optional typed codec helpers | waymaker-core, waymaker-flash |

### The must-not-own table

Design document §05. These strings are `must_not_own` in `xtask::policy::LAYERS` verbatim —
the `claude-md` rule fails a build in which this table and that one stop agreeing, so the
row you are reading is the string the gate reads.

| Crate | Must not own |
| --- | --- |
| `waymaker-core` | allocation, serialization framework, CRC, clock, storage driver, executor, logging |
| `waymaker-flash` | activities, workflow types, timers, Embassy |
| `waymaker-embassy` | on-media authority or hidden global state |

`waymaker-embassy` is the only crate permitted to know Embassy exists, and that permission is
`policy::EMBASSY_FACADE` plus `policy::EMBASSY_PREFIXES`, not a habit. A host or browser
adapter can be written later against the same semantic kernel; it must not expand the
firmware traits to accommodate host conveniences.

Seven crates are in the workspace and are *not* layers:

- `xtask` — host tooling, the gate itself. Kept out of firmware builds by `default-members`.
- `waymaker-size-probe` — firmware linked only so its section sizes can be measured. It
  declares all three layers as *optional* dependencies, on purpose — the baseline variant
  links none of them, which is what makes the code-flash budget a delta rather than an
  absolute — and nothing depends on it.
- `waymaker-fault` — the in-memory storage model and crash injector, `policy::TEST_SUPPORT_CRATES`.
  Host-side, `std`, no third-party dependencies, and outside `default-members`. It depends on
  `waymaker-flash` for the storage contract; no layer depends on it, in any dependency kind,
  and the only crates that do are `waymaker-spec`, `waymaker-rig` and `xtask` — the last
  because the write-amplification figure `cargo xtask size` publishes is measured by running
  the real writer over this model rather than transcribed. The tests that drive the harness
  live with the harness. See
  [ADR 0013](docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md).
- `waymaker-conformance` — the conformance suite for §12's storage contract and the
  `embedded-storage` port, also `policy::TEST_SUPPORT_CRATES`. Outside `default-members`, and
  nothing depends on it. Two things make it unlike the other two test-support crates, and both
  are deliberate: it is `#![no_std]` and allocation-free, because the adapter author it exists
  for may only be able to run it on the target the driver is for; and it carries a third-party
  dependency, `embedded-storage`, which outside `xtask` is allowed in this crate and — as a
  *dev*-dependency only, so that `waymaker-rig`'s `tests/port.rs` can drive a rig through this
  crate's port — in the rig. No layer may reach it in any dependency kind, which is
  `dependency-direction` and `kernel-zero-dependencies` rather than a convention. See
  [ADR 0016](docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md).
- `waymaker-rig` — design document §15's power-cut and watchdog-reset rig, also
  `policy::TEST_SUPPORT_CRATES`. The deterministic workload and cut plan, the durable witness
  of what the writer had done when the supply went, the wear meter, the oracle, the log
  line a violation is reproducible from, and issue #31's row vocabulary and census. Outside `default-members`, and nothing depends on it
  except `xtask`, which runs it to measure the write amplification it publishes. It is
  `#![no_std]` and allocation-free for a sharper reason than `waymaker-conformance`'s: a rig
  that could only run on a host would be a simulation wearing a rig's name, so the code that
  cuts the supply has to be code a board can link — which also means it may not keep what it
  knew in RAM, because RAM is the thing a power cut takes. See
  [ADR 0021](docs/adr/0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md).
- `waymaker-drive` — issue [#28](https://github.com/madmax983/waymaker/issues/28)'s
  synchronous driver for §06's explicit kernel boundary, also
  `policy::TEST_SUPPORT_CRATES`. The workflow's half of the boundary and the world's, the
  loop that joins `waymaker-flash`'s recovery scan and two-barrier writer to
  `waymaker-core`'s transition table, §07's seven-step effect protocol (issue
  [#29](https://github.com/madmax983/waymaker/issues/29)), and a reference workflow the
  firmware target builds. §07 is here rather than in `waymaker-flash` because step 4 is an
  activity, and that crate's must-not-own cell names activities — see
  [ADR 0025](docs/adr/0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md).
  Outside `default-members`, and nothing depends on it. It is the third member of this
  category that is `#![no_std]` and allocation-free, and the reason is the claim it exists to
  make: issue #28 asks for a workflow driven to completion with "no `Future`, no Embassy, and
  no allocation", and a driver that could only be built for the host would leave the last
  third of that unchecked — so the `drive-firmware` stage builds its library for
  `thumbv6m-none-eabi`. It is deliberately not `waymaker-embassy`: `Ctx`, the async
  dispatcher and wakeups are rung 0.4's, and a façade that contained the protocol would be
  the opposite of the thing #28 asks to be proved. See
  [ADR 0024](docs/adr/0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md).
- `waymaker-spec` — the formal specification of the recovery invariants, also
  `policy::TEST_SUPPORT_CRATES`. The ghost model of committed history, the journal and bank
  state machines, and the exhaustive search that discharges design document §14's guarantees
  over them. Host-side, outside `default-members`, and above `waymaker-fault` for the reason
  the harness is above the layers: an exhaustive state-space enumerator has no business in an
  8 KiB flash budget. See
  [ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md).

## Budgets

Design document §04. The first three live in `waymaker_core::budget` and are gated by
`cargo xtask size` — the numbers are in the kernel rather than in the gate, because a budget
in two places is a budget that ends up disagreeing with itself. The fourth,
persistent flash, no longer has no gate behind it: `bank::BankLayout::new` refuses a device
of fewer than two erase blocks, which is §04's "two erase blocks minimum" as a build-time
refusal rather than a sentence — though it is still not a *measurement*, because there is no
linked image with banks in it. Nothing compares the numbers in this table to `budget.rs`, so treat
`budget.rs` as the source if they ever differ.

| Budget | Target |
| --- | --- |
| Runtime RAM | ≤ 768 B with a 512 B scratch page (§04, v0.1) |
| Kernel state | ≤ 128 B, excluding any page buffer (§04, v0.1) |
| Incremental code flash | ≤ 18 KiB for core + flash adapter, on `thumbv6m-none-eabi` (§04 states 8 KiB as a **v0.1** target; [ADR 0017](docs/adr/0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md) raises it to 16 KiB for rung 0.2's two-bank lifecycle and [ADR 0020](docs/adr/0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md) to 18 KiB for §10's capacity reserve) |
| Persistent flash | two erase blocks minimum (§04, v0.1) |

The code-flash row is the one place this repository and the design document now disagree, and
the disagreement is deliberate rather than drift. `budget.rs` is what CI enforces; ADR 0017
records the measurement that moved it — 8180 B to 10976 B, of which 1484 B is
`waymaker-flash`'s bank layer and 1032 B is the size probe's own arithmetic — and says what is
owed. Issue #24's commit seal and two-barrier writer then took it to 14764 B against the same
16 KiB gate, and
[ADR 0019](docs/adr/0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md)
splits that: 2420 B is the seal reaching the codec and the two readers, and 1368 B is the
writer with the probe section that keeps it alive. Issue #25's capacity reserve then took it
to 16456 B, and
[ADR 0020](docs/adr/0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md)
splits *that* the same way — and says plainly that almost none of it is the module: some
tens of bytes are the library change measured on its own, and the rest is what the probe
drags in to reach twelve public functions. That is the budget conversation ADR 0019
deferred, and ADR 0020 has it: the gate goes to **18 KiB**, the second and — it argues — the
last raise that should happen before issue
[#72](https://github.com/madmax983/waymaker/issues/72) is fixed, because a raise argued from
a figure a third of which is the probe is a raise argued from the wrong number. Issue
[#26](https://github.com/madmax983/waymaker/issues/26)'s bank swap is to be measured against
a corrected figure rather than against a third raise, and it was: §10's seven-step swap
lands at **18098 B** against the same 18 KiB gate, with no raise asked for.
[ADR 0022](docs/adr/0022-the-bank-swap-is-a-typestate-and-step-one-is-a-value-being-consumed.md)
records what it took, because the first measurement was 42 B *over* — a plan carrying a
geometry the region beside it already held, and five steps taking that plan by value to
compare one field of it. Both were real defects, and with a trim of the probe's own
arithmetic beside them they are 444 B. A third of the
measured "core + flash adapter" figure is still the probe rather than either, which is a
defect in the measurement and is filed as issue
[#72](https://github.com/madmax983/waymaker/issues/72).

The workflow future is user memory and is reported separately. A kernel state type added to
`kernel_state_types!` is asserted at compile time, registered in the size report, and
counted in the total — it cannot be in one without being in the others.

## Writing code here

- `#![no_std]`, `#![forbid(unsafe_code)]` and `#![warn(missing_docs)]` in every firmware
  crate root; `#![warn(missing_docs)]` in every crate root, `xtask` and the probe included.
  `extern crate std;` and `extern crate alloc;` are rejected — an attribute is not a
  guarantee.
- No `unwrap()`, `expect()`, `panic!()` or indexing in production code. The workspace denies
  them; `clippy.toml` exempts test bodies, not helper functions in an integration test.
- Pedantic and nursery clippy are on, workspace-wide, via `[lints] workspace = true` in every
  member manifest.
- Public items need doc comments. `cargo doc` runs under `RUSTDOCFLAGS=-D warnings`, so a
  missing one — or a broken intra-doc link — fails the build.
- Coverage is gated per crate at 85% of lines, never as a workspace total: a total is exactly
  how an untested kernel hides behind a tested adapter.
- Errors: `thiserror` in libraries, `anyhow` in binaries — when either is reachable at all,
  which in the firmware crates it is not.

### Adding a gate rule

Rules live in `xtask/src/`, one module per subject, each a pure function over already-read
input so it can be tested against a workspace that does not exist. A new rule needs **five**
things, not the three the wiring test covers:

1. its id in `xtask::RULES`;
2. a `violations.extend(...)` line in `check_inputs`;
3. a row in the broken-workspace fixture — the wiring test fails if any of these three is
   missing;
4. a backticked row in [the rule table below](#what-the-gate-rejects), and the literal rule
   count in the sentence above it, which the `claude-md` rule compares against `RULES`;
5. a row in the README's rule table, which `the_readme_documents_every_rule_the_gate_declares`
   compares against `RULES`.

The last two are worth spelling out because they fail in different places: 4 fails
`check-layering` itself, and 5 fails `cargo test` while `check-layering` prints `ok`. A
contributor working from a three-item list gets a green gate and a red build.

### Changing the record representation, or a recovery guarantee

Issue [#20](https://github.com/madmax983/waymaker/issues/20) asks for a specific order, and
it is the opposite of the one that comes naturally: **the model and the invariants first,
then the proofs, then the code.** A representation changed first and modelled afterwards is a
model written to agree with what was already built, which is the one thing a specification
must not be.

1. `crates/waymaker-spec/src/model.rs` — the ghost state, the transition, its preconditions
   and its postconditions. If a precondition is new, it goes in `Guard` so it can be removed
   on its own.
2. `crates/waymaker-spec/src/invariant.rs` — what §14 now requires, if that changed.
3. The proofs. `tests/necessity.rs` needs a row for a new guard and `tests/teeth.rs` a row
   for a new wrong reader; both fail without one. `tests/census.rs` pins the reachable state
   count *and* the per-kind edge counts, and both are expected to move — the pin exists
   because the dangerous direction is a machine that silently shrank, not because the numbers
   are sacred.
4. `tests/refinement.rs` — the firmware has to still be a refinement of the model, at every
   crash point, or the model is now describing something else.
5. The code.

A guarantee added or removed is a row in `xtask::docs::SPEC_CLAUSES`, a row in
`crates/waymaker-spec/src/obligation.rs`, a row in
[the guarantees table](#the-guarantees-and-what-holds-each-up), and a line in
[ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md).
The `recovery-spec` rule fails a build in which those four disagree. Nothing can check the
*order* above; what it checks is that the four never drift, which is the part that fails
silently.

### Adding an ADR

Copy [`docs/adr/0000-template.md`](docs/adr/0000-template.md) to the next unused number, fill
it in, and add a row to [the index](docs/adr/README.md). A decision that is revisited gets a
new ADR naming what it supersedes; an accepted ADR is never edited to say something else.

### Adding a diagram

[`docs/architecture.md`](docs/architecture.md). Label the fence with
`<!-- diagram: some-id -->` on the line above it, and add a `DiagramSpec` row to
`xtask::docs::DIAGRAMS` naming the labels it must carry.

## What the gate rejects

All 46 rules `cargo xtask check-layering` can emit. The id is what appears in the failure, so
this table is how you find out what a red build is telling you.

### Layering

| Rule | Fires when |
| --- | --- |
| `dependency-direction` | A layer declares a dependency its row in `policy::LAYERS` does not allow. |
| `dependency-direction-transitive` | A layer *reaches* a crate it may not depend on, through another crate. |
| `kernel-zero-dependencies` | `waymaker-core` grows a dependency of any kind, in any table. |
| `kernel-owns-no-encoding` | A `waymaker-core` source converts between bytes and a value — `from_le_bytes` and its five siblings, or an `impl From<&[u8]>`/`TryFrom<&[u8]>`. `kernel-zero-dependencies` stops the kernel *importing* a serialization framework; this stops it *writing* one, which needs no dependency and no `pub`. A floor, not a proof: a hand-rolled shift-and-or loop is still a review question. |
| `replay-cursor-surface` | The replay cursor's public function surface differs from `source::REPLAY_SURFACE`, in either direction — a method added that nobody weighed against `replay-is-sequential`, or the module gone so the pin checks nothing. Absence is what issue #14's "no API requires random access by effect ID" asks for, and a method that does not exist cannot be caught by a test that calls it; pinning the surface makes adding `record_at(id)` a line a reviewer writes on purpose. |
| `effect-scheduled-fields` | `RecordRef::EffectScheduled` declares a field set other than `source::EFFECT_SCHEDULED_FIELDS`, in either direction — or the module is gone, so the pin checks nothing. [ADR 0011](docs/adr/0011-a-scheduled-effect-records-a-length-and-a-digest.md) settles §16's third deferred question at four fields and 24 bytes on media; a fifth is 17% more journal on every effect for the life of the format, and a fourth removed is a wire-format change on a record firmware in the field has already written. |
| `integrity-check` | `waymaker-flash`'s checksum module stops using one of `source::INTEGRITY_CHECK_PARAMETERS` — a polynomial or an initial value — the right number of times inside the function that owns it; or it or one of its submodules grows an array — a `const`, `static`, `type` alias or local — outside `#[cfg(test)]`; or it is gone, so the pin checks nothing. Or the *binding* drifts: `waymaker-flash/src/integrity.rs` is gone; the integrity trait or the shipped `impl` is renamed, missing, or declared twice — a decoy above the real one is what a first-match scan reads; a seal in `source::SEAL_BINDINGS` stops returning the width §09's frame spends on it; or the shipped method body is anything but one unqualified call to the function that owns its algorithm, `fast::crc32(bytes)` included. Or the *routing* drifts, in any of the four files that have one. In `waymaker-flash/src/frame.rs`: a body pinned by `source::SEALING_FUNCTIONS` stops computing the seals its row names exactly once, or the file names `crc16` or `crc32` anywhere outside `input_digest` — the one documented exception, because a `const fn` cannot go through a trait method — or `decode_with` and `frame_len_of_with` stop verifying a header through `verify_header_with`, or the scan's `next` stops walking with `decode_with`. The rows are *derived* rather than whitelisted: a function generic over the check that no row pins is a body that can compute a seal and is pinned by nothing, and the scan that finds them reads joined signatures and generic `impl` blocks, because a `where` clause and a method in `impl<C: IntegrityCheck>` each escaped a one-line scan. The same in `waymaker-flash/src/bank.rs`, whose five sealing bodies each reach the seals their row in `source::BANK_SEALING_FUNCTIONS` names. And in `waymaker-flash/src/append.rs`, which is the writer: its `stage` must reach the codec through `frame::encode_with::<C>` — one call covers both the frame and its commit seal, because the seal is derived from the check the codec just computed — and it may name neither a checksum function nor a seal method. Without it, `frame::encode` in place of the generic sibling would seal every appended record with the shipped check whatever the recovery that positioned the writer verified with, which is a journal one half of a firmware can read. And in `waymaker-flash/src/recovery.rs`, which computes no seal at all: its two steps must reach the codec through `frame::decode_with::<C>` and `frame::frame_len_of_with::<C>`, and the file may name neither a checksum function nor a seal method — `Recovery<C>`'s parameter is a promise that a journal is verified with the algorithm that sealed it, and dropping both turbofishes passed every rule and every test before this existed. And in `waymaker-flash/src/swap.rs`, which installs a bank: its `stage` must reach the bank codec through `bank::encode_header_with::<C>`, `bank::seal_for_with::<C>` and `bank::encode_seal_with::<C>`, and the file may name neither a checksum function nor a seal method — a device whose two banks were sealed by two algorithms is a device only half of which boots. A trait nothing is obliged to call is a swap point that selects nothing. A firmware that sealed its banks with one algorithm and its records with another could read back neither half with the other's reader. [ADR 0012](docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md), and one rule id because it is one decision. [ADR 0010](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md) settles §16's first deferred question with measurements: the polynomial is free (52 B either way), the table is not (64 B for a nibble table, 1024 B for a byte table against an 8 KiB budget). A changed polynomial passes every round-trip test here and fails against every zlib in the world. |
| `storage-contract` | The public function surface of `waymaker-flash`'s storage module differs from `source::STORAGE_CONTRACT_SURFACE`, in either direction — or the module is gone, so the pin checks nothing. Design document §05 says a host or browser adapter "must not expand the firmware traits to accommodate host conveniences", and §12 is the trait it means: a `read_all`, a `flush`, a `write_at` or a `capacity()` shortcut would each break no layering rule, need no dependency, and turn a four-operation contract every port must implement into a surface only a host can afford. The pin compares names, so a widened offset or a validator that stopped validating is still a reviewer's job. |
| `recovery-surface` | The storage-backed recovery reader's public function surface differs from `source::RECOVERY_SURFACE`, in either direction — or the module is gone, so the pin checks nothing. §02 decision 2's "no `Journal::get(id)` and no in-memory event index" is a rule about the reader that touches media as much as about the cursor: a `seek`, a `resume_at` or a `read_all` would each break no layering rule and turn a forward scan whose RAM is one caller-owned page into one that seeks or holds history. One name is load-bearing for a second reason. `append_offset` is the only way an offset leaves the module and it answers `Some` only for a scan that ran to erased media; a second accessor returning the stopping offset regardless points at cells a program cycle has already cleared, and on NOR that bank never boots again. `waymaker-fault`'s sweep demonstrates that mutation rather than arguing it. |
| `commit-discipline` | The two-barrier writer's public function surface differs from `source::APPEND_SURFACE`; or the typestate that makes design document §07's order unrepresentable comes apart — the staged frame grows a second method or the word `program`, the sealable frame grows anything but `commit`, the sealable frame is constructed anywhere but inside `payload_barrier`, or that barrier stops calling `storage.barrier` exactly once. Issue [#24](https://github.com/madmax983/waymaker/issues/24) asks that "it is not possible to program a seal without the intervening payload barrier having returned", and a `compile_fail` doctest in the crate proves that of the code as it stands. This is what stops it being given back: a `Staged::commit`, a `Journal::write` that did all four steps in one call, or a second constructor for `Sealable` would each break no other rule and turn a protocol into a convention. What it cannot see is whether the barrier is a real one — that is §12's contract and `waymaker-conformance`'s across-reset witness. |
| `capacity-reserve` | §10's capacity reserve gains a public function `source::CAPACITY_SURFACE` does not list, in either direction — or the gate comes apart: `source::CAPACITY_GATE` declares no inherent `impl`, declares `stage` other than exactly once, or its `stage` does not *open* with `source::CAPACITY_ADMISSION_CALL` and go on to `source::CAPACITY_DELEGATION`. §10 says "the runtime never overwrites committed history to make room", and every way of giving that back is an *addition*: a `Reserved::stage_unchecked`, a `Reserved::into_journal` handing the ungated writer back, a `Reserve::none()`, or a `Reserve::for_bytes(tail)` taking the figure from its caller rather than from a `BankLayout` — which is the sharpest of the four, because a reserve is only a promise because a layout vouched for it. The order half is the other word §10 uses: scheduling fails **early**, and issue #25 asks that the failure "produce no mutation at all". §12 says a failed program may still have changed media, so the only refusal that changes nothing is one taken before the device is called. The decision must therefore be the body's **first** statement, not merely one that precedes the delegation — review of this change wrote an admission inside `if false`, inside a closure nobody calls, and guarded so that only `RunStarted` reached it, and watched a rule that only checked the order stay green on all three. The blocks are read for the named type rather than by finding the first `fn stage` in the file, because the surface half counts only *public* functions and a private decoy carrying the pinned call stood in for the real one. What it cannot see is the arithmetic: a `tail_bytes` that quietly stopped counting the outcome record is `crates/waymaker-flash/tests/capacity.rs`'s, where `a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding` drives the wrong reserve and watches a run reach a state it can never leave. |
| `swap-discipline` | §10's bank swap gains a public function `source::SWAP_SURFACE` does not list, in either direction — or its step order comes apart: a state in `source::SWAP_TYPESTATE` declares anything but the one method its row names, `Staged` names `program`, a value in `source::SWAP_CONSTRUCTIONS` is built anywhere but inside the body its row names, `payload_barrier` stops taking `source::SWAP_BARRIER_CALL`, or a row of `source::SWAP_ERASE_CALLS` stops erasing exactly the bank it names, before a barrier, without naming the other one. Issue [#26](https://github.com/madmax983/waymaker/issues/26) states §10 as seven steps and two recovery rules — "a crash before step 5 recovers the old run, a crash after step 6 recovers the new run" — and every one of those is a statement about *where the barriers are*. A `Prepared::commit` skipping the header, a `Staged::seal_now` skipping the payload barrier, an `Installed` built anywhere but in `commit`, or a `Swap::install(bank)` taking the bank to erase from its caller would each break no other rule and turn a protocol into a convention. The erase rows are the sharpest: which bank a swap clears is derived from the authority the device booted, and a `prepare` that erased the *retiring* bank is a device clearing the run it is executing. What it cannot see is whether the barriers are real, which is §12's contract and `waymaker-conformance`'s across-reset witness, nor whether the crash windows behave — that is `crates/waymaker-fault/tests/swap.rs`, at every crash point of all seven steps. |
| `rig-oracle` | `waymaker-rig`'s oracle or its census gains a public function `source::RIG_AUDIT_SURFACE` or `source::RIG_CENSUS_SURFACE` does not list, in either direction — or either file is gone, so the pin checks nothing. A rig is the one piece of code here whose bugs are *invisible*: a firmware bug shows up as a failing test, a rig bug as a passing one. Every way of giving the instrument back is an addition — an `Audit::assume_passed`, an `Audit::ignore`, a `Breach::suppress`, a second `finish` taking the authority count as advisory, a `Coverage::force_complete`, a `Gap::ignore` — and each would break no other rule, need no dependency and pass every test that exists. The census is a file of its own rather than part of `phase.rs` for this rule's sake: `Phase` and `ResetCause` each declare an `index`, a `from_index` and a `name`, and a pin that compares names cannot tell two such declarations apart. What it cannot see is whether the oracle's arithmetic is right — `crates/waymaker-rig/tests/teeth.rs` is what holds that, with two writers wrong in one way each and a control writer required to pass. |
| `transition-surface` | The replay machine's public function surface differs from `source::TRANSITION_SURFACE`, in either direction. Issue #15 asks for divergence that is "terminal and loud: no reinterpretation of history, no best-effort recovery", and every word of that is an *absence*: a `reset`, a `clear_divergence`, a `force` flag on `intent` would each break no other rule and turn "stop, never guess" into a suggestion. A test cannot call a function that is not there, so the surface is pinned instead. |
| `kernel-boundary` | Design document §06's kernel boundary stops being the one that was reviewed, in either half. The *shape* half: a type in `source::BOUNDARY_TYPES` — `EffectRequest`, `Intent`, `Resolve`, `Outcome`, `Next` — declares a member the pin does not have, or stops declaring one it does, or is gone so the pin checks nothing. Issue [#28](https://github.com/madmax983/waymaker/issues/28) asks that "adding a new record kind does not change this signature", and §09 numbers eleven record kinds of which five — `TIMER_SCHEDULED`, `TIMER_FIRED`, `VERSION_MARKER`, `SIGNAL_RECEIVED`, `CHILD_STARTED` — have no body yet. A `Resolve::TimerFired` arriving with the first of them would break no other rule, need no dependency, and turn one boundary into a boundary per record. The *routing* half: `waymaker-drive`'s driver stops naming a row of `source::BOUNDARY_DECISIONS`, or grows one of `source::DRIVER_FORBIDDEN_VOCABULARY` — `RecordKind`, `Step` or `EffectIdAllocator`, matched as *identifiers*, because a `Step::` spelling ban is evaded by `Step ::Record` and by `use …::Step as S;` and fires on an unrelated `BootStep::`. A driver that decided from a record rather than from `Intent` and `Resolve` would be a second transition table, and the one below it would no longer be where §08 is enforced. The allocator is issue [#30](https://github.com/madmax983/waymaker/issues/30)'s: §14's fourth guarantee is that a retry and a reboot redeliver the *original* identity, and the driver keeps it by never having an identity of its own — every `(RunId, EffectSeq)` it dispatches under comes from `Intent::Schedule` or `Resolve::Redeliver`, and a fresh mint for an outstanding effect is a second effect to every downstream system. `RecordRef` is not on the list because the driver constructs them — the kernel names the record it wants written and something has to write it — and it reads two, which [what is not checked](#what-is-not-checked) names rather than leaves implied. Both halves read the file with its `#[cfg(test)]` modules removed, for `integrity-check`'s reason: a decision named only under `cfg(test)` discharges nothing about the code that ships. A type declared *twice* fails too — `braced_body` reads the first declaration, so a decoy above the real one is what a first-match scan reads. One rule id because it is one decision. What it cannot see is a *widened* member behind a name already on the list, and a driver that names every decision and then ignores one; `crates/waymaker-drive/tests/` is what holds the behaviour. [ADR 0024](docs/adr/0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md). |
| `effect-protocol` | Design document §07's seven-step effect protocol stops being the one that was reviewed. `waymaker-drive/src/effect.rs` gains or loses a public function `source::EFFECT_PROTOCOL_SURFACE` lists; the state in `source::EFFECT_DISPATCH_STATE` declares anything but the two methods its row names; a type in `source::EFFECT_TYPE_METHODS` is declared twice, stops being a braced struct, declares a public field, or declares a method set other than its row's — read at *every* visibility, because a surface pin counts `pub ` and not `pub(`; a type in `source::EFFECT_NO_SELF_LITERAL` builds a `Self` or implements a trait; the file declares a module; a value in `source::EFFECT_CONSTRUCTIONS` is built outside the two bodies its row names, or is not built inside each of them; a body in `source::EFFECT_STEP_BODIES` — located in its owning type's own `impl` blocks — stops taking each of `source::EFFECT_STEPS` exactly once, in that order, and at the body's own nesting depth — braces, parentheses and brackets together — or declares a closure or a short-circuit (`|`, `&&`); `source::EFFECT_PROOF_AFTER`'s body builds a proof before the step its row names; or `redelivering` names one of `source::EFFECT_REDELIVERY_FORBIDDEN`. Issue [#29](https://github.com/madmax983/waymaker/issues/29) asks that step 4 be unreachable without step 3, "structurally, not by review", and every way of giving that back is an *addition*: a `DurableIntent::new`, a public `id` field on it, an `Effect::dispatchable_now`, a `Dispatchable::into_writer`, or a `Resolution::outcome` a caller can call before step 7. Each would break no other rule, need no dependency, and turn §02 decision 3 back into a convention. The step rows are the other half: §07 states the frame, the payload barrier and the seal twice, and a body that takes them in another order is not that protocol. Six of the halves are things review demonstrated rather than things anybody predicted, and each was watched passing on a mutation before it was closed: a `pub(crate) const fn new(id) -> Self` on `DurableIntent`, wired into the driver, with the gate green; a `Self { .. }` the name-based construction pin cannot see; a private free `fn resolve` above the real one, taking all three steps while `Dispatchable::resolve` stopped at the payload barrier; a decoy `pub struct` above the real one; a `pub` tuple field, where `braced_body` reads the first `{` after a declaration and so reported on the `impl` block below; and an `impl` inside a nested module, which `inherent_impl_bodies` cannot see because it reads `impl` at column zero. Codex round 1 found the seventh: the construction pin and the order pin were independent, so an early `return` carrying a freshly built `Dispatchable` satisfied both. Codex round 2 found the eighth, which is the sharpest of the lot: brace depth is not execution, and `false.then(|| self.writer.stage(..).payload_barrier(..).commit(..))` has no braces at all — three pinned calls, in order, at brace depth zero, in a closure nothing runs. The depth counts parentheses and brackets now, and a step body may not declare a closure. Round 3 found the ninth in the same family — `false && self.writer.stage(..)?…` puts every call once, in order, at depth zero, on a right-hand side that never runs — so the two short-circuit operators are refused as well. A scanner cannot follow control flow, so what it does instead is refuse the constructs that create it, and [what is not checked](#what-is-not-checked) says which. Round 2's other finding did not reproduce: `public_functions` counts a trait `impl`'s method as callable, so the surface pin already rejected an `impl From<EffectId> for DurableIntent`; the direct refusal is here anyway, because the two pins that are *about* construction do go blind on a trait `impl` and a guarantee should not rest on another pin's side effect. Read with `#[cfg(test)]` modules removed, for `integrity-check`'s reason. What it cannot see is a step added from another file — it pins one file, exactly as `capacity-reserve` and `recovery-surface` do — nor whether the barriers are real, which is §12's contract and `waymaker-conformance`'s across-reset witness; the crash windows are `crates/waymaker-drive/tests/crash.rs`. [ADR 0025](docs/adr/0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md). |
| `embassy-below-facade` | A *layer* other than `waymaker-embassy` reaches the Embassy ecosystem. The rule iterates `policy::LAYERS`, so `xtask` and the size probe are outside it. |
| `layer-missing` | A crate named in `policy::LAYERS` is not in the workspace. |
| `layer-not-local` | A crate with a layer's name resolves to a registry crate rather than the path dependency. |
| `workspace-membership` | A workspace member is neither a layer, declared host tooling, a measurement crate, nor declared test support. |
| `inputs-incomplete` | A crate is in the graph but contributed no manifest, or a workspace member contributed no crate root, so rules silently skipped it. |

### Crates and manifests

| Rule | Fires when |
| --- | --- |
| `crate-attributes` | A firmware crate root loses `#![no_std]` or `#![forbid(unsafe_code)]` or declares `extern crate std/alloc`; or any crate the layering covers — the three layers and the test-support crates — loses `#![forbid(unsafe_code)]` or allows unsafe code; or one of `policy::NO_STD_TEST_SUPPORT_CRATES` loses `#![no_std]` or declares `extern crate std/alloc`. Most test-support crates are host code, so `#![no_std]` is not asked of them; an unreviewed `unsafe` block in the harness the layers are tested against is. The three that *claim* `#![no_std]` are held to it here rather than by their firmware-target build stages, which cannot hold the allocation half: `cargo build --lib` produces an rlib and never links, so no global allocator is required and an `extern crate alloc` under any of them compiles clean. |
| `empty-default-features` | A layer's or a test-support crate's `default` feature enables anything, so an optional cost stops being opt-in. |
| `no-build-scripts` | A layer or a test-support crate grows a `build.rs`. |
| `member-manifest` | A layer's or a test-support crate's manifest drops `[lints] workspace = true`, declares a non-empty `default` feature, opts out of its own test binary with `[lib] test = false` — which would make an untested crate report "no coverable lines" and pass the coverage gate — or, for the kernel, grows a dependency table. |
| `workspace-lints` | The workspace lint table drifts from what this project requires (`manifest::REQUIRED_CLIPPY_GROUPS` and `REQUIRED_CLIPPY_DENIALS`). The design document says nothing about lints; only the release profile comes from §04. |
| `release-profile` | `[profile.release]` drifts from the size settings the budgets are measured against. |
| `cargo-config-profile` | `.cargo/config.toml` is missing, rewrites the `xtask` alias or the profile, declares an `[env]` key, or sets `[build] rustflags` — each of which turns a gate into a command that exits zero. |

### Pipeline and measurement

| Rule | Fires when |
| --- | --- |
| `ci-pipeline` | The workflow drops a stage, reorders one within a job, or makes one unable to fail — an `if:`, a `continue-on-error:`, a missing `RUSTDOCFLAGS`, an `on:` block no pull request triggers, a job with no `runs-on:`, or a tab in the indentation. |
| `pre-commit-hook` | `.githooks/pre-commit` is missing, not executable, or not byte-for-byte what the stage table renders. |
| `toolchain-targets` | `rust-toolchain.toml` stops pinning `thumbv6m-none-eabi` or `llvm-tools-preview`. |
| `size-probe` | The size probe stops being the `#![no_std]`, `#![no_main]`, feature-gated firmware the size gate links. |
| `size-probe-reach` | A layer grows a public function the probe does not reach, so no budget charges for it. |
| `gate-broken` | The gate's own expected values do not parse. A gate must not be able to silently uncheck one of its rules. |

### Documentation

| Rule | Fires when |
| --- | --- |
| `claude-md` | This file loses a must-not-own cell, a permitted dependency edge, a settled-decision id, a backticked gate rule id, a pipeline command, or its links to the decision record and the diagrams. |
| `recovery-spec` | The recovery specification and the four places it lives stop agreeing: a clause in `docs::SPEC_CLAUSES` is missing from this file, from [ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md), or from `crates/waymaker-spec/src/obligation.rs`; its row here does not carry the guarantee's words or the test target that discharges it; the count is wrong; the crate declares a clause the table never did; or the clause table is not where the gate looks for it. Issue #20 asks that a change to the record representation update the model and the invariants first, then the proofs, then the code. Nothing mechanical can check the *order* — this checks that the four never disagree, which is the part that fails silently. |
| `storage-conformance` | Design document §12's storage contract and the four places it lives stop agreeing: a clause in `docs::STORAGE_CONTRACT_CLAUSES` is missing from this file, from [ADR 0016](docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md), or from `crates/waymaker-conformance/src/clause.rs`; its row here does not carry the sentence or what discharges it; the count is wrong; the crate discharges a clause differently than the table does; the crate declares a clause the table never did; or the clause table is not where the gate looks for it. Two tables agreeing on the names of six things and disagreeing about what any of them costs is the failure worth catching, so ids and discharges are compared in both directions. What it cannot see is inside the crate: that a clause the table calls in-process is reached by a case is `crates/waymaker-conformance/tests/clauses.rs`. |
| `hardware-attestation` | Rung 0.2's board runs and the places they are recorded stop agreeing: a target in `docs::HARDWARE_TARGETS` has no backticked table row in this file, its row does not carry the headline or the status the table renders, the count is wrong, a target marked `Passed` has no accepted ADR carrying `docs::HARDWARE_ATTESTATION_MARKER` for it or has more than one, a target marked `Not run` is nevertheless claimed by an ADR, or an ADR attests a target the table never declared. What it cannot check is that a `Passed` row is *true* — the evidence is a log from a bench — only that the claim is a line in an accepted decision record rather than a status somebody flipped. |
| `failure-matrix` | Design document §14's failure-semantics table and the five places it lives stop agreeing: a row in `docs::FAILURE_ROWS` is missing from this file, from [ADR 0027](docs/adr/0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md), from `crates/waymaker-rig/src/matrix.rs`'s `Row::id`, or has no `#[test]` of its own name in `crates/waymaker-drive/tests/matrix.rs`; its row here does not carry the failure point, the test or the rig standing the table renders; the count is wrong; a row the table calls swept is never named in `crates/waymaker-rig/tests/matrix.rs`; the rig declares a row the table never did; or one of the three files is not where the gate looks for it. What it cannot see is whether a named test asserts the row's *behaviour* — that is each file's own census, which requires every row to be reached on the model and the rig's to refuse at the first owed row. |
| `adr-numbering` | An ADR skips or reuses a number, is not named `NNNN-slug.md`, or the record has no template. |
| `adr-structure` | An ADR loses its title, `- Status:`, `- Date:`, `## Context`, `## Decision` or `## Consequences`, or carries an unrecognised status. |
| `adr-index` | An ADR is not linked from `docs/adr/README.md`, or the index links one that does not exist. |
| `settled-decisions` | The §02 ADR stops recording one of the eight decisions, or its headline. |
| `deferred-questions` | A question in `docs::DEFERRED_QUESTIONS` is missing from this file, its row does not carry the headline and the status the table renders, the count is wrong, a settled one's ADR is absent, unaccepted or does not carry its `Settles deferred question:` marker, two ADRs claim one question, an open one is already claimed by an ADR, or an ADR claims a question the table never declared. |
| `diagrams` | `docs/architecture.md` loses a labelled Mermaid block, a protocol step, a layer, or a permitted dependency edge — or draws an edge the layering does not permit, or labels two blocks with one id. |
| `missing-docs` | A crate root stops warning, denying or forbidding `missing_docs`, or turns it back off — `allow`, `expect`, the `warnings` group, a `cfg_attr` wrapper, or an attribute split over several lines are all the same regression. |

## What is not checked

Stated so that nobody mistakes silence for coverage:

- **Prose.** The rules match ids, crate names, `must_not_own` cells, permitted edges,
  pipeline commands and protocol steps. The sentences around them are reviewed by people —
  deliberately, so they stay free to be rewritten. A row that says the opposite of what it
  means will pass as long as the anchor is in it.
- **The "Owns" column above, and the budget numbers.** Both are transcribed from the design
  document and from `waymaker_core::budget`, and nothing compares them. `budget.rs` is the
  source of truth for the numbers.
- **That the recovery state machine diagram matches the model.** `diagrams` checks that
  `docs/architecture.md`'s `recovery-state-machine` block carries every state and every
  precondition `docs::RECOVERY_STATE_MACHINE_LABELS` names. It does not check that the
  *arrows* between them are the ones `waymaker-spec` admits — that is `tests/machine.rs`,
  over every edge of the enumerated machine, and the picture is the picture.
- **That a diagram renders.** The Mermaid check is a text scan — it proves every layer,
  every permitted edge and every step is in the right block, and that no edge contradicts
  the layering. It does not run Mermaid. The pull request preview is the render.
- **An unlabelled diagram.** Only blocks carrying a `<!-- diagram: ... -->` label are
  checked. Every block in `docs/architecture.md` carries one today; a new block without one
  would be illustration that nothing keeps honest.
- **That a new §02-style decision was added to `SETTLED_DECISIONS`.** The table is the spec;
  nothing detects a ninth decision nobody wrote down.
- **That §16 still lists exactly the five questions `DEFERRED_QUESTIONS` holds.** Same shape
  as the line above, and the same reason: the design document is a checked-in HTML file, the
  table is the spec, and a sixth deferred question is a row somebody writes.
- **That an ADR settling a question says so.** `deferred-questions` reads the
  `Settles deferred question:` marker, so an ADR that answers an open question without
  writing the line leaves the row unchallenged. The marker is what makes the check possible
  in the first place — "this ADR is about the integrity check" is a judgement about prose —
  so the rule catches a stale table only when the ADR's author cooperates.
- **The width of a pinned field, or the size of a record on media.**
  `effect-scheduled-fields` compares *names*. ADR 0011's actual claim is 24 bytes per
  scheduled effect, and widening an existing field rather than adding one is invisible to
  it; `waymaker-flash`'s frame tests are what hold the layout.
- **That a fault the harness models is a fault the hardware has.** `waymaker-fault` models
  NOR flash — erased is `0xFF`, programming only clears bits, an operation the geometry
  forbids never reaches media — and a model wrong in the same direction as the code it
  tests would agree with it. §15's hardware half, "run hardware power-cut loops against real
  NOR flash", is owed at rung 0.2, where the boards are.
- **That a watchdog reset behaves the way this part's does.** `Interruption::Watchdog` says
  the controller finishes the unit in flight and the call never returns. Both are choices, and
  the first is the optimistic one: a part that *aborts* the unit leaves a partial program a
  brownout would leave, which is a world the power-cut half already sweeps, so the model errs
  toward the world the sweep would otherwise miss. What no model supplies is the reset-cause
  register, retained RAM, and a watchdog that fires on a timer rather than at a call boundary
  — the last of which is why the dispatch-window cell is a board's. See
  [ADR 0023](docs/adr/0023-a-watchdog-reset-is-modelled-and-its-difference-is-one-return.md).
- **A lookup table outside the checksum module.** `integrity-check`'s table scan reads
  `waymaker-flash/src/crc.rs` and the modules it is split into, so a table in a sibling
  module that `crc.rs` calls is out of its scope.
- **That a seal a body computes is the seal it stores.** `integrity-check`'s binding and
  routing halves are scanners, and three rounds of review on pull request #60 spent
  themselves on the same seam: a scanner cannot resolve a name or trace a value. What they
  now hold is the *shape* — one unqualified call in a delegation, an unaliased depth-zero
  import of it, no local definition shadowing it, and an invocation of `C::header_check` /
  `C::frame_check` whose answer is used where it is computed. What is left uncovered is a
  named binding read by something other than the expression that stores the seal, and the
  reason that is tolerable is that it is not silent: `unused_variables` plus `-D warnings`
  fails CI on a binding nothing reads, which is why an *underscore-prefixed* binding is
  refused and a plain one is not. `waymaker-flash`'s golden frames and
  `tests/integrity.rs` are what hold the behaviour; these rules hold what a reviewer can
  check by eye.
- **`[lints] workspace = true` in `xtask` and the size probe.** The manifest and crate-root
  rules iterate `policy::checked_members` — the three layers plus the test-support crates —
  so those two are outside them. The size probe has `size-probe` of its own; `xtask` is the
  gate.
- **A public function added to the recovery reader from another file.** `recovery-surface`
  pins one file, exactly as `storage-contract` does. An `impl Recovery { pub fn seek(..) }` in
  a sibling module adds the method with the rule silent, and so would a
  `trait RecoveryExt: ...` with a blanket impl. The same shape as the two bullets around this
  one, and stated for the same reason.
- **That the header length is computed by the check the caller chose.** The
  `integrity-check` routing pin requires `stage` to call `frame::frame_len_of_with::<C>`, and
  no test can observe the difference: a recovery whose header length came from the *shipped*
  check still refuses a foreign journal, because `decode_with::<C>` verifies the same header
  a moment later and stops at the same frame. What the swap point buys there is a stride
  computed from a length the caller's algorithm vouched for, which only diverges on a
  colliding header seal — one in 2^16. The pin is the whole of the check, and
  `crates/waymaker-flash/tests/recovery.rs` says so where it tests the other half.
- **A host convenience added to the storage contract from another file.** `storage-contract`
  pins one file. A `trait StorageExt: StableStorage { fn read_all(..) }` with a blanket impl
  in a sibling module adds a method to every port's type with the rule silent, the same way
  `integrity-check`'s table scan cannot see a table in a module `crc.rs` calls.
- **Crash points of operations that exist only after an injected failure.** `injections` is
  computed from the *fault-free* write sequence, so a retry a writer performs only because a
  call failed has no crash points of its own — it is never torn, interrupted or power-lost
  in any run. Everything before the injection point is identical by construction, which is
  what makes the enumeration exact for that sequence; it is not a fixpoint over the
  sequences a reacting writer can produce.
- **What the ghost model does not have dimensions for.** The bound is *not* the binding
  constraint, and saying only "closed within `Bound::PROOF`" would invite the wrong reading:
  raising it to four or five records changes no verdict, because the shapes of history it
  admits are one-dimensional. What is missing is expressiveness, and three gaps are worth
  naming. The model's **banks hold no records** — no transition changes a bank and a record
  at once — so §14's "never recover the old run as current" is not a statement it can make.
  There is **no reboot**: `recover()` is applied to a state the power has left, and nothing
  consumes a recovered history and carries on, so a second boot and a third are outside the
  machine. And **a writer that retries** after a failed program or a failed erase is not
  describable, because an append-only journal with a half-written record in it cannot
  advance and compaction is rung 0.2's. Each is recorded in `obligation.rs`'s `owed` column
  and in
  [ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md).
- **Every malformed input.** `bounded-decoding` sweeps a domain it states: exhaustive to
  three bytes, every truncation, every single-byte mutation, coordinated pairs over an
  eight-value corruption alphabet, every declared payload length, and a generated set of scan
  layouts. A bug needing three corrupt fields at once, or two outside that alphabet, is
  outside it. The `owed` column in `obligation.rs` is where that is written down.
- **That a named proof contains a proof.** `recovery-spec` checks that a clause's proof file
  is the one both tables name; `tests/obligations.rs` checks that the file exists and holds
  at least two tests. Neither reads what those tests assert, and neither can.
- **That the crash oracle is as strict as the specification.** `waymaker_fault::verify_oracle`
  compares a recovery against *committed* history, which filters out records that never
  reached media, so it accepts a history that skips a gap and carries on; the specification's
  prefix safety is a prefix of *declaration order* and refuses it. The two agree over the
  specified machine — no reachable state has a gap — and `tests/oracle.rs` measures where
  they stop agreeing rather than leaving the difference implied.
- **That the ghost model is a model of *this* firmware.** `tests/refinement.rs` drives the
  real codec through the injector and requires every crash it can be in to be a state the
  model describes, which is what makes the model more than a second implementation. It covers
  records; it does not cover banks, because rung 0.2 owns the two-bank adapter and there is
  nothing yet to abstract. `single-authority` is therefore proved about a model and not about
  a device, and its row in `obligation.rs` says so.
- **That a clause was updated before the code it constrains.** `recovery-spec` compares the
  four places a recovery invariant lives and fails when they disagree. Issue #20 asks for the
  model and the invariants to be changed *first*, then the proofs, then the code, and the
  order of edits inside one commit is not a thing a rule can read.
- **Allocation, as a measurement.** `bounded-decoding` proves the decoder is total and stays
  inside its input; the allocation half is structural — a `no_std` crate with no dependencies
  and no `extern crate alloc` cannot allocate, and `crate-attributes` and
  `kernel-zero-dependencies` fail a build over each of those. A global allocator that counted
  allocations would need the `unsafe` this workspace denies.
- **Coverage of non-test code specifically.** llvm-cov instruments the test binary, so the
  85% floor is a floor on a diluted number. See
  [ADR 0001](docs/adr/0001-one-pipeline-table-and-a-per-crate-coverage-gate.md).
- **How much of the code-flash delta is the library.** `cargo xtask size` measures an image
  the probe keeps alive, so the probe's own `match` arms and folds are in the number §04
  calls "core + flash adapter" — roughly 5 KiB of 18098 B at rung 0.2. ADR 0002 says so, ADR
  0017 attributes rung 0.2's first figure by symbol, ADR 0019 splits the writer out and
  ADR 0020 the reserve, but nothing *checks* the split: doing so needs a call graph, and all
  three attributions are readings of measurements rather than gates.
- **That a bank's journal region holds a legal journal.** `bank::sealed_generation` decides
  whether a bank is a candidate from its header and its seal. What is between them is
  `frame::Scan`'s, and the two are joined by `BankHeader::journal_offset` rather than by
  anything that checks it: a bank whose header and seal agree and whose journal is damaged is
  authoritative, and the scan is what stops at the damage. That is §14's "frame ignored;
  previous history prefix wins", and it is the division of labour rather than a gap — but it
  does mean "authoritative" is a statement about a bank's header and seal alone.
- **That a bank's seal covers its journal.** A generation seal names the bank *header*'s
  digest and nothing else, so a bank whose header and seal agree is authoritative however
  damaged its journal is. That is the division of labour — §14's "frame ignored; previous
  history prefix wins" is `frame::Scan`'s — but it has a sharp edge worth naming: an erase
  that cleared a *middle or trailing* block of a journal leaves a hole running to the end of
  the region, which is exactly the shape `Scan`'s erased-tail rule reads as a clean end of
  history. Acknowledged records would be lost with nothing to say so. No crash point in
  `waymaker-fault` can produce it — an interrupted erase there always lands a block-aligned
  prefix, and the header sits at the bank base — so this is stated rather than swept.
- **That a re-written header cannot be claimed by a stale seal.** The bank header records no
  generation, so re-writing byte-identical header content under a seal that survived from an
  earlier generation of the same bank produces a matching digest and a bank that reports the
  *stale* generation. Reaching it needs a writer that carries on past a failed erase, which no
  writer in the sweep does. Feeding the generation into the header would close it and is a
  wire-format change; ADR 0017 records it as considered rather than taken.
- **That the two-barrier writer is a refinement of the ghost model.** `waymaker-spec`
  imports `frame` and `storage` and never `append`, and `tests/refinement.rs` writes each
  record with one `frame::encode` and one program — the one-shot writer the model describes.
  The model has no transition for the state §07's payload barrier creates, so it could not
  tell the two writers apart even if it drove both. What *is* true is that the two-step
  writer's intermediate state — the frame body on media and the seal not — is a prefix of the
  record's bytes, which the model already calls a tear and already forbids recovering; and
  `waymaker-fault`'s sweep holds the real writer to §15's oracle at every crash point. Neither
  is the refinement, and [ADR 0019](docs/adr/0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md)
  says so rather than letting a passing `refinement.rs` imply it.
- **That a payload barrier is necessary.** `waymaker_fault::Device` applies programs in the
  order they were issued, so a sweep against it cannot tell a writer that barriers between a
  frame and its seal from one that does not. §12's "no later mutation may become durable
  before mutations ordered by a completed barrier" is an obligation on the driver, and
  `waymaker-conformance`'s across-reset witness is what holds a real one to it. What the sweep
  *can* falsify is the other half — that a seal never lands over an incomplete frame — and
  `crates/waymaker-fault/tests/commit_discipline.rs` does, with a seal-before-frame writer as
  the tooth.
- **That a reserve is the reserve §10 needs.** `capacity-reserve` pins the surface and the
  order the one gate is applied in. It compares *names* and one spelling, so a
  `Reserve::tail_bytes` that stopped counting the outcome record — the term §08's transition
  table makes load-bearing, since there is no edge from an unresolved effect to a terminal
  record — passes it unchanged. `crates/waymaker-flash/tests/capacity.rs` is what holds the
  arithmetic, against `frame::encoded_len` of real records rather than against a second copy
  of the same sum, and `a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding` is
  what makes the outcome term falsifiable rather than argued.
- **An ungated writer added to the reserve from another file, or without `pub`.**
  `capacity-reserve` pins one file, exactly as `storage-contract` and `recovery-surface` do.
  An `impl Reserved { pub fn stage_unchecked(..) }` in a sibling module, or a
  `trait ReservedExt: ...` with a blanket impl, adds the ungated path with the rule silent —
  and so does a `pub(crate)` one *inside* the pinned file, because the scan reads `pub ` and
  not `pub(`. A macro-generated `pub fn` is invisible to it too. The same shape as the two
  bullets above, and stated for the same reason.
- **That the admission the gate opens with is reachable in every case.** The rule pins a
  *position* — the first statement of `Reserved::stage` — and the spelling of the call. It
  does not evaluate the body, so an admission that is first and then conditionally skipped by
  something inside it is outside what a scanner can say. What makes the position enough is
  that the pinned spelling propagates with `?`: a first statement that refuses cannot be
  followed by one that programs.
- **That a caller went through the reserve at all.** `capacity::Reserved` consumes the
  `Journal` it gates, so a caller holding both had to construct a second one from a second
  recovery — the same linear discipline `Journal::after` uses, and the same limit: it makes
  the ungated path a line somebody wrote on purpose, not one that cannot be written. Nothing
  in the workspace obliges a future dispatcher to use the gated writer; that is rung 0.4's,
  and it is stated here so its absence is a decision.
- **That a device is an *instance* rather than a geometry.** Four modules refuse storage that
  is "not the device this was validated against" — `append` at three steps, `recovery`,
  `capacity` and `swap` at all five — and all four decide it by comparing a `Geometry`. Two
  parts of the same model have the same one, so none of them can tell two instances apart: a
  caller holding two chips can prepare a swap on one and commit it on the other, sealing a
  bank whose erase happened elsewhere or erasing an unrelated device's active bank. Codex
  found it on issue #26's second review round. It is stated rather than closed because it is
  one contract in four places and a `swap` that bound an instance while the writer beside it
  did not would be the one module whose `WrongDevice` meant something else; binding the
  storage with a borrow instead of a comparison is issue
  [#84](https://github.com/madmax983/waymaker/issues/84).
- **That a run id a swap installs is one the device has never used.** `SwapError::RunReused`
  compares the next run against the one being retired, which is the adjacent mistake and not
  a uniqueness check: a run id from any *earlier* run passes it, and the `(RunId, EffectSeq)`
  collision is the same one — an external service holding deduplication state would read the
  new run's first effects as redeliveries of that older run's. Nothing on the device
  remembers the ids it has retired, so global freshness is a precondition on
  `Swap::beginning` rather than something the swap can check.
- **That the authority a swap was planned from is the device's.** `Swap::beginning` takes
  `bank::select`'s answer and the retiring run's id as arguments and reads no media, so
  neither is verified. A wrong `run` disables the `SwapError::RunReused` refusal; a *stale*
  `booted`, naming a bank that has since lost a swap, makes `Swap::prepare` erase the bank
  that is really authoritative — and every check in the module passes, because the retired
  reader genuinely is over the bank `booted` named. The refusal that would close it is a read
  of the spare bank's seal and it cannot be made fail-closed: the header it would decode is as
  long as the previous run's input, which the caller's page need not hold. It is a
  precondition on `Swap::beginning`, and closing it by construction is the dispatcher's, at
  0.4 — the same standing as "nothing obliges a future dispatcher to use the gated writer".
- **A swap step added to §10's protocol from another file, and what a swap really erases.**
  `swap-discipline` pins one file, exactly as `capacity-reserve`, `recovery-surface` and
  `storage-contract` each say of the one they pin: an `impl Prepared { pub fn commit(..) }` in
  a sibling module, or a `trait SwapExt` with a blanket impl, adds the step that skips the
  header with the rule silent. And the erase rows compare a *spelling*: they say `prepare`
  erases `self.plan.installing` and `reclaim` erases `self.plan.retiring`, and they cannot say
  that `installing` is the bank the device did not boot. That is
  `crates/waymaker-flash/tests/swap.rs`, which reads the recorded mutation sequence back, and
  `crates/waymaker-fault/tests/swap.rs`, which requires the retired bank never to return to
  authority at any crash point of the lazy erase.
- **A public function added to the rig's oracle from another file.** `rig-oracle` pins three
  files — `audit.rs`, `census.rs` and `run.rs`. An `impl Audit { pub fn assume_passed(..) }`
  in a sibling module, or a `trait AuditExt` with a blanket impl, adds the escape with the
  rule silent, exactly as `recovery-surface`, `storage-contract` and `capacity-reserve` each
  say of the one file they pin. The rig is the sharpest case of the three, because its bugs
  show up as *passing* tests.
- **That a row-named test asserts its row.** `failure-matrix` reads names: a `#[test]` per
  row in the model file and a `Row::` per swept row in the rig file. A test that kept its name
  and lost its assertions passes it. What holds the behaviour is each file's census —
  `every_row_of_the_table_is_reached_and_every_crash_point_is_in_a_row` requires every row to
  be reached on the model, and the rig's requires its census to refuse at the first owed row
  — and the assertions are reviewed by people.
- **That a crash point is in the row it was put in.** The model classifies from the recorded
  operation index, through a six-by-four map the sweep pins, and cross-checks each class
  against the media: a torn outcome with a clean tail, or a schedule row whose effect was
  performed, is a panic. The rig classifies from the witness, the recovered count, the ending
  and the dispatcher's log. Both are test-side functions, and a wrong map that agreed with the
  media in every case would agree with a bug of the same shape.
- **That the published write-amplification figure is any particular size.** `cargo xtask size`
  fails when the measurement cannot be *taken* — a part that cannot be laid out, a run its own
  oracle rejects — but there is no budget the figure is compared against, so it can regress
  arbitrarily with the build green. Issue #27 asks only that it be published. A budget would
  need a target §04 does not state.
- **That the rig's `no_std` claim survives its dependencies.** The `rig-firmware` stage builds
  `waymaker-rig`'s library for `thumbv6m-none-eabi`, which is what makes "written to run on the
  part" a build failure rather than an attribute. What it cannot check is the other half of ADR
  0021's claim — that the rig *fits*, in flash or in RAM, next to an engine. There is no size
  probe for it and no budget it is measured against, because the board it would be measured on
  is the thing that is owed.
- **A widened member of the kernel boundary, or a driver that names a row and ignores it.**
  `kernel-boundary` compares *names*, exactly as `effect-scheduled-fields` and every surface
  pin do: a `Resolve::Replayed` that grew a third field, an `EffectRequest::kind` retyped, or
  a `match` arm that names `Intent::Schedule` and then does the wrong thing are each
  invisible to it. `crates/waymaker-core/tests/transition.rs` holds the kernel's behaviour
  and `crates/waymaker-drive/tests/` the driver's — a diverging workflow that dispatches
  nothing, a redelivery that reuses its identity, and the protocol swept at every crash point
  the injector lists. The routing half also pins one file, the way `capacity-reserve`,
  `recovery-surface` and `storage-contract` each do: a decision taken in a sibling module of
  `waymaker-drive` is a decision the rule cannot see.
- **That the synchronous driver cannot mint an effect identity.** `kernel-boundary`'s
  routing half forbids `EffectIdAllocator` in `waymaker-drive/src/drive.rs`, which is a floor
  and not a proof, in two ways worth naming separately. `EffectId`'s fields are public, so
  `EffectId { run, seq }` written by hand evades it; and the pinned file is the driver, while
  the identity is actually *constructed* one file over, in `effect.rs`, which this rule does
  not read. What holds the behaviour is
  `crates/waymaker-drive/tests/redelivery.rs::what_redelivery_answers_is_not_what_a_fresh_mint_would`,
  which drives the real driver and compares what it redelivered against what an allocator
  would have minted, and `tests/crash.rs`, which does the same at every crash point of the
  window. The same shape as `kernel-owns-no-encoding`: a rule that makes the wrong thing a
  line somebody wrote on purpose.
- **That the synchronous driver never reads a record.** `kernel-boundary`'s routing half
  forbids `RecordKind` and `Step`, and `waymaker-drive` names neither. It does read
  `RecordRef` twice, and both are decisions rather than transcription: `recorded` classifies a
  terminal record when the workflow ends outside an effect boundary, and `begin` refuses a
  journal whose first record is not a `RunStarted`. Neither can be taken from the boundary —
  the first happens where there is no request to compare against and the second before the
  run has started — and both are exhaustive matches, so a record kind added to
  `waymaker-core` is a compile error in them. That is the compiler rather than this rule, and
  saying so is better than a row that reads as though the rule prevented it.
- **That the synchronous driver is a driver anything is obliged to use.** `waymaker-drive` is
  above the layers and nothing depends on it, so it demonstrates that the boundary is
  sufficient rather than obliging a future dispatcher to go through it. That is rung 0.4's,
  and it is the same standing as "nothing obliges a future dispatcher to use the gated
  writer".
- **What the synchronous driver does after a crash that leaves no append point.** It refuses.
  A torn or unsealed tail has no append offset — ADR 0018's anti-bricking rule — and
  recovering from that is §10's `continue_as_new`, which `waymaker-flash`'s `swap` owns and
  which this driver does not call. `crates/waymaker-drive/tests/crash.rs` measures how often
  each happens rather than assuming, and requires both a crash image the run carries on from
  and one it cannot.
- **That a redelivered intent really has a schedule record on media.**
  `Effect::redelivering` mints §07's proof of durable intent from a sequence number. The
  proof is the kernel's `Resolve::Redeliver`, which the driver reads and this module cannot
  see, so it is a precondition on the caller rather than a check — the same standing as
  `Swap::beginning`'s two unverified arguments. What holds it in this workspace is that the
  one caller is `waymaker-drive`'s own boundary, and `crates/waymaker-drive/tests/drive.rs`
  drives a reboot through it.
- **That an exhausted effect can be told from an effect that failed with no detail.** Both
  are an `EffectFailed` with an empty payload, because an empty payload is the only payload
  that fits every declared bound, `effect_result_bytes == 0` included. ADR 0025 states the
  loss rather than hiding it.
- **A §07 step reachable only on one branch, or written by a macro.** `effect-protocol`
  requires each of §07's storage steps to be a statement of its body — at nesting depth zero,
  counting parentheses and brackets as well as braces, in a body that declares neither a
  closure nor a short-circuit — and every proof of durable intent to be built after the
  commit barrier. Codex rounds 2 and 3 are why: `false.then(|| ...)` has no braces, and
  `false && …` has no nesting either, and both put three calls in order at depth zero in code
  that never runs. A scanner cannot follow control flow, so it refuses the constructs that
  create it — which is a *syntactic* answer to a semantic question, and it holds only as far
  as the list of constructs does. A macro is outside it, since a macro-generated `fn` is in
  no `impl` body the rule reads, and so is any future spelling nobody has thought of.
  `capacity-reserve` records a limit of the same shape, and
  `crates/waymaker-drive/tests/crash.rs` is what holds the behaviour.
- **A §07 step added from another file.** `effect-protocol` pins one file, exactly as
  `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one they pin:
  an `impl Dispatchable { fn ... }` in a sibling module of `waymaker-drive` adds a step with
  the rule silent. Inside the file it is closed — the method sets are compared at every
  visibility and a submodule is refused — but a sibling file is a sibling file.
- **That the run half of a redelivered identity is the device's.** §14's guarantee is about
  a `(RunId, EffectSeq)` pair, and only the sequence half is read from media: `ReplayCursor`
  takes it from the schedule record, and the `RunId` is an argument to `Driver::new` that no
  boot compares against the bank header. A caller that derived the run differently between
  two boots — or drove one bank's region with the other bank's run — redelivers the right
  sequence under the wrong run, and every check in the driver passes, because §07 keeps the
  run id in the bank header rather than in every record. That is a precondition on the
  caller, the same standing as `Swap::beginning`'s two unverified arguments, and closing it
  by construction is the dispatcher's at rung 0.4.
- **That an effect happens once.** Waymaker promises at-least-once delivery under a stable
  `(RunId, EffectSeq)`, and no more. Power can fail after an activity changed the world and
  before §07 step 7 commits the outcome, so the next boot redelivers the same identity and
  the activity is performed again. Exactly-once needs an idempotent activity or a downstream
  system that deduplicates that pair; both are outside this engine, and
  [`Activities`](crates/waymaker-drive/src/activity.rs) is where an implementer is told so.
- **That an activity did not truncate its own answer.** `Performed::Exhausted` is how an
  answer wider than `out` is reported, and an activity that instead writes what fits and
  returns `Completed(out.len())` produces a valid short record the driver cannot tell from a
  complete one. It replays for ever. That is a contract on the implementor, stated on
  `Activities::perform`, and nothing can check it: the driver never sees the answer the
  activity had.
- **That the whole-answer crash sweep could fail for a reason this crate owns.**
  `crates/waymaker-drive/tests/crash.rs`'s
  `no_recovered_outcome_holds_part_of_an_answer_at_any_crash_point` is a statement about
  media, and what makes it true is §09's commit seal rather than anything in
  `waymaker-drive`: no change to `effect.rs` or `drive.rs` turns it red. Its falsifier is
  `crates/waymaker-fault/tests/commit_discipline.rs`, whose seal-before-frame writer reaches
  the state it asserts is unreachable. The *exposure* half — that a workflow sees no part of
  an answer — is `crates/waymaker-drive/tests/effect.rs`, and that one is falsifiable here.
- **Stack usage.** Section sizes cannot see a cursor that lives on the caller's stack, and
  the size report says so rather than implying otherwise.

## Status

Rung 0.1, in progress. The three firmware crates exist so the layering is enforceable and the
budgets have one place to be read from. `waymaker-core` now owns effect identity — `RunId`,
`EffectSeq` and `EffectId` — the activity-kind vocabulary, the `EffectIdAllocator` that is
the only thing permitted to mint a sequence, the capacity and decode error vocabulary the
kernel refuses work with (issue #12), and the borrowed record views and record-kind numbering
of design document §09 (issue #13). `waymaker-flash` now owns the bytes those views are
decoded from: §09's handwritten, fixed-endian, self-delimiting frame, its two checksums, and
the append scan that turns a bank into a committed prefix — see
[ADR 0007](docs/adr/0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md).
`waymaker-core` also owns the streaming replay cursor of design document §06 and §02
decision 2 (issue #14): a position that advances through one run's committed history a
record at a time, refuses an ordering no execution could have produced, and holds no borrow
of the caller's 512 B scratch page — see
[ADR 0008](docs/adr/0008-the-replay-cursor-is-pumped-by-its-caller.md). On top of it sits
§08's transition table (issue #15): `ReplayMachine` is the cursor plus the one thing the
cursor cannot know — what the workflow just asked for — and it is the only place
`NondeterministicWorkflow` comes from. Divergence is terminal, and refused before the record
it disagreed with is consumed, so a diverging replay cannot dispatch; see
[ADR 0009](docs/adr/0009-the-transition-table-is-a-machine-that-owns-the-cursor.md).
§16's deferred questions are now a table as well (issue #16): `xtask::docs::DEFERRED_QUESTIONS`
holds all five, two of them settled with evidence and three of them open with the rung that
owns each and what would close it. The integrity check is
[ADR 0010](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md) — CRC-32/ISO-HDLC
and CRC-16/CCITT-FALSE, table-free, decided on measurements taken on `thumbv6m-none-eabi`
rather than on preference — and the metadata a scheduled effect carries is
[ADR 0011](docs/adr/0011-a-scheduled-effect-records-a-length-and-a-digest.md), which fixes it
at a sequence, a kind, a length and a digest. Each answer has a rule holding it: a checksum
that changed polynomial or grew a table fails `integrity-check`, and a fifth field on
`EffectScheduled` fails `effect-scheduled-fields`.
Issue #17 then asked ADR 0010's answer to be *held* rather than assumed:
[ADR 0012](docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md)
puts the two seals behind `waymaker-flash`'s `IntegrityCheck` trait, binds the shipped
answer to `Catalogued`, and settles the widths as the trait's own return types — sixteen
bits over the header, thirty-two over the header and payload, which is what §09's frame
spends. The trait itself costs nothing — 7288 B against 7296 B before, with the probe held still —
and the `default` row reads 8180 B of the 8192 B budget, because `size-probe-reach` makes
the probe name every entry point, run one codec body twice, and link a driver that goes
through §12's three validators. Twelve bytes of headroom is a real result, and
[ADR 0013](docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md) says so rather
than absorbing it: rung 0.2's banks, seals and barriers do not fit in it. The rejected CRC-32C candidate is implemented in
`waymaker-flash`'s integrity tests, in all three forms ADR 0010 measured, so the rejection is
a comparison rather than an assertion, and the failure modes §09 names — a write torn at a
program-unit boundary, a stale erased tail, a partial program that can only clear bits — are
swept there rather than argued. What a CRC still is not is authentication, and that is a
passing test too.
Issue #18 then makes design document §15's opening sentence — "crash testing is part of the
design, not a post-MVP hardening phase" — a thing the workspace does rather than a thing it
intends. §12's storage contract is now real: `waymaker-flash`'s `Geometry` is the only legal
description of a device and the only thing that decides whether an offset and a length are
allowed, and `StableStorage` is the four operations and one barrier every port implements,
with its public surface pinned by `storage-contract` so §05's "must not expand the firmware
traits to accommodate host conveniences" is a build failure rather than a sentence. Above the
layers, `waymaker-fault` is the in-memory storage model and the crash injector: media that
starts erased and only clears bits, a recorder of the write sequence, and `injections` — a
pure function that lists *every* point at which that sequence can be interrupted, at every
byte of a program, at every erase block of an erase, and before and after every barrier. The
three record states §15 asks for are computed as the writer runs rather than guessed from the
bytes, and §15's core property oracle is a function that fails closed four different ways.
Three unrelated writers are driven through it unmodified, so "reusable without modification"
is a test rather than a claim — see
[ADR 0013](docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md), which also
records what this leaves owed: the hardware power-cut loops, at 0.2, where the boards are.
Issue #19 then makes that harness prove something. §15's oracle is now all four of its lines —
the prefix, the acknowledgment obligation, the dispatched-intent obligation, and exactly one
authoritative bank — and it is swept over record sequences drawn from a seed on geometries
drawn from the same one, at every crash point the injector lists. What #19 asks to be covered
is asserted rather than claimed: a census fails the build when the sweep thins out, and two
tests read the enumeration itself to check that a tear lands at every byte and every program
unit and that the power goes before and after every barrier. The other half of the exit
criterion is that the suite can fail, and it is shown rather than argued: codecs that stop
sealing what they say they seal — ordinary implementations of ADR 0012's `IntegrityCheck`
trait, so the journal is written and read by the same weakened firmware — are caught with
`RecoveredATornRecord`, and history read one record short, out of order, one skipped or one
invented past the end is caught too. See
[ADR 0014](docs/adr/0014-the-oracle-is-four-lines-and-the-sweep-is-seeded.md), which also
says where the limits are: the cursor mutants are models of a bug rather than injections into
a `const fn` state machine, and the two-bank generation seal is a stand-in for the storage
unit rung 0.2 owns.
Issue #20 then asks §14's guarantees to be *stated* rather than sampled, and `waymaker-spec`
is that statement: a ghost model of committed history in the vocabulary §15 already uses, the
journal and bank state machines as one total transition function, and §14's guarantees as
predicates over a state and a reader's answer. The proofs are exhaustive rather than seeded —
a breadth-first search closed under every transition, with a ceiling it fails against rather
than truncates at — and each is falsifiable twice over: every one of the model's five
preconditions is removed in turn and required to break a named guarantee, and a catalogue of
readers wrong in one way each is required to be caught by the guarantee it breaks. The
firmware is held to the model at every crash point `waymaker-fault` lists, so the model is not
a second implementation nobody tests. Two findings came out of writing the proofs rather than
out of reading the code: acknowledged durability needs writes to be append-only, which is what
makes "prefix of committed history" and "prefix of declaration order" the same statement and
what `waymaker-fault`'s `Ledger::committed` filter had been resting on; and a bank guard that
only forbade stranding the device still permitted erasing the *newer* of two sealed banks,
which is §14's "never recover the old run as current". See
[ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md),
which also says what is bounded, what is owed at 0.2, and what a general proof would cost.
Issue #22 then arrives at rung 0.2 and gives §10's two-bank lifecycle a shape on media.
`waymaker-flash`'s `bank` module is the layout — `erase_blocks / 2` whole blocks per bank,
derived from a `Geometry` rather than written down, refusing a device that cannot hold two —
the bank header of §10 and issue #22 (the `RunId`, the workflow identity, an input schema and
the bounded run input, in a self-delimiting frame checksummed twice like §09's), the
generation seal, and `select`, which is §10's "the bank with the highest valid generation
seal is authoritative" as a total function. Two decisions in it are about crash windows
rather than about layout, and
[ADR 0017](docs/adr/0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md)
is where both are argued. The seal names its header — it carries the header frame's own
digest — so a seal that outlived the erase which took its header, or one written over a torn
header by a writer that did not check, is not a candidate at any generation, with no
assumption about erase order anywhere in the module. And generations do not wrap:
`Generation::successor` refuses at `MAX`, so the plain `u32` order *is* the order of the
swaps and issue #22's "explicitly rather than by unsigned comparison luck" is answered by
making the wrap unreachable rather than by getting a comparison right. A tie is
`Authority::Ambiguous` and is reported rather than resolved. The header also carries the
program granularity it was written at, which is the fact `frame::Scan`'s documentation named
as rung 0.2's to record. All of it is swept at every partial-swap crash point the injector
lists — `crates/waymaker-fault/tests/banks.rs`, which used to *model* this protocol and now
drives it — with four mutants holding the sweep honest, the sharpest being a seal-blind
reader booting a bank whose header was never written. The `integrity-check` rule grew a
second routing table so the bank's seals cannot drift around the trait either. What it cost
is 8180 B to 10976 B of code flash against an 8192 B budget, which is the budget conversation
[ADR 0013](docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md) said had to happen
before these were written; ADR 0017 has it, raises the gate to 16 KiB, and writes down that a
third of the measured figure is the size probe rather than the engine.
Issue #23 then makes recovery a thing a *device* does. `frame::Scan` walks a journal that is
already in RAM, which §04's 768 B runtime budget — stated with a 512 B scratch page — says a
device cannot do with a 4 KiB bank; `waymaker-flash`'s `recovery` module is the other reader.
`JournalRegion` is the bytes between a bank's header and its seal, validated once as a legal
*program* rather than as a read — a geometry nests, so whatever may be programmed may be
read, and a region that is readable but not programmable is one whose append offset no driver
would accept. It also keeps the geometry it was validated against and every step compares
that against the storage it is handed, because bounds proved on one device say nothing about
another. `Recovery` is a position in that region and nothing else — forty bytes, a 28-byte
region and an offset and a verdict, all asserted at compile time — pumped by its caller with
a page it never retains, which is
[ADR 0008](docs/adr/0008-the-replay-cursor-is-pumped-by-its-caller.md)'s decision made twice
for the same reason. And `Ending` is what a finished scan learned, with
the append offset carried by exactly one of its three shapes: a scan that stopped at damage
has nowhere safe to write, because without §09's commit seal it may have stopped at a
half-programmed header, and on NOR appending there — or past it — is a bank that never boots
again. The invariant is that an append offset is always the start of an erased run reaching
the end of the region *and* an offset this device can program at, and it is *swept* rather
than asserted: `waymaker-fault` drives the
real reader over media a real crash left behind, at every point a power loss can land, with a
tooth that finds a crash point at which the obvious implementation would have pointed at
programmed media. Two readers of one format drift, so they are held to each other — 256
generated journals in `waymaker-flash` and every crash image in `waymaker-fault`, walked both
ways and required to agree record for record and offset for offset. `frame::frame_len_of` is
what makes a page-bounded reader possible at all, and is §09's first checksum finally used
rather than only explained: a length that is known to be the one the writer wrote, read
before the payload is in hand. See
[ADR 0018](docs/adr/0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md),
which also says what was owed at the time — "unsealed" was issue #24's, so a torn tail and a
damaged frame stopped the scan in the same place, and out-of-sequence stays the replay
cursor's, held sound by an append offset derived from the ending rather than from the offset.
Both halves of the append guarantee were found by review rather than by writing them down:
the region validated only as a read, and then the captured geometry never compared with the
caller's storage. The erased-tail walk is the cost that will be felt first — a 64 KiB bank
with a 512 B page is 128 reads on every boot, however short its history. ADR 0018 called that
the strongest practical argument for the commit seal, and issue #24 showed the argument was
wrong: a seal says a record is committed, not that no record follows, so the walk survives it
and what would close it is a record kind saying "history ends here" rather than a seal.
Issue #24 then puts §07's two barriers on media, which is the last thing rung 0.2 owed the
record format. A record is now a padded frame body **and** a commit seal one program unit
wide: the seal is the frame's own check with bit 7 of each byte cleared, repeated to fill the
unit, so no byte of a seal is ever `0xFF` — an erased program unit is never a seal, a seal
that did not land whole is never a whole one, and a seal is bound to the frame it covers —
at `min(align, 4) * 7` bits, which is seven on a byte-programmable part and is stated rather
than rounded up. The first two make "sealed but incomplete" a state that cannot be
reached rather than one a reader has to detect, and §09's first stop condition finally exists:
`DecodeError::Unsealed`, and `Ending::Unsealed`, which is the fourth shape
`crates/waymaker-flash/src/recovery.rs` predicted it would have to grow. `waymaker-flash`'s
`append` module is the writer, and it is three types rather than one because the ordering has
to not compile: `Journal::stage` programs a frame body and hands back a `Staged`, whose only
method is `payload_barrier`, which is the only thing that produces a `Sealable`, whose only
method programs the seal. A `compile_fail,E0599` doctest beside a compiling twin is issue
#24's second "done when", and `commit-discipline` is what stops the typestate being given
back. `Journal::after` takes a finished `Recovery` and nothing else, so ADR 0018's
anti-bricking rule is structural rather than documented. Write amplification is four counters
of what the device was *asked* for and no division, because a divider is not free on
`thumbv6m`. The exit criterion is swept rather than argued —
`crates/waymaker-fault/tests/commit_discipline.rs` classifies every record slot of every crash
image and requires "sealed but incomplete" to be unreachable, with a seal-before-frame writer
as the tooth that reaches it. Two findings came out of it. The over-striding hole
`frame::Scan` documented as undetectable is closed, because a seal sits at a fixed offset from
the frame it seals and a larger stride looks for it in erased media; and the erased-tail walk
is *not* closed, which `recovery.rs` had claimed the seal would fix — a seal says a record is
committed, not that no record follows. See
[ADR 0019](docs/adr/0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md),
which also says what the sweep cannot falsify: the fault harness does not reorder stores, so
the payload barrier's necessity is §12's contract and `waymaker-conformance`'s across-reset
witness rather than something this sweep can break.
Issue #25 then makes §10's "capacity is explicit" a thing the writer does. `waymaker-flash`'s
`capacity` module is the reserve: `Bounds` is what a run declares its records may be worth,
`Reserve::for_layout` prices §10's two exits against a real bank — a terminal record, and the
`continue_as_new` header the swap writes into the *inactive* bank — and refuses bounds under
which a bank could not roll over, or under which the run a swap installs would have no exit of
its own. `Reserve::admits` is §10's decision as a pure predicate over a length, and `Reserved`
is the gate: it consumes the `Journal` it wraps and takes the decision before `Journal::stage`
is called at all, so issue #25's "the failure produces no mutation at all" is a property of
the call order rather than of an undo — the device is not read, not programmed, not
barriered, and the write-amplification counters do not move.
The reserve's least obvious term is the one that makes it correct, and it came out of §08
rather than out of §10. A schedule record creates an obligation: `ReplayCursor` has no edge
from an unresolved effect to a terminal record, so a run with an effect outstanding cannot end
until its outcome is written — and the reserve everybody writes first, one terminal record,
admits a schedule and then strands the run with an outcome that does not fit and a terminal
record §08 will not let follow it. That is driven and watched failing rather than argued:
`a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding` runs the wrong policy
against the real writer and finds the crash point at which it strands. `continue_as_new` from
the near-capacity state is a test too, run through §10's seven steps by hand because the
writer that will do them is issue #26's. See
[ADR 0020](docs/adr/0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md).
Two more things came out of review rather than out of writing it. The floor a bank is accepted
against is the next run's opening record, one effect scheduled and resolved, and its exit —
not merely "the run can end", which accepts 4004 measured configurations in which the very
first `EffectScheduled` is refused for ever. And issue #24's own layout guard reserved a frame
*body* where a record is a body and a commit seal, so a bank whose journal could hold a frame
and not the seal that commits it was a legal layout; that is fixed here, because
`Reserve::for_layout` now takes `BankRegion::max_run_input_bytes` as its first gate. What is
left owed is written down: the reserve is not obliged on anybody, because the dispatcher that
would be obliged is rung 0.4's; §08's order is a precondition on the caller rather than
something `Reserved` can enforce; the `waymaker-fault` sweep of the reboot join is not here;
and the code-flash gate needed a second raise, which ADR 0020 argues should be the last
before issue #72 corrects what the figure is measuring.
Issue #27 is rung 0.2's exit criterion, and one bullet of it — "power-cut loops pass on one
Cortex-M0+ board and one Cortex-M4 board" — is the first thing in this repository that no
amount of host-side work discharges. `waymaker-rig` is everything else: §15's power-cut and
watchdog-reset rig, `#![no_std]`, allocation-free, and built for `thumbv6m-none-eabi` by a CI
stage of its own, because a rig that could only run on a host would be a simulation wearing a
rig's name. What makes it more than a second `waymaker-fault` is that it keeps a **durable
witness** — a power cut takes RAM, and two of §14's guarantees are statements about the
*writer* rather than about bytes: `acknowledged-durability` is about barriers that returned
and `durable-intent` about effects that were dispatched, and neither is readable off a
journal. Marks go down before a record's first program, after its commit barrier, and before
each physical effect, and every one of those positions is chosen for which way an interrupted
mark is allowed to be wrong. The run and the cut point are pure functions of a seed and an
iteration, so a log line carries the whole run; the witness travels in the line too, along with
the evidence a verdict rests on — how many records recovery accepted and how many banks claimed
authority — because two of the six breaches are caused by bytes a line cannot carry, and a
rebuilt part reaches a pass. A watchdog reset was, at the time, a
*cause the rig carried* rather than one it could perform: the plan armed it, the cutter was
handed it and the log recorded it, but every injection the harness made was a power loss, so
the host sweep filled the three power-cut cells and the census refused the run. That is no
longer where this stands — issue #27's own paragraph below, and
[ADR 0023](docs/adr/0023-a-watchdog-reset-is-modelled-and-its-difference-is-one-return.md), are
what the rig does now. Two more corrections came out of
review, and both are the same shape — an instrument reading its own bookkeeping as a finding
about the firmware. A mark is not evidence of the thing it marks, so the dispatch cell is
earned by the dispatcher having been entered; and a part this run was never *installed* on is
not a verdict about it, so `Rig::judge` walks a journal only when exactly one bank is
authoritative **and** its header names this run. Without the second, `verify(n)` met the part
that finished `n - 1` — which is every iteration after the first — and reported a §14 violation
on a healthy board. An earlier version of this partitioned
the injector by how much of an operation completed and called half of it watchdog coverage,
which is the relabelling this crate exists to avoid, and Codex was right to reject it. The rig is
driven at every crash point `waymaker-fault` enumerates, judged by its own oracle, and held
honest by writers wrong in one way each that must be caught by the guarantee they break and by
no other. §04's write amplification is published by `cargo xtask size`, measured by running the
real `Journal` over three parts that differ in program granularity, because that is what the
answer turns on. Three findings came out of review rather than out of writing it, and all three
were live: a torn witness mark decoded as a whole one about once in 256, which is
[ADR 0019](docs/adr/0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md)'s
masked-repeat trick reimplemented without the trick; the audit never checked that the witness
it read belonged to the iteration it was judging, so a reset landing before the instrument was
erased made a healthy device report a violation; and a standard 256-byte-page NOR was accepted
at construction and then failed in the middle of installing a bank. See
[ADR 0021](docs/adr/0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md), and
[what the boards still owe](#what-the-boards-still-owe), which is where the exit criterion
itself is recorded as unmet.
Issue #26 is the last thing rung 0.2 owed, and `waymaker-flash`'s own documentation said so
in a sentence: "what is still owed at 0.2 is the bank swap and `continue_as_new`".
[`swap`] is that swap. §10's seven steps are five types, and the ordering the two recovery
rules rest on — "a crash before step 5 recovers the old run, a crash after step 6 recovers
the new run" — is enforced by which type a step hands back rather than by the order lines
appear in. Step 1 is the sharpest of the five and the one worth reading twice: "stop
accepting new effects for the current run" is `Swap::beginning` *consuming* the `Journal` the
run was appending with, so a swap in progress is a run with no writer left. It takes a
finished `Recovery` as well, because a bank whose scan ended damaged or unsealed has no
writer at all and is exactly the bank §10 says to recycle — a constructor that only took a
`Journal` would have been unavailable in the case the swap exists for. Everything decidable
is decided before the erase: a device with no single authority, a generation at the ceiling,
a next run repeating the retired run's id, a reader from the wrong bank or the wrong device,
and a next-run input that would leave the installed bank no journal are all refused with no
byte moved — and the order matters, because a swap that erased the spare bank and *then*
found the header would not fit has destroyed the only other copy of anything the device
holds. Which bank is erased is never a parameter, at either end: the bank installed into is
the one the device did not boot and the bank reclaimed is the one it did, both fixed at
`beginning`, which is what makes §10's lazy step 7 crash-safe by construction — the new bank
already carries a strictly higher generation, so an interrupted erase of the old one can only
remove a candidate and never promote one. A seal is never programmed over a header that did not land, and
the reason is the `?` on the program call rather than where the digest came from —
`waymaker-fault`'s `swap_that_seals_whatever_landed` has two bugs and the real writer avoids
the second. Three
different mechanisms hold three different claims, and the ADR is explicit about which:
`swap-discipline` and a `compile_fail` doctest hold the step order; the crash windows are
`crates/waymaker-fault/tests/swap.rs`, which drives the real writer at every crash point of
every step, censuses the steps so a thinning sweep fails the build, and keeps five wrong
swaps as teeth — one that clears the bank it booted, one that repeats a generation, one that
takes step 7 before step 6, one whose seal is not a seal, and one that installs a run the
header does not name; and that the barriers are *real* is still §12's contract and
`waymaker-conformance`'s across-reset witness. Effect identity is the third "done when":
`Installed::allocator` is the run the swap installed, starting at `EffectSeq::FIRST`, and
`SwapError::RunReused` refuses the one input under which the two runs' `(RunId, EffectSeq)`
pairs would collide — as far as the `run` it is handed is the real one, which is a
precondition and not a check. Two things came out of this rather than out of
reading the code. The first measurement was 42 B *over* the 18 KiB gate, and the two defects
that closed it were a plan carrying a geometry the region beside it already held and five
steps taking that eighty-byte plan by value to compare one field. Those two and a trim of the
probe's own arithmetic are 444 B between them, and no third raise, which is what ADR 0020
asked for. And what is still owed is written down: `waymaker-spec`'s
banks hold no records, so §14's "never recover the old run as current" is still proved about
a model rather than about this code; nothing obliges a caller to consult the capacity reserve
before swapping; and two of `Swap::beginning`'s arguments are preconditions this module
cannot check — all three are rung 0.4's dispatcher. Review found the sharper half of that
last one, and it is in [what is not checked](#what-is-not-checked) rather than implied: a
*stale* `booted` makes the swap erase the bank that is really authoritative, and every check
in the module passes. See
[ADR 0022](docs/adr/0022-the-bank-swap-is-a-typestate-and-step-one-is-a-value-being-consumed.md).

[`swap`]: crates/waymaker-flash/src/swap.rs

Issue #27's third bullet — "watchdog-reset tests at the same three points" — was the one
acceptance criterion in this repository with no coverage at all, and
[ADR 0023](docs/adr/0023-a-watchdog-reset-is-modelled-and-its-difference-is-one-return.md)
closes as much of it as a host can. `waymaker_fault::Interruption` gains a third variant, and
it is a fault of its own rather than a label on an existing one: the supply holds, so the
flash controller finishes the unit the core stopped believing in, and the call never returns —
where a power cut at `Progress::Whole` returns `Ok(())` first and lets §02 decision 3's
dispatch happen. It is enumerated at unit boundaries rather than at every byte, because a reset
inside a unit leaves what the boundary above it leaves and answers the caller the same way,
and an exhaustive list that counts one crash point twice is no longer a count of anything —
but *not* folded into the brownout that left the same bytes, because the two hand the writer
different errors and this crate's writer is under no obligation to propagate one. That second
clause is Codex's, from the first review round, and the first version of this change had the
argument wrong. Both halves are measured rather than argued, and so is the consequence: on
media a watchdog reset is *weaker* than a brownout, and
`every_watchdog_image_is_one_a_power_cut_also_produces` proves the inclusion over the real
journal writer and requires it to be proper, so the two causes cannot become one model wearing
two names. The rig's census credits a cell from the injector's own cause and never from a
reading of `Progress`, which is the rule the first attempt at this broke and Codex was right
to reject. Every cell of the census is now filled on a host — the dispatch cell at the next
operation's `Progress::None`, since the barrier that would have carried it does not return
under this cause — and the census is a complete census *of the model*, with the boards owing the physical
half of both causes exactly as before. The third difference a watchdog reset has — RAM survives it — is
modelled nowhere and measured as a cost:
`a_rig_that_skipped_the_journal_scan_would_notice_no_loss_at_all` shows a rig that judged from
the history it still held excusing *every* loss the media-reading rig catches — a tautology,
written as one. The witness half is the one that can fail and does not:
`a_witness_kept_in_ram_over_claims_by_one_mark_and_still_accuses_nobody` measures that a
retained witness, derived per run rather than from a finished one, accuses no healthy run,
because every mark goes down after the thing it attests — and the tooth beside it shows a
writer with that order reversed accusing one.

Issue #28 is labelled rung 0.3, and what it asks for is a thing the workspace *does* rather
than a thing it argues. §06's explicit kernel boundary has existed since issue #15 —
`EffectRequest`, `Intent`, `Resolve`, `Outcome` and `Next`, with `ReplayMachine` answering in
them — and `waymaker-flash` has had both halves it is answered from since issues #23 and #24.
Nothing joined them, so "the protocol goes through this boundary, and `waymaker-embassy` is a
façade and nothing more" was an argument. `waymaker-drive` is the join: a synchronous driver
that recovers a journal, replays it through the machine, dispatches what the machine says to
dispatch, and records what comes back — with no `Future` and no executor anywhere in it. A
workflow is a plain value with a method, and it suspends by propagating `Suspended` with `?`,
which is the place `.await` will go. The claim that it allocates nothing is a *build*: the
crate is `#![no_std]` and the `drive-firmware` stage links its library for
`thumbv6m-none-eabi`, so the reference workflow a board would run is a workflow a board can
link. §02 decision 3 stops being a comment and becomes the order of two calls — the schedule
record is committed in one and the world is not heard from until the other. Two calls in one
function is an order a later change can swap, which is what issue #29 fixes; at 0.3's first
half it was an order rather than a type. It is swept at every crash point `waymaker-fault` enumerates, with
a hand-written driver that dispatches first as the tooth. Every append goes through §10's
`Reserved` rather than through `Journal`, and that is the sharpest thing review found: an
ungated driver commits a schedule record, tells the world to perform the effect, and *then*
finds the outcome record does not fit — and §08 has no edge from an unresolved effect to a
terminal record, so the run can never end and every boot after it performs the effect again.
The reserve refuses before the schedule record, so the run declines to start the effect
rather than having already asked for it. The lifetime discipline #28 asks to
be *documented* is enforced instead: every borrowed result points into a buffer the caller
owns and the driver reuses, and `Boundary::call` derives its borrow from `&mut self`, so a
workflow holding one across the next boundary does not compile — a `compile_fail` doctest
beside a compiling twin. The second "done when" is `kernel-boundary`, a rule with two halves:
adding a record kind cannot change the boundary's signature, because the member sets are
pinned in both directions, and the driver has to decide from `Intent` and `Resolve` rather
than from a record. What is owed is written down rather than implied: this driver does not
swap banks, so a crash that leaves a torn tail is refused rather than repaired; nothing
obliges anybody to use it; and bank selection stays `waymaker-flash`'s. All three are rung
0.4's dispatcher. See
[ADR 0024](docs/adr/0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md).

Issue #29 is the rest of rung 0.3, and it is one sentence from §07 made structural: "a
physical effect never precedes its committed intent". The seven steps existed in the right
order after issue #28; nothing stopped a later change from reordering them, because the order
was two statements in one function. `waymaker-drive`'s `effect` module is the protocol as
three types. `Effect::schedule` takes steps 1, 2 and 3 and hands back a `Dispatchable`;
`Dispatchable::intent` is the only source of a `DurableIntent`; and `Activities::perform` —
step 4 — accepts no other proof. A `compile_fail` doctest shows a caller cannot forge one.
The one exception is `Effect::redelivering`, which mints a proof from a sequence number
because §08's redelivery row is the kernel's word rather than anything this module can read;
it is `pub(crate)`, so the trust is confined to the one caller beside it, and review found
that it had been public. §07's last sentence
is a type too: `Dispatchable::resolve` takes steps 5, 6 and 7 and returns the only `Outcome` a
caller can reach, so the workflow observes the result after step 7's barrier and at no earlier
point. `effect-protocol` pins the surface, the two methods the dispatch state may declare, the
private fields, the two bodies a proof may be built in, and that each step body names the
frame, the payload barrier and the seal exactly once and in that order.
The third work item is the one that was a live defect rather than a missing rail. An activity
whose answer was longer than the bound failed the boot — and the schedule record was already
committed, so the next boot redelivered it, met the same answer, and failed again, for ever.
§08 has no edge from an unresolved effect to a terminal record, so that run could never end.
It is now `Resolution::Exhausted`, recorded as an `EffectFailed` with no payload: the run
makes progress, and no part of the answer reaches the workflow. Review found the fix
half-applied — an activity that *reported* more than the buffer it was handed still met a
refusal three lines below, with the same stranding behind it — and both routes are the
exhaustion now, because on media they are the same statement. Two bounds became one at the
same time — the activity is handed a buffer of exactly `Bounds::effect_result_bytes`, from the
reserve that priced the bank, and a narrower result buffer is refused before the run's own
record is written. Both "done when"s are swept rather than argued:
`crates/waymaker-drive/tests/crash.rs` requires every dispatched effect to have a recoverable
schedule record and every committed outcome to hold a whole answer, at every crash point the
injector lists, with the exhaustion path swept beside them. See
[ADR 0025](docs/adr/0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md),
which also says what is owed: the redelivery proof is the kernel's word, and an exhausted
effect cannot be told from one that failed with no detail.

Issue #30 is rung 0.3's last item, and what it asks for is one guarantee held at the layer
that can break it. §14's fourth guarantee — "retries and reboot redelivery reuse the original
effect identity" — was proved about `EffectIdAllocator` by `waymaker-spec` and about nothing
else, and a driver that handed a redelivered effect a fresh sequence would have passed every
test in the workspace: the run still completes, the journal still reads back, and only a
downstream service sees two effects where there was one. `waymaker-drive` never had an
identity of its own — every `(RunId, EffectSeq)` it dispatches under comes from
`Intent::Schedule` or `Resolve::Redeliver` — and that is now a build failure rather than a
reading: `EffectIdAllocator` joins `RecordKind` and `Step` in
`source::DRIVER_FORBIDDEN_VOCABULARY`, so a driver reaching for the one thing permitted to
mint a sequence fails `kernel-boundary`. It is a floor and says so in
[what is not checked](#what-is-not-checked), because `EffectId`'s fields are public.
What holds the behaviour is measured twice.
`crates/waymaker-drive/tests/redelivery.rs` drives an **in-boot retry** — another boot with
no reset at all, so the workflow value, the world and the RAM all survive, which is the case
a reboot test cannot tell from an identity held in a counter — and then five of them, and
requires one identity across the lot. Every case uses the run's *second* effect, because the
allocator starts at `EffectSeq(0)` and a run whose outstanding effect is its first cannot
tell redelivery from a fresh mint; that is a tooth rather than a comment claiming it.
`crates/waymaker-drive/tests/crash.rs` adds issue #30's first "done when": the crash points
at which the crashed boot had *already performed* the effect and its outcome record did not
commit, which is the window between §07 step 4 and step 7's barrier. The assertion is
**total** over that window and it splits two ways — a crash at an operation boundary leaves a
whole journal and the next boot redelivers, and a crash inside the outcome frame or its seal
leaves a torn tail with no append point, which ADR 0018 refuses rather than repairs. Both
classes are censused, because an earlier version skipped the second with a `continue` and so
asserted about six of the window's 165 crash points. A third test reads what the driver
*wrote*: a schedule record carries the length and the digest of the bytes the workflow
passed, which is the one thing a run cannot catch about itself, since a driver that records a
constant length agrees with itself on every replay. The
digest half is §08's fourth row at the driver rather than at the kernel: a changed input on a
resolved effect stops the run, and — the sharper case — a changed input on an *outstanding*
one stops it rather than redelivering, which is issue #30's "not a silent re-dispatch". Two
inputs, of the recorded length and of another; a driver cannot vary one half of §09's digest
alone, so isolating the length from the checksum stays `waymaker-core`'s. No retry
*policy* is introduced: §16 leaves `retry-policy-placement` open at rung 0.4, and a driver
that decided when to try again would settle a deferred question by accident. What Waymaker
promises is now stated where an implementer reads it, on `Activities` itself: one effect can
be performed more than once, every attempt carries one identity, and exactly-once physical
side effects are not on offer under any setting. See
[ADR 0026](docs/adr/0026-redelivery-is-the-kernels-answer-and-at-least-once-is-the-contract.md).

Issue #31 is rung 0.3's exit criterion, and it asks for §14's failure-semantics table to
become executable row by row. It now is: `waymaker_rig::matrix::Row` is the ten rows as a
vocabulary a board can link, `crates/waymaker-drive/tests/matrix.rs` is one test per row named
after it — 541 crash points classified from the operation the crash interrupted and
cross-checked against the media, with the two bank rows driving the real swap and then
*booting the driver* on the bank `select` names — and `Rig::resume` carries a cut iteration on
so that the rig can observe the behaviour column rather than only the recovery column.
`crates/waymaker-rig/tests/matrix.rs` classifies and resumes 434 crash points into six rows
and requires the rig's census to refuse at the seventh, which is the honest shape of a rig
with no swap workload. The `failure-matrix` rule holds the five places a row lives to one
table. One finding came out of writing the rows down rather than out of reading the code: §14
row 5 says a torn completion is redelivered, and under ADR 0018 it cannot be, in this bank or
by this rig; the table above says `continue_as_new` instead, and
[ADR 0027](docs/adr/0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md)
records it.

The kernel-state registry has two entries, so the 128 B budget is a number about something.
Timers and the `TimerScheduled`/`TimerFired` records are the rest of rung 0.1, and the async
`Ctx` and dispatcher arrive with 0.4. The
gates went in before the code they govern, which is the point: a gate retrofitted after
coverage has slipped is a gate that ratifies the slip.
