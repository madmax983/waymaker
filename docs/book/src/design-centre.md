# The design centre

Waymaker makes the **observable path** through a deterministic workflow durable. It does not
make arbitrary Rust futures durable.

Read that sentence before you read any API. Everything else in this book follows from it.

## What is durable

A workflow calls out to the world at named points. Waymaker calls each of those points an
**effect**. Waymaker writes a record before it asks the world. That record says the effect will
happen. After the world answers, Waymaker writes a second record. That record says what the
world answered.

Each record crosses two barriers. The first barrier comes after the device programs the
record body. The second comes after the device programs the seal that commits it.

The journal is the ordered list of those records. It, the bank header that names the run, and
the seal over that header are what survive a reset.

## What is not durable

The workflow's stack, its local variables, and the state machine an `async fn` compiles
into. Waymaker writes none of that to media. Waymaker never snapshots a suspended
future.

## How a run resumes

After a reset the device re-creates the workflow **from its beginning** and runs it again.
Waymaker then matches each effect the workflow reaches against the journal, in order:

- If the journal holds a matching outcome, Waymaker returns the recorded value and asks the
  world nothing. A match means that the effect's kind and its input digest agree with what
  the workflow just asked for. A mismatch is a divergence, not a replay.
- If the journal holds a schedule record with no outcome, Waymaker delivers the same effect
  again. It uses the identity that the schedule record committed.
- If the journal holds nothing more, this is new work. Waymaker schedules it.

Replay is therefore fast-forward through recorded history. It is not a restoration of a
saved process.

## A workflow

A workflow is a value with a method. It declares what it is, and it asks the boundary for
each effect in turn.

```rust,ignore
{{#include ../../../crates/waymaker-drive/tests/book.rs:a_workflow_is_a_value_with_a_method}}
```

## What that buys

Run the same workflow again over the same journal. Waymaker asks the world nothing.

```rust,ignore
{{#include ../../../crates/waymaker-drive/tests/book.rs:a_replayed_run_asks_the_world_nothing}}
```

The second boot builds a new `Fetch` value. It shares no memory with the first. The result
is the same because the result came off media.

## The price

Because the run re-executes, the code between two effects must produce the same calls every
time. That obligation is the [determinism contract](determinism.md), and it is the next
chapter.
