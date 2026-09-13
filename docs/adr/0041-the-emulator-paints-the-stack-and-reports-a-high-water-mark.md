# ADR 0041: the emulator paints the stack, and reports a high-water mark

- Status: accepted
- Date: 2026-09-13
- Issue: [#47](https://github.com/madmax983/waymaker/issues/47)
- Supersedes: nothing
- Related: [0040](0040-the-emulator-runs-the-rig-and-attests-to-no-board.md),
  [0038](0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md),
  [0035](0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md),
  [0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md)

## Context

Issue #47 is a question ADR 0002 asked itself and then deferred: `cargo xtask size` gates
runtime RAM as an ELF section delta, and named that "engine statics" rather than "runtime
RAM" for a stated reason — design document §04's own accounting is "cursor, context, record
header, and storage scratch", and most of that lives on the stack, not in `.data` or `.bss`.
A deeper call chain moves no writable section and no type size, so the gate stays green while
the real number goes wherever it likes. ADR 0035 composed the figure further — a scratch
page, a kernel-state registry, a context — and said the same thing about the fourth term:

> What is still not measured is the depth of the call chain... The report says so where it
> prints the total, rather than printing "runtime RAM: ok".

ADR 0040 then built the one thing in this workspace that runs Waymaker's code on the part
§04's budgets are stated for, and named the obvious next use of it without taking it:

> An emulated boot is the first thing in this workspace that could measure it — paint the
> region and read the high-water mark after the run — and it does not. Doing it needs a
> second `unsafe` expansion and a memory-map symbol, and this change would rather ship a boot
> that is honestly a boot than a boot with a half-argued number attached.

Issue #47's own "done when" accepts a report that says precisely what is measured and what is
not — and that half was already true before this change. What was still missing was a
measured number, and two of the three routes issue #47 names are closed by this workspace's
own toolchain: `-Z emit-stack-sizes` and `cargo-call-stack` both need nightly, and
`rust-toolchain.toml` pins stable deliberately, for `workspace-lints`' reason — a floating or
dual toolchain turns an unrelated change red for a lint or a feature nobody in it saw. The
third route, a compile-time bounded-depth invariant, proves the engine recurses nowhere; it
does not produce a byte figure, because depth and frame size are two different facts and a
proof of the first says nothing about the second.

## Decision

**Each emulated boot paints its own unused stack before the rig runs, and reports how far the
paint was disturbed after.**

`waymaker_emu::stack` is the new module, and it is the second and last reason this crate
writes the `unsafe` keyword by hand, after `#[cortex_m_rt::entry]` and `debug::exit`.
`main::measured_run` — `#[inline(never)]`, so its frame cannot be folded back into `main`'s —
holds every local the rig and the conformance suite use. `main` takes the address of a local
of its own, `depth_from`, before calling anything else; `stack::paint` fills every byte from
the linker's `__ebss` up to `depth_from` (less a small guard margin near `depth_from`, which
is never painted, so the two functions measuring it cannot corrupt their own frames) with a
repeating, non-trivial byte; `measured_run` is called; and `stack::high_water_mark` scans back
from `__ebss` for the first byte the run disturbed. The figure can only read *high*: nothing
below the deepest disturbed byte was touched, and the guard margin is never painted, so it
always counts as used.

**The number is reported and gated, not folded into §04's own figure.** `cargo xtask size` and
`cargo xtask emulate` measure two different things and always will: the size gate's runtime
RAM figure is the *engine's* four terms, on a host build that never links `waymaker-rig` or
`waymaker-conformance` at all; this image links both alongside the three layers, boots the
whole thing on real ARM cores, and reports the *whole call chain's* depth on *this* run. A
number that conflated the two would be exactly the "half-argued number" ADR 0040 declined to
attach. So `xtask::emulate::StackUsage` is read from a third census line — `used` and
`available` — gated on its own: a run that reports an empty region, or one in which the paint
was disturbed all the way down, is not a measurement that happened, and `Report::shortfall`
fails it by the same rule every other emulate failure obeys. The two machines' `StackUsage`
values are **not** required to agree, unlike their `Census`: a Cortex-M0 and a Cortex-M4
compile the same source into different instructions, so a different byte count here is
expected, where a different census would mean the rig behaved differently on the two.

**`emulation-boot`'s `unsafe`-keyword rule grows a third, narrowly named exception.** Before
this, the rule read: no file of `waymaker-emu` writes `unsafe`, full stop, with the two macro
expansions never spelling the keyword in this crate's own source at all. `paint` and
`high_water_mark` are hand-written, so the rule now also permits the keyword inside the
braced body of a function named in `xtask::emulate::PERMITTED_UNSAFE_FUNCTIONS`, and inside
the one `unsafe extern "C" { .. }` block the 2024 edition requires to name a linker symbol —
both confined to the one file `xtask::emulate::STACK_MODULE` names. A decoy `fn paint`
anywhere else in the crate gets none of it; `unsafe_in_paint_or_high_water_mark_is_permitted_only_in_stack_rs`
is the test that says so.

## Consequences

**A real, measured stack figure exists where before there was none**, on both architectures
this workspace is built for. It is reported per machine, gated for running out of room on
either, and never smoothed into a single cross-machine number the way the census is — because
the two machines' code is not the same code, only the same source.

**It is not §04's figure, and does not become one.** `cargo xtask size`'s runtime RAM total —
the scratch page, the kernel-state registry, the context, and the statics delta — is
untouched: it is still stack-blind for call-chain depth, and its own report still says so.
This ADR closes issue #47 by the "or" in its own "done when": a real number now exists, it is
gated, and the two figures' difference in scope is stated rather than implied — which is a
sharper answer than folding one into the other would have been, because the two are not
interchangeable.

**The exception surface grows by two named functions, not by a crate.** `emulation-boot`'s
scan is unchanged everywhere else in the crate: `main.rs`, `boot.rs` and `nor.rs` still permit
nothing but the lint name. `hand_written_unsafe_is_reported` and
`hand_written_unsafe_in_a_sibling_module_is_reported` still pass unmodified, and
`unsafe_in_stack_rs_outside_the_two_named_functions_is_reported` is the sibling test showing
the same file does not get a blanket pass.

**What is still owed.** A decoy `stack.rs` filed in a different directory of the crate would
be read as the permitted module too, the same limit `check_image_attributes` already carries
for `main.rs`; `CLAUDE.md`'s "what is not checked" names it. The figure is a whole-image
number: it cannot be split into the engine's share and the rig-and-conformance harness's
without a call graph, which is the same tool this ADR declined to add nightly for. And the
guard margin's size — 128 bytes — is chosen rather than measured; it is conservative against
what two small, register-heavy functions could plausibly spill, not a proven bound.

## Alternatives considered

**`#[cortex_m_rt::pre_init]`**, which runs before `.data`/`.bss` are initialised and so avoids
any question of where `paint`'s own locals land relative to the caller's. Rejected: the
attribute's own documentation warns that even `&1` inside a `#[pre_init]` function or
anything it calls is immediate undefined behaviour, through rvalue static promotion, before
any static is valid to touch — a hazard this workspace cannot verify by running the result,
since no board and no CI job here can catch a bug that only sometimes reproduces. Doing the
paint from ordinary, fully-initialised `main`-time code, with the frame-isolation `main`'s own
marker and `measured_run`'s `#[inline(never)]` provide, trades a theoretically tighter window
for a mechanism this change could actually reason about and inspect in the linked image's
disassembly.

**A generic "any `unsafe` block carrying a `// SAFETY:` comment" exception**, rather than two
named functions in one named file. Rejected for matching this workspace's own convention worse:
every other pin in this gate — `EFFECT_STEP_BODIES`, `SEALING_FUNCTIONS`, and
`emulation-boot`'s own file-and-attribute checks — names a program element and requires it
found, not a floating comment convention a scanner cannot verify is even true of the code
beneath it. A comment can be copied anywhere; a function name checked against a fixed list,
in one file, is what the rest of this gate already does.

**Threading `Trouble::Stack` through `boot::Trouble` for a degenerate paint region.** Rejected
because the two questions are independent: `boot::Trouble` is about whether the rig and the
conformance suite behaved, and a degenerate stack region is a fact about the image's own
memory map, checked in `main` before `measured_run` is ever called. Folding it in would have
made `boot::run`'s signature answer for something it does not do.
