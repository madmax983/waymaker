# The hardware compatibility matrix

**No board has ever run Waymaker.** The table says so, and this repository will not let it
say anything else: a row moves to `Passed` only when an accepted decision record carries an
attestation for it, and the `hardware-matrix` gate rule reads the power-cut column out of
that record rather than out of this page.

The last column is measured on every run of the gate. If the engine's write amplification
changes, this page fails the build until it is corrected.

| Id | Part | Erase/program/read | Power cut | Clock | Written B per effect |
| --- | --- | --- | --- | --- | --- |
| `cortex-m0plus` | A Cortex-M0+ board | Not measured | Not run | Not known | Not measured |
| `cortex-m4` | A Cortex-M4 board | Not measured | Not run | Not known | Not measured |
| `rtc-power-loss` | A board with a backed RTC | Not measured | Not run | Backed RTC | Not measured |
| `byte-programmable` | Modelled NOR, byte programs | 4096 / 1 / 1 | Swept on the host model | None | 63.37 |
| `word-programmable` | Modelled NOR, word programs | 4096 / 4 / 1 | Swept on the host model | None | 72.50 |
| `page-programmable` | Modelled NOR, 16-byte pages | 4096 / 16 / 1 | Swept on the host model | None | 118.00 |

## How to read the columns

**Erase/program/read** is the geometry in bytes. `Not measured` means no board has reported
one.

**Power cut** is whether the power-cut and watchdog-reset loops have passed. `Not run` means
they have not run on that part at all. `Swept on the host model` means the loops ran against
`waymaker-fault`'s model of NOR, at every crash point it enumerates, and the rig's own oracle
accepted every one.

**Clock** is whether the part has a clock that outlives the supply. A model has none, so a
firmware on one may arm no `AtPersistentTime` deadline.

**Written B per effect** is bytes programmed per completed effect, measured over an
eight-effect run at a fixed seed. It counts what the device was *asked* for, including calls
that would have failed: a failed program may still have changed media, and a figure that
counted only successes would understate the runs that wore the part hardest.

## Why the figure depends on the part

A record's frame is the same everywhere. Its commit seal is one program unit. A part that
programs sixteen bytes at a time therefore pays sixteen bytes to commit a frame that a
byte-programmable part commits in one.

## What the host cannot supply

A model is not a part. It starts erased, it only clears bits, its barrier does nothing, and
it finishes the unit it was interrupted in. A real part can program a cell weakly, can abort
the unit in flight, and has a reset-cause register and retained RAM that no model here has.

Erase counts over a part's life, and the wear those erases really cause, are a board's to
report.

## What would fill a row

A rig log from the board, with the census complete, the failure matrix complete and no
breach — plus, for `rtc-power-loss`, the supply removed for longer than the interval and the
first replay after it recognising the deadline as elapsed.

`waymaker-rig` is written to link on the target. It has never been on one.
