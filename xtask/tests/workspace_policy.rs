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
