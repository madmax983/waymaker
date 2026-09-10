# CLAUDE.md

Waymaker is a firmware-first durable workflow engine for Rust. A workflow is re-created from
its beginning after reboot and deterministically replayed through an ordered journal:
completed effects return their recorded results, and the first unresolved effect becomes the
next piece of work.

This file is what a contributor — human or agent — works to. It states the invariants, the
layering rules, and what each crate must not own.

Much of it is checked rather than remembered: the must-not-own cells, the permitted
dependency edges, the eight decision ids, the command list, the five deferred questions and
all 53 rule ids below are compared against the tables that own them, and `cargo xtask check-layering` fails a pull
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
cargo clippy --locked -p waymaker-embassy --all-targets --features postcard -- -D warnings
cargo test --locked -p waymaker-embassy --features postcard
cargo doc --locked -p waymaker-embassy --no-deps --features postcard
cargo doc --locked --workspace --no-deps --no-default-features
cargo --locked xtask coverage
cargo build --locked --no-default-features --target thumbv6m-none-eabi
cargo build --locked -p waymaker-rig --no-default-features --lib --target thumbv6m-none-eabi
cargo build --locked -p waymaker-drive --no-default-features --lib --target thumbv6m-none-eabi
cargo build --locked -p waymaker-drive --no-default-features --features without-facade --lib --target thumbv6m-none-eabi
cargo build --locked -p waymaker-embassy --no-default-features --features postcard --lib --target thumbv6m-none-eabi
cargo clippy --locked -p waymaker-size-probe --target thumbv6m-none-eabi --features probe,embassy-postcard --bins -- -D warnings
cargo --locked xtask size
cargo test --locked -p waymaker-spec --no-default-features
cargo test --locked -p waymaker-drive -p waymaker-rig --no-default-features --test matrix
cargo test --locked -p waymaker-flash --no-default-features --test corpus
cargo --locked xtask profile
cargo --locked xtask check-layering
```

`cargo xtask profile` needs valgrind, which no rustup profile carries — the pipeline
installs it in the `profiling` job, and the command fails closed rather than passing when it
is absent, because a measurement that did not happen is not a measurement that passed.

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
| `wire-format-migration` | How stable wire-format migration is performed after a deployed fleet outlives v1. | [Settled by 0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md](docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md): the format is frozen byte by byte with a committed corpus, the read side is a set and the write side one number, and migration is a new bank at a `continue_as_new` boundary. |

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

"Without `embedded-storage` becoming a kernel dependency" is not a promise either.
`waymaker-core`'s and `waymaker-flash`'s `may_depend_on_external` lists in
`xtask::policy::LAYERS` are empty — only `waymaker-embassy` has entries, and only for issue
#37's optional codecs — so the kernel growing that dependency fails
`kernel-zero-dependencies` and `waymaker-flash` growing it fails `dependency-direction`.

## The frozen wire format

Design document §09 states the journal and the wire format, and issue
[#41](https://github.com/madmax983/waymaker/issues/41) freezes it at v1. The promise is
one-directional and worth stating in those words: **records a shipped device wrote stay
readable by every later 1.x firmware.** An earlier firmware meeting a later record kind
stops, so downgrade is not supported —
[ADR 0037](docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md) is
the policy and settles §16's fifth deferred question.

The format is stated byte by byte in
[`docs/format/wire-format-v1.md`](docs/format/wire-format-v1.md): the frame, the commit
seal, the record table, the bank header, the generation seal, and both check algorithms with
their parameters. That document is what a porter implements from, and the `wire-format` rule
is what stops it drifting from the code.

Three things hold the freeze, and they hold different halves of it:

- **The corpus** — [`crates/waymaker-flash/tests/corpus/v1`](crates/waymaker-flash/tests/corpus/v1/README.md),
  twenty-one files of frozen bytes, run by `crates/waymaker-flash/tests/corpus.rs` as the
  `corpus` CI stage. It and the `wire-format` rule are the two things here that notice a
  *renumbering*, and they notice differently — the rule names the constant, the corpus holds
  the byte. Nothing else can: the encoder takes a kind's number from `RecordRef::kind` and
  the decoder matches the same constants, so swapping two leaves every round trip, every
  property test and every crash sweep green. The
  bytes were produced by an encoder written from the field list rather than from `frame.rs`,
  and that encoder reproduces `tests/frame.rs`'s golden frames byte for byte. A case is
  added, never regenerated.
- **The `wire-format` rule** — the frozen numbers, the record numbering in both directions,
  the specification document, and the corpus's own lengths and digests. That last one is what
  makes "a case is added, never regenerated" a build failure: regenerating a case means
  editing a digest a reviewer can see.
- **The read set** — `frame::reads_format_version` is the format versions this firmware
  reads and `FORMAT_VERSION` is the one it writes. Both decoders take their answer from the
  predicate, and `tests/frame.rs` and `tests/bank.rs` each hold theirs to it over all 256
  values a version byte can hold. At v1 the set is a single value, so the mechanism costs
  nothing and reads as an equality; it stops reading as one the day a transition firmware
  widens it, which is the day a range that reached one decoder and not the other would be
  found by a fleet rather than by a test.

Migration is §10's swap and nothing new: a bank is single-version by construction, so the
retiring bank is read at its own version and the installing bank written at the new one, and
steps 5 and 6 of the swap are the format transition — the seal programmed at 5 and durable
at 6. ADR 0037 states the fleet end to end, and says plainly that the rollout is one-way
while it runs.

## What the boards still owe

Two rungs have an exit criterion no amount of host-side work discharges. Rung 0.2's is issue
[#27](https://github.com/madmax983/waymaker/issues/27): "power-cut loops pass on one Cortex-M0+
board and one Cortex-M4 board". Rung 0.5's is issue
[#34](https://github.com/madmax983/waymaker/issues/34): an `AtPersistentTime` deadline armed,
the supply removed entirely for longer than the interval, and the deadline recognised as
elapsed on the first replay after it. Nothing in this repository has ever run on a board.

That is a sentence a green CI would otherwise contradict, so it is a table.
`xtask::docs::HARDWARE_TARGETS` holds it and the `hardware-attestation` rule compares it
against this section in both directions — the same move
[the guarantees table](#the-guarantees-and-what-holds-each-up) makes for what
`waymaker-spec` still owes.

All 3 hardware targets, with the id to cite when a change touches one:

| Id | Target | Where it stands | What would discharge it |
| --- | --- | --- | --- |
| `cortex-m0plus` | power-cut and watchdog-reset loops on a Cortex-M0+ board | Not run | a rig log from a board, with the census complete and no breach. `waymaker-rig` is written to link on the target and has never been on one. The census completes on a host now, but against a model: no weak bits, no reset-cause register, no retained RAM, and a watchdog that lands at a call boundary rather than on a timer. |
| `cortex-m4` | power-cut and watchdog-reset loops on a Cortex-M4 board | Not run | the same log from a second core, because a rig that only ever ran on one part has measured that part rather than the protocol. |
| `rtc-power-loss` | an AtPersistentTime deadline across a total power cut on a board with a backed RTC | Not run | a board with a battery- or supercapacitor-backed RTC, the supply removed for longer than the interval, and the first replay after it recognising the deadline as elapsed. `waymaker-rig`'s `rtc` and `epoch` modules are written to link on the target and have never been on one. `waymaker-drive/tests/power_loss.rs` drives the scenario on a host, but against a model: no oscillator to drift, no supply to sag, and a continuity flag a test sets rather than a backup domain that failed. |

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
`crates/waymaker-rig/tests/matrix.rs`: one test per swept row, named after it with
`_on_the_rig`, which classifies every crash point the injector lists, resumes the run with
`Rig::resume`, and holds it to the row. Both run in the `verification` job as the `matrix`
stage. The rig half runs on the host through `waymaker-fault`; no board has run it, and
[the boards](#what-the-boards-still-owe) stay `Not run`. The rig reaches six rows; the "On the
rig" column says which, and `the_rig_fills_six_rows_and_names_the_seventh_as_its_gap` requires
the rig's census to refuse rather than to stop at six.

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

Row 5 does not hold as §14 writes it. It says "redeliver": a torn completion leaves no append
point ([ADR 0018](docs/adr/0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)),
so the driver and the rig both refuse the bank. The test asserts the refusal beside the two
halves that do hold: the torn completion is ignored and no partial bytes reach the workflow.
The run's continuation is §10's `continue_as_new`, a new run under a new id, so an effect
performed before the crash is performed again under another `(RunId, EffectSeq)`. That is the
duplicate `stable-redelivery` forbids, and it is issue
[#95](https://github.com/madmax983/waymaker/issues/95).

The four `Owed` rows are the rig's, not the model's: a swap workload, a capacity refusal and a
divergent replay are things this rig does not do — issue
[#96](https://github.com/madmax983/waymaker/issues/96). To move a row to `Swept`: name its
rig test in `FAILURE_ROWS`, reach it in the rig's census, and move the pinned gap test. The
same issue records what a board cannot do: rows 2, 3 and 4 are told apart by whether the
dispatcher was entered and returned, which the harness sees and a reset takes with the RAM.

## The layering

`waymaker-embassy` → `waymaker-flash` → `waymaker-core`, and never the other way. The table
is `xtask::policy::LAYERS`; the diagram is
[here](docs/architecture.md#crate-dependency-flow). Adding a crate to the workspace means
adding a row to that table — a member no rule covers fails `workspace-membership`.

The "May depend on" column is `may_depend_on` plus `may_depend_on_external` in
`policy::LAYERS`, rendered the way the gate renders it; the `claude-md` rule compares the
two. The façade's last five are the external half: only a non-default codec feature enables
any of them, no default build links one, and `codec-is-optional` is what keeps that true.

| Crate | Owns | May depend on |
| --- | --- | --- |
| `waymaker-core` | Borrowed record views, effect identity, replay cursor, transition rules, timer semantics and the clock-kind vocabulary, the workflow-version range and gate vocabulary, capacity errors | nothing |
| `waymaker-flash` | Stable wire encoding, the integrity-check trait and its shipped binding, the storage contract and its geometry, CRC and seals, the commit seal and the two-barrier write discipline, the two-bank layout, bank selection, append scanning, storage-backed recovery and the append offset, the capacity reserve, the seven-step bank swap and `continue_as_new`, compaction transition | waymaker-core |
| `waymaker-embassy` | `Ctx`, activity futures, dispatcher, wakeups, the persistent-clock capability, optional typed codec helpers | waymaker-core, waymaker-flash, cobs, postcard, serde, serde_core, thiserror |

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
- `waymaker-size-probe` — firmware linked only so its section sizes and its symbols can be
  measured. It
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
  line a violation is reproducible from, and issue #31's row vocabulary and census. Outside
  `default-members`, and nothing depends on it except `xtask`, which runs it to measure the
  write amplification it publishes. It is
  `#![no_std]` and allocation-free for a sharper reason than `waymaker-conformance`'s: a rig
  that could only run on a host would be a simulation wearing a rig's name, so the code that
  cuts the supply has to be code a board can link — which also means it may not keep what it
  knew in RAM, because RAM is the thing a power cut takes. See
  [ADR 0021](docs/adr/0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md).
  Issue #34 adds the two board clocks of §11 — `rtc`, an RTC in a battery- or supercapacitor-backed
  domain, and `epoch`, an epoch a network restores — and with them this crate's one dependency on a
  layer above `waymaker-flash`: `waymaker-embassy`, for the `PersistentClock` the two implement.
  The edge is legal because `dependency-direction` and `embassy-below-facade` both read
  `policy::LAYERS`, and this crate is not a layer — but it is a *normal* dependency on the one crate
  that may know about Embassy, so when rung 0.4 gives `waymaker-embassy` its Embassy dependencies
  the firmware-target stage that builds this crate begins linking them. See
  [ADR 0031](docs/adr/0031-a-persistent-clock-is-two-registers-and-the-board-run-is-a-checked-absence.md).
- `waymaker-drive` — issue [#28](https://github.com/madmax983/waymaker/issues/28)'s
  synchronous driver for §06's explicit kernel boundary, also
  `policy::TEST_SUPPORT_CRATES`. The workflow's half of the boundary and the world's, the
  loop that joins `waymaker-flash`'s recovery scan and two-barrier writer to
  `waymaker-core`'s transition table, §07's seven-step effect protocol (issue
  [#29](https://github.com/madmax983/waymaker/issues/29)), and a reference workflow the
  firmware target builds. §07 is here rather than in `waymaker-flash` because step 4 is an
  activity, and that crate's must-not-own cell names activities — see
  [ADR 0025](docs/adr/0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md).
  Outside `default-members`, and the only crate that depends on it is `xtask`, which reads
  §04's context term and the generated workflow future sizes out of §06's example rather than
  transcribing them — the same reason `xtask` depends on `waymaker-fault` and
  `waymaker-rig`. It is the third member of this
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

Design document §04. Every row but the last lives in `waymaker_core::budget` and is gated by
`cargo xtask size` — the numbers are in the kernel rather than in the gate, because a budget
in two places is a budget that ends up disagreeing with itself. Two of the rows are not §04's
own and say so: the context is a *term* §04 names inside runtime RAM, and the façade's
code-flash ceiling is a crate §04's row does not cover. The last row,
persistent flash, no longer has no gate behind it: `bank::BankLayout::new` refuses a device
of fewer than two erase blocks, which is §04's "two erase blocks minimum" as a build-time
refusal rather than a sentence — though it is still not a *measurement*, because there is no
linked image with banks in it. Nothing compares the numbers in this table to `budget.rs`, so treat
`budget.rs` as the source if they ever differ.

| Budget | Target |
| --- | --- |
| Runtime RAM | ≤ 768 B with a 512 B scratch page (§04, v0.1). Composed and gated since [ADR 0035](docs/adr/0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md): the scratch page, the kernel-state registry, the context, and the largest statics delta of any row |
| Kernel state | ≤ 128 B, excluding any page buffer (§04, v0.1) |
| Context | ≤ 128 B — what kernel state leaves of the 256 B the scratch page leaves of runtime RAM. Not a §04 row: §04 names the context as a runtime RAM term and nothing measured it before ADR 0035 |
| Incremental code flash | ≤ 13 KiB for core + flash adapter, on `thumbv6m-none-eabi` (§04 states 8 KiB as a **v0.1** target; [ADR 0017](docs/adr/0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md) raises it to 16 KiB for rung 0.2's two-bank lifecycle and [ADR 0020](docs/adr/0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md) to 18 KiB for §10's capacity reserve; [ADR 0029](docs/adr/0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md) cut it to 12 KiB once the gate stopped charging the size probe's own arithmetic, and [ADR 0036](docs/adr/0036-workflow-versioning-is-a-range-and-a-recorded-branch.md) takes it to 13 KiB for §08's versioning) |
| Incremental code flash, with the façade | ≤ 14 KiB for the three layers on `thumbv6m-none-eabi`. Not a §04 row either: §04 states the row above for "core + flash adapter", and [ADR 0035](docs/adr/0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md) gives the façade a ceiling of its own rather than raising the kernel's to pay for a crate above it |
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
lands at **18098 B** against the same 18 KiB gate, with no raise asked for. §11's timer
vocabulary then took it to **18386 B** against that same gate, leaving 46 B —
[ADR 0028](docs/adr/0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md), which
says plainly that issue #33's record bodies do not fit under it — a conclusion the next
paragraph corrects, because the figure it was drawn from was measuring the wrong thing.
[ADR 0022](docs/adr/0022-the-bank-swap-is-a-typestate-and-step-one-is-a-value-being-consumed.md)
records what it took, because the first measurement was 42 B *over* — a plan carrying a
geometry the region beside it already held, and five steps taking that plan by value to
compare one field of it. Both were real defects, and with a trim of the probe's own
arithmetic beside them they are 444 B.

Every number in that paragraph is a whole-image delta, and issue
[#72](https://github.com/madmax983/waymaker/issues/72) is that a third of each one is the
size probe. `cargo xtask size` now reads the symbol table and gates what it attributes to the
layers instead —
[ADR 0029](docs/adr/0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md).
Of rung 0.5's 18386 B, **7534 B is the probe's own arithmetic and 10852 B is the layers'**,
so the gate comes down from 18 KiB to **12 KiB** — a cut of 6 KiB, not a raise, and 1436 B
of room for issue #33's record bodies. They cost **1370 B** of it, so the layers then measured
**12222 B** of 12288 with 66 B left and no raise asked for —
[ADR 0030](docs/adr/0030-a-timer-is-a-boundary-and-its-clock-kind-is-on-media.md), which also
says plainly that rung 0.4 does not fit under it and needs issue #72's kind of accounting
rather than a third raise. Rung 1.0's versioning is the first thing that does not fit, and
[ADR 0036](docs/adr/0036-workflow-versioning-is-a-range-and-a-recorded-branch.md) raises the
gate to **13 KiB** on a figure the corrected accounting produced: §08's version boundary
costs **600 B** of layers, of which 176 B is the library change measured through the reach
the probe already had and 424 B is what `size-probe-reach` then demands. The layers measure
**12820 B** of 13312, with 492 B left. Every byte no symbol attributes to the probe stays
charged to the layers: `.rodata` strings, `compiler_builtins`, the `__aeabi_*` helpers and
the padding between functions. The report prints `Δflash`, `probe` and `layers` on every row
of every run, so the split is legible rather than taken on trust.

The workflow future is user memory and is reported separately, which since
[ADR 0035](docs/adr/0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md) is a section
of the size report rather than a sentence: `waymaker_drive::ota::WORKFLOW_FUTURES` is the
registry, the report prints it under a heading saying it is in no total above it, and a
report that names no future at all is `Unmeasurable` rather than a pass. A kernel state type
added to `kernel_state_types!` is asserted at compile time, registered in the size report,
and counted in the total — it cannot be in one without being in the others.

## What the engine does not allocate

Design document §02 decision 1 says the kernel is `no_std`, `no_alloc` and dependency-free.
Two of those three have been build failures since rung 0.0 — `crate-attributes` fails a crate
that drops `#![no_std]` or declares `extern crate alloc`, and `kernel-zero-dependencies` fails
one that grows a dependency of any kind. The third was an argument, and this file said so:
`no_alloc` rested on a crate having no way to *spell* an allocation, which is a fact about a
crate rather than about a linked image, and the routes by which `alloc` arrives without
anybody writing the words are exactly the routes nobody is watching.

`cargo xtask profile` is the measurement. Valgrind intercepts `malloc` in the binary, below
anything Rust can express, so it needs no global allocator and none of the `unsafe` this
workspace denies — which is what the argument against measuring this had always been. Four
workloads drive real library code over `waymaker-fault`'s model of NOR, one per part of the
engine the others do not reach:

| Workload | Drives | Reaches |
| --- | --- | --- |
| `journal` | §09's frame codec and commit seal, §10's reserve and the recovery scan, under the rig | core, flash, rig |
| `driver` | §06's boundary and §07's effect protocol, run to a terminal record | core, flash, drive |
| `facade` | §06's OTA example through `poll_ota` — `Ctx` and its four futures | core, flash, drive, embassy |
| `conformance` | §12's storage contract, as `waymaker-conformance` runs it | flash, conformance |

The "Reaches" column is measured rather than declared, and a run in which the four together
do not reach every gated crate fails. That is not a hypothetical: the first version of this
gate had two workloads and six gated crates, so `waymaker-embassy` — linked through
`waymaker-drive` and executed by nothing — and `waymaker-conformance` — not in the dependency
graph at all — were held to zero blocks in name while nothing looked at either. A crate no
workload executes scores the zero a deleted crate would score.

| Measured | Gate | Where it stands |
| --- | --- | --- |
| Heap blocks allocated by an engine crate | 0, by `profile::ENGINE_HEAP_BLOCKS` | 0 on all four workloads, against 16 blocks the harness and the runtime allocated in the same process |
| Every gated crate reached by some workload | all 6, or the run fails naming the gap | all 6 |
| Instructions executed in engine code | none — §04 states no target | `journal` 545 096 Ir over 8 effects; `driver` 41 788 over 2; `facade` 50 842 over 3; `conformance` 486 292 over 22 cases |

The engine is the three layers plus `policy::NO_STD_TEST_SUPPORT_CRATES`, derived from the
layering table rather than listed again, so a crate joining either category is gated without
anybody remembering a row. `waymaker-fault` is deliberately *not* in it: it models media in a
`Vec`, so engine code calling `program` on it reaches an allocation on every workload, through
the model rather than through anything a real driver would link. The innermost workspace frame
of a stack is what decides, and that column is printed rather than hidden, because "the engine
allocated nothing" is worth nothing beside a process that allocated nothing at all — a DHAT
run that saw no allocation anywhere is `Unmeasurable` rather than a pass, and so is a
callgrind run that attributed no instruction to any engine crate.

The instruction figures are **published and not gated**, for the reason the write
amplification is: §04 states no instruction target, and a ceiling invented here would be a
number nobody agreed to. They are also not a fact about a part. They are host instructions,
on the host's instruction set, under a cargo profile that is not the one a board is flashed
with, so nothing here converts into a cycle count on the hardware §04's budgets are stated
for — what they are good for is the comparison, because an instruction count is deterministic
where a wall clock is not. The boards owe the real figure exactly as
[the hardware table](#what-the-boards-still-owe) records for everything else.

Two decisions in the attribution are worth reading twice, and both were bought rather than
designed. A crate named in a *generic argument* is not the crate that wrote the code:
`with_capacity_in<waymaker_core::activity::ActivityKind, ..>` is `alloc`'s body with a kernel
type passed to it, and the first run of this gate failed a row over a four-byte `Vec` in the
harness beside it. And attribution reads the *source path* before the symbol, because a
function inlined into another crate keeps its own file in the debug info and loses its crate
from the printed name —
[ADR 0029](docs/adr/0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md)
records name-only attribution mistaking an inlined body for its caller as an accepted limit of
the code-flash gate, and here that would be this gate passing for the reason it exists to
catch. Both tools are therefore run with `--fullpath-after=`, and the profile the workload is
linked under turns fat LTO off. See
[ADR 0038](docs/adr/0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md).

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
- Errors: `thiserror` in libraries, `anyhow` in binaries — when either is reachable at all.
  `anyhow` is reachable nowhere; `thiserror` is reachable only from `waymaker-embassy`, and
  only with the `postcard` feature on, through `cobs`. No firmware crate names either.

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

All 53 rules `cargo xtask check-layering` can emit. The id is what appears in the failure, so
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
| `timer-record-fields` | `RecordRef::TimerScheduled` or `RecordRef::TimerFired` declares a field set other than `source::TIMER_RECORD_FIELDS`, in either direction — or the `enum RecordRef` body is gone, so the pin checks nothing. `effect-scheduled-fields`'s twin, and a rule of its own because the two settle different things: ADR 0011 settled how much metadata a scheduled effect carries, and this settles which facts about *time* reach media. Two fields are the point. §11 says a persistent timer record includes its clock kind "so recovery cannot silently reinterpret one policy as another": a record carrying the deadline alone decodes without error and means something else after a firmware change, and nothing downstream can tell. And `armed_at` is the monotonicity floor a persistent deadline is measured against — it lives in RAM, a power cut takes RAM, and this is what carries it across the reset. In the other direction a `remaining`, a `fired_at` or a payload on the firing is bytes on every timer for the life of the format. What it cannot see is a *width*: it compares names, so a `deadline` narrowed to a `u32` is `crates/waymaker-flash/tests/frame.rs`'s golden bytes. [ADR 0030](docs/adr/0030-a-timer-is-a-boundary-and-its-clock-kind-is-on-media.md). |
| `version-gate` | Design document §08's workflow versioning stops being the one that was reviewed, in any of its four halves. The *record* half: `RecordRef::VersionMarker` declares a field set other than `source::VERSION_MARKER_FIELDS`, in either direction, or the `enum RecordRef` body is gone or declared twice so the pin checks nothing. `effect-scheduled-fields`'s and `timer-record-fields`'s third twin. `version` is the branch, and without it the record says a decision was taken and not which one; `gate` is the field that reads as redundant beside the sequence and is not, because two gates that swapped places keep every sequence and the sequence check cannot see them. In practice the *compiler* is the first line of defence here — a field added or renamed breaks four exhaustive matches across three crates — and the pin is the second. The *surface* half: `waymaker-core/src/version.rs` gains or loses a public function `source::VERSION_GATE_SURFACE` lists. A `VersionRange::any()`, a `widen`, an `admits_or_default` or a `GateId::from_location` would each break no layering rule, need no dependency, and turn §08's "a firmware image that cannot replay the recorded version returns `IncompatibleWorkflow`" into a preference. The *shape* half: `VersionRange` is declared twice, stops being a braced struct, declares a public field, declares an associated constant, or declares a method set other than `source::VERSION_RANGE_METHODS` — read at *every* visibility; or the module declares a submodule, a `type VersionRange` alias, a `fn` outside the pinned `impl`, or a macro invocation inside it; or `waymaker-core/src/lib.rs` stops re-exporting `version::VersionRange` by that *source* name. `oldest <= current` is the whole invariant and every one of those is a way to give it back: review of this change ran all six and watched a three-check version print `ok` on each. The associated constant is `timer-capability`'s third recorded defeat, the module-scope `fn` is `dispatch-wiring`'s, the macro is the board half's, and the rename-plus-decoy is the one `timer-capability` grew a crate-root half for — met here rather than inherited. The *identity* half is §08's fourth rule: `waymaker-core/src/version.rs` or `waymaker-core/src/transition.rs` invokes one of `source::SOURCE_LOCATION_MACROS` — `file!`, `line!`, `column!`, `module_path!` — imports one under an alias, or names one of `source::SOURCE_LOCATION_CALLERS`, which is `core::panic::Location`'s route to the same three numbers with none of the macros spelled. §08 says source-location hashes are **not** stable identity, and the failure is the quiet one: a gate keyed on `line!()` changes identity when a comment above it moves, so a reformatting is a divergence and a moved function is a run that can never be replayed. Read with `#[cfg(test)]` modules removed, for `integrity-check`'s reason. What it cannot see is a function added from a *sibling* module — an `impl VersionRange` in `activity.rs` is invisible, the limit `capacity-reserve`, `recovery-surface` and `storage-contract` each record — and it cannot reach the modules where a `GateId` is *chosen*, which is where §08's fourth rule actually bites; [what is not checked](#what-is-not-checked) says so rather than leaving the ban looking exhaustive. It compares *names*, so an `admits` that stopped consulting its argument is `crates/waymaker-core/tests/version.rs`'s. [ADR 0036](docs/adr/0036-workflow-versioning-is-a-range-and-a-recorded-branch.md). |
| `wire-format` | Design document §09's frozen v1 format stops being the one that was reviewed, in any of its three halves. The *numbers* half: a row of `docs::WIRE_FORMAT_CONSTANTS` — twenty of them: the three magics, the two format-version numbers, the frame's header, trailer and check widths, the seal's pattern width and its mask, the erased byte, the widest payload, the four record-body widths, and the bank header's prefix, trailer and seal widths — is not declared, is declared twice, or is declared with another literal, in the file its row names. The literal rather than the value, because a rule that evaluated `HEADER_BYTES + TRAILER_BYTES` would be a second implementation of the arithmetic it checks. The *numbering* half: `waymaker-core`'s record module numbers a `RecordKind` differently from `docs::WIRE_FORMAT_RECORD_KINDS`, declares none of them, or declares one the table does not name — both directions, because a kind renumbered and a kind added are the same failure from two ends. A renumbering is the format break nothing else here can see: the encoder takes a kind's number from `RecordRef::kind` and the decoder matches the same constants, so swapping two leaves every round trip, every property test and every crash sweep green and makes every journal a shipped device wrote unreadable. The *specification* half: [`docs/format/wire-format-v1.md`](docs/format/wire-format-v1.md) is missing, or no line of it states a frozen constant beside its value or a record beside its number — one line rather than the whole document, because a bare `contains("1")` cannot fail for any document and `contains("4")` cannot fail for this one; or `CLAUDE.md` stops linking it or stops naming the corpus, or [ADR 0037](docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md) is missing or unaccepted. The *corpus* half: [`crates/waymaker-flash/tests/corpus/v1`](crates/waymaker-flash/tests/corpus/v1/README.md) is empty, or a file of it is missing, is another length, carries another digest, or is not named by `docs::WIRE_FORMAT_CORPUS_FILES` — which is what makes "a case is added, never regenerated" a build failure rather than a sentence in a README. What it cannot see is a *width* behind a name already on the list — a `deadline` narrowed to a `u32` changes no constant and no kind number — which is the corpus's and `crates/waymaker-flash/tests/frame.rs`'s golden frames'; and it pins three files — `frame.rs`, `bank.rs` and `record.rs` — which is the limit `capacity-reserve`, `recovery-surface` and `storage-contract` each record for the one they pin, met three times. [ADR 0037](docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md). |
| `integrity-check` | `waymaker-flash`'s checksum module stops using one of `source::INTEGRITY_CHECK_PARAMETERS` — a polynomial or an initial value — the right number of times inside the function that owns it; or it or one of its submodules grows an array — a `const`, `static`, `type` alias or local — outside `#[cfg(test)]`; or it is gone, so the pin checks nothing. Or the *binding* drifts: `waymaker-flash/src/integrity.rs` is gone; the integrity trait or the shipped `impl` is renamed, missing, or declared twice — a decoy above the real one is what a first-match scan reads; a seal in `source::SEAL_BINDINGS` stops returning the width §09's frame spends on it; or the shipped method body is anything but one unqualified call to the function that owns its algorithm, `fast::crc32(bytes)` included. Or the *routing* drifts, in any of the four files that have one. In `waymaker-flash/src/frame.rs`: a body pinned by `source::SEALING_FUNCTIONS` stops computing the seals its row names exactly once, or the file names `crc16` or `crc32` anywhere outside `input_digest` — the one documented exception, because a `const fn` cannot go through a trait method — or `decode_with` and `frame_len_of_with` stop verifying a header through `verify_header_with`, or the scan's `next` stops walking with `decode_with`. The rows are *derived* rather than whitelisted: a function generic over the check that no row pins is a body that can compute a seal and is pinned by nothing, and the scan that finds them reads joined signatures and generic `impl` blocks, because a `where` clause and a method in `impl<C: IntegrityCheck>` each escaped a one-line scan. The same in `waymaker-flash/src/bank.rs`, whose five sealing bodies each reach the seals their row in `source::BANK_SEALING_FUNCTIONS` names. And in `waymaker-flash/src/append.rs`, which is the writer: its `stage` must reach the codec through `frame::encode_with::<C>` — one call covers both the frame and its commit seal, because the seal is derived from the check the codec just computed — and it may name neither a checksum function nor a seal method. Without it, `frame::encode` in place of the generic sibling would seal every appended record with the shipped check whatever the recovery that positioned the writer verified with, which is a journal one half of a firmware can read. And in `waymaker-flash/src/recovery.rs`, which computes no seal at all: its two steps must reach the codec through `frame::decode_with::<C>` and `frame::frame_len_of_with::<C>`, and the file may name neither a checksum function nor a seal method — `Recovery<C>`'s parameter is a promise that a journal is verified with the algorithm that sealed it, and dropping both turbofishes passed every rule and every test before this existed. And in `waymaker-flash/src/swap.rs`, which installs a bank: its `stage` must reach the bank codec through `bank::encode_header_with::<C>`, `bank::seal_for_with::<C>` and `bank::encode_seal_with::<C>`, and the file may name neither a checksum function nor a seal method — a device whose two banks were sealed by two algorithms is a device only half of which boots. A trait nothing is obliged to call is a swap point that selects nothing. A firmware that sealed its banks with one algorithm and its records with another could read back neither half with the other's reader. [ADR 0012](docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md), and one rule id because it is one decision. [ADR 0010](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md) settles §16's first deferred question with measurements: the polynomial is free (52 B either way), the table is not (64 B for a nibble table, 1024 B for a byte table against an 8 KiB budget). A changed polynomial passes every round-trip test here and fails against every zlib in the world. |
| `storage-contract` | The public function surface of `waymaker-flash`'s storage module differs from `source::STORAGE_CONTRACT_SURFACE`, in either direction — or the module is gone, so the pin checks nothing. Design document §05 says a host or browser adapter "must not expand the firmware traits to accommodate host conveniences", and §12 is the trait it means: a `read_all`, a `flush`, a `write_at` or a `capacity()` shortcut would each break no layering rule, need no dependency, and turn a four-operation contract every port must implement into a surface only a host can afford. The pin compares names, so a widened offset or a validator that stopped validating is still a reviewer's job. |
| `recovery-surface` | The storage-backed recovery reader's public function surface differs from `source::RECOVERY_SURFACE`, in either direction — or the module is gone, so the pin checks nothing. §02 decision 2's "no `Journal::get(id)` and no in-memory event index" is a rule about the reader that touches media as much as about the cursor: a `seek`, a `resume_at` or a `read_all` would each break no layering rule and turn a forward scan whose RAM is one caller-owned page into one that seeks or holds history. One name is load-bearing for a second reason. `append_offset` is the only way an offset leaves the module and it answers `Some` only for a scan that ran to erased media; a second accessor returning the stopping offset regardless points at cells a program cycle has already cleared, and on NOR that bank never boots again. `waymaker-fault`'s sweep demonstrates that mutation rather than arguing it. |
| `commit-discipline` | The two-barrier writer's public function surface differs from `source::APPEND_SURFACE`; or the typestate that makes design document §07's order unrepresentable comes apart — the staged frame grows a second method or the word `program`, the sealable frame grows anything but `commit`, the sealable frame is constructed anywhere but inside `payload_barrier`, or that barrier stops calling `storage.barrier` exactly once. Issue [#24](https://github.com/madmax983/waymaker/issues/24) asks that "it is not possible to program a seal without the intervening payload barrier having returned", and a `compile_fail` doctest in the crate proves that of the code as it stands. This is what stops it being given back: a `Staged::commit`, a `Journal::write` that did all four steps in one call, or a second constructor for `Sealable` would each break no other rule and turn a protocol into a convention. What it cannot see is whether the barrier is a real one — that is §12's contract and `waymaker-conformance`'s across-reset witness. |
| `capacity-reserve` | §10's capacity reserve gains a public function `source::CAPACITY_SURFACE` does not list, in either direction — or the gate comes apart: `source::CAPACITY_GATE` declares no inherent `impl`, declares `stage` other than exactly once, or its `stage` does not *open* with `source::CAPACITY_ADMISSION_CALL` and go on to `source::CAPACITY_DELEGATION`. §10 says "the runtime never overwrites committed history to make room", and every way of giving that back is an *addition*: a `Reserved::stage_unchecked`, a `Reserved::into_journal` handing the ungated writer back, a `Reserve::none()`, or a `Reserve::for_bytes(tail)` taking the figure from its caller rather than from a `BankLayout` — which is the sharpest of the four, because a reserve is only a promise because a layout vouched for it. The order half is the other word §10 uses: scheduling fails **early**, and issue #25 asks that the failure "produce no mutation at all". §12 says a failed program may still have changed media, so the only refusal that changes nothing is one taken before the device is called. The decision must therefore be the body's **first** statement, not merely one that precedes the delegation — review of this change wrote an admission inside `if false`, inside a closure nobody calls, and guarded so that only `RunStarted` reached it, and watched a rule that only checked the order stay green on all three. The blocks are read for the named type rather than by finding the first `fn stage` in the file, because the surface half counts only *public* functions and a private decoy carrying the pinned call stood in for the real one. What it cannot see is the arithmetic: a `tail_bytes` that quietly stopped counting the outcome record is `crates/waymaker-flash/tests/capacity.rs`'s, where `a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding` drives the wrong reserve and watches a run reach a state it can never leave. |
| `swap-discipline` | §10's bank swap gains a public function `source::SWAP_SURFACE` does not list, in either direction — or its step order comes apart: a state in `source::SWAP_TYPESTATE` declares anything but the one method its row names, `Staged` names `program`, a value in `source::SWAP_CONSTRUCTIONS` is built anywhere but inside the body its row names, `payload_barrier` stops taking `source::SWAP_BARRIER_CALL`, or a row of `source::SWAP_ERASE_CALLS` stops erasing exactly the bank it names, before a barrier, without naming the other one. Issue [#26](https://github.com/madmax983/waymaker/issues/26) states §10 as seven steps and two recovery rules — "a crash before step 5 recovers the old run, a crash after step 6 recovers the new run" — and every one of those is a statement about *where the barriers are*. A `Prepared::commit` skipping the header, a `Staged::seal_now` skipping the payload barrier, an `Installed` built anywhere but in `commit`, or a `Swap::install(bank)` taking the bank to erase from its caller would each break no other rule and turn a protocol into a convention. The erase rows are the sharpest: which bank a swap clears is derived from the authority the device booted, and a `prepare` that erased the *retiring* bank is a device clearing the run it is executing. What it cannot see is whether the barriers are real, which is §12's contract and `waymaker-conformance`'s across-reset witness, nor whether the crash windows behave — that is `crates/waymaker-fault/tests/swap.rs`, at every crash point of all seven steps. |
| `ctx-facade` | Issue [#35](https://github.com/madmax983/waymaker/issues/35)'s façade stops adding sugar and starts adding authority. `waymaker-embassy/src/ctx.rs` or `waymaker-embassy/src/journal.rs` gains or loses a public function `source::CTX_SURFACE` or `source::CTX_JOURNAL_SURFACE` lists; `ctx.rs` declares a future `source::CTX_FUTURES` does not name, or a number of `fn poll` bodies other than that list's length; `Ctx` declares a method `source::CTX_SURFACE` and `source::CTX_PRIVATE_METHODS` do not list between them — read at *every* visibility — or an associated constant; **any** file of the crate names one of `source::CTX_FORBIDDEN_VOCABULARY` — `StableStorage`, `Reserved`, `RecordRef`, `Recovery`, `ReplayMachine`, `BankLayout`, `Swap` — declares a `static`, declares a `macro_rules!`, or implements `Future` for a type `CTX_FUTURES` does not name; or a `waymaker-drive` module outside `source::FACADE_DRIVER_MODULES` names one of `source::FACADE_FREE_VOCABULARY`. §05's must-not-own cell for this crate is "on-media authority or hidden global state", and every way of giving that back is an *addition*: a `Ctx::record` that appends for itself, a journal method that answers a question the workflow never asked, a `static` buffer two runs share. Each would break no layering rule — `waymaker-embassy` may depend on `waymaker-flash`, so nothing else stops the façade reaching a writer — and pass every test, because the run still completes. The surface pin sets `poll` aside, because four futures declare it and a pin that is a list of names cannot speak about a name declared four times; `CTX_FUTURES` holds the count instead, and the *set* of types the crate implements `Future` for beside it, so a fifth future is a line a reviewer writes wherever it is declared. The vocabulary, `static`, macro and future-set bans read every file of the crate rather than the two the surfaces are pinned in, because those four are statements about the crate: review of this change put a renamed `StableStorage`, a `pub static AtomicUsize`, a `macro_rules!` expanding a tenth public method into `impl Ctx`, and a fifth future in `dispatch.rs` — one file over — and watched a two-file version stay green on all four. Codex round 4 found the fifth, and it is about the *reader* rather than the rule: the future-set scan tested `starts_with("impl")` on the raw line while every classifier beside it stripped attributes first, so `#[rustfmt::skip] impl Future for SignalFuture` — a spelling `cargo fmt` preserves — walked past the one check that looks outside `ctx.rs`. The driver half is the fast half of issue #35's second "done when": every `waymaker-drive` module but the three in `source::FACADE_DRIVER_MODULES` is held to naming none of `source::FACADE_FREE_VOCABULARY`, so a module added tomorrow is covered without anyone remembering a row. The half a *compiler* decides is the `drive-facadeless` stage, which builds the crate under `without-facade` — with `facade.rs`, `ota.rs` and their four lines in `lib.rs` gone — for the firmware target. Both exist because a scanner cannot see an import routed through `crate::facade`, or a dependency renamed in a manifest, and a compiler sees each at once. What the stage does *not* establish is that the crate would build with `waymaker-embassy` deleted: the manifest entry is not optional, so a compile error inside the façade fails it too — issue [#106](https://github.com/madmax983/waymaker/issues/106). What it cannot see is a public *function* added from a sibling module — the two surfaces are pinned in one file each, exactly as `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one they pin — and it compares *names*, so a `Ctx::payload` that started handing back the journal's buffer is `crates/waymaker-embassy/tests/ctx.rs`'s. The `static` scan sets the `'static` *lifetime* aside before it looks for the identifier: the rule is about a `static` **item**, and `&'static str` is what compile-time metadata is spelled as — issue #36's activity names. An item is `static NAME:` and never `'static`, so the narrowing loses nothing, and `a_static_item_beside_a_static_lifetime_is_still_reported` is what says so rather than leaving it argued. [ADR 0032](docs/adr/0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md). |
| `dispatch-wiring` | Issue [#36](https://github.com/madmax983/waymaker/issues/36)'s dispatch path stops being a number. `waymaker-embassy/src/dispatch.rs` or `waymaker-embassy/src/wiring.rs` gains or loses a public function `source::DISPATCH_SURFACE` or `source::WIRING_SURFACE` lists, or declares one of them twice so the pin can no longer speak about it; a type in `source::WIRING_TYPE_METHODS` — `Activity`, `Table` — declares a method set other than its row's, read at *every* visibility, stops being a braced struct, or declares a public field; or a body in `source::WIRING_SELECTION_BODIES` is declared other than exactly once or names one of `source::WIRING_SELECTION_FORBIDDEN`; or either module is gone, so the pin checks nothing. Issue #36 states two of its work items as absences — "numeric `ActivityKind` on the dispatch path", and "no dynamic workflow loading and no string-addressed activity registry" — and every way of giving either back is an *addition*: a `Table::by_name`, a `Table::register`, a `pub rows` field a caller can rewrite at run time, a lookup that falls back to a label. Each would break no layering rule, need no dependency, and pass every test in the workspace, because the run still completes. The selection half is read out of the file rather than out of an `impl` body, because `poll_dispatch` is a *trait* method, which `inherent_impl_bodies` skips. The declaration is counted first, for `effect-protocol`'s reason: `braced_body` takes the first match, so a decoy above the real one is what a first-match scan reads. Four of the halves are things review demonstrated rather than things anybody predicted, and each was watched passing on a mutation before it was closed: a free `pub(crate) fn by_name` at *module* scope, which is on neither a surface pin nor a method pin — and which the label ban does not catch either, because `names_identifier` reads `by_name` as one identifier; a `register` beside it, which is the dynamic-loading non-goal as a free function; a `mod shim { pub struct Table {} }` above the real one, whose empty body is what the public-field scan read; and `Activity::name` renamed to `label` with the accessor left in place, which frees a selection body to compare it and names nothing forbidden. The function pin therefore reads every `fn` in the file at every visibility, and the field pin compares names as well as visibility — which is also what refuses a tuple struct, since one has no braced body of its own for the field scan to read. What it cannot see is a function added from a sibling module — it pins two files, exactly as `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one they pin — and it compares *names*, so that a label never reaches media is `crates/waymaker-drive/tests/dispatch.rs`'s, which reads the device image back with a needle short enough to fit a record. [ADR 0033](docs/adr/0033-the-dispatcher-answers-in-a-bound-the-journal-states.md). |
| `codec-is-optional` | Issue [#37](https://github.com/madmax983/waymaker/issues/37)'s codec helpers stop being optional. A `waymaker-embassy` module other than `source::CODEC_PATH` names one of `source::CODEC_VOCABULARY` — `serde`, `postcard`, `Serialize`, `Deserialize`, `DeserializeOwned`, `Coded`, `Format`, `Postcard`, `FromPostcard`, matched as *identifiers* over code with its comments and `#[cfg(test)]` modules removed; an item of the codec module that names one of them — anywhere in the item, not only on its declaration line — carries no bare `#[cfg(feature = ..)]`; `source::CODEC_FREE_TRAIT` is missing or is itself behind a feature; a dependency in `source::CODEC_DEPENDENCIES` is declared without `optional = true`; or a feature in `source::CODEC_FEATURES` stops enabling what its row names. §02 decision 4 says Serde and Postcard are "optional conveniences, never wire-format requirements", and the way that is given back is not a dependency — it is a *bound*: a `Ctx::activity` asking for `DeserializeOwned`, or a `Handoff` naming a codec type, makes every workflow carry the codec whatever the manifest says, and every other rule stays green because the run still completes. The manifest half is the other one that fails silently: a `serde` declared without `optional` links in every build, and the size report's row for it then measures an image that already had it. What it cannot see is a codec named from a sibling crate, or a bound written without one of those words — a type alias for `DeserializeOwned` declared in the codec module and used in `ctx.rs` names nothing forbidden. It reads *items* rather than lines, because review of this change wrote a `pub struct Bridge {` whose declaration line named no codec and whose field below it did, and watched a line-based version stay green. Two things it does not read: which feature gates an item — `code_only` removes string literals along with comments, and any single positive feature gate keeps the item out of a default build, which is the whole of the claim — and a compound `#[cfg(all(..))]`, `any(..)` or `not(..)`, which is *not* read as gating, so an item behind one is reported rather than trusted. It pins one crate and one module of it, the way `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one file they pin. [ADR 0034](docs/adr/0034-a-codec-is-a-bridge-behind-a-feature-and-the-probe-mirrors-it.md). |
| `rig-oracle` | `waymaker-rig`'s oracle or its census gains a public function `source::RIG_AUDIT_SURFACE` or `source::RIG_CENSUS_SURFACE` does not list, in either direction — or either file is gone, so the pin checks nothing. A rig is the one piece of code here whose bugs are *invisible*: a firmware bug shows up as a failing test, a rig bug as a passing one. Every way of giving the instrument back is an addition — an `Audit::assume_passed`, an `Audit::ignore`, a `Breach::suppress`, a second `finish` taking the authority count as advisory, a `Coverage::force_complete`, a `Gap::ignore` — and each would break no other rule, need no dependency and pass every test that exists. The census is a file of its own rather than part of `phase.rs` for this rule's sake: `Phase` and `ResetCause` each declare an `index`, a `from_index` and a `name`, and a pin that compares names cannot tell two such declarations apart. What it cannot see is whether the oracle's arithmetic is right — `crates/waymaker-rig/tests/teeth.rs` is what holds that, with two writers wrong in one way each and a control writer required to pass. |
| `transition-surface` | The replay machine's public function surface differs from `source::TRANSITION_SURFACE`, in either direction. Issue #15 asks for divergence that is "terminal and loud: no reinterpretation of history, no best-effort recovery", and every word of that is an *absence*: a `reset`, a `clear_divergence`, a `force` flag on `intent` would each break no other rule and turn "stop, never guess" into a suggestion. A test cannot call a function that is not there, so the surface is pinned instead. |
| `timer-capability` | Design document §11's timer semantics stop being the ones that were reviewed, in any of its four halves. The *kernel* half: `waymaker-core/src/timer.rs` gains or loses a public function `source::TIMER_SURFACE` lists, or a type in `source::TIMER_TYPES` — `TimerSpec`, `ClockCapability`, `Deadline` — is declared twice, is gone, or declares a member set other than its row's. It also pins each type's *methods*, at every visibility (`source::TIMER_TYPE_METHODS`), and refuses a public field on a type in `source::TIMER_BRACED_STRUCTS`; and it checks that `waymaker-core/src/lib.rs` re-exports each pinned type. The *façade* half: `waymaker-embassy/src/clock.rs` gains or loses a public function `source::CLOCK_SURFACE` lists, names one of `source::CLOCK_FORBIDDEN_VOCABULARY` — `AfterBoot`, `BootOnly`, matched as *identifiers* over code with its comments stripped — or names a `TimerSpec` that is not `source::CLOCK_SPEC_CONSTRUCTION` — as a name and not a prefix — or names none at all. The crate-root half compares the *source* name of `pub use timer::…`, so an alias or a path through a submodule is not the pinned type. The *board* half reads the two modules `source::BOARD_CLOCK_MODULES` names — `waymaker-rig/src/rtc.rs` and `waymaker-rig/src/epoch.rs` — and fires when either gains or loses a public function its `surface` lists, declares a method its `methods` list does not have at *any* visibility, declares a public field on its driver type, declares any constant that is not a `const fn`, names one of `source::CLOCK_FORBIDDEN_VOCABULARY`, or names one of `source::BOARD_CLOCK_FORBIDDEN_VOCABULARY` — `TimerSpec`, `ClockCapability` — because a driver reports a reading and decides no policy. §02 decision 8 is that timer semantics match the hardware's clock and never pretend, and every way of giving that back is an *addition*: a `TimerSpec::best_effort(capability)`, a `Timer::arm_or_downgrade`, a `Timer::force_elapsed`, a `PersistentClock::now_or_zero`, a third `Deadline` meaning "cannot tell", or a second constructor for a persistent timer that takes a reading rather than a clock. Each would break no layering rule, need no dependency, and pass every other gate. The kernel half is three checks rather than one because review of that change defeated the version without them and watched the gate stay green: a `pub(crate) const fn arm_or_downgrade` on `impl Timer`, which a surface pin counting `pub ` and not `pub(` cannot see; a `pub spec` field on `Timer`, which adds no function and changes no member and makes the invariant the whole design rests on a value any caller can set; and a `pub const BEST_EFFORT: Self = Self::AfterBoot { ticks: 0 }` on `impl TimerSpec`, reached from the façade as `TimerSpec::BEST_EFFORT` behind an "epoch not restored yet" guard — no banned identifier, no changed surface, and a persistent deadline served by a clock that restarts on every reset. So the façade's spec pin is positive rather than negative: it must name a spec, and every spec it names must be the persistent one. Codex then found two more of the same shape, and both are tests: `TimerSpec::AtPersistentTimeFallback` walked past a `starts_with`, and `pub use timer::TimerPolicy as TimerSpec` — or `pub use timer::compat::TimerSpec` — satisfied a root check that only asked whether the identifier appeared. Review of the *board* half then landed the same three on it — a `pub(crate) fn counter_unchecked` on `impl Rtc`, a `pub registers` field on `Rtc`, and a `pub const ASSUME_HELD: Self = Self::Held` on `impl Continuity` — which is why the board half carries a method pin at every visibility, a public-field refusal, and a constant ban read over the whole module rather than over one `impl` body: the constant was declared on the *enum a driver answers with*, which no per-driver pin looks at. The member sets are a wire-format commitment as much as an API one — issue #33 puts the clock kind on media for the life of the format. Read with `#[cfg(test)]` modules removed, for `integrity-check`'s reason. What it cannot see is an `admits` that stopped consulting its argument or an `evaluate` that credited an interval it could not measure, which is `crates/waymaker-core/tests/timer.rs`'s; and each half pins one file, so a door added from a sibling module is a door the rule is silent about — including a `macro_rules!` in a sibling module invoked inside a pinned `impl`, which review of the board half landed and which expands to exactly the accessor the pin is written against. [ADR 0028](docs/adr/0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md). |
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
| `size-probe` | The size probe stops being the `#![no_std]`, `#![no_main]`, feature-gated firmware the size gate links — or it stops mirroring a layer feature under a feature of its own, so the row named after that feature links code the probe can reach none of. A probe cannot `#[cfg]` on another crate's feature, so `--features waymaker-embassy/postcard` would report the delta of an image nobody exercised, and no other rule would notice: the row is not identical to its base, because the probe's own constants already differ. |
| `size-probe-reach` | A layer grows a public function the probe does not reach, so no budget charges for it. |
| `gate-broken` | The gate's own expected values do not parse. A gate must not be able to silently uncheck one of its rules. |

### Documentation

| Rule | Fires when |
| --- | --- |
| `claude-md` | This file loses a must-not-own cell, a permitted dependency edge, a settled-decision id, a backticked gate rule id, a pipeline command, or its links to the decision record and the diagrams. |
| `recovery-spec` | The recovery specification and the four places it lives stop agreeing: a clause in `docs::SPEC_CLAUSES` is missing from this file, from [ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md), or from `crates/waymaker-spec/src/obligation.rs`; its row here does not carry the guarantee's words or the test target that discharges it; the count is wrong; the crate declares a clause the table never did; or the clause table is not where the gate looks for it. Issue #20 asks that a change to the record representation update the model and the invariants first, then the proofs, then the code. Nothing mechanical can check the *order* — this checks that the four never disagree, which is the part that fails silently. |
| `storage-conformance` | Design document §12's storage contract and the four places it lives stop agreeing: a clause in `docs::STORAGE_CONTRACT_CLAUSES` is missing from this file, from [ADR 0016](docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md), or from `crates/waymaker-conformance/src/clause.rs`; its row here does not carry the sentence or what discharges it; the count is wrong; the crate discharges a clause differently than the table does; the crate declares a clause the table never did; or the clause table is not where the gate looks for it. Two tables agreeing on the names of six things and disagreeing about what any of them costs is the failure worth catching, so ids and discharges are compared in both directions. What it cannot see is inside the crate: that a clause the table calls in-process is reached by a case is `crates/waymaker-conformance/tests/clauses.rs`. |
| `hardware-attestation` | Rung 0.2's board runs and the places they are recorded stop agreeing: a target in `docs::HARDWARE_TARGETS` has no backticked table row in this file, its row does not carry the headline or the status the table renders, the count is wrong, a target marked `Passed` has no accepted ADR carrying `docs::HARDWARE_ATTESTATION_MARKER` for it or has more than one, a target marked `Not run` is nevertheless claimed by an ADR, or an ADR attests a target the table never declared. What it cannot check is that a `Passed` row is *true* — the evidence is a log from a bench — only that the claim is a line in an accepted decision record rather than a status somebody flipped. |
| `failure-matrix` | Design document §14's failure-semantics table and the five places it lives stop agreeing: a row in `docs::FAILURE_ROWS` is missing from this file or from [ADR 0027](docs/adr/0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md), or its variant is answered with another id, or none, by the `fn id` body of `crates/waymaker-rig/src/matrix.rs` — pairs rather than a set, because two ids swapped between arms leave the set whole; it has no `#[test]` of its own name in `crates/waymaker-drive/tests/matrix.rs`, or that test's body never names its variant; a row the table calls swept has no `#[test]` of its rig name in `crates/waymaker-rig/tests/matrix.rs`, or that test's body never names its variant — the body rather than the file, because two tests with their names swapped keep every variant in the file; its row here does not carry the failure point, the test or the rig standing the table renders; the count is wrong; the rig answers a variant the table never declared; or one of the three files is not where the gate looks for it. A test under `#[ignore]` or `#[cfg(` is not a test. What it cannot see is whether a named test asserts the row's *behaviour*: that is each file's own census, which pins the count per row on the model and requires the rig's to refuse at the first owed row. |
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
- **Allocation, as a measurement — no longer.** This bullet used to end "a global allocator
  that counted allocations would need the `unsafe` this workspace denies", and that was true
  of the only mechanism it considered. `cargo xtask profile` needs no allocator at all:
  Valgrind intercepts `malloc` in the binary, and
  [what the engine does not allocate](#what-the-engine-does-not-allocate) is the number. What
  is still structural is the *reasoning* — `bounded-decoding` proves the decoder is total and
  stays inside its input, and `crate-attributes` and `kernel-zero-dependencies` fail a build
  over `extern crate alloc` and over a dependency of any kind. What is measured is four
  workloads on a host, between them reaching every crate the gate holds at zero. A path
  through engine code that none of them takes is a path nothing has watched allocate, which
  is the honest scope and is the next bullet.
- **Coverage of non-test code specifically.** llvm-cov instruments the test binary, so the
  85% floor is a floor on a diluted number. See
  [ADR 0001](docs/adr/0001-one-pipeline-table-and-a-per-crate-coverage-gate.md).
- **A layer body the optimiser inlined into the probe.** The code-flash gate subtracts what
  the symbol table attributes to `waymaker-size-probe`, which is the whole of issue #72's
  fix. Fat LTO can inline a layer's body into a probe symbol, and the subtraction then
  charges that body to the probe — the one direction in which the corrected figure
  *understates* the layers. The `facade` row shows the shape of it: `waymaker-embassy`
  carries no symbol of its own, because its `const fn` façade is inlined into the probe's
  call sites. Deciding it needs a call graph, not a symbol table. What errs the other way is
  everything unattributable — `.rodata`, `compiler_builtins`, padding — which stays charged
  to the layers.
  [ADR 0029](docs/adr/0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md).
- **Which layer a byte belongs to.** The gate attributes to the probe or to nobody, and
  everything that is not the probe's is the layers'. It is not a per-crate accounting: ADR
  0017 attributes rung 0.2's figure by symbol, ADR 0019 splits the writer out and ADR 0020
  the reserve, and all three are readings of a measurement rather than gates.
- **A trait's provided method, on either side.** `defining_crate` refuses the `v0` `Y`
  production — `<Self as Trait>::method` for a body the trait provides — because it names
  the self type first, so its first crate root is the crate that wrote the `impl` rather
  than the crate that wrote the code. Refusing charges it to the layers, which is right when
  the trait is a layer's and wrong when the trait is the probe's. The wrong case makes the
  gate stricter, which is the direction to be wrong in; reading it correctly would need most
  of the mangling grammar rather than one production of it.
- **That the corrected figure is stable under a different optimiser.** Rebuilt with
  `lto = false` the same two images attribute 12414 B to the layers rather than 10852 B, a
  14% swing and larger than the headroom under the gate. `release-profile` fails a build in
  which `[profile.release]` moves at all, so the settings cannot drift — but the number is a
  reading of one optimiser's output, not a property of the source.
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
  row in the model file and per swept row in the rig file. A test that kept its name and lost
  its assertions passes it, and so does a `Row::` variant mentioned in a string. What holds
  the behaviour is each file's census —
  `every_row_of_the_table_is_reached_and_the_sweeps_have_not_thinned` pins the count per row
  on the model, and the rig's requires its census to refuse at the first owed row — and the
  assertions are reviewed by people.
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
  above the layers, and the one crate that depends on it is `xtask`, which reads two numbers
  out of it. So it demonstrates that the boundary is
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
- **That a persistent clock stayed monotonic across a reboot.** `Timer::evaluate` refuses a
  reading below the reading the timer was armed at, and that floor lives in RAM. A power cut
  takes it, so a run that re-arms on the next boot has nothing to compare the new reading
  against: an RTC that moved backwards while the power was off is invisible here. Issue #33's
  `TimerScheduled` record is what carries the floor across a reboot, which is why that record
  has to hold the arming reading as well as the deadline.
- **An associated `const` on a timer type, and an aliased import of one.**
  `timer-capability`'s method pin reads `fn` declarations, so a
  `pub const BEST_EFFORT: Self = Self::AfterBoot { ticks: 0 }` on `impl TimerSpec` is
  invisible to it; and the façade's spec pin drops `use` declarations before it scans, so
  `use waymaker_core::timer::TimerSpec::BEST_EFFORT as PERSISTENT_SPEC;` removes the one
  occurrence that ties the alias to the type. Together they are a downgrade the gate passes,
  and `admits` does not backstop it either, because the constant resolves to a variant that
  already exists. Codex found it on the fourth review round of issue #32, and it is issue
  [#99](https://github.com/madmax983/waymaker/issues/99). `effect-protocol` and
  `kernel-boundary` read `fn` declarations too, so the blind spot is theirs as well. The same
  gap is why `ClockKind`'s numbers are unpinned, below.
- **That a pinned timer type is the type the crate ships.** `timer-capability`'s member pin
  reads a header string, so a rename that carries the crate root with it — `TimerSpec` becomes
  `TimerSpecV2`, a decoy `mod compat` keeps the pinned name and the pinned members — leaves it
  comparing a type nobody ships. Review of this change ran it. The crate-root half closes the
  careless version, and what closes the dangerous one is not the gate: `ClockCapability::admits`
  names every pair and uses no `_`, so a third policy is a compile error at the arm that would
  have permitted it. `kernel-boundary` shares the reader and the same limit.
- **`ClockKind`'s numbers.** The surface pin counts functions and the member pin names three
  enums, so `pub const AFTER_BOOT: Self = Self(1)` — the one thing in this module that reaches
  media — can be renumbered with the gate green.
  `crates/waymaker-core/tests/timer.rs` is what holds them until issue #33 makes them a wire
  format.
- **That a persistent deadline is measured by a persistent reading.** `PersistentTimer::timer`
  returns the kernel type and `Timer::evaluate` takes a bare `u64`, so a caller that goes
  around `poll` can judge a persistent deadline with a boot-clock reading and get a verdict
  rather than an error. The accessor exists because issue #33 records what was armed; `poll`
  is the path that carries the clock and its high-water mark, and nothing obliges a caller to
  take it.
- **That two instances of one clock driver are one clock.** `PersistentTimer<C>` carries the
  clock *type* that armed it, so a firmware with an RTC driver and a network-epoch driver
  cannot poll one timer with the other — two epochs are both `u64`, and comparing across
  them fires a durable deadline early or late. Codex found that on the first review round of
  this change. What the type cannot tell apart is two *instances* of one driver, or one
  driver re-created with a different epoch: it is the same limit `waymaker-flash` records
  four modules over for a `Geometry`, and it has the same shape of fix — bind the storage,
  or here the clock, by a borrow rather than by a type. A deadline that borrowed its clock
  could not be held across the wait it exists to describe, so this one is stated rather than
  closed.
- **That a firmware really has the clock it declares.** `ClockCapability::Persistent` is the
  firmware's word. `Timer::arm` believes it, exactly as `Swap::beginning` believes the two
  arguments it is handed. What the façade adds is a *witness* — `PersistentTimer::arm` takes
  a `&mut C: PersistentClock`, so on that path the declaration cannot be made without the
  hardware — and nothing obliges a caller to take that path. Joining the two by construction
  is rung 0.4's dispatcher, the same standing as "nothing obliges a future dispatcher to use
  the gated writer".
- **That an `AfterBoot` interval is measured against a clock that is really monotonic.** The
  kernel reads no clock: every reading is an argument. A boot clock that skipped, stalled or
  ran at the wrong rate produces deadlines this code cannot fault, because it has no second
  source to disagree with. That is design document §11's own division of labour — the clock
  is the driver's — and issue #34's board test is where a real one is measured.
- **How long a boot deadline really waits once a reset has happened.**
  `TimerSpec::rearmed_at` measures an `AfterBoot` interval from the lower of the recorded
  arming reading and the clock now, so the interval accrues within a power cycle and restarts
  across one — which is what stops a reset stranding the run for ever with
  `ClockWentBackwards` on a boundary §08 gives no way to close. What it cannot do is tell a
  reset from an in-boot re-drive once the new cycle's clock has climbed back past the old
  mark: there the interval accrues from that mark, and a 1000-tick deadline armed at 5000 is
  reached at 6000 ticks of the new cycle.
  `a_boot_deadline_carried_across_a_reset_waits_longer_than_it_asked_for` measures it rather
  than describing it. A boot clock offers no reset evidence at all, which is §11's own reason
  for calling this deadline not power-loss durable; a reset-cause register and retained RAM
  are a board's, and issue #34 is where a real one is met.
- **That a timer's capacity reserve is exact.** `Reserve::exit_bytes_after` prices a
  `TimerScheduled` at an `EffectScheduled`'s figure — the outcome record the run's bounds
  declare, plus a terminal record. A `TimerFired` has no payload, so it is never wider than
  that: the reserve holds back a few bytes more than a timer needs and never fewer, which
  refuses slightly early in the last moments of a bank's life. The approximation errs in the
  safe direction and is stated rather than discovered; pricing it exactly is a third term in a
  sum on a firmware with 66 B of code budget left.
- **That a firmware's declared clock capability is the hardware it has.**
  `Clocks::capability` and `ClockCapability` are the firmware's word, and `timer_intent`
  believes it exactly as `Swap::beginning` believes the two arguments it is handed. The
  witness is `waymaker-embassy`'s `PersistentTimer::arm`, which takes a `&mut C:
  PersistentClock`, and nothing obliges a caller to take that path. Joining the two by
  construction is rung 0.4's dispatcher — the same standing as "nothing obliges a future
  dispatcher to use the gated writer".
- **That a board's continuity register tells the truth.** `waymaker_rig::rtc::BackedRtc`
  asks a board two things: the counter, and whether the backup domain held. The driver
  believes the second, exactly as `Timer::arm` believes a declared `ClockCapability`. A board
  whose implementation returned `Continuity::Held` unconditionally would fire every
  persistent deadline on the device on the boot after its battery died, and nothing below the
  board can see it. That is what the `rtc-power-loss` row in
  [what the boards still owe](#what-the-boards-still-owe) is for.
- **That a board clock module is the only place a board clock's surface can grow.**
  `timer-capability`'s board half pins `waymaker-rig/src/rtc.rs` and
  `waymaker-rig/src/epoch.rs` — each driver's methods at every visibility, no public field on
  it, and no constant anywhere in the module. It reads those two files and no others. Review
  of this change put a `macro_rules!` in a sibling module of the same crate and invoked it
  inside `impl Rtc`; it expands to a public method returning the raw counter and the gate
  stays green. That is the same limit `capacity-reserve`, `recovery-surface` and
  `storage-contract` each record for the one file they pin, met once more.
- **That a monotonic tick is in the unit the restored epoch counts in.**
  `RestoredEpoch` adds a tick count to a restored reading and never converts, because a
  conversion needs a rate and a rate nobody checked is a clock that runs fast. A board whose
  epoch is seconds and whose timer counts 32 kHz has to divide before it answers
  `Monotonic::ticks`, and every arithmetic guard in that module passes on a reading 32768
  times too large. It is a board's obligation the way the continuity bit is.
- **That an RTC counter is wide enough for the deadline it is asked to measure.** The driver
  reports the register and never widens it: RAM did not survive the cut, so there is no epoch
  to widen it with, and inventing one is the substitution §02 decision 8 forbids. A counter
  that wrapped therefore reads below the arming reading the record carries, and the kernel
  refuses the interval rather than crediting a wrong one —
  `a_counter_that_rolled_over_is_refused_rather_than_credited` measures that. Whether a given
  part's counter can wrap inside a given deadline is arithmetic about a board, and the board
  is what has to do it.
- **That a device on the externally-restored-epoch path can reach its network.**
  `waymaker_rig::epoch::RestoredEpoch` answers `NotRestored` until the firmware has told it
  the time, and a driver reports that as a clock it cannot read — so the run suspends and the
  deadline is neither fired nor discarded. A device that never reaches the network never
  fires an `AtPersistentTime` deadline at all. That is the honest outcome for a device with
  no clock and no time, and it is not a thing this engine can improve on.
- **When a timer fired.** `TimerFired` carries a sequence and no body, so a journal says that
  a deadline passed and never when. Replay hands the workflow back the fact and nothing else,
  which is all `Boundary::wait` returns; a firing reading would be a second `u64` on media
  that nothing reads. It can be added later behind the same record number, which is what §09's
  forward-compatibility rule is for. ADR 0030 records the loss rather than hiding it.
- **A façade function added from a sibling module, and a `pub(crate)` one outside `Ctx`.**
  `ctx-facade` pins the *surfaces* of `ctx.rs` and `journal.rs` only, and pins methods at
  every visibility on `Ctx` alone. A `trait CtxExt` with a blanket impl in `clock.rs`, or a
  `pub(crate) fn` on `ActivityFuture`, is a door it is silent about — the same shape
  `capacity-reserve`, `recovery-surface` and `storage-contract` each record for the one file
  they pin. Its authority ban, its `static` ban and its macro ban do read every file of the
  crate, because those three are statements about the crate.
- **`waymaker_flash::append::Journal`, and the two types beside it.** The façade's own
  durable-half trait is called `Journal` too, and `names_identifier` compares names rather
  than resolving them, so the writer cannot be banned by name. Neither are `Staged` and
  `Sealable`. Reaching a writer still needs a `Reserved` or a `Recovery`, both of which
  *are* banned, so this is a hole in a list presented as exhaustive rather than an open
  door.
- **That `waymaker-drive` would build with `waymaker-embassy` deleted.** Its dependency is
  not optional, so the `drive-facadeless` stage still resolves and compiles the façade
  crate — a `compile_error!` inside the façade fails that stage too, which Codex round 3
  measured. What the stage establishes is the weaker and still useful claim: no module
  outside `facade.rs`, `ota.rs` and `provisioning.rs` *needs* the façade, because the crate
  compiles with those deleted. Making the dependency optional would take the façade out of
  the lint, test, docs and coverage stages, which all pass `--no-default-features`; moving
  the façade-naming modules into a crate of their own would say it in the dependency graph,
  and is issue [#106](https://github.com/madmax983/waymaker/issues/106).
- **That a workflow stops at its own ending, for a caller that is not an `async fn`.**
  `TerminalFuture` never resolves and every other future refuses once a conclusion is
  recorded, which is two mechanisms for one rule: a run that ended has no boundaries left.
  Both are `crates/waymaker-embassy/tests/ctx.rs`'s. Two things neither can stop. A caller
  that never asks — nothing obliges anybody to read `Ctx::conclusion` at all, and a caller
  that ignored it would report a run that did not end. And a caller that *cancels*: the rule
  holds only while the future that recorded the ending is alive, because `TerminalFuture`
  and `ContinueFuture` each keep their "I have done my side" flag in the future rather than
  in the `Ctx`. Poll a `complete`, drop it, poll a `fail`, and the second overwrites the
  first; poll a `continue_as_new`, drop it, and the `Ctx` is unconcluded with the run
  already asked to be replaced. No `async fn` reaches either — both futures are `Pending`
  for ever, so no straight-line code follows the `.await` — which is why this is stated
  rather than fixed at the round it was found. Codex round 5 of #105; issue
  [#107](https://github.com/madmax983/waymaker/issues/107), and the executor of issue
  [#110](https://github.com/madmax983/waymaker/issues/110) is what makes cancellation a thing a
  caller really does.
- **That the façade's journal is the driver below it.** `ctx-facade` pins two files in
  `waymaker-embassy` and six in `waymaker-drive`. It says the façade declares no authority
  and that the driver names no façade type; it cannot say that a given `Journal`
  implementation is honest. A journal that answered `Handoff::Replayed` from a buffer rather
  than from media would satisfy every rule here, and the façade would dispatch nothing.
  `crates/waymaker-drive/tests/ota.rs` is what runs the real driver under the real façade.
- **That a workflow future is small.** §04 says the workflow future is user memory and is
  *reported* rather than budgeted, and `cargo xtask size` now reports it: a section of its
  own, summed into nothing, with a line saying it is not part of the runtime RAM total above
  it. Nothing gates it, and a ceiling on a user's own state machine would be a limit on what
  a workflow may be rather than on what this engine costs. Two things are still owed. The
  figure is a *host* size, and the target's is rather smaller — 168 B against 104 B for
  `ota_update` — because a state machine holding borrows narrows where a pointer does; there
  is no exact check, since a future's size has no `const` value a firmware build can compare
  and reading it off the linked image needs a symbol attribute this workspace cannot declare
  without the `unsafe` it forbids. And the registry holds one workflow: §06's, which is the
  only `async fn` here. Issue
  [#38](https://github.com/madmax983/waymaker/issues/38) is where each further example joins
  it. A handle held across three boundaries is a discipline the OTA example demonstrates
  rather than one anything enforces.
- **That the workflow-future registry is complete.** `WORKFLOW_FUTURES` is a list somebody
  writes, unlike `kernel_state_types!`, which applies its own assertion to every type it
  registers. A second `async fn` workflow, or a second concrete `Ctx<_, D, J>`, joins neither
  the registry nor `assert_context_size!`, and no rule notices. `ota_update` is the only
  `async fn` workflow here today, so the registry is exhaustive as a fact rather than as a
  guarantee.
- **That a workflow future's growth is noticed between runs.** The size report's base-branch
  diff cannot read the base checkout's registry, for `kernel_state_change`'s reason — the
  head binary is the only one that can read a type size — so `runtime_ram_change` always says
  "not compared". A future that grew is visible in the run that measured it and nowhere else.
- **How deep the call chain goes.** Runtime RAM is now composed rather than sampled — the
  caller's scratch page, the kernel-state registry, the context, and the largest statics
  delta of any row, gated against §04's 768 B. Three of those four live on the stack, and
  what is still unaccounted is the *depth* of the chain holding them: a deeper one moves no
  writable section and no type size. The report says so where it prints the total rather
  than printing "runtime RAM: ok", and stack accounting needs a call graph.
- **That the façade registers a wakeup.** §05's Owns cell for `waymaker-embassy` names
  wakeups, and this crate registers none of its own: it plumbs the task's waker to
  `ActivityDispatcher::poll_dispatch`, which is the one thing that knows when the world will
  answer. Two paths therefore register nothing at all — a halted boot, because there is
  nothing left to wake, and a deadline that has not passed, because there is no in-boot
  sleep yet. `crates/waymaker-embassy/tests/ctx.rs` measures both with a counting waker
  rather than leaving them implied, and issue
  [#110](https://github.com/madmax983/waymaker/issues/110)'s in-boot sleep is where a
  hardware alarm arrives.
- **That `continue_as_new` does anything.** `waymaker-drive`'s `Boundary::continue_as_new`
  refuses with `DriveError::ContinueUnsupported`. §10's swap works on a *bank* and this
  driver is pointed at a `JournalRegion`, so it cannot name the bank a swap would install
  into. `ContinueFuture` is therefore a real future over a real boundary operation whose one
  implementation today is a refusal, and issue
  [#110](https://github.com/madmax983/waymaker/issues/110) is where the two are
  joined.
- **A dispatch-wiring function added from a sibling module, and what a name really reaches.**
  `dispatch-wiring` pins two files, exactly as `capacity-reserve`, `recovery-surface` and
  `storage-contract` each say of the one they pin: a `trait TableExt` with a blanket impl in
  `decode.rs` adds a `by_name` with the rule silent — `ctx.rs` is not the example, because `public_functions` counts a trait method as
  callable and `ctx-facade`'s own surface pin would catch it there. And it compares *names*, so that a row's
  label never reaches media is `crates/waymaker-drive/tests/dispatch.rs`'s, which drives a
  run and then reads the device image back for the label's bytes. What makes that true is
  §09's `EffectScheduled` — a sequence, a kind, a length and a digest — and
  `effect-scheduled-fields` is the rule holding it.
- **That the bound a journal states is the bound the run was priced against.**
  `Handoff::Dispatch` carries `result_bytes`, and the façade narrows the world's buffer to
  it. `waymaker-drive` reads the figure from the reserve that priced the bank, so the two
  cannot disagree there; another `Journal` implementor could state anything, and the façade
  would narrow to it. It is a precondition on the implementor, the same standing as
  `Swap::beginning`'s two unverified arguments.
- **That a `Format` implementor reads the bytes it was handed.** `codec-is-optional` keeps
  a codec optional and out of the boundary; it says nothing about what one does. An
  implementation that ignored `bytes` and answered a constant satisfies every rule here, and
  the workflow would replay a value the journal never held. Same standing as
  `Activities::perform`'s contract not to truncate its own answer, and stated for the same
  reason. `crates/waymaker-embassy/tests/codec.rs` holds what the shipped bridge and the
  shipped format do.
- **A codec named from a sibling crate, or a bound written without a codec's name.**
  `codec-is-optional` reads `waymaker-embassy`'s own files and compares *identifiers*, so a
  `pub type Owned = DeserializeOwned;` declared in `decode.rs` and used in `ctx.rs` names
  nothing forbidden, and neither does a codec reached through a `waymaker-drive` module. The
  same limit `capacity-reserve`, `recovery-surface` and `storage-contract` each record for
  the one file they pin.
- **That a proc macro contributes nothing to an image.**
  `dependency-direction-transitive` stops at a package whose library target is a proc macro
  and does not walk through it, because a proc macro is compiled for the build host. That is
  a claim about how Cargo builds one rather than something the gate measures, and it is why
  `syn`, `quote` and `proc-macro2` are absent from `waymaker-embassy`'s allowlist while
  `thiserror`, which really is linkable, is on it. A *direct* edge to a proc macro is still
  `dependency-direction`'s, which reads the manifest.
- **Which column a generic layer's bytes land in.** ADR 0029 already records that fat LTO
  can inline a layer body into a probe symbol. Issue #37's codec is the first code here that
  is *entirely* generic, so almost all of it lands in `probe`: of postcard's 208 B the
  symbol table attributes 198 B to the probe and 10 B to the layers. The whole-image
  `Δflash` is the honest figure for a generic codec — a firmware pays the same way, at its
  own call sites — and the `layers` column is not. Deciding it needs a call graph.
- **What a codec costs a firmware with more than one answer type.** `Format::read<T>` is
  generic, so each distinct result type instantiates its own deserializer. The
  `waymaker-embassy/postcard` row drives one `T` and reports 208 B; a second type takes the
  row to 542 B, so the marginal type costs 334 B. The row is a floor, not the figure a
  firmware with three answer types should plan against.
- **A `GateId` keyed on where the call is written.** `version-gate`'s identity half reads
  `waymaker-core/src/version.rs` and `waymaker-core/src/transition.rs` — the engine's half of
  §08's fourth rule — and that is *not* every place a source location could become identity.
  `GateId`'s field is public and the number in it is a **workflow author's**, chosen in a
  workflow module: `waymaker-drive`'s three examples, or a user crate the gate can never
  reach. Review of this change wrote
  `pub const UPGRADE_GATE: GateId = GateId(line!() as u16);` in `demo.rs` and the gate printed
  `ok`. What the ban buys is that the *engine* never derives identity from a source location;
  a workflow author who does is a review question, and no rule in this workspace can be
  otherwise.
- **An inherent `impl VersionRange` in a sibling module of `waymaker-core`.** The shape half
  pins one file, so a `pub(crate) const fn admits_anything` written in `activity.rs` is a way
  past every refusal in the versioning module with the gate silent. The same limit
  `capacity-reserve`, `recovery-surface` and `storage-contract` each record for the one file
  they pin, and review of this change ran it. A `trait VersionRangeExt` in the same place is
  caught, but by `size-probe-reach` rather than by this rule, which is a side effect worth
  not relying on.
- **That a workflow branches on a *recorded* version rather than on an ambient one.**
  `version-gate` bans the source-location routes and pins the vocabulary; it cannot see
  a workflow that reads `identity().versions.current()` and branches on it. That is not a
  fact about where a call is written, so no scanner reaches it, and it is the same limit §08
  records about determinism generally. What catches it is the divergence check at the next
  effect boundary, which is terminal —
  `crates/waymaker-drive/tests/versioning.rs::an_added_effect_with_no_gate_is_a_divergence`
  drives `Branching::ImageVersion` and watches it stop the run.
- **That a version range is one a fleet can actually deploy.** `VersionRange::admits`
  refuses a recorded branch outside the range, and nothing checks the *order* in which a
  fleet moves the two ends. An image that narrowed `oldest` past a version still on devices
  bricks those runs until an image that can replay them is deployed again; ADR 0036 records
  it as a cost rather than closing it, because the ordering is a release process rather than
  a property of a binary.
- **That a version gate is reachable through the façade.** `Boundary::gate` is
  `waymaker-drive`'s, and `waymaker-embassy`'s `Ctx` has four futures and no fifth. A gate
  writes a record and waits for nothing, so an async workflow that needs one reaches the
  synchronous boundary or does without. Issue #40 is `area:core`; joining the two is the same
  standing as "nothing obliges a future dispatcher to use the gated writer".
- **What a marker torn by a power cut leaves.** Nothing, which is the right answer and not a
  free one: the record was never durable, so the next boot re-decides the branch — and under
  a *different* image it may decide differently. The window is narrower than §07's effect
  window, because no physical effect follows a gate, and it is why `Boundary::gate` writes
  before it returns rather than after. `crates/waymaker-drive/tests/versioning.rs` measures
  the durable half; the torn half is a state no test can assert a branch about, because there
  is no branch.
- **That a corpus file is the byte sequence a v1 device really wrote.** The corpus was
  produced by an encoder written from the field list rather than from `frame.rs`, which is a
  cross-check between two independent implementations and not a reading off a device. One
  case is the artifact of that cross-check — `record-08-run-failed.bin` against
  `golden::RUN_FAILED` — and the rest of it left nothing behind. No board has written a
  journal, which is the same absence
  [what the boards still owe](#what-the-boards-still-owe) records for everything else.
- **That a record kind added later joins the corpus, if it is added without a `RecordRef`
  variant.** The census derives what a writer produces from `RecordRef`'s variants through an
  exhaustive `match`, so a reserved kind given a body is a compile error in
  `crates/waymaker-flash/tests/corpus.rs` and then a census failure. A number written to
  media by something that is not a `RecordRef` is outside it — and so is a macro-generated
  `pub const` in `record.rs`, which `wire-format`'s numbering scan cannot see either.
- **That `reads_format_version` is the *right* set, only that it is the range its two
  constants name.** Three `const` assertions in `frame.rs` pin the predicate's ends and
  `wire-format` freezes both literals, so the body cannot quietly lose a bound — review of
  this change deleted the lower one and watched the whole workspace stay green before they
  existed. What no rule can say is whether a version belongs in the set: that is
  [ADR 0037](docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md)'s
  reasoning about bodies, not a number.
- **A width behind a name the frozen table already carries.** `wire-format` compares
  *literals* against constant declarations and *numbers* against record kinds, exactly as
  `effect-scheduled-fields` compares names: a `deadline` narrowed from a `u64` to a `u32`
  changes no constant and no kind number. The corpus and `crates/waymaker-flash/tests/frame.rs`'s
  golden frames are what hold the layout.
- **A frozen constant declared in a sibling module.** `wire-format` reads
  `waymaker-flash/src/frame.rs`, `waymaker-flash/src/bank.rs` and
  `waymaker-core/src/record.rs`. A `const MAGIC` declared elsewhere and imported is a
  declaration the rule is silent about — the limit `capacity-reserve`, `recovery-surface`
  and `storage-contract` each record for the one file they pin.
- **That widening the read set is sound.** `frame::reads_format_version` is a range, and it
  is sound only while a later format version adds record kinds and changes none of the ones
  below it. A version that changed an existing body would have to be refused rather than
  admitted, and nothing mechanical can tell the two apart: the constant is one line and the
  reasoning is
  [ADR 0037](docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md)'s.
  It is a decision somebody takes, not a number somebody moves.
- **A downgrade that reclaims a run.** An image meeting a record kind newer than itself stops
  with `UnknownRecordKind`, recovery ends `Damaged`, and a damaged recovery is exactly the
  input §10 says to recycle — so `Swap::beginning` accepts it and the old image may
  `continue_as_new` over a run a newer image could have finished. Recovery is doing the right
  thing with the information it has; what it does not have is that the damage is a record
  from the future. `Ending` does not distinguish the two, and separating them is a variant
  and four exhaustive matches that would change no decision the driver takes. Rolling a fleet
  back past a record kind is a data-loss operation, and ADR 0037 is where that is written
  down rather than implied.
- **That the ordering of a format migration's two images was respected.** ADR 0037 ships the
  reading image before the writing one. A device that meets a `v+1` bank with a `v`-only
  reader has no authority at all, and no binary can check the order a fleet was upgraded in —
  the same standing ADR 0036 records for widening `oldest` before narrowing `current`.
- **A path through the engine that no workload takes.** `cargo xtask profile` measures four
  runs, and every *crate* it gates is reached by one of them — that much is checked, and a
  run where it stops being true fails. What is not checked is every *path*. `WORKLOADS`
  reaches §09's codec, §10's reserve, the recovery scan, §06's boundary, §07's protocol,
  §12's contract and the façade's four futures; it reaches no bank swap, no
  `continue_as_new`, no capacity refusal, no divergent replay and no timer — the same four rows
  [the failure matrix](#the-failure-matrix-row-by-row) calls `Owed` on the rig, met again one
  gate over. An allocation on one of those paths is an allocation nothing has watched for. It
  is a *sampled* gate where the specification's proofs are exhaustive, and saying so is better
  than a zero that reads as a proof.
- **That the profile the figures are measured under is the profile a board is flashed with.**
  It is not, and deliberately: `[profile.profiling]` turns fat LTO off so that a crate
  boundary survives for the attribution to read. `release-profile` still fails a build in
  which `[profile.release]` moves at all, so the two cannot drift into each other — but every
  instruction figure here is a reading of one optimiser's output at one setting, which is the
  standing ADR 0029 already records for the corrected code-flash figure.
- **What an allocation would cost, as opposed to that there is none.** The gate is a block
  count at zero. It says nothing about a firmware that links an allocator for its *own*
  reasons and passes Waymaker a buffer from it, which is a firmware author's decision and one
  this engine is written to allow.
- **Stack usage.** Section sizes cannot see a cursor that lives on the caller's stack, and
  the size report says so rather than implying otherwise. Neither can either tool here: DHAT
  is a heap profiler and callgrind counts instructions, so the depth of the chain
  [the budgets](#budgets) already say is unaccounted stays unaccounted.

## Status

Rungs 0.1 through 0.3 are done, and rungs 0.4 and 0.5 have begun. The three firmware crates exist so the layering is enforceable and the
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
become executable row by row. It now is. `waymaker_rig::matrix::Row` is the ten rows as a
vocabulary a board can link. `crates/waymaker-drive/tests/matrix.rs` is one test per row,
named after it: 542 crash points classified from the operation the crash interrupted and
cross-checked against the media, with the two bank rows driving the real swap and then
*booting the driver* on the bank `select` names, and the counts per row pinned.
`Rig::resume` carries a cut iteration on, continuing the witness the reset left, so the rig
observes the behaviour column and a part is judgeable at every point a reset can land.
`crates/waymaker-rig/tests/matrix.rs` classifies and resumes 434 crash points into five rows,
drives two runs for the sixth, and requires the rig's census to refuse at the seventh, which
is the honest shape of a rig with no swap workload. The `failure-matrix` rule holds the five
places a row lives to one table, and the `matrix` stage runs both halves as a check of their
own. One finding came out of writing the rows down rather than out of reading the code: §14
row 5 says a torn completion is redelivered, and under ADR 0018 it cannot be, in this bank or
by this rig; the table above says `continue_as_new` instead, which forfeits the effect's
identity, and issue #95 and
[ADR 0027](docs/adr/0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md)
record it.

Issue #32 opens rung 0.5, and what it asks for is one sentence from §11 made structural: a
delay that needs a clock which survives power loss is never quietly served by one that does
not. `waymaker-core`'s `timer` module is §11's vocabulary — `TimerSpec`'s two deadlines,
`ClockKind`'s two numbers, `ClockCapability`, `Timer` and `Deadline` — and it reads no clock,
because the kernel's must-not-own cell names one. Every reading is an argument.
`waymaker-embassy`'s `clock` module is the capability, one layer up for the reason
`StableStorage` is one layer up: `PersistentClock::now` reads hardware. `waymaker-flash` was
never a candidate — its own must-not-own cell names timers.
Three mechanisms hold three claims, and ADR 0028 is explicit about which. A `Timer` carries
the `TimerSpec` it was armed from and holds no second description of its deadline, so
`Timer::arm` has two answers and not three: a refusal, or a timer for that spec. The refusal
names the missing capability — `KernelError::NoPersistentClock`, "this firmware has no
persistent clock" — which is what issue #34 asks of it, and which `IncompatibleWorkflow` could
not say. And `PersistentTimer::arm` takes a `&mut C: PersistentClock` and is the only
constructor, so firmware with no clock cannot write the call at all: that is issue #32's
compile-time half. `evaluate` adds nothing anywhere — an interval is compared against a
difference, and the difference is only taken after the reading has been checked against the
arming reading — so no input wraps a deadline into the past or into a future that never
arrives, and a clock that went backwards is `ClockWentBackwards` rather than a credited or a
discarded interval. Both "done when"s are driven rather than argued:
`an_after_boot_timer_restarts_its_interval_after_a_reset` takes the reset that clears the boot
clock *and* the RAM the timer lived in, and requires the whole interval to be owed again after
1 040 ticks of total powered time; the persistent twin removes the power for longer than the
interval and requires the first look at the restored epoch to say `Elapsed`.
`timer-capability` is what stops the shape being given back, in both crates, and it refuses
two identifiers outright: a persistent-clock module that names `AfterBoot` or `BootOnly` is
either substituting one policy for the other or fabricating a permission.
Two things came out of this rather than out of reading the code. The `facade` row of
`cargo xtask size` measured 0 B and carried a standing notice asking for something to call,
because `waymaker-embassy` declared no code at all; it now reads 320 B and the notice is gone.
And the budget is the number worth recording: §11's vocabulary cost 284 B, 18102 B to
18386 B against the same 18 KiB gate and no third raise, which leaves 46 B — not enough for
issue #33's two record bodies, so issue #72 is now this rung's binding constraint rather than
a tidy-up. What is owed is written down: the backwards-clock floor lives in RAM and so is only
a floor within one arming, which is one reason #33's record carries the arming reading; and
nothing obliges a caller to reach the persistent capability through the façade, which is rung
0.4's dispatcher. See
[ADR 0028](docs/adr/0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md).

Issue #72 then answers the budget question ADR 0028 had just made this rung's binding
constraint, and the answer is that the gate had been measuring the wrong thing. §04 states
the code-flash budget for "core + flash adapter"; `cargo xtask size` gated the delta between
two linked images, and the larger one holds the size probe's `match` arms, folds and calls
as well as the layers'. That is not incidental: `size-probe-reach` requires a call for every
public function a layer declares, so the probe's share rises with the library's. It had
reached **7534 B of 18386 B** — more than a third of a budget stated for two crates, spent
by a third crate that ships nothing. `xtask::elf::symbols` now reads `.symtab` the way
`sections` reads the section header table, `size::defining_crate` reads the first crate-root
component of a mangled name — the crate that *declares* the body, so a generic
`waymaker-flash` defines and the probe instantiates stays the engine's — and the gate
charges the image delta less what the probe grew by. The corrected figure is **10852 B**,
and the budget comes down from 18 KiB to **12 KiB**, which is the first move of that number
in this repository that is not a raise. Two things err deliberately. Everything
unattributable stays charged to the layers, so `.rodata` strings, `compiler_builtins` and
the padding between functions are the engine's. And the split is *printed* as well as gated
— `Δflash`, `probe` and `layers` on every row of the table and of the JSON, every run, and
`probe` beside `layers` in the base-branch diff for the row the budget is held against — so a
reader is never asked to take the gated number on trust. The
matrix links with `strip` off to have symbols to read, which is the one thing ADR 0002
decided against; `check_symbols_are_not_measured` is why that is safe rather than assumed,
because it fails a run in which the image carries no symbol table or in which a symbol,
string or debug section is allocated. Everything that reads zero still fails closed: a row
that attributes nothing to the probe, or more to it than the image holds, is `Unmeasurable`
rather than a probe that cost nothing. What is owed is written down: fat LTO can inline a
layer body into a probe symbol, and that body is then charged to the probe — the `facade`
row is the visible case, where `waymaker-embassy` carries no symbol at all. See
[ADR 0029](docs/adr/0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md).

Issue #33 then puts §11's deadlines on media, which is the last thing §09's v0.1 record
table owed. `TimerScheduled` carries a sequence, a clock kind, a deadline and the reading it
was armed at; `TimerFired` carries a sequence and nothing else. The clock kind is the field
§11 asks for by name — "a persistent timer record includes its clock kind so recovery cannot
silently reinterpret one policy as another" — and the failure it prevents is the quiet one: a
firmware built without an RTC replaying a journal an RTC wrote, with no checksum failing and
no frame malformed. The arming reading is the field that reads as redundant and is not. A
persistent deadline's monotonicity floor lives in RAM, a power cut takes RAM, and ADR 0028
named this record as what would carry it across the reset.
An unknown clock-kind byte is not a policy at either layer: `TimerSpec::recorded` is total
with no wildcard arm, so the codec refuses such a body with `MalformedRecord` and the kernel
with `IncompatibleWorkflow` — a `_ =>` on that path is how a zeroed page becomes a boot
deadline. A recorded clock this firmware cannot service is `IncompatibleWorkflow` and never a
substitution, which is issue #33's fourth work item and the distinction from
`NoPersistentClock`: one is this firmware refusing what a workflow asks for now, the other is
this firmware refusing history that already exists.
Timers share the run's sequence space and its cursor — `ReplayCursor` gains one state beside
`AwaitingOutcome`, a timer takes its sequence from the same allocator an activity does, and a
run has at most one open boundary of either kind — which is #33's "one ordered history, not a
parallel timer table" as a representation rather than a convention. §08's five rows are asked
of a deadline through a second pair of calls, `timer_intent` and `timer_outcome`, and
`Intent`, `Resolve`, `Next`, `Outcome` and `EffectRequest` did not move: issue #28's "adding
a new record kind does not change this signature" survived the first two of the five bodies
it was written against, and `kernel-boundary` now pins all eight types.
Both "done when"s are driven rather than argued. `waymaker-drive` grows `Clocks` beside
`Activities` and `Boundary::wait` beside `call`, and reading a clock is the only thing that
driver does to timing hardware — so "replay of a fired timer re-arms nothing" is a counted
call, and `replaying_a_fired_timer_reads_no_clock_at_all` requires zero. The second flips the
kind byte *on media* and re-seals the frame with the real codec, so what recovery meets is a
frame a writer could have written: the firmware with the clock refuses it as a divergence and
the firmware without one refuses it as an incompatible workflow.
Two numbers are worth recording. The record bodies cost **1370 B** of the 1436 B ADR 0029
left for them, so the layers measure 12222 B of a 12288 B gate with 66 B to spare and no
raise asked for; and kernel state goes from 88 B to 104 B of 128, because a timer's recorded
state is 24 bytes where an effect's digest is 12. Both say the same thing about rung 0.4.
What is owed is written down: a `TimerFired` records no firing time, the capacity reserve
prices a timer at an effect's figure and so over-reserves by a few bytes, an `AfterBoot`
timer's recorded arming reading is meaningless after the reset that cleared its clock, and
this driver polls rather than sleeps — §11's in-boot sleep and a dispatcher that arms a
hardware alarm are rung 0.4's. See
[ADR 0030](docs/adr/0030-a-timer-is-a-boundary-and-its-clock-kind-is-on-media.md).

Issue #34 is rung 0.5's exit criterion, and it closes §11 with the two clocks a board really
brings. `waymaker_rig::rtc` is the first: `BackedRtc` is two register reads — the counter, and
whether the backup domain held — and `Rtc` is the driver over them. The continuity bit is the
one every part has and every abstraction drops, and it is the whole point. A backup domain
that lost power leaves the counter at its reset value, which is zero on most parts and below
every instant a workflow ever waits for, so a driver that reported the number would fire every
persistent deadline on the device at once with no checksum failing and no record malformed.
`Rtc::now` answers `RtcFault::ContinuityLost` instead, the counter is read *before* the
continuity bit so that a supply which sagged during the read is still caught, and
`timer-capability` now pins that module as well — an `Rtc::assume_held` or an
`Rtc::counter_unchecked` breaks no layering rule, needs no dependency and passes every other
gate. That pin is four checks and not one, and the extra three were bought the way the kernel
half's were: review of this change landed a `pub(crate) fn counter_unchecked`, a
`pub registers` field and a `pub const ASSUME_HELD: Self = Self::Held` on the enum the driver
answers with, and watched a surface pin stay green through all three.
`waymaker_rig::epoch` is the second, and it is what §11's "an epoch a network restores" means
as code rather than as a phrase. A restored epoch is anchored in RAM, a cut takes RAM, and so
`RestoredEpoch::now` answers `NotRestored` until the network has answered again. A device on
that path never fires a durable deadline early on the strength of a zero, and one that never
reaches the network never fires it at all. The anchor alone is not enough, and review of this
change is what said so: an anchor stops detecting a boot clock that reset the moment the clock
climbs back past the anchor tick, so a reading already given as 1400 is followed by an
`Ok(1100)` — and a deadline armed under that reading fires *late* rather than being refused,
because `Timer::evaluate` floors at the recorded arming reading. Propagating the anchor's own
failure out of `restore` was the mirror image, locking a device out of the re-sync that is the
remedy for the state it is in. One floor answers both: the highest reading produced or
anchored to, standing whether or not the anchor still evaluates.
Both drivers live in `waymaker-rig` rather than in the façade, for two reasons and the second
decided it: they are board support for hardware Waymaker does not ship, and a layer pays for
every public function it declares against §04's budget, which ADR 0030 left 66 B of.
Both "done when"s are driven.
`a_persistent_deadline_survives_a_total_power_cut_and_is_elapsed_on_the_first_replay` arms the
deadline, takes the supply away for longer than the interval and requires the first replay
after it to finish the run — and the cut is a *function boundary*, because
`power_loss.rs`'s `power_up` takes the media and the backup domain and builds the board, the
driver, the workflow and both buffers itself, so nothing else can cross it.
`a_board_with_no_persistent_clock_refuses_the_same_workflow` is the complementary image:
`NoPersistentClock`, whose message is "this firmware has no persistent clock", and a journal
holding the run's own record and nothing else.
What is *not* discharged is the thing the issue asks for first, and it is a row rather than a
claim: `rtc-power-loss` joins the two boards rung 0.2 owes, `Not run`. See
[ADR 0031](docs/adr/0031-a-persistent-clock-is-two-registers-and-the-board-run-is-a-checked-absence.md).

Issue #35 opens rung 0.4, and what it asks for is one line: the façade "must add sugar,
never authority". `waymaker-embassy`'s `ctx` module is [`Ctx`] and four futures — an
activity, a deadline, a new run, and the run's own ending — over two traits it declares and
neither of which it implements. `journal::Journal` is the durable half: `schedule` takes
design document §07 steps 1 to 3, `resolve` takes steps 5 to 7, and the split between them
is where §07 puts the world. `dispatch::ActivityDispatcher` is that world. So `Ctx` is
*only* the join, and every arm of it is a call to something else — which is what the
must-not-own cell asks for and what `ctx-facade` holds: a `StableStorage`, a `Reserved`, a
`RecordRef` or a `static` in either file fails the build.
There is no Embassy dependency, and that is a decision rather than an omission. §13 asks for
futures the executor polls, and a plain `core::future::Future` is one; a façade that pulled
in an executor to hand out four futures would be more than the adapter §02 decision 5 says
it is. `embassy-below-facade` still guards the edge if a later rung needs one.
Both "done when"s are driven rather than argued.
`crates/waymaker-drive/tests/ota.rs` runs §06's OTA example — three activities and a
completion, with the image crossing every boundary as an eight-byte handle — through the
real façade, the real driver and `waymaker-fault`'s NOR model: it completes, it dispatches
nothing on replay, a reboot mid-run redelivers the identity the schedule record committed,
and the synchronous `Activities` world is asked zero times. The second is structural rather
than behavioural: `Boundary`, `Driver` and §07's typestate name no `waymaker-embassy` type,
so the façade edge is two modules — `facade.rs` and `ota.rs` — plus their four lines in
`lib.rs` and the manifest entry. `waymaker-drive`'s `without-facade` feature deletes the two
modules and the `drive-facadeless` stage builds that configuration for the part, which is as
much of the claim as a compile can make while the manifest entry stands; `ctx-facade` is the
scan beside it, and it reads every module of the crate rather than a list, so a module added
tomorrow is covered. Issue
[#106](https://github.com/madmax983/waymaker/issues/106) is the rest of it.
Two things came out of this rather than out of reading the code. A dispatcher that answered
`Poll::Pending` after the schedule record was committed left the boot with no recorded
reason at all, because the façade tells the journal nothing on a stall and only the driver
holds the identity; the driver now reports the outstanding effect, which is what
`Performed::Pending` reports on the undivided path. And the budget is the number worth
recording: the façade costs **182 B** of code flash — the `facade` row goes from 12334 B to
12516 B of layers — while the *gated* row, which is §04's "core + flash adapter", does not
move at all. Issue #39 is where that row becomes a gate.
What is owed is written down: `continue_as_new` has no implementation that swaps a bank,
the dispatcher's ergonomic wrapper is issue #36's, the optional codec helpers are #37's, the
provisioning example and the generated-future measurements are #38's, and in-boot sleep and
a dispatcher that obliges a caller to go through any of this are rung 0.4's rest. See
[ADR 0032](docs/adr/0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md).

[`Ctx`]: crates/waymaker-embassy/src/ctx.rs

Issue #36 is rung 0.4's second item, and what it asks for is one sentence made structural:
the output "is written into a caller-owned buffer and the returned length is validated
against the bound before any record is written". The bound was the missing word. §10 prices
a bank against `Bounds`, `Ctx::new` asks for a buffer as wide as the *wider* of the run's two
bounds, and the façade was checking a reported length against that buffer — a different
figure, and one `Answer::Completed`'s own documentation contradicted. `Handoff::Dispatch`
now carries `result_bytes` beside the identity, filled from the reserve that priced the
bank, and the façade hands the world `out[..min(result_bytes, out.len())]`. So the validation is not a check
a later change can drop: a dispatcher **cannot** write past the bound, and a reported length
over it is `Answer::Exhausted` — a failure with no payload, so the run makes progress and no
part of the answer reaches the workflow. `waymaker-drive` re-checked and refused before this,
so no truncated record ever reached media through *that* driver; a `Journal` implementor that
believed the documentation would have written one.
The second thing owed was named in the trait's own doc comment: "a *typed* failure payload
has no route through this trait ... which is issue #36's to close". `poll_dispatch` now
answers `Poll<Result<Produced, Self::Error>>`, and `Produced::Failed(len)` is §09's bounded
failure payload — recorded, and replayed. `Err(E)` keeps its old meaning, which is a failure
with nothing recordable, for the implementor's log.
The third is the two work items that are absences. §13 sketches `async fn dispatch`, and
that cannot be honoured no-alloc — the future an `async fn` in a trait returns borrows the
dispatcher, `ActivityFuture` already holds that borrow, and storing it needs a
self-referential value. `waymaker-embassy`'s `wiring` module is the ergonomics instead: a
`const` table of `(number, name, fn)` rows, and `Table::over(world, rows)` is an
`ActivityDispatcher`. Selection is by number. A row's **name** is compile-time metadata a log
reads through `Table::name_of`, it is not on the dispatch path, and no record holds one —
which is issue #36's third work item and, with it, its two non-goals: there is no dynamic
loading and no string-addressed registry, because a lookup takes a `u16`. Both are held by
`dispatch-wiring` rather than by a paragraph: a `Table::by_name`, a `Table::register`, a
`pub rows` field or a selection body that reads a label each fail a build.
Both "done when"s are driven rather than argued.
`crates/waymaker-drive/tests/dispatch.rs` runs an async workflow over the real façade, the
real driver and `waymaker-fault`'s NOR model, with a run declaring four bytes of effect result
under eight of terminal payload — so "wider than the buffer" and "wider than the bound" are
two different failures and it drives the second: the journal holds a schedule, an
`EffectFailed` with no payload, and a terminal record saying the workflow observed zero bytes.
The second is the crash sweep beside it: at every point the injector lists, every effect the
façade dispatched has a recoverable schedule record. The typestate that makes that true is
rung 0.3's; what this measures is that the façade path does not go round it.
Two numbers. The **gated** row of `cargo xtask size` — §04's "core + flash adapter" — does
not move at all, staying at 12222 B of 12288; the `facade` row goes from 12516 B to 12618 B,
so the table, the bound and the two `Produced` shapes cost **102 B**. What is owed is written
down: an unknown activity number is recorded as a *permanent* failure, because the two
alternatives strand the run or spin for ever, and a third answer meaning "this firmware
cannot service this kind" is issue
[#111](https://github.com/madmax983/waymaker/issues/111); and in-boot sleep and the
`continue_as_new` join — which the docs used to point at this issue — are issue
[#110](https://github.com/madmax983/waymaker/issues/110). See
[ADR 0033](docs/adr/0033-the-dispatcher-answers-in-a-bound-the-journal-states.md).

Issue #37 is rung 0.4's third item, and it is §02 decision 4's second half: a codec is a
convenience, and enabling one costs a number. `waymaker-embassy`'s `decode` module gains two
features and nothing in the default build. `serde` is a *bridge that names no format* —
`Format` is one method, read `bytes` as a `T`, and `Coded<F, T>` is the `Decode` a workflow
names — so a firmware with its own codec implements one trait and gets `Decode` for every
type that codec reads, without enabling postcard and without this crate naming a format.
`postcard` is one `Format` above it. The bound is `DeserializeOwned` rather than
`Deserialize<'de>`, because `bytes` is the caller's buffer and the next boundary overwrites
it: a value that borrowed it would be a dangling read one `.await` later.
Neither is re-exported at the crate root, and that is the rule finding something rather than
a preference: `codec-is-optional` refuses a codec named in any façade module but
`decode.rs`, and the first version of this change had re-exported all four. §02 decision 4
is not given back by a manifest — it is given back by a *bound*, and a `Ctx::activity`
asking for `DeserializeOwned` makes every workflow carry the codec whatever the features
say, with every other rule green because the run still completes.
Both "done when"s are measured rather than argued. The default feature set pulls in no codec
— `empty-default-features` and the `firmware` stage are what say so, and §06's OTA example
still reads its eight-byte handle with `try_into`. And enabling postcard costs **208 B** of
code flash against the `facade` row, with the bridge alone costing 32 B — not additive, since
`postcard` enables `serde`, so the format above the bridge is 176 B of the 208. The *gated* row —
§04's "core + flash adapter" — reads 12220 B of 12288, two below where issue #36 left it,
which is a larger probe making the optimiser choose slightly differently rather than an
engine that shrank.
That second number needed the size matrix fixed before it meant anything.
`--features waymaker-embassy/postcard` enables the layer's feature and defines no `cfg` in
the size probe, so the probe could not write a call the row turns on: the row linked the
codec, reached none of it, and reported the delta of an image nobody exercised. The probe
now declares a *mirror* feature per layer feature, derived from both names rather than
tabulated, and `check_probe_mirrors` makes it compulsory three ways — a layer feature with no
mirror, a mirror that does not enable it, and a mirror the probe never `#[cfg]`s on each fail
the `size-probe` rule rather than producing a quiet row of zero. `matrix` never falls back to
the `<layer>/<feature>` spelling either, because `xtask size` runs before `check-layering` and
a fallback would print `ok` over an unmeasured row. Where the 208 B lands
is worth reading twice, and is written down rather than smoothed over: the symbol table
charges 198 B of it to the probe and 10 B to the layers, because this codec is very nearly
all generic and fat LTO monomorphises it into the probe's call site. That is ADR 0029's stated limit met for the first time.
Two decisions came out of review rather than out of writing it, and both are measurements.
`postcard::from_bytes` reads a *prefix* — it stops at the end of a value and never asks what
follows — so a firmware that narrowed a result type would read an old record as a plausible
wrong value on every boot, with no checksum failing; `Format::read` uses `take_from_bytes`
and refuses a remainder, at 28 B. And `Coded` carries no `Debug`, because one would forward
to `T`'s and pull `core::fmt` into a firmware that only wanted to decode: the row goes from
208 B to 3028 B. `Clone` and `Copy` are there, bounded on `T` alone.
Four CI stages are new, and they exist because every other stage passes
`--no-default-features`: `codec-lint`, `codec-test`, `codec-docs` and `codec-firmware` are
what lint this code, test it, build its documentation, and build it for the part. `cargo xtask coverage` runs
`--no-default-features` too, so none of it is in a coverage denominator, and the stages are
what stand in for that. One consequence is recorded rather than hidden: `thiserror` is now
reachable from a firmware layer, through `cobs`, when the feature is on — and
`dependency-direction-transitive` stops at a proc macro, so `syn`, `quote` and
`proc-macro2` are not, because none of them is in an image. See
[ADR 0034](docs/adr/0034-a-codec-is-a-bridge-behind-a-feature-and-the-probe-mirrors-it.md).

Issue #39 is rung 0.4's exit criterion, and what it asks for is that the budgets be *paid*
on the configuration that ships rather than on the one below it. Three of them were not.
The `facade` row was measured and gated by nothing, so a regression in `waymaker-embassy`
moved a number no build failed over. Runtime RAM was gated as `.data + .bss`, which for this
engine is 0 B — §04's sentence is "cursor, context, record header, and storage scratch", and
three of those four are not statics. And the *context* — `waymaker-embassy`'s `Ctx`, the one
term §04 names that no registry held — was measured nowhere at all, for five rungs, behind a
report line that honestly called its own figure "a floor".
All three are gates now. The `facade` row is held to `FACADE_CODE_FLASH_BYTES`, 13 KiB,
which is a ceiling of its own rather than a raise of the kernel's: §04 states the 12 KiB for
"core + flash adapter" and the façade is a third crate, so paying for it out of the engine's
number is how a kernel budget widens for a cost the kernel does not carry. A `const`
assertion refuses a façade ceiling below the engine's, because the façade image strictly
contains the engine one, and any *other* gated row falls back to the stricter of the two.
Runtime RAM is now composed rather than sampled — the 512 B caller-owned scratch page, the
kernel-state registry, the context, and the largest `Δram` of any row — and the sum is
what is held to §04's 768 B. Each term keeps a sub-budget, and `CONTEXT_RAM_BYTES` is what
kernel state leaves of `ENGINE_RAM_BYTES` rather than a number of its own, asserted at
compile time to partition it exactly: two independent shares can both pass while their sum
fails.
The context is measured on the types the firmware links —
`Ctx<'_, Downloader, Bridge<'_>>`, in §06's own example — and
`waymaker_core::assert_context_size!` beside it is the gate for the target the budget is
stated for, which the `drive-firmware` stage compiles. `Ctx` borrows everything it uses, so
its size is the same for every `D` and `J`; naming the pair the firmware really links is what
makes the figure a reading of this image rather than of a fixture.
The generated workflow future is §04's fourth ask and the one that had nothing at all behind
it: `WORKFLOW_FUTURES` is a `const` registry, the report prints it in a section of its own
under a line saying it is part of no total above it, and a test drives a 64 KiB future
through the gate and requires every gated number to stay where it was. Nothing budgets it —
§04 excludes user workflow memory, and a ceiling on a user's state machine is a limit on what
a workflow may be. Every one of these fails closed: no gated `facade` row, no runtime section,
no named future, or a term missing from the composition is `Unmeasurable` rather than a pass.
The numbers, all passing and with no ceiling raised to make them: code flash **12220 B** of
12288 on `default` and **12618 B** of 13312 on `facade`; runtime RAM **672 B** of 768, being
512 B of scratch page, 104 B of kernel state, 56 B of context and 0 B of statics; kernel state
104 B of 128; and `ota_update`'s future **168 B**, budgeted by nothing.
What is owed is written down. The context and future figures are *host* sizes, which is
`KernelState::measured`'s standing and the same argument — only pointers differ and a
`thumbv6m` pointer is narrower, so the host figure is an upper bound and gating it fails early
rather than late. On the target the same two types are 28 B and 104 B, so both reported
figures overstate the part by about a factor of two; the context has an exact check and the
future has none, because a future's size has no `const` value a firmware build can compare and
reading it off the linked image needs an attribute this workspace cannot declare without the
`unsafe` it forbids. And a stack frame is still nobody's: it moves no section and no type
size. See
[ADR 0035](docs/adr/0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md).

Issue #38 adds design document §06's second example. `waymaker-drive`'s `provisioning`
module is `provision`: wait for a persistent-time window, register — retrying up to three
times on failure, each attempt its own effect — then end with a completion carrying a real
token or a failure carrying a real reason. Three boundaries `ota_update` does not use: a
timer, a workflow-driven retry, and a terminal payload that is not empty. It also closes a
gap [ADR 0032](docs/adr/0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md)
named: `ota_update`'s input is a module constant no boot reads back, so nothing ties
`Workflow::identity` to what a run asks for. `Provisioning` carries its input as a field
instead, and `tests/provisioning.rs`'s sharpest test reboots with a different one and
requires `DriveError::NotThisWorkflow`.
Both "done when"s already claimed for OTA needed a second look, not just a first one for
provisioning. The future-size report now names two rows — `ota_update` and `provision` —
from `waymaker_drive::provisioning::WORKFLOW_FUTURES` read beside `ota`'s own, and
`cargo xtask size` prints both under one heading. And "exercised by the crash rig" was not
true of OTA before this: `tests/ota.rs` had reboot-by-hand tests but no `waymaker-fault`
sweep. `tests/ota.rs` and the new `tests/provisioning.rs` each carry one now, at every
crash point the injector lists, over the real façade and the real driver.
`poll_provisioning` is the concrete path the firmware build monomorphises, the way
`poll_ota` already was — `drive-firmware` links both. No new ADR: nothing here moves a
must-not-own cell, a dependency edge, or a rule id.


Issue #40 opens rung 1.0, and what it asks for is design document §08's four versioning
rules made enforceable. None of them was. `RunStarted` has carried a `workflow_version`
since issue #13 and the driver compared it for **equality**, so every run in flight was
refused the moment the binary changed — the opposite of §08's first rule, and the failure a
durable workflow engine exists to prevent. §09 numbered `VersionMarker` at 9 with no body
behind it, so the third rule had no mechanism at all.
Two mechanisms answer the four rules, because they are about two different questions.
`waymaker_core::version::VersionRange` is what an image declares it can replay — `oldest`,
the earliest recorded version whose branches this binary still holds, and `current`, the
version it writes into a new run — and `admits` is §08's second rule as a total function
with two answers and no third that means "close enough". `Identity::versions` replaces
`Identity::version`, and `begin` admits rather than compares. `NotThisWorkflow` stays and
now means only what it says: the journal belongs to some *other* workflow.
`RecordRef::VersionMarker { seq, gate, version }` is the other, and it is §09's kind 9 at
four payload bytes. `Boundary::gate` is the API: the first execution to reach a gate records
`VersionRange::current`, and every later boot is handed that number back whatever branch the
image running it would have chosen. Three properties make it more than a note beside
history. It **spends a sequence**, so a gate added, removed or moved shifts every boundary
after it and §08's third rule is enforced by the sequence check that already exists. It
**resolves itself** — there is no world between a gate's intent and its answer, because the
effect of a gate is a branch inside the workflow — so `version_intent` is one call where
`intent`/`outcome` and `timer_intent`/`timer_outcome` are two, there is no `VersionResolve`,
and kernel state stays at 104 B. And the **record is durable before the branch is
observable**: `gate` crosses §07's two barriers and returns the number afterwards, which is
§02 decision 3's shape at a boundary whose effect is a branch.
`Boundary::recorded_version` is the smaller companion — the version the run's own
`RunStarted` holds, written by nothing, and a fact about history rather than about the image.
§08's fourth rule is an *absence*, so it is a gate rule: `version-gate` fails a build over
the marker's field set, the versioning surface, `VersionRange`'s methods at every visibility
with no public field on it, and any invocation of `file!`, `line!`, `column!` or
`module_path!` in the three files a source-location hash would have to pass through. A gate
keyed on `line!()` changes identity when a comment above it moves, so a reformatting would
be a divergence.
Issue #40's third work item is driven rather than argued.
`crates/waymaker-drive/tests/versioning.rs` runs a run recorded at version N against firmware
for N+1, over the real driver, the real codec and `waymaker-fault`'s model of NOR, both with
a marker at the divergence point and without one: with it the run completes identically, and
without it the added effect is `NondeterministicWorkflow` with nothing dispatched and nothing
written. `Branching::ImageVersion` in the reference workflow is that mistake written down, so
the tooth is a value a test picks rather than a paragraph.
The budget is the number worth recording, and it is the first raise argued from a corrected
figure: §08's versioning costs **600 B** of layers — 176 B for the library change through
the reach the probe already had, and 424 B for what `size-probe-reach` then demands — so the
gate goes from 12 KiB to **13 KiB** and the layers measure 12820 B of 13312 with 492 B left.
What is owed is written down: a rollback that meets a newer recorded branch refuses the run
rather than guessing, and nothing enforces widening `oldest` before narrowing `current`; a
marker torn by a power cut leaves the branch undecided, so a *different* image may decide it
differently on the next boot; the façade has no gate future, so a gate is reachable through
the synchronous driver alone; and `waymaker-spec` does not model markers, because a
self-resolving record opens no boundary and §14's six guarantees are unchanged by it. See
[ADR 0036](docs/adr/0036-workflow-versioning-is-a-range-and-a-recorded-branch.md).

Issue #41 is the second thing rung 1.0 owed, and it is a promise rather than a feature:
after it, records a shipped device wrote stay readable by every later 1.x firmware. Three
things stood in the way of writing it down. The format was frozen in prose and nowhere else —
every test in `waymaker-flash` drives the encoder and the decoder together, so a kind
renumbered or a field reordered moves both sides at once and every round trip still passes.
A reader's obligation on a record kind it does not know was a *behaviour* rather than a
rule, and "stop" is only half of it: a reader that stopped and then appended, or stopped and
then truncated, loses a committed record while obeying the half that was written down. And
the read side was an **equality** — `decode` refused any version but `FORMAT_VERSION`, and so
did the bank header reader, so a fleet in a format transition could not be described in the
code at all. That last one is §08's `workflow_version` defect met one layer down, in the
bytes rather than in the workflow, and ADR 0036 had already fixed it above.
[`docs/format/wire-format-v1.md`](docs/format/wire-format-v1.md) is the format byte by byte;
the corpus is twenty-one files of frozen bytes produced by an encoder written from that
field list rather than from `frame.rs` — six of them wide, carrying a distinct non-zero byte
in every position of every multi-byte field, because the rest cannot say a width narrowed —
run as a CI stage of its own, because a red `corpus` says a byte a shipped device wrote is no
longer a byte this firmware reads, which is the one failure here whose blast radius is a
fleet rather than a branch. `frame::reads_format_version` is the
read set, both decoders take their answer from it, and each is held to it over all 256 values
a version byte can hold. It costs **0 B** of code flash: the layers measure 12820 B of 13312,
exactly where ADR 0036 left them. The `wire-format` rule is the third holder — the frozen
numbers, the record numbering in both directions, the specification document read a line at a
time, and the corpus's own lengths and digests — and it
was watched failing on every mutation its own test modules name, the sharpest being a
renumbered `RUN_STARTED`, a kind added to the kernel that nothing wrote down, a regenerated
corpus file, and the bank header reader reverted to the equality the predicate replaced —
which review of this change ran with the whole workspace green before the routing pin
existed. Migration is §10's swap
and nothing new: a bank is single-version by construction, so read-old/write-new is what two
banks already are, and steps 5 and 6 are the format transition. What is owed is written
down: the
corpus is a cross-check between two implementations rather than a reading off a board;
widening the read set is sound only while a version adds kinds and changes none, which no
rule can tell; and a downgrade past a new record kind may reclaim a run, because recovery
reads a record from the future as damage and §10 recycles a damaged bank. Issue #42's book is
where this document becomes a chapter. See
[ADR 0037](docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md).

`cargo xtask profile` then closes a limit this file had recorded about *itself*. §02 decision
1 is that the kernel is `no_std`, `no_alloc` and dependency-free; `crate-attributes` and
`kernel-zero-dependencies` had held the first and the third since rung 0.0, and the second was
an argument from crate attributes — which is a fact about a crate rather than about a linked
image, and this workspace accepts that shape of argument nowhere else. The bullet that said so
ended "a global allocator that counted allocations would need the `unsafe` this workspace
denies", and that considered one mechanism: DHAT intercepts `malloc` in the *binary*, so it
needs no allocator, no attribute and no exception to `unsafe_code = "deny"`. Four workloads
drive real library code over `waymaker-fault`'s model of NOR — §09's codec and commit seal and
§10's reserve under the rig, §06's boundary and §07's protocol run to a terminal record, §06's
OTA example through the façade's four futures, and §12's contract as `waymaker-conformance`
runs it — and the engine's heap is gated at **zero blocks**, in blocks rather than bytes
because `malloc(0)` returns a pointer. It measures zero on all four, against sixteen blocks the
harness and the runtime allocate in the same process, which is the column that says the tool
was watching. Callgrind runs beside it and is *published* rather than gated, for the reason the
write-amplification figure is: §04 states no instruction target. Everything fails closed — a
DHAT run that saw nothing, a callgrind run that attributed nothing to the engine, a workload
that completed no unit of work, a declared workload with no row, a gated crate no workload
reached, and per-function costs that do not add up to callgrind's own total, which is one check
that catches three different ways of misreading that format.
Four findings came out of writing it and reviewing it rather than out of reading the code, and
every one had produced a *plausible* number. Two are attribution: a crate named in a generic
argument is not the crate that wrote the body — the first run failed a row over a four-byte
`Vec` in the harness beside it, through
`with_capacity_in<waymaker_core::activity::ActivityKind, ..>` — and a body inlined into another
crate loses its crate from the printed symbol and keeps its source file, which is why the path
is read first and why the workload's profile turns fat LTO off. The third is the one that
matters most, and Codex found it: the gate named six crates and two workloads executed four, so
`waymaker-embassy` — linked and never run — and `waymaker-conformance` — not in the dependency
graph at all — each scored the zero a *deleted* crate would score. Deriving the gated list was
half a mechanism; the other half is that a gated crate nothing reaches now fails the run, and
the `facade` and `conformance` workloads are what make it pass. The fourth is that the
per-unit cost truncated below what was measured, contradicting its own doc comment — it ceils
now, and its test asserts the invariant across divisors rather than the literal it used to
assert, which is how the truncating version passed. What is owed is written down: every gated
*crate* is reached and that is checked, every *path* is not, and the paths it does not reach
are the four rows the failure matrix already calls `Owed`. See
[ADR 0038](docs/adr/0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md).


The kernel-state registry has three entries — the replay machine, the record view and an
armed timer — so the 128 B budget is a number about something, and 104 B of it is spent. The
async `Ctx`, the dispatcher, the codec helpers, the two examples and rung 0.4's exit
criterion are here — issues #35, #36, #37, #38 and #39, above — and in-boot sleep is the
rest of 0.4. The gates went in before the code they govern, which is the point: a gate
retrofitted after coverage has slipped is a gate that ratifies the slip.
