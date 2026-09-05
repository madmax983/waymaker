# CLAUDE.md

Waymaker is a firmware-first durable workflow engine for Rust. A workflow is re-created from
its beginning after reboot and deterministically replayed through an ordered journal:
completed effects return their recorded results, and the first unresolved effect becomes the
next piece of work.

This file is what a contributor — human or agent — works to. It states the invariants, the
layering rules, and what each crate must not own.

Much of it is checked rather than remembered: the must-not-own cells, the permitted
dependency edges, the eight decision ids, the command list, the five deferred questions and
all 37 rule ids below are compared against the tables that own them, and `cargo xtask check-layering` fails a pull
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
| `waymaker-flash` | Stable wire encoding, the integrity-check trait and its shipped binding, the storage contract and its geometry, CRC and seals, the two-bank layout, bank selection, append scanning, compaction transition | waymaker-core |
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

Five crates are in the workspace and are *not* layers:

- `xtask` — host tooling, the gate itself. Kept out of firmware builds by `default-members`.
- `waymaker-size-probe` — firmware linked only so its section sizes can be measured. It
  declares all three layers as *optional* dependencies, on purpose — the baseline variant
  links none of them, which is what makes the code-flash budget a delta rather than an
  absolute — and nothing depends on it.
- `waymaker-fault` — the in-memory storage model and crash injector, `policy::TEST_SUPPORT_CRATES`.
  Host-side, `std`, no third-party dependencies, and outside `default-members`. It depends on
  `waymaker-flash` for the storage contract; nothing depends on it, and no layer may, in any
  dependency kind — the tests that drive the harness live with the harness. See
  [ADR 0013](docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md).
- `waymaker-conformance` — the conformance suite for §12's storage contract and the
  `embedded-storage` port, also `policy::TEST_SUPPORT_CRATES`. Outside `default-members`, and
  nothing depends on it. Two things make it unlike the other two test-support crates, and both
  are deliberate: it is `#![no_std]` and allocation-free, because the adapter author it exists
  for may only be able to run it on the target the driver is for; and it carries a third-party
  dependency, `embedded-storage`, which is the only place in this workspace outside `xtask`
  that one is allowed. See
  [ADR 0016](docs/adr/0016-the-storage-contract-is-a-conformance-suite-and-a-port.md).
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
in two places is a budget that ends up disagreeing with itself. The fourth, persistent
flash, is a §04 statement with no constant and no gate behind it: there is no linked image
with banks in it yet. Nothing compares the numbers in this table to `budget.rs`, so treat
`budget.rs` as the source if they ever differ.

| Budget | Target |
| --- | --- |
| Runtime RAM | ≤ 768 B with a 512 B scratch page (§04, v0.1) |
| Kernel state | ≤ 128 B, excluding any page buffer (§04, v0.1) |
| Incremental code flash | ≤ 16 KiB for core + flash adapter, on `thumbv6m-none-eabi` (§04 states 8 KiB as a **v0.1** target; [ADR 0017](docs/adr/0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md) raises the gate once for rung 0.2's two-bank lifecycle) |
| Persistent flash | two erase blocks minimum (§04, v0.1) |

The code-flash row is the one place this repository and the design document now disagree, and
the disagreement is deliberate rather than drift. `budget.rs` is what CI enforces; ADR 0017
records the measurement that moved it — 8180 B to 10776 B, of which 1492 B is
`waymaker-flash`'s bank layer and 920 B is the size probe's own arithmetic — and says what is
owed. A third of the measured "core + flash adapter" figure is the probe rather than either,
which is a defect in the measurement and is filed as its own issue.

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

All 37 rules `cargo xtask check-layering` can emit. The id is what appears in the failure, so
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
| `integrity-check` | `waymaker-flash`'s checksum module stops using one of `source::INTEGRITY_CHECK_PARAMETERS` — a polynomial or an initial value — the right number of times inside the function that owns it; or it or one of its submodules grows an array — a `const`, `static`, `type` alias or local — outside `#[cfg(test)]`; or it is gone, so the pin checks nothing. Or the *binding* drifts: `waymaker-flash/src/integrity.rs` is gone; the integrity trait or the shipped `impl` is renamed, missing, or declared twice — a decoy above the real one is what a first-match scan reads; a seal in `source::SEAL_BINDINGS` stops returning the width §09's frame spends on it; or the shipped method body is anything but one unqualified call to the function that owns its algorithm, `fast::crc32(bytes)` included. Or the *routing* drifts: `encode_with` or `decode_with` stops computing a seal through `C::header_check`/`C::frame_check` exactly once, names a checksum function directly, `input_digest` stops calling `crc32` once, or the scan's `next` stops walking with `decode_with` — a trait nothing is obliged to call is a swap point that selects nothing. The same routing pin covers `waymaker-flash/src/bank.rs`, whose five sealing bodies each reach the seals their row in `source::BANK_SEALING_FUNCTIONS` names: a firmware that sealed its banks with one algorithm and its records with another could read back neither half with the other's reader. [ADR 0012](docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md), and one rule id because it is one decision. [ADR 0010](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md) settles §16's first deferred question with measurements: the polynomial is free (52 B either way), the table is not (64 B for a nibble table, 1024 B for a byte table against an 8 KiB budget). A changed polynomial passes every round-trip test here and fails against every zlib in the world. |
| `storage-contract` | The public function surface of `waymaker-flash`'s storage module differs from `source::STORAGE_CONTRACT_SURFACE`, in either direction — or the module is gone, so the pin checks nothing. Design document §05 says a host or browser adapter "must not expand the firmware traits to accommodate host conveniences", and §12 is the trait it means: a `read_all`, a `flush`, a `write_at` or a `capacity()` shortcut would each break no layering rule, need no dependency, and turn a four-operation contract every port must implement into a surface only a host can afford. The pin compares names, so a widened offset or a validator that stopped validating is still a reviewer's job. |
| `transition-surface` | The replay machine's public function surface differs from `source::TRANSITION_SURFACE`, in either direction. Issue #15 asks for divergence that is "terminal and loud: no reinterpretation of history, no best-effort recovery", and every word of that is an *absence*: a `reset`, a `clear_divergence`, a `force` flag on `intent` would each break no other rule and turn "stop, never guess" into a suggestion. A test cannot call a function that is not there, so the surface is pinned instead. |
| `embassy-below-facade` | A *layer* other than `waymaker-embassy` reaches the Embassy ecosystem. The rule iterates `policy::LAYERS`, so `xtask` and the size probe are outside it. |
| `layer-missing` | A crate named in `policy::LAYERS` is not in the workspace. |
| `layer-not-local` | A crate with a layer's name resolves to a registry crate rather than the path dependency. |
| `workspace-membership` | A workspace member is neither a layer, declared host tooling, a measurement crate, nor declared test support. |
| `inputs-incomplete` | A crate is in the graph but contributed no manifest, or a workspace member contributed no crate root, so rules silently skipped it. |

### Crates and manifests

| Rule | Fires when |
| --- | --- |
| `crate-attributes` | A firmware crate root loses `#![no_std]` or `#![forbid(unsafe_code)]` or declares `extern crate std/alloc`; or any crate the layering covers — the three layers and the test-support crates — loses `#![forbid(unsafe_code)]` or allows unsafe code. A test-support crate is host code, so `#![no_std]` is not asked of it; an unreviewed `unsafe` block in the harness the layers are tested against is. |
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
  calls "core + flash adapter" — roughly 4 KiB of 10776 B at rung 0.2. ADR 0002 says so and
  ADR 0017 attributes the current figure by symbol, but nothing *checks* the split: doing so
  needs a call graph, and the attribution in ADR 0017 is a reading of `llvm-nm` output rather
  than a gate.
- **That a bank's journal region holds a legal journal.** `bank::sealed_generation` decides
  whether a bank is a candidate from its header and its seal. What is between them is
  `frame::Scan`'s, and the two are joined by `BankHeader::journal_offset` rather than by
  anything that checks it: a bank whose header and seal agree and whose journal is damaged is
  authoritative, and the scan is what stops at the damage. That is §14's "frame ignored;
  previous history prefix wins", and it is the division of labour rather than a gap — but it
  does mean "authoritative" is a statement about a bank's header and seal alone.
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
is 8180 B to 10776 B of code flash against an 8192 B budget, which is the budget conversation
[ADR 0013](docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md) said had to happen
before these were written; ADR 0017 has it, raises the gate to 16 KiB, and writes down that a
third of the measured figure is the size probe rather than the engine.
The kernel-state registry has two entries, so the 128 B budget is a number about something.
Timers and the `TimerScheduled`/`TimerFired` records are the rest of rung 0.1; the commit
seal and the bank swap arrive with 0.2, and the async `Ctx` and dispatcher with 0.4. The
gates went in before the code they govern, which is the point: a gate retrofitted after
coverage has slipped is a gate that ratifies the slip.
