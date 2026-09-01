//! The gate, run against the real workspace.
//!
//! The unit tests inside `xtask` prove each rule fires on a broken workspace built out of
//! synthetic input. These tests prove the same thing end to end: the workspace we ship
//! passes, a copy of it with one forbidden line added does not, and the binary a
//! developer and CI actually run reports it and exits non-zero.

// `clippy.toml` exempts `#[test]` bodies from `expect_used`, but not the free helper
// functions an integration-test crate needs. Every function in this file is test code.
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace root, which is this crate's parent directory.
///
/// `allow-expect-in-tests` covers `#[test]` bodies, not free functions in an integration
/// test crate, so this resolves the parent without panicking.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn render(violations: &[xtask::Violation]) -> String {
    violations
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_workspace_satisfies_its_own_layering_policy() {
    let violations =
        xtask::check_workspace(&workspace_root()).expect("the policy check should be runnable");

    assert!(
        violations.is_empty(),
        "workspace policy violations:\n{}",
        render(&violations)
    );
}

#[test]
fn the_three_firmware_crates_exist_and_are_layered() {
    let inputs =
        xtask::collect_inputs(&workspace_root()).expect("the workspace inputs should be readable");
    let graph = xtask::graph::PackageGraph::from_cargo_metadata(&inputs.metadata_json)
        .expect("cargo metadata should parse");

    for spec in xtask::policy::LAYERS {
        assert!(
            graph.find(spec.name).is_some(),
            "{} is missing from the workspace",
            spec.name
        );
    }

    assert!(
        graph.transitive_dependencies("waymaker-core").is_empty(),
        "the kernel must be dependency-free"
    );
    assert_eq!(
        graph.transitive_dependencies("waymaker-flash"),
        core::iter::once("waymaker-core".to_owned()).collect(),
        "waymaker-flash may only reach waymaker-core"
    );
    assert_eq!(
        graph.transitive_dependencies("waymaker-embassy"),
        ["waymaker-core".to_owned(), "waymaker-flash".to_owned()]
            .into_iter()
            .collect(),
        "waymaker-embassy may only reach the two crates below it"
    );
}

#[test]
fn every_firmware_crate_is_no_std_and_forbids_unsafe() {
    let inputs =
        xtask::collect_inputs(&workspace_root()).expect("the workspace inputs should be readable");

    for spec in xtask::policy::LAYERS {
        let (_, source) = inputs
            .crate_sources
            .iter()
            .find(|(name, _)| name == spec.name)
            .expect("every layer contributes a crate root");
        assert!(source.contains("#![no_std]"), "{} is not no_std", spec.name);
        assert!(
            source.contains("#![forbid(unsafe_code)]"),
            "{} does not forbid unsafe code",
            spec.name
        );
    }
}

/// The binary a developer and CI run, exercised end to end.
///
/// `CARGO_BIN_EXE_xtask` is set by cargo for integration tests of a package with a
/// binary target, so this runs the real binary rather than re-implementing its wiring.
fn run_xtask(args: &[&str], current_dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(args)
        .current_dir(current_dir)
        .output()
        .expect("the xtask binary should be runnable")
}

#[test]
fn the_binary_reports_success_on_the_real_workspace() {
    let output = run_xtask(&["check-layering"], &workspace_root());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("workspace policy: ok"), "stdout: {stdout}");
    assert!(
        stdout.contains(&xtask::RULES.len().to_string()),
        "the success line should count the declared rules: {stdout}"
    );
}

#[test]
fn the_binary_rejects_an_unknown_command() {
    let output = run_xtask(&["demolish-layering"], &workspace_root());
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown command"), "stderr: {stderr}");
}

#[test]
fn the_binary_reports_a_check_error_and_exits_non_zero() {
    let scratch = scratch_workspace("stale-binary");
    add_path_crate(&scratch.root, "stowaway");
    let manifest = scratch.root.join("crates/waymaker-core/Cargo.toml");
    let existing = std::fs::read_to_string(&manifest).expect("the kernel manifest should be read");
    std::fs::write(
        &manifest,
        format!("{existing}\n[dependencies]\nstowaway = {{ path = \"../stowaway\" }}\n"),
    )
    .expect("the kernel manifest should be writable");
    // No lockfile refresh: the gate must stop rather than re-resolve.

    let output = run_xtask(&["check-layering"], &scratch.root);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("xtask: "), "stderr: {stderr}");
}

#[test]
fn the_binary_prints_usage_for_help() {
    let output = run_xtask(&["--help"], &workspace_root());
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("check-layering"));
}

/// Copies the workspace into `destination`, skipping `target` and `.git`.
fn copy_workspace(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let to = destination.join(&name);
        if entry.file_type()?.is_dir() {
            copy_workspace(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

/// A scratch copy of the workspace that deletes itself when the test ends.
struct ScratchWorkspace {
    root: PathBuf,
}

impl Drop for ScratchWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scratch_workspace(label: &str) -> ScratchWorkspace {
    let root = std::env::temp_dir().join(format!(
        "waymaker-policy-{label}-{}-{}",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_dir_all(&root);
    copy_workspace(&workspace_root(), &root).expect("the workspace should be copyable");
    ScratchWorkspace { root }
}

/// Adds a new path crate to a scratch workspace and refreshes its lockfile offline.
///
/// A path dependency keeps the whole fixture free of the network and of the registry
/// cache, so this test behaves the same on a developer's machine and on a cold CI runner.
fn add_path_crate(root: &Path, name: &str) {
    let crate_dir = root.join("crates").join(name);
    std::fs::create_dir_all(crate_dir.join("src"))
        .expect("the crate directory should be creatable");
    std::fs::write(
        crate_dir.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n"),
    )
    .expect("the manifest should be writable");
    std::fs::write(crate_dir.join("src/lib.rs"), "//! Scratch.\n")
        .expect("the crate root should be writable");

    let manifest = root.join("Cargo.toml");
    let existing = std::fs::read_to_string(&manifest).expect("the manifest should be read");
    std::fs::write(
        &manifest,
        existing.replace(
            "members = [\n    \"crates/waymaker-core\",",
            &format!("members = [\n    \"crates/{name}\",\n    \"crates/waymaker-core\","),
        ),
    )
    .expect("the manifest should be writable");

    refresh_lockfile(root);
}

/// Brings `Cargo.lock` back in step with the manifests without touching the network.
///
/// The gate runs `cargo metadata --locked`, which fails closed on a stale lockfile. A
/// contributor would have regenerated the lock before pushing; this does the same.
fn refresh_lockfile(root: &Path) {
    let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--offline"])
        .current_dir(root)
        .output()
        .expect("cargo should be runnable");
    assert!(
        output.status.success(),
        "could not refresh the scratch lockfile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_dependency_added_to_the_kernel_is_rejected() {
    // This is issue #8's acceptance criterion, run against a real workspace on disk
    // rather than against a synthetic graph.
    let scratch = scratch_workspace("kernel-dep");
    add_path_crate(&scratch.root, "stowaway");

    let manifest = scratch.root.join("crates/waymaker-core/Cargo.toml");
    let existing = std::fs::read_to_string(&manifest).expect("the kernel manifest should be read");
    std::fs::write(
        &manifest,
        format!("{existing}\n[dependencies]\nstowaway = {{ path = \"../stowaway\" }}\n"),
    )
    .expect("the kernel manifest should be writable");
    refresh_lockfile(&scratch.root);

    let violations =
        xtask::check_workspace(&scratch.root).expect("the policy check should be runnable");

    assert!(
        violations.iter().any(|violation| {
            violation.rule == "kernel-zero-dependencies" && violation.subject == "waymaker-core"
        }),
        "a dependency on the kernel must be rejected:\n{}",
        render(&violations)
    );
    assert!(
        violations.iter().any(|violation| {
            violation.rule == "workspace-membership" && violation.subject == "stowaway"
        }),
        "a workspace member that is not a layer must be rejected:\n{}",
        render(&violations)
    );
}

#[test]
fn a_stale_lockfile_fails_the_gate_closed() {
    // The gate reads the committed resolution. A manifest change that was never locked
    // must stop the gate, not be silently re-resolved from the network.
    let scratch = scratch_workspace("stale-lock");
    add_path_crate(&scratch.root, "stowaway");
    let manifest = scratch.root.join("crates/waymaker-core/Cargo.toml");
    let existing = std::fs::read_to_string(&manifest).expect("the kernel manifest should be read");
    std::fs::write(
        &manifest,
        format!("{existing}\n[dependencies]\nstowaway = {{ path = \"../stowaway\" }}\n"),
    )
    .expect("the kernel manifest should be writable");
    // Deliberately no refresh_lockfile here.

    let error = xtask::check_workspace(&scratch.root)
        .expect_err("a stale lockfile must stop the gate rather than be re-resolved");
    assert!(error.to_string().contains("--locked"), "{error}");
}

#[test]
fn a_crate_that_stops_being_no_std_is_rejected() {
    let scratch = scratch_workspace("no-std");
    let root = scratch.root.join("crates/waymaker-flash/src/lib.rs");
    let existing = std::fs::read_to_string(&root).expect("the crate root should be read");
    std::fs::write(&root, existing.replace("#![no_std]\n", ""))
        .expect("the crate root should be writable");

    let violations =
        xtask::check_workspace(&scratch.root).expect("the policy check should be runnable");

    assert!(
        violations.iter().any(|violation| {
            violation.rule == "crate-attributes"
                && violation.subject == "waymaker-flash"
                && violation.detail.contains("no_std")
        }),
        "dropping #![no_std] must be rejected:\n{}",
        render(&violations)
    );
}

#[test]
fn a_drifted_release_profile_is_rejected() {
    let scratch = scratch_workspace("profile");
    let manifest = scratch.root.join("Cargo.toml");
    let existing = std::fs::read_to_string(&manifest).expect("the manifest should be read");
    std::fs::write(
        &manifest,
        existing.replace("opt-level = \"z\"", "opt-level = 3"),
    )
    .expect("the manifest should be writable");

    let violations =
        xtask::check_workspace(&scratch.root).expect("the policy check should be runnable");

    assert!(
        violations
            .iter()
            .any(|violation| violation.rule == "release-profile"),
        "a drifted release profile must be rejected:\n{}",
        render(&violations)
    );
}

#[test]
fn the_binary_exits_non_zero_on_a_broken_workspace() {
    let scratch = scratch_workspace("binary");
    let root = scratch.root.join("crates/waymaker-core/src/lib.rs");
    let existing = std::fs::read_to_string(&root).expect("the crate root should be read");
    std::fs::write(&root, format!("{existing}\nextern crate std;\n"))
        .expect("the crate root should be writable");

    let output = run_xtask(&["check-layering"], &scratch.root);

    assert!(
        !output.status.success(),
        "the gate must fail the build; stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("crate-attributes"), "stderr: {stderr}");
    assert!(stderr.contains("extern crate std"), "stderr: {stderr}");
    assert!(
        stderr.contains("design document"),
        "the report should point at the contract: {stderr}"
    );
}

// ---------------------------------------------------------------------------------------
// Issue #9: the pipeline, the firmware target, and the coverage gate.
//
// The tests above prove the layering contract. These prove the three things issue #9 says
// the change is done when: a change that breaks the `thumbv6m-none-eabi` build fails, a
// change that drops coverage below the gate fails, and the hook and the pipeline run the
// same commands.
// ---------------------------------------------------------------------------------------

/// Runs a pipeline stage's command with `cargo` in `directory`.
///
/// The command comes from the stage table rather than being retyped, so a test cannot
/// prove something about a command CI does not run.
fn run_stage(stage: &xtask::pipeline::Stage, directory: &Path) -> std::process::Output {
    let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    let arguments: Vec<&str> = stage
        .command
        .split_whitespace()
        .skip(1) // the leading `cargo`
        .collect();
    Command::new(cargo)
        .args(&arguments)
        .current_dir(directory)
        .output()
        .expect("cargo should be runnable")
}

fn stage(name: &str) -> &'static xtask::pipeline::Stage {
    xtask::pipeline::STAGES
        .iter()
        .find(|stage| stage.name == name)
        .expect("the stage should be in the pipeline table")
}

#[test]
fn the_firmware_crates_build_for_the_firmware_target() {
    let output = run_stage(stage("firmware"), &workspace_root());
    assert!(
        output.status.success(),
        "the firmware build must pass on the workspace we ship: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_change_that_only_works_on_the_host_fails_the_firmware_build() {
    // Issue #9: "A PR that breaks the `thumbv6m` build fails CI."
    //
    // `AtomicU64` exists in `core` on x86-64 and does not exist on `thumbv6m-none-eabi`,
    // which has neither 64-bit atomics nor atomic compare-and-swap. It is therefore a
    // change that a host build accepts and only the firmware build rejects — which is the
    // whole reason the firmware build is in the pipeline.
    let scratch = scratch_workspace("firmware-break");
    let root = scratch.root.join("crates/waymaker-core/src/lib.rs");
    let existing = std::fs::read_to_string(&root).expect("the crate root should be read");
    std::fs::write(
        &root,
        format!(
            "{existing}\n/// A counter the firmware target cannot provide.\n\
             #[must_use]\npub const fn counter() -> core::sync::atomic::AtomicU64 {{\n    \
             core::sync::atomic::AtomicU64::new(0)\n}}\n"
        ),
    )
    .expect("the crate root should be writable");

    let host = run_stage(stage("build"), &scratch.root);
    assert!(
        host.status.success(),
        "the host build must accept it, or the test proves nothing about the target: {}",
        String::from_utf8_lossy(&host.stderr)
    );

    let firmware = run_stage(stage("firmware"), &scratch.root);
    assert!(
        !firmware.status.success(),
        "the firmware build must reject what the host build accepted"
    );
    let stderr = String::from_utf8_lossy(&firmware.stderr);
    assert!(stderr.contains("AtomicU64"), "stderr: {stderr}");
}

/// An llvm-cov export putting each named crate at `covered` of 100 lines.
fn coverage_export(crates: &[(&str, u64)], root: &Path) -> String {
    let files: Vec<String> = crates
        .iter()
        .map(|(name, covered)| {
            let path = root.join("crates").join(name).join("src/lib.rs");
            format!(
                r#"{{"filename":"{}","summary":{{"lines":{{"count":100,"covered":{covered}}}}}}}"#,
                path.display()
            )
        })
        .collect();
    format!(r#"{{"data":[{{"files":[{}]}}]}}"#, files.join(","))
}

/// Writes `report` to a path of its own and gates it with the real binary.
///
/// The filename carries the caller's label because these tests run concurrently in one
/// process and share a workspace root; a single fixture path would let one test read
/// another's report.
fn run_coverage(label: &str, report: &str, root: &Path) -> std::process::Output {
    let path = root
        .join("target")
        .join(format!("test-coverage-{label}.json"));
    std::fs::create_dir_all(root.join("target")).expect("the target directory should be creatable");
    std::fs::write(&path, report).expect("the report should be writable");
    run_xtask(
        &[
            "coverage",
            "--report",
            path.to_str().expect("the path should be UTF-8"),
        ],
        root,
    )
}

#[test]
fn a_crate_below_the_gate_fails_the_coverage_command() {
    // Issue #9: "A PR that drops coverage below the gate fails CI." The workspace total
    // in this report is 68%, but it is `waymaker-core` at 4% that has to be named.
    let root = workspace_root();
    let report = coverage_export(
        &[
            ("waymaker-core", 4),
            ("waymaker-flash", 100),
            ("waymaker-embassy", 100),
        ],
        &root,
    );

    let output = run_coverage("below-gate", &report, &root);

    assert!(!output.status.success(), "the gate must fail the build");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("waymaker-core"), "stderr: {stderr}");
    assert!(stderr.contains("4.00%"), "stderr: {stderr}");
    assert!(stderr.contains("85.00%"), "stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("waymaker-flash"),
        "every crate gets a row: {stdout}"
    );
}

#[test]
fn a_report_over_the_gate_passes_the_coverage_command() {
    let root = workspace_root();
    let report = coverage_export(&[("waymaker-core", 85)], &root);

    let output = run_coverage("over-gate", &report, &root);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("coverage: ok"), "stdout: {stdout}");
    assert!(stdout.contains("85.00%"), "stdout: {stdout}");
}

#[test]
fn an_unreadable_coverage_report_fails_the_command() {
    // A coverage run that did not happen is not a coverage run that passed.
    let output = run_coverage("unreadable", "not json", &workspace_root());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("coverage report"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_missing_coverage_report_fails_the_command() {
    let output = run_xtask(
        &["coverage", "--report", "no/such/report.json"],
        &workspace_root(),
    );
    assert!(!output.status.success());
}

#[test]
fn the_coverage_command_rejects_an_unknown_argument() {
    let output = run_xtask(&["coverage", "--lenient"], &workspace_root());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown argument"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_coverage_command_rejects_a_report_flag_with_no_path() {
    let output = run_xtask(&["coverage", "--report"], &workspace_root());
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("needs a path"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_hook_and_the_pipeline_run_the_same_commands() {
    // Issue #9: "The hook and the pipeline run the same commands." Not by convention: the
    // hook is rendered from the same table this reads, and the gate rejects a workflow or
    // a hook that has drifted from it.
    let hook = std::fs::read_to_string(workspace_root().join(xtask::pipeline::PRE_COMMIT_PATH))
        .expect("the hook should be committed");
    let workflow = std::fs::read_to_string(workspace_root().join(xtask::pipeline::WORKFLOW_PATH))
        .expect("the workflow should be committed");
    let steps = xtask::pipeline::run_steps(&workflow);

    for stage in xtask::pipeline::hook_stages() {
        assert!(
            hook.contains(stage.command),
            "the hook does not run {}",
            stage.name
        );
        assert!(
            steps.iter().any(|step| step.command == stage.command),
            "the workflow does not run {}",
            stage.name
        );
    }
    assert_eq!(hook, xtask::pipeline::render_pre_commit_hook());
}

#[test]
fn the_committed_hook_is_executable() {
    let inputs =
        xtask::collect_inputs(&workspace_root()).expect("the workspace inputs should be readable");
    if cfg!(unix) {
        assert_eq!(
            inputs.pre_commit_hook_is_executable,
            Some(true),
            "git skips a hook without the execute bit"
        );
    }
}

#[test]
fn install_hooks_writes_the_hook_the_gate_expects() {
    let scratch = scratch_workspace("install-hooks");
    std::fs::remove_file(scratch.root.join(xtask::pipeline::PRE_COMMIT_PATH))
        .expect("the copied hook should be removable");

    let output = run_xtask(&["install-hooks"], &scratch.root);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let written = std::fs::read_to_string(scratch.root.join(xtask::pipeline::PRE_COMMIT_PATH))
        .expect("the hook should have been written");
    assert_eq!(written, xtask::pipeline::render_pre_commit_hook());
    assert!(
        xtask::pipeline::check_pre_commit_hook(Some(&written), Some(true)).is_empty(),
        "the generated hook must satisfy the rule that checks it"
    );
}

#[test]
fn a_hook_edited_by_hand_is_rejected() {
    let scratch = scratch_workspace("hook-drift");
    let hook = scratch.root.join(xtask::pipeline::PRE_COMMIT_PATH);
    let existing = std::fs::read_to_string(&hook).expect("the hook should be readable");
    std::fs::write(&hook, existing.replace("--locked", "")).expect("the hook should be writable");

    let violations =
        xtask::check_workspace(&scratch.root).expect("the policy check should be runnable");

    assert!(
        violations
            .iter()
            .any(|violation| violation.rule == "pre-commit-hook"),
        "a hand-edited hook must be rejected:\n{}",
        render(&violations)
    );
}

#[test]
fn a_workflow_that_stops_building_the_firmware_target_is_rejected() {
    let scratch = scratch_workspace("workflow-drift");
    let workflow = scratch.root.join(xtask::pipeline::WORKFLOW_PATH);
    let existing = std::fs::read_to_string(&workflow).expect("the workflow should be readable");
    std::fs::write(
        &workflow,
        existing.replace(stage("firmware").command, "cargo build"),
    )
    .expect("the workflow should be writable");

    let violations =
        xtask::check_workspace(&scratch.root).expect("the policy check should be runnable");

    assert!(
        violations
            .iter()
            .any(|violation| violation.rule == "ci-pipeline" && violation.subject == "firmware"),
        "dropping the firmware build from CI must be rejected:\n{}",
        render(&violations)
    );
}

#[test]
fn a_toolchain_that_stops_pinning_the_firmware_target_is_rejected() {
    let scratch = scratch_workspace("toolchain-drift");
    let toolchain = scratch.root.join(xtask::pipeline::TOOLCHAIN_PATH);
    let existing = std::fs::read_to_string(&toolchain).expect("the toolchain should be readable");
    std::fs::write(
        &toolchain,
        existing.replace(
            &format!("targets = [\"{}\"]\n", xtask::pipeline::FIRMWARE_TARGET),
            "",
        ),
    )
    .expect("the toolchain should be writable");

    let violations =
        xtask::check_workspace(&scratch.root).expect("the policy check should be runnable");

    assert!(
        violations
            .iter()
            .any(|violation| violation.rule == "toolchain-targets"),
        "dropping the firmware target from the toolchain must be rejected:\n{}",
        render(&violations)
    );
}
