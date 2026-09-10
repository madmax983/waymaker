# ADR 0039: the emulator runs the rig, and attests to no board

- Status: accepted
- Date: 2026-09-10
- Issue: none — this closes a limit `CLAUDE.md` recorded about its own firmware stages
- Supersedes: nothing
- Related: [0021](0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md),
  [0016](0016-the-storage-contract-is-a-conformance-suite-and-a-port.md),
  [0031](0031-a-persistent-clock-is-two-registers-and-the-board-run-is-a-checked-absence.md),
  [0038](0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md)

## Context

`waymaker-rig` exists because a rig that could only run on a host would be a simulation
wearing a rig's name — [ADR 0021](0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md)
is the whole of that argument, and the `rig-firmware` stage is what it rests on: the crate's
library is built for `thumbv6m-none-eabi` on every push, so "written to run on the part" is a
build failure rather than an attribute.

`CLAUDE.md` has always been exact about what that stage does *not* buy, and said it in the
same breath as the `crate-attributes` rule that backstops it:

> The three that *claim* `#![no_std]` are held to it here rather than by their
> firmware-target build stages, which cannot hold the allocation half: `cargo build --lib`
> produces an rlib and never links, so no global allocator is required and an
> `extern crate alloc` under any of them compiles clean.

An rlib is not an image. Nothing places a reset vector, nothing resolves a `#[panic_handler]`,
nothing links `compiler_builtins`, and no instruction is ever retired. So the position before
this change was that every claim about the rig on ARM rested on a compiler's willingness to
*emit* code that nothing had ever executed. `waymaker-fault`, `waymaker-spec` and every test
in the workspace run on x86-64, under `std`, with a 64-bit ALU and an allocator present.

[ADR 0038](0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md) is
the shape of the fix, one gate over. It took a sentence this file had carried for five rungs
— *"a global allocator that counted allocations would need the `unsafe` this workspace
denies"* — and observed that it considered exactly one mechanism. The same reading applies
here: "we cannot run firmware in CI because we have no boards" considers one mechanism, and
an emulator is another.

What an emulator is not is a board, and the temptation this change has to be argued against
is precisely that. `docs::HARDWARE_TARGETS` holds three rows at `Not run` — power-cut loops
on a Cortex-M0+, the same on a Cortex-M4, and an `AtPersistentTime` deadline across a total
supply loss on a part with a backed RTC — and a green check named "emulated boot" beside them
is an invitation to conclude that two of the three are now covered. They are not, and this
decision is as much about saying so as about the stage.

## Decision

**A linked image runs the rig on both instruction sets, and attests to nothing about a
board.**

`waymaker-emu` is a new workspace member in a category of its own,
`policy::EMULATION_CRATES`: a `#![no_std]`, `#![no_main]` firmware binary with a reset
vector, a vector table and a memory map, behind `required-features = ["emu"]` so that no host
build tries to link it. `cargo xtask emulate` builds it once per core, starts each under
`qemu-system-arm`, and gates what the images say they did. The `emulate` stage runs it in a CI
job of its own.

**Two cores, and their answers must agree.** `-machine microbit` is a Cortex-M0 — **ARMv6-M**,
the architecture `thumbv6m-none-eabi` targets and the one design document §04's budgets are
stated for. `-machine mps2-an386` is a Cortex-M4 — **ARMv7E-M**. The two censuses are then
required to be *equal*, which is the sharper half of the gate: the plan is deterministic, so a
difference is not a tolerance but the rig behaving differently on two instruction sets. A
`u64` shift lowered through `compiler_builtins` on one and an instruction on the other, an
alignment assumption, a `usize` narrowing — none of those show up as anything else here, and
none of them shows up on a host at all.

**The media is a model, and the model is interrogated first.** Neither machine has a flash
part a driver can program, so `waymaker_emu::nor::Nor` is NOR modelled in the emulator's RAM:
erased is `0xFF`, a program only clears bits, and an operation the geometry forbids never
reaches a cell. It is presented through `waymaker_conformance::nor::NorFlashStorage` — the
adapter [ADR 0016](0016-the-storage-contract-is-a-conformance-suite-and-a-port.md) landed —
rather than through a second `StableStorage` written here, so the boot exercises the port this
workspace ships. And before the rig is run over it, `waymaker-conformance`'s suite is run over
it: that crate is `#![no_std]` and allocation-free *precisely* so an adapter author can run it
on the target the driver is for, and this is the first place in the workspace that takes it up
on the offer. A rig run over a model nobody had asked whether it obeys §12 would be a rig run
over an unknown quantity.

**The census is the gate, not the exit code.** An image whose `main` returned before it
reached the rig exits exactly the way a complete one does. So the image prints what it
counted, the harness parses it, and a run is failed when no conformance case passed, when no
iteration ran, when no iteration was cut — without which only clean runs were driven and
recovery answered nothing — when a cut run was left unaccounted for, or when a run reached no
verdict. Both sides check: the image refuses its own census (`boot::Census::complete`) and so
exits non-zero, and the harness checks again (`emulate::Census::shortfall`), because a gate
that trusted the subject's own verdict would be reading a claim rather than taking a
measurement.

**Everything fails closed**, which is this workspace's rule met by one more tool: QEMU absent,
an image that will not link, a non-zero exit, a run that outlives the timeout, and — the one
that matters — a run that exited zero having printed nothing.

**And no row of `docs::HARDWARE_TARGETS` moves.** `hardware-attestation` already requires an
accepted ADR carrying the attestation marker to move a row, and this ADR deliberately carries
none. That is not an oversight to be corrected later by this image; see the next section for
why it could not be.

The gate rule is `emulation-boot`, and its five halves are in `CLAUDE.md`. The sharpest is the
`unsafe` one. This is the only crate in the workspace carrying `#![allow(unsafe_code)]` — the
workspace manifest names the escape in as many words, *"`deny` keeps a documented exception a
reviewable one-line `#![allow(unsafe_code)]` plus an ADR"* — and the whole of what it is
carried for is two macro expansions: `#[cortex_m_rt::entry]`, which writes the exported symbol
the reset vector points at, and `debug::exit`, which performs the semihosting call. So the
rule requires that no file of the crate writes the `unsafe` **keyword**, as opposed to naming
the lint in the `allow`. Without that, the exception would be a licence for a crate rather
than for two expansions, and the one place in this workspace where `unsafe` is permitted would
be the one place nothing checks.

## Consequences

**What is now established that was not.** The rig's code executes on ARMv6-M and on ARMv7E-M:
every branch it takes here is a branch a Cortex-M0 and a Cortex-M4 really retired, its 64-bit
arithmetic went through the routines a core with no 64-bit ALU uses, and — the half no `--lib`
stage can reach — the image *linked*, with a panic handler resolved, `compiler_builtins`
pulled in, and no allocator anywhere. The two cores agree, case for case and count for count.

**What is not established, and cannot be by this.** A board. Neither machine has:

- **A NOR part.** The media is an array in RAM. Program-disturb, weak bits, a unit aborted in
  flight, a part that finishes a unit the model would not — every one of them is outside this,
  exactly as `CLAUDE.md` already records for `waymaker-fault`.
- **A supply that can be removed.** The "cut" is the host cut: the iteration stops where it
  stands, and the RAM survives it. §15's power-cut loops are a bench.
- **A reset-cause register, retained RAM, or a backup domain.** So the `rtc-power-loss` row is
  untouched, and `waymaker_rig::rtc::BackedRtc`'s continuity bit is still a board's word.
- **A Cortex-M0+.** QEMU has no such machine. A Cortex-M0 implements the same instruction set
  and is a different core — different pipeline, an optional MPU, a different fast-I/O port —
  so this covers the *architecture* of that row and not the part.

All three rows therefore stay `Not run`, and this ADR carries no attestation marker, so
`hardware-attestation` would fail a build in which somebody moved one and cited this.

**The gate was watched failing before it was closed**, which is the only thing that makes a
passing run worth anything. Three mutations, each on the real images and the real harness. An
image that starts, runs and calls `debug::exit(EXIT_SUCCESS)` before reaching the rig — the
exact shape of the failure this gate exists for — is reported as *"the image ran and printed
no census this gate can read, which is not a measurement that passed"*, not as a pass. A boot
whose cutter is `NeverCut`, so that only clean runs are driven and recovery is asked nothing,
is refused by the image's own census and then by the harness. And a hand-written `unsafe`
block in a sibling module of the crate fails `emulation-boot` on the file that wrote it. The
two-cores-disagree case is a unit test rather than a mutation, because forcing it needs a
compiler bug.

**The cost.** Two third-party firmware dependencies — `cortex-m-rt` and
`cortex-m-semihosting` — enter the workspace for the first time. They reach no layer, no
test-support crate and no shipped image: `waymaker-emu` is outside `default-members`, nothing
depends on it, and both are behind its `emu` feature. `dependency-direction` and
`kernel-zero-dependencies` are what keep that true rather than the sentence.

A second Rust target, `thumbv7em-none-eabi`, is pinned in `rust-toolchain.toml`, which every
fresh checkout now installs. `emulation-boot` requires every machine's target to be pinned, so
a core added to the table without a target is a build failure rather than a stage that fails
on a clean machine for a reason that is not a defect.

The `emulate` stage needs a tool no rustup profile carries. That is `profile`'s cost paid a
second time, and the answer is the same: the pipeline installs it in the job, and the command
fails closed rather than skipping.

**What is still owed.** Stack depth. `CLAUDE.md` records under
[the budgets](../../CLAUDE.md#budgets) that three of runtime RAM's four terms live on the
stack and that the *depth* of the chain holding them is unaccounted, because it moves no
writable section and no type size. An emulated boot is the first thing in this workspace that
could measure it — paint the region and read the high-water mark after the run — and it does
not. Doing it needs a second `unsafe` expansion and a memory-map symbol, and this change
would rather ship a boot that is honestly a boot than a boot with a half-argued number
attached. It is the obvious next thing this image is good for.

## Alternatives considered

**Renode rather than QEMU.** Renode models real parts rather than architectures — an nRF52840
or an RA4M1 with an internal flash peripheral — and can script a reset mid-write, which is
much closer to what §15 asks for. It was not taken *first* for three reasons, and none of them
is that QEMU is better. It is a 200 MB install against an `apt-get` package. Its flash
peripherals are generally RAM-backed too, so the "real NOR" it appears to offer is mostly not
one — a `Nor` model whose §12 conformance is checked in the same boot is the more honest
version of the same thing. And it brings a `.resc`/`.repl` scripting layer, which is a second
thing to keep in step with a stage table this workspace already checks byte for byte. What
Renode would genuinely add is a reset *inside* a write on a modelled peripheral, and that is
worth doing on its own terms, as an addition to `emulate::MACHINES` rather than a replacement
for it. It would still not be a board.

**One machine rather than two.** Half the cost, and it would have measured one encoding. The
equality check between the two censuses is the thing this gate has that a single-core version
could not, and it is the check most likely to catch something.

**Writing a `StableStorage` directly instead of going through the port.** Fewer moving parts,
and it would have put an adapter in the boot path that nothing else in the workspace ships —
so a green run would have been reporting on that adapter. Going through
`NorFlashStorage` means the emulated boot exercises the port issue #21 landed, on the target
the port exists for.

**Making the image `#![forbid(unsafe_code)]` like every other crate.** Not possible: the reset
vector and the semihosting exit are macro expansions containing the attribute, and `forbid`
cannot be relaxed. The `emulation-boot` `unsafe`-keyword ban is what recovers most of what
`forbid` would have bought, and it is scoped where `forbid` could not be.

**Moving `cortex-m-rt` into `waymaker-rig` and giving that crate a binary.** It would have
avoided a new workspace member, and it would have put a firmware runtime and a semihosting
channel into the crate the `rig-firmware` stage builds — so the thing being tested and the
harness testing it would be one crate, which is the arrangement
[ADR 0013](0013-the-fault-harness-is-a-crate-above-the-layers.md) argues against for the fault
harness and [ADR 0021](0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md) for
the rig.
