# 1. One pipeline table, and a per-crate coverage gate

- Status: accepted
- Date: 2026-09-01
- Issue: [#9](https://github.com/madmax983/waymaker/issues/9)
- Design document: §04 resource budgets, §15 testing and verification

## Context

Issue #9 asks for four things: an ordered pipeline (`fmt` → `clippy` → `test` → coverage),
a `thumbv6m-none-eabi` build with no default features, a coverage gate at 85% "reported per
crate so kernel coverage cannot hide behind adapter code", and pre-commit hooks that mirror
the pipeline "so CI is confirmation, not discovery".

The obvious implementation is a YAML file plus a shell script. Both would contain the same
commands on the day they are written, and neither would contain the same commands a month
later. Nothing in that arrangement notices when they diverge, and the failure is silent in
the worst direction: the hook keeps passing while CI is testing something else.

The repository already has an answer to that shape of problem. The layering contract from
design document §05 is not a convention that reviewers remember; it is a table in
`xtask/src/policy.rs` that a gate reads and fails a pull request over.

## Decision

**The pipeline is a table.** `xtask/src/pipeline.rs` holds one `Stage` row per command —
its name, the workflow job it belongs to, the command verbatim, and whether the pre-commit
hook runs it. Three rules read that table:

| Rule | Fails when |
| --- | --- |
| `ci-pipeline` | `.github/workflows/ci.yml` stops running a stage, runs it in a different job, or runs the stages of one job out of order |
| `pre-commit-hook` | `.githooks/pre-commit` is missing, is not executable, or is not byte-for-byte what the table renders |
| `toolchain-targets` | `rust-toolchain.toml` stops pinning `thumbv6m-none-eabi` |

The hook is *generated* by `cargo xtask install-hooks` rather than written, so "the hook and
the pipeline run the same commands" is a fact about how the file is produced rather than a
claim in a README.

**The firmware target is pinned in `rust-toolchain.toml`, not installed by a CI step.** A
step installs it for CI only; a `targets` entry installs it for a local checkout too, which
is what makes running the firmware build locally possible at all.

**The coverage gate is per crate, and it is 85% of lines.** `cargo xtask coverage` runs
`cargo llvm-cov` over the same workspace and feature selection as the test stage, buckets
its JSON export by the crate each file belongs to, and fails if any single crate is below
the gate. The workspace total is printed and deliberately not gated: a total is precisely
how an untested kernel hides behind a tested adapter, which is the failure issue #9 names.

The comparison is integer arithmetic in basis points (8500 = 85.00%), not floating point,
so a crate at 84.999% is below the gate and no rounding mode gets a say in it.

**A crate with no coverable lines passes, and the report says so.** At rung 0.0 the three
firmware crates are documentation and attributes; llvm-cov reports no regions for them. A
gate that failed them would be measuring the absence of code rather than the absence of
tests. Their rows read `n/a — no coverable lines` rather than being omitted, because a crate
disappearing from the report is exactly the failure this gate exists to catch. The moment
one of them has a function, it has lines, and the gate applies to it with no change here.

## Consequences

- Adding a pipeline stage is one row. Forgetting to add it to the workflow or the hook is a
  failed build with the stage named.
- Changing a command means regenerating the hook (`cargo xtask install-hooks`); a
  hand-edited hook fails the gate.
- Today the coverage gate has teeth only on `xtask`, the only crate with executable code.
  That is honest rather than aspirational: the gate is in place and measured before the code
  it will govern is written, instead of being retrofitted after coverage has already
  slipped.
- The gate depends on `cargo-llvm-cov`, which no rustup profile carries. CI installs it
  before the pipeline starts, and `cargo xtask coverage` fails with an install hint rather
  than passing when the tool is absent: a coverage run that did not happen is not a coverage
  run that passed.
- `ci-pipeline` scans the workflow for `run:` commands rather than parsing YAML, which keeps
  `xtask` at two dependencies. It recognises inline and block scalars and ignores comments;
  it is not a general YAML implementation, and a workflow written in an exotic style could
  confuse it. The tests pin the forms this repository uses.

## Alternatives considered

- **`cargo llvm-cov --fail-under-lines 85`.** One flag instead of a module, and one number
  for the whole workspace — the exact failure issue #9 asks to prevent.
- **A hook installed by a script that copies commands out of the YAML.** Turns the drift
  into a parsing problem without turning it into a failure.
- **`rustup target add` as a CI step.** Leaves a local checkout unable to run the firmware
  build, so the firmware build becomes a thing CI discovers rather than a thing a
  contributor confirms.
