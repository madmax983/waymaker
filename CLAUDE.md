# CLAUDE.md

Waymaker is a firmware-first durable workflow engine for Rust. A workflow is re-created from
its beginning after reboot and deterministically replayed through an ordered journal:
completed effects return their recorded results, and the first unresolved effect becomes the
next piece of work.

This file is what a contributor — human or agent — works to. It states the invariants, the
layering rules, and what each crate must not own.

Much of it is checked rather than remembered: the must-not-own cells, the permitted
dependency edges, the eight decision ids, the command list, the five deferred questions and
all 34 rule ids below are compared against the tables that own them, and `cargo xtask check-layering` fails a pull
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
| `waymaker-flash` | Stable wire encoding, CRC and seals, bank selection, append scanning, compaction transition | waymaker-core |
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

Two crates are in the workspace and are *not* layers:

- `xtask` — host tooling, the gate itself. Kept out of firmware builds by `default-members`.
- `waymaker-size-probe` — firmware linked only so its section sizes can be measured. It
  declares all three layers as *optional* dependencies, on purpose — the baseline variant
  links none of them, which is what makes the code-flash budget a delta rather than an
  absolute — and nothing depends on it.

## Budgets

Design document §04. The first three live in `waymaker_core::budget` and are gated by
`cargo xtask size` — the numbers are in the kernel rather than in the gate, because a budget
in two places is a budget that ends up disagreeing with itself. The fourth, persistent
flash, is a §04 statement with no constant and no gate behind it: there is no linked image
with banks in it yet. Nothing compares the numbers in this table to `budget.rs`, so treat
`budget.rs` as the source if they ever differ.

| Budget | v0.1 target |
| --- | --- |
| Runtime RAM | ≤ 768 B with a 512 B scratch page |
| Kernel state | ≤ 128 B, excluding any page buffer |
| Incremental code flash | ≤ 8 KiB for core + flash adapter, on `thumbv6m-none-eabi` |
| Persistent flash | two erase blocks minimum |

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

### Adding an ADR

Copy [`docs/adr/0000-template.md`](docs/adr/0000-template.md) to the next unused number, fill
it in, and add a row to [the index](docs/adr/README.md). A decision that is revisited gets a
new ADR naming what it supersedes; an accepted ADR is never edited to say something else.

### Adding a diagram

[`docs/architecture.md`](docs/architecture.md). Label the fence with
`<!-- diagram: some-id -->` on the line above it, and add a `DiagramSpec` row to
`xtask::docs::DIAGRAMS` naming the labels it must carry.

## What the gate rejects

All 34 rules `cargo xtask check-layering` can emit. The id is what appears in the failure, so
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
| `integrity-check` | `waymaker-flash`'s checksum module loses one of `source::INTEGRITY_CHECK_PARAMETERS` — a polynomial or an initial value — grows an array — a `const`, `static`, `type` alias or local — outside `#[cfg(test)]`, or is gone so the pin checks nothing. [ADR 0010](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md) settles §16's first deferred question with measurements: the polynomial is free (52 B either way), the table is not (64 B for a nibble table, 1024 B for a byte table against an 8 KiB budget). A changed polynomial passes every round-trip test here and fails against every zlib in the world. |
| `transition-surface` | The replay machine's public function surface differs from `source::TRANSITION_SURFACE`, in either direction. Issue #15 asks for divergence that is "terminal and loud: no reinterpretation of history, no best-effort recovery", and every word of that is an *absence*: a `reset`, a `clear_divergence`, a `force` flag on `intent` would each break no other rule and turn "stop, never guess" into a suggestion. A test cannot call a function that is not there, so the surface is pinned instead. |
| `embassy-below-facade` | A *layer* other than `waymaker-embassy` reaches the Embassy ecosystem. The rule iterates `policy::LAYERS`, so `xtask` and the size probe are outside it. |
| `layer-missing` | A crate named in `policy::LAYERS` is not in the workspace. |
| `layer-not-local` | A crate with a layer's name resolves to a registry crate rather than the path dependency. |
| `workspace-membership` | A workspace member is neither a layer, declared host tooling, nor a measurement crate. |
| `inputs-incomplete` | A crate is in the graph but contributed no manifest, or a workspace member contributed no crate root, so rules silently skipped it. |

### Crates and manifests

| Rule | Fires when |
| --- | --- |
| `crate-attributes` | A firmware crate root loses `#![no_std]` or `#![forbid(unsafe_code)]`, allows unsafe code, or declares `extern crate std/alloc`. |
| `empty-default-features` | A firmware crate's `default` feature enables anything, so an optional cost stops being opt-in. |
| `no-build-scripts` | A firmware crate grows a `build.rs`. |
| `member-manifest` | A member manifest drops `[lints] workspace = true`, declares a non-empty `default` feature, opts out of its own test binary with `[lib] test = false` — which would make an untested crate report "no coverable lines" and pass the coverage gate — or, for the kernel, grows a dependency table. |
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
- **A lookup table outside the checksum module.** `integrity-check` reads
  `waymaker-flash/src/crc.rs` and nothing else, so a table in a sibling module that `crc.rs`
  calls is out of its scope.
- **`[lints] workspace = true` in `xtask` and the size probe.** `member-manifest` iterates
  `policy::LAYERS`, so it covers the three firmware crates only.
- **Coverage of non-test code specifically.** llvm-cov instruments the test binary, so the
  85% floor is a floor on a diluted number. See
  [ADR 0001](docs/adr/0001-one-pipeline-table-and-a-per-crate-coverage-gate.md).
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
The kernel-state registry has two entries, so the 128 B budget is a number about something.
Timers and the `TimerScheduled`/`TimerFired` records are the rest of rung 0.1; the commit
seal and the bank swap arrive with 0.2, and the async `Ctx` and dispatcher with 0.4. The
gates went in before the code they govern, which is the point: a gate retrofitted after
coverage has slipped is a gate that ratifies the slip.
