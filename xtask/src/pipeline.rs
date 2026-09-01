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
}

/// Every command the workflow runs, in file order.
///
/// This is a scanner rather than a YAML parser on purpose: `xtask` has two dependencies
/// and the rule needs one thing from the file, which is which commands run in which job
/// and in what order. Both `run: cmd` and a `run: |` block are recognised; a commented
/// line is not a command.
#[must_use]
pub fn run_steps(workflow: &str) -> Vec<RunStep> {
    let mut steps = Vec::new();
    let mut job = String::new();
    let mut in_jobs = false;
    let mut block: Option<usize> = None;

    for line in workflow.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();

        if let Some(block_indent) = block {
            if trimmed.is_empty() {
                continue;
            }
            if indent > block_indent {
                steps.push(RunStep {
                    job: job.clone(),
                    command: trimmed.to_owned(),
                });
                continue;
            }
            block = None;
        }

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if indent == 0 {
            in_jobs = trimmed == "jobs:";
            job.clear();
            continue;
        }

        if in_jobs && indent == JOB_INDENT {
            if let Some(name) = trimmed.strip_suffix(':') {
                name.trim().clone_into(&mut job);
            }
            continue;
        }

        let Some(value) = run_value(trimmed) else {
            continue;
        };
        if is_block_scalar(value) {
            block = Some(indent);
        } else if !value.is_empty() {
            steps.push(RunStep {
                job: job.clone(),
                command: value.to_owned(),
            });
        }
    }

    steps
}

/// The indentation GitHub Actions job keys sit at: two spaces under `jobs:`.
const JOB_INDENT: usize = 2;

/// The value of a `run:` key, whether it is the whole line or a `- run:` step.
fn run_value(trimmed: &str) -> Option<&str> {
    let without_dash = trimmed.strip_prefix("- ").unwrap_or(trimmed);
    without_dash.strip_prefix("run:").map(str::trim)
}

/// Whether a `run:` value opens a block scalar rather than holding the command itself.
fn is_block_scalar(value: &str) -> bool {
    value.starts_with('|') || value.starts_with('>')
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
    // (stage index, index of the step that runs it) per job, to check order.
    let mut positions: Vec<(&'static Stage, usize)> = Vec::new();

    for stage in STAGES {
        let matching: Vec<(usize, &RunStep)> = steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step.command == stage.command)
            .collect();

        let Some((step_index, _)) = matching.iter().find(|(_, step)| step.job == stage.job) else {
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

        positions.push((stage, *step_index));
    }

    for window in positions.windows(2) {
        let [(earlier, earlier_at), (later, later_at)] = window else {
            continue;
        };
        if earlier.job == later.job && earlier_at > later_at {
            violations.push(Violation::new(
                "ci-pipeline",
                later.name,
                format!(
                    "runs before `{}` in job `{}`; the pipeline order is {}",
                    earlier.name,
                    later.job,
                    render_order(later.job)
                ),
            ));
        }
    }

    violations
}

/// The stage names of one job, in pipeline order, for a violation message.
fn render_order(job: &str) -> String {
    STAGES
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

/// Rule: the committed hook is the rendered hook, and git will actually run it.
///
/// `is_executable` is `None` where the platform records no execute bit, which the rule
/// reports nothing about rather than guessing.
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
    if hook != render_pre_commit_hook() {
        violations.push(Violation::new(
            "pre-commit-hook",
            PRE_COMMIT_PATH,
            "has drifted from the pipeline table in xtask/src/pipeline.rs; run `cargo xtask install-hooks` to regenerate it",
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

    let targets: Vec<&str> = document
        .get("toolchain")
        .and_then(|section| section.get("targets"))
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    if targets.contains(&FIRMWARE_TARGET) {
        Vec::new()
    } else {
        vec![Violation::new(
            "toolchain-targets",
            TOOLCHAIN_PATH,
            format!(
                "does not pin `{FIRMWARE_TARGET}` in [toolchain] targets, so a local checkout cannot run the firmware build"
            ),
        )]
    }
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
    use super::{FIRMWARE_TARGET, STAGES};

    /// A workflow that runs every stage, in table order, in the job the table names.
    #[must_use]
    pub fn clean_workflow() -> String {
        let mut jobs: Vec<&str> = Vec::new();
        for stage in STAGES {
            if !jobs.contains(&stage.job) {
                jobs.push(stage.job);
            }
        }

        let mut yaml: Vec<String> = vec!["name: CI\n\njobs:\n".to_owned()];
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

    /// A toolchain file that pins the firmware target.
    #[must_use]
    pub fn clean_toolchain() -> String {
        format!("[toolchain]\nchannel = \"1.97\"\ntargets = [\"{FIRMWARE_TARGET}\"]\n")
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
    fn a_block_scalar_run_step_counts() {
        let workflow = "\
name: CI
jobs:
  check:
    steps:
      - name: Format
        run: |
          cargo fmt --all --check
";
        let steps = run_steps(workflow);
        assert!(
            steps
                .iter()
                .any(|step| step.job == "check" && step.command == "cargo fmt --all --check"),
            "{steps:?}"
        );
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
        let toolchain = format!(
            "[toolchain]\nchannel = \"1.97\"\ntargets = [\"{FIRMWARE_TARGET}\"]\n\
             components = [\"rustfmt\", \"clippy\"]\n"
        );
        assert!(check_toolchain(Some(&toolchain)).is_empty());
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
    fn the_components_the_pipeline_needs_are_documented_by_the_stage_table() {
        // The coverage stage shells out to a tool that is not part of a rustup profile.
        let coverage = STAGES
            .iter()
            .find(|stage| stage.name == "coverage")
            .expect("coverage is a pipeline stage");
        assert_eq!(coverage.command, "cargo --locked xtask coverage");
    }
}
