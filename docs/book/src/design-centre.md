# The design centre

Waymaker makes the **observable path** through a deterministic workflow durable. It does not
make arbitrary Rust futures durable.

Read that sentence before you read any API. Everything else in this book follows from it.

## What is durable

A workflow calls out to the world at named points. Waymaker calls each of those points an
**effect**. Before the world is asked, Waymaker writes a record that says the effect is
about to happen. After the world answers, Waymaker writes a record that says what it
answered. Both records cross a durability barrier.

The journal is the ordered list of those records. It is the only thing that survives a
reset.

## What is not durable

The workflow's stack, its local variables, and the state machine an `async fn` compiles
into. None of that is written to media. Waymaker never snapshots a suspended future.

## How a run resumes

After a reset the device re-creates the workflow **from its beginning** and runs it again.
Each effect the workflow reaches is matched against the journal, in order:

- If the journal holds the outcome, the recorded value is returned. The world is not asked.
- If the journal holds a schedule record with no outcome, the same effect is delivered
  again, under the identity the schedule record committed.
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

Re-run the same workflow over the same journal and the world is asked nothing.

```rust,ignore
{{#include ../../../crates/waymaker-drive/tests/book.rs:a_replayed_run_asks_the_world_nothing}}
```

The second boot builds a new `Fetch` value. It shares no memory with the first. The result
is the same because the result came off media.

## The price

Because the run re-executes, the code between two effects must produce the same calls every
time. That obligation is the [determinism contract](determinism.md), and it is the next
chapter.
