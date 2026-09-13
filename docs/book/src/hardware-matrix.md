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

`waymaker-rig` links on the target. Since the `emulate` stage it also *runs* on three of
them, under QEMU: a Cortex-M0 (ARMv6-M) and a Cortex-M4 (ARMv7E-M), and an ESP32-S3 (Xtensa
LX7). That is every architecture Waymaker is built for, executing the rig's own code — and
it is not three parts.

The ESP32-S3 joins the run only when `WAYMAKER_XTENSA_OPT_IN` is set. Its toolchain, its
emulator, its cargo cache, `esptool`, and the emulator's libraries are provisioned beside
the checkout rather than in CI, so the CI `emulation` job runs the ARM pair. An opted-in
machine with missing dependencies still fails the run rather than skipping: the opt-in
chooses the machines, it never excuses a missing one.

The ESP32-S3 run is emulation, not hardware: no physical board has ever run Waymaker. Its
image is built by the Espressif Rust fork — `espup` installs it as the `esp` toolchain —
because only the fork knows `xtensa-esp32s3-none-elf`. The build needs `-Z build-std=core`
(the fork ships prebuilt std for the host alone), links against the crate's
`memory-xtensa.x` through the toolchain's GCC driver with `-nostartfiles` (so no toolchain
`crt0` supplies its own `_start`) — the driver is `xtensa-esp32s3-elf-gcc` in
`~/.rustup/toolchains/esp/xtensa-esp-elf/esp-15.2.0_20250920/xtensa-esp-elf/bin`, which
the build puts on `PATH` itself rather than requiring the toolchain's export script —
and goes through `esptool elf2image` (from `~/workspace/esptools/py`)
into a 4 MiB flash image with the image at offset `0x0` — the offset the S3 ROM loads the
boot image from, the one real hardware boots. The QEMU is the Espressif fork at 9.2.2
(`esp_develop_9.2.2_20260417`), at `~/workspace/waymaker/esp32s3/qemu/bin/qemu-system-xtensa`,
started as

```console
qemu-system-xtensa -nographic -machine esp32s3 \
    -drive file=flash_image.bin,if=mtd,format=raw \
    -serial file:uart0.log -monitor none -no-reboot
```

The guest never exits, so the harness polls `uart0.log` for the census and terminates QEMU
itself once a complete one is there. Starting the ELF directly with `-kernel` was never
verified — ROM boot from the flash image is the only verified path. The fork's binary
links against libraries no package manager provides (`libslirp.so.0` among them); the
harness puts `~/workspace/tooling/qemu-esp32/libs/usr/lib/x86_64-linux-gnu` on
`LD_LIBRARY_PATH`, as the spike verified.

No emulated machine has NOR flash, a supply to remove, a reset-cause register or a backup
domain. QEMU has no Cortex-M0+ at all. So every row above stays `Not run`, and no emulated
boot may be cited to move one. See
[ADR 0040](https://github.com/madmax983/waymaker/blob/main/docs/adr/0040-the-emulator-runs-the-rig-and-attests-to-no-board.md).

`waymaker-rig` has never run on a board.
