# Waymaker

> The machine may die. The workflow does not get to pretend the committed past did not happen.

Waymaker is a firmware-first durable workflow engine for Rust. After a reboot, Waymaker
re-creates a workflow from its beginning and replays it deterministically through an
ordered journal. A completed effect returns its recorded result. The first unresolved
effect becomes the next piece of work.

The semantic kernel does not persist Rust stacks, futures, heap graphs, or executor
state. It persists a compact history of effect boundaries and their outcomes. An
optional Embassy adapter turns that history into familiar `ctx.activity(...).await` and
timer APIs.

**Waymaker does not make arbitrary Rust futures durable. It makes the observable path
through a deterministic workflow durable.**

## Why Waymaker

Most "durable execution" frameworks assume a host with an OS, a disk, and enough RAM to
hold a checkpoint. Waymaker assumes none of that. It is built for a microcontroller: a
few kilobytes of code flash, a few hundred bytes of RAM, NOR flash instead of a
filesystem, and a power supply that can disappear at any instant — mid-write, mid-effect,
mid-anything.

The engine makes one promise and states it precisely: **at-least-once delivery under a
stable effect identity.** No exactly-once claim, no snapshotted futures, no in-memory
event index a device cannot afford. Every other design choice follows from that promise
and from the hardware it has to run on.

## How it works

A workflow is plain Rust with a durable boundary at each effect: schedule the effect,
let the world perform it, record the outcome. Three steps, and Waymaker keeps its
promise about all three:

1. **Schedule.** The intent to run an effect is written to flash and crosses a
   durability barrier *before* the effect is dispatched. A physical effect never
   precedes its committed intent.
2. **Dispatch.** The world performs the effect — an HTTP call, a sensor read, a flash
   write of its own. Power can fail here; Waymaker redelivers the same effect under the
   same stable identity rather than minting a new one.
3. **Complete.** The outcome is written back, behind its own durability barrier. Once
   committed, replay returns the recorded result instead of running the effect again.

On the next boot, Waymaker replays the journal forward: every resolved effect returns
its recorded answer, and the first unresolved effect becomes the next piece of work. No
event is read out of order, and there is no `Journal::get(id)` — the cursor only ever
moves forward.

## Shape

| Crate | Owns | Must not own |
| --- | --- | --- |
| `waymaker-core` | Borrowed record views, effect identity, replay cursor, transition rules, timer semantics and the clock-kind vocabulary, capacity errors | Allocation, serialization framework, CRC, clock, storage driver, executor, logging |
| `waymaker-flash` | Stable wire encoding, the storage contract and its geometry, CRC/seals, the commit seal and the two-barrier write discipline, the two-bank layout, bank selection, append scanning, storage-backed recovery and the append offset, compaction transition | Activities, workflow types, timers, Embassy |
| `waymaker-embassy` | `Ctx`, activity futures, dispatcher, wakeups, the persistent-clock capability, optional typed codec helpers | On-media authority or hidden global state |

Dependency direction is strict, in one line: `waymaker-embassy` → `waymaker-flash` →
`waymaker-core`. The kernel is `no_std`, `no_alloc`, and dependency-free. This is a CI
gate, not a convention — see [Development](#development).

Nine further workspace members are not layers, and no layer may depend on any of them:
`xtask` is the gate itself; `waymaker-size-probe` is firmware linked only so its section
sizes can be measured; `waymaker-emu` is firmware linked and *run*, on two emulated
cores, so the rig below executes real instructions rather than only compiling; `waymaker-fault`
is the crash harness; `waymaker-spec` is the formal specification of the recovery
invariants; `waymaker-conformance` is the storage-contract suite and the
`embedded-storage` port; `waymaker-rig` is the power-cut and watchdog-reset rig and the
two board clocks (an RTC in a backed-up domain, and an epoch a network restores);
`waymaker-drive` is the synchronous driver that runs a workflow to completion through
the kernel boundary; and `waymaker-facade-demo` is the bridge from `waymaker-drive` to
`waymaker-embassy`, kept in its own crate so `waymaker-drive` names no dependency on the
façade at all. Every one of these except `xtask` and the size probe is `#![no_std]` and
allocation-free, because each exists to run *on* the part, not just to be built for it.
None of them is in the image the code-flash budget is measured against.

## Guarantees

- **Prefix safety** — recovery exposes only a legal prefix of committed records.
- **Acknowledged durability** — any record acknowledged after its barrier is recovered after reset.
- **Durable intent** — no Waymaker-dispatched effect lacks a recoverable schedule record.
- **Stable redelivery** — retries and reboot redelivery reuse the original effect identity.
- **Bounded decoding** — malformed storage cannot cause out-of-bounds reads or allocation.

There is no exactly-once physical promise. Power can fail after an activity changes the
world but before its completion is committed; Waymaker redelivers the same stable effect
ID. Exactly-once behavior needs an idempotent activity or downstream deduplication of
that ID — Waymaker cannot supply it, and does not pretend to.

Every guarantee above is proven, not just tested: `crates/waymaker-spec` holds an
exhaustive, seeded search over a ghost model of the protocol, with a falsifier for each
one — a catalogue of readers that are wrong in exactly one way each, every one of which
the proof must catch. See [`CLAUDE.md`](CLAUDE.md#the-guarantees-and-what-holds-each-up)
for the full table and what discharges each row.

## Roadmap

| Rung | Deliverable | Exit criterion |
| --- | --- | --- |
| 0.1 · protocol | Borrowed record codec, streaming recovery cursor, in-memory storage model, crash injector | Exhaustive fault tests prove committed-prefix recovery |
| 0.2 · flash | Two-bank NOR adapter, record seals, barriers, capacity reserve, continue-as-new | Power-cut tests pass on one Cortex-M0+ and one Cortex-M4 board |
| 0.3 · effects | Durable schedule/dispatch/complete protocol with stable IDs and bounded results | No dispatch is observable without durable intent across all injected crashes |
| 0.4 · embassy | Async `Ctx`, activity dispatcher, in-boot timer, provisioning and OTA examples | Runtime RAM and code-flash budgets pass on `thumbv6m` |
| 0.5 · time | Persistent-clock capability and recorded timers | RTC-backed timer survives total power loss with defined semantics |
| 1.0 | Workflow version markers, stable wire format, book, hardware compatibility matrix | Format frozen and migration policy documented |

For where the project stands against this roadmap today, see the git history and
[`docs/adr/`](docs/adr/README.md) — this README states the shape of the thing, not a
progress report on it.

## Development

The toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml); `rustup` picks
it up automatically, including the `thumbv6m-none-eabi` target the firmware build needs
and the `thumbv7em-none-eabi` target the emulated boot's second core needs.

```sh
cargo xtask install-hooks   # once per clone: generates .githooks/pre-commit and points git at it
cargo install mdbook --locked   # only to build the book; no other stage needs it
```

The pipeline, in order:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo build  --locked --workspace --no-default-features
cargo test   --locked --workspace --no-default-features
cargo test   --locked --workspace --no-default-features --release
cargo clippy --locked -p waymaker-embassy --all-targets --features postcard -- -D warnings
cargo test   --locked -p waymaker-embassy --features postcard
cargo doc    --locked -p waymaker-embassy --no-deps --features postcard
cargo doc    --locked --workspace --no-deps --no-default-features
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

These commands are not transcribed here by hand. They come from one table,
[`xtask/src/pipeline.rs`](xtask/src/pipeline.rs), which is also what CI is checked
against and what `cargo xtask install-hooks` renders the pre-commit hook from. A stage
dropped from the workflow, a hook edited by hand, or a toolchain that stops pinning the
firmware target each fail `cargo xtask check-layering` — and so does a stage that stays
in the workflow but cannot fail: an `if:` on the step or job, a `continue-on-error:`, a
missing `RUSTDOCFLAGS`, a job with no `runs-on:`, or an `on:` block no pull request
triggers.

The pre-commit hook runs format, lint and test — the three fast stages. The docs build
and coverage are left to CI. The firmware build is a CI job of its own, though the hook
reaches it anyway, since `cargo test` cross-compiles the workspace and a deliberately
broken copy of it.

The firmware build takes no `--workspace` and no `-p` flags: `default-members` in the
workspace manifest is exactly the three firmware crates, so a crate added to the
layering is built for `thumbv6m-none-eabi` without anyone remembering a flag.

### Coverage

```sh
cargo --locked xtask coverage              # runs cargo llvm-cov, then gates the result
cargo --locked xtask coverage --report r.json   # gates an export produced earlier
```

The gate is **85% of lines, per crate** — never per workspace, because a workspace total
is exactly how an untested kernel hides behind a well-tested adapter. Every workspace
member gets a row, including a crate with nothing to cover yet, which reports `n/a`
rather than vanishing from the table. See
[ADR 0001](docs/adr/0001-one-pipeline-table-and-a-per-crate-coverage-gate.md).

The command needs [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov), which
no rustup profile installs:

```sh
cargo install cargo-llvm-cov
```

It fails with that hint, rather than passing, when the tool is missing — and it fails
the same way when a report attributes nothing to this workspace, or a crate with code
in its root reports no coverable lines at all. A coverage run that did not happen is not
a coverage run that passed.

### Heap and instruction profile

```sh
cargo --locked xtask profile                  # runs both tools, gates the heap, publishes the cost
cargo --locked xtask profile --report r.json  # gates a report produced earlier
```

The kernel is `no_std`, `no_alloc`, and dependency-free by design. The first and third
have been build failures since day one; the second is measured directly.
[DHAT](https://valgrind.org/docs/manual/dh-manual.html) intercepts `malloc` in the
built binary, so it needs no global allocator and no `unsafe`. Four workloads drive real
library code over `waymaker-fault`'s model of NOR — the journal writer, the synchronous
driver, the async façade, and the storage-contract suite — and every heap block is
attributed to the crate whose frame is nearest the allocation. **An engine crate is
allowed zero**, in blocks rather than bytes, since `malloc(0)` still returns a pointer.

Callgrind runs beside it and counts instructions, published for comparison across
commits but not gated — an instruction count is deterministic where a wall clock is
not, and no target for it exists.

The command needs valgrind, which no rustup profile installs:

```sh
sudo apt-get install valgrind
```

It fails with that hint, rather than passing, when the tool is missing. See
[ADR 0038](docs/adr/0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md).

### Size budgets

```sh
cargo --locked xtask size                    # links the matrix, gates it, diffs the base branch
cargo --locked xtask size --no-baseline      # skip the base-branch diff
cargo --locked xtask size --report r.json    # gates a report produced earlier
```

The gate links [`crates/waymaker-size-probe`](crates/waymaker-size-probe) — an example
firmware that exists only to be measured — on `thumbv6m-none-eabi`, reads the section
headers out of each image, and compares every row against a baseline image that links no
Waymaker at all. The budget is incremental, so the number is a subtraction, not an
absolute size that would charge Waymaker for the panic handler and drift with the
toolchain.

The probe is a crate too, and its own code grows with the library it measures. So the
gate also reads the symbol table and subtracts what it attributes to the probe itself,
printing `Δflash`, `probe`, and `layers` on every row — see
[ADR 0029](docs/adr/0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md).

| Measured | Gated on | How |
| --- | --- | --- |
| Incremental code flash | the `default` row, [`waymaker_core::budget::INCREMENTAL_CODE_FLASH_BYTES`](crates/waymaker-core/src/budget.rs) | every allocated section stored in the image, minus the baseline, minus what the symbol table attributes to the probe |
| Incremental code flash, with the façade | the `facade` row, `FACADE_CODE_FLASH_BYTES` | the same measurement on the image that also links `waymaker-embassy` |
| Engine statics | both gated rows | every allocated writable, non-thread-local section, minus the baseline |
| Context | `waymaker_core::budget` | `size_of` of the `Ctx` the firmware links |
| Runtime RAM | `waymaker_core::budget` | the caller-owned scratch page, plus the kernel-state registry, plus the context, plus the largest statics delta of any row |
| Kernel state | `waymaker_core::budget` | a `const` assertion evaluated for the firmware target |

A generated workflow future is reported in a section of its own and summed into
nothing — a small context must not be able to hide a large state machine behind it. A
report that names no future at all is a failure, not a pass.

A public function a layer declares but the probe never calls is discarded by the
linker, so no row can charge for it. `size-probe-reach` fails the build on exactly that
case, naming the missing call. A feature that measures identically to the row below it
is not a failure — it may genuinely cost nothing — but it is always printed as a notice,
so a silently unreached feature does not stand in for a proof that the feature is free.

`cargo xtask check-layering` is the layering contract from design document §05, turned
into something that fails a pull request. It reads the resolved `cargo metadata` graph
rather than the manifests, so a forbidden dependency cannot hide behind a target table,
an optional feature, a rename, or one level of indirection. Its rules:

| Rule | Fails when |
| --- | --- |
| `dependency-direction` | a firmware crate declares a dependency its layer does not allow |
| `layer-missing` | a crate the layering table names is not in the workspace at all |
| `dependency-direction-transitive` | it reaches one through another crate; the report names the edge that admitted it |
| `kernel-zero-dependencies` | `waymaker-core` declares any dependency, including a dev- or build-dependency |
| `kernel-owns-no-encoding` | a `waymaker-core` source converts between bytes and a value by hand — `from_le_bytes` and its siblings, or an `impl From<&[u8]>`/`TryFrom<&[u8]>` |
| `embassy-below-facade` | anything under `waymaker-embassy` reaches an Embassy crate |
| `layer-not-local` | a crate with a layer's name resolves to a registry rather than a path here |
| `workspace-membership` | a workspace member is neither a layer, declared host tooling, a measurement fixture, an emulation image, nor declared test support |
| `emulation-boot` | the emulated image stops being firmware, writes hand-written `unsafe` outside its two permitted expansions, disagrees with the harness about the prefix it prints, declares a binary not behind its feature, or names a core the toolchain does not pin |
| `no-build-scripts` | a layer or a test-support crate has a `build.rs` |
| `empty-default-features` | a layer or a test-support crate has a non-empty `default` feature |
| `crate-attributes` | a firmware crate root drops `#![no_std]` or declares `extern crate std`/`alloc`, or any crate the layering covers drops `#![forbid(unsafe_code)]` or allows unsafe code |
| `member-manifest` | a layer or a test-support crate stops inheriting the workspace lints, or opts out of its own test binary |
| `release-profile` | `[profile.release]` drifts from design document §04 |
| `cargo-config-profile` | `.cargo/config.toml` declares a profile, an `[env]` table or `[build] rustflags`, or stops aliasing `cargo xtask` to the gate |
| `workspace-lints` | the lint table stops denying `unwrap_used`, or a lint group loses its negative priority |
| `ci-pipeline` | the CI workflow stops running a pipeline stage, moves it to another job, runs a job's stages out of order, or leaves a stage in place while making it unable to fail |
| `pre-commit-hook` | `.githooks/pre-commit` is missing, is not executable, or has drifted from the pipeline table |
| `toolchain-targets` | `rust-toolchain.toml` stops pinning `thumbv6m-none-eabi` or a component a stage needs |
| `size-probe` | the size probe is missing, its binary leaves `required-features`, a layer stops being an optional dependency of it, one of its features stops enabling the crates its row measures, or it stops mirroring a layer feature under a feature of its own |
| `replay-cursor-surface` | the replay cursor's public surface differs from the pinned list, so a lookup by effect id could arrive without a reviewer writing it down |
| `transition-surface` | the replay machine's public surface differs from the pinned list, so a way out of a divergence could arrive without a reviewer writing it down |
| `kernel-boundary` | design document §06's boundary types gain or lose a member the pin does not have, or `waymaker-drive` stops deciding purely from `Intent` and `Resolve` |
| `effect-protocol` | design document §07's seven-step effect protocol gains a public function the pinned list does not have, or its order comes apart, so an outcome could be observed before its commit barrier |
| `timer-capability` | design document §11's timer semantics gain or lose a public function the pinned list does not have, so a deadline that needs an RTC could be quietly served by a clock that restarts on every reset |
| `ctx-facade` | the façade's `Ctx` or its durable half gains a public function the pinned list does not have, or names a piece of on-media authority, a `static`, or an unexpandable macro |
| `dispatch-wiring` | the dispatcher trait or its table gains a function the pinned list does not have, or a row can be selected by its label instead of its number |
| `codec-is-optional` | an optional codec helper stops being optional, so §02 decision 4's "never a wire-format requirement" would rest on a manifest nobody reads |
| `rig-oracle` | the rig's oracle or its census gains a public function the pin does not list, so an instrument whose bugs show up as *passing* tests could be turned off silently |
| `storage-contract` | the storage contract's public surface differs from the pinned list, so a host convenience could arrive on a trait every port has to implement |
| `recovery-surface` | the storage-backed recovery reader's public surface differs from the pinned list, or `Recovery` becomes `Clone`, so one scan could hand out two writers at one offset |
| `commit-discipline` | the two-barrier writer's public surface differs from the pinned list, or the type that may write a commit seal becomes reachable from somewhere other than the payload barrier |
| `capacity-reserve` | §10's capacity reserve gains a public function the pinned list does not have, or its gate stops opening with the admission decision |
| `swap-discipline` | §10's bank swap gains a public function the pinned list does not have, or its step order comes apart |
| `size-probe-reach` | a layer declares a public function the probe never calls, so the linker discards it and no size budget charges for it |
| `effect-scheduled-fields` | `RecordRef::EffectScheduled` declares a field set other than the pinned one, in either direction |
| `timer-record-fields` | `RecordRef::TimerScheduled` or `RecordRef::TimerFired` declares a field set other than the pinned one, in either direction |
| `version-gate` | design document §08's workflow versioning stops being the one that was reviewed |
| `wire-format` | design document §09's frozen v1 format drifts: a renumbered kind, a mismatched corpus file, or a decoder that decides its own version instead of asking the shared predicate |
| `integrity-check` | `waymaker-flash`'s checksum module stops using a catalogued polynomial or initial value, grows a lookup table outside its one exception, or its seals stop routing through the shared trait |
| `inputs-incomplete` | a crate is in the workspace but a rule could not be run against it |
| `gate-broken` | the gate's own expected value is malformed, so a rule could not check what it claims to |
| `claude-md` | `CLAUDE.md` loses a must-not-own row, a settled-decision id, a gate rule id, or its links to the decision record and the diagrams |
| `recovery-spec` | a §14 guarantee stops being named by all four of `CLAUDE.md`, its ADR, the docs table, and `waymaker-spec`'s own clause table |
| `storage-conformance` | a §12 storage-contract clause stops being named the same way across its four places |
| `storage-shapes` | an operation shape stops being named the same way across its four places |
| `hardware-attestation` | rung 0.2's board runs stop being recorded honestly: a target loses its row, a row disagrees about where it stands, or a target is marked passed with no accepted ADR attesting it |
| `failure-matrix` | a row of design document §14's failure-semantics table stops being named the same way across its five places |
| `adr-numbering` | an ADR skips or reuses a number, is not named `NNNN-slug.md`, or the record has no template |
| `adr-structure` | an ADR loses its title, `- Status:`, `- Date:`, `## Context`, `## Decision` or `## Consequences`, or carries a status outside the vocabulary |
| `adr-index` | an ADR is not linked from `docs/adr/README.md`, or the index links one that does not exist |
| `settled-decisions` | the ADR recording design document §02 stops recording one of the eight decisions, or its headline |
| `deferred-questions` | one of design document §16's five open questions loses its row, a settled one's ADR is missing or unaccepted, or an ADR settles one the table still calls open |
| `diagrams` | `docs/architecture.md` loses a labelled Mermaid block, a protocol step, a layer, or a permitted dependency edge |
| `book` | the book stops being the book issue #42 asks for — a chapter loses its file or link, a fence carries source instead of a tested include, or `CLAUDE.md` and `README.md` stop linking the book's summary |
| `hardware-matrix` | the compatibility matrix stops covering every part, its table stops matching the derived rows, or it says `Passed` where the record says `Not run` |
| `missing-docs` | a crate root loses `#![warn(missing_docs)]`, allows it back, or a workspace member has no crate root the rule could run on |

Every one of these is spelled out in full — what it protects and why — in
[`CLAUDE.md`](CLAUDE.md#what-the-gate-rejects). The contract itself lives in
[`xtask/src/policy.rs`](xtask/src/policy.rs), the pipeline table in
[`xtask/src/pipeline.rs`](xtask/src/pipeline.rs), and the checked documentation table in
[`xtask/src/docs.rs`](xtask/src/docs.rs).

## Documentation

| Document | What it is for |
| --- | --- |
| [`CLAUDE.md`](CLAUDE.md) | The invariants, the layering rules, the must-not-own table, and every rule the gate can fail you over. Start here. |
| [`docs/architecture.md`](docs/architecture.md) | The crate dependency flow, the seven-step durable effect protocol, and the two-bank swap, as Mermaid diagrams. |
| [`docs/book/`](docs/book/src/SUMMARY.md) | The book: the design centre, the determinism contract, the effect protocol, the failure semantics, the wire format, what is not promised, the porting guide, and the hardware compatibility matrix. Rendered by `cargo xtask book`. |
| [`docs/format/wire-format-v1.md`](docs/format/wire-format-v1.md) | The frozen v1 wire format, byte by byte. What a porter implements from, and what the `corpus` stage holds. |
| [`docs/adr/`](docs/adr/README.md) | The decision record: why each settled decision is the way it is, and what it costs. |
| [`docs/design/waymaker-design-v0.2.html`](docs/design/waymaker-design-v0.2.html) | The design document everything above is taken from. |

## License

See [LICENSE](LICENSE).
