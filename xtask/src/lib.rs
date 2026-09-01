//! Workspace policy checks for the Waymaker repository.
//!
//! The design document's §05 "must not own" column is a contract. This crate turns that
//! contract into something a machine can fail a pull request over:
//!
//! * the dependency direction `waymaker-embassy → waymaker-flash → waymaker-core`,
//! * the kernel's zero-dependency rule,
//! * empty default features in every firmware crate,
//! * `#![no_std]` and `#![forbid(unsafe_code)]` in every firmware crate,
//! * the release profile and workspace lint table from the design document.
//!
//! Every rule is a pure function over parsed input so that it can be tested against a
//! deliberately broken workspace, not only against the real one.

pub mod graph;
pub mod manifest;
pub mod policy;
pub mod source;

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A single breach of workspace policy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Violation {
    /// Stable identifier for the rule that fired, for example `dependency-direction`.
    pub rule: &'static str,
    /// What the rule was looking at: a crate name, a manifest path, a lint key.
    pub subject: String,
    /// Human-readable explanation, naming the offending value where there is one.
    pub detail: String,
}

impl Violation {
    /// Records a breach of `rule` observed on `subject`.
    pub fn new(rule: &'static str, subject: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            rule,
            subject: subject.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}: {}", self.rule, self.subject, self.detail)
    }
}

/// Something went wrong while collecting the inputs for the checks.
///
/// This is distinct from a [`Violation`]: a violation means the workspace is wrong, a
/// [`CheckError`] means the check itself could not be run.
#[derive(Debug)]
pub struct CheckError {
    message: String,
}

impl CheckError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CheckError {}

/// Everything the rules need, already read off disk.
///
/// Holding this separately from [`check_workspace`] keeps every rule a pure function and
/// lets the tests build a workspace that does not exist.
#[derive(Debug, Clone)]
pub struct WorkspaceInputs {
    /// Output of `cargo metadata --format-version 1 --all-features`.
    pub metadata_json: String,
    /// Contents of the workspace root `Cargo.toml`.
    pub workspace_manifest: String,
    /// `(crate name, manifest contents)` for each firmware crate that exists.
    pub member_manifests: Vec<(String, String)>,
    /// `(crate name, `lib.rs` contents)` for each firmware crate that exists.
    pub crate_sources: Vec<(String, String)>,
}

/// Runs every rule against already-collected inputs.
///
/// Returns the violations sorted and deduplicated, so the output is stable enough to diff
/// between runs.
///
/// # Errors
///
/// Returns [`CheckError`] if the `cargo metadata` output cannot be parsed.
pub fn check_inputs(inputs: &WorkspaceInputs) -> Result<Vec<Violation>, CheckError> {
    let graph = graph::PackageGraph::from_cargo_metadata(&inputs.metadata_json)
        .map_err(|err| CheckError::new(format!("could not parse cargo metadata: {err}")))?;

    let mut violations = Vec::new();
    violations.extend(graph::check_dependency_direction(&graph));
    violations.extend(graph::check_kernel_has_no_dependencies(&graph));
    violations.extend(graph::check_embassy_stays_above_flash(&graph));
    violations.extend(graph::check_empty_default_features(&graph));
    violations.extend(manifest::check_release_profile(&inputs.workspace_manifest));
    violations.extend(manifest::check_workspace_lints(&inputs.workspace_manifest));
    for (name, contents) in &inputs.member_manifests {
        violations.extend(manifest::check_member_manifest(name, contents));
    }
    let sources: Vec<source::CrateSource<'_>> = inputs
        .crate_sources
        .iter()
        .map(|(name, contents)| source::CrateSource {
            name: name.as_str(),
            contents: contents.as_str(),
        })
        .collect();
    violations.extend(source::check_crate_attributes(&sources));

    violations.sort();
    violations.dedup();
    Ok(violations)
}

/// Collects the inputs for the workspace rooted at `root` and runs every rule.
///
/// # Errors
///
/// Returns [`CheckError`] if `cargo metadata` cannot be run or its output cannot be
/// parsed, or if the workspace manifest cannot be read.
pub fn check_workspace(root: &Path) -> Result<Vec<Violation>, CheckError> {
    let inputs = collect_inputs(root)?;
    check_inputs(&inputs)
}

/// Reads the workspace manifest, `cargo metadata`, and each firmware crate's sources.
///
/// Crates named in [`policy::LAYERS`] that do not exist yet contribute no manifest and no
/// source; their absence is reported by the graph rules rather than as an I/O error, so a
/// half-built workspace still produces a useful report.
///
/// # Errors
///
/// Returns [`CheckError`] if `cargo metadata` fails or the root manifest is unreadable.
pub fn collect_inputs(root: &Path) -> Result<WorkspaceInputs, CheckError> {
    let metadata_json = run_cargo_metadata(root)?;
    let workspace_manifest_path = root.join("Cargo.toml");
    let workspace_manifest = read_to_string(&workspace_manifest_path)?;

    let graph = graph::PackageGraph::from_cargo_metadata(&metadata_json)
        .map_err(|err| CheckError::new(format!("could not parse cargo metadata: {err}")))?;

    let mut member_manifests = Vec::new();
    let mut crate_sources = Vec::new();
    for layer in policy::LAYERS {
        let Some(package) = graph.find(layer.name) else {
            continue;
        };
        if let Some(path) = package.manifest_path.as_ref() {
            member_manifests.push((layer.name.to_owned(), read_to_string(path)?));
        }
        if let Some(path) = package.lib_source_path.as_ref() {
            crate_sources.push((layer.name.to_owned(), read_to_string(path)?));
        }
    }

    Ok(WorkspaceInputs {
        metadata_json,
        workspace_manifest,
        member_manifests,
        crate_sources,
    })
}

fn read_to_string(path: &Path) -> Result<String, CheckError> {
    std::fs::read_to_string(path)
        .map_err(|err| CheckError::new(format!("could not read {}: {err}", path.display())))
}

fn run_cargo_metadata(root: &Path) -> Result<String, CheckError> {
    let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    let output = Command::new(cargo)
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--all-features"])
        .output()
        .map_err(|err| CheckError::new(format!("could not run cargo metadata: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CheckError::new(format!(
            "cargo metadata failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }

    String::from_utf8(output.stdout)
        .map_err(|err| CheckError::new(format!("cargo metadata emitted invalid UTF-8: {err}")))
}
