# ADR 0038: `no_alloc` is a measurement, and the instruction figure is a comparison

- Status: accepted
- Date: 2026-09-10
- Issue: none — this closes a limit `CLAUDE.md` recorded about itself
- Supersedes: nothing
- Related: [0029](0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md),
  [0013](0013-the-fault-harness-is-a-crate-above-the-layers.md),
  [0021](0021-the-rig-is-a-no-std-library-and-its-knowledge-is-durable.md)

## Context

Design document §02 decision 1 is that the kernel is `no_std`, `no_alloc` and
dependency-free. Two of those three have been build failures since rung 0.0:
`crate-attributes` fails a crate that drops `#![no_std]` or declares `extern crate alloc`,
and `kernel-zero-dependencies` fails one that grows a dependency of any kind.

The third was an argument, and `CLAUDE.md` said so in the list of things nobody should
mistake silence for:

> **Allocation, as a measurement.** `bounded-decoding` proves the decoder is total and stays
> inside its input; the allocation half is structural — a `no_std` crate with no dependencies
> and no `extern crate alloc` cannot allocate, and `crate-attributes` and
> `kernel-zero-dependencies` fail a build over each of those. A global allocator that counted
> allocations would need the `unsafe` this workspace denies.

Both halves of that are true and the conclusion does not follow. `#![no_std]` and the absence
of `extern crate alloc` are facts about a *crate*. What a firmware pays for is what the linked
image does, and the gap between the two is every route by which `alloc` arrives without
anybody writing the words: a dependency that turns a feature on, a generic instantiated where
it is available, a `format!` on a branch nothing links today. This workspace does not accept
that shape of argument anywhere else — it is the same shape as "the probe reaches every public
function, so the budget covers them", which `size-probe-reach` makes a build failure rather
than a claim.

The last sentence is the one that had been doing the work, and it considered one mechanism.
A `#[global_allocator]` that counted allocations would indeed need `unsafe`. But a heap
profiler does not run inside the program: Valgrind intercepts `malloc` in the *binary*, below
anything Rust can express, so it needs no allocator, no attribute and no exception to
`unsafe_code = "deny"`.

There is a second gap beside it, smaller and worth taking in the same change. §04 budgets
flash and RAM and says nothing about time, so this repository publishes no cost figure at all
— and the obvious one to publish, wall-clock time, is worthless in CI, because a loaded runner
and an idle one disagree about a commit that did not change. An instruction count does not:
the same commit measures the same, so a figure that moved is a change rather than a neighbour.

## Decision

`cargo xtask profile` runs both tools over two workloads and answers in one report. It is a
stage of its own, in a `profiling` job, for the reason the `verification` and `layering` jobs
are: a red one says a §02 decision no longer holds, and that belongs in the checks list under
a name that says so.

**The heap half is a gate at zero.** `profile::ENGINE_HEAP_BLOCKS` is `0`, in blocks rather
than bytes, because `malloc(0)` returns a pointer and a firmware that reached it has an
allocator linked whatever the byte count says.

**The instruction half is published and not gated**, for the reason `xtask::wear` publishes
write amplification without a budget: §04 states no instruction target, and a ceiling invented
here would be a number nobody agreed to.

**The engine is derived, not listed.** It is `policy::LAYERS` plus
`policy::NO_STD_TEST_SUPPORT_CRATES` — the three layers and the three test-support crates that
each claim to be allocation-free — so a crate joining either category is gated without anybody
remembering a row. `waymaker-fault` is deliberately outside it: it models media in a `Vec`, so
engine code calling `program` reaches an allocation on every workload, through the model rather
than through anything a real driver links.

**Attribution reads the source path first, then the symbol's root, then the mangled name.**
Each of the three is a decision:

* *The path first*, because a function inlined into another crate keeps its own source file in
  the debug info and loses its crate from the printed symbol. ADR 0029 records name-only
  attribution mistaking an inlined body for its caller as an accepted limit of the code-flash
  gate, where it makes the number stricter; here it would charge inlined engine code to the
  harness, which is this gate passing for the reason it exists to catch. Both tools are
  therefore run with `--fullpath-after=`, and `[profile.profiling]` turns fat LTO off so that
  there is a boundary left to read.
* *The symbol's **root***, not a mention of it anywhere in the string. This was a live defect
  rather than a precaution: the first run of this gate failed the `driver` row over four bytes
  attributed to `waymaker-core`, and the frame was
  `with_capacity_in<waymaker_core::activity::ActivityKind, alloc::alloc::Global>` — `alloc`'s
  body with a kernel type passed to it, allocating a `Vec` in the harness beside it. A generic
  argument is not a definition.
* *The mangled name last*, through `size::defining_crate` — the reader the code-flash gate
  already attributes by, so the two gates cannot disagree about which crate wrote a body.

**Everything fails closed**, which is this workspace's rule met by a tool rather than by a
rule. A DHAT run that saw no allocation anywhere in the process is `Unmeasurable` rather than a
pass, because the zero it prints for the engine is the zero it would print for a process that
never started. So is a callgrind run that attributed no instruction to any engine crate, a
workload that completed no effect, a report with no rows, and a report missing a row
`WORKLOADS` declares. And `parse_callgrind` refuses to answer at all unless the per-function
costs add up to callgrind's own declared total — which is not belt and braces, because three
separate ways of misreading that format each produce a plausible number: counting the cost line
after a `calls=` as self cost, which double-counts every callee and grows the figure with call
depth; reading `fl=` and `fn=` out of one name table, when callgrind compresses each position
kind in a namespace of its own and their ids collide in a real run of this workspace more than
a hundred times; and missing the event column, which contributes silently zero. The sum catches
all three.

**The effect count is read from the workload rather than taken from the table.** A workload
whose denominator came from `WORKLOADS` would publish a cost per effect over a number nothing
measured, and the error would *flatter* the engine — the direction `xtask::wear` already avoids
by counting what the device was asked for.

## Consequences

The claim is now a number, and the number is zero: both workloads allocate nothing in an engine
crate, against sixteen blocks the harness and the runtime allocate in the same process. The
instruction figures are `journal` at 544 281 Ir over eight effects and `driver` at 41 709 over
two.

What got worse, in the order it will be noticed.

**The pipeline grew a dependency the toolchain does not carry.** `rust-toolchain.toml` cannot
pin valgrind, so the `profiling` job installs it with `apt-get` — the same standing as
`cargo-llvm-cov` in the `check` job, and the same mitigation: the command fails closed when the
tool is absent, so a missing valgrind is an install failure rather than a gate that quietly
passed.

**The workspace manifest grew a second profile.** `[profile.profiling]` inherits `release` and
overrides three settings, each buying something the measurement cannot be taken without.
`release-profile` still fails a build in which `[profile.release]` moves at all, so the row the
code-flash budget is measured against cannot drift — but it does mean the instruction figures
are a reading of one optimiser's output at a setting no board is flashed with. That is the
standing ADR 0029 already records for the corrected code-flash figure, met once more.

**It is a sampled gate where the specification's proofs are exhaustive.** Two workloads reach
§09's codec, §10's reserve, the recovery scan, §06's boundary and §07's protocol. They reach no
bank swap, no `continue_as_new`, no capacity refusal, no divergent replay and no timer — the
same four rows the failure matrix calls `Owed` on the rig, met again one gate over. An
allocation on one of those paths is an allocation nothing watches for, and
[what is not checked](../../CLAUDE.md#what-is-not-checked) says so rather than letting a zero
read as a proof.

**The instruction figures are not about a part, and will be read as though they were.** They
are host instructions, on the host's instruction set. A Cortex-M0+ has another encoding,
another cost per instruction and no branch predictor, so nothing here converts into a cycle
count on the hardware §04's budgets are stated for. The report says this on every run rather
than in a document somebody has to find, and the boards owe the real figure exactly as
`docs::HARDWARE_TARGETS` records for everything else.

**`xtask` grew a subcommand nothing else calls.** `profile-workload` is the process the tools
are pointed at, and it exists only for the length of one measurement.

## Alternatives considered

**A `#[global_allocator]` that counts.** The mechanism the old bullet considered and rejected,
and the rejection was right: it needs `unsafe`, which this workspace denies, and it would only
see allocations that go through Rust's allocator — not one a C dependency makes. It also
measures nothing about an image nobody linked with it.

**A crate of its own for the workloads.** A new workspace member is a row in `policy::LAYERS`
or in one of the three categories beside it, a bullet in `CLAUDE.md`, and a category to argue
about — for code nothing links, which runs under a tool for the length of one measurement.
`xtask` already depends on all four crates the workloads drive, for the reason `wear` gives.

**Wall-clock time instead of instruction counts.** It is the number people ask for and it
cannot be gated or compared: two runs of one commit disagree by more than a real regression.
An instruction count is deterministic, which is the entire reason callgrind is here and
`perf` is not.

**Gating the instruction figure too.** §04 states no instruction target. A ceiling invented
here would be a number nobody agreed to, and the first pull request to exceed it would raise
it — which is how a budget stops meaning anything. `xtask::wear` publishes an ungated figure
for the same reason, and says so.

**Charging an allocation to the outermost workspace frame.** It would make `waymaker-fault`'s
media `Vec` the engine's on every workload, so the gate could never pass and would be turned
off. The innermost workspace frame is the code that decided to allocate, which is the question
being asked.

**Attributing by symbol name alone.** Simpler, and wrong in the one direction that matters:
an inlined engine body loses its crate from the printed name, and the allocation lands on
whoever inlined it. The path is the half an optimiser cannot move.
