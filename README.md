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

Rung 0.0. The design is settled at draft v0.2 and the three crates exist as `no_std`
scaffolding with the layering mechanically enforced. No protocol code has been written yet.
See [`docs/design/waymaker-design-v0.2.html`](docs/design/waymaker-design-v0.2.html) for the
full document, and the issue tracker for the build-out.

## Shape

| Crate | Owns | Must not own |
| --- | --- | --- |
| `waymaker-core` | Borrowed record views, effect identity, replay cursor, transition rules, capacity errors | Allocation, serialization framework, CRC, clock, storage driver, executor, logging |
| `waymaker-flash` | Stable wire encoding, CRC/seals, bank selection, append scanning, compaction transition | Activities, workflow types, timers, Embassy |
| `waymaker-embassy` | `Ctx`, activity futures, dispatcher, wakeups, optional typed codec helpers | On-media authority or hidden global state |

Dependency direction is strict: `waymaker-embassy` → `waymaker-flash` → `waymaker-core`.
The kernel is `no_std`, `no_alloc`, and dependency-free. This is a CI gate, not a
convention — see [Development](#development).

## Budgets

| Budget | v0.1 target |
| --- | --- |
| Runtime RAM | ≤ 768 B with a 512 B scratch page |
| Kernel state | ≤ 128 B (`waymaker-core` only, no page buffer) |
| Incremental code flash | ≤ 8 KiB core + flash adapter on `thumbv6m-none-eabi` |
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
cargo doc    --locked --workspace --no-deps --no-default-features
cargo --locked xtask coverage
cargo build --locked --no-default-features --target thumbv6m-none-eabi
cargo --locked xtask size
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

| Measured | Gated on | How |
| --- | --- | --- |
| Incremental code flash | the `default` row, 8 KiB | every allocated section whose bytes are stored in the image, minus the baseline |
| Engine statics | the `default` row, 256 B | every allocated writable, non-thread-local section, minus the baseline: 768 B of runtime RAM less the 512 B scratch page the caller owns |
| Kernel state | 128 B | a `const` assertion in [`waymaker_core::budget`](crates/waymaker-core/src/budget.rs), evaluated for the firmware target by every row of the matrix but the baseline |

Section sizes see `.data` and `.bss` and nothing else, so the statics figure is a **floor**
on §04's runtime RAM rather than the rule itself: a cursor, context or record header that
lives on the caller's stack moves no writable section, and neither does a deeper call frame.
The report says so rather than printing "runtime RAM: ok"; stack accounting needs a call
graph and arrives with the code that has one.

Only the `default` row is gated on the first two, because §04 states them for "core + flash
adapter" and that row is exactly that. The `facade` row and the per-feature rows are
reported with their incremental cost and not gated: §04 requires an optional cost to be
*shown* and budgets none of them, and gating the façade against the kernel's number would
either fail a build for a cost that number never covered or quietly widen the kernel's
budget to pay for it.

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

So a layer cannot grow public code that the budget quietly stops covering. What is left is
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
the calls go, and they carry a marker for the rung that fills them in.

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
| `embassy-below-facade` | anything under `waymaker-embassy` reaches an Embassy crate |
| `layer-not-local` | a crate with a layer's name resolves to a registry rather than a path here |
| `workspace-membership` | a workspace member is neither a layer, declared host tooling, nor a measurement fixture |
| `no-build-scripts` | a firmware crate has a `build.rs` |
| `empty-default-features` | a firmware crate has a non-empty `default` feature |
| `crate-attributes` | a crate root drops `#![no_std]` or `#![forbid(unsafe_code)]`, allows unsafe code, or declares `extern crate std`/`alloc` |
| `member-manifest` | a firmware crate stops inheriting the workspace lints, or opts out of its own test binary |
| `release-profile` | `[profile.release]` drifts from design document §04 |
| `cargo-config-profile` | `.cargo/config.toml` declares a profile, an `[env]` table or `[build] rustflags`, or stops aliasing `cargo xtask` to the gate |
| `workspace-lints` | the lint table stops denying `unwrap_used`, or a lint group loses its negative priority |
| `ci-pipeline` | the CI workflow stops running a pipeline stage, moves it to another job, runs a job's stages out of order, or leaves a stage in place while making it unable to fail |
| `pre-commit-hook` | `.githooks/pre-commit` is missing, is not executable, or has drifted from the pipeline table |
| `toolchain-targets` | `rust-toolchain.toml` stops pinning `thumbv6m-none-eabi` or a component a stage needs |
| `size-probe` | the size probe is missing, its binary leaves `required-features`, a layer stops being an optional dependency of it, one of its features stops enabling the crates its row measures, or its crate root stops being bare-metal firmware |
| `size-probe-reach` | a layer declares a public function the probe never calls, so the linker discards it and no size budget charges for it |
| `inputs-incomplete` | a crate is in the workspace but a rule could not be run against it |
| `gate-broken` | the gate's own expected value is malformed, so a rule could not check what it claims to |

The contract lives in one table, [`xtask/src/policy.rs`](xtask/src/policy.rs), transcribed
from the design document's "must not own" column. Adding a crate means adding a row. The
pipeline has a table of its own, [`xtask/src/pipeline.rs`](xtask/src/pipeline.rs); adding a
stage means adding a row there and running `cargo xtask install-hooks`.

## License

See [LICENSE](LICENSE).
