# CLAUDE.md

Waymaker is a firmware-first durable workflow engine for Rust. After a reboot, Waymaker
re-creates a workflow from its beginning and replays it deterministically through an
ordered journal. A completed effect returns its recorded result. The first unresolved
effect becomes the next piece of work.

This file states the rules a contributor — human or agent — works to: the invariants, the
layering rules, and what each crate must not own.

Most of it is checked, not just written down: the must-not-own cells, the permitted
dependency edges, the eight decision ids, the command list, the five deferred questions
and all 57 rule ids below are compared against the tables that own them, and `cargo
xtask check-layering` fails a pull request when this file and those tables disagree. The
rest is prose that people review by hand — [What is not checked](#what-is-not-checked)
says which parts.

- The architecture, drawn: [`docs/architecture.md`](docs/architecture.md)
- The book, for a reader rather than a contributor: [`docs/book`](docs/book/src/SUMMARY.md)
- Why things are the way they are: [`docs/adr`](docs/adr/README.md)
- The design document this is all taken from: [`docs/design/waymaker-design-v0.2.html`](docs/design/waymaker-design-v0.2.html)

## Run this before you claim anything works

This is every command CI runs, in order. The `claude-md` rule checks this list against
`xtask::pipeline::STAGES`. Add a stage to the pipeline and forget it here, and the build
fails.

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo build --locked --workspace --no-default-features
cargo test --locked --workspace --no-default-features
cargo test --locked --workspace --no-default-features --release
cargo clippy --locked -p waymaker-embassy --all-targets --features postcard -- -D warnings
cargo test --locked -p waymaker-embassy --features postcard
cargo doc --locked -p waymaker-embassy --no-deps --features postcard
cargo doc --locked --workspace --no-deps --no-default-features
cargo --locked xtask coverage
cargo build --locked --no-default-features --target thumbv6m-none-eabi
cargo build --locked -p waymaker-rig --no-default-features --lib --target thumbv6m-none-eabi
cargo build --locked -p waymaker-drive --no-default-features --lib --target thumbv6m-none-eabi
cargo build --locked -p waymaker-facade-demo --no-default-features --lib --target thumbv6m-none-eabi
cargo build --locked -p waymaker-embassy --no-default-features --features postcard --lib --target thumbv6m-none-eabi
cargo clippy --locked -p waymaker-size-probe --target thumbv6m-none-eabi --features probe,embassy-postcard --bins -- -D warnings
cargo clippy --locked -p waymaker-emu --target thumbv6m-none-eabi --features emu --bins -- -D warnings
cargo --locked xtask size
cargo test --locked -p waymaker-spec --no-default-features
cargo test --locked -p waymaker-drive -p waymaker-rig --no-default-features --test matrix
cargo test --locked -p waymaker-flash --no-default-features --test corpus
cargo --locked xtask profile
cargo --locked xtask emulate
cargo --locked xtask book
cargo --locked xtask check-layering
```

`cargo xtask profile` needs valgrind. `cargo xtask emulate` needs `qemu-system-arm`. No
rustup profile installs either, so the pipeline installs them in the `profiling` and
`emulation` jobs. Both commands fail closed, not silently pass, when their tool is
missing — a measurement that did not happen is not a measurement that passed.

`cargo doc` needs `RUSTDOCFLAGS=-D warnings` to actually fail on a broken doc link. That
flag lives in the workflow's `env:` block; the `ci-pipeline` rule fails a build without
it.

`cargo xtask install-hooks` points git at `.githooks`, which runs format, lint and test —
the three fast stages — before every commit. The hook is generated from the same stage
table used above, so the hook and CI always run the same commands.

## The invariants

Design document §02 settles eight decisions. Each has a stable id, recorded in
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

Two more rules hold everywhere, though they are not from §02:

- **No behavior ships without a test, and no invariant ships without something that fails
  a build over it.** A rule that can be broken silently is a comment.
- **A measurement that did not happen is not a measurement that passed.** Every gate
  fails closed: a missing tool, an unreadable report, a crate that contributed nothing, an
  unparseable input.

## What is still undecided

Design document §16 leaves five questions open. Issue
[#16](https://github.com/madmax983/waymaker/issues/16) sets the deadline: each needs an
answer before the wire format freezes at 1.0. They are held as a table in
`xtask::docs::DEFERRED_QUESTIONS`, so an open question is checked as strictly as a settled
one — the `deferred-questions` rule compares that table against this section and against
the ADR record, in both directions.

5 deferred questions, with the id to cite when a change touches one:

| Id | Question | Where it stands |
| --- | --- | --- |
| `integrity-check-algorithm` | Whether the default integrity check is CRC32C or a smaller table-free CRC implementation. | [Settled by 0010-the-integrity-check-is-catalogued-and-table-free.md](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md): CRC-32/ISO-HDLC and CRC-16/CCITT-FALSE, both table-free at the time. The polynomial turned out to be free on `thumbv6m` and the table not to be. [ADR 0046](docs/adr/0046-crc16-folds-its-nibble-round-to-a-multiply-crc32-stays-bitwise.md) later folds `crc16`'s nibble round to a multiply — still table-free — and declines a table for `crc32`; [ADR 0053](docs/adr/0053-a-crc32-nibble-table-still-beats-the-branchless-loop.md) then supersedes that `crc32` clause alone, once a profile of this workspace's own workloads showed the table still winning against ADR 0046's own branchless loop. |
| `retry-policy-placement` | Whether retry policy belongs in the Embassy façade or remains workflow code. | Open, owned by rung 0.4 · embassy. Settles when the dispatcher exists and the cost of a recorded retry representation can be measured against reimplementing backoff in every workflow. |
| `effect-scheduled-metadata` | How much input metadata an EffectScheduled record stores beyond length and digest. | [Settled by 0011-a-scheduled-effect-records-a-length-and-a-digest.md](docs/adr/0011-a-scheduled-effect-records-a-length-and-a-digest.md): `seq`, `kind`, `input_len`, `input_crc`, and nothing else. |
| `explicit-state-snapshots` | Whether a future explicit-state workflow API may support true storage snapshots. | Open, owned by after rung 1.0. Settles when a non-async, explicit-state API has been designed far enough that the snapshot it would take can be described in records, without relaxing the no-snapshotted-futures decision for the async façade. |
| `wire-format-migration` | How stable wire-format migration is performed after a deployed fleet outlives v1. | [Settled by 0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md](docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md): the format is frozen byte by byte with a committed corpus, the read side is a set and the write side one number, and migration is a new bank at a `continue_as_new` boundary. |

An ADR that answers one of these carries `Settles deferred question:` and the id; the row
in `DEFERRED_QUESTIONS` moves from `Open` to `Settled` in the same change. Writing the ADR
without moving the row, or moving the row without the ADR, each fail the build.

Do not decide one of these early. An ADR written for a question whose implementation does
not exist is a snapshot of an opinion, and [the record](docs/adr/README.md) says a
decision record must never be that.

## The guarantees, and what holds each up

Design document §14 states five guarantees; §02 decision 7 states a sixth. They are the
reason this project exists, so they are a table, not a paragraph: `xtask::docs::SPEC_CLAUSES`
holds them, `crates/waymaker-spec` proves them, and the `recovery-spec` rule fails a build
in which this section, that table, the crate's own `obligation.rs`, and
[ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md)
stop naming the same set.

All 6 recovery invariants, with the id to cite when a change touches one:

| Id | Guarantee | Discharged by |
| --- | --- | --- |
| `prefix-safety` | recovery exposes only a legal prefix of committed records | `tests/spine.rs`, exhaustively over every reachable state, and refined against the real `Scan` at every crash point |
| `acknowledged-durability` | any record acknowledged after its barrier is recovered after reset | `tests/spine.rs`; `tests/necessity.rs` shows which precondition it rests on |
| `durable-intent` | no Waymaker-dispatched effect lacks a recoverable schedule record | `tests/spine.rs`, with §02 decision 3 as a precondition rather than a hope |
| `single-authority` | exactly one bank is authoritative after any crash | `tests/spine.rs`, exhaustively over the model — records now carry a `BankId` and recovery is scoped to the bank a reader would boot from (issue #67), so a reader that boots the retired bank is caught by `tests/teeth.rs`'s `Mutant::BootsTheRetiredBank`, and refined against a real two-bank swap since issue #73's `tests/refinement.rs` abstraction of `waymaker_flash::bank` — though that refinement and the record refinement beside it have never been driven by one writer, so the bank check itself is still owed against a real device with a record on it |
| `stable-redelivery` | retries and reboot redelivery reuse the original effect identity | `tests/redelivery.rs`, over every resume point of a bounded run, against the real allocator |
| `bounded-decoding` | malformed storage cannot cause out-of-bounds reads or allocation | `tests/bounded_decoding.rs`, over a stated domain: every byte string to three bytes, every truncation, every single-byte mutation and coordinated pair of three real frames, and every payload length a header can declare |

These proofs run as a CI stage of their own, `cargo test --locked -p waymaker-spec
--no-default-features`, in the `verification` job — and again in the workspace test stage.
The separate job exists so that a guarantee which stopped holding is legible in the checks
list by name.

The proofs are bounded and say so: `Bound::PROOF` travels in every result, reaching the
state ceiling is an error rather than a silent truncation, and `tests/census.rs` pins the
reachable state count so a machine that quietly shrank fails a build rather than passing
every proof about the part of it that is left.

A guarantee is worth only the evidence that it could have failed. Every one here has a
falsifier: `tests/necessity.rs` removes each of the model's six preconditions in turn and
requires a named guarantee to break, and `tests/teeth.rs` runs a catalogue of readers wrong
in one way each and requires each to be caught by the guarantee it breaks.

Records carry one distinction and no more: a **schedule** or an **outcome**. The model is
otherwise incurious about content — that keeps it a model of the protocol, not of the
codec. But §14's third guarantee means *schedule* record specifically: without the
distinction, an effect could be accounted for by an acknowledged completion, which is
history written after the world was already changed. The same distinction backs §11's
order rule: a schedule may not be declared while an earlier one is unresolved, which
`waymaker_core::ReplayCursor` enforces by refusing that shape as malformed history.

## The storage contract, and what each sentence rests on

Design document §12 states a storage contract in five sentences. Issue
[#21](https://github.com/madmax983/waymaker/issues/21) asks for them to be documented and
tested. `waymaker-flash` owns the contract — `Geometry` and `StableStorage`, with the
trait's public surface pinned by `storage-contract` — and `waymaker-conformance` is what
any adapter is run against. `xtask::docs::STORAGE_CONTRACT_CLAUSES` holds the table, the
crate's own `clause.rs` holds it again, and `storage-conformance` fails a build in which
this section, that table, the crate, and
[ADR 0016](docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md) stop
naming the same set.

The "Discharged by" column matters: three of the five sentences cannot be observed by a
suite running inside one process, and a suite that claimed "all clauses covered" would
only be reporting on the two it can.

All 6 storage-contract clauses, with the id to cite when a change touches one:

| Id | Sentence | Discharged by |
| --- | --- | --- |
| `interruptible-mutations` | `program` and `erase` may fail or be interrupted at any supported unit. | a crash injector, not a suite: `waymaker-fault` interrupts a write at every byte of every program and every block of every erase, and a driver that never fails satisfies "may fail" vacuously |
| `barrier-is-durable` | After `barrier` returns, all earlier successful mutations survive reset. | the across-reset witness: `durability::arm`, a reset the caller performs, then `durability::verify` |
| `barrier-orders-what-follows` | No later mutation may become durable before mutations ordered by a completed barrier. | the across-reset witness, by the same two calls — a write that is on media while the seal ordered before it is not |
| `validated-before-media` | The adapter validates erase/program alignment before touching media. | the in-process suite, in every case about what an adapter *refuses*, including the three that read the media back afterwards to see whether the refusal came first |
| `one-way-bits-are-the-drivers` | Flash-specific one-way bit programming rules remain the driver's responsibility. | the driver, not the protocol — named here so its absence from the suite is a decision rather than an oversight |
| `operations-act-on-what-they-name` | `read`, `program` and `erase` act on exactly the region they name, and `barrier` changes no media. | the in-process suite, in every case about what it does when it *agrees*. Not one of §12's five: it is `StableStorage`'s own documentation, and without it the suite would be a suite of refusals that never checked that a legal operation works |

The suite is proven able to fail, which is what makes a passing run worth anything.
`crates/waymaker-conformance/tests/teeth.rs` runs adapters wrong in one way each and
requires the matching case to go red, with a control adapter required to pass — the check
each flaw must break is an exhaustive `match`, so a flaw added to the model and left out
of the test does not compile. It runs against two real adapters — `waymaker_fault::Device`,
which knows nothing about this crate, and an `embedded-storage` `NorFlash` through
`NorFlashStorage`.

The suite refuses to guess two things, because guessing is how a broken adapter talks a
suite out of testing it: erased is always `0xFF`, a fixed constant rather than something
learned from the device under test, and no case names a byte outside the caller's region,
even in an operation expected to be refused.

`waymaker-core`'s and `waymaker-flash`'s `may_depend_on_external` lists in
`xtask::policy::LAYERS` are empty; only `waymaker-embassy` has entries, for its optional
codecs. So `embedded-storage` cannot become a kernel dependency without failing
`kernel-zero-dependencies`, and it cannot become a `waymaker-flash` dependency without
failing `dependency-direction`.

## The storage-shape catalogue

Issue [#130](https://github.com/madmax983/waymaker/issues/130) asks that every legal
storage-operation shape the firmware issues appear in the suite. `xtask::docs::STORAGE_SHAPES`
holds the six shapes, the conformance crate's own `shape.rs` holds them again, and
`storage-shapes` fails a build in which this section, that table, the crate, and
[ADR 0047](docs/adr/0047-a-shape-catalogue-holds-the-suite-to-the-writers.md) stop naming
the same set.

A shape is a claim about a legal call, transcribed by a reviewer from `waymaker-flash`'s
writers. `shape::ShapeWitness` wraps a `StableStorage` and records which shapes a run
really issues, crediting nothing an adapter refused —
`crates/waymaker-conformance/tests/shapes.rs::a_full_run_issues_every_declared_shape` fails
a build in which a declared shape goes unexercised.

All 6 storage shapes, with the id to cite when a change touches one:

| Id | Sentence | Issued by |
| --- | --- | --- |
| `program-single-unit` | A program of exactly one program unit. | `append::Sealable::commit`'s record commit seal, `append::Journal::stage`'s frame body, `swap::Prepared::stage`'s bank header and `swap::Sealable::commit`'s bank seal, whenever the padded value — at the journal's own alignment, which may be coarser than the device program unit — comes to exactly one device program unit |
| `program-multi-unit` | A program of more than one program unit in one call. | `append::Sealable::commit`'s record commit seal, `append::Journal::stage`'s frame body, `swap::Prepared::stage`'s bank header and `swap::Sealable::commit`'s bank seal, whenever that padded value spans more than one device program unit |
| `erase-single-block` | An erase of exactly one erase block. | `swap::Swap::prepare` and `Installed::reclaim`, on a device whose bank is one erase block |
| `erase-multi-block` | An erase of more than one erase block in one call. | `swap::Swap::prepare` and `Installed::reclaim`, on a device with at least four erase blocks |
| `read-single-unit` | A read of exactly one read unit. | `recovery::Recovery::stage`'s header read and its erased-tail walk, whenever the bytes actually read — bounded by the geometry and by what remains of the region — come to exactly one read unit |
| `read-multi-unit` | A read of more than one read unit in one call. | `recovery::Recovery::stage`'s whole-record read, always at least two read units by construction; and its header read and erased-tail walk, whenever the bytes actually read — bounded by the geometry and by what remains of the region — span more than one read unit |

Issue #130's third item — a generator that mutation-tests the suite against its own model —
is still open. It is `waymaker-spec`-shaped work, not a small addition to `tests/teeth.rs`,
and this catalogue does not attempt it.
## The frozen wire format

Design document §09 states the journal and the wire format. Issue
[#41](https://github.com/madmax983/waymaker/issues/41) freezes it at v1. The promise runs
one direction only: **records a shipped device wrote stay readable by every later 1.x
firmware.** An earlier firmware that meets a later record kind stops — downgrade is not
supported. [ADR 0037](docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md)
is the policy, and it settles §16's fifth deferred question.

The format is stated byte by byte in
[`docs/format/wire-format-v1.md`](docs/format/wire-format-v1.md): the frame, the commit
seal, the record table, the bank header, the generation seal, and both check algorithms
with their parameters. A porter implements from that document; the `wire-format` rule
stops it drifting from the code.

Three things hold the freeze, and each holds a different half of it:

- **The corpus** — [`crates/waymaker-flash/tests/corpus/v1`](crates/waymaker-flash/tests/corpus/v1/README.md),
  twenty-one files of frozen bytes, run by `crates/waymaker-flash/tests/corpus.rs` as the
  `corpus` CI stage. It and the `wire-format` rule are the two things that notice a record
  kind *renumbering*: nothing else can, because the encoder takes a kind's number from
  `RecordRef::kind` and the decoder matches the same constants, so swapping two numbers
  leaves every round trip, every property test, and every crash sweep green. A corpus case
  is added, never regenerated.
- **The `wire-format` rule** — the frozen numbers, the record numbering in both directions,
  the specification document, and the corpus's own lengths and digests.
- **The read set** — `frame::reads_format_version` is the set of format versions this
  firmware reads; `FORMAT_VERSION` is the one it writes. Both decoders take their answer
  from the predicate. At v1 the set is a single value, so this costs nothing and reads as
  a plain equality check; it starts to matter the day a transition firmware widens it.

Migration is §10's bank swap and nothing new: a bank is single-version by construction, so
the retiring bank is read at its own version and the installing bank is written at the new
one. Steps 5 and 6 of the swap are the format transition. ADR 0037 states the fleet
rollout end to end, and states plainly that the rollout is one-way while it runs.

## What the boards still owe

Two rungs have an exit criterion no amount of host-side work can discharge. Rung 0.2's is
issue [#27](https://github.com/madmax983/waymaker/issues/27): power-cut loops passing on
one Cortex-M0+ board and one Cortex-M4 board. Rung 0.5's is issue
[#34](https://github.com/madmax983/waymaker/issues/34): an `AtPersistentTime` deadline
armed, the supply removed entirely for longer than the interval, and the deadline
recognised as elapsed on the first replay after it. Nothing in this repository has ever
run on a board.

`xtask::docs::HARDWARE_TARGETS` holds that fact as a table, and `hardware-attestation`
compares it against this section in both directions, so a green CI run can never quietly
contradict it. The `emulate` stage is the check most likely to be mistaken for one of
these rows, and is not one — see
[the emulated boot](#the-emulated-boot-and-what-it-is-not).

3 hardware targets, with the id to cite when a change touches one:

| Id | Target | Where it stands | What would discharge it |
| --- | --- | --- | --- |
| `cortex-m0plus` | power-cut and watchdog-reset loops on a Cortex-M0+ board | Not run | a rig log from a board, with the census complete and no breach. `waymaker-rig` is written to link on the target and has never been on one. The census completes on a host now, but against a model: no weak bits, no reset-cause register, no retained RAM, and a watchdog that lands at a call boundary rather than on a timer. |
| `cortex-m4` | power-cut and watchdog-reset loops on a Cortex-M4 board | Not run | the same log from a second core, because a rig that only ever ran on one part has measured that part rather than the protocol. |
| `rtc-power-loss` | an AtPersistentTime deadline across a total power cut on a board with a backed RTC | Not run | a board with a battery- or supercapacitor-backed RTC, the supply removed for longer than the interval, and the first replay after it recognising the deadline as elapsed. `waymaker-rig`'s `rtc` and `epoch` modules are written to link on the target and have never been on one. `waymaker-drive/tests/power_loss.rs` drives the scenario on a host, but against a model: no oscillator to drift, no supply to sag, and a continuity flag a test sets rather than a backup domain that failed. |

Moving a row to `Passed` needs an accepted ADR that carries the attestation marker and the
id, in the same change. Writing that ADR line without moving the row fails the build too.

What a host *can* discharge, it does: `waymaker-rig` is driven at every crash point
`waymaker-fault` enumerates — every byte of every program, every block of every erase,
before and after every barrier, and a watchdog reset at every unit boundary — and every
cell of the census is filled, with writers wrong in one way each required to be caught by
the guarantee they break and by no other. What no host-side model can supply is media that
behaves like a real part (a real controller may abort a unit mid-write where this one
finishes it), a reset-cause register, a watchdog that fires on a timer rather than at a
call boundary, or retained RAM. §12's `barrier-is-durable` and `barrier-orders-what-follows`
are, likewise, still owed against a real driver.

## The emulated boot, and what it is not

Every firmware stage above builds a **library**; `cargo build --lib` never links. Nothing
places a reset vector, resolves a `#[panic_handler]`, links `compiler_builtins`, or
retires a single instruction. Every test in this workspace otherwise runs on x86-64 under
`std`.

`waymaker-emu` is a linked image — reset vector, vector table, memory map. `cargo xtask
emulate` starts it on two QEMU machines and gates what it says it did.

| Machine | Core | Architecture | Why |
| --- | --- | --- | --- |
| `microbit` | Cortex-M0 | ARMv6-M | the architecture §04's budgets are stated for, and the target every firmware stage builds |
| `mps2-an386` | Cortex-M4 | ARMv7E-M | a second encoding, because a rig that only ever ran on one has measured that one |

Three things hold it:

- **The two censuses must be equal.** The plan is deterministic, so any difference is the
  rig behaving differently on two instruction sets, not a tolerance to average away.
- **The media model is checked against the storage contract before the rig runs over
  it.** Neither machine has a real flash part, so the media is NOR modelled in RAM. §12's
  conformance suite runs against that model first, in the same boot.
- **The census is the gate, not the exit code.** An image whose `main` returns early exits
  the same way a complete one does, so the image itself refuses a run with no case passed,
  no iteration run, or no verdict reached — and the harness checks that refusal again
  rather than trusting the image's own report.

**It attests to no board, and that is the point of saying so here.** Neither machine has a
NOR part, a removable supply, a reset-cause register, retained RAM, or a backup domain, and
a Cortex-M0 is not a Cortex-M0+. The emulated boot covers the *architecture* of two rows of
[the hardware table](#what-the-boards-still-owe) and none of their hardware. All three rows
stay `Not run`, and [ADR 0040](docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md)
carries no attestation marker.

## The failure matrix, row by row

Design document §14 states failure semantics as a table of ten rows. Issue
[#31](https://github.com/madmax983/waymaker/issues/31) is rung 0.3's exit criterion: each
row is a named test, and the matrix runs on the in-memory model and on the rig. The rows
are `waymaker_rig::matrix::Row`, `xtask::docs::FAILURE_ROWS` holds the table, and
`failure-matrix` fails a build in which this section, that table, the rig's vocabulary,
the model's test file, and
[ADR 0027](docs/adr/0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md)
stop naming the same set.

The model half is `crates/waymaker-drive/tests/matrix.rs`: one test per row, named after
it. The rig half is `crates/waymaker-rig/tests/matrix.rs`: one test per swept row, named
after it with `_on_the_rig`. Two rows — a capacity refusal and a declared-workflow
mismatch — are driven rather than swept, since neither is a media crash the injector
produces. Both halves run in the `verification` job as the `matrix` stage. No board has
run either half.

All 10 failure rows, with the id to cite when a change touches one:

| Id | Failure point | Discharged on the model by | On the rig |
| --- | --- | --- | --- |
| `during-schedule-frame-write` | During schedule frame write | `during_schedule_frame_write_the_frame_is_ignored_and_the_activity_was_not_yet_dispatchable` | Swept |
| `after-schedule-barrier-before-dispatch` | After schedule barrier, before dispatch | `after_schedule_barrier_before_dispatch_the_stable_effect_id_is_redelivered` | Swept |
| `during-physical-activity` | During physical activity | `during_physical_activity_the_effect_is_redelivered_and_the_activity_tolerates_the_duplicate_attempt` | Swept |
| `after-activity-before-completion-barrier` | After physical activity, before completion barrier | `after_physical_activity_before_completion_barrier_the_same_id_is_redelivered` | Swept |
| `during-completion-write` | During completion write | `during_completion_write_the_torn_completion_is_ignored_and_no_partial_result_bytes_are_exposed` | Swept |
| `after-completion-barrier` | After completion barrier | `after_completion_barrier_the_completion_is_replayed_and_the_activity_never_runs_again` | Swept |
| `during-inactive-bank-erase-or-write` | During inactive-bank erase/write | `during_inactive_bank_erase_or_write_the_old_bank_remains_authoritative_and_the_old_run_continues` | Swept |
| `after-new-bank-seal-barrier` | After new bank seal barrier | `after_new_bank_seal_barrier_the_new_bank_is_authoritative_and_the_old_run_is_never_current` | Swept |
| `history-capacity-reached` | History capacity reached | `history_capacity_reached_is_a_capacity_error_with_no_mutation_or_an_explicit_continue_as_new` | Driven |
| `replay-divergence` | Replay divergence | `replay_divergence_is_a_deterministic_fault_with_no_further_execution_and_history_untouched` | Driven |

Row 5 needs a note. §14 says a torn completion is *redelivered*. No writer starts a record
before the one ahead of it has sealed, so an interrupted attempt only ever touches its own
reserved slot. Recovery now checks: if every byte from the frame's unpadded length to the
end of that slot is erased, the record is ignored and the slot becomes the append point,
so the same run redelivers the effect under its own identity rather than being forced into
`continue_as_new`. See
[ADR 0052](docs/adr/0052-a-torn-record-redelivers-when-its-reserved-slot-is-clean.md). A
tear *inside* the commit seal itself is different: those bytes are neither erased nor a
real seal, so recovery still cannot tell an interrupted append from damage, and that half
of the row still refuses, as
[ADR 0018](docs/adr/0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)
requires.

## The book

Issue [#42](https://github.com/madmax983/waymaker/issues/42)'s mdBook is
[`docs/book`](docs/book/src/SUMMARY.md). Where this file is written for a contributor, the
book is written for a reader — a workflow author, or someone porting to a part. Eight
chapters, held as a table in `xtask::book::BOOK_CHAPTERS`. `cargo xtask book` renders it;
the `book` and `hardware-matrix` rules say what it must contain.

Two decisions about it are worth knowing before you change it.

**A chapter carries no Rust sample of its own.** Every sample is an `{{#include}}` of an
anchor in [`crates/waymaker-drive/tests/book.rs`](crates/waymaker-drive/tests/book.rs),
named after a real `#[test]` in that file. The bytes the book shows are bytes the `test`
stage actually compiles and runs, against `waymaker-fault`'s model of NOR, the real
driver, and the real codec.

**The `book` stage runs `cargo xtask book`, not `mdbook build`.** mdBook exits `0` for an
`{{#include}}` whose file is missing, and for an anchor a file does not declare — it
renders the second case as nothing at all, with no log line. `cargo xtask book` fails
closed on both, and on a page that shows none of the sample it includes.

The hardware compatibility matrix is [one chapter](docs/book/src/hardware-matrix.md), and
every cell but the clock column is *derived* — the geometry from `xtask::wear::PARTS`, the
power-cut standing from `xtask::docs::HARDWARE_TARGETS`, the write amplification from a
measurement the gate takes on every run. The book cannot say `Passed` where
[the attestation record](#what-the-boards-still-owe) says `Not run`.
## The layering

`waymaker-embassy` → `waymaker-flash` → `waymaker-core`, and never the other way. The table
is `xtask::policy::LAYERS`; the diagram is
[here](docs/architecture.md#crate-dependency-flow). Adding a crate to the workspace means
adding a row to that table — a member no rule covers fails `workspace-membership`.

The "May depend on" column below is `may_depend_on` plus `may_depend_on_external` in
`policy::LAYERS`. The façade's last five entries are the external half: only a non-default
codec feature enables any of them, no default build links one, and `codec-is-optional`
keeps that true.

| Crate | Owns | May depend on |
| --- | --- | --- |
| `waymaker-core` | Borrowed record views, effect identity, replay cursor, transition rules, timer semantics and the clock-kind vocabulary, the workflow-version range and gate vocabulary, capacity errors | nothing |
| `waymaker-flash` | Stable wire encoding, the integrity-check trait and its shipped binding, the storage contract and its geometry, CRC and seals, the commit seal and the two-barrier write discipline, the two-bank layout, bank selection, append scanning, storage-backed recovery and the append offset, the capacity reserve, the seven-step bank swap and `continue_as_new`, compaction transition | waymaker-core |
| `waymaker-embassy` | `Ctx`, activity futures, dispatcher, wakeups, the persistent-clock capability, optional typed codec helpers | waymaker-core, waymaker-flash, cobs, postcard, serde, serde_core, thiserror |

### The must-not-own table

Design document §05. These strings are `must_not_own` in `xtask::policy::LAYERS` verbatim.
The `claude-md` rule fails a build in which this table and that one disagree.

| Crate | Must not own |
| --- | --- |
| `waymaker-core` | allocation, serialization framework, CRC, clock, storage driver, executor, logging |
| `waymaker-flash` | activities, workflow types, timers, Embassy |
| `waymaker-embassy` | on-media authority or hidden global state |

`waymaker-embassy` is the only crate permitted to know Embassy exists. A host or browser
adapter can be written later against the same semantic kernel, but it must not expand the
firmware traits to accommodate host conveniences.

Nine workspace members are not layers:

- **`xtask`** — host tooling, the gate itself. Kept out of firmware builds by
  `default-members`.
- **`waymaker-size-probe`** — firmware linked only so its section sizes and symbols can be
  measured. All three layers are *optional* dependencies of it, so a baseline build links
  none of them, which is what makes the code-flash budget a delta rather than an absolute.
  Nothing depends on it.
- **`waymaker-emu`** (`policy::EMULATION_CRATES`) — a linked, executed image, not another
  measurement crate. It is what `cargo xtask emulate` starts on a Cortex-M0 and a Cortex-M4
  so `waymaker-rig` runs on both instruction sets Waymaker targets. It is also the only
  crate here carrying `#![allow(unsafe_code)]` — a reset vector needs it — scoped by the
  `emulation-boot` rule to two macro expansions (`#[cortex_m_rt::entry]` and the
  semihosting exit) plus one linker-symbol read. See
  [ADR 0040](docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md).
- **`waymaker-fault`** (`policy::TEST_SUPPORT_CRATES`) — the in-memory storage model and
  crash injector. Host-side, `std`, no third-party dependencies. It depends on
  `waymaker-flash` for the storage contract; no layer depends on it. See
  [ADR 0013](docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md).
- **`waymaker-conformance`** (`policy::TEST_SUPPORT_CRATES`) — the §12 storage-contract
  suite and the `embedded-storage` port. `#![no_std]` and allocation-free, because an
  adapter author may only be able to run it on the target their driver is for. It carries
  one third-party dependency, `embedded-storage`, which no layer may reach. See
  [ADR 0016](docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md).
- **`waymaker-rig`** (`policy::TEST_SUPPORT_CRATES`) — design document §15's power-cut and
  watchdog-reset rig: the deterministic workload and cut plan, the durable witness, the
  wear meter, the oracle, and the failure-matrix row vocabulary and census. `#![no_std]`
  and allocation-free, because a rig that could only run on a host would be a simulation
  wearing a rig's name. It carries one dependency on a layer above `waymaker-flash` —
  `waymaker-embassy`, for the `PersistentClock` its `rtc` and `epoch` modules implement.
  That edge is legal because this crate is not a layer. See
  [ADR 0021](docs/adr/0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md) and
  [ADR 0031](docs/adr/0031-a-persistent-clock-is-two-registers-and-the-board-run-is-a-checked-absence.md).
- **`waymaker-drive`** (`policy::TEST_SUPPORT_CRATES`) — issue
  [#28](https://github.com/madmax983/waymaker/issues/28)'s synchronous driver for §06's
  explicit kernel boundary: the loop that joins `waymaker-flash`'s recovery scan and
  two-barrier writer to `waymaker-core`'s transition table, §07's seven-step effect
  protocol, and a reference workflow the firmware target builds. §07 lives here rather than
  in `waymaker-flash` because step 4 is an activity, and that crate's must-not-own cell
  names activities. `#![no_std]` and allocation-free, built for `thumbv6m-none-eabi`; it
  names no dependency on `waymaker-embassy` at all, in any table. See
  [ADR 0024](docs/adr/0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md)
  and
  [ADR 0025](docs/adr/0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md).
- **`waymaker-facade-demo`** (`policy::TEST_SUPPORT_CRATES`) — issue
  [#106](https://github.com/madmax983/waymaker/issues/106)'s bridge from `waymaker-drive`
  to `waymaker-embassy`, and design document §06's two example workflows. Holding the edge
  here rather than inside `waymaker-drive` is what makes `waymaker-drive`'s independence
  from the façade a fact `cargo metadata` states, not a claim a feature flag argued for.
  `#![no_std]` and allocation-free, built for `thumbv6m-none-eabi`. See
  [ADR 0032](docs/adr/0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md).
- **`waymaker-spec`** (`policy::TEST_SUPPORT_CRATES`) — the formal specification of the
  recovery invariants: the ghost model of committed history, the journal and bank state
  machines, and the exhaustive search that discharges §14's guarantees over them.
  Host-side, above `waymaker-fault`, because an exhaustive state-space enumerator has no
  business in an 8 KiB flash budget. See
  [ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md).

## Budgets

Design document §04. Every row but the last lives in `waymaker_core::budget` and is gated
by `cargo xtask size` — the numbers live in the kernel, not in the gate, because a budget
kept in two places is a budget that eventually disagrees with itself.

| Budget | Target |
| --- | --- |
| Runtime RAM | ≤ 768 B with a 512 B scratch page (§04, v0.1). Composed: the scratch page, the kernel-state registry, the context, and the largest statics delta of any row |
| Kernel state | ≤ 128 B, excluding any page buffer (§04, v0.1) |
| Context | ≤ 128 B — what kernel state leaves of the 256 B the scratch page leaves of runtime RAM. Not a §04 row: §04 names the context as a runtime-RAM term, and nothing measured it before [ADR 0035](docs/adr/0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md) |
| Incremental code flash | ≤ 13 KiB for core + flash adapter, on `thumbv6m-none-eabi` (§04 states 8 KiB as a v0.1 target; later rungs raised it as the two-bank lifecycle, the capacity reserve, and workflow versioning were added, and one raise was later withdrawn once the code-flash gate stopped charging the size probe's own arithmetic to the layers) |
| Incremental code flash, with the façade | ≤ 14 KiB for the three layers on `thumbv6m-none-eabi`. Not a §04 row: the façade has its own ceiling rather than a raise of the kernel's |
| Persistent flash | two erase blocks minimum (§04, v0.1) |

The code-flash row is the one place this repository and the design document now disagree
on purpose: `waymaker_core::budget.rs` is what CI enforces, and the ADR trail explains each
move — see [ADR 0017](docs/adr/0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md),
[ADR 0019](docs/adr/0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md),
[ADR 0020](docs/adr/0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md),
[ADR 0029](docs/adr/0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md),
[ADR 0030](docs/adr/0030-a-timer-is-a-boundary-and-its-clock-kind-is-on-media.md), and
[ADR 0036](docs/adr/0036-workflow-versioning-is-a-range-and-a-recorded-branch.md). Treat
`budget.rs` as the source of truth if this table ever drifts from it.

`cargo xtask size` reads the symbol table as well as the section headers, so it can charge
each byte to the layers or to the size probe's own arithmetic separately — `Δflash`,
`probe`, and `layers` are printed on every row of every run.

The workflow future is user memory and is reported separately:
`waymaker_drive::ota::WORKFLOW_FUTURES` is the registry, and a report that names no future
at all is `Unmeasurable` rather than a pass. A kernel-state type added to
`kernel_state_types!` is asserted at compile time, registered in the size report, and
counted in the total — it cannot be in one without being in the others.
## What the engine does not allocate

Design document §02 decision 1 says the kernel is `no_std`, `no_alloc`, and
dependency-free. `crate-attributes` and `kernel-zero-dependencies` have enforced the first
and third since rung 0.0. The second used to rest only on a crate having no way to *spell*
an allocation — a fact about source code, not about a linked image.

`cargo xtask profile` measures it directly. Valgrind intercepts `malloc` in the built
binary, below anything Rust can express, so it needs no global allocator and no `unsafe`.
Four workloads drive real library code over `waymaker-fault`'s model of NOR, each reaching
a different part of the engine:

| Workload | Drives | Reaches |
| --- | --- | --- |
| `journal` | §09's frame codec and commit seal, §10's reserve and the recovery scan, under the rig | core, flash, rig |
| `driver` | §06's boundary and §07's effect protocol, run to a terminal record | core, flash, drive |
| `facade` | §06's OTA example through `poll_ota` — `Ctx` and its four futures | core, flash, drive, embassy |
| `conformance` | §12's storage contract, as `waymaker-conformance` runs it | flash, conformance |

The "Reaches" column is measured, not declared: a crate no workload actually executes
scores the zero a deleted crate would score, and a run in which the four together miss a
gated crate fails.

| Measured | Gate | Where it stands |
| --- | --- | --- |
| Heap blocks allocated by an engine crate | 0, by `profile::ENGINE_HEAP_BLOCKS` | 0 on all four workloads, against 16 blocks the harness and the runtime allocated in the same process |
| Every gated crate reached by some workload | all 6, or the run fails naming the gap | all 6 |
| Instructions executed in engine code | none — §04 states no target | published for comparison across commits, not gated |

The engine is the three layers plus `policy::NO_STD_TEST_SUPPORT_CRATES`, derived from the
layering table so a crate joining either category is gated automatically.
`waymaker-fault` is deliberately excluded: it models media in a `Vec`, so any engine code
that calls `program` on it allocates through the model, not through anything a real driver
would link.

The instruction counts are published, not gated — §04 states no target, and a ceiling
invented here would bind nobody agreed to it. They are host instructions on a host
instruction set, so they convert to no cycle count on real hardware; they are useful only
for comparing one commit against another, since an instruction count is deterministic
where a wall clock is not. See
[ADR 0038](docs/adr/0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md).

## Writing code here

- `#![no_std]`, `#![forbid(unsafe_code)]` and `#![warn(missing_docs)]` in every firmware
  crate root; `#![warn(missing_docs)]` in every crate root, `xtask` and the probe included.
  `extern crate std;` and `extern crate alloc;` are rejected outright.
- No `unwrap()`, `expect()`, `panic!()`, or indexing in production code. The workspace
  denies them; `clippy.toml` exempts test bodies, not helper functions in an integration
  test.
- Pedantic and nursery clippy are on, workspace-wide, via `[lints] workspace = true` in
  every member manifest.
- Public items need doc comments. `cargo doc` runs under `RUSTDOCFLAGS=-D warnings`, so a
  missing one, or a broken intra-doc link, fails the build.
- Coverage is gated per crate at 85% of lines, never as a workspace total — a total is
  exactly how an untested kernel hides behind a tested adapter.
- Errors: `thiserror` in libraries, `anyhow` in binaries, when either is reachable at all.
  `anyhow` is reachable nowhere; `thiserror` is reachable only from `waymaker-embassy`,
  with the `postcard` feature on. No firmware crate names either.

### Adding a gate rule

Rules live in `xtask/src/`, one module per subject, each a pure function over already-read
input so it can be tested against a workspace that does not exist. A new rule needs five
things:

1. its id in `xtask::RULES`;
2. a `violations.extend(...)` line in `check_inputs`;
3. a row in the broken-workspace fixture;
4. a backticked row in [the rule table below](#what-the-gate-rejects), and the correct
   rule count in the sentence above it — the `claude-md` rule compares this against
   `RULES`;
5. a row in the README's rule table, checked by
   `the_readme_documents_every_rule_the_gate_declares`.

Steps 1–3 are covered by the wiring test. Steps 4 and 5 are not, and they fail in
different places: step 4 fails `check-layering`, step 5 fails `cargo test` while
`check-layering` still prints `ok`.

### Changing the record representation, or a recovery guarantee

Issue [#20](https://github.com/madmax983/waymaker/issues/20) asks for a specific order —
the model and the invariants first, then the proofs, then the code. A representation
changed first and modelled afterward is a model written to agree with what was already
built, which a specification must never be.

1. `crates/waymaker-spec/src/model.rs` — the ghost state, the transition, its
   preconditions and postconditions. A new precondition goes in `Guard` so it can be
   removed on its own.
2. `crates/waymaker-spec/src/invariant.rs` — what §14 now requires, if that changed.
3. The proofs: `tests/necessity.rs` needs a row for a new guard, `tests/teeth.rs` a row
   for a new wrong reader. `tests/census.rs` pins the reachable state count and the
   per-kind edge counts — both are expected to move; the pin exists to catch a machine
   that silently shrank, not to freeze the numbers.
4. `tests/refinement.rs` — the firmware must still refine the model at every crash point.
5. The code.

A guarantee added or removed needs a matching row in `xtask::docs::SPEC_CLAUSES`, in
`crates/waymaker-spec/src/obligation.rs`, in
[the guarantees table](#the-guarantees-and-what-holds-each-up), and a line in
[ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md).
`recovery-spec` fails a build in which those four disagree.

### Adding an ADR

Copy [`docs/adr/0000-template.md`](docs/adr/0000-template.md) to the next unused number,
fill it in, and add a row to [the index](docs/adr/README.md). A decision that is revisited
gets a new ADR naming what it supersedes; an accepted ADR is never edited to say something
else.

### Adding a diagram

Add the diagram to [`docs/architecture.md`](docs/architecture.md). Label the fence with
`<!-- diagram: some-id -->` on the line above it, and add a `DiagramSpec` row to
`xtask::docs::DIAGRAMS` naming the labels it must carry.
## What the gate rejects

All 57 rules `cargo xtask check-layering` can emit. The id is what appears in the failure, so
this table is how you find out what a red build is telling you. Most rules pin a crate's
*public surface* to a fixed list — a function, a type, or a field set a reviewer already
weighed against an invariant — so that a change which would quietly widen what a crate can
do fails the build instead of passing every test. A pinned surface is compared by name, at
every visibility, and a rule that pins one file cannot see the identical thing declared in
a sibling file; that residual is what [What is not checked](#what-is-not-checked) records.

### Layering

| Rule | Fires when |
| --- | --- |
| `dependency-direction` | A layer declares a dependency its row in `policy::LAYERS` does not allow. |
| `dependency-direction-transitive` | A layer *reaches* a crate it may not depend on, through another crate. |
| `kernel-zero-dependencies` | `waymaker-core` grows a dependency of any kind, in any table. |
| `kernel-owns-no-encoding` | A `waymaker-core` source converts between bytes and a value by hand — `from_le_bytes` and its siblings, or an `impl From<&[u8]>`/`TryFrom<&[u8]>` — which needs no dependency and so evades `kernel-zero-dependencies`. |
| `replay-cursor-surface` | The replay cursor's public function surface differs from the pinned list, in either direction — a lookup-by-id method the "no in-memory event index" invariant forbids, or the module gone so the pin checks nothing. |
| `effect-scheduled-fields` | `RecordRef::EffectScheduled` declares a field set other than the one ADR 0011 settled on, in either direction. A fifth field is 17% more journal on every effect for the life of the format; a fourth removed changes a record firmware in the field has already written. |
| `timer-record-fields` | `RecordRef::TimerScheduled` or `RecordRef::TimerFired` declares a field set other than the pinned one. The clock kind stops recovery reinterpreting one timer policy as another; the arming reading is the monotonicity floor a power cut takes out of RAM. |
| `version-gate` | Design document §08's workflow versioning stops matching what was reviewed — the version-marker record's fields, the versioning module's public surface, `VersionRange`'s shape, or a source-location hash (`file!`, `line!`, `column!`, `module_path!`) used as identity where §08 forbids it. |
| `wire-format` | The frozen v1 format drifts: a frozen constant declared with another value or twice, a `RecordKind` renumbered or added with no row, a decoder that decides its own version instead of asking the shared predicate, a missing or mismatched corpus file, or the specification document losing a stated number. |
| `integrity-check` | `waymaker-flash`'s checksum module stops using a catalogued polynomial or initial value, grows a lookup table outside the one exception ADR 0053 permits, or the routing that reaches it from the frame codec, the bank codec, the writer, or the recovery reader stops going through the shared trait. |
| `storage-contract` | The storage module's public function surface differs from the pinned list — a `read_all`, a `flush`, or any other host convenience that a real port would not have to implement. |
| `recovery-surface` | The storage-backed recovery reader's public surface differs from the pinned list, or `Recovery` becomes `Clone` — by derive, `cfg_attr`, or a handwritten `impl` — which would let one scan hand out two writers at one offset. |
| `commit-discipline` | The two-barrier writer's public surface differs from the pinned list, or its typestate comes apart so a seal could be programmed without the payload barrier in front of it. |
| `capacity-reserve` | §10's capacity reserve gains a public function the pinned list does not have, or its admission check stops being the first statement of the gated writer's body. |
| `swap-discipline` | §10's bank swap gains a public function the pinned list does not have, or its step order comes apart — a header skipped, a seal built outside its one legal constructor, or an erase that does not name exactly the bank its row specifies. |
| `ctx-facade` | The façade's `Ctx` or its durable-half traits gain a public function or method the pinned list does not have, or any façade file names on-media authority (`StableStorage`, `Reserved`, `RecordRef`, `Recovery`, `ReplayMachine`, `BankLayout`, `Swap`), holds a `static`, or declares a macro. |
| `dispatch-wiring` | The dispatcher trait or its activity table gain a function the pinned list does not have, or a row can be selected by its label instead of its number — which would make issue #36's "no string-addressed registry" a convention rather than a build failure. |
| `codec-is-optional` | An optional codec helper stops being optional — named outside its own gated module, gated by no feature, or its dependency declared without `optional = true`. |
| `rig-oracle` | The rig's oracle or its census gain a public function the pinned list does not have — an escape hatch that would let a broken run report as passed. |
| `transition-surface` | The replay machine's public function surface differs from the pinned list — a `reset` or `clear_divergence` that would turn a terminal divergence into a resumable one. |
| `timer-capability` | Design document §11's timer semantics stop matching what was reviewed — a downgrade path from a persistent clock to a boot clock, a board clock driver gaining a public field or method, or an associated constant substituting for a real reading. |
| `kernel-boundary` | Design document §06's boundary types gain or lose a member the pin does not have, or `waymaker-drive` stops deciding purely from `Intent` and `Resolve` — deciding from a raw record kind instead would make it a second transition table. |
| `effect-protocol` | Design document §07's seven-step effect protocol gains a public function the pinned list does not have, or its typestate comes apart so a proof of durable intent could be built, or an outcome observed, out of order. |
| `embassy-below-facade` | A layer other than `waymaker-embassy` reaches the Embassy ecosystem. |
| `layer-missing` | A crate named in `policy::LAYERS` is not in the workspace. |
| `layer-not-local` | A crate with a layer's name resolves to a registry crate rather than the path dependency here. |
| `workspace-membership` | A workspace member is neither a layer, declared host tooling, a measurement crate, an emulation image, nor declared test support. |
| `inputs-incomplete` | A crate is in the graph but contributed no manifest, or a workspace member contributed no crate root — a rule silently skipped it. |

### Crates and manifests

| Rule | Fires when |
| --- | --- |
| `crate-attributes` | A firmware crate root loses `#![no_std]` or `#![forbid(unsafe_code)]`, or declares `extern crate std`/`alloc`; or any crate the layering covers allows unsafe code. |
| `empty-default-features` | A layer's or a test-support crate's `default` feature enables anything, so an optional cost stops being opt-in. |
| `no-build-scripts` | A layer or a test-support crate grows a `build.rs`. |
| `member-manifest` | A layer's or a test-support crate's manifest drops `[lints] workspace = true`, declares a non-empty `default` feature, opts out of its own test binary, or — for the kernel — grows a dependency table. |
| `workspace-lints` | The workspace lint table drifts from what this project requires. |
| `release-profile` | `[profile.release]` drifts from the size settings the budgets are measured against. |
| `cargo-config-profile` | `.cargo/config.toml` is missing, rewrites the `xtask` alias or the profile, declares an `[env]` key, or sets `[build] rustflags`. |

### Pipeline and measurement

| Rule | Fires when |
| --- | --- |
| `ci-pipeline` | The workflow drops a stage, reorders one within a job, or makes one unable to fail — an `if:`, a `continue-on-error:`, a missing `RUSTDOCFLAGS`, an `on:` block no pull request triggers, or a job with no `runs-on:`. |
| `pre-commit-hook` | `.githooks/pre-commit` is missing, not executable, or not byte-for-byte what the stage table renders. |
| `toolchain-targets` | `rust-toolchain.toml` stops pinning `thumbv6m-none-eabi` or `llvm-tools-preview`. |
| `size-probe` | The size probe stops being the feature-gated firmware the size gate links, or stops mirroring a layer feature under a feature of its own. |
| `size-probe-reach` | A layer grows a public function the probe does not reach, so no budget charges for it. |
| `emulation-boot` | The emulated image stops being the thing the `emulate` stage started — the wrong attributes, an `unsafe` keyword outside the two permitted expansions and one linker-symbol read, a missing prefix line, or a binary not gated behind `required-features`. |
| `gate-broken` | The gate's own expected values do not parse. A gate must not be able to silently uncheck one of its rules. |

### Documentation

| Rule | Fires when |
| --- | --- |
| `claude-md` | This file loses a must-not-own cell, a permitted dependency edge, a settled-decision id, a backticked gate rule id, a pipeline command, or its links to the decision record and the diagrams. |
| `recovery-spec` | The recovery specification and the four places it lives (this file, the ADR, `xtask::docs::SPEC_CLAUSES`, and `waymaker-spec`'s own clause table) stop agreeing on the guarantee, its words, or its discharging test. |
| `storage-conformance` | Design document §12's storage contract and its four places (this file, the ADR, the docs table, and the conformance crate's own clause table) stop naming the same set, or disagree on what discharges a clause. |
| `storage-shapes` | Issue #130's shape catalogue and its four places stop naming the same set, or disagree on a shape's issuer. |
| `hardware-attestation` | A hardware target loses its row here, its status disagrees with `xtask::docs::HARDWARE_TARGETS`, a target marked `Passed` has no accepted ADR attesting it, or an ADR attests a target the table never declared. |
| `failure-matrix` | A failure-matrix row stops being named the same way across this file, the ADR, `xtask::docs::FAILURE_ROWS`, the rig's own `id` function, and a real `#[test]` of its name in the matching test file. |
| `adr-numbering` | An ADR skips or reuses a number, is not named `NNNN-slug.md`, or the record has no template. |
| `adr-structure` | An ADR loses its title, `- Status:`, `- Date:`, `## Context`, `## Decision`, or `## Consequences`, or carries an unrecognised status. |
| `adr-index` | An ADR is not linked from `docs/adr/README.md`, or the index links one that does not exist. |
| `settled-decisions` | The §02 ADR stops recording one of the eight decisions, or its headline. |
| `deferred-questions` | A deferred question loses its row here, its ADR is absent or unaccepted, two ADRs claim one question, or an ADR claims a question the table never declared. |
| `diagrams` | `docs/architecture.md` loses a labelled Mermaid block, a protocol step, a layer, or a permitted dependency edge, or draws one the layering does not permit. |
| `missing-docs` | A crate root stops warning, denying, or forbidding `missing_docs`. |
| `book` | The book stops matching issue #42's shape — a chapter with no file or link, a fence carrying real source instead of a tested `{{#include}}`, an anchor with no matching test, or the wire-format and failure chapters losing what they must restate from the source tables. |
| `hardware-matrix` | The compatibility matrix stops covering every board and modelled part, its table stops matching the derived rows cell for cell, or it says `Passed` where the attestation record says `Not run`. |
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
  module that `crc.rs` calls is out of its scope. A `mod` declaration the walk cannot
  resolve to exactly one scanned file — a missing file, or both `name.rs` and
  `name/mod.rs` present — fails the scan closed rather than silently scanning a tree
  the compiler would not build.
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
- **A `Recovery` rebuilt by hand from its own public readings.** `region`, `offset` and
  `ending` are all public accessors, so code inside `recovery.rs` — the one file this half
  of `recovery-surface` reads — can read a scan's whole state and hand it to a struct
  literal `Recovery { region, offset, ending, check: PhantomData }` without deriving
  `Clone` or naming it in an `impl` anywhere. That is a duplicate exactly as dangerous as
  the one issue #77 closed, reached by a route this pin does not scan for. It needs field
  visibility, so it is unreachable from any other file in the workspace — the same scope
  the bullet above states for a method on a sibling type.
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
- **That a shape's `issued_by` names the function that really issues it.** `storage-shapes`
  compares the sentence and the issuer *text* across the four places the table lives, the
  same as `storage-conformance` does for a clause's discharge — it does not resolve
  `append::Sealable::commit` against the crate and check that such a function exists. A row
  transcribed against the wrong type — a program attributed to `Journal::commit` when the
  method is `Sealable::commit`'s — reads and checks the same as a correct one; review of this
  section is what catches it.
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
  prefix safety is a prefix of *declaration order* and refuses it. `waymaker-fault`'s own
  `tests/harness.rs` drives that acceptance on purpose — a writer whose middle record's
  program call fails outright and who carries on to the next is exactly the case a stricter
  oracle would wrongly refuse — so the fix for issue #67's smaller item is not a stricter
  `verify_oracle`. The two agree over the specified machine because no reachable state has a
  gap, and that used to be argued from a theorem about a different type
  (`tests/machine.rs`'s, about `Journal`) rather than checked directly against what the
  agreement tests actually judge; `tests/oracle.rs`'s
  `the_ledger_the_oracle_judges_never_has_a_gap_before_committed_history` is now that check,
  against the `Ledger` itself.
- **That the ghost model is a model of *this* firmware.** `tests/refinement.rs` drives the
  real codec through the injector and requires every crash it can be in to be a state the
  model describes, which is what makes the model more than a second implementation. It covers
  records, and — since issue #73 — banks: a real swap writer, styled on
  `crates/waymaker-fault/tests/banks.rs`'s own, is folded into `[Bank; 2]` at every crash
  point and checked against the model's reachable set. Issue #67 gave the *model* the
  dimension that made a bank-aware refinement possible in the first place — a `Record`'s
  `BankId`, and recovery scoped to the one a reader would boot from. What it has not made
  possible yet is a *joint* refinement: the record writers never touch a bank and the
  bank-swap writer never declares a record, so `single_authority`'s own bank check — that a
  recovered record's bank is the sole authoritative one — has been refined only against
  `recovered: &[]`, never against a real crash that leaves a device with both an authority
  and a record on it. `obligation.rs`'s row says so rather than the earlier, wider claim that
  `single-authority` was fully proved about a device; Codex found the gap between the claim
  and the two sweeps' actual coverage on review of the pull request that closed issue #67.
  What is not covered is every bank sequence a firmware could produce, only the one swap this
  file drives — and, until the two sweeps compose, no sequence with a record in it. See
  [ADR 0041](docs/adr/0041-the-bank-refinement-abstracts-a-real-swap.md) and
  [ADR 0043](docs/adr/0043-the-model-gains-banked-records-a-reboot-and-a-live-compaction.md).
- **That a clause was updated before the code it constrains.** `recovery-spec` compares the
  four places a recovery invariant lives and fails when they disagree. Issue #20 asks for the
  model and the invariants to be changed *first*, then the proofs, then the code, and the
  order of edits inside one commit is not a thing a rule can read.
- **A wrong-bank reader whose wrong bank happens to be empty.** `Invariant::SingleAuthority`'s
  bank check — issue #67 — is handed `recovered` and nothing else, matching every other
  guarantee here: a `Reader` reports what it produced, not which bank it consulted. An empty
  answer from the retired bank is byte-for-byte the same `Vec` an empty answer from the
  authoritative one would be, and `legal_recoveries` already treats stopping before anything
  is required as correct, so flagging every empty `recovered` would reject readers that broke
  no rule. Codex's review of the pull request that added this clause asked for exactly this
  fix and it is not one: it is the same shape of gap `tests/teeth.rs`'s `Mutant::SkipsGaps`
  already lives with under `Guard::AppendOnly` — a wrong reader coinciding with a legal
  answer in *some* states without being right everywhere — and that file's
  `Mutant::BootsTheRetiredBank` already supplies the one state where this mutant and a
  correct reader diverge, which is what a falsifier owes rather than every state.
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
- **What the parsed scanners cannot resolve.** These scanners parse real Rust (`syn`) and
  real Markdown (`pulldown-cmark`) rather than matching text, so comments, strings, char
  literals, `use` aliases and `#[path]` modules do not blind them the way a naive text
  scan would. They still do not perform full name resolution, and the residual gaps are
  consistent in shape: a glob import (`use foo::*;`) is never followed, because this
  scanner cannot know what it brings into scope; `cfg` is not evaluated, except that an
  always-false formula (`#[cfg(any())]`, or two attributes whose conditions can never
  both hold) is recognised and its declaration ignored — two declarations of one name
  that are each genuinely reachable under different, unevaluated flags are treated as
  live alternatives, and the scanner counts a construction reachable through either
  rather than silently picking one, because a missed count is the dangerous direction
  and an extra one is not; a macro invocation, an attribute macro, or a custom derive is
  never expanded, so a file containing one is refused outright rather than guessed
  through; `crate::`-qualified and absolute (`::`) paths are not resolved against the
  crate root, because a single-file scan cannot tell whether it is looking at `lib.rs`;
  and a name that could mean either a type/module or a value, imported the same way in
  two different namespaces, is not disambiguated — this scanner has no namespace
  information for a bare `use`. Two narrower, permanently open gaps of the same shape:
  `declares_name` (used to tell whether two same-named declarations in one scope could
  ever coexist) does not recognise a function or a static declared inside an `extern`
  block, so such a declaration cannot exclude a conflicting `#[cfg]`-gated alternative
  the way an ordinary item can — tracked as a residual rather than chased, since it
  needs the same per-item-kind machinery this family has already built several times
  over; and a bare, unqualified reference to a trait used only as a trait-object bound
  (`dyn Alias`) can resolve through a sibling module's alias even when a block-local
  trait declaration of the same name should shadow it — the safe side of the trade,
  since it can only cause a false positive on legitimate code, never miss a real one.
  Every one of these is a deliberate, stated trade — a scanner that guessed instead of
  refusing or over-counting would be indistinguishable from one that quietly stopped
  checking, which is the one thing this project's own review discipline refuses to
  accept. The scanners that stayed purely textual are the ones whose rule is about
  spelling — a forbidden vocabulary item, a handwritten `unsafe` keyword — where a
  mention inside a string or comment is a false positive rather than an evasion, so
  they read comment- and string-stripped text instead.
- **An attribute macro on a trait member, and an import that shadows an allowlisted
  derive name.** `effect.rs`'s macro-and-attribute defenses (built for issue #92) do not
  visit an attribute macro attached to a trait member specifically, and `push_resolved_names`
  can be fooled by a local `use some_crate::Debug;` that renames an unrelated procedural
  derive macro to the name of a builtin one this scanner trusts. Both are open; see issue
  [#186](https://github.com/madmax983/waymaker/issues/186).
- **A foreign function or static, inside an `extern` block, is invisible to namespace
  disambiguation.** `declares_name` and `is_namespace_unambiguous` — used to decide
  whether two same-named declarations in one scope could ever really coexist — read
  every ordinary item kind but not a `syn::ForeignMod`'s own members, each of which has
  a name `declares_name` was never taught to read. Stated as an accepted residual rather
  than chased further.
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
- **That a reserve formula is safe to tighten across a firmware upgrade.** `Reserve` is
  recomputed fresh from `Bounds` on every boot, and nothing about it is on media. A schedule
  a weaker firmware admitted carries no record of what its own formula promised, so a later
  firmware whose formula asks for more can meet the same stranding ADR 0052 closed for one
  firmware version, reopened across two.
  [ADR 0054](docs/adr/0054-the-capacity-reserve-formula-is-a-fleet-precondition.md) states
  this as a precondition on how a fleet is upgraded rather than closing it in code — the
  same shape ADR 0036 and ADR 0037 already use for version-range and read-set ordering — and
  `a_schedule_admitted_by_a_weaker_reserve_can_strand_a_stricter_retry` in
  `crates/waymaker-flash/tests/capacity.rs` is the falsifier: the stall it drives is real,
  but bounded — no byte moves, and every later boot meets the same refusal.
- **That a device is an *instance* rather than a geometry, in `capacity`.** Issue
  [#84](https://github.com/madmax983/waymaker/issues/84) closed this for `append`,
  `recovery` and `swap`: each now borrows `storage` for the whole life of its protocol, so a
  caller holding two chips of one model cannot start on one and finish on the other — the
  call to do so does not exist, rather than being refused at runtime.
  `capacity::CapacityError::WrongDevice` stays a value comparison, and deliberately: neither
  `Reserve::for_layout` nor `Reserved::over` takes a `storage` argument at all, so there is
  no device instance here to bind — what disagrees is a bank size and a program granularity,
  both plain numbers a `BankLayout` derived, and two devices of one model agreeing on those
  is the check working rather than the gap issue #84 named.
  [ADR 0044](docs/adr/0044-a-device-is-a-borrow-in-three-modules-and-a-value-in-a-fourth.md)
  says which of the two shapes each `WrongDevice` variant is.
- **That the device handed to the *first* call of a new protocol invocation is the right
  one.** `Journal::stage`, `Recovery::new`/`with_integrity` and `Swap::prepare` are each
  still the one place a caller introduces a device, and that introduction is still a
  `Geometry` comparison — the same check issue #84 opens with, because there is no earlier
  borrow yet to hold a first call to. Issue #84's fix ties one record's three steps, one
  scan's many reads, and one swap's seven steps to a single device each; it does not tie one
  journal's *many separate records* to one device, so a caller who calls `Journal::stage`
  once per record over a journal's life can still hand two different same-model chips to two
  separate calls with no refusal. That is a precondition on the caller, the same standing
  `Swap::beginning`'s two unverified arguments already have, and
  [ADR 0044](docs/adr/0044-a-device-is-a-borrow-in-three-modules-and-a-value-in-a-fourth.md)
  is where it is argued rather than implied closed.
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
  precondition on `Swap::beginning` for a caller that reads it from somewhere else; issue
  #110's `waymaker-drive::Driver::at_bank` closes it for its own callers by never reading it
  from anywhere else — both arguments come from the same boot's own selection, never carried
  from an earlier one.
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
- **A crash sweep of `continue_as_new` itself.** `waymaker-drive::Driver::at_bank`'s
  `Boundary::continue_as_new` calls the same `Swap`/`Prepared`/`Staged`/`Sealable`/`Installed`
  typestate `crates/waymaker-flash/tests/swap.rs` and `crates/waymaker-fault/tests/swap.rs`
  already sweep exhaustively, unmodified — but neither sweep drives it through this driver,
  and this driver's own bank-selection reads happen nowhere in either. What
  `crates/waymaker-drive/tests/continue_as_new.rs` holds is the fault-free path: a real
  two-bank device, a real swap, and the next boot reading the installed bank's own bytes
  back. A crash landing inside the reads that pick which bank to boot from, or inside the
  seven steps as this driver sequences them rather than as the two lower sweeps do, is not
  yet driven anywhere.
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
- **A §07 step reachable only on one branch.** `effect-protocol` requires each of §07's
  storage steps to be a statement of its body — at nesting depth zero, counting parentheses
  and brackets as well as braces, in a body that declares neither a closure nor a
  short-circuit — and every proof of durable intent to be built after the commit barrier.
  Codex rounds 2 and 3 are why: `false.then(|| ...)` has no braces, and `false && …` has no
  nesting either, and both put three calls in order at depth zero in code that never runs. A
  scanner cannot follow control flow, so it refuses the constructs that create it — which is a
  *syntactic* answer to a semantic question, and it holds only as far as the list of
  constructs does. What is no longer outside it is a macro: issue #92's fifteenth round found
  a macro-generated `fn`, or a macro-generated construction, invisible to every scan built on
  a parsed `impl` body, and `effect.rs` now refuses any macro invocation at all rather than
  try to enumerate the shapes one could take. `capacity-reserve` records a limit of the same
  shape for its own pinned file, and `crates/waymaker-drive/tests/crash.rs` is what holds the
  step-order behaviour.
- **That the code before a permitted construction site does what it claims.**
  `CHECKED_DISPATCH_CONSTRUCTION` counts where `CheckedDispatch` is built, not what happens
  first: deleting `Dispatchable::perform`'s length/digest check, or swapping its literal's
  `bytes: input` for a hardcoded `bytes: &[]`, leaves the pin's own counts unchanged, so
  `effect-protocol` stays green over a value nothing real checked. `capacity-reserve`'s own
  admission-reachability limit is the same shape, met here for the first time: a positional
  pin cannot evaluate the body around it. `perform_refuses_input_that_disagrees_with_what_was_scheduled` and
  `perform_binds_the_kind_and_forwards_matching_input` are what actually hold this — tests
  rather than a scanner, for the class of defect a syntax-only pin cannot see.
- **A §07 step added from another file.** `effect-protocol` pins one file, exactly as
  `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one they pin:
  an `impl Dispatchable { fn ... }` in a sibling module of `waymaker-drive` adds a step with
  the rule silent. Inside the file it is closed — the method sets are compared at every
  visibility and a submodule is refused — but a sibling file is a sibling file.
- **A type-alias projection through a concrete type.** Issue #171 taught
  `type_is_qself_projection` to also refuse `T::Assoc` when `T` is one of the alias's own
  generic type parameters — the same unresolvable shape as `<T as Trait>::Assoc`, spelled
  without the syntax that names it. `type Foo = Via::Dispatch;`, where `Via` is some other
  type this file already defines rather than a parameter of `Foo`'s own, is not on that list:
  every syntactic signal it carries is an ordinary module-qualified path, and telling it apart
  from a real projection through `Via`'s own trait `impl` needs the name resolution `syn`
  does not do. Scoping the check to the alias's own parameters is what keeps it from guessing;
  the gap it leaves is named here rather than closed by a guess.
- **An associated-type binding as a construction site.** Issue #184 closed the gap Codex
  found one level over from the bullet above: a function's or an `impl`'s own generic
  parameter can bind an associated type to a guarded name directly —
  `T: Alias<Dispatch = CheckedDispatch<'a>>` — and `T::Dispatch { .. }` builds the guarded
  type under a name no construction pin compares. No type inference is needed to catch the
  direct shape: the guarded name is right there in the bound's own text. Adversarial review
  of that fix found the direct read alone was not enough — `type Hidden = CheckedDispatch;`
  beside `T: Alias<Dispatch = Hidden>` writes an unguarded name at the binding's own
  position — so `generic_assoc_type_bindings_naming` now resolves the binding's value
  through the same `use`/`type` alias chain `struct_literal_counts` already chases for a
  struct literal's own path, rather than reading the binding's own text alone. What it
  still does not resolve is a projection *as* the binding's value —
  `Dispatch = <Foo as Bar>::CheckedDispatch` — the same unresolvable shape the bullet above
  states for a `type` alias's own target, and every other gap "what the parsed scanners
  cannot resolve" already names for this alias-resolution machinery (a glob import, an
  out-of-line module, a macro expansion, an unevaluated `cfg`). Left as a residual rather
  than chased further, per this project's own two-or-three-round guidance.
- **Whether a block-local `use` overrides a shadowed generic parameter's own name, when the
  `use`'s own target is a value rather than a type.** Issue #205, round 3, then round 4:
  `generic_assoc_type_bindings_naming` treats any same-named block-local declaration as
  reason enough to ask the fail-closed backstop whether a shadowed name reaches the guarded
  type, `use` included. A `use` importing a value of the same name as the parameter (`use
  values::CheckedDispatch;` where that names a function) can make the backstop report a
  match that a real build would still resolve to the parameter — the same "same-spelled
  alias across namespaces" residual named below, met here for a shadowed generic parameter
  rather than an ordinary path. Narrowing the gate to a namespace-unambiguous declaration
  closes that over-count and reopens a real miss instead: a `use` importing a same-named
  *type* alias is exactly as valid an override as a `mod`, and this scanner cannot tell the
  two `use` shapes apart without resolving what each one names. Accepted as an over-count
  rather than chased further, for the reason every bullet in this list gives: a missed count
  is the danger, not an extra one.
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
- **A constant added from a sibling module, for the three bans issue #99 added.** Issue
  [#99](https://github.com/madmax983/waymaker/issues/99)'s route is closed. `TimerSpec`,
  `Timer` and `ClockCapability` may declare no associated constant. `ClockKind`'s two
  constants are pinned by name and value (`source::CLOCK_KIND_CONSTANTS`) instead: a `u8`
  newtype has no member and no method for the pins above to read. The façade's `TimerSpec`
  pin resolves `use` aliases before it scans. `effect-protocol` and `kernel-boundary` gained
  the same constant ban for their own pinned types. All four checks read one file each, the
  way `capacity-reserve`, `recovery-surface` and `storage-contract` do: a constant added from
  a sibling module is invisible to them. The `ClockKind` value pin also reads one line: a
  right-hand side that wraps to a second line reports an empty value. That is a mismatch, not
  a silent pass — but it is not a read of the real value either. Six more routes Codex found
  on this change's own pull request are closed rather than left open. A raw identifier —
  `r#BEST_EFFORT` names the same constant as `BEST_EFFORT` — no longer drops the whole
  declaration from what the scan sees. Every one of the six pinned types now refuses a trait
  `impl` outright, because `inherent_impl_bodies` reads only inherent ones and a trait `impl`
  can carry a constant of its own; `DurableIntent` and `Dispatchable` already refused one for
  the construction pin's reason, and `Effect`, every `source::BOUNDARY_TYPES` type and
  `ClockKind` refuse one now for this reason instead. `ClockKind` declared twice — even under
  two mutually exclusive `#[cfg]` attributes this scan does not evaluate — is ambiguous
  rather than resolved by whichever value a map happens to keep last. A leading attribute on
  the same line as a declaration — `#[rustfmt::skip] pub const X: Self = ..;`, which
  survives `cargo fmt` — is stripped before the line is read, `next_impl_line`'s reason, so
  it no longer hides the declaration entirely. And `implemented_type` and
  `implements_trait_for` both now strip a self type's path before comparing it to a pinned
  name, so `impl crate::timer::ClockKind` and `impl Forge for crate::timer::ClockKind` are
  found the same way a bare `impl ClockKind` already was — the fix is shared by every other
  pin built on either function, not only the three this issue added. Two routes Codex found
  stay open, both a name split across lines rather than a name misread on one — the residual
  every line-scanned pin in this file already carries, not a new one. A macro invocation
  inside a pinned `impl` — `impl ClockKind { extra!(); }`, where `extra!` expands to a
  `pub const` — is a line these bans read and not a constant they see, the same shape
  `effect-protocol` and `dispatch-wiring` already carry this limit for. And a declaration
  `#[rustfmt::skip]` holds split before its name — `pub const\nBEST_EFFORT: Self = ..;` — is
  two lines neither of which reads as a whole declaration, the same shape the wrapped-value
  residual above is. Both need a token parser rather than a line scanner, which is a larger
  change than this issue's three bans; each is a hand-written, `#[rustfmt::skip]`-guarded
  spelling rather than one `cargo fmt` produces. A third stays open for the same reason and
  needs more than a token parser: `implements_trait_for` compares a written name, so
  `type Alias = ClockKind; impl Forge for Alias { .. }` is invisible to it, the same way a
  glob import is invisible to the `syn`-based scanners issue #51 built — "what the parsed
  scanners cannot resolve", above, in [what is not checked](#what-is-not-checked), is the
  same floor, one scanner over. Resolving an alias needs a name-resolution pass this
  workspace's gate does not have for any of its scanners, textual or parsed.
- **That a pinned timer type is the type the crate ships.** `timer-capability`'s member pin
  reads a header string, so a rename that carries the crate root with it — `TimerSpec` becomes
  `TimerSpecV2`, a decoy `mod compat` keeps the pinned name and the pinned members — leaves it
  comparing a type nobody ships. Review of this change ran it. The crate-root half closes the
  careless version, and what closes the dangerous one is not the gate: `ClockCapability::admits`
  names every pair and uses no `_`, so a third policy is a compile error at the arm that would
  have permitted it. `kernel-boundary` shares the reader and the same limit.
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
- **That `Suspended::awaiting_dispatch` is called only when a dispatcher is genuinely still
  working.** `waymaker-facade-demo`'s `Bridge` stores the boundary's own `Suspended` on
  every call that returns one, and `ota`'s and `provisioning`'s `Workflow::run` read it back
  rather than minting a fresh value. One stall has no boundary call behind it at all:
  `ActivityFuture`'s dispatching stage answers `Poll::Pending` straight from
  `ActivityDispatcher::poll_dispatch`, with nothing recorded and nothing to propagate, so
  `take_suspended()` reads back `None` and the fallback is
  `Suspended::awaiting_dispatch()` — a value named for that one case rather than a fresh
  `Suspended::NEW`, which stays `pub(crate)` to `waymaker-drive`. `awaiting_dispatch` is
  `pub`, though, because a caller outside this crate needs it too; nothing stops a
  `Workflow` calling it without any dispatcher behind it at all, which is the same
  standing `Suspended::NEW`'s own doc comment already states the type cannot prevent on its
  own — a scanner cannot see every future's poll body, only the examples' tests can.
- **That a workflow stops at its own ending, for a caller that is not an `async fn`.**
  `TerminalFuture` never resolves and every other future refuses once a conclusion is
  recorded, which is two mechanisms for one rule: a run that ended has no boundaries left.
  Both are `crates/waymaker-embassy/tests/ctx.rs`'s. One thing no mechanism here can stop: a
  caller that never asks. Nothing obliges anybody to read `Ctx::conclusion` at all, and a
  caller that ignored it would report a run that did not end. A caller that *cancels* used to
  defeat the other mechanism: `TerminalFuture` and `ContinueFuture` each kept their "I have
  done my side" flag in the future rather than in the `Ctx`, so a future polled once and
  dropped took the flag with it, and a second future could re-decide or un-decide the run.
  Issue [#107](https://github.com/madmax983/waymaker/issues/107) moved the flag into the
  `Ctx`, shared by all four futures, so a dropped future cannot be replaced by one that
  changes what it recorded. No `async fn` reaches this path at all — both futures are
  `Pending` for ever, so no straight-line code follows the `.await` — which is why a scanner
  cannot hold this and `crates/waymaker-embassy/tests/ctx.rs` does. Codex round 5 of #105
  found it. Issue [#110](https://github.com/madmax983/waymaker/issues/110) turned out not to
  be the executor that would make cancellation a thing a caller really does — it closed rung
  0.4's in-boot sleep and the `continue_as_new` join instead, and this crate still has none
  of its own, by §02 decision 5.
- **That the façade's journal is the driver below it.** `ctx-facade` pins two files in
  `waymaker-embassy` and holds every file of `waymaker-drive` to naming no façade — seven of
  them, since issue #106 moved `facade`, `ota` and `provisioning` above the crate. It says
  the façade declares no authority and that the driver names no façade type; it cannot say
  that a given `Journal` implementation is honest. A journal that answered
  `Handoff::Replayed` from a buffer rather than from media would satisfy every rule here, and
  the façade would dispatch nothing. `crates/waymaker-facade-demo/tests/ota.rs` is what runs
  the real driver under the real façade.
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
- **How deep the call chain goes, in this composed figure.** Runtime RAM is now composed
  rather than sampled — the caller's scratch page, the kernel-state registry, the context,
  and the largest statics delta of any row, gated against §04's 768 B. Three of those four
  live on the stack, and what is still unaccounted *here* is the *depth* of the chain holding
  them: a deeper one moves no writable section and no type size. The report says so where it
  prints the total rather than printing "runtime RAM: ok". Depth is measured elsewhere now,
  by painting the stack rather than by a call graph — see the "Stack usage" entry below — and
  that figure is the whole emulated image's, not this composed one's, so this bullet's own gap
  stands even though the workspace is no longer silent about call-chain depth everywhere.
- **That a document's row set is complete, without a live build.** `runtime_ram_total`
  composes the largest `Δram` of *every* row, gated or not — a per-feature row is a
  configuration somebody ships, and §04 states one runtime-RAM ceiling for the device, not
  one per configuration. `--report` reads a document this process did not produce, and
  before issue [#115](https://github.com/madmax983/waymaker/issues/115) nothing checked that
  the row set itself was complete: a document that omitted the row with the largest `Δram`
  composed a smaller, wrong total and could pass a budget a complete document would fail.
  `completeness_shortfalls` closes it by resolving `cargo metadata` for the workspace at
  `--report`'s own path and holding the document's rows to what `matrix` derives from it —
  cheap, since it links nothing, which is what keeps `--report` usable without a firmware
  build. What it cannot see is a document read against a *different* checkout than the one
  on disk: the comparison is against *this* workspace's `cargo metadata`, not against
  whatever commit actually produced the document.
- **That a row's own `ram` and `bss` figures are honest.** Issue
  [#172](https://github.com/madmax983/waymaker/issues/172): `shortfalls` refuses a row
  whose `ram` reads smaller than its own `bss` plus `data`. `bss` and `data` are writable,
  non-thread-local sections, and `ram` counts both. `shortfalls` also refuses a gated row
  whose `ram` reads smaller than the baseline's — `flash`'s own rule, one section over.
  Neither check can catch a row that reports `0 B` of `ram` and `bss` together. `0 B` is
  this engine's real, current figure today (ADR 0035). A report of `0` is not proof of a
  lie. Mirroring flash's floor here would fail every honest report the gate produces. This
  process has no real build to check a self-reported figure against.
- **That the façade registers a wakeup, for an activity.** §05's Owns cell for
  `waymaker-embassy` names wakeups, and this crate registers none of its own for an
  activity: it plumbs the task's waker to `ActivityDispatcher::poll_dispatch`, which is the
  one thing that knows when the world will answer. A halted boot registers nothing at all,
  because there is nothing left to wake. `crates/waymaker-embassy/tests/ctx.rs` measures
  both with a counting waker rather than leaving them implied. A deadline that has not
  passed is no longer in this list: issue
  [#110](https://github.com/madmax983/waymaker/issues/110) has `TimerFuture` arm whatever
  `Alarm` it was given, and a firmware with none passes `NoAlarm` and gets exactly this
  paragraph's old behaviour back.
- **That a workflow calling `continue_as_new` can tell whether the swap it asked for
  happened.** It cannot, by design, on either the synchronous or the async path:
  `Boundary::continue_as_new` answers `Suspended` and `Journal::continue_as_new` answers
  `Halted`, both with nothing else, whether the driver performed §10's swap, refused it, or
  cannot swap at all. Only the caller of `Driver::boot`, reading its `Progress` or
  `DriveError` after the workflow has already returned, can tell — `Progress::Migrated`
  from a refusal, from `DriveError::Swap`, or from `DriveError::SwapStep`. A workflow that
  needs to react differently to each has nothing here to read.
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
- **Stack usage, by `cargo xtask size`, and the limit of the technique that measures it
  elsewhere.** Section sizes cannot see a cursor that lives on the caller's stack, and the
  size report says so rather than implying otherwise. Neither can either tool `cargo xtask
  profile` runs: DHAT is a heap profiler and callgrind counts instructions, so the depth of
  the chain [the budgets](#budgets) already say is unaccounted stays unaccounted *there*.
  [ADR 0045](docs/adr/0045-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md)
  closes the other half: each emulated boot now paints its own unused stack before the rig
  runs and reports how far the paint was disturbed after, gated by `emulate::StackUsage`. What
  that figure is *not* is §04's runtime RAM total — it is the whole image's call-chain depth,
  on one run, on one core, and this image links `waymaker-rig` and `waymaker-conformance`
  alongside the three layers, so it is not the engine's share alone. Nor is it exact: painting
  the stack and reading back a high-water mark is a lower bound, not a ceiling — a frame can
  reserve bytes it never writes, and a byte like that still reads as the paint pattern
  afterwards, so a run can use more than it reports — by an amount the guard margin does
  nothing to bound, since an unwritten reservation can sit anywhere below the scan's own
  ceiling. What the guard margin guarantees is narrower: it is never painted or scanned, so
  the reported figure itself can never read below the margin's own width, whatever a run
  actually did. That is a floor on the *number*, not a ceiling on how far it can underestimate
  real usage. The two machines are not required to agree about the figure, unlike
  their census: different cores compile the same source into different instructions. A decoy
  `stack.rs` reproducing the crate-relative path in a different, deeper directory is still
  read as the permitted module — narrower than a bare-file-name suffix would allow, not
  eliminated; `a_decoy_stack_rs_in_another_directory_is_rejected` is the test that shows the
  common case is closed. A closure or nested item defined but never invoked, at the permitted
  function's own nesting depth, would not be caught by depth alone either — the same shape of
  gap `effect-protocol` accepts for its own pinned bodies.
- **That an emulated core is the part a row of the hardware table names.** It is not, in four
  ways, and each one is a whole class of failure. QEMU's `microbit` is a Cortex-M0 and
  `cortex-m0plus` names a **Cortex-M0+** — the same instruction set and a different core, with
  a different pipeline, an optional MPU and a different fast-I/O port. Neither machine has a
  **NOR part**, so the media is an array in RAM and program-disturb, weak bits and a unit the
  controller aborts in flight are all outside it, exactly as this file already records for
  `waymaker-fault`. Neither has a **supply that can be removed**, so the rig's cut is the host
  cut — the iteration stops where it stands and the RAM survives it. And neither has a
  **reset-cause register, retained RAM or a backup domain**, so the `rtc-power-loss` row is
  untouched and `BackedRtc`'s continuity bit is still a board's word. What the stage does
  establish is [above](#the-emulated-boot-and-what-it-is-not); what it does not is why all
  three rows stay `Not run`.
- **That the emulated boot's media model is a model of a part.** `waymaker_emu::nor::Nor`
  clears bits and validates against a `Geometry`, and §12's conformance suite is run over it in
  the same boot rather than a doc comment claiming it — which is a stronger check than
  `waymaker-fault`'s model gets, and it is still the same limit: a model wrong in the same
  direction as the code it tests would agree with it, and no suite written against the
  contract can see a behaviour the contract does not describe.
- **The emulated image, by a coverage denominator.** `waymaker-emu`'s binary is behind
  `required-features`, so `cargo xtask coverage` reports "no coverable lines" for it — the
  size probe's standing, and the same answer: `emu-lint` is what a compiler says about it and
  the `emulate` stage is what a run says. Those two are stronger than a line percentage here,
  because the thing being checked is that the code *executes on a core*, which no host
  instrumentation can observe — but the number in the coverage table is not evidence of
  anything and should not be read as any.
- **A path through the rig that the emulated workload does not take.** `boot::run` drives a
  fixed seed for a fixed number of iterations, so it reaches the layout, the journal writer,
  the recovery scan, the resume and the audit — and no bank swap, no capacity refusal and no
  timer, which are the rows [the failure matrix](#the-failure-matrix-row-by-row) already calls
  `Owed` on the rig, met again one gate over. An instruction-set difference on a path the
  workload does not take is a difference nothing here has watched for. It is a *sampled* gate,
  and saying so is better than a green check that reads as a proof.
- **What the book's sentences say.** `book` matches chapter ids, file names, include targets
  and anchor names; `hardware-matrix` matches rendered cells. A chapter whose prose describes
  another engine passes both as long as its includes and its tables are right. It is the same
  limit the first bullet of this list states, met in a document a reader is more likely to
  believe than this one.
- **That a book sample is the sample the chapter needed.** The tie is the anchor's *name*: an
  anchor must be a `#[test]` of that name, and the test must be in a file
  `book::BOOK_SAMPLE_FILES` names. Nothing says the test *asserts* what the surrounding
  paragraph claims — that is `crates/waymaker-drive/tests/book.rs` being read by a person,
  the same standing `failure-matrix` records for a row-named test.
- **That the rendered page shows the right sample.** `cargo xtask book` looks for one
  identifier out of each anchor's body in the page that includes it, and reports the anchor
  when it can find no witness at all — which is the case that matters, because mdBook renders
  a missing anchor as nothing and exits zero, and an earlier version of that function looked
  the witness up *inside* the anchor and so had nothing to look for exactly when there was
  nothing on the page. What is left is a body whose longest identifier also appears in the
  chapter's prose. The tie that matters is the `book` rule's, which resolves every anchor
  against the file it is taken from and requires it to contain its own test.
- **A book page filed outside `docs/book/src`.** The gate reads every file of that directory,
  to a depth of eight, and `book.toml`. A chapter sourced from somewhere else through an
  mdBook preprocessor is a page no rule covers — the same shape of limit `capacity-reserve`,
  `recovery-surface` and `storage-contract` each record for the one file they pin. The depth
  bound is what stops a symlink cycle under the book's source walking for ever.
- **That a fixture anchor shows the type its chapter is about.** `book::BOOK_FIXTURE_ANCHORS`
  is the escape hatch for an anchor that shows a type a test uses rather than the test
  itself, and the rule asks only that such an anchor declare an item. Which item, and whether
  the paragraph beside it describes that item, is a reviewer's. Three anchors use it today.
- **That the modelled rows of the matrix were swept at their own program unit.** Two of them
  were not, and `render_power_cut` says so rather than printing a sweep that never ran. What
  no rule can say is whether a four-byte sweep generalises to a part that programs one byte
  or sixteen; that is what [the boards](#what-the-boards-still-owe) are for.
- **That a board's matrix row is true.** `hardware-matrix` reads the power-cut cell out of
  `docs::HARDWARE_TARGETS`, so the book cannot claim more than the decision record does. What
  makes a `Passed` row true is a log from a bench, which is `hardware-attestation`'s limit and
  is stated there.

