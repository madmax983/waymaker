# ADR 0055: the driver and the bridge are layers firmware ships

- Status: accepted
- Date: 2026-10-27
- Issue: none — raised by a downstream board crate
- Supersedes: [0024](0024-the-kernel-boundary-is-driven-synchronously-by-a-crate-above-the-layers.md)'s
  placement of `waymaker-drive` above the layers, and
  [0032](0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md)'s placement
  of `Bridge` in `waymaker-facade-demo`. Both ADRs' other decisions stand.
- Related: [0013](0013-the-fault-harness-is-a-crate-above-the-layers.md),
  [0025](0025-the-effect-protocol-is-a-typestate-and-an-exhausted-answer-is-a-record.md),
  [0029](0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md),
  [0035](0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md)

## Context

ADR 0024 put `waymaker-drive` in `policy::TEST_SUPPORT_CRATES` for one stated reason: a
layer's public functions must all be reached by the size probe, so a driver listed in
`policy::LAYERS` "would be charged against design document §04's code-flash budget — and this
is not firmware Waymaker ships". ADR 0032 and issue #106 then put `Bridge`, the one type that
turns the driver's `Boundary` into the façade's `Journal`, in `waymaker-facade-demo` beside
§06's two examples, for the same reason one crate up.

That reason no longer holds. A device that runs a workflow on flash needs exactly what the
driver is: the recovery scan, the two-barrier writer and the transition table joined into
one loop, with §07's seven steps as a typestate. A device whose workflows are `async fn`s
over `Ctx` needs the bridge as well. The first downstream board — an ESP32-S3 agent harness
whose run and supervisor are `Ctx` workflows — reached for both and found them in crates
marked "never linked into firmware, never published". Its options were to depend on test
support, or to copy 2,700 lines of driver whose every crash window this workspace's tests
have already swept. Either is worse than the driver being what it already behaves like.

Three things kept the driver out of the layers besides the budget, and each still has to be
answered:

- **It must not reach the façade.** Issue #106's point was that `cargo metadata` proves the
  protocol is usable without `waymaker-embassy`. A layer's row says what it may depend on, so
  this becomes `dependency-direction` holding the driver's row to `waymaker-core` and
  `waymaker-flash`, beside `ctx-facade`'s source half and its manifest half, which stay.
- **Its tests use the fault harness.** A layer may not depend on test support in any
  dependency kind — `dependency-direction` reads every table — and the driver's tests drive
  `waymaker-fault`'s crash injector and `waymaker-rig`'s matrix vocabulary.
- **Its reference workflow is a demonstration.** `waymaker_drive::demo` was a public module
  so that the firmware target built it, and a layer's public functions are charged to its row.

## Decision

`waymaker-drive` and a new crate, `waymaker-bridge`, are layers. `policy::LAYERS` lists five
crates, from the kernel outwards:

| Layer | May depend on | Must not own |
| --- | --- | --- |
| `waymaker-core` | nothing | unchanged |
| `waymaker-flash` | `waymaker-core` | unchanged |
| `waymaker-drive` | `waymaker-core`, `waymaker-flash` | on-media authority, the transition table, futures, Embassy, allocation |
| `waymaker-embassy` | `waymaker-core`, `waymaker-flash`, and its five codec crates | unchanged |
| `waymaker-bridge` | the four layers above, and the façade's five codec crates | on-media authority, workflows, or a dispatcher |

Every layer is in `default-members`, so the `firmware` stage builds the driver and the
bridge for `thumbv6m-none-eabi` and the `drive-firmware` stage is gone; issue #28's "no
`Future`, no Embassy, no allocation" is held by `firmware`, `crate-attributes` and the
driver's row together.

**The bridge.** `waymaker-bridge` is `Bridge` and nothing else, moved verbatim out of
`waymaker-facade-demo`. It is the only layer whose row names `waymaker-embassy`.
`embassy-below-facade` therefore lets a layer whose row permits the façade reach the façade
crate itself, and nothing behind it: an executor reached through the bridge is still
Embassy, and still a violation. `ctx-facade`'s authority, `static`, future-set and macro bans
read the bridge's files as layer sources.

**The tests and the demonstration move up.** The driver's ten integration test files and
its `demo` module move into `waymaker-facade-demo`, which keeps its dev-dependencies on
`waymaker-fault` and `waymaker-rig`. Coverage is bucketed by the file a line is in, not the
crate a test is in, so those tests still count toward the driver's 85%. The three unit tests
in the driver's `effect.rs` — one of them the only caller of its crate-private redelivery
path — keep running in the crate, against a 1 KiB RAM-backed NOR written in the test module. The `matrix` stage's model half is
`waymaker-facade-demo`'s `tests/matrix.rs`, and `failure-matrix` reads it there.

**The budgets.** Each new layer gets a row of the size matrix and a ceiling of its own,
for the façade's reason (ADR 0035): §04 states the engine's number for "core + flash
adapter", and the driver is neither.

- The `drive` row links the engine and the driver. `DRIVE_CODE_FLASH_BYTES` is the engine's
  ceiling plus `DRIVE_SHARE_BYTES`, 6 KiB. The row measured 18,368 B of layers, 5,204 B over
  the `default` row, against a 19,456 B ceiling.
- The `bridge` row links every layer. `BRIDGE_CODE_FLASH_BYTES` is the façade's ceiling plus
  the driver's share plus `BRIDGE_SHARE_BYTES`, 3 KiB. The row measured 22,316 B, 8,758 B
  over the `facade` row, against a 23,552 B ceiling.

The probe reaches every public function of both crates, which is what `size-probe-reach`
asks for. It does so through one workflow type, so the `bridge` row prices the bridge
rather than a second monomorphised copy of `Driver::boot`: a first draft booted a second
workflow there and read 24,144 B.

## Consequences

- A firmware can depend on `waymaker-drive` and `waymaker-bridge` as layers, with their
  cost measured and gated on every pull request. Esper's ESP32-S3 board pins this
  repository's revision until the crates are published.
- The engine's own ceiling did not move. Every byte the driver and the bridge add is charged
  to a row that names them.
- `waymaker-facade-demo` is now the crate whose name least describes it. It holds §06's two
  examples, the driver's reference workflow, and the crash-swept tests of both layers it
  sits on. Renaming it would have touched every path the book, the matrix and the
  documentation quote; that is a follow-up, not part of this decision.
- The driver's public surface is now firmware surface. Every function it adds is a call the
  probe must make and a byte the `drive` row charges, which is the pressure ADR 0024 wanted
  to avoid. It is accepted as the price of shipping the loop rather than asking boards to
  copy it.
- The `drive` and `bridge` rows' shares are fitted to today's figure plus about a kilobyte,
  as the façade's was. A rung that grows the driver raises its share here, with a reason, in
  the same pull request.

## Alternatives considered

- **Depend on test support from the board.** It works today, and it would leave a product
  firmware linking a crate whose manifest says it is never linked into firmware. Nothing
  would measure its cost, and nothing would stop it growing a `std` dependency the board
  first discovers at link time.
- **Copy the driver into the board crate.** It would fork the one piece of Waymaker whose
  crash windows are swept exhaustively, and the copy would be swept by nothing.
- **Promote the driver and leave the bridge in `waymaker-facade-demo`.** The board would
  still depend on a demonstration crate to run a `Ctx` workflow on flash, which is the case
  that started this.
- **Fold the bridge into `waymaker-embassy`.** The façade would then depend on the driver,
  and `waymaker-embassy` → `waymaker-drive` is an edge issue #106 removed so that the protocol
  and the façade could each be used without the other.

## See also

- [CLAUDE.md](../../CLAUDE.md#the-layering) — the layer table and the must-not-own table.
- [The architecture diagrams](../architecture.md#crate-dependency-flow) — the five layers.
- [The design document](../design/waymaker-design-v0.2.html) — §04's budgets, §05's
  layering, §06's boundary.
