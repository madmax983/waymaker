# Waymaker

> The machine may die. The workflow does not get to pretend the committed past did not happen.

Waymaker is a firmware-first durable workflow engine for Rust. A workflow is re-created from
its beginning after reboot and deterministically replayed through an ordered journal. Completed
effects return their recorded results; the first unresolved effect becomes the next piece of work.

The semantic kernel does not persist Rust stacks, futures, heap graphs, or executor state. It
persists only a compact history of effect boundaries and outcomes. An optional Embassy adapter
turns those boundaries into familiar `ctx.activity(...).await` and timer APIs.

**Waymaker does not make arbitrary Rust futures durable. It makes the observable path through a
deterministic workflow durable.**

## Status

Rung 0.1, in progress. The design is settled at draft v0.2, the three crates exist as
`no_std` libraries with the layering mechanically enforced, and `waymaker-core` now holds
effect identity, activity kinds, the error vocabulary, the borrowed record views, the
streaming replay cursor, and the §08 transition table that decides at each effect boundary
whether history answers the workflow's question or the world has to. `waymaker-flash` holds
the record codec those views are decoded from, and design document §12's storage contract —
the geometry that decides whether an offset and a length are legal, and the four operations
and one barrier every port implements. Above the layers, `waymaker-fault` is the in-memory
storage model and the crash injector: media that starts erased and only clears bits, and the
complete list of points at which a write sequence can be interrupted — every byte of a
program, every erase block of an erase, before and after every barrier — enumerated rather
than sampled, with §15's recovery oracle as a function
([ADR 0013](docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md)).

That oracle is now all four lines design document §15 states, and it is swept over histories
drawn from a seed on geometries drawn from the same one, at every crash point the injector
lists rather than at a sample of them
([ADR 0014](docs/adr/0014-the-oracle-is-four-lines-and-the-sweep-is-seeded.md)). Issue #19's
coverage list is asserted rather than claimed: a census fails the build when the sweep stops
covering it, and two tests read the enumeration directly to check that a tear really does
land at every byte and every program unit and that the power really does go before and after
every barrier. The suite is also proven able to fail — codecs that stop sealing what they say
they seal, written and read by the same weakened firmware through the `IntegrityCheck` trait,
are caught, as is history read one record short, out of order, one skipped or one invented.
The commit seal is on media too: every record now ends in one program unit whose bytes are
the frame's own check with bit 7 of each cleared, written only after a payload barrier, so a
frame with no valid seal over it was never committed and recovery says so. §10's capacity
reserve is the last thing rung 0.2's record format owed, and it writes nothing at all: a run
declares what its records may be worth, the reserve prices the two ways out — a terminal
record and a `continue_as_new` header — and ordinary scheduling is refused with
`HistoryNearCapacity` while both are still affordable, before the device is called at all.
§10's bank swap is the last thing rung 0.2 owed, and it is five types rather than one
function: the run being retired gives up its writer at step 1, so a swap in progress is a run
with nothing left to append with, and a generation seal cannot be programmed without the
payload barrier of step 4 in front of it. Every crash point of all seven steps is swept
against §10's two recovery rules — a crash before step 5 recovers the old run, a crash after
step 6 recovers the new one — with five wrong swaps required to be caught. Timers and the two
timer records are the rest of 0.1.

Design document §16's five deferred questions are tracked in `xtask::docs::DEFERRED_QUESTIONS`
rather than only in the design document. Two are settled: the integrity check
([ADR 0010](docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md)) and the
metadata a scheduled effect records
([ADR 0011](docs/adr/0011-a-scheduled-effect-records-a-length-and-a-digest.md)). The other
three carry the rung that owns them and the evidence that would close them. The integrity
check also lives behind a trait rather than being hard-wired into the codec
([ADR 0012](docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md)),
so the algorithm stays swappable while the seal widths on media do not.

See [`docs/design/waymaker-design-v0.2.html`](docs/design/waymaker-design-v0.2.html) for the
full document, and the issue tracker for the build-out.

## Shape

| Crate | Owns | Must not own |
| --- | --- | --- |
| `waymaker-core` | Borrowed record views, effect identity, replay cursor, transition rules, timer semantics and the clock-kind vocabulary, capacity errors | Allocation, serialization framework, CRC, clock, storage driver, executor, logging |
| `waymaker-flash` | Stable wire encoding, the storage contract and its geometry, CRC/seals, the commit seal and the two-barrier write discipline, the two-bank layout, bank selection, append scanning, storage-backed recovery and the append offset, compaction transition | Activities, workflow types, timers, Embassy |
| `waymaker-embassy` | `Ctx`, activity futures, dispatcher, wakeups, the persistent-clock capability, optional typed codec helpers | On-media authority or hidden global state |

Dependency direction is strict: `waymaker-embassy` → `waymaker-flash` → `waymaker-core`.
The kernel is `no_std`, `no_alloc`, and dependency-free. This is a CI gate, not a
convention — see [Development](#development).

Seven workspace members are not layers: `xtask` is the gate itself, `waymaker-size-probe` is
firmware linked only so that its section sizes can be measured, `waymaker-fault` is the crash
harness, `waymaker-spec` is the formal specification of the recovery invariants,
`waymaker-conformance` is the storage-contract suite and the `embedded-storage` port,
`waymaker-rig` is the power-cut and watchdog-reset rig and the two board clocks of design
document §11 — an RTC in a backed-up domain and an epoch a network restores — and
`waymaker-drive` is the
synchronous driver that runs a workflow to completion through the kernel boundary, together
with design document §07's seven-step effect protocol — which is here rather than in
`waymaker-flash` because step 4 is an activity, and that layer must not own activities. No layer
may depend on any of them. The last three are `#![no_std]` and allocation-free, because each
exists to be run on the part rather than only about it; CI builds `waymaker-rig` and
`waymaker-drive` for `thumbv6m-none-eabi`, and `waymaker-conformance` is meant to be built by
an adapter author for the target their driver is for. None is in the image the code-flash
budget is measured against.

## Budgets

| Budget | Target |
| --- | --- |
| Runtime RAM | ≤ 768 B with a 512 B scratch page — composed: the scratch page, the kernel-state registry, the context, and the largest statics delta of any row |
| Kernel state | ≤ 128 B (`waymaker-core` only, no page buffer) |
| Context (not a §04 row; §04 names it as a runtime RAM *term*) | ≤ 128 B — what kernel state leaves of runtime RAM after the scratch page ([ADR 0035](docs/adr/0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md)) |
| Incremental code flash | ≤ 12 KiB core + flash adapter on `thumbv6m-none-eabi` (§04 states 8 KiB as a *v0.1* target; [ADR 0017](docs/adr/0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md) raised it to 16 KiB for rung 0.2's two-bank lifecycle and [ADR 0020](docs/adr/0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md) to 18 KiB for the capacity reserve; [ADR 0029](docs/adr/0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md) cut it to 12 KiB once the gate stopped charging the probe's own arithmetic) |
| Incremental code flash, with the façade (not a §04 row) | ≤ 13 KiB for the three layers on `thumbv6m-none-eabi` — the façade's own ceiling rather than a raise of the row above ([ADR 0035](docs/adr/0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md)) |
| Persistent flash | Two erase blocks minimum |
| Effect payload | Compile-time / application bound |

Prefix safety, the layering contract, the firmware build, per-crate coverage and the size
budgets are CI gates today. `cargo xtask size` links an example firmware once per feature
combination and fails the build when a delta exceeds a budget, naming the number; kernel
state is additionally a `const` assertion that fails at compile time. See
[Size budgets](#size-budgets).

## Guarantees

- **Prefix safety** — recovery exposes only a legal prefix of committed records.
- **Acknowledged durability** — any record acknowledged after its barrier is recovered after reset.
- **Durable intent** — no Waymaker-dispatched effect lacks a recoverable schedule record.
- **Stable redelivery** — retries and reboot redelivery reuse the original effect identity.
- **Bounded decoding** — malformed storage cannot cause out-of-bounds reads or allocation.

There is no exactly-once physical promise. Power can fail after an activity changes the world but
before its completion is committed; Waymaker redelivers the same stable effect ID. Exactly-once
behavior requires an idempotent activity or downstream deduplication of that ID.

## Roadmap

| Rung | Deliverable | Exit criterion |
| --- | --- | --- |
| 0.1 · protocol | Borrowed record codec, streaming recovery cursor, in-memory storage model, crash injector | Exhaustive fault tests prove committed-prefix recovery |
| 0.2 · flash | Two-bank NOR adapter, record seals, barriers, capacity reserve, continue-as-new | Power-cut tests pass on one Cortex-M0+ and one Cortex-M4 board |
| 0.3 · effects | Durable schedule/dispatch/complete protocol with stable IDs and bounded results | No dispatch is observable without durable intent across all injected crashes |
| 0.4 · embassy | Async `Ctx`, activity dispatcher, in-boot timer, provisioning and OTA examples | Runtime RAM and code-flash budgets pass on `thumbv6m` |
| 0.5 · time | Persistent-clock capability and recorded timers | RTC-backed timer survives total power loss with defined semantics |
| 1.0 | Workflow version markers, stable wire format, book, hardware compatibility matrix | Format frozen and migration policy documented |

## Development

The toolchain is pinned in [`rust-toolchain.toml`](rust-toolchain.toml); `rustup` picks it
up automatically, including the `thumbv6m-none-eabi` target the firmware build needs.

```sh
cargo xtask install-hooks   # once per clone: generates .githooks/pre-commit and points git at it
```

The pipeline, in order:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo build  --locked --workspace --no-default-features
cargo test   --locked --workspace --no-default-features
cargo clippy --locked -p waymaker-embassy --all-targets --features postcard -- -D warnings
cargo test   --locked -p waymaker-embassy --features postcard
cargo doc    --locked -p waymaker-embassy --no-deps --features postcard
cargo doc    --locked --workspace --no-deps --no-default-features
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

Those commands are not transcribed here by hand. They come from one table,
[`xtask/src/pipeline.rs`](xtask/src/pipeline.rs), which is also what CI is checked against
and what `cargo xtask install-hooks` renders the pre-commit hook from. A workflow that stops
running a stage, a hook edited by hand, or a toolchain that stops pinning the firmware target
each fail `cargo xtask check-layering`. So does a workflow that leaves a stage in place but
cannot fail on it: an `if:` on the step or its job, a `continue-on-error:`, a stage buried in
a `run: |` block where a dead shell branch can skip it, a missing `RUSTDOCFLAGS`, a job with
no `runs-on:`, or an `on:` block no pull request triggers. A stage is one step with one
inline `run:`.

The hook runs format, lint and test. The docs build and coverage are left to CI. The
firmware build is a CI job of its own, though the hook reaches it anyway: `cargo test` runs
the integration tests that cross-compile the workspace and a deliberately broken copy of
it.

The firmware build takes no `--workspace` and no `-p` flags: `default-members` in the
workspace manifest is exactly the three firmware crates, so a crate added to the layering is
built for `thumbv6m-none-eabi` without anyone remembering a flag, and `xtask`'s host-only
dependencies never reach the target.

### Coverage

```sh
cargo --locked xtask coverage              # runs cargo llvm-cov, then gates the result
cargo --locked xtask coverage --report r.json   # gates an export produced earlier
```

The gate is **85% of lines, per crate** — not per workspace, because a workspace total is
how an untested kernel hides behind a well-tested adapter. Every workspace member gets a row,
including crates with nothing to cover yet, which report `n/a` rather than vanishing from the
table. The reasoning is in
[ADR 0001](docs/adr/0001-one-pipeline-table-and-a-per-crate-coverage-gate.md).

The command needs [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov), which no
rustup profile carries:

```sh
cargo install cargo-llvm-cov
```

It fails with that hint rather than passing when the tool is absent, and it fails the same
way when a report attributes nothing to this workspace, or when a crate that has code in its
root reports no coverable lines at all. A coverage run that did not happen is not a coverage
run that passed, and "not measured" is not "covered".

The reported percentage includes each crate's inline `#[cfg(test)]` module bodies, which are
covered by construction. The gate is therefore a floor on a number that test code dilutes;
see the ADR for why that is stated rather than worked around.

### Heap and instruction profile

```sh
cargo --locked xtask profile                  # runs both tools, gates the heap, publishes the cost
cargo --locked xtask profile --report r.json  # gates a report produced earlier
```

Design document §02 decision 1 says the kernel is `no_std`, `no_alloc` and dependency-free.
The first and third have been build failures since rung 0.0 — `crate-attributes` and
`kernel-zero-dependencies`. The second was an argument from crate attributes, and what a
firmware pays for is what the linked image does.

This is the measurement. [DHAT](https://valgrind.org/docs/manual/dh-manual.html) intercepts
`malloc` in the binary, so it needs no global allocator and none of the `unsafe` this
workspace denies — which is what the argument against measuring this had always been. Four
workloads drive real library code over `waymaker-fault`'s model of NOR — the journal writer,
the synchronous driver, the async façade and §12's conformance suite — and every heap block is
attributed to the crate whose frame is nearest the allocation. **An engine crate is allowed
zero**, in blocks rather than bytes, because `malloc(0)` returns a pointer and a firmware that
reached it has an allocator linked whatever the byte count says. Every gated crate must also
be *reached* by some workload: a crate nothing executes cannot be attributed an allocation, so
its zero would be the zero a deleted crate scores.

Callgrind runs beside it and counts instructions. That figure is **published and not gated**,
for the reason the write-amplification figure is: §04 states no instruction target, and a
ceiling invented here would be a number nobody agreed to. It is also not a fact about a part —
it is host instructions under a host profile, useful for comparing two commits, because an
instruction count is deterministic where a wall clock is not.

The command needs valgrind, which no rustup profile carries:

```sh
sudo apt-get install valgrind
```

It fails with that hint rather than passing when the tool is absent, and it fails the same way
when DHAT saw no allocation anywhere in the process, when callgrind attributed no instruction
to any engine crate, when a workload completed no unit of work, when a declared workload has
no row, when a gated crate is reached by no workload, or when the per-function costs do not add
up to callgrind's own total. A profile that did not
happen is not a profile that passed. The reasoning, and the two attribution defects that
writing it turned up, are in
[ADR 0038](docs/adr/0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md).

### Size budgets

```sh
cargo --locked xtask size                    # links the matrix, gates it, diffs the base branch
cargo --locked xtask size --no-baseline      # skip the base-branch diff
cargo --locked xtask size --report r.json    # gates a report produced earlier
```

Design document §04 says of the code-flash budget that it "is a gate, not an unverified
claim". The gate links [`crates/waymaker-size-probe`](crates/waymaker-size-probe) — an
example firmware that exists only to be measured — on `thumbv6m-none-eabi` with the
release-size profile, reads the section headers out of each image, and compares every row
against a **baseline image that links no Waymaker at all**. The budget is incremental, so
the measurement is a subtraction rather than an absolute size that would charge Waymaker for
the panic handler and drift with the toolchain.

One more subtraction, and it is the one that makes the number mean what §04 says. The probe
is a crate too, and `size-probe-reach` makes it call every public function each layer
declares, so its own `match` arms and folds grow with the library's. The gate therefore
reads the symbol table as well as the section headers and charges the image delta **less
what the symbol table attributes to the probe** — 7534 B of 18386 B at rung 0.5, which is
[ADR 0029](docs/adr/0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md).
Everything no symbol names as the probe's stays charged to the layers, and the report prints
`Δflash`, `probe` and `layers` on every row so the split is legible rather than trusted.

| Measured | Gated on | How |
| --- | --- | --- |
| Incremental code flash | the `default` row, [`waymaker_core::budget::INCREMENTAL_CODE_FLASH_BYTES`](crates/waymaker-core/src/budget.rs) — 12 KiB | every allocated section whose bytes are stored in the image, minus the baseline, minus what the symbol table attributes to the probe |
| Incremental code flash, with the façade | the `facade` row, `FACADE_CODE_FLASH_BYTES` — 13 KiB | the same measurement on the image that links `waymaker-embassy` as well |
| Engine statics | both gated rows, 256 B | every allocated writable, non-thread-local section, minus the baseline |
| Context | 128 B | `size_of` of the `Ctx` the firmware links, and a `const` assertion beside it that the `drive-firmware` stage evaluates for the target |
| Runtime RAM | 768 B | the 512 B caller-owned scratch page, plus the kernel-state registry, plus the context, plus the largest statics delta of any row |
| Kernel state | 128 B | a `const` assertion in [`waymaker_core::budget`](crates/waymaker-core/src/budget.rs), evaluated for the firmware target by every row of the matrix but the baseline |

Generated workflow futures are reported in a section of their own and summed into nothing:
§04 excludes user workflow memory from the budget, and a small context must not be able to
hide a large state machine. A report that names none is a failure rather than a pass.

What runtime RAM still does not see is a **stack frame**. §04 names four terms — cursor,
context, record header, storage scratch — and the composition covers all four: the cursor and
the record header are the kernel-state registry, and the statics term is added on top. A
deeper call chain is none of them: it moves no writable section and no type size. The report says so where it prints the total rather than printing "runtime RAM: ok";
stack accounting needs a call graph and arrives with the code that has one. The context and
the future figures are host sizes, which are upper bounds on the target's, and the report
labels them as such.

The `default` and `facade` rows are gated; the per-feature rows are reported with their
incremental cost and not gated, because §04 requires an optional cost to be *shown* and
budgets none of them. The two gated rows have separate code-flash ceilings on purpose: §04
states its number for "core + flash adapter", so gating the façade against it would either
fail a build for a cost that number never covered or quietly widen the kernel's budget to
pay for it.

### What "no bookkeeping" does and does not mean

The matrix is derived from `cargo metadata`, not written down: the `default` and `facade`
rows, plus one row per feature every layer declares. Adding `serde`, `postcard`, `defmt` or
a CRC choice to a crate makes a row appear with nothing to remember.

Making that row *mean something* is a different question. A delta can only charge for code
the linker keeps, and with `lto = "fat"` and `--gc-sections` the linker keeps only what the
probe reaches. Enabling the optional dependency is not enough, and neither is naming the
crate: a public function nothing calls is discarded, and the row keeps reporting the probe's
own arithmetic while the real firmware grows.

Half of that **is** a gate. `size-probe-reach` fails a pull request on any public function
of a layer that the probe does not call, and names it:

```
[size-probe-reach] waymaker-size-probe: does not call `advance`, declared in
crates/waymaker-core/src/lib.rs, so the linker discards it and no row charges for it;
add a call in the probe or the size report understates waymaker-core for ever
```

It counts a trait's methods and a trait impl's methods too, which carry no `pub` at all. It
is a floor rather than a proof: a scanner can see that every public function's name appears
in the probe in call position, not that each was called, so two layers declaring the same
name are satisfied by one call to either. What it catches every time is the case that
arrives silently — a layer gains a function and nobody wires the probe up to it. What is left is
the *feature* half: a feature row whose code the probe does not reach comes back identical
to the row below it, and that cannot be a gate — a feature which genuinely costs nothing is
indistinguishable from one the probe does not exercise. It is a **notice** instead, printed
on every run, naming the row and saying what to do:

```
notice: `waymaker-core/serde` measured exactly the same image as `default`, so its
incremental cost is 0 B: either it costs nothing, or waymaker-size-probe does not reach
any code the feature adds and the linker discarded it. ...
```

So the row is automatic, a public function the probe stops reaching fails the build, and a
feature row measuring nothing is named on every run. The probe's `engine` and `facade`
functions in
[`crates/waymaker-size-probe/src/main.rs`](crates/waymaker-size-probe/src/main.rs) are where
the calls go. `engine` calls the kernel surface that exists; `facade` still carries a marker
for the rung that fills it in.

The report — absolute sizes, per-section deltas, and each row's cost over the row it is an
increment on — is written to `target/waymaker-size.json` and uploaded as a CI artifact. On a
pull request the base branch is checked out into a worktree, measured with the same build of
the gate, and diffed on *incremental cost* rather than on absolute size, so a rustc bump
that moves every number without changing anyone's cost is not reported as a change. A base that cannot be measured — a shallow clone, or a commit from
before the probe existed — is reported as "not compared" rather than as a failure: a missing
comparison is not a budget breach, and the budgets are gated either way. The reasoning is in
[ADR 0002](docs/adr/0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md).

`cargo xtask check-layering` is the layering contract from design document §05, turned into
something that fails a pull request. It reads the resolved `cargo metadata` graph rather
than the manifests, so a forbidden dependency cannot hide behind a target table, an
optional feature, a rename, or one level of indirection. Its rules:

| Rule | Fails when |
| --- | --- |
| `dependency-direction` | a firmware crate declares a dependency its layer does not allow |
| `layer-missing` | a crate the layering table names is not in the workspace at all |
| `dependency-direction-transitive` | it reaches one through another crate; the report names the edge that admitted it |
| `kernel-zero-dependencies` | `waymaker-core` declares any dependency, including a dev- or build-dependency |
| `kernel-owns-no-encoding` | a `waymaker-core` source converts between bytes and a value — `from_le_bytes` and its siblings, or an `impl From<&[u8]>`/`TryFrom<&[u8]>` — which needs no dependency for the previous rule to catch |
| `embassy-below-facade` | anything under `waymaker-embassy` reaches an Embassy crate |
| `layer-not-local` | a crate with a layer's name resolves to a registry rather than a path here |
| `workspace-membership` | a workspace member is neither a layer, declared host tooling, a measurement fixture, nor declared test support |
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
| `size-probe` | the size probe is missing, its binary leaves `required-features`, a layer stops being an optional dependency of it, one of its features stops enabling the crates its row measures, it stops mirroring a layer feature under a feature of its own — so the row for that feature links code the probe can reach none of — or its crate root stops being bare-metal firmware |
| `replay-cursor-surface` | the replay cursor's public surface differs from the pinned list, so a lookup by effect id could arrive without a reviewer writing it down |
| `transition-surface` | the replay machine's public surface differs from the pinned list, so a way out of a divergence — a `reset`, a `clear_divergence` — could arrive without a reviewer writing it down |
| `kernel-boundary` | design document §06's boundary types gain or lose a member the pin does not have — a `Resolve::TimerFired` when §09's reserved record kinds land, a `RecordKind` field on `EffectRequest` — or `waymaker-drive` stops deciding from `Intent` and `Resolve`, so the protocol would be driven somewhere other than through the kernel boundary |
| `effect-protocol` | design document §07's seven-step effect protocol gains a public function the pinned list does not have — a `DurableIntent` a caller can forge, an outcome a workflow can observe before step 7's barrier — or its order comes apart: the state that holds a durable intent declares more than the two things §07 gives it, a proof of durable intent is built somewhere other than a barrier that returned, one of the two step bodies stops taking the frame, the payload barrier and the seal exactly once, in that order and at the body's own brace depth, a proof is built before the commit barrier that earns it, a pinned type is declared twice, stops being a braced struct, declares a public field, implements a trait, or declares a method set other than its own — at any visibility, since `pub(crate)` is invisible to a surface pin — a step body declares a closure or a short-circuit, or takes a step inside a block, an argument list or a closure, the file grows a submodule, or the redelivery path writes a second schedule record |
| `timer-capability` | design document §11's timer semantics gain or lose a public function the pinned list does not have — a `TimerSpec::best_effort`, a `Timer::arm_or_downgrade`, a `PersistentClock::now_or_zero`, an `Rtc::assume_held` — a pinned vocabulary declares a member the pin does not have, the persistent-clock module names the boot clock, or a board clock driver gains a method at any visibility, a public field, or a constant, so a deadline that needs an RTC could be quietly served by a clock that restarts on every reset or by a counter no register vouched for |
| `ctx-facade` | issue #35's `Ctx` or the durable half it asks gains a public function the pinned list does not have — a `Ctx::record` that appends for itself, a fifth journal method — `Ctx` gains a method at any visibility or an associated constant, or any file of the façade crate declares a fifth future, names a piece of on-media authority (`StableStorage`, `Reserved`, `RecordRef`, `Recovery`, `ReplayMachine`, `BankLayout`, `Swap`), holds a `static`, or declares a `macro_rules!` a scanner cannot expand; or a synchronous-driver module outside the façade edge names the façade crate, one of its two modules, or the `Bridge` they re-export |
| `dispatch-wiring` | issue #36's dispatcher trait or its table gains or loses a function the pinned list does not have — a `Table::by_name`, a `Table::register`, read anywhere in the file and at every visibility, because a free `pub(crate) fn` at module scope is on neither a surface pin nor a method pin — either type is declared twice, declares a public field or a field set the pin does not have, the file grows a submodule, or one of the two bodies that select a row names a row's label, so an activity could be reached by its name rather than by its number and issue #36's string-addressed-registry non-goal would be a convention rather than a build failure |
| `codec-is-optional` | issue #37's codec helpers stop being optional — a façade module other than the codec one names a codec, a codec item in that module — anywhere in the item, not only on its first line — is gated by no feature, the trait every recorded answer goes through is gated by one, or a codec dependency is not `optional`, so design document §02 decision 4's "never a wire-format requirement" would rest on a manifest nobody reads |
| `rig-oracle` | the rig's oracle or its census gains a public function the pin does not list — an `Audit::assume_passed`, a `Coverage::force_complete` — so an instrument whose bugs show up as *passing* tests could be turned off without a reviewer writing it down |
| `storage-contract` | the storage contract's public surface differs from the pinned list, so a host convenience — a `read_all`, a `flush` — could arrive on a trait every port has to implement without a reviewer writing it down |
| `recovery-surface` | the storage-backed recovery reader's public surface differs from the pinned list, so a `seek`, a `resume_at`, or a second route to an append offset could arrive without a reviewer writing it down |
| `commit-discipline` | the two-barrier writer's public surface differs from the pinned list, a staged frame grows a second method or the ability to program, or the type that may write a commit seal becomes reachable from somewhere other than the payload barrier |
| `capacity-reserve` | §10's capacity reserve gains a public function the pinned list does not have — an ungated writer handed back, a reserve built from numbers rather than from a bank layout — or the gate comes apart: its type declares `stage` other than once, or that `stage` stops *opening* with the admission decision |
| `swap-discipline` | §10's bank swap gains a public function the pinned list does not have — a `commit` that skips the header, a seal a caller can program without the payload barrier, an erase whose bank comes from an argument — or its step order comes apart: a state declares more than the one method §10 gives it, a value that may act after a barrier is built somewhere else, or one of the two erases stops naming exactly the bank its row names |
| `size-probe-reach` | a layer declares a public function the probe never calls, so the linker discards it and no size budget charges for it |
| `effect-scheduled-fields` | `RecordRef::EffectScheduled` declares a field set other than the pinned one, in either direction — a fifth field is 17% more journal on every effect, and a field removed is a wire-format change on a record already written in the field |
| `timer-record-fields` | `RecordRef::TimerScheduled` or `RecordRef::TimerFired` declares a field set other than the pinned one, in either direction — the clock kind is what stops recovery reading a persistent instant as a boot interval, and the arming reading is the monotonicity floor a power cut takes out of RAM |
| `version-gate` | Design document §08's workflow versioning stops being the one that was reviewed: `RecordRef::VersionMarker`'s field set moves in either direction, the versioning vocabulary's surface moves, `VersionRange` gains a public field, an associated constant, a method at any visibility the pin does not list, a submodule, an alias or a free function beside it, the crate root stops re-exporting it by source name, or the kernel's two versioning files reach a source location — §08 says a source-location hash is not stable identity, so a reformatting must not be a divergence |
| `wire-format` | Design document §09's frozen v1 format drifts: a frozen number is declared with another literal or declared twice, a `RecordKind` is renumbered, duplicated or added without a row, a decoder stops asking `reads_format_version` and decides the version for itself, a corpus file is missing or carries another digest, or the byte-by-byte specification stops stating a number beside its name — a renumbering passes every round trip in the workspace and makes every journal a shipped device wrote unreadable |
| `integrity-check` | `waymaker-flash`'s checksum module stops using a catalogued polynomial or initial value inside the function that owns it, or grows a lookup table outside `#[cfg(test)]`; or the binding drifts — the integrity trait or its shipped implementation is gone, renamed or declared twice, a seal changes width, or the shipped implementation stops being one unqualified call to the algorithm ADR 0010 settled on; or one of the four files with a route — the record codec, the bank codec, the recovery reader, the two-barrier writer — stops reaching its seals through the trait, names a checksum function where it may not, or grows a function generic over the check that no table pins |
| `inputs-incomplete` | a crate is in the workspace but a rule could not be run against it |
| `gate-broken` | the gate's own expected value is malformed, so a rule could not check what it claims to |
| `claude-md` | `CLAUDE.md` loses a must-not-own row, a settled-decision id, a gate rule id, or its links to the decision record and the diagrams |
| `recovery-spec` | a §14 guarantee stops being named by all four of `CLAUDE.md`, ADR 0015, `xtask::docs::SPEC_CLAUSES` and `waymaker-spec`'s own clause table, or the clause table is not where the gate looks for it |
| `storage-conformance` | a §12 storage-contract clause stops being named by all four of `CLAUDE.md`, ADR 0016, `xtask::docs::STORAGE_CONTRACT_CLAUSES` and `waymaker-conformance`'s own clause table, the two tables disagree about what discharges one, or the clause table is not where the gate looks for it |
| `hardware-attestation` | rung 0.2's two board runs stop being recorded honestly: a target loses its row in `CLAUDE.md`, a row disagrees with `xtask::docs::HARDWARE_TARGETS` about where it stands, a target is marked passed with no accepted ADR attesting it, or an ADR attests one the table never declared |
| `failure-matrix` | a row of design document §14's failure-semantics table stops being named by all five of `CLAUDE.md`, ADR 0027, `xtask::docs::FAILURE_ROWS`, the rig's `Row::id` (its variant answered with its id, not merely the id declared somewhere) and a `#[test]` of its own name in `waymaker-drive`'s matrix whose body names its variant, a row the table calls swept has no `#[test]` of its rig name in `waymaker-rig`'s matrix whose body names its variant, or one of those files is not where the gate looks for it |
| `adr-numbering` | an ADR skips or reuses a number, is not named `NNNN-slug.md`, or the record has no template |
| `adr-structure` | an ADR loses its title, `- Status:`, `- Date:`, `## Context`, `## Decision` or `## Consequences`, or carries a status outside the vocabulary |
| `adr-index` | an ADR is not linked from `docs/adr/README.md`, or the index links one that does not exist |
| `settled-decisions` | the ADR recording design document §02 stops recording one of the eight decisions, or its headline |
| `deferred-questions` | one of design document §16's five open questions loses its row in `CLAUDE.md`, a settled one's ADR is missing, unaccepted or does not claim it, or an ADR settles one the table still calls open |
| `diagrams` | `docs/architecture.md` loses a labelled Mermaid block, a protocol step, a layer, or a permitted dependency edge |
| `missing-docs` | a crate root loses `#![warn(missing_docs)]`, allows it back, or a workspace member has no crate root the rule could run on |

The contract lives in one table, [`xtask/src/policy.rs`](xtask/src/policy.rs), transcribed
from the design document's "must not own" column. Adding a crate means adding a row. The
pipeline has a table of its own, [`xtask/src/pipeline.rs`](xtask/src/pipeline.rs); adding a
stage means adding a row there and running `cargo xtask install-hooks`. The documentation
has a third, [`xtask/src/docs.rs`](xtask/src/docs.rs), which is what stops `CLAUDE.md`, the
decision record and the architecture diagrams from drifting away from the other two.

## Documentation

| Document | What it is for |
| --- | --- |
| [`CLAUDE.md`](CLAUDE.md) | The invariants, the layering rules, the must-not-own table, and every rule the gate can fail you over. Start here. |
| [`docs/architecture.md`](docs/architecture.md) | The crate dependency flow, the seven-step durable effect protocol, and the two-bank swap, as Mermaid diagrams. |
| [`docs/format/wire-format-v1.md`](docs/format/wire-format-v1.md) | The frozen v1 wire format, byte by byte. What a porter implements from, and what the `corpus` stage holds. |
| [`docs/adr/`](docs/adr/README.md) | The decision record: why each settled decision is the way it is, and what it costs. |
| [`docs/design/waymaker-design-v0.2.html`](docs/design/waymaker-design-v0.2.html) | The design document everything above is taken from. |

## License

See [LICENSE](LICENSE).
