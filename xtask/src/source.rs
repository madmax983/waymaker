//! Rules over crate roots.
//!
//! `#![no_std]` and `#![forbid(unsafe_code)]` are one-line attributes that a refactor can
//! delete without anything on the host noticing. Checking for them here means the deletion
//! fails a pull request rather than surfacing later as a firmware build error.

use crate::Violation;
use crate::policy::LAYERS;

/// A firmware crate's library root, ready to be inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrateSource<'a> {
    /// The package name.
    pub name: &'a str,
    /// The contents of `src/lib.rs`.
    pub contents: &'a str,
}

/// Inner attributes that every firmware crate root must carry.
pub const REQUIRED_INNER_ATTRIBUTES: &[&str] = &["#![no_std]", "#![forbid(unsafe_code)]"];

/// Rule: every firmware crate is `no_std` and forbids unsafe code.
///
/// A crate named in [`LAYERS`] but absent from `sources` is not reported here; the graph
/// rules already report it as a missing layer.
#[must_use]
pub fn check_crate_attributes(sources: &[CrateSource<'_>]) -> Vec<Violation> {
    let mut violations = Vec::new();

    for spec in LAYERS {
        let Some(source) = sources.iter().find(|source| source.name == spec.name) else {
            continue;
        };

        let attributes = inner_attributes(source.contents);
        for required in REQUIRED_INNER_ATTRIBUTES {
            if !attributes.iter().any(|line| line == required) {
                violations.push(Violation::new(
                    "crate-attributes",
                    spec.name,
                    format!("src/lib.rs is missing `{required}`"),
                ));
            }
        }

        if attributes
            .iter()
            .any(|line| line.starts_with("#![allow(") && line.contains("unsafe_code"))
        {
            violations.push(Violation::new(
                "crate-attributes",
                spec.name,
                "src/lib.rs allows unsafe code; a documented exception belongs in an ADR",
            ));
        }
    }

    violations
}

/// Collects the crate-level inner attributes, ignoring comments and normalising
/// whitespace, so that formatting does not decide whether the rule passes.
fn inner_attributes(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("#!["))
        .map(|line| line.split_whitespace().collect::<String>())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "//! Docs.\n#![no_std]\n#![forbid(unsafe_code)]\n";

    fn sources<'a>(name: &'a str, contents: &'a str) -> Vec<CrateSource<'a>> {
        vec![CrateSource { name, contents }]
    }

    #[test]
    fn a_no_std_crate_that_forbids_unsafe_passes() {
        assert!(check_crate_attributes(&sources("waymaker-core", GOOD)).is_empty());
    }

    #[test]
    fn a_missing_no_std_attribute_is_reported() {
        let violations =
            check_crate_attributes(&sources("waymaker-core", "#![forbid(unsafe_code)]\n"));
        assert!(violations.iter().any(|v| v.detail.contains("no_std")));
    }

    #[test]
    fn a_missing_forbid_unsafe_attribute_is_reported() {
        let violations = check_crate_attributes(&sources("waymaker-flash", "#![no_std]\n"));
        assert!(violations.iter().any(|v| v.detail.contains("unsafe_code")));
    }

    #[test]
    fn a_commented_out_attribute_does_not_count() {
        let commented = "//! Docs.\n// #![no_std]\n#![forbid(unsafe_code)]\n";
        let violations = check_crate_attributes(&sources("waymaker-core", commented));
        assert!(violations.iter().any(|v| v.detail.contains("no_std")));
    }

    #[test]
    fn extra_whitespace_inside_an_attribute_is_tolerated() {
        let spaced = "  #![ no_std ]\n#![forbid( unsafe_code )]\n";
        assert!(check_crate_attributes(&sources("waymaker-core", spaced)).is_empty());
    }

    #[test]
    fn allowing_unsafe_code_is_reported() {
        let sneaky = "#![no_std]\n#![forbid(unsafe_code)]\n#![allow(unsafe_code)]\n";
        let violations = check_crate_attributes(&sources("waymaker-core", sneaky));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("allows unsafe"))
        );
    }

    #[test]
    fn a_crate_that_is_not_a_layer_is_ignored() {
        assert!(check_crate_attributes(&sources("xtask", "fn main() {}")).is_empty());
    }
}
