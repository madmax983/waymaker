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

The `kernel_state_types!` list in `waymaker_core::budget`'s source takes one list of types
and produces three things from it: the registry the size report prints, the total, and a
`const` assertion per type and on the total. A type *in that list* therefore cannot escape
any of the three — it cannot be registered without being asserted, or asserted without
being reported.

What that does not do is force a new type into the list. Nothing detects a `Cursor` added to
`waymaker-core` that nobody registers; the list is a convention, and the report is honest
about its own emptiness (`0 B of 128 B across 0 registered type(s)`). A rule that guessed
which public types are live kernel state would have to false-positive on `TypeSize` itself,
so at rung 0.0, with no types to register, this is written down rather than enforced.

The assertion is evaluated for whichever target the crate is compiled for. Every row of the
size matrix *but the baseline* compiles `waymaker-core` for `thumbv6m-none-eabi` — the
baseline links no layer at all, which is its purpose — and so does the firmware job, so the
authoritative evaluation is on the target the budget is stated for.

The figure `cargo xtask size` prints is a host build of the same constant. A type holding a
pointer is smaller on the target, so the host figure is an **upper bound** on the target
one. Gating it is therefore conservative in the safe direction: it can fail early, never
late, and a build it fails might have fitted on the target. It is not evidence that the
target is over budget — the target-side assertion is what says that.

### The probe measures what it reaches, and the reach is a gate

A delta can only charge for code the linker keeps, and with `lto = "fat"` and
`--gc-sections` the linker keeps what the probe reaches. Enabling the optional dependency
is not enough, and neither is naming the crate: a public function nothing calls is
discarded. So a layer can grow an arbitrary amount of code while the 8 KiB gate keeps
reporting the same twenty-odd bytes of the probe's own arithmetic — and nothing else
notices, because the row is not identical to its base and the positive-delta test still
passes.

`size-probe-reach` closes that. It scans each layer's sources for public functions, outside
their `#[cfg(test)]` modules, and fails the build naming any the probe does not call. A
function the probe genuinely should not charge for is a function that should not be public,
or a deliberate exception — either is a conversation in review, which is where a decision
about what the budget covers belongs.

The rule matches identifiers in code and in call position, not in prose. It has to: the
first version was satisfied by the English word "of" appearing five times in the probe's
documentation while `TypeSize::of` went unlinked, which is the exact failure it exists to
catch. It counts a trait's methods and a trait impl's methods too, which carry no `pub` at
all — the trait's visibility is what makes them callable, and a `pub fn` scan would have let
a whole storage backend be implemented and dead-stripped without a word.

**It is a floor, not a proof.** A scanner establishes that every public function's name
appears in the probe in call position; it cannot establish that each was called. Two layers
declaring the same name are satisfied by one call to either, and a call behind a generic or
a trait object is credited to the name written rather than the body that runs. Deciding
those needs name resolution — a call graph, not a scanner. What the rule catches
mechanically and every time is the case that arrives silently: a layer gains a function and
nobody wires the probe up to it. That is the common case, and it is the one no other check
sees.

What remains beyond a gate is the *feature* half. A feature row whose code the probe does
not reach comes back byte for byte identical to the row below it, and that cannot be made a
gate: a feature which genuinely costs nothing is indistinguishable from one the probe does
not exercise. `SizeReport::notices` reports it instead, on every run, naming the row and
pointing at the probe.

At rung 0.0 the layers have no functions, so what the `default` and `facade` rows measure is
the probe's own arithmetic plus the cost of linking the crates. That is an honest zero for
code that does not exist, and `the_engine_costs_more_flash_than_the_baseline` in
`xtask/tests/size_budgets.rs` asserts only what it can: that the probe's own reachable code
survives the linker. Rung 0.1 fills in the marked call sites in `engine`, and the same test
becomes a statement about the kernel.

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

### RAM accounting is a floor, and is named for what it measures

The budget is reported as **engine statics**, not as runtime RAM, because that is what
`.data + .bss` is. Calling it runtime RAM would print "ok" for a §04 rule the measurement
cannot evaluate: a cursor, context or record header on the caller's stack moves no writable
section, and neither does a deeper call frame, so the number would stay green while real
runtime RAM went past 768 B. Stack accounting needs a call graph — `-Z emit-stack-sizes` or
equivalent — and belongs with the rung that first has one; issue filed separately.

### What the statics figure covers

`.data + .bss` is what a linked image says about RAM, and it is what the gate measures. §04's
runtime RAM rule — "Cursor, context, record header, and storage scratch" — covers things a
`no_std`, `no_alloc` design mostly does not put in statics: the cursor and context live on
the caller's stack, and the scratch page is the caller's. So this measures the engine's
statics, which is a floor on its RAM and not a measurement of the rule. The `size_of`
registry is the mechanism for the rest, and it is what grows at rung 0.1.

Thread-local sections are excluded: `.tdata` and `.tbss` are a template that thread storage
is initialised *from*, and counting both charges the same bytes twice. Nothing else is
excluded — without a linker script there is no memory map to say that `.got` or
`.init_array` were placed in flash, so they count as RAM. That errs toward failing the gate
rather than passing it, which is the direction a budget should err in.

### The matrix is derived, never written down

§04 requires every optional feature to show its own cost. A hand-written list of feature
combinations is a list a new feature can be left out of, silently, by the pull request that
would most want measuring. `size::matrix` reads the features each layer declares out of
`cargo metadata`, so adding a feature adds a row and there is nothing to remember. A
feature of `waymaker-embassy` is measured with the façade linked and everything below it on
the engine, decided from the crate name rather than from a table.

Only the `default` row is gated. §04 states the 8 KiB for "core + flash adapter", and that
row is exactly that. The `facade` row is reported: gating the Embassy façade against the
kernel's number would either fail a build for a cost that number never covered or — worse,
once someone raised the number to make it pass — quietly widen the kernel's budget to pay
for the façade. Per-feature rows are reported for the same reason: §04 requires an optional
cost to be *shown* and budgets none of them. The façade and each feature get a budget of
their own in `waymaker_core::budget` when they need one, and the base-branch diff is what
makes their growth visible until then.

### The base branch is measured, not remembered

A committed baseline file would need updating in every pull request that changes a byte,
which is the manual bookkeeping this issue asks to avoid. Instead the base branch is checked
out into a `git worktree` and measured with the *same* build of the gate — which is how the
pull request introducing the gate can still produce a diff, and how a later change to the
accounting rules compares like with like.

The diff compares *incremental cost* rather than absolute size. Both sides are built in the
same job with the same toolchain today, so the two agree — but a rustc bump or a change to
the panic handler moves every absolute number while changing nobody's cost, and a diff that
reported all of that as "changed" is a diff people stop reading.

`cargo metadata` resolves the nearest manifest at or above its working directory, and the
worktree lives under `target/`. A base commit with no manifest of its own would therefore
leave cargo walking up into the current workspace and measuring *that*, reporting "no
change" for every row — the most convincing possible way to report nothing at all. The gate
checks `workspace_root` against the directory it asked about and fails closed.

A base that cannot be measured is reported as "not compared", never as a failure. A missing
comparison is not a budget breach, and the budgets are gated either way.

### Everything that reads zero fails closed

The gate's one passing state that is not a measurement is a zero, so every route to a zero
is an error rather than a pass:

- a report with no rows, or with no `baseline` row;
- a baseline image reporting no bytes in flash at all, which no linked firmware does but a
  section-stripped file does;
- an ELF whose section header table holds only the reserved null entry, which is what
  `llvm-objcopy --strip-sections` leaves;
- a section name offset at or past the end of the string table, which would otherwise read
  as a nameless section and drop its size out of the breakdown;
- an image for a machine other than ARM — a host executable parses cleanly and measures
  plausibly;
- a report read back from JSON with a missing or mistyped size, or taken on another target.

A base worktree is swept only when it is older than any run could be, never by process id:
removing a checkout that a concurrent run is still measuring makes *that* run report "not
compared", which is a comparison lost silently rather than an error anyone sees.

Each variant also links into a target directory named after it. They share one artifact file
name, so a shared directory lets one variant's image be uplifted over another's between the
build and the read — which is a flaky test, and also a real image of the wrong variant
measured as though it were the right one.

## Consequences

- The size job links the matrix twice on a pull request, once for each side of the diff. It
  is its own job so nothing waits on it, and a budget breach is legible in the checks list.
- Every number in the report is ~0 at rung 0.0, because there is almost nothing to link
  yet. That is the honest reading, and the harness is what the issue asks for: rungs 0.1
  through 0.4 get their cost report without further work.
- The probe is a fourth firmware crate in the workspace that ships nothing. It is declared
  in `policy::MEASUREMENT_CRATES` so the membership rule does not read it as a layer, and
  `size::check_size_probe` applies the rules that do belong to it — including that each of
  its features still enables the crates its row is supposed to measure, because `engine = []`
  is a plausible thing to write while debugging a link failure and collapses every delta to
  zero, and that the probe calls every public function the layers declare.
- The probe is the one crate no lint stage covers: `required-features` keeps it out of
  `cargo clippy --all-targets`, which is the trade the `example` alternative below was
  rejected to get. Mistakes in it surface in the size job.
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
