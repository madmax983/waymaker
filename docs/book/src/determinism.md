# The determinism contract

Waymaker replays a workflow by running it again. The second run must ask for the same
effects, in the same order, with the same inputs. This chapter is the contract a workflow
author works to.

**What Waymaker actually compares is what a replay asks for at a boundary**: the effect's
position, its kind, its input length and its input digest. A difference there is terminal.
A difference anywhere else is invisible — including the end of the run, where a replay that
would conclude differently is answered from history without complaint. Keeping to the
contract is the author's job; the boundary check is a backstop, not a proof.

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
  rather than replayed wrongly. Widen before you narrow: an image that moves `oldest` past a
  version still on devices bricks those runs until an image that can replay them ships
  again, and nothing in the engine enforces the order a fleet is upgraded in.
- A **version gate** records which branch the first execution took. Every later boot is
  given that number back, whatever branch this firmware would have chosen. A gate spends a
  sequence number, so a gate added, removed or moved is caught by the same ordering check
  that catches a moved effect.

To branch on the version the run itself recorded — rather than on the image's own, which is
the last row of the table above — read it back with `Boundary::recorded_version`. That number
is a fact about history and is the same on every boot.

Do not key a gate on a source location. A hash of the file and line changes when a comment
above it moves, so a reformat becomes a divergence.

## What divergence does

It is terminal and loud. Waymaker refuses the mismatched step **before** it consumes the
record it disagreed with, so a diverging replay dispatches nothing and writes nothing. It
does not reinterpret history, and it does not recover on a best-effort basis.
