//! The pipeline contract: one table, three consumers.
//!
//! Issue #9 asks for a pre-commit hook that mirrors CI "so CI is confirmation, not
//! discovery". Two files that happen to contain the same commands are not a mirror; they
//! are two copies waiting to drift. So the stages live in [`STAGES`], the hook is
//! *rendered* from that table, and the rules here fail a pull request in which the
//! committed workflow or the committed hook no longer matches it.

use std::path::{Path, PathBuf};

use toml::Value;

use crate::Violation;

/// The firmware target every change is built for.
///
/// Design document §15: "Build on `thumbv6m-none-eabi` with no default features." A change
/// that only builds on the host is not a change that works.
pub const FIRMWARE_TARGET: &str = "thumbv6m-none-eabi";

/// Where the workflow the stages must appear in lives, relative to the workspace root.
pub const WORKFLOW_PATH: &str = ".github/workflows/ci.yml";

/// Where the generated hook lives, relative to the workspace root.
///
/// Not `.git/hooks`: that directory is not tracked. `cargo xtask install-hooks` points
/// git at this one with `core.hooksPath`, so the hook is reviewable like any other file.
pub const PRE_COMMIT_PATH: &str = ".githooks/pre-commit";

/// Where the pinned toolchain lives, relative to the workspace root.
pub const TOOLCHAIN_PATH: &str = "rust-toolchain.toml";

/// Workflow environment a stage's command relies on to mean what the table says.
///
/// `cargo doc` reports a broken intra-doc link as a warning and exits zero. What turns it
/// into a failure is `RUSTDOCFLAGS`, which lives in the workflow's `env:` block rather than
/// in the command, so comparing the command alone would let the stage keep passing the gate
/// while doing nothing.
pub const REQUIRED_WORKFLOW_ENV: &[(&str, &str)] = &[("RUSTDOCFLAGS", "-D warnings")];

/// Events the workflow must be triggered by.
///
/// A pipeline that no pull request runs is a pipeline that gates nothing, and the change
/// that removes the trigger is one line in a file no rule would otherwise read.
pub const REQUIRED_TRIGGERS: &[&str] = &["pull_request"];

/// Toolchain components a stage needs that `profile = "minimal"` does not carry.
///
/// `cargo llvm-cov` needs `llvm-tools-preview`. Left out of the toolchain file, the tool
/// installs it itself over the network in the middle of the pipeline, which works until it
/// does not and is invisible until then.
pub const REQUIRED_COMPONENTS: &[&str] = &["llvm-tools-preview"];

/// One command the pipeline runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stage {
    /// Short identifier, used as the violation subject and in the hook's progress lines.
    pub name: &'static str,
    /// The workflow job this stage belongs to. Order is checked within a job, because
    /// separate jobs run in parallel and have no order to check.
    pub job: &'static str,
    /// The command, verbatim. Compared byte for byte against the workflow and rendered
    /// byte for byte into the hook, so a dropped flag is a violation rather than a
    /// near-enough match.
    pub command: &'static str,
    /// Whether the pre-commit hook runs it.
    pub in_hook: bool,
    /// Why the stage exists, quoted in the violation when it goes missing.
    pub why: &'static str,
}

/// The pipeline, in the order issue #9 specifies: fmt, clippy, test, coverage.
///
/// This table is the contract. The workflow and the hook are checked against it, and
/// [`render_pre_commit_hook`] generates the hook from it, so a stage cannot be added to
/// one and forgotten in the other.
pub const STAGES: &[Stage] = &[
    Stage {
        name: "format",
        job: "check",
        command: "cargo fmt --all --check",
        in_hook: true,
        why: "a formatting diff in review is noise that hides the change",
    },
    Stage {
        name: "lint",
        job: "check",
        command: "cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings",
        in_hook: true,
        why: "the workspace denies pedantic, nursery and unwrap_used; -D warnings is what makes that real",
    },
    Stage {
        name: "build",
        job: "check",
        command: "cargo build --locked --workspace --no-default-features",
        in_hook: false,
        why: "the workspace builds on the host before anything is measured against it",
    },
    Stage {
        name: "test",
        job: "check",
        command: "cargo test --locked --workspace --no-default-features",
        in_hook: true,
        why: "no behavior ships without a test",
    },
    Stage {
        name: "docs",
        job: "check",
        command: "cargo doc --locked --workspace --no-deps --no-default-features",
        in_hook: false,
        why: "RUSTDOCFLAGS=-D warnings turns a broken intra-doc link into a failure",
    },
    Stage {
        name: "coverage",
        job: "check",
        command: "cargo --locked xtask coverage",
        in_hook: false,
        why: "the per-crate coverage gate; kept out of the hook because it re-runs the whole suite under instrumentation",
    },
    Stage {
        name: "firmware",
        job: "firmware",
        // No `--workspace`: `default-members` in the workspace manifest is exactly the
        // three firmware crates, so a crate added to the layering is built here without
        // anyone remembering to add a `-p` flag. `xtask` is host tooling and is excluded
        // by the same mechanism, which is the point of it.
        command: "cargo build --locked --no-default-features --target thumbv6m-none-eabi",
        in_hook: false,
        why: "design document §15: a change that only builds on the host is not a change that works",
    },
    Stage {
        name: "probe-lint",
        job: "firmware",
        // The size probe's binary is behind `required-features`, so the `lint` stage above
        // — which is `--all-targets --no-default-features` on the host — never compiles it.
        // Its crate root carries `#![no_std]`, `#![forbid(unsafe_code)]` and
        // `#![warn(missing_docs)]` like every other, and without this stage none of the
        // three is ever checked by a compiler: the layering gate would report the
        // attributes present while an undocumented public item sat underneath them.
        //
        // On the firmware target and with the features on, because that is the only
        // configuration in which a `#![no_main]` crate with a `#[panic_handler]` links at
        // all. `facade` implies `engine`, so this covers all three layers.
        command: "cargo clippy --locked -p waymaker-size-probe --target thumbv6m-none-eabi --features probe,facade --bins -- -D warnings",
        in_hook: false,
        why: "the probe's crate attributes are checked by the layering gate and by no compiler without this",
    },
    Stage {
        name: "size",
        job: "size",
        // Its own job because it links every feature combination on the firmware target
        // and, on a pull request, links the base branch's too. That is minutes of work
        // nothing else in the pipeline is waiting for, and a budget breach should be
        // legible in the checks list rather than at the end of another job's log.
        //
        // The command takes no arguments so that this table can compare it against the
        // workflow byte for byte: which base branch to diff against comes from the
        // environment GitHub already sets, not from an interpolated `${{ }}` expression
        // that no fixed string could match.
        command: "cargo --locked xtask size",
        in_hook: false,
        why: "design document \u{a7}04: the size budgets are gates rather than unverified claims",
    },
    Stage {
        name: "layering",
        job: "layering",
        command: "cargo --locked xtask check-layering",
        in_hook: false,
        why: "the layering contract from design document §05, as a check a reviewer can see",
    },
];

/// The stages the pre-commit hook runs, in pipeline order.
pub fn hook_stages() -> impl Iterator<Item = &'static Stage> {
    STAGES.iter().filter(|stage| stage.in_hook)
}

/// A `run:` command found in a workflow, with the job that contains it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStep {
    /// The job key the step was found under, or an empty string above the first job.
    pub job: String,
    /// One command line from the step's `run:` value.
    pub command: String,
    /// Whether an `if:` on the step or on its job can stop it from running.
    ///
    /// A stage that is present but conditional is not a stage that runs, so the rule
    /// treats this as a violation rather than as a match.
    pub conditional: bool,
}

/// Every command the workflow runs, in file order.
///
/// This is a scanner rather than a YAML parser on purpose: `xtask` has two dependencies
/// and the rule needs a handful of facts out of one file it also owns — which commands run,
/// in which job, in what order, and whether anything can stop them.
///
/// What it understands: job keys at whatever indent the file's first job uses, quoted and
/// anchored keys, trailing comments, `run: cmd`, `run: "cmd"`, and `run: |` blocks, and
/// `if:` on a step or on a job. A `run:` key counts only as a direct key of a list item, so
/// a `run:` nested under `with:` or `env:` is an input rather than a command.
///
/// What it deliberately does not credit a stage to, all of which fail closed — the stage
/// reads as missing rather than as present:
///
/// * a multi-line `run: |` or `run: >` block, because a line inside a shell script is not
///   a command that runs: it can sit in a dead `if` branch, after `set +e`, or inside a
///   heredoc, and a byte-for-byte match on such a line proves only that the text is there;
/// * flow mappings (`- {run: cmd}`).
///
/// A pipeline stage is therefore one step with one inline `run:`, which is also the form
/// that reads clearly in a workflow.
#[must_use]
pub fn run_steps(workflow: &str) -> Vec<RunStep> {
    Scan::of(workflow).steps
}

/// One list item being read, and what has been seen inside it so far.
#[derive(Debug)]
struct StepScan {
    /// The indent its keys sit at.
    key_indent: usize,
    /// The commands its `run:` key contributed.
    commands: Vec<String>,
    /// Whether it carries an `if:`.
    conditional: bool,
}

/// The scanner's state, and what it found.
#[derive(Debug, Default)]
struct Scan {
    steps: Vec<RunStep>,
    /// Jobs whose own `if:` can stop every step inside them.
    conditional_jobs: Vec<String>,
    /// Keys directly under the top-level `on:` key: the events that trigger the workflow.
    triggers: Vec<String>,
    /// Jobs that declare a `runs-on:`; a job without one is a job GitHub will not run.
    jobs_with_a_runner: Vec<String>,
    job: String,
    in_jobs: bool,
    in_on: bool,
    job_indent: Option<usize>,
    job_child_indent: Option<usize>,
    step: Option<StepScan>,
    block: Option<usize>,
}

impl Scan {
    fn of(workflow: &str) -> Self {
        let mut scan = Self::default();
        for line in workflow.lines() {
            scan.read(line);
        }
        scan.flush_step();
        scan.apply_job_conditions();
        scan
    }

    fn read(&mut self, line: &str) {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();

        if let Some(block_indent) = self.block {
            if trimmed.is_empty() {
                return;
            }
            if indent > block_indent {
                // Skipped, not recorded. A line inside a block scalar is a line of a shell
                // script, and its presence says nothing about whether it runs: it can sit
                // in a dead `if` branch, behind `set +e`, or inside a heredoc. The scanner
                // credits nothing to a block, so a stage written that way reads as missing
                // rather than as present.
                return;
            }
            self.block = None;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            return;
        }

        if indent == 0 {
            self.flush_step();
            self.in_jobs = key_name(trimmed).is_some_and(|key| key == "jobs");
            self.in_on = key_name(trimmed).is_some_and(|key| key == "on");
            if self.in_on {
                self.triggers.extend(flow_sequence(&scalar_value(trimmed)));
            }
            self.job.clear();
            self.job_indent = None;
            self.job_child_indent = None;
            return;
        }

        if self.in_on {
            if let Some(key) = key_name(trimmed) {
                self.triggers.push(key.to_owned());
            }
            return;
        }

        if self.in_jobs {
            if self.job_indent.is_none() {
                self.job_indent = Some(indent);
            }
            if self.job_indent == Some(indent) {
                self.flush_step();
                key_name(trimmed)
                    .unwrap_or_default()
                    .clone_into(&mut self.job);
                self.job_child_indent = None;
                return;
            }
            if self.job_child_indent.is_none() {
                self.job_child_indent = Some(indent);
            }
        }

        let (key_indent, text) = list_item(trimmed).map_or((indent, trimmed), |(offset, rest)| {
            (indent.saturating_add(offset), rest)
        });

        if key_indent != indent {
            // A list item at or above the current item's key indent ends that item.
            if self
                .step
                .as_ref()
                .is_none_or(|step| key_indent <= step.key_indent)
            {
                self.flush_step();
                self.step = Some(StepScan {
                    key_indent,
                    commands: Vec::new(),
                    conditional: false,
                });
            }
        }

        let Some(key) = key_name(text) else {
            return;
        };

        if self.in_jobs && Some(key_indent) == self.job_child_indent {
            match key {
                "if" => self.conditional_jobs.push(self.job.clone()),
                "runs-on" => self.jobs_with_a_runner.push(self.job.clone()),
                _ => {}
            }
        }

        let Some(step) = self.step.as_mut() else {
            return;
        };
        if key_indent != step.key_indent {
            // A key nested deeper than the item's own keys — `run:` under `with:` is an
            // action input, not a command.
            return;
        }
        match key {
            "if" => step.conditional = true,
            "run" => {
                let value = scalar_value(text);
                if value.starts_with('|') || value.starts_with('>') {
                    self.block = Some(key_indent);
                } else if !value.is_empty() {
                    step.commands.push(value);
                }
            }
            _ => {}
        }
    }

    fn flush_step(&mut self) {
        let Some(step) = self.step.take() else {
            return;
        };
        for command in step.commands {
            self.steps.push(RunStep {
                job: self.job.clone(),
                command,
                conditional: step.conditional,
            });
        }
    }

    /// A job's own `if:` may appear after its steps, so it is applied once at the end.
    fn apply_job_conditions(&mut self) {
        for step in &mut self.steps {
            if self.conditional_jobs.contains(&step.job) {
                step.conditional = true;
            }
        }
    }
}

/// The offset of a list item's content within its line, and that content.
fn list_item(trimmed: &str) -> Option<(usize, &str)> {
    let rest = trimmed.strip_prefix('-')?;
    let content = rest.trim_start();
    // `-` alone opens an item whose keys are on the following lines; there is no content
    // to attribute to it here.
    if content.is_empty() {
        return None;
    }
    Some((trimmed.len().saturating_sub(content.len()), content))
}

/// The key a `key: value` line declares, unquoted and without any trailing comment.
///
/// Returns `None` for a line that is not a mapping entry, such as a plain list item.
fn key_name(text: &str) -> Option<&str> {
    let (key, _) = text.split_once(':')?;
    Some(unquote(key.trim()))
}

/// The value a `key: value` line carries, unquoted and without any trailing comment.
fn scalar_value(text: &str) -> String {
    let Some((_, value)) = text.split_once(':') else {
        return String::new();
    };
    let value = value.trim();
    if value.starts_with('"') || value.starts_with('\'') {
        return unquote(value).to_owned();
    }
    // In a plain scalar, ` #` opens a YAML comment; the command stops there.
    value
        .split_once(" #")
        .map_or(value, |(command, _)| command.trim_end())
        .to_owned()
}

/// The entries of an inline `[a, b]` sequence, or nothing if `value` is not one.
fn flow_sequence(value: &str) -> Vec<String> {
    let Some(inner) = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return Vec::new();
    };
    inner
        .split(',')
        .map(|entry| unquote(entry.trim()).to_owned())
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// Strips one layer of matching quotes.
fn unquote(text: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = text.strip_prefix(quote).and_then(|r| r.strip_suffix(quote)) {
            return inner;
        }
    }
    text
}

/// Rule: the workflow runs every stage, in the job and the order the table names.
#[must_use]
pub fn check_workflow(workflow: Option<&str>) -> Vec<Violation> {
    let Some(workflow) = workflow else {
        return vec![Violation::new(
            "ci-pipeline",
            WORKFLOW_PATH,
            "there is no CI workflow, so no stage of the pipeline runs",
        )];
    };

    let steps = run_steps(workflow);
    let mut violations = Vec::new();

    for stage in STAGES {
        let matching: Vec<&RunStep> = steps
            .iter()
            .filter(|step| step.command == stage.command)
            .collect();

        let Some(matched) = matching.iter().find(|step| step.job == stage.job) else {
            let detail = if matching.is_empty() {
                format!(
                    "the workflow does not run `{}`: {}",
                    stage.command, stage.why
                )
            } else {
                format!(
                    "`{}` runs, but not in the `{}` job the pipeline table names",
                    stage.command, stage.job
                )
            };
            violations.push(Violation::new("ci-pipeline", stage.name, detail));
            continue;
        };

        if matched.conditional {
            violations.push(Violation::new(
                "ci-pipeline",
                stage.name,
                format!(
                    "`{}` is guarded by an `if:`, so it does not necessarily run; a stage that can be skipped is not a stage",
                    stage.command
                ),
            ));
        }
    }

    violations.extend(check_stage_order(STAGES, &steps));
    violations.extend(check_workflow_shape(workflow));

    for (key, value) in REQUIRED_WORKFLOW_ENV {
        if !sets_environment(workflow, key, value) {
            violations.push(Violation::new(
                "ci-pipeline",
                *key,
                format!(
                    "the workflow does not set `{key}: {value}`, without which a stage that depends on it passes while doing nothing"
                ),
            ));
        }
    }

    violations
}

/// Rule: within each job, the stages run in the order the table lists them.
///
/// Grouped by job rather than comparing table-adjacent rows: adjacency only happens to work
/// while every job's rows are contiguous in the table, and a rule that stops checking when
/// someone regroups the table is not a rule. Takes the table as a parameter so that the
/// regrouping can be tested rather than assumed.
#[must_use]
pub fn check_stage_order(stages: &[Stage], steps: &[RunStep]) -> Vec<Violation> {
    let mut violations = Vec::new();

    for job in jobs(stages) {
        let positions: Vec<(&Stage, usize)> = stages
            .iter()
            .filter(|stage| stage.job == job)
            .filter_map(|stage| {
                steps
                    .iter()
                    .position(|step| step.job == job && step.command == stage.command)
                    .map(|at| (stage, at))
            })
            .collect();

        for window in positions.windows(2) {
            let [(earlier, earlier_at), (later, later_at)] = window else {
                continue;
            };
            if earlier_at > later_at {
                violations.push(Violation::new(
                    "ci-pipeline",
                    later.name,
                    format!(
                        "runs before `{}` in job `{job}`; the pipeline order is {}",
                        earlier.name,
                        render_order(stages, job)
                    ),
                ));
            }
        }
    }

    violations
}

/// Rules about the workflow as a whole rather than about one stage.
///
/// Each of these is a way to leave every stage in place and still have the pipeline gate
/// nothing, which is the shape of drift the stage comparison alone cannot see.
fn check_workflow_shape(workflow: &str) -> Vec<Violation> {
    let mut violations = Vec::new();

    // YAML forbids tabs in indentation and GitHub refuses to load the file, so a
    // tab-indented workflow runs no stage at all while satisfying every other rule.
    if workflow
        .lines()
        .any(|line| line.starts_with('\t') || line.starts_with(" \t"))
    {
        violations.push(Violation::new(
            "ci-pipeline",
            WORKFLOW_PATH,
            "indents a line with a tab, which YAML forbids; GitHub would refuse to load the workflow",
        ));
    }

    if let Some(line) = workflow
        .lines()
        .map(str::trim)
        .find(|line| !line.starts_with('#') && line.starts_with("continue-on-error"))
    {
        violations.push(Violation::new(
            "ci-pipeline",
            WORKFLOW_PATH,
            format!(
                "declares `{line}`; a stage whose failure is ignored is not a gate, and no stage in this pipeline is advisory"
            ),
        ));
    }

    let scan = Scan::of(workflow);
    for job in jobs(STAGES) {
        if !scan.jobs_with_a_runner.iter().any(|name| name == job) {
            violations.push(Violation::new(
                "ci-pipeline",
                job,
                "declares no `runs-on:`, so GitHub has nowhere to run it and refuses to load the workflow",
            ));
        }
    }

    for required in REQUIRED_TRIGGERS {
        if !scan.triggers.iter().any(|trigger| trigger == required) {
            violations.push(Violation::new(
                "ci-pipeline",
                WORKFLOW_PATH,
                format!("is not triggered by `{required}`, so the pipeline does not gate anything"),
            ));
        }
    }

    violations
}

/// Every job a table names, in the order it first names them.
fn jobs(stages: &[Stage]) -> Vec<&'static str> {
    let mut jobs: Vec<&'static str> = Vec::new();
    for stage in stages {
        if !jobs.contains(&stage.job) {
            jobs.push(stage.job);
        }
    }
    jobs
}

/// Whether the workflow sets `key: value` somewhere in an `env:` block.
///
/// Scanned rather than parsed, for the same reason [`run_steps`] is: the rule needs one
/// fact out of the file, not a YAML implementation.
fn sets_environment(workflow: &str, key: &str, value: &str) -> bool {
    let wanted = format!("{key}: {value}");
    workflow
        .lines()
        .map(str::trim)
        .any(|line| !line.starts_with('#') && line == wanted)
}

/// The stage names of one job, in pipeline order, for a violation message.
fn render_order(stages: &[Stage], job: &str) -> String {
    stages
        .iter()
        .filter(|stage| stage.job == job)
        .map(|stage| stage.name)
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// The pre-commit hook, rendered from [`STAGES`].
///
/// Generated rather than hand-written so that the hook and the pipeline cannot disagree:
/// [`check_pre_commit_hook`] compares the committed file against this string.
#[must_use]
pub fn render_pre_commit_hook() -> String {
    const PREAMBLE: &str = "\
#!/bin/sh
# Generated by `cargo xtask install-hooks` from the stage table in
# xtask/src/pipeline.rs. Do not edit: `cargo xtask check-layering` fails when this
# file drifts from that table, because a hook that runs something other than CI
# makes CI discovery rather than confirmation.
#
# Skip a single commit with `git commit --no-verify`; the pipeline still runs.
set -eu

";

    let mut hook: Vec<String> = vec![PREAMBLE.to_owned()];
    for stage in hook_stages() {
        hook.push(format!(
            "echo '[pre-commit] {}: {}'\n{}\n\n",
            stage.name, stage.command, stage.command
        ));
    }
    hook.push(format!(
        "echo '[pre-commit] ok ({} stages)'\n",
        hook_stages().count()
    ));
    hook.concat()
}

/// Rule: the committed hook is the rendered hook, and carries the execute bit.
///
/// `is_executable` is `None` where the platform records no execute bit, which the rule
/// reports nothing about rather than guessing.
///
/// What this cannot check is whether the clone has `core.hooksPath` pointing here: that is
/// local git configuration rather than a tracked file, and CI has no reason to set it, so a
/// rule that required it would fail every CI run. `cargo xtask install-hooks` sets it, and
/// the README says to run it once per clone.
#[must_use]
pub fn check_pre_commit_hook(hook: Option<&str>, is_executable: Option<bool>) -> Vec<Violation> {
    let Some(hook) = hook else {
        return vec![Violation::new(
            "pre-commit-hook",
            PRE_COMMIT_PATH,
            "the hook is missing; run `cargo xtask install-hooks` to generate it",
        )];
    };

    let mut violations = Vec::new();
    let expected = render_pre_commit_hook();
    if hook != expected {
        violations.push(Violation::new(
            "pre-commit-hook",
            PRE_COMMIT_PATH,
            format!(
                "has drifted from the pipeline table in xtask/src/pipeline.rs ({}); run `cargo xtask install-hooks` to regenerate it",
                describe_difference(hook, &expected)
            ),
        ));
    }
    if is_executable == Some(false) {
        violations.push(Violation::new(
            "pre-commit-hook",
            PRE_COMMIT_PATH,
            "is not executable, so git skips it silently; `chmod +x` it or run `cargo xtask install-hooks`",
        ));
    }
    violations
}

/// Names the first line at which the committed hook and the rendered hook disagree.
///
/// A rule that says only "this file is wrong" makes the reader diff it by hand, which the
/// rest of the gate does not do: the release-profile rule names the key and both values.
fn describe_difference(committed: &str, expected: &str) -> String {
    for (index, (left, right)) in committed.lines().zip(expected.lines()).enumerate() {
        if left != right {
            return format!(
                "line {} is `{left}`, expected `{right}`",
                index.saturating_add(1)
            );
        }
    }

    let committed_lines = committed.lines().count();
    let expected_lines = expected.lines().count();
    match committed_lines.cmp(&expected_lines) {
        core::cmp::Ordering::Less => {
            format!("it stops after {committed_lines} of {expected_lines} lines")
        }
        core::cmp::Ordering::Greater => {
            format!("it has {committed_lines} lines where {expected_lines} are expected")
        }
        core::cmp::Ordering::Equal => "it differs only in trailing whitespace".to_owned(),
    }
}

/// Rule: the pinned toolchain installs the firmware target.
///
/// Installing it in a CI step instead would leave a local checkout without it, and the
/// firmware build is not an occasional check.
#[must_use]
pub fn check_toolchain(toolchain: Option<&str>) -> Vec<Violation> {
    let Some(toolchain) = toolchain else {
        return vec![Violation::new(
            "toolchain-targets",
            TOOLCHAIN_PATH,
            "the toolchain is not pinned, so nothing installs the firmware target",
        )];
    };

    let Ok(document) = toolchain.parse::<toml::Table>() else {
        return vec![Violation::new(
            "toolchain-targets",
            TOOLCHAIN_PATH,
            "is not valid TOML",
        )];
    };

    let list = |key: &str| -> Vec<String> {
        document
            .get("toolchain")
            .and_then(|section| section.get(key))
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut violations = Vec::new();
    let targets = list("targets");
    if !targets.iter().any(|target| target == FIRMWARE_TARGET) {
        violations.push(Violation::new(
            "toolchain-targets",
            TOOLCHAIN_PATH,
            format!(
                "does not pin `{FIRMWARE_TARGET}` in [toolchain] targets, so a local checkout cannot run the firmware build"
            ),
        ));
    }

    let components = list("components");
    for required in REQUIRED_COMPONENTS {
        if !components.iter().any(|component| component == required) {
            violations.push(Violation::new(
                "toolchain-targets",
                TOOLCHAIN_PATH,
                format!(
                    "does not pin the `{required}` component, so the stage that needs it downloads it mid-pipeline"
                ),
            ));
        }
    }

    violations
}

/// The `core.hooksPath` value `install_pre_commit_hook` points git at.
pub const HOOKS_PATH: &str = ".githooks";

/// Writes the rendered hook into `root` and makes it executable.
///
/// Returns the path written. The caller still has to point git at it; the hook directory
/// is tracked precisely so that the file is reviewable, which `.git/hooks` is not.
///
/// # Errors
///
/// Returns the underlying I/O error if the directory or the file cannot be written.
pub fn install_pre_commit_hook(root: &Path) -> std::io::Result<PathBuf> {
    let path = root.join(PRE_COMMIT_PATH);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, render_pre_commit_hook())?;
    make_executable(&path)?;
    Ok(path)
}

/// Sets the execute bit, where the platform has one.
#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// Sets the execute bit, where the platform has one.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Fixtures shared by this module's tests and the wiring tests in [`crate`].
#[cfg(test)]
pub mod tests_support {
    use super::{FIRMWARE_TARGET, REQUIRED_COMPONENTS, REQUIRED_WORKFLOW_ENV, STAGES};

    /// A workflow that runs every stage, in table order, in the job the table names.
    #[must_use]
    pub fn clean_workflow() -> String {
        let mut jobs: Vec<&str> = Vec::new();
        for stage in STAGES {
            if !jobs.contains(&stage.job) {
                jobs.push(stage.job);
            }
        }

        let mut yaml: Vec<String> = vec![format!(
            "name: CI\n\non:\n  pull_request:\n\nenv:\n{}\njobs:\n",
            REQUIRED_WORKFLOW_ENV
                .iter()
                .map(|(key, value)| format!("  {key}: {value}\n"))
                .collect::<Vec<_>>()
                .concat()
        )];
        for job in jobs {
            yaml.push(format!(
                "  {job}:\n    runs-on: ubuntu-latest\n    steps:\n"
            ));
            for stage in STAGES.iter().filter(|stage| stage.job == job) {
                yaml.push(format!(
                    "      - name: {}\n        run: {}\n",
                    stage.name, stage.command
                ));
            }
        }
        yaml.concat()
    }

    /// A toolchain file that pins the firmware target and every required component.
    #[must_use]
    pub fn clean_toolchain() -> String {
        format!(
            "[toolchain]\nchannel = \"1.97\"\ntargets = [\"{FIRMWARE_TARGET}\"]\ncomponents = {REQUIRED_COMPONENTS:?}\n"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workflow that runs every stage, in table order, in the job the table names.
    fn good_workflow() -> String {
        tests_support::clean_workflow()
    }

    #[test]
    fn every_stage_has_a_unique_name_and_a_non_empty_command() {
        let mut names: Vec<&str> = STAGES.iter().map(|stage| stage.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "stage names must be unique");
        assert!(STAGES.iter().all(|stage| !stage.command.trim().is_empty()));
        assert!(STAGES.iter().all(|stage| !stage.why.trim().is_empty()));
    }

    #[test]
    fn the_pipeline_runs_format_then_lint_then_test_then_coverage() {
        // Issue #9: "Pipeline order: `fmt --check` -> `clippy -D warnings` -> `test` ->
        // coverage."
        let position = |name: &str| {
            STAGES
                .iter()
                .position(|stage| stage.name == name)
                .expect("the stage should be in the table")
        };
        assert!(position("format") < position("lint"));
        assert!(position("lint") < position("test"));
        assert!(position("test") < position("coverage"));
    }

    #[test]
    fn the_hook_runs_format_lint_and_test() {
        let hooked: Vec<&str> = hook_stages().map(|stage| stage.name).collect();
        assert_eq!(hooked, ["format", "lint", "test"]);
    }

    #[test]
    fn the_pipeline_builds_the_firmware_target_with_no_default_features() {
        let firmware = STAGES
            .iter()
            .find(|stage| stage.name == "firmware")
            .expect("the firmware build is a pipeline stage");
        assert!(firmware.command.contains(FIRMWARE_TARGET));
        assert!(firmware.command.contains("--no-default-features"));
    }

    #[test]
    fn a_workflow_that_runs_every_stage_in_order_passes() {
        let violations = check_workflow(Some(&good_workflow()));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_missing_workflow_is_reported() {
        let violations = check_workflow(None);
        assert!(violations.iter().any(|v| v.rule == "ci-pipeline"));
    }

    #[test]
    fn a_workflow_that_drops_the_firmware_build_is_rejected() {
        let firmware = STAGES
            .iter()
            .find(|stage| stage.name == "firmware")
            .expect("the firmware build is a pipeline stage");
        let without = good_workflow().replace(firmware.command, "cargo build");
        let violations = check_workflow(Some(&without));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "ci-pipeline" && v.subject == "firmware"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_workflow_that_drops_no_default_features_from_a_stage_is_rejected() {
        let mangled = good_workflow().replace("--no-default-features", "");
        assert!(!check_workflow(Some(&mangled)).is_empty());
    }

    #[test]
    fn a_workflow_that_runs_the_tests_before_the_lint_is_rejected() {
        let lint = STAGES
            .iter()
            .find(|stage| stage.name == "lint")
            .expect("lint is a pipeline stage");
        let test = STAGES
            .iter()
            .find(|stage| stage.name == "test")
            .expect("test is a pipeline stage");
        let swapped = good_workflow()
            .replace(lint.command, "@@LINT@@")
            .replace(test.command, lint.command)
            .replace("@@LINT@@", test.command);

        let violations = check_workflow(Some(&swapped));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "ci-pipeline" && v.detail.contains("order")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_stage_run_in_the_wrong_job_is_rejected() {
        let workflow = good_workflow().replace("  layering:", "  unrelated:");
        let violations = check_workflow(Some(&workflow));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "ci-pipeline" && v.detail.contains("job")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_commented_out_step_does_not_count_as_running_the_stage() {
        let commented = good_workflow().replace("        run: cargo fmt", "     #  run: cargo fmt");
        let violations = check_workflow(Some(&commented));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "ci-pipeline" && v.subject == "format"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_stage_guarded_by_a_step_condition_is_rejected() {
        // `if: false` leaves the command in the file and stops it running, which is the
        // cheapest way to disable a stage without a reviewer noticing.
        let workflow = good_workflow().replace(
            "        run: cargo fmt --all --check",
            "        if: false\n        run: cargo fmt --all --check",
        );
        let violations = check_workflow(Some(&workflow));
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "format" && v.detail.contains("if:")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_stage_guarded_by_a_job_condition_is_rejected() {
        let workflow = good_workflow().replace(
            "  check:\n    runs-on: ubuntu-latest",
            "  check:\n    if: false\n    runs-on: ubuntu-latest",
        );
        let violations = check_workflow(Some(&workflow));
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "format" && v.detail.contains("if:")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_stage_whose_failure_is_ignored_is_rejected() {
        let workflow = good_workflow().replace(
            "        run: cargo fmt --all --check",
            "        continue-on-error: true\n        run: cargo fmt --all --check",
        );
        let violations = check_workflow(Some(&workflow));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("continue-on-error")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_workflow_no_pull_request_triggers_is_rejected() {
        let violations = check_workflow(Some(
            &good_workflow().replace("on:\n  pull_request:\n", "on:\n  workflow_dispatch:\n"),
        ));
        assert!(
            violations.iter().any(|v| v.detail.contains("pull_request")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_inline_trigger_list_counts() {
        let workflow =
            good_workflow().replace("on:\n  pull_request:\n", "on: [push, pull_request]\n");
        assert!(
            !check_workflow(Some(&workflow))
                .iter()
                .any(|v| v.detail.contains("pull_request")),
            "an inline `on: [..]` list must be read"
        );
    }

    #[test]
    fn a_workflow_that_does_not_set_the_required_environment_is_rejected() {
        // `cargo doc` exits zero on a broken intra-doc link; RUSTDOCFLAGS is what makes
        // the docs stage mean anything, and it lives outside the command.
        let violations = check_workflow(Some(&good_workflow().replace("-D warnings\n", "\n")));
        assert!(
            violations.iter().any(|v| v.subject == "RUSTDOCFLAGS"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_tab_indented_workflow_is_rejected() {
        // GitHub refuses to load it, so every stage is "present" and none of them runs.
        let workflow = good_workflow().replace("      - name: format", "\t- name: format");
        assert!(
            check_workflow(Some(&workflow))
                .iter()
                .any(|v| v.detail.contains("tab")),
        );
    }

    #[test]
    fn a_job_without_a_runner_is_rejected() {
        let workflow = good_workflow().replace("    runs-on: ubuntu-latest\n", "");
        assert!(
            check_workflow(Some(&workflow))
                .iter()
                .any(|v| v.detail.contains("runs-on")),
        );
    }

    #[test]
    fn a_stage_hidden_in_a_shell_block_does_not_count() {
        // The command is in the file, and the shell never reaches it. A gate that matched
        // it would be checking for text rather than for a stage.
        let workflow = good_workflow().replace(
            "        run: cargo fmt --all --check",
            "        run: |\n          if [ \"$SKIP\" = \"0\" ]; then\n          cargo fmt --all --check\n          fi",
        );
        let violations = check_workflow(Some(&workflow));
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "format" && v.detail.contains("does not run")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_run_key_nested_under_an_action_input_is_not_a_command() {
        let workflow = "\
name: CI
on:
  pull_request:
jobs:
  check:
    steps:
      - uses: some/action@v1
        with:
          run: cargo fmt --all --check
";
        assert!(
            run_steps(workflow).is_empty(),
            "an action input named `run` is not a command: {:?}",
            run_steps(workflow)
        );
    }

    #[test]
    fn a_folded_scalar_does_not_satisfy_a_stage() {
        // YAML folds `>` into one command, so the lines below it are not the commands they
        // look like. The scanner credits nothing rather than crediting the first line and
        // ignoring an argument appended on the second.
        let workflow = "\
name: CI
jobs:
  check:
    steps:
      - run: >
          cargo fmt --all --check
          --a-flag-nobody-sees
";
        assert!(run_steps(workflow).is_empty(), "{:?}", run_steps(workflow));
    }

    #[test]
    fn a_quoted_command_with_a_trailing_comment_still_counts() {
        let workflow = "\
name: CI
jobs:
  check:
    steps:
      - name: Format
        run: \"cargo fmt --all --check\"
      - name: Lint
        run: cargo clippy # keep this honest
";
        let commands: Vec<String> = run_steps(workflow)
            .into_iter()
            .map(|step| step.command)
            .collect();
        assert_eq!(commands, ["cargo fmt --all --check", "cargo clippy"]);
    }

    #[test]
    fn an_anchored_or_quoted_job_key_is_still_the_job() {
        let workflow = "\
name: CI
jobs:
  \"check\": &main   # the job everything else waits on
    steps:
      - run: cargo fmt --all --check
";
        let steps = run_steps(workflow);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps.first().map(|step| step.job.as_str()), Some("check"));
    }

    #[test]
    fn jobs_indented_by_four_spaces_are_still_jobs() {
        // The job indent is taken from the file rather than assumed to be two spaces.
        let workflow = "\
name: CI
jobs:
    check:
        steps:
            - run: cargo fmt --all --check
";
        let steps = run_steps(workflow);
        assert_eq!(steps.first().map(|step| step.job.as_str()), Some("check"));
    }

    #[test]
    fn a_top_level_key_whose_children_look_like_jobs_is_not_jobs() {
        let workflow = "\
name: CI
on:
  check:
    branches: [main]
jobs:
  firmware:
    steps:
      - run: cargo fmt --all --check
";
        let steps = run_steps(workflow);
        assert_eq!(
            steps.first().map(|step| step.job.as_str()),
            Some("firmware")
        );
    }

    #[test]
    fn a_block_scalar_does_not_swallow_the_step_that_follows_it() {
        // The block ends at the dedent, and the next step is read normally.
        let workflow = "\
name: CI
jobs:
  check:
    steps:
      - run: |
          cargo test
          run: not a step
      - run: cargo fmt --all --check
";
        let commands: Vec<String> = run_steps(workflow)
            .into_iter()
            .map(|step| step.command)
            .collect();
        assert_eq!(commands, ["cargo fmt --all --check"]);
    }

    #[test]
    fn the_order_rule_holds_when_a_job_s_stages_are_not_contiguous_in_the_table() {
        // The table happens to group each job's rows together today. The order check must
        // not depend on that, because nothing stops someone regrouping the table.
        let stages = [
            Stage {
                name: "first",
                job: "check",
                command: "cargo one",
                in_hook: false,
                why: "test fixture",
            },
            Stage {
                name: "interloper",
                job: "other",
                command: "cargo two",
                in_hook: false,
                why: "test fixture",
            },
            Stage {
                name: "second",
                job: "check",
                command: "cargo three",
                in_hook: false,
                why: "test fixture",
            },
        ];
        let workflow = "\
name: CI
jobs:
  check:
    steps:
      - run: cargo three
      - run: cargo one
  other:
    steps:
      - run: cargo two
";
        let violations = check_stage_order(&stages, &run_steps(workflow));
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "second" && v.detail.contains("order")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_block_scalar_credits_no_command() {
        let workflow = "\
name: CI
jobs:
  check:
    steps:
      - name: Format
        run: |
          cargo fmt --all --check
";
        assert!(run_steps(workflow).is_empty(), "{:?}", run_steps(workflow));
    }

    #[test]
    fn run_steps_are_attributed_to_the_job_that_contains_them() {
        let steps = run_steps(&good_workflow());
        for stage in STAGES {
            assert!(
                steps
                    .iter()
                    .any(|step| step.job == stage.job && step.command == stage.command),
                "{} was not attributed to job {}",
                stage.name,
                stage.job
            );
        }
    }

    #[test]
    fn the_rendered_hook_runs_every_hook_stage_and_stops_at_the_first_failure() {
        let hook = render_pre_commit_hook();
        assert!(hook.starts_with("#!/bin/sh\n"));
        assert!(hook.contains("set -eu"), "{hook}");
        for stage in hook_stages() {
            assert!(hook.contains(stage.command), "{} is missing", stage.name);
        }
        for stage in STAGES.iter().filter(|stage| !stage.in_hook) {
            assert!(
                !hook.contains(stage.command),
                "{} does not belong in the hook",
                stage.name
            );
        }
    }

    #[test]
    fn the_rendered_hook_satisfies_its_own_rule() {
        let hook = render_pre_commit_hook();
        let violations = check_pre_commit_hook(Some(&hook), Some(true));
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_hook_that_drifts_from_the_pipeline_is_rejected() {
        let drifted = render_pre_commit_hook().replace("--locked", "");
        let violations = check_pre_commit_hook(Some(&drifted), Some(true));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "pre-commit-hook" && v.detail.contains("install-hooks")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_missing_hook_is_reported() {
        let violations = check_pre_commit_hook(None, None);
        assert!(violations.iter().any(|v| v.rule == "pre-commit-hook"));
    }

    #[test]
    fn a_hook_that_is_not_executable_is_rejected() {
        // A pre-commit hook without the execute bit is silently skipped by git, which is
        // the quietest way for the mirror to stop existing.
        let hook = render_pre_commit_hook();
        let violations = check_pre_commit_hook(Some(&hook), Some(false));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "pre-commit-hook" && v.detail.contains("executable")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_unknown_execute_bit_is_not_held_against_the_hook() {
        // Windows checkouts have no execute bit to read. The rule reports what it can see.
        let hook = render_pre_commit_hook();
        assert!(check_pre_commit_hook(Some(&hook), None).is_empty());
    }

    #[test]
    fn a_toolchain_that_pins_the_firmware_target_passes() {
        assert!(check_toolchain(Some(&tests_support::clean_toolchain())).is_empty());
    }

    #[test]
    fn a_toolchain_without_the_coverage_component_is_rejected() {
        let toolchain = format!("[toolchain]\ntargets = [\"{FIRMWARE_TARGET}\"]\n");
        let violations = check_toolchain(Some(&toolchain));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("llvm-tools-preview")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_toolchain_without_the_firmware_target_is_rejected() {
        let toolchain = "[toolchain]\nchannel = \"1.97\"\n";
        let violations = check_toolchain(Some(toolchain));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "toolchain-targets" && v.detail.contains(FIRMWARE_TARGET)),
            "{violations:?}"
        );
    }

    #[test]
    fn a_toolchain_that_pins_a_different_target_is_rejected() {
        let toolchain = "[toolchain]\nchannel = \"1.97\"\ntargets = [\"thumbv7em-none-eabihf\"]\n";
        assert!(!check_toolchain(Some(toolchain)).is_empty());
    }

    #[test]
    fn a_toolchain_that_is_not_valid_toml_is_reported() {
        let violations = check_toolchain(Some("[toolchain"));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "toolchain-targets" && v.detail.contains("TOML")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_missing_toolchain_file_is_reported() {
        assert!(!check_toolchain(None).is_empty());
    }

    #[test]
    fn the_hook_rule_names_the_line_that_drifted() {
        let drifted = render_pre_commit_hook().replace("cargo fmt --all --check", "cargo fmt");
        let violations = check_pre_commit_hook(Some(&drifted), Some(true));
        let detail = violations
            .first()
            .map(|violation| violation.detail.clone())
            .unwrap_or_default();
        assert!(detail.contains("line "), "{detail}");
        assert!(detail.contains("cargo fmt --all --check"), "{detail}");
    }

    #[test]
    fn a_truncated_hook_is_described_as_truncated() {
        let hook = render_pre_commit_hook();
        let truncated: String = hook
            .lines()
            .take(3)
            .map(|line| format!("{line}\n"))
            .collect::<Vec<_>>()
            .concat();
        let violations = check_pre_commit_hook(Some(&truncated), Some(true));
        let detail = violations
            .first()
            .map(|violation| violation.detail.clone())
            .unwrap_or_default();
        assert!(detail.contains("stops after"), "{detail}");
    }

    #[test]
    fn a_hook_with_extra_lines_is_described_as_too_long() {
        let hook = format!("{}echo 'and one more thing'\n", render_pre_commit_hook());
        let violations = check_pre_commit_hook(Some(&hook), Some(true));
        let detail = violations
            .first()
            .map(|violation| violation.detail.clone())
            .unwrap_or_default();
        assert!(detail.contains("lines where"), "{detail}");
    }
}
