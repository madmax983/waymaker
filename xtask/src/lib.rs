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

#![warn(missing_docs)]

pub mod graph;
pub mod manifest;
pub mod policy;
pub mod source;

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Every rule identifier the gate can emit.
///
/// A rule that is written but never wired into [`check_inputs`] is not a rule. This list
/// is what the wiring test compares against, and what the CLI counts when it reports
/// success, so a new rule cannot be added without appearing in both.
pub const RULES: &[&str] = &[
    "cargo-config-profile",
    "crate-attributes",
    "dependency-direction",
    "dependency-direction-transitive",
    "embassy-below-facade",
    "empty-default-features",
    "gate-broken",
    "inputs-incomplete",
    "kernel-zero-dependencies",
    "layer-missing",
    "layer-not-local",
    "member-manifest",
    "no-build-scripts",
    "release-profile",
    "workspace-lints",
    "workspace-membership",
];

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
    /// Contents of `.cargo/config.toml`, when the workspace has one.
    pub cargo_config: Option<String>,
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
    violations.extend(graph::check_workspace_membership(&graph));
    violations.extend(graph::check_layers_are_local(&graph));
    violations.extend(graph::check_no_build_scripts(&graph));
    violations.extend(check_inputs_are_complete(&graph, inputs));
    violations.extend(manifest::check_release_profile(&inputs.workspace_manifest));
    violations.extend(manifest::check_workspace_lints(&inputs.workspace_manifest));
    violations.extend(manifest::check_cargo_config(inputs.cargo_config.as_deref()));
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

/// Rule: a firmware crate present in the graph contributed both a manifest and a crate
/// root.
///
/// Without this, an unreadable manifest or an unrecognised library target would remove a
/// crate from the manifest and attribute rules silently, and the gate would report
/// success because no rule had anything to say about it.
#[must_use]
fn check_inputs_are_complete(
    graph: &graph::PackageGraph,
    inputs: &WorkspaceInputs,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    for spec in policy::LAYERS {
        if graph.find(spec.name).is_none() {
            // Already reported as a missing layer.
            continue;
        }
        if !inputs
            .member_manifests
            .iter()
            .any(|(name, _)| name == spec.name)
        {
            violations.push(Violation::new(
                "inputs-incomplete",
                spec.name,
                "the crate is in the workspace but its manifest could not be located, so the manifest rules did not run on it",
            ));
        }
        if !inputs
            .crate_sources
            .iter()
            .any(|(name, _)| name == spec.name)
        {
            violations.push(Violation::new(
                "inputs-incomplete",
                spec.name,
                "the crate is in the workspace but has no library root, so the #![no_std] and unsafe rules did not run on it",
            ));
        }
    }

    violations
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

    let cargo_config_path = root.join(".cargo").join("config.toml");
    let cargo_config = if cargo_config_path.is_file() {
        Some(read_to_string(&cargo_config_path)?)
    } else {
        None
    };

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
        cargo_config,
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
        // --locked so the gate reads exactly the resolution committed to Cargo.lock, and
        // --all-features so that optional and feature-gated dependencies are resolved
        // into the graph rather than hidden behind a feature nobody enabled.
        .args([
            "metadata",
            "--format-version",
            "1",
            "--all-features",
            "--locked",
        ])
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Metadata for a workspace that breaks every graph rule at once:
    /// - `waymaker-core` has a registry dependency and a build script,
    /// - `waymaker-flash` reaches Embassy and has a non-empty default feature,
    /// - `waymaker-embassy` is absent,
    /// - `stowaway` is a workspace member that is not a layer.
    const BROKEN_METADATA: &str = r#"{
      "packages": [
        {
          "id": "core", "name": "waymaker-core",
          "source": "registry+https://github.com/rust-lang/crates.io-index",
          "manifest_path": "/w/core/Cargo.toml",
          "dependencies": [{ "name": "memchr", "kind": null }],
          "features": {},
          "targets": [
            { "kind": ["lib"], "src_path": "/w/core/src/lib.rs" },
            { "kind": ["custom-build"], "src_path": "/w/core/build.rs" }
          ]
        },
        {
          "id": "flash", "name": "waymaker-flash", "source": null,
          "manifest_path": "/w/flash/Cargo.toml",
          "dependencies": [
            { "name": "waymaker-core", "kind": null },
            { "name": "embassy-time", "kind": null }
          ],
          "features": { "default": ["std"], "std": [] },
          "targets": [{ "kind": ["lib"], "src_path": "/w/flash/src/lib.rs" }]
        },
        {
          "id": "memchr", "name": "memchr", "source": "registry+https://crates.io",
          "dependencies": [], "features": {}, "targets": []
        },
        {
          "id": "embassy-time", "name": "embassy-time", "source": "registry+https://crates.io",
          "dependencies": [], "features": {}, "targets": []
        },
        {
          "id": "stowaway", "name": "stowaway", "source": null,
          "dependencies": [], "features": {}, "targets": []
        }
      ],
      "workspace_members": ["core", "flash", "stowaway"],
      "resolve": {
        "nodes": [
          { "id": "core", "deps": [{ "pkg": "memchr" }] },
          { "id": "flash", "deps": [{ "pkg": "core" }, { "pkg": "embassy-time" }] },
          { "id": "memchr", "deps": [] },
          { "id": "embassy-time", "deps": [] },
          { "id": "stowaway", "deps": [] }
        ]
      }
    }"#;

    fn broken_inputs() -> WorkspaceInputs {
        WorkspaceInputs {
            metadata_json: BROKEN_METADATA.to_owned(),
            // No [profile.release], no [workspace.lints].
            workspace_manifest: "[workspace]\nmembers = []\n".to_owned(),
            member_manifests: vec![(
                // No [lints] workspace = true, and a dependency table in the kernel.
                "waymaker-core".to_owned(),
                "[package]\nname = \"waymaker-core\"\n\n[dependencies]\nmemchr = \"2\"\n"
                    .to_owned(),
            )],
            // waymaker-flash is in the graph but contributes no source: inputs-incomplete.
            crate_sources: vec![("waymaker-core".to_owned(), "// no attributes\n".to_owned())],
            cargo_config: Some("[profile.release]\nopt-level = 3\n".to_owned()),
        }
    }

    fn rules_fired(inputs: &WorkspaceInputs) -> BTreeSet<&'static str> {
        check_inputs(inputs)
            .expect("the inputs should be checkable")
            .iter()
            .map(|violation| violation.rule)
            .collect()
    }

    #[test]
    fn check_inputs_wires_up_every_rule() {
        // This is the test that stops a rule from being written and then quietly not
        // called. If a `violations.extend(...)` line is deleted from `check_inputs`, its
        // rule id disappears from this set and the test fails.
        let expected: BTreeSet<&str> = [
            "cargo-config-profile",
            "crate-attributes",
            "dependency-direction",
            "dependency-direction-transitive",
            "embassy-below-facade",
            "empty-default-features",
            "inputs-incomplete",
            "kernel-zero-dependencies",
            "layer-missing",
            "layer-not-local",
            "member-manifest",
            "no-build-scripts",
            "release-profile",
            "workspace-lints",
            "workspace-membership",
        ]
        .into_iter()
        .collect();

        assert_eq!(rules_fired(&broken_inputs()), expected);
    }

    #[test]
    fn every_rule_the_gate_emits_is_declared_in_rules() {
        let declared: BTreeSet<&str> = RULES.iter().copied().collect();
        let fired = rules_fired(&broken_inputs());
        assert!(
            fired.is_subset(&declared),
            "undeclared rule ids: {:?}",
            fired.difference(&declared).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rules_is_sorted_and_free_of_duplicates() {
        let mut sorted = RULES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, RULES);
    }

    #[test]
    fn check_inputs_reports_nothing_for_a_clean_workspace() {
        let inputs = WorkspaceInputs {
            metadata_json: CLEAN_METADATA.to_owned(),
            workspace_manifest: CLEAN_WORKSPACE_MANIFEST.to_owned(),
            member_manifests: policy::LAYERS
                .iter()
                .map(|spec| {
                    (
                        spec.name.to_owned(),
                        "[package]\nname = \"x\"\n\n[lints]\nworkspace = true\n".to_owned(),
                    )
                })
                .collect(),
            crate_sources: policy::LAYERS
                .iter()
                .map(|spec| {
                    (
                        spec.name.to_owned(),
                        "#![no_std]\n#![forbid(unsafe_code)]\n".to_owned(),
                    )
                })
                .collect(),
            cargo_config: Some("[alias]\nxtask = \"run -p xtask --\"\n".to_owned()),
        };

        let violations = check_inputs(&inputs).expect("the inputs should be checkable");
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn check_inputs_surfaces_a_metadata_parse_failure_as_an_error() {
        let mut inputs = broken_inputs();
        inputs.metadata_json = "not json".to_owned();
        let error = check_inputs(&inputs).expect_err("malformed metadata must fail closed");
        assert!(error.to_string().contains("cargo metadata"), "{error}");
    }

    #[test]
    fn check_inputs_reports_a_crate_with_no_library_root() {
        let mut inputs = broken_inputs();
        inputs.crate_sources.clear();
        let incomplete: Vec<Violation> = check_inputs(&inputs)
            .expect("checkable")
            .into_iter()
            .filter(|violation| violation.rule == "inputs-incomplete")
            .collect();
        assert!(
            incomplete
                .iter()
                .any(|violation| violation.subject == "waymaker-core"
                    && violation.detail.contains("library root")),
            "{incomplete:?}"
        );
    }

    #[test]
    fn violations_are_sorted_and_deduplicated() {
        let violations = check_inputs(&broken_inputs()).expect("checkable");
        let mut expected = violations.clone();
        expected.sort();
        expected.dedup();
        assert_eq!(violations, expected);
    }

    #[test]
    fn a_violation_renders_its_rule_subject_and_detail() {
        let violation = Violation::new("some-rule", "some-crate", "some detail");
        assert_eq!(violation.to_string(), "[some-rule] some-crate: some detail");
    }

    #[test]
    fn a_check_error_renders_its_message() {
        assert_eq!(CheckError::new("boom").to_string(), "boom");
    }

    const CLEAN_METADATA: &str = r#"{
      "packages": [
        { "id": "core", "name": "waymaker-core", "source": null, "dependencies": [],
          "features": {}, "targets": [{ "kind": ["lib"], "src_path": "/w/core/src/lib.rs" }] },
        { "id": "flash", "name": "waymaker-flash", "source": null,
          "dependencies": [{ "name": "waymaker-core", "kind": null }],
          "features": {}, "targets": [{ "kind": ["lib"], "src_path": "/w/flash/src/lib.rs" }] },
        { "id": "embassy", "name": "waymaker-embassy", "source": null,
          "dependencies": [{ "name": "waymaker-core", "kind": null },
                           { "name": "waymaker-flash", "kind": null }],
          "features": {}, "targets": [{ "kind": ["lib"], "src_path": "/w/e/src/lib.rs" }] }
      ],
      "workspace_members": ["core", "flash", "embassy"],
      "resolve": { "nodes": [
        { "id": "core", "deps": [] },
        { "id": "flash", "deps": [{ "pkg": "core" }] },
        { "id": "embassy", "deps": [{ "pkg": "core" }, { "pkg": "flash" }] }
      ] }
    }"#;

    const CLEAN_WORKSPACE_MANIFEST: &str = r#"
[workspace]
resolver = "3"

[workspace.lints.rust]
unsafe_code = "forbid"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
nursery = { level = "warn", priority = -1 }
unwrap_used = "deny"

[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
panic = "abort"
strip = "symbols"
"#;
}
