# The hardware compatibility matrix

**No board has ever run Waymaker.** The table says so, and this repository will not let it
say anything else. Every cell below but the clock column is *derived*: the power-cut column
is read out of the attestation record, the geometry out of the modelled parts, and the last
column out of a measurement the gate takes on every run. A row that stated anything else
would fail the build, and so would prose on this page that claimed a board had cleared its
loops.

| Id | Part | Erase/program/read | Power cut | Clock | Written B per effect |
| --- | --- | --- | --- | --- | --- |
| `cortex-m0plus` | A Cortex-M0+ board | Not measured | Not run | Not known | Not measured |
| `cortex-m4` | A Cortex-M4 board | Not measured | Not run | Not known | Not measured |
| `rtc-power-loss` | A board with a backed RTC | Not measured | Not run | Backed RTC | Not measured |
| `byte-programmable` | Modelled NOR, byte programs | 4096 / 1 / 1 | Not swept at this program unit | None | 63.37 |
| `word-programmable` | Modelled NOR, word programs | 4096 / 4 / 1 | Swept on the host model | None | 72.50 |
| `page-programmable` | Modelled NOR, 16-byte pages | 4096 / 16 / 1 | Not swept at this program unit | None | 118.00 |

## How to read the columns

**Erase/program/read** is the geometry in bytes. `Not measured` means no board has reported
one.

**Power cut** is whether the power-cut and watchdog-reset loops have run on that part. `Not
run` means no board has ever been attached. `Swept on the host model` means the loops ran
against `waymaker-fault`'s model of NOR, at every crash point it enumerates, and the rig's
own oracle accepted every one. `Not swept at this program unit` is the honest answer for the
other two modelled rows: every crash sweep in this repository lays the part out with a
four-byte program unit, so those two rows carry a measured wear figure and no sweep of their
own.

**Clock** is whether the part has a clock that outlives the supply. A model has none, so a
firmware on one may arm no `AtPersistentTime` deadline.

**Written B per effect** is bytes programmed per completed effect, measured over an
eight-effect run at a fixed seed, with no crash injected. It counts what the device was
*asked* for, including calls that would have failed: a failed program may still have changed
media, and a figure that counted only successes would understate the runs that wore the part
hardest.

## Why the figure depends on the part

A record is a frame padded up to the program unit, plus a commit seal of one more unit. A
part that programs sixteen bytes at a time pays rounding twice where a byte-programmable part
pays none.

## What the host cannot supply

A model is not a part. It starts erased, it only clears bits, its barrier does nothing, and
it finishes the unit it was interrupted in. A real part can program a cell weakly, can abort
the unit in flight, and has a reset-cause register and retained RAM that no model here has.

Erase counts over a part's life, and the wear those erases really cause, are a board's to
report.

## What would fill a row

A rig log from the board, with the census complete and no breach — plus, for
`rtc-power-loss`, the supply removed for longer than the interval and the first replay after
it recognising the deadline as elapsed.

A board cannot complete the failure matrix, and is not asked to. The rig reaches six of its
ten rows by design, and three of those six are told apart by whether the dispatcher was
entered and returned — which a harness sees and a reset takes with the RAM.

`waymaker-rig` is written to link on the target. It has never been on one.
