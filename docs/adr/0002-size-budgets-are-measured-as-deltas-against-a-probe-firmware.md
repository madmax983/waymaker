# ADR 0002: Size budgets are measured as deltas against a probe firmware

- Status: accepted
- Date: 2026-09-01
- Issue: [#10](https://github.com/madmax983/waymaker/issues/10)
- Supersedes: nothing
- Related: [ADR 0001](0001-one-pipeline-table-and-a-per-crate-coverage-gate.md)

## Context

Design document §04 states four resource budgets and is explicit that the code-flash one
"is a gate, not an unverified claim". It also requires that "CI records section sizes for
every feature combination" and that "adding Serde, Postcard, `defmt`, Embassy, or a CRC
implementation must show its own incremental cost".

Three of those budgets are numbers about a linked image, and at rung 0.0 the workspace has
no linked image: three library crates with documentation in them and nothing to link. A
gate that waits for something to measure is a gate that arrives after the code it was
supposed to constrain.

## Decision

### The budgets live in `waymaker-core`, not in the gate

`waymaker_core::budget` holds the numbers, and `xtask` depends on that crate rather than
transcribing them. A budget that lives in two places is a budget that ends up disagreeing
with itself, and the compile-time assertions and the CI gate are exactly the two places
that must not.

### Kernel state is a compile error, not a report line

`waymaker_core::budget::kernel_state_types!` takes one list of types and produces three
things from it: the registry the size report prints, the total, and a `const` assertion per
type and on the total. A type cannot join the kernel's live state without being budgeted,
and cannot be budgeted without appearing in the report.

The assertion is evaluated for whichever target the crate is compiled for. Every row of the
size matrix compiles `waymaker-core` for `thumbv6m-none-eabi`, and so does the firmware
job, so the authoritative evaluation is on the target the budget is stated for. The figure
`cargo xtask size` prints is a host build of the same constant — reported so the number has
a place in the artifact, and gated too, because a host figure over budget means the target
figure is over budget as well. Where the two can differ (a type holding a pointer is smaller
on the target), the target-side assertion is the one that fails the build.

### Sizes are deltas against a baseline image

`crates/waymaker-size-probe` is an example firmware whose only purpose is to be linked. It
is built twice: once with none of the layers linked, and once with them. The budget is the
difference.

An absolute size would charge Waymaker for the panic handler and the ARM exception index,
and would move with the toolchain rather than with this repository. §04's number is
incremental — "≤ 8 KiB core + flash adapter" — so the measurement is a subtraction.

Deltas saturate at zero. A row that links *less* than the baseline is a measurement fault or
a linker that dropped something, not a negative cost, and letting it offset another row's
growth is precisely the arithmetic a budget must not do.

### The probe has no runtime and no `unsafe`

There is no `cortex-m-rt`, no linker script, and no entry symbol. The image is linked, never
flashed and never run; leaving out a startup file keeps the baseline at very nearly nothing,
which is what makes the delta a measurement of Waymaker.

A firmware entry point would normally need `#[unsafe(no_mangle)]`, and the workspace denies
`unsafe_code`. It is not needed: a `#[used]` static holding a function pointer survives
`--gc-sections` and drags the code it points at along with it. The probe therefore obeys the
same `#![forbid(unsafe_code)]` rule as the crates it measures, and there is no exemption for
a reviewer to weigh.

The probe declares no scratch page. The runtime RAM budget is stated "with a 512 B scratch
page" and the page is caller-owned; a probe that declared one would put the caller's buffer
into `.bss` and charge the engine for it. The RAM gate is therefore
`ENGINE_RAM_BYTES` — 768 B less the 512 B page — which is what the engine may own itself.

Its binary sits behind `required-features = ["probe"]`. Without that, `cargo build
--workspace` and `cargo clippy --all-targets` would try to link `#![no_main]` firmware for
the host, and the fix somebody reaches for under that pressure is to make the probe `std` —
at which point every delta becomes a measurement of the standard library. `size-probe` is a
gate rule so that this stays true.

### Sections are read, not shelled out for

`xtask::elf` parses the ELF section header table rather than running `llvm-size` or
`arm-none-eabi-size`. A gate whose measurement depends on a binary that may or may not be
installed reports "tool missing" on the day it matters, and a parser is a pure function over
bytes, so a truncated table or an out-of-range name offset can be tested against images no
linker would produce.

Only section headers are read, never symbols, which is what makes this work against the
`strip = "symbols"` release profile the budgets are measured with.

Accounting is by flags and type rather than by name:

- **flash** is every allocated section whose type is not `SHT_NOBITS`. Wider than
  `.text + .rodata + .data`: `.ARM.exidx` costs flash and is named after none of them, and
  the budget is about bytes programmed into the part.
- **RAM** is every allocated section that is writable — `.data` plus `.bss`.
- `.text`, `.rodata`, `.data` and `.bss` are reported individually, each folding in the
  pieces the linker split out of it (`.text.unlikely`, `.bss.probe`), because a report that
  missed those would show a shrinking `.text` for a growing image.

### The matrix is derived, never written down

§04 requires every optional feature to show its own cost. A hand-written list of feature
combinations is a list a new feature can be left out of, silently, by the pull request that
would most want measuring. `size::matrix` reads the features each layer declares out of
`cargo metadata`, so adding a feature adds a row and there is nothing to remember. A
feature of `waymaker-embassy` is measured with the façade linked and everything below it on
the engine, decided from the crate name rather than from a table.

The `default` and `facade` rows are gated: they are the engine with no optional cost
enabled, which is what the v0.1 targets describe. Per-feature rows are reported but not
gated, because §04 requires an optional cost to be *shown* and sets no per-feature budget.
The base-branch diff is what makes their growth visible in review.

### The base branch is measured, not remembered

A committed baseline file would need updating in every pull request that changes a byte,
which is the manual bookkeeping this issue asks to avoid. Instead the base branch is checked
out into a `git worktree` and measured with the *same* build of the gate — which is how the
pull request introducing the gate can still produce a diff, and how a later change to the
accounting rules compares like with like.

`cargo metadata` resolves the nearest manifest at or above its working directory, and the
worktree lives under `target/`. A base commit with no manifest of its own would therefore
leave cargo walking up into the current workspace and measuring *that*, reporting "no
change" for every row — the most convincing possible way to report nothing at all. The gate
checks `workspace_root` against the directory it asked about and fails closed.

A base that cannot be measured is reported as "not compared", never as a failure. A missing
comparison is not a budget breach, and the budgets are gated either way.

## Consequences

- The size job links the matrix twice on a pull request, once for each side of the diff. It
  is its own job so nothing waits on it, and a budget breach is legible in the checks list.
- Every number in the report is ~0 at rung 0.0, because there is almost nothing to link
  yet. That is the honest reading, and the harness is what the issue asks for: rungs 0.1
  through 0.4 get their cost report without further work.
- The probe is a fourth firmware crate in the workspace that ships nothing. It is declared
  in `policy::MEASUREMENT_FIXTURES` so the membership rule does not read it as a layer, and
  `size::check_size_probe` applies the rules that do belong to it.
- `xtask` now depends on `waymaker-core`. The gate depends on the thing it gates, which is
  the point: there is one table of numbers.

## Alternatives considered

**Shell out to `llvm-size`.** Rejected: it makes the measurement depend on a binary the
toolchain does not guarantee, and puts the parsing in a format this repository would then
have to parse anyway.

**Commit a baseline JSON file and diff against it.** Rejected: every size-changing pull
request would have to update it by hand, which is the bookkeeping the issue rules out, and a
stale committed number is worse than no number.

**Make the probe an `example` of `waymaker-flash`.** Rejected: examples are built by
`cargo clippy --all-targets`, which runs on the host, and gating one behind a feature would
put a `size-probe` feature into the auto-derived matrix as though it were an optional cost.

**Gate every per-feature row.** Rejected for now: §04 budgets the engine, not each optional
feature it can carry. When a feature needs a budget of its own it gets a row in
`waymaker_core::budget` and this decision is revisited.
