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
| `ci-pipeline` | `.github/workflows/ci.yml` stops running a stage, runs it in a different job, runs the stages of one job out of order, or leaves a stage in place while making it unable to fail — an `if:` on the step or its job, a `continue-on-error:`, a missing `RUSTDOCFLAGS`, an `on:` block no pull request triggers, or a tab in the indentation |
| `pre-commit-hook` | `.githooks/pre-commit` is missing, is not executable, or is not byte-for-byte what the table renders |
| `toolchain-targets` | `rust-toolchain.toml` stops pinning `thumbv6m-none-eabi` |

The hook is *generated* by `cargo xtask install-hooks` rather than written, so "the hook and
the pipeline run the same commands" is a fact about how the file is produced rather than a
claim in a README.

**The firmware target is pinned in `rust-toolchain.toml`, not installed by a CI step.** A
step installs it for CI only; a `targets` entry installs it for a local checkout too, which
is what makes running the firmware build locally possible at all. The same file pins
`llvm-tools-preview` for the same reason: left out, `cargo llvm-cov` downloads it itself in
the middle of the pipeline.

**A gate is checked for whether it can fail, not only for whether it is present.** Comparing
command strings proves the text is in the file. It does not prove the command runs, and
every cheap way to make a stage unable to fail lives outside the command: an `if:` on the
step or the job, a `continue-on-error:`, a `run: |` block whose body sits in a dead shell
branch, a missing `RUSTDOCFLAGS`, an `on:` block no pull request triggers, a job with no
`runs-on:`. Each of those is a rule. So is `.cargo/config.toml`, which is read by every
cargo invocation and by no other rule: rewriting the `xtask` alias turns both gates into
commands that exit zero while every command string in the workflow still matches.

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
tests. Their rows read `n/a` with the verdict `no coverable lines` rather than being omitted,
because a crate disappearing from the report is exactly the failure this gate exists to
catch. The moment
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
- **The percentage includes `#[cfg(test)]` module bodies.** llvm-cov instruments the test
  binary, and a test body is covered by construction, so a crate with a lot of test code
  reports a higher number than its non-test code earns. Splitting `xtask/src/coverage.rs` at
  the `mod tests` boundary at the time of writing: 85.5% for the module, 99.4% for its
  tests. The gate is therefore a floor on a diluted number, not a precise statement about
  untested source. It is stated here rather than worked around because llvm-cov has no
  per-module exclusion, and excluding whole files would hide the very code the gate exists
  to measure.
- "No coverable lines" is the gate's one passing state that is not a measurement, so it is
  where anything hidden from the measurement lands — `[lib] test = false`, an exclusion
  regex, code behind a feature the coverage run does not enable. A crate whose root declares
  a function and which contributed no lines is therefore an error, not a pass, and `[lib]
  test = false` is rejected by the manifest rule outright.
- Coverage measurement must not leak into the builds it measures. `cargo llvm-cov` works by
  installing a `RUSTC_WRAPPER`, and a test that shells out to `cargo build --target
  thumbv6m-none-eabi` inherits it — that build then fails, because there is no profiler
  runtime for the firmware target, or worse, succeeds as a stale cache hit and verifies
  nothing. The tests spawn cargo through `coverage::uninstrumented_cargo`, and the coverage
  run itself drops `LLVM_COV_FLAGS`, which could otherwise remove files from the report.
- A report that attributes no file to any crate in this workspace is an error, not a pass.
  Bucketing is by path prefix, so an export produced in another checkout — a downloaded CI
  artifact, a container with a different working directory — would otherwise report every
  crate as having nothing to cover and exit zero.
- The gate depends on `cargo-llvm-cov`, which no rustup profile carries. CI installs it
  before the pipeline starts, and `cargo xtask coverage` fails with an install hint rather
  than passing when the tool is absent: a coverage run that did not happen is not a coverage
  run that passed.
- `ci-pipeline` scans the workflow for `run:` commands rather than parsing YAML, which keeps
  `xtask` at two dependencies. It handles the forms this repository uses and a few it does
  not — quoted and anchored job keys, trailing comments, any job indent, `run: |` blocks —
  and ignores comments in both. It is not a general YAML implementation. Two constructs are
  deliberately unsupported and fail closed, reading as a missing stage rather than a present
  one: a folded scalar (`run: >`), whose lines YAML joins into a single command that a
  line-oriented scanner would misread, and flow mappings (`- {run: cmd}`).

## Alternatives considered

- **A YAML parser.** `serde_yaml` is deprecated and the alternatives are a new dependency
  in a repository whose gate crate has two. The scanner fails closed on what it does not
  understand, which is the property that matters.
- **`cargo llvm-cov --fail-under-lines 85`.** One flag instead of a module, and one number
  for the whole workspace — the exact failure issue #9 asks to prevent.
- **A hook installed by a script that copies commands out of the YAML.** Turns the drift
  into a parsing problem without turning it into a failure.
- **`rustup target add` as a CI step.** Leaves a local checkout unable to run the firmware
  build, so the firmware build becomes a thing CI discovers rather than a thing a
  contributor confirms.
