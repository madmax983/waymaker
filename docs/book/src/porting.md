# Porting to a new part

A port is two traits. Neither needs an allocator, and neither needs `unsafe`.

- `StableStorage`, in `waymaker-flash`. Four operations and a barrier.
- `PersistentClock`, in `waymaker-embassy`. One reading. Write it only if the board has a
  clock that survives power loss.

## Storage

Describe the part with a `Geometry`: capacity, erase size, program size and read size. The
units must nest. Then implement the five methods.

```rust,ignore
{{#include ../../../crates/waymaker-drive/tests/book.rs:a_storage_adapter_is_four_operations_and_a_barrier}}
```

### What the contract requires

| Rule | What it means for your driver |
| --- | --- |
| Validate before you touch media | Call the geometry's validator first. A refused operation must change nothing |
| Act on exactly the named region | No read-ahead into the caller's buffer, no write outside the named bytes |
| `barrier` changes no media | It orders. It does not program |
| After `barrier` returns, earlier mutations survive a reset | Flush the write buffer, the cache and the command queue here |
| Nothing after a completed barrier becomes durable before what it ordered | The ordering is the whole guarantee |
| `program` and `erase` may fail or be interrupted | A failed program may still have changed media. Say so, and do not retry silently |
| One-way bit rules are yours | Waymaker programs; it does not know your part's rules for programming a cell twice |

### Do not add convenience

Do not add a `read_all`, a `flush`, a `write_at` or a `capacity()` shortcut. The trait is
four operations and a barrier because every port must implement all of it. The
`storage-contract` gate rule fails a build that widens it.

### Check the port

`waymaker-conformance` is the suite. It is `#![no_std]` and allocation-free, so you can run
it on the target the driver is for. Two of design document §12's clauses need a real reset
and cannot be checked in one process: arm the witness, reset the board, then verify.

## The persistent clock

Implement `PersistentClock` only if the reading really survives power loss.

```rust,ignore
{{#include ../../../crates/waymaker-drive/tests/book.rs:a_persistent_clock_is_a_reading_and_a_bit}}
```

### What the contract requires

- A reading is in your own unit, and the same unit across reboots. Waymaker compares
  readings. It never converts them.
- Never substitute a value. A zero fires every persistent deadline at once. A maximum fires
  none of them. Return an error instead.
- Report a backwards move as an error. A battery change or a re-synchronised epoch can move
  a clock back.
- Do not widen a counter that can wrap. RAM did not survive the cut, so there is no epoch to
  widen it with. A wrapped counter reads below the arming reading, and the kernel refuses
  the interval rather than crediting a wrong one.

### If the board has no such clock

Declare `ClockCapability::BootOnly`. A workflow that asks for `AtPersistentTime` is then
refused with `NoPersistentClock`, which is the honest answer. Do not declare `Persistent`
and hope; no code below the board can catch that.

## Then measure it

Run the power-cut rig, and add a row to
[the hardware compatibility matrix](hardware-matrix.md).
