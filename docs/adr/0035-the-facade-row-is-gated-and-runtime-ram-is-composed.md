# ADR 0035: the façade row is gated, and runtime RAM is composed rather than sampled

- Status: accepted
- Date: 2026-09-09
- Issue: [#39](https://github.com/madmax983/waymaker/issues/39)
- Supersedes: one decision of
  [ADR 0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md) — "RAM
  accounting is a floor, and is named for what it measures". Nothing else in 0002 changes:
  its deltas, its probe and its baseline are what this builds on.
- Related:
  [ADR 0029](0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md),
  [ADR 0032](0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md)

## Context

Design document §04 states four resource budgets and says of the code-flash one that it "is
a gate, not an unverified claim". Issue #39 is rung 0.4's exit criterion: the budgets were
set before the façade existed, and this is where they are paid.

Three things were true before this change, and each is a budget that was not being paid.

The `facade` row was measured and printed, and nothing failed a build over it. §04 states
the code-flash budget for "core + flash adapter", so `INCREMENTAL_CODE_FLASH_BYTES` covers
the `default` row and covers the façade not at all. A regression in `waymaker-embassy` moved
a number nobody gated.

Runtime RAM was gated as `.data + .bss`, which for this engine is 0 B. §04's sentence is
"cursor, context, record header, and storage scratch": the cursor and the record header are
in `waymaker_core::budget`'s kernel-state registry, the scratch page is the caller's, and the **context** — `waymaker-embassy`'s `Ctx` — was measured nowhere at all.
The size report said so honestly, calling its figure "a floor on §04's runtime RAM and not
the rule itself", and a floor of zero is not a budget.

The generated workflow future was measured nowhere either. §04 excludes it from the runtime
RAM budget and reports it separately; nothing reported it. Issue #39 states the risk
exactly: "a tiny context does not compensate for a large generated future".

## Decision

**The `facade` row is gated, against a ceiling of its own.** `matrix()` marks it
`gated: true`, and `code_flash_budget_for` holds it to
`waymaker_core::budget::FACADE_CODE_FLASH_BYTES` — 13 KiB — rather than to the engine's
12 KiB. Two ceilings rather than one raise: the façade is a third crate, and raising the
engine's number to pay for it is how a kernel budget widens for a cost the kernel does not
carry. A `const` assertion refuses a façade ceiling below the engine's, because the façade
image strictly contains the engine one. Any other gated row falls back to the engine's
ceiling, which is the stricter of the two.

**Runtime RAM is composed, and the composition is what is gated.**
`SizeReport::runtime_ram_total` adds the 512 B caller-owned scratch page, the kernel-state
registry, the context, and the largest `Δram` of any row, and `Budget::RuntimeRam`
holds the sum to §04's 768 B. Each term keeps a sub-budget: `KERNEL_STATE_BYTES` is 128 B,
and `CONTEXT_RAM_BYTES` is what that leaves of `ENGINE_RAM_BYTES` — 128 B — asserted at
compile time to partition it exactly, so two shares cannot both pass while their sum fails.

**The context is measured on the types the firmware links.**
`waymaker_drive::ota::CONTEXT_BYTES` is `size_of::<Ctx<'_, Downloader, Bridge<'_>>>()`, and
`waymaker_core::assert_context_size!` beside it is the gate for the target the budget is
stated for — the `drive-firmware` stage compiles that module for `thumbv6m-none-eabi`.
`Ctx` borrows its journal, its dispatcher and its buffer, so its size is the same for every
`D` and `J`; naming the pair the firmware links is what makes the figure a reading of this
image rather than of a fixture.

**Generated workflow futures are reported in a section of their own and summed into
nothing.** `waymaker_drive::ota::WORKFLOW_FUTURES` is a `const` registry, and the size
report prints it under a heading that says it is not part of the runtime RAM budget above
it. Nothing gates it — §04 sets no budget for user memory — and a test requires that a
future 64 KiB wide move no gated number.

**Every one of them fails closed.** A report with no gated `facade` row, no runtime RAM
section, no named workflow future, or a term missing from the composition is
`BudgetShortfall::Unmeasurable` rather than a pass.

## Consequences

The measured figures. The code-flash and statics rows are read off images linked for
`thumbv6m-none-eabi` with the release-size profile; the context, kernel-state and future rows
are host `size_of`, which the paragraph below explains:

| Budget | Measured | Ceiling |
| --- | --- | --- |
| Code flash, `default` | 12220 B | 12288 B |
| Code flash, `facade` | 12618 B | 13312 B |
| Runtime RAM | 672 B | 768 B |
| — context | 56 B | 128 B |
| — kernel state | 104 B | 128 B |
| — scratch page | 512 B | — |
| — engine statics | 0 B | 256 B |
| Kernel state | 104 B | 128 B |
| `ota_update` future | 168 B | not budgeted |

All three budgets pass with the façade in the build, and no ceiling was raised to make them.
The façade's 13 KiB is new rather than a raise: it leaves 694 B, which is the rest of rung
0.4 rather than a number fitted to today's image.

The context and the future figures are **host** sizes, because `xtask` is compiled for the
host. That is [`KernelState::measured`]'s standing and the same argument: only pointers
differ and a `thumbv6m` pointer is narrower, so the host figure is an upper bound and gating
it can fail early but never late. Measured on the target the same two types are 28 B and
104 B — half of 56 and rather less than 168 — so both reported figures overstate the part by
about a factor of two. The exact check for the context is the `const` assertion, which the
firmware stages compile. There is no exact check for the future: a future's size has no
`const` value a firmware build can compare, and reading it off the linked image would need a
symbol this workspace cannot declare without the `unsafe` it forbids.

What is still not measured is the **depth of the call chain**. §04 names four terms and the
composition covers all four, with the statics delta added on top; three of them live on the
stack, and a deeper chain holding them moves no writable section and no type size. The report says so where it prints the total, rather than printing "runtime RAM: ok".

The report schema is 3. `runtime` is absent from a base-branch report, because
`measure_baseline` links that worktree with *this* binary and can no more read its context
than it can read its kernel-state registry; `runtime_ram_change` says "not compared" rather
than printing the head's figure as though it were both.

`xtask` now depends on `waymaker-drive`, which is the third test-support crate it reaches
for a measurement it declines to transcribe — `waymaker-fault` and `waymaker-rig` are the
other two, for the write-amplification figure. The gate is unaffected:
`check_dependency_direction` iterates `policy::LAYERS` and `xtask` is `policy::HOST_TOOLS`,
so this grants no layer anything.

That edge does cost one thing, and
[ADR 0032](0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md) named it
before it existed. `waymaker-drive`'s `without-facade` is a negative feature, and the stated
reason it was safe was that nothing depended on the crate. Something does now. Feature
unification would take `ota` out of the crate `xtask` reads its context term from — so
enabling `without-facade` workspace-wide breaks the `xtask` build. No build does: the
`drive-facadeless` stage selects it with `-p waymaker-drive`, which unifies with nothing. The
failure would be a compile error rather than a quiet zero, which is the direction to fail in,
and the manifest comment says so where the feature is declared.

Two things the gate still does not know. The registry of workflow futures is a list somebody
writes, where `kernel_state_types!` applies its own assertion to every type it registers — a
second `async fn` workflow joins neither the registry nor the context assertion, and no rule
notices. And `runtime_ram_change` always reports "not compared", for `kernel_state_change`'s
reason: only the head binary can read a type size, so a future that grew is visible in the run
that measured it and in no diff.

## Alternatives considered

**Raise `INCREMENTAL_CODE_FLASH_BYTES` to 13 KiB and gate both rows on it.** One number is
simpler, and it is the wrong one: §04 states that budget for "core + flash adapter", so the
engine row would have gained 1 KiB of slack it never asked for, and the next reader would
find a kernel budget that had grown to pay for a crate above it. ADR 0029 cut this number
precisely because it had twice been raised for something it did not measure.

**Measure the context in `waymaker-embassy`.** `Ctx` is generic and the crate declares no
concrete `D` or `J`, so a registry there would need dummy types written only to be sized —
and a public function on a layer is one `size-probe-reach` obliges the probe to call, which
would charge §04's code-flash budget for the measurement of §04's RAM budget.

**Read the future size out of the linked image.** A `static` in a named section, sized by
the future, would give the target figure exactly. Both `#[unsafe(no_mangle)]` and
`#[unsafe(link_section)]` are unsafe attributes, and every firmware crate here is
`#![forbid(unsafe_code)]`. Rejected in favour of a host figure that is honestly labelled as
an upper bound.

**Gate the workflow future too.** §04 excludes user workflow memory from the budget, and a
ceiling on a user's own state machine is a limit on what a workflow may be rather than on
what this engine costs. What issue #39 asks for is that it be *visible*, so it is reported
and a test requires it to move no gated number.
