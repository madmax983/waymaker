# CLAUDE.md

Waymaker is a firmware-first durable workflow engine for Rust. A workflow is re-created from
its beginning after reboot and deterministically replayed through an ordered journal:
completed effects return their recorded results, and the first unresolved effect becomes the
next piece of work.

This file is what a contributor — human or agent — works to. It states the invariants, the
layering rules, and what each crate must not own.

Much of it is checked rather than remembered: the must-not-own cells, the permitted
dependency edges, the eight decision ids, the command list, the five deferred questions and
all 57 rule ids below are compared against the tables that own them, and `cargo xtask check-layering` fails a pull
request when this file and those tables stop agreeing. The rest is prose, and
[What is not checked](#what-is-not-checked) says which.

- The architecture, drawn: [`docs/architecture.md`](docs/architecture.md)
- The book, for a reader rather than a contributor: [`docs/book`](docs/book/src/SUMMARY.md)
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

`cargo xtask profile` needs valgrind and `cargo xtask emulate` needs `qemu-system-arm`, and
no rustup profile carries either — the pipeline installs them in the `profiling` and
`emulation` jobs, and both commands fail closed rather than passing when their tool is
absent, because a measurement that did not happen is not a measurement that passed.

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
| `integrity-check-algorithm` | Whether the default integrity check is CRC32C or a smaller table-free CRC implementation. | [Settled by 0010-the-integrity-check-is-catalogued-and-table-free.md](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md): CRC-32/ISO-HDLC and CRC-16/CCITT-FALSE, both table-free at the time. The polynomial turned out to be free on `thumbv6m` and the table not to be. [ADR 0046](docs/adr/0046-crc16-folds-its-nibble-round-to-a-multiply-crc32-stays-bitwise.md) later folds `crc16`'s nibble round to a multiply — still table-free — and declines a table for `crc32`; [ADR 0053](docs/adr/0053-a-crc32-nibble-table-still-beats-the-branchless-loop.md) then supersedes that `crc32` clause alone, once a profile of this workspace's own workloads showed the table still winning against ADR 0046's own branchless loop. |
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
| `single-authority` | exactly one bank is authoritative after any crash | `tests/spine.rs`, exhaustively over the model — records now carry a `BankId` and recovery is scoped to the bank a reader would boot from (issue #67), so a reader that boots the retired bank is caught by `tests/teeth.rs`'s `Mutant::BootsTheRetiredBank`, and refined against a real two-bank swap since issue #73's `tests/refinement.rs` abstraction of `waymaker_flash::bank` — though that refinement and the record refinement beside it have never been driven by one writer, so the bank check itself is still owed against a real device with a record on it |
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

## The storage-shape catalogue

Issue [#130](https://github.com/madmax983/waymaker/issues/130) item 2 asks that "every legal
operation shape the firmware issues must appear in the suite". `xtask::docs::STORAGE_SHAPES`
holds the six shapes, the conformance crate's own `shape.rs` holds them again, and the
`storage-shapes` rule fails a build in which this section, that table, the crate and
[ADR 0047](docs/adr/0047-a-shape-catalogue-holds-the-suite-to-the-writers.md) stop naming the
same set.

A shape is a claim about a legal call, transcribed by a reviewer from `waymaker-flash`'s
writers the same way `STORAGE_CONTRACT_CLAUSES` transcribes §12 rather than deriving it from
source. What holds the claim to the suite is `shape::ShapeWitness`, which wraps a
`StableStorage` and records which shapes a run really issues, crediting nothing an adapter
refused —
`crates/waymaker-conformance/tests/shapes.rs::a_full_run_issues_every_declared_shape` fails a
build in which a declared shape goes unexercised.

All 6 storage shapes, with the id to cite when a change touches one:

| Id | Sentence | Issued by |
| --- | --- | --- |
| `program-single-unit` | A program of exactly one program unit. | `append::Sealable::commit`'s record commit seal, `append::Journal::stage`'s frame body, `swap::Prepared::stage`'s bank header and `swap::Sealable::commit`'s bank seal, whenever the padded value — at the journal's own alignment, which may be coarser than the device program unit — comes to exactly one device program unit |
| `program-multi-unit` | A program of more than one program unit in one call. | `append::Sealable::commit`'s record commit seal, `append::Journal::stage`'s frame body, `swap::Prepared::stage`'s bank header and `swap::Sealable::commit`'s bank seal, whenever that padded value spans more than one device program unit |
| `erase-single-block` | An erase of exactly one erase block. | `swap::Swap::prepare` and `Installed::reclaim`, on a device whose bank is one erase block |
| `erase-multi-block` | An erase of more than one erase block in one call. | `swap::Swap::prepare` and `Installed::reclaim`, on a device with at least four erase blocks |
| `read-single-unit` | A read of exactly one read unit. | `recovery::Recovery::stage`'s header read and its erased-tail walk, whenever the bytes actually read — bounded by the geometry and by what remains of the region — come to exactly one read unit |
| `read-multi-unit` | A read of more than one read unit in one call. | `recovery::Recovery::stage`'s whole-record read, always at least two read units by construction; and its header read and erased-tail walk, whenever the bytes actually read — bounded by the geometry and by what remains of the region — span more than one read unit |

Issue #130 item 3 — a generator that mutation-tests the suite against its own model, with a
conformant arm so a false positive is reachable and not only a broken adapter — is still
open. It is `waymaker-spec`-shaped work, not a small addition to `tests/teeth.rs`, and this
catalogue does not attempt it.

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
against this section in both directions. The `emulate` stage is the green check most likely
to be mistaken for one of these rows and is not one:
[the emulated boot](#the-emulated-boot-and-what-it-is-not) says what it covers, which is two
architectures and no part — the same move
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

## The emulated boot, and what it is not

Every firmware stage above builds a **library**, and `cargo build --lib` produces an rlib and
never links. That is stated twice already — once for `crate-attributes`, which exists because
of it, and once in [what is not checked](#what-is-not-checked) — and the consequence is
larger than the allocation half it is usually quoted for: nothing places a reset vector,
nothing resolves a `#[panic_handler]`, nothing links `compiler_builtins`, and no instruction
is ever retired. Until the `emulate` stage existed, every claim about the rig on ARM rested on
a compiler's willingness to *emit* code nothing had executed, and every test in this workspace
ran on x86-64 under `std` with a 64-bit ALU and an allocator present.

`waymaker-emu` is a linked image — reset vector, vector table, memory map — and
`cargo xtask emulate` starts it on two QEMU machines and gates what it says it did. It is
`policy::EMULATION_CRATES`, a category of its own, and `emulation-boot` is the rule.

| Machine | Core | Architecture | Why |
| --- | --- | --- | --- |
| `microbit` | Cortex-M0 | ARMv6-M | the architecture §04's budgets are stated for, and the target every firmware stage builds |
| `mps2-an386` | Cortex-M4 | ARMv7E-M | a second encoding, because a rig that only ever ran on one has measured that one |

Three things hold it, and they hold different halves.

- **The two censuses must be equal.** The plan is deterministic, so a difference is not a
  tolerance — it is the rig behaving differently on two instruction sets. A `u64` shift
  lowered through `compiler_builtins` on one core and an instruction on the other, an
  alignment assumption, a `usize` narrowing: none of those shows up as anything else here,
  and none of them shows up on a host at all.
- **The media model is interrogated before the rig is run over it.** Neither machine has a
  flash part a driver can program, so the media is NOR modelled in RAM — erased is `0xFF`,
  a program only clears bits — presented through `waymaker-conformance`'s own
  `NorFlashStorage` rather than through a second `StableStorage` written for the occasion. And
  §12's suite is run over it first, which is the crate that is `#![no_std]` and
  allocation-free *precisely* so an adapter author can run it on the target the driver is for.
  A rig run over a model nobody had asked whether it obeys the storage contract would be a rig
  run over an unknown quantity.
- **The census is the gate, not the exit code.** An image whose `main` returned before it
  reached the rig exits exactly the way a complete one does. So the image prints what it
  counted and the run is failed when no conformance case passed, no iteration ran, no
  iteration was *cut* — without which only clean runs were driven and recovery answered
  nothing — a cut run was left unaccounted for, or a run reached no verdict. Both sides check
  it: the image refuses its own census and exits non-zero, and the harness checks again,
  because a gate that trusted the subject's own verdict would be reading a claim.

**It attests to no board, and that is the point of saying it here.** Neither machine has a NOR
part, a supply that can be removed, a reset-cause register, retained RAM or a backup domain,
and QEMU has no Cortex-M0+ machine at all — a Cortex-M0 implements the same instruction set
and is a different core. So the emulated boot covers the *architecture* of two rows of
[the hardware table](#what-the-boards-still-owe) and the part of none of them, all three stay
`Not run`, and
[ADR 0040](docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md) carries no
attestation marker — so `hardware-attestation` fails a build in which somebody moves a row and
cites it.

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
`_on_the_rig` for the six effect rows and the two bank rows, resumes the run with
`Rig::resume` (or, once a swap has moved authority, reads the bank it moved to directly),
and holds it to the row. The two remaining rows are driven rather than swept — a capacity
refusal and a declared-workflow mismatch are not media crashes the injector produces — and
credit their row without the `_on_the_rig` suffix, matching the model half's own naming for
its driven rows. Both run in the `verification` job as the `matrix` stage. The rig half runs
on the host through `waymaker-fault`; no board has run it, and
[the boards](#what-the-boards-still-owe) stay `Not run`. The rig reaches all ten rows; the
"On the rig" column says how, and
`every_row_of_the_table_is_reached_and_the_sweeps_have_not_thinned` pins every count so a
sweep that quietly thinned fails closed.

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

Row 5 now holds as §14 writes it. It says "redeliver": no writer starts a record before the
one ahead of it has sealed, so a torn completion's own reserved slot is the whole of what an
interrupted attempt touched, and issue [#95](https://github.com/madmax983/waymaker/issues/95)
teaches recovery to look — if every byte from the frame's own unpadded length to the end of
that slot is erased (the padding and the seal, never the frame body itself, which a
checksum-valid unsealed frame always has programmed), the record is ignored and the slot
becomes the append point, so the same run redelivers the effect under its own identity rather
than being forced into §10's `continue_as_new`. The effect already ran by the time a
completion record is written, so nothing about `durable-intent-before-effect` changes; what
changes is that the run keeps the `(RunId, EffectSeq)` the duplicate `stable-redelivery`
forbids would otherwise cost it. A tear *inside* the commit seal itself is not this fix's —
the bytes there are neither erased nor a real seal, so recovery still cannot tell an
interrupted append from damage — and that half of the row still refuses, exactly as
[ADR 0018](docs/adr/0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)
says of a bank recovery cannot vouch for. The test sweeps both outcomes. See
[ADR 0052](docs/adr/0052-a-torn-record-redelivers-when-its-reserved-slot-is-clean.md).

Issue [#96](https://github.com/madmax983/waymaker/issues/96) closed the four rows the rig
used to owe. Rows 7 and 8 needed a workload that rolls over: `Rig::iterate_until_rollover`
writes a run's opening records and stops before its `RunCompleted`, a test drives §10's
seven-step swap directly against the bank it leaves off in, and the crash injector sweeps
every point of the combined sequence. Which row a point lands in is read from
`bank::select` alone — a swap writes no journal record and marks no witness, so the old
bank still authoritative is row 7 and the new bank authoritative is row 8. That needed
`Rig::judge` and `Rig::resume` to stop assuming `Rig::BANK` is always the bank a boot would
choose: the fix is `Rig::authority`, and reverting it reproduces the defect the rows exist
to catch — a resumed run answering as current from a bank a swap had already retired. Row 9
gates `Rig::iterate_reserved`/`Rig::resume_reserved` with `waymaker_flash::capacity::Reserve`
and finds a declared tail wide enough to refuse the second effect on the rig's own fixture,
then drives the same explicit swap to show the exit past it. Row 10 gives `Workload` a
`diverging` knob that changes one schedule's activity kind with everything else — shape, run
identity, every other record — untouched, and `Rig::resume_declaring` audits history against
it instead of the workload that wrote it. The same issue records what a board still cannot
do: rows 2, 3 and 4 are told apart by whether the dispatcher was entered and returned, which
the harness sees and a reset takes with the RAM.

## The book

Issue [#42](https://github.com/madmax983/waymaker/issues/42)'s mdBook is
[`docs/book`](docs/book/src/SUMMARY.md), and it is written for a *reader* — a workflow
author, or somebody porting to a part — where this file is written for a contributor. Eight
chapters, one per bullet of the issue, held as a table in `xtask::book::BOOK_CHAPTERS`.
`cargo xtask book` renders it; the `book` and `hardware-matrix` rules say what it must
contain.

Two things about it are worth reading before changing it, because both are decisions rather
than conveniences.

**A chapter may not carry a Rust sample of its own.** Every sample is an `{{#include}}` of an
anchor in [`crates/waymaker-drive/tests/book.rs`](crates/waymaker-drive/tests/book.rs), and
the anchor's name must be the name of a `#[test]` in that file. So the bytes the book shows
are the bytes of something the `test` stage compiles and runs against `waymaker-fault`'s
model of NOR, the real driver and the real codec. A Rust fence is still how mdBook renders an
included sample as code, so the ban is on a fence carrying *source*, not on the fence: the
body may hold include directives and nothing else.

**The stage runs `cargo xtask book` rather than `mdbook build`, and that is the whole reason
the rule exists.** mdBook exits `0` for an `{{#include}}` whose file is missing — it renders
the directive into the page and logs an `ERROR` — and for an anchor a file does not declare,
which it renders as *nothing at all*, with no log line. Both were reproduced against mdBook
0.5.4 before [ADR 0039](docs/adr/0039-the-book-quotes-tests-and-the-matrix-is-derived.md) was
written. So the command fails closed on a missing renderer, on an `ERROR` beside a zero exit,
on a chapter with no HTML, and on a page that shows none of the sample it includes.

The hardware compatibility matrix is [one chapter](docs/book/src/hardware-matrix.md) and
every cell of it but one is *derived*: the geometry from `xtask::wear::PARTS`, the power-cut
standing from `xtask::docs::HARDWARE_TARGETS`, and the write amplification from a measurement
the gate takes on every run. Only the clock column is declared, because no table here holds
it. The book therefore cannot say `Passed` where
[the attestation record](#what-the-boards-still-owe) says `Not run`, and a wear figure that
drifted fails a build rather than going on being published.

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

Nine crates are in the workspace and are *not* layers:

- `xtask` — host tooling, the gate itself. Kept out of firmware builds by `default-members`.
- `waymaker-size-probe` — firmware linked only so its section sizes and its symbols can be
  measured. It
  declares all three layers as *optional* dependencies, on purpose — the baseline variant
  links none of them, which is what makes the code-flash budget a delta rather than an
  absolute — and nothing depends on it.
- `waymaker-emu` — firmware linked, started and *executed*, `policy::EMULATION_CRATES`. A
  category of its own rather than a second measurement crate, because nothing about it is
  measured: it is the image `cargo xtask emulate` starts on a Cortex-M0 and a Cortex-M4 so
  that `waymaker-rig` **runs** on the two instruction sets Waymaker is built for. Outside
  `default-members`, its binary behind `required-features`, and nothing depends on it. It is
  also the one crate in this workspace carrying `#![allow(unsafe_code)]`, which is the other
  reason it is not a row in one of the categories above: every one of those holds its members
  to `#![forbid(unsafe_code)]`, and a reset vector cannot be spelled without the attribute.
  The exception is scoped to the two macro expansions that need it — `#[cortex_m_rt::entry]`
  and the semihosting exit — by `emulation-boot`, which fails a build over the `unsafe`
  *keyword* appearing anywhere in the crate. It carries the workspace's only two firmware
  runtime dependencies, `cortex-m-rt` and `cortex-m-semihosting`, and no layer, test-support
  crate or shipped image links either. See
  [ADR 0040](docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md).
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
  Outside `default-members`, and it names no dependency on `waymaker-embassy` at all, in any
  table — issue [#106](https://github.com/madmax983/waymaker/issues/106). It is the third
  member of this
  category that is `#![no_std]` and allocation-free, and the reason is the claim it exists to
  make: issue #28 asks for a workflow driven to completion with "no `Future`, no Embassy, and
  no allocation", and a driver that could only be built for the host would leave the last
  third of that unchecked — so the `drive-firmware` stage builds its library for
  `thumbv6m-none-eabi`. It is deliberately not `waymaker-embassy`: `Ctx`, the async
  dispatcher and wakeups are rung 0.4's, and a façade that contained the protocol would be
  the opposite of the thing #28 asks to be proved. See
  [ADR 0024](docs/adr/0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md).
- `waymaker-facade-demo` — issue
  [#106](https://github.com/madmax983/waymaker/issues/106)'s bridge from `waymaker-drive` to
  `waymaker-embassy`, also `policy::TEST_SUPPORT_CRATES`. `facade`, `ota` and `provisioning`
  used to be three modules of `waymaker-drive` itself, removed by a `without-facade` feature
  to test that the driver did not need them — which proved only that no *other* module
  needed them, since `waymaker-drive`'s manifest kept the edge to `waymaker-embassy` either
  way. This crate holds the edge instead, so `waymaker-drive`'s independence from the façade
  is a fact `cargo metadata` states rather than a claim a feature flag argued for. Outside
  `default-members`, and the only crate that depends on it is `xtask`, which reads §04's
  context term and the generated workflow future sizes out of §06's two examples rather than
  transcribing them — the same reason `xtask` depends on `waymaker-fault` and
  `waymaker-rig`. It is the fourth member of this category that is `#![no_std]` and
  allocation-free, for `waymaker-drive`'s own reason one layer up: a bridge that could only
  be built for the host would leave "the protocol is fully usable through the synchronous
  driver" unchecked on the one crate that names the façade at all — so the
  `facade-demo-firmware` stage builds its library for `thumbv6m-none-eabi`. See
  [ADR 0032](docs/adr/0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md).
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
| Runtime RAM | ≤ 768 B with a 512 B scratch page (§04, v0.1). Composed and gated since [ADR 0035](docs/adr/0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md): the scratch page, the kernel-state registry, the context, and the largest statics delta of any row — every row, gated or not, since issue [#115](https://github.com/madmax983/waymaker/issues/115) closed a gap where a `--report` document could omit the row with the largest delta and compose a smaller, wrong total; the document's row *set* is now held to what `matrix` derives for the workspace |
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

All 57 rules `cargo xtask check-layering` can emit. The id is what appears in the failure, so
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
| `integrity-check` | `waymaker-flash`'s checksum module stops using one of `source::INTEGRITY_CHECK_PARAMETERS` — a polynomial or an initial value — the right number of times inside the function that owns it (ADR 0046 retargeted the two polynomial rows to `crc16_nibble`/`crc32_nibble`, which is where each now lives); or it or one of its submodules grows an array — a `const`, `static`, `type` alias or local — outside `#[cfg(test)]`, and outside the one exception `source::INTEGRITY_CHECK_TABLES` names; or it is gone, so the pin checks nothing. `INTEGRITY_CHECK_TABLES` is ADR 0053's structural pin for the one table this module is allowed — `crc32_nibble_table`'s sixteen-armed `match` over `crc32_nibble`, which LLVM compiles into a real lookup table with no `[u32; 16]` ever appearing in source, so the array ban cannot see it and this is what stands in for that ban on the one exception: each of `crc32_nibble(0)` through `crc32_nibble(15)` exactly once, and `crc32_nibble(` exactly sixteen times in total, so a seventeenth arm cannot hide behind the other sixteen being correct. `crc16` needed no such exception — its own nibble table reduces to a closed-form multiply for this specific polynomial, so it stays table-free in the fullest sense. Or the *binding* drifts: `waymaker-flash/src/integrity.rs` is gone; the integrity trait or the shipped `impl` is renamed, missing, or declared twice — a decoy above the real one is what a first-match scan reads; a seal in `source::SEAL_BINDINGS` stops returning the width §09's frame spends on it; or the shipped method body is anything but one unqualified call to the function that owns its algorithm, `fast::crc32(bytes)` included. Or the *routing* drifts, in any of the four files that have one. In `waymaker-flash/src/frame.rs`: a body pinned by `source::SEALING_FUNCTIONS` stops computing the seals its row names exactly once, or the file names `crc16` or `crc32` anywhere outside `input_digest` — the one documented exception, because a `const fn` cannot go through a trait method — or `decode_with` and `frame_len_of_with` stop verifying a header through `verify_header_with`, or the scan's `next` stops walking with `decode_with`. The rows are *derived* rather than whitelisted: a function generic over the check that no row pins is a body that can compute a seal and is pinned by nothing, and the scan that finds them reads joined signatures and generic `impl` blocks, because a `where` clause and a method in `impl<C: IntegrityCheck>` each escaped a one-line scan. The same in `waymaker-flash/src/bank.rs`, whose five sealing bodies each reach the seals their row in `source::BANK_SEALING_FUNCTIONS` names. And in `waymaker-flash/src/append.rs`, which is the writer: its `stage` must reach the codec through `frame::encode_with::<C>` — one call covers both the frame and its commit seal, because the seal is derived from the check the codec just computed — and it may name neither a checksum function nor a seal method. Without it, `frame::encode` in place of the generic sibling would seal every appended record with the shipped check whatever the recovery that positioned the writer verified with, which is a journal one half of a firmware can read. And in `waymaker-flash/src/recovery.rs`, which computes no seal at all: its two steps must reach the codec through `frame::decode_with::<C>` and `frame::frame_len_of_with::<C>`, and the file may name neither a checksum function nor a seal method — `Recovery<C>`'s parameter is a promise that a journal is verified with the algorithm that sealed it, and dropping both turbofishes passed every rule and every test before this existed. And in `waymaker-flash/src/swap.rs`, which installs a bank: its `stage` must reach the bank codec through `bank::encode_header_with::<C>`, `bank::seal_for_with::<C>` and `bank::encode_seal_with::<C>`, and the file may name neither a checksum function nor a seal method — a device whose two banks were sealed by two algorithms is a device only half of which boots. A trait nothing is obliged to call is a swap point that selects nothing. A firmware that sealed its banks with one algorithm and its records with another could read back neither half with the other's reader. [ADR 0012](docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md), and one rule id because it is one decision. [ADR 0010](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md) settles §16's first deferred question with measurements: the polynomial is free (52 B either way), the table is not (64 B for a nibble table, 1024 B for a byte table against an 8 KiB budget). A changed polynomial passes every round-trip test here and fails against every zlib in the world. [ADR 0046](docs/adr/0046-crc16-folds-its-nibble-round-to-a-multiply-crc32-stays-bitwise.md) folds `crc16`'s nibble round to a closed-form multiply — no table at all — and declines a table for `crc32`, keeping it a branchless bitwise loop; [ADR 0053](docs/adr/0053-a-crc32-nibble-table-still-beats-the-branchless-loop.md) then supersedes ADR 0046's `crc32` clause alone, once a profile of this workspace's own workloads showed a 64 B nibble table — spent as a `match` rather than an array so `crc32` can stay a `const fn` — still beating that branchless loop. |
| `storage-contract` | The public function surface of `waymaker-flash`'s storage module differs from `source::STORAGE_CONTRACT_SURFACE`, in either direction — or the module is gone, so the pin checks nothing. Design document §05 says a host or browser adapter "must not expand the firmware traits to accommodate host conveniences", and §12 is the trait it means: a `read_all`, a `flush`, a `write_at` or a `capacity()` shortcut would each break no layering rule, need no dependency, and turn a four-operation contract every port must implement into a surface only a host can afford. The pin compares names, so a widened offset or a validator that stopped validating is still a reviewer's job. |
| `recovery-surface` | The storage-backed recovery reader's public function surface differs from `source::RECOVERY_SURFACE`, in either direction — or the module is gone, so the pin checks nothing. §02 decision 2's "no `Journal::get(id)` and no in-memory event index" is a rule about the reader that touches media as much as about the cursor: a `seek`, a `resume_at` or a `read_all` would each break no layering rule and turn a forward scan whose RAM is one caller-owned page into one that seeks or holds history. One name is load-bearing for a second reason. `append_offset` is the only way an offset leaves the module and it answers `Some` only for a scan that ran to erased media; a second accessor returning the stopping offset regardless points at cells a program cycle has already cleared, and on NOR that bank never boots again. `waymaker-fault`'s sweep demonstrates that mutation rather than arguing it. Since issue [#77](https://github.com/madmax983/waymaker/issues/77), the rule also fires when `Recovery` is `Clone` — a derive, a derive behind a `cfg_attr`, or a handwritten `impl`, resolved through the file's `use` aliases — because `Journal::after` takes a recovery by value so that one scan cannot hand out two writers at one offset, and a clone hands out two writers from one scan anyway; and it fires when the file declares no `Recovery` struct at all, rather than reading a rename as a clean pass. |
| `commit-discipline` | The two-barrier writer's public function surface differs from `source::APPEND_SURFACE`; or the typestate that makes design document §07's order unrepresentable comes apart — the staged frame grows a second method or the word `program`, the sealable frame grows anything but `commit`, the sealable frame is constructed anywhere but inside `payload_barrier`, or that barrier stops calling `storage.barrier` exactly once. Issue [#24](https://github.com/madmax983/waymaker/issues/24) asks that "it is not possible to program a seal without the intervening payload barrier having returned", and a `compile_fail` doctest in the crate proves that of the code as it stands. This is what stops it being given back: a `Staged::commit`, a `Journal::write` that did all four steps in one call, or a second constructor for `Sealable` would each break no other rule and turn a protocol into a convention. What it cannot see is whether the barrier is a real one — that is §12's contract and `waymaker-conformance`'s across-reset witness. |
| `capacity-reserve` | §10's capacity reserve gains a public function `source::CAPACITY_SURFACE` does not list, in either direction — or the gate comes apart: `source::CAPACITY_GATE` declares no inherent `impl`, declares `stage` other than exactly once, or its `stage` does not *open* with `source::CAPACITY_ADMISSION_CALL` and go on to `source::CAPACITY_DELEGATION`. §10 says "the runtime never overwrites committed history to make room", and every way of giving that back is an *addition*: a `Reserved::stage_unchecked`, a `Reserved::into_journal` handing the ungated writer back, a `Reserve::none()`, or a `Reserve::for_bytes(tail)` taking the figure from its caller rather than from a `BankLayout` — which is the sharpest of the four, because a reserve is only a promise because a layout vouched for it. The order half is the other word §10 uses: scheduling fails **early**, and issue #25 asks that the failure "produce no mutation at all". §12 says a failed program may still have changed media, so the only refusal that changes nothing is one taken before the device is called. The decision must therefore be the body's **first** statement, not merely one that precedes the delegation — review of this change wrote an admission inside `if false`, inside a closure nobody calls, and guarded so that only `RunStarted` reached it, and watched a rule that only checked the order stay green on all three. The blocks are read for the named type rather than by finding the first `fn stage` in the file, because the surface half counts only *public* functions and a private decoy carrying the pinned call stood in for the real one. What it cannot see is the arithmetic: a `tail_bytes` that quietly stopped counting the outcome record is `crates/waymaker-flash/tests/capacity.rs`'s, where `a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding` drives the wrong reserve and watches a run reach a state it can never leave. |
| `swap-discipline` | §10's bank swap gains a public function `source::SWAP_SURFACE` does not list, in either direction — or its step order comes apart: a state in `source::SWAP_TYPESTATE` declares anything but the one method its row names, `Staged` names `program`, a value in `source::SWAP_CONSTRUCTIONS` is built anywhere but inside the body its row names, `payload_barrier` stops taking `source::SWAP_BARRIER_CALL`, or a row of `source::SWAP_ERASE_CALLS` stops erasing exactly the bank it names, before a barrier, without naming the other one. Issue [#26](https://github.com/madmax983/waymaker/issues/26) states §10 as seven steps and two recovery rules — "a crash before step 5 recovers the old run, a crash after step 6 recovers the new run" — and every one of those is a statement about *where the barriers are*. A `Prepared::commit` skipping the header, a `Staged::seal_now` skipping the payload barrier, an `Installed` built anywhere but in `commit`, or a `Swap::install(bank)` taking the bank to erase from its caller would each break no other rule and turn a protocol into a convention. The erase rows are the sharpest: which bank a swap clears is derived from the authority the device booted, and a `prepare` that erased the *retiring* bank is a device clearing the run it is executing. What it cannot see is whether the barriers are real, which is §12's contract and `waymaker-conformance`'s across-reset witness, nor whether the crash windows behave — that is `crates/waymaker-fault/tests/swap.rs`, at every crash point of all seven steps. |
| `ctx-facade` | Issue [#35](https://github.com/madmax983/waymaker/issues/35)'s façade stops adding sugar and starts adding authority. `waymaker-embassy/src/ctx.rs` or `waymaker-embassy/src/journal.rs` gains or loses a public function `source::CTX_SURFACE` or `source::CTX_JOURNAL_SURFACE` lists; `ctx.rs` declares a future `source::CTX_FUTURES` does not name, or a number of `fn poll` bodies other than that list's length; `Ctx` declares a method `source::CTX_SURFACE` and `source::CTX_PRIVATE_METHODS` do not list between them — read at *every* visibility — or an associated constant; **any** file of `waymaker-embassy` or `waymaker-facade-demo` names one of `source::CTX_FORBIDDEN_VOCABULARY` — `StableStorage`, `Reserved`, `RecordRef`, `Recovery`, `ReplayMachine`, `BankLayout`, `Swap` — declares a `static`, declares a `macro_rules!`, or implements `Future` for a type `CTX_FUTURES` does not name; or a `waymaker-drive` module outside `source::FACADE_DRIVER_MODULES` names one of `source::FACADE_FREE_VOCABULARY`. §05's must-not-own cell for this crate is "on-media authority or hidden global state", and every way of giving that back is an *addition*: a `Ctx::record` that appends for itself, a journal method that answers a question the workflow never asked, a `static` buffer two runs share. Each would break no layering rule — `waymaker-embassy` may depend on `waymaker-flash`, so nothing else stops the façade reaching a writer — and pass every test, because the run still completes. The surface pin sets `poll` aside, because four futures declare it and a pin that is a list of names cannot speak about a name declared four times; `CTX_FUTURES` holds the count instead, and the *set* of types the crate implements `Future` for beside it, so a fifth future is a line a reviewer writes wherever it is declared. The vocabulary, `static`, macro and future-set bans read every file of `waymaker-embassy` rather than the two the surfaces are pinned in, because those four are statements about the crate: review of this change put a renamed `StableStorage`, a `pub static AtomicUsize`, a `macro_rules!` expanding a tenth public method into `impl Ctx`, and a fifth future in `dispatch.rs` — one file over — and watched a two-file version stay green on all four. Issue #106's review found the same four bans stopped at the crate boundary: `Bridge` moved to `waymaker-facade-demo`, and a `facade.rs` naming `StableStorage` directly passed every check here until the same four bans started reading that crate's files too, attributing each violation to whichever crate the offending file is actually in. Codex round 4 found the fifth, and it is about the *reader* rather than the rule: the future-set scan tested `starts_with("impl")` on the raw line while every classifier beside it stripped attributes first, so `#[rustfmt::skip] impl Future for SignalFuture` — a spelling `cargo fmt` preserves — walked past the one check that looks outside `ctx.rs`. The driver half is the fast half of issue #35's second "done when": every `waymaker-drive` module is held to naming none of `source::FACADE_FREE_VOCABULARY`, so a module added tomorrow is covered without anyone remembering a row — `source::FACADE_DRIVER_MODULES` is empty rather than gone, because issue [#106](https://github.com/madmax983/waymaker/issues/106) moved `facade`, `ota` and `provisioning` into `waymaker-facade-demo`, a crate above `waymaker-drive`, rather than exempting a file inside it. The half a scanner cannot decide is `cargo metadata`'s: a companion check reads the resolved graph directly and refuses a dependency from `waymaker-drive` to an Embassy crate — a direct one in any table, the manifest-only edge a source scan cannot see since a dependency needs no `use` to be linked, *and* one reached through a chain of `[dependencies]` at any depth, the shape a later normal dependency on some other crate would take. The walk stops at the first `[dev-dependencies]` or `[build-dependencies]` edge rather than crossing it, at the root or below: `waymaker-drive` dev-depends on `waymaker-rig`, which normal-depends on `waymaker-embassy` for `PersistentClock` (issue #34) — a legitimate edge a walk that crossed every kind would misattribute, and one that stopped at the root entirely would miss the same edge arriving through a *normal* dependency instead. Together the two halves are what make "the protocol is fully usable through the synchronous driver" a fact about the resolved graph rather than a claim a feature flag argued for. A fifth round found the direct half had a gap of its own: it had skipped every `Normal`-kind declaration on the assumption the walk would catch it, but `facade = { package = "waymaker-embassy", optional = true }` with no feature enabling it stays in `packages[].dependencies` and drops out of `resolve.nodes[].deps` entirely, so an unresolved optional dependency was invisible to both halves at once. The direct check now reads every declared dependency regardless of kind, and the two halves' findings are deduplicated by crate name so an *enabled* optional dependency — caught by both — is reported once. A sixth round found both halves resolved `waymaker-drive` with a bare name search, which `cargo metadata` does not promise returns the workspace's own package first: a same-named dependency at another version or source sorting ahead of it in `packages[]` would have let the real driver declare or reach Embassy unnoticed. Both now resolve the root through a lookup that also checks `workspace_members`. What it cannot see is a public *function* added from a sibling module — the two surfaces are pinned in one file each, exactly as `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one they pin — and it compares *names*, so a `Ctx::payload` that started handing back the journal's buffer is `crates/waymaker-embassy/tests/ctx.rs`'s. The `static` scan sets the `'static` *lifetime* aside before it looks for the identifier: the rule is about a `static` **item**, and `&'static str` is what compile-time metadata is spelled as — issue #36's activity names. An item is `static NAME:` and never `'static`, so the narrowing loses nothing, and `a_static_item_beside_a_static_lifetime_is_still_reported` is what says so rather than leaving it argued. [ADR 0032](docs/adr/0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md). |
| `dispatch-wiring` | Issue [#36](https://github.com/madmax983/waymaker/issues/36)'s dispatch path stops being a number. `waymaker-embassy/src/dispatch.rs` or `waymaker-embassy/src/wiring.rs` gains or loses a public function `source::DISPATCH_SURFACE` or `source::WIRING_SURFACE` lists, or declares one of them twice so the pin can no longer speak about it; a type in `source::WIRING_TYPE_METHODS` — `Activity`, `Table` — declares a method set other than its row's, read at *every* visibility, stops being a braced struct, or declares a public field; or a body in `source::WIRING_SELECTION_BODIES` is declared other than exactly once or names one of `source::WIRING_SELECTION_FORBIDDEN`; or either module is gone, so the pin checks nothing. Issue #36 states two of its work items as absences — "numeric `ActivityKind` on the dispatch path", and "no dynamic workflow loading and no string-addressed activity registry" — and every way of giving either back is an *addition*: a `Table::by_name`, a `Table::register`, a `pub rows` field a caller can rewrite at run time, a lookup that falls back to a label. Each would break no layering rule, need no dependency, and pass every test in the workspace, because the run still completes. The selection half is read out of the file rather than out of an `impl` body, because `poll_dispatch` is a *trait* method, which `inherent_impl_bodies` skips. The declaration is counted first, for `effect-protocol`'s reason: `braced_body` takes the first match, so a decoy above the real one is what a first-match scan reads. Four of the halves are things review demonstrated rather than things anybody predicted, and each was watched passing on a mutation before it was closed: a free `pub(crate) fn by_name` at *module* scope, which is on neither a surface pin nor a method pin — and which the label ban does not catch either, because `names_identifier` reads `by_name` as one identifier; a `register` beside it, which is the dynamic-loading non-goal as a free function; a `mod shim { pub struct Table {} }` above the real one, whose empty body is what the public-field scan read; and `Activity::name` renamed to `label` with the accessor left in place, which frees a selection body to compare it and names nothing forbidden. The function pin therefore reads every `fn` in the file at every visibility, and the field pin compares names as well as visibility — which is also what refuses a tuple struct, since one has no braced body of its own for the field scan to read. What it cannot see is a function added from a sibling module — it pins two files, exactly as `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one they pin — and it compares *names*, so that a label never reaches media is `crates/waymaker-drive/tests/dispatch.rs`'s, which reads the device image back with a needle short enough to fit a record. [ADR 0033](docs/adr/0033-the-dispatcher-answers-in-a-bound-the-journal-states.md). |
| `codec-is-optional` | Issue [#37](https://github.com/madmax983/waymaker/issues/37)'s codec helpers stop being optional. A `waymaker-embassy` module other than `source::CODEC_PATH` names one of `source::CODEC_VOCABULARY` — `serde`, `postcard`, `Serialize`, `Deserialize`, `DeserializeOwned`, `Coded`, `Format`, `Postcard`, `FromPostcard`, matched as *identifiers* over code with its comments and `#[cfg(test)]` modules removed; an item of the codec module that names one of them — anywhere in the item, not only on its declaration line — carries no bare `#[cfg(feature = ..)]`; `source::CODEC_FREE_TRAIT` is missing or is itself behind a feature; a dependency in `source::CODEC_DEPENDENCIES` is declared without `optional = true`; or a feature in `source::CODEC_FEATURES` stops enabling what its row names. §02 decision 4 says Serde and Postcard are "optional conveniences, never wire-format requirements", and the way that is given back is not a dependency — it is a *bound*: a `Ctx::activity` asking for `DeserializeOwned`, or a `Handoff` naming a codec type, makes every workflow carry the codec whatever the manifest says, and every other rule stays green because the run still completes. The manifest half is the other one that fails silently: a `serde` declared without `optional` links in every build, and the size report's row for it then measures an image that already had it. What it cannot see is a codec named from a sibling crate, or a bound written without one of those words — a type alias for `DeserializeOwned` declared in the codec module and used in `ctx.rs` names nothing forbidden. It reads *items* rather than lines, because review of this change wrote a `pub struct Bridge {` whose declaration line named no codec and whose field below it did, and watched a line-based version stay green. Two things it does not read: which feature gates an item — `code_only` removes string literals along with comments, and any single positive feature gate keeps the item out of a default build, which is the whole of the claim — and a compound `#[cfg(all(..))]`, `any(..)` or `not(..)`, which is *not* read as gating, so an item behind one is reported rather than trusted. It pins one crate and one module of it, the way `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one file they pin. [ADR 0034](docs/adr/0034-a-codec-is-a-bridge-behind-a-feature-and-the-probe-mirrors-it.md). |
| `rig-oracle` | `waymaker-rig`'s oracle or its census gains a public function `source::RIG_AUDIT_SURFACE` or `source::RIG_CENSUS_SURFACE` does not list, in either direction — or either file is gone, so the pin checks nothing. A rig is the one piece of code here whose bugs are *invisible*: a firmware bug shows up as a failing test, a rig bug as a passing one. Every way of giving the instrument back is an addition — an `Audit::assume_passed`, an `Audit::ignore`, a `Breach::suppress`, a second `finish` taking the authority count as advisory, a `Coverage::force_complete`, a `Gap::ignore` — and each would break no other rule, need no dependency and pass every test that exists. The census is a file of its own rather than part of `phase.rs` for this rule's sake: `Phase` and `ResetCause` each declare an `index`, a `from_index` and a `name`, and a pin that compares names cannot tell two such declarations apart. What it cannot see is whether the oracle's arithmetic is right — `crates/waymaker-rig/tests/teeth.rs` is what holds that, with two writers wrong in one way each and a control writer required to pass. |
| `transition-surface` | The replay machine's public function surface differs from `source::TRANSITION_SURFACE`, in either direction. Issue #15 asks for divergence that is "terminal and loud: no reinterpretation of history, no best-effort recovery", and every word of that is an *absence*: a `reset`, a `clear_divergence`, a `force` flag on `intent` would each break no other rule and turn "stop, never guess" into a suggestion. A test cannot call a function that is not there, so the surface is pinned instead. |
| `timer-capability` | Design document §11's timer semantics stop being the ones that were reviewed, in any of its four halves. The *kernel* half: `waymaker-core/src/timer.rs` gains or loses a public function `source::TIMER_SURFACE` lists, or a type in `source::TIMER_TYPES` — `TimerSpec`, `ClockCapability`, `Deadline` — is declared twice, is gone, or declares a member set other than its row's. It also pins each type's *methods*, at every visibility (`source::TIMER_TYPE_METHODS`), and refuses a public field on a type in `source::TIMER_BRACED_STRUCTS`; and it checks that `waymaker-core/src/lib.rs` re-exports each pinned type. The *façade* half: `waymaker-embassy/src/clock.rs` gains or loses a public function `source::CLOCK_SURFACE` lists, names one of `source::CLOCK_FORBIDDEN_VOCABULARY` — `AfterBoot`, `BootOnly`, matched as *identifiers* over code with its comments stripped — or names a `TimerSpec` that is not `source::CLOCK_SPEC_CONSTRUCTION` — as a name and not a prefix — or names none at all. The crate-root half compares the *source* name of `pub use timer::…`, so an alias or a path through a submodule is not the pinned type. The *board* half reads the two modules `source::BOARD_CLOCK_MODULES` names — `waymaker-rig/src/rtc.rs` and `waymaker-rig/src/epoch.rs` — and fires when either gains or loses a public function its `surface` lists, declares a method its `methods` list does not have at *any* visibility, declares a public field on its driver type, declares any constant that is not a `const fn`, names one of `source::CLOCK_FORBIDDEN_VOCABULARY`, or names one of `source::BOARD_CLOCK_FORBIDDEN_VOCABULARY` — `TimerSpec`, `ClockCapability` — because a driver reports a reading and decides no policy. §02 decision 8 is that timer semantics match the hardware's clock and never pretend, and every way of giving that back is an *addition*: a `TimerSpec::best_effort(capability)`, a `Timer::arm_or_downgrade`, a `Timer::force_elapsed`, a `PersistentClock::now_or_zero`, a third `Deadline` meaning "cannot tell", or a second constructor for a persistent timer that takes a reading rather than a clock. Each would break no layering rule, need no dependency, and pass every other gate. The kernel half is three checks rather than one because review of that change defeated the version without them and watched the gate stay green: a `pub(crate) const fn arm_or_downgrade` on `impl Timer`, which a surface pin counting `pub ` and not `pub(` cannot see; a `pub spec` field on `Timer`, which adds no function and changes no member and makes the invariant the whole design rests on a value any caller can set; and a `pub const BEST_EFFORT: Self = Self::AfterBoot { ticks: 0 }` on `impl TimerSpec`, reached from the façade as `TimerSpec::BEST_EFFORT` behind an "epoch not restored yet" guard — no banned identifier, no changed surface, and a persistent deadline served by a clock that restarts on every reset. So the façade's spec pin is positive rather than negative: it must name a spec, and every spec it names must be the persistent one. Codex then found two more of the same shape, and both are tests: `TimerSpec::AtPersistentTimeFallback` walked past a `starts_with`, and `pub use timer::TimerPolicy as TimerSpec` — or `pub use timer::compat::TimerSpec` — satisfied a root check that only asked whether the identifier appeared. Review of the *board* half then landed the same three on it — a `pub(crate) fn counter_unchecked` on `impl Rtc`, a `pub registers` field on `Rtc`, and a `pub const ASSUME_HELD: Self = Self::Held` on `impl Continuity` — which is why the board half carries a method pin at every visibility, a public-field refusal, and a constant ban read over the whole module rather than over one `impl` body: the constant was declared on the *enum a driver answers with*, which no per-driver pin looks at. The member sets are a wire-format commitment as much as an API one — issue #33 puts the clock kind on media for the life of the format. Read with `#[cfg(test)]` modules removed, for `integrity-check`'s reason. What it cannot see is an `admits` that stopped consulting its argument or an `evaluate` that credited an interval it could not measure, which is `crates/waymaker-core/tests/timer.rs`'s; and each half pins one file, so a door added from a sibling module is a door the rule is silent about — including a `macro_rules!` in a sibling module invoked inside a pinned `impl`, which review of the board half landed and which expands to exactly the accessor the pin is written against. Issue [#99](https://github.com/madmax983/waymaker/issues/99) closed the two doors `BEST_EFFORT` opened. `TimerSpec`, `Timer` and `ClockCapability` may declare no associated constant. `source::CLOCK_KIND_CONSTANTS` pins `ClockKind`'s two constants by name and value, since a `u8` newtype has no member and no method for the pins above to read. And the façade's `TimerSpec` pin resolves `use` aliases before it scans, so `use TimerSpec::BEST_EFFORT as PERSISTENT_SPEC;` is seen for what it names. Codex then found six more on this pull request: a raw `r#BEST_EFFORT` no longer drops the whole declaration from what the scan sees; `ClockKind` refuses a trait `impl`, which could otherwise carry a constant the pin above never reads; `ClockKind` declared twice is reported rather than resolved by whichever value a map collect happens to keep; a leading attribute on the same line as a declaration no longer hides it from the scan; and a self type's path — `impl crate::timer::ClockKind` and `impl Forge for crate::timer::ClockKind` — is stripped before either scan compares a name, a fix shared by every other pin built on the same two functions. [ADR 0028](docs/adr/0028-timer-semantics-are-a-spec-a-capability-and-no-downgrade.md). |
| `kernel-boundary` | Design document §06's kernel boundary stops being the one that was reviewed, in either half. The *shape* half: a type in `source::BOUNDARY_TYPES` — `EffectRequest`, `Intent`, `Resolve`, `Outcome`, `Next` — declares a member the pin does not have, or stops declaring one it does, or is gone so the pin checks nothing. Issue [#28](https://github.com/madmax983/waymaker/issues/28) asks that "adding a new record kind does not change this signature", and §09 numbers eleven record kinds of which five — `TIMER_SCHEDULED`, `TIMER_FIRED`, `VERSION_MARKER`, `SIGNAL_RECEIVED`, `CHILD_STARTED` — have no body yet. A `Resolve::TimerFired` arriving with the first of them would break no other rule, need no dependency, and turn one boundary into a boundary per record. The *routing* half: `waymaker-drive`'s driver stops naming a row of `source::BOUNDARY_DECISIONS`, or grows one of `source::DRIVER_FORBIDDEN_VOCABULARY` — `RecordKind`, `Step` or `EffectIdAllocator`, matched as *identifiers*, because a `Step::` spelling ban is evaded by `Step ::Record` and by `use …::Step as S;` and fires on an unrelated `BootStep::`. A driver that decided from a record rather than from `Intent` and `Resolve` would be a second transition table, and the one below it would no longer be where §08 is enforced. The allocator is issue [#30](https://github.com/madmax983/waymaker/issues/30)'s: §14's fourth guarantee is that a retry and a reboot redeliver the *original* identity, and the driver keeps it by never having an identity of its own — every `(RunId, EffectSeq)` it dispatches under comes from `Intent::Schedule` or `Resolve::Redeliver`, and a fresh mint for an outstanding effect is a second effect to every downstream system. `RecordRef` is not on the list because the driver constructs them — the kernel names the record it wants written and something has to write it — and it reads two, which [what is not checked](#what-is-not-checked) names rather than leaves implied. Both halves read the file with its `#[cfg(test)]` modules removed, for `integrity-check`'s reason: a decision named only under `cfg(test)` discharges nothing about the code that ships. A type declared *twice* fails too — `braced_body` reads the first declaration, so a decoy above the real one is what a first-match scan reads. One rule id because it is one decision. A type in `source::BOUNDARY_TYPES` may also declare no associated constant — issue [#99](https://github.com/madmax983/waymaker/issues/99)'s shape. A constant is neither a member nor a function, so it is invisible to the member pin above. So is one carried by a trait `impl`, which Codex found on this pull request and which each of the five types now refuses outright. What it cannot see is a *widened* member behind a name already on the list, and a driver that names every decision and then ignores one; `crates/waymaker-drive/tests/` is what holds the behaviour. [ADR 0024](docs/adr/0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md). |
| `effect-protocol` | Design document §07's seven-step effect protocol stops being the one that was reviewed. `waymaker-drive/src/effect.rs` gains or loses a public function `source::EFFECT_PROTOCOL_SURFACE` lists; the state in `source::EFFECT_DISPATCH_STATE` declares anything but the two methods its row names; a type in `source::EFFECT_TYPE_METHODS` is declared twice, stops being a braced struct, declares a public field, or declares a method set other than its row's — read at *every* visibility, because a surface pin counts `pub ` and not `pub(`; a type in `source::EFFECT_NO_SELF_LITERAL` builds a `Self` or implements a trait; the file declares a module; a value in `source::EFFECT_CONSTRUCTIONS` is built outside the two bodies its row names, or is not built inside each of them; a name in `source::EFFECT_PROOF_FIELDS` is assigned to, has a `&mut` reference taken to it, has a method called on it, or is bound `ref mut` in a struct pattern, anywhere in the file; a `type` alias anywhere in the file targets a qualified associated-type projection; a body in `source::EFFECT_STEP_BODIES` — located in its owning type's own `impl` blocks — stops taking each of `source::EFFECT_STEPS` exactly once, in that order, and at the body's own nesting depth — braces, parentheses and brackets together — or declares a closure or a short-circuit (`|`, `&&`); `source::EFFECT_PROOF_AFTER`'s body builds a proof before the step its row names; or `redelivering` names one of `source::EFFECT_REDELIVERY_FORBIDDEN`. Issue [#29](https://github.com/madmax983/waymaker/issues/29) asks that step 4 be unreachable without step 3, "structurally, not by review", and every way of giving that back is an *addition*: a `DurableIntent::new`, a public `id` field on it, an `Effect::dispatchable_now`, a `Dispatchable::into_writer`, or a `Resolution::outcome` a caller can call before step 7. Each would break no other rule, need no dependency, and turn §02 decision 3 back into a convention. The step rows are the other half: §07 states the frame, the payload barrier and the seal twice, and a body that takes them in another order is not that protocol. Six of the halves are things review demonstrated rather than things anybody predicted, and each was watched passing on a mutation before it was closed: a `pub(crate) const fn new(id) -> Self` on `DurableIntent`, wired into the driver, with the gate green; a `Self { .. }` the name-based construction pin cannot see; a private free `fn resolve` above the real one, taking all three steps while `Dispatchable::resolve` stopped at the payload barrier; a decoy `pub struct` above the real one; a `pub` tuple field, where `braced_body` reads the first `{` after a declaration and so reported on the `impl` block below; and an `impl` inside a nested module, which `inherent_impl_bodies` cannot see because it reads `impl` at column zero. Codex round 1 found the seventh: the construction pin and the order pin were independent, so an early `return` carrying a freshly built `Dispatchable` satisfied both. Codex round 2 found the eighth, which is the sharpest of the lot: brace depth is not execution, and `false.then(|| self.writer.stage(..).payload_barrier(..).commit(..))` has no braces at all — three pinned calls, in order, at brace depth zero, in a closure nothing runs. The depth counts parentheses and brackets now, and a step body may not declare a closure. Round 3 found the ninth in the same family — `false && self.writer.stage(..)?…` puts every call once, in order, at depth zero, on a right-hand side that never runs — so the two short-circuit operators are refused as well. A scanner cannot follow control flow, so what it does instead is refuse the constructs that create it, and [what is not checked](#what-is-not-checked) says which. Round 2's other finding did not reproduce: `public_functions` counts a trait `impl`'s method as callable, so the surface pin already rejected an `impl From<EffectId> for DurableIntent`; the direct refusal is here anyway, because the two pins that are *about* construction do go blind on a trait `impl` and a guarantee should not rest on another pin's side effect. Read with `#[cfg(test)]` modules removed, for `integrity-check`'s reason. A type in `source::EFFECT_TYPE_METHODS` may also declare no associated constant — issue [#99](https://github.com/madmax983/waymaker/issues/99)'s shape, closed here the way `timer-capability`'s kernel half closes it. A constant is neither a member nor a function, so it is invisible to the method pin above. So is one carried by a trait `impl`: `DurableIntent` and `Dispatchable` already refused one for the construction pin's reason, and `Effect` now refuses one for this reason, which Codex found on this pull request. What it cannot see is a step added from another file — it pins one file, exactly as `capacity-reserve` and `recovery-surface` do — nor whether the barriers are real, which is §12's contract and `waymaker-conformance`'s across-reset witness; the crash windows are `crates/waymaker-drive/tests/crash.rs`. [ADR 0025](docs/adr/0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md). `DurableIntent::kind`, `Dispatchable::perform`, `CheckedDispatch::bytes` and `CheckedDispatch::durable_intent` are issue [#92](https://github.com/madmax983/waymaker/issues/92)'s. A `DurableIntent` now carries the request step 3 committed. So `kind` cannot be read from a second argument, and `perform` refuses `input` that disagrees with the digest, before an activity is ever asked. `Activities::perform` then takes the one `CheckedDispatch` that refusal builds, not an identity and a `&[u8]` as two separate arguments, so calling it directly carries no bytes the check did not vouch for and no identity paired with another call's bytes either. None of the four is a storage step, so `EFFECT_STEP_BODIES` is unchanged. `source::CHECKED_DISPATCH_CONSTRUCTION` is `EFFECT_CONSTRUCTIONS`'s twin for `CheckedDispatch`, one body rather than two: `Dispatchable::perform` is the only place that may build one, so `EFFECT_NO_SELF_LITERAL`'s ban on a `Self` literal and a trait `impl` is not carrying the whole guarantee alone against a `pub(crate)` forge elsewhere in the file, which is what Codex's third round on this issue found. A fourth round found that the construction pin's own scanner, `struct_literal_counts`, resolved only `use` aliases — `type Unchecked<'a> = CheckedDispatch<'a>;` followed by a literal spelled `Unchecked { .. }` built the pinned type under a name the pin never compared against. The scanner now resolves `type` aliases the same way it already resolved `use` aliases (issue #99), chased through a chain of either kind, which closes the gap for every construction pin built on it — `EFFECT_CONSTRUCTIONS` included — rather than for this one alone. A fifth round found the scanner's own remaining blind spot: it walked file items and inline modules only, so a `type` alias declared *inside* a function body — legal Rust — was invisible to it just as the file-scoped one had been. `struct_literal_counts` now gives every block its own alias scope, entered on the way in and popped on the way out, and resolves a name against the innermost scope that declares it — a local alias correctly shadows a same-named one declared elsewhere in the file, rather than the scan picking whichever declaration happens to sort first in a flattened list. A sixth round found a hole in what counts as an alias's right-hand side rather than in where it is looked for: `type Unchecked<'a> = (CheckedDispatch<'a>);` is valid Rust, `#[allow(unused_parens)]` lets it through `-D warnings`, and `syn` keeps the parens as their own `Type::Paren` node — so the `Type::Path` match that reads a target saw nothing and built no alias at all. `type_alias_target` now unwraps `Type::Paren`, and `Type::Group` beside it for the same reason, recursively. A seventh round found a different shape of gap: `CheckedDispatch`'s, `DurableIntent`'s and `Dispatchable`'s fields are private to the *module*, not to the type, so a sibling function anywhere in `effect.rs` could already write `dispatch.bytes = other;` on a legitimately built value — no struct literal anywhere, so no construction pin sees it — or take a `&mut` reference to the field and rewrite it through `core::mem::swap`, `core::mem::replace`, or any other `&mut`-taking call, none of which spells `=` either. `source::EFFECT_PROOF_FIELDS` names the fields this matters for — `id` and `request` from `DurableIntent`, `intent` from `Dispatchable` and `CheckedDispatch`, `bytes` from `CheckedDispatch` — and `check_effect_proof_fields_are_not_rebound` refuses both routes, anywhere in the file. Nesting the type in a private submodule to get real per-type field privacy was considered and rejected: the file-declares-a-module refusal a few clauses up exists precisely so a construction site cannot hide there, and a submodule added for this reason would open that same hole. An eighth round found the third route the first two left open: a method call. `dispatch.bytes.clone_from(&other)` reassigns `bytes` through an *implicit* `&mut self` autoref — nothing in the source spells `=` or `&mut`, so neither the assignment check nor the reference check sees it, and whether a given method really takes `&mut self` is a question `syn` cannot answer without type inference. So `mutated_field_names` refuses every method call whose receiver is a guarded field, not only the ones a reviewer could confirm mutate — over-broad the same way every other scanner here is, and free here because no method is ever legitimately called directly on one of these fields today, only on the whole value through its own accessor, whose receiver is a plain path rather than a field access. A ninth round found two more gaps in two different mechanisms. The first is a fourth route into the field-rebinding problem: `let CheckedDispatch { bytes: ref mut slot, .. } = dispatch;` borrows `bytes` mutably through the pattern itself, with no assignment, `&mut` expression or method call anywhere for the first three routes to see; `mutated_field_names` now also refuses a `ref mut` binding on a guarded field in any struct pattern, found at any nesting depth by walking the sub-pattern with a nested visitor rather than checking only its outermost shape. A field bound `mut slot` with no `ref` is deliberately left alone, since it moves or copies the value into a fresh local rather than aliasing the original place. The second is in the type-alias resolution itself: `type Unchecked = <Via as Alias>::Dispatch;` is a qualified associated-type projection, which `type_alias_target` explicitly skips — correctly, since resolving what a trait's `impl` names as its associated type needs type inference this scanner does not have — but skipping was silently permissive, since the alias built nothing and so counted as nothing while the projection itself could still name `CheckedDispatch`. Unlike a tuple, a reference or a trait object, none of which can ever appear where `Name { .. }` construction syntax is legal, a projection genuinely can resolve to a struct usable that way — so `qself_type_alias_names` reports every such alias, and a new check refuses the file outright over it, a hard refusal of the construct rather than an attempt at type resolution neither `syn` nor this scanner can safely do. A tenth round found that `mutated_field_names` checked only the outermost field of a chain — `dispatch.intent.request.kind = x;` assigns to `kind`, not a guarded name, but `intent` and `request` are guarded *ancestors* in the same chain — so `note` now walks the whole chain back to its root for all three routes at once. Two further tenth-round findings — an implicit mutable alias match ergonomics can bind with no `ref`/`mut`/`&mut` written anywhere, and a generic type alias with a trait bound (`type Unchecked<T: Alias> = T::Dispatch;`) projecting with no `qself` for the ninth round's check to key on — are real and not fixed here: ten rounds deep, both need genuinely new detection machinery rather than a completion of what exists, and this project's review-depth guidance is to stop past two or three rounds and open an issue once a fourth still finds real bugs. They are issue [#171](https://github.com/madmax983/waymaker/issues/171). An eleventh round, on the merge of this branch with a concurrent one, found a gap in the tenth's own fix: `dispatch.intent.request.kind ^= 1;` rewrites `kind` in place, but `syn` parses every compound-assignment operator — `+=`, `^=`, and the other eight — as a `BinOp` on an `Expr::Binary`, never as `Expr::Assign`, which is `=` alone, so the visitor that only visited `Expr::Assign` never called `note` for one. `mutated_field_names` now also visits `Expr::Binary` and calls `note` on the left operand for any of the ten assignment operators. A twelfth round found a gap in the eleventh's own review rather than its fix: `(dispatch.bytes,) = (replacement,);` is still an `Expr::Assign`, but its left side is `Expr::Tuple`, not `Expr::Field`, and `note`'s chain walk only recognised the latter. `note` now recurses into each element of a tuple or array and each field's value in a struct literal — arbitrarily nested — before falling back to the field-chain walk, so a field buried inside a destructuring assignment target is found the same way one behind a compound assignment is. A thirteenth round found a gap in the tenth round's own chain walk rather than in the eleventh's or twelfth's: `(dispatch.intent.request).kind = x;` puts a guarded ancestor behind an `Expr::Paren`, and the chain walk's `while let Expr::Field(field) = current { .. current = &field.base; }` loop stopped the moment `.base` was a paren rather than another field access, so `intent` and `request` were invisible behind their own parenthesized prefix. The walk now unwraps `Expr::Paren` and `Expr::Group` as it descends, the same two wrappers `type_alias_target` already unwraps for the same reason. Three rounds deep in this post-merge chain — this project's own review-depth guidance is to stop past two or three and open an issue once a fourth still finds real bugs — so a fourteenth finding of the same shape goes to a new issue rather than a fourteenth fix here. Review of the merge itself found a separate bug, in code the merge introduced rather than in `mutated_field_names`'s own chain: `resolve_local_alias_chain`, written to keep function-local type-alias resolution working after issue #169's rewrite of `struct_literal_counts`, resolved a block's own aliases with `collect_item_aliases`, which recurses into any `mod` the block declares — so a nested module's own, private alias could shadow the block's real one at the construction site. `own_aliases` is generalized to take any `&syn::Item` iterator, and `resolve_local_alias_chain` now calls it instead — the same non-recursive, own-level-only collection a module lookup already gets. Review of that fix found the inverse leak the same round: `visit_item_mod` pushed the nested module's own items onto `self.stack` and popped them on the way out, but never touched `self.block_items`, so a block's own local aliases stayed visible while the visitor traversed a `mod` declared inside that same block — a scope a nested module never inherits, whether the enclosing scope is a function body or another module. `visit_item_mod` now sets `block_items` aside with `core::mem::take` before descending and restores it afterward, the same discipline `self.stack`'s own push/pop already has. Codex then found a gap in a different mechanism, not in the alias scanner at all: `syn::Visit` does not descend into a `macro_rules!` body, which is an opaque token stream, so a local macro defined and invoked inside `effect.rs` and expanding to `CheckedDispatch { intent, bytes }` builds the pinned type at a construction site none of `struct_literal_counts`'s callers can see. Rather than attempt to expand or inspect a macro body — the same kind of type resolution this file already declines for a qualified associated-type projection — `check_effect_types` now refuses the file outright over a bare `macro_rules` identifier, the same shape `ctx-facade` already uses for the same reason: a scanner cannot expand a macro, so it refuses the construct. Codex found the gap that ban left open the same round: it reads the *definition* identifier, so an invocation of a macro defined anywhere else in the crate — `emit!(CheckedDispatch { intent, bytes })`, spelling `macro_rules` nowhere — reaches the identical opaque-token blind spot. `crate::parse::invokes_any_macro` replaces the identifier check with a `syn::Visit` override of `visit_macro`, the one node type `ItemMacro`, `StmtMacro`, `ExprMacro`, `TypeMacro` and `PatMacro` all share, so a single override catches every invocation shape — `macro_rules!` included — without naming any of them individually; `effect.rs` now refuses the file outright over any macro use at all, outside `#[cfg(test)]`. Codex found a third shape in the same family the round after: an attribute macro or a custom derive is a `syn::Attribute`, not a `syn::Macro` invocation, so `invokes_any_macro`'s `visit_macro` override — however exhaustive over every invocation shape — never sees `#[forge]` on a method or `#[derive(Forge)]` on a struct, either of which expands in its own defining crate with nothing here able to read what comes out. `crate::parse::unaudited_attributes` closes it the same way as the two before it: rather than resolve what an attribute expands to, every attribute in `effect.rs` is required to be one of a fixed set (`source::EFFECT_ALLOWED_ATTRIBUTES`) the compiler itself interprets with no macro behind it, and a `#[derive(..)]` to name only the compiler's own derives (`source::EFFECT_ALLOWED_DERIVES`) — since `#[derive(A, B)]` can mix an inert one with a custom one in the same attribute, each name in the list is checked on its own rather than the attribute as a whole. `cfg_attr` is refused outright rather than classified recursively, since it can emit an arbitrary attribute and `effect.rs` has no legitimate use for one. Three rounds deep in this macro-opacity family — this project's own review-depth guidance is to stop past two or three and open an issue once a fourth still finds real bugs — so a fourth finding of this shape goes to a new issue rather than a fourth round here. A fourth round of Codex review found two more, back in the alias-scoping mechanism rather than the macro one: one is closed here, and one is not. `resolve_local_alias_chain` correctly stops at the end of a block's own aliases — a block-local `type Inner = Outer;` where `Outer` is itself a *module*-level alias — but the caller took that partial result as final rather than feeding it on to `resolve_segments`'s own module-level lookup, so `Inner { .. }` was never counted as whatever `Outer` really named. `resolve_local_alias_chain` now reports whether its own chain ended on an absolute alias or simply ran out of block-local names to try, and in the latter case the leftover head is resolved again through `resolve_segments_from` — the module-lookup half of `resolve_segments` split out for reuse — which is a no-op if the name is not itself a module-level alias. The other finding is not fixed: `own_aliases` skips only `#[cfg(test)]`, so two mutually exclusive `#[cfg(..)]`-gated `type` aliases sharing one name both enter the alias list, and whichever is declared last wins the lookup regardless of which one a real build would actually compile — the same "`cfg` is not evaluated" limitation this file's own parsing documentation already states as an accepted, structural property of every scanner in this family, met here in a form specific enough that resolving it would mean evaluating `cfg` predicates this workspace has decided its scanners do not attempt. Codex found one more on review of that same merge, in a different dimension from every one above: `CHECKED_DISPATCH_CONSTRUCTION` counts *where* `CheckedDispatch` is built and never *what precedes it*, so deleting the length/digest check inside `Dispatchable::perform` outright, or replacing its literal's `bytes: input` field with a hardcoded `bytes: &[]`, still leaves one construction, all of it inside the pinned body — the pin's own counts do not move, so `effect-protocol` stays green over a value `Activities::perform` receives with no real check behind it. This is not a new class of gap; it is `capacity-reserve`'s own "the admission the gate opens with is reachable in every case" limit, met here for the first time: a scanner counts a construction's position, never whether the code around it does what it claims, and closing that needs the semantic evaluation this whole scanner family has consistently declined to build. What holds the real guarantee is not this rule but two tests — `perform_refuses_input_that_disagrees_with_what_was_scheduled` fails if the check is weakened or removed, and `perform_binds_the_kind_and_forwards_matching_input` fails if the literal stops forwarding the checked bytes — which is this project's own "no invariant ships without a test" answer for exactly the class of defect a syntax-only pin cannot see. Codex found one more on review of the same merge, in `EFFECT_ALLOWED_DERIVES` rather than in the two tests: `Default` sat on that list beside `Clone`, `Copy` and `Debug`, and unlike every other entry on it, `Default` *constructs* — `#[derive(Default)]` on `DurableIntent` or `CheckedDispatch` would generate `CheckedDispatch::default()`, a second public constructor that is neither a `Self` literal `EFFECT_NO_SELF_LITERAL` refuses nor a construction site `CHECKED_DISPATCH_CONSTRUCTION` counts, since both look for something written in source and a derive's expansion is neither. `Default` is removed from the allowlist rather than special-cased for the three proof types, matching every other refusal in this file's macro-and-derive family, which is stated file-wide for the same reason a module ban and a macro-invocation ban both are: resolving which types are "proof types" needs the same name resolution this scanner has consistently declined to build. `a_derive_default_on_a_proof_type_is_reported` is the regression.  Issue [#171](https://github.com/madmax983/waymaker/issues/171) then closed two further gaps in the same family, independently of the rounds above: a field bound `mut slot` with no `ref`, or a bare `field` with neither, had been read as safe on the reasoning that it moves or copies the value into a fresh local — true only when the scrutinee is owned, and `syn` sees the pattern's syntax, never whether `dispatch` is `&mut Foo` or `Foo`. Rust's match ergonomics (RFC 2005) let the scrutinee's own type pick the *default binding mode* for an identifier pattern with no `ref`/`mut` of its own, so the identical bare `bytes` aliases the field as `&mut &'a [u8]` under a `&mut` scrutinee and only copies it under an owned one — one syntax, two meanings, decided by a type this scanner never reads. `mutated_field_names` now refuses *every* named binding on a guarded field, of any form, which is sound without the type inference a narrower fix would need. And `type Unchecked<T: Alias> = T::Dispatch;` has no `qself` at all — no `<`, no `as` — so it parsed as an ordinary path, `["T", "Dispatch"]`, chased as one and dead-ending at `Dispatch` rather than reaching whatever `T::Dispatch` really names; `type_is_qself_projection` now also reports a bare `T::Assoc` whose first segment is one of the alias's own declared generic type parameters, the same unresolvable shape as `<T as Trait>::Assoc` spelled without the disambiguating syntax. Scoped to the alias's own parameters on purpose: a UFCS-style projection through a *concrete*, already-defined type (`type Foo = Via::Dispatch;`) is a module-qualified path by every syntactic signal available, and telling the two apart needs real name resolution this scanner does not have — a residual limit stated where it bites rather than guessed past. A further review pass found a third gap the same family had left open: `let ref mut slot = x.field;` borrows the field directly through a `let` binding's own pattern, with no struct pattern for the sixth route to read and no `=`, `&mut` expression or method call for the first five — `visit_local` now notes the initializer whenever the whole pattern is one `ref mut` identifier binding straight to it. And `implements_trait_for` — the reader `timer-capability`, `kernel-boundary` and this row's own constant bans share for a trait `impl` — searched for a bare `"\nimpl"`, so an `impl` preceded on its own line by a leading attribute (`#[rustfmt::skip] impl Forge for ClockKind { .. }`, which survives `cargo fmt`) was invisible to it, the same blindness `next_impl_line`'s own doc comment already records for `inherent_impl_bodies`'s reader; it is walked with `next_impl_line` now, the same fix shared by every pin built on it. Issue #184 closed the same shape one level over `qself_type_alias_names`: a function's or an `impl`'s own generic parameter can bind an associated type to a guarded name directly — `T: Alias<Dispatch = CheckedDispatch>` — and `T::Dispatch { .. }` builds the guarded type under a name no construction pin compares. `generic_assoc_type_bindings_naming` reads every such binding, outside `#[cfg(test)]`, and refuses the file when its value names a guarded type — no type inference needed, since the name is written in the bound's own text — resolved through the same `use`/`type` alias chain `struct_literal_counts` already chases for a struct literal's own path, since a plain `type Hidden = CheckedDispatch;` beside the bound is not enough to hide it (adversarial review of the first version, which read the binding's own text alone); what it does not resolve is a projection as the binding's own value, the same residual `qself_type_alias_names` already leaves for a `type` alias's own target. [ADR 0050](docs/adr/0050-a-durable-intent-carries-its-request-and-perform-checks-it.md). Issue [#189](https://github.com/madmax983/waymaker/issues/189) found the same gap one level up. `syn` does not expand a macro. So `mutated_field_names` cannot see a rebind written inside a macro's own tokens. This was checked, not assumed: `effect.rs` already refuses any file with a macro in it, outside `#[cfg(test)]`. So a macro that hides a rebind is still caught, before this narrower gap can matter. Two tests prove it: `crate::parse::raw_identifier_tests::a_rebind_hidden_inside_a_macro_invocation_is_invisible_to_this_scan_alone` pins the scanner's own blind spot, and `a_proof_field_rebind_hidden_behind_a_macro_is_reported` pins the file-level catch. |
| `embassy-below-facade` | A *layer* other than `waymaker-embassy` reaches the Embassy ecosystem. The rule iterates `policy::LAYERS`, so `xtask` and the size probe are outside it. |
| `layer-missing` | A crate named in `policy::LAYERS` is not in the workspace. |
| `layer-not-local` | A crate with a layer's name resolves to a registry crate rather than the path dependency. |
| `workspace-membership` | A workspace member is neither a layer, declared host tooling, a measurement crate, an emulation image, nor declared test support. |
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
| `toolchain-targets` | `rust-toolchain.toml` stops pinning `thumbv6m-none-eabi` or `llvm-tools-preview`. The emulated boot's own targets are `emulation-boot`'s, which reads the same file: a rule about which cores the rig is started on belongs with the rest of that subject rather than here. |
| `size-probe` | The size probe stops being the `#![no_std]`, `#![no_main]`, feature-gated firmware the size gate links — or it stops mirroring a layer feature under a feature of its own, so the row named after that feature links code the probe can reach none of. A probe cannot `#[cfg]` on another crate's feature, so `--features waymaker-embassy/postcard` would report the delta of an image nobody exercised, and no other rule would notice: the row is not identical to its base, because the probe's own constants already differ. |
| `size-probe-reach` | A layer grows a public function the probe does not reach, so no budget charges for it. |
| `emulation-boot` | The emulated image stops being the thing the `emulate` stage started, in any of its five halves. The *attributes* half: `crates/waymaker-emu/src/main.rs` loses `#![no_std]` or `#![no_main]`, or declares its `unsafe_code` exception without a `reason` — an image that quietly became a host binary has no reset vector for a machine to start, and an unreasoned `allow` is the one thing the workspace manifest asks of the exception it permits. The *`unsafe`* half: any file of the crate writes the `unsafe` **keyword** outside `emulate::PERMITTED_UNSAFE_FUNCTIONS`, as opposed to naming the lint `unsafe_code` in the `allow`. This is the one crate in the workspace that carries `#![allow(unsafe_code)]`, and the whole of what it is carried for is two macro expansions — `#[cortex_m_rt::entry]`, which writes the exported symbol the reset vector points at, and `debug::exit`, which performs the semihosting call — and, since [ADR 0045](docs/adr/0045-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md), one measurement: `stack::paint` and `stack::high_water_mark`, confined to `emulate::STACK_MODULE`'s crate-relative path, plus the one `unsafe extern "C" { .. }` block the 2024 edition requires to name the linker's `_stack_end` symbol. A name is not the whole of the pin: the header must reach a `{` before a `;` or a bodiless signature would still borrow the exemption, every permitted `unsafe` must sit at that function's own nesting depth or a nested item or a closure could hide a second one beside it, that depth-zero `unsafe` must also be the *only* one the function declares or a second, sibling `unsafe` placed beside the legitimate one would pass unnoticed, and the one permitted extern block must declare `_stack_end` and `_stack_start` and nothing else or a foreign function whose `unsafe` is also followed by the word `extern` would pass as one. Without this half the exception would be a licence for a crate rather than for two expansions and one measurement, and the one place `unsafe` is permitted would be the one place nothing checks. The *prefix* half: the image no longer declares `emulate::PREFIX`. The harness reads the image's own lines to decide whether a boot was a measurement, so a space added on one side turns every later run into "the image printed no census" — which fails closed, and fails for a reason nobody would find quickly. The *manifest* half: the `[[bin]]` is not behind `required-features = ["emu"]`, without which every host build in the workspace tries to link a `#![no_main]` firmware binary. The *machines* half: a core in `emulate::MACHINES` has no Rust target pinned in `rust-toolchain.toml`, or no pipeline stage runs `cargo xtask emulate` at all — a machine the table claims and nothing starts. What it cannot see is whether the image *does* anything, which is the run's own job: `emulate::Census::shortfall`, `emulate::StackUsage::shortfall` and `emulate::Report::shortfall` read what the boot printed, and a scanner and a run answer different questions. [ADR 0040](docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md), [ADR 0045](docs/adr/0045-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md). |
| `gate-broken` | The gate's own expected values do not parse. A gate must not be able to silently uncheck one of its rules. |

### Documentation

| Rule | Fires when |
| --- | --- |
| `claude-md` | This file loses a must-not-own cell, a permitted dependency edge, a settled-decision id, a backticked gate rule id, a pipeline command, or its links to the decision record and the diagrams. |
| `recovery-spec` | The recovery specification and the four places it lives stop agreeing: a clause in `docs::SPEC_CLAUSES` is missing from this file, from [ADR 0015](docs/adr/0015-the-recovery-invariants-are-a-ghost-model-and-an-exhaustive-proof.md), or from `crates/waymaker-spec/src/obligation.rs`; its row here does not carry the guarantee's words or the test target that discharges it; the count is wrong; the crate declares a clause the table never did; or the clause table is not where the gate looks for it. Issue #20 asks that a change to the record representation update the model and the invariants first, then the proofs, then the code. Nothing mechanical can check the *order* — this checks that the four never disagree, which is the part that fails silently. |
| `storage-conformance` | Design document §12's storage contract and the four places it lives stop agreeing: a clause in `docs::STORAGE_CONTRACT_CLAUSES` is missing from this file, from [ADR 0016](docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md), or from `crates/waymaker-conformance/src/clause.rs`; its row here does not carry the sentence or what discharges it; the count is wrong; the crate discharges a clause differently than the table does; the crate declares a clause the table never did; or the clause table is not where the gate looks for it. Two tables agreeing on the names of six things and disagreeing about what any of them costs is the failure worth catching, so ids and discharges are compared in both directions. What it cannot see is inside the crate: that a clause the table calls in-process is reached by a case is `crates/waymaker-conformance/tests/clauses.rs`. |
| `storage-shapes` | Issue #130 item 2's shape catalogue and the four places it lives stop agreeing: a shape in `docs::STORAGE_SHAPES` is missing from this file, from [ADR 0047](docs/adr/0047-a-shape-catalogue-holds-the-suite-to-the-writers.md), or from `crates/waymaker-conformance/src/shape.rs`; its row here does not carry the sentence or the issuer; the count is wrong; the crate names a shape's issuer differently than the table does; the crate declares a shape the table never did; or the shape table is not where the gate looks for it. What it cannot see is inside the crate: that a declared shape is really issued by a run is `crates/waymaker-conformance/tests/shapes.rs::a_full_run_issues_every_declared_shape`. |
| `hardware-attestation` | Rung 0.2's board runs and the places they are recorded stop agreeing: a target in `docs::HARDWARE_TARGETS` has no backticked table row in this file, its row does not carry the headline or the status the table renders, the count is wrong, a target marked `Passed` has no accepted ADR carrying `docs::HARDWARE_ATTESTATION_MARKER` for it or has more than one, a target marked `Not run` is nevertheless claimed by an ADR, or an ADR attests a target the table never declared. What it cannot check is that a `Passed` row is *true* — the evidence is a log from a bench — only that the claim is a line in an accepted decision record rather than a status somebody flipped. |
| `failure-matrix` | Design document §14's failure-semantics table and the five places it lives stop agreeing: a row in `docs::FAILURE_ROWS` is missing from this file or from [ADR 0027](docs/adr/0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md), or its variant is answered with another id, or none, by the `fn id` body of `crates/waymaker-rig/src/matrix.rs` — pairs rather than a set, because two ids swapped between arms leave the set whole; it has no `#[test]` of its own name in `crates/waymaker-drive/tests/matrix.rs`, or that test's body never names its variant; a row the table calls swept has no `#[test]` of its rig name in `crates/waymaker-rig/tests/matrix.rs`, or that test's body never names its variant — the body rather than the file, because two tests with their names swapped keep every variant in the file; its row here does not carry the failure point, the test or the rig standing the table renders; the count is wrong; the rig answers a variant the table never declared; or one of the three files is not where the gate looks for it. A test under `#[ignore]`, `#[cfg(` or `#[cfg_attr(` is not a test — a conditional attribute is refused outright, because a row test is either a test or it is not (issue #97). What it cannot see is whether a named test asserts the row's *behaviour*: that is each file's own census, which pins the count per row on the model and requires the rig's to refuse at the first owed row. |
| `adr-numbering` | An ADR skips or reuses a number, is not named `NNNN-slug.md`, or the record has no template. |
| `adr-structure` | An ADR loses its title, `- Status:`, `- Date:`, `## Context`, `## Decision` or `## Consequences`, or carries an unrecognised status. |
| `adr-index` | An ADR is not linked from `docs/adr/README.md`, or the index links one that does not exist. |
| `settled-decisions` | The §02 ADR stops recording one of the eight decisions, or its headline. |
| `deferred-questions` | A question in `docs::DEFERRED_QUESTIONS` is missing from this file, its row does not carry the headline and the status the table renders, the count is wrong, a settled one's ADR is absent, unaccepted or does not carry its `Settles deferred question:` marker, two ADRs claim one question, an open one is already claimed by an ADR, or an ADR claims a question the table never declared. |
| `diagrams` | `docs/architecture.md` loses a labelled Mermaid block, a protocol step, a layer, or a permitted dependency edge — or draws an edge the layering does not permit, or labels two blocks with one id. |
| `missing-docs` | A crate root stops warning, denying or forbidding `missing_docs`, or turns it back off — `allow`, `expect`, the `warnings` group, a `cfg_attr` wrapper, or an attribute split over several lines are all the same regression. |
| `book` | Issue [#42](https://github.com/madmax983/waymaker/issues/42)'s book stops being the book it asks for. The *shape*: a row of `book::BOOK_CHAPTERS` has no file under `docs/book/src` or no link of its own title in `SUMMARY.md`, or a **file** appears under that directory that the table does not declare — every file rather than every `.md` file, because review of this change added a chapter named `rogue.MD`, linked it, and watched a case-sensitive collector leave it covered by nothing. The *samples*: a fence whose language `book::QUOTABLE_FENCE_LANGUAGES` does not name — the *unlabelled* one included — carries anything but `{{#include}}` directives; a line carries a directive and something else; a directive that is not `{{#include}}` appears at all; a line is indented four spaces; an include names a file `book::BOOK_SAMPLE_FILES` does not, an anchor that file does not declare, an anchor with no `#[test]` of its name, or a line *range*; an anchor is declared and no chapter shows it; or an anchor does not **contain** the `#[test] fn` of its own name and is not named in `book::BOOK_FIXTURE_ANCHORS` — a fixture anchor being one that shows a type a test uses, which must still declare an item rather than commentary. A `#[test]` under `#[ignore]`, `#[cfg(` or `#[cfg_attr(` is not a test, which is `failure-matrix`'s standard met here. Five of those are things review demonstrated rather than predicted, each watched passing on a mutation before it was closed: a bare ` ``` ` fence carrying Rust, past a version that asked whether the info string said "rust"; `{{#include a}} let x = 1; {{#include b}}`, which satisfies `starts_with` and `ends_with` and renders the source between them; `{{#playground}}`, which renders an arbitrary source file as a Rust block with no fence at all; `#[ignore]` written *above* the anchor marker, where a reader of the book never sees it and the test never runs; and an anchor shrunk to two comment lines advertising an API that does not exist, which a name-only tie accepted. The *contents*: the wire-format chapter must `{{#include}}` [`docs/format/wire-format-v1.md`](docs/format/wire-format-v1.md) and state no table of its own; the failure chapter must carry every row of `docs::FAILURE_ROWS`; the non-goals chapter every row of `book::NON_GOALS`; and `CLAUDE.md` and `README.md` must both link `docs/book/src/SUMMARY.md` — the path rather than the directory, because `docs/book (deleted; see the archive)` satisfied the looser check. What it cannot see is prose, and it cannot say the *rendered* book is whole: `cargo xtask book` is what does that, because mdBook exits zero for an include it cannot resolve and renders an anchor it cannot find as nothing at all. |
| `hardware-matrix` | The matrix stops covering every part, or starts claiming something. A board in `docs::HARDWARE_TARGETS` or a modelled part in `wear::PARTS` has no row in `book::HARDWARE_MATRIX`; a row names a board or a part neither table declares; the matrix chapter's table is not, cell for cell and row for row, `book::MATRIX_TABLE_HEADER` followed by every derived row in order; the chapter says `Passed` while no row renders it; or a modelled part's figure could not be measured, which is a failure rather than a blank column. Whole rows compared by equality rather than each cell searched for somewhere in the page, because review of this change fabricated a table of two boards that do not exist — both `Passed`, with invented geometry and invented wear — hid the honest rows in HTML comments, and watched a substring version print `ok`; it separately took a wear figure from `63.37` to `163.37`, which no `contains` can see, and duplicated a declared id with `Passed` in it. Every cell but the clock column is derived: the geometry from `wear::PARTS`, the power-cut standing from `HARDWARE_TARGETS` for a board and from `book::SWEPT_PROGRAM_BYTES` for a model — every crash sweep in this workspace lays the part out at a four-byte program unit, so the other two modelled rows say so rather than borrowing a sweep that never ran — and the written bytes per effect from the measurement this run took. So the book cannot say `Passed` where the record says `Not run`, and moving that record needs an accepted ADR, which is `hardware-attestation`'s. What it cannot see is whether a `Passed` row is *true*; that is a log from a bench, and [what the boards still owe](#what-the-boards-still-owe) is where its absence is recorded. |

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
- **What the parsed scanners cannot resolve.** Issue #51 moved the evadable half of the
  scanners — crate attributes, `extern crate`, `Future` implementors, associated-item
  uses, struct-literal construction counts, test declarations, module trees, ADR prose —
  from text patterns to real parsing (`syn` for Rust, `pulldown-cmark` for Markdown), so
  comments, strings, char literals, `use` aliases and `#[path]` modules no longer blind
  them. Parsing is not name resolution: glob imports are not followed, macros are not
  expanded, and a path inside a macro body is invisible. `cfg` is evaluated only enough to
  drop an alias that can never compile — `Cfg::requires_test` answers that for any formula,
  not only `test`, because a formula that is always false is, vacuously, "false whenever
  `test` is false" too (round 43 of Codex review on PR #143). That closes the literal case
  issue #185 named, `#[cfg(any())]`, but not two declarations of one name that are each live
  under a *different*, unevaluated flag: `own_aliases` still deterministically picks one —
  the first at module scope, the last at block scope — so whichever a real build compiles
  can lose to the other. `struct_literal_counts` closes that residual for its own callers
  with `path_could_reach_target`/`segments_could_reach_target` (issues #185 and #197): a
  construction is counted when *any* live alias sharing its name could reach it, not only
  the one declaration `resolve_local_alias_chain`/`resolve_segments_from` would pick — for a
  bare construction path or a qualified one, and chased into a module a multi-segment alias
  target names, mirroring `resolve_segments_from`'s own `own_modules` descent. What that
  search still does not do is intersect a candidate's own `cfg` with the construction
  *site's* own enclosing `cfg`: a candidate declared under `feature = "a"` is treated as a
  live branch even when the function containing the literal is itself only compiled under
  `not(feature = "a")`, so no build ever has both — a real, `rustc`-confirmed over-count
  (Codex review of PR #204) that needs a general `cfg`-vs-`cfg` satisfiability check beside
  `Cfg::requires_test`, enclosing-`cfg` accumulation in the visitor, and a wider
  `AliasLookupCache` key than `(scope, name)` to stay sound, none of which exists yet. See
  issue [#206](https://github.com/madmax983/waymaker/issues/206), filed rather than chased
  under review-driven time pressure for the same reason issues #171/#186/#193 were: it needs
  new machinery across several pieces, and getting one wrong in the exclude direction is a
  missed count, the opposite failure mode from the one it would fix.
  `resolved_path_uses` and `future_trait_implementors` still have the wider residual, because
  they need `resolve_segments`'s and `resolve_segments_from`'s one deterministic answer for
  reasons of their own — see the Status section's own paragraph on issue #185 for why
  widening those two was not taken up here. Alias resolution also stops at the file it
  reads: a chain of `use .. as ..` renames resolves within one module (issue #109), a
  nested module does not inherit an outer one's aliases,
  and `self::` and `super::` reach the scope each names explicitly rather than by
  inheritance — a stack of each module's own aliases from the file this scan read down makes
  both well-defined *within that file's own nesting*. `crate::` is not well-defined at all:
  this scan sees one file and never the crate, so it has no way to tell whether that file is
  really the crate root — treating its own top level as `crate`'s target was tried (Codex
  review, PR #160, round 5) and reverted (round 6), because it is right only for the one file
  that happens to be `lib.rs` and a guess everywhere else, catching an unrelated
  `impl crate::X for Y` as a false fifth future in one file and staying silent on a real one
  reached through a named submodule (`crate::traits::X`) in another — the same shape of
  over- vs under-matching namespace ambiguity settles below, decided the same way. `super::`
  has the identical edge once it is asked to step *above* the file's own top level — a
  `super::X` written with no enclosing `mod {}` inside the scanned file names the module that
  declared that file as `mod child;`, which this scan equally never sees, and the stack's own
  floor at index 0 had silently stood in for it (round 7) exactly the way index 0 had stood
  in for the crate root; it is left unresolved the same way `crate::` is, rather than guessed
  against the file's own aliases. A *plain relative* path naming a sibling module declared in
  this same file — `traits::Pollable`, where `mod traits { pub use .. as Pollable; }` sits in
  the same scope, with no `crate`/`super`/`self` prefix at all — is resolved (issue
  [#169](https://github.com/madmax983/waymaker/issues/169)): `resolve_segments` steps into a
  sibling `mod` block by name when no alias matches, and keeps resolving there, chained
  through as many levels of nested sibling module as the path names. Three narrower limits are
  left where descent cannot go. An out-of-line declaration (`mod traits;`, no body in this
  file) and a module gated on exactly `#[cfg(test)]` are both left unresolved rather than
  guessed at. The first is because the module's real content lives in a file this per-file
  scan never reads. The second is for `own_aliases`'s own reason (issue #51: test code is not
  shipped code). Once resolution has stepped into a module by name it is off the lexical
  ancestor stack, so `self::` still resolves inside it but `super::` does not. That is the
  same residual-limit shape as `crate::` and a top-level `super::` above, and the same reason:
  a module reached by name has no ancestor this per-file scan can identify past the point it
  was entered from. An alias declared in one module
  and reached through a `use` in another *file* is invisible outright, the same limit
  `capacity-reserve`, `recovery-surface` and `storage-contract` each record for the one file
  they pin. Module descent (issue #169) inherited this scanner's oldest limit rather than
  adding a new one: `resolve_segments` tracked no function-body scope at all, so a generic
  parameter or a block-local item named the same as a `use` alias was already able to shadow
  it unsoundly before #169 existed, and a sibling `mod` block reached the same way could be
  shadowed the same way (issue
  [#181](https://github.com/madmax983/waymaker/issues/181), Codex review, PR #176). The
  generic-parameter half is closed: `resolve_segments`/`resolve_segments_from` take a shadow
  set of the type-parameter names a `fn`, `impl` or `trait` item declares, and a bare head
  segment that set names is left unresolved rather than substituted through a same-named
  module or alias — every visitor built on `resolve_segments` (`resolved_path_uses`,
  `name_uses`, `struct_literal_counts`) carries the same five overrides, factored into one
  `shadow_generic_params!` macro so the five stay one definition rather than three copies. A
  nested `fn`, `impl` or `trait` resets the set to only its own generics rather than adding to
  the enclosing one, and a nested `mod` resets it to empty, both restored on the way back out
  — rustc's own words for why: "nested items are independent from their parent item ... for
  name resolution" (`E0401`). Two rounds of Codex review, on this pull request itself, found
  the first version wrong in exactly the direction this residual-limit bullet exists to
  catch: it only ever added a shadowing item's own names and never reset, so a nested item one
  level inside the shadowing one inherited a shadow real Rust never gives it, hiding a real
  alias the pre-#181 code had resolved correctly. A block-local item sharing a name with a
  sibling module or alias stays open, filed as issue
  [#193](https://github.com/madmax983/waymaker/issues/193): closing it needs a block's own
  item declarations to become a scope of their own too, ahead of every module-level lookup —
  more machinery than the generic-parameter half needed, the same standing #169 itself had on
  PR #160 before it was filed rather than chased. A `struct`, `enum`, `union` or `type` alias
  can declare its own generic type parameter too, and none of the five overrides tracks one:
  a residual narrower than #193's, left stated rather than closed, since none of the pinned
  rules this scanner backs constructs a struct literal or a suffix path from inside one of
  those declarations today. Nor does
  it carry a namespace: two `use` items can bind one local name in different namespaces — a
  function and a trait can both spell `Pollable` — and a syntactic scan cannot tell which one
  a later occurrence meant. Picking the first-declared alias can silently miss a real match;
  exploring every alias that name could mean can just as easily attribute an unrelated,
  legitimate construct to a different one (Codex review, PR #160, rounds 3 and 4 — the second
  finding is what took the first back out). Guessing a direction was tried and rejected in
  both directions, matching this repository's own rule about the storage-contract suite:
  guessing is how a broken input talks a check out of testing it. A same-spelled alias across
  namespaces is a residual limit rather than a guess. Markdown parsing does not check
  that a rendered claim is true, only that it is rendered prose rather than a code fence.
  The scanners that stayed textual are the ones whose rule is
  about *spelling* — a forbidden vocabulary item, a handwritten `unsafe` keyword — and they
  read comment- and string-stripped text, because there a mention in prose is a false
  positive, not an evasion.
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
and CRC-16/CCITT-FALSE, table-free at the time, decided on measurements taken on
`thumbv6m-none-eabi` rather than on preference (issue #153 and, far below,
[ADR 0046](docs/adr/0046-crc16-folds-its-nibble-round-to-a-multiply-crc32-stays-bitwise.md),
which folds `crc16`'s nibble round to a multiply and declines a table for `crc32`, and
[ADR 0053](docs/adr/0053-a-crc32-nibble-table-still-beats-the-branchless-loop.md), which
supersedes that `crc32` clause once a profile of this workspace's own workloads showed the
table still winning against ADR 0046's own branchless loop) — and the metadata a scheduled
effect carries is
[ADR 0011](docs/adr/0011-a-scheduled-effect-records-a-length-and-a-digest.md), which fixes it
at a sequence, a kind, a length and a digest. Each answer has a rule holding it: a checksum
that changed polynomial fails `integrity-check`, and so does one that grew a table outside
the one exception ADR 0053 later carves out, and a fifth field on `EffectScheduled` fails
`effect-scheduled-fields`.
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
row 5 says a torn completion is redelivered, and at the time neither this bank nor this rig
could — recovery had no way to tell an interrupted append from damage, so both refused the
bank, and the table's own continuation was `continue_as_new`, a new run that forfeits the
effect's identity. [ADR 0027](docs/adr/0027-the-failure-matrix-is-ten-named-tests-and-a-rig-that-resumes.md)
recorded the deviation; issue #95 and
[ADR 0052](docs/adr/0052-a-torn-record-redelivers-when-its-reserved-slot-is-clean.md)
close most of it, below.

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
`crates/waymaker-facade-demo/tests/ota.rs` (moved there by issue #106, below) runs §06's OTA
example — three activities and a completion, with the image crossing every boundary as an
eight-byte handle — through the real façade, the real driver and `waymaker-fault`'s NOR
model: it completes, it dispatches nothing on replay, a reboot mid-run redelivers the
identity the schedule record committed, and the synchronous `Activities` world is asked zero
times. The second is structural rather than behavioural: `Boundary`, `Driver` and §07's
typestate name no `waymaker-embassy` type, so the façade edge is two modules — `facade.rs`
and `ota.rs` — plus their four lines in `lib.rs` and the manifest entry. `waymaker-drive`'s
`without-facade` feature deleted the two modules and the `drive-facadeless` stage built that
configuration for the part, which was as much of the claim as a compile could make while the
manifest entry stood; `ctx-facade` is the scan beside it, and it reads every module of the
crate rather than a list, so a module added tomorrow is covered. Issue
[#106](https://github.com/madmax983/waymaker/issues/106), below, is the rest of it — moving
the edge above the crate instead, so the manifest never stands.
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

Issue #107 closes a gap Codex found in round 5 of #105: `TerminalFuture` and `ContinueFuture`
each kept their own "already recorded" flag, so a caller that polled one, dropped it, and
polled another could re-decide or un-decide the run. The flag now lives in the `Ctx`, shared
by all four futures. `crates/waymaker-embassy/tests/ctx.rs` drives both scenarios directly,
since no `async fn` can reach either path.

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


`cargo xtask emulate` then closes a limit this file had recorded about its own firmware
stages. `crate-attributes` exists because "`cargo build --lib` produces an rlib and never
links, so no global allocator is required and an `extern crate alloc` under any of them
compiles clean" — and that sentence is larger than the allocation half it is quoted for.
Nothing placed a reset vector, nothing resolved a `#[panic_handler]`, nothing linked
`compiler_builtins`, and no Waymaker instruction had ever been retired outside an x86 test
binary. `waymaker-emu` is a linked image, started on a Cortex-M0 and a Cortex-M4 — **ARMv6-M**
and **ARMv7E-M**, the two architectures this workspace builds for — and required to lay a part
out, run twelve iterations into a cut, resume each and judge every one. Three things hold it,
and [the section above](#the-emulated-boot-and-what-it-is-not) has them: the two cores'
censuses must be *equal*, because the plan is deterministic and a difference is the rig
behaving differently on two instruction sets; the RAM-backed NOR model is put through §12's
conformance suite in the same boot, which is the first time in this workspace that
`waymaker-conformance`'s `#![no_std]`-so-a-driver-author-can-run-it-on-the-part claim has been
taken up; and the *census* is the gate rather than the exit code, because an image whose
`main` returned early exits exactly the way a complete one does. Both cores measure the same
thing — 21 conformance cases passed and 2 exempt, 12 iterations, 12 cuts, 12 resumes, 8
redeliveries, 24 verdicts and 42 effects — which is the result worth recording, because the
two agreeing is the check a single-core stage could not make. `emulation-boot` is what stops
the image drifting, and its sharpest half is the `unsafe` one: this is the workspace's only
`#![allow(unsafe_code)]`, the whole of what it is carried for is two macro expansions, and the
rule fails a build over the *keyword* appearing anywhere in the crate. What is *not*
discharged is the thing a reader will reach for first, and it is a row rather than a claim:
all three of [the hardware table](#what-the-boards-still-owe)'s rows stay `Not run`, because
neither machine has a NOR part, a supply to remove, a reset-cause register or a backup domain,
and a Cortex-M0 is not a Cortex-M0+. ADR 0040 carries no attestation marker, so
`hardware-attestation` fails a build in which somebody moves a row and cites it. See
[ADR 0040](docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md).

Issue #51 then answers a question ADR 0005 had left open: the documentation rules this file's
own gate enforces were hand-rolled text scanners, and four review rounds on the pull request
that added them (#11/#50) found the same defect fourteen times — a scanner accepting syntax
`rustc` or a Markdown renderer would not. Five more were filed rather than patched, because the
fifth round would have found a sixth spelling: a `#![warn(missing_docs)]` on a nested module
satisfying the crate-root rule; a `reason = "/*"` string truncating the attribute that held it;
ADR metadata readable only inside a fenced example; a `~~~` fence the scanner did not recognise
as a fence; and a `- Date:` with no value passing a bare presence check. `xtask/src/parse.rs`
is the answer: `syn::parse_file` reads Rust, `pulldown-cmark` reads Markdown, and
`xtask/src/source.rs` and `xtask/src/docs.rs` are rewritten to ask the parsed tree the question
each rule needs answered rather than to scan lines for it. All five filed evasions are now
named tests — `an_attribute_on_a_nested_module_does_not_satisfy_the_crate_rule`,
`a_reason_string_containing_a_comment_opener_does_not_hide_an_allow`,
`adr_structure_ignores_fenced_code`, `a_marker_inside_a_tilde_fenced_example_does_not_settle_anything`,
`an_empty_adr_date_is_reported` — and so are nine further evasions the same rewrite closed
along the way, filed as their own issues (#59, #62, #68, #82, #90, #97, #99, #108, #109) rather
than folded in silently. `parse.rs`'s own module documentation states what parsing still cannot
answer: no name resolution across crates, no macro expansion, no `cfg` evaluation, no glob
imports followed, and a Markdown check that reads structure rather than judging whether a
claim is true.
That did not close the class outright, and a later adversarial pass over the same two files
found why: `used_call`, the check behind `integrity-check`'s call routing, resolves a pinned
function's body with `syn` and then renders it back to text for a boundary-match scan — and a
literal token's rendered text reproduces its exact source spelling, string contents included.
A digest function whose body never touched the real checksum, decorated with a string merely
*mentioning* it — `let _spoof = "route via crc32(input).into() for humans";` — satisfied the
pin, because to the scan `crc32(input).into()` inside the quotes reads the same as a real call,
though to `rustc` a string's contents are data and never a call. Issue #51's own pattern,
reintroduced one level up by the fix that closed it elsewhere: a text scan built on a real
parse is still a text scan wherever it renders the parse back to text before matching against
it. Filed as issue #158 and closed in the same change: `block_text` now blanks every string
and byte-string literal before rendering, through a token-level pass beside `unraw_tokens`'
own, so a mention inside a string can no longer spell a callee's name. The regression is
`a_callee_name_inside_a_string_literal_is_not_a_call`, isolated to `used_call` the way issue
#62's own decoy reproduction was, for the same reason: `check_integrity_routing` end to end
cannot isolate a single routing function from the others it also checks.

Issue #73 then closes the refinement half of `single-authority`'s gap. Issue #22 added the
real two-bank adapter; nothing had abstracted it into the ghost model yet, so a state rebuilt
from a real crashed run had no banks and answered the guarantee vacuously.
`waymaker-spec`'s `refine` module gains two functions, `bank_after_erase` and
`bank_after_seal`, each folding one crash into a `Bank` from what the bank's own region shows
afterwards — not from whether the writer's call returned `Ok`, because `waymaker-fault`'s
writes land synchronously and a watchdog reset can finish a unit in flight and still answer
`Err`. `tests/refinement.rs` drives a swap writer styled on
`crates/waymaker-fault/tests/banks.rs`'s own, and checks every crash point three ways: is the
reconstructed state one the model's search reaches, does the guarantee hold of it, and does
`waymaker_flash::bank::select` over the real bytes agree with it. `obligation.rs`'s row for
`single-authority` no longer says the refinement is missing. What is left owed is the model's
own expressiveness, unchanged by this issue: a bank holds no record, so "never recover the
old run as current" is not a statement the machine can make, only "exactly one bank is
bootable"; and a generation is an unbounded integer, where the firmware refuses at the
ceiling rather than proving the refusal unnecessary. See
[ADR 0041](docs/adr/0041-the-bank-refinement-abstracts-a-real-swap.md).

Issue #47 then closes a question ADR 0002 had deferred rather than answered: `cargo xtask
size` gates *engine statics*, not runtime RAM, because most of §04's own accounting —
cursor, context, record header — lives on the stack, and a deeper call chain moves no
writable section the size gate can see. ADR 0035 composed the figure further and said the
same about what was left: three of its four terms are stack-resident, and the depth of the
chain holding them was still unaccounted. ADR 0040 built the one thing that could account for
it — a linked image, run on real cores — and declined to, naming a second `unsafe` expansion
and a memory-map symbol as the cost. This closes it: `waymaker_emu::stack` paints the image's
own unused stack before the rig runs and reads back how far the paint was disturbed, over the
region between the linker's `_stack_end` and a stack-pointer reading `main` takes — through
`cortex_m::register::msp::read()`, a plain register read, before calling anything else.
`measured_run` — `#[inline(never)]` — holds every local the run touches in a frame of its own,
so the paint can never reach memory `main` still needs. The figure is a *lower bound* rather
than an exact reading — a frame can reserve bytes it never writes, and painting cannot see
that — and the module says so rather than the sharper claim an earlier draft made; a guard
margin near the reading still guarantees a floor, never a ceiling. The figure is real and it
fails closed on a degenerate measurement, but it is not gated against a §04 ceiling — none
exists for it, the same standing as the write-amplification and `no_alloc` instruction
figures — and it is not §04's own number regardless: this image links `waymaker-rig` and
`waymaker-conformance` alongside the three layers, so what is reported is the whole call
chain's depth on this run, not the engine's share of it, and the two machines are not required
to agree about it the way their census is — different cores compile the same source into
different instructions. `emulation-boot`'s `unsafe`-keyword rule grows its third and last
named exception, and a name alone was not enough to close it: a bodiless signature, a nested
item, a sibling `unsafe` placed beside the legitimate one at the same depth, and a foreign
function all passed an earlier version of the rule and are refused by this one, each behind a
test that was watched failing first. Every other file of the crate is held to the rule
exactly as before. `paint`, `high_water_mark` and `available_bytes` are safe `pub fn`s over a
caller-supplied `depth_from`, and a safe function has to stay sound for any argument — so a
second linker symbol, `_stack_start`, names the stack's other end and
`stack::clamp_to_stack_region` holds `depth_from` inside `[_stack_end, _stack_start]` before
either function computes a pointer from it. Being inside that range is not being below the
*live* stack pointer, though — a stale address that is still a legal stack address, or
`usize::MAX` clamped down to `_stack_start`, both pass that check — so the clamp also takes
the lower of the region-clamped value and a fresh stack-pointer reading of its own, taken at
the moment either function is called; a stale or wrong reading is now a wrong measurement,
never an out-of-bounds access, and a caller's `depth_from` can only narrow what gets touched,
never widen it past where the stack genuinely is. Three independent live readings taken at
three different points in the boot can still disagree with each other by those same few
bytes, and `available_bytes` — called earliest, before `paint`, with the least stack consumed
since `main`'s own reading — tends to disagree in the wrong direction: a run that disturbed
every painted byte could report `used` a few bytes short of `available`, which is exactly the
case `StackUsage::shortfall`'s `used >= available` check exists to catch. `paint` now returns
the bound it resolved, and `main` passes that same value to `high_water_mark` and to a second,
later `available_bytes` call rather than letting either re-derive its own reading. A region no
wider than `GUARD_BYTES` is one `paint` writes nothing into at all, and `high_water_mark`
honestly reports `used = 0` over it — but `available_bytes` does not know about `GUARD_BYTES`
and reports the region's full width regardless, so a resolved bound 50 bytes wide read as
`used=0 available=50`, which the shortfall check's original `available == 0` line did not
catch even though nothing was measured. `emulate::StackUsage::shortfall` now refuses any
`available` no wider than a duplicated `STACK_GUARD_BYTES`, held to the real constant by a
test that reads the literal back out of the shipped file. Passing the same `resolved` value to
two separate calls still let them disagree, because each still clamped it against its own
fresh stack-pointer reading at its own call site — `stack::high_water_mark` now returns both
`used` and `available` together, computed from the one `depth_from` it resolves for itself in
that single call, so there is no second call left to read a different bound. A bounded address
is not the same thing as an initialized one, though, and that gap was still open: `paint`'s own
doc comment asked a caller to pass its return value on to `high_water_mark`, but nothing
stopped a safe caller reaching `high_water_mark` directly with an arbitrary `usize` and no
`paint` call at all — and a raw read of stack memory nobody painted is undefined behavior
however tightly the address is clamped. `paint` now returns `Painted`, a type this module is
the only one able to construct, and `high_water_mark` takes one instead of a bare `usize`, so
a caller with no `Painted` in hand cannot call it at all. Two more findings came from a
review of the merge that followed. `current_stack_pointer` read the core's MSP register
unconditionally, which is correct only because this image never selects the other Thread-mode
candidate, PSP — a fact about how the crate happens to be used today rather than one its
`pub` signature states; it now reads `CONTROL.SPSEL` first and follows it to whichever
register is actually live. And `emulation-boot`'s extern-block exemption was a byte *range*
rather than the one keyword's own offset the two permitted functions are each held to, so a
second, unrelated `unsafe` sitting anywhere between the linker-symbol block's braces passed
unnoticed; it now matches only that one offset, the same way `sole_depth_zero_unsafe` already
does for `paint` and `high_water_mark`. A third finding on that same commit is that the
`CONTROL.SPSEL` check answered a narrower question than the one it needed to: `SPSEL` only
governs which register *Thread mode* uses, and Handler mode — running an exception — always
executes on MSP regardless of it, so a caller reached from a handler after Thread mode had
selected PSP would still have read the inactive register. `current_stack_pointer` now checks
`SCB::vect_active()` first — a safe function, reading a read-only status register with no
side effects — and answers MSP outright in Handler mode, consulting `SPSEL` only in Thread
mode. A fourth finding is that answering "which register is active" correctly is not the same
question as "is it safe to paint below it": MSP genuinely is active in Handler mode, but an
exception can interrupt Thread mode while Thread mode was using PSP for a *second*, still-live
stack this module's one `_stack_end`/`_stack_start` pair has no way to represent — so treating
MSP as though everything below it were free is wrong in exactly the case a second stack
exists, whichever register correctly answered "active". `clamp_to_stack_region` now checks
`SCB::vect_active()` itself and collapses to the empty region at `stack_floor()` outside
Thread mode, reusing the same degenerate case a region no wider than `GUARD_BYTES` already
produces — which `paint` already declines to write into and `StackUsage::shortfall` already
refuses as a measurement that did not happen, so no new mechanism was needed to close it. A
fifth finding is that checking processor *mode* answered only half of "is MSP the register in
use": Handler mode always executes on MSP, but Thread mode can select PSP too, and nothing
about being in Thread mode confines a PSP reading to `[_stack_end, _stack_start]` — a PSP
value above `_stack_start` makes `clamp_to_stack_region`'s own `.min` a no-op, silently
dropping the live-pointer protection back to the region clamp alone. `clamp_to_stack_region`
now asks the real question directly — `msp_is_the_stack_in_use`, true unconditionally in
Handler mode and by `SPSEL` in Thread mode — and collapses to the same empty region whenever
MSP is not it. That fifth fix's own framing was the sixth finding's bug: answering "is MSP the
register in use" `true` for Handler mode is a true fact about which register is active, and not
the fact the fourth finding needed — MSP being active in Handler mode does not make the memory
below it safe, because an exception can land there having interrupted a Thread-mode context
that was using PSP for a second, still-live stack. Treating "which register" as "safe" quietly
undid the fourth finding's unconditional Handler-mode refusal. The two conditions are
conjunctive, not a choice of which to ask: `clamp_to_stack_region` now trusts the live reading
in exactly one state, Thread mode with `SPSEL` naming MSP, and collapses in every other one —
Handler mode included, unconditionally, regardless of what register answers there. This image
runs in Thread mode with MSP selected for the whole of every boot this ADR measures, so the
branch is dead code here too; it exists for the caller this crate does not have yet. See
[ADR 0045](docs/adr/0045-the-emulator-paints-the-stack-and-reports-a-high-water-mark.md).

The kernel-state registry has three entries — the replay machine, the record view and an
armed timer — so the 128 B budget is a number about something, and 104 B of it is spent. The
async `Ctx`, the dispatcher, the codec helpers, the two examples and rung 0.4's exit
criterion are here — issues #35, #36, #37, #38 and #39, above — and in-boot sleep is the
rest of 0.4. The gates went in before the code they govern, which is the point: a gate
retrofitted after coverage has slipped is a gate that ratifies the slip.

Issue #67 closes the three dimensions issue #20's review found `waymaker-spec`'s ghost model
short of, none of which raising `Bound::PROOF` would have reached: the shapes of history the
model admitted were one-dimensional, and the bound was never what they were short of.
`model::Record` now carries a `BankId`; `Journal::begin_erase` drops exactly the erased
bank's records and dispatch-log entries, and `recover`, `committed`, `declared` and
`acknowledged` are all scoped to the bank a reader would boot from — so "never recover the
old run as current" is a fact `Journal::recover` states rather than a sentence the model had
no way to say. `Invariant::SingleAuthority` takes the recovered history now, not only
`state.banks`, and `tests/teeth.rs`'s `Mutant::BootsTheRetiredBank` shows the guarantee
falsifiable by a reader instead of only by deleting a guard — issue #67's own "done when",
met by name. Record identity moved off `records.len()` onto a counter that only grows, which
is what let `begin_erase` drop a record from the middle of history without a later `declare`
reissuing its id to something else; `Transition::Reboot` is the transition that identity
scheme exists for, legal only while unpowered, restoring power and changing nothing else —
`recover()` already computes the survived prefix fresh from the same bytes, and an earlier
version that pruned `records` toward that answer was a real bug review caught: the prune was
scoped to one bank and silently erased the *other* bank's own history too, which only
`begin_erase` may do, and let a live write land back in a bank a crash had left with no legal
append point. Compaction — the obvious firmware response to
`Interruption::Failure`, retrying elsewhere rather than being stuck behind a torn record —
turned out to need no transition of its own: `begin_seal` and `begin_erase` never consulted
the other bank's records, so a live device behind a torn tail could already seal the blank
bank and carry on there, and `tests/compaction.rs` is the proof that this reachable state is
reached with no power loss anywhere in the run. A necessity proof found
`Guard::DispatchFromCurrentBank` — added on the reasoning that a real swap consumes the old
run's writer — unnecessary once `durable_intent` correctly treats a dispatch from a retired
bank as moot the same way `continue_as_new` already forfeits identity across one (issue #95);
it was removed rather than kept, per this crate's own rule that a guard removable at no cost
was never load-bearing. Generation arithmetic now refuses at `u32::MAX`
(`checked_add`, not `saturating_add`) instead of silently repeating it — a hand-built state
in `model.rs`'s own `#[cfg(test)]` module, since no explorable bound reaches the ceiling.
And a direct "no gap before committed history" check in `waymaker_fault::verify_oracle` was
tried and reverted: `waymaker-fault`'s own `tests/harness.rs` deliberately drives a writer
whose failed middle record is skipped over, which the oracle's committed-history filter is
what makes acceptable, so the circularity issue #67 named is closed by a test that checks the
claim directly against the `Ledger` the agreement tests build
(`tests/oracle.rs`'s `the_ledger_the_oracle_judges_never_has_a_gap_before_committed_history`)
rather than by a stricter oracle. Issue #73 had already closed the refinement half of this
guarantee's gap — the paragraph above — and this issue closes the other half, the model's
own expressiveness, so `obligation.rs`'s `single-authority` row now says nothing is owed. See
[ADR 0043](docs/adr/0043-the-model-gains-banked-records-a-reboot-and-a-live-compaction.md).

Review of the pull request that closed issue #67 then found a fourth gap `Guard::
NeverEraseTheAuthority` left open: `authoritative()` is always empty before the first seal,
so the guard protected nothing pre-seal — `BeginErase(A)` was legal on a fresh device even
though `A` is where `declare` puts every record, letting a live `Declare`-`Program` land
inside a bank already `Erasing` and survive `CommitErase`, which never touches `records`.
`Journal::protects_current_run(bank)` closes it: post-seal it is `authoritative().contains`
exactly as before, and pre-seal it is `bank == current_bank()`, the implicit bank a fresh
device writes into. `tests/machine.rs`'s
`erasing_the_pre_seal_current_bank_is_refused_the_same_as_erasing_the_authority` is the
proof, over every pre-seal reachable state; `REACHABLE_STATES` and `TRANSITION_EDGES` in
`tests/census.rs` moved again, down rather than up, because a whole family of states in
which a fresh device erased its only writable bank stopped being reachable.

A fifth finding asked for a guard refusing `Dispatch` against a bank a swap has since
retired, arguing a dispatch happening *after* retirement is a physical effect with no run
behind it rather than merely an old run's forfeited one. Investigated directly: restricting
`Dispatch` to `current_bank()` moves `TRANSITION_EDGES` and leaves `REACHABLE_STATES`
unchanged, because every state a post-retirement dispatch could reach is also reachable by
dispatching while the bank is still current and retiring it afterward — a `Journal` is a
snapshot rather than a log, so the two are one state, not two. `tests/necessity.rs`'s
`a_dispatch_from_a_bank_a_swap_later_retires_can_happen_before_the_swap_ever_starts`
constructs that legitimate trace by hand; no guard was added, and ADR 0043's alternatives
section says why.

A sixth found a real bug: `Journal::from_parts` (which `Journal::reconstructed` uses to turn
a crash harness's observation into a state) takes `next_id` from the observation's own record
ids via `saturating_add`, so an observation naming `RecordId(u32::MAX)` left `next_id` at the
ceiling, and `declare`'s plain `+= 1` then overflowed on the next declaration — a panic with
overflow checks, a reused `RecordId(0)` without them. `next_id` now advances with
`checked_add`, refusing with `Illegal::CapacityReached` instead;
`a_record_id_at_the_ceiling_is_refused_rather_than_reused` is a hand-built unit test beside
the existing generation-ceiling one, since no exhaustive search reaches either ceiling.

A seventh found that `tests/refinement.rs`'s own `REFINEMENT` bound — one generation, because
its own comment says no writer there touches a bank — was reachable in states split across
both banks anyway, since nothing stops the model's first-ever seal landing on bank B while
bank A already holds records. `Observation` carries no bank identity, so
`reachable_observations` flattening one of those states in declaration order could produce a
`Whole` record following a gap, a shape no single-bank writer this file drives could ever
leave, and the refinement check's first assertion ("is this a state the model says is
reachable") would have accepted it. `reachable_observations` now filters to states where
every record is in `BankId::A` before projecting;
`no_reachable_observation_is_a_shape_no_single_bank_writer_could_leave` asserts the filtered
set is clean, verified to fail without the filter (488 of the run's reachable states leaked
an impossible shape) and pass with it.

An eighth finding was the sharpest of the round: `Declare -> Program -> Barrier` in bank A
(pre-seal, current), then `BeginSeal(B) -> CommitSeal(B)` as the device's very first seal,
left A's `Bank` tag at `Erased` — it was never touched — while A still held the record it
declared before B took over. `Journal::begin_seal` read only that tag, so `BeginSeal(A)`
afterward saw nothing wrong and resealed A's stale record at a higher generation than B, with
no erase anywhere in the trace — recovering a superseded run as current, exactly what §14's
failure table forbids. `begin_seal` now also refuses a bank that still holds records unless
that bank is the one currently being written to (`current_bank()`), which is the ordinary
shape of a device's very first seal and the only case a bank may carry records into its own
seal. `tests/machine.rs`'s `a_bank_the_first_seal_retires_cannot_be_resealed_without_an_erase`
drives the trace end to end; `REACHABLE_STATES` and `TRANSITION_EDGES` moved down a third
time, and `tests/compaction.rs`'s surviving-bank test needed its witness search corrected to
look for a bank with no records rather than an `Erased` tag alone — the tag-only search had
been finding this same bug's witness and calling it a demonstration.

A ninth, in the same round, was in the previous round's own fix: `next_id`'s `checked_add`
refused *before* advancing, so a `Journal` reconstructed with `RecordId(u32::MAX)` already
the highest id in it refused `RecordId(u32::MAX)` itself rather than only the id after it —
the last representable id was never allocatable, not just the one past the ceiling.
`next_id: Option<u32>` fixes it: `None` means no id is left, `Some(u32::MAX)` still hands out
the last one and then becomes `None`. `the_last_record_id_is_still_allocated_exactly_once`
and `declaring_past_the_last_record_id_is_refused_rather_than_reused` are the positive and
negative halves.

This branch's own merge with issue #73's landed on `main`, reconciling the two independent
halves of `single-authority`'s "owed" gap each issue closed, drew a tenth and an eleventh
finding on the merge commit. The tenth: `Journal::reboot` restored power and left *every*
record in place, `OnMedia::Absent` ones included — a record `declare` puts in `records`
before a single byte is programmed, so it is a fact about RAM rather than about media, and a
power cut takes RAM with the power. `Declare(Schedule)` immediately followed by
`PowerLoss`/`Reboot` left the phantom declaration in place, and `unresolved_schedule_in` and
`whole_before` read it exactly as they would a real one — permanently stranding that bank,
refusing a second `Declare` as `OutOfProtocolOrder` and refusing every later `Program` behind
the absent record's own `whole_before` check, with no real bytes anywhere to blame it on.
`reboot` now discards every still-`Absent` record and prunes `dispatched` to match, the same
way `begin_erase` already does for an erased bank's records; `next_id` does not roll back,
matching the "total ever declared" accounting `declare`'s own doc comment already states for
an erased bank. `a_reboot_discards_only_records_still_absent_from_media` is the positive
claim and `a_declared_record_is_never_renumbered_or_removed_except_by_erasing_its_bank`
gained `Reboot` as a second, narrower exception beside `BeginErase`'s. `REACHABLE_STATES` and
`TRANSITION_EDGES` moved up substantially this time — a whole family of states that used to
dead-end at `Reboot` behind an undischarged declaration reopened into the states a bank that
had never declared anything reaches.

The eleventh was independent of the tenth, on the observation/reconstruction path
`tests/refinement.rs` abstracts a real crashed device through: `Journal::from_parts` inferred
`next_id` from `records.iter().map(id).max() + 1`, which is exact only while nothing has ever
been dropped from `records`. A device that declares record 0, swaps authority away from its
bank and erases it has a real counter at 1 with no record anywhere naming 0; inferring from
what survives computed 0 and handed that id out a second time on the very next `Declare` —
the identity collision issue #67's whole scheme exists to forbid, unreachable by any test
here only because the bank-swap refinement declares no records at all. `Observation` gained
its own `next_id: Option<u32>` field, reported by the caller rather than inferred — exactly
the shape `banks` and `sealed_once` already take, for the same reason: `Journal::observation`
reads the real field directly, `refine::abstraction`'s record-only callers compute it from
the ledger's own records because none of them ever erases, and `Journal::from_parts` takes it
as a parameter instead of computing it at all.
`reconstruction_never_reissues_an_id_an_erase_already_spent` is the regression, built directly
against a hand-supplied `Observation` rather than a full crash-harness run, since the gap is
in the reconstruction machinery itself rather than in any particular writer's behaviour.

A twelfth finding, on the same round's next pass, was in the tenth's own fix: dropping a
still-`Absent` record on reboot without rolling `next_id` back too meant a device that
crashes before its first-ever media write, over and over, eventually reads
`Illegal::CapacityReached` against media that has never held a single byte —
`Declare`/`PowerLoss`/`Reboot` repeated `bound.records` times, with nothing else ever
happening, strands a device the real firmware would never strand. Codex's evidence was the
real firmware itself: `waymaker_core::id::EffectIdAllocator::resume` derives the next
sequence from the *highest committed* one, so an attempt that never durably landed is never
counted against a run at all. `reboot` now rolls `next_id` back by exactly the count of
`Absent` records it discards — but never past `1 +` the highest surviving id, because an id
`begin_erase` retired earlier in the *same* run is not in `records` to protect itself, and
rolling all the way down to what the current residents alone justify would let a later,
unrelated `Declare` reuse an id an erased bank had already spent — the collision issue #67's
whole scheme exists to forbid, reached through the combination of the two transitions rather
than through either alone. `a_crash_before_the_first_media_write_never_spends_capacity` is
the positive claim, driven ten cycles deep against `Bound::PROOF`'s three-record ceiling
(which stranded a device after exactly three, before the fix); the sharper edge is
`an_id_an_erase_already_spent_survives_a_later_reboots_own_rollback`, which declares and
commits two records in one bank, retires it behind a seal on the other, erases it, and
requires a crash-and-reboot of a fresh, still-`Absent` declaration in the new bank to skip
both of the first bank's spent ids rather than reusing either — verified against the naive
fix (recomputing purely from surviving residents, with no floor) before landing this one,
since that naive version reaches exactly the collision this test exists to catch.
`REACHABLE_STATES` and `TRANSITION_EDGES` moved again, down substantially this time (8,360 →
5,620): every distinct crash count a device could accumulate before its first media write
used to be a distinct state, purely because `next_id` climbed higher with each cycle even
though nothing on media ever changed, and those cycles now collapse back onto the states a
device that crashed once, or never, already reaches.

Review of that round's fix drew two more findings, both on the observation/reconstruction
path rather than on `reboot` itself. The first was documentation left behind by the second
finding's own history: this ADR still said a reboot "changes nothing but the power" and
still named `a_reboot_changes_nothing_but_the_power` as the proof, both stale since the tenth
finding taught `reboot` to discard `Absent` records — [ADR 0043](docs/adr/0043-the-model-gains-banked-records-a-reboot-and-a-live-compaction.md)
now narrates all three versions of the transition in order, with the test's current, narrower
name. The second was a real gap in `refine::Observation::reconstructed`: the eleventh
finding's `next_id` field is the caller's own report and was checked against nothing, so an
observation could name a `next_id` that collides with a record in its *own* `records` list
rather than only with one an earlier erase had dropped. Records 0 and 1 with
`next_id: Some(1)` used to reconstruct without complaint; a `Reboot` then a
`Declare(Schedule)` minted a second `RecordId(1)`, and the `Program` after it found the older
record already whole and refused with `RecordAlreadyWritten`, stranding the new declaration
on the identity collision issue #67's whole counter scheme exists to forbid — reached with no
erase anywhere in the trace. `reconstructed` now refuses with
`Impossible::NextIdReissuesAResident` whenever `next_id` is not strictly past every resident
record's id, the same floor `reboot` itself keeps.
`reconstruction_refuses_a_next_id_that_reissues_a_resident` is the regression, verified
against a scratch reproduction of the stranding before the check existed and checking both
the refusal and that the exact floor (one past the highest resident id) is still accepted.

The same round's next pass found the sharper of the two: `Observation`'s per-record tuple
carried no bank identity at all, so `Journal::reconstructed` hardcoded every record to
`BankId::A` regardless of which bank `observation()` had actually read it from. A device that
retires a record in bank A behind its very first seal — landing on B — and then declares a
fresh record in B recovers `[1]` directly; round-tripped through `observation()` and
`reconstructed()`, both records land in `BankId::A`, `recovering_bank()` stays `B`, and
neither record's bank matches it, so the reconstructed state recovers `[]` instead — a real
defect in the general bridge, invisible only because every writer this crate currently drives
through it is single-bank. `Observation::records` now carries each record's bank as a fifth
tuple element; `Journal::observation()` reports the real field, `Journal::reconstructed()`
uses it instead of the hardcoded convention, and `refine::abstraction()` tags every record
`BankId::A`, matching the module docs' "no writer this function abstracts ever touches a
second bank" exactly as it already does for `next_id`.
`observation_and_reconstruction_agree_on_a_state_with_records_in_two_banks` is the
regression, driving the real two-record two-bank sequence end to end and asserting the
round-trip preserves what `Specified.recover` returns, verified against a scratch
reproduction of the `[1]` vs `[]` divergence before the fix existed.

The round after that found the sharper defect the new bank field made reachable:
`Journal::bank_of` — which `single_authority` and `durable_intent` both call to ask "which
bank is this id's record really in" — answers with the *first* matching record it finds,
ignoring bank entirely, so a hand-built `Observation` naming `RecordId(0)` once in a retired
bank and again in the sole authoritative bank made `single_authority` misreport a
legitimately recovered record as belonging to the retired one, and could equally make
`durable_intent` skip checking a dispatch that needed checking. The real firmware's own
effect sequence does restart at zero across a swap
(`waymaker_core::id::EffectIdAllocator` via `Installed::allocator`), but that is a different
identity space from this crate's `RecordId`: the model's id is a single, device-wide counter
invented by issue #67 specifically so it is never reused, so no legal transition sequence
can ever declare the same id twice, in one bank or two — a caller bridging a real device that
restarts its own sequence per run has to assign each record a distinct label the way
`refine::abstraction()` already does, not reuse the real restarting number directly.
`Journal::reconstructed` now refuses with a new `Impossible::RecordIdDeclaredTwice` whenever
an observation's `records` names one id more than once, closing the gap the same way as
`next_id`'s own floor rather than by threading bank identity through every by-id lookup and
through `dispatched` (which carries no bank tag at all, and would need one too).
`reconstruction_refuses_the_same_id_declared_in_two_banks` is the regression, verified
against a scratch reproduction of `single_authority`'s false-positive breach before the check
existed.

The same round's Codex pass raised a second finding on the same shift the eleventh finding's
fix uses: `refine::bank_after_seal`'s `real_generation.0 + 1` numbering has no model value for
the real firmware's actual final usable generation, since `Generation::successor` only
refuses *at* `Generation::MAX` and the shift would need `u32::MAX + 1` to represent sealing
there. Investigated rather than fixed: the reservation exists so a bank with no seal
(`authoritative_generation() == None`) and a bank the model has not yet distinguished from one
both read as "nothing sealed here" through `begin_seal`'s own `None => 1`, and removing it
(numbering the model's first seal `0`, since `Bank`'s variants already tell "unsealed" apart
from `Sealed(0)` without needing the reservation) would change how many distinct generation
values `Bound::generations` admits at a given cap, moving `tests/census.rs`'s pinned counts —
for a boundary nothing here drives anywhere near: `Bound::PROOF` and the bank-swap refinement
sweep both cap generations at 3, and the one place this crate reaches a real `u32::MAX` is
`model.rs`'s own hand-built `a_generation_at_the_ceiling_is_refused_rather_than_tied_with_the_other_bank`,
entirely on the model's own terms and never through this shift. Documented as the same
standing `obligation.rs` already records for `single-authority`'s generation dimension — "a
generation is an unbounded integer, where the firmware refuses at the ceiling rather than
proving the refusal unnecessary" — one integer narrower than stated there, in both
`refine::bank_after_seal`'s doc comment and beside `begin_seal`'s own `checked_add`, rather
than moving the census for a boundary no proof or refinement test comes near.

The same round's next pass on the pull request that closed issue #67 found that the "nothing
is owed" `single-authority` row itself overclaimed. `tests/refinement.rs`'s two refinements
never compose: the record writers (`journal`, `effect_protocol`, and the one that survives a
failed program) never touch a bank, and the bank-swap writer's own bound —
`BANK_REFINEMENT.records: 0` — never declares one, so every `Observation` the swap sweep
builds is `records: Vec::new()` and `single_authority` is checked there against
`recovered: &[]`. `Invariant::SingleAuthority`'s bank check — that a recovered record's bank
is the sole authoritative one, the very thing this round's earlier finding fixed `bank_of`
over — has therefore never been refined against a real crashed device that both wrote a
record and swapped banks, only proved exhaustively over the model and unit-tested directly
against a hand-built multi-bank `Observation`. `obligation.rs`'s `single-authority` row now
names this gap in its `owed` field instead of `None`, matching the standing every other
partly-discharged clause here already has;
`what_is_still_owed_is_written_down_rather_than_left_out` in `tests/obligations.rs` pins the
exact set of clauses with something owed, which is now `single-authority` and
`bounded-decoding` rather than `bounded-decoding` alone. CLAUDE.md's own guarantees table and
its "that the ghost model is a model of *this* firmware" bullet are corrected the same way,
rather than left to read as though the earlier, wider claim still held. Closing it for real
needs a writer that both declares records and performs a real two-bank swap — the two things
`crates/waymaker-fault/tests/banks.rs` and `tests/refinement.rs`'s two existing halves each
do on their own — refined together the way each already is separately.

A merge-time round found a second real bug beside the ADR staleness above: `durable_intent`'s
own retired-bank exemption compared `bank_of(intent) != recovering_bank()`, and `bank_of`
answers `None` when `intent` names no record at all — not only when it names one in a bank
other than the one recovery would boot from. `None != Some(bank)` took the same branch as a
genuine retired-bank mismatch, so a dispatched effect with no schedule record on media
*anywhere* was silently exempted rather than breaching, even though `refine::abstraction`'s
`dispatched` parameter is documented to report exactly that shape on purpose — "an effect
that reached the world" independent of what the ledger holds, so a run whose effect left
nothing recoverable behind can be described and judged rather than shrugged off as moot. The
fix only takes the exemption when `bank_of` answers `Some(bank)` that disagrees with
`recovering_bank()`, via `is_some_and`; `a_dispatch_with_no_record_at_all_is_a_breach_rather_than_moot`
in `tests/refinement.rs` is the regression, verified to fail against the old check. No
reachable state changes: `Journal::dispatch` refuses a `Transition::Dispatch` naming a record
that does not exist, so `explore()`'s exhaustive search never produces the shape this bug
needed — only a hand-built `Observation`, of the kind `refine::abstraction` exists to build
from a real crashed device, could reach it.

The next round found a third: `Journal::reconstructed` passed `observation.sealed_once`
straight through with no check against `observation.banks`, so a hand-built `Observation`
could claim `banks: [Bank::Erased, Bank::Sealed(1)]` while leaving `sealed_once` at its
default of `false` — a combination no real transition sequence can produce, since
`commit_seal` is the only place a bank becomes `Sealed` and it sets `sealed_once` true in the
same step. Left unrefused, `recovering_bank()` took the pre-seal convention at face value and
answered `BankId::A` regardless of which bank was really sealed, and `has_sealed()` then
exempted the state from `SingleAuthority` entirely — a reconstructed state could recover a
stale bank's records while the truly sealed bank's were ignored, with the one guarantee built
to catch exactly that never even consulted. `Impossible::SealedBeforeAnyHistoryOfSealing` is
the fix, refused in `reconstructed` the same way as the existing `RecordIdDeclaredTwice` and
`NextIdReissuesAResident` checks; `reconstruction_refuses_a_sealed_bank_with_sealed_once_left_false`
in `tests/refinement.rs` is the regression, verified to fail against the old code.

Issue #77 closed a gap in issue #23's own anti-bricking argument. `Journal::after` takes a
`Recovery` by value so that one scan cannot hand out two writers at one offset — but
`Recovery` derived `Clone`, and `Journal::after(recovery.clone())` was one scan handing out
two writers anyway, each ready to program its frame over the other's. `Recovery` is no
longer `Clone`: a caller that wants two writers now has to run two scans, each its own
`Recovery` built and pumped from scratch. A compiling doctest and a `compile_fail,E0599`
twin hold the shape the way issue #24's typestate doctests do, and `recovery-surface` grew
a second half: it fires on a `Clone` derived on `Recovery`, on one derived behind a
`cfg_attr`, on one reached through a renamed derive macro, on a handwritten `impl`, and on
a rename of `Recovery` itself — read as "this pin is checking nothing" rather than as a
clean pass, the way every other surface pin here already treats a missing module. Review
of this change found the first three by trying them against the real file, which is why
all five are swept rather than argued. `waymaker-fault`'s and `waymaker-flash`'s own tests
never called `.clone()` on a `Recovery`, so nothing needed to change beside the type and
the three places that documented it — `recovery.rs`'s own doc and `append.rs`'s two.
Sixteen further review rounds hardened `recovery-surface`'s Clone-detection scanner
against a raw identifier, a `super`-qualified or capped alias, a bare macro invocation at
any nesting depth (item, statement, or an out-of-line submodule reached through a
function-body `#[path]` declaration), a chained trait alias, a local type alias on a
self-type, and a parenthesized self-type — each verified against the real crate with an
actual compiling bypass before being closed. Rounds 15 and 16 closed five more of the same
shape, each again verified against a real, compiling bypass before being closed: an `impl`
declared as a local item inside a function body, which `collect_trait_implementors` did not
descend into; a `mod` declared one control-flow block deeper than a function's own
statements, which `child_modules`' function-body descent still could not see; a `#[path]`
reached only through a `cfg_attr`, which the old scan read as no `#[path]` at all and fell
back to a harmless natural sibling; a parenthesized type-alias target (`type R =
(super::Recovery);`), the alias-declaration side of the parenthesizing round 14 had already
closed on the self-type side; and a self-type reached through a type-position macro
invocation, which `declares_item_macro` now flags alongside the item- and
statement-position macros it already caught. Round 17 found three more. A local `type`
alias declared inside the same function body as the `impl` that names it was invisible to
`collect_type_aliases`, which read only `Item::Mod` even though `collect_trait_implementors`
had descended into function bodies since round 15 — so `collect_type_aliases` gained the
same descent, into a free function, an `impl` block's own methods and associated consts, a
trait's default method bodies and default associated consts, and a `const`/`static`
initializer, all via a shared `nested_body_items` the trait-implementor scan now uses too,
closing a `const _: () = { impl Clone for super::Recovery { .. } }; };` reaching neither
scanner at all. And round 16's own fix had a bug: it put a `cfg_attr`-nested `path` target
into the *same* flat candidate list as the natural `name.rs`/`name/mod.rs` pair, so a real,
legal layout — both files present, for two different builds — made the exact-one resolver
misreport the workspace as `Ambiguous`. `ChildModule::candidates` is now grouped rather than
flat: each group (the natural pair, or one `cfg_attr` target) resolves independently to at
most one file, and every group's resolution is scanned rather than requiring exactly one
across the whole thing — ambiguous only when rustc itself would reject one group, never
because two different builds legally pick two different files. Round 18 found two more, both
the same shape as round 17's `const`/`static` finding one level over. `collect_child_modules`
— the module-tree walk that discovers an out-of-line child file at all, which round 17 left
with its own Fn/Impl/Trait descent rather than the shared `nested_body_items` because it has
to carry a `test_gated` flag `nested_body_items` throws away — still read only those three
item shapes, so a `mod` declared inside a `const`/`static` initializer's own block, an enum
variant's discriminant, or a type alias's array-length expression was never even reached: the
child file existed on disk and nothing scanned it, which is a more severe gap than an
unresolved self-type inside a file that was reached. And `nested_body_items` itself — the
Clone-detection scanners' shared descent — covered `Item::Const`/`Item::Static` since round 17
but not `Item::Enum`'s discriminants or `Item::Type`'s own type, so an `impl Clone for
super::Recovery` buried in either shape reached neither `collect_type_aliases` nor
`collect_trait_implementors`. Both are fixed the same way: `collect_child_modules` gained its
own `Const`/`Static`/`Enum`/`Type` arms (each verified against a real, compiling bypass — a
`mod` behind `#[path]` inside a `const _: () = { .. };`, reached and flagged, where round 17's
own fix left it unreached), and `nested_body_items` gained `Enum` and `Type` arms via a new
shared `type_items` helper, mirroring `block_items`/`expr_items`. Round 19 found the same
type-bearing shape one level over: a struct's own field types can each carry a buried block —
`struct Holder { field: [(); { impl Clone for super::Recovery { .. }; 0 }] }` — exactly the
way a type alias's own type can, and neither `nested_body_items` nor `collect_child_modules`
read `Item::Struct` at all. Both gained a `Struct` arm, the latter through a new
`struct_field_bodies` helper (mirroring `impl_member_bodies`/`trait_member_bodies`) split out
to keep `collect_child_modules` under this file's own line-count lint, each field's `#[cfg(test)]`
gate carried onward the same way an enum variant's already was.

Round 20 found two more, one of the same type-bearing shape and one of a different kind
entirely. The same shape: an enum variant's own *fields*, not only its discriminant, can
each carry a buried block too (`enum E { V([(); { impl Clone for super::Recovery { .. };
0 }]) }`), so both `nested_body_items`'s enum arm and `collect_child_modules`'s gained a walk
over each variant's fields alongside its discriminant, the latter through a new
`enum_variant_bodies` helper mirroring `struct_field_bodies`. The different kind was a false
*positive* rather than one of this scanner's usual false negatives: `trait_implementors`
built one alias table for an entire file by recursing `collect_item_aliases` and
`collect_type_aliases` through every inline module and flattening everything into one shared
table, so an unrelated nested module's own `use core::clone::Clone as C;` — which real Rust
scopes strictly to that `mod { .. }` block, never letting it leak to a sibling scope or its
parent — could resolve an unrelated, identically-named alias used by a completely different
`impl` elsewhere in the file, rejecting a `Recovery` that implemented neither `Clone` nor
anything resolving to it. `collect_type_aliases` no longer recurses into `Item::Mod` at all;
a new `module_scope_aliases` builds a table from one module's own direct `use` and type-alias
declarations only, and `collect_trait_implementors`'s own `Item::Mod` arm now calls it afresh
for each nested module instead of inheriting the caller's table — matching that a `mod { .. }`
block is a real scope boundary in Rust while a function, `impl`, `const`, `enum`, `struct` or
`type` body is not, so `nested_body_items`'s descent into those still inherits whatever table
the caller passes down.

Round 21 found two more, both closing an asymmetry round 20's own fix left standing rather
than opening a new one. Plain-path type aliases had reached any nesting depth
`nested_body_items` covers since round 17 — a local `type R = super::Recovery;` inside a
function body — but the parallel `use`-alias table only ever read a scope's own direct items,
never descending into a body: `fn install() { use core::clone::Clone as C; impl C for
Recovery { .. } }` is legal Rust exactly like round 17's local type alias, and resolved `C` to
nothing. And every prior round that walked a function or method descended only into its
*body* — never its own parameter types or return type, which can carry a buried block exactly
the way a type alias's, a struct field's, or an enum variant field's own type already could
(`fn hidden(_: [(); { impl Clone for super::Recovery { .. }; 0 }]) {}`). A new
`fn_signature_type_items` walks a signature's inputs and output with `type_items`, used
everywhere a function or method's body already was — `nested_body_items`'s `Fn`/`Impl`/`Trait`
arms, `impl_member_bodies`, `trait_member_bodies` (now producing an entry for a trait method
with no default body too, since its signature is parsed either way), and
`collect_child_modules`'s own `Fn` arm.

Round 22 then found that round 21's own fix for the `use`-alias gap had reintroduced round
20's exact false positive one level down. Giving the `use`-alias table the same nested-body
descent the type-alias half already had meant recursing into *every* function body in a
scope and accumulating all of their aliases into one table shared across every other item in
that same scope — so an unrelated function's own local `use core::clone::Clone as C;` could
resolve an unrelated `impl C for Recovery` in a *different* function, or at the module's own
top level, exactly the shape of false positive round 20 closed for `mod` blocks. The fix
redesigns how a body's own local aliases are threaded rather than patching the symptom:
`collect_type_aliases` and the round-21 `use`-alias walk are both retired, replaced by
`direct_scope_aliases` — one flat item list's own directly-declared `use` and `type` aliases,
with no recursion into anything, module or body alike — and `collect_trait_implementors`
itself now computes a *fresh* extension of the ambient table from each item's own
`nested_body_items`, immediately before recursing into that one item alone, so no two
sibling items (two functions, two impl blocks, one of each) ever share an extension.
`module_scope_aliases` is now a thin wrapper over `direct_scope_aliases`, used only for file
scope and each nested module's own fresh table, matching round 20's original design. Round
22 also found two findings of its own established shape: a `union`'s own field types can
carry a buried block exactly the way a struct's or an enum variant's already could, closed
the same way with a new `union_field_bodies` mirroring `struct_field_bodies`; and a function
signature's own generics — a type parameter's bounds and default, and a `where` clause
predicate's bounded type and bounds — can carry one too, closed by a new `generics_items`
chained into `fn_signature_type_items` alongside its parameter and return types.

Round 23 found three more, and the deepest of the three closed a gap round 22's own
redesign left standing at a finer grain than the module or item boundary it was drawn
at. `collect_trait_implementors` still computed each item's own alias extension from
its *whole* body flattened by `nested_body_items` — every item reachable through
control flow, however deeply nested — so `use self::Harmless as C;` at module scope,
followed by `fn install() { if false { use core::clone::Clone as C; } impl C for
Recovery {} }`, resolved the harmless `impl` through the inner `use`, even though that
`use` is scoped only to the `if false { .. }` block it is declared in. The fix replaces
the whole flatten-then-extend design with one that walks exactly one `syn::Block` at a
time: `nested_body_items` is retired, along with the `BlockItemVisitor` it and
`block_items`/`expr_items`/`type_items`/`generics_items` shared; a new
`DirectChildBlockVisitor` captures blocks instead of items, stopping at each one rather
than flattening through it (and at a nested item's own boundary, which is handled
separately), giving `direct_blocks_in_expr`/`direct_blocks_in_type`/
`direct_blocks_in_generics`/`direct_blocks_in_signature` and
`direct_child_blocks_of_block`. `collect_trait_implementors_in_item_body` finds every
scope-root block an item's signature, body, initializer or member types can carry, and
`collect_trait_implementors_in_block` walks one block's own direct items to build that
block's own scope, then recurses into every block nested directly in one of its own
statements — one level at a time, in real Rust's own order — so a sibling block's
aliases are never on a table it did not declare them in, while a body still correctly
inherits its enclosing scope (a body is not a Rust scope boundary the way `mod { .. }`
is). The second: `impl T for X { type A = [(); { impl Clone for Recovery { .. }; 0 }];
}` buries a non-local `impl` inside an associated type's own type exactly the way a
type alias's, a struct field's, an enum variant field's, or a union field's type
already could, and the fallback for an `impl` block's own members — in both the
handwritten-implementation scan and `impl_member_bodies`, the module-tree scan's
identical case — dropped `ImplItem::Type` on the floor instead of walking it with
`type_items`. The third: `impl Clone for <() as Alias>::Target`, where a reached file
defines `trait Alias { type Target; }` and binds `Target` to `Recovery`, is legal Rust
whose self-type is a projected associated type — `syn`'s `Type::Path` stores the
qualified self and the trait separately from `path`, which here is only `Target`, so
resolving `path` alone through the alias table missed that rustc normalizes the
projection to `Recovery`. This scan does not resolve trait bindings, so a self-type
carrying a `qself` now fails closed to `UNRESOLVED_DERIVE` the same way a
`super`-qualified path already does, rather than silently comparing the unqualified
associated-type name.

Round 24 found three more. `Item::Impl`, `Item::Trait`, `Item::Enum`, `Item::Type`,
`Item::Struct` and `Item::Union` each declare their own `syn::Generics` — a type
parameter's bounds and default, and a `where` clause predicate — and round 22's
`generics_items` had only ever been chained into a function or method *signature*'s
own generics; none of the six item kinds' own generics were read at all, so `struct
Holder<T = Wrapper<{ impl Clone for Recovery { .. }; 0 }>>(T);` reached neither
`collect_trait_implementors_in_item_body` nor `collect_child_modules`. Both gained a
`direct_blocks_in_generics`/`generics_items` pass over each item's own `.generics`
alongside its existing member-body pass — `collect_child_modules` needed a new
`nested_item_bodies_for_child_modules` split out to keep that function under this
file's own line-count lint once the six new passes joined it. The second is round 23's
projected-associated-type finding one hop earlier: `type R = <() as Alias>::Target;`,
after a reached file binds `Target` to `Recovery`, is legal Rust whose target is the
same kind of projection a self-type can be — but `direct_scope_aliases`'s `Item::Type`
arm read `target.path` and discarded `target.qself`, so `R` resolved to the unqualified
name `Target` instead of to `Recovery` or to `UNRESOLVED_DERIVE`; a `type` alias whose
target carries a `qself` now stores `UNRESOLVED_DERIVE` as its own target, so any
self-type chased through it fails closed the same way. The third is a structural gap
in `extend_with_local_scope` itself: it appended a body's own local aliases to the
ambient table rather than having a local one *shadow* an ambient one of the same
name, so `use core::clone::Clone as C;` at module scope beside a function body's own
`use self::Harmless as C;` before `impl C for Recovery {}` left both bindings on the
table — real Rust resolves the impl to the block-local `Harmless` and the ambient
`Clone` is unreachable inside that block, but `every_resolution` still found it and
rejected a `Recovery` that never implements `Clone`. The fix drops every ambient alias
whose local name is redeclared in the new scope before adding the new ones, so a
shadowed name resolves only through its innermost declaration.

Round 25 found four more, one of them in round 24's own fix. The first: round 24's
shadowing fix dropped an ambient alias whenever *any* local declaration of the same
name existed, including one behind `#[cfg(any())]`, which never compiles — module scope
importing `use core::clone::Clone as C;` beside a function body's `#[cfg(any())] use
self::Harmless as C;` never actually shadows the ambient binding in the only
configuration that ships, but the unconditional version dropped it anyway and missed
that `impl C for Recovery` still resolves to `Clone`. Only a local declaration with no
`#[cfg(..)]` at all (checked with `has_any_cfg`, not `has_cfg_test`) now shadows the
ambient alias it redeclares; a conditionally-declared one is added alongside the
ambient binding instead, keeping both candidates reachable the same way this scan
already reads past any other unevaluated `cfg`. The second: `impl crate::C for
Recovery` can name a crate-root `use core::clone::Clone as C;` in `lib.rs`, a file this
per-file scan never reads, but `lookup_candidate` stripped a leading `crate` the same
way it correctly strips a leading `self`, silently resolving `C` against *this* file's
own table instead of failing closed. The first fix for it treated *every*
`crate`-qualified path as unresolved, and review of that fix found it rejecting
`waymaker-embassy/src/wiring.rs`'s own `use crate::dispatch::ActivityDispatcher;` — an
ordinary, unaliased import this codebase uses throughout — because `ctx-facade`'s
future-detection scan shares this same machinery. The corrected fix is narrower: only a
*bare*, two-segment `crate::NAME` fails closed to `UNRESOLVED_DERIVE`, since that shape
alone asks to look `NAME` up in a table this scan does not have; a longer
`crate::a::b::NAME` is a path to another module's own real declaration and falls
through to the ordinary "no matching alias, take the last segment" branch every other
unresolvable multi-segment path already uses. The third: `const N: [(); { impl Clone
for Recovery { .. }; 0 }] = [];` buries a non-local `impl` inside an associated const's
own *declared type* exactly the way its initializer already could, and every const-like
arm — `ImplItem::Const`, `TraitItem::Const`, `Item::Const`, `Item::Static` — walked only
the initializer expression, never the type ascription; a trait const with no default
value used to contribute nothing at all; now every one contributes its type regardless,
matching how a method's signature is already read whether or not it has a default body.
The fourth: `impl Marker<{ impl Clone for Recovery { .. }; 0 }> for Holder {}` is legal
Rust with `non_local_definitions` allowed — the impl header's own trait path and self
type can each bury a block through a const generic argument exactly the way the impl's
own generic *declarations* already could, and neither was read; a new
`direct_blocks_in_path`/`path_items` pair mirrors the existing type-walking helpers for
this shape. `collect_trait_implementors_in_item_body`'s `Item::Impl` arm needed a split
into `impl_member_scope_roots`/`trait_member_scope_roots` to stay under this file's own
line-count lint once the header roots joined it.

Round 26 found four more. The first: `extern "C" { fn hidden(_: [(); { impl Clone for
Recovery { .. }; 0 }]); }` is legal Rust, and `Item::ForeignMod` reached this scan's
fallback arm entirely — a foreign function's own signature and a foreign static's own
declared type, either of which can bury an impl the same way an ordinary signature or
declared type already could, were never walked at all. Both
`collect_trait_implementors_in_item_body` and the module-tree walk's
`nested_item_bodies_for_child_modules` gained a `ForeignMod` arm, the latter split into
its own `foreign_mod_bodies` to stay under this file's line-count lint; verifying it
against the real crate needed a standalone `rustc` file rather than a `waymaker-flash`
build, because an `extern` block requires an `unsafe extern` block since edition 2024
and this crate's `#![forbid(unsafe_code)]` refuses one regardless of where it is
written, so the shape cannot appear anywhere in this crate's own tree even though the
scanner has to handle it as general Rust syntax. The second: `trait Outer { type A<T>
where T: Marker<{ impl Clone for Recovery { .. }; 0 }>; }` is a GAT-shaped associated
type *declaration* in a trait, as opposed to an impl's associated type, which round 23
already covers — `TraitItem::Type` fell through both scanners' wildcard arms, so its own
generics (whose `where` clause bounds can bury a block), its own trait bounds, and its
default type were never visited. The third: `trait Outer: Marker<{ impl Clone for
Recovery { .. }; 0 }> {}` — a trait's own supertrait bound list was never walked by
either scanner, only its generics and selected members, so a supertrait bound's own
const generic argument could bury an impl invisibly; a new
`direct_blocks_in_bounds`/`bound_items` pair mirrors the existing generics-walking
helpers for a `Punctuated<TypeParamBound, Token![+]>`, shared by the trait's own
supertraits and by the second finding's associated-type bounds. The fourth was a false
positive rather than a false negative: `enum_variant_bodies` gated every field of a
variant by the *variant's* own `#[cfg(test)]` alone, unlike `struct_field_bodies` and
`union_field_bodies`, which each also gate a field by its own attribute — so a field
carrying its own `#[cfg(test)]` inside an otherwise-ungated variant was read as
reachable in production, and a legitimate test-only reimplementation buried in such a
field's type was wrongly reported as a production `Clone` impl. Each field is now its
own `(bool, Vec<&syn::Item>)` entry gated by `variant_gated || has_cfg_test(field)`,
matching the two sibling helpers; the discriminant, having no per-part gate of its own
to combine with, keeps its single entry at the variant's own gate.

Round 27 found two more. The first: `impl T for X { type A<U: Marker<{ impl Clone for
Recovery { .. }; 0 }>> = (); }` is legal Rust — a generic associated-type
*implementation*'s own type-parameter bound can bury a block through a const generic
argument, matching the trait's own declared bound for coherence, exactly the way an
impl's own generic *declarations* already could since round 24 — and both
`impl_member_scope_roots`'s and `impl_member_bodies`'s `ImplItem::Type` arms read only
`assoc_type.ty`, never `assoc_type.generics`. Both gained a
`direct_blocks_in_generics`/`generics_items` pass over the associated type's own
generics alongside its existing type pass. The second was a false positive in
`declares_item_macro` rather than a false negative in the Clone scan: its visitor
checked `#[cfg(test)]` only on the enclosing `syn::Item`, so `#[cfg(test)] fn helper()
{ generate_clone!(); }` inside an otherwise-production `impl` or `trait` block was
still reached by the default descent into the *member*, because `syn::visit::Visit`
dispatches a member through `visit_impl_item`/`visit_trait_item` rather than through
`visit_item` again — the one override this visitor had. A macro that only ever
compiles under `#[cfg(test)]` therefore failed the whole file closed over code that
ships with nothing generated at all. The visitor gained `visit_impl_item` and
`visit_trait_item` overrides, mirroring its existing `visit_item` one, backed by a new
`trait_item_attrs` alongside the existing `impl_item_attrs`.

Round 28 found three more. The first was the same macro-visitor gap one subitem
further: `#[cfg(test)] field: generate_type!()` on a production struct's field, an
enum variant, and a foreign item each carry their own gate the visitor still walked
past, since `syn::visit::Visit` dispatches each of those through its own method
(`visit_field`, `visit_variant`, `visit_foreign_item`) rather than through any of the
three overrides round 27 added. All three gained the same `has_cfg_test`-and-return
guard, the last backed by a new `foreign_item_attrs`. The second: `type Identity<T> =
T; type R = Identity<super::Recovery>; impl Clone for R { .. }` is legal Rust whose
alias target names a real generic alias with an argument substituted in —
`direct_scope_aliases`'s `Item::Type` arm read only the target path's segment
identifiers, discarding `<super::Recovery>`, so `R` resolved to `Identity`'s own
declared target, `T`, rather than to the type actually substituted in, and the alias
was silently accepted as not `Clone` instead of failing closed. This module does not
perform generic substitution — that is real type-checking, not parsing — so an alias
target carrying a generic argument anywhere along its path now fails closed to
`UNRESOLVED_DERIVE`, the same way a projected associated type already does. The
third: `mod traits { pub use core::clone::Clone as C; } impl traits::C for
super::Recovery { .. }` is legal Rust, and the qualified trait path `traits::C` had
no alias to resolve against at all — `every_resolution` only ever looked up a single
segment as a candidate, so the ordinary identifier `traits` fell through to the "no
matching alias, take the last segment" branch and reported the bare, still-aliased
name `C` rather than `Clone`. A new `direct_scope_module_aliases` registers
`traits::C` as a synthetic alias for whatever `C` resolves to inside `traits`' own
scope (one level of qualification only, matching how deep this round's finding
reaches), folded into `module_scope_aliases` alongside the existing
`direct_scope_aliases`; a new `qualified_candidate` — `lookup_candidate`'s two-segment
twin, both built over an extracted `strip_self_prefix` — is tried before the plain
single-segment lookup at every hop, branching over both rather than stopping at the
first the way this scan's other duplicate-candidate cases already do.

Round 29 found three more, two of them the derive-side and file-boundary twins of
round 28's own findings. The first: `struct_derives` collected its aliases with a
hand-rolled loop over `Item::Use` alone, predating `module_scope_aliases` itself, so it
read neither a plain-path `type` alias (`type Klon = core::clone::Clone;
#[derive(Klon)]`, resolved for every other caller since round 13) nor an alias exported
one level through an inline module's own name (`mod traits { pub use
core::clone::Clone as C; } #[derive(traits::C)]`, round 28's own fix for a handwritten
`impl`). It now reads `module_scope_aliases` like every other caller, gaining both for
free. The second: a production-reachable child file, or an inline module nested
anywhere the module tree reaches, is free to declare its own, wholly unrelated `struct
Recovery` and hand it a `Clone` impl with nothing to do with the pinned type — real
Rust name resolution has the unqualified `Recovery` written there mean the *local*
declaration, exactly as `struct_derives` already reads only a *top-level* declaration
in the pinned type's own file as the one that counts for a derive, but the
handwritten-impl scan read both as the same bare name and rejected a file that never
gave two writers to anything. A new `shadow_aliases_for_local_types` registers a
synthetic, self-referential alias for every struct, enum or union directly declared in
a scope, resolving to a new sentinel, `LOCAL_SHADOWED_TYPE`, rather than to its own
name; `trait_implementors_for_pinned_type` — `trait_implementors`'s refinement for this
one caller, so `future_trait_implementors`'s unrelated scan is untouched — folds it into
every nested inline module's own scope unconditionally, and into the scanned file's own
top level whenever that file is not the one the pinned type is actually declared in,
because a child file reached through `mod name;` is exactly as nested, from the whole
tree's point of view, as `mod name { .. }` would have been had its contents been
written inline. The third: `mod traits;`, with its content in a sibling file this
per-file scan never opens, had no alias for a qualified `traits::C` to resolve against
at all — round 28 closed this same gap for an *inline* `mod traits { .. }`, whose
content is right here in the same file to read, but an out-of-line module's content
lives somewhere this scan cannot see, so the honest answer is the same fail-closed one
a `super`-qualified path already gets rather than a guess. A new
`direct_scope_opaque_module_aliases` registers every out-of-line `mod name;` as a
synthetic alias to `UNRESOLVED_DERIVE`, folded into `module_scope_aliases` alongside the
other two; and `every_resolution` gained a matching guard so that sentinel propagates
through a further hop (`traits::C` resolving one hop to `["<unresolved derive>", "C"]`)
rather than letting the tail segment silently survive as the harmless-looking `"C"` —
the same shape of gap the `LOCAL_SHADOWED_TYPE` sentinel needed its own propagation
guard for, added beside it on the same review round.

Round 30 found two more, both the same shape one level deeper than round 29's own
fixes. The first: a function is just as free to declare its own local `struct
Recovery` as an inline module is — `fn install() { struct Recovery; impl Clone for
Recovery { .. } }` is legal Rust whose unqualified `Recovery` means the block-local
declaration — but `extend_with_local_scope`, the function that threads a block's own
local aliases into the ambient table, only ever called `direct_scope_aliases`, which
reads `use` and `type` items alone; a block-local struct, enum or union never earned
the `LOCAL_SHADOWED_TYPE` marker `shadow_aliases_for_local_types` registers for the
identical shape at module scope, so it was rejected as though it implemented `Clone`
for the pinned type. `extend_with_local_scope` now takes the same `shadow_locals` flag
every other caller in this chain already threads, and — when set — folds
`shadow_aliases_for_local_types` into both the block's own local aliases and the set
that unconditionally shadows an ambient one of the same name, exactly as a block-local
`use` or `type` alias already does. The second: `mod traits { pub use
core::clone::Clone as C; } use traits::*; #[derive(C)] struct Recovery;` is legal
Rust, and `collect_tree_aliases` deliberately drops `UseTree::Glob` — this scan does
not perform name resolution, so it has no way to know what a glob import actually
brings into scope, and issue #51's own "what is not checked" already states that
limit for every scanner in this module. Silently treating `C` as an ordinary,
unaliased identifier let it resolve to the harmless-looking bare name `C` instead of
`Clone`, rather than to the fail-closed answer a `super`-qualified path already gets.
A new `GLOB_IMPORT_MARKER` sentinel, registered by `glob_marker_alias` for any scope
whose directly declared `use` items name a glob anywhere in their tree (mirroring
`collect_tree_aliases`'s own recursive walk through a `UseTree::Group`), makes
`every_resolution`'s "no matching alias" fallback fail closed to `UNRESOLVED_DERIVE`
rather than trust the bare name whenever it is present. Both fixes are threaded only
through `shadow_locals`-gated callers — `trait_implementors_for_pinned_type` and
`struct_derives` — so `trait_implementors`'s unrelated `future_trait_implementors`
scan, which asks no such question about a *specific* pinned type, is untouched by
either.

Round 31 found two more. The first is round 28's own qualified-alias fix one module
deeper: `mod traits { pub mod nested { pub use core::clone::Clone as C; } } impl
traits::nested::C for super::Recovery { .. }` is legal Rust, and
`direct_scope_module_aliases` only ever read a directly nested module's own *direct*
aliases — never a module nested inside that one — so `traits::nested::C` had nothing to
resolve against and fell through to the harmless-looking bare name `C`.
`direct_scope_module_aliases` is now recursive, chaining a nested module's own direct
aliases with the aliases every module nested inside *that* one contributes, and
prefixing every one of them with the current module's own name — building a qualified
name of arbitrary depth rather than one level. `every_resolution`'s own qualified-lookup
half needed the matching generalization: `qualified_candidate`, which only ever tried a
fixed two-segment join, is now `qualified_candidates`, trying every prefix length from
longest to shortest so a path qualified through any number of nested modules has a
candidate to match against. The second is not an invocation at all, syntactically: an
**attribute** macro. `declares_item_macro` had flagged an item-, statement- or
type-position macro *invocation* since round 16, but never asked whether an item
carried an attribute macro at all — `#[a_transform] struct Anything;` compiles today,
and unlike a derive, an attribute macro may rewrite the item it decorates or splice an
unrelated item in beside it, so nothing here could say it does not expand to `struct
Anything; impl Clone for Recovery { .. }`. Verified against a real, compiling two-crate
example — a `proc_macro_attribute` that injects exactly that impl beside an unrelated
struct — rather than only against `syn`'s parse of the shape, since a real attribute
macro is what makes the finding a live one rather than a hypothetical. The fix reads
every attribute the visitor's existing traversal already reaches, at any nesting depth,
through `syn`'s own generated callback for every attribute node rather than a case added
at each place one can appear, and asks of each one whether it is a builtin the compiler
interprets itself or a namespace rustc treats as opaque to a named tool — `cfg_attr`
included, read at any depth for the same reason `collect_derive_names_from_meta`'s own
recursion is — with anything else read the same way an item-, statement- or
type-position macro invocation already is.

Round 32 found two more, on the pull request's own merge of a substantial upstream
drift: `future_trait_implementors` had independently been rewritten on `main` to a
lexical-scope `resolve_segments` (issues #109 and #169, PRs #160 and #176) while this
branch built `every_resolution` and its sentinels on the older, shared alias-list
design — the merge kept both as fully independent implementations rather than
re-deriving either against the other's architecture, `future_trait_implementors`
restored to `resolve_segments` verbatim and this branch's `trait_implementors`/
`trait_implementors_for_pinned_type`/`collect_trait_implementors` left as their own
functions. The first finding is the same macro-visitor gap as round 27's and round 28's,
one shape further: `fn helper() { #[cfg(test)] generate_clone!(); }` is legal Rust whose
macro statement never exists in a shipped build, but `visit_stmt_macro` read only the
fact that a `StmtMacro` node was reached, never its own `attrs` — `syn::visit::Visit`
dispatches a statement-level macro through this method rather than back through
`visit_item`, the same way a field, a variant or a foreign item already needed its own
override. The second is round 25's own `crate::`-qualification fix one segment deeper,
and the merge is what made the full fix possible: `impl crate::traits::C for
super::Recovery { .. }`, naming a crate-root `mod traits { pub use core::clone::Clone
as C; }` two segments down, resolved to the bare, harmless-looking name `C` exactly the
way a bare `crate::C` used to, because round 25 closed only that bare, two-segment
shape — reasoning that a longer `crate::a::b::NAME` was another module's own real
declaration, and that failing closed on it broadly rejected `waymaker-embassy/src/
wiring.rs`'s own ordinary `use crate::dispatch::ActivityDispatcher;` when
`every_resolution` was still `future_trait_implementors`'s scan too. That sharing had
just ended in this same round's merge, so the reasoning no longer held: `wiring.rs` is
a file `recovery-surface`'s own scan never reaches, and nothing in `waymaker-flash`
names a trait or a derive through a multi-segment `crate::` path today.
`every_resolution` now fails closed on a `crate`-qualified path of any length, not only
the bare one.

Round 33 found three more, none of them in the alias-resolution machinery the merge had
just split apart. The first is in `module_tree` itself, one layer below the alias scan:
an unconditional `mod clone_impl;` whose resolved file opens with its own
`#![cfg(test)]` inner attribute is exactly as test-only as one the parent gated with
`#[cfg(test)] mod clone_impl;` — the attribute lands on the module the `mod` item names
either way, only spelled where the module's own file can carry it instead of where it
is declared — but `module_tree` classified a visited file from the *parent's* own
gating alone and never read the file's own top-level attribute, so a `Clone` impl or a
macro invocation that exists only under a file-level `#![cfg(test)]` was walked as
production-reachable and rejected code that never ships. A new
`crate::parse::crate_root_is_cfg_test_gated` parses a file and reads `has_cfg_test` on
its own `syn::File::attrs`, and `module_tree` ORs that into the gating a child inherits
alongside the parent's, so gating now compounds down the tree from either source. The
second is in `push_resolved_names`: `#[derive(MakeClone)] struct Recovery;`, where
`MakeClone` is a procedural derive macro, is legal Rust whose expansion this module
cannot see — a derive macro is not bound to generate an implementation only for the
trait its own name suggests, so it could expand to `impl Clone for Recovery` beside
whatever else it derives — and recording the resolved name literally let it through as
an ordinary, harmless-looking derive that simply is not `"Clone"`. Every name
`every_resolution` produces is now filtered through `DERIVABLE_BUILTIN_TRAITS`, the nine
traits `derive` can name without a third-party macro; anything else, `UNRESOLVED_DERIVE`
and `LOCAL_SHADOWED_TYPE` included, is recorded as `UNRESOLVED_DERIVE` and fails closed
the same way an alias this scan gave up chasing already does. The third closes the last
of four positions a macro invocation can occupy: `declares_item_macro` flagged one at
item, statement and type position, but never *expression* position, and `const _: () =
make_clone!();` is legal Rust whose macro sits there — a block is a legal expression and
Rust's block grammar admits item statements inside one, the same construct round 15
already found reaching an `impl` through a function body, so an arbitrary macro can
expand to `{ impl Clone for Recovery { .. }; }` and still type as `()`. A new
`visit_expr_macro` override flags one there too, but only when the invoked path is not
one of `is_known_safe_expression_macro`'s roughly thirty compiler-builtin or
standard-library macros — `assert!`, `matches!`, `write!` and the rest — whose expansion
is fixed and fully specified by the reference and never emits a freestanding item, since
`waymaker-flash` itself calls several of them in expression position throughout its own
production code and flagging every invocation there would reject the file this rule
exists to protect. All three were verified against real compilation: the first by
injecting a scratch `#[cfg(test)]`-gated child file with a real `Clone` impl into
`waymaker-flash`'s own `recovery` module and building it, the second and third by a
standalone two-crate `rustc` example each, since a real derive macro and a real
function-like macro cannot be added to `waymaker-flash` without an external dependency
the layering forbids.

Round 34 found two more, both in the exact hand-off round 33 had just drawn: what
`every_resolution` does when a path cannot be substituted rather than merely chased
through an alias table. The first is the absolute-path twin of round 32's own
`crate::`-qualification fix: `impl ::dep::C for Recovery { .. }`, where `extern crate
self as dep;` makes `dep` name this very crate and `pub use core::clone::Clone as C;`
sits at its root, is legal Rust that implements `Clone` for `Recovery` — but a leading
`::` had always been read as a signal to trust the last segment as a plain, unaliased
name, so `every_resolution` never consulted this file's own alias table even when that
table already had `C` bound to `Clone` right here. `every_resolution` now fails closed
on every absolute path, the same way a `crate`-qualified one already does, because an
absolute path reaches the extern prelude and this per-file scan has no crate-level view
of what a self-reference there might rename. The second is round 28's own alias-target
finding one hop later: `type Identity<T> = T; impl Clone for Identity<Recovery> { .. }`
implements `Clone` for `Recovery` itself, because substituting `Recovery` for `T` makes
`Identity<Recovery>` the type `Recovery` — but the self-type scan reads only the
segment identifier (`Identity`), discarding the generic argument that decides what the
substitution actually produces, so it followed `Identity` to its own declared target,
`T`, and reported an implementor named `T` rather than `Recovery`. `every_resolution`
now fails closed whenever a segment that carries a generic argument is also a locally
aliased name — excluding a `LOCAL_SHADOWED_TYPE` entry, which names a struct, enum or
union declared right in this file and so is never a substitution risk, only a real
generic type using its own real name; without that exclusion the fix would have
rejected every ordinary generic type declared and implemented in the same file, which a
negative test now holds open. Both were verified against real compilation:
`extern crate self as dep;` and the re-export it reaches, and the generic alias with
`Recovery` substituted in for it, were each injected into `waymaker-flash`'s own crate
root and `recovery` module and built, with `check-layering` catching both before the
injection was reverted — no external dependency was needed for either, unlike round
33's two macro findings.

Round 35 found two more, both against the fixes round 33 had just landed rather than
against the alias-resolution machinery rounds 32 and 34 touched. The first is in
`has_cfg_test` itself: it matched only the bare `#[cfg(test)]` spelling, so
`#![cfg(any(test))]` and `#![cfg(all(test, feature = "x"))]` — both guaranteed false
whenever `test` is, exactly as test-only as the bare form — fell through
`parse_args::<syn::Ident>()` unparsed and answered `false`, leaving a file gated either
way still walked as production-reachable. A new `meta_requires_test` recognizes both
compounds recursively — an `all(..)` naming `test` among its conjuncts can never hold
without it, and an `any(..)` every one of whose branches is itself test-only can only be
satisfied under test — while deliberately leaving `#[cfg(any(test, other))]` alone,
since it is satisfiable under `other` with no test anywhere and treating it as test-only
would hide production-reachable code from every one of `has_cfg_test`'s 72 call sites,
not only `recovery-surface`'s. The second is the gap round 33's own `visit_expr_macro`
left in place while closing the position it was written for: naming a macro as safe
vouches for nothing about its *arguments*, which `syn` never parses into structured
syntax at all — they are an opaque token stream — so `#[allow(non_local_definitions)]
const _: () = assert!({ impl Clone for super::Recovery { .. } true });` puts a real,
globally-applying `impl` inside `assert!`'s own condition, invisible to a visitor that
only ever asked whether the macro's *name* was one of the roughly thirty it trusts. A
new `token_stream_contains_a_brace_group` refuses the one thing every whitelisted
macro's grammar shares rather than reparsing each one's own — `assert!`, `matches!` and
`write!` each take a different shape, one of them a pattern rather than an expression at
all — since only a brace-delimited group can open a block and only a block can carry an
item statement, so a whitelisted macro's tokens are trusted only when they carry no
brace group anywhere, at any depth. Both were verified against real compilation: the
`any`/`all` compounds were each injected as a genuinely test-gated child file into
`waymaker-flash`'s own `recovery` module, built under both a production and a `cfg(test)`
configuration to confirm the impl really exists only under the second and that
`check-layering` correctly reports nothing for either — a negative result being the
point, since a false accusation of a legitimate test-only impl is exactly what round 33
introduced and this closes; and the `assert!`-hidden impl was injected unconditionally,
confirmed to compile in a plain production build, and confirmed caught by
`check-layering` before the injection was reverted.

Round 36 found two more, both on the very fixes round 35 had just landed, and neither in
`has_cfg_test` or the brace-group scan themselves. The first is a whitelist bypass a
name-only check could never close: `use crate::make_clone as assert; const _: () =
assert!();` is legal Rust whose `assert!` invocation is not `core::assert!` at all — a
local `use` rebinds a name in the macro namespace exactly as it would in the value or
type namespace — but `is_known_safe_expression_macro` matched the spelled name alone, so
a locally-imported macro wearing a whitelisted name walked past the one check meant to
catch an unexpandable one. Resolving *which* invocation a given `use` shadows would need
the same scope-stack machinery `resolve_segments` and `every_resolution` each carry for
their own callers, which this visitor does not have, so the fix is coarser on purpose: a
new `shadowed_expression_macro_names` collects every local name a `use` binds to one of
the thirty whitelisted names anywhere in the file — file scope, a nested module, or a
function body, since `use` is legal in all three — and poisons that name for the *whole*
file rather than only the scope the shadowing `use` sits in, which can only reject more
than a precise version would, never less. The second is the mirror image of round 32's
own statement-macro fix, one shape further: a macro used as a *tail* expression still
carries its own attributes on the `ExprMacro` node — `fn helper() { #[cfg(test)]
make_clone!() }` is legal Rust whose macro is removed from every non-test build exactly
like a gated statement already is — but `visit_expr_macro` read only the macro's path,
never `node.attrs`, so a test-gated expression-position invocation failed the whole file
closed over a macro that never ships. Both were verified against real compilation. The
shadowing case needed a macro genuinely reachable by path from outside the scanned file
to avoid also tripping the pre-existing, unconditional ban on a macro *declaration*
found in the same file: `#[macro_export] macro_rules! round36_make_clone` at the crate
root, aliased to `assert` and invoked from a `recovery` child file, was shown to produce
a real, globally-applying impl by declaring a second, ordinary `impl Clone for Recovery`
beside it and watching rustc refuse the conflict — the sharpest proof available that the
first one is real — before `check-layering` was confirmed to catch it and the injection
reverted. The tail-expression case reused the same cross-file macro, this time gated
`#[cfg(test)]` at the call site, confirmed to compile to nothing in a production build
and to a real impl under `cargo test`, with `check-layering` confirmed to report nothing
for either configuration.

Round 37 found two more, one in each half of round 36's own review. The first is round
32's own qualified-alias reach one hop further: `direct_scope_module_aliases` copied a
nested module's own alias *target* straight onto the qualified entry it registers for
the outer scope, which is right for an ordinary re-export and wrong for a chained one —
`mod traits { pub use core::clone::Clone as C; pub use self::C as D; } impl traits::D
for Recovery { .. }` names `self::C`, which is `C` in `traits`' own scope, exactly the
chained re-export `every_resolution`'s own doc comment already describes resolving
within a single file's alias table — but the qualified alias `traits::D` carried
`self::C` unresolved out to the *outer* scope, whose own table has only the qualified
`traits::C`, never the bare `C` `traits` would resolve it through, so the second hop
fell through to the harmless-looking last segment, `C`. The fix factors
`every_resolution`'s own hop-chasing BFS out of its path-specific pre-checks into a new
`resolve_segment_chain`, taking an already-extracted segment list rather than a
`syn::Path`, and `direct_scope_module_aliases` now runs a nested module's own alias
target through it — against that same nested scope, before qualifying — rather than
handing an unresolved reference to a table with no way to finish resolving it. The
second is `has_cfg_test`'s own outer filter one spelling further: `#![cfg_attr(not(test),
cfg(test))]` is exactly as test-only as a bare `#![cfg(test)]`, because rustc's rewrite
of a `cfg_attr` leaves nothing else it could mean — `cfg(test)` in every build where
`not(test)` holds (every non-test one, excluding the file) and no attribute at all in
every build where it does not (every test one, where the guarded `cfg(test)` would have
excluded nothing anyway) — but `has_cfg_test` read only an attribute whose own path was
`cfg`, so a `cfg_attr`-spelled equivalent never reached the predicate at all. A new
`attribute_requires_test` reads the whole attribute, `cfg_attr` included, recursing into
its own injected items; a new `meta_holds_without_test` is `meta_requires_test`'s dual —
guaranteed *true* whenever `test` is false, needed because a `cfg_attr`'s own condition
has to be shown to hold in exactly the builds its guarded `cfg` would have excluded, and
the two functions call each other for `not(..)`, the connective whose truth table is the
other's. Both were verified against real compilation: the chained export was injected as
a real, doubly-nested re-export chain in `waymaker-flash`'s own `recovery` module,
confirmed to compile (visibility warnings only) and confirmed caught by `check-layering`
before reverting; and the `cfg_attr` compound was injected the same way round 35's own
compounds were, confirmed by a duplicate-impl conflict to produce a real `Clone` impl
only under `cargo test`, and confirmed cleared by `check-layering` in both
configurations.

Round 38 found a gap in the scan's own reach rather than in its alias-chasing: a `Clone`
implementation for the pinned type need not live anywhere `recovery.rs`'s own `mod`
declarations reach at all. `impl Clone for crate::recovery::Recovery { .. }` in
`append.rs`, say, is legal Rust the crate root reaches directly through its own `pub mod
append;` and `recovery.rs` never reaches by any path — `check_recovery_is_not_clone`
walked the module tree rooted at `recovery.rs` itself, so the whole rest of the crate,
every sibling file `lib.rs` declares independently, was outside the scan regardless of
what it implemented. `waymaker-flash`'s single-writer invariant is a property of the
*crate*, not of one module's own descendants, and `Journal::after`'s by-value `Recovery`
is defeated exactly the same way from either. The fix walks from
`RECOVERY_ADAPTER_ROOT_PATH` — the crate root — instead, so the reachable set is every
production source `waymaker-flash` actually ships, `recovery.rs`'s own descendants
included, rather than only the latter. Resolving that walk needed a real bug in
`child_modules` fixed first: `mod` resolution had only ever been exercised from a
non-root file before, and a crate root resolves a `mod` declared in it exactly the way
`mod.rs` does — beside itself, never under a `lib/` subdirectory — which nothing had
ever taught the function, so a first attempt reported every top-level module of
`waymaker-flash` as unresolvable. `lib.rs` and `main.rs` are now recognized as crate
roots the same way `mod.rs` already was, verified with a scratch fixture before the
walk was ever pointed at them. What the wider reach does *not* do is loosen who counts
as a match: `every_resolution`'s existing fail-closed handling of a `crate::`-qualified
self type — any length, since round 32 — means a genuine `impl Clone for
crate::recovery::Recovery` fails closed to `UNRESOLVED_DERIVE` rather than resolving
cleanly to the pinned name, and an *unrelated* type's own `crate::`-qualified `Clone`
impl elsewhere in the crate would fail exactly the same conservative way — but an
unrelated type named the ordinary, relative way real code in this crate names its own
self-type does not, so the widened scan gains no new false positive against a
production `Clone` impl that never mentions `Recovery` at all. Every existing
`recovery-surface` fixture was single-file and had never needed a crate root beside it,
so all forty-odd of them needed a synthetic `lib.rs` declaring `pub mod recovery;` added
alongside — folded into the two shared helpers where a test used them, and by hand into
the half-dozen that built their own source lists directly. Verified against the real
crate by injecting `impl Clone for crate::recovery::Recovery<'_, u8> { fn clone(&self)
-> Self { unreachable!() } }` into `append.rs` — a file `recovery.rs`'s own tree never
reaches and the crate root reaches directly — confirmed to compile under
`cargo build -p waymaker-flash --no-default-features`, confirmed to be missed by
`check-layering` before this fix and caught by it after, and reverted cleanly.

Round 39 found three more, all on the same commit round 38 was found on. The first is
`KNOWN_SAFE_EXPRESSION_MACROS`'s own asymmetry: `include_str!` and `include_bytes!` can
only ever produce a string or byte-string literal, but `include!` splices the *named
file's own tokens* in as Rust source, and `token_stream_contains_a_brace_group`'s brace
scan reads only the invocation's own arguments — a single string literal for
`include!("clone.inc")`, with no brace anywhere in it. The file that string names is
never opened by this per-file scan, the same blind spot an out-of-line `mod name;` has,
so `const _: () = include!("clone.inc");` in a production-reachable file, where
`clone.inc` holds `{ impl Clone for Recovery { .. }; 0 }`, walked past the whitelist
unseen. `include` is no longer on it; no source in this workspace calls it bare in
expression position, so nothing accepted loses anything. The second is
`direct_scope_module_aliases`'s own qualification one shape further: `mod traits { mod
nested { pub use core::clone::Clone as C; } pub use nested::*; } impl traits::C for
Recovery { .. }` is legal Rust — `traits`' own glob re-exports `nested::C` as
`traits::C` — but the synthetic scope this function builds for a nested module is
assembled only from that module's own explicit aliases and its nested modules' own
*qualified* aliases, never from a glob any of them declares, so `traits::C` had nothing
registered to resolve against and fell through to the harmless-looking bare name `C`.
A glob anywhere in a nested module's own scope — its own direct glob, or one a deeper
nested module already reduced to its own qualified marker — is now re-registered one
level of qualification up as a `"name::*"` marker, and `resolve_segment_chain`'s own
"no matching alias" fallback checks every qualifying prefix of the segments it could
not otherwise resolve against that marker, not only the bare, unqualified case round 30
already covered. The third is `push_resolved_names`'s own trust boundary: `use
custom::Debug; #[derive(Debug)] struct Recovery;` is legal Rust whose `Debug` is not
`core::fmt::Debug` at all — an ordinary `use` shadows the prelude name exactly as a
`use .. as` rename would, and a third-party crate is free to name a procedural derive
macro `Debug` on purpose — but `every_resolution` chases the alias to
`["custom", "Debug"]`, finds no further alias for `custom`, and the same "take the last
segment" fallback reports the string `"Debug"` again, indistinguishable by name alone
from the literal builtin. `push_resolved_names` now also asks whether the derive path's
own first segment, as written, was ever the local side of an alias at all; if it was,
none of the eight non-`Clone` builtin names is trusted from that resolution, whatever
string the fallback produced. `Clone` stays exempt, because resolving *to* it through an
alias is round 13's own intended detection rather than something to distrust. All three
were verified against real compilation: the `include!` gap needed no separate included
file to demonstrate, since removing the name from the whitelist is what the invocation
alone now trips; the glob re-export was injected as a real nested-module chain reaching
`Recovery` in `waymaker-flash`'s own `recovery` module, confirmed to compile (visibility
warnings only) and confirmed caught by `check-layering` before reverting; and the
shadowed-builtin-derive finding, needing a real external proc-macro crate this
workspace's layering forbids adding, was verified with a standalone two-crate example — a
`#[proc_macro_derive(Debug)]` that emits `impl Clone for Recovery` — and shown live by
calling `.clone()` on the derived type and watching the macro's own `unreachable!()`
panic fire, the sharpest proof available that the import really does shadow the prelude
derive.

Round 40 found two more, both on the same commit round 39 was found on. The first is
`struct_derives`'s own narrow scope: it validates only the *pinned* type's own derive
list, in the one file that declares it, but a procedural derive macro is not obliged to
emit an implementation only for the trait its own name suggests, or only for the type
it is attached to — a derive macro receives the whole item as input and is free to emit
whatever tokens it likes, so `#[derive(Evil)] struct Helper;` anywhere in a
production-reachable file, naming a struct with nothing to do with `Recovery` at all,
can expand to `impl Clone for crate::recovery::Recovery` exactly as freely as a derive
on `Recovery` itself. A new `unresolved_derive_elsewhere` walks every other struct,
enum and union a file declares — at module scope and at any depth of inline-module
nesting, mirroring `collect_trait_implementors`'s own module-boundary alias threading —
and asks the same question `push_resolved_names` already asks of the pinned type's own
list, with the same fail-closed answer for a name this scan cannot vouch for. A
block-local struct, enum or union is a narrower residual this round leaves open, noted
in the function's own doc rather than chased here. The second is
`token_stream_hides_a_possible_item`'s (formerly `token_stream_contains_a_brace_group`)
own brace scan one macro deeper: `#[allow(non_local_definitions)] const _: () =
assert!(evil!());`, where `evil!` is an ordinary, unrecognized macro that expands to
`{ impl Clone for Recovery { .. } true }`, carries no brace anywhere in `assert!`'s own
tokens at all — only `evil`, `!` and an empty `(..)` group — because the brace the
check was built to catch sits one level down, in `evil!`'s own expansion, which is
exactly as opaque to `syn` as the outer, whitelisted macro's is. The function now also
refuses any further macro invocation nested in a whitelisted macro's own tokens, at any
depth: an identifier immediately followed by `!` and a delimited group, whichever
delimiter it uses. Both were verified against real compilation, and neither could be
demonstrated inside `waymaker-flash` itself without the same external-dependency
problem round 33 first ran into: the derive finding was shown with a standalone
two-crate example — a `#[proc_macro_derive(Evil)]` invoked on an unrelated `Helper`
that emits `impl Clone for Recovery` — confirmed live by calling `.clone()` on
`Recovery` and watching the macro's own `unreachable!()` fire; the nested-macro finding
was shown with a single-file `rustc` compile of `assert!(evil!())`, `evil!` declared
locally as an ordinary `macro_rules!`, confirmed live the same way.

Round 41 found a false *positive* rather than another way through: `trait Clone { fn
conjure() -> Self; } impl Clone for Recovery { .. }` is legal Rust whose `Clone` is a
local, unrelated trait — Rust resolves an unqualified name to the nearest declaration
in scope, and a trait declared right here shadows `core::clone::Clone` for every
unqualified reference inside this same scope exactly as a local struct, enum or union
already shadows an imported type (round 29's own finding). `shadow_aliases_for_local_
types` registered a self-referential shadow for the first three but never for a trait
declaration, so a bare `Clone` resolved as though no local declaration existed at all
and was rejected as implementing the real trait it plainly does not — the `super::`
self-type behind it then failed closed on its own unrelated grounds, compounding a
harmless file into a reported violation. `LOCAL_SHADOWED_TYPE` needed no new sentinel
and no change to `resolve_segment_chain`'s own handling of it: a trait declaration is
already in the *type* namespace a struct, enum or union name occupies, which is what
the sentinel's name has always meant, so this is a shadow declaration this function had
simply never asked about `Item::Trait`. A fully qualified `impl core::clone::Clone for
Recovery` sitting in the very same scope is unaffected, because a qualified path never
consults the bare-name shadow at all — the same way Rust's own resolution bypasses
local shadowing once a path is qualified. Verified against the real crate: the exact
local-trait-and-impl pair was injected into `waymaker-flash`'s own `recovery` module,
confirmed to compile, confirmed to be a false positive under `check-layering` before
this fix and cleared by it after.

Round 42 found the residual `unresolved_derive_elsewhere`'s own doc comment had named
when round 40 landed it: "a block-local struct, enum or union (declared inside a
function body) is not walked". `#[derive(Evil)] struct Helper;` inside a production
function reaches neither `declares_item_macro`'s trust of the outer `derive` attribute
nor that module-scoped walk, so a procedural derive on a block-local item can emit a
non-local `impl Clone for crate::recovery::Recovery` exactly as freely as one on a
struct declared at module scope. The fix shares rather than duplicates the block-descent
machinery `collect_trait_implementors_in_block` already carries for a handwritten
`impl`: the roots computation `collect_trait_implementors_in_item_body` used to inline —
every scope-root block a function's signature and body, a method, a trait's default
method, a const or static initializer, or a field, variant or generic bound's own type
can hide — is now `scope_root_blocks_of_item`, called by both the Clone-impl scan and a
new derive-checking twin, `any_unresolved_derive_in_item_body`. A new
`any_unresolved_derive_in_block` walks one block at a time exactly the way
`collect_trait_implementors_in_block` does, so a block-local `use` or `type` alias
resolves a nested derive's name under its own lexical scope rather than a sibling
block's, and `any_unresolved_derive_in_scope` calls it for every item its own loop
reaches — Struct, Enum and Union's own field types included, since those can bury a
further block the identical way a function body can. Verified against real
compilation: a standalone two-crate example, an `evil_macro` proc-macro crate whose
`#[derive(Evil)]` ignores the item it decorates and emits a hardcoded
`impl Clone for Recovery` instead, compiled cleanly with the derive placed on a
block-local struct inside an ordinary function — and, placed beside a second, explicit
`impl Clone for Recovery`, produced rustc's own `E0119` conflicting-implementation
error, the sharpest proof available that the injected impl is real. A real derive macro
cannot be added to `waymaker-flash` itself without an external dependency the layering
forbids, matching round 33's own two findings of this shape.

Round 43 found three more on the same commit, none of them in the block-descent
machinery round 42 had just landed. The first is in `has_cfg_test` itself, shared by
every rule in this file that reads whether an item is test-gated: rounds 35 through 37
widened a single attribute's own recursive predicate to recognize `#[cfg(any(test))]`,
`#[cfg(all(test, feature = "x"))]` and a `cfg_attr`-spelled equivalent, but every one of
those rounds still combined several attributes on one item with `.any()` — sound only
in the direction it was built for, since a single attribute proving an item test-only is
enough to prove the whole item test-only, but several `#[cfg(..)]` attributes on one
item are conjunctive, exactly like `all(..)`'s own arguments, and a per-attribute answer
cannot see a combination that is test-only only because two attributes *correlate*
through a flag neither one alone pins down. `#[cfg(any(test, feature = "x"))]
#[cfg(not(feature = "x"))]` is exactly that: read together the two admit only `test &&
!x`, which requires `test`, but neither attribute alone does. The fix replaces the
per-node recursive rule with `Cfg`, a small formula type every attribute's own
condition is parsed into, joined with `Cfg::All` across the whole attribute list, and
answered by `Cfg::requires_test` through *exhaustive enumeration* over the distinct
named flags the combination actually contains (capped at twenty, past which the scan
gives up rather than paying for `2^n` assignments) — a flag occurring twice, once under
`any` and once negated under a sibling attribute, is now the same variable held to the
same value in every assignment tried, which is what a recursive per-node rule
structurally cannot express no matter how many connectives it special-cases. The second
is `every_resolution`'s own hand-off one case further: `extern crate self as dep; pub
use core::clone::Clone as C;` at the crate root, reached from a sibling file with `impl
dep::C for crate::recovery::Recovery { .. }`, makes `dep::C` name `Clone` — but `dep` is
declared nowhere the sibling's own per-file alias table reads, the identical residual
round 34's absolute-path fix left open one shape narrower, so `dep::C` matched no alias
at all and fell through to trusting its own last segment, `C`. `resolve_segment_chain`
now fails closed on any qualified (multi-segment) path that matches no alias on its very
first hop — the path exactly as the source wrote it, before any local alias this scan
can see has had a chance to explain it — while a path already substituted through at
least one local alias keeps trusting its own last segment once no further one applies,
which is what lets an ordinary `use core::clone::Clone as C;` still resolve at all. The
third is `trait_implementors_for_pinned_type`'s own exemption for the pinned file:
round 29 skipped *every* top-level shadow there to keep `Recovery`'s own declaration
from shadowing itself, but that also dropped round 41's trait shadow for an unrelated
local declaration sharing the searched name — `recovery.rs` itself declaring `trait
Clone { .. }` beside `impl Clone for Recovery { .. }` implements only that local trait,
but with the whole top level unshadowed the bare `Clone` resolved past it to the real
`core::clone::Clone` and rejected code that never implements it. Only the entry named
`Recovery` is dropped from the pinned file's own shadow list now, so every other local
declaration there shadows the way round 29 and round 41 already say one must. All three
were verified against real compilation: the `cfg` conjunction against the full existing
test suite, which stayed green on every earlier round's own regression case under the
new exhaustive evaluator; the self-crate alias by injecting `extern crate self as
round43_dep; pub use core::clone::Clone as Round43C;` and a sibling `impl
round43_dep::Round43C for crate::recovery::Recovery<'_, u8> { .. }` into
`waymaker-flash` itself, confirmed to compile and confirmed caught by `check-layering`
before reverting; and the pinned-file shadow by a standalone `rustc` compile showing a
local `trait Clone` and its impl on an unrelated `Recovery` leave no real
`core::clone::Clone` impl behind (`r.clone()` fails to resolve at all), the same
underlying Rust fact round 41's own fix rests on.

Round 44 found one more on the same commit, and it turned out to already be closed:
`#[derive(custom::Debug)]`, where `custom` exports a procedural derive macro named
`Debug` that could emit anything, is legal Rust `push_resolved_names`'s own
`locally_rebound` check does not catch — that check asks only whether the derive path's
first segment was ever handed to a *local* alias, and an unaliased, directly qualified
path like `custom::Debug` never is, so round 39's fix (built for a *renamed* import
resolving to the same bare name) never applied. Before round 43's fix to
`resolve_segment_chain`, `custom::Debug` matched no alias at all and fell through to
trusting its own last segment, the harmless-looking string `"Debug"`, indistinguishable
from the real builtin. Round 43's fix already closes it from underneath, for the
identical reason it closes the self-crate-alias case above: an unaliased, qualified
path unresolved on its own first hop now resolves to `UNRESOLVED_DERIVE` rather than its
last segment, so `push_resolved_names` never reaches the builtin-name check with a
trustable string at all. Verified by neutralizing round 43's fix alone and confirming
the new regression test fails against the unpatched fallback, then passes once restored
— no code change beyond the test.

Round 45 found two more on the same commit, both genuine code changes this time. The
first is `push_resolved_names`'s own `Clone` exemption: `use evil::Clone; #[derive(Clone)]
struct Helper;` is round 39's exact bypass — an explicitly imported procedural derive
macro sharing a name with a real builtin — with `Clone` itself as the shadowed name
rather than `Debug`. The unconditional `name == "Clone"` clause trusted a resolution
of `Clone` regardless of `locally_rebound`, reasoning that resolving *to* `Clone`
through an alias was the intended detection round 13's own `Klon` test relies on — but
that reasoning proves too much: it also trusts a `Clone`-named import that was never
`core::clone::Clone` at all. `Clone` needs no exemption of its own — it is already the
first entry of `DERIVABLE_BUILTIN_TRAITS` — so removing the separate clause folds it
into the same `!locally_rebound` guard the other eight names already have. Round 13's
own `Klon` case (`locally_rebound` is `true` there) now reports `UNRESOLVED_DERIVE`
instead of the literal name `Clone`, which still names the pinned type's own violation
— the fail-closed message names `Clone` by text too — and still flags an unrelated
struct's derive through the same alias as worth a human's review. The second is the
reverse gap in `meta_is_unresolved_attribute_macro`: it read every injected attribute of
a `cfg_attr` without asking whether the `cfg_attr`'s own condition could hold in a
production build at all, so `#[cfg_attr(test, Evil)]` on an otherwise ordinary
production item — which only ever injects `Evil` under `cfg(test)`, with rustc removing
the whole attribute in every other build — was read as though the injected attribute
might apply in production and rejected valid, test-only instrumentation. `Cfg::
requires_test`, the same predicate round 43's `has_cfg_test` fix built, now decides
whether the `cfg_attr`'s own condition is provably test-only before recursing into what
it injects; a condition this scan cannot prove test-only still reads its injected
attributes exactly as before. Verified against real compilation throughout: the `Clone`
exemption with a standalone two-crate example — a `#[proc_macro_derive(Clone)]` that
emits a hardcoded `impl Clone for Recovery`, imported as `use evil::Clone;` and invoked
on an unrelated `Helper` — compiled cleanly, and conflicted (`E0119`) against an
explicit second `impl Clone for Recovery`, confirming the injected impl is real; the
`cfg_attr` fix by injecting `#[cfg_attr(test, round45_a_transform)]` into
`waymaker-flash` itself, confirmed caught by `check-layering` before the fix and cleared
by it after, reverted cleanly.

Round 46 found two more of the same shape round 45 had just closed for one caller, left
open in a second. The first: `collect_derive_names_from_meta` — the derive-list twin of
`meta_is_unresolved_attribute_macro`, and unrelated to it in code even though both walk a
`cfg_attr`'s own injected attributes — still recursed into every `derive(..)` a `cfg_attr`
injects regardless of the condition, so `#[cfg_attr(test, derive(Clone))]` on `Recovery`
read as an unconditional `Clone` derive and `recovery-surface` rejected a crate whose
production build never carries one at all. The fix is the identical check round 45 gave
the attribute-macro scan, given to the derive scan too: `Cfg::requires_test` on the
`cfg_attr`'s own first argument, skipping the recursion when it is provably true. The
second is sharper, in `declares_item_macro`'s own `MacroVisitor`: every gate it carries —
one per node reachable through an item, a member, a field, a variant or a foreign item —
asked `has_cfg_test` of that one node's own attributes alone, discarding what an
*enclosing* node's own `cfg` had already narrowed down. `#[cfg(any(test, feature = "x"))]
mod parent { #[cfg(not(feature = "x"))] fn helper() { evil!(); } }` can never include
`helper` in a non-test build — the two conditions correlate through the shared flag
exactly the way round 43's `has_cfg_test` fix closed for several attributes on *one*
item — but neither `parent`'s own condition (satisfiable under `feature = "x"` with no
test) nor `helper`'s (satisfiable under `!x` the same way) requires `test` alone, so a
visitor that reduced every level to its own separate boolean read past both and reached
`evil!()`. `MacroVisitor` gains `enclosing_cfg`, a `Cfg` accumulated with `Cfg::All` as
the walk descends through `visit_item`, `visit_impl_item`, `visit_trait_item`,
`visit_field`, `visit_variant` and `visit_foreign_item` and restored on the way back out;
`visit_stmt_macro` and `visit_expr_macro` ask the combination too, since a macro
statement's or expression's own gate is checked against everything that has to hold for
it to be *reached* rather than against its own attributes in isolation. `attrs_cfg` is
the `Cfg` half of `has_cfg_test` split out so a caller can combine it with an enclosing
scope's own formula before asking, which `has_cfg_test` alone — answering only a bare
`bool` — could not do. Both were verified against real compilation: the derive fix by
injecting `#[cfg_attr(test, derive(Clone))]` onto the real `Recovery` struct in
`waymaker-flash` itself, confirmed caught by `check-layering` before the fix (via the
regression test's own neutralization) and cleared by it after, reverted cleanly; the
visitor fix could not be injected into the real crate without either a genuinely
undefined macro breaking `check-layering`'s own `cargo build` step (round 42's pitfall)
or a `macro_rules!` declaration that would itself trip the unconditional item-macro check
regardless of the nested-cfg gating under test, so it was verified by neutralizing the
fix in place — dropping `enclosing_cfg` from the combination — and confirming the
regression test fails exactly at the correlated-conditions case while its positive twin
(two conditions that do not correlate) stays green throughout.

Round 47 was found merging this branch with `main` rather than by a further Codex review
round: `main` had independently gained issue #95's redelivery work in the meantime, and
`waymaker-flash/src/frame.rs`'s new `redeliverable_kind` calls `matches!(decoded,
RecordRef::EffectCompleted { .. } | RecordRef::EffectFailed { .. } |
RecordRef::TimerFired { .. })` — real, shipping production code that `check-layering`
failed on the merged tree. `matches!`'s second argument is a *pattern*, and
`token_stream_hides_a_possible_item` read the brace after each qualified variant name as
though it might open a block, exactly as it would for `assert!({ .. })`'s own condition —
but a pattern's own `{ .. }` can only ever hold the rest-pattern, never a statement,
because Rust's pattern grammar has no bare block anywhere in it. A brace group
immediately qualified by `path::` and containing nothing but `..` no longer counts as
hiding an item on its own; `is_opaque_variant_pattern_brace` is the two-part check —
requiring the `::` before the identifier rules out a keyword that really can open a block
directly (`loop {`, `unsafe {`), since no keyword can itself be a path segment before
`::`, and requiring the contents to be bare `..` rules out a named field, whose *value* —
in a struct literal, though never in a pattern — could still be an arbitrary block this
function has no business trusting. Closing only that qualified, `..`-only shape is
deliberate rather than an oversight: a bare, unqualified `Foo { .. }` and a named-field
pattern both still fail closed exactly as before, and
`a_matches_macro_over_an_unqualified_variant_pattern_is_still_rejected` is the regression
that pins the narrower residual open on purpose, beside
`a_matches_macro_over_a_qualified_variant_pattern_is_not_reported`'s positive case,
verified via neutralization to fail exactly where the fix is removed. The real fix was
verified against real compilation directly, since `frame.rs`'s own `redeliverable_kind`
is the live case: `check-layering` failed on the merged tree before this fix and passed
after it, with no injection or reversion needed because the finding was already sitting
in the tree the merge produced.

Issue #84 then closes a gap the second review round of issue #26 had only stated: four
modules refused storage that was "not the device this was validated against", and all four
decided it by comparing a `Geometry` — a description of a part number, which two chips of
one model share, so none of the four could tell two *instances* apart. `append`,
`recovery` and `swap` now borrow `storage` for the whole life of their protocol instead of
re-accepting it at every step: `Journal::stage`, `Recovery::new`/`with_integrity` and
`Swap::prepare` are the one call each protocol still takes a `storage` argument at, and
`Staged`, `Sealable`, `Recovery` and `swap`'s own `Prepared`/`Staged`/`Sealable`/`Installed`
all carry that borrow onward. A caller who wants to finish a record, a scan, or a swap on a
second device does not meet `AppendError::WrongDevice` or `SwapStepError::WrongDevice` at
run time — the call is not one the type lets a caller write, which a `compile_fail,E0061`
doctest in each module proves of the code as it stands. `capacity`'s `WrongDevice` stays a value
comparison rather than moving to a borrow, and correctly: neither of its two entry points
takes a `storage` argument at all, so there is no device instance there to bind, only a bank
size and a program granularity that two devices of one model are right to agree on. Every
`WrongDevice` variant's documentation now says which of the two shapes it is, and
[ADR 0044](docs/adr/0044-a-device-is-a-borrow-in-three-modules-and-a-value-in-a-fourth.md)
is where the choice is argued end to end.

Closing the gap by construction rather than by a wider comparison cost two new functions —
`Journal::after_taking_storage` and `Recovery::into_storage` — because a `Recovery` that no
longer hands `storage` back at every call still has to hand it back *once*, to the writer a
finished scan starts. Both are on `APPEND_SURFACE` and `RECOVERY_SURFACE`, both are linked
from `waymaker-size-probe`, and both are why `waymaker-drive`'s `Context` changed shape:
a struct cannot have one field borrow another field of the same instance, so once `Recovery`
holds the device itself, `Context` cannot hold `storage` as an independent field beside it.
`Source<'storage, S, C>` is the fix — the device lives in every state of it, `Scanning`,
`Writing`, `Spent`, and a `Taken` sentinel for the one `mem::replace` needs — so the boot has
exactly one owner of the device rather than two that would have to stay in step. The test
suites that drove the old per-call API — `waymaker-fault`'s crash sweeps,
`waymaker-rig`'s writers under test, and the size probe's linked calls — moved to the new
one; two of `waymaker-fault`'s writers needed a way to declare a record's operations after
the fact rather than live, since a borrowed `Sealable` now owns the device for the span they
used to bracket, which is `Session::operations` and `Session::mark_operations`. Nothing
about `Sealable` or `Staged` grew to make any of this easier: `commit-discipline` and
`swap-discipline` both still hold "the one type that may program a seal should do nothing
else," and a `storage_mut` accessor tried against both was rejected by the gate for exactly
that reason.

Issue #95 then closes most of §14 row 5's deviation, which issue #31's own table had left
open with no plan to close it: a torn completion left a journal with no append point, so the
bank was refused and the run's only way on was `continue_as_new` — a new run under a new
`RunId`, which is the duplicate `stable-redelivery` exists to forbid, on an effect that had
already physically run. No writer starts a record before the one ahead of it has sealed —
`waymaker-flash`'s own append discipline, unchanged since issue #24 — so an unsealed record's
reserved slot, its padded body and its commit seal, is the whole of what an interrupted
attempt ever touched. `Recovery::next` and `Scan::next` both now look: if every byte between
the frame's own unpadded length and the end of that slot is erased, the record is ignored —
never yielded — and the scan carries on scanning *past* the slot rather than stopping there,
which is what lets a *later* boot's own committed history be found rather than hidden behind
a slot no scan ever gets past. The bounded check is deliberate and is not the same question as
"is the rest of the region erased": a reader walked at a wider granularity than a journal was
written at computes a slot that runs past real, committed records, and checking only the
unpadded frame's own bytes is what still refuses that case rather than silently accepting a
miscomputed slot that happens to land on erased media further out —
`a_scan_at_a_larger_alignment_than_the_writer_used_is_caught_by_the_seal` is the regression
that found the wider check wrong before this one replaced it. A tear *inside* the commit seal
itself is unaffected: those bytes are neither erased nor a real seal, so recovery still cannot
tell an interrupted append from damage, and the bank is still refused exactly as
[ADR 0018](docs/adr/0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md)
says of media nothing legitimate should be in. `waymaker-drive`'s and `waymaker-rig`'s own
row-5 tests sweep both outcomes rather than only the one that used to hold.
The fix is scoped to an outcome rather than generic over every record kind — the first
version of this issue let any unsealed frame be ignored, and Codex's review of it found the
scope was the mistake. `frame::redeliverable_kind` answers `true` for exactly
`EffectCompleted`, `EffectFailed` and `TimerFired`, read from the same decode `sealed` already
needs to check the seal, so neither `Recovery` nor `Scan` pays for a second call to
`frame::decode_with::<C>` to ask. Every other kind — `RunStarted`, a schedule, a version
marker, a terminal record — still ends the scan as `Ending::Unsealed` when its seal does not
hold, exactly as it always did: losing a schedule is free by row 5's own argument (an
unsealed schedule's effect was never dispatched either way), but losing a *terminal* record
this way is not, and neither is losing `RunStarted` in general — see the capacity paragraph
below for why. `Recovery::next`'s one call to `frame::decode_with::<C>` is kept in a private
`sealed` helper for the `integrity-check` routing pin's sake, and the loop this needed is
bounded the same way every offset advance in this module already is — by `stride > 0` — so a
chain of ignored slots from repeated crashes still terminates over a region of finite length.
Costs 100 B of layers for the recovery fix and 44 B more for `redeliverable_kind`, 12964 B of
13312 with 348 B left and no raise asked for; runtime RAM and kernel state are unmoved,
because nothing here grows what `Recovery` carries between calls. `waymaker-spec`'s ghost
model is untouched: it already treats the two-barrier write as coarser than this — see
[what is not checked](#what-is-not-checked)'s note that the model "has no transition for the
state §07's payload barrier creates" — so this is a fact about bytes the model was never
fine-grained enough to see change.

Redelivering an outcome in place moved a cost that used to be absorbed by starting a fresh
run: the bytes a torn, ignored attempt consumes cannot be reclaimed on NOR, and §10's capacity
reserve priced a schedule against only the *real* outcome that would eventually land, not
against one that might be wasted first. `Reserve::exit_bytes_after`'s
`EffectScheduled`/`TimerScheduled` arm and `Reserve::for_layout`'s floor both now reserve one
extra outcome's worth — `redelivery_slack` — so a single tear at the reserve boundary cannot
strand the run: without it, dispatch happens on `Ending::Clean`, before capacity is ever
checked, so the activity would be redelivered on every later boot while the retry that has to
record its outcome refused with `NearCapacity` forever. One wasted attempt is tolerated, not
an unbounded number, matching this codebase's other single-crash guarantees;
`a_torn_outcome_at_the_reserve_boundary_still_leaves_room_for_the_retry` drives exactly that
shape end to end. Codex found this on review of the fix above, and then found the same shape
a second time against a *terminal* record: nothing prices a terminal's own retry, so treating
it as redeliverable the same way could strand a run's only exit for ever. Widening the
reserve a second time, for a kind whose own retry-safety turns out to depend on the relative
sizes of a workflow's declared `Bounds` — `RunStarted`'s in particular, since
`run_input_bytes` can dwarf everything else `Reserve::for_layout` prices — is what
`redeliverable_kind`'s narrower scope avoids rather than chases. A third finding on the same
mechanism is filed rather than fixed here: `redelivery_slack` is unversioned across a
firmware upgrade, since `Reserve` is recomputed fresh from `Bounds` on every boot and nothing
about it is on media, so a schedule admitted by firmware that predates this fix carries no
record of the weaker guarantee it left behind — see issue
[#188](https://github.com/madmax983/waymaker/issues/188), filed rather than fixed because no
device has ever run this firmware to make the scenario reachable today. See
[ADR 0052](docs/adr/0052-a-torn-record-redelivers-when-its-reserved-slot-is-clean.md).

Issue #99 then closes the route Codex found on issue #32's fourth review round. A
`pub const BEST_EFFORT: Self = Self::AfterBoot { ticks: 0 }` on `impl TimerSpec`, reached
through an aliased `use`, named no forbidden identifier and changed no surface. So
`timer-capability` passed a persistent deadline served by a clock that restarts on reset.
`TimerSpec`, `Timer` and `ClockCapability` may now declare no associated constant at all.
The façade's construction pin resolves `use` aliases before it scans, so the alias is seen
for what it names rather than dropped as an import. `ClockKind` is a `u8` newtype with no
member and no method for either pin to read. Its two values — the byte a `TimerScheduled`
record carries — are pinned on their own, by name and value (`source::CLOCK_KIND_CONSTANTS`).
A renumbering is now a build failure rather than a round-trip test that stays green.
`effect-protocol` and `kernel-boundary` shared the same blind spot — an associated constant
is neither a function nor a member — and gained the same ban for their own pinned types.

Issue #97 closes a gap Codex found in the `failure-matrix` rule itself: a row test under
`#[cfg_attr(.., ignore)]` is a test the compiler can skip, and the old scan refused only a
direct `#[ignore]` or `#[cfg(..)]`, so such a test still vouched for its row. The scanner's
own rewrite for issue #51 had already closed this — `crate::parse::declares_test` reads a
test function's own attributes through `syn` and refuses `#[cfg_attr(..)]` the same way —
but no regression test drove that specific attribute through `failure-matrix`'s own check,
and `book`'s matching scanner, `#[cfg_attr(..)]`-aware since issue #42, had the same untested
gap. `an_ignored_or_compiled_out_test_does_not_vouch_for_its_row` now does.

`book`'s own scanner turned out to need more than a test. It was a hand-written line scan —
collect the lines above a `fn name(` that look like attributes, reset on anything that does
not, refuse if `#[ignore]`, `#[cfg(..)]` or `#[cfg_attr(..)]` is one of them by *prefix* —
and across four Codex review rounds on this PR it lost every one of those four ways: a
spelling with extra whitespace or a raw-identifier marker never matched the prefix; two
attributes sharing one line hid the second behind the first a patched version checked; an
attribute spanning several lines lost its own continuation to the "reset on anything that
does not look like an attribute" rule; and once that was patched with a bracket count, a
delimiter character inside a string literal — `doc = ")]"` — closed the count early and let
the same multi-line trick back in. Four patches to one heuristic is four attempts to
reimplement enough of Rust's grammar to answer "is this really `#[cfg_attr(..)]`" by hand,
which is the mistake: `crate::parse::declares_test` never had any of these four bugs, because
a real parser has no such thing as a line or a bracket count.

`book`'s `declares_test` now asks `syn` the same way: [`crate::parse::fns_matching`] — the
same structural lookup the `failure-matrix` scanner already uses, made `pub(crate)` for this
— finds the function by name and hands back its real attributes, and every attribute is
checked regardless of order, line breaks, whitespace, a raw-identifier marker, or what a
string literal inside it happens to contain.

Codex found a fifth bug in the line index the caller still needs, to check the test sits
inside its anchor: it was found by a second, independent plain-text search, kept apart from
the attribute check on the theory that position is a *shape* question and skippability is a
*does this run* question. Two independent searches for "the same" declaration can each answer
about a different one when a name is declared twice — a real, running
`#[test] pub fn a_first_sample()` declared earlier in the file (found first by `syn`, since it
does not care about visibility) paired its own passing attributes with the position of a
*later*, non-test `fn a_first_sample()` the text search found instead (since `pub` does not
match a search for a bare `"fn a_first_sample("` prefix) — and that later declaration is the
one actually sitting inside the anchor. The anchor passed while showing untested content.
`crate::parse::NamedFn` now carries `line`, the 1-indexed source line of the exact function
whose attributes were just checked, read off that function's own `syn` span rather than
re-found by a second search; `proc-macro2`'s `span-locations` feature is what makes a span
carry a real line outside an actual proc-macro.

Codex found a sixth bug, the mirror image of the fifth, on the very next round: fixing "the
first declaration found can be the wrong one" by taking `fns_matching`'s first match still
takes *a* first match — of every declaration of `name` in the file, not of the ones that are
actually candidates for *this* anchor. A plain helper `fn a_first_sample()` declared earlier
in the file, not a test at all, made `declares_test` stop there and report "declares no
test" for an anchor whose own content was a real, running `#[test]` — the old line scanner
had tolerated exactly this by continuing past a same-named non-test, and the structural
rewrite lost it. `declares_test` now takes the anchor's own line range as a third argument
and prefers, among every candidate `fns_matching` finds, the one whose line falls inside it;
only when none do is the first candidate taken, which is what keeps the "declared, but
outside the anchor" report for a file with exactly one declaration. This one change closes
both the fifth bug and the sixth by the same construction — preferring the in-anchor
candidate answers "is the anchor's own declaration a real test" directly, rather than "does
some declaration of this name run," which is a different question in each direction once a
name can be declared more than once.

Round 7 found two more, both in the sixth's own fix. The first is the sixth's mistake one
level in: preferring the first *in-anchor* candidate is still "the first match," now scoped
to a smaller pool rather than answered. Two same-named declarations can both sit inside one
anchor — an ordinary helper in one nested module, a real `#[test]` in another — and the
first one is not necessarily the qualifying one. `declares_test` now tries every in-anchor
candidate in turn (widening to every declaration in the file only when none sit in the
anchor at all) and takes the first that actually qualifies, via a `verdict` helper the
per-candidate check was pulled into.

The second is sharper: [`crate::parse::NamedFn::line`] read the function's *identifier*
span, and a line comment between the `fn` keyword and the name — legal Rust, since a comment
is whitespace to the lexer — can put the keyword outside an anchor whose line range still
contains the identifier. mdBook would then render the fragment starting after `fn`, which is
not the tested function the check claims to have found. `line` now reads
`Signature::fn_token`'s own span instead, the start of the item rather than the start of its
name.

The parametrized test grew eleven cases across the first four rounds, one or more per bug
found, and every earlier one still passes unmodified against each fix in turn. Rounds five
through seven were not spellings any single attribute check could see — each was a mismatch
between which declaration answered and which one the anchor actually meant — so
`a_real_test_declared_elsewhere_cannot_vouch_for_a_decoy_of_the_same_name`,
`a_non_test_declared_elsewhere_cannot_block_the_real_test_in_the_anchor`,
`a_qualifying_test_is_found_even_behind_a_non_test_inside_the_same_anchor` and
`an_anchor_marker_between_fn_and_the_name_does_not_count_as_containing_the_test` each stand
beside that parametrized test rather than inside it.

Round eight found an eighth: `NamedFn::line` is still only where the item *starts*, and
nothing checks where it *ends*, so an anchor whose own end marker sits between the `fn`
keyword and the identifier "contains" a function that is, on the rendered page, the single
word `fn`. Left open rather than fixed here — by round eight the construction needed to
show it is an anchor's own end marker planted inside a function signature, which is the
kind of input this design has never claimed to survive: this function's whole positional
check exists for authors, not adversaries. Tracked as issue
[#165](https://github.com/madmax983/waymaker/issues/165) instead of a ninth round on this
one. No new ADR: nothing here moves a must-not-own cell, a dependency edge, or a rule id.

Issue #92 then closes a gap Codex found on the fourth review round of issue #91, past that
change's review budget: `DurableIntent` proved *an* effect was committed, not *which* one.
`Activities::perform` took the kind and the input as free arguments beside it. So a caller
could dispatch one effect's identity under another effect's kind or input. Nothing in
`waymaker-drive`'s own path could do this: `decide` builds its dispatch call from the same
locals it built the request from. But a second caller at rung 0.4 would have had no such
accident to rely on. `DurableIntent` now carries the `EffectRequest` step 3 committed;
`DurableIntent::kind` reads it, so `Activities::perform` drops the free `kind` argument —
there is no second place to read one from. `Dispatchable::perform` is the one route from a
proof and raw bytes to a dispatch, and it checks the input's length and digest against that
request before `Activities::perform` is ever called; a mismatch is `InputMismatch`, and the
world is not asked. Codex found three more gaps in review, one round apart each. The first
version left `Activities::perform` taking a bare `&[u8]`, so a caller could still skip the
check by calling `activities.perform(intent, wrong_bytes, out)` directly. Wrapping the bytes
in a `CheckedInput` — a type with a private field that only `Dispatchable::perform` could
build — closed that, but left the identity a second, separate argument, so an adapter
forwarding to another `Activities` implementor could still pair one effect's `DurableIntent`
with a different effect's `CheckedInput` and produce a value that was individually valid in
each field and wrong as a pair. `Activities::perform` now takes one `CheckedDispatch`,
binding the identity and the checked bytes into a single value `Dispatchable::perform` builds
atomically — there is no second value left for an adapter to swap in. That closed the field,
but not the file: `EFFECT_NO_SELF_LITERAL` refuses a `Self` literal inside `CheckedDispatch`'s
own `impl` and a trait built for it, and said nothing about a `pub(crate)` sibling function
elsewhere in `effect.rs` naming the type directly and pairing its own fields by hand — the
third gap, closed by `source::CHECKED_DISPATCH_CONSTRUCTION`, `EFFECT_CONSTRUCTIONS`'s twin
for a type with one legitimate origin: `Dispatchable::perform`, and nowhere else in the file.
Codex found a fourth gap in that pin on the round after: `struct_literal_counts` resolved
only `use` aliases, so `type Unchecked<'a> = CheckedDispatch<'a>;` followed by a literal
spelled `Unchecked { .. }` built the type under a name the pin never compared against —
`Self`-literal and construction-site refusals alike name `"CheckedDispatch"`, never the local
alias a caller wrote. `xtask::parse::struct_literal_counts` now resolves `type` aliases the
same way it already resolved `use` aliases (issue #99), chased through a chain of either
kind, so `type A = B; type B = CheckedDispatch;` still reaches `CheckedDispatch` from a
literal spelled `A { .. }`. The fix lives in the shared scanner rather than in this rule
alone, so every other construction pin built on `struct_literal_counts` —
`EFFECT_CONSTRUCTIONS` included — closed the same gap in the same change.
Codex found a fifth gap in the scanner itself, in the round right after: it walked file
items and inline modules only, so a `type` alias declared *inside* a helper's own function
body — legal Rust, and invisible to a scan built for module-level declarations — evaded it
exactly as the file-scoped one had. `struct_literal_counts` now gives every block its own
alias scope: entering a block collects the `use` and `type` aliases declared directly in its
own statements, a name resolves against the innermost scope that declares it — so a local
alias correctly shadows a same-named one declared elsewhere in the file, the same rule a real
compiler resolves a name under, rather than the scan picking whichever declaration a flat
list happened to sort first — and that scope is popped on the way back out. The same
block-scoped resolution is what every construction pin built on `struct_literal_counts` now
gets, not a change scoped to this one pin.
Codex found a sixth gap the round after that, and it is a hole in what counts as an alias's
right-hand side rather than in where one is looked for: `type Unchecked<'a> =
(CheckedDispatch<'a>);` is valid Rust — `#[allow(unused_parens)]` lets it through
`-D warnings` — and `syn` keeps the parens as their own `Type::Paren` node rather than
discarding them, so the `Type::Path` match that reads a type alias's target saw nothing
there and built no alias at all. `type_alias_target` now unwraps `Type::Paren`, and
`Type::Group` beside it for the same reason — a macro's own hygiene grouping is the same
shape — recursively, so `((CheckedDispatch))` reaches the same target in two hops.
A seventh round found a gap in a different shape from all six before it: not a way to
*build* a forged value, but a way to *rewrite* a legitimate one's field in place.
`CheckedDispatch`'s, `DurableIntent`'s and `Dispatchable`'s fields are private to the
*module* `effect.rs` declares, not to the type — Rust has no finer grain — so a sibling
function anywhere in that module can already write `dispatch.bytes = other;` on an
otherwise-legitimate value, with no struct literal anywhere for a construction pin to count.
A `&mut` reference taken to the field is the same capability under a second spelling:
`core::mem::swap`, `core::mem::replace`, and passing the reference to an arbitrary
`&mut`-taking function all rewrite the field with no `=` in the source at all.
`source::EFFECT_PROOF_FIELDS` names the fields this matters for — `id` and `request` from
`DurableIntent`, `intent` from `Dispatchable` and `CheckedDispatch` (spelled once, since both
name it the same way), and `bytes` from `CheckedDispatch` — and
`check_effect_proof_fields_are_not_rebound` refuses both an assignment and a `&mut`
reference to any of them, anywhere in the file. `Effect`'s and `Dispatchable`'s `writer` field is deliberately left off: it
carries no identity, kind or byte binding, so rewriting it is not the guarantee this list
exists for. Nesting `CheckedDispatch` in a private submodule of its own — the usual way Rust
narrows field visibility below module scope — was considered and rejected: `effect-protocol`
already refuses a module declared anywhere in this file, precisely so a construction site
cannot hide from a scan that reads `effect.rs` as one flat file, and a submodule added for
this reason would open exactly the hole that refusal exists to close. Costs nothing measured:
the check is entirely on the `xtask` side, over `effect.rs`'s own text, and touches no line
the shipped crate builds — the gated `layers` figure stays at 12732 B.
An eighth round found the third route the first two left open, and it needs neither `=` nor
`&mut` written anywhere: `dispatch.bytes.clone_from(&other)` reassigns `bytes` through an
*implicit* `&mut self` autoref, the method-call form every mutating trait method — `Clone`'s
included — takes. Whether a given method really takes `&mut self` is a question `syn` cannot
answer without type inference, so `mutated_field_names` refuses every method call whose
receiver is a guarded field, not only the ones a reviewer could confirm mutate — over-broad
the same way every other scanner in this workspace already is, and free here because no
method is ever legitimately called directly on one of these fields today, only on the whole
value through its own accessor, whose receiver is a plain path rather than a field access.
A ninth round found two more gaps, in two different mechanisms. The first is a fourth route
into the field-rebinding problem the seventh and eighth rounds closed:
`let CheckedDispatch { bytes: ref mut slot, .. } = dispatch;` borrows `bytes` mutably
through the pattern itself, with no assignment, no `&mut` expression and no method call
anywhere for the first three routes to see. `mutated_field_names` now also refuses a
`ref mut` binding on a guarded field in any struct pattern, found at any nesting depth by
walking the sub-pattern with a nested visitor rather than checking only its outermost shape.
A field bound `mut slot` with no `ref` stays legal: it moves or copies the value into a
fresh local, which is a read, and reconstructing `CheckedDispatch` from that local
afterward is a struct literal the construction pins already cover. The second is in the
type-alias resolution itself: `type Unchecked = <Via as Alias>::Dispatch;` is a qualified
associated-type projection, and `type_alias_target` explicitly skips every `Type::Path`
with a `qself` — correctly, since resolving what a trait's `impl` names as its associated
type needs type inference this scanner does not have. Skipping was silently permissive,
though: the alias built nothing, so it counted as nothing, while the projection itself
could still name `CheckedDispatch`. Unlike a tuple, a reference or a trait object — none of
which can ever appear where `Name { .. }` construction syntax is legal — a projection
genuinely can resolve to a struct usable that way, so `qself_type_alias_names` reports every
such alias, and a new check refuses the file outright over it: a hard refusal of the
construct, the same shape as `effect-protocol`'s ban on a module declared anywhere in this
file, rather than an attempt at the type resolution neither `syn` nor this scanner can
safely do.
A tenth round found that `mutated_field_names` itself checked only the outermost field of a
chain: `dispatch.intent.request.kind = x;` assigns to `kind`, which is not a guarded name,
but `intent` and `request` are both guarded *ancestors* in the same chain, and rewriting
through either reaches the identity or the kind the whole family of checks exists to
protect. `note` now walks the full chain of field accesses back to its root rather than only
its leaf, for assignment, `&mut` reference and method call alike, since all three share the
one helper. Two further findings from that round are real and not fixed here: that Rust's
match ergonomics can bind a struct pattern's field to a mutable alias with no `ref`, `mut`
or `&mut` written anywhere, purely from the scrutinee's own reference-ness, which `syn`
cannot see; and that a generic type alias with a trait bound
(`type Unchecked<T: Alias> = T::Dispatch;`) is an associated-type projection with no `qself`
for the ninth round's check to key on. Ten review rounds deep, both would need genuinely new
detection machinery rather than a completion of what already exists, and this project's own
review-depth guidance is to stop iterating past two or three rounds and open an issue once a
fourth round is still finding real bugs rather than continue an unbounded loop — so they are
tracked in issue [#171](https://github.com/madmax983/waymaker/issues/171) instead of fixed
in this change.
`Effect::redelivering` threads the same binding through with no kernel-boundary change:
`decide` already checks the workflow's current request against history with
`ReplayMachine::intent` before the redelivery row is reached, so the request `redelivering`
binds is the one the kernel already vouched for — issue #92's own guess that this would need
`Resolve::Redeliver` widened did not hold up once the call site was read. `effect-protocol`
pins all four new methods (`DurableIntent::kind`, `Dispatchable::perform`,
`CheckedDispatch::bytes` and `CheckedDispatch::durable_intent`) on `EFFECT_PROTOCOL_SURFACE`
and `EFFECT_TYPE_METHODS`; none is one of §07's storage steps, so `EFFECT_STEP_BODIES` is
unchanged. The gated `layers` figure moves to **12732 B** of 13312, 88 B below ADR 0036's
12820 B and unmoved again by
`CheckedDispatch`, which the optimiser erases — the wider `DurableIntent` and the new checked
call cost less than the free `kind` argument they replace, on this optimiser, at this
setting; no raise is asked for. See
[ADR 0050](docs/adr/0050-a-durable-intent-carries-its-request-and-perform-checks-it.md).

An eleventh round, found on review of this branch's own merge with a concurrent one, closed
a gap in the tenth's fix rather than opening a new one. `note`'s chain walk covers a
compound assignment once reached — `dispatch.intent.request.kind ^= 1;` still names `kind`
at the end of the same chain `x.field = value;` does — but nothing called `note` for it:
`visit_expr_assign` was the only route in, and `syn` parses every compound-assignment
operator, `^=` and the other nine, as a `BinOp` on an `Expr::Binary` rather than as an
`Expr::Assign`, which is `=` alone. `mutated_field_names` now also visits `Expr::Binary`
and calls `note` on the left operand for any of the ten assignment operators, leaving an
ordinary binary expression — which reads a field and rewrites nothing — untouched.

A twelfth round found a gap in the eleventh's own review, not its fix.
`(dispatch.bytes,) = (replacement,);` is still an `Expr::Assign` — `note` is still called on
its left side — but that left side is `Expr::Tuple`, not `Expr::Field`, and `note`'s chain
walk only recognised the latter, doing nothing at all otherwise. A field buried inside a
tuple, array or struct-literal destructuring target was invisible the same way a
compound-assignment target had been. `note` now recurses into each element of a tuple or
array and each field's value in a struct literal — arbitrarily nested, since a tuple can
hold another tuple — before falling back to the field-chain walk.

A thirteenth round found a gap in the tenth round's own chain walk, rather than in the
eleventh's or twelfth's own fixes: `(dispatch.intent.request).kind = x;` is still a plain
field assignment, but a parenthesized ancestor sits in the middle of the chain — the outer
field's `.base` is an `Expr::Paren`, not the `Expr::Field` the tenth round's `while let`
loop matched, so it stopped there and never saw `intent` or `request` inside the parens.
The loop now unwraps `Expr::Paren` and `Expr::Group` as it descends, the same two wrappers
`type_alias_target` already unwraps for the same shape of reason (the sixth round, above),
so `((dispatch.intent).request).kind = x;` still reports both ancestors through two hops of
nesting. This is the third round of Codex findings in this file's own post-merge review of
commit 16c7135, and this project's own review-depth guidance is to stop iterating past two
or three rounds and open an issue once a fourth still finds real bugs — so a fourth finding
of this shape is issue [#171](https://github.com/madmax983/waymaker/issues/171)'s rather
than a fourteenth round here.

Review of the merge itself found a separate bug, in code the merge introduced rather than in
`mutated_field_names`'s own chain above: `resolve_local_alias_chain` — written to keep issue
#92's function-local type-alias resolution working after issue #169's rewrite of
`struct_literal_counts` onto a stack of raw `&[syn::Item]` slices — resolved a block's own
aliases by calling `collect_item_aliases`, which recurses into any `mod` the block declares.
A block that declares `type S = Foo;` directly and also a `mod hidden { type S = Bar; }`
alongside it has `hidden`'s own `S` collected into the same flat list as the block's own, so
a bare `S {}` outside `hidden` could resolve to `hidden::S` — a scope `real` Rust never lets
it see — rather than to the block's own alias, exactly the leak `own_aliases`'s own doc
comment already states module-level lookups must not have. `own_aliases` is generalized to
take any `&syn::Item` iterator rather than only a `&[syn::Item]` slice, and
`resolve_local_alias_chain` now calls it instead of `collect_item_aliases` — the same
non-recursive, own-level-only collection a module lookup already gets, reused rather than
reimplemented. `a_nested_modules_alias_does_not_leak_into_the_enclosing_blocks_lookup` is
the regression, confirmed RED against the unpatched lookup.

Review of that fix found the inverse leak in the same round: `visit_item_mod` pushed the
nested module's own items onto `self.stack` and popped them afterward, but never touched
`self.block_items`, so a block's own local aliases stayed visible while the visitor
traversed a `mod` declared directly inside that block — a scope a nested module never
inherits, whether the enclosing scope is a function body or another module. A block
declaring `type S = Foo;` and, alongside it, `mod hidden { pub struct S; fn make() -> S { S
{} } }` had `hidden::make`'s own `S {}` incorrectly resolved through the outer block's alias
to `Foo`, when real Rust never lets `hidden` see that alias at all. `visit_item_mod` now
sets `block_items` aside with `core::mem::take` before descending into the module and
restores it on the way back out, the same discipline `self.stack`'s own push/pop already
has. `a_blocks_local_alias_does_not_leak_into_a_nested_module` is the regression, confirmed
RED against the unpatched visitor.

Codex then found a gap in a different mechanism the same round: `syn::Visit` never descends
into a `macro_rules!` body, since it is an opaque token stream to a syntax-only scan, so a
local macro defined and invoked inside `effect.rs` and expanding to
`CheckedDispatch { intent, bytes }` builds the pinned type at a site `struct_literal_counts`
and every check built on it cannot see, whatever the alias scanner's own scope discipline is.
Expanding or inspecting a macro body was rejected for the same reason resolving a qualified
associated-type projection already was — it needs machinery this scanner does not have — so
`check_effect_types` now refuses `effect.rs` outright over a bare `macro_rules` identifier,
the same construct `ctx-facade` already refuses in its own two pinned files for the identical
reason. `a_macro_rules_in_the_effect_protocol_file_is_reported` is the regression, confirmed
RED against the unpatched rule.

Codex found, in the same round, the gap that ban left open: it reads the *definition's own*
identifier, so an invocation of a macro defined anywhere else in the crate —
`emit!(CheckedDispatch { intent, bytes })`, spelling `macro_rules` nowhere at all — reaches
the identical opaque-token construction site a local definition does. `syn::Macro` is the one
type every invocation shares, whether it sits in item, statement, expression, type or pattern
position, so a new function, `crate::parse::invokes_any_macro`, overrides `syn::Visit`'s
single `visit_macro` method instead of naming `macro_rules` as text — catching every
invocation shape, `macro_rules!` included, in one mechanism rather than five. `effect.rs` now
refuses the file outright over any macro use at all, outside `#[cfg(test)]`, which is
strictly stronger than the identifier ban it replaces and closes the same class of gap this
file's own "what is not checked" bullet on a macro-generated `fn` used to record.
`a_macro_invocation_in_the_effect_protocol_file_is_reported` is the regression, confirmed RED
against the identifier-only check.

A third round in the same family found the shape neither of the first two catches: an
attribute macro or a custom derive is a `syn::Attribute`, not a `syn::Macro` invocation, so
`invokes_any_macro`'s `visit_macro` override never sees `#[forge]` on a method or
`#[derive(Forge)]` on a struct — each expands in its own defining crate with nothing here
able to read what comes out. `crate::parse::unaudited_attributes` closes it the same way:
every attribute in `effect.rs` is required to be one of a fixed set the compiler itself
interprets with no macro behind it (`source::EFFECT_ALLOWED_ATTRIBUTES`), and a
`#[derive(..)]` to name only the compiler's own derives (`source::EFFECT_ALLOWED_DERIVES`),
each name in the list checked on its own since one attribute can mix an inert derive with a
custom one. `cfg_attr` is refused outright, since it can emit an arbitrary attribute and
`effect.rs` has no legitimate use for one. Three rounds deep in this macro-opacity family —
this project's own review-depth guidance is to stop past two or three and open an issue once
a fourth still finds real bugs — so a fourth finding of this shape goes to a new issue rather
than a fourth round here.
`a_procedural_attribute_in_the_effect_protocol_file_is_reported` and
`a_custom_derive_in_the_effect_protocol_file_is_reported` are the regressions, confirmed RED
against the unpatched rule. A fourth round found two more real gaps — an attribute macro on a
trait member the new check never visits, and a `use ... as Clone;` import that shadows an
allowlisted derive name — both left open rather than fixed here, per this project's own
review-depth guidance once a fourth round is still finding real bugs; they are issue
[#186](https://github.com/madmax983/waymaker/issues/186).

The same fourth round found a real bug back in the alias-scoping mechanism instead, and this
one is fixed. `resolve_local_alias_chain` correctly stops at the end of a block's own
aliases — it only ever searches its own scope, the same discipline a module lookup has — but
a block-local `type Inner = Outer;` beside a *module*-level `type Outer = Foo;` left
`Inner {}` resolved only as far as `Outer`, because the caller took that partial chain as
final instead of feeding it on to `resolve_segments`'s own module-level lookup.
`resolve_local_alias_chain` now reports whether its own chain ended on an absolute alias — in
which case it is already fully resolved, the same as `resolve_segments`'s leading-colon
short-circuit — or simply ran out of block-local names, in which case the leftover head is
resolved again through `resolve_segments_from`, the module-lookup half of `resolve_segments`
split out for reuse; it is a no-op when the leftover name is not itself a module-level alias.
`a_function_local_alias_of_a_module_level_alias_still_resolves` and
`a_checked_dispatch_built_through_a_local_alias_of_a_module_level_alias_is_reported` are the
regressions, confirmed RED against the unpatched caller. The round's second alias finding —
two mutually exclusive `#[cfg(..)]`-gated `type` aliases sharing one name, where `own_aliases`
skips only `#[cfg(test)]` and so lets whichever is declared last win the lookup regardless of
which a real build compiles — is not fixed: it is the same "`cfg` is not evaluated" limitation
this file's own parsing documentation already states as an accepted, structural property of
every scanner in this family, met here in a form specific enough to be worth naming rather
than a new gap needing new machinery.

A fifth review round, on this pull request's own merge of a concurrent branch, closed two gaps in the same family from the opposite direction: not a construction this scanner missed, but a rewrite of an already-checked proof field. `pattern_binds_ref_mut` only flagged an explicit `ref mut` struct-pattern binding, and Rust's match ergonomics (RFC 2005) mean a bare `field` or a by-value `field: mut slot` binds by mutable reference too, whenever the scrutinee itself is `&mut` — a fact about the scrutinee's type that `syn` cannot see, since the pattern's AST is identical either way. Widened to `pattern_binds_a_name`: every named binding (`Pat::Ident`, any form) on a guarded field is now refused, not only `ref mut` — sound without type inference, since no narrower rule can be. And `type_is_qself_projection` only matched a qualified `<T as Trait>::Assoc`/`<T>::Assoc` projection; a bare `T::Assoc` has no `qself` at all — it parses as an ordinary path — so `type Unchecked<T: Alias> = T::Dispatch;` resolved to `["T", "Dispatch"]` and chased to `Dispatch`, never `CheckedDispatch`. It now also flags a multi-segment path whose first segment names one of the alias's own declared generic type parameters (from `ItemType::generics`), the same unresolvable-without-type-inference shape as `<T as Trait>::Assoc` — deliberately scoped to the alias's own parameters, so a UFCS-style projection through some other, already-defined concrete type is left alone, since telling that apart from an ordinary qualified path needs real name resolution this scanner does not have. Issue #171 is now closed on both fronts: the parenthesized-ancestor, compound-assignment, destructuring-assignment and alias-leak gaps above, and these two.

The same round found two narrower gaps in the same functions. `let ref mut slot = dispatch.intent.request;` borrows the initializer's own place directly through a `let` binding's pattern — no `Expr::Assign`, no `Expr::Reference` and no method call anywhere, so none of `note`'s routes sees it, and there is no struct pattern here for `visit_field_pat` to read either: the whole pattern is one identifier. `ref` and `mut` are explicit keywords here, not a scrutinee-dependent default binding mode the way a struct pattern's field is, so the check is exact rather than over-broad: `visit_local` now notes the initializer whenever the whole pattern is one `ref mut` identifier binding directly to it. And `implements_trait_for` — the reader `effect-protocol`'s own constant ban shares with `timer-capability`'s and `kernel-boundary`'s — searched for a bare `"\nimpl"`, so an `impl` preceded on its own line by a leading attribute (`#[rustfmt::skip] impl Forge for ClockKind { .. }`, which survives `cargo fmt`) was invisible to it, the same blindness `next_impl_line`'s own doc comment already records for `inherent_impl_bodies`'s reader; it is walked with `next_impl_line` now, the fix shared by every pin built on it.

Issue #184 closes the gap Codex found on review of PR #183, one level above the fix that
closed issue #171. A function's or an `impl`'s own generic parameter can bind an associated
type to a guarded name directly — `T: Alias<'a, Dispatch = CheckedDispatch<'a>>` — and
`T::Dispatch { intent, bytes }` builds `CheckedDispatch` under a name
`struct_literal_counts` never compares. No type inference is needed to catch it: the
guarded name is written in the bound's own text.
`parse::generic_assoc_type_bindings_naming` reads every `Assoc = Type` binding a file
declares, outside `#[cfg(test)]`, and reports one whose value names a guarded type;
`check_no_generic_assoc_type_bindings` refuses `effect.rs` outright when it finds one, the
same hard refusal `check_no_projected_type_aliases` already gives an unresolvable `type`
alias.
`a_checked_dispatch_built_through_a_generic_associated_type_binding_is_reported` is the
regression, confirmed RED against the unpatched pin.

Adversarial review of that fix found the direct read was not enough on its own: `type
Hidden = CheckedDispatch;` beside `T: Alias<Dispatch = Hidden>` writes an unguarded name,
`Hidden`, at the binding's own position, and the first version of this check compared only
that position — verified live against the unpatched function, which answered `found: []`
for it. `generic_assoc_type_bindings_naming` now carries the same module-and-block alias
stack `struct_literal_counts` does, and resolves a binding's value through it exactly the
way a struct literal's own path is resolved — `resolve_local_alias_chain`,
`resolve_segments` and `resolve_segments_from`, unchanged. What it still does not resolve
is a projection *as* the binding's value, the same shape `qself_type_alias_names` already
leaves for a `type` alias's own target — see
[what is not checked](#what-is-not-checked)'s own bullet for this issue.

Issue #111 closes a gap ADR 0033 named in its own consequences section. The old code
recorded a kind with no row as an `EffectFailed`, with no payload. That decision was
permanent: §08 has no edge back from a resolved effect to an unresolved one. So no later
firmware could complete that effect, no matter how many rows it gained. `dispatch::Produced`
gains a third answer, `Unserviceable`, carrying no payload — the same shape §11's
`KernelError` already uses for "this firmware cannot service this now", as distinct from
"this firmware can never replay this history". `Ctx`'s `ActivityFuture` stops for it as it
stops for `Poll::Pending` — neither calls `Journal::resolve`, so the effect stays outstanding
under the identity its schedule record already committed — but it is not a retry: `stage`
moves to `Ended` rather than staying at `Dispatching`, so a future retained across a spurious
repoll within the same boot never asks a dispatcher already known to have no answer for this
kind. Codex found the gap on review of this change: the first version left `stage` where a
retry leaves it, and a repoll would have asked again. A second round found the fix itself
incomplete: `stage` lives in the future, so a dropped-then-recreated `ActivityFuture` — a
`select!` cancellation, say — started fresh at `Stage::Scheduling` and asked again this boot.
The flag now lives in `Ctx`, shared across every future it builds, the way issue #107 moved
`TerminalFuture` and `ContinueFuture`'s own flag into `Ctx` for the identical reason.
`wiring::Table` answers it for a kind no row declares, in place of the
`Unhandled::NoSuchActivity` it used to construct; since that was `Unhandled`'s only reason to
exist beside wrapping a row's own error, `Unhandled<E>` is gone and `Table<W, E>::Error` is
`E` itself. Two tests are the "done when", at two levels: `crates/waymaker-embassy/tests/wiring.rs`'s
`a_firmware_that_later_gains_the_row_completes_the_run_its_predecessor_could_not` proves the
façade's own sequencing over a fake journal, and
`crates/waymaker-facade-demo/tests/dispatch.rs`'s
`a_firmware_that_later_gains_the_row_completes_the_run_its_predecessor_left_outstanding`
proves the same claim over real media — a table with no row commits a schedule record and
writes nothing else, and a second boot over the same device, with a table that has the row,
redelivers and completes it. That second test's own boot 1 answers `Ok(Progress::Waiting)`,
because its `Wired::run` bridges the façade through `waymaker-drive`'s synchronous boundary
the same way `ota.rs`'s `Downloader::run` does: it reads back the real `Suspended` the
boundary returned on its last call, and falls back to `Suspended::awaiting_dispatch()` on the
one path with no boundary call behind it at all. A third round found the flag from the second
had only ever been read in `ActivityFuture`: `waymaker-drive`'s boundary refuses a *second*
boundary call while an effect is outstanding, so a workflow that met `Unserviceable` on one
boundary and then asked for a timer, a completion or a `continue_as_new` on the same boot
turned its own clean stall into a hard `DriveError::EffectOutstanding` — the very failure
this issue exists to prevent, met one boundary over. `TimerFuture`, `ContinueFuture` and
`TerminalFuture` now each carry the same flag and refuse to reach the journal or record a
conclusion once it is set. That same round found the test's own `Wired::run` still asserting
the wrong thing — an earlier draft believed the bridge had no way to build a real `Suspended`
for this stall, which was false: `ota.rs`'s bridge already carried the mechanism, and this
crate's test harness had simply not used it. A fourth round found the third round's own flag
was read only by `Ctx`'s own futures: `ota.rs`'s and `provisioning.rs`'s bridges each fall
back to the workflow's bare `Result` whenever `ctx.conclusion()` answers `None` and the poll
answered `Ready`, which is right for an ordinary `?`-propagated activity failure and wrong
for a workflow that polled a stalled `ActivityFuture` directly — a `select!` that dropped it
for another branch — and then returned on its own with the effect still outstanding and
nothing recorded. `Ctx` gains a fourth accessor, `unserviceable`, and both bridges now refuse
that fallback while it answers true, folding the case into the same clean stall a direct
`.await` already produces.
`crates/waymaker-facade-demo/tests/dispatch.rs`'s
`a_workflow_that_abandons_a_stalled_effect_and_returns_directly_still_waits` is the
regression: it polls one activity once, drops it, and returns `Ok(())` directly, and the boot
answers `Ok(Progress::Waiting)` rather than `Err(DriveError::EffectOutstanding)`. The
code-flash cost was nil for the first round's own diff — `cargo xtask size`'s `facade` row
measured 13174 B of layers both before and after, because removing `Unhandled`'s wrapping
paid for the third `Produced` arm — the second and third rounds' three added `&bool` fields
moved that figure to 13122 B, and the fourth round's new accessor holds it there: the size
probe now calls it too, so its cost lands entirely in `probe` rather than `layers`.
One thing stays the same throughout: a caller still cannot tell "the world is slow" from "no
firmware will ever service this" from the return value alone. Both cases return
`Poll::Pending` — a halted journal and an unpassed deadline already work the same way. Design
document §13's boundary gives no reason for any stop, and this change adds none. See
[ADR 0049](docs/adr/0049-an-unserviceable-kind-is-an-answer-not-a-record.md).

Issue #115 closes a gap Codex found on the fourth review round of issue #39's own pull
request: `SizeReport::runtime_ram_total` composed the statics term from the largest `Δram`
of *every* row the document held, and `--report` reads a document this process did not
produce, so nothing checked that the row *set* was the one the workspace actually derives.
A document that left out the row with the largest `Δram` composed a smaller, wrong total and
could pass a budget a complete document would have failed — the one figure this gate takes
*across* rows rather than gating each row on its own.

The first version of this fix took the issue's fourth, smallest option: narrow the
composition to gated rows alone, which are pinned and required, so an omitted row could no
longer starve it. Codex's review of that version on this pull request found the cost of
narrowing: a per-feature row is a configuration somebody ships, and design document §04
states one runtime-RAM ceiling for the *device*, not one per configuration — ADR 0035's own
words for the original design were "a per-feature row is a configuration somebody ships, and
taking the largest is the direction that fails closed". Narrowing to gated rows stopped
gating every configuration that enables an optional feature, silently, forever, which is a
real regression rather than only the closing of an adversarial-document hole — and it is
exactly the cost issue #115 named for its first option and did not take.

The fix taken instead is that first option, made affordable: `runtime_ram_total` composes
the largest `Δram` of *every* row again, matching ADR 0035 unchanged, and a new function,
`completeness_shortfalls`, closes the omission by resolving `cargo metadata` for the
workspace and holding the document's row set to what `matrix` derives from it — a row
`matrix` would produce and the document lacks, by name or by feature selection, is refused.
Resolving metadata costs nothing a firmware build would: no image is linked, which is what
keeps `--report` usable without one. `main.rs`'s `run_size` runs it alongside
`SizeReport::shortfalls` and renders both lists as one report. `missing_rows` is refused
outright on an empty `expected` rather than read as nothing to check — a workspace with no
`waymaker-size-probe` has `matrix` derive no row at all, and a document from before the
probe was removed would otherwise pass against it vacuously, the same empty matrix
`measure_into` already refuses to link. Four tests drive it: a per-feature row's large
`Δram` raising the total again, `missing_rows` catching a document missing a row `matrix`
derives, catching a row whose name is reused with a narrowed feature selection, and catching
a document read against a probe-less workspace. Review also found an out-of-scope,
genuinely separate gap — a gated row's own `ram`/`bss` fields carry no non-zero floor,
unlike `flash`'s — filed as issue
[#172](https://github.com/madmax983/waymaker/issues/172) rather than folded in, since `0 B`
is this engine's real, current statics figure and a floor there would fail every honest
report. No new ADR: nothing here moves a must-not-own cell, a dependency edge, or a rule
id — see
[ADR 0035](docs/adr/0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md), which this
leaves exactly as accepted.

Issue #106 then closes a gap Codex found on the third review round of issue #35: the
`drive-facadeless` stage proved that no *other* `waymaker-drive` module needed the façade,
not that the crate would build with `waymaker-embassy` deleted — the dependency was never
optional, so a façade regression failed the same stage a façade absence would have.
`waymaker-facade-demo` is the fix. `facade`, `ota` and `provisioning` — issue #35's bridge
and design document §06's two examples — now live in a crate above `waymaker-drive` rather
than inside it, and `waymaker-drive` names no dependency on `waymaker-embassy` in any table.
`cargo metadata` states the claim the feature flag could only argue for. `without-facade`
and the `drive-facadeless` stage are gone; `facade-demo-firmware` replaces the second,
building the new crate's library for `thumbv6m-none-eabi` — `nm` on the rlib still finds
`ota_update` and `ActivityFuture`, for `ota.rs`'s own reason. `ctx-facade`'s driver half
keeps its shape and loses its exemption: `source::FACADE_DRIVER_MODULES` is empty, so every
`waymaker-drive` module is held to naming no façade rather than every module but three.

Moving `ota` and `provisioning` moved a dependency their new home could not carry for free.
Both mint a `Suspended` when a driven future stalls with no recorded ending, and
`Suspended`'s field is private to `waymaker-drive` — a privilege the two modules held only
by being files of that crate. `facade::Bridge` now keeps the real value the boundary
returned on every call that produced one, and the two examples read it back after their
poll rather than building a new one; `Suspended::NEW` stays `pub(crate)` and neither example
names it. That is a strengthening rather than a workaround: the value returned is now
provably the boundary's own, carried across the `.await`, rather than a fresh one asserted
to match it.

The lint, test, docs and coverage stages that pass `--no-default-features` reach the new
crate exactly as they reached the three modules before: an ordinary workspace member rather
than a feature-gated one, so nothing fell out of them. See
[ADR 0032](docs/adr/0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md),
which records the split as superseding its own `without-facade` compromise.

Issue #96 closes the four rows [the failure matrix](#the-failure-matrix-row-by-row) owed on
the rig, and it does so without moving a single count of the six rows already swept. The
insight it rests on is that `Rig::judge` and `Rig::resume` hardcoded `Rig::BANK` in a way
that happened to be harmless, because nothing had ever asked this rig to install a second
run: `bank::select`'s own answer was always `Rig::BANK`, so reading it by name and reading it
by authority were the same read. `Rig::authority` is the fix — a new private method that
keeps *which* bank `bank::select` named rather than only how many — and `installed_journal`
now refuses a bank whose header names this run but is not the one currently authoritative,
which a stale, unerased losing bank's header could otherwise still satisfy. Reverting that
one check and rerunning the row 8 test reproduces the defect directly: a resumed run answers
`Ok(Completed { recovered: 3, .. })` from the retiring bank's own three records, on a device
whose authority had already moved to the bank a swap installed.

Rows 7 and 8 needed a workload that rolls over, and `Rig::iterate_until_rollover` is the
smallest addition that provides one: it writes a run's `RunStarted` and as many
schedule/completion pairs as it is asked for, and stops — no `RunCompleted`, because the
run's continuation is whichever bank a swap leaves authoritative rather than this bank's own
end. `crates/waymaker-rig/tests/matrix.rs`'s `drive_rollover_swap` drives §10's seven
steps directly against the bank the partial run left off in, the same way
`crates/waymaker-fault/tests/swap.rs` drives them for the model; the whole sequence — the
partial run, the swap, and a small complete run written into the bank it installs — runs
through the crash injector once, and every point is classified by `bank::select`'s answer
alone. A swap declares no journal record and marks no witness, so there is nothing else a
row-7-or-8 point could be read from. Where the point lands *before* the swap's own
operations began, it is one of rows 1 to 6 already, not a new one — `classify_rollover`
reads the operation index against a boundary taken from a separate fault-free run of just
the partial sequence, so the two counts stay apart.

Rows 9 and 10 are driven rather than swept, matching the model half's own treatment of the
same two rows: a capacity refusal and a declared-workflow mismatch are not media crashes the
injector produces. `Rig::iterate_reserved` and `Rig::resume_reserved` gate every append with
`waymaker_flash::capacity::Reserve` instead of the ungated writer, and a search over
declared tail widths on the rig's own fixture finds one that refuses the second effect's
schedule once the first has completed — the same shape of search `waymaker-drive`'s row nine
already uses, run here against a real bound instead of a real geometry, because this rig's
own construction-time check already prices every record at its worst case and only a
reserve stricter than that check can refuse before the run's true end. The explicit exit
past it is the same seven-step swap rows 7 and 8 drive, run once by hand rather than swept.
`Workload::diverging` is row 10's whole addition: an index and a different activity kind at
it, leaving the run's shape, its identity and every other record's bytes exactly as they
were, so `Rig::resume_declaring` meets a genuine one-record disagreement rather than a
shortened or corrupted run — and refuses it with `Breach::RecordDiffers`, before any effect
runs again and before any byte is written, at every crash point that leaves the changed
effect's schedule recovered and its completion outstanding.

What is owed is written down rather than implied closed. The two bank rows are swept at one
`effects_before_swap` value and one declared next-run input; `waymaker-fault`'s own swap
sweep is the one that varies the geometry and the step at which every crash lands. Row 9's
search is over declared bounds rather than over geometries, so it says nothing about a bank
sized differently than the rig's shared fixture. And issue #96's board half — rows 2, 3 and
4 need the dispatcher's own record of what it was entered for, which a reset takes with the
RAM — is exactly as unmet as it was before this issue, and stays a board's to close.

Review of the pull request that closed issue #96 found two more real defects, both the same
shape as `Rig::authority`'s own fix: an instrument reading a state issue #96 made reachable
for the first time and answering the question it was never asked to answer. The first is in
row 9's own no-mutation claim: `iterate_reserved` and `resume_reserved` wrote the witness's
`Attempted` mark for a record *before* asking `Reserve::admits` whether that record would fit
— so the very first encounter with a near-capacity refusal genuinely mutated the device's
instrument region, even though `Reserved::stage` itself never touched the journal. The
existing test could not see it: a *replay*'s witness continuation skips a mark the first
attempt already wrote, so `assert_replay_refuses_without_mutation`'s wear comparison compared
two states that were already equal for the wrong reason. A free `admits` function — the same
`Reserve::admits` call `Reserved::stage` makes internally, read one call earlier — closes it;
`the_first_capacity_refusal_writes_no_mark_of_its_own` compares the first refusal's rig-only
wear against a device that legitimately stops after the same one-effect prefix and never
meets a refusal at all, verified failing against the prior order (one extra program operation
and barrier — the mark) before the fix landed.

The second is `Rig::judge` itself, and it is the sharper of the two: gating the audit on
*current* authority, the way `Rig::resume` correctly must, made a row-8 rollover's own
retired bank read as though its acknowledged records had gone missing, because `uninstalled`
assumes a bank with no current authority has nothing to say about this run rather than that
it said something and was superseded. `installed_journal` stays authority-gated for
`resume` and `recover_prefix`, which do need to know whether a bank is still the one a boot
would choose; `judge` moves to a new `own_bank_journal`, which reads `Rig::BANK`'s own header
by run id alone and audits what it holds regardless of which bank is authoritative now — a
retired bank's own history does not change when a swap moves authority away from it.
The row 8 test now asserts `rig.verify(0, ..)` reports `Outcome::Passed` at every one of its
crash points, beside the assertion that `resume` refuses them; verified failing with
`Breached(LostAcknowledgedRecord { .. })` against the prior single check before this split
existed.

A further round found two more, again the shape of an instrument answering a question the
previous round made reachable for the first time. The first is in the pair of fixes above:
`Rig::new` sizes the bank and the witness for `self.effects` alone, and `resume_declaring`'s
`declared` can share this rig's seed and iteration — so it matches the recovered prefix —
while naming more effects than either was ever provisioned for. Left as the earlier round
left it, that call ran past its own provisioning until an unrelated capacity error
(`AppendError::NoRoom`, `WitnessError::Full`) stopped it, rather than the refusal-before-
mutation `resume`'s own postcondition promises. `resume_as` now refuses any workload wider
than `self.effects` before touching the device at all, ahead of the recovery this rig's own
authority check already gates on. Reproduced first from the same crash point the earlier
round used — both of the rig's own effects durably completed, `RunCompleted` not yet begun —
where the prior code durably appended and dispatched the extra effect's schedule record
before this refusal existed.

The second found that `rollover_sweep`'s combined run never called `Installed::reclaim` at
all: `Installed::recovery` and `Installed::reclaim` both consume the value `commit` returns,
and the sweep took the former to keep writing into the bank it installed, so the crash
injector never produced a point during the retiring bank's own erase or its barrier — even
though row 8 held authoritative throughout that window exactly as it does after `commit`,
and the row was published as fully swept regardless. `drive_rollover_swap` now reclaims
first and re-derives the installed bank's journal region by hand — the same read
`iterate_until_rollover_and_iterate_reserved_refuse_a_bank_a_swap_moved_past` already does —
so the erase is under the injector and the caller still gets a writer for the bank it
installed. Row 8's own count moved from 167 to 175 crash points, the eight new ones all
inside the erase and its barrier; row 7's count did not move, because reclaiming the
retiring bank can only ever lose it `bank::select`'s vote, never regain it ahead of the
bank the swap already installed.

A further round found a fourth, in `require_own_authority` itself: it names the *bank* —
`Rig::BANK` must be the current sole authority — and never the *run*, so a bank that is
this rig's own and currently authoritative could still have been installed for a different
iteration, and `iterate`, `iterate_until_rollover` and `iterate_reserved` would each write
that iteration's records and witness marks into the wrong iteration's journal rather than
refuse. `journal_region` — the write path's own twin of the run-id check
`installed_journal` already holds `resume` and `recover_prefix` to — now takes the
workload it means to write and refuses unless the bank's header names that workload's own
run. Reproduced first by preparing a part for iteration 0 and calling `iterate(1, ..)` on
it directly: the unfixed code answered `Ok(Stop::Completed)`, having written iteration 1's
records and marks over iteration 0's bank, rather than `RigError::Bank`. Two existing tests
had built their own fixtures by relying on exactly that gap — one in `resume`'s own test
suite, one in the rig's crash-sweep judge test — and both now reach the same device states
through the lower-level primitives `Rig` itself writes with, rather than through the write
path this fix closes.

A fifth was `resume_declaring` itself. `recover_prefix`'s audit only compares `declared`
against what recovery actually found, so a crash landing before
[`Workload::diverging`](crate::workload::Workload::diverging)'s own changed index left
nothing there for the audit to disagree with — `resume_as`'s loop then wrote the declared,
diverged record fresh and dispatched it, same as any other never-before-recorded record.
The fix widens what a fresh write is checked against: a record `resume_as` is about to
write for the first time must now agree with this rig's own undiverged truth —
`self.workload(iteration)` — before it is marked, appended or dispatched, not only with
whatever recovery happened to find. For an ordinary `resume` the two are the same workload
and the check never fires; `resume_declaring`'s `declared` is where it can differ.
Reproduced first from a crash point that left the first effect durably completed and the
second effect's schedule — the record `diverging` changes — not yet recovered at all: the
unfixed code answered `Ok(Completed { recovered: 3, .. })`, having written and dispatched
the diverged record, rather than `RigError::Breach(Breach::RecordDiffers { .. })` before
either happened.

A sixth was the capacity preflight's own other half: it refused a `declared` *wider* than
`self.effects` but let a *narrower* one through. A narrower run's opening effects are a
byte-for-byte prefix of a longer one's, so its early records still agree with this rig's
own truth and the per-record check above does not fire — the mutation and the dispatch it
exists to prevent both happen for every record before the declaration's own early
`RunCompleted` finally collides with an index the real run still has open. The preflight
now refuses any effect count other than `self.effects`, not only a wider one:
`resume_declaring` only ever means to audit a workload that agrees with this rig's own run
everywhere but the one record `diverging` names, and a workload of another length is not
that shape. Reproduced first from a crash point that left only `RunStarted` durable and a
narrower declared workload: the unfixed code dispatched the first effect and only then
answered `Breach::RecordDiffers` at the index where the shapes finally disagreed.

A seventh was the per-record check's own placement, in the same shape once more: it ran
*inside* the write loop, so a record that genuinely agreed with this rig's own truth was
still written — and dispatched, if it scheduled an effect — before the loop reached
whichever later index `declared` actually disagreed at. Row 10's own promise, "no further
execution and history untouched", is about the whole declaration, not only the one record
that turns out to disagree. The check now runs once, over every record this resume would
still need to write, before the outstanding-effect redelivery or the write loop touch
anything — so a disagreement anywhere in what is left of the run refuses before the first
agreeing record in front of it is touched, not only before the disagreeing one itself.
Reproduced first from a crash point that left only `RunStarted` durable, with a `declared`
diverging at the *second* effect: the unfixed code dispatched the first effect — which
agreed with `declared` — before reaching the second and refusing there.

An eighth returned to row 9, and it is the same shape once more, in `iterate_reserved` and
`resume_reserved` rather than in `resume_declaring`: `admits` is checked for the record
about to be written and nothing else, so a schedule that fits a reserve whose
`effect_result_bytes` is too narrow for the completion still gets marked, appended and
dispatched — the effect runs — before the loop reaches the completion's own index and
discovers `Refusal::OverDeclaredBound` there. By then refusing cannot undo the dispatch,
and every retry through `resume_reserved` performs the effect again. A schedule's own
width does not depend on `effect_result_bytes`, so the schedule's admission is never
evidence that its completion's will follow. Both fresh-write loops now preflight the
completion `Workload::completion_index` names, admitting it against the same reserve
before the schedule's effect is dispatched — the same shape the redelivery branch above
each loop already carried for an *outstanding* effect, extended to a schedule written
fresh in the same call. Reproduced first with a zero-width `effect_result_bytes` reserve
against a freshly prepared device: `iterate_reserved` dispatched effect 0 and only then
answered `Refusal::OverDeclaredBound` at its completion's index, and `resume_reserved`
did the same from a device recovered no further than `RunStarted`, where the main loop
rather than the redelivery branch reaches the fresh schedule.

A ninth, and a different shape from every one before it: `Workload::diverging` took a raw
record index rather than an effect number, and `record`'s only consultation of it is in
the `Role::Schedule` arm — so a caller who passed the index of a `Start`, `Completion` or
`Finish` record, or one past the end of the run, got back a workload that agreed with the
base one everywhere, byte for byte, rather than diverging at all. That is the same
silent-masking shape every earlier finding in this issue closed against a media state;
here the wrong input is a caller's own argument, and the fix is the same kind wrong-role
arguments elsewhere in this codebase are refused by construction: `diverging` now takes
the effect whose schedule diverges and derives the record index itself through
`schedule_index`, so a `Start`, `Completion` or `Finish` index cannot be named through
this API at all. An effect this run does not schedule still diverges nothing — honestly,
since there is no schedule record for it to disagree at, which `schedule_index` already
answers `None` for. Reproduced first against the old signature: `.diverging(0)` — record
index 0, `Role::Start` — produced a workload indistinguishable from the base one across
every record, and every existing caller turned out to have already been deriving the
right index through `schedule_index` before calling it, so none needed anything but the
one call site simplified to the effect number it was computing a schedule index from.

A tenth returned to round 7's write-path check, and it is one identity narrower than the
round it followed: `journal_region` compared `header.run` against the workload's own run
id and stopped there, so a bank whose header names the right run but a different
`workflow_kind` or `input` still passed. A run id agreeing is not the whole of a
workflow's identity — a real boot (`crates/waymaker-drive/src/drive.rs`) refuses a
recorded kind or input that disagrees with the one it expected — and nothing stops the
public swap surface installing a header that reuses a run id under a different declared
identity, since `SwapError::RunReused` only compares the *next* run against the
*retiring* one. `journal_region` now also compares the header's `workflow_kind`,
`workflow_version` and `input` against `workload`'s own opening record before handing
back a region to write into. Reproduced first with two real swaps rather than a
hand-fabricated header — a single swap moves authority to the *other* bank and would
refuse earlier, at `require_own_authority`, for an unrelated reason: the first retires a
freshly prepared run onto the other bank under a throwaway identity, and the second
retires that throwaway run back onto `Rig::BANK` naming one iteration's own run id but
another iteration's workflow input. The unfixed code answered `Ok(Completed)`, having
written and dispatched into the mismatched bank, before this check existed.

An eleventh found the read path's own twin of the ninth's gap: `resume_as`'s preflight
compared `workload.effects()` against `self.effects`, but never `workload`'s own run
against the run `iteration` names. Two iterations of one plan share an effect count by
construction, so a `declared` sharing this rig's seed and *another* iteration's number
passes that check while still naming a bank installed for a different run.
`recover_prefix`'s audit then checks `declared` against the bank's own header, which
agrees — `declared` genuinely is that other iteration's own workload — and once the
recovered prefix already covers the whole run, the `recovered >= records` branch answers
`Completed` before the per-record comparison against `self.workload(iteration)` is ever
reached: a run belonging to iteration 0 is reported as iteration 1's. `resume_as` now
also refuses unless `workload.run()` agrees with `self.workload(iteration).run()`, before
recovery is read at all. Reproduced first by completing iteration 0 with no cut anywhere,
then calling `resume_declaring(1, rig.workload(0), ..)` on the same device: the unfixed
code answered `Ok(Completed { recovered: 6, .. })`, reporting iteration 0's own history as
iteration 1's, rather than refusing with `RigError::Workload`.

A twelfth found the eleventh's own fix unsound: comparing `Workload::run` is comparing a
hash, and `RunId` is `SplitMix64::new(seed).at(iteration)` — `mix(seed + GAMMA *
(iteration + 1))`, injective in the mixed word but not in the *pair* the word is built
from. `seed` plus `GAMMA` at iteration zero sums to the same word as plain `seed` at
iteration one, so a device whose real history was written by a wholly different rig —
seeded `SEED` plus `GAMMA` rather than this rig's own `SEED` — at iteration zero satisfies
both the eleventh's check and `own_bank_journal`'s header comparison for a
`resume_declaring(1, ..)` call on the genuine rig: the header truly names the colliding
run, and so does the declaration. `resume_as` now compares the seed and the iteration
directly against this rig's own plan and the `iteration` argument, rather than routing the
comparison through a hash nothing ever claimed was injective over two arguments at once.
Reproduced first with two rigs sharing a geometry and differing only by that one seed
offset: the foreign rig wrote a complete run at its own iteration zero, and the genuine
rig's `resume_declaring(1, ..)` over that same device answered `Ok(Completed { recovered:
6, .. })` under the eleventh's fix alone, never having written a byte to the device it
just reported completing.
Issue #110 closes the two things ADR 0032 had left as "rung 0.4's dispatcher", and they
turned out to need no dispatcher at all. In-boot sleep is `waymaker-embassy`'s new `alarm`
module: an `Alarm` capability — one method, `wake_after(kind, remaining, waker)` — that
`TimerFuture` calls on a halt exactly when `Journal::deadline_remaining()` answers `Some`,
instead of asking `wait` again straight away with nothing arming a wakeup for it.
`NoAlarm` is the zero-cost answer for a firmware with no such peripheral, and `Boundary`
grows the same `deadline_remaining` query so the synchronous driver answers it too. The
`continue_as_new` join is `waymaker-drive`'s: `Driver` gains a second constructor,
`Driver::at_bank(layout, reserve)`, that reads both banks fresh at every `boot` and keeps
what `bank::select` decided for the length of that boot — closing the two preconditions ADR
0022 left on `Swap::beginning`'s caller, because neither `booted` nor `run` is ever a value
this driver could be carrying stale. `Boundary::continue_as_new` on a bank-pointed driver now
performs the whole seven-step swap, mints the next run with the new `RunId::successor()`,
and checks `Reserve::for_layout` before touching the device — closing issue #110's third
precondition, that nothing obliged a capacity check before swapping. Review of this change
found three more ways a live call could go wrong that no test had driven yet, and each is a
refusal before any byte moves: an effect scheduled and not yet resolved has a durable
schedule record in the bank about to be reclaimed, so `swap_in` refuses with
`DriveError::EffectOutstanding` rather than forfeiting an identity no crash took; a next-run
input wider than the run's own declared bound would install a journal below
`Reserve::for_layout`'s own floor, so it is `DriveError::NextRunInputTooLong`; and a bank a
swap has just installed has no `RunStarted` record yet for `begin` to check the next
workflow's identity against, so the new `verify_header_identity` makes the same comparison
against the header instead, refusing with `DriveError::NotThisWorkflow` when they disagree.
`Driver::new`, pointed at a fixed region, is unchanged and still refuses with
`DriveError::ContinueUnsupported`: a region genuinely does not name a bank. The façade needed
no changes of its own for this half — `Journal::continue_as_new` was already a pass-through to
`Boundary::continue_as_new`, so a driver behind it that can swap makes an awaited
`ctx.continue_as_new(..)` really swap, unchanged. `crates/waymaker-drive/tests/continue_as_new.rs`
drives a real two-bank device through the swap and reads the installed bank's bytes and its
seal's generation back the way a cold boot has to, rather than trusting the call that wrote
them, then boots a third time with a mismatched workflow to prove that read came from the
bank the swap installed and not a stale one; the three refusals above each have a test of
their own, reading the device back untouched afterwards. What it does not yet do is a crash
sweep of `continue_as_new` itself; the seven steps it calls are already exhaustively swept one
layer down, in `crates/waymaker-flash/tests/swap.rs` and `crates/waymaker-fault/tests/swap.rs`,
unmodified. Measured cost: zero bytes on every `cargo xtask size` row, and zero heap blocks
on `cargo xtask profile`. See
[ADR 0051](docs/adr/0051-an-alarm-is-armed-on-a-halt-and-a-driver-at-a-bank-can-swap.md).

Issue #172 closes a gap found in review of issue #115's own fix. `SizeReport::shortfalls`
held a gated row's `flash` to two floors. It held `ram`/`bss` to none. A hand-edited row
could report `ram: 0, bss: 0` and pass every check. That deflates `runtime_ram_total`'s
composed figure. Mirroring flash's "cannot cost nothing" floor was rejected. `0 B` is this
engine's real, current statics figure, and that floor would fail every honest report. Two
narrower checks close what a check can close without a real build.
`row_reading_shortfalls` refuses a row whose `ram` reads smaller than its own `bss` plus
`data`. `bss` and `data` are writable, non-thread-local sections, and `ram` counts both.
So a smaller `ram` is an internal contradiction, not a guess. A gated row's `ram` may also
not read smaller than the baseline's — flash's own rule, one section over. Neither check
can tell an honest `ram: 0, bss: 0` row from a forged one. That gap stays open. It is
documented in `runtime_ram_total`'s own doc comment and in
[what is not checked](#what-is-not-checked). No new ADR: nothing here moves a
must-not-own cell, a dependency edge, or a rule id.

Issue #185 closes a gap Codex named on review of PR #183, and checks that a gap it
described had *already* closed by accident. Codex's own repro was `#[cfg(any())] type
Unchecked = Decoy; type Unchecked = CheckedDispatch;`, naming `own_aliases`'s
`.find()` as the cause: it kept every declaration of one name and picked whichever
sorted first. Testing it first, the way this repo's own review-depth guidance asks —
red before green — found it was already green. Round 43's `Cfg::requires_test`
rewrite, made for a different reason (several `#[cfg(..)]` attributes on one item
correlating through a shared flag), answers a wider question than its name says: an
always-false formula is, vacuously, "false whenever `test` is false" too, so
`own_aliases` already drops a `#[cfg(any())]` declaration on its own. Two regression
tests lock that in, at both scopes `own_aliases` feeds — module scope, where the
first declaration wins, and block scope, where `resolve_local_alias_chain` searches
in reverse and the *last* one wins — so a future change cannot quietly lose either
direction.

What was not already closed is real, and is the harder half of the finding: two
declarations of one name that are each *live*, under different flags this scanner
cannot evaluate — `#[cfg(feature = "a")] type Unchecked = Decoy; #[cfg(not(feature =
"a"))] type Unchecked = CheckedDispatch;` — where `own_aliases`'s pick still favors
whichever declaration sorts first (module scope) or last (block scope), regardless of
which one a real build compiles. Both directions were driven and watched fail before
being fixed. Closing this by changing what `resolve_local_alias_chain` and
`resolve_segments_from` deterministically resolve to was rejected: `resolved_path_uses`
and `future_trait_implementors` depend on that one answer too, and this repo's own
history (Codex review, PR #160, rounds 3 and 4) already tried "explore every alias a
name could mean" for a different kind of ambiguity and reverted it, because it can
attribute an unrelated, legitimate construct to the wrong one as easily as a
first-match pick can miss a real one — the same risk a wider change here would
reopen for two callers that do not need it. `alias_could_reach_target` is the
narrower fix instead, scoped to `struct_literal_counts` alone: it asks only whether
*some* live alias sharing a name could reach the target, and counts a construction
when it can, even when the deterministic answer disagreed. A construction pin's
danger is an uncounted forgery, not an extra count, so failing closed here means
counting more, not resolving differently. Every construction pin built on
`struct_literal_counts` — `EFFECT_CONSTRUCTIONS`, `CHECKED_DISPATCH_CONSTRUCTION`,
and `timer-capability`'s and `kernel-boundary`'s associated-constant bans among them —
gets this for free, because they all funnel through the one function.

Three review rounds on this change found three more, and this file's own
review-depth guidance — stop past two or three, open an issue once a further round
is still finding real bugs — applies to the last two. The first was in the fix
itself: `alias_could_reach_target`'s first version chased every alias's target by its
bare last segment alone, so `use foo::Bridge as Entry;` beside an unrelated `type
Bridge = CheckedDispatch;` chained `Entry` through `Bridge` on name alone and counted
a construction that was never really `CheckedDispatch` — a false positive, and for a
caller that compares `total` against `inside` by exact equality, a false positive is
a spurious violation on honest code. Fixed by chasing a target only while it stays
one segment; a multi-segment target is compared directly and the chain stops, matching
the same imprecision `struct_literal_counts` already accepts for a single, correctly
resolved path. `an_unrelated_alias_sharing_a_bare_target_name_is_not_chained_through`
is the regression. The second is a doc-comment bug rather than a logic one: the first
version's doc block for the new function sat directly above it with no blank line
between it and `struct_literal_counts`'s own, older doc comment, so the whole run
attached to the wrong item — `struct_literal_counts`'s rustdoc page lost its real
description, keeping only its "§Errors" line. `cargo doc -D warnings` does not catch
this, because both items still carry *some* doc text; it is exactly the "a
measurement that did not happen is not a measurement that passed" gap this file
already states, met in documentation rather than code. Moved the function after
`struct_literal_counts` instead of before it, with a real blank line on both sides.

The third is real and not fixed here: `alias_could_reach_target` only runs for a
bare, single-segment construction path, matching the same restriction
`resolve_local_alias_chain` already has — so a qualified site
(`super::Unchecked { .. }`) under the identical live/live ambiguity is not checked
at all. And it does not follow a live alias whose target itself steps into another
module (`use traits::Marker as Unchecked;`), unlike `resolve_segments_from`'s own
`own_modules` descent. Both were confirmed against a real `rustc` build: each
constructs the pinned type under a real, compiling configuration, and
`struct_literal_counts` reports zero for it either way. Closing them needs the same
branching threaded through `resolve_segments_from`'s module descent, which is the
wider change this paragraph's own second round already argued against for
`resolved_path_uses`'s and `future_trait_implementors`'s sake. Tracked as issue
[#197](https://github.com/madmax983/waymaker/issues/197) instead of a fourth round
here. What stays owed, all in one place now: `resolved_path_uses` and
`future_trait_implementors` still pick one answer under the same live/live
ambiguity, and so does `struct_literal_counts` itself for a qualified construction
site or a target reached through another module — which
[what is not checked](#what-is-not-checked) names rather than leaves implied by a
stale claim about `#[cfg(test)]` alone. No new ADR: nothing here moves a
must-not-own cell, a dependency edge, or a rule id.

Issue #197 closes the two gaps the paragraph above named. `path_could_reach_target`
replaces `alias_could_reach_target` as `struct_literal_counts`'s fail-closed check. It
runs for a qualified construction path (`super::Unchecked { .. }`) as well as a bare
one. A helper beside it, `segments_could_reach_target`, follows a multi-segment alias
target into the module it names — the same `self`/`super` and module-descent state
`resolve_segments_from` already keeps, kept separate from it on purpose, for the
reason the paragraph above states. One shared, decrementing budget bounds the whole
search, so a crafted alias cycle across two modules still cannot loop forever. Tests
for both gaps, and their negative controls, are in `xtask/src/parse.rs`'s own
`cfg_alias_ambiguity_tests` module.

Two unrelated, pre-existing bugs surfaced while fixing this and are fixed alongside
it. `generic_assoc_type_bindings_naming` still called `resolve_segments` and
`resolve_segments_from` with two arguments after issue #181 gave both a third,
`shadow` — a plain build break on `main`. It now passes `&[]`, matching what another
caller with no shadow tracking of its own already does. And `struct_literal_counts`
had already grown past clippy's `too_many_lines` gate before this issue touched it;
its `Literals` visitor moves to module scope, unchanged otherwise, to fix that.

Review found two more real problems in the fix itself, both real gaps rather than
nitpicks. First: `resolve_local_alias_chain` chases more than one block-local hop
before it gives up — `type A = C; type C = CheckedDispatch;`, both declared in one
function body, is a real, two-hop chain. The first version of this fix tried
`block_items` only on the very first hop, so a live chain like that was missed. A
missed count is exactly the danger this whole check exists to close. Fixed by
threading `block_items` through `segments_could_reach_target` itself: it applies at
every hop where no module has been entered by name and the search is still at the
scope it started from — the same reach a block-local alias has in real Rust, and no
wider.

Second: a file with many `#[cfg(..)]`-ambiguous aliases sharing one name, each
pointing into a different module, plus many struct literals of that name, took
seconds rather than milliseconds — [what is not checked](#what-is-not-checked)'s own
standard for this file's scanners. Two causes, both fixed. The search's own spending
limit was computed from the whole file on every construction site; `alias_search_budget`
computes it once per file instead, capped at a flat ceiling — `ALIAS_SEARCH_BUDGET_CEILING`
— since no real alias chain in this codebase needs more than a handful of hops. And the
search recomputed `own_aliases`/`own_modules` for the same scope from scratch on every
branch that revisited it; `AliasLookupCache`, shared across a whole file's search, computes
each scope's aliases and modules once and reuses them. `many_ambiguous_aliases_and_literals_resolve_quickly`
is the regression: forty modules, two hundred literals, real branching, held to a two-second
ceiling it clears in well under one.

A second review round found a third real problem, in the fix for the first one above. A
block-local `type`/`use` item shadows an enclosing generic type parameter of the same name
unconditionally in real Rust — confirmed against `rustc` — and `resolve_local_alias_chain`
chases it with no `shadow` check at all. Only the deterministic resolver's own fallback,
reached when no block-local alias exists at all, ever consults `shadow`. The block-items fix
above checked `shadow` before trying `block_items`, so a block-local alias sharing a name
with a generic parameter was refused instead of searched — the same "missed count" danger
the whole mechanism exists to close. `shadow` is now checked once, against the path's own
first segment, and gates only the module-scope half of a hop; a block-local alias is tried
regardless. `a_block_local_alias_still_resolves_when_its_name_shadows_a_generic_parameter`
and its control are the regression.

An automated review of the open pull request then found three more, all real. First: running
out of budget answered `false`, the same as a genuine "not reachable" — but a search that
exhausted its budget has shown nothing, not that `target` is unreachable, and answering
`false` is exactly the missed-count danger this whole mechanism exists to close. Exhausting
the budget now answers `true`. Second: once a hop found a live alias, a same-named sibling
module was never tried, even when none of that hop's aliases reached `target` — but an
unevaluated `cfg` can make a module and an alias of one name mutually exclusive the same way
it can two aliases, so a module is now tried too. Third: `self::Unchecked` names the
enclosing module, never a block-local item, in real Rust — but `self` does not move `scope`,
so a block-local alias still answered for a path real Rust resolves at module scope alone.
Whether the search may consult `block_items` at all is now decided once, from the shape of
the original path, not per hop. `exhausting_the_budget_counts_the_construction_rather_than_clearing_it`,
`a_module_is_still_tried_when_a_same_named_alias_did_not_reach_the_target`, and
`a_self_qualified_path_does_not_reach_a_block_local_alias` — each with its own control — are
the three regressions.

A further review of the same pull request found a fourth: `own_modules` can return more than
one sibling module of one name, declared under mutually exclusive `cfg` branches, the same
way `own_aliases` can return more than one alias — but module descent took the first match
and stopped there. Taking a hop's every same-named module as a further live branch needed
module descent to recurse rather than loop in place, the way an alias hop already did: once a
name can name more than one live module, "the one match" is no longer a thing a loop can just
step into and carry on from. `segments_could_reach_target` no longer loops at all — every hop,
alias or module, is its own recursive call now. `a_second_live_module_of_one_name_is_still_tried`
and its control are the regression.

The same review found two more, both in the fixes just above. First: a module-scope alias's
own target is resolved in the scope it was declared in, never the caller's block — but a
module-scope alias's recursive call kept the caller's own `block_eligible`, so a
construction site's own function-local alias could answer for a name the module-scope
alias's target never meant. `block_eligible` is now forced `false` for a module-scope
alias's own target; a block-local alias's own target keeps it, since that one really can
chain through a further block-local hop. Second: `self::T`/`super::T` explicitly names a
module's own item — a generic type parameter has no `self::`/`super::` form at all — but
`shadow` was checked after `self`/`super` had already been stripped, so an unrelated generic
parameter could suppress a search real Rust never lets it touch. `shadow` is now checked
once, in `path_could_reach_target`, against the path's own first segment exactly as written,
before any prefix is consumed. `a_module_scope_alias_does_not_reach_a_later_block_local_shadow`
and `a_self_qualified_path_reaches_a_live_live_module_ambiguity_despite_a_generic_shadow` —
each with its own control — are the two regressions; one earlier test
(`a_block_local_alias_still_applies_after_a_module_scope_hop`) rested on a premise real Rust
does not allow at all — a module-scope alias's target naming an item only a later, unrelated
function declares — and is replaced by the first of the two.

A further round found a fifth in the same family: a block-local alias's own target can itself
be `self::`/`super::`-qualified — `type B = self::A;`, declared beside a later, unrelated
`type A = CheckedDispatch;` in the same block — and `self::A` explicitly names the enclosing
*module*'s own `A`, never the block-local one, exactly the fact `path_could_reach_target`'s
own `block_eligible` already states of the original construction path. But a block-local
alias's own recursive call forwarded `block_eligible` unconditionally, with no check on
whether *its own target* carried that same qualification, so `B { .. }` was counted as
possibly reaching `CheckedDispatch` through the later block-local `A` — a name real Rust never
lets `self::A` see. `try_alias_candidates` now withholds `block_eligible` from a candidate
whose own target begins `self`/`super`, the same test every other qualified path in this
search is already held to. `a_block_local_alias_to_a_self_qualified_path_does_not_chain_through_a_shadow`
and its control, `a_block_local_alias_to_a_bare_name_still_chains_through_a_shadow`, are the
regression.

The same round found two more, both about the opposite danger from every fix before them:
not a missed count, but a name accepted before the search had actually shown it reaches
`target`. The first is in `try_alias_candidates`'s own fast path: it compared an alias
candidate's *resolved* segments against `target` by name and returned `true` on a match,
without asking whether that name was itself a further local alias whose own target
resolves elsewhere — `type CheckedDispatch = Decoy;` beside a `#[cfg(feature = "a")] type
Marker = CheckedDispatch;` made `Marker` count as reaching the guarded type under that
feature, even though `CheckedDispatch`'s own alias sends every real build to `Decoy`
instead. The fast path is now kept only for an *absolute* alias, whose own target reaches
past every local scope by construction — every other candidate recurses into
`segments_could_reach_target` instead, letting that function's own base case decide the
same way it already would for a name with no alias at all.
`a_module_scope_alias_whose_target_is_itself_shadowed_does_not_count` and its control,
`a_module_scope_alias_that_really_reaches_the_target_still_counts`, are the regression.

The second is a level up, in `segments_could_reach_target`'s own closing fallback: once
module descent had tried a same-named module and found it did not reach `target`, the
caller's own fallback still compared the *untouched, pre-resolution* segments' last piece
against `target` by name — so `traits::Marker`, whose own alias always resolves to
`Decoy`, counted as reaching a target literally spelled `"Marker"`, the tail of the path
as written, a name it never actually constructs. The fallback now fires only when nothing
already claimed `first` — no block-local alias, no module-scope alias, and no same-named
module — because a name any of those three explains is never a bare, direct reference,
whatever its own resolution turned out to answer.
`a_module_scope_alias_that_resolves_away_does_not_count_its_own_written_name` and its
control, `a_module_scope_alias_still_reaches_the_name_it_really_resolves_to`, are the
regression.

A further round found a sixth, and it is the opposite direction from the round before it —
a missed count rather than a false one. `block_eligible` was computed from the original
path's own segment count, so a plain (no `self`/`super`) *multi*-segment path never tried
a block-local alias for its own first segment — but a block-local `use good as traits;`
really does let `traits::Marker` reach `good::Marker`, confirmed against real `rustc`, and
this search missed it the same way `resolve_local_alias_chain` — the deterministic
resolver's own block-local chaser, scoped to one bare segment for an unrelated, older
reason — already does. `block_eligible` is now computed from whether the path's own first
segment is `self`/`super`, not from its length; the deterministic resolver stays as
narrow as it was, since this search is a backstop over it and widening what it alone can
find still counts every real construction. `a_block_local_alias_still_qualifies_a_further_segment`
and its control, `a_self_qualified_path_still_does_not_reach_a_block_local_alias_of_its_first_segment`,
are the regression.

The same round found a seventh, back to an over-count and reaching one hop deeper than the
round before it: once resolution had stepped into a module by name (issue #169's descent),
only `self` was ever stripped from a further alias target — a leading `super` was left
sitting in the segments as an ordinary token, so `traits::Marker`, whose own target is
`super::CheckedDispatch`, counted as reaching a target spelled `"CheckedDispatch"` even
when that name is itself only a root-scope alias for `Decoy`, confirmed against real
`rustc`: the construction is `Decoy`, never a distinct guarded type. `super` now escapes an
entered module back to the scope that named it — which `scope` already *is*, since module
descent by name never moves it, so escaping is stripping the one token rather than a
further decrement `consume_scope_prefix`'s own floor would refuse to take on a
single-level file. Scoped to one entered module only: `scope` cannot say which of several
nested parents a second `super` would need, and a deeper chain is left the same residual
`resolve_segments_from` — the deterministic resolver sharing this exact limitation — already
has. `a_super_qualified_alias_target_reached_through_module_descent_does_not_count` and its
control, `a_super_qualified_alias_target_that_really_reaches_the_target_still_counts`, are
the regression.

A further round found an eighth, and it is a missed count again: module descent only ever
read the *enclosing scope's* own items — `items`, from `stack`/`entered` — never
`block_items`, so a `mod` declared directly inside a function body was invisible to it,
even though a block-local *alias* of the same name was already tried right beside it.
Confirmed against real `rustc`: a block-local `mod` really is qualifiable from within its
own body. `own_modules` is generalized to take any `&syn::Item` iterator, the way
`own_aliases` already was (issue #92's post-merge review), so
`try_block_local_candidates` can search `block_items` for a same-named `mod` the moment a
block-local alias branch does not pan out — mirroring the outer scope's own
alias-then-module order, and unable to rule either out under an unevaluated `cfg` any more
than two aliases can rule each other out. `a_block_local_module_is_searched_for_a_qualified_path_head`
and its control, `a_block_local_module_does_not_count_an_unrelated_name`, are the
regression.

A further round found a ninth and a tenth together, and both are the same shape:
real Rust shadowing this backstop had never modelled at all, rather than a further
ambiguity to branch over. An *unconditional* (no `#[cfg]`) block-local `type`/`use`
declaration of a name completely shadows any same-named declaration further out — a
module-scope alias of the same name, or an outer block's own alias of the same name —
confirmed against real `rustc`: the shadowed declaration and its target type are both
"never used"/"never constructed" in the compiler's own diagnostics, and neither
`resolve_local_alias_chain`'s block-local search nor the backstop's own module-scope
section had ever asked whether a closer, unconditional declaration made the farther one
dead code. `live_block_declarations` is the fix, shared by both directions: it walks
`block_items` in reverse — innermost first, the order real Rust shadowing resolves in —
skipping any `#[cfg(test)]` item (test code is not shipped code, `own_aliases`'s own
reason), and stops at, and includes, the first *unconditional* (`!has_any_cfg`) match; an
unconditional match means every declaration further out is dead and reports so, and
running out with none found means module scope is still a live candidate exactly as
before. `try_block_local_candidates` and `try_block_module_candidates` now search only
the live slice this returns rather than the whole of `block_items`, and
`segments_could_reach_target` wraps its own module-scope alias-and-descent section behind
the "module scope still live" flag this walk reports, skipping it outright once a
block-local declaration has shadowed it — two mutually exclusive `#[cfg]`-gated
declarations of one name still count as separate live branches, exactly as an ambiguous
alias or module already does, because neither one alone is unconditional.
`an_unconditional_block_local_alias_shadows_a_module_scope_one` and its control,
`a_conditional_block_local_alias_still_lets_module_scope_through`, are the ninth's
regression; `an_unconditional_inner_block_alias_shadows_an_outer_block_one` and its
control, `a_conditional_inner_block_alias_still_lets_the_outer_one_through`, are the
tenth's.

A further round found an eleventh, and it is a second look at the seventh's own fix
rather than a new class of gap: `entered` had been modelled as a single `Option`, so a
`super` inside a module nested *two* deep — `mod a { type X = super::CheckedDispatch;
mod b { type Marker = super::X; } }` — escaped straight to the file's own top level
the moment it popped one level, skipping the immediate parent module (`a`) it actually
names, confirmed against real `rustc`: a live cfg-gated alias to `a::b::Marker` really
constructs `CheckedDispatch`, but the search resolved the inner `super::X` against the
root, found nothing there named `X`, and missed the construction — the seventh's own
documented residual, "one entered module only," restated for the case that residual
had named as out of reach. `entered` is now a stack, `&[&'a [syn::Item]]`, pushed onto
by every module-descent hop rather than replaced by it, so `super` pops exactly one
level and leaves any further-out entered module still in view for the hop after — the
deterministic resolver keeps its own single-`Option` shape and its own matching
residual, since this search is a backstop over it and widening what only the backstop
can find still counts every real construction.
`a_super_qualified_alias_target_reached_through_nested_module_descent_still_counts`
and its control,
`a_super_qualified_alias_target_reached_through_nested_module_descent_that_resolves_elsewhere_does_not_count`,
are the regression.

A further round found a twelfth, and it is a different class from every one before it:
not a missing branch under an unevaluated `cfg`, but a block-local alias's own target
resolved against the wrong scope entirely. A block-local `use good as traits;` declared
in an outer block, referenced from a nested inner block that later redeclares its own,
unrelated `mod good`, still names the *outer* `good` — confirmed against real `rustc`,
twice: once at crate scope and once at block scope, both printing the outer binding's
own marker. `block_items` had always been one flat, growing list — every enclosing
block's own items concatenated, with no record of which block declared which item — so
resolving an alias's own target reused the same flat list the *reference* site sees,
letting a block nested more deeply than the alias's own declaration shadow a name the
alias itself could never have resolved to. `resolve_local_alias_chain` (the
deterministic resolver) carried the identical bug, for the identical reason: both
functions treated every enclosing block as one merged scope rather than a stack of
separate ones, so the combined `resolved_elsewhere || path_could_reach_target(..)`
this search's own answer is `or`ed into missed the real construction either way — not
the narrower "the backstop lags the deterministic resolver" standing this family's own
docs excuse elsewhere, but a shared defect in the actual gate.
`block_items` is now a stack, `&[Vec<&'a syn::Item>]`, one entry per enclosing block
rather than one flattened list, threaded through both resolvers. `live_block_declarations`
pairs each live declaration with the depth it was found at, and both
`resolve_local_alias_chain` and `try_block_local_candidates` narrow the stack to
`blocks[..=depth]` before resolving that declaration's own target on the next hop —
bounding a further lookup to the declaration's own scope and everything enclosing it,
never a block only the reference site could see.
`a_block_local_alias_target_resolves_at_its_own_declaration_block_despite_a_later_shadow`
and its control,
`a_block_local_alias_target_shadowed_at_its_own_declaration_block_does_not_count`, are
the regression — both RED against the pre-fix code, in opposite directions (a missed
count and an over-count), confirming the flat list got both scenarios wrong rather than
merely one.

A further round found a thirteenth: only one leading `super` was ever stripped per hop,
so a target written `super::super::X` — from a module nested two deep by name — left the
second `super` as an ordinary segment no real declaration is ever spelled, confirmed
against real `rustc`: both tokens really do escape, landing at the scope that named the
outermost entered module. The stripping loop now keeps popping the entered-module stack
and removing a leading `super` until either runs out, rather than stopping after one;
once `entered` empties, a further `super` falls through to `consume_scope_prefix`, which
already loops over the lexical ancestor stack the same way.
`a_chain_of_two_leading_supers_escapes_both_entered_modules` and its control,
`a_chain_of_two_leading_supers_that_resolves_elsewhere_does_not_count`, are the
regression, RED against the pre-fix code.

The same round also raised a finding this search declines: that a `crate`-qualified
path should be resolved against the file's own top level before candidate lookup. It is
not taken up, because it is the identical fix this file's own parsing limits already
tried and reverted for the deterministic resolver (Codex review, PR #160, rounds 5 and
6): this scan reads one file and has no way to tell whether that file is really the
crate root, so treating its own top level as `crate`'s target is right only for the one
file that happens to be `lib.rs` and a guess everywhere else — the same over- vs
under-matching shape [what is not checked](#what-is-not-checked) already states for it.
Both resolvers already agree on the honest answer: `consume_scope_prefix` leaves
`crate` unconsumed, so a `crate`-qualified path is compared by its own untouched last
segment, exactly as `resolve_segments_from` does — this is a shared, deliberate limit
rather than a gap where the backstop trails the deterministic path. No new ADR: nothing
here moves a must-not-own cell, a dependency edge, or a rule id.

A further round found a fourteenth, on two Codex findings from PR #204 itself: an
unconditional module-scope alias or type declaration beside a `#[cfg]`-gated duplicate
of the exact same name, in the exact same scope, is not a live/live ambiguity the way
two declarations under mutually exclusive `cfg` flags are — confirmed against real
`rustc`, twice: the feature-off build compiles with only the unconditional declaration,
and the feature-on build fails outright with `E0428` ("the name `Unchecked` is defined
multiple times"), so the conditional declaration is never reachable in any real,
successful build and treating it as a second live candidate is an over-count, the
opposite of the missed-count direction this scanner's own fail-closed rule usually
guards against — and equally worth closing, since an over-count under an exact-count
gate rejects valid code rather than only missing a forgery. `declares_name` and
`live_named_items_in_scope` are the shared primitive: the latter scans a *whole* scope
for an unconditional match — rather than stopping at the first found walking in one
direction, which Codex's own sharper, block-scope finding shows depends on declaration
order for a question real Rust does not — and treats it, once found, as the only live
declaration of that name in the scope. `live_block_declarations` calls it once per
block depth, innermost to outermost, stopping at the first unconditional match; and
`AliasLookupCache` moves from per-scope to per-(scope, name) caching, since which
declarations are live is now itself a function of the name being looked up rather than
only of the scope. `try_module_scope_candidates` and its two new cache methods,
`live_aliases_of`/`live_modules_of`, are the module-scope backstop's own callers.

Fixing only that backstop left the block-scope finding open, and a RED test caught it
before the fix was ever committed: `struct_literal_counts`'s own *deterministic*
resolvers — `resolve_local_alias_chain` at block scope and `resolve_segments_from` at
module scope — had always picked among several same-named declarations by declaration
order alone (last-declared wins at block scope, first-declared wins at module scope),
with no regard for which one a real build could ever have. So an unconditional
declaration textually *after* a `#[cfg]`-gated duplicate was silently outvoted by one
that can never coexist with it — and because the visitor consults the fail-closed
backstop only when the deterministic answer disagrees with the target, a wrongly
confident deterministic pick short-circuits before the backstop is ever asked. The
module-scope regression test had passed the backstop alone only because its own
declaration order happened to already agree with `.find()`'s first-match rule — the
unconditional alias was written first — not because the resolver was correct; reversing
that order reproduces the identical bug one scope up. `preferred_alias` is the fix,
shared by both resolvers: within one scope, it prefers an unconditional declaration
outright over every conditional one, falling back to each resolver's own prior
tie-break only when none is unconditional — a case still genuinely ambiguous under a
`cfg` this scanner cannot evaluate, and still left to `path_could_reach_target`'s own
separate, fail-closed search to catch what one deterministic pick still might miss.
`an_unconditional_module_scope_alias_excludes_a_same_scope_cfg_duplicate` and
`an_unconditional_block_local_alias_excludes_a_same_scope_cfg_duplicate_regardless_of_order`
are the regression, both RED against the pre-fix code — the first by coincidence of
ordering once `preferred_alias` did not yet exist, the second unconditionally. No new
ADR: nothing here moves a must-not-own cell, a dependency edge, or a rule id.

A further round found a fifteenth, and it is a different class from the fourteenth's
own two findings: not a missing branch under an unevaluated `cfg`, but a namespace this
scanner has no way to read. `declares_name` and `preferred_alias` both treated a `use`
item exactly as they treat a `mod` or a `type` alias, but a `use` can import a *value*,
and real Rust lets a value share a name with a module with no collision at all —
confirmed against real `rustc`: `use values::traits;`, importing a function named
`traits`, compiles cleanly beside `mod traits { .. }` in the same scope, where two
`type` aliases or two `mod`s of one name would be a real `E0428`/`E0255`. So an
unconditional value import was wrongly read as the scope's one live declaration of
`traits`, and `live_named_items_in_scope` excluded the module entirely — a missed
count, the fourteenth round's own danger direction, reached through a route the
fourteenth round's own fix had not closed. `is_namespace_unambiguous` is the fix: only
a `mod` or a `type` alias is ever provably in the type namespace, so only one of those
may now be the unconditional winner `live_named_items_in_scope` treats as exclusive,
and only one of those may be excluded by such a winner — a `use` match is always kept
as an independently live candidate, the same residual [what is not
checked](#what-is-not-checked) already states for a same-spelled alias across
namespaces, restored here rather than silently narrowed away by the fourteenth round's
own exclusion logic. `preferred_alias` takes the identical guard, so its own single
deterministic pick cannot be short-circuited by an unconditional `use` either — a
weaker requirement than the backstop's, since callers built on it already carry the
documented live/live-ambiguity residual, but left inconsistent with a stale doc comment
otherwise. `an_unconditional_value_use_does_not_shadow_a_same_named_module` and its
control, `an_unconditional_value_use_does_not_count_an_unrelated_name`, are the
regression, confirmed RED against the pre-fix code (`total: 0` against an expected `1`)
before this fix landed. No new ADR: nothing here moves a must-not-own cell, a
dependency edge, or a rule id.

Issue #153 revisits ADR 0010, the way its own text said a revisit would have to: "a profile
of a real workload... showing the checksum on the critical path". `cargo xtask profile`'s
four workloads put `crc16` and `crc32` at 33–49% of engine-attributed host instructions in
every workload that runs them. Two independent lines of work answered it at once, without
either knowing about the other: one rewrote both checksums — `crc16` as a closed-form
nibble multiply, `crc32` as a per-nibble lookup table — and the other, on `main`, reached
`crc16`'s identical multiply-fold independently and then measured `crc32`'s own table
against ADR 0010's stricter bar — "a profile of a real workload **on real flash**, against a
latency requirement §04 does not currently state" — found neither exists, declined the
table, and instead gave `crc32`'s bitwise loop a branchless, masked rewrite of its own eight
rounds
([ADR 0046](docs/adr/0046-crc16-folds-its-nibble-round-to-a-multiply-crc32-stays-bitwise.md)).
Merging the two found they could not both ship — one crate computes a checksum one way —
and what decided between them was a measurement neither prior line had: the table,
remeasured against `main`'s own branchless loop rather than the bitwise loop it replaced.
It still wins, by a fifth to a quarter of engine-attributed instructions on every workload
that runs it (`journal` -23.2%, `driver` -20.5%, `facade` -14.5%, `conformance` unaffected),
so
[ADR 0053](docs/adr/0053-a-crc32-nibble-table-still-beats-the-branchless-loop.md)
supersedes ADR 0046's `crc32` clause alone — its `crc16` clause is untouched, confirmed
rather than assumed, since the multiply-fold measures identically whichever `crc32`
treatment runs beside it. `crc32_nibble_table`'s sixteen-armed `match` over a `crc32_nibble`
helper is spent as a `match` rather than a `[u32; 16]`: this workspace denies
`indexing_slicing` unconditionally and `<[T]>::get` is not const-stable on the pinned
toolchain, so an actual array was unreachable without giving up either the lint or `crc32`
remaining a `const fn`, and a `match` whose every arm is a distinct compile-time constant is
what lets LLVM's own switch-to-lookup-table pass build the same table anyway — confirmed by
disassembling the built object rather than assumed. Getting the `match` to actually compile
into a table needed `#[inline(always)]` on both the table function and the helper it calls,
each with a documented `#[allow(clippy::inline_always)]`: a softer `#[inline]` left the
switch un-inlined into `crc32`'s loop and measured as a real function call per nibble,
worse than the loop it replaced. `xtask::source::INTEGRITY_CHECK_TABLES` is the structural
pin the `integrity-check` gate rule holds `crc32_nibble_table`'s shape to, because no array
ever appears in `crc.rs` for the pre-existing array ban to see: each of `crc32_nibble(0)`
through `crc32_nibble(15)` exactly once, and `crc32_nibble(` exactly sixteen times in
total, so a seventeenth arm cannot hide behind the other sixteen being correct.
`INTEGRITY_CHECK_PARAMETERS`'s two polynomial rows retarget to the `crc16_nibble`/
`crc32_nibble` helpers, where each polynomial now lives; the initial-value and final-xor
rows are unmoved. What is owed is written down in ADR 0053 rather than implied: the
instruction-count figures are host-side and convert to no cycle count on any part, §04
still states no latency budget for a checksum to be on the critical path of, and the
"compiles to a table load" claim is a disassembly with reproduction steps rather than
something CI re-derives on every run.

Merging this branch (issue #197's fifteenth round, above) with `main`'s own issue #153 and
#189 work found one real collision, in code neither line of work knew the other had
touched. Both independently met the same build break — `generic_assoc_type_bindings_naming`
calling `resolve_segments`/`resolve_segments_from` with two arguments after issue #181
widened both to take a third, `shadow` — and fixed it two different ways: this branch by
adding real shadow tracking to its own, function-local `AssocBindings` visitor, and `main`
by the same shadow tracking plus five further Codex-found rounds (struct, enum, union,
`type`-alias and trait-alias generics; a generic associated type's own generics; a trait
alias's generics) hardening it, and by lifting the whole visitor to a top-level `struct`
to clear `clippy::too_many_lines` — the identical fix this branch made for
`struct_literal_counts`'s own `Literals` visitor, for the identical lint. Git's own
three-way merge could not reconcile the two `AssocBindings` definitions and produced a
literal `E0428` duplicate rather than a text conflict, because `main`'s version landed
whole in a stretch of the file this branch's own diff never touched. The merge keeps
`main`'s top-level placement and its five additional generic-shadow overrides — no reason
to have less shadow tracking than a version already reviewed for it — with one change:
`block_items` stays this branch's own `Vec<Vec<&'ast syn::Item>>` stack rather than
`main`'s flat `Vec<&'ast syn::Item>`, because [`resolve_local_alias_chain`]'s own
signature is the stack shape issue #197's twelfth round gave it, and a caller passing the
older, flat shape would not compile at all. No behavior changed beyond what each branch
already reviewed on its own: `cargo test -p xtask --locked --lib` (1984 passed),
`cargo clippy -p xtask --locked --all-targets -- -D warnings`, `cargo fmt --all --check`
and `cargo xtask check-layering` (57 rules) are all clean on the merged tree. No new ADR:
nothing here moves a must-not-own cell, a dependency edge, or a rule id.

Codex review of the merge commit found a sixteenth: the `shadowed` fast path's own
fallback ignored a block-local search that had already run and failed. Confirmed against
real `rustc`: `struct T; struct Decoy; fn forge<T>() { type T = Decoy; let _ = T {}; }`
constructs `Decoy`, because an unconditional block-local `type T = Decoy;` shadows the
generic parameter `T` completely — the same way an unconditional block-local declaration
already shadows module scope — but `segments_could_reach_target`'s `if shadowed { return
segments.last() == target; }` compared the untouched, pre-resolution text and never
consulted `resolved_elsewhere` or `module_scope_shadowed`, so it counted a construction
the block-local search directly above it had just shown reaches `Decoy`, never the
guarded type — an over-count, the same direction the fourteenth round's own finding was.
The fix gates the fallback on `!module_scope_shadowed` rather than on `resolved_elsewhere`
directly: `module_scope_shadowed` is already the fact that the closest live block-local
declaration is unconditional, so the generic-parameter reading is dead code in every
build, not only the one a conditional block-local alias happens to resolve under — a
`resolved_elsewhere` gate alone would have made a merely `#[cfg]`-conditional shadow
suppress the generic-parameter branch too, a missed count under whichever build the
conditional alias is absent from, which
`a_block_local_alias_still_resolves_when_its_name_shadows_a_generic_parameter`'s own
conditional fixture already requires to keep counting.
`an_unconditional_block_local_alias_shadows_a_generic_parameter_away_from_the_target` and
its control, `an_unconditional_block_local_alias_that_really_reaches_the_target_still_counts`,
are the regression, the first confirmed RED against the pre-fix code (`total: 1` against
an expected `0`) before this fix landed. No new ADR: nothing here moves a must-not-own
cell, a dependency edge, or a rule id.

Codex review of that same commit found a seventeenth, in a different primitive: a plain
`struct`/`enum`/`union`/`trait` declaration was invisible to `declares_name` entirely, so
neither `live_named_items_in_scope` nor `preferred_alias` ever saw it competing for a
name at all. Confirmed against real `rustc`: `mod m { pub struct Unchecked; #[cfg(feature
= "a")] pub type Unchecked = Decoy; #[cfg(feature = "b")] pub type Unchecked =
CheckedDispatch; }` can only ever compile with neither feature enabled — enabling either
collides with the unconditional struct (`E0428`) — so neither `#[cfg]`-gated alias is ever
a live branch, yet both were treated as live and the `CheckedDispatch` one counted, an
over-count in the same direction as the fourteenth and sixteenth rounds' own findings.
`declares_name` and `is_namespace_unambiguous` now also recognize a `struct`, `enum`,
`union` or `trait` declaration of the name, exactly as they already recognize a `mod` or a
`type` alias — all six are unambiguously in the type namespace, so two of any of them
sharing one name in one scope are a real `E0428`/`E0255` the same way. Since neither
`own_aliases` nor `own_modules` ever produces an entry for a plain `struct`/`enum`/
`union`/`trait`, becoming the scope's one live, namespace-unambiguous declaration this way
correctly excludes every conditional alias of the name from `live_aliases_of` without
itself ever appearing as a candidate to chase — the search falls through to comparing the
name directly, which is what a real build actually does.
`an_unconditional_struct_shadows_conflicting_cfg_gated_type_aliases_of_one_name` and its
control, `a_conflicting_type_alias_still_counts_with_no_competing_struct`, are the
regression, the first confirmed RED against the pre-fix code (`total: 1` against an
expected `0`) before this fix landed. No new ADR: nothing here moves a must-not-own cell,
a dependency edge, or a rule id.

CI then went red on the commit above, over `many_ambiguous_aliases_and_literals_resolve_quickly`'s
own 2-second ceiling: 2.229s measured on the runner. Investigated rather than assumed a
flake — the same sandbox measured 1.6-2.3s across a handful of runs at every commit
checked back to the fixture's own original one, including the commit before either of
this issue's two review-round fixes above touched this file, so the margin was already
this tight and neither round's own change is what narrowed it. The ceiling needed
headroom, not the fixture: the bug this test exists to catch — a per-construction-site
budget recomputed from the whole file rather than once per file — measured 10-23s, an
order of magnitude past even the slowest run seen here, so raising the ceiling to 6s
still separates "fixed" from "regressed to the shape this test was written to catch"
with real margin on both sides, rather than narrowing what the test can distinguish. No
new ADR: nothing here moves a must-not-own cell, a dependency edge, or a rule id.

Codex review of PR #204's commit before that CI fix found an eighteenth: `type_alias_target`
discarded a `type` alias's own leading `::` entirely, and both its callers —
`collect_item_aliases` and `own_aliases` — recorded `absolute: false` for every `type`
alias unconditionally, never reading it. Confirmed against real `rustc`: `type Unchecked =
::core::ops::Range<u8>;` reaches the extern prelude's own `core::ops::Range` directly,
past every local scope, exactly as `use ::a::b as c;` already does (issue #92's own
`absolute` field, added for exactly this shape) — but a local `mod core { pub mod ops {
pub type Range = CheckedDispatch; } }` sharing the crate's own name was chased as though
it might be what the alias really named, because nothing here ever told
`try_alias_candidates`'s `if alias.absolute` short-circuit that this alias's target had a
leading `::` at all. `type_alias_target` now returns the target segments paired with
whether the path had one, and both callers thread it into `UseAlias::absolute` instead of
a literal `false`. `an_absolute_type_alias_target_does_not_chase_a_same_named_local_module`
and its control, `a_relative_type_alias_target_still_chases_a_same_named_local_module`,
are the regression, the first confirmed RED against the pre-fix code before this fix
landed. No new ADR: nothing here moves a must-not-own cell, a dependency edge, or a rule
id.

Codex review of that same commit found a nineteenth, back in module descent rather than in
an alias: `preferred_alias` correctly answers `None` when the head name's unconditional
winner is a concrete type — a `struct`, `enum`, `union` or `trait`, none of which
`own_aliases` ever turns into an alias — but `resolve_segments_from`'s own caller read that
`None` as "no matching alias" and fell straight through to `own_modules`, which finds a
`#[cfg]`-gated `mod` of the same name with no regard for the unconditional type sitting in
the same scope. Confirmed against real `rustc`: `use values::m;` (a value import, no
collision) beside `enum m { Marker { x: u8 } }` compiles cleanly, but adding
`#[cfg(feature = "a")] mod m { type Marker = super::CheckedDispatch; }` makes feature `a` a
duplicate-definition error (`E0428`) — the module can never exist in any build that
compiles — yet `m::Marker { x: 0 }` in the valid, feature-off build was resolved through
that impossible module and counted as `CheckedDispatch`, an over-count in the same
direction as the fourteenth, sixteenth and seventeenth rounds' own findings. The fix
filters `own_modules`'s own candidates through `live_named_items_in_scope` first — the same
primitive `AliasLookupCache::live_modules_of` already wraps for the fail-closed backstop
search, now reused by the deterministic resolver's own module descent, so a `mod` an
unconditional concrete type of the same name has already ruled out is never a live
candidate to step into. `an_unconditional_concrete_type_excludes_a_cfg_gated_module_of_one_name`
and its control, `a_cfg_gated_module_still_counts_with_no_competing_concrete_type`, are the
regression, the first confirmed RED against the pre-fix code (`total: 1` against an
expected `0`) before this fix landed. No new ADR: nothing here moves a must-not-own cell, a
dependency edge, or a rule id.

Codex review of that same commit found a twentieth, on the opposite face of the nineteenth's
own fix: `preferred_alias` picks one candidate, `chosen`, as the type-namespace winner for a
name, and when `chosen` is a concrete type that yields no alias, the function had always
returned `None` outright — discarding a genuinely live `use` of the same name in the
*value* namespace, which real Rust lets coexist with a type-namespace declaration with no
collision at all. Confirmed against real `rustc`: `use values::forbidden as allowed; mod
allowed {}` compiles cleanly — a value import and a module never collide — and a call
`allowed()` names the value import, never the module; but `preferred_alias` answering `None`
left `resolve_segments_from` and `resolve_local_alias_chain` substituting nothing, so the
call resolved to the bare, unaliased name `allowed` instead of `values::forbidden`, hiding
whatever a caller (`resolved_path_uses`, `name_uses`, and every construction pin built on
`struct_literal_counts`) was really looking for behind an alias. The fix cannot simply
always fall back to a `use` candidate once the type-namespace winner yields nothing, though:
real Rust's own grammar says a path segment followed by another can only ever name a module,
a type, an enum or a trait — never a plain value — so `allowed::Marker` can only mean the
module, and substituting the value alias there would trade a missed resolution for a wrong
one. `preferred_alias` now takes a `terminal` flag — whether the segment it is resolving is
the *whole* remaining path, computed by each caller from its own `segments.len() == 1` — and
tries a `use` candidate as a fallback only when `terminal` is true, which is exactly the
condition under which a value-namespace answer is ever the correct one syntactically, not a
guess between two live possibilities the way this file's own parsing limits already refuse
elsewhere. `a_value_namespace_alias_is_not_suppressed_by_a_same_named_module` and its
control, `a_qualified_path_through_the_same_name_still_names_the_module`, are the
regression, the first confirmed RED against the pre-fix code (resolving to the bare name
`allowed` rather than `values::forbidden`) before this fix landed, the second confirmed
green against both the pre-fix and the fixed code alike, since the multi-segment case was
never broken. No new ADR: nothing here moves a must-not-own cell, a dependency edge, or a
rule id.

Codex review of that same commit found a twenty-first, in the same shape as the seventeenth
and nineteenth: `extern crate self as m;` binds `m` in the type namespace exactly as a `mod`
or a `type` alias does, but `declares_name` and `is_namespace_unambiguous` never recognized
`Item::ExternCrate` at all, so it was invisible when deciding whether a `#[cfg]`-gated `mod
m` of the same name could ever coexist with it. Confirmed against real `rustc`:
`extern crate self as m;` beside `#[cfg(feature = "a")] mod m { .. }` is a duplicate-
definition error the moment feature `a` is enabled, so the module can never exist in any
build that compiles, yet it was still treated as a live branch and its construction
counted. `declares_name` now reads an `extern crate`'s own bound name — its `as` rename when
one is written, its crate name otherwise — and `is_namespace_unambiguous` now recognizes it
too, the same way both already recognize a `struct`/`enum`/`union`/`trait`. Since neither
`own_aliases` nor `own_modules` ever produces an entry for an `extern crate`, becoming the
scope's one live declaration this way correctly excludes a conflicting `#[cfg]`-gated module
without itself ever being chased into.
`an_unconditional_extern_crate_alias_excludes_a_cfg_gated_module_of_one_name` and its
control, `a_cfg_gated_module_still_counts_with_no_competing_extern_crate_alias`, are the
regression, the first confirmed RED against the pre-fix code (`total: 1` against an expected
`0`) before this fix landed. No new ADR: nothing here moves a must-not-own cell, a dependency
edge, or a rule id.

Codex review of that same commit found a twenty-second, and it is a real gap in the
twentieth round's own fix rather than a new class of bug: `terminal` means "no further path
segment follows", never "this reference could be a value" — a struct-literal head like
`Allowed { .. }` is terminal too, and always type-namespace, so the twentieth round's
value-namespace fallback wrongly applied there as well. Confirmed against real `rustc`: `use
values::CheckedDispatch as Allowed; struct Allowed { .. }` compiles — `values::CheckedDispatch`
is a function, so the value import and the struct occupy different namespaces — but `Allowed
{ .. }` constructs the local struct, never the function, and the fallback substituted the
value alias anyway, over-counting a guarded type that was never really built. Worse, the
same shape reached the fail-closed backstop too: `AliasLookupCache::live_aliases_of` and
`live_block_declarations` — used exclusively by `struct_literal_counts`'s own construction-
path search — inherited `live_named_items_in_scope`'s "always keep a `use` live" rule from
the fifteenth round, which is the right general answer for a scanner that cannot tell value
from type position, but categorically wrong for this one caller, whose path can never be
value-namespace at all. `resolve_segments`, `resolve_segments_from` and
`resolve_local_alias_chain` now take a `value_position` flag, stated once by each entry
point from what kind of path it is resolving rather than guessed from segment count:
`struct_literal_counts`'s own construction-path resolution, `generic_assoc_type_bindings_naming`'s
associated-type-binding resolution, and `future_trait_implementors`'s and
`resolve_impl_trait_path`'s trait-path resolution all pass `false`, since none of the three
can ever denote a value in real Rust's own grammar; `resolved_path_uses`'s and `name_uses`'s
own general path scans pass `true`, keeping the twentieth round's fix for a call's own
callee and the same-spelled-alias-across-namespaces residual those two scans already carry
for their remaining, undistinguished paths. `live_named_items_in_scope` takes the identical
flag, and `live_aliases_of`/`live_modules_of`/`live_block_declarations` — reached only from
`struct_literal_counts`'s own search — always pass `false`, dropping a `use` once an
unconditional namespace-unambiguous winner exists, the same as if it were namespace-
unambiguous too. `a_struct_literal_head_does_not_fall_back_to_a_value_namespace_alias` is the
regression for the demonstrated case, confirmed RED against the pre-fix code (`total: 1`
against an expected `0`) before landing; `an_impl_trait_path_does_not_fall_back_to_a_value_namespace_alias`
covers the same fix applied proactively to the trait-path call sites, on the identical
categorical reasoning, confirmed RED the same way before landing. No new ADR: nothing here
moves a must-not-own cell, a dependency edge, or a rule id.

Codex review of that same commit found a twenty-third, and it is a gap in the twentieth
round's own value-namespace fallback that neither the twenty-first nor the twenty-second
round closed: the fallback substitutes the first or last `use` candidate it finds with no
regard for whether an *unconditional* value-namespace winner already sits in the same
scope. A unit or tuple struct's own name is bound in the value namespace too, as the
implicit constructor real Rust generates for either shape — `struct Allowed;` or
`struct Allowed(u8);`, each callable as a value — unlike a record/braced struct, which has
no constructor and stays type-namespace-only. Confirmed against real `rustc`:
`struct Allowed(u8);` beside `#[cfg(feature = "a")] use values::forbidden as Allowed;`
compiles only with the feature off, where `Allowed(0)` calls the tuple-struct constructor;
enabling the feature collides in the value namespace (E0255), so the `use` can never be
live wherever this struct compiles. `resolved_path_uses` still passed `terminal = true` for
a call's own callee, and the fallback rewrote the feature-off call to `values::forbidden` —
a name no compiling configuration of it ever reaches, exactly the "unconditional winner
excludes a conditional duplicate" pattern the fourteenth, seventeenth, nineteenth and
twenty-first rounds each closed for the type namespace, met here in the value namespace
instead. `is_unit_or_tuple_struct` is the new primitive, matching `syn::Item::Struct` with
`Fields::Unit` or `Fields::Unnamed` — a record struct's `Fields::Named` is excluded, since
it binds no value at all. `preferred_alias` now tracks whether its own strict, unconditional
find (as opposed to the `prefer_last` fallback pick used when no unconditional winner
exists) produced the chosen item, and when that unconditional winner is a unit or tuple
struct, the value-namespace fallback never runs at all — the struct's own constructor
already is the position's one live answer, and a competing `use` is either impossible code
(if it too is unconditional) or dead code under every configuration that also compiles the
struct (if `#[cfg]`-gated), never a second live candidate to substitute.
`a_unit_or_tuple_struct_excludes_a_cfg_gated_value_alias_of_one_name` is the regression,
confirmed RED against the pre-fix code (a call rewritten to `values::forbidden` in a
feature-off build) before landing; `a_named_field_struct_does_not_suppress_a_value_alias_of_one_name`
is the control, confirmed the fallback still substitutes a value alias when the
unconditional winner is a record struct, which binds no value for it to compete with. No
new ADR: nothing here moves a must-not-own cell, a dependency edge, or a rule id.

Codex review of that same commit found a twenty-fourth, and it is a gap in the twenty-third
round's own fix rather than a new class of bug: the twenty-third round's check only
recognized a unit or tuple struct as an unconditional value-namespace winner, but a free
function, a `const` and a `static` each bind their name in the value namespace
unconditionally too — and, sharper still, none of the three had ever been recognized by
`declares_name` at all, so a plain `fn allowed() {}` could not even become a candidate,
let alone shadow a competing `use`. Confirmed against real `rustc`: `fn allowed() {}`
beside `#[cfg(feature = "a")] use values::forbidden as allowed;` compiles only with the
feature off, where `allowed()` calls the local function; enabling the feature collides in
the value namespace (E0255), so the `use` can never be live wherever the function is
unconditional — yet with the function invisible to `declares_name`, the tie-break between
`chosen` and the terminal fallback's own `uses` pick had nothing stopping it from landing
on the `use` instead, rewriting a feature-off call to `values::forbidden`, a name no
compiling configuration of it ever reaches. `declares_name` now also recognizes
`Item::Fn`, `Item::Const` and `Item::Static`, each by its own identifier; a new
`is_unconditional_value_declaration` generalizes the twenty-third round's
`is_unit_or_tuple_struct` check to cover all four shapes, and `preferred_alias`'s new
check runs *before* the `chosen` tie-break is ever computed — not only after, the way the
twenty-third round's narrower, struct-only check did — because with the function now a
candidate, the tie-break itself could pick the `use` directly and return its alias before
ever reaching a later check. Widening `declares_name` was checked against every other
caller it feeds (`preferred_alias`'s own candidate filter, `live_named_items_in_scope`'s
three callers, and `resolve_segments_from`'s module-descent live-item filter): all five
only ever extract a `use`/`type` alias or a `mod` from what they're handed, and neither a
`fn`, a `const` nor a `static` produces anything for either extraction, so becoming
visible as a *candidate* changes nothing about what those callers do with one. A foreign
function or `static` declared inside an `extern` block is the same shape once more but is
left unrecognized: `declares_name` compares one item against one name, and a
`syn::Item::ForeignMod` names none of its own — it holds a list of `ForeignItem`s, each
with a name of its own — which needs machinery this function does not attempt, stated as a
residual rather than chased further.
`an_unconditional_function_excludes_a_cfg_gated_value_alias_of_one_name` and
`an_unconditional_const_excludes_a_cfg_gated_value_alias_of_one_name` are the regressions,
each confirmed RED against the pre-fix code — with the competing `use` declared *before*
the function/`const` on purpose, since `resolve_segments_from`'s own tie-break prefers the
first candidate when nothing else decides it, and a naive test with the declaration order
reversed would have passed by coincidence of that tie-break rather than by the fix. No new
ADR: nothing here moves a must-not-own cell, a dependency edge, or a rule id.

Codex review of that same commit found a twenty-fifth, and it is a real gap — confirmed
against real `rustc` — but not one this round fixes. `path_could_reach_target`'s own search
treats two same-named declarations under mutually exclusive `#[cfg]` flags as separate live
branches whenever neither is provably unconditional, an accepted residual (this scanner
cannot evaluate `cfg`). What it had never asked is whether a candidate's own `cfg` can even
coexist with the *construction site's* own enclosing `cfg`: `#[cfg(not(feature = "a"))] type
Unchecked = Decoy; #[cfg(feature = "a")] type Unchecked = CheckedDispatch;` beside a
`#[cfg(not(feature = "a"))] fn forge() { let _ = self::Unchecked { .. }; }` can never
construct `CheckedDispatch` from `forge` in any build — `forge` itself only exists where
`feature = "a"` is off, and in that exact build the `feature = "a"` alias does not exist
either — yet the search counted it regardless of `forge`'s own gating. Investigated rather
than fixed: closing it soundly needs four new pieces at once, not a completion of what
exists — a general `Cfg`-vs-`Cfg` satisfiability check beside `Cfg::requires_test` (which
only ever answers "does this formula entail `test`", never "can these two formulas both
hold"); enclosing-`cfg` accumulation through `struct_literal_counts`'s own visitor, a stack
discipline it does not currently keep at all; the check has to run while a candidate's
source `syn::Item` (and its own `attrs`) is still in hand, before `own_aliases`/`own_modules`
erase it into a `UseAlias`/a bare item slice with no `cfg` attached; and `AliasLookupCache`'s
own cache key would have to widen past `(scope, name)`, since liveness would no longer be a
pure function of those two things — and a wider key risks reintroducing the exact
O(file-size)-per-construction-site cost issue #197 fixed
(`many_ambiguous_aliases_and_literals_resolve_quickly`'s own regression), because two call
sites with genuinely different enclosing `cfg` could no longer share one cached answer the
way most calls in one file do today. Getting any one of the four wrong in the *exclude*
direction is a missed count — the opposite failure mode from the over-count this finding
itself reports, and the one this whole mechanism exists to avoid above all else. Filed as
issue [#206](https://github.com/madmax983/waymaker/issues/206) rather than chased under
review pressure, the same bar issues #171/#186/#193 were opened at: a real finding whose fix
needs new machinery across several pieces rather than a narrow, provably-correct change. No
new ADR: nothing here moves a must-not-own cell, a dependency edge, or a rule id.

Codex review of that same commit found a twenty-sixth, and it is a real regression in the
twenty-fourth round's own fix — the sharpest kind, since it is `resolved_path_uses`/
`name_uses` themselves that carry it, not the narrower fail-closed backstop. Those two scans
pass `value_position = true` for *every* path they visit, type-position and value-position
alike — an already-accepted, documented residual, since telling the two apart there would
need the same per-syntactic-role machinery round 22 gave `struct_literal_counts` and its
three siblings but deliberately did not give these two general scans. The twenty-fourth
round's own check fired on any unconditional value declaration among the candidates,
regardless of whether the competing `use` it would have suppressed was itself conditional —
so it could not tell `fn allowed() {}` beside a genuinely `#[cfg]`-gated `use` (its own
demonstrated case, where suppressing is correct) from `fn Allowed() {}` beside a wholly
*unconditional* `use core::fmt::Debug as Allowed;` (where it is not): confirmed against real
`rustc`, the second pair compiles cleanly — a trait import and a function occupy different
namespaces with nothing conditional about either one — and a trait bound `T: Allowed` still
means `core::fmt::Debug`, never the function. Before the twenty-fourth round even existed
this resolved correctly, because a plain `fn` was invisible to `declares_name` and the `use`
was the only candidate; the twenty-fourth round's own widening is what put a second,
unrelated candidate in the running and let its check treat both cases alike. The fix folds
the value-declaration check into the very same `unconditional_winner` find that already
decides `chosen` — so `chosen` can never land on an arbitrary `prefer_last`/`first` tie-break
between a value declaration and a same-named `use`, the exact failure mode a check placed
only *after* `chosen` was already picked could not prevent — and narrows the terminal
fallback's own suppression to fire only when the specific `use`/`type` candidate it would
otherwise pick is itself `#[cfg]`-gated: an *unconditional* competing alias is a namespace
this scanner already knows is distinct, proven by the fact that both compiled together at
all, so substituting it is exactly as safe an answer as this fallback has always given.
`uses` itself is narrowed too, from "not namespace-unambiguous" to exactly `Item::Use`/
`Item::Type` — the only two kinds `own_aliases` ever produces anything for — since leaving a
function or a `const` in that pool made which item an ordering-dependent pick landed on
matter in a way it should not: `own_aliases` never resolves either one, so a `.first()`/
`.last()` tie-break that happened to land on the function instead of the genuine `use` was
silently losing the correct answer to declaration order alone.
`an_unconditional_value_alias_still_resolves_past_an_unconditional_function` is the
regression, confirmed RED against the pre-fix code (resolving to the bare name `Allowed`
rather than `["core", "fmt", "Debug"]`) before landing; the twenty-fourth round's own two
tests were re-run alongside it and stayed green, confirming the narrower suppression still
fires exactly where that round's own repro needs it to. No new ADR: nothing here moves a
must-not-own cell, a dependency edge, or a rule id.

Codex review of that same commit found a twenty-seventh, and it is a different dimension
from the twenty-fourth's and twenty-sixth's own: not whether a value declaration should
*suppress* a competing `use` in a value (terminal) position, but whether a value-*only*
declaration should be in the candidate pool at all in a position that can never be a value
in the first place. `future_trait_implementors` resolves a trait path with
`value_position = false` always, so `terminal` is `false` at every call
`preferred_alias` ever makes from it — but the twenty-fourth round's own widening of
`declares_name` to recognize `fn`/`const`/`static` at all put a plain, unrelated function
into `candidates` regardless of position, and with no namespace-unambiguous winner among
the candidates to prefer, the arbitrary `prefer_last`/`first` tie-break could still land
on it: `own_aliases` never produces an alias for a function, so the pick found none,
and — being outside a terminal position — `preferred_alias` returned `None` immediately,
never trying the genuine `use`/`type` candidate the fallback exists to find. Confirmed
against real `rustc`: `fn Allowed() {}` beside `use core::future::Future as Allowed;`,
referenced as `impl Allowed for Real {}`, compiles and always means
`core::future::Future` — a function and a trait import occupy different namespaces, with
nothing conditional about either — and before the twenty-fourth round even existed this
resolved correctly, because a plain `fn` was invisible to `declares_name` and the `use`
was the only candidate. The fix is a fourth filter on `candidates` itself, ahead of every
other check: a value-only declaration (a free function, a `const` or a `static`, via a new
`is_value_only_declaration` — narrower than `is_unconditional_value_declaration`, since a
unit/tuple struct's own constructor is still a value the terminal fallback can legitimately
answer with) is dropped from the pool outright whenever `!terminal`, rather than left in it
to win an ordering-dependent tie-break in a position it is categorically irrelevant to.
`a_value_only_declaration_does_not_win_a_type_only_tie_break` is the regression, confirmed
RED against the pre-fix code (`implementors: []` against an expected `["Real"]`) before
landing, with the function declared first to match the shape that actually loses under the
old tie-break; `a_use_declared_first_still_resolves_past_a_later_value_only_declaration` is
the control, confirming the fix is not merely papering over one declaration order. No new
ADR: nothing here moves a must-not-own cell, a dependency edge, or a rule id.
