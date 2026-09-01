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

/// `extern crate` declarations that re-admit what `#![no_std]` excludes.
///
/// `#![no_std]` is an attribute, not a guarantee: `extern crate std;` below it puts the
/// standard library back, and `extern crate alloc;` puts the allocator back, with the
/// attribute still sitting there for a reviewer to see. The `thumbv6m-none-eabi` build now
/// catches the `std` half — there is no `std` to link against on that target — but not the
/// `alloc` half, which cross-compiles perfectly well. This scan is what stops both, and it
/// says which line did it rather than leaving a linker error to be interpreted.
pub const FORBIDDEN_EXTERN_CRATES: &[&str] = &["std", "alloc"];

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

        if silences_lint(&attributes, "unsafe_code") {
            violations.push(Violation::new(
                "crate-attributes",
                spec.name,
                "src/lib.rs allows unsafe code; a documented exception belongs in an ADR",
            ));
        }

        for name in extern_crates(source.contents) {
            if FORBIDDEN_EXTERN_CRATES.contains(&name.as_str()) {
                violations.push(Violation::new(
                    "crate-attributes",
                    spec.name,
                    format!(
                        "src/lib.rs declares `extern crate {name};`, which puts back what #![no_std] excludes"
                    ),
                ));
            }
        }
    }

    violations
}

/// Collects the crate names in `extern crate <name>;` declarations, ignoring comments.
fn extern_crates(contents: &str) -> Vec<String> {
    contents
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("extern crate "))
        .filter_map(|rest| {
            rest.split([' ', ';'])
                .find(|token| !token.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

/// Lint levels that turn a lint on.
///
/// `missing_docs` is allow-by-default, so only naming it explicitly enables it — the
/// `warnings` group does not, because a lint that is off by default is not among the
/// warnings.
pub const ENFORCING_LEVELS: &[&str] = &["warn", "deny", "forbid"];

/// Lint levels that turn a warning off.
///
/// `expect` belongs here with `allow`: an expectation that the compiler *fulfils* — which
/// is exactly what an undocumented item does to `#![expect(missing_docs)]` — emits nothing
/// at all, so it silences a lint as completely as an allow and looks more deliberate doing
/// it.
pub const SILENCING_LEVELS: &[&str] = &["allow", "expect"];

/// The lint group that silences every warning at once.
pub const EVERY_WARNING: &str = "warnings";

/// Collects the crate-level inner attributes, normalising whitespace and joining an
/// attribute that is written across several lines.
///
/// Formatting must not decide whether a rule passes, and the multi-line form is the
/// interesting case rather than a tidiness one: `rustfmt` writes any attribute with a
/// `reason =` across three lines, and a scanner that reads only the first of them sees
/// `#![allow(` — a fragment that names no lint, so a crate can silence the very lint a
/// rule is watching for while the rule reports nothing.
///
/// `pub(crate)` because [`crate::size::check_size_probe`] and [`crate::docs`] ask the same
/// question of other crate roots. One scanner, so that the tests here — a commented-out
/// attribute does not count, extra whitespace is tolerated, a bracket inside a string is
/// not a bracket — are load-bearing for every rule rather than for whichever copy they
/// happen to sit beside.
pub(crate) fn inner_attributes(contents: &str) -> Vec<String> {
    let mut attributes = Vec::new();
    let mut open: Option<String> = None;
    let mut in_block_comment = false;

    for raw in contents.lines() {
        let uncommented = strip_block_comments(raw, &mut in_block_comment);
        let line = uncommented.trim();
        let code = strip_line_comment(line);
        match open.as_mut() {
            Some(buffer) => buffer.push_str(code),
            None if code.starts_with("#![") => open = Some(code.to_owned()),
            None => continue,
        }
        if open.as_deref().is_some_and(is_balanced) {
            if let Some(buffer) = open.take() {
                attributes.push(buffer.split_whitespace().collect());
            }
        }
    }

    // An attribute whose brackets never balance is scanned anyway rather than dropped: a
    // rule that quietly forgot an attribute is a rule with a hole exactly where someone
    // would put one.
    if let Some(buffer) = open {
        attributes.push(buffer.split_whitespace().collect());
    }

    attributes
}

/// `line` with any `/* ... */` comment removed, carrying the open state across lines.
///
/// A commented-out attribute must not count as present. `//` was already handled; a block
/// comment was not, and commenting an attribute out with `/* */` is the more natural thing
/// to do to three lines of it — which would leave the gate reading an attribute the
/// compiler never sees.
fn strip_block_comments(line: &str, in_comment: &mut bool) -> String {
    let mut kept = String::new();
    let mut rest = line;
    loop {
        if *in_comment {
            let Some((_, after)) = rest.split_once("*/") else {
                return kept;
            };
            rest = after;
            *in_comment = false;
            continue;
        }
        let Some((before, after)) = rest.split_once("/*") else {
            kept.push_str(rest);
            return kept;
        };
        kept.push_str(before);
        // A space so that `a/*x*/b` does not become the identifier `ab`.
        kept.push(' ');
        rest = after;
        *in_comment = true;
    }
}

/// `line` up to a `//` that is not inside a string literal.
fn strip_line_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                return line.get(..index).unwrap_or(line).trim_end();
            }
            _ => {}
        }
    }
    line
}

/// Whether every bracket in `fragment` outside a string literal is closed.
fn is_balanced(fragment: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for byte in fragment.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'[' | b'(' => depth = depth.saturating_add(1),
            b']' | b')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth == 0
}

/// Whether any of `attributes` turns `lint` on, unconditionally.
///
/// Compared argument by argument rather than against a fixed set of whole attributes, so
/// that `#![warn(missing_docs, unreachable_pub)]` — a correct crate root — is not rejected
/// for having said two things in one attribute.
///
/// Only the outermost group counts. `#![cfg_attr(any(), warn(missing_docs))]` names the
/// lint at a level rustc never applies, and this scanner cannot evaluate a `cfg` predicate,
/// so a conditional enabling is not an enabling. [`silences_lint`] takes the opposite rule
/// for the same reason: each direction answers the way that fails closed.
pub(crate) fn enables_lint(attributes: &[String], lint: &str) -> bool {
    attributes.iter().any(|attribute| {
        ENFORCING_LEVELS.iter().any(|level| {
            attribute
                .strip_prefix(&format!("#![{level}("))
                .and_then(balanced_body)
                .is_some_and(|body| split_arguments(body).contains(&lint))
        })
    })
}

/// Whether any of `attributes` turns `lint` off.
///
/// Unlike [`enables_lint`], a lint named inside a `cfg_attr` counts here: an attribute that
/// silences a lint under some configuration is a silencing, and the scanner cannot say which
/// configuration is built.
///
/// Matching on the lint level rather than on the literal string `#![allow(` is what stops
/// the four ways of saying the same thing — `expect` instead of `allow`, the `warnings`
/// group instead of the lint, a lint listed second in a group, an attribute `rustfmt` split
/// over three lines — from each being a separate hole. Arguments are compared whole, so
/// `missing_docs_in_private_items` is not `missing_docs`.
pub(crate) fn silences_lint(attributes: &[String], lint: &str) -> bool {
    attributes.iter().any(|attribute| {
        SILENCING_LEVELS.iter().any(|level| {
            lint_arguments(attribute, level)
                .iter()
                .any(|argument| *argument == lint || *argument == EVERY_WARNING)
        })
    })
}

/// The comma-separated arguments of every `<level>(...)` group in `attribute`.
///
/// Nested groups are flattened, so `allow(unused, clippy::pedantic, missing_docs)` yields
/// each name, and a level name that is the tail of a longer identifier — the `allow(` in
/// `disallow(` — is not a group at all.
fn lint_arguments<'a>(attribute: &'a str, level: &str) -> Vec<&'a str> {
    let needle = format!("{level}(");
    let mut arguments = Vec::new();
    let mut searched = 0usize;

    while let Some(rest) = attribute.get(searched..) {
        let Some(offset) = rest.find(&needle) else {
            break;
        };
        let start = searched.saturating_add(offset);
        let is_own_identifier = attribute
            .get(..start)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|character| !character.is_alphanumeric() && character != '_');
        searched = start.saturating_add(needle.len());
        if !is_own_identifier {
            continue;
        }
        if let Some(body) = balanced_body(attribute.get(searched..).unwrap_or_default()) {
            arguments.extend(split_arguments(body));
        }
    }

    arguments
}

/// The text up to the `)` that closes the group `rest` starts inside.
fn balanced_body(rest: &str) -> Option<&str> {
    let mut depth: u32 = 1;
    for (index, character) in rest.char_indices() {
        match character {
            '(' | '[' => depth = depth.saturating_add(1),
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return rest.get(..index);
                }
            }
            _ => {}
        }
    }
    None
}

/// `body` split on the commas that are not inside a nested group.
fn split_arguments(body: &str) -> Vec<&str> {
    let mut arguments = Vec::new();
    let mut depth: u32 = 0;
    let mut start = 0usize;
    for (index, character) in body.char_indices() {
        match character {
            '(' | '[' => depth = depth.saturating_add(1),
            ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                if let Some(argument) = body.get(start..index) {
                    arguments.push(argument.trim());
                }
                start = index.saturating_add(1);
            }
            _ => {}
        }
    }
    if let Some(argument) = body.get(start..) {
        arguments.push(argument.trim());
    }
    arguments
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
    fn a_multiline_attribute_is_read_as_one_attribute() {
        // Rust accepts an attribute split over several lines, and a line-oriented scan
        // reads only `#![allow(` — which is how a crate silences a lint the gate is
        // watching for while the gate reports nothing.
        let split = "#![no_std]\n#![forbid(\n    unsafe_code\n)]\n";
        assert_eq!(
            inner_attributes(split),
            ["#![no_std]", "#![forbid(unsafe_code)]"]
        );
    }

    #[test]
    fn a_trailing_comment_does_not_change_an_attribute() {
        let commented = "#![no_std] // bare metal\n#![forbid(unsafe_code)]\n";
        assert!(check_crate_attributes(&sources("waymaker-core", commented)).is_empty());
    }

    #[test]
    fn a_multiline_allow_of_unsafe_code_is_reported() {
        let sneaky = "#![no_std]\n#![forbid(unsafe_code)]\n#![allow(\n    unsafe_code\n)]\n";
        let violations = check_crate_attributes(&sources("waymaker-core", sneaky));
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("allows unsafe")),
            "{violations:?}"
        );
    }

    #[test]
    fn expecting_a_lint_silences_it_just_as_allowing_it_does() {
        let attributes = inner_attributes("#![expect(missing_docs)]\n");
        assert!(silences_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn allowing_every_warning_silences_a_lint_by_name() {
        let attributes = inner_attributes("#![allow(warnings)]\n");
        assert!(silences_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn a_lint_whose_name_merely_starts_with_another_is_not_confused_for_it() {
        let attributes = inner_attributes("#![allow(missing_docs_in_private_items)]\n");
        assert!(!silences_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn a_lint_named_inside_a_nested_group_is_found() {
        let attributes = inner_attributes("#![allow(unused, clippy::pedantic, missing_docs)]\n");
        assert!(silences_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn warning_on_a_lint_does_not_silence_it() {
        let attributes = inner_attributes("#![warn(missing_docs)]\n");
        assert!(!silences_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn a_lint_name_ending_in_allow_is_not_a_silencing_level() {
        let attributes = inner_attributes("#![deny(clippy::disallow(missing_docs))]\n");
        assert!(!silences_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn a_bracket_inside_a_string_does_not_swallow_the_next_attribute() {
        let quoted = "#![allow(dead_code, reason = \"a ) in a string\")]\n#![no_std]\n";
        assert!(inner_attributes(quoted).contains(&"#![no_std]".to_owned()));
    }

    #[test]
    fn a_block_commented_attribute_does_not_count() {
        // Commenting three lines out with `/* */` is the natural thing to do to a
        // multi-line attribute, and it leaves the text in the file for a scanner to find
        // while the compiler never sees it.
        let commented = "/* off while we finish:\n#![no_std]\n*/\n#![forbid(unsafe_code)]\n";
        let violations = check_crate_attributes(&sources("waymaker-core", commented));
        assert!(
            violations.iter().any(|v| v.detail.contains("no_std")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_block_comment_between_attributes_does_not_join_them() {
        let spaced = "#![no_std] /* note */ \n#![forbid(unsafe_code)]\n";
        assert!(check_crate_attributes(&sources("waymaker-core", spaced)).is_empty());
    }

    #[test]
    fn a_lint_silenced_through_cfg_attr_is_found() {
        let attributes = inner_attributes("#![cfg_attr(all(), allow(missing_docs))]\n");
        assert!(silences_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn a_lint_enabled_alongside_another_in_one_attribute_is_found() {
        let attributes = inner_attributes("#![warn(missing_docs, unreachable_pub)]\n");
        assert!(enables_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn denying_and_forbidding_also_enable_a_lint() {
        for level in ["deny", "forbid"] {
            let attributes = inner_attributes(&format!("#![{level}(missing_docs)]\n"));
            assert!(enables_lint(&attributes, "missing_docs"), "{level}");
        }
    }

    #[test]
    fn warning_on_every_warning_does_not_enable_an_allow_by_default_lint() {
        // `missing_docs` is allow-by-default, so it is not one of the `warnings`.
        let attributes = inner_attributes("#![warn(warnings)]\n");
        assert!(!enables_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn a_lint_enabled_only_under_a_cfg_predicate_does_not_count_as_enabled() {
        // `any()` is false, so rustc applies no attribute at all — but the lint name is
        // right there in the file for a scanner that flattens `cfg_attr`.
        let attributes = inner_attributes("#![cfg_attr(any(), warn(missing_docs))]\n");
        assert!(!enables_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn a_lint_silenced_only_under_a_cfg_predicate_still_counts_as_silenced() {
        // The opposite answer to the test above, on purpose: each direction takes the one
        // that fails closed.
        let attributes = inner_attributes("#![cfg_attr(any(), allow(missing_docs))]\n");
        assert!(silences_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn allowing_a_lint_does_not_enable_it() {
        let attributes = inner_attributes("#![allow(missing_docs)]\n");
        assert!(!enables_lint(&attributes, "missing_docs"));
    }

    #[test]
    fn a_crate_that_is_not_a_layer_is_ignored() {
        assert!(check_crate_attributes(&sources("xtask", "fn main() {}")).is_empty());
    }
}
