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
//! It does the same for the pipeline. The stages CI runs are a table in [`pipeline`], the
//! pre-commit hook is rendered from that table, and the rules here fail a pull request in
//! which the committed workflow, the committed hook, or the pinned toolchain has drifted
//! from it — so "the hook and CI run the same commands" is checked rather than promised.
//! [`coverage`] gates each crate's line coverage on its own, because a workspace total is
//! how an untested kernel hides behind a tested adapter.
//!
//! The documentation is the third contract. `CLAUDE.md`, the ADR record, and the
//! architecture diagrams are prose that states the same invariants the tables hold, so
//! [`docs`] compares them against those tables — a "must not own" cell, a permitted
//! dependency edge, a settled decision, a protocol step — and fails a pull request in
//! which the words and the gate have stopped agreeing.
//!
//! Every rule is a pure function over parsed input so that it can be tested against a
//! deliberately broken workspace, not only against the real one.

#![warn(missing_docs)]

pub mod coverage;
pub mod docs;
pub mod elf;
pub mod graph;
pub mod manifest;
pub mod pipeline;
pub mod policy;
pub mod size;
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
    "adr-index",
    "adr-numbering",
    "adr-structure",
    "cargo-config-profile",
    "ci-pipeline",
    "claude-md",
    "crate-attributes",
    "deferred-questions",
    "dependency-direction",
    "dependency-direction-transitive",
    "diagrams",
    "effect-scheduled-fields",
    "embassy-below-facade",
    "empty-default-features",
    "gate-broken",
    "inputs-incomplete",
    "integrity-check",
    "kernel-owns-no-encoding",
    "kernel-zero-dependencies",
    "layer-missing",
    "layer-not-local",
    "member-manifest",
    "missing-docs",
    "no-build-scripts",
    "pre-commit-hook",
    "release-profile",
    "replay-cursor-surface",
    "settled-decisions",
    "size-probe",
    "size-probe-reach",
    "storage-contract",
    "toolchain-targets",
    "transition-surface",
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
    /// Contents of the CI workflow the pipeline stages must appear in.
    pub workflow: Option<String>,
    /// Contents of the committed pre-commit hook.
    pub pre_commit_hook: Option<String>,
    /// Whether that hook carries the execute bit, where the platform has one.
    ///
    /// `None` on a checkout whose filesystem does not record it, which the rule treats as
    /// "cannot tell" rather than as a violation.
    pub pre_commit_hook_is_executable: Option<bool>,
    /// Contents of `rust-toolchain.toml`.
    pub toolchain: Option<String>,
    /// Contents of the size probe's manifest, when the workspace has one.
    pub probe_manifest: Option<String>,
    /// Contents of the size probe's crate root, when the workspace has one.
    pub probe_source: Option<String>,
    /// Every source file of every firmware layer, for the probe-reach rule.
    ///
    /// Every file, not just the crate root: a public function in a submodule costs exactly
    /// as much flash as one in `lib.rs`.
    pub layer_sources: Vec<size::LayerSource>,
    /// `CLAUDE.md`, the decision record, the diagrams, and every crate root.
    pub docs: docs::DocsInputs,
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
    violations.extend(pipeline::check_workflow(inputs.workflow.as_deref()));
    violations.extend(pipeline::check_pre_commit_hook(
        inputs.pre_commit_hook.as_deref(),
        inputs.pre_commit_hook_is_executable,
    ));
    violations.extend(pipeline::check_toolchain(inputs.toolchain.as_deref()));
    violations.extend(size::check_size_probe(
        &graph,
        inputs.probe_manifest.as_deref(),
        inputs.probe_source.as_deref(),
    ));
    violations.extend(size::check_probe_reach(
        &inputs.layer_sources,
        inputs.probe_source.as_deref(),
    ));
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
    violations.extend(source::check_kernel_owns_no_encoding(&inputs.layer_sources));
    violations.extend(source::check_replay_cursor_surface(&inputs.layer_sources));
    violations.extend(source::check_transition_surface(&inputs.layer_sources));
    violations.extend(source::check_storage_contract(&inputs.layer_sources));
    violations.extend(source::check_effect_scheduled_fields(&inputs.layer_sources));
    violations.extend(source::check_integrity_check(&inputs.layer_sources));
    violations.extend(source::check_integrity_binding(&inputs.layer_sources));
    violations.extend(source::check_integrity_routing(&inputs.layer_sources));
    violations.extend(docs::check_documentation(&inputs.docs, RULES));

    violations.sort();
    violations.dedup();
    Ok(violations)
}

/// Rule: a firmware crate present in the graph contributed both a manifest and a crate
/// root, and every workspace member contributed at least one crate root.
///
/// Without this, an unreadable manifest or an unrecognised library target would remove a
/// crate from the manifest and attribute rules silently, and the gate would report
/// success because no rule had anything to say about it. The second half is what keeps a
/// workspace member out of reach of the `missing-docs` rule: a member with no target the
/// gate recognises is a member that rule never runs on.
#[must_use]
fn check_inputs_are_complete(
    graph: &graph::PackageGraph,
    inputs: &WorkspaceInputs,
) -> Vec<Violation> {
    let mut violations = Vec::new();

    for id in graph.workspace_members() {
        let Some(package) = graph.by_id(id) else {
            continue;
        };
        if !inputs
            .docs
            .crate_roots
            .iter()
            .any(|root| root.package == package.name)
        {
            violations.push(Violation::new(
                "inputs-incomplete",
                package.name.clone(),
                "is a workspace member with no crate root the gate could read, so the \
                 missing_docs rule did not run on it",
            ));
        }
    }

    for member in policy::checked_members() {
        if graph.find(member).is_none() {
            // Already reported as a missing layer.
            continue;
        }
        if !inputs
            .member_manifests
            .iter()
            .any(|(name, _)| name == member)
        {
            violations.push(Violation::new(
                "inputs-incomplete",
                member,
                "the crate is in the workspace but its manifest could not be located, so the manifest rules did not run on it",
            ));
        }
        if !inputs.crate_sources.iter().any(|(name, _)| name == member) {
            violations.push(Violation::new(
                "inputs-incomplete",
                member,
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

    let cargo_config = read_optional(&root.join(".cargo").join("config.toml"))?;
    let workflow = read_optional(&root.join(pipeline::WORKFLOW_PATH))?;
    let toolchain = read_optional(&root.join(pipeline::TOOLCHAIN_PATH))?;
    let hook_path = root.join(pipeline::PRE_COMMIT_PATH);
    let pre_commit_hook = read_optional(&hook_path)?;
    let pre_commit_hook_is_executable = pre_commit_hook
        .as_ref()
        .and_then(|_| is_executable(&hook_path));

    let mut member_manifests = Vec::new();
    let mut crate_sources = Vec::new();
    for name in policy::checked_members() {
        let Some(package) = graph.find(name) else {
            continue;
        };
        if let Some(path) = package.manifest_path.as_ref() {
            member_manifests.push((name.to_owned(), read_to_string(path)?));
        }
        if let Some(path) = package.lib_source_path.as_ref() {
            crate_sources.push((name.to_owned(), read_to_string(path)?));
        }
    }

    let mut layer_sources = Vec::new();
    for layer in policy::LAYERS {
        let Some(package) = graph.find(layer.name) else {
            continue;
        };
        let Some(source_root) = package
            .lib_source_path
            .as_ref()
            .and_then(|path| path.parent())
        else {
            continue;
        };
        for path in rust_sources(source_root) {
            layer_sources.push(size::LayerSource {
                crate_name: layer.name.to_owned(),
                path: path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
                contents: read_to_string(&path)?,
            });
        }
    }

    let probe = graph.find(size::PROBE_PACKAGE);
    let probe_manifest = probe
        .and_then(|package| package.manifest_path.as_ref())
        .map(|path| read_to_string(path))
        .transpose()?;
    let probe_source = probe
        .and_then(|package| package.bins.first())
        .and_then(|bin| bin.src_path.as_ref())
        .map(|path| read_to_string(path))
        .transpose()?;

    let docs = collect_docs_inputs(root, &graph)?;

    Ok(WorkspaceInputs {
        metadata_json,
        workspace_manifest,
        member_manifests,
        crate_sources,
        cargo_config,
        workflow,
        pre_commit_hook,
        pre_commit_hook_is_executable,
        toolchain,
        probe_manifest,
        probe_source,
        layer_sources,
        docs,
    })
}

/// Reads `CLAUDE.md`, the decision record, the architecture document, and every crate root.
///
/// Every crate root, not only the firmware layers': `xtask` and the size probe have public
/// items too, and issue #11 asks for `missing_docs` in *each* crate. A workspace member
/// whose targets contribute no readable root is reported by `inputs-incomplete`, the rule
/// that already exists for "a crate no rule could be run against".
///
/// # Errors
///
/// Returns [`CheckError`] if a file that exists cannot be read, or if `docs/adr` exists and
/// cannot be listed. A directory the gate cannot read is not a directory with nothing in
/// it, and reporting "the record has no index" when the truth is "the record is unreadable"
/// sends the reader to the wrong place.
fn collect_docs_inputs(
    root: &Path,
    graph: &graph::PackageGraph,
) -> Result<docs::DocsInputs, CheckError> {
    let adr_dir = root.join(docs::ADR_DIR);
    let mut adrs = Vec::new();
    let mut adr_index = None;
    match std::fs::read_dir(&adr_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(CheckError::new(format!(
                "could not read {}: {error}",
                adr_dir.display()
            )));
        }
        Ok(entries) => {
            let mut paths = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|error| {
                    CheckError::new(format!(
                        "could not read an entry of {}: {error}",
                        adr_dir.display()
                    ))
                })?;
                // Only Markdown. An ADR may reasonably sit beside an image it refers to,
                // and reading that image would fail the whole gate with a UTF-8 error
                // instead of reporting the 28 rules.
                if entry.path().extension().is_some_and(|kind| kind == "md") {
                    paths.push(entry.path());
                }
            }
            paths.sort();
            for path in paths {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let contents = read_to_string(&path)?;
                if name == docs::ADR_INDEX {
                    adr_index = Some(contents);
                } else {
                    adrs.push(docs::AdrFile { name, contents });
                }
            }
        }
    }

    let mut crate_roots = Vec::new();
    for path in workspace_crate_roots(graph) {
        let (package, path) = path;
        crate_roots.push(docs::CrateRoot {
            package,
            path: path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string(),
            contents: read_to_string(&path)?,
        });
    }
    crate_roots
        .sort_by(|left, right| (&left.package, &left.path).cmp(&(&right.package, &right.path)));

    Ok(docs::DocsInputs {
        claude_md: read_optional(&root.join(docs::CLAUDE_MD_PATH))?,
        architecture: read_optional(&root.join(docs::ARCHITECTURE_PATH))?,
        adr_index,
        adrs,
        crate_roots,
    })
}

/// `(package name, crate root)` for every library and binary target of every workspace
/// member.
fn workspace_crate_roots(graph: &graph::PackageGraph) -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    for id in graph.workspace_members() {
        let Some(package) = graph.by_id(id) else {
            // `check_workspace_membership` already reports a member that is not a package.
            continue;
        };
        for path in package
            .lib_source_path
            .iter()
            .cloned()
            .chain(package.bins.iter().filter_map(|bin| bin.src_path.clone()))
        {
            roots.push((package.name.clone(), path));
        }
    }
    roots
}

/// Every `.rs` file under `directory`, in a stable order.
///
/// Walked rather than globbed so that `xtask` keeps its two dependencies, and sorted so
/// that a violation list does not reorder itself between runs.
fn rust_sources(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Reads a file that the workspace may legitimately not have yet.
///
/// A missing file is `None` rather than an error: the rule that wanted it reports its
/// absence in the same report as every other violation, instead of stopping the gate
/// before the other rules have run.
fn read_optional(path: &Path) -> Result<Option<String>, CheckError> {
    if path.is_file() {
        read_to_string(path).map(Some)
    } else {
        Ok(None)
    }
}

/// Whether `path` carries an execute bit, or `None` where the platform has none to read.
#[cfg(unix)]
fn is_executable(path: &Path) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    Some(mode & 0o111 != 0)
}

/// Whether `path` carries an execute bit, or `None` where the platform has none to read.
#[cfg(not(unix))]
fn is_executable(_path: &Path) -> Option<bool> {
    None
}

fn read_to_string(path: &Path) -> Result<String, CheckError> {
    std::fs::read_to_string(path)
        .map_err(|err| CheckError::new(format!("could not read {}: {err}", path.display())))
}

pub(crate) fn run_cargo_metadata(root: &Path) -> Result<String, CheckError> {
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
            // No workflow, no hook, and a toolchain that never heard of the firmware
            // target: ci-pipeline, pre-commit-hook and toolchain-targets all fire.
            workflow: None,
            pre_commit_hook: None,
            pre_commit_hook_is_executable: None,
            toolchain: Some("[toolchain]\nchannel = \"1.97\"\n".to_owned()),
            // The broken workspace has no size probe at all, which is itself a violation:
            // a budget nothing links cannot be measured.
            probe_manifest: None,
            probe_source: None,
            // A kernel with a public function the probe does not call: the reach rule
            // fires, because a function nothing links is a function no budget charges for.
            layer_sources: vec![size::LayerSource {
                crate_name: "waymaker-core".to_owned(),
                path: "crates/waymaker-core/src/lib.rs".to_owned(),
                contents:
                    "pub fn advance() {}\nfn read(b: &[u8]) -> u32 { u32::from_le_bytes(b) }\n"
                        .to_owned(),
            }],
            // No CLAUDE.md, no architecture document, no ADR index, an ADR that is
            // numbered but structurally empty, and a crate root with no missing_docs
            // attribute: every documentation rule fires.
            docs: docs::DocsInputs {
                claude_md: None,
                architecture: None,
                adr_index: None,
                adrs: vec![docs::AdrFile {
                    name: "0001-undated.md".to_owned(),
                    contents: "no title, no status, no sections\n".to_owned(),
                }],
                crate_roots: vec![docs::CrateRoot {
                    package: "waymaker-core".to_owned(),
                    path: "crates/waymaker-core/src/lib.rs".to_owned(),
                    contents: "pub fn advance() {}\n".to_owned(),
                }],
            },
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
            "adr-index",
            "adr-numbering",
            "adr-structure",
            "cargo-config-profile",
            "ci-pipeline",
            "claude-md",
            "crate-attributes",
            "deferred-questions",
            "dependency-direction",
            "dependency-direction-transitive",
            "diagrams",
            "effect-scheduled-fields",
            "embassy-below-facade",
            "empty-default-features",
            "inputs-incomplete",
            "integrity-check",
            "kernel-owns-no-encoding",
            "kernel-zero-dependencies",
            "layer-missing",
            "layer-not-local",
            "member-manifest",
            "missing-docs",
            "no-build-scripts",
            "pre-commit-hook",
            "release-profile",
            "replay-cursor-surface",
            "settled-decisions",
            "size-probe",
            "size-probe-reach",
            "storage-contract",
            "toolchain-targets",
            "transition-surface",
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

    /// Inputs describing a workspace every rule accepts.
    ///
    /// A function rather than a literal inside one test, so that a test about *one* rule can
    /// take a clean workspace and break exactly one thing in it — which is the only way to
    /// prove a rule is wired into `check_inputs` when its id is already fired by a sibling.
    fn clean_inputs() -> WorkspaceInputs {
        WorkspaceInputs {
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
            cargo_config: Some(format!(
                "[alias]\nxtask = \"{}\"\n",
                manifest::REQUIRED_XTASK_ALIAS
            )),
            workflow: Some(pipeline::tests_support::clean_workflow()),
            pre_commit_hook: Some(pipeline::render_pre_commit_hook()),
            pre_commit_hook_is_executable: Some(true),
            toolchain: Some(pipeline::tests_support::clean_toolchain()),
            probe_manifest: Some(size::tests_support::clean_probe_manifest()),
            probe_source: Some(format!(
                "{}{}",
                size::tests_support::clean_probe_source(),
                source::tests_support::clean_probe_calls()
            )),
            // Two sources, because both surface pins fail closed when the module they pin
            // is not in the workspace at all: `replay-cursor-surface` for the cursor's
            // public API, `transition-surface` for the replay machine's.
            layer_sources: vec![
                size::LayerSource {
                    crate_name: "waymaker-core".to_owned(),
                    path: format!("crates/{}", source::REPLAY_SURFACE_PATH),
                    contents: source::tests_support::clean_replay_surface(),
                },
                size::LayerSource {
                    crate_name: "waymaker-core".to_owned(),
                    path: format!("crates/{}", source::TRANSITION_SURFACE_PATH),
                    contents: source::tests_support::clean_transition_surface(),
                },
                // And two more, for the two pins issue #16's answers are held by:
                // `effect-scheduled-fields` for the metadata ADR 0011 settled, and
                // `integrity-check` for the checksum ADR 0010 settled. Both fail closed
                // when the module they pin is absent, so a fixture without them describes
                // a workspace the gate rejects for a reason no test here is about.
                size::LayerSource {
                    crate_name: "waymaker-core".to_owned(),
                    path: format!("crates/{}", source::EFFECT_SCHEDULED_PATH),
                    contents: source::tests_support::clean_record_module(),
                },
                // And the binding issue #17's answer is held by: the trait the seals go
                // through, and the type the shipped algorithms are bound to. It fails
                // closed when the module is absent, for the same reason as the three
                // above.
                // And the storage contract of §12, which `storage-contract` pins for the
                // same reason: renamed or deleted, the pin checks nothing.
                size::LayerSource {
                    crate_name: "waymaker-flash".to_owned(),
                    path: format!("crates/{}", source::STORAGE_CONTRACT_PATH),
                    contents: source::tests_support::clean_storage_contract(),
                },
                size::LayerSource {
                    crate_name: "waymaker-flash".to_owned(),
                    path: format!("crates/{}", source::INTEGRITY_BINDING_PATH),
                    contents: source::tests_support::clean_integrity_binding(),
                },
                size::LayerSource {
                    crate_name: "waymaker-flash".to_owned(),
                    path: format!("crates/{}", source::INTEGRITY_ROUTING_PATH),
                    contents: source::tests_support::clean_integrity_routing(),
                },
                size::LayerSource {
                    crate_name: "waymaker-flash".to_owned(),
                    path: format!("crates/{}", source::INTEGRITY_CHECK_PATH),
                    contents: source::tests_support::clean_checksum_module(),
                },
            ],
            docs: docs::DocsInputs {
                // A root per workspace member, because `inputs-incomplete` now reports a
                // member the `missing-docs` rule could not be run against.
                crate_roots: [
                    "waymaker-core",
                    "waymaker-flash",
                    "waymaker-embassy",
                    "waymaker-size-probe",
                ]
                .into_iter()
                .map(|package| docs::CrateRoot {
                    package: package.to_owned(),
                    path: format!("crates/{package}/src/lib.rs"),
                    contents: "//! Docs.\n#![warn(missing_docs)]\n".to_owned(),
                })
                .collect(),
                ..docs::tests_support::clean_inputs(RULES)
            },
        }
    }

    #[test]
    fn check_inputs_reports_nothing_for_a_clean_workspace() {
        let violations = check_inputs(&clean_inputs()).expect("the inputs should be checkable");
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn check_inputs_holds_claude_md_to_the_real_rule_list() {
        // `check_documentation` takes the rule list as a parameter so that it can be
        // tested against a list of its own. Nothing otherwise pins what `check_inputs`
        // passes: with `&[]` there, every test in this crate still passes, and CLAUDE.md
        // could stop documenting the gate entirely.
        let mut inputs = broken_inputs();
        inputs.docs.claude_md =
            Some(docs::tests_support::clean_claude_md(RULES).replace("`toolchain-targets`", ""));
        let violations = check_inputs(&inputs).expect("checkable");
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "claude-md" && v.subject == "toolchain-targets"),
            "{violations:?}"
        );
    }

    #[test]
    fn check_inputs_wires_up_both_halves_of_the_integrity_pin() {
        // `check_inputs_wires_up_every_rule` cannot see these. `integrity-check` already
        // fires on the broken fixture from `check_integrity_check`, so deleting either of
        // the other two `violations.extend(..)` lines leaves the rule id in the set and
        // every test green — which is exactly what happened when review of this change
        // commented one out: 501 tests, 0 failures, and the gate printed `ok`. So each half
        // is asserted through a detail only it emits.
        for (contents, path, marker) in [
            (
                source::tests_support::clean_integrity_binding()
                    .replace("-> u16;", "-> u32;")
                    .replace("-> u16 {", "-> u32 {"),
                source::INTEGRITY_BINDING_PATH,
                "has changed width",
            ),
            (
                source::tests_support::clean_integrity_routing()
                    .replace("C::header_check(bytes)", "crc16(bytes)"),
                source::INTEGRITY_ROUTING_PATH,
                "goes around the trait",
            ),
        ] {
            let mut inputs = clean_inputs();
            for layer_source in &mut inputs.layer_sources {
                if layer_source.path.ends_with(path) {
                    layer_source.contents.clone_from(&contents);
                }
            }
            let violations = check_inputs(&inputs).expect("checkable");
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.rule == "integrity-check"
                        && violation.detail.contains(marker)),
                "the half emitting {marker:?} is not wired into check_inputs: {violations:?}"
            );
        }
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
          "features": {}, "targets": [{ "kind": ["lib"], "src_path": "/w/e/src/lib.rs" }] },
        { "id": "probe", "name": "waymaker-size-probe", "source": null,
          "manifest_path": "/w/probe/Cargo.toml",
          "dependencies": [{ "name": "waymaker-core", "kind": null },
                           { "name": "waymaker-flash", "kind": null },
                           { "name": "waymaker-embassy", "kind": null }],
          "features": { "probe": [], "engine": [], "facade": [] },
          "targets": [{ "kind": ["bin"], "name": "waymaker-size-probe",
                        "src_path": "/w/probe/src/main.rs",
                        "required-features": ["probe"] }] }
      ],
      "workspace_members": ["core", "flash", "embassy", "probe"],
      "resolve": { "nodes": [
        { "id": "core", "deps": [] },
        { "id": "flash", "deps": [{ "pkg": "core" }] },
        { "id": "embassy", "deps": [{ "pkg": "core" }, { "pkg": "flash" }] },
        { "id": "probe", "deps": [{ "pkg": "core" }, { "pkg": "flash" }, { "pkg": "embassy" }] }
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
