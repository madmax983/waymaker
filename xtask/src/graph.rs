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
        }
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
        self.default_features = features.iter().map(|feature| (*feature).to_owned()).collect();
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
}

impl PackageGraph {
    /// Builds a graph from an explicit package list.
    #[must_use]
    pub fn new(packages: Vec<Package>) -> Self {
        Self { packages }
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
        let mut queue: VecDeque<&str> = root
            .resolved_deps
            .iter()
            .map(String::as_str)
            .collect();

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

    /// Parses the output of `cargo metadata --format-version 1`.
    ///
    /// # Errors
    ///
    /// Returns [`MetadataError`] if the JSON is malformed or is missing the `packages`
    /// array. A missing `resolve` section is tolerated (it is absent under `--no-deps`)
    /// and leaves every package's resolved edges empty.
    pub fn from_cargo_metadata(json: &str) -> Result<Self, MetadataError> {
        let root: Value = serde_json::from_str(json)
            .map_err(|err| MetadataError::new(format!("invalid JSON: {err}")))?;

        let package_values = root
            .get("packages")
            .and_then(Value::as_array)
            .ok_or_else(|| MetadataError::new("metadata has no `packages` array"))?;

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
                            let kind = DepKind::from_metadata(
                                dep.get("kind").and_then(Value::as_str),
                            );
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

            packages.push(Package {
                resolved_deps: resolved.get(&id).cloned().unwrap_or_default(),
                id,
                name,
                manifest_deps,
                default_features,
                manifest_path,
                lib_source_path,
            });
        }

        Ok(Self { packages })
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

fn lib_target_source(package: &Value) -> Option<PathBuf> {
    let targets = package.get("targets").and_then(Value::as_array)?;
    targets
        .iter()
        .find(|target| {
            target
                .get("kind")
                .and_then(Value::as_array)
                .is_some_and(|kinds| {
                    kinds
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|kind| kind == "lib" || kind == "rlib")
                })
        })
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

        let allowed: BTreeSet<&str> = spec.may_depend_on.iter().copied().collect();

        for dep in &package.manifest_deps {
            if !allowed.contains(dep.name.as_str()) {
                violations.push(Violation::new(
                    "dependency-direction",
                    spec.name,
                    format!(
                        "declares `{}` in [{}]; this layer may only depend on {}. It must not own {}.",
                        dep.name,
                        dep.kind,
                        render_allowed(spec.may_depend_on),
                        spec.must_not_own
                    ),
                ));
            }
        }

        for reached in graph.transitive_dependencies(spec.name) {
            if !allowed.contains(reached.as_str()) {
                violations.push(Violation::new(
                    "dependency-direction-transitive",
                    spec.name,
                    format!(
                        "reaches `{reached}` through its dependency graph; this layer may only depend on {}",
                        render_allowed(spec.may_depend_on)
                    ),
                ));
            }
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

    for reached in graph.transitive_dependencies(kernel.name) {
        violations.push(Violation::new(
            "kernel-zero-dependencies",
            kernel.name,
            format!("reaches `{reached}`; the kernel is dependency-free by contract"),
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

fn render_allowed(allowed: &[&str]) -> String {
    if allowed.is_empty() {
        "nothing".to_owned()
    } else {
        allowed.join(", ")
    }
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

    fn fired(violations: &[Violation], rule: &str) -> bool {
        violations.iter().any(|violation| violation.rule == rule)
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
        assert!(fired(&violations, "kernel-zero-dependencies"));
        assert!(fired(&violations, "dependency-direction"));
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
        assert!(fired(&violations, "kernel-zero-dependencies"));
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("dev-dependencies")),
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

        assert!(fired(&rules(&graph), "kernel-zero-dependencies"));
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
        assert!(fired(&violations, "dependency-direction"));
        assert!(fired(&violations, "embassy-below-facade"));
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
            fired(&violations, "dependency-direction-transitive"),
            "the transitive edge must be caught: {violations:?}"
        );
        assert!(fired(&violations, "embassy-below-facade"));
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

        assert!(fired(&rules(&graph), "dependency-direction"));
    }

    #[test]
    fn the_facade_may_depend_on_both_lower_layers() {
        let violations = rules(&legal_workspace());
        assert!(!fired(&violations, "embassy-below-facade"));
    }

    #[test]
    fn a_missing_layer_is_reported() {
        let graph = PackageGraph::new(vec![Package::new("waymaker-core")]);
        let violations = rules(&graph);
        assert!(fired(&violations, "layer-missing"));
        assert_eq!(
            violations
                .iter()
                .filter(|violation| violation.rule == "layer-missing")
                .count(),
            2
        );
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

        assert!(fired(&rules(&graph), "empty-default-features"));
    }

    #[test]
    fn a_dependency_cycle_does_not_hang_the_closure() {
        let graph = PackageGraph::new(vec![
            Package::new("a").with_dependency("b", DepKind::Normal),
            Package::new("b").with_dependency("a", DepKind::Normal),
        ]);

        let reached = graph.transitive_dependencies("a");
        assert_eq!(reached, ["b".to_owned()].into_iter().collect());
    }

    const METADATA: &str = r#"{
      "packages": [
        {
          "id": "path+file:///w/core#waymaker-core@0.0.0",
          "name": "waymaker-core",
          "manifest_path": "/w/core/Cargo.toml",
          "dependencies": [],
          "features": {},
          "targets": [{ "kind": ["lib"], "src_path": "/w/core/src/lib.rs" }]
        },
        {
          "id": "path+file:///w/flash#waymaker-flash@0.0.0",
          "name": "waymaker-flash",
          "manifest_path": "/w/flash/Cargo.toml",
          "dependencies": [
            { "name": "waymaker-core", "kind": null },
            { "name": "proptest", "kind": "dev" }
          ],
          "features": { "default": ["std"], "std": [] },
          "targets": [{ "kind": ["lib"], "src_path": "/w/flash/src/lib.rs" }]
        }
      ],
      "resolve": {
        "nodes": [
          { "id": "path+file:///w/core#waymaker-core@0.0.0", "deps": [] },
          {
            "id": "path+file:///w/flash#waymaker-flash@0.0.0",
            "deps": [{ "pkg": "path+file:///w/core#waymaker-core@0.0.0" }]
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

        let flash = graph.find("waymaker-flash").expect("flash should be present");
        assert_eq!(flash.manifest_deps.len(), 2);
        assert_eq!(flash.manifest_deps[1].kind, DepKind::Development);
        assert_eq!(flash.default_features, ["std".to_owned()]);
        assert_eq!(
            graph.transitive_dependencies("waymaker-flash"),
            ["waymaker-core".to_owned()].into_iter().collect()
        );
    }

    #[test]
    fn malformed_metadata_is_an_error_not_a_pass() {
        assert!(PackageGraph::from_cargo_metadata("not json").is_err());
        assert!(PackageGraph::from_cargo_metadata("{}").is_err());
    }
}
