//! The resolved package graph and the rules that read it.
//!
//! The graph is built from `cargo metadata`, which already flattens target-specific and
//! feature-gated dependency tables. Reading it instead of the raw manifests is what stops
//! a `[target.'cfg(...)'.dependencies]` table or an optional dependency from slipping a
//! crate past the layering gate.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::path::PathBuf;

use serde_json::Value;

use crate::Violation;
use crate::policy::{self, LAYERS};

/// Cargo's opaque package identifier.
pub type PackageId = String;

/// Which dependency table a dependency came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DepKind {
    /// `[dependencies]`.
    Normal,
    /// `[dev-dependencies]`.
    Development,
    /// `[build-dependencies]`.
    Build,
}

impl DepKind {
    /// Maps `cargo metadata`'s `kind` field, where `null` means a normal dependency.
    #[must_use]
    pub fn from_metadata(kind: Option<&str>) -> Self {
        match kind {
            Some("dev") => Self::Development,
            Some("build") => Self::Build,
            _ => Self::Normal,
        }
    }
}

impl fmt::Display for DepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Normal => "dependencies",
            Self::Development => "dev-dependencies",
            Self::Build => "build-dependencies",
        };
        f.write_str(name)
    }
}

/// A dependency as declared in a manifest, before resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDep {
    /// The dependency's package name.
    pub name: String,
    /// The table it was declared in.
    pub kind: DepKind,
}

/// One package in the workspace's resolved graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Cargo's package identifier.
    pub id: PackageId,
    /// The package name.
    pub name: String,
    /// Dependencies declared in the manifest, across every table and target.
    pub manifest_deps: Vec<ManifestDep>,
    /// The features enabled by the `default` feature, empty when there is no `default`.
    pub default_features: Vec<String>,
    /// Resolved graph edges, by package id.
    pub resolved_deps: Vec<PackageId>,
    /// Absolute path to the package's `Cargo.toml`, when known.
    pub manifest_path: Option<PathBuf>,
    /// Absolute path to the package's library root, when the package has a library.
    pub lib_source_path: Option<PathBuf>,
    /// Cargo's `source` field: `None` for a path or workspace package, `Some` for a
    /// registry or git package.
    pub source: Option<String>,
    /// Whether the package has a `build.rs`.
    pub has_build_script: bool,
}

impl Package {
    /// Builds a package with no dependencies, no features, and no on-disk paths.
    ///
    /// Tests use this to describe a workspace that does not exist.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            id: name.to_owned(),
            name: name.to_owned(),
            manifest_deps: Vec::new(),
            default_features: Vec::new(),
            resolved_deps: Vec::new(),
            manifest_path: None,
            lib_source_path: None,
            source: None,
            has_build_script: false,
        }
    }

    /// Marks the package as coming from a registry or git source rather than a path.
    #[must_use]
    pub fn from_source(mut self, source: &str) -> Self {
        self.source = Some(source.to_owned());
        self
    }

    /// Marks the package as having a `build.rs`.
    #[must_use]
    pub const fn with_build_script(mut self) -> Self {
        self.has_build_script = true;
        self
    }

    /// Adds a declared and resolved dependency on `name`, in the given table.
    #[must_use]
    pub fn with_dependency(mut self, name: &str, kind: DepKind) -> Self {
        self.manifest_deps.push(ManifestDep {
            name: name.to_owned(),
            kind,
        });
        self.resolved_deps.push(name.to_owned());
        self
    }

    /// Sets the packages enabled by the `default` feature.
    #[must_use]
    pub fn with_default_features(mut self, features: &[&str]) -> Self {
        self.default_features = features
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect();
        self
    }
}

/// Failed to turn `cargo metadata` output into a [`PackageGraph`].
#[derive(Debug)]
pub struct MetadataError {
    message: String,
}

impl MetadataError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MetadataError {}

/// Every package cargo resolved for the workspace, keyed by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageGraph {
    packages: Vec<Package>,
    workspace_members: Vec<PackageId>,
}

impl PackageGraph {
    /// Builds a graph from an explicit package list, with no workspace members declared.
    #[must_use]
    pub const fn new(packages: Vec<Package>) -> Self {
        Self {
            packages,
            workspace_members: Vec::new(),
        }
    }

    /// Declares which package ids are workspace members.
    #[must_use]
    pub fn with_workspace_members(mut self, ids: &[&str]) -> Self {
        self.workspace_members = ids.iter().map(|id| (*id).to_owned()).collect();
        self
    }

    /// The package ids cargo reports as workspace members.
    #[must_use]
    pub fn workspace_members(&self) -> &[PackageId] {
        &self.workspace_members
    }

    /// Returns every package in the graph.
    #[must_use]
    pub fn packages(&self) -> &[Package] {
        &self.packages
    }

    /// Finds a package by name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|package| package.name == name)
    }

    /// Finds a package by cargo package id.
    #[must_use]
    pub fn by_id(&self, id: &str) -> Option<&Package> {
        self.packages.iter().find(|package| package.id == id)
    }

    /// Returns the names of every package reachable from `name`, excluding `name` itself.
    ///
    /// An edge to an id that is not in the graph is skipped rather than reported: the
    /// direct-dependency rule already names anything declared but unresolved.
    #[must_use]
    pub fn transitive_dependencies(&self, name: &str) -> BTreeSet<String> {
        let mut reached = BTreeSet::new();
        let Some(root) = self.find(name) else {
            return reached;
        };

        let mut seen_ids: BTreeSet<&str> = BTreeSet::new();
        seen_ids.insert(root.id.as_str());
        let mut queue: VecDeque<&str> = root.resolved_deps.iter().map(String::as_str).collect();

        while let Some(id) = queue.pop_front() {
            if !seen_ids.insert(id) {
                continue;
            }
            let Some(package) = self.by_id(id) else {
                continue;
            };
            reached.insert(package.name.clone());
            for next in &package.resolved_deps {
                queue.push_back(next.as_str());
            }
        }

        reached.remove(name);
        reached
    }

    /// Returns the shortest path to every package reachable from `name` that is not in
    /// `allowed`, stopping at the first illegal hop on each branch.
    ///
    /// Truncating at the first illegal package is what keeps the report readable. One
    /// forbidden dependency drags in its whole subtree; naming the subtree tells a
    /// reviewer nothing they can act on, while naming the edge that admitted it tells
    /// them exactly what to delete.
    ///
    /// Each path starts at `name` and ends at the offending package.
    #[must_use]
    pub fn illegal_reach_paths(&self, name: &str, allowed: &BTreeSet<&str>) -> Vec<Vec<String>> {
        let Some(root) = self.find(name) else {
            return Vec::new();
        };

        let mut paths = Vec::new();
        let mut seen_ids: BTreeSet<&str> = BTreeSet::new();
        seen_ids.insert(root.id.as_str());

        let mut queue: VecDeque<(&str, Vec<String>)> = root
            .resolved_deps
            .iter()
            .map(|id| (id.as_str(), vec![root.name.clone()]))
            .collect();

        while let Some((id, path)) = queue.pop_front() {
            if !seen_ids.insert(id) {
                continue;
            }
            let Some(package) = self.by_id(id) else {
                continue;
            };

            let mut path = path;
            path.push(package.name.clone());

            if package.name == name || allowed.contains(package.name.as_str()) {
                for next in &package.resolved_deps {
                    queue.push_back((next.as_str(), path.clone()));
                }
            } else {
                // Do not descend: everything below is a consequence of this one edge.
                paths.push(path);
            }
        }

        paths.sort();
        paths
    }

    /// Parses the output of `cargo metadata --format-version 1`.
    ///
    /// # Errors
    ///
    /// Returns [`MetadataError`] if the JSON is malformed, or is missing the `packages`
    /// array, the `resolve` section, or `workspace_members`.
    ///
    /// A missing `resolve` section is an error rather than an empty graph: it is what
    /// `--no-deps` produces, and silently accepting it would turn every transitive rule
    /// into a no-op while the gate still reported success.
    pub fn from_cargo_metadata(json: &str) -> Result<Self, MetadataError> {
        let root: Value = serde_json::from_str(json)
            .map_err(|err| MetadataError::new(format!("invalid JSON: {err}")))?;

        let package_values = root
            .get("packages")
            .and_then(Value::as_array)
            .ok_or_else(|| MetadataError::new("metadata has no `packages` array"))?;

        if root
            .get("resolve")
            .and_then(|resolve| resolve.get("nodes"))
            .is_none()
        {
            return Err(MetadataError::new(
                "metadata has no `resolve.nodes`; the transitive rules need a resolved graph, so do not pass --no-deps",
            ));
        }
        let workspace_members = root
            .get("workspace_members")
            .and_then(Value::as_array)
            .ok_or_else(|| MetadataError::new("metadata has no `workspace_members` array"))?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();

        let resolved = resolved_edges(&root);

        let mut packages = Vec::with_capacity(package_values.len());
        for value in package_values {
            let id = string_field(value, "id")
                .ok_or_else(|| MetadataError::new("a package has no `id`"))?;
            let name = string_field(value, "name")
                .ok_or_else(|| MetadataError::new(format!("package {id} has no `name`")))?;

            let manifest_deps = value
                .get("dependencies")
                .and_then(Value::as_array)
                .map(|deps| {
                    deps.iter()
                        .filter_map(|dep| {
                            let name = string_field(dep, "name")?;
                            let kind =
                                DepKind::from_metadata(dep.get("kind").and_then(Value::as_str));
                            Some(ManifestDep { name, kind })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let default_features = value
                .get("features")
                .and_then(Value::as_object)
                .and_then(|features| features.get("default"))
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();

            let manifest_path = string_field(value, "manifest_path").map(PathBuf::from);
            let lib_source_path = lib_target_source(value);
            let source = string_field(value, "source");
            let has_build_script = has_target_kind(value, "custom-build");

            packages.push(Package {
                resolved_deps: resolved.get(&id).cloned().unwrap_or_default(),
                id,
                name,
                manifest_deps,
                default_features,
                manifest_path,
                lib_source_path,
                source,
                has_build_script,
            });
        }

        Ok(Self {
            packages,
            workspace_members,
        })
    }
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn resolved_edges(root: &Value) -> BTreeMap<PackageId, Vec<PackageId>> {
    let mut edges = BTreeMap::new();
    let Some(nodes) = root
        .get("resolve")
        .and_then(|resolve| resolve.get("nodes"))
        .and_then(Value::as_array)
    else {
        return edges;
    };

    for node in nodes {
        let Some(id) = string_field(node, "id") else {
            continue;
        };
        let deps = node
            .get("deps")
            .and_then(Value::as_array)
            .map(|deps| {
                deps.iter()
                    .filter_map(|dep| string_field(dep, "pkg"))
                    .collect()
            })
            .unwrap_or_default();
        edges.insert(id, deps);
    }

    edges
}

/// Every crate type that produces a library, and therefore has a crate root the
/// attribute rules must read.
///
/// Matching only `lib` and `rlib` would mean that `crate-type = ["staticlib"]` — an
/// entirely plausible choice for firmware exposing a C ABI — silently removed the crate
/// from the attribute check.
const LIBRARY_TARGET_KINDS: &[&str] =
    &["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"];

fn target_kinds(target: &Value) -> impl Iterator<Item = &str> {
    target
        .get("kind")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn has_target_kind(package: &Value, wanted: &str) -> bool {
    package
        .get("targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|target| target_kinds(target).any(|kind| kind == wanted))
}

fn lib_target_source(package: &Value) -> Option<PathBuf> {
    let targets = package.get("targets").and_then(Value::as_array)?;
    targets
        .iter()
        .find(|target| target_kinds(target).any(|kind| LIBRARY_TARGET_KINDS.contains(&kind)))
        .and_then(|target| string_field(target, "src_path"))
        .map(PathBuf::from)
}

/// Rule: every firmware crate may only reach the crates its layer allows.
///
/// Checks both the declared dependencies and the full transitive closure, so an
/// indirection through a third crate does not launder a forbidden edge.
#[must_use]
pub fn check_dependency_direction(graph: &PackageGraph) -> Vec<Violation> {
    let mut violations = Vec::new();

    for spec in LAYERS {
        let Some(package) = graph.find(spec.name) else {
            violations.push(Violation::new(
                "layer-missing",
                spec.name,
                "the workspace does not contain this crate",
            ));
            continue;
        };

        let allowed: BTreeSet<&str> = spec.allowed_dependencies().collect();

        for dep in &package.manifest_deps {
            if !allowed.contains(dep.name.as_str()) {
                violations.push(Violation::new(
                    "dependency-direction",
                    spec.name,
                    format!(
                        "declares `{}` in [{}]; this layer may only depend on {}. It must not own {}.",
                        dep.name,
                        dep.kind,
                        spec.render_allowed(),
                        spec.must_not_own
                    ),
                ));
            }
        }

        for path in graph.illegal_reach_paths(spec.name, &allowed) {
            // A path of two names is a direct edge, already reported above.
            if path.len() <= 2 {
                continue;
            }
            let Some(offender) = path.last() else {
                continue;
            };
            violations.push(Violation::new(
                "dependency-direction-transitive",
                spec.name,
                format!(
                    "reaches `{offender}` through {}; this layer may only depend on {}",
                    path.join(" -> "),
                    spec.render_allowed()
                ),
            ));
        }
    }

    violations
}

/// Rule: `waymaker-core` has zero dependencies of any kind.
///
/// This is stricter than the direction rule, which would be satisfied by an empty
/// allowlist. Stating it separately gives the failure its own name in CI output, and
/// covers dev- and build-dependencies explicitly.
#[must_use]
pub fn check_kernel_has_no_dependencies(graph: &PackageGraph) -> Vec<Violation> {
    let Some(kernel) = LAYERS.first() else {
        return Vec::new();
    };
    let Some(package) = graph.find(kernel.name) else {
        return Vec::new();
    };

    let mut violations: Vec<Violation> = package
        .manifest_deps
        .iter()
        .map(|dep| {
            Violation::new(
                "kernel-zero-dependencies",
                kernel.name,
                format!(
                    "declares `{}` in [{}]; the kernel is dependency-free by contract",
                    dep.name, dep.kind
                ),
            )
        })
        .collect();

    for path in graph.illegal_reach_paths(kernel.name, &BTreeSet::new()) {
        if path.len() <= 2 {
            continue;
        }
        let Some(offender) = path.last() else {
            continue;
        };
        violations.push(Violation::new(
            "kernel-zero-dependencies",
            kernel.name,
            format!(
                "reaches `{offender}` through {}; the kernel is dependency-free by contract",
                path.join(" -> ")
            ),
        ));
    }

    violations
}

/// Rule: nothing below the façade may see Embassy.
///
/// The direction rule already forbids it. This one exists so that the failure message
/// says "Embassy", which is the wording the layering contract uses.
#[must_use]
pub fn check_embassy_stays_above_flash(graph: &PackageGraph) -> Vec<Violation> {
    let mut violations = Vec::new();

    for spec in LAYERS {
        if spec.name == policy::EMBASSY_FACADE {
            continue;
        }
        for reached in graph.transitive_dependencies(spec.name) {
            if policy::is_embassy_package(&reached) {
                violations.push(Violation::new(
                    "embassy-below-facade",
                    spec.name,
                    format!(
                        "reaches Embassy crate `{reached}`; only `{}` may know about Embassy",
                        policy::EMBASSY_FACADE
                    ),
                ));
            }
        }
    }

    violations
}

/// Rule: every firmware crate has empty default features.
#[must_use]
pub fn check_empty_default_features(graph: &PackageGraph) -> Vec<Violation> {
    LAYERS
        .iter()
        .filter_map(|spec| graph.find(spec.name))
        .filter(|package| !package.default_features.is_empty())
        .map(|package| {
            Violation::new(
                "empty-default-features",
                package.name.clone(),
                format!(
                    "default feature enables {}; default features must be empty so every optional cost is opt-in",
                    package.default_features.join(", ")
                ),
            )
        })
        .collect()
}

/// Rule: every workspace member is either a firmware layer or declared host tooling.
///
/// Without this, a fourth crate added to the workspace is subject to no rule at all: it
/// can be `std`, depend on Embassy, and skip the workspace lints, because every other
/// rule iterates [`LAYERS`] and simply never looks at it.
#[must_use]
pub fn check_workspace_membership(graph: &PackageGraph) -> Vec<Violation> {
    graph
        .workspace_members()
        .iter()
        .filter_map(|id| {
            let Some(package) = graph.by_id(id) else {
                return Some(Violation::new(
                    "workspace-membership",
                    id.clone(),
                    "cargo reports this workspace member but it is not in the package list",
                ));
            };
            let known = policy::layer(&package.name).is_some()
                || policy::HOST_TOOLS.contains(&package.name.as_str());
            (!known).then(|| {
                Violation::new(
                    "workspace-membership",
                    package.name.clone(),
                    "is a workspace member but is neither a layer nor declared host tooling; add a row to policy::LAYERS or to policy::HOST_TOOLS",
                )
            })
        })
        .collect()
}

/// Rule: a crate with a layer's name is that layer, not a look-alike from a registry.
///
/// The allowlist matches on names. Without this rule, `waymaker-core = "0.1"` from
/// crates.io would satisfy `waymaker-flash`'s allowlist just as well as the path
/// dependency it is supposed to name.
#[must_use]
pub fn check_layers_are_local(graph: &PackageGraph) -> Vec<Violation> {
    LAYERS
        .iter()
        .filter_map(|spec| graph.find(spec.name))
        .filter_map(|package| {
            package.source.as_ref().map(|source| {
                Violation::new(
                    "layer-not-local",
                    package.name.clone(),
                    format!(
                        "resolves to `{source}` rather than a path in this workspace; the allowlist matches on names, so a look-alike would pass every other rule"
                    ),
                )
            })
        })
        .collect()
}

/// Rule: no firmware crate has a build script.
///
/// A `build.rs` needs no build-dependencies to run arbitrary host code on every build and
/// inject `cfg`s and environment into the crate it belongs to. In the kernel that is
/// hidden global state, which is exactly what the layering contract excludes.
#[must_use]
pub fn check_no_build_scripts(graph: &PackageGraph) -> Vec<Violation> {
    LAYERS
        .iter()
        .filter_map(|spec| graph.find(spec.name))
        .filter(|package| package.has_build_script)
        .map(|package| {
            Violation::new(
                "no-build-scripts",
                package.name.clone(),
                "has a build.rs; a firmware crate must not run host code at build time",
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legal_workspace() -> PackageGraph {
        PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash").with_dependency("waymaker-core", DepKind::Normal),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ])
    }

    fn rules(graph: &PackageGraph) -> Vec<Violation> {
        let mut all = check_dependency_direction(graph);
        all.extend(check_kernel_has_no_dependencies(graph));
        all.extend(check_embassy_stays_above_flash(graph));
        all.extend(check_empty_default_features(graph));
        all
    }

    /// A rule firing on the wrong crate is not the rule firing. Every assertion names
    /// the subject it expects.
    fn fired(violations: &[Violation], rule: &str, subject: &str) -> bool {
        violations
            .iter()
            .any(|violation| violation.rule == rule && violation.subject == subject)
    }

    #[test]
    fn a_legal_workspace_has_no_violations() {
        assert_eq!(rules(&legal_workspace()), Vec::new());
    }

    #[test]
    fn a_dependency_on_the_kernel_is_a_violation() {
        let mut graph = legal_workspace();
        let packages = graph.packages().to_vec();
        let mut packages = packages;
        packages[0] = Package::new("waymaker-core").with_dependency("serde", DepKind::Normal);
        packages.push(Package::new("serde"));
        graph = PackageGraph::new(packages);

        let violations = rules(&graph);
        assert!(fired(
            &violations,
            "kernel-zero-dependencies",
            "waymaker-core"
        ));
        assert!(fired(&violations, "dependency-direction", "waymaker-core"));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "dependency-direction" && v.detail.contains("serde")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_dev_dependency_on_the_kernel_is_still_a_violation() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core").with_dependency("proptest", DepKind::Development),
            Package::new("proptest"),
            Package::new("waymaker-flash").with_dependency("waymaker-core", DepKind::Normal),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);

        let violations = rules(&graph);
        assert!(fired(
            &violations,
            "kernel-zero-dependencies",
            "waymaker-core"
        ));
        assert!(
            violations
                .iter()
                .any(|violation| violation.rule == "kernel-zero-dependencies"
                    && violation.detail.contains("dev-dependencies")),
            "the message should name the table: {violations:?}"
        );
    }

    #[test]
    fn a_build_dependency_on_the_kernel_is_still_a_violation() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core").with_dependency("cc", DepKind::Build),
            Package::new("cc"),
            Package::new("waymaker-flash").with_dependency("waymaker-core", DepKind::Normal),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);

        let violations = rules(&graph);
        assert!(fired(
            &violations,
            "kernel-zero-dependencies",
            "waymaker-core"
        ));
        assert!(
            violations
                .iter()
                .any(|violation| violation.rule == "kernel-zero-dependencies"
                    && violation.detail.contains("build-dependencies")),
            "a build dependency must be reported as one: {violations:?}"
        );
    }

    #[test]
    fn flash_may_not_reach_embassy_directly() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("embassy-time", DepKind::Normal),
            Package::new("embassy-time"),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);

        let violations = rules(&graph);
        assert!(fired(&violations, "dependency-direction", "waymaker-flash"));
        assert!(fired(&violations, "embassy-below-facade", "waymaker-flash"));
    }

    #[test]
    fn flash_may_not_reach_embassy_through_another_crate() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("innocent-helper", DepKind::Normal),
            Package::new("innocent-helper").with_dependency("embassy-executor", DepKind::Normal),
            Package::new("embassy-executor"),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);

        let violations = rules(&graph);
        assert!(
            violations.iter().any(|violation| {
                violation.rule == "dependency-direction" && violation.subject == "waymaker-flash"
            }),
            "the edge that admitted Embassy must be named: {violations:?}"
        );
        assert!(
            violations.iter().any(|violation| {
                violation.rule == "embassy-below-facade"
                    && violation.subject == "waymaker-flash"
                    && violation.detail.contains("embassy-executor")
            }),
            "the Embassy crate itself must be named even though it is two hops away: {violations:?}"
        );
    }

    #[test]
    fn the_kernel_may_not_depend_on_the_layers_above_it() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core").with_dependency("waymaker-flash", DepKind::Normal),
            Package::new("waymaker-flash"),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);

        assert!(fired(
            &rules(&graph),
            "dependency-direction",
            "waymaker-core"
        ));
    }

    #[test]
    fn the_kernel_is_checked_for_embassy_too() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core").with_dependency("embassy-sync", DepKind::Normal),
            Package::new("embassy-sync"),
            Package::new("waymaker-flash").with_dependency("waymaker-core", DepKind::Normal),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);

        let violations = rules(&graph);
        assert!(
            fired(&violations, "embassy-below-facade", "waymaker-core"),
            "the kernel must be checked for Embassy, not only flash: {violations:?}"
        );
    }

    #[test]
    fn the_facade_may_depend_on_both_lower_layers_and_is_not_flagged_for_embassy() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash").with_dependency("waymaker-core", DepKind::Normal),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);
        assert!(check_embassy_stays_above_flash(&graph).is_empty());
    }

    #[test]
    fn a_missing_layer_is_reported() {
        let graph = PackageGraph::new(vec![Package::new("waymaker-core")]);
        let violations = rules(&graph);
        let missing: Vec<&str> = violations
            .iter()
            .filter(|violation| violation.rule == "layer-missing")
            .map(|violation| violation.subject.as_str())
            .collect();
        assert_eq!(missing, ["waymaker-flash", "waymaker-embassy"]);
    }

    #[test]
    fn non_empty_default_features_are_reported() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core").with_default_features(&["std"]),
            Package::new("waymaker-flash").with_dependency("waymaker-core", DepKind::Normal),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);

        assert!(fired(
            &rules(&graph),
            "empty-default-features",
            "waymaker-core"
        ));
        assert!(
            rules(&graph)
                .iter()
                .any(|v| v.rule == "empty-default-features" && v.detail.contains("std")),
            "the offending feature must be named"
        );
    }

    #[test]
    fn every_layer_is_checked_for_default_features() {
        for spec in LAYERS {
            let graph = PackageGraph::new(vec![
                Package::new(spec.name).with_default_features(&["std"]),
            ]);
            let violations = check_empty_default_features(&graph);
            assert_eq!(violations.len(), 1, "{} is not checked", spec.name);
            assert_eq!(violations[0].subject, spec.name);
        }
    }

    #[test]
    fn the_kernel_reports_a_transitive_dependency_with_its_path() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core").with_dependency("direct", DepKind::Normal),
            Package::new("direct").with_dependency("indirect", DepKind::Normal),
            Package::new("indirect"),
        ]);

        let violations = check_kernel_has_no_dependencies(&graph);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].detail.contains("declares `direct`"),
            "the direct edge is the actionable one: {}",
            violations[0].detail
        );

        // With the direct edge removed the transitive branch is the one that reports.
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core").with_dependency("waymaker-core-shim", DepKind::Normal),
            Package::new("waymaker-core-shim").with_dependency("indirect", DepKind::Normal),
            Package::new("indirect"),
        ]);
        let violations = check_kernel_has_no_dependencies(&graph);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].detail.contains("waymaker-core-shim"));
    }

    #[test]
    fn the_kernel_allowlist_renders_as_nothing() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core").with_dependency("serde", DepKind::Normal),
        ]);
        let violations = check_dependency_direction(&graph);
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("may only depend on nothing")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_illegal_transitive_reach_is_reported_once_at_the_edge_that_admitted_it() {
        // waymaker-embassy -> waymaker-flash (allowed) -> innocent-helper (not allowed)
        // -> embassy-executor -> junk-a, junk-b
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("innocent-helper", DepKind::Normal),
            Package::new("innocent-helper").with_dependency("embassy-executor", DepKind::Normal),
            Package::new("embassy-executor")
                .with_dependency("junk-a", DepKind::Normal)
                .with_dependency("junk-b", DepKind::Normal),
            Package::new("junk-a"),
            Package::new("junk-b"),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);

        let violations = check_dependency_direction(&graph);
        let reported: Vec<&Violation> = violations
            .iter()
            .filter(|violation| {
                violation.rule == "dependency-direction-transitive"
                    && violation.subject == "waymaker-embassy"
            })
            .collect();

        assert_eq!(
            reported.len(),
            1,
            "the subtree below the offending edge must not be enumerated: {reported:?}"
        );
        assert!(
            reported[0]
                .detail
                .contains("waymaker-embassy -> waymaker-flash -> innocent-helper"),
            "the message must name the path that admitted it: {}",
            reported[0].detail
        );
        assert!(
            !reported[0].detail.contains("junk-a"),
            "consequences of the offending edge are noise: {}",
            reported[0].detail
        );
    }

    #[test]
    fn a_direct_illegal_dependency_is_not_also_reported_as_transitive() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("embassy-time", DepKind::Normal),
            Package::new("embassy-time"),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
        ]);

        let violations = check_dependency_direction(&graph);
        let flash: Vec<&Violation> = violations
            .iter()
            .filter(|violation| violation.subject == "waymaker-flash")
            .collect();

        assert_eq!(flash.len(), 1, "one edge, one violation: {flash:?}");
        assert_eq!(flash[0].rule, "dependency-direction");
    }

    #[test]
    fn illegal_reach_paths_takes_the_shortest_route() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-flash")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("detour", DepKind::Normal),
            Package::new("waymaker-core").with_dependency("banned", DepKind::Normal),
            Package::new("detour").with_dependency("banned", DepKind::Normal),
            Package::new("banned"),
        ]);

        let allowed = core::iter::once("waymaker-core").collect();
        let paths = graph.illegal_reach_paths("waymaker-flash", &allowed);

        assert_eq!(
            paths.len(),
            2,
            "detour and banned are both illegal: {paths:?}"
        );
        assert!(paths.contains(&vec![
            "waymaker-flash".to_owned(),
            "waymaker-core".to_owned(),
            "banned".to_owned()
        ]));
    }

    #[test]
    fn illegal_reach_paths_terminates_on_a_cycle() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core").with_dependency("a", DepKind::Normal),
            Package::new("a").with_dependency("b", DepKind::Normal),
            Package::new("b").with_dependency("a", DepKind::Normal),
        ]);

        let paths = graph.illegal_reach_paths("waymaker-core", &BTreeSet::new());
        assert_eq!(
            paths,
            vec![vec!["waymaker-core".to_owned(), "a".to_owned()]]
        );
    }

    #[test]
    fn a_dependency_cycle_does_not_hang_the_closure() {
        let graph = PackageGraph::new(vec![
            Package::new("a").with_dependency("b", DepKind::Normal),
            Package::new("b").with_dependency("a", DepKind::Normal),
        ]);

        let reached = graph.transitive_dependencies("a");
        assert_eq!(reached, core::iter::once("b".to_owned()).collect());
    }

    const METADATA: &str = r#"{
      "packages": [
        {
          "id": "path+file:///w/core#waymaker-core@0.0.0",
          "name": "waymaker-core",
          "source": null,
          "manifest_path": "/w/core/Cargo.toml",
          "dependencies": [],
          "features": {},
          "targets": [{ "kind": ["lib"], "src_path": "/w/core/src/lib.rs" }]
        },
        {
          "id": "path+file:///w/flash#waymaker-flash@0.0.0",
          "name": "waymaker-flash",
          "source": null,
          "manifest_path": "/w/flash/Cargo.toml",
          "dependencies": [
            { "name": "waymaker-core", "kind": null },
            { "name": "proptest", "kind": "dev" },
            { "name": "cc", "kind": "build" }
          ],
          "features": { "default": ["std"], "std": [] },
          "targets": [
            { "kind": ["custom-build"], "src_path": "/w/flash/build.rs" },
            { "kind": ["staticlib"], "src_path": "/w/flash/src/lib.rs" }
          ]
        },
        {
          "id": "registry+https://github.com/rust-lang/crates.io-index#proptest@1.0.0",
          "name": "proptest",
          "source": "registry+https://github.com/rust-lang/crates.io-index",
          "manifest_path": "/reg/proptest/Cargo.toml",
          "dependencies": [],
          "features": {},
          "targets": [{ "kind": ["lib"], "src_path": "/reg/proptest/src/lib.rs" }]
        }
      ],
      "workspace_members": [
        "path+file:///w/core#waymaker-core@0.0.0",
        "path+file:///w/flash#waymaker-flash@0.0.0"
      ],
      "resolve": {
        "nodes": [
          { "id": "path+file:///w/core#waymaker-core@0.0.0", "deps": [] },
          {
            "id": "path+file:///w/flash#waymaker-flash@0.0.0",
            "deps": [
              { "pkg": "path+file:///w/core#waymaker-core@0.0.0" },
              { "pkg": "registry+https://github.com/rust-lang/crates.io-index#proptest@1.0.0" }
            ]
          },
          {
            "id": "registry+https://github.com/rust-lang/crates.io-index#proptest@1.0.0",
            "deps": []
          }
        ]
      }
    }"#;

    #[test]
    fn cargo_metadata_is_parsed_into_a_graph() {
        let graph = PackageGraph::from_cargo_metadata(METADATA).expect("metadata should parse");

        let core = graph.find("waymaker-core").expect("core should be present");
        assert!(core.manifest_deps.is_empty());
        assert_eq!(
            core.lib_source_path.as_deref(),
            Some(std::path::Path::new("/w/core/src/lib.rs"))
        );
        assert_eq!(core.source, None, "a path package has no source");
        assert!(!core.has_build_script);

        let flash = graph
            .find("waymaker-flash")
            .expect("flash should be present");
        assert_eq!(flash.manifest_deps.len(), 3);
        assert_eq!(flash.manifest_deps[1].kind, DepKind::Development);
        assert_eq!(
            flash.manifest_deps[2].kind,
            DepKind::Build,
            "a build dependency must be parsed as one"
        );
        assert_eq!(flash.default_features, ["std".to_owned()]);
        assert!(flash.has_build_script, "build.rs must be detected");
        assert_eq!(
            flash.lib_source_path.as_deref(),
            Some(std::path::Path::new("/w/flash/src/lib.rs")),
            "a staticlib is still a crate root the attribute rules must read"
        );

        let proptest = graph.find("proptest").expect("proptest should be present");
        assert!(proptest.source.is_some(), "a registry package has a source");

        assert_eq!(graph.workspace_members().len(), 2);
    }

    #[test]
    fn every_library_crate_type_yields_a_crate_root() {
        for kind in LIBRARY_TARGET_KINDS {
            let json = format!(
                r#"{{"packages":[{{"id":"a","name":"a","dependencies":[],"features":{{}},
                   "targets":[{{"kind":["{kind}"],"src_path":"/a/src/lib.rs"}}]}}],
                   "workspace_members":["a"],"resolve":{{"nodes":[{{"id":"a","deps":[]}}]}}}}"#
            );
            let graph = PackageGraph::from_cargo_metadata(&json).expect("should parse");
            let package = graph.find("a").expect("present");
            assert!(
                package.lib_source_path.is_some(),
                "crate-type {kind} must not remove a crate from the attribute check"
            );
        }
    }

    #[test]
    fn a_binary_only_target_is_not_a_crate_root() {
        let json = r#"{"packages":[{"id":"a","name":"a","dependencies":[],"features":{},
            "targets":[{"kind":["bin"],"src_path":"/a/src/main.rs"}]}],
            "workspace_members":["a"],"resolve":{"nodes":[{"id":"a","deps":[]}]}}"#;
        let graph = PackageGraph::from_cargo_metadata(json).expect("should parse");
        assert!(
            graph.find("a").expect("present").lib_source_path.is_none(),
            "a binary has no library root"
        );
    }

    #[test]
    fn dependency_kinds_are_parsed_from_metadata() {
        assert_eq!(DepKind::from_metadata(None), DepKind::Normal);
        assert_eq!(DepKind::from_metadata(Some("dev")), DepKind::Development);
        assert_eq!(DepKind::from_metadata(Some("build")), DepKind::Build);
        assert_eq!(DepKind::Normal.to_string(), "dependencies");
        assert_eq!(DepKind::Development.to_string(), "dev-dependencies");
        assert_eq!(DepKind::Build.to_string(), "build-dependencies");
    }

    #[test]
    fn malformed_metadata_is_an_error_not_a_pass() {
        assert!(PackageGraph::from_cargo_metadata("not json").is_err());
        assert!(PackageGraph::from_cargo_metadata("{}").is_err());
    }

    #[test]
    fn metadata_without_a_resolved_graph_is_an_error_not_a_pass() {
        // `--no-deps` produces this. Accepting it would turn every transitive rule into
        // a no-op while the gate still reported success.
        let json = r#"{"packages":[{"id":"a","name":"a","dependencies":[],"features":{},
            "targets":[]}],"workspace_members":["a"]}"#;
        let error = PackageGraph::from_cargo_metadata(json)
            .expect_err("a metadata document with no resolve section must be rejected");
        assert!(error.to_string().contains("--no-deps"), "{error}");
    }

    #[test]
    fn metadata_without_workspace_members_is_an_error_not_a_pass() {
        let json = r#"{"packages":[],"resolve":{"nodes":[]}}"#;
        assert!(PackageGraph::from_cargo_metadata(json).is_err());
    }

    #[test]
    fn a_workspace_member_that_is_not_a_layer_is_reported() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash").with_dependency("waymaker-core", DepKind::Normal),
            Package::new("waymaker-embassy")
                .with_dependency("waymaker-core", DepKind::Normal)
                .with_dependency("waymaker-flash", DepKind::Normal),
            Package::new("waymaker-time"),
        ])
        .with_workspace_members(&[
            "waymaker-core",
            "waymaker-flash",
            "waymaker-embassy",
            "waymaker-time",
        ]);

        let violations = check_workspace_membership(&graph);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].subject, "waymaker-time");
        assert_eq!(violations[0].rule, "workspace-membership");
    }

    #[test]
    fn layers_and_declared_host_tools_are_accepted_as_members() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash"),
            Package::new("waymaker-embassy"),
            Package::new("xtask"),
        ])
        .with_workspace_members(&[
            "waymaker-core",
            "waymaker-flash",
            "waymaker-embassy",
            "xtask",
        ]);

        assert!(check_workspace_membership(&graph).is_empty());
    }

    #[test]
    fn a_layer_resolved_from_a_registry_is_reported() {
        let graph = PackageGraph::new(vec![
            Package::new("waymaker-core"),
            Package::new("waymaker-flash").from_source("registry+https://crates.io"),
            Package::new("waymaker-embassy"),
        ]);

        let violations = check_layers_are_local(&graph);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].subject, "waymaker-flash");
        assert_eq!(violations[0].rule, "layer-not-local");
    }

    #[test]
    fn path_resolved_layers_are_accepted() {
        assert!(check_layers_are_local(&legal_workspace()).is_empty());
    }

    #[test]
    fn a_build_script_in_a_firmware_crate_is_reported() {
        for spec in LAYERS {
            let graph = PackageGraph::new(vec![Package::new(spec.name).with_build_script()]);
            let violations = check_no_build_scripts(&graph);
            assert_eq!(violations.len(), 1, "{} is not checked", spec.name);
            assert_eq!(violations[0].subject, spec.name);
            assert_eq!(violations[0].rule, "no-build-scripts");
        }
    }

    #[test]
    fn a_workspace_without_build_scripts_passes() {
        assert!(check_no_build_scripts(&legal_workspace()).is_empty());
    }
}
