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

These are CI gates, not unverified claims.

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
up automatically.

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo build  --locked --workspace --no-default-features
cargo test   --locked --workspace --no-default-features
cargo --locked xtask check-layering
```

`cargo xtask check-layering` is the layering contract from design document §05, turned into
something that fails a pull request. It reads the resolved `cargo metadata` graph rather
than the manifests, so a forbidden dependency cannot hide behind a target table, an
optional feature, a rename, or one level of indirection. Its rules:

| Rule | Fails when |
| --- | --- |
| `dependency-direction` | a firmware crate declares a dependency its layer does not allow |
| `dependency-direction-transitive` | it reaches one through another crate; the report names the edge that admitted it |
| `kernel-zero-dependencies` | `waymaker-core` declares any dependency, including a dev- or build-dependency |
| `embassy-below-facade` | anything under `waymaker-embassy` reaches an Embassy crate |
| `layer-not-local` | a crate with a layer's name resolves to a registry rather than a path here |
| `workspace-membership` | a workspace member is neither a layer nor declared host tooling |
| `no-build-scripts` | a firmware crate has a `build.rs` |
| `empty-default-features` | a firmware crate has a non-empty `default` feature |
| `crate-attributes` | a crate root drops `#![no_std]` or `#![forbid(unsafe_code)]`, allows unsafe code, or declares `extern crate std`/`alloc` |
| `member-manifest` | a firmware crate stops inheriting the workspace lints |
| `release-profile` | `[profile.release]` drifts from design document §04 |
| `cargo-config-profile` | `.cargo/config.toml` declares a profile, which would silently override it |
| `workspace-lints` | the lint table stops denying `unwrap_used`, or a lint group loses its negative priority |
| `inputs-incomplete` | a crate is in the workspace but a rule could not be run against it |

The contract lives in one table, [`xtask/src/policy.rs`](xtask/src/policy.rs), transcribed
from the design document's "must not own" column. Adding a crate means adding a row.

`xtask` is host tooling and is excluded from firmware-target builds by `default-members`;
build the firmware crates for a target with:

```sh
cargo build -p waymaker-core -p waymaker-flash -p waymaker-embassy \
  --no-default-features --target thumbv6m-none-eabi
```

## License

See [LICENSE](LICENSE).
