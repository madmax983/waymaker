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

Pre-implementation. The design is settled at draft v0.2; the crates do not exist yet.
See [`docs/design/waymaker-design-v0.2.html`](docs/design/waymaker-design-v0.2.html) for the
full document, and the issue tracker for the build-out.

## Shape

| Crate | Owns | Must not own |
| --- | --- | --- |
| `waymaker-core` | Borrowed record views, effect identity, replay cursor, transition rules, capacity errors | Allocation, serialization framework, CRC, clock, storage driver, executor, logging |
| `waymaker-flash` | Stable wire encoding, CRC/seals, bank selection, append scanning, compaction transition | Activities, workflow types, timers, Embassy |
| `waymaker-embassy` | `Ctx`, activity futures, dispatcher, wakeups, optional typed codec helpers | On-media authority or hidden global state |

Dependency direction is strict: `waymaker-embassy` → `waymaker-flash` → `waymaker-core`.
The kernel is `no_std`, `no_alloc`, and dependency-free.

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

## License

See [LICENSE](LICENSE).
