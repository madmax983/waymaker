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

/// The cargo alias every gate in this repository is invoked through.
///
/// `cargo xtask check-layering` and `cargo xtask coverage` are the two gates, and both go
/// through this alias in CI, in the hook, and in the README. Editing the alias — appending
/// `--help`, pointing it at another package — turns every one of those invocations into a
/// no-op that exits zero, from a file no other rule reads. So the alias is pinned like the
/// release profile is.
pub const REQUIRED_XTASK_ALIAS: &str = "run --quiet --package xtask --";

/// Rule: the release profile matches the design document exactly.
#[must_use]
pub fn check_release_profile(manifest: &str) -> Vec<Violation> {
    check_release_profile_against(REQUIRED_RELEASE_PROFILE, manifest)
}

/// [`check_release_profile`] against an explicit specification.
///
/// Taking the specification as a parameter is what makes the "the gate itself is broken"
/// branch reachable from a test.
#[must_use]
pub fn check_release_profile_against(
    specification: &[(&str, &str)],
    manifest: &str,
) -> Vec<Violation> {
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

    specification
        .iter()
        .filter_map(|(key, expected)| {
            // A gate must never be able to silently uncheck one of its own rules: an
            // expected literal that does not parse is a bug in the gate, reported as one.
            let Ok(expected_value) = expected.parse::<Value>() else {
                return Some(Violation::new(
                    "gate-broken",
                    "REQUIRED_RELEASE_PROFILE",
                    format!("the expected value for {key} is not valid TOML: {expected}"),
                ));
            };
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

/// Rule: `.cargo/config.toml` changes nothing about how the workspace builds or is gated.
///
/// This file is read by every cargo invocation and by no other rule, which makes it the
/// quietest place in the repository to disable something. A `[profile.*]` table overrides
/// the manifest, so four lines here defeat the size budget the release profile exists to
/// protect. An `[env]` table sets environment variables for every build, including the ones
/// `cargo llvm-cov` reads to decide what to measure. `[build] rustflags` changes what every
/// crate is compiled with. And the `xtask` alias is how both gates are invoked, so a
/// rewritten alias turns them into commands that exit zero.
///
/// Banning them outright is simpler and more honest than reimplementing cargo's precedence.
#[must_use]
pub fn check_cargo_config(config: Option<&str>) -> Vec<Violation> {
    let Some(config) = config else {
        return vec![Violation::new(
            "cargo-config-profile",
            ".cargo/config.toml",
            format!("is missing, so `cargo xtask` is not aliased to `{REQUIRED_XTASK_ALIAS}`"),
        )];
    };
    let Some(document) = parse(config) else {
        return vec![Violation::new(
            "cargo-config-profile",
            ".cargo/config.toml",
            "is not valid TOML",
        )];
    };

    let mut violations = Vec::new();

    if let Some(profiles) = document.get("profile").and_then(Value::as_table) {
        for name in profiles.keys() {
            violations.push(Violation::new(
                "cargo-config-profile",
                ".cargo/config.toml",
                format!(
                    "declares [profile.{name}], which silently overrides the release profile in Cargo.toml; profiles belong in the workspace manifest"
                ),
            ));
        }
    }

    if let Some(variables) = document.get("env").and_then(Value::as_table) {
        for name in variables.keys() {
            violations.push(Violation::new(
                "cargo-config-profile",
                ".cargo/config.toml",
                format!(
                    "declares [env] {name}, which is set for every cargo invocation including the coverage run; environment that changes what is measured does not belong in a tracked file"
                ),
            ));
        }
    }

    if document
        .get("build")
        .and_then(|build| build.get("rustflags"))
        .is_some()
    {
        violations.push(Violation::new(
            "cargo-config-profile",
            ".cargo/config.toml",
            "declares [build] rustflags, which changes how every crate in the workspace is compiled",
        ));
    }

    let alias = document
        .get("alias")
        .and_then(|alias| alias.get("xtask"))
        .and_then(Value::as_str);
    if alias != Some(REQUIRED_XTASK_ALIAS) {
        violations.push(Violation::new(
            "cargo-config-profile",
            ".cargo/config.toml",
            format!(
                "aliases `cargo xtask` to {}, expected `{REQUIRED_XTASK_ALIAS}`; the alias is how both gates are run, so rewriting it makes them exit zero without checking anything",
                alias.map_or_else(|| "nothing".to_owned(), |value| format!("`{value}`"))
            ),
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

    // `[lib] test = false` stops the crate's test binary being built at all, which stops
    // llvm-cov instrumenting it, which makes a crate full of untested code report "no
    // coverable lines" and pass the coverage gate. It also silently stops its unit tests
    // running. Two lines, in a table nothing else reads.
    for key in ["test", "doctest", "harness"] {
        let disabled = document
            .get("lib")
            .and_then(|lib| lib.get(key))
            .and_then(Value::as_bool)
            .is_some_and(|enabled| !enabled);
        if disabled {
            violations.push(Violation::new(
                "member-manifest",
                name.to_owned(),
                format!(
                    "declares `[lib] {key} = false`, which stops the crate being measured; a crate that opts out of testing opts out of the coverage gate"
                ),
            ));
        }
    }

    if LAYERS.first().is_some_and(|kernel| kernel.name == name) {
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
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "clippy::unwrap_used")
        );
    }

    #[test]
    fn permitted_unsafe_code_is_reported() {
        let allowed = GOOD_WORKSPACE.replace("unsafe_code = \"forbid\"", "unsafe_code = \"allow\"");
        let violations = check_workspace_lints(&allowed);
        assert!(violations.iter().any(|v| v.subject == "unsafe_code"));
    }

    #[test]
    fn a_lint_group_set_to_allow_is_reported() {
        let disabled = GOOD_WORKSPACE.replace(
            "pedantic = { level = \"warn\", priority = -1 }",
            "pedantic = { level = \"allow\", priority = -1 }",
        );
        let violations = check_workspace_lints(&disabled);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "clippy::pedantic" && v.detail.contains("warn, deny")),
            "an allowed group is a disabled group: {}",
            details(&violations)
        );
    }

    #[test]
    fn a_merely_warned_unwrap_is_reported() {
        let warned = GOOD_WORKSPACE.replace("unwrap_used = \"deny\"", "unwrap_used = \"warn\"");
        let violations = check_workspace_lints(&warned);
        assert!(
            violations
                .iter()
                .any(|v| v.subject == "clippy::unwrap_used"),
            "the issue asks for denied, not warned: {}",
            details(&violations)
        );
    }

    #[test]
    fn merely_warned_unsafe_code_is_reported() {
        let warned = GOOD_WORKSPACE.replace("unsafe_code = \"forbid\"", "unsafe_code = \"warn\"");
        let violations = check_workspace_lints(&warned);
        assert!(violations.iter().any(|v| v.subject == "unsafe_code"));
    }

    #[test]
    fn a_denied_unsafe_code_is_accepted() {
        let denied = GOOD_WORKSPACE.replace("unsafe_code = \"forbid\"", "unsafe_code = \"deny\"");
        assert!(check_workspace_lints(&denied).is_empty());
    }

    #[test]
    fn a_missing_lint_table_is_reported() {
        let without = "[workspace]\nmembers = []\n";
        let violations = check_workspace_lints(without);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].subject, "Cargo.toml");
    }

    #[test]
    fn an_invalid_workspace_manifest_is_reported_by_both_manifest_rules() {
        let broken = "[workspace\nmembers = [";
        assert!(
            check_workspace_lints(broken)
                .iter()
                .any(|v| v.detail.contains("not valid TOML"))
        );
        assert!(
            check_release_profile(broken)
                .iter()
                .any(|v| v.detail.contains("not valid TOML"))
        );
    }

    #[test]
    fn an_invalid_member_manifest_is_reported() {
        let violations = check_member_manifest("waymaker-core", "[package\nname =");
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("not valid TOML"))
        );
        assert!(violations.iter().all(|v| v.subject == "waymaker-core"));
    }

    #[test]
    fn every_dependency_table_in_the_kernel_manifest_is_reported() {
        for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let manifest = format!(
                "[package]\nname = \"waymaker-core\"\n\n[lints]\nworkspace = true\n\n[{table}]\nmemchr = \"2\"\n"
            );
            let violations = check_member_manifest("waymaker-core", &manifest);
            assert!(
                violations.iter().any(|v| v.detail.contains(table)),
                "[{table}] in the kernel manifest went unreported: {}",
                details(&violations)
            );
        }
    }

    #[test]
    fn dependency_tables_are_only_banned_in_the_kernel() {
        let manifest = "[package]\nname = \"waymaker-flash\"\n\n[lints]\nworkspace = true\n\n[dependencies]\nwaymaker-core = { path = \"../waymaker-core\" }\n";
        assert!(check_member_manifest("waymaker-flash", manifest).is_empty());
    }

    /// A `.cargo/config.toml` that carries the alias and nothing else.
    fn good_cargo_config() -> String {
        format!("[alias]\nxtask = \"{REQUIRED_XTASK_ALIAS}\"\n")
    }

    #[test]
    fn a_cargo_config_with_only_the_alias_passes() {
        let violations = check_cargo_config(Some(&good_cargo_config()));
        assert!(violations.is_empty(), "{}", details(&violations));
    }

    #[test]
    fn a_missing_cargo_config_is_reported() {
        // Without the file there is no `cargo xtask`, so neither gate is reachable by the
        // command CI, the hook and the README all use.
        assert!(!check_cargo_config(None).is_empty());
    }

    #[test]
    fn a_rewritten_xtask_alias_is_reported() {
        // The attack this rule exists for: `cargo xtask check-layering` and
        // `cargo xtask coverage` both print usage and exit zero, and every command string
        // in the workflow still matches the table byte for byte.
        let config = "[alias]\nxtask = \"run --quiet --package xtask -- --help\"\n";
        let violations = check_cargo_config(Some(config));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("exit zero") && v.detail.contains("--help")),
            "{}",
            details(&violations)
        );
    }

    #[test]
    fn a_missing_xtask_alias_is_reported() {
        let violations = check_cargo_config(Some("[alias]\nlint = \"clippy\"\n"));
        assert!(
            violations.iter().any(|v| v.detail.contains("nothing")),
            "{}",
            details(&violations)
        );
    }

    #[test]
    fn a_profile_in_the_cargo_config_is_reported() {
        let config = format!(
            "{}\n[profile.release]\nopt-level = 3\n",
            good_cargo_config()
        );
        let violations = check_cargo_config(Some(&config));
        assert_eq!(violations.len(), 1, "{}", details(&violations));
        assert_eq!(violations[0].rule, "cargo-config-profile");
        assert!(violations[0].detail.contains("profile.release"));
    }

    #[test]
    fn every_profile_in_the_cargo_config_is_named() {
        let config = format!(
            "{}\n[profile.release]\nopt-level = 3\n\n[profile.dev]\ndebug = false\n",
            good_cargo_config()
        );
        let violations = check_cargo_config(Some(&config));
        assert_eq!(violations.len(), 2, "{}", details(&violations));
    }

    #[test]
    fn an_env_table_in_the_cargo_config_is_reported() {
        // `LLVM_COV_FLAGS = "--ignore-filename-regex=..."` here would quietly remove files
        // from the coverage report the gate then passes.
        let config = format!(
            "{}\n[env]\nLLVM_COV_FLAGS = \"--ignore-filename-regex=untested\"\n",
            good_cargo_config()
        );
        let violations = check_cargo_config(Some(&config));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("LLVM_COV_FLAGS")),
            "{}",
            details(&violations)
        );
    }

    #[test]
    fn build_rustflags_in_the_cargo_config_are_reported() {
        let config = format!(
            "{}\n[build]\nrustflags = [\"-C\", \"opt-level=3\"]\n",
            good_cargo_config()
        );
        let violations = check_cargo_config(Some(&config));
        assert!(
            violations.iter().any(|v| v.detail.contains("rustflags")),
            "{}",
            details(&violations)
        );
    }

    #[test]
    fn a_crate_that_opts_out_of_its_own_test_binary_is_reported() {
        // `[lib] test = false` stops llvm-cov instrumenting the crate, so a crate full of
        // untested code reports "no coverable lines" and clears the coverage gate.
        let manifest = "[package]\nname = \"waymaker-core\"\n\n[lints]\nworkspace = true\n\n[lib]\ntest = false\n";
        let violations = check_member_manifest("waymaker-core", manifest);
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("stops the crate being measured")),
            "{}",
            details(&violations)
        );
    }

    #[test]
    fn every_expected_profile_value_is_a_toml_literal() {
        // Guards the constant itself: an entry that does not parse would otherwise
        // silently uncheck its key.
        for (key, expected) in REQUIRED_RELEASE_PROFILE {
            assert!(
                expected.parse::<Value>().is_ok(),
                "the expected value for {key} is not valid TOML: {expected}"
            );
        }
    }

    #[test]
    fn an_unparseable_expected_value_is_reported_as_a_broken_gate() {
        // `z` rather than `"z"`: a plausible tidy-up that would otherwise disable the
        // check for that key without a word.
        let violations = check_release_profile_against(&[("opt-level", "z")], GOOD_WORKSPACE);
        assert_eq!(violations.len(), 1, "{}", details(&violations));
        assert_eq!(violations[0].rule, "gate-broken");
        assert!(violations[0].detail.contains("opt-level"));
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
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("workspace = true"))
        );
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
