# The determinism contract

Waymaker replays a workflow by running it again. The second run must ask for the same
effects, in the same order, with the same inputs. If it does not, Waymaker stops the run
with a divergence error. It does not guess.

This chapter is the contract a workflow author works to.

## Do not read these directly

Each of these can answer differently on a later boot. Reading one inside workflow code
makes the workflow non-deterministic.

| Do not read | Why it changes |
| --- | --- |
| The wall clock or a monotonic counter | Time moves, and a reset restarts a boot clock |
| A random number generator | Every draw differs |
| A sensor, an ADC, a GPIO level | The world moves |
| A network response | The peer answers differently |
| A unique id, a MAC address read at run time, a serial number | Cheap to record once, and free to get wrong |
| The firmware's own version | It changes when you ship |

## Read them through an effect instead

Ask the boundary. The driver records the answer. Every later replay gets the recorded
answer and the world is not asked again.

```rust,ignore
{{#include ../../../crates/waymaker-drive/tests/book.rs:a_reading_of_the_world_is_recorded_rather_than_taken_again}}
```

The clock moved between the two boots. The workflow saw one reading, because the reading is
on media.

## What is safe to do directly

- Arithmetic on values the workflow already holds.
- Branching on a recorded effect result.
- Branching on the run's own input, which the opening record carries.
- Anything that is a pure function of the two.

## When the code itself changes

Shipping new firmware changes the code between the effects. Waymaker handles that with two
mechanisms.

- A workflow declares a **version range**: the oldest recorded version it can still replay,
  and the version it writes into a new run. A run recorded outside that range is refused
  rather than replayed wrongly.
- A **version gate** records which branch the first execution took. Every later boot is
  given that number back, whatever branch this firmware would have chosen. A gate spends a
  sequence number, so a gate added, removed or moved is caught by the same ordering check
  that catches a moved effect.

Do not key a gate on a source location. A hash of the file and line changes when a comment
above it moves, so a reformat becomes a divergence.

## What divergence does

It is terminal and loud. Waymaker refuses the mismatched step **before** it consumes the
record it disagreed with, so a diverging replay dispatches nothing and writes nothing. It
does not reinterpret history, and it does not recover on a best-effort basis.
