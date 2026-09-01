//! Rules over manifest text.
//!
//! `cargo metadata` says nothing about profiles or lint tables, so these rules read the
//! TOML directly. They exist because a release profile that quietly drifts from the design
//! document, or a `pedantic` lint group that silently outranks `unwrap_used`, is exactly
//! the kind of regression nobody notices in review.

use toml::Value;

use crate::Violation;
use crate::policy::LAYERS;

/// The release profile from §04 of the design document, as `(key, TOML value)` pairs.
pub const REQUIRED_RELEASE_PROFILE: &[(&str, &str)] = &[
    ("opt-level", "\"z\""),
    ("lto", "\"fat\""),
    ("codegen-units", "1"),
    ("panic", "\"abort\""),
    ("strip", "\"symbols\""),
];

/// Lint groups that must be enabled workspace-wide with a negative priority.
///
/// Without `priority = -1` a group set at the same priority as an individual lint can
/// override it, which would silently un-deny `clippy::unwrap_used`.
pub const REQUIRED_CLIPPY_GROUPS: &[&str] = &["pedantic", "nursery"];

/// Individual lints that must be denied workspace-wide.
pub const REQUIRED_CLIPPY_DENIALS: &[&str] = &["unwrap_used"];

/// Rule: the release profile matches the design document exactly.
#[must_use]
pub fn check_release_profile(manifest: &str) -> Vec<Violation> {
    let Some(document) = parse(manifest) else {
        return vec![Violation::new(
            "release-profile",
            "Cargo.toml",
            "the workspace manifest is not valid TOML",
        )];
    };

    let profile = document
        .get("profile")
        .and_then(|profile| profile.get("release"));

    let Some(profile) = profile else {
        return vec![Violation::new(
            "release-profile",
            "Cargo.toml",
            "there is no [profile.release] section; the size budgets depend on it",
        )];
    };

    REQUIRED_RELEASE_PROFILE
        .iter()
        .filter_map(|(key, expected)| {
            let expected_value = expected.parse::<Value>().ok()?;
            match profile.get(*key) {
                Some(actual) if *actual == expected_value => None,
                Some(actual) => Some(Violation::new(
                    "release-profile",
                    "Cargo.toml",
                    format!("[profile.release] {key} is {actual}, expected {expected}"),
                )),
                None => Some(Violation::new(
                    "release-profile",
                    "Cargo.toml",
                    format!("[profile.release] is missing {key} = {expected}"),
                )),
            }
        })
        .collect()
}

/// Rule: the workspace lint table denies what the project says it denies.
#[must_use]
pub fn check_workspace_lints(manifest: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let Some(document) = parse(manifest) else {
        violations.push(Violation::new(
            "workspace-lints",
            "Cargo.toml",
            "the workspace manifest is not valid TOML",
        ));
        return violations;
    };

    let lints = document
        .get("workspace")
        .and_then(|workspace| workspace.get("lints"));

    let Some(lints) = lints else {
        violations.push(Violation::new(
            "workspace-lints",
            "Cargo.toml",
            "there is no [workspace.lints] table",
        ));
        return violations;
    };

    let clippy = lints.get("clippy");
    for group in REQUIRED_CLIPPY_GROUPS {
        let entry = clippy.and_then(|clippy| clippy.get(*group));
        match entry {
            None => violations.push(Violation::new(
                "workspace-lints",
                format!("clippy::{group}"),
                "the lint group is not enabled workspace-wide",
            )),
            Some(entry) => {
                if !matches!(lint_level(entry), Some("warn" | "deny" | "forbid")) {
                    violations.push(Violation::new(
                        "workspace-lints",
                        format!("clippy::{group}"),
                        "the lint group must be set to warn, deny, or forbid",
                    ));
                }
                if lint_priority(entry) >= 0 {
                    violations.push(Violation::new(
                        "workspace-lints",
                        format!("clippy::{group}"),
                        "the lint group needs priority = -1 so individual lints outrank it",
                    ));
                }
            }
        }
    }

    for lint in REQUIRED_CLIPPY_DENIALS {
        let level = clippy
            .and_then(|clippy| clippy.get(*lint))
            .and_then(lint_level);
        if !matches!(level, Some("deny" | "forbid")) {
            violations.push(Violation::new(
                "workspace-lints",
                format!("clippy::{lint}"),
                "must be denied workspace-wide (tests are exempted through clippy.toml)",
            ));
        }
    }

    let unsafe_code = lints
        .get("rust")
        .and_then(|rust| rust.get("unsafe_code"))
        .and_then(lint_level);
    if !matches!(unsafe_code, Some("deny" | "forbid")) {
        violations.push(Violation::new(
            "workspace-lints",
            "unsafe_code",
            "must be forbidden workspace-wide; documented exceptions belong in an ADR",
        ));
    }

    violations
}

/// Rule: every firmware crate inherits the workspace lints and declares no default
/// features.
#[must_use]
pub fn check_member_manifest(name: &str, manifest: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let Some(document) = parse(manifest) else {
        violations.push(Violation::new(
            "member-manifest",
            name.to_owned(),
            "the manifest is not valid TOML",
        ));
        return violations;
    };

    let inherits_lints = document
        .get("lints")
        .and_then(|lints| lints.get("workspace"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !inherits_lints {
        violations.push(Violation::new(
            "member-manifest",
            name.to_owned(),
            "needs `[lints] workspace = true`, or the workspace lint table does not apply",
        ));
    }

    let default_feature = document
        .get("features")
        .and_then(|features| features.get("default"))
        .and_then(Value::as_array);
    if default_feature.is_some_and(|entries| !entries.is_empty()) {
        violations.push(Violation::new(
            "member-manifest",
            name.to_owned(),
            "declares a non-empty `default` feature; default features must be empty",
        ));
    }

    if LAYERS
        .first()
        .is_some_and(|kernel| kernel.name == name)
    {
        for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if document
                .get(table)
                .and_then(Value::as_table)
                .is_some_and(|entries| !entries.is_empty())
            {
                violations.push(Violation::new(
                    "member-manifest",
                    name.to_owned(),
                    format!("declares [{table}]; the kernel is dependency-free by contract"),
                ));
            }
        }
    }

    violations
}

/// Parses a whole manifest document.
///
/// `Value`'s own `FromStr` parses a single TOML *value*, not a document, so documents go
/// through `Table` and are re-wrapped for uniform `get` access.
fn parse(manifest: &str) -> Option<Value> {
    manifest.parse::<toml::Table>().map(Value::Table).ok()
}

fn lint_level(entry: &Value) -> Option<&str> {
    entry
        .as_str()
        .or_else(|| entry.get("level").and_then(Value::as_str))
}

fn lint_priority(entry: &Value) -> i64 {
    entry
        .get("priority")
        .and_then(Value::as_integer)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_WORKSPACE: &str = r#"
[workspace]
resolver = "3"
members = ["crates/waymaker-core"]

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

    fn details(violations: &[Violation]) -> String {
        violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_documented_release_profile_passes() {
        let violations = check_release_profile(GOOD_WORKSPACE);
        assert!(violations.is_empty(), "{}", details(&violations));
    }

    #[test]
    fn a_missing_release_profile_is_reported() {
        assert!(!check_release_profile("[workspace]\nmembers = []\n").is_empty());
    }

    #[test]
    fn each_drifted_release_profile_key_is_named() {
        let drifted = GOOD_WORKSPACE.replace("opt-level = \"z\"", "opt-level = 3");
        let violations = check_release_profile(&drifted);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].detail.contains("opt-level"));
    }

    #[test]
    fn a_dropped_release_profile_key_is_named() {
        let dropped = GOOD_WORKSPACE.replace("panic = \"abort\"\n", "");
        let violations = check_release_profile(&dropped);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].detail.contains("panic"));
    }

    #[test]
    fn the_documented_lint_table_passes() {
        let violations = check_workspace_lints(GOOD_WORKSPACE);
        assert!(violations.is_empty(), "{}", details(&violations));
    }

    #[test]
    fn a_missing_lint_group_is_reported() {
        let without = GOOD_WORKSPACE.replace("nursery = { level = \"warn\", priority = -1 }\n", "");
        let violations = check_workspace_lints(&without);
        assert!(violations.iter().any(|v| v.subject == "clippy::nursery"));
    }

    #[test]
    fn a_lint_group_without_negative_priority_is_reported() {
        let flat = GOOD_WORKSPACE.replace(
            "pedantic = { level = \"warn\", priority = -1 }",
            "pedantic = \"warn\"",
        );
        let violations = check_workspace_lints(&flat);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "clippy::pedantic" && v.detail.contains("priority")),
            "{}",
            details(&violations)
        );
    }

    #[test]
    fn an_allowed_unwrap_is_reported() {
        let allowed = GOOD_WORKSPACE.replace("unwrap_used = \"deny\"", "unwrap_used = \"allow\"");
        let violations = check_workspace_lints(&allowed);
        assert!(violations.iter().any(|v| v.subject == "clippy::unwrap_used"));
    }

    #[test]
    fn permitted_unsafe_code_is_reported() {
        let allowed = GOOD_WORKSPACE.replace("unsafe_code = \"forbid\"", "unsafe_code = \"allow\"");
        let violations = check_workspace_lints(&allowed);
        assert!(violations.iter().any(|v| v.subject == "unsafe_code"));
    }

    #[test]
    fn a_member_that_inherits_the_lints_passes() {
        let manifest = "[package]\nname = \"waymaker-flash\"\n\n[lints]\nworkspace = true\n";
        assert!(check_member_manifest("waymaker-flash", manifest).is_empty());
    }

    #[test]
    fn a_member_that_ignores_the_workspace_lints_is_reported() {
        let manifest = "[package]\nname = \"waymaker-flash\"\n";
        let violations = check_member_manifest("waymaker-flash", manifest);
        assert!(violations.iter().any(|v| v.detail.contains("workspace = true")));
    }

    #[test]
    fn a_member_with_a_non_empty_default_feature_is_reported() {
        let manifest = "[package]\nname = \"waymaker-flash\"\n\n[lints]\nworkspace = true\n\n[features]\ndefault = [\"std\"]\nstd = []\n";
        let violations = check_member_manifest("waymaker-flash", manifest);
        assert!(violations.iter().any(|v| v.detail.contains("default")));
    }

    #[test]
    fn an_empty_default_feature_is_allowed() {
        let manifest = "[package]\nname = \"waymaker-flash\"\n\n[lints]\nworkspace = true\n\n[features]\ndefault = []\n";
        assert!(check_member_manifest("waymaker-flash", manifest).is_empty());
    }

    #[test]
    fn a_dependency_table_in_the_kernel_manifest_is_reported() {
        let manifest = "[package]\nname = \"waymaker-core\"\n\n[lints]\nworkspace = true\n\n[dev-dependencies]\nproptest = \"1\"\n";
        let violations = check_member_manifest("waymaker-core", manifest);
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("dev-dependencies")),
            "{}",
            details(&violations)
        );
    }
}
