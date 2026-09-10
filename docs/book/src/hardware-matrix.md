# The hardware compatibility matrix

**No board has ever run Waymaker.** The table says so, and this repository will not let it
say anything else.

The gate derives every cell below except the clock column:

- the power-cut column comes from the attestation record;
- the geometry comes from the modelled parts;
- the last column comes from a measurement the gate takes on every run.

A row that stated anything else would fail the build. So would prose on this page that
claimed that a board cleared its loops.

| Id | Part | Erase/program/read | Power cut | Clock | Written B per effect |
| --- | --- | --- | --- | --- | --- |
| `cortex-m0plus` | A Cortex-M0+ board | Not measured | Not run | Not known | Not measured |
| `cortex-m4` | A Cortex-M4 board | Not measured | Not run | Not known | Not measured |
| `rtc-power-loss` | A board with a backed RTC | Not measured | Not run | Backed RTC | Not measured |
| `byte-programmable` | Modelled NOR, byte programs | 4096 / 1 / 1 | Not swept at this program unit | None | 63.37 |
| `word-programmable` | Modelled NOR, word programs | 4096 / 4 / 1 | Swept on the host model | None | 72.50 |
| `page-programmable` | Modelled NOR, 16-byte pages | 4096 / 16 / 1 | Not swept at this program unit | None | 118.00 |

## How to read the columns

**Erase/program/read** is the geometry in bytes. `Not measured` means that no board has
reported one.

**Power cut** says whether the power-cut and watchdog-reset loops have run on that part.
There are three answers:

- `Not run` — no board has ever run these loops.
- `Swept on the host model` — the loops ran against `waymaker-fault`'s model of NOR, at every
  crash point it enumerates. The rig's own oracle accepted every one.
- `Not swept at this program unit` — every crash sweep in this repository lays the part out
  with a four-byte program unit. Two modelled rows therefore carry a measured wear figure
  and no sweep of their own.

**Clock** says whether the part has a clock that outlives the supply. A model has none, so a
firmware on a model may arm no `AtPersistentTime` deadline.

**Written B per effect** is the bytes programmed per completed effect. The gate measures it
over an eight-effect run at a fixed seed, and injects no crash.

The figure counts what the device was *asked* for, including calls that would have failed.
Design document §12 says a failed program may still have changed media. A figure that
counted only the successes would understate the runs that wore the part hardest.

## Why the figure depends on the part

A record is a frame padded up to the program unit, plus a commit seal of one more unit. A
part that programs sixteen bytes at a time pays rounding twice where a byte-programmable part
pays none.

## What the host cannot supply

A model is not a part. The model starts erased. It only clears bits. Its barrier does
nothing. It finishes the unit that interrupted it.

A real part can do more. It can program a cell weakly. It can abort the unit in flight. It
has a reset-cause register and retained RAM, and no model here has either.

A board must report the erase counts over a part's life, and the wear those erases cause.

## What would fill a row

A rig log from the board fills a row. The census must be complete, and there must be no
breach.

The `rtc-power-loss` row needs more. Remove the supply for longer than the interval. The
first replay after that must recognise the deadline as elapsed.

A board cannot complete the failure matrix, and nothing asks it to. The rig reaches six of
its ten rows by design. To tell three of those six apart, you must know whether the
dispatcher ran and returned. A harness knows that. A reset takes it with the RAM.

`waymaker-rig` links on the target. Since the `emulate` stage it also *runs* on two of
them, under QEMU: a Cortex-M0 and a Cortex-M4. That is the two architectures Waymaker is
built for, executing the rig's own code — and it is not two parts.

An emulated core has no NOR flash, no supply to remove, no reset-cause register and no
backup domain. QEMU has no Cortex-M0+ at all. So every row above stays `Not run`, and the
emulated boot may not be cited to move one. See
[ADR 0040](https://github.com/madmax983/waymaker/blob/main/docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md).

`waymaker-rig` has never run on a board.
