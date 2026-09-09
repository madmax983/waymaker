//! Rules over crate roots.
//!
//! `#![no_std]` and `#![forbid(unsafe_code)]` are one-line attributes that a refactor can
//! delete without anything on the host noticing. Checking for them here means the deletion
//! fails a pull request rather than surfacing later as a firmware build error.

use std::collections::BTreeSet;

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
///
/// Scanned over *every* source file of every layer, not only the crate roots. Rust admits
/// `extern crate alloc;` inside a nested module, so a scan of `src/lib.rs` alone leaves a
/// decoder free to allocate with the crate root still saying `#![no_std]` and every gate
/// still green — which is exactly what `bounded-decoding`'s allocation clause rests on not
/// being possible. Codex caught that on pull request #66.
pub const FORBIDDEN_EXTERN_CRATES: &[&str] = &["std", "alloc"];

/// Rule: every firmware crate is `no_std` and forbids unsafe code, and every test-support
/// crate forbids unsafe code.
///
/// A crate named in [`LAYERS`] but absent from `sources` is not reported here; the graph
/// rules already report it as a missing layer.
///
/// A test-support crate is deliberately held to less: most of them are host code, so
/// `#![no_std]` and the `extern crate std` scan would be wrong. `#![forbid(unsafe_code)]` is
/// not — nothing about modelling media in a `Vec<u8>` needs it, and a harness the layers are
/// tested against is the last place an unreviewed `unsafe` block should be able to appear.
///
/// The three in [`NO_STD_TEST_SUPPORT_CRATES`](crate::policy::NO_STD_TEST_SUPPORT_CRATES) do
/// make the `#![no_std]` claim, and are held to it here. The firmware-target build stages
/// cannot: `cargo build --lib` produces an rlib and never links, so no global allocator is
/// required and an `extern crate alloc` under any of the three compiles clean. Issue #28's
/// "no allocation" would otherwise be an inspection, which is the one thing this workspace
/// says an invariant must never be.
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

    for member in crate::policy::NO_STD_TEST_SUPPORT_CRATES {
        let Some(source) = sources.iter().find(|source| &source.name == member) else {
            continue;
        };
        let attributes = inner_attributes(source.contents);
        if !attributes.iter().any(|line| line == "#![no_std]") {
            violations.push(Violation::new(
                "crate-attributes",
                *member,
                "src/lib.rs is missing `#![no_std]`, which this crate's own documentation \
                 claims",
            ));
        }
        for name in extern_crates(source.contents) {
            if FORBIDDEN_EXTERN_CRATES.contains(&name.as_str()) {
                violations.push(Violation::new(
                    "crate-attributes",
                    *member,
                    format!(
                        "src/lib.rs declares `extern crate {name};`, which puts back what \
                         #![no_std] excludes; the firmware-target build stage cannot catch \
                         this, because `--lib` produces an rlib and never links"
                    ),
                ));
            }
        }
    }

    for member in crate::policy::checked_members() {
        let Some(source) = sources.iter().find(|source| source.name == member) else {
            continue;
        };
        let attributes = inner_attributes(source.contents);
        if !attributes
            .iter()
            .any(|line| line == "#![forbid(unsafe_code)]")
        {
            violations.push(Violation::new(
                "crate-attributes",
                member,
                "src/lib.rs is missing `#![forbid(unsafe_code)]`",
            ));
        }
        if silences_lint(&attributes, "unsafe_code") {
            violations.push(Violation::new(
                "crate-attributes",
                member,
                "src/lib.rs allows unsafe code; a documented exception belongs in an ADR",
            ));
        }
    }

    violations
}

/// Collects the crate names in `extern crate <name>;` declarations, ignoring comments.
fn extern_crates(contents: &str) -> Vec<String> {
    contents.lines().filter_map(declared_extern_crate).collect()
}

/// The crate `line` declares with `extern crate`, if it declares one.
///
/// Not `strip_prefix("extern crate ")` on the trimmed line, which is what this was and which
/// Codex caught on pull request #66: `pub extern crate alloc;` is valid Rust — visibility on
/// an `extern crate` re-exports the name — and rustfmt leaves it alone, so the scan read
/// nothing and the decoder could allocate. `pub(crate)`, `pub(super)`, `pub(in path)` and a
/// same-line `#[macro_use]` are the same evasion in four more spellings, and arbitrary
/// whitespace between the keywords is a fifth.
///
/// So the line is tokenised rather than prefix-matched: attributes and visibility are
/// stripped, then the first two tokens must be exactly `extern` and `crate`. A comment or a
/// doc line keeps its marker as the first token and is therefore never mistaken for a
/// declaration.
fn declared_extern_crate(line: &str) -> Option<String> {
    let rest = strip_visibility(strip_leading_attributes(line.trim()));
    let mut tokens = rest.split_whitespace();
    if tokens.next() != Some("extern") || tokens.next() != Some("crate") {
        return None;
    }
    let name = tokens.next()?.trim_end_matches(';');
    (!name.is_empty()).then(|| name.to_owned())
}

/// `line` with any `#[..]` or `#![..]` attributes at its start removed.
fn strip_leading_attributes(line: &str) -> &str {
    let mut rest = line;
    loop {
        let Some(after_hash) = rest.strip_prefix('#') else {
            return rest;
        };
        let after_bang = after_hash.strip_prefix('!').unwrap_or(after_hash);
        let Some(body) = after_bang.strip_prefix('[') else {
            return rest;
        };
        // Nested brackets are possible — `#[cfg(all(a, b))]` has none, but `#[doc = "[x]"]`
        // does — so the close is found by depth rather than by the first `]`.
        let mut depth = 1_usize;
        let mut end = None;
        for (index, character) in body.char_indices() {
            match character {
                '[' => depth = depth.saturating_add(1),
                ']' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { return rest };
        let Some(after) = body.get(end.saturating_add(1)..) else {
            return rest;
        };
        rest = after.trim_start();
    }
}

/// `line` with a leading visibility modifier removed.
fn strip_visibility(line: &str) -> &str {
    let Some(after_pub) = line.strip_prefix("pub") else {
        return line;
    };
    // `pub` has to be a whole token: `public_thing` is not a visibility modifier.
    match after_pub.chars().next() {
        Some('(') => {
            let Some(close) = after_pub.find(')') else {
                return line;
            };
            after_pub
                .get(close.saturating_add(1)..)
                .map_or(line, str::trim_start)
        }
        Some(character) if character.is_whitespace() => after_pub.trim_start(),
        _ => line,
    }
}

/// Rule: no source file of a layer, or of a `no_std` test-support crate, re-admits `std` or
/// `alloc`.
///
/// The other half of [`check_crate_attributes`]'s `extern crate` scan, which reads crate
/// roots. A nested module may declare `extern crate alloc;` perfectly legally, and nothing
/// about the crate root would change — so a firmware crate could allocate with `#![no_std]`
/// above it, `cargo build --target thumbv6m-none-eabi` still green (`alloc` cross-compiles),
/// and `waymaker-spec`'s `bounded-decoding` row still claiming allocation-freedom is
/// structural. It is structural only because of this.
///
/// The same hole exists for the three crates in
/// [`NO_STD_TEST_SUPPORT_CRATES`](crate::policy::NO_STD_TEST_SUPPORT_CRATES), and it is worse
/// there: their firmware-target stages build `--lib`, which produces an rlib and never links,
/// so no global allocator is required and `cargo build` stays green either way. They are read
/// here for that reason.
///
/// Fires under `crate-attributes` rather than under an id of its own: it is the same rule
/// about the same thing, read over more files.
#[must_use]
pub fn check_layer_sources_are_bare_metal(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for source in sources {
        let covered = crate::policy::layer(&source.crate_name).is_some()
            || crate::policy::NO_STD_TEST_SUPPORT_CRATES.contains(&source.crate_name.as_str());
        if !covered {
            continue;
        }
        for name in extern_crates(&source.contents) {
            if FORBIDDEN_EXTERN_CRATES.contains(&name.as_str()) {
                violations.push(Violation::new(
                    "crate-attributes",
                    source.crate_name.clone(),
                    format!(
                        "{} declares `extern crate {name};`, which puts back what \
                         #![no_std] excludes — an attribute on the crate root is not a \
                         guarantee about the modules under it",
                        source.path
                    ),
                ));
            }
        }
    }
    violations
}

#[cfg(test)]
mod bare_metal_tests {
    use super::check_layer_sources_are_bare_metal;
    use crate::size::LayerSource;

    fn source(crate_name: &str, path: &str, contents: &str) -> LayerSource {
        LayerSource {
            crate_name: crate_name.to_owned(),
            path: path.to_owned(),
            contents: contents.to_owned(),
        }
    }

    #[test]
    fn a_no_std_test_support_crate_is_read_the_same_way() {
        // Codex, pull request #89. The three crates in `NO_STD_TEST_SUPPORT_CRATES` claim
        // `#![no_std]`, and their firmware-target stages build `--lib` — an rlib, which
        // never links, so no global allocator is required and `cargo build` stays green
        // whatever a nested module declares. This rule is the whole of the check.
        let violations = check_layer_sources_are_bare_metal(&[source(
            "waymaker-drive",
            "crates/waymaker-drive/src/activity.rs",
            "extern crate alloc;\n",
        )]);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].detail.contains("activity.rs"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_host_side_test_support_crate_is_left_alone() {
        // `waymaker-fault` models media in a `Vec` and `waymaker-spec` enumerates a state
        // space. Asking either for `#![no_std]` would be asking it to stop doing its job.
        let violations = check_layer_sources_are_bare_metal(&[source(
            "waymaker-fault",
            "crates/waymaker-fault/src/device.rs",
            "extern crate alloc;\n",
        )]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_nested_module_that_puts_the_allocator_back_is_caught() {
        // Codex, pull request #66. The crate root still says `#![no_std]`, the firmware
        // target still builds — `alloc` cross-compiles — and before this rule every gate
        // was green while the decoder could allocate.
        let violations = check_layer_sources_are_bare_metal(&[source(
            "waymaker-flash",
            "crates/waymaker-flash/src/frame.rs",
            "extern crate alloc;\npub fn decode() {}\n",
        )]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "crate-attributes");
        assert_eq!(violations[0].subject, "waymaker-flash");
        assert!(violations[0].detail.contains("frame.rs"), "{violations:?}");
        assert!(violations[0].detail.contains("extern crate alloc"));
    }

    #[test]
    fn every_spelling_of_a_visible_extern_crate_is_caught() {
        // Codex, pull request #66 round 3. `pub extern crate alloc;` is valid Rust — a
        // visibility modifier on an `extern crate` re-exports the name — and rustfmt leaves
        // it alone, so a prefix match on `extern crate ` read nothing while the decoder
        // could allocate. These are the spellings that evasion has.
        for line in [
            "pub extern crate alloc;",
            "pub(crate) extern crate alloc;",
            "pub(super) extern crate alloc;",
            "pub(in crate::frame) extern crate alloc;",
            "#[macro_use] extern crate alloc;",
            "#[macro_use] pub extern crate alloc;",
            "extern   crate   alloc;",
            "    pub  extern crate alloc as _;",
        ] {
            let violations = check_layer_sources_are_bare_metal(&[source(
                "waymaker-flash",
                "crates/waymaker-flash/src/frame.rs",
                line,
            )]);
            assert_eq!(violations.len(), 1, "`{line}` was not caught");
            assert!(
                violations[0].detail.contains("extern crate alloc"),
                "`{line}`"
            );
        }
    }

    #[test]
    fn a_line_that_only_looks_like_a_declaration_is_not_one() {
        for line in [
            "/// extern crate alloc;",
            "//! extern crate alloc;",
            "// pub extern crate alloc;",
            "/// `pub extern crate alloc;` is what this rule refuses",
            "pub_extern_crate_alloc();",
            "unsafe extern \"C\" { fn f(); }",
            "let extern_crate_alloc = 1;",
        ] {
            assert!(
                check_layer_sources_are_bare_metal(&[source(
                    "waymaker-core",
                    "crates/waymaker-core/src/replay.rs",
                    line,
                )])
                .is_empty(),
                "`{line}` was mistaken for a declaration"
            );
        }
    }

    #[test]
    fn a_nested_module_that_puts_the_standard_library_back_is_caught() {
        let violations = check_layer_sources_are_bare_metal(&[source(
            "waymaker-core",
            "crates/waymaker-core/src/replay.rs",
            "    extern crate std;\n",
        )]);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].detail.contains("extern crate std"));
    }

    #[test]
    fn an_ordinary_layer_source_passes() {
        assert!(
            check_layer_sources_are_bare_metal(&[source(
                "waymaker-flash",
                "crates/waymaker-flash/src/frame.rs",
                "use core::mem::size_of;\npub const fn decode() {}\n",
            )])
            .is_empty()
        );
    }

    #[test]
    fn a_crate_that_is_not_a_layer_is_not_held_to_this() {
        // `waymaker-fault` and `waymaker-spec` are host code that models media in a `Vec`.
        // The rule iterates the layers, so a source belonging to anything else is skipped.
        assert!(
            check_layer_sources_are_bare_metal(&[source(
                "waymaker-fault",
                "crates/waymaker-fault/src/device.rs",
                "extern crate alloc;\n",
            )])
            .is_empty()
        );
    }

    #[test]
    fn a_third_party_crate_named_in_an_extern_crate_line_is_not_this_rule() {
        assert!(
            check_layer_sources_are_bare_metal(&[source(
                "waymaker-embassy",
                "crates/waymaker-embassy/src/lib.rs",
                "extern crate embassy_time;\n",
            )])
            .is_empty()
        );
    }
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
    let mut comment_depth = 0u32;

    for raw in contents.lines() {
        let uncommented = strip_block_comments(raw, &mut comment_depth);
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

/// `line` with any `/* ... */` comment removed, carrying the open depth across lines.
///
/// A commented-out attribute must not count as present. `//` was already handled; a block
/// comment was not, and commenting an attribute out with `/* */` is the more natural thing
/// to do to three lines of it — which would leave the gate reading an attribute the
/// compiler never sees.
///
/// A *depth*, not a flag: Rust nests block comments, so the `*/` that closes an inner one
/// leaves the outer one open. A scanner that reopened there would read everything after the
/// inner close as live code — which is where an attribute would most plausibly sit.
fn strip_block_comments(line: &str, depth: &mut u32) -> String {
    let mut kept = String::new();
    let mut rest = line;
    loop {
        if *depth > 0 {
            let open = rest.find("/*");
            let close = rest.find("*/");
            match (open, close) {
                (Some(open), Some(close)) if open < close => {
                    *depth = depth.saturating_add(1);
                    rest = rest.get(open.saturating_add(2)..).unwrap_or_default();
                }
                (_, Some(close)) => {
                    *depth = depth.saturating_sub(1);
                    rest = rest.get(close.saturating_add(2)..).unwrap_or_default();
                }
                (Some(open), None) => {
                    *depth = depth.saturating_add(1);
                    rest = rest.get(open.saturating_add(2)..).unwrap_or_default();
                }
                (None, None) => return kept,
            }
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
        *depth = 1;
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

/// Byte-decoding constructs the kernel may not contain.
///
/// Design document §05 puts *serialization framework* and *CRC* in `waymaker-core`'s
/// must-not-own cell, and `kernel-zero-dependencies` already stops the kernel *importing*
/// either. It cannot stop the kernel *writing* one: a hand-rolled `const fn crc32(bytes:
/// &[u8]) -> u32` and a `u32::from_le_bytes` in a decode loop add no dependency, no
/// manifest entry and no graph edge, so every existing rule passes and the layering claim
/// quietly becomes prose.
///
/// These are the shapes that claim actually rules out. Each is a *conversion between bytes
/// and a value*, which is the thing the kernel delegates: the kernel names record kinds and
/// holds borrowed slices, and `waymaker-flash` is what turns bytes into either.
pub const KERNEL_FORBIDDEN_CONSTRUCTS: &[(&str, &str)] = &[
    (
        "from_le_bytes",
        "decoding an integer from bytes is the wire format, which belongs to waymaker-flash",
    ),
    (
        "from_be_bytes",
        "the kernel reads no bytes, in any endianness",
    ),
    (
        "from_ne_bytes",
        "native endianness is not a wire format at all, and the kernel has neither",
    ),
    (
        "to_le_bytes",
        "encoding an integer to bytes is the wire format, which belongs to waymaker-flash",
    ),
    (
        "to_be_bytes",
        "the kernel writes no bytes, in any endianness",
    ),
    (
        "to_ne_bytes",
        "native endianness is not a wire format at all, and the kernel has neither",
    ),
];

/// Trait implementations that would make the kernel a decoder without a single `pub fn`.
///
/// `impl TryFrom<&[u8]> for RecordRef<'_>` needs no dependency, no `pub`, and is credited by
/// `size-probe-reach` the moment anything in the probe writes `try_from(` — which
/// `usize::try_from` already does. It is the cheapest way for a serialization framework to
/// arrive in `waymaker-core`, so it is named rather than left to review.
pub const KERNEL_FORBIDDEN_IMPL_MARKERS: &[&str] = &["From<&[u8]>", "TryFrom<&[u8]>"];

/// Rule: the kernel converts nothing between bytes and values.
///
/// Scanned rather than parsed, like every other rule here, and comments are stripped first
/// so that this file's own prose — and `waymaker-core`'s, which explains at length what it
/// does *not* do — is not read as code.
///
/// This is a floor and says so. It catches the shapes a decoder is actually written in; a
/// determined author could still hand-roll a shift-and-or loop the scan does not recognise.
/// What it makes impossible is the *accidental* arrival, which is the one that gets merged.
#[must_use]
pub fn check_kernel_owns_no_encoding(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const KERNEL: &str = "waymaker-core";
    let mut violations = Vec::new();

    for source in sources.iter().filter(|source| source.crate_name == KERNEL) {
        let code = code_only(&source.contents);
        for (construct, why) in KERNEL_FORBIDDEN_CONSTRUCTS {
            if code.contains(construct) {
                violations.push(Violation::new(
                    "kernel-owns-no-encoding",
                    KERNEL,
                    format!("{} uses `{construct}`: {why}", source.path),
                ));
            }
        }
        let headers = impl_headers(&code);
        for marker in KERNEL_FORBIDDEN_IMPL_MARKERS {
            let needle = marker.replace(' ', "");
            let implemented = headers.iter().any(|header| {
                let header = erase_lifetimes(header).replace(' ', "");
                implements_trait(&header, &needle)
            });
            if implemented {
                violations.push(Violation::new(
                    "kernel-owns-no-encoding",
                    KERNEL,
                    format!(
                        "{} implements `{marker}`: a decoder needs no `pub fn` and no \
                         dependency, so this is how a serialization framework arrives in the \
                         kernel unnoticed",
                        source.path
                    ),
                ));
            }
        }
    }

    violations
}

/// The file whose public function surface [`REPLAY_SURFACE`] pins.
pub const REPLAY_SURFACE_PATH: &str = "waymaker-core/src/replay.rs";

/// Every public function the streaming replay cursor is allowed to have, in sorted order.
///
/// Issue #14's third acceptance criterion is "no API on the cursor requires random access by
/// effect ID", and design document §02 decision 2 is the invariant behind it: "There is no
/// `Journal::get(id)` and no in-memory event index." Absence is a hard thing to test — a
/// method that does not exist cannot be called by a test that would fail — so the surface is
/// pinned instead, and adding to it is a line a reviewer has to write on purpose.
///
/// A `fn record_at(&self, id: EffectId) -> Option<RecordRef<'_>>` is the shape this exists
/// to stop: it needs no dependency, breaks no layering rule, passes every other gate, and
/// would turn a constant-memory cursor into one that either seeks or indexes. On a device
/// whose whole runtime budget is 768 bytes, an index is the difference between replay
/// working and replay being impossible.
///
/// `pending_timer` is issue #33's, and it is the same shape of answer as `pending`: the one
/// open boundary, named. It is not a lookup — it takes no key and reads no history — which
/// is what makes it admissible beside a rule about random access.
///
/// Sorted, so that the comparison below can be a set comparison and the list can be read.
pub const REPLAY_SURFACE: &[&str] = &[
    "advance",
    "is_terminal",
    "new",
    "next_effect_id",
    "next_seq",
    "pending",
    "pending_timer",
    "position",
    "run",
];

/// The file whose public function surface [`TRANSITION_SURFACE`] pins.
pub const TRANSITION_SURFACE_PATH: &str = "waymaker-core/src/transition.rs";

/// Every public function the replay machine of design document §08 is allowed to have.
///
/// Issue #15 asks for divergence that is "terminal and loud: no reinterpretation of
/// history, no best-effort recovery". Every word of that is an *absence*, and a test cannot
/// call a function that is not there — so the surface is pinned, and a way back out of a
/// refusal is a line a reviewer has to write on purpose.
///
/// A `pub fn reset(&mut self)`, `clear_divergence` or `resume` is the shape this exists to
/// stop: each breaks no layering rule, needs no dependency, passes every other gate, and
/// turns "stop, never guess" into a suggestion. The same pin fails in the other direction,
/// which is the half that matters more: a name the module no longer declares means the
/// machine was renamed or deleted and the pin has stopped checking anything.
///
/// `timer_intent`, `timer_outcome` and `pending_timer` are issue #33's, and they are a
/// second *pair of halves* rather than a second table. §08's five rows are asked of a
/// deadline as well as of an activity, and the two boundaries share one cursor and one
/// sequence space. A `timer_downgrade`, a `timer_force_elapsed` or a `clear_timer` would be
/// the same door this list exists to keep shut, arriving on the timer side.
///
/// What it does **not** catch, so that nobody reads more into a green build than is there:
/// this compares *names*. A `force: bool` added to `intent`, or any other change of
/// signature or behaviour behind a name already on the list, is invisible to it and is a
/// reviewer's job. The pin raises the cost of a new door, not of widening an existing one.
///
/// Sorted, so that the comparison below can be a set comparison and the list can be read.
pub const TRANSITION_SURFACE: &[&str] = &[
    "advance",
    "diverged",
    "divergence_from",
    "intent",
    "message",
    "new",
    "outcome",
    "pending",
    "pending_timer",
    "position",
    "run",
    "timer_intent",
    "timer_outcome",
];

/// Rule: the replay cursor's public surface is exactly the one that was reviewed.
///
/// Fails in both directions, and the second is the one that matters more. A function the
/// surface gained is a new way into the cursor that nobody weighed against §02 decision 2. A
/// function it *lost* — including all of them, because the module was renamed or deleted —
/// means the pin is checking nothing, and a gate that silently stops checking is the failure
/// mode every rule here is written to avoid.
///
/// Scanned with the same reader `size-probe-reach` uses, so `#[cfg(test)]` helpers are
/// skipped and a trait method counts even without `pub` on it.
#[must_use]
pub fn check_replay_cursor_surface(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    check_pinned_surface(
        "replay-cursor-surface",
        "waymaker-core",
        REPLAY_SURFACE_PATH,
        REPLAY_SURFACE,
        sources,
        "the cursor's public API is where design document \u{a7}02 decision 2 is enforced, so \
         a lookup by effect id cannot be added without a reviewer writing it down",
    )
}

/// Rule: the replay machine's public surface is exactly the one that was reviewed.
///
/// The same shape as [`check_replay_cursor_surface`] and for a different invariant: §08's
/// divergence is terminal, and "there is no way back" is an absence no test can call.
#[must_use]
pub fn check_transition_surface(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    check_pinned_surface(
        "transition-surface",
        "waymaker-core",
        TRANSITION_SURFACE_PATH,
        TRANSITION_SURFACE,
        sources,
        "the machine's public API is where design document \u{a7}08's \"stop, never guess\" is \
         enforced, so a way out of a divergence cannot be added without a reviewer writing \
         it down",
    )
}

/// The file whose timer semantics `timer-capability` pins.
pub const TIMER_SEMANTICS_PATH: &str = "waymaker-core/src/timer.rs";

/// The file whose persistent-clock capability `timer-capability` pins.
pub const CLOCK_CAPABILITY_PATH: &str = "waymaker-embassy/src/clock.rs";

/// Every public function design document §11's timer semantics are allowed to have.
///
/// §02 decision 8 is that timer semantics match the hardware's clock and never pretend. The
/// way that is given back is an *addition*: a `TimerSpec::best_effort(capability)`, a
/// `Timer::arm_or_downgrade`, a `Timer::force_elapsed`, or a `Deadline::assume_elapsed`
/// would each break no layering rule, need no dependency, pass every other gate, and turn
/// §11 into a preference. A test cannot call a function that is not there, so the surface is
/// pinned and a way to pretend is a line a reviewer writes on purpose.
///
/// `deadline`, `recorded` and `rearmed_at` are issue #33's. The first two are the halves of
/// one round trip:
/// a spec becomes a clock kind and a number on media, and comes back. `recorded` is the one
/// that has to be weighed, because it *builds* a spec from a byte — and it is admissible for
/// exactly one reason, which is that it is total with no wildcard arm. A kind number this
/// firmware does not know has no spec, so an erased or zeroed byte cannot decode as a
/// policy. That is the reinterpretation §11 forbids, closed at the only place it could
/// arrive. The caller supplies the kind, so it is not a downgrade route either: the one
/// module that must never choose a kind is `waymaker-embassy`'s, where
/// [`CLOCK_SPEC_CONSTRUCTION`] already refuses every `TimerSpec` name but the persistent one.
///
/// `rearmed_at` is the third, and it is the one that decides what a recorded arming reading
/// means on the far side of a reset. It is admissible because it pretends nothing: it returns
/// one of the two readings it was handed and never a third, so it cannot credit an interval
/// that did not pass. What it must not become is a `TimerSpec::assume_elapsed` or a
/// `rearmed_at` that answers `now.saturating_sub(ticks)` — either would make a deadline
/// arrive because a firmware wanted it to, which is §02 decision 8's whole subject.
///
/// The pin fails in the other direction too: a name this file no longer declares means the
/// module was renamed or deleted and the pin has stopped checking anything.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const TIMER_SURFACE: &[&str] = &[
    "admits",
    "arm",
    "armed_at",
    "clock_kind",
    "deadline",
    "evaluate",
    "rearmed_at",
    "recorded",
    "spec",
];

/// Every public function the persistent-clock capability is allowed to have.
///
/// The façade half of the same decision. `PersistentTimer::arm` takes a `&mut C:
/// PersistentClock`, and it is the only constructor, so a firmware with no clock cannot
/// write the call — that is issue #32's compile-time half. A `PersistentTimer::assume`, a
/// `PersistentClock::now_or_zero`, or a second constructor taking a reading the caller chose
/// would each give it back, and each would pass every other gate.
///
/// `now` is on the list because a trait method is callable without `pub` on it.
///
/// Sorted, for [`TIMER_SURFACE`]'s reason.
pub const CLOCK_SURFACE: &[&str] = &["arm", "now", "poll", "timer"];

/// The two timer vocabularies, and the members each may declare.
///
/// §11 offers two deadlines and this workspace services two clocks. Both are wire-format
/// commitments as much as API ones: issue
/// [#33](https://github.com/madmax983/waymaker/issues/33)'s `TimerScheduled` record carries
/// the clock kind so that "recovery cannot silently reinterpret one policy as another", and
/// a third policy is a number spent on media for the life of the format. `Deadline` is here
/// because a third answer — a "probably elapsed", a "cannot tell" — is exactly the pretence
/// §02 decision 8 forbids.
pub const TIMER_TYPES: &[BoundaryType] = &[
    BoundaryType {
        header: "pub enum TimerSpec",
        members: &["AfterBoot", "AtPersistentTime"],
    },
    BoundaryType {
        header: "pub enum ClockCapability",
        members: &["BootOnly", "Persistent"],
    },
    BoundaryType {
        header: "pub enum Deadline",
        members: &["Elapsed", "Remaining"],
    },
];

/// What the persistent-clock module may not name, and why.
///
/// Matched as *identifiers*, over code with its comments and string literals stripped, for
/// `kernel-boundary`'s reason: a spelling ban is evaded by a longer path that ends in the
/// same segments, and a doc comment explaining the ban would otherwise trip it.
///
/// Both are the boot clock. A module whose whole purpose is the clock that survives power
/// loss has no honest use for either: reaching for the boot spec is substituting one policy
/// for the other, and reaching for the boot-only capability is fabricating a refusal or a
/// permission the caller did not give. Neither breaks a layering rule and neither needs a
/// dependency, which is why the ban is mechanical.
pub const CLOCK_FORBIDDEN_VOCABULARY: &[(&str, &str)] = &[
    (
        "AfterBoot",
        "is the boot deadline; a persistent-clock module that names it is substituting one \
         clock policy for another, which is the downgrade design document §11 forbids",
    ),
    (
        "BootOnly",
        "is the capability of a firmware with no persistent clock; a module reached only \
         through a clock cannot honestly claim it",
    ),
];

/// Each timer type and the methods it may declare, at every visibility.
///
/// [`TIMER_SURFACE`] counts `pub ` and not `pub(`, which is not enough here. Review of this
/// change added `pub(crate) const fn arm_or_downgrade(spec, capability, now) -> Self` to
/// `impl Timer` and watched the gate stay green — the same mutation ADR 0025's review used
/// on `DurableIntent`, and `pub(crate)` is reach enough for a downgrade, because rung 0.4's
/// `Ctx` lands in this crate. `effect-protocol` closed it by pinning method *sets* at every
/// visibility; this is that guard, for the module where the policy lives.
pub const TIMER_TYPE_METHODS: &[(&str, &[&str])] = &[
    ("Timer", &["arm", "armed_at", "evaluate", "spec"]),
    (
        "TimerSpec",
        &["clock_kind", "deadline", "rearmed_at", "recorded"],
    ),
    ("ClockCapability", &["admits"]),
];

/// The timer types that must stay braced structs with no public field.
///
/// `pub struct Timer { pub spec: TimerSpec, pub armed_at: u64 }` adds no function, changes no
/// enum member, and makes the invariant the whole design rests on — a timer holds the spec it
/// was armed from — a field any caller can set. `effect-protocol` pins exactly this for
/// `DurableIntent`, and for the same reason: a public field is a constructor.
pub const TIMER_BRACED_STRUCTS: &[&str] = &["Timer"];

/// The one way `waymaker-embassy`'s clock module may name a `TimerSpec`.
///
/// An identifier blacklist closes one spelling at a time, and review of this change walked
/// straight past it: `pub const BEST_EFFORT: Self = Self::AfterBoot { ticks: 0 };` on
/// `impl TimerSpec`, reached from `arm` as `TimerSpec::BEST_EFFORT` behind a plausible
/// "epoch not restored yet" guard. Neither file named `AfterBoot` or `BootOnly`, no surface
/// changed, and the whole pipeline was green on a persistent deadline served by a clock that
/// restarts on every reset.
///
/// So the pin is positive rather than negative, which is `effect-protocol`'s move: the
/// module must name a spec — a pin that matches nothing checks nothing — and every name it
/// gives one must be this one. That closes the associated-const route and every spelling
/// nobody has thought of, where a blacklist closes the two that were.
pub const CLOCK_SPEC_CONSTRUCTION: &str = "TimerSpec::AtPersistentTime";

/// The file whose board RTC `timer-capability` pins.
pub const RIG_RTC_PATH: &str = "waymaker-rig/src/rtc.rs";

/// The file whose externally-restored epoch `timer-capability` pins.
pub const RIG_EPOCH_PATH: &str = "waymaker-rig/src/epoch.rs";

/// Every public function the board RTC is allowed to have.
///
/// `counter` and `continuity` are the board's two register reads, and `now` is the reading
/// they produce. `over` is the one constructor.
///
/// What the list refuses is an accessor. An `Rtc::counter_unchecked`, an `Rtc::assume_held`
/// or an `Rtc::set` would each hand out a number the continuity register never vouched for,
/// break no layering rule, need no dependency, and pass every other gate. A backup domain
/// that lost power reads zero on most parts, and zero is below every instant a workflow
/// waits for, so such a reading fires every persistent deadline on the device at once.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const RIG_RTC_SURFACE: &[&str] = &["continuity", "counter", "now", "over"];

/// Every public function the externally-restored epoch is allowed to have.
///
/// `ticks` is the board's boot clock, `awaiting` the one constructor, `restore` what the
/// firmware calls with the network's answer, and `now` the reading.
///
/// The absence is the same one. A `RestoredEpoch::assume`, a `now_or_zero`, or a second
/// constructor carrying an epoch nobody restored would each turn "this device does not know
/// the time" into a number — and that number is the boot clock's, which is design document
/// §11's downgrade arriving through a driver instead of through a spec.
///
/// Sorted, for [`RIG_RTC_SURFACE`]'s reason.
pub const RIG_EPOCH_SURFACE: &[&str] = &["awaiting", "now", "restore", "ticks"];

/// One board clock module, and everything `timer-capability` pins about it.
///
/// Two modules rather than one, because a pinned surface is a list of *names* and both
/// implement `PersistentClock`. One file holding both would declare `now` twice, and a pin
/// cannot speak about a name used twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoardClock {
    /// Where the module lives, relative to `crates/`.
    pub path: &'static str,
    /// Every public function it may declare.
    pub surface: &'static [&'static str],
    /// The driver type, whose fields must all be private.
    pub driver: &'static str,
    /// Every method the driver's inherent `impl` blocks may declare, at *any* visibility.
    pub methods: &'static [&'static str],
}

/// The board clock modules `timer-capability` pins.
///
/// `methods` names a private helper as well as the public ones, which reads as
/// over-specification and is the point: review of this change added
/// `pub(crate) fn counter_unchecked` to `impl Rtc` and watched the gate stay green, because
/// a surface pin counts `pub ` and not `pub(`. `TIMER_TYPE_METHODS` closed exactly that on
/// the kernel side, and CLAUDE.md had recorded the defeat; the board side was added without
/// it. So the list is every method, and a private helper is one a reviewer writes down.
pub const BOARD_CLOCK_MODULES: &[BoardClock] = &[
    BoardClock {
        path: RIG_RTC_PATH,
        surface: RIG_RTC_SURFACE,
        driver: "Rtc",
        methods: &["over"],
    },
    BoardClock {
        path: RIG_EPOCH_PATH,
        surface: RIG_EPOCH_SURFACE,
        driver: "RestoredEpoch",
        methods: &["awaiting", "reading_at", "restore"],
    },
];

/// What a board clock module may not name, beyond [`CLOCK_FORBIDDEN_VOCABULARY`].
///
/// Matched as identifiers over code with comments and string literals stripped, for
/// [`CLOCK_FORBIDDEN_VOCABULARY`]'s reason.
///
/// A driver reports a reading. It decides no policy and it answers for no image. Both names
/// below are policy, and a driver that reached for either would be deciding §02 decision 8
/// in the one place the kernel cannot see.
pub const BOARD_CLOCK_FORBIDDEN_VOCABULARY: &[(&str, &str)] = &[
    (
        "TimerSpec",
        "is a deadline policy; a clock driver reports a reading, and which deadline that \
         reading meets is decided in `waymaker-core` and nowhere a board can reach",
    ),
    (
        "ClockCapability",
        "is a firmware's declaration of which clocks it can service; a driver that names it \
         is answering for the image it happens to be linked into",
    ),
];

/// Rule: design document §11's timer semantics are the ones that were reviewed, and a
/// persistent deadline still needs a persistent clock.
///
/// Three halves, one id, because it is one decision. The kernel half pins the semantics
/// module's surface and the three vocabularies §11 and issue #33 rest on. The façade half
/// pins the capability's surface and refuses the boot clock's vocabulary in the one module
/// that exists because a boot clock is not good enough. The board half is issue
/// [#34](https://github.com/madmax983/waymaker/issues/34)'s two drivers: it pins what each
/// may declare and refuses both the boot vocabulary and any deadline policy, because a
/// reading enters the workspace there and a driver that could hand out one the hardware
/// never vouched for gives §02 decision 8 back from below the two pins above.
///
/// All three fail closed: a module the pin cannot find is a pin that has stopped checking,
/// which is the failure mode every rule here is written to avoid. Both read the file with
/// its `#[cfg(test)]` modules removed, for `integrity-check`'s reason — a downgrade written
/// under `cfg(test)` discharges nothing about the code that ships.
///
/// What it cannot see, so that nobody reads more into a green build than is there. It
/// compares *names*: an `admits` that stopped consulting its argument, or an `evaluate` that
/// credited an interval it could not measure, are invisible to it and are
/// `crates/waymaker-core/tests/timer.rs`'s. And it pins one file per half, exactly as
/// `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one they
/// pin: an `impl Timer { pub fn force(..) }` in a sibling module of `waymaker-core`, or a
/// `trait PersistentTimerExt` with a blanket impl beside the façade, adds the door with the
/// rule silent.
#[must_use]
pub fn check_timer_capability(
    sources: &[crate::size::LayerSource],
    rig_sources: &[crate::size::LayerSource],
) -> Vec<Violation> {
    const RULE: &str = "timer-capability";
    const KERNEL: &str = "waymaker-core";
    const FACADE: &str = "waymaker-embassy";

    let mut violations = check_pinned_surface(
        RULE,
        KERNEL,
        TIMER_SEMANTICS_PATH,
        TIMER_SURFACE,
        sources,
        "design document \u{a7}11's semantics are where \u{a7}02 decision 8 is enforced, so a \
         way to pretend that time passed cannot be added without a reviewer writing it down",
    );

    if let Some(source) = find_source(sources, TIMER_SEMANTICS_PATH) {
        let code = without_test_modules(&code_only(&source.contents));
        let pin = MemberPin {
            rule: RULE,
            subject: KERNEL,
            path: TIMER_SEMANTICS_PATH,
            table: "TIMER_TYPES",
            why: "\u{a7}11 offers two deadlines and this workspace services two clocks, and \
                  issue #33 puts the clock kind on media for the life of the format",
        };
        for pinned in TIMER_TYPES {
            violations.extend(check_boundary_type(&pin, &code, pinned));
        }
        violations.extend(check_timer_types(&code));
    }
    violations.extend(check_timer_root_reexport(sources));

    violations.extend(check_pinned_surface(
        RULE,
        FACADE,
        CLOCK_CAPABILITY_PATH,
        CLOCK_SURFACE,
        sources,
        "the capability is issue #32's compile-time half \u{2014} a persistent deadline needs \
         a clock in hand \u{2014} so a second route to one cannot be added without a reviewer \
         writing it down",
    ));

    if let Some(source) = find_source(sources, CLOCK_CAPABILITY_PATH) {
        let code = without_test_modules(&code_only(&source.contents));
        for (forbidden, why) in CLOCK_FORBIDDEN_VOCABULARY {
            if names_identifier(&code, forbidden) {
                violations.push(Violation::new(
                    RULE,
                    FACADE,
                    format!("{CLOCK_CAPABILITY_PATH} names `{forbidden}`, which {why}"),
                ));
            }
        }
        violations.extend(check_clock_spec_construction(&code));
    }

    violations.extend(check_board_clocks(rig_sources));

    violations
}

/// The board's half: the two drivers issue #34 brings, and the vocabulary neither may name.
///
/// A third half of one decision. The kernel decides what a deadline means, the façade holds
/// the capability, and a driver reports a reading. This is the pin on the third: a clock
/// driver that could hand out a number no register vouched for, or that reached for a
/// deadline policy of its own, would give back §02 decision 8 from the one place the two
/// pins above cannot see.
///
/// It fails closed for [`check_timer_capability`]'s reason: a module the pin cannot find is
/// a pin that has stopped checking.
fn check_board_clocks(rig_sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "timer-capability";
    const BOARD: &str = "waymaker-rig";

    let mut violations = Vec::new();
    for clock in BOARD_CLOCK_MODULES {
        violations.extend(check_pinned_surface(
            RULE,
            BOARD,
            clock.path,
            clock.surface,
            rig_sources,
            "a board clock is where a reading enters the workspace, so a way to hand out one \
             the hardware never vouched for cannot be added without a reviewer writing it \
             down",
        ));
        let Some(source) = find_source(rig_sources, clock.path) else {
            continue;
        };
        let code = without_test_modules(&code_only(&source.contents));
        for (forbidden, why) in CLOCK_FORBIDDEN_VOCABULARY
            .iter()
            .chain(BOARD_CLOCK_FORBIDDEN_VOCABULARY)
        {
            if names_identifier(&code, forbidden) {
                violations.push(Violation::new(
                    RULE,
                    BOARD,
                    format!("{} names `{forbidden}`, which {why}", clock.path),
                ));
            }
        }
        violations.extend(check_board_clock_driver(clock, &code));
        violations.extend(check_board_clock_has_no_constant(clock, &code));
    }
    violations
}

/// The driver type keeps its fields private and declares only the methods it is pinned for.
///
/// The two defeats CLAUDE.md records against the kernel half, closed here as well. A public
/// field is a constructor — `pub struct Rtc<R> { pub registers: R }` lets every caller reach
/// `rtc.registers.counter()` and skip the continuity check entirely — and a `pub(crate) fn`
/// is reach enough within a crate that already holds the rig the drivers are written for.
fn check_board_clock_driver(clock: &BoardClock, code: &str) -> Vec<Violation> {
    const RULE: &str = "timer-capability";
    const BOARD: &str = "waymaker-rig";

    let mut violations = Vec::new();
    let header = format!("pub struct {}", clock.driver);
    let declarations = declaration_count(code, &header);
    if declarations != 1 {
        violations.push(Violation::new(
            RULE,
            BOARD,
            format!(
                "{} declares `{header}` {declarations} times, not once; the scans below read \
                 the first, so a decoy above the real one is what they would check",
                clock.path
            ),
        ));
        return violations;
    }
    if !declares_braced_struct(code, &header) {
        violations.push(Violation::new(
            RULE,
            BOARD,
            format!(
                "`{}` is not a braced struct: the field scan reads the first `{{` after the \
                 declaration, so a tuple struct would have it reporting on whatever follows \
                 — and a `pub` tuple field is a register block anybody can read around the \
                 driver",
                clock.driver
            ),
        ));
        return violations;
    }
    if braced_body(code, &header).is_some_and(|body| count_tokens(body, "pub") != 0) {
        violations.push(Violation::new(
            RULE,
            BOARD,
            format!(
                "`{}` declares a public field: the whole of this driver is that a reading \
                 comes with the register that vouches for it, and a public field is a way \
                 round it that adds no function",
                clock.driver
            ),
        ));
    }

    let blocks = inherent_impl_bodies(code, clock.driver);
    if blocks.is_empty() {
        violations.push(Violation::new(
            RULE,
            BOARD,
            format!(
                "{} declares no inherent `impl` for `{}`, so its methods are pinned against \
                 nothing",
                clock.path, clock.driver
            ),
        ));
        return violations;
    }
    let body = blocks.join("\n");
    let declared = declared_function_names(&body);
    let mut expected: Vec<String> = clock
        .methods
        .iter()
        .map(|method| (*method).to_owned())
        .collect();
    expected.sort();
    if declared != expected {
        violations.push(Violation::new(
            RULE,
            BOARD,
            format!(
                "`{}` declares {declared:?} rather than {expected:?}: read at every \
                 visibility, because a surface pin counts `pub ` and not `pub(`, and this \
                 crate is the one the drivers were written for",
                clock.driver
            ),
        ));
    }
    violations
}

/// A board clock module declares no constant.
///
/// Refused outright rather than listed, and read over the whole module rather than over one
/// `impl` body. Review of this change added `impl Continuity { pub const ASSUME_HELD: Self =
/// Self::Held; }` and watched every other half of this rule stay green: it adds no function,
/// changes no enum member, and gives a caller a `Continuity` the hardware never reported.
/// These modules have no honest constant — every value in them comes from a register or from
/// the network — so a whitelist would be a list of one exception waiting to be added to.
fn check_board_clock_has_no_constant(clock: &BoardClock, code: &str) -> Vec<Violation> {
    const RULE: &str = "timer-capability";
    const BOARD: &str = "waymaker-rig";

    module_constants(code)
        .into_iter()
        .map(|constant| {
            Violation::new(
                RULE,
                BOARD,
                format!(
                    "{} declares the constant `{constant}`; a clock driver has no honest \
                     constant, and one that names a reading or a continuity is a value a \
                     caller can reach without changing a surface",
                    clock.path
                ),
            )
        })
        .collect()
}

/// Every `const` a module declares at any depth, `const fn` excepted.
///
/// [`declared_associated_constants`] reads one `impl` body and reports its top level. This
/// reads the file, because the door it is written against is an `impl` block on a *second*
/// type — the enum a driver answers with — which no per-driver pin looks at.
fn module_constants(code: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in code.lines() {
        let trimmed = line.trim();
        let rest = trimmed
            .strip_prefix("pub ")
            .or_else(|| {
                trimmed
                    .split_once(") ")
                    .filter(|(head, _)| head.starts_with("pub("))
                    .map(|(_, rest)| rest)
            })
            .unwrap_or(trimmed);
        if let Some(declaration) = rest.strip_prefix("const ")
            && !declaration.starts_with("fn ")
            && let Some(name) = declaration.split([':', ' ']).next()
            && !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            names.push(name.to_owned());
        }
    }
    names.sort_unstable();
    names
}

/// Every `TimerSpec` the clock module names is the persistent one, and it names at least one.
///
/// Both halves matter. A module that names none has a pin checking nothing; a module that
/// names another has a downgrade in the file whose whole purpose is that there is not one.
fn check_clock_spec_construction(code: &str) -> Vec<Violation> {
    const RULE: &str = "timer-capability";
    const FACADE: &str = "waymaker-embassy";
    const SPEC: &str = "TimerSpec";

    // Without the `use` declarations. An import names the type and constructs nothing, and a
    // module that may name exactly one spec still has to import it.
    let code: String = code
        .lines()
        .filter(|line| !line.trim_start().starts_with("use "))
        .collect::<Vec<&str>>()
        .join("\n");
    let code = code.as_str();

    let mut violations = Vec::new();
    let mut named = 0_usize;
    let continues = |character: char| character.is_alphanumeric() || character == '_';

    for (index, _) in code.match_indices(SPEC) {
        let is_identifier = code
            .get(..index)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|character| !continues(character));
        if !is_identifier {
            continue;
        }
        named = named.saturating_add(1);
        let rest = code.get(index..).unwrap_or_default();
        // A prefix is not a match. Codex found `TimerSpec::AtPersistentTimeFallback`, which
        // `starts_with` accepts and which an associated constant in the kernel — invisible to
        // a method pin that reads `fn` — can define as the boot spec. The boundary is what
        // makes the pin a name rather than a prefix, exactly as `names_identifier` does.
        let is_the_pinned_spec = rest.starts_with(CLOCK_SPEC_CONSTRUCTION)
            && rest
                .get(CLOCK_SPEC_CONSTRUCTION.len()..)
                .and_then(|tail| tail.chars().next())
                .is_none_or(|character| !continues(character));
        if !is_the_pinned_spec {
            let quoted: String = rest.chars().take(48).collect();
            violations.push(Violation::new(
                RULE,
                FACADE,
                format!(
                    "{CLOCK_CAPABILITY_PATH} names a `{SPEC}` other than \
                     `{CLOCK_SPEC_CONSTRUCTION}`, at `{quoted}`; the persistent-clock module \
                     may reach exactly one spec, so an associated constant or any other \
                     spelling cannot stand in for a boot deadline"
                ),
            ));
        }
    }

    if named == 0 {
        violations.push(Violation::new(
            RULE,
            FACADE,
            format!(
                "{CLOCK_CAPABILITY_PATH} names no `{SPEC}`, so the construction pin is \
                 checking nothing; the module exists to build `{CLOCK_SPEC_CONSTRUCTION}` and \
                 nothing else"
            ),
        ));
    }

    violations
}

/// The timer types are declared once, keep their methods, and expose no field.
fn check_timer_types(code: &str) -> Vec<Violation> {
    const RULE: &str = "timer-capability";
    const KERNEL: &str = "waymaker-core";

    let mut violations = Vec::new();

    for name in TIMER_BRACED_STRUCTS {
        let header = format!("pub struct {name}");
        let declarations = declaration_count(code, &header);
        if declarations != 1 {
            violations.push(Violation::new(
                RULE,
                KERNEL,
                format!(
                    "{TIMER_SEMANTICS_PATH} declares `{header}` {declarations} times, not \
                     once; the scans below read the first, so a decoy above the real one is \
                     what they would check"
                ),
            ));
            continue;
        }
        if !declares_braced_struct(code, &header) {
            violations.push(Violation::new(
                RULE,
                KERNEL,
                format!(
                    "`{name}` is not a braced struct: the field scan reads the first `{{` \
                     after the declaration, so a tuple struct would have it reporting on \
                     whatever follows — and a `pub` tuple field is a spec anybody can rewrite"
                ),
            ));
            continue;
        }
        if braced_body(code, &header).is_some_and(|body| count_tokens(body, "pub") != 0) {
            violations.push(Violation::new(
                RULE,
                KERNEL,
                format!(
                    "`{name}` declares a public field: the invariant this design rests on is \
                     that a timer holds the spec it was armed from, and a public field makes \
                     that a value any caller can set"
                ),
            ));
        }
    }

    for (name, methods) in TIMER_TYPE_METHODS {
        let blocks = inherent_impl_bodies(code, name);
        if blocks.is_empty() {
            violations.push(Violation::new(
                RULE,
                KERNEL,
                format!(
                    "{TIMER_SEMANTICS_PATH} declares no inherent `impl` for `{name}`, so its \
                     methods are pinned against nothing"
                ),
            ));
            continue;
        }
        let body = blocks.join("\n");
        let declared = declared_function_names(&body);
        let mut expected: Vec<String> = methods.iter().map(|method| (*method).to_owned()).collect();
        expected.sort();
        if declared != expected {
            violations.push(Violation::new(
                RULE,
                KERNEL,
                format!(
                    "`{name}` declares {declared:?} rather than {expected:?}: read at every \
                     visibility, because a surface pin counts `pub ` and not `pub(`, and a \
                     `pub(crate) fn arm_or_downgrade` is reach enough for rung 0.4's `Ctx`, \
                     which lands in this crate"
                ),
            ));
        }
        // And no associated constant at all, which is the door the pin above cannot see. A
        // `pub const BEST_EFFORT: Self = Self::AfterBoot { ticks: 0 };` adds no function and
        // changes no member, and `TimerSpec::BEST_EFFORT` behind an "epoch not restored yet"
        // guard is the downgrade §02 decision 8 forbids. Issue #99 records it against the
        // façade, where `CLOCK_SPEC_CONSTRUCTION` at least pins the spellings; issue #33 made
        // `transition.rs` a `TimerSpec`-constructing module with no such pin, so the door is
        // inside the kernel now. Refused outright rather than listed: these three types have
        // no honest associated constant, so a whitelist would be a list of one exception
        // waiting to be added to.
        for constant in declared_associated_constants(&body) {
            violations.push(Violation::new(
                RULE,
                KERNEL,
                format!(
                    "`{name}` declares the associated constant `{constant}`, which the method \
                     pin cannot see; a constant that names a deadline is a spec any caller \
                     can reach without changing a surface"
                ),
            ));
        }
    }

    violations
}

/// The pinned types are the ones the crate root re-exports.
///
/// `check_boundary_type` reads a header string, so it is defeated by a rename that leaves a
/// decoy behind: rename the shipped `pub enum TimerSpec` to `TimerSpecV2`, keep a
/// `mod compat { pub enum TimerSpec { AfterBoot, AtPersistentTime } }`, and the member pin
/// finds the decoy, declared once, with exactly the pinned members. Review of this change ran
/// that and shipped a third policy with the gate green.
///
/// The crate root is what closes it. `waymaker-core` re-exports its vocabulary, so a renamed
/// type either loses its re-export — reported here — or keeps it, and then the decoy and the
/// real type collide on one name in one `pub use`, which does not compile.
fn check_timer_root_reexport(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "timer-capability";
    const KERNEL: &str = "waymaker-core";
    const ROOT: &str = "waymaker-core/src/lib.rs";

    let Some(source) = find_source(sources, ROOT) else {
        return vec![Violation::new(
            RULE,
            KERNEL,
            format!(
                "no {ROOT} in the workspace, so nothing shows that the pinned timer types are \
                 the ones this crate ships"
            ),
        )];
    };

    let code = without_test_modules(&code_only(&source.contents));
    let exported = reexported_from_timer(&code);

    TIMER_TYPES
        .iter()
        .map(|pinned| pinned.header)
        .chain(TIMER_BRACED_STRUCTS.iter().copied())
        .filter_map(|header| header.rsplit(' ').next())
        .filter(|name| !exported.contains(*name))
        .map(|name| {
            Violation::new(
                RULE,
                KERNEL,
                format!(
                    "{ROOT} does not re-export `timer::{name}`, so the pinned type is not the \
                     one the crate ships; a rename that leaves a decoy behind defeats a pin \
                     that only reads a header string"
                ),
            )
        })
        .collect()
}

/// The names `pub use timer::…` re-exports, as they are spelled *in the module*.
///
/// The source name and not the alias. Codex found the version that only asked whether the
/// crate root mentioned the identifier: `pub use timer::TimerPolicy as TimerSpec;` mentions
/// it, so the decoy survived and the rename scenario this function exists to close stayed
/// open. What is compared now is the left-hand side, which is the name the member pin read.
///
/// Both spellings are read — `pub use timer::Timer;` and a braced list — and a list may span
/// lines, so the scan runs to the `;` rather than to the end of a line. An entry that still
/// holds a `::` is not one of these: `pub use timer::compat::TimerSpec` re-exports whatever
/// is in `compat`, which is the decoy the rename left behind.
fn reexported_from_timer(code: &str) -> BTreeSet<&str> {
    const PREFIX: &str = "pub use timer::";

    let mut exported = BTreeSet::new();
    for (index, _) in code.match_indices(PREFIX) {
        let rest = code.get(index.saturating_add(PREFIX.len())..).unwrap_or("");
        let Some(end) = rest.find(';') else { continue };
        let list = rest.get(..end).unwrap_or("");
        for entry in list
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .split(',')
        {
            // The source name, before any `as`. A remaining `::` disqualifies it: the pin is
            // that `timer::<Name>` is the type the crate ships, and a
            // `pub use timer::compat::TimerSpec` re-exports the decoy rather than the type
            // the member pin read.
            let source = entry.split(" as ").next().unwrap_or("").trim();
            if !source.is_empty() && !source.contains("::") {
                exported.insert(source);
            }
        }
    }
    exported
}

/// The file whose public surface [`STORAGE_CONTRACT_SURFACE`] pins.
pub const STORAGE_CONTRACT_PATH: &str = "waymaker-flash/src/storage.rs";

/// Every public function design document §12's storage contract is allowed to have.
///
/// §05 says a host or browser adapter "must not expand the firmware traits to accommodate
/// host conveniences", and §12 is the firmware trait it is talking about. That sentence is
/// a rule about *absence*: a `read_all`, a `write_at`, a `flush`, a `Geometry::from_bytes`
/// or a `capacity()` shortcut on the trait would each break no layering rule, need no
/// dependency, pass every other gate, and turn a four-operation contract every port must
/// implement into a surface only a host can afford. A test cannot call a method that is not
/// there, so the surface is pinned instead and a fifth operation is a line a reviewer
/// writes on purpose.
///
/// The pin fails in the other direction too, which matters as much: a name this file no
/// longer declares means the contract was renamed or deleted and the pin has stopped
/// checking anything. `fmt` is on the list because a trait `impl`'s methods are callable
/// without `pub`, and `message` because a driver with no console still has to report
/// something.
///
/// What it does **not** catch: this compares *names*. A `&mut self` turned into `&self`, an
/// offset widened to `u64`, or a validator that stopped validating are all invisible to it
/// and are a reviewer's job. `tests/storage.rs` is what holds the behaviour.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const STORAGE_CONTRACT_SURFACE: &[&str] = &[
    "barrier",
    "capacity",
    "erase",
    "erase_blocks",
    "erase_size",
    "fmt",
    "geometry",
    "message",
    "new",
    "program",
    "program_size",
    "read",
    "read_size",
    "validate_erase",
    "validate_program",
    "validate_read",
];

/// Rule: the storage contract's public surface is exactly the one that was reviewed.
///
/// The same shape as [`check_replay_cursor_surface`], for design document §12's trait and
/// the geometry that guards it.
#[must_use]
pub fn check_storage_contract(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    check_pinned_surface(
        "storage-contract",
        "waymaker-flash",
        STORAGE_CONTRACT_PATH,
        STORAGE_CONTRACT_SURFACE,
        sources,
        "design document \u{a7}05 says a host adapter must not expand the firmware traits to \
         accommodate host conveniences, so a fifth storage operation cannot be added without \
         a reviewer writing it down",
    )
}

/// The file whose public surface [`RECOVERY_SURFACE`] pins.
pub const RECOVERY_SURFACE_PATH: &str = "waymaker-flash/src/recovery.rs";

/// Every public function the storage-backed recovery of issue #23 is allowed to have.
///
/// Design document §02 decision 2 — "a cursor advances through history in workflow order;
/// there is no `Journal::get(id)` and no in-memory event index" — is a rule about the
/// *reader* as much as about the cursor, and this is the reader that touches media. A
/// `seek(offset)`, a `resume_at(offset)`, a `rewind`, or a `read_all(&mut self) -> Vec<_>`
/// would each break no layering rule, need no dependency, pass every other gate, and turn a
/// forward scan whose RAM is one caller-owned page into one that either seeks into the
/// middle of history or holds it.
///
/// One name on this list is load-bearing for a different reason. `append_offset` is the only
/// way an offset leaves this module, and it answers `Some` only for a scan that ran to
/// erased media — because appending anywhere else programs cells a cycle has already
/// cleared, and on NOR that bank never boots again. A second accessor that returned the
/// stopping offset regardless is the mutation `waymaker-fault`'s sweep demonstrates as
/// dangerous, and it is a line a reviewer has to write on purpose.
///
/// The pin fails in the other direction too, which matters as much: a name this file no
/// longer declares means the reader was renamed or deleted and the pin has stopped checking
/// anything. `fmt` is on the list because a trait `impl`'s methods are callable without
/// `pub`, and `message` because a device with no console still has to report something.
///
/// What it does **not** catch: this compares *names*. An `offset` widened to `u64`, or an
/// `append_offset` that started answering for a damaged journal, are both invisible to it —
/// `crates/waymaker-flash/tests/recovery.rs` and `waymaker-fault`'s crash sweep are what
/// hold the behaviour.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const RECOVERY_SURFACE: &[&str] = &[
    "align",
    "append_offset",
    "base",
    "bytes",
    "ending",
    "fmt",
    "message",
    "new",
    "next",
    "of",
    "offset",
    "region",
    "spanning",
    "with_integrity",
];

/// Rule: the recovery reader's public surface is exactly the one that was reviewed.
///
/// The same shape as [`check_replay_cursor_surface`], for the reader that walks a journal on
/// media rather than one in RAM.
#[must_use]
pub fn check_recovery_surface(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    check_pinned_surface(
        "recovery-surface",
        "waymaker-flash",
        RECOVERY_SURFACE_PATH,
        RECOVERY_SURFACE,
        sources,
        "the reader's public API is where design document \u{a7}02 decision 2 and the rule \
         that an append offset is only ever erased media are both enforced, so a seek or a \
         second way to an offset cannot be added without a reviewer writing it down",
    )
}

/// The file whose public surface [`check_rig_oracle`] pins: the rig's oracle.
pub const RIG_AUDIT_PATH: &str = "waymaker-rig/src/audit.rs";

/// The file whose public surface [`check_rig_oracle`] also pins: the rig's census.
///
/// The census lives apart from `phase.rs` for this rule's sake: `Phase` and `ResetCause` each
/// declare an `index`, a `from_index` and a `name`, and a pin that compares names cannot tell
/// two such declarations apart — which is exactly the hole
/// `check_pinned_surface`'s duplicate check refuses to leave open.
pub const RIG_CENSUS_PATH: &str = "waymaker-rig/src/census.rs";

/// Every public function the rig's oracle is allowed to have.
///
/// Issue [#27](https://github.com/madmax983/waymaker/issues/27) asks for a rig that "verifies
/// recovery against the oracle". The whole of that is an *absence*, and it is the same
/// absence [`APPEND_SURFACE`] and [`CAPACITY_SURFACE`] pin: an `Audit::assume_passed`, an
/// `Audit::ignore`, a `Breach::suppress` or a second `finish` that took the authority count
/// as advisory would each break no other rule, need no dependency, pass every test that
/// exists — and turn an instrument into a formality.
///
/// A rig is the one piece of code in this workspace whose bugs are *invisible*: a firmware
/// bug shows up as a failing test, and a rig bug shows up as a passing one.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const RIG_AUDIT_SURFACE: &[&str] = &[
    "code",
    "finish",
    "fmt",
    "name",
    "name_of_code",
    "new",
    "progress",
    "recovered",
    "saw",
];

/// Every public function the rig's census is allowed to have.
///
/// `Coverage::verdict` is what makes "the rig ran
/// for an hour" different from "the rig covered the six cells", and issue #27 asks for both
/// reset causes at all three write points. A `Coverage::force_complete`, a
/// `Coverage::assume_covered` or a `Gap::ignore` would each give that back in one line.
///
/// `saturated` is on the list deliberately: it raises a cell to its ceiling, which the census
/// already treats as covered, so it grants nothing a long run would not. It exists because
/// the saturation behaviour is otherwise only reachable by four billion iterations, and an
/// overflow rule nobody can test is an overflow rule nobody has tested.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const RIG_CENSUS_SURFACE: &[&str] = &[
    "cause",
    "fmt",
    "iterations",
    "phase",
    "record",
    "saturated",
    "total",
    "verdict",
];

/// The file whose public surface [`check_rig_oracle`] also pins: the rig's runner.
///
/// The oracle decides; this is where the decision is *produced*. A `Rig::verify_lenient`, or
/// a `judge` that swallowed an [`Outcome`](waymaker_rig::log::Outcome)'s breach, would give
/// the instrument back from a file the other two pins cannot see — which is the same hole
/// `recovery-surface`, `storage-contract` and `capacity-reserve` each pin one file against.
pub const RIG_RUN_PATH: &str = "waymaker-rig/src/run.rs";

/// Every public function the rig's runner is allowed to have.
///
/// `marks_per_run` is on the list rather than private because it is the arithmetic
/// `Rig::new` refuses a too-small instrument against, and a caller sizing a witness region
/// needs the same number. It grants nothing: it is a pure function of an effect count.
///
/// `outcome`, `recovered` and `banks` are `Verdict`'s accessors. They are what makes a logged
/// breach investigable rather than merely named — a `record-differs` is caused by bytes a log
/// line cannot carry, so the line carries what recovery *produced* instead. Read-only, and on
/// a type the rig constructs: there is no way to build a `Verdict` from outside this file,
/// which is what keeps them evidence rather than an input.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const RIG_RUN_SURFACE: &[&str] = &[
    "banks",
    "cut_at",
    "effects",
    "entry",
    "instrument_base",
    "iterate",
    "judge",
    "layout",
    "marks_per_run",
    "new",
    "outcome",
    "part",
    "plan",
    "prepare",
    "recovered",
    "reset_budget",
    "resume",
    "verify",
    "witness_region",
    "workload",
];

/// The file whose public surface [`check_rig_oracle`] also pins: the failure matrix.
///
/// Issue [#31](https://github.com/madmax983/waymaker/issues/31)'s ten rows and the census over
/// them. A `Matrix::force_complete` or a `Gap::ignore` would let a sweep that reached six rows
/// report ten, which is the relabelling the census exists to refuse.
pub const RIG_MATRIX_PATH: &str = "waymaker-rig/src/matrix.rs";

/// Every public function the failure matrix is allowed to have.
///
/// `saturated` is on the list for [`RIG_CENSUS_SURFACE`]'s reason.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const RIG_MATRIX_SURFACE: &[&str] = &[
    "fmt",
    "from_index",
    "id",
    "index",
    "iterations",
    "record",
    "row",
    "saturated",
    "total",
    "verdict",
];

/// Rule: the rig's oracle and its census are exactly the surfaces that were reviewed.
///
/// Two files, one rule id, because they are one decision: what the instrument is obliged to
/// notice. A rig that could be told to overlook a breach, or to call a run covered that was
/// not, is a rig whose green runs mean nothing — and unlike every other pin in this file, the
/// failure it prevents is silent by construction.
#[must_use]
pub fn check_rig_oracle(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    let mut violations = check_pinned_surface(
        "rig-oracle",
        "waymaker-rig",
        RIG_AUDIT_PATH,
        RIG_AUDIT_SURFACE,
        sources,
        "the oracle's public API is where design document \u{a7}14's guarantees are actually \
         demanded of a recovery, so a way to pass without demanding them cannot be added \
         without a reviewer writing it down",
    );
    violations.extend(check_pinned_surface(
        "rig-oracle",
        "waymaker-rig",
        RIG_CENSUS_PATH,
        RIG_CENSUS_SURFACE,
        sources,
        "the census is what makes an uncovered cell a refusal rather than a silence, so a \
         way to call a run covered that was not cannot be added without a reviewer writing \
         it down",
    ));
    violations.extend(check_pinned_surface(
        "rig-oracle",
        "waymaker-rig",
        RIG_RUN_PATH,
        RIG_RUN_SURFACE,
        sources,
        "the runner is where the oracle's verdict is produced, so a lenient verify or a \
         judge that swallowed a breach cannot be added without a reviewer writing it down",
    ));
    violations.extend(check_pinned_surface(
        "rig-oracle",
        "waymaker-rig",
        RIG_MATRIX_PATH,
        RIG_MATRIX_SURFACE,
        sources,
        "an unreached row is a refusal, not a silence; a way to mark a row reached needs a \
         reviewer to write it down",
    ));
    violations
}

/// The file whose public surface and typestate [`check_commit_discipline`] pins.
pub const APPEND_SURFACE_PATH: &str = "waymaker-flash/src/append.rs";

/// Every public function the two-barrier writer of issue #24 is allowed to have.
///
/// Design document §07 puts a payload barrier between a record's frame body and its commit
/// seal, and issue [#24](https://github.com/madmax983/waymaker/issues/24) asks for a writer
/// in which programming the seal without it is not expressible. The whole of that guarantee
/// is an *absence*: a `Staged::commit`, a `Staged::seal`, a `Journal::write` that did all
/// four steps in one call, or a second constructor for `Sealable` would each break no
/// layering rule, need no dependency, pass every other gate — and turn a protocol back into
/// a convention.
///
/// `after` is on this list for a second reason. It is the only constructor a [`Journal`] has,
/// and it takes a finished recovery, so a writer cannot be pointed at an offset no scan
/// vouched for. A `Journal::at(region, offset)` is the mutation `waymaker-fault`'s sweep
/// demonstrates as dangerous, and it is a line a reviewer has to write on purpose.
///
/// What §10's capacity reserve needed from this type is deliberately *not* here. It reads the
/// writer's region — its granularity and its size — to price a record, and that accessor is
/// `pub(crate)`: a same-crate caller is served without widening the surface this rule exists
/// to make expensive, and without obliging the size probe to link a call it has no use for.
///
/// What it does **not** catch: this compares *names*. It is the surface half of the rule;
/// [`check_commit_discipline`] also checks the shape the names sit in.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
///
/// [`Journal`]: https://github.com/madmax983/waymaker/blob/main/crates/waymaker-flash/src/append.rs
pub const APPEND_SURFACE: &[&str] = &[
    "after",
    "amplification",
    "barriers",
    "commit",
    "offset",
    "overhead_bytes",
    "payload_barrier",
    "payload_bytes",
    "plus",
    "program_operations",
    "programmed_bytes",
    "room",
    "stage",
];

/// The typestate's three types, in the order a record passes through them.
///
/// A record is staged, then barriered, then committed, and the middle step is the whole
/// point: the second type is reachable only from the first type's one method.
pub const APPEND_TYPESTATE: [&str; 2] = ["Staged", "Sealable"];

/// The one method a staged frame is allowed to have.
pub const APPEND_BARRIER_STEP: &str = "payload_barrier";

/// The one method a sealable frame is allowed to have.
pub const APPEND_COMMIT_STEP: &str = "commit";

/// The whole expression the payload barrier must be taken with.
///
/// A pin on the *spelling*, like [`RECOVERY_ROUTING_STEPS`], and for a sharper reason than
/// usual. "Calls `storage.barrier` exactly once and uses the answer" is satisfied by
/// `let ordered = storage.barrier(); let _ = ordered.is_ok();` — a body that hands back a
/// [`Sealable`] after a barrier that *failed*, which is the one state
/// `payload_barrier`'s postcondition says cannot exist. Review of this change wrote exactly
/// that and watched the rule stay green.
///
/// So what is pinned is the propagation, characters and all. Whitespace is normalised before
/// the comparison, so rustfmt may break the line; anything else is a line a reviewer writes.
///
/// [`Sealable`]: https://github.com/madmax983/waymaker/blob/main/crates/waymaker-flash/src/append.rs
pub const APPEND_BARRIER_CALL: &str = "storage.barrier().map_err(AppendError::Storage)?";

/// The whole expression a commit seal must be programmed with.
///
/// §07 step 3's other half. [`APPEND_BARRIER_CALL`] pins the *payload* barrier, and until a
/// review of issue #26's swap found the same shape missing there, the commit's own program
/// and barrier were pinned by nothing: a `let _ = storage.barrier();` here leaves a caller
/// holding a `WriteAmplification` for a record whose seal never became durable, and the
/// journal advances over it. Neither the fault sweep nor any test can see it —
/// `waymaker_fault::Device`'s barrier changes no media, so the image is identical either way.
pub const APPEND_COMMIT_CALL: &str =
    "storage.program(self.seal_at, self.seal).map_err(AppendError::Storage)?";

/// Rule: a commit seal cannot be programmed without the payload barrier in front of it.
///
/// Issue #24's second "done when" is a `compile_fail` doctest in the crate itself, which
/// proves the type has no `commit` today. This is the other half — that it still has none
/// tomorrow — and it is four checks rather than one, because each of them is a different way
/// to give the guarantee back:
///
/// * the file's public surface is [`APPEND_SURFACE`], so a `Staged::commit` is a line
///   somebody adds to a list a reviewer reads;
/// * the `Staged` impl declares exactly one method, and it is
///   [`APPEND_BARRIER_STEP`] — the type with the frame body on media can do one thing;
/// * that impl programs nothing, so a "convenience" that wrote the seal early would have
///   nowhere to live;
/// * and `Sealable` is constructed in exactly one place, which is the barrier's own body. A
///   second construction anywhere is a second route to the type that programs seals, and the
///   `compile_fail` doctest would keep passing beside it.
///
/// What it cannot see: whether the barrier is a *real* barrier. `payload_barrier` calling
/// `storage.barrier()` is checked; that the driver beneath it establishes durability is
/// §12's contract and `waymaker-conformance`'s across-reset witness, not a scanner's.
#[must_use]
pub fn check_commit_discipline(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "commit-discipline";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = check_pinned_surface(
        RULE,
        ADAPTER,
        APPEND_SURFACE_PATH,
        APPEND_SURFACE,
        sources,
        "the writer's public API is where design document \u{a7}07's payload barrier stops \
         being a convention, so a way to program a seal without one cannot be added by \
         accident",
    );

    let Some(source) = find_source(sources, APPEND_SURFACE_PATH) else {
        // `check_pinned_surface` has already reported the missing file.
        return violations;
    };
    violations.extend(check_append_typestate(&source.contents));
    violations
}

/// The typestate half of [`check_commit_discipline`], over one file's text.
///
/// Split out because clippy's line budget refuses the two together, and because the surface
/// half is a set comparison and this half is a shape: they fail for different reasons and a
/// reader chasing one does not have to read the other.
fn check_append_typestate(contents: &str) -> Vec<Violation> {
    const RULE: &str = "commit-discipline";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = Vec::new();
    let code = without_test_modules(&code_only(contents));

    let [staged, sealable] = APPEND_TYPESTATE;
    let staged_impls = inherent_impl_bodies(&code, staged);
    if staged_impls.is_empty() {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "{APPEND_SURFACE_PATH} declares no inherent `impl` for `{staged}`, so the \
                 state a frame body waits in is pinned against nothing"
            ),
        ));
        return violations;
    }
    let staged_impl = staged_impls.join("\n");

    let staged_methods = declared_function_names(&staged_impl);
    if staged_methods != vec![APPEND_BARRIER_STEP.to_owned()] {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`{staged}` declares {staged_methods:?} rather than only \
                 `{APPEND_BARRIER_STEP}`: a frame body on media may do exactly one thing, \
                 and \u{a7}07 says which"
            ),
        ));
    }
    if count_tokens(&staged_impl, "program") != 0 {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`{staged}` names `program`: the type that holds an unsealed frame must not \
                 be able to write one, which is the whole of issue #24's \"not possible to \
                 program a seal without the intervening payload barrier having returned\""
            ),
        ));
    }

    let sealable_impls = inherent_impl_bodies(&code, sealable);
    if sealable_impls.is_empty() {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "{APPEND_SURFACE_PATH} declares no inherent `impl` for `{sealable}`, so the \
                 only type that may program a commit seal is pinned against nothing"
            ),
        ));
    } else {
        let methods = declared_function_names(&sealable_impls.join("\n"));
        if methods != vec![APPEND_COMMIT_STEP.to_owned()] {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{sealable}` declares {methods:?} rather than only \
                     `{APPEND_COMMIT_STEP}`: the one type that may program a seal should do \
                     nothing else"
                ),
            ));
        }
    }

    // One construction, and it is the barrier's. A `Sealable { .. }` anywhere else is a
    // second route to the type that programs seals, and the `compile_fail` doctest in the
    // crate would keep passing beside it.
    let constructions = struct_literals(&code, sealable);
    let barrier_body = braced_body(&code, &format!("fn {APPEND_BARRIER_STEP}"));
    let in_barrier = barrier_body.map_or(0, |body| struct_literals(body, sealable));
    if constructions == 0 || in_barrier == 0 || constructions != in_barrier {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`{sealable}` is constructed {constructions} time(s), {in_barrier} of them \
                 inside `{APPEND_BARRIER_STEP}`: the only value that can program a commit \
                 seal must come from the barrier and from nowhere else"
            ),
        ));
    }
    let takes_the_barrier = barrier_body.is_some_and(|body| {
        invocation(body, "storage.barrier") == Invocation::Once
            && squeezed(body).contains(&squeezed(APPEND_BARRIER_CALL))
    });

    violations.extend(check_append_commit(&code));
    if !takes_the_barrier {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`{APPEND_BARRIER_STEP}` does not take the barrier as \
                 `{APPEND_BARRIER_CALL}`, exactly once: a body that calls it and does not \
                 propagate the failure hands back a value that may program a seal over a \
                 frame no barrier ever ordered"
            ),
        ));
    }

    violations
}

/// §07 step 3: the seal's own program, and the commit barrier after it.
///
/// Split out of [`check_append_typestate`] for clippy's line budget. The same pin the payload
/// barrier has had, for the step that had none until a review of issue #26's swap found the
/// same shape missing here.
fn check_append_commit(code: &str) -> Vec<Violation> {
    const RULE: &str = "commit-discipline";
    const ADAPTER: &str = "waymaker-flash";

    let commit = braced_body(code, &format!("fn {APPEND_COMMIT_STEP}")).map(tightened);
    let sealed_then_barriered = commit.as_ref().is_some_and(|body| {
        match (
            body.find(tightened(APPEND_COMMIT_CALL).as_str()),
            body.find(tightened(APPEND_BARRIER_CALL).as_str()),
        ) {
            (Some(programmed), Some(barriered)) => barriered > programmed,
            _ => false,
        }
    });
    if sealed_then_barriered {
        return Vec::new();
    }
    vec![Violation::new(
        RULE,
        ADAPTER,
        format!(
            "`{APPEND_COMMIT_STEP}` does not program the seal as `{APPEND_COMMIT_CALL}` and \
             then take `{APPEND_BARRIER_CALL}`: a commit that swallowed either would advance \
             the journal over a record whose seal is not durable, and no test in this \
             workspace can see it"
        ),
    )]
}

/// The file whose public surface and admission order [`check_capacity_reserve`] pins.
pub const CAPACITY_SURFACE_PATH: &str = "waymaker-flash/src/capacity.rs";

/// Every public function §10's capacity reserve is allowed to have.
///
/// Design document §10: "Waymaker reserves enough tail space for a terminal record or
/// `continue_as_new`. Ordinary effect scheduling fails early with `HistoryNearCapacity`; the
/// runtime never overwrites committed history to make room." Issue
/// [#25](https://github.com/madmax983/waymaker/issues/25) asks for that, and — as with §07's
/// two barriers — most of the guarantee is an *absence*.
///
/// A `Reserved::stage_unchecked`, a `Reserved::into_journal` handing the ungated writer back,
/// a `Reserve::none()`, or a `Reserve::for_bytes(tail)` taking the figure from the caller
/// instead of from a [`BankLayout`] would each break no layering rule, need no dependency,
/// pass every other gate, and turn a reserve into a suggestion. The last one is the sharpest:
/// a reserve is only a promise because a layout vouched for it, and a constructor that
/// accepted numbers would accept numbers that describe no device.
///
/// `journal` is on the list because a caller needs the writer's offset, room and write
/// amplification, and it returns a *shared* borrow — [`Journal::stage`] needs a unique one,
/// so it cannot be used to append around the reserve. A `journal_mut` would be exactly that,
/// and is the name this pin exists to make somebody write down.
///
/// What it does **not** catch: this compares *names*. A `tail_bytes` that quietly stopped
/// counting the outcome record is invisible to it —
/// `crates/waymaker-flash/tests/capacity.rs` is what holds the arithmetic, and
/// `a_terminal_only_reserve_strands_a_run_with_an_effect_outstanding` is what makes the
/// outcome term falsifiable.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
///
/// [`BankLayout`]: https://github.com/madmax983/waymaker/blob/main/crates/waymaker-flash/src/bank.rs
/// [`Journal::stage`]: https://github.com/madmax983/waymaker/blob/main/crates/waymaker-flash/src/append.rs
pub const CAPACITY_SURFACE: &[&str] = &[
    "admits",
    "bounds",
    "exit_bytes_after",
    "fmt",
    "for_layout",
    "journal",
    "kernel_error",
    "message",
    "over",
    "reserve",
    "rollover_bytes",
    "stage",
    "tail_bytes",
];

/// The whole expression a reserved append must take the admission decision with.
///
/// A pin on the *spelling*, like [`APPEND_BARRIER_CALL`], and for the same reason. "Calls
/// `admits` somewhere in the body" is satisfied by `let _ = self.reserve.admits(..);`, which
/// refuses nothing and programs the record anyway — and by an `admits` taken *after* the
/// delegation, which is a refusal that arrives once the frame body is already on media. §10
/// says scheduling fails **early**, and issue #25 asks that the failure "produces no
/// mutation at all"; §12 is explicit that a failed program may still have changed media, so
/// the only refusal that changes nothing is one taken before the device is called.
///
/// So what is pinned is the propagation, characters and all: the room comes from the journal
/// this writer gates rather than from an argument, and the failure is returned rather than
/// observed. Whitespace is normalised before the comparison, so rustfmt may break the line;
/// anything else is a line a reviewer writes.
pub const CAPACITY_ADMISSION_CALL: &str =
    "self.reserve.admits(record, self.journal.room()).map_err(ReservedError::Capacity)?;";

/// The delegation the admission has to come before.
pub const CAPACITY_DELEGATION: &str = "self.journal.stage(storage, record, page)";

/// The type whose one method §10's reserve is applied in.
///
/// The rule reads *this type's* inherent `impl` blocks rather than the first `fn stage` in
/// the file. `stage` is a collidable name, and a private decoy carrying the pinned admission
/// — invisible to the surface half, which counts only public functions — stood in for the
/// real one until review of this change tried it.
pub const CAPACITY_GATE: &str = "Reserved";

/// Rule: §10's reserve is applied before anything reaches media, and cannot be given back.
///
/// Two halves, for the reason [`check_commit_discipline`] has two: the surface is a set
/// comparison and the order is a shape, and they fail for different reasons.
#[must_use]
pub fn check_capacity_reserve(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "capacity-reserve";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = check_pinned_surface(
        RULE,
        ADAPTER,
        CAPACITY_SURFACE_PATH,
        CAPACITY_SURFACE,
        sources,
        "the reserve's public API is where design document \u{a7}10's \"the runtime never \
         overwrites committed history to make room\" stops being a sentence, so an ungated \
         writer or a reserve built from numbers rather than from a layout cannot be added \
         without a reviewer writing it down",
    );

    let Some(source) = find_source(sources, CAPACITY_SURFACE_PATH) else {
        // `check_pinned_surface` has already reported the missing file.
        return violations;
    };
    violations.extend(check_capacity_gate(&source.contents));
    violations
}

/// The shape half of [`check_capacity_reserve`], over one file's text.
///
/// Split out for the reason [`check_append_typestate`] is: the surface half is a set
/// comparison and this half is a shape, and a reader chasing one does not have to read the
/// other.
fn check_capacity_gate(contents: &str) -> Vec<Violation> {
    const RULE: &str = "capacity-reserve";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = Vec::new();
    let code = without_test_modules(&code_only(contents));

    // *This type's* blocks, not the first `fn stage` in the file. `inherent_impl_bodies`
    // reads every block for the type, which is the hole review closed in
    // `commit-discipline` and the same one a private `fn stage` decoy opened here.
    let blocks = inherent_impl_bodies(&code, CAPACITY_GATE);
    if blocks.is_empty() {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "{CAPACITY_SURFACE_PATH} declares no inherent `impl` for `{CAPACITY_GATE}`, \
                 so the one place \u{a7}10's reserve is applied is pinned against nothing"
            ),
        ));
        return violations;
    }
    let gated = blocks.join("\n");

    let staged = declared_function_names(&gated)
        .into_iter()
        .filter(|name| name == "stage")
        .count();
    if staged != 1 {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`{CAPACITY_GATE}` declares `stage` {staged} time(s) rather than once: the \
                 gate is one method, and a second one is a second way to the writer it wraps"
            ),
        ));
        return violations;
    }
    let Some(body) = braced_body(&gated, "fn stage") else {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!("`{CAPACITY_GATE}::stage` has no body the gate could read"),
        ));
        return violations;
    };

    // The *first* statement, not merely one that precedes the delegation. "Occurs before"
    // is satisfied by an admission inside `if false { .. }`, inside a closure nothing calls,
    // or guarded so that only some record kinds reach it — and the third is a plausible diff
    // that turns off the gate for exactly the records \u{a7}10 is about. Review of this change
    // wrote all three and watched the rule stay green.
    let tight = tightened(body);
    let admission = tightened(CAPACITY_ADMISSION_CALL);
    let delegation = tightened(CAPACITY_DELEGATION);
    if !tight.starts_with(&admission) {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`{CAPACITY_GATE}::stage` does not open with \
                 `{CAPACITY_ADMISSION_CALL}`: \u{a7}10 says scheduling fails *early* and issue \
                 #25 asks that the failure produce no mutation at all, so the decision is \
                 the first thing the body does or it is a decision taken too late"
            ),
        ));
    }
    if !tight.contains(&delegation) {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`{CAPACITY_GATE}::stage` does not delegate as `{CAPACITY_DELEGATION}`: the \
                 gate is a gate rather than a second writer, and what reaches media is \
                 `append`'s to say"
            ),
        ));
    }
    // A second admission is a second decision, and the one that mattered may be either.
    let admissions = count_tokens(body, "admits");
    if admissions != 1 {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`{CAPACITY_GATE}::stage` names `admits` {admissions} time(s) rather than \
                 once"
            ),
        ));
    }

    violations
}

/// The file whose public surface and step order [`check_swap_discipline`] pins.
pub const SWAP_SURFACE_PATH: &str = "waymaker-flash/src/swap.rs";

/// Every public function §10's bank swap is allowed to have.
///
/// Design document §10's two-bank lifecycle, as issue
/// [#26](https://github.com/madmax983/waymaker/issues/26) states it: seven steps, of which
/// three are barriers or the things a barrier separates. As with §07's two barriers, most of
/// the guarantee is an *absence*.
///
/// A `Prepared::commit` skipping the header, a `Staged::seal_now` skipping the payload
/// barrier, a `Swap::install(bank)` taking the bank to erase from its caller, an
/// `Installed::retire_now` reachable before the commit barrier, or a
/// `Swap::without_retiring()` that plans a swap while the old run still holds its writer —
/// each would break no layering rule, need no dependency, pass every other gate, and turn
/// §10's crash windows into a convention. The third is the sharpest: which bank a swap
/// erases is derived from the authority the device booted, and a parameter is how a device
/// erases the bank it is running from.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const SWAP_SURFACE: &[&str] = &[
    "allocator",
    "authority",
    "beginning",
    "commit",
    "fmt",
    "message",
    "payload_barrier",
    "prepare",
    "reclaim",
    "region",
    "stage",
];

/// The states a swap passes through, and the one method each is allowed.
///
/// [`Swap`](SWAP_SURFACE) itself is not here: it has a constructor as well as a step, and a
/// constructor is what §10 step 1 *is*. The three below are the states that already hold
/// something on media, and each may do exactly one thing with it.
pub const SWAP_TYPESTATE: [(&str, &str); 3] = [
    ("Prepared", "stage"),
    ("Staged", "payload_barrier"),
    ("Sealable", "commit"),
];

/// The two values that may exist in only one place, and the body that may produce each.
///
/// `Sealable` is the only value that can program a generation seal and `Installed` the only
/// one that can erase the retired bank, so a second construction of either is a second route
/// to a step §10 puts after a barrier. The `compile_fail` doctest in the crate would keep
/// passing beside it.
pub const SWAP_CONSTRUCTIONS: [(&str, &str); 2] =
    [("Sealable", "payload_barrier"), ("Installed", "commit")];

/// The whole expression every barrier in a swap must be taken with.
///
/// A pin on the *spelling*, like [`APPEND_BARRIER_CALL`], and for that rule's reason: a body
/// that calls `storage.barrier()` and does not propagate the failure hands back a value
/// whose next step programs over media no barrier ever ordered.
pub const SWAP_BARRIER_CALL: &str = "storage.barrier().map_err(SwapStepError::Storage)?";

/// The two erases of §10, the body each belongs to, and the bank each may name.
///
/// Step 2 erases the bank being installed into and step 7 the bank being retired, and the
/// pin is the whole call because the bank is the only thing in it that can be wrong. A
/// `prepare` that erased `retiring` is a device clearing the bank it is running from, and a
/// `reclaim` that erased `installing` is a device throwing away the run it has just
/// installed — neither breaks another rule, and neither has a symptom until a boot.
///
/// The third element is the bank that body may **not** name at all, which is what keeps the
/// pin from being satisfied by a correct call beside an incorrect one.
pub const SWAP_ERASE_CALLS: [(&str, &str, &str); 2] = [
    (
        "prepare",
        "storage.erase(self.plan.installing.base(), self.plan.installing.bytes()).map_err(SwapStepError::Storage)?",
        "retiring",
    ),
    (
        "reclaim",
        "storage.erase(self.plan.retiring.base(), self.plan.retiring.bytes()).map_err(SwapStepError::Storage)?",
        "installing",
    ),
];

/// §10 step 5's program and step 6's barrier, and the bank the commit may not name.
///
/// Every other barrier in the protocol is pinned by [`SWAP_ERASE_CALLS`] or by
/// `payload_barrier`'s own row, and this one — the barrier that *is* §10 step 6, the moment
/// "a crash after step 6 recovers the new run" becomes true — was pinned by nothing until a
/// review found it. The hole is not theoretical: `let _ = storage.barrier();` here passes
/// `swap-discipline`, `integrity-check`, and both crash sweeps, because
/// `waymaker_fault::Device`'s barrier changes no media and the seal is on it either way. What
/// it leaves is a caller holding an `Installed` after a barrier that failed, which then
/// erases the retiring bank — a device whose new seal never became durable and whose old bank
/// is gone.
///
/// The program is pinned beside it for [`SWAP_ERASE_CALLS`]' reason: a seal aimed at the
/// retiring bank's offset is a swap that seals the run it is replacing.
pub const SWAP_COMMIT_STEP: (&str, &str, &str) = (
    "commit",
    "storage.program(self.plan.installing.seal_offset(), self.seal).map_err(SwapStepError::Storage)?",
    "retiring",
);

/// Rule: §10's seven steps happen in §10's order, and no step can be reached without them.
///
/// Two halves, for the reason [`check_commit_discipline`] has two: the surface is a set
/// comparison and the order is a shape, and they fail for different reasons.
#[must_use]
pub fn check_swap_discipline(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "swap-discipline";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = check_pinned_surface(
        RULE,
        ADAPTER,
        SWAP_SURFACE_PATH,
        SWAP_SURFACE,
        sources,
        "the swap's public API is where design document \u{a7}10's \"a crash before step 5 \
         recovers the old run\" stops being a sentence, so a way to seal a bank before its \
         payload barrier, or to erase a bank a caller named, cannot be added by accident",
    );

    let Some(source) = find_source(sources, SWAP_SURFACE_PATH) else {
        // `check_pinned_surface` has already reported the missing file.
        return violations;
    };
    violations.extend(check_swap_typestate(&source.contents));
    violations
}

/// The shape half of [`check_swap_discipline`], over one file's text.
fn check_swap_typestate(contents: &str) -> Vec<Violation> {
    const RULE: &str = "swap-discipline";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = Vec::new();
    let code = without_test_modules(&code_only(contents));

    for (state, step) in SWAP_TYPESTATE {
        let blocks = inherent_impl_bodies(&code, state);
        if blocks.is_empty() {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{SWAP_SURFACE_PATH} declares no inherent `impl` for `{state}`, so a \
                     state \u{a7}10 puts a barrier in front of is pinned against nothing"
                ),
            ));
            continue;
        }
        let block = blocks.join("\n");
        let methods = declared_function_names(&block);
        if methods != vec![step.to_owned()] {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{state}` declares {methods:?} rather than only `{step}`: a swap in this \
                     state may do exactly one thing, and \u{a7}10 says which"
                ),
            ));
        }
        // The state holding an unsealed bank must not be able to write one, which is
        // `commit-discipline`'s move for the same position in the record protocol.
        if state == "Staged" && count_tokens(&block, "program") != 0 {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{state}` names `program`: the type that holds a bank header no barrier \
                     has ordered must not be able to seal it"
                ),
            ));
        }
    }

    for (value, from) in SWAP_CONSTRUCTIONS {
        let constructions = struct_literals(&code, value);
        let inside = braced_body(&code, &format!("fn {from}"))
            .map_or(0, |body| struct_literals(body, value));
        if constructions == 0 || inside == 0 || constructions != inside {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{value}` is constructed {constructions} time(s), {inside} of them \
                     inside `{from}`: the value that may take the step after a barrier has \
                     to come from that barrier's own body and from nowhere else"
                ),
            ));
        }
    }

    violations.extend(check_swap_barriers(&code));
    violations
}

/// The barrier and erase half of [`check_swap_typestate`], split out for clippy's line
/// budget and because a reader chasing one does not have to read the other.
fn check_swap_barriers(code: &str) -> Vec<Violation> {
    const RULE: &str = "swap-discipline";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = Vec::new();
    let barrier = tightened(SWAP_BARRIER_CALL);

    let takes_the_barrier = braced_body(code, "fn payload_barrier")
        .is_some_and(|body| tightened(body).matches(barrier.as_str()).count() == 1);
    if !takes_the_barrier {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`payload_barrier` does not take the barrier as `{SWAP_BARRIER_CALL}`, exactly \
                 once: a body that calls it and does not propagate the failure hands back a \
                 value that seals a bank no barrier ever made durable"
            ),
        ));
    }

    // The two erases and the commit, each held to the one media call its row names, before a
    // barrier, without naming the other bank. One loop, because the three fail the same way.
    let steps: [(&str, &str, &str); 3] =
        [SWAP_ERASE_CALLS[0], SWAP_ERASE_CALLS[1], SWAP_COMMIT_STEP];
    for (step, erase, forbidden) in steps {
        let Some(body) = braced_body(code, &format!("fn {step}")) else {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{SWAP_SURFACE_PATH} declares no `fn {step}`, so \u{a7}10's erase of the \
                         bank it names is pinned against nothing"
                ),
            ));
            continue;
        };
        let tight = tightened(body);
        let Some(erased_at) = tight.find(tightened(erase).as_str()) else {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{step}` does not mutate media as `{erase}`: which bank a swap touches is \
                     derived from the authority the device booted rather than from an \
                     argument, and a call spelled any other way — a dropped `?` included — is \
                     one a reviewer has to check by eye"
                ),
            ));
            continue;
        };
        if count_tokens(body, forbidden) != 0 {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{step}` names `{forbidden}`: the other bank has no business in this \
                     step, and a correct erase beside an incorrect one is what the pin above \
                     cannot see"
                ),
            ));
        }
        // Step 2's barrier is the protocol rather than caution: \u{a7}12 orders only what a
        // completed barrier ordered, so a header programmed before it may become durable
        // ahead of the erase that would take it.
        match tight.find(barrier.as_str()) {
            Some(barriered_at) if barriered_at > erased_at => {}
            _ => violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{step}` does not take `{SWAP_BARRIER_CALL}` after its mutation: \u{a7}12 \
                     orders only what a completed barrier ordered, so without it what follows \
                     may become durable first — and for `commit` the barrier *is* \u{a7}10 \
                     step 6, the moment the new run becomes the one a reader boots"
                ),
            )),
        }
    }

    violations
}

/// The file whose route to the bank codec [`check_swap_routing`] pins.
pub const SWAP_ROUTING_PATH: &str = SWAP_SURFACE_PATH;

/// The swap's one body that reaches the codec, and the generic entry points it must use.
///
/// Three, because a bank swap seals twice: the header carries its own two checksums, and the
/// generation seal names the header's digest and then checksums itself. All three go through
/// the caller's `C`, or a device sealed by one algorithm is booted by another.
pub const SWAP_ROUTING_STEPS: &[(&str, &[&str])] = &[(
    "stage",
    &[
        "bank::encode_header_with",
        "bank::seal_for_with",
        "bank::encode_seal_with",
    ],
)];

/// Rule: the swap seals the bank it installs with the check its caller chose.
///
/// Reported under `integrity-check` like the other four halves, because it is the same
/// decision, and written the way [`check_append_routing`] is: the file names no checksum
/// function and no seal method at all, and the one body that reaches the bank codec reaches
/// it through the *generic* entry points.
///
/// Without it, `bank::encode_header` in place of `bank::encode_header_with::<C>` would
/// compile, pass every rule, and install a bank sealed with the shipped check whatever the
/// recovery that positioned the swap verified with — a device whose two banks need two
/// different readers.
#[must_use]
pub fn check_swap_routing(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let Some(source) = find_source(sources, SWAP_ROUTING_PATH) else {
        return vec![Violation::new(
            RULE,
            ADAPTER,
            format!(
                "no {SWAP_ROUTING_PATH} in the workspace, so nothing says \u{a7}10's swap still \
                 seals with the check its caller chose"
            ),
        )];
    };

    let code = without_test_modules(&code_only(&source.contents));
    let callable: String = code
        .lines()
        .filter(|line| !line.trim_start().starts_with("use "))
        .collect::<Vec<&str>>()
        .join("\n");
    let mut violations = Vec::new();

    for seal in SEAL_BINDINGS {
        for named in [seal.delegates_to, seal.method] {
            if count_tokens(&callable, named) != 0 {
                violations.push(Violation::new(
                    RULE,
                    ADAPTER,
                    format!(
                        "{SWAP_ROUTING_PATH} names `{named}` directly. The swap computes no \
                         seal of its own: it hands a header to the bank codec, and the codec \
                         is what the type parameter selects"
                    ),
                ));
            }
        }
    }

    for (step, entries) in SWAP_ROUTING_STEPS {
        let Some(body) = braced_body(&code, &format!("fn {step}")) else {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{SWAP_ROUTING_PATH} declares no `fn {step}`, so the swap's route to the \
                     bank codec is pinned against nothing"
                ),
            ));
            continue;
        };
        for through in *entries {
            let generic = format!("{through}::<C>");
            if invocation(body, &generic) != Invocation::Once {
                violations.push(Violation::new(
                    RULE,
                    ADAPTER,
                    format!(
                        "`{step}` does not call `{generic}` exactly once and use the answer, \
                         so the bank it installs is sealed with whichever check the codec \
                         defaults to rather than the one its caller asked for"
                    ),
                ));
            }
        }
    }

    violations
}

/// `code` with every whitespace character removed.
///
/// [`squeezed`] collapses runs of whitespace to one space, which survives rustfmt *breaking*
/// a chain and not rustfmt *joining* it — so a pinned expression that fits on one line after
/// an unrelated rename would fail its rule for a formatting reason. Removing whitespace
/// altogether pins the tokens and nothing else.
fn tightened(code: &str) -> String {
    code.split_whitespace().collect::<String>()
}

/// `code` with every run of whitespace collapsed, so a pinned expression survives rustfmt.
fn squeezed(code: &str) -> String {
    code.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// The bodies of every inherent `impl` block for `type_name` the file declares.
///
/// Line-scanned by brace depth, like every other rule here. An `impl Trait for Type` is not
/// one: its methods are the trait's, and a trait cannot add a way to program a seal without
/// also declaring it.
fn inherent_impl_bodies(code: &str, type_name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut cursor = 0_usize;
    // *Every* block, not the first. Review of this change pointed out that a second
    // `impl Staged { .. }` further down the file is invisible to a rule that stops at the
    // first, and a private helper in it could program a seal with the pin green.
    while let Some(at) = code.get(cursor..).and_then(next_impl_line) {
        let start = cursor.saturating_add(at);
        let Some(rest) = code.get(start..) else {
            break;
        };
        cursor = start.saturating_add(5);
        let Some(header) = rest.get(..rest.find('{').unwrap_or(0)) else {
            continue;
        };
        if header.contains(" for ") {
            continue;
        }
        if implemented_type(header).as_deref() == Some(type_name)
            && let Some(body) = braced_body(rest, "impl")
        {
            found.push(body.to_owned());
        }
    }
    found
}

/// Where the next `impl` keyword at the start of a line begins.
///
/// The offset of the keyword, not of the newline, because a leading attribute may sit
/// between them: `#[rustfmt::skip] impl Bank { pub(crate) fn raw() {} }` is one line that
/// survives `cargo fmt`, and a scan for `"\nimpl"` does not see it. Codex round 3 found the
/// same blindness in the reader beside this one.
fn next_impl_line(code: &str) -> Option<usize> {
    let mut offset = 0_usize;
    for line in code.split_inclusive('\n') {
        // A line at the very start of `code` is mid-line as far as this scan is concerned:
        // the caller resumes from inside a block it has already read.
        if offset > 0 {
            let bare = crate::size::without_leading_attributes(line);
            if bare.starts_with("impl") {
                let within = line.len().saturating_sub(bare.len());
                return Some(offset.saturating_add(within));
            }
        }
        offset = offset.saturating_add(line.len());
    }
    None
}

/// The type an inherent `impl` header names, with its generics stripped.
///
/// `impl<'a, C: IntegrityCheck> Sealable<'a, C>` is a block for `Sealable`, and the two
/// angle-bracket groups mean different things: the first declares parameters and the second
/// applies them. So the leading one is skipped by matching brackets rather than by taking
/// the last whitespace-separated word, which reads `C>` out of exactly that header.
fn implemented_type(header: &str) -> Option<String> {
    let after_keyword = header.strip_prefix("impl")?.trim_start();
    let rest = if after_keyword.starts_with('<') {
        let mut depth = 0_u32;
        let mut end = None;
        for (index, character) in after_keyword.char_indices() {
            match character {
                '<' => depth = depth.saturating_add(1),
                '>' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(index.saturating_add(1));
                        break;
                    }
                }
                _ => {}
            }
        }
        after_keyword.get(end?..)?.trim_start()
    } else {
        after_keyword
    };
    let name: String = rest
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// How many times `code` builds a `name { .. }` struct literal.
///
/// A declaration is not one: `pub struct Sealable<..> {` and `impl<..> Sealable<..> {` both
/// put an angle bracket between the name and the brace, and a return type puts a comma or a
/// closing bracket there. Only a literal puts a brace directly after the name.
fn struct_literal_positions(code: &str, name: &str) -> Vec<usize> {
    let continues = |character: char| character.is_alphanumeric() || character == '_';

    code.match_indices(name)
        .filter(|(index, _)| {
            let before = code.get(..*index).unwrap_or_default();
            // A `:` *is* allowed to precede a construction, unlike in `invocation`:
            // `self::Sealable { .. }` and `crate::append::Sealable { .. }` build the same
            // value the bare name does, and review of this change found that rejecting them
            // let a second construction be added by writing one extra path segment.
            let before_is_boundary = before
                .chars()
                .next_back()
                .is_none_or(|character| !continues(character));
            // A declaration is not a construction, and a type with no generics puts the
            // brace in the same place a literal does: `impl Sealable {` and
            // `pub struct Sealable {` both have to be skipped by what line they are on.
            let line = before.rsplit_once('\n').map_or(before, |(_, last)| last);
            let declares = {
                let start = line.trim_start();
                start.starts_with("impl")
                    || start.starts_with("struct")
                    || start.starts_with("pub struct")
                    || start.starts_with("pub(crate) struct")
                    // `fn barrier(self) -> Sealable {` puts the brace exactly where a
                    // literal does. What tells them apart is the arrow.
                    || before.trim_end().ends_with("->")
            };
            let after_is_literal = code
                .get(index.saturating_add(name.len())..)
                .unwrap_or_default()
                .trim_start()
                .starts_with('{');
            before_is_boundary && after_is_literal && !declares
        })
        .map(|(index, _)| index)
        .collect()
}

/// How many times `name` is constructed in `code`.
fn struct_literals(code: &str, name: &str) -> usize {
    struct_literal_positions(code, name).len()
}

/// Whether `code` declares `header` as a struct with a braced body.
///
/// A tuple or unit struct has no `{` of its own, so [`braced_body`] would walk on to the
/// next one — an `impl` block, usually — and a field scan would then report on code the
/// declaration does not contain. Generic parameters are allowed between the two, so this
/// looks for the first `{`, `(` or `;` and asks which came first.
fn declares_braced_struct(code: &str, header: &str) -> bool {
    let continues = |character: char| character.is_alphanumeric() || character == '_';
    code.match_indices(header)
        .filter(|(index, _)| {
            code.get(..*index)
                .and_then(|before| before.chars().next_back())
                .is_none_or(|character| !continues(character))
        })
        .any(|(index, _)| {
            let rest = code.get(index + header.len()..).unwrap_or_default();
            rest.find(['{', '(', ';'])
                .is_some_and(|at| rest.get(at..at + 1) == Some("{"))
        })
}

/// Whether `code` has a hand-written `impl Trait for type_name` block.
///
/// A `#[derive(..)]` is not one: it produces no `impl` line for this scan to find, which is
/// what keeps `Clone`, `Copy` and `Debug` out of the way.
///
/// Codex round 2 asked for this, on a premise that does not hold: `public_functions` counts a
/// method of a trait `impl` as callable whatever its visibility, so the surface pin already
/// rejected `impl From<EffectId> for DurableIntent` — as a name it does not list, and, when
/// the method reuses a pinned name, as one declared twice. Both were measured before this was
/// written.
///
/// It is here anyway, because the two pins that are *about* construction — the method set and
/// the `Self` scan — do both go blind on a trait `impl`, and a guarantee that holds only
/// through a third pin's side effect is one nobody can check by reading this file.
fn implements_trait_for(code: &str, type_name: &str) -> bool {
    let mut cursor = 0_usize;
    while let Some(at) = code.get(cursor..).and_then(|rest| rest.find("\nimpl")) {
        let start = cursor.saturating_add(at).saturating_add(1);
        let Some(rest) = code.get(start..) else {
            break;
        };
        cursor = start.saturating_add(5);
        let header = rest.get(..rest.find('{').unwrap_or(0)).unwrap_or_default();
        let Some((_, implemented)) = header.rsplit_once(" for ") else {
            continue;
        };
        // The bare name, with any generic arguments cut off: `DurableIntent` and
        // `Dispatchable<C>` are the same type here.
        let named = implemented
            .trim()
            .split(['<', ' ', '\n'])
            .next()
            .unwrap_or_default();
        if named == type_name {
            return true;
        }
    }
    false
}

/// The nesting depth at `index`, counted from the start of `code`.
///
/// Braces, parentheses and brackets together. `code` has already been through [`code_only`],
/// so a bracket in a comment or a string literal is not in it. Depth zero is a statement of
/// the body itself; anything deeper is inside a block, an argument list, a match arm or a
/// closure, and a step taken there is a step a branch can skip.
///
/// The parentheses are Codex's, from round 2. Brace depth alone equated nesting with
/// execution, and `false.then(|| self.writer.stage(..).payload_barrier(..).commit(..))` has
/// no braces at all: three pinned calls, in order, at brace depth zero, in a closure nothing
/// runs.
fn nesting_depth_at(code: &str, index: usize) -> usize {
    let before = code.get(..index).unwrap_or_default();
    let opened = before.matches(['{', '(', '[']).count();
    let closed = before.matches(['}', ')', ']']).count();
    opened.saturating_sub(closed)
}

/// The names of the functions declared directly in an `impl` body.
fn declared_function_names(body: &str) -> Vec<String> {
    let mut depth = 0_i32;
    let mut names = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if depth == 0
            && let Some(name) = function_declaration_name(trimmed)
        {
            names.push(name);
        }
        let opens = i32::try_from(trimmed.matches('{').count()).unwrap_or(0);
        let closes = i32::try_from(trimmed.matches('}').count()).unwrap_or(0);
        depth = depth.saturating_add(opens).saturating_sub(closes);
    }
    names.sort_unstable();
    names
}

/// The associated constants an `impl` body declares, at any visibility.
///
/// [`declared_function_names`]'s twin, and it exists because that one reads `fn`. A
/// `pub const BEST_EFFORT: Self = Self::AfterBoot { ticks: 0 };` on `impl TimerSpec` adds no
/// function, changes no member, and is reached as `TimerSpec::BEST_EFFORT` — a downgrade
/// behind a plausible guard, with the gate green. Review of this change ran exactly that.
///
/// Depth-zero lines only, for [`declared_function_names`]'s reason: a `const` inside a
/// function body is a local, not a door.
fn declared_associated_constants(body: &str) -> Vec<String> {
    let mut depth = 0_i32;
    let mut names = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if depth == 0
            && let Some(rest) = trimmed
                .strip_prefix("pub ")
                .or_else(|| {
                    trimmed
                        .split_once(") ")
                        .filter(|(head, _)| head.starts_with("pub("))
                        .map(|(_, rest)| rest)
                })
                .or(Some(trimmed))
            && let Some(declaration) = rest.strip_prefix("const ")
            // `const fn` is a function, and `declared_function_names` owns those.
            && !declaration.starts_with("fn ")
            && let Some(name) = declaration.split([':', ' ']).next()
            && !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            names.push(name.to_owned());
        }
        let opens = i32::try_from(trimmed.matches('{').count()).unwrap_or(0);
        let closes = i32::try_from(trimmed.matches('}').count()).unwrap_or(0);
        depth = depth.saturating_add(opens).saturating_sub(closes);
    }
    names.sort_unstable();
    names
}

/// The file whose `EffectScheduled` field set [`EFFECT_SCHEDULED_FIELDS`] pins.
pub const EFFECT_SCHEDULED_PATH: &str = "waymaker-core/src/record.rs";

/// Every field `RecordRef::EffectScheduled` is allowed to carry, in sorted order.
///
/// Design document §16's third deferred question is "how much input metadata an
/// `EffectScheduled` record stores beyond length and digest", and issue #16 states the cost
/// that makes it a question: "every extra field is paid per effect, per record, in flash and
/// in write amplification". ADR 0011 answers it — the sequence, the activity kind, the input
/// length and a CRC-32 of the input, and nothing else — and this is that answer in a form a
/// build can fail over.
///
/// A `deadline_ms`, a `priority`, a `retry_count` or a copy of the input itself is the shape
/// this exists to stop. None of them breaks a layering rule, none needs a dependency, and
/// each is four more bytes on every scheduled effect for the life of the format.
///
/// The pin fails in both directions. A field *removed* is a wire-format change on a record
/// firmware in the field has already written; it is not a thing to discover from a failing
/// round-trip test in `waymaker-flash`.
///
/// Sorted, so that the comparison below can be a set comparison and the list can be read.
pub const EFFECT_SCHEDULED_FIELDS: &[&str] = &["input_crc", "input_len", "kind", "seq"];

/// Every field `RecordRef::TimerScheduled` is allowed to carry, and every field
/// `RecordRef::TimerFired` is, in sorted order.
///
/// [`EFFECT_SCHEDULED_FIELDS`]'s twin for issue
/// [#33](https://github.com/madmax983/waymaker/issues/33)'s two record bodies, and it holds
/// two things design document §11 says a build must fail over.
///
/// `clock_kind` is the first. §11: a persistent timer record includes its clock kind so
/// recovery cannot silently reinterpret one policy as another. A record that carried the
/// deadline alone would decode without error and mean something else after a firmware
/// change — a boot interval where a persistent instant was meant, or the reverse — and
/// nothing downstream could tell.
///
/// `armed_at` is the second, and it is the field a reviewer is most likely to read as
/// redundant. The monotonicity floor a persistent deadline is measured against lives in
/// RAM, and a power cut takes RAM; this is what carries it across the reset. Without it a
/// clock that moved backwards while the power was absent is invisible.
///
/// A `remaining`, a `fired_at`, a `retry_count` or a payload on the firing is the shape the
/// pin stops in the other direction. Each is bytes on every timer for the life of the
/// format, and none breaks another rule.
///
/// The empty body for `TimerFired` is deliberate rather than an oversight: a deadline's
/// whole result is that it passed, and the sequence that says which timer is in the frame
/// header. It is also what the codec's exact-length refusal rests on.
///
/// Sorted, so that the comparisons can be set comparisons and the lists can be read.
pub const TIMER_RECORD_FIELDS: &[(&str, &[&str])] = &[
    (
        "TimerScheduled",
        &["armed_at", "clock_kind", "deadline", "seq"],
    ),
    ("TimerFired", &["seq"]),
];

/// The file whose kernel-boundary types [`BOUNDARY_TYPES`] pins.
pub const KERNEL_BOUNDARY_PATH: &str = "waymaker-core/src/transition.rs";

/// One type at design document §06's kernel boundary, and the shape it is pinned at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundaryType {
    /// The declaration, as the source writes it: `enum Resolve<'a>` becomes `enum Resolve`,
    /// because the scanner matches a header at a token boundary.
    pub header: &'static str,
    /// The variants — or, for a struct, the fields — it is allowed to declare, sorted.
    pub members: &'static [&'static str],
}

/// Every type issue #28's boundary is made of, and the members each may declare.
///
/// Issue [#28](https://github.com/madmax983/waymaker/issues/28)'s second "done when" is that
/// "adding a new record kind does not change this signature", and that is an *absence*: no
/// type below names a record, a record kind or a step. Design document §09 numbers eleven
/// record kinds. Six have a `RecordRef` body; five are reserved without one —
/// `TIMER_SCHEDULED`, `TIMER_FIRED`, `VERSION_MARKER`, `SIGNAL_RECEIVED`, `CHILD_STARTED` —
/// and every one of those is a body somebody will write.
/// A `Resolve::TimerFired`, an `Intent::Signal`, or a `kind: RecordKind` field on
/// `EffectRequest` would each break no other rule, need no dependency, and turn one boundary
/// into a boundary per record.
///
/// Issue [#33](https://github.com/madmax983/waymaker/issues/33) wrote the first two of those
/// five bodies and the five types above did not move, which is the claim holding. What it
/// added is a *boundary* — §11's deadline is a different question from §08's effect, asked
/// with a capability the effect boundary has no use for — and its three types are pinned
/// here beside them. `TimerRequest`'s two fields are the whole of §02 decision 8: the
/// deadline, and what this firmware can measure. A `TimerRequest::best_effort` flag, a
/// `TimerIntent::Downgrade` or a `TimerResolve::Assume` would each break no other rule and
/// turn the refusal into a preference.
///
/// The pin fails in both directions, and the second matters more: a member the list no
/// longer finds means the type was renamed or deleted and the pin is checking nothing.
///
/// What it does **not** catch is a *widened* member: a `Resolve::Replayed` that grew a
/// third field, or an `EffectRequest::kind` retyped, is invisible to a comparison of names.
/// `transition-surface` pins the machine's functions and
/// `crates/waymaker-core/tests/transition.rs` holds the behaviour; this raises the cost of a
/// new door, not of widening an existing one.
///
/// Sorted, so that the comparisons below can be set comparisons and the lists can be read.
pub const BOUNDARY_TYPES: &[BoundaryType] = &[
    BoundaryType {
        header: "pub struct EffectRequest",
        members: &["input_crc", "input_len", "kind"],
    },
    BoundaryType {
        header: "pub enum Intent",
        members: &["Finished", "Recorded", "Schedule"],
    },
    BoundaryType {
        header: "pub enum Resolve",
        members: &["Redeliver", "Replayed"],
    },
    BoundaryType {
        header: "pub enum Outcome",
        members: &["Completed", "Failed"],
    },
    BoundaryType {
        header: "pub enum Next",
        members: &["EndOfHistory", "Record"],
    },
    BoundaryType {
        header: "pub struct TimerRequest",
        members: &["capability", "spec"],
    },
    BoundaryType {
        header: "pub enum TimerIntent",
        members: &["Finished", "Recorded", "Schedule"],
    },
    BoundaryType {
        header: "pub enum TimerResolve",
        members: &["Fired", "Rearm"],
    },
];

/// The file whose decisions [`BOUNDARY_DECISIONS`] pins.
pub const DRIVER_PATH: &str = "waymaker-drive/src/drive.rs";

/// Every kernel answer the synchronous driver is required to decide from.
///
/// Matched at a path boundary by [`names_identifier`'s rule](DRIVER_FORBIDDEN_VOCABULARY),
/// because a substring test is satisfied by any longer path ending in the same segments.
///
/// Issue #28's first work item is to "drive the whole protocol through this boundary so
/// `waymaker-embassy` is provably a façade and nothing more", and the claim only holds while
/// the driver's *decisions* come from [`BOUNDARY_TYPES`]. A driver that read a record and
/// decided for itself whether it was a schedule would be a second transition table, and the
/// one below it would no longer be where §08 is enforced.
///
/// Every row is one row of §08's table, so a row lost means an arm was deleted or renamed
/// and the driver is deciding somewhere else.
pub const BOUNDARY_DECISIONS: &[&str] = &[
    "Intent::Finished",
    "Intent::Recorded",
    "Intent::Schedule",
    "Resolve::Redeliver",
    "Resolve::Replayed",
];

/// Why [`DRIVER_FORBIDDEN_VOCABULARY`] refuses the two record names.
///
/// One string rather than two identical ones, so the pair cannot drift into two reasons for
/// one ban.
const RECORD_VOCABULARY: &str = "is the record vocabulary the kernel decides from; a driver \
                                 that reads it is a second transition table, and the one \
                                 below it is no longer where \u{a7}08 is enforced";

/// Vocabulary the driver may not name, with the reason each is banned.
///
/// `RecordKind` is §09's numbering and `Step` is what the cursor answers an `advance` with:
/// a driver that matched on either would be reading history rather than being told about it.
///
/// `EffectIdAllocator` is issue [#30](https://github.com/madmax983/waymaker/issues/30)'s.
/// §14's fourth guarantee is that a retry and a reboot redeliver the *original* identity, and
/// the driver keeps that guarantee by never having an identity of its own: every
/// `(RunId, EffectSeq)` it dispatches under comes from `Intent::Schedule` or
/// `Resolve::Redeliver`. A driver that reached for the allocator would be minting, and a
/// fresh mint for an outstanding effect is a second effect as far as every downstream
/// system is concerned. It is a floor rather than a proof — `EffectId`'s fields are public,
/// so a literal evades it, and
/// [what is not checked](https://github.com/madmax983/waymaker/blob/main/CLAUDE.md#what-is-not-checked)
/// says so.
///
/// Matched as *identifiers* rather than as substrings, which is what makes the ban a ban.
/// A spelling ban on `Step::` is evaded by `Step ::Record`, by `use …::Step as S;`, and by
/// importing the type and naming it in a signature; and it would fire on an unrelated
/// `BootStep::`. The scan compares token boundaries on both sides instead.
///
/// `RecordRef` is deliberately **not** here, and the reason is narrower than "the driver
/// does not read records". It constructs them, because the kernel names the record it wants
/// written and something has to write it. It also reads two, and
/// [what is not checked](https://github.com/madmax983/waymaker/blob/main/CLAUDE.md#what-is-not-checked)
/// names both rather than leaving them implied.
pub const DRIVER_FORBIDDEN_VOCABULARY: &[(&str, &str)] = &[
    ("RecordKind", RECORD_VOCABULARY),
    ("Step", RECORD_VOCABULARY),
    (
        "EffectIdAllocator",
        "is the one thing permitted to mint an effect identity; a driver that names it can \
         hand a redelivered effect a fresh sequence, which \u{a7}14 says is a second effect",
    ),
];

/// The file whose surface and step order [`check_effect_protocol`] pins.
pub const EFFECT_PROTOCOL_PATH: &str = "waymaker-drive/src/effect.rs";

/// Every public function design document §07's protocol is allowed to have.
///
/// Issue [#29](https://github.com/madmax983/waymaker/issues/29) asks that step 4 be
/// unreachable without step 3, "structurally, not by review". The structure is an absence: no
/// function here hands out an effect identity, a writer, or an outcome without the barrier
/// that earns it.
///
/// `redelivering` is deliberately not on this list. §08's redelivery row takes the kernel's
/// word that committed history holds a schedule record with no outcome, so it mints a proof
/// from a sequence number — which is a forge in any hand but the driver's beside it. It is
/// `pub(crate)`, and [`EFFECT_TYPE_METHODS`] is what keeps it declared.
///
/// Sorted, so that the comparison can be a set comparison and the list can be read.
pub const EFFECT_PROTOCOL_SURFACE: &[&str] =
    &["id", "intent", "into_writer", "over", "resolve", "schedule"];

/// Every type §07's protocol is made of, and every method it may declare — at any visibility.
///
/// The visibility is the point. `check_pinned_surface` reads `public_functions`, which counts
/// a line starting `pub ` and not one starting `pub(`, so a `pub(crate) fn dispatchable_now`
/// is invisible to it — and `waymaker-drive` is the one crate that must not be able to call
/// such a thing, because the driver that would call it is in the same crate. Review of this
/// change added `pub(crate) const fn new(id) -> Self` to `DurableIntent`, wired it into the
/// driver, and watched the gate print `ok`. The scan behind this list reads the declaration
/// rather than the keyword in front of it.
///
/// Each list is sorted, so the comparison can be a set comparison.
pub const EFFECT_TYPE_METHODS: [(&str, &[&str]); 3] = [
    ("DurableIntent", &["id"]),
    ("Dispatchable", &["intent", "resolve"]),
    (
        "Effect",
        &["into_writer", "over", "redelivering", "schedule"],
    ),
];

/// The values that prove a schedule record is durable, and the bodies that may build them.
///
/// Two bodies each, and both are a durability proof: `schedule`, after step 3's commit
/// barrier returned, and `redelivering`, for a record committed before this boot. A third
/// construction is a third way to reach step 4. Both bodies are read out of `Effect`'s own
/// `impl` blocks rather than out of the file, for [`EFFECT_STEP_BODIES`]' reason.
pub const EFFECT_CONSTRUCTIONS: [(&str, [&str; 2]); 2] = [
    ("DurableIntent", ["schedule", "redelivering"]),
    ("Dispatchable", ["schedule", "redelivering"]),
];

/// The proof types whose own `impl` blocks may not build a `Self`.
///
/// The construction scan counts a type's *name*, so `Self { .. }` inside the type's own
/// `impl` is a construction it cannot see. Review of this change used exactly that, beside a
/// `pub(crate)` constructor, to mint a `DurableIntent` with the gate green. `Effect` is not
/// here: it is not a proof of anything, and its own constructor is a `Self`.
pub const EFFECT_NO_SELF_LITERAL: [&str; 2] = ["DurableIntent", "Dispatchable"];

/// The type that owns each body §07's storage steps happen in, and that body's name.
///
/// The owner is load-bearing. `braced_body` takes the *first* match in the file, so a private
/// free `fn resolve` above the real one is the body the pin reads — and review of this change
/// wrote one that took all three steps while `Dispatchable::resolve` stopped at the payload
/// barrier, leaving an outcome handed to the workflow from a record that was never sealed.
/// `capacity-reserve` closed the same hole for the same reason; this reads the type's own
/// `impl` blocks.
pub const EFFECT_STEP_BODIES: [(&str, &str); 2] =
    [("Effect", "schedule"), ("Dispatchable", "resolve")];

/// §07's three storage steps, spelled as the method calls they are.
///
/// Steps 1, 2 and 3 for a schedule record; steps 5, 6 and 7 for an outcome. The same three
/// calls, because §07 states them twice.
///
/// A call rather than an identifier, like every sibling pin. `count_tokens(body, "commit")`
/// is satisfied by a local named `commit`, and it disagreed with the `str::find` the order
/// half used: a `let payload_barrier_at = 0;` between the steps made `find` match inside the
/// binding and the order check pass over a body that sealed before it barriered. Counted and
/// located on the same whitespace-free text, so the two cannot disagree again.
pub const EFFECT_STEPS: [&str; 3] = [".stage(", ".payload_barrier(", ".commit("];

/// What the already-durable path may not name.
///
/// `redelivering` writes nothing: the schedule record committed in an earlier boot. A body
/// that programmed anything here would be a second schedule record for one effect.
pub const EFFECT_REDELIVERY_FORBIDDEN: [&str; 4] =
    ["stage", "payload_barrier", "commit", "program"];

/// The body a proof of durable intent is built in, its owner, and the step it must follow.
///
/// Codex found the hole this closes: one half said *where* a proof may be built and another
/// said the barriers happen in order, and neither related the two. An early return that built
/// a `Dispatchable` before `stage` satisfied both, and reintroduced the
/// dispatch-before-durable-intent path the whole rule exists to reject.
///
/// `redelivering` is not here: it takes no step, because §07 steps 1 to 3 happened in an
/// earlier boot, and [`EFFECT_REDELIVERY_FORBIDDEN`] is what holds it to that.
pub const EFFECT_PROOF_AFTER: (&str, &str, &str) = ("Effect", "schedule", ".commit(");

/// The file whose contents [`INTEGRITY_CHECK_PARAMETERS`] pins.
pub const INTEGRITY_CHECK_PATH: &str = "waymaker-flash/src/crc.rs";

/// One catalogued parameter, and where in the checksum module it has to appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChecksumParameter {
    /// The function whose body must contain it, by name.
    pub function: &'static str,
    /// What the literal is to the algorithm, for a violation message.
    pub role: &'static str,
    /// The literal, spelled as the source spells it.
    pub literal: &'static str,
    /// How many times that body must contain it.
    ///
    /// Two for CRC-32's `0xFFFF_FFFF`, which is both its initial value and its final xor.
    /// Codex caught the reason this is a count rather than a presence check on PR #58:
    /// change one of the two and the other still satisfies "the file contains it".
    pub occurrences: usize,
}

/// The catalogued parameters the two checksums are required to keep, per function.
///
/// Design document §16's first deferred question is "whether the default integrity check is
/// CRC32C or a smaller table-free CRC implementation". ADR 0010 answers it with measurements
/// taken on `thumbv6m-none-eabi`: the polynomial costs nothing either way — both bitwise
/// loops assemble to 52 bytes — so the choice falls to which algorithm a host can check a
/// device's journal against without reimplementing anything, and that is CRC-32/ISO-HDLC.
///
/// Pinned as the literal each parameter appears as, because the parameter *is* the
/// algorithm: a reflected polynomial quietly changed from `0xEDB8_8320` to `0x82F6_3B78` is
/// a different CRC that passes every round-trip test in this repository, and fails against
/// every zlib in the world.
///
/// Scoped to a function body and counted, which two rounds of review were needed to get
/// right. A bare `contains` over the file let `0xFFFF_FFFF` vouch for `0xFFFF` — so CRC-16's
/// initial value could not be lost — and then let CRC-32's initial value vouch for its own
/// final xor. A pin that cannot fail is worse than no pin, because the report says it
/// checked.
pub const INTEGRITY_CHECK_PARAMETERS: &[ChecksumParameter] = &[
    ChecksumParameter {
        function: "crc16",
        role: "CRC-16/CCITT-FALSE polynomial",
        literal: "0x1021",
        occurrences: 1,
    },
    ChecksumParameter {
        function: "crc16",
        role: "CRC-16/CCITT-FALSE initial value",
        literal: "0xFFFF",
        occurrences: 1,
    },
    ChecksumParameter {
        function: "crc32",
        role: "CRC-32/ISO-HDLC reflected polynomial",
        literal: "0xEDB8_8320",
        occurrences: 1,
    },
    ChecksumParameter {
        function: "crc32",
        role: "CRC-32/ISO-HDLC initial value and final xor",
        literal: "0xFFFF_FFFF",
        occurrences: 2,
    },
];

/// The file that binds the shipped integrity check to an algorithm.
///
/// Separate from [`INTEGRITY_CHECK_PATH`], which is where the two loops live. This is where
/// the codec is told which loops to use, and the two can drift apart in a way neither pin
/// would see on its own: a `crc.rs` whose parameters are untouched, bound to nothing, is a
/// checksum module the codec no longer calls.
pub const INTEGRITY_BINDING_PATH: &str = "waymaker-flash/src/integrity.rs";

/// The trait the frame's two seals go through.
pub const INTEGRITY_TRAIT: &str = "trait IntegrityCheck";

/// The implementation this firmware ships, and the one ADR 0010 settled on.
pub const INTEGRITY_SHIPPED_IMPL: &str = "impl IntegrityCheck for Catalogued";

/// The codec, which must reach a seal through the trait rather than around it.
pub const INTEGRITY_ROUTING_PATH: &str = "waymaker-flash/src/frame.rs";

/// The codec's functions generic over the integrity check, and which seals each must route
/// through.
///
/// A binding rule that reads only `integrity.rs` pins a trait nothing is obliged to call.
/// Review of this change found exactly that: a codec re-hard-wired to `crc16` and `crc32`,
/// with `integrity.rs` left perfectly intact, passed every rule. So the routing is pinned
/// too — each body below must name the seals its row lists, exactly once and with the answer
/// used, and must not name a checksum function at all.
///
/// Named per function rather than "both seals in both bodies", for the reason
/// [`BANK_SEALING_FUNCTIONS`] is: the header seal is computed in exactly one place.
/// [`verify_header_with`] is that place, and [`decode_with`] routes through it, which is
/// what [`HEADER_STEP`] holds. An empty method list is "this body must compute no seal of
/// its own": the file-wide checksum ban is what holds it, and the row exists because the
/// derived scan in [`check_integrity_routing`] requires every function generic over the
/// check to be accounted for — a `frame_len_of_with` that took `C`, ignored it and called
/// `crc16` is exactly the mutation that scan exists to catch.
///
/// [`verify_header_with`]: https://github.com/madmax983/waymaker/blob/main/crates/waymaker-flash/src/frame.rs
/// [`decode_with`]: https://github.com/madmax983/waymaker/blob/main/crates/waymaker-flash/src/frame.rs
pub const SEALING_FUNCTIONS: &[(&str, &[&str])] = &[
    ("encode_with", &["header_check", "frame_check"]),
    ("verify_header_with", &["header_check"]),
    ("decode_with", &["frame_check"]),
    ("frame_len_of_with", &[]),
    ("input_digest_with", &["frame_check"]),
    // `Scan`'s three, which are generic because the block they sit in is. None computes a
    // seal — the scan reaches one through `decode_with`, which [`SCAN_STEP`] pins — so all
    // three carry the empty list, and the empty list is not nothing: it says "compute no seal
    // of your own", which the file-wide ban below enforces. They are here because a method in
    // a generic `impl` block is the most likely place a new seal would be written, and a row
    // is what makes adding one a line somebody writes on purpose.
    ("next", &[]),
    ("offset", &[]),
    ("with_integrity", &[]),
];

/// The bank codec of design document §10, which seals two more structures.
///
/// A second file with the same hazard. `frame.rs` is pinned above; without this, the bank
/// header and the generation seal could be hard-wired to `crc16` and `crc32` with every
/// other rule green, and a caller that asked for a different integrity check would get the
/// shipped one for its banks and its chosen one for its records — a device sealing one part
/// of its media with an algorithm it does not verify the other part with.
pub const BANK_ROUTING_PATH: &str = "waymaker-flash/src/bank.rs";

/// The bank codec's sealing functions, and which seals each body must route through.
///
/// Unlike [`SEALING_FUNCTIONS`], not every body computes both seals: a generation seal
/// carries only its own sixteen-bit check, and the digest that binds it to a bank header is
/// computed in exactly one place. Naming the subset per function is what keeps the rule a
/// statement about each body rather than the loosest thing true of all of them.
pub const BANK_SEALING_FUNCTIONS: &[(&str, &[&str])] = &[
    ("encode_header_with", &["header_check", "frame_check"]),
    ("decode_header_with", &["header_check", "frame_check"]),
    ("encode_seal_with", &["header_check"]),
    ("decode_seal_with", &["header_check"]),
    ("seal_for_with", &["frame_check"]),
    // Routes by delegation rather than by computing a seal itself: it decodes the seal and
    // asks `seal_for_with` for the one the header deserves. An empty method list is what
    // "this body must compute no seal of its own" is spelled as — the file-wide checksum ban
    // is what holds it, and the row exists because the derived scan below requires every
    // function generic over the check to be accounted for. A body that started computing a
    // seal here would be a row somebody has to widen.
    ("sealed_generation_with", &[]),
];

/// The one function permitted to call the checksum module from the codec, and what it may
/// call.
///
/// [`crate::size`] cannot see this and neither can a type: `input_digest` is a `const fn`,
/// a trait method cannot be one, so ADR 0011's digest reaches `crc32` directly. That is the
/// single documented exception, and naming it here is what stops it from becoming a habit.
pub const DIGEST_FUNCTION: (&str, &str) = ("input_digest", "crc32");

/// The reader that walks a journal on media, which reaches its seals through the codec.
///
/// A third file generic over the integrity check, and until review of this change it had no
/// pin at all: replacing `decode_with::<C>` and `frame_len_of_with::<C>` in it with the
/// non-generic `decode` and `frame_len_of` — so that every recovery verified with the shipped
/// check whatever its caller chose — passed all 38 rules and the whole test suite. That is
/// exactly the mutation [`SEALING_FUNCTIONS`] exists to catch, one file over.
pub const RECOVERY_ROUTING_PATH: &str = "waymaker-flash/src/recovery.rs";

/// The recovery reader's two steps, and the entry point each must reach the codec through.
///
/// Unlike [`SEALING_FUNCTIONS`], these are not bodies that compute a seal: nothing in
/// `recovery.rs` may, and the file-wide ban is what says so. They are the two places the
/// reader hands bytes to the codec, and each has to hand them to the *generic* one — which
/// the rule spells `<callee>::<C>`, so a call that dropped the turbofish and took the default
/// is a mention rather than a route.
///
/// The callee is written module-qualified because that is how `recovery.rs` writes it, and a
/// pin is a pin on the spelling: importing these two unqualified would fail this rule and is
/// a line a reviewer writes, which is the same bargain every other pin here makes.
pub const RECOVERY_ROUTING_STEPS: &[(&str, &str)] = &[
    ("next", "frame::decode_with"),
    ("stage", "frame::frame_len_of_with"),
];

/// The writer, which reaches its seals through the codec the caller chose.
///
/// A fourth file generic over the integrity check, and the one with the most to lose from
/// dropping a turbofish: a journal *written* with the shipped check and *read* with the
/// caller's is a bank whose first record fails at its own header seal on the next boot.
/// `Journal<C>`'s parameter is a promise that a record is sealed with the algorithm the
/// recovery that positioned the writer verified with, and one call keeps it.
pub const APPEND_ROUTING_PATH: &str = "waymaker-flash/src/append.rs";

/// The writer's one step into the codec, and the entry point it must take.
///
/// `frame::encode_with::<C>` produces the frame *and* its commit seal, so this single route
/// covers both — the seal is derived from the frame check the codec just computed, which is
/// why [`APPEND_ROUTING_PATH`] needs no seal-method row of its own and is banned from naming
/// one.
pub const APPEND_ROUTING_STEPS: &[(&str, &str)] = &[("stage", "frame::encode_with")];

/// The scan's step, and the entry point it must walk a journal with.
///
/// `decode` rather than `decode_with::<C>` here would make [`crate::size`]'s type parameter
/// decorative: every scan would verify with the shipped check whatever its caller asked
/// for, which is a reader silently disagreeing with the writer.
pub const SCAN_STEP: (&str, &str) = ("next", "decode_with");

/// The decoder's header step, and the one function it must verify a header with.
///
/// [`SEALING_FUNCTIONS`] says `decode_with` computes no header seal of its own; without
/// this, "no header seal" would be satisfied by a `decode_with` that skipped the header
/// check altogether and read `payload_len` out of bytes nothing verified. That is the whole
/// of §09's first checksum undone, and it is the shape a page-bounded reader depends on:
/// `frame_len_of` promises a length the writer is known to have written.
pub const HEADER_STEP: (&str, &str) = ("decode_with", "verify_header_with");

/// The header-length accessor, and the one function it must verify a header with.
///
/// The same decision as [`HEADER_STEP`], for the function a recovery calls before it decides
/// how many bytes to stage. A `frame_len_of_with` that returned a length without checking
/// the seal over it would let an erased page send a reader anywhere.
pub const FRAME_LEN_STEP: (&str, &str) = ("frame_len_of_with", "verify_header_with");

/// One seal: what the trait returns for it, and what the shipped implementation computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealBinding {
    /// The trait method, by name.
    pub method: &'static str,
    /// The return type, spelled as the signature spells it. This *is* the seal's width on
    /// media.
    pub width: &'static str,
    /// The function in the checksum module the shipped implementation must delegate to.
    pub delegates_to: &'static str,
    /// What the seal covers, for a violation message.
    pub covers: &'static str,
}

/// The two seals, their widths, and what the shipped implementation computes them with.
///
/// Issue [#17](https://github.com/madmax983/waymaker/issues/17) asks for two things this
/// table holds together. The check has to stay *swappable* — hence a trait, and hence a rule
/// that reads the trait rather than the codec — and the frame's `header_crc` and
/// `payload_crc` **widths** have to be settled *as a result*, which they are: they are the
/// return types below, and §09's frame spends exactly that many bytes on each. A width is
/// not an implementation detail. Sixteen bits to thirty-two on the header is two more bytes
/// per record on media for the life of the format, and the frame's own `const` assertions
/// only catch it if someone changes the constants to match.
///
/// The delegation column is the other half. A trait anything may implement is a trait the
/// *shipped* answer can quietly leave: `Catalogued` rebound to a different loop passes every
/// round-trip test in this repository, exactly as a changed polynomial does, and
/// [ADR 0010](https://github.com/madmax983/waymaker/blob/main/docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md)
/// is the decision that would have been undone without a line in a diff saying so.
pub const SEAL_BINDINGS: &[SealBinding] = &[
    SealBinding {
        method: "header_check",
        width: "u16",
        delegates_to: "crc16",
        covers: "the header's first ten bytes",
    },
    SealBinding {
        method: "frame_check",
        width: "u32",
        delegates_to: "crc32",
        covers: "the header and the payload",
    },
];

/// Rule: the shipped integrity check is bound to ADR 0010's algorithms, at ADR 0012's
/// widths.
///
/// Reported under `integrity-check`, the same id as [`check_integrity_check`], because it is
/// the same decision: that rule says the loops are still the catalogued table-free ones,
/// this one says the codec still calls them and still spends the same bytes on media. A
/// contributor reading a failure does not care which half of the pin caught it.
///
/// Scanned rather than parsed, like every rule here, over code with its comments and string
/// literals stripped — this module's own documentation names both functions repeatedly, and
/// a rule satisfied by prose is a rule that passes when the code is gone.
#[must_use]
pub fn check_integrity_binding(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let Some(source) = find_source(sources, INTEGRITY_BINDING_PATH) else {
        return vec![Violation::new(
            RULE,
            ADAPTER,
            format!(
                "no {INTEGRITY_BINDING_PATH} in the workspace, so nothing binds the frame \
                 seals to an algorithm; issue #17 requires the integrity check to live behind \
                 a trait so the choice stays swappable, and a binding that is gone is a pin \
                 checking nothing. A `crc/`-style split into `integrity/mod.rs` is not \
                 followed here on purpose: this pin is about one named binding, so moving it \
                 is a change a reviewer sees"
            ),
        )];
    };

    let code = without_test_modules(&code_only(&source.contents));
    let mut violations = Vec::new();

    // Exactly one of each, not "at least one". A scan that takes the first match is a scan a
    // decoy defeats: a `mod legacy` above the real one, carrying a conforming trait and a
    // conforming `impl`, satisfied every check below while the real declaration drifted.
    // Review of this change demonstrated it, so ambiguity is a violation rather than a
    // tie-break.
    let declaration = sole_declaration(&code, INTEGRITY_TRAIT, RULE, ADAPTER, &mut violations);
    let shipped = sole_declaration(
        &code,
        INTEGRITY_SHIPPED_IMPL,
        RULE,
        ADAPTER,
        &mut violations,
    );

    for seal in SEAL_BINDINGS {
        violations.extend(seal_binding_violations(seal, &code, declaration, shipped));
    }
    violations
}

/// The module a shipped seal's algorithm must be imported from.
pub const CHECKSUM_MODULE: &str = "crate::crc";

/// Whether `code` brings `name` into scope from [`CHECKSUM_MODULE`], unconditionally.
///
/// Both spellings a `use` can take: `use crate::crc::crc16;` and
/// `use crate::crc::{crc16, crc32};`. Three things do not count, and each is a way the
/// import could be there while the name resolves elsewhere:
///
/// * **An alias.** `use other::thing as crc16;` is exactly the rename this pin exists to see.
/// * **An attributed `use`.** Codex caught this on PR #60: `#[cfg(any())] use
///   crate::crc::{crc16, crc32};` is an import that never exists, and beside a local `fn
///   crc16` it is a textual proof of a resolution that does not happen. Any attribute at all
///   disqualifies it — a `cfg` this rule tried to evaluate would be a rule pretending to be
///   a compiler.
/// * **An import inside a nested module.** The round after, Codex found
///   `mod inner { use crate::crc::{crc16, crc32}; }` beside a file-scope
///   `use crate::forged::{crc16, crc32};`: the shipped `impl` is at file scope and cannot
///   see the nested one. Only depth-zero imports count.
/// * **A local definition of the same name**, which is checked by the caller: an import that
///   is shadowed by an item in the same file brings nothing into scope at the call.
fn imports_from_checksum_module(code: &str, name: &str) -> bool {
    let prefix = format!("use {CHECKSUM_MODULE}::");
    let mut attributed = false;
    let mut depth = 0_i32;

    for line in code.lines() {
        let trimmed = line.trim();
        let opens = i32::try_from(trimmed.matches('{').count()).unwrap_or(0);
        let closes = i32::try_from(trimmed.matches('}').count()).unwrap_or(0);
        let at_file_scope = depth == 0;
        // A `use` line's own braces — `use crate::crc::{crc16, crc32};` — are not a module
        // body, so they are not counted. Anything else that opens a brace is.
        if !trimmed.starts_with("use ") {
            depth = depth.saturating_add(opens).saturating_sub(closes);
        }
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let Some(at) = trimmed.find(&prefix) else {
            // An attribute on its own line applies to the item that follows it.
            attributed = trimmed.starts_with("#[");
            continue;
        };
        // Three ways an import can be there and not be in scope for the shipped `impl`:
        // an attribute on the line above, an attribute on the same line, or a nested module.
        // Codex found the third on PR #60 round 3: `mod inner { use crate::crc::{..}; }`
        // beside a file-scope `use forged::{..}` is an import the shipped impl cannot see.
        if attributed || at > 0 || !at_file_scope {
            attributed = false;
            continue;
        }
        attributed = false;
        let Some(rest) = trimmed.get(at.saturating_add(prefix.len())..) else {
            continue;
        };
        let Some(end) = rest.find(';') else {
            continue;
        };
        let Some(imported) = rest.get(..end) else {
            continue;
        };
        if !imported.contains(" as ")
            && imported
                .trim_matches(|character: char| character == '{' || character == '}')
                .split(',')
                .any(|item| item.trim() == name)
        {
            return true;
        }
    }

    false
}

/// The body of the one `header` in `code`, or `None` with a violation pushed.
///
/// Absent and ambiguous are different failures and get different messages, because they are
/// different mistakes: one is a rename or a deletion, the other is a second declaration that
/// makes the first unreadable.
fn sole_declaration<'a>(
    code: &'a str,
    header: &str,
    rule: &'static str,
    subject: &str,
    violations: &mut Vec<Violation>,
) -> Option<&'a str> {
    match count_tokens(code, header) {
        1 => braced_body(code, header),
        0 => {
            violations.push(Violation::new(
                rule,
                subject,
                format!(
                    "{INTEGRITY_BINDING_PATH} declares no `{header}`, so what it pins is \
                     pinned against nothing"
                ),
            ));
            None
        }
        found => {
            violations.push(Violation::new(
                rule,
                subject,
                format!(
                    "{INTEGRITY_BINDING_PATH} declares `{header}` {found} times, so a scan \
                     that reads the first one is reading whichever a contributor put first; \
                     one binding, or the pin is a decoy away from meaning nothing"
                ),
            ));
            None
        }
    }
}

/// Violations for a shipped seal method whose body is not exactly the delegation it must be.
///
/// A token count is not enough, which review of this change proved: `count_tokens(body,
/// "crc32") == 1` is satisfied by `fast::crc32(bytes)` calling a Castagnoli loop in a
/// sibling module, by `other::crc32(bytes)`, and by `{ let _ = crc32; forged(bytes) }`. Each
/// leaves `crc.rs` untouched, so the other half of the rule passes too, and the shipped seal
/// is quietly a different algorithm.
///
/// Counting *calls* was not enough either, which Codex caught on PR #60:
/// `{ let crc32 = |_| 0_u32; crc32(bytes) }` makes exactly one unqualified call to something
/// named `crc32`, and it is a closure returning zero. A name resolves against whatever is in
/// scope, and a scanner does not resolve names.
///
/// So the body must be the delegation and nothing else — one unqualified call, one argument,
/// no statement before it — and [`check_integrity_binding`] separately requires the name to
/// be imported from the checksum module. Between them there is nowhere left for a local
/// binding to stand. Strict on purpose: a binding of a seal to an algorithm that needs a
/// second statement is a review conversation, which is where ADR 0010 says a change to this
/// belongs.
fn delegation(body: &str, seal: &SealBinding, rule: &'static str, subject: &str) -> Vec<Violation> {
    let calls = calls(body);
    let mut violations = Vec::new();
    let complaint = |detail: String| Violation::new(rule, subject, detail);

    if !calls.is_empty() && !is_only_a_call(body) {
        violations.push(complaint(format!(
            "`Catalogued::{}` does more than delegate — its body is `{}` rather than one \
             call — so what computes the seal over {} is not decidable by reading it; a \
             local binding can shadow any name a scanner trusts",
            seal.method,
            body.split_whitespace().collect::<Vec<&str>>().join(" "),
            seal.covers
        )));
    }

    match calls.as_slice() {
        [call] if call.name == seal.delegates_to && !call.qualified => {}
        [call] if call.name == seal.delegates_to && call.qualified => {
            violations.push(complaint(format!(
                "`Catalogued::{}` calls a path-qualified `{}`, which may be any function of \
                 that name in any module; the seal over {} must be the one in the checksum \
                 module this rule's other half pins",
                seal.method, seal.delegates_to, seal.covers
            )));
        }
        [] => violations.push(complaint(format!(
            "`Catalogued::{}` calls nothing, so the shipped seal over {} is bound to no \
             algorithm",
            seal.method, seal.covers
        ))),
        found => violations.push(complaint(format!(
            "`Catalogued::{}` is not a single unqualified call to `{}` but {}, so the \
             shipped seal over {} is no longer ADR 0010's; a rebound checksum passes every \
             round-trip test in this repository and fails against every journal already on \
             a device",
            seal.method,
            seal.delegates_to,
            found
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<&str>>()
                .join(", "),
            seal.covers
        ))),
    }

    violations
}

/// Whether `body` is one call expression and nothing else: `name(argument)`.
///
/// Whitespace-normalised first, because the real bodies are one line and a formatter may not
/// keep them that way. A `let`, a second statement, a trailing semicolon or a compound
/// expression all fail, which is the point — see [`delegation`].
fn is_only_a_call(body: &str) -> bool {
    let normalised: String = body.split_whitespace().collect::<Vec<&str>>().join(" ");
    let trimmed = normalised.trim();
    let Some((callee, rest)) = trimmed.split_once('(') else {
        return false;
    };
    let Some(argument) = rest.strip_suffix(')') else {
        return false;
    };
    let is_identifier = |text: &str| {
        !text.is_empty()
            && text
                .chars()
                .all(|character| character.is_alphanumeric() || character == '_')
    };
    is_identifier(callee.trim()) && is_identifier(argument.trim())
}

/// A call found in a body: the function's name, and whether it was reached through a path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    name: String,
    qualified: bool,
}

/// Every call in `code`, in the order they appear.
///
/// A call is an identifier immediately followed by `(` or by a turbofish. Grouping
/// parentheses have no identifier before them and are skipped, and a `(` after a keyword is
/// not a call — `if (a)` and `match (a)` are not functions, and a rule that thought they
/// were would report a body nobody wrote.
fn calls(code: &str) -> Vec<Call> {
    const KEYWORDS: [&str; 8] = ["if", "match", "while", "for", "return", "in", "else", "as"];

    let characters: Vec<char> = code.chars().collect();
    let mut found = Vec::new();
    let mut at = 0;

    while at < characters.len() {
        if characters.get(at).copied() != Some('(') {
            at = at.saturating_add(1);
            continue;
        }
        // Walk back over whitespace, then over a turbofish, then over the identifier.
        let mut end = at;
        while end > 0
            && characters
                .get(end.saturating_sub(1))
                .is_some_and(|character| character.is_whitespace())
        {
            end = end.saturating_sub(1);
        }
        if end > 0 && characters.get(end.saturating_sub(1)).copied() == Some('>') {
            let mut depth = 0_i32;
            while end > 0 {
                match characters.get(end.saturating_sub(1)).copied() {
                    Some('>') => depth = depth.saturating_add(1),
                    Some('<') => depth = depth.saturating_sub(1),
                    _ => {}
                }
                end = end.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
            // Past the `::` of the turbofish.
            while end > 0 && characters.get(end.saturating_sub(1)).copied() == Some(':') {
                end = end.saturating_sub(1);
            }
        }
        let mut start = end;
        while start > 0
            && characters
                .get(start.saturating_sub(1))
                .is_some_and(|character| character.is_alphanumeric() || *character == '_')
        {
            start = start.saturating_sub(1);
        }
        let name: String = characters
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .collect();
        if !name.is_empty() && !KEYWORDS.contains(&name.as_str()) {
            let qualified =
                start > 0 && characters.get(start.saturating_sub(1)).copied() == Some(':');
            found.push(Call { name, qualified });
        }
        at = at.saturating_add(1);
    }

    found
}

/// Everything one seal's binding has to satisfy, in `integrity.rs`.
///
/// Split out of [`check_integrity_binding`] so that each half of the rule stays readable:
/// this one is about the shipped algorithm, and the caller is about the shape of the file
/// the algorithm is bound in.
fn seal_binding_violations(
    seal: &SealBinding,
    code: &str,
    declaration: Option<&str>,
    shipped: Option<&str>,
) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = Vec::new();
    // The name in the delegation resolves against whatever is in scope, and a scanner
    // does not resolve names. Pinning the import is the other half of what makes
    // `crc16(bytes)` mean the function ADR 0010 settled on rather than something a
    // `use` line was pointed at.
    if !imports_from_checksum_module(code, seal.delegates_to) {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "{INTEGRITY_BINDING_PATH} does not import `{}` from `{CHECKSUM_MODULE}` \
                 unconditionally and unaliased, so the call that computes the seal over \
                 {} may resolve to any function of that name",
                seal.delegates_to, seal.covers
            ),
        ));
    }
    // And the import must not be shadowed by an item in the same file. An import that is
    // there and a local `fn crc16` beside it is a call that resolves to the local one.
    if count_tokens(code, &format!("fn {}", seal.delegates_to)) != 0 {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "{INTEGRITY_BINDING_PATH} declares its own `{}`, which shadows the import \
                 and makes the seal over {} whatever that local function computes",
                seal.delegates_to, seal.covers
            ),
        ));
    }

    if let Some(body) = declaration {
        match signature(body, seal.method) {
            None => violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{INTEGRITY_TRAIT}` declares no `{}`, so the seal over {} has no \
                     width to be pinned at",
                    seal.method, seal.covers
                ),
            )),
            Some(found) => {
                if let Some(returned) = return_type(&found) {
                    if returned != seal.width {
                        violations.push(Violation::new(
                            RULE,
                            ADAPTER,
                            format!(
                                "`{}` returns `{returned}` rather than `{}`, so the seal \
                                 over {} has changed width; that is bytes on media for \
                                 the life of the format, not an implementation detail",
                                seal.method, seal.width, seal.covers
                            ),
                        ));
                    }
                } else {
                    violations.push(Violation::new(
                        RULE,
                        ADAPTER,
                        format!(
                            "`{}` declares no return type, so the seal over {} has no \
                             width to be pinned at",
                            seal.method, seal.covers
                        ),
                    ));
                }
            }
        }
    }

    if let Some(body) = shipped {
        match braced_body(body, &format!("fn {}", seal.method)) {
            None => violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{INTEGRITY_SHIPPED_IMPL}` does not implement `{}`, so the shipped \
                     seal over {} is bound to nothing",
                    seal.method, seal.covers
                ),
            )),
            Some(body) => violations.extend(delegation(body, seal, RULE, ADAPTER)),
        }
    }

    violations
}

/// Rule: the codec reaches its seals through the trait rather than around it.
///
/// Reported under `integrity-check` like the other two halves. This is the one that stops
/// the swap point being decorative: `integrity.rs` can be perfect and the codec can still
/// call `crc16` and `crc32` directly, in which case the type parameter selects nothing and
/// every journal is sealed with the shipped check whatever a caller asked for. Review of
/// this change confirmed that mutation passed all 34 rules before this existed.
#[must_use]
pub fn check_integrity_routing(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let Some(source) = find_source(sources, INTEGRITY_ROUTING_PATH) else {
        return vec![Violation::new(
            RULE,
            ADAPTER,
            format!(
                "no {INTEGRITY_ROUTING_PATH} in the workspace, so nothing says the codec \
                 still reaches its seals through the integrity trait"
            ),
        )];
    };

    let code = without_test_modules(&code_only(&source.contents));
    let mut violations = Vec::new();
    let checksums: Vec<&str> = SEAL_BINDINGS.iter().map(|seal| seal.delegates_to).collect();

    // Derived, not whitelisted, for the reason `check_bank_routing` derives: a table of five
    // names pins five bodies and says nothing about a sixth. A
    // `pub fn frame_len_of_with<C: IntegrityCheck>` that took the type parameter, ignored it
    // and called `crc16` would be a second reader of a header sealed by a check its caller
    // did not choose, with every other rule green.
    for name in integrity_generic_functions(&code) {
        if !SEALING_FUNCTIONS.iter().any(|(pinned, _)| *pinned == name) {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{INTEGRITY_ROUTING_PATH} declares `fn {name}` generic over the integrity \
                     check and no row of `SEALING_FUNCTIONS` pins it, so a body that can \
                     compute a seal is pinned by nothing; add a row naming the seals it must \
                     route through, or stop taking `C`"
                ),
            ));
        }
    }

    for (function, methods) in SEALING_FUNCTIONS {
        // Filtered from `SEAL_BINDINGS` rather than described here, so a seal whose width or
        // name changes changes in one place. A method named in a row that no longer exists
        // in `SEAL_BINDINGS` selects nothing, which is why the count is checked.
        let seals: Vec<&SealBinding> = SEAL_BINDINGS
            .iter()
            .filter(|seal| methods.contains(&seal.method))
            .collect();
        if seals.len() != methods.len() {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{function}` is pinned to a seal `SEAL_BINDINGS` does not declare, so \
                     the row checks less than it says it does"
                ),
            ));
            continue;
        }
        violations.extend(sealing_function_violations(
            INTEGRITY_ROUTING_PATH,
            function,
            &code,
            &seals,
            &checksums,
        ));
    }
    // Both of these were token counts until Codex's third round, and both fell to the same
    // thing the sealing functions did: `let _ = crc32; 0` is valid in a `const fn`, and
    // `let _ = decode_with::<C>;` beside `decode(rest)` makes every scan verify with the
    // default check whatever its caller asked for.
    let (digest, computed_with) = DIGEST_FUNCTION;
    violations.extend(used_call(
        &code,
        digest,
        computed_with,
        "so ADR 0011's digest is no longer the frame's own seal, and a scheduled effect \
         records a number no replay can reproduce",
    ));

    let (step, walks_with) = SCAN_STEP;
    violations.extend(used_call(
        &code,
        step,
        walks_with,
        "so a scan verifies with whichever check the codec defaults to rather than the one \
         its caller asked for",
    ));

    violations.extend(checksums_named_outside_the_digest(&code, &checksums));

    for (function, verified_with) in [HEADER_STEP, FRAME_LEN_STEP] {
        violations.extend(used_call(
            &code,
            function,
            verified_with,
            "so a header's `payload_len` decides where a frame ends without the seal over it \
             having been checked, which is design document \u{a7}09's first checksum undone",
        ));
    }

    violations
}

/// Violations for every checksum the record codec names outside its one documented exception.
///
/// File-wide, which is what a per-body ban is not. Review of this change demonstrated the hole
/// the per-body version leaves: a `fn shadow_seal<C>(..) -> u16 where C: IntegrityCheck`
/// calling `crc16`, added beside the pinned bodies and called from one of them, passed every
/// rule. [`integrity_generic_functions`] now sees that shape; this closes the same hole for a
/// helper that takes no `C` at all, which no scan over signatures can see by construction.
///
/// `input_digest` is the exception, and a named one rather than a habit: [`DIGEST_FUNCTION`]
/// says which function it is and which checksum it may call, because a `const fn` cannot go
/// through a trait method. `bank.rs` and `recovery.rs` have no exception at all.
fn checksums_named_outside_the_digest(code: &str, checksums: &[&str]) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    // `use` lines dropped first: an import is not a call, `input_digest` needs one, and an
    // import nothing calls fails CI on its own under `unused_imports` with `-D warnings`.
    let callable: String = code
        .lines()
        .filter(|line| !line.trim_start().starts_with("use "))
        .collect::<Vec<&str>>()
        .join("\n");
    let exempt = braced_body(&callable, &format!("fn {}", DIGEST_FUNCTION.0)).unwrap_or_default();

    checksums
        .iter()
        .filter_map(|checksum| {
            let outside =
                count_tokens(&callable, checksum).saturating_sub(count_tokens(exempt, checksum));
            (outside != 0).then(|| {
                Violation::new(
                    RULE,
                    ADAPTER,
                    format!(
                        "{INTEGRITY_ROUTING_PATH} names `{checksum}` {outside} time(s) outside \
                         `{}`, which is the only body permitted to reach the checksum module \
                         directly. Every other seal here goes through the trait, or the type \
                         parameter selects nothing",
                        DIGEST_FUNCTION.0
                    ),
                )
            })
        })
        .collect()
}

/// Every function in `code` that is generic over the integrity check, by name.
///
/// A function is generic over the check when its own signature names the bound, or when it is
/// declared inside an `impl` block whose header does. Both halves are needed and both were
/// once missing: review of this change demonstrated three shapes that reached a seal through
/// `C` and were invisible to a one-line scan, each of which passed the whole gate.
///
/// * `fn shadow_seal<C>(..) -> u16 where C: IntegrityCheck` — the `fn` line has no bound and
///   the bound line has no `fn`. Signatures are therefore **joined** up to the `{` or `;`
///   that ends them, so a `where` clause and a wrapped signature read the same as a
///   single-line one.
/// * A method inside `impl<'a, C: IntegrityCheck> Scan<'a, C>`, which is the most likely
///   place somebody would actually put such code. An `impl` header naming the bound therefore
///   makes every `fn` it directly contains generic too.
/// * The same again with the `impl` header itself split across lines, which the joining
///   above covers.
///
/// It is still a floor, and the floor is stated rather than implied: it counts a `fn` as a
/// direct child of the block that makes it callable, so a nested `fn` inside a method body is
/// missed, and a body that computes a seal without taking `C` at all is caught by the
/// file-wide checksum ban instead of by this.
fn integrity_generic_functions(code: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut depth: i32 = 0;
    // Depths at which the body of an `impl` generic over the check begins. A `fn` opened at
    // one of these depths is generic whether or not its own signature says so.
    let mut generic_impls: Vec<i32> = Vec::new();
    // A declaration being joined across lines, and whether it is an `impl` header.
    let mut joining: Option<(String, bool)> = None;

    for line in code.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        if joining.is_none() {
            // Leading attributes set aside for `future_implementors`'s reason, and this is
            // the sharper of the two: an `impl` header the scan does not join registers no
            // depth, so every method inside a generic `impl` block goes unfound and is
            // pinned by no row of `SEALING_FUNCTIONS`. Codex round 4's class, third
            // instance. `struct_literal_positions`'s `starts_with("impl")` is the fourth
            // and is left alone: blinding it reports a declaration as a construction, so
            // it over-reports rather than going quiet.
            let bare = crate::size::without_leading_attributes(trimmed);
            let is_impl = bare.starts_with("impl");
            if is_impl || trimmed.contains("fn ") {
                joining = Some((String::new(), is_impl));
            }
        }
        if let Some((buffer, _)) = joining.as_mut() {
            buffer.push(' ');
            buffer.push_str(trimmed);
        }

        let opens = i32::try_from(trimmed.matches('{').count()).unwrap_or(0);
        let closes = i32::try_from(trimmed.matches('}').count()).unwrap_or(0);

        // A declaration ends at the brace that opens its body, or at the `;` of one that has
        // none — a trait method's signature, say.
        if opens > 0 || trimmed.ends_with(';') {
            if let Some((declaration, is_impl)) = joining.take() {
                let bound = declaration.contains("IntegrityCheck");
                if is_impl {
                    if bound && opens > 0 {
                        generic_impls.push(depth.saturating_add(1));
                    }
                } else if let Some(name) = function_declaration_name(&declaration) {
                    if bound || generic_impls.contains(&depth) {
                        found.push(name);
                    }
                }
            }
        }

        depth = depth.saturating_add(opens).saturating_sub(closes);
        // Every generic `impl` body this line closed is no longer open.
        generic_impls.retain(|body| *body <= depth);
    }

    found
}

/// The name a `fn` signature line declares, if the line declares one.
fn function_declaration_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    // A `use` line or a `where` clause mentions the bound without declaring anything.
    if trimmed.starts_with("use ") || trimmed.starts_with("//") {
        return None;
    }
    // `fn` has to stand as a word: `impl Foo for Bar` carries no `fn `, but a joined
    // declaration can carry a lifetime or a path that ends in one.
    let (_, after) = trimmed.split_once("fn ")?;
    let name: String = after
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Rule: the recovery reader reaches the codec through the check its caller chose.
///
/// Reported under `integrity-check` like the other three halves, because it is the same
/// decision. `Recovery<C>`'s type parameter is a promise that a journal is verified with the
/// algorithm that sealed it; two calls keep that promise, and review of this change
/// demonstrated that replacing both with their non-generic siblings passed every rule and
/// every test. A parameter nothing is obliged to use is a swap point that selects nothing.
///
/// Three checks, and the file-wide ban is the one that does not depend on a name:
///
/// * every step in [`RECOVERY_ROUTING_STEPS`] calls its codec entry point exactly once and
///   uses the answer;
/// * the file names no checksum function at all — unlike `frame.rs` there is no `const fn`
///   here and so no exception, exactly as in `bank.rs`;
/// * and the file names no [`INTEGRITY_SHIPPED_IMPL`] method directly, so a body cannot reach
///   `Catalogued::header_check` around the type parameter either.
#[must_use]
pub fn check_recovery_routing(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let Some(source) = find_source(sources, RECOVERY_ROUTING_PATH) else {
        return vec![Violation::new(
            RULE,
            ADAPTER,
            format!(
                "no {RECOVERY_ROUTING_PATH} in the workspace, so nothing says the recovery \
                 reader still verifies with the check its caller chose"
            ),
        )];
    };

    let code = without_test_modules(&code_only(&source.contents));
    let callable: String = code
        .lines()
        .filter(|line| !line.trim_start().starts_with("use "))
        .collect::<Vec<&str>>()
        .join("\n");
    let mut violations = Vec::new();

    for seal in SEAL_BINDINGS {
        for named in [seal.delegates_to, seal.method] {
            if count_tokens(&callable, named) != 0 {
                violations.push(Violation::new(
                    RULE,
                    ADAPTER,
                    format!(
                        "{RECOVERY_ROUTING_PATH} names `{named}` directly. The reader computes \
                         no seal of its own: it hands bytes to the codec, and the codec is \
                         what the type parameter selects"
                    ),
                ));
            }
        }
    }

    for (step, through) in RECOVERY_ROUTING_STEPS {
        let Some(body) = braced_body(&code, &format!("fn {step}")) else {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{RECOVERY_ROUTING_PATH} declares no `fn {step}`, so the reader's route to \
                     the codec is pinned against nothing"
                ),
            ));
            continue;
        };
        let generic = format!("{through}::<C>");
        match invocation(body, &generic) {
            Invocation::Once => {}
            Invocation::Discarded | Invocation::Missing | Invocation::Repeated => {
                violations.push(Violation::new(
                    RULE,
                    ADAPTER,
                    format!(
                        "`{step}` does not call `{generic}` exactly once and use the answer, so \
                         a recovery verifies with whichever check the codec defaults to rather \
                         than the one its caller asked for"
                    ),
                ));
            }
        }
    }

    violations
}

/// Rule: the writer seals a record with the check its caller chose.
///
/// Reported under `integrity-check` like the other three halves, because it is the same
/// decision, and written the same way as [`check_recovery_routing`]: the file names no
/// checksum function and no seal method at all, and each step in [`APPEND_ROUTING_STEPS`]
/// reaches the codec through the *generic* entry point.
///
/// Without it, `frame::encode` in place of `frame::encode_with::<C>` would compile, pass
/// every rule, and seal every appended record with the shipped check whatever the recovery
/// that positioned the writer verified with — which is a journal one half of the firmware
/// can read.
#[must_use]
pub fn check_append_routing(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let Some(source) = find_source(sources, APPEND_ROUTING_PATH) else {
        return vec![Violation::new(
            RULE,
            ADAPTER,
            format!(
                "no {APPEND_ROUTING_PATH} in the workspace, so nothing says the writer still \
                 seals with the check its caller chose"
            ),
        )];
    };

    let code = without_test_modules(&code_only(&source.contents));
    let callable: String = code
        .lines()
        .filter(|line| !line.trim_start().starts_with("use "))
        .collect::<Vec<&str>>()
        .join("\n");
    let mut violations = Vec::new();

    for seal in SEAL_BINDINGS {
        for named in [seal.delegates_to, seal.method] {
            if count_tokens(&callable, named) != 0 {
                violations.push(Violation::new(
                    RULE,
                    ADAPTER,
                    format!(
                        "{APPEND_ROUTING_PATH} names `{named}` directly. The writer computes \
                         no seal of its own: it hands a record to the codec, and the codec is \
                         what the type parameter selects"
                    ),
                ));
            }
        }
    }

    for (step, through) in APPEND_ROUTING_STEPS {
        let Some(body) = braced_body(&code, &format!("fn {step}")) else {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{APPEND_ROUTING_PATH} declares no `fn {step}`, so the writer's route to \
                     the codec is pinned against nothing"
                ),
            ));
            continue;
        };
        let generic = format!("{through}::<C>");
        match invocation(body, &generic) {
            Invocation::Once => {}
            Invocation::Discarded | Invocation::Missing | Invocation::Repeated => {
                violations.push(Violation::new(
                    RULE,
                    ADAPTER,
                    format!(
                        "`{step}` does not call `{generic}` exactly once and use the answer, \
                         so an appended record is sealed with whichever check the codec \
                         defaults to rather than the one its caller asked for"
                    ),
                ));
            }
        }
    }

    violations
}

/// Rule: the bank codec reaches its seals through the trait rather than around it.
///
/// The same decision as [`check_integrity_routing`], reported under the same id, for the
/// second file that computes seals. Design document §10's bank header and generation seal
/// are sealed with the same two checks a record frame is, and a caller that chose an
/// integrity check gets to have chosen it for its whole device — a firmware whose banks are
/// sealed with one algorithm and whose records are sealed with another is a firmware that
/// cannot read half of what it wrote.
///
/// Each body is held to the seals its row in [`BANK_SEALING_FUNCTIONS`] names, exactly once
/// and with the answer used, and to naming no checksum function at all. There is no
/// `DIGEST_FUNCTION`-style exception here: nothing in the bank codec is a `const fn`, so
/// every seal on this path can and does go through the trait.
#[must_use]
pub fn check_bank_integrity_routing(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let Some(source) = find_source(sources, BANK_ROUTING_PATH) else {
        return vec![Violation::new(
            RULE,
            ADAPTER,
            format!(
                "no {BANK_ROUTING_PATH} in the workspace, so nothing says the bank header \
                 and the generation seal still reach their checks through the integrity \
                 trait"
            ),
        )];
    };

    let code = without_test_modules(&code_only(&source.contents));
    let checksums: Vec<&str> = SEAL_BINDINGS.iter().map(|seal| seal.delegates_to).collect();
    let mut violations = Vec::new();

    // File-wide rather than per-body, which is what `frame.rs` has to settle for. `frame.rs`
    // has one documented exception — `input_digest` is a `const fn`, so it cannot go through
    // a trait method — and `bank.rs` has none: nothing here is `const`, so every seal on this
    // path can and does go through `C`. A ban on the whole file therefore costs nothing and
    // closes the hole a per-body ban leaves, which is a *new* helper that names a checksum
    // and is called from a pinned body.
    for checksum in &checksums {
        if count_tokens(&code, checksum) != 0 {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{BANK_ROUTING_PATH} names `{checksum}` directly. The bank codec has no \
                     `const fn` that needs one, so every seal here goes through `C` and a \
                     direct call is a seal the caller's chosen check did not compute"
                ),
            ));
        }
    }

    // Derived, not whitelisted. A table of five names pins five bodies and says nothing
    // about a sixth: review of this change added a `pub fn header_is_intact_with<C: IntegrityCheck>`
    // that took the type parameter, ignored it, called `crc16` and passed every rule. It also
    // says nothing about a row *deleted* — dropping `seal_for_with` from the table and
    // rewiring its body to `crc32` left the gate and all 538 tests green, because the clean
    // fixture is rendered from the same table and adapts to any deletion. Reading the file
    // for functions generic over the check closes both directions.
    for name in integrity_generic_functions(&code) {
        if !BANK_SEALING_FUNCTIONS
            .iter()
            .any(|(pinned, _)| *pinned == name)
        {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{BANK_ROUTING_PATH} declares `fn {name}` generic over the integrity \
                     check and no row of `BANK_SEALING_FUNCTIONS` pins it, so a body that can \
                     compute a seal is pinned by nothing; add a row naming the seals it must \
                     route through, or stop taking `C`"
                ),
            ));
        }
    }

    for (function, methods) in BANK_SEALING_FUNCTIONS {
        // Filtered from `SEAL_BINDINGS` rather than described here, so a seal whose width or
        // name changes changes in one place. A method named in a row that no longer exists
        // in `SEAL_BINDINGS` selects nothing, which is why the count is checked.
        let seals: Vec<&SealBinding> = SEAL_BINDINGS
            .iter()
            .filter(|seal| methods.contains(&seal.method))
            .collect();
        if seals.len() != methods.len() {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{function}` is pinned to a seal `SEAL_BINDINGS` does not declare, so \
                     the row checks less than it says it does"
                ),
            ));
            continue;
        }
        violations.extend(sealing_function_violations(
            BANK_ROUTING_PATH,
            function,
            &code,
            &seals,
            // Empty: the file-wide ban above already covers every body in this file, and
            // reporting the same mutation twice makes a failure harder to read, not safer.
            &[],
        ));
    }

    violations
}

/// Violations unless `function`'s body calls `callee` exactly once and uses the answer.
///
/// The shape three of this rule's checks share, factored out after the third round of review
/// found the same hole in each of them separately: a token count is satisfied by a mention,
/// and counting calls is satisfied by a call whose answer is thrown away.
fn used_call(code: &str, function: &str, callee: &str, consequence: &str) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let Some(body) = braced_body(code, &format!("fn {function}")) else {
        return vec![Violation::new(
            RULE,
            ADAPTER,
            format!("{INTEGRITY_ROUTING_PATH} declares no `fn {function}`"),
        )];
    };

    match invocation(body, callee) {
        Invocation::Once => Vec::new(),
        Invocation::Discarded => vec![Violation::new(
            RULE,
            ADAPTER,
            format!("`{function}` calls `{callee}` and throws the answer away, {consequence}"),
        )],
        Invocation::Missing | Invocation::Repeated => vec![Violation::new(
            RULE,
            ADAPTER,
            format!(
                "`{function}` does not call `{callee}` exactly once and use the answer, \
                 {consequence}"
            ),
        )],
    }
}

/// What `code` does with `path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Invocation {
    /// Called once, and the answer is used.
    Once,
    /// Never called. A mention — `let _ = C::header_check;` — is this, not a call.
    Missing,
    /// Called more than once, so which one seals the frame is not decidable by reading it.
    Repeated,
    /// Called, and the answer thrown away.
    Discarded,
}

/// How `code` uses `path`: called once into something, or one of the three ways that is not
/// a route to a checksum.
///
/// A call is the path at a token boundary followed, after any whitespace, by `(` or a
/// turbofish. Two rounds of review were needed to get this far, and both are worth stating
/// because each looked like the whole answer at the time:
///
/// * a token count made `let _ = C::header_check;` a route, beside a seal some other helper
///   computed;
/// * counting *calls* made `let _ = C::header_check(&sealed_header);` a route, for the same
///   reason — the call is real and its answer goes nowhere.
///
/// So the result has to be used: bound to a pattern that is not `_`, or consumed on the spot
/// by a method call or a comparison, which are the two things a checksum is for here. A body
/// that computes a seal and uses it three statements later fails this, deliberately — see
/// [`delegation`] for why these pins are strict rather than clever.
fn invocation(code: &str, path: &str) -> Invocation {
    let continues = |character: char| character.is_alphanumeric() || character == '_';
    let mut found = Invocation::Missing;

    for (at, _) in code.match_indices(path) {
        let before = code.get(..at).unwrap_or_default();
        let before_is_boundary = before
            .chars()
            .next_back()
            .is_none_or(|character| !continues(character) && character != ':');
        let after = code
            .get(at.saturating_add(path.len())..)
            .unwrap_or_default();
        let trimmed = after.trim_start();
        if !before_is_boundary || !(trimmed.starts_with('(') || trimmed.starts_with("::<")) {
            continue;
        }
        if found != Invocation::Missing {
            return Invocation::Repeated;
        }
        found = if result_is_used(before, after) {
            Invocation::Once
        } else {
            Invocation::Discarded
        };
    }

    found
}

/// Whether a call's answer goes anywhere.
///
/// `before` is everything ahead of the call, `after` everything from its own name onward.
/// Used is one of two things:
///
/// * **Bound to a name the compiler will hold you to.** `let frame_crc = C::frame_check(..)`
///   is fine precisely because `unused_variables` is a warning and this workspace builds
///   with `-D warnings`: a binding nothing reads fails CI on its own, so this rule does not
///   have to trace it. `let _ =` and `let _selected =` are the exceptions, and they are
///   Codex's third finding on PR #60 — an underscore is how a Rust author says "I know this
///   is unused", so it silences the one check that would otherwise catch a seal computed and
///   abandoned.
/// * **Consumed where it stands**: the closing parenthesis is followed by `.` for a method
///   call, `!` or `=` for a comparison, `{` for a `match` scrutinee, `,` or `)` for an
///   argument — or by nothing at all, which makes it the body's tail expression and so the
///   function's return value. Every one of those is a shape the real codec uses, and the
///   test `a_seal_compared_or_bound_by_name_is_a_route_to_it` pins them so this rule cannot
///   quietly become "refuse everything".
///
/// What this still cannot see is a named binding that is read by something other than the
/// expression that stores the seal. That needs dataflow rather than a scan, and
/// [CLAUDE.md](https://github.com/madmax983/waymaker/blob/main/CLAUDE.md) says so under
/// "What is not checked" rather than leaving the limit for somebody to find.
fn result_is_used(before: &str, after: &str) -> bool {
    if let Some(pattern) = before.trim_end().strip_suffix('=').map(str::trim_end) {
        let bound = pattern
            .rsplit_once("let ")
            .map_or(pattern, |(_, tail)| tail)
            .split(':')
            .next()
            .unwrap_or_default()
            .trim();
        // A destructuring pattern binds several names; the compiler holds you to each.
        return !bound.starts_with('_');
    }

    let mut depth = 0_i32;
    for (index, character) in after.char_indices() {
        match character {
            '(' => depth = depth.saturating_add(1),
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let tail = after
                        .get(index.saturating_add(1)..)
                        .unwrap_or_default()
                        .trim_start();
                    // Nothing after it — including only the closing brace of the body it is
                    // the last expression of — makes it the return value.
                    return tail.is_empty()
                        || tail.starts_with('.')
                        || tail.starts_with('!')
                        || tail.starts_with('=')
                        || tail.starts_with('{')
                        || tail.starts_with(',')
                        || tail.starts_with(')');
                }
            }
            _ => {}
        }
    }
    false
}

/// Everything one sealing function has to satisfy, in `frame.rs`.
///
/// Split out of [`check_integrity_routing`] to keep each function under the workspace's
/// line limit, and because the two halves answer different questions: this one is "does this
/// body reach the seal through the trait", and the caller's is "are the bodies that must do
/// so all there".
fn sealing_function_violations(
    path: &str,
    function: &str,
    code: &str,
    seals: &[&SealBinding],
    checksums: &[&str],
) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = Vec::new();
    // Exactly one, not "the first one". `braced_body` takes the first textual match, so a
    // second declaration of a pinned name is a decoy: a `mod shim` above the real code,
    // carrying five conforming bodies, satisfied every check below while every real call
    // site was rewired to `crc16` and `crc32`. Review of this change demonstrated it on both
    // routing pins, so ambiguity is a violation rather than a tie-break — the same rule
    // `check_integrity_binding` already applies to the trait and its `impl`.
    let declaration = format!("fn {function}");
    let declared = count_tokens(code, &declaration);
    if declared != 1 {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            if declared == 0 {
                format!(
                    "{path} declares no `fn {function}`, so the codec's route to its seals \
                     is pinned against nothing"
                )
            } else {
                format!(
                    "{path} declares `fn {function}` {declared} times, so a scan that reads \
                     the first one is reading whichever a contributor put first; one \
                     declaration, or the pin is a decoy away from meaning nothing"
                )
            },
        ));
        return violations;
    }
    let Some(body) = braced_body(code, &declaration) else {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "{path} declares no `fn {function}`, so the codec's \
                 route to its seals is pinned against nothing"
            ),
        ));
        return violations;
    };
    for seal in seals {
        let through = format!("C::{}", seal.method);
        // An *invocation*, not a mention. Codex caught this on PR #60: a token count is
        // satisfied by `let _ = C::header_check;` left beside a seal some other helper
        // now computes, and the checksum-name check below would not see a helper called
        // anything else. A dead reference is not a call.
        match invocation(body, &through) {
            Invocation::Once => {}
            Invocation::Discarded => violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{function}` calls `{through}` and throws the answer away, so the \
                     seal over {} is whatever the next expression computes; a call whose \
                     result is discarded is not a route to a checksum either",
                    seal.covers
                ),
            )),
            Invocation::Missing | Invocation::Repeated => violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{function}` does not call `{through}` exactly once, so the seal \
                     over {} is not computed by the integrity check its caller chose — \
                     and a mention of it that is not a call is not a route to it",
                    seal.covers
                ),
            )),
        }
    }
    for checksum in checksums {
        if count_tokens(body, checksum) != 0 {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{function}` names `{checksum}` directly, which goes around the \
                     trait the choice of algorithm lives in; the codec seals through \
                     `C`, and the one documented exception is `{}`",
                    DIGEST_FUNCTION.0
                ),
            ));
        }
    }

    violations
}

/// The text of the signature `fn <name>` opens: its parameter list and what follows, up to
/// the body, the `;`, or a `where` clause.
///
/// The parameter list is skipped by depth-counting parentheses rather than by scanning for
/// the first `;`. Review of this change found why that matters: `fn header_check(bytes:
/// &[u8; 10])` is a plausible refactor — the seal covers exactly ten bytes — and cutting at
/// the first `;` truncated the signature mid-parameter, then reported the *width* as wrong.
/// A rule whose message names the wrong cause is worse than one that says nothing.
fn signature(code: &str, name: &str) -> Option<String> {
    let header = format!("fn {name}");
    let continues = |character: char| character.is_alphanumeric() || character == '_';
    let after = code.match_indices(&header).find_map(|(index, _)| {
        let before_is_boundary = code
            .get(..index)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|character| !continues(character));
        let rest = code.get(index.saturating_add(header.len())..)?;
        let after_is_boundary = rest.chars().next().is_none_or(|c| !continues(c));
        (before_is_boundary && after_is_boundary).then_some(rest)
    })?;

    let mut depth = 0_i32;
    let mut end = None;
    for (index, character) in after.char_indices() {
        match character {
            '(' => depth = depth.saturating_add(1),
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(index.saturating_add(1));
                    break;
                }
            }
            _ => {}
        }
    }
    let tail = after.get(end?..)?;
    let stop = [tail.find([';', '{']), tail.find(" where ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(tail.len());
    tail.get(..stop).map(str::to_owned)
}

/// The return type a signature's tail declares, if it declares one.
fn return_type(tail: &str) -> Option<String> {
    tail.split_once("->")
        .map(|(_, returned)| returned.split_whitespace().collect::<Vec<&str>>().join(" "))
        .filter(|returned| !returned.is_empty())
}

/// Rule: a scheduled effect records exactly the metadata ADR 0011 settled on.
///
/// Scanned rather than parsed, like every rule in this module, and scanned over code with
/// its comments and string literals stripped, so that a doc comment — which is full of
/// colons — cannot add a field and a
/// commented-out one cannot keep the pin green. The declaration is found by locating the
/// `enum RecordRef` body first: `Self::EffectScheduled { .. }` appears in every `match` over
/// the enum, and a scan that took the first mention would pin a pattern.
#[must_use]
pub fn check_effect_scheduled_fields(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "effect-scheduled-fields";
    const KERNEL: &str = "waymaker-core";
    const ENUM: &str = "enum RecordRef";
    const VARIANT: &str = "EffectScheduled";

    let Some(source) = find_source(sources, EFFECT_SCHEDULED_PATH) else {
        return vec![Violation::new(
            RULE,
            KERNEL,
            format!(
                "no {EFFECT_SCHEDULED_PATH} in the workspace, so the pinned field set is \
                 checking nothing; \u{a7}16's third deferred question is how much metadata a \
                 scheduled effect carries, and every extra field is paid per effect for the \
                 life of the format"
            ),
        )];
    };

    let code = without_test_modules(&code_only(&source.contents));
    // Read the *only* declaration, or none at all. `braced_body` takes the first
    // token-boundary match, so a same-named decoy above the real enum — a conforming
    // `mod compat { pub(crate) enum RecordRef { .. } }` — is what every scan below would
    // check, and the shipped enum could then gain or lose any field it liked. Review of this
    // change ran exactly that and watched the gate print `ok`. The same guard
    // `kernel-boundary`, `timer-capability` and `effect-protocol` already apply, and for the
    // same reason.
    //
    // Counted over production code alone. `code_only` strips comments and strings and leaves
    // `#[cfg(test)]` modules, so a harmless test fixture named `enum RecordRef` would fail
    // this guard — and, worse, a conforming test-only declaration would satisfy the pin if
    // the shipped enum ever moved to another file, with nothing that ships checked. Codex
    // found that. A fixture discharges nothing about the code on a device.
    let declarations = declaration_count(&code, ENUM);
    if declarations != 1 {
        return vec![Violation::new(
            RULE,
            KERNEL,
            format!(
                "{EFFECT_SCHEDULED_PATH} declares `{ENUM}` {declarations} times, not once;                  the scan below reads the first, so a decoy above the real one is what it                  would check"
            ),
        )];
    }
    let Some(body) = braced_body(&code, ENUM) else {
        return vec![Violation::new(
            RULE,
            KERNEL,
            format!(
                "{EFFECT_SCHEDULED_PATH} declares no `{ENUM}`, so the pinned field set is \
                 checking nothing"
            ),
        )];
    };
    let Some(variant) = braced_body(body, VARIANT) else {
        return vec![Violation::new(
            RULE,
            KERNEL,
            format!(
                "`{ENUM}` in {EFFECT_SCHEDULED_PATH} has no `{VARIANT}` variant with a field \
                 list, so the pinned field set is checking nothing"
            ),
        )];
    };

    let declared = field_names(variant);
    let pinned: BTreeSet<&str> = EFFECT_SCHEDULED_FIELDS.iter().copied().collect();
    let found: BTreeSet<&str> = declared.iter().map(String::as_str).collect();
    let mut violations = Vec::new();

    for added in found.difference(&pinned) {
        violations.push(Violation::new(
            RULE,
            KERNEL,
            format!(
                "`RecordRef::{VARIANT}` declares `{added}`, which is not in \
                 EFFECT_SCHEDULED_FIELDS; ADR 0011 settled the metadata a scheduled effect \
                 carries, and a field added here is paid per effect, per record, in flash \
                 and in write amplification"
            ),
        ));
    }
    for removed in pinned.difference(&found) {
        violations.push(Violation::new(
            RULE,
            KERNEL,
            format!(
                "`RecordRef::{VARIANT}` no longer declares `{removed}`, which \
                 EFFECT_SCHEDULED_FIELDS pins; a field dropped is a wire-format change on a \
                 record firmware in the field has already written"
            ),
        ));
    }

    violations
}

/// Rule: design document §11's two timer records carry what §11 says they carry.
///
/// `effect-scheduled-fields`'s twin, and a rule of its own rather than a second list inside
/// it because the two answer different questions: ADR 0011 settled how much *metadata* a
/// scheduled effect carries, and this settles which *facts about time* reach media. A
/// failure that named the effect rule would send a reader to ADR 0011 for a decision it
/// does not hold.
///
/// The pin fails in both directions. A field added is bytes on every timer for the life of
/// the format; a field removed is a wire-format change on a record firmware in the field
/// has already written, and for `clock_kind` and `armed_at` it is also the semantic loss
/// §11 names — see [`TIMER_RECORD_FIELDS`].
///
/// Read with `#[cfg(test)]` modules removed, for `integrity-check`'s reason, and the
/// declaration is found by locating the `enum RecordRef` body first: `Self::TimerFired`
/// appears in every `match` over the enum, so a scan that took the first mention would pin
/// a pattern.
///
/// What it cannot see is a *width*: it compares names, exactly as `effect-scheduled-fields`
/// does, so a `deadline` narrowed to a `u32` is invisible to it and is
/// `crates/waymaker-flash/tests/frame.rs`'s golden bytes.
#[must_use]
pub fn check_timer_record_fields(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "timer-record-fields";
    const KERNEL: &str = "waymaker-core";
    const ENUM: &str = "enum RecordRef";

    let Some(source) = find_source(sources, EFFECT_SCHEDULED_PATH) else {
        return vec![Violation::new(
            RULE,
            KERNEL,
            format!(
                "no {EFFECT_SCHEDULED_PATH} in the workspace, so the pinned timer record \
                 bodies are checking nothing; \u{a7}11 puts a clock kind on media so that \
                 recovery cannot reinterpret one policy as another"
            ),
        )];
    };

    let code = without_test_modules(&code_only(&source.contents));
    // Read the *only* declaration, or none at all. `braced_body` takes the first
    // token-boundary match, so a same-named decoy above the real enum — a conforming
    // `mod compat { pub(crate) enum RecordRef { .. } }` — is what every scan below would
    // check, and the shipped enum could then gain or lose any field it liked. Review of this
    // change ran exactly that and watched the gate print `ok`. The same guard
    // `kernel-boundary`, `timer-capability` and `effect-protocol` already apply, and for the
    // same reason.
    //
    // Counted over production code alone. `code_only` strips comments and strings and leaves
    // `#[cfg(test)]` modules, so a harmless test fixture named `enum RecordRef` would fail
    // this guard — and, worse, a conforming test-only declaration would satisfy the pin if
    // the shipped enum ever moved to another file, with nothing that ships checked. Codex
    // found that. A fixture discharges nothing about the code on a device.
    let declarations = declaration_count(&code, ENUM);
    if declarations != 1 {
        return vec![Violation::new(
            RULE,
            KERNEL,
            format!(
                "{EFFECT_SCHEDULED_PATH} declares `{ENUM}` {declarations} times, not once;                  the scan below reads the first, so a decoy above the real one is what it                  would check"
            ),
        )];
    }
    let Some(body) = braced_body(&code, ENUM) else {
        return vec![Violation::new(
            RULE,
            KERNEL,
            format!(
                "{EFFECT_SCHEDULED_PATH} declares no `{ENUM}`, so the pinned timer record \
                 bodies are checking nothing"
            ),
        )];
    };

    let mut violations = Vec::new();
    for (variant, fields) in TIMER_RECORD_FIELDS {
        let Some(declared) = braced_body(body, variant) else {
            violations.push(Violation::new(
                RULE,
                KERNEL,
                format!(
                    "`{ENUM}` in {EFFECT_SCHEDULED_PATH} has no `{variant}` variant with a \
                     field list, so the pinned field set is checking nothing"
                ),
            ));
            continue;
        };
        let pinned: BTreeSet<&str> = fields.iter().copied().collect();
        let names = field_names(declared);
        let found: BTreeSet<&str> = names.iter().map(String::as_str).collect();

        for added in found.difference(&pinned) {
            violations.push(Violation::new(
                RULE,
                KERNEL,
                format!(
                    "`RecordRef::{variant}` declares `{added}`, which is not in \
                     TIMER_RECORD_FIELDS; a field added here is paid on every timer, in \
                     flash and in write amplification, for the life of the format"
                ),
            ));
        }
        for removed in pinned.difference(&found) {
            violations.push(Violation::new(
                RULE,
                KERNEL,
                format!(
                    "`RecordRef::{variant}` no longer declares `{removed}`, which \
                     TIMER_RECORD_FIELDS pins; \u{a7}11 needs the clock kind so recovery \
                     cannot reinterpret one policy as another, and the arming reading \
                     because the floor it is measured against does not survive a power cut"
                ),
            ));
        }
    }

    violations
}

/// The façade's own module, whose public surface `ctx-facade` pins.
pub const CTX_FACADE_PATH: &str = "waymaker-embassy/src/ctx.rs";

/// The one façade module a codec may be named in.
pub const CODEC_PATH: &str = "waymaker-embassy/src/decode.rs";

/// The façade crate, whose codec `codec-is-optional` holds to being optional.
pub const CODEC_CRATE: &str = "waymaker-embassy";

/// How a codec is spelled, so that no other façade module can name one.
///
/// Design document §02 decision 4: records are numeric kinds and borrowed bytes, and Serde
/// and Postcard are conveniences "never wire-format requirements". The way that is given
/// back is not a feature — it is a bound: a `Ctx::activity` that asked for
/// `DeserializeOwned`, or a `Handoff` that named a codec type, makes every workflow carry
/// the codec whatever the manifest says.
pub const CODEC_VOCABULARY: &[&str] = &[
    "Coded",
    "Deserialize",
    "DeserializeOwned",
    "Format",
    "FromPostcard",
    "Postcard",
    "Serialize",
    "postcard",
    "serde",
];

/// The trait every recorded answer goes through, which names no codec.
///
/// Declared outside every `cfg`, so a default build still has it.
pub const CODEC_FREE_TRAIT: &str = "pub trait Decode";

/// The codec features, and what each must enable.
///
/// `postcard` enables `serde` rather than `dep:serde` on its own: the bridge is what the
/// format plugs into, so a `postcard` that skipped it would be a format with nothing to be
/// a format for.
pub const CODEC_FEATURES: &[(&str, &[&str])] = &[
    ("serde", &["dep:serde"]),
    ("postcard", &["dep:postcard", "serde"]),
];

/// The dependencies a codec feature is the only thing that enables.
pub const CODEC_DEPENDENCIES: &[&str] = &["postcard", "serde"];

/// Rule: a codec stays optional, and stays out of the boundary.
///
/// Design document §02 decision 4 makes a codec a convenience. Three ways of taking that
/// back break no other rule and need no new dependency:
///
/// * a codec named in another façade module — a `Ctx::activity` bounded on
///   `DeserializeOwned` makes every workflow carry the codec whatever the manifest says;
/// * a codec item in [`CODEC_PATH`] that no feature gates, which links the codec into the
///   default build and stops [`CODEC_FREE_TRAIT`] being free of one;
/// * a codec dependency that is not `optional`, which links it whatever feature is
///   selected and makes the size report's per-feature row measure nothing.
///
/// # What it cannot see
///
/// A codec named from a *sibling* crate, and a bound written without one of
/// [`CODEC_VOCABULARY`]'s words — a type alias for `DeserializeOwned` declared in
/// `decode.rs` and used in `ctx.rs` names nothing forbidden. It pins one crate and one
/// module of it, the way `capacity-reserve`, `recovery-surface` and `storage-contract`
/// each say of the one file they pin. `crates/waymaker-embassy/tests/codec.rs` holds the
/// behaviour, and the `codec-test` stage runs it.
#[must_use]
pub fn check_codec_is_optional(
    sources: &[crate::size::LayerSource],
    manifests: &[(String, String)],
) -> Vec<Violation> {
    const RULE: &str = "codec-is-optional";

    let mut violations = Vec::new();

    for source in sources
        .iter()
        .filter(|source| source.crate_name == CODEC_CRATE)
    {
        let path = source.path.replace('\\', "/");
        if path.ends_with(CODEC_PATH) {
            continue;
        }
        let code = without_test_modules(&code_only(&source.contents));
        for word in CODEC_VOCABULARY {
            if names_identifier(&code, word) {
                violations.push(Violation::new(
                    RULE,
                    CODEC_CRATE,
                    format!(
                        "{path} names `{word}`; only `{CODEC_PATH}` may name a codec, because \
                         a codec named on the boundary is one every workflow carries whatever \
                         the manifest says"
                    ),
                ));
            }
        }
    }

    match sources
        .iter()
        .find(|source| source.path.replace('\\', "/").ends_with(CODEC_PATH))
    {
        Some(source) => violations.extend(check_codec_module(RULE, &source.contents)),
        None => violations.push(Violation::new(
            RULE,
            CODEC_CRATE,
            format!("{CODEC_PATH} is not where the gate looks for it, so the pin checks nothing"),
        )),
    }

    violations.extend(check_codec_manifest(
        RULE,
        manifests
            .iter()
            .find(|(name, _)| name == CODEC_CRATE)
            .map(|(_, contents)| contents.as_str()),
    ));
    violations
}

/// Every codec item of the codec module is behind a feature, and the trait is not.
fn check_codec_module(rule: &'static str, contents: &str) -> Vec<Violation> {
    let code = without_test_modules(&code_only(contents));
    let mut violations = Vec::new();
    let mut seen_trait = false;

    for item in items(&code) {
        if item.declares_the_codec_free_trait() {
            seen_trait = true;
            // `conditional`, not `gated`. A compound `#[cfg(all(feature = "serde"))]` is not
            // read as gating, so an item behind one is *reported* — but the trait branch
            // only reported when the item was classified as gated, so the same spelling on
            // the trait was silent. Review of this change ran it: `Decode` left the default
            // build with the gate green. The two halves now fail in the same direction.
            if item.conditional {
                violations.push(Violation::new(
                    rule,
                    CODEC_CRATE,
                    format!(
                        "{CODEC_PATH} declares `{CODEC_FREE_TRAIT}` behind a `#[cfg]`; it is \
                         the trait every recorded answer goes through, so a default build \
                         has to have it"
                    ),
                ));
            }
            continue;
        }
        if item.gated {
            continue;
        }
        // The whole item, not its first line. A declaration can put the codec in its body
        // — `pub struct Bridge {` followed by `inner: Postcard,` — and review of this
        // change wrote exactly that. The compiler refuses it in a default build, because
        // what it names is behind the same feature, but a rule that leans on the compiler
        // for its own claim is a rule that stops holding the day the codec becomes
        // reachable another way.
        if let Some(word) = CODEC_VOCABULARY
            .iter()
            .find(|word| names_identifier(&item.text, word))
        {
            violations.push(Violation::new(
                rule,
                CODEC_CRATE,
                format!(
                    "{CODEC_PATH} declares `{}`, which names `{word}` and no \
                     `#[cfg(feature = ..)]` gates: a codec in the default build is a codec \
                     every firmware pays for",
                    item.headline()
                ),
            ));
        }
    }

    if !seen_trait {
        violations.push(Violation::new(
            rule,
            CODEC_CRATE,
            format!("{CODEC_PATH} declares no `{CODEC_FREE_TRAIT}`, so the pin checks nothing"),
        ));
    }
    violations
}

/// One top-level item of a module, with the attributes above it.
struct Item {
    /// Its attributes, declaration and body, comments already removed.
    text: String,
    /// Whether a bare `#[cfg(feature = "..")]` stands above it.
    gated: bool,
    /// Whether *any* `#[cfg]` naming a feature stands above it, compound ones included.
    ///
    /// [`Self::gated`] is the narrow reading and this is the wide one. An item needs the
    /// narrow one to be excused; the trait needs the wide one to be reported.
    conditional: bool,
}

impl Item {
    /// The first line, for a violation message.
    fn headline(&self) -> &str {
        self.text
            .lines()
            .find(|line| !line.starts_with('#'))
            .unwrap_or_default()
            .trim_end()
    }

    /// Whether this item is the pinned trait, as a declaration and not as a prefix.
    ///
    /// `starts_with` alone reads `pub trait Decoder` as the pinned trait, so a rename would
    /// satisfy the "declares no `pub trait Decode`" branch with the trait gone. Review of
    /// this change ran it.
    fn declares_the_codec_free_trait(&self) -> bool {
        self.text
            .lines()
            .find(|line| !line.starts_with('#'))
            .and_then(|line| line.strip_prefix(CODEC_FREE_TRAIT))
            .is_some_and(|rest| {
                rest.chars()
                    .next()
                    .is_none_or(|character| !character.is_alphanumeric() && character != '_')
            })
    }
}

/// The top-level items of `code`, each with the attributes above it applied.
///
/// An item begins at column zero and runs to the next one, so a declaration that spans
/// lines is one item rather than a first line and some members. Attributes stand above the
/// item they apply to and are not items themselves.
///
/// # What counts as gated
///
/// A bare `#[cfg(feature = "<name>")]`, and nothing else. An `all(`, an `any(` or a `not(`
/// is not read as gating, so an item behind one is reported. That is the conservative
/// direction — `any(feature = "postcard", unix)` really is present in a default build on a
/// host — and it costs a contributor a plain `#[cfg]` or a conversation in review, which is
/// where a codec in the default build belongs.
///
/// Which feature it names is *not* read, because [`code_only`] removes string literals
/// along with comments and this attribute's argument is one. That loses nothing the rule
/// claims: any single positive feature gate keeps the item out of a default build, which is
/// what "a codec in the default build is a codec every firmware pays for" is about. Gating
/// a codec on the *wrong* feature is a compile error the moment that feature is enabled,
/// because what the item names is behind the codec's.
fn items(code: &str) -> Vec<Item> {
    /// `#[cfg(feature = "x")]` with its literal and its spaces gone, which is the shape
    /// [`code_only`] leaves and the one this compares against.
    const GATE: &str = "#[cfg(feature=)]";

    let mut items: Vec<Item> = Vec::new();
    let mut gated = false;
    let mut conditional = false;
    // Attributes are part of the item they stand above, so that a codec named in one —
    // `#[cfg_attr(feature = "serde", derive(serde::Deserialize))]` — is a codec the scan
    // reads. A scan that skipped them read the derive as no codec at all.
    let mut attributes: Vec<String> = Vec::new();
    for line in code.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
            if compact == GATE {
                gated = true;
            }
            if compact.starts_with("#[cfg(") && compact.contains("feature") {
                conditional = true;
            }
            attributes.push(trimmed.to_owned());
            continue;
        }
        if line.starts_with(char::is_whitespace) {
            if let Some(item) = items.last_mut() {
                item.text.push('\n');
                item.text.push_str(trimmed);
            }
            continue;
        }
        let mut text = attributes.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(trimmed);
        items.push(Item {
            text,
            gated,
            conditional,
        });
        attributes.clear();
        gated = false;
        conditional = false;
    }
    items
}

/// The codec dependencies are optional, and the codec features enable what they must.
fn check_codec_manifest(rule: &'static str, manifest: Option<&str>) -> Vec<Violation> {
    let Some(parsed) = manifest.and_then(|manifest| manifest.parse::<toml::Table>().ok()) else {
        return vec![Violation::new(
            rule,
            CODEC_CRATE,
            "its manifest could not be read, so the rules about what is optional did not run",
        )];
    };

    let mut violations = Vec::new();
    let dependencies = parsed.get("dependencies").and_then(toml::Value::as_table);
    for name in CODEC_DEPENDENCIES {
        let optional = dependencies
            .and_then(|table| table.get(*name))
            .and_then(toml::Value::as_table)
            .and_then(|entry| entry.get("optional"))
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        if !optional {
            violations.push(Violation::new(
                rule,
                CODEC_CRATE,
                format!(
                    "declares `{name}` without `optional = true`, so every build links it and \
                     the size report's row for it measures nothing"
                ),
            ));
        }
    }

    let features = parsed.get("features").and_then(toml::Value::as_table);
    for (feature, required) in CODEC_FEATURES {
        let enabled: Vec<&str> = features
            .and_then(|table| table.get(*feature))
            .and_then(toml::Value::as_array)
            .map(|entries| entries.iter().filter_map(toml::Value::as_str).collect())
            .unwrap_or_default();
        for wanted in *required {
            if !enabled.contains(wanted) {
                violations.push(Violation::new(
                    rule,
                    CODEC_CRATE,
                    format!(
                        "its `{feature}` feature does not enable `{wanted}`, so the row that \
                         measures it links less than the feature is supposed to add"
                    ),
                ));
            }
        }
    }
    violations
}

/// The durable half the façade asks, whose public surface `ctx-facade` pins.
pub const CTX_JOURNAL_PATH: &str = "waymaker-embassy/src/journal.rs";

/// Every public function issue #35's `Ctx` declares.
///
/// Four futures, a constructor, and three accessors a caller reads after a boot. A ninth
/// name is a way for the façade to do something, and the whole of issue #35 is that it may
/// only ask.
pub const CTX_SURFACE: &[&str] = &[
    "activity",
    "complete",
    "conclusion",
    "continue_as_new",
    "fail",
    "new",
    "payload",
    "timer",
];

/// The four futures `Ctx` hands out.
///
/// Pinned so that a fifth is a reviewer's decision. It is also what makes the `poll`
/// exemption below safe: the surface pin cannot speak about a name declared four times, so
/// the count is held here instead.
pub const CTX_FUTURES: &[&str] = &[
    "ActivityFuture",
    "ContinueFuture",
    "TerminalFuture",
    "TimerFuture",
];

/// The type `ctx-facade` pins the methods of, at every visibility.
pub const CTX_TYPE: &str = "Ctx";

/// The methods `Ctx` declares that are not part of its surface.
///
/// One: the shared body behind `complete` and `fail`. Pinned rather than allowed, so a
/// second private method is a line a reviewer writes.
pub const CTX_PRIVATE_METHODS: &[&str] = &["ending"];

/// The one name the façade's surface pin does not compare.
///
/// `Future::poll` is `core`'s name and every future declares it exactly once, so a pin that
/// is a list of names cannot say anything about four of them. [`CTX_FUTURES`] holds the
/// count instead.
const FUTURE_POLL: &str = "poll";

/// Every method the durable half declares.
///
/// Four, one per thing a workflow can ask for. A fifth is a question the façade would be
/// answering for itself.
pub const CTX_JOURNAL_SURFACE: &[&str] = &["continue_as_new", "resolve", "schedule", "wait"];

/// What the façade may not name, and why.
///
/// Each is a piece of authority the façade must not hold. `waymaker-embassy`'s must-not-own
/// cell is "on-media authority or hidden global state", and every one of these is the first.
pub const CTX_FORBIDDEN_VOCABULARY: &[(&str, &str)] = &[
    (
        "StableStorage",
        "is the device, and a façade that reached one would write records for itself",
    ),
    (
        "Reserved",
        "is §10's gated writer, and admitting a record is the journal's decision",
    ),
    (
        "RecordRef",
        "is a record, and a façade that named one would decide what history holds",
    ),
    (
        "Recovery",
        "is the recovery scan, which decides what committed history is",
    ),
    (
        "ReplayMachine",
        "is §08's transition table, which decides what may follow what",
    ),
    (
        "BankLayout",
        "is §10's two-bank layout, which decides which bank is authoritative",
    ),
    (
        "Swap",
        "is §10's bank swap, which is the journal's to perform",
    ),
];

/// The three driver files that may name the façade.
///
/// Issue #35's second "done when" is that removing the Embassy crate leaves the protocol
/// fully usable through the synchronous driver. Every *other* module of `waymaker-drive` is
/// held to naming no façade, and the list is the exemptions rather than the modules held —
/// so a module added tomorrow is covered without anyone remembering to add a row.
///
/// `lib.rs` is here because a crate root declares its modules and re-exports a name from
/// them. Its own honesty is the `drive-facadeless` build's rather than this scan's.
pub const FACADE_DRIVER_MODULES: &[&str] = &[
    "waymaker-drive/src/facade.rs",
    "waymaker-drive/src/lib.rs",
    "waymaker-drive/src/ota.rs",
];

/// What a driver module outside [`FACADE_DRIVER_MODULES`] may not name, and why.
///
/// The crate itself, and the three source-level routes to it that name no crate: the two
/// modules that hold the edge, and the type they re-export. Review of this change reached
/// the façade with `use crate::facade::Bridge;` while a ban on the crate name alone stayed
/// green.
pub const FACADE_FREE_VOCABULARY: &[(&str, &str)] = &[
    (
        "waymaker_embassy",
        "is the façade crate, so a module that names it cannot compile with the façade \
         removed",
    ),
    (
        "facade",
        "is the module that holds the façade edge; reaching it is reaching the façade",
    ),
    (
        "ota",
        "is design document §06's example over the façade; reaching it is reaching the \
         façade",
    ),
    (
        "Bridge",
        "is the façade's journal over this boundary, re-exported from the crate root",
    ),
];

/// Rule: the façade adds sugar and never authority.
///
/// Issue [#35](https://github.com/madmax983/waymaker/issues/35) says it in one line — "it
/// must add sugar, never authority" — and design document §05 says it as
/// `waymaker-embassy`'s must-not-own cell: on-media authority or hidden global state. Both
/// are absences, and an absence is what a test cannot check: a `Ctx::record` that appended
/// a record for itself would break no layering rule, need no new dependency, and pass every
/// test in the workspace, because the run would still complete.
///
/// So this pins three things.
///
/// The **surface**, in both directions. A tenth public function on `Ctx` or a fifth method
/// on the journal is a reviewer's decision rather than a commit.
///
/// The **vocabulary**. `waymaker-embassy` may depend on `waymaker-flash`, so nothing else
/// stops the façade reaching a `StableStorage` or a `Reserved` and writing through it.
/// [`CTX_FORBIDDEN_VOCABULARY`] names each piece of authority and why it is not the
/// façade's.
///
/// And **no hidden global state**: a `static` in either module is the other half of the
/// must-not-own cell, and a façade with one is a façade two runs on a device would share.
///
/// The fourth half is the driver's. Every `waymaker-drive` module but the three in
/// [`FACADE_DRIVER_MODULES`] is held to naming none of [`FACADE_FREE_VOCABULARY`], so a
/// module added tomorrow is covered without anyone remembering a row. That is the fast half
/// of "removing the Embassy crate leaves the protocol fully usable"; the `drive-facadeless`
/// pipeline stage is the half a compiler decides.
///
/// # What it cannot see
///
/// A function added from a sibling module — it pins two files, exactly as
/// `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one they
/// pin. And it compares *names*: a `Ctx::payload` that started returning the journal's
/// buffer rather than the caller's is `crates/waymaker-embassy/tests/ctx.rs`'s.
#[must_use]
pub fn check_ctx_facade(
    sources: &[crate::size::LayerSource],
    driver: &[crate::size::LayerSource],
) -> Vec<Violation> {
    const RULE: &str = "ctx-facade";
    const FACADE: &str = "waymaker-embassy";
    const DRIVER: &str = "waymaker-drive";

    let mut violations = Vec::new();
    for (path, pinned) in [
        (CTX_FACADE_PATH, CTX_SURFACE),
        (CTX_JOURNAL_PATH, CTX_JOURNAL_SURFACE),
    ] {
        violations.extend(check_facade_surface(RULE, FACADE, path, pinned, sources));
        let Some(source) = find_source(sources, path) else {
            continue;
        };
        if path == CTX_FACADE_PATH {
            let code = without_test_modules(&code_only(&source.contents));
            violations.extend(check_facade_futures(RULE, FACADE, path, &code));
            violations.extend(check_facade_type_members(RULE, FACADE, path, &code));
        }
    }

    // The authority ban, the `static` ban and the macro ban are statements about the
    // *crate*, so they read every one of its files. Review of this change put a
    // `pub use waymaker_flash::storage::StableStorage as Device;`, a
    // `pub static ATTEMPTS: AtomicUsize` and a `macro_rules!` expanding a tenth public
    // method into `impl Ctx` in `dispatch.rs` — one file over from the two the surface pins
    // read — and watched the gate stay green on all three.
    for source in sources.iter().filter(|source| source.crate_name == FACADE) {
        let path = source.path.replace('\\', "/");
        let code = without_test_modules(&code_only(&source.contents));
        for (forbidden, why) in CTX_FORBIDDEN_VOCABULARY {
            if names_identifier(&code, forbidden) {
                violations.push(Violation::new(
                    RULE,
                    FACADE,
                    format!("{path} names `{forbidden}`, which {why}"),
                ));
            }
        }
        violations.extend(check_no_hidden_state(RULE, FACADE, &path, &code));
        // And the future set, for the same reason: a fifth future whose `impl Future` lives
        // one file over is a fifth thing a workflow can `.await` that the count in `ctx.rs`
        // cannot see. Review of this change declared one in `dispatch.rs`.
        for future in future_implementors(&code) {
            if !CTX_FUTURES.contains(&future.as_str()) {
                violations.push(Violation::new(
                    RULE,
                    FACADE,
                    format!(
                        "{path} implements `Future` for `{future}`, which `CTX_FUTURES` does \
                         not name: a fifth thing a workflow can `.await` is a reviewer's \
                         decision rather than a commit"
                    ),
                ));
            }
        }
        // A scanner cannot expand a macro, so it refuses the construct — the answer
        // `effect-protocol` gives to a closure and a short-circuit.
        if names_identifier(&code, "macro_rules") {
            violations.push(Violation::new(
                RULE,
                FACADE,
                format!(
                    "{path} declares a `macro_rules!`, which can expand a public method into \
                     a pinned `impl`, or a future's `poll`, where no pin can read it"
                ),
            ));
        }
    }

    violations.extend(check_facade_free_driver(RULE, DRIVER, driver));
    violations
}

/// The dispatcher trait's own module, whose public surface `dispatch-wiring` pins.
pub const DISPATCH_PATH: &str = "waymaker-embassy/src/dispatch.rs";

/// The dispatch table's module, whose public surface `dispatch-wiring` pins.
pub const WIRING_PATH: &str = "waymaker-embassy/src/wiring.rs";

/// Every public function the dispatcher trait declares.
///
/// One. Design document §13's dispatcher performs §07 step 4 and answers; a second method
/// is a second thing the façade would have to sequence.
pub const DISPATCH_SURFACE: &[&str] = &["poll_dispatch"];

/// Every public function the dispatch wiring declares.
///
/// Two constructors, two accessors on a row, two on the world, one name lookup, and the
/// trait method. Issue [#36](https://github.com/madmax983/waymaker/issues/36) names two
/// non-goals — no dynamic workflow loading, no string-addressed activity registry — and
/// both are reached by *adding* a name here: a `by_name`, a `register`, an `insert`, a
/// `load`. None breaks a layering rule, and a run through any of them still completes.
pub const WIRING_SURFACE: &[&str] = &[
    "kind",
    "name",
    "name_of",
    "new",
    "over",
    "poll_dispatch",
    "world",
    "world_mut",
];

/// Every function `waymaker-embassy/src/dispatch.rs` declares that is not on its surface.
///
/// None. The module is a trait and a two-shape answer.
pub const DISPATCH_PRIVATE_FUNCTIONS: &[&str] = &[];

/// Every function the wiring declares that is not on its surface.
///
/// One: the row lookup both `name_of` and `poll_dispatch` go through. Pinned rather than
/// allowed, so a second private function is a line a reviewer writes.
pub const WIRING_PRIVATE_FUNCTIONS: &[&str] = &["row"];

/// The fields each pinned struct declares.
///
/// A *name* pin as well as a visibility one, because the label ban below is one identifier
/// deep: renaming `Activity::name` to `label` — with `pub const fn name(&self) -> &'static
/// str { self.label }` left in place — keeps the surface and method pins intact and frees a
/// selection body to compare `row.label`, which names nothing forbidden. Review of this
/// change identified exactly that.
///
/// It is also what refuses a *tuple* struct. `braced_body` walks to the next `{` when a
/// declaration has no brace of its own, so a tuple struct with public fields reads back
/// either the `impl` block below it — noisy but safe — or, with a field-free braced item
/// between them, an empty body that passes. Comparing the field set catches both.
pub const WIRING_TYPE_FIELDS: &[(&str, &[&str])] = &[
    ("Activity", &["kind", "name", "perform"]),
    ("Table", &["rows", "world"]),
];

/// The wiring's types, and the methods each declares at *every* visibility.
///
/// A surface pin counts `pub ` and not `pub(`, which is the defeat `timer-capability`,
/// `effect-protocol` and `ctx-facade` each record. `waymaker-embassy` is one crate, so a
/// `pub(crate) fn by_name` here is reachable from `ctx.rs`.
pub const WIRING_TYPE_METHODS: &[(&str, &[&str])] = &[
    ("Activity", &["kind", "name", "new"]),
    ("Table", &["name_of", "over", "row", "world", "world_mut"]),
];

/// The bodies that select a row.
///
/// `poll_dispatch` is a *trait* method, so `inherent_impl_bodies` cannot see it and the
/// file is read instead — with the count checked first, because `braced_body` takes the
/// first match and a decoy above the real one is what a first-match scan reads.
pub const WIRING_SELECTION_BODIES: &[&str] = &["poll_dispatch", "row"];

/// What a selection body may not name, and why.
pub const WIRING_SELECTION_FORBIDDEN: &[(&str, &str)] = &[(
    "name",
    "is a row's compile-time label, and a dispatch path that read one would be the \
     string-addressed activity registry issue #36 names as an explicit non-goal",
)];

/// Rule: an activity is reached by its number, and its name is only ever metadata.
///
/// Issue [#36](https://github.com/madmax983/waymaker/issues/36) states two work items as
/// absences. "Numeric `ActivityKind` on the dispatch path. Activity names are compile-time
/// metadata for logs and diagnostics and are never stored in records", and "no dynamic
/// workflow loading and no string-addressed activity registry — that is an explicit
/// non-goal". Neither is a thing a test can fail on: a `Table::by_name` would break no
/// layering rule, need no dependency, and pass every test in the workspace, because every
/// run still completes.
///
/// So this pins three things.
///
/// The **surfaces** of both modules, in both directions. A second trait method, or a ninth
/// function on the wiring, is a reviewer's decision rather than a commit.
///
/// The **methods** of `Activity` and `Table`, at every visibility, and no public field on
/// either. A `pub rows` field is a table any caller can rebuild at run time, which is the
/// dynamic-loading non-goal reached without adding a function.
///
/// And the **selection bodies**: the two places a row is chosen may not name a row's
/// label. Without it, a lookup that fell back to a name would satisfy both pins above.
///
/// # What it cannot see
///
/// A function added from a sibling module — it pins two files, exactly as
/// `capacity-reserve`, `recovery-surface` and `storage-contract` each say of the one they
/// pin, and a `trait TableExt` with a blanket impl is the shape. And it compares *names*:
/// that a name never reaches media is `crates/waymaker-drive/tests/dispatch.rs`, which
/// reads the device image back, and §09's `EffectScheduled` is what makes it true.
#[must_use]
pub fn check_dispatch_wiring(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "dispatch-wiring";
    const FACADE: &str = "waymaker-embassy";

    let mut violations = Vec::new();
    for (path, pinned) in [
        (DISPATCH_PATH, DISPATCH_SURFACE),
        (WIRING_PATH, WIRING_SURFACE),
    ] {
        violations.extend(check_dispatch_surface(RULE, FACADE, path, pinned, sources));
    }

    for (path, surface, private) in [
        (DISPATCH_PATH, DISPATCH_SURFACE, DISPATCH_PRIVATE_FUNCTIONS),
        (WIRING_PATH, WIRING_SURFACE, WIRING_PRIVATE_FUNCTIONS),
    ] {
        let Some(source) = find_source(sources, path) else {
            continue;
        };
        let code = without_test_modules(&code_only(&source.contents));
        violations.extend(check_module_functions(
            RULE, FACADE, path, surface, private, &code,
        ));
    }

    let Some(source) = find_source(sources, WIRING_PATH) else {
        return violations;
    };
    let code = without_test_modules(&code_only(&source.contents));
    violations.extend(check_wiring_types(RULE, FACADE, &code));
    violations.extend(check_wiring_selection(RULE, FACADE, &code));
    violations
}

/// Every function a file declares, anywhere in it and at any visibility.
///
/// [`declared_function_names`] reads one body at depth zero and
/// [`crate::size::public_functions`] reads `pub`, so a free `pub(crate) fn by_name` at
/// *module* scope is in neither reader's field of view. Review of this change wrote one,
/// routed `poll_dispatch` through it, and watched the gate stay green — and the label ban
/// did not fire either, because `names_identifier` reads `by_name` as one identifier and
/// the `_` before `name` is a token character. `waymaker-embassy` is one crate, so such a
/// function is reachable from `ctx.rs`.
///
/// `fn ` with the space is what makes a `fn(..)` pointer type — `Perform`'s own definition —
/// not a declaration.
fn declared_functions_anywhere(code: &str) -> Vec<String> {
    let continues = |character: char| character.is_alphanumeric() || character == '_';
    let mut names = Vec::new();
    for (index, _) in code.match_indices("fn ") {
        let preceded = code
            .get(..index)
            .and_then(|before| before.chars().next_back())
            .is_some_and(continues);
        if preceded {
            continue;
        }
        let Some(rest) = code.get(index.wrapping_add("fn ".len())..) else {
            continue;
        };
        let name: String = rest.chars().take_while(|c| continues(*c)).collect();
        if !name.is_empty() {
            names.push(name);
        }
    }
    names.sort();
    names
}

/// One module declares exactly the functions its two pins list, at every visibility.
fn check_module_functions(
    rule: &'static str,
    subject: &str,
    path: &str,
    surface: &[&str],
    private: &[&str],
    code: &str,
) -> Vec<Violation> {
    let declared = declared_functions_anywhere(code);
    let mut expected: Vec<String> = surface
        .iter()
        .chain(private)
        .map(|name| (*name).to_owned())
        .collect();
    expected.sort();
    if declared == expected {
        return Vec::new();
    }
    vec![Violation::new(
        rule,
        subject.to_owned(),
        format!(
            "{path} declares {declared:?} rather than {expected:?}: read anywhere in the \
             file and at every visibility, because a free `pub(crate) fn by_name` at module \
             scope is on neither a surface pin nor a method pin, and this crate is the one \
             that would call it"
        ),
    )]
}

/// One module declares exactly the public functions its pin lists.
fn check_dispatch_surface(
    rule: &'static str,
    subject: &str,
    path: &str,
    pinned: &[&str],
    sources: &[crate::size::LayerSource],
) -> Vec<Violation> {
    let Some(source) = find_source(sources, path) else {
        return vec![Violation::new(
            rule,
            subject.to_owned(),
            format!(
                "no {path} in the workspace, so the pinned surface is checking nothing; \
                 issue #36's dispatch path is a number and never a name"
            ),
        )];
    };

    let mut declarations: Vec<String> =
        crate::size::public_functions(core::slice::from_ref(source))
            .into_iter()
            .map(|function| function.name)
            .collect();
    declarations.sort_unstable();

    let mut violations = Vec::new();
    for (index, name) in declarations.iter().enumerate() {
        if declarations.get(index.wrapping_add(1)) == Some(name) {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "{path} declares `{name}` more than once, so the pin can no longer speak \
                     about it; give the second one a name of its own"
                ),
            ));
        }
    }
    for name in &declarations {
        if !pinned.contains(&name.as_str()) {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "{path} declares `{name}`, which the pinned surface does not list: a way \
                     to reach an activity that is not its number is issue #36's explicit \
                     non-goal"
                ),
            ));
        }
    }
    for name in pinned {
        if !declarations.iter().any(|declared| declared == name) {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!("{path} no longer declares `{name}`, which the pinned surface lists"),
            ));
        }
    }
    violations
}

/// `Activity` and `Table` declare their pinned methods, at every visibility, and no public
/// field.
fn check_wiring_types(rule: &'static str, subject: &str, code: &str) -> Vec<Violation> {
    let mut violations = Vec::new();

    // One flat module, for `effect-protocol`'s reason: `inherent_impl_bodies` reads `impl`
    // at column zero, so an `impl` in a nested module of this same file is indented and
    // invisible to it. Review of this change put a `mod shim { pub struct Table {} }` above
    // the real one and made both of `Table`'s fields public, and the gate stayed green.
    if count_tokens(code, "mod") != 0 {
        violations.push(Violation::new(
            rule,
            subject.to_owned(),
            format!(
                "{WIRING_PATH} declares a module: the wiring is one flat module, because \
                 the method pin below reads `impl` at column zero and an `impl` inside a \
                 submodule escapes it"
            ),
        ));
    }

    for (type_name, methods) in WIRING_TYPE_METHODS {
        // Declared twice fails too. `braced_body` reads the first declaration, so a decoy
        // above the real one is the body the public-field scan reads — which is how the
        // `mod shim` above got two public fields past a version without this.
        let declarations = count_declarations(code, &format!("struct {type_name}"));
        if declarations != 1 {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "{WIRING_PATH} declares `struct {type_name}` {declarations} times rather \
                     than once, so the field pin reads whichever comes first"
                ),
            ));
            continue;
        }
        let blocks = inherent_impl_bodies(code, type_name);
        if blocks.is_empty() {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "{WIRING_PATH} declares no inherent `impl` for `{type_name}`, so its \
                     methods are pinned against nothing"
                ),
            ));
            continue;
        }
        let declared = declared_function_names(&blocks.join("\n"));
        let mut expected: Vec<String> = methods.iter().map(|name| (*name).to_owned()).collect();
        expected.sort();
        if declared != expected {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "`{type_name}` declares {declared:?} rather than {expected:?}: read at \
                     every visibility, because a surface pin counts `pub ` and not `pub(`, \
                     and the crate that would call a `pub(crate)` lookup is this one"
                ),
            ));
        }
        let Some(body) = braced_body(code, &format!("struct {type_name}")) else {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!("{WIRING_PATH} declares no braced `struct {type_name}`"),
            ));
            continue;
        };
        for field in body
            .lines()
            .map(str::trim)
            .filter(|line| crate::size::without_leading_attributes(line).starts_with("pub"))
        {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "`{type_name}` declares the public field `{field}`: a caller that can \
                     write the rows can build a table at run time, which is issue #36's \
                     dynamic-loading non-goal reached without adding a function"
                ),
            ));
        }
        let Some(pinned) = WIRING_TYPE_FIELDS
            .iter()
            .find(|(named, _)| named == type_name)
        else {
            continue;
        };
        let mut declared = field_names(body);
        declared.sort();
        let mut expected: Vec<String> = pinned.1.iter().map(|name| (*name).to_owned()).collect();
        expected.sort();
        if declared != expected {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "`{type_name}` declares the fields {declared:?} rather than {expected:?}: \
                     the label ban is one identifier deep, so a `name` renamed to `label` \
                     frees a selection body to compare it and names nothing forbidden — and a \
                     tuple struct has no braced body of its own for the public-field scan \
                     above to read"
                ),
            ));
        }
    }
    violations
}

/// A row is chosen by its number, and never by its label.
fn check_wiring_selection(rule: &'static str, subject: &str, code: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    for body_name in WIRING_SELECTION_BODIES {
        let header = format!("fn {body_name}");
        if count_declarations(code, &header) != 1 {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "{WIRING_PATH} declares `fn {body_name}` other than exactly once, so the \
                     selection pin reads whichever comes first"
                ),
            ));
            continue;
        }
        let Some(body) = braced_body(code, &header) else {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!("{WIRING_PATH} declares no `fn {body_name}` body to pin"),
            ));
            continue;
        };
        for (forbidden, why) in WIRING_SELECTION_FORBIDDEN {
            if names_identifier(body, forbidden) {
                violations.push(Violation::new(
                    rule,
                    subject.to_owned(),
                    format!("`{body_name}` names `{forbidden}`, which {why}"),
                ));
            }
        }
    }
    violations
}

/// How many times `header` is declared at a token boundary.
fn count_declarations(code: &str, header: &str) -> usize {
    let continues = |character: char| character.is_alphanumeric() || character == '_';
    code.match_indices(header)
        .filter(|(index, _)| {
            let before = code
                .get(..*index)
                .and_then(|before| before.chars().next_back())
                .is_none_or(|character| !continues(character));
            let after = code
                .get(index.wrapping_add(header.len())..)
                .and_then(|rest| rest.chars().next())
                .is_none_or(|character| !continues(character));
            before && after
        })
        .count()
}

/// Every `waymaker-drive` module but the three that hold the façade edge names no façade.
///
/// Discovered from the sources rather than listed, so a module added tomorrow is covered.
/// A scanner is not the whole of this claim and does not have to be: the `drive-facadeless`
/// pipeline stage builds the crate with `without-facade`, which a `use crate::facade::Bridge`
/// and a dependency renamed in a manifest both fail. This is the fast, local half.
fn check_facade_free_driver(
    rule: &'static str,
    subject: &str,
    driver: &[crate::size::LayerSource],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut seen = 0_usize;
    for source in driver {
        let path = source.path.replace('\\', "/");
        if FACADE_DRIVER_MODULES
            .iter()
            .any(|exempt| path.ends_with(exempt))
        {
            continue;
        }
        seen = seen.saturating_add(1);
        let code = code_only(&source.contents);
        for (forbidden, why) in FACADE_FREE_VOCABULARY {
            if names_identifier(&code, forbidden) {
                violations.push(Violation::new(
                    rule,
                    subject.to_owned(),
                    format!(
                        "{path} names `{forbidden}`, which {why}; the edge belongs in \
                         waymaker-drive/src/facade.rs and waymaker-drive/src/ota.rs"
                    ),
                ));
            }
        }
    }
    if seen == 0 {
        violations.push(Violation::new(
            rule,
            subject.to_owned(),
            "no waymaker-drive module outside the façade edge is in the workspace, so \
             nothing says the synchronous driver still compiles with the façade removed"
                .to_owned(),
        ));
    }
    violations
}

/// `Ctx` declares its methods at every visibility, and no associated constant.
///
/// The two defeats CLAUDE.md already records against `timer-capability` and
/// `effect-protocol`, met here as well: a surface pin counts `pub ` and not `pub(`, and
/// reads `fn` declarations and so cannot see a `const`. `waymaker-embassy` is one crate, so
/// a `pub(crate) fn commit_raw` on `Ctx` is reachable from `clock.rs` — which is where the
/// authority would go.
fn check_facade_type_members(
    rule: &'static str,
    subject: &str,
    path: &str,
    code: &str,
) -> Vec<Violation> {
    let blocks = inherent_impl_bodies(code, CTX_TYPE);
    if blocks.is_empty() {
        return vec![Violation::new(
            rule,
            subject.to_owned(),
            format!(
                "{path} declares no inherent `impl` for `{CTX_TYPE}`, so its methods are \
                 pinned against nothing"
            ),
        )];
    }
    let joined = blocks.join("\n");
    let mut violations = Vec::new();
    let declared = declared_function_names(&joined);
    let mut expected: Vec<String> = CTX_SURFACE
        .iter()
        .chain(CTX_PRIVATE_METHODS)
        .map(|name| (*name).to_owned())
        .collect();
    expected.sort();
    if declared != expected {
        violations.push(Violation::new(
            rule,
            subject.to_owned(),
            format!(
                "`{CTX_TYPE}` declares {declared:?} rather than {expected:?}: read at every \
                 visibility, because a surface pin counts `pub ` and not `pub(`, and the \
                 crate that would call a `pub(crate)` escape hatch is this one"
            ),
        ));
    }
    violations.extend(
        declared_associated_constants(&joined)
            .into_iter()
            .map(|constant| {
                Violation::new(
                    rule,
                    subject.to_owned(),
                    format!(
                        "`{CTX_TYPE}` declares the associated constant `{constant}`: a surface \
                     pin reads `fn` declarations, so a constant is a value every caller \
                     reaches without changing a surface"
                    ),
                )
            }),
    );
    violations
}

/// The pinned surface, with [`FUTURE_POLL`] set aside.
///
/// [`check_pinned_surface`] refuses a name declared twice, and four futures declare `poll`.
/// Everything else is compared the same way and in both directions.
fn check_facade_surface(
    rule: &'static str,
    subject: &str,
    path: &str,
    pinned: &[&str],
    sources: &[crate::size::LayerSource],
) -> Vec<Violation> {
    let Some(source) = find_source(sources, path) else {
        return vec![Violation::new(
            rule,
            subject.to_owned(),
            format!(
                "no {path} in the workspace, so the pinned surface is checking nothing; \
                 issue #35's façade must add sugar and never authority"
            ),
        )];
    };

    let mut declarations: Vec<String> =
        crate::size::public_functions(core::slice::from_ref(source))
            .into_iter()
            .map(|function| function.name)
            .filter(|name| name != FUTURE_POLL)
            .collect();
    declarations.sort_unstable();

    let mut violations = Vec::new();
    for (index, name) in declarations.iter().enumerate() {
        if declarations.get(index.wrapping_add(1)) == Some(name) {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "{path} declares `{name}` more than once, so the pin can no longer speak \
                     about it; give the second one a name of its own"
                ),
            ));
        }
    }
    for name in &declarations {
        if !pinned.contains(&name.as_str()) {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "{path} declares `{name}`, which the pinned surface does not list: the \
                     façade must add sugar and never authority, so a way for it to do \
                     something rather than ask cannot be added without a reviewer writing it \
                     down"
                ),
            ));
        }
    }
    for name in pinned {
        if !declarations.iter().any(|declared| declared == name) {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!("{path} no longer declares `{name}`, which the pinned surface lists"),
            ));
        }
    }
    violations
}

/// Exactly the futures [`CTX_FUTURES`] names, each with one `poll`.
///
/// The count is what the surface pin gave up when it set `poll` aside. A fifth future is a
/// fifth thing a workflow can `.await`, which is a reviewer's decision rather than a commit.
fn check_facade_futures(
    rule: &'static str,
    subject: &str,
    path: &str,
    code: &str,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for future in CTX_FUTURES {
        let header = format!("pub struct {future}");
        let declarations = declaration_count(code, &header);
        if declarations != 1 {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!("{path} declares `{header}` {declarations} times, not once"),
            ));
        }
    }
    let polls = code
        .lines()
        .filter(|line| line.trim_start().starts_with("fn poll("))
        .count();
    if polls != CTX_FUTURES.len() {
        violations.push(Violation::new(
            rule,
            subject.to_owned(),
            format!(
                "{path} declares {polls} `fn poll` bodies and {} futures; the surface pin \
                 sets `poll` aside, so a future the pin cannot see is one nobody weighed",
                CTX_FUTURES.len()
            ),
        ));
    }
    violations
}

/// Every type the file implements `Future` for.
///
/// A line scan, like every other rule here: the header may be spelled `impl Future for X`,
/// `impl core::future::Future for X` or with generics in between, and what is wanted is the
/// self type. Generics on the type itself are dropped, so `ActivityFuture<'_, T, D, J>` is
/// `ActivityFuture`.
fn future_implementors(code: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in code.lines() {
        // Leading attributes first, for the reason every other classifier here sets them
        // aside: `#[rustfmt::skip] impl Future for SignalFuture { .. }` is one line that
        // `cargo fmt` preserves, and this was the third reader still reading past it. The
        // `fn poll` count is `ctx.rs`'s alone, so a fifth future declared anywhere else in
        // the crate rested on this scan. Codex round 4.
        let trimmed = crate::size::without_leading_attributes(line.trim());
        if !trimmed.starts_with("impl") {
            continue;
        }
        let Some((before, after)) = trimmed.split_once(" for ") else {
            continue;
        };
        if !names_identifier(before, "Future") {
            continue;
        }
        let name: String = after
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .collect();
        if !name.is_empty() {
            found.push(name);
        }
    }
    found
}

/// A module that declares no `static`.
///
/// `waymaker-embassy`'s must-not-own cell names hidden global state, and a `static` is what
/// that means in a `no_std` crate with no allocator: one device, two runs, one buffer.
/// `const` is not global state — it has no address a caller can observe — so only `static`
/// is refused.
/// The `'static` lifetime, which [`check_no_hidden_state`] is not about.
const LIFETIME: &str = "'static";

fn check_no_hidden_state(
    rule: &'static str,
    subject: &str,
    path: &str,
    code: &str,
) -> Vec<Violation> {
    code.lines()
        // The `static` keyword as a *token*, anywhere on the line. Reading the start of the
        // line after one `pub` prefix let `#[allow(dead_code)] static SHARED: [u8; 8] = ..`
        // through, and it survives `cargo fmt`. Review of this change landed exactly that.
        //
        // The `'static` *lifetime* is set aside first, because this rule is about a `static`
        // item and a lifetime is not one. `&'static str` is what compile-time metadata is
        // spelled as — issue #36's activity names — and refusing it would push a name into a
        // borrow the table cannot outlive, for no gain. An item is `static NAME:`, never
        // `'static`, so dropping the apostrophe form loses nothing the rule was written for.
        .map(|line| (line, line.replace(LIFETIME, "")))
        .filter(|(_, without_lifetimes)| names_identifier(without_lifetimes, "static"))
        .map(|(line, _)| line)
        .map(|line| {
            Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "{path} declares `{}`, which is hidden global state; the façade's \
                     must-not-own cell names it, and a buffer two runs share is the failure \
                     it names",
                    line.trim()
                ),
            )
        })
        .collect()
}

/// Rule: design document §07's seven steps happen in §07's order, and step 4 cannot be
/// reached without step 3.
///
/// Four halves, one id, because it is one decision. The surface is a set comparison; the
/// types are a shape — declared once, braced, no public field, and exactly the methods the
/// pin lists, at every visibility; the constructions are where a durable intent may come
/// from; and the step bodies are where §07's barriers are.
///
/// Read with `#[cfg(test)]` modules removed, for `integrity-check`'s reason: a construction
/// under `cfg(test)` discharges nothing about the code that ships.
///
/// What it cannot see is a protocol step added from another file — it pins one file, exactly
/// as `capacity-reserve`, `recovery-surface` and `storage-contract` each do — a method a
/// macro expands to, and whether the barriers are real, which is §12's contract and
/// `waymaker-conformance`'s across-reset witness. The crash windows are
/// `crates/waymaker-drive/tests/crash.rs`.
#[must_use]
pub fn check_effect_protocol(driver: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "effect-protocol";
    const DRIVER: &str = "waymaker-drive";

    let mut violations = check_pinned_surface(
        RULE,
        DRIVER,
        EFFECT_PROTOCOL_PATH,
        EFFECT_PROTOCOL_SURFACE,
        driver,
        "design document \u{a7}07 says a physical effect never precedes its committed intent, \
         and the protocol's public API is where that stops being a sentence: a way to name an \
         effect identity, or to observe an outcome, without the barrier that earns it cannot \
         be added by accident",
    );

    let Some(source) = find_source(driver, EFFECT_PROTOCOL_PATH) else {
        // `check_pinned_surface` has already reported the missing file.
        return violations;
    };
    let code = without_test_modules(&code_only(&source.contents));
    violations.extend(check_effect_types(&code));
    violations.extend(check_effect_constructions(&code));
    violations.extend(check_effect_steps(&code));
    violations
}

/// The three types §07's protocol is made of, over one file's text.
fn check_effect_types(code: &str) -> Vec<Violation> {
    const RULE: &str = "effect-protocol";
    const DRIVER: &str = "waymaker-drive";

    let mut violations = Vec::new();

    // One flat module. `inherent_impl_bodies` reads `impl` at column zero, so an `impl` in a
    // nested module of this same file is indented and invisible to it — review of this change
    // added `pub(crate) fn abandon(self) -> Reserved<C>` to `Dispatchable` that way, and the
    // gate stayed green. Nothing here needs a submodule but the test one, which is removed
    // before this runs.
    if count_tokens(code, "mod") != 0 {
        violations.push(Violation::new(
            RULE,
            DRIVER,
            format!(
                "{EFFECT_PROTOCOL_PATH} declares a module: \u{a7}07's protocol is one flat \
                 module, because the method pin below reads `impl` at column zero and an \
                 `impl` inside a submodule escapes it"
            ),
        ));
    }

    for (type_name, methods) in EFFECT_TYPE_METHODS {
        let header = format!("pub struct {type_name}");
        if !names_identifier(code, &header) {
            violations.push(Violation::new(
                RULE,
                DRIVER,
                format!(
                    "{EFFECT_PROTOCOL_PATH} declares no `{header}`, so a type \u{a7}07's proof \
                     is carried in is pinned against nothing"
                ),
            ));
            continue;
        }
        // Before anything reads a body: `braced_body` takes the first match, so a decoy above
        // the real one is what every scan below would read. `kernel-boundary` fails over the
        // same shape.
        let declarations = declaration_count(code, &header);
        if declarations != 1 {
            violations.push(Violation::new(
                RULE,
                DRIVER,
                format!(
                    "`{header}` is declared {declarations} times: the scans below read the \
                     first, so a decoy above the real one is what they would check"
                ),
            ));
            continue;
        }
        // And before the field scan, because `braced_body` looks for the next `{` and a tuple
        // struct has none: it would read the `impl` block below and report on that instead.
        if !declares_braced_struct(code, &header) {
            violations.push(Violation::new(
                RULE,
                DRIVER,
                format!(
                    "`{type_name}` is not a braced struct: the field scan reads the first `{{` \
                     after the declaration, so a tuple or unit struct would have it reporting \
                     on whatever follows — and a `pub` tuple field is a proof anybody can forge"
                ),
            ));
            continue;
        }
        if braced_body(code, &header).is_some_and(|body| count_tokens(body, "pub") != 0) {
            violations.push(Violation::new(
                RULE,
                DRIVER,
                format!(
                    "`{type_name}` declares a public field: a public field is a constructor, \
                     and a forged proof of durable intent is a dispatch nobody committed"
                ),
            ));
        }

        violations.extend(check_effect_methods(code, type_name, methods));
    }

    violations
}

/// One type's method set, read at every visibility, over one file's text.
fn check_effect_methods(code: &str, type_name: &str, methods: &[&str]) -> Vec<Violation> {
    const RULE: &str = "effect-protocol";
    const DRIVER: &str = "waymaker-drive";

    let blocks = inherent_impl_bodies(code, type_name);
    if blocks.is_empty() {
        return vec![Violation::new(
            RULE,
            DRIVER,
            format!(
                "{EFFECT_PROTOCOL_PATH} declares no inherent `impl` for `{type_name}`, so the \
                 methods \u{a7}07 gives it are pinned against nothing"
            ),
        )];
    }
    let mut violations = Vec::new();
    let joined = blocks.join("\n");
    let declared = declared_function_names(&joined);
    let mut expected: Vec<String> = methods.iter().map(|name| (*name).to_owned()).collect();
    expected.sort();
    if declared != expected {
        violations.push(Violation::new(
            RULE,
            DRIVER,
            format!(
                "`{type_name}` declares {declared:?} rather than {expected:?}: read at every \
                 visibility, because a surface pin counts `pub ` and not `pub(`, and the \
                 crate that would call a `pub(crate)` forge is this one"
            ),
        ));
    }
    if EFFECT_NO_SELF_LITERAL.contains(&type_name) && implements_trait_for(code, type_name) {
        violations.push(Violation::new(
            RULE,
            DRIVER,
            format!(
                "`{type_name}` implements a trait: a trait body is not an inherent `impl` and \
                 writes `Self` rather than the type's name, so the method pin and the \
                 construction pin are both blind to it. What catches a \
                 `From<EffectId> for {type_name}` today is the surface pin, incidentally — \
                 either as a name it does not list or as one listed twice — and a proof of \
                 durable intent should not rest on an incidental"
            ),
        ));
    }
    if EFFECT_NO_SELF_LITERAL.contains(&type_name) && struct_literals(&joined, "Self") != 0 {
        violations.push(Violation::new(
            RULE,
            DRIVER,
            format!(
                "`{type_name}`'s own `impl` builds a `Self`: the construction pin counts the \
                 type's name, so a `Self {{ .. }}` is a proof of durable intent built where \
                 nothing can see it"
            ),
        ));
    }
    violations
}

/// Where a durable intent may be built, over one file's text.
fn check_effect_constructions(code: &str) -> Vec<Violation> {
    const RULE: &str = "effect-protocol";
    const DRIVER: &str = "waymaker-drive";

    let mut violations = Vec::new();
    // Both bodies belong to `Effect`, and they are read out of its own `impl` blocks for
    // `EFFECT_STEP_BODIES`' reason: a free `fn schedule` above the real one is the body a
    // first-match scan reads.
    let owner = inherent_impl_bodies(code, "Effect").join("\n");
    for (value, bodies) in EFFECT_CONSTRUCTIONS {
        let total = struct_literals(code, value);
        let mut inside = 0_usize;
        for body in bodies {
            let built = braced_body(&owner, &format!("fn {body}"))
                .map_or(0, |text| struct_literals(text, value));
            if built == 0 {
                violations.push(Violation::new(
                    RULE,
                    DRIVER,
                    format!(
                        "`{value}` is not built inside `{body}`: \u{a7}07 has two ways an \
                         intent becomes durable, and each has to be one of them"
                    ),
                ));
            }
            inside = inside.saturating_add(built);
        }
        if total != inside {
            violations.push(Violation::new(
                RULE,
                DRIVER,
                format!(
                    "`{value}` is built {total} time(s), {inside} of them inside {bodies:?}: a \
                     proof of durable intent has to come from a barrier that returned and \
                     from nowhere else"
                ),
            ));
        }
    }
    violations
}

/// §07's storage steps, in §07's order, over one file's text.
fn check_effect_steps(code: &str) -> Vec<Violation> {
    const RULE: &str = "effect-protocol";
    const DRIVER: &str = "waymaker-drive";

    let mut violations = Vec::new();
    for (owner, body_name) in EFFECT_STEP_BODIES {
        let blocks = inherent_impl_bodies(code, owner).join("\n");
        let Some(body) = braced_body(&blocks, &format!("fn {body_name}")) else {
            violations.push(Violation::new(
                RULE,
                DRIVER,
                format!(
                    "`{owner}` declares no `fn {body_name}`, so one half of \u{a7}07's protocol \
                     is pinned against nothing"
                ),
            ));
            continue;
        };
        // Counted and located on one whitespace-free copy, so a count and a position cannot
        // disagree about which occurrence they mean.
        let tight = tightened(body);
        violations.extend(check_effect_step_body_is_unconditional(
            owner, body_name, &tight,
        ));
        let mut previous = 0_usize;
        for step in EFFECT_STEPS {
            if tight.matches(step).count() != 1 {
                violations.push(Violation::new(
                    RULE,
                    DRIVER,
                    format!(
                        "`{owner}::{body_name}` does not take `{step}` exactly once: \u{a7}07 \
                         states the frame, the payload barrier and the seal as three steps, \
                         and a body that takes one of them twice or not at all is not that \
                         protocol"
                    ),
                ));
                continue;
            }
            let Some(at) = tight.find(step) else {
                continue;
            };
            if at < previous {
                violations.push(Violation::new(
                    RULE,
                    DRIVER,
                    format!(
                        "`{owner}::{body_name}` takes `{step}` before the step \u{a7}07 puts in \
                         front of it: the order is the guarantee, not the calls"
                    ),
                ));
            }
            // A step inside a block is a step a branch can skip, and three calls in the
            // right order on a path nothing takes is not §07's protocol. The position pin
            // below cannot see this, because a construction after a skipped `.commit(` is
            // still textually after it.
            if nesting_depth_at(&tight, at) != 0 {
                violations.push(Violation::new(
                    RULE,
                    DRIVER,
                    format!(
                        "`{owner}::{body_name}` takes `{step}` inside a block, an argument \
                         list or a closure: \u{a7}07's steps are what this body does, not what \
                         one of its branches does"
                    ),
                ));
            }
            previous = at;
        }
    }

    let owner = inherent_impl_bodies(code, "Effect").join("\n");
    let Some(body) = braced_body(&owner, "fn redelivering") else {
        violations.push(Violation::new(
            RULE,
            DRIVER,
            "`Effect` declares no `fn redelivering`, so \u{a7}08's redelivery row is pinned \
             against nothing",
        ));
        return violations;
    };
    for forbidden in EFFECT_REDELIVERY_FORBIDDEN {
        if count_tokens(body, forbidden) != 0 {
            violations.push(Violation::new(
                RULE,
                DRIVER,
                format!(
                    "`redelivering` names `{forbidden}`: the schedule record committed in an \
                     earlier boot, so a body that writes here writes a second one"
                ),
            ));
        }
    }

    violations.extend(check_effect_proof_position(code));
    violations
}

/// A step body runs its steps unconditionally, as far as a scanner can tell.
///
/// Two rounds of review reached the same shape from two directions.
/// `false.then(|| self.writer.stage(..).payload_barrier(..).commit(..))` has no braces, and
/// `false && self.writer.stage(..)?…` has no nesting either: both put all three pinned calls
/// once, in order, at depth zero, in code that never runs.
///
/// A scanner cannot follow control flow, so it refuses the constructs that create it. `|`
/// covers a closure and the `||` half of a short-circuit at once. Neither of the two bodies
/// this runs on has any use for either. That is a syntactic answer to a semantic question and
/// holds only as far as this list does, which
/// [what is not checked](https://github.com/madmax983/waymaker/blob/main/CLAUDE.md#what-is-not-checked)
/// says plainly.
fn check_effect_step_body_is_unconditional(
    owner: &str,
    body_name: &str,
    tight: &str,
) -> Vec<Violation> {
    const RULE: &str = "effect-protocol";
    const DRIVER: &str = "waymaker-drive";

    let mut violations = Vec::new();
    if tight.contains('|') {
        violations.push(Violation::new(
            RULE,
            DRIVER,
            format!(
                "`{owner}::{body_name}` declares a closure, or short-circuits with `||`: \
                 \u{a7}07's steps have to be what this body does, and a closure is a body of \
                 its own that may never be called"
            ),
        ));
    }
    if tight.contains("&&") {
        violations.push(Violation::new(
            RULE,
            DRIVER,
            format!(
                "`{owner}::{body_name}` short-circuits with `&&`: the right-hand side of one \
                 is code that may never run, and \u{a7}07's steps have to be what this body does"
            ),
        ));
    }
    violations
}

/// Every proof of durable intent is built after the barrier that earns it.
///
/// [`EFFECT_PROOF_AFTER`] says which type, which body and which step. Codex found that the
/// construction pin and the order pin were independent, so a `return` before `.stage(`
/// carrying a freshly built `Dispatchable` passed both.
fn check_effect_proof_position(code: &str) -> Vec<Violation> {
    const RULE: &str = "effect-protocol";
    const DRIVER: &str = "waymaker-drive";

    let mut violations = Vec::new();
    let (owner, body_name, step) = EFFECT_PROOF_AFTER;
    let blocks = inherent_impl_bodies(code, owner).join("\n");
    let Some(body) = braced_body(&blocks, &format!("fn {body_name}")) else {
        // `check_effect_steps` has already reported the missing body.
        return violations;
    };
    let tight = tightened(body);
    let Some(barrier) = tight.find(step) else {
        // Likewise: a body that never takes the step is already a violation.
        return violations;
    };
    for (value, _) in EFFECT_CONSTRUCTIONS {
        let literal = format!("{value}{{");
        for (at, _) in tight.match_indices(&literal) {
            if at < barrier {
                violations.push(Violation::new(
                    RULE,
                    DRIVER,
                    format!(
                        "`{owner}::{body_name}` builds `{value}` before the barrier that earns \
                         it: a proof of durable intent taken before `{step}` is a dispatch \
                         whose schedule record is not durable"
                    ),
                ));
            }
        }
    }
    violations
}

/// Rule: design document §06's kernel boundary is the one that was reviewed, and the driver
/// decides from it.
///
/// Two halves, one id, because it is one decision. The *shape* half pins the types issue #28
/// names — [`BOUNDARY_TYPES`] — in both directions, which is that issue's second "done when"
/// as a build failure: §09 has five record kinds reserved and not yet decoded, and a
/// `Resolve::TimerFired` added when the first of them lands would turn one boundary into a
/// boundary per record. The *routing* half pins that `waymaker-drive` decides from
/// [`BOUNDARY_DECISIONS`] and names none of [`DRIVER_FORBIDDEN_VOCABULARY`], which is what
/// makes "the protocol is driven through this boundary" a check rather than a reading.
///
/// Scanned over code with comments and string literals stripped, like every rule here, so a
/// doc comment naming `Resolve::Replayed` cannot vouch for an arm that is not there and a
/// commented-out variant cannot fail the pin.
///
/// What it cannot see: a widened member behind a name already on the list, and a driver that
/// names every decision and then ignores one. `crates/waymaker-drive/tests/` is what holds
/// the behaviour — a diverging workflow that dispatches nothing, a redelivery that reuses
/// its identity, and the whole protocol swept at every crash point the injector lists.
#[must_use]
pub fn check_kernel_boundary(
    layers: &[crate::size::LayerSource],
    driver: &[crate::size::LayerSource],
) -> Vec<Violation> {
    const RULE: &str = "kernel-boundary";
    const KERNEL: &str = "waymaker-core";
    const DRIVER: &str = "waymaker-drive";

    let mut violations = Vec::new();

    match find_source(layers, KERNEL_BOUNDARY_PATH) {
        None => violations.push(Violation::new(
            RULE,
            KERNEL,
            format!(
                "no {KERNEL_BOUNDARY_PATH} in the workspace, so the pinned boundary is \
                 checking nothing; issue #28 asks that adding a record kind not change this \
                 signature, and a pin that cannot find the types checks neither direction"
            ),
        )),
        Some(source) => {
            let code = without_test_modules(&code_only(&source.contents));
            let pin = MemberPin {
                rule: RULE,
                subject: KERNEL,
                path: KERNEL_BOUNDARY_PATH,
                table: "BOUNDARY_TYPES",
                why: "issue #28 asks that adding a record kind not change this signature, \
                      and \u{a7}09 reserves five kinds nobody has written a body for yet",
            };
            for pinned in BOUNDARY_TYPES {
                violations.extend(check_boundary_type(&pin, &code, pinned));
            }
        }
    }

    let Some(source) = find_source(driver, DRIVER_PATH) else {
        violations.push(Violation::new(
            RULE,
            DRIVER,
            format!(
                "no {DRIVER_PATH} in the workspace, so nothing shows that the protocol is \
                 driven through the kernel boundary; issue #28's first work item is that \
                 `waymaker-embassy` be provably a fa\u{e7}ade and nothing more"
            ),
        ));
        return violations;
    };

    // Without the test modules, for `integrity-check`'s reason: a decision named only under
    // `#[cfg(test)]` discharges nothing about the code that ships.
    let code = without_test_modules(&code_only(&source.contents));
    for decision in BOUNDARY_DECISIONS {
        // At a path boundary, like the forbidden half below. A `contains` is satisfied by a
        // longer path that ends in the same segments — a `SomeIntent::Finished` would vouch
        // for an `Intent::Finished` arm that is not there, and a pin that cannot fail is
        // worse than no pin because the report says it checked.
        if !names_identifier(&code, decision) {
            violations.push(Violation::new(
                RULE,
                DRIVER,
                format!(
                    "{DRIVER_PATH} names no `{decision}`, so one row of \u{a7}08's table is \
                     decided somewhere other than the kernel boundary"
                ),
            ));
        }
    }
    for (forbidden, why) in DRIVER_FORBIDDEN_VOCABULARY {
        if names_identifier(&code, forbidden) {
            violations.push(Violation::new(
                RULE,
                DRIVER,
                format!("{DRIVER_PATH} names `{forbidden}`, which {why}"),
            ));
        }
    }

    violations
}

/// Where a member pin lives, and why the members it lists are the members it lists.
///
/// Shared by `kernel-boundary` and `timer-capability`: both pin a *vocabulary* — the members
/// a type may declare — and both fail in either direction, so the reader is one function and
/// everything that differs between them is a field here.
struct MemberPin<'a> {
    /// The rule that reports.
    rule: &'static str,
    /// The crate the file belongs to.
    subject: &'a str,
    /// The file, for the message.
    path: &'a str,
    /// The constant holding the pin, so a failure names what to go and read.
    table: &'static str,
    /// Why a member added here costs something. One clause, appended to the message.
    why: &'static str,
}

/// One pinned type, compared against what the file declares.
fn check_boundary_type(pin: &MemberPin<'_>, code: &str, pinned: &BoundaryType) -> Vec<Violation> {
    let MemberPin {
        rule,
        subject,
        path,
        table,
        why,
    } = *pin;

    // Before the members, because `braced_body` reads the *first* declaration: a decoy above
    // the real one leaves the pin comparing something nobody ships. `integrity-check` fails
    // over the same shape and this is the same guard.
    let declarations = declaration_count(code, pinned.header);
    if declarations != 1 {
        return vec![Violation::new(
            rule,
            subject,
            format!(
                "{path} declares `{}` {declarations} times, not once; the pin reads the \
                 first declaration, so a second one leaves it comparing a type nobody ships",
                pinned.header
            ),
        )];
    }
    let Some(body) = braced_body(code, pinned.header) else {
        return vec![Violation::new(
            rule,
            subject,
            format!(
                "{path} declares no `{}`, so its pinned members are checking nothing",
                pinned.header
            ),
        )];
    };
    let declared = if pinned.header.starts_with("pub struct") {
        field_names(body)
    } else {
        variant_names(body)
    };
    let expected: BTreeSet<&str> = pinned.members.iter().copied().collect();
    let found: BTreeSet<&str> = declared.iter().map(String::as_str).collect();

    let mut violations = Vec::new();
    for added in found.difference(&expected) {
        violations.push(Violation::new(
            rule,
            subject,
            format!(
                "`{}` declares `{added}`, which {table} does not pin; {why}",
                pinned.header
            ),
        ));
    }
    for removed in expected.difference(&found) {
        violations.push(Violation::new(
            rule,
            subject,
            format!(
                "`{}` no longer declares `{removed}`, which {table} pins; a member the pin \
                 cannot find means the type was renamed and the pin has stopped checking it",
                pinned.header
            ),
        ));
    }
    violations
}

/// Rule: the integrity check is the catalogued, table-free one ADR 0010 settled on.
///
/// Two things. The algorithm parameters have to still be there, because a polynomial is the
/// algorithm and a changed one passes every round-trip test in this repository. And the
/// module has to declare no lookup table, because ADR 0010's measurement is what makes a
/// table a decision: 64 bytes of rodata for a nibble table, 1024 for a byte table, against
/// an 8 KiB incremental code-flash budget for the kernel and this adapter together.
///
/// `#[cfg(test)]` modules are skipped. `crc.rs` already holds a `const MESSAGE: [u8; 12]`
/// for its bit-flip sweep, and a rule that could not tell a test fixture from a lookup table
/// would be a rule that punishes testing.
#[must_use]
pub fn check_integrity_check(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let Some(source) = find_source(sources, INTEGRITY_CHECK_PATH) else {
        return vec![Violation::new(
            RULE,
            ADAPTER,
            format!(
                "no {INTEGRITY_CHECK_PATH} in the workspace, so the integrity check is \
                 checking nothing; \u{a7}16's first deferred question is which checksum the \
                 format uses, and ADR 0010 answers it with a measurement"
            ),
        )];
    };

    // `code_only` first, then the test-module scan: the scan counts braces and looks for an
    // attribute, and both are wrong on raw text — see `without_test_modules`.
    let code = without_test_modules(&code_only(&source.contents));
    let mut violations = Vec::new();

    for parameter in INTEGRITY_CHECK_PARAMETERS {
        let header = format!("fn {}", parameter.function);
        let Some(body) = braced_body(&code, &header) else {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{INTEGRITY_CHECK_PATH} declares no `{header}`, so the {} is pinned \
                     against nothing",
                    parameter.role
                ),
            ));
            continue;
        };
        let found = count_tokens(body, parameter.literal);
        if found != parameter.occurrences {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "`{}` in {INTEGRITY_CHECK_PATH} uses `{}` {found} time(s) where the {} \
                     is {} — a checksum whose parameters changed is a different checksum, \
                     and it passes every round-trip test in this repository",
                    parameter.function, parameter.literal, parameter.role, parameter.occurrences
                ),
            ));
        }
    }

    // The checksum module and anything it is split into. Codex asked for this on PR #58:
    // a table in `crc/table.rs` that `crc.rs` imports is the same 1 KiB of rodata, and a
    // rule that read one file would have called it absent.
    for scanned in checksum_sources(sources, &source.contents) {
        let scanned_code = without_test_modules(&code_only(&scanned.contents));
        for name in array_items(&scanned_code) {
            violations.push(Violation::new(
                RULE,
                ADAPTER,
                format!(
                    "{} declares `{name}` as an array, which is a lookup table; ADR 0010 \
                     measured what one costs — 64 B of rodata for a nibble table, 1024 B \
                     for a byte table, against an 8 KiB incremental code-flash budget — so \
                     adding one is a superseding ADR, not an optimisation",
                    scanned.path.replace('\\', "/")
                ),
            ));
        }
    }

    violations
}

/// The checksum module, plus every source under a `crc/` directory beside it, minus the
/// ones the parent declares behind `#[cfg(test)]`.
///
/// Splitting `crc.rs` into `crc/mod.rs` and `crc/table.rs` is an ordinary refactor and it is
/// how a lookup table would arrive without this rule seeing it, so the scan follows the
/// module rather than the file.
///
/// Moving the tests out to `crc/tests.rs` behind `#[cfg(test)] mod tests;` is an ordinary
/// refactor too, and Codex pointed out on PR #58 that the first version of this punished it:
/// the child file arrives without its parent's attribute, so the bit-flip sweep's
/// `const MESSAGE: [u8; 12]` would have been reported as a production lookup table. A rule
/// that rejects a test-only refactor is a rule contributors learn to work around.
#[must_use]
fn checksum_sources<'a>(
    sources: &'a [crate::size::LayerSource],
    parent: &str,
) -> Vec<&'a crate::size::LayerSource> {
    let directory = INTEGRITY_CHECK_PATH.trim_end_matches(".rs");
    let test_only = test_gated_modules(parent);

    sources
        .iter()
        .filter(|source| {
            let path = source.path.replace('\\', "/");
            if path.ends_with(INTEGRITY_CHECK_PATH) {
                return true;
            }
            if !path.contains(&format!("{directory}/")) {
                return false;
            }
            // `crc/tests.rs` and `crc/tests/mod.rs` both belong to the module `tests`.
            let stem = path
                .rsplit_once(&format!("{directory}/"))
                .map(|(_, tail)| tail)
                .unwrap_or_default()
                .trim_end_matches(".rs")
                .trim_end_matches("/mod");
            !test_only.iter().any(|name| name == stem)
        })
        .collect()
}

/// The names of modules `parent` declares out of line behind `#[cfg(test)]`.
///
/// Read from the raw text rather than from `code_only` output, because the attribute is what
/// is being looked for and stripping comments cannot help with that: a `// #[cfg(test)]`
/// line is prose, so the scan skips comment lines itself.
#[must_use]
fn test_gated_modules(parent: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut pending = false;

    for line in parent.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.contains("#[cfg(test)]") {
            pending = true;
        }
        if !pending {
            continue;
        }
        if let Some(rest) = trimmed.split_once("mod ").map(|(_, rest)| rest)
            && let Some(name) = rest.strip_suffix(';')
        {
            names.push(name.trim().to_owned());
            pending = false;
        } else if trimmed.contains('{') || trimmed.ends_with(';') {
            // Some other item took the attribute.
            pending = false;
        }
    }

    names
}

/// How many times `code` contains `token` as a whole token.
///
/// Counted rather than merely found, because CRC-32 uses `0xFFFF_FFFF` twice — as its
/// initial value and as its final xor — and Codex pointed out on PR #58 that a presence
/// check lets either one vouch for the other.
#[must_use]
fn count_tokens(code: &str, token: &str) -> usize {
    let continues = |character: char| character.is_alphanumeric() || character == '_';

    code.match_indices(token)
        .filter(|(index, _)| {
            let before_is_boundary = code
                .get(..*index)
                .and_then(|before| before.chars().next_back())
                .is_none_or(|character| !continues(character));
            let after_is_boundary = code
                .get(index + token.len()..)
                .and_then(|after| after.chars().next())
                .is_none_or(|character| !continues(character));
            before_is_boundary && after_is_boundary
        })
        .count()
}

/// The layer source whose path ends with `path`, if the workspace contributed one.
///
/// Path separators are normalised first: the gate runs on Windows too, and a pin that
/// silently found nothing there would be a pin that passes by not looking.
#[must_use]
fn find_source<'a>(
    sources: &'a [crate::size::LayerSource],
    path: &str,
) -> Option<&'a crate::size::LayerSource> {
    sources
        .iter()
        .find(|source| source.path.replace('\\', "/").ends_with(path))
}

/// The body of the first `{ ... }` block opened after `header` appears in `code`.
///
/// `header` is matched at a token boundary, which review of PR #58 found missing: a plain
/// `split_once("EffectScheduled")` was satisfied by an `EffectScheduledV1` variant declared
/// above it, so the pin read the decoy's field list and the real variant grew a fifth field
/// unseen. The same held one level up for an `enum RecordRefV2`. This module had already
/// settled the convention — `impl_headers` checks a boundary, with a test that says so —
/// and the pin was not following it.
///
/// Returns `None` when `header` is absent at a boundary, or opens no brace that closes
/// before the end of the input, so a caller that cannot find what it pins reports that
/// rather than pinning nothing.
#[must_use]
pub(crate) fn braced_body<'a>(code: &'a str, header: &str) -> Option<&'a str> {
    let continues = |character: char| character.is_alphanumeric() || character == '_';

    let after = code.match_indices(header).find_map(|(index, _)| {
        let before_is_boundary = code
            .get(..index)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|character| !continues(character));
        let rest = code.get(index + header.len()..)?;
        let after_is_boundary = rest.chars().next().is_none_or(|c| !continues(c));
        (before_is_boundary && after_is_boundary).then_some(rest)
    })?;

    let open = after.find('{')?;
    let body = after.get(open + 1..)?;

    let mut depth = 1_u32;
    for (index, character) in body.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return body.get(..index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether `code` names `identifier` at a token boundary.
///
/// `contains` is not enough for a vocabulary ban. `Step::` is evaded by `Step ::Record` and
/// by `use ...::Step as S;`, and it fires on an unrelated `BootStep::`. This compares both
/// sides, so `Step` matches the type and nothing else.
#[must_use]
fn names_identifier(code: &str, identifier: &str) -> bool {
    let continues = |character: char| character.is_alphanumeric() || character == '_';
    code.match_indices(identifier).any(|(index, _)| {
        let before = code
            .get(..index)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|character| !continues(character));
        let after = code
            .get(index + identifier.len()..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|character| !continues(character));
        before && after
    })
}

/// How many times `code` declares `header` at a token boundary.
///
/// A first-match scan reads a decoy. `integrity-check` already fails a build over a shipped
/// `impl` "declared twice — a decoy above the real one is what a first-match scan reads",
/// and the pin here is the same shape and needs the same guard.
#[must_use]
fn declaration_count(code: &str, header: &str) -> usize {
    let continues = |character: char| character.is_alphanumeric() || character == '_';
    code.match_indices(header)
        .filter(|(index, _)| {
            let before = code
                .get(..*index)
                .and_then(|before| before.chars().next_back())
                .is_none_or(|character| !continues(character));
            let after = code
                .get(index + header.len()..)
                .and_then(|rest| rest.chars().next())
                .is_none_or(|character| !continues(character));
            before && after
        })
        .count()
}

/// The variant names declared directly in an enum `body`, ignoring anything nested.
///
/// A variant is the first identifier of each top-level comma-separated segment. Attribute
/// contents are inside brackets and so are nested; a `#` at depth zero is not an identifier
/// character, so it clears the token rather than becoming one.
#[must_use]
fn variant_names(body: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut depth = 0_u32;
    let mut token = String::new();
    let mut recorded = false;

    for character in body.chars() {
        match character {
            '{' | '(' | '[' | '<' => {
                if depth == 0 && !recorded && !token.is_empty() {
                    names.push(token.clone());
                    recorded = true;
                }
                depth += 1;
                token.clear();
            }
            '}' | ')' | ']' | '>' => {
                depth = depth.saturating_sub(1);
                token.clear();
            }
            ',' if depth == 0 => {
                if !recorded && !token.is_empty() {
                    names.push(token.clone());
                }
                token.clear();
                recorded = false;
            }
            c if depth == 0 && (c.is_alphanumeric() || c == '_') => token.push(c),
            _ => {
                if depth == 0 && !recorded && !token.is_empty() {
                    names.push(token.clone());
                    recorded = true;
                }
                token.clear();
            }
        }
    }
    if !recorded && !token.is_empty() {
        names.push(token);
    }
    names.sort_unstable();
    names.dedup();
    names
}

/// The `name:` field names declared directly in `body`, ignoring anything nested.
///
/// A path-qualified type is the case worth naming: `seq: crate::id::EffectSeq` must not read
/// as a field called `crate`, so a `:` that is part of a `::` never ends a field name.
#[must_use]
fn field_names(body: &str) -> Vec<String> {
    let characters: Vec<char> = body.chars().collect();
    let mut names = Vec::new();
    let mut depth = 0_u32;
    let mut token = String::new();

    let mut index = 0;
    while index < characters.len() {
        let character = characters.get(index).copied().unwrap_or(' ');
        match character {
            '{' | '(' | '[' | '<' => {
                depth += 1;
                token.clear();
            }
            '}' | ')' | ']' | '>' => {
                depth = depth.saturating_sub(1);
                token.clear();
            }
            ':' if depth == 0 => {
                let doubled = characters.get(index + 1).copied() == Some(':');
                if !doubled && !token.is_empty() {
                    names.push(token.clone());
                }
                token.clear();
                if doubled {
                    index += 1;
                }
            }
            c if c.is_alphanumeric() || c == '_' => token.push(c),
            _ => token.clear(),
        }
        index += 1;
    }

    names
}

/// The names of items in `code` that declare an array — a lookup table, however spelled.
///
/// Scanned as tokens over the whole text rather than a line at a time, which the first
/// version got wrong four ways, three of them found by review of PR #58:
///
/// * a line-anchored scan that stripped `pub ` missed `pub(crate) const TABLE: [u32; 16]` —
///   the spelling this module uses for its own helpers, so the first one a contributor
///   would reach for;
/// * it missed a long item `rustfmt` had wrapped so the type sat on the next line;
/// * it missed `type Nibbles = [u32; 16]` with a `const` of that type, which is a table
///   with a name in front of it;
/// * and it deliberately excused a `let`, on the reasoning that "a table has to outlive the
///   call to be a table". That reasoning is wrong on this target: compiled for
///   `thumbv6m-none-eabi` at `opt-level = "z"`, a local `[u32; 16]` is emitted as
///   constant-pool words inside `.text` plus a stack copy — the same ~64 B ADR 0010
///   measured and rejected, and small enough that `cargo xtask size` would not notice it
///   either. So a local counts.
///
/// A gate that permits exactly the thing it claims to reject is worse than no gate, because
/// the report says it checked.
///
/// A reference in front does not stop it being a table, so `&[u32; 16]` and `&'static [u32]`
/// count. `const fn` does not: the keyword is the same and the item is code, which is why
/// the real module's `pub(crate) const fn crc32` does not trip this.
#[must_use]
fn array_items(code: &str) -> Vec<String> {
    const KEYWORDS: [&str; 4] = ["const", "static", "type", "let"];

    let mut names = Vec::new();
    for (index, _) in code.char_indices() {
        let Some(tail) = code.get(index..) else {
            continue;
        };
        let Some(keyword) = KEYWORDS.iter().find(|keyword| tail.starts_with(**keyword)) else {
            continue;
        };
        // A token boundary before, so that `MY_const` and `statics` are not items.
        if code
            .get(..index)
            .and_then(|before| before.chars().next_back())
            .is_some_and(|character| character.is_alphanumeric() || character == '_')
        {
            continue;
        }
        let Some(after) = tail.get(keyword.len()..) else {
            continue;
        };
        if !after.starts_with(char::is_whitespace) {
            continue;
        }
        let after = after.trim_start();
        // `static mut TABLE` and `let mut table` are still tables. `const fn` is not.
        let after = after.strip_prefix("mut ").map_or(after, str::trim_start);
        if after.starts_with("fn ") {
            continue;
        }

        let name: String = after
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let Some(after_name) = after.get(name.len()..).map(str::trim_start) else {
            continue;
        };

        // `const`, `static` and an annotated `let` declare the array in their type; a
        // `type` alias and an initialised `let` declare it on the right of the `=`. A
        // doubled separator is neither: `::` is a path and `==` a comparison, so the
        // keyword was not an item header at all.
        let declares_array = after_name
            .strip_prefix([':', '='])
            .is_some_and(|declared| !declared.starts_with([':', '=']) && is_array_type(declared));

        if declares_array {
            names.push(name);
        }
    }
    names
}

/// Whether `declared_type` is an array, or any number of references to one.
///
/// Lifetimes and `mut` are skipped, so `&'static mut [u32; 16]` reads as the table it is.
#[must_use]
fn is_array_type(declared_type: &str) -> bool {
    let mut rest = declared_type;
    loop {
        rest = rest.trim_start();
        if let Some(stripped) = rest.strip_prefix('&') {
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix("mut ") {
            rest = stripped;
        } else if rest.starts_with('\'') {
            let lifetime: usize = rest
                .chars()
                .take_while(|character| {
                    *character == '\'' || character.is_alphanumeric() || *character == '_'
                })
                .map(char::len_utf8)
                .sum();
            let Some(stripped) = rest.get(lifetime..) else {
                return false;
            };
            rest = stripped;
        } else {
            return rest.starts_with('[');
        }
    }
}

/// `code` with every `#[cfg(test)]` block blanked out, line for line.
///
/// **Give this `code_only` output, not raw source.** Review of PR #58 found the raw-text
/// version wrong in both directions: `#[cfg(test)]` written inside a block comment armed
/// the scan, and a `{` inside a string literal — a `"{{"` in a format string, say — left
/// the brace depth permanently short, so the test module never closed and every line after
/// it was dropped. `crc.rs`'s own `"byte {index} bit {bit}"` happens to balance, which is
/// luck and not a property anybody maintains.
///
/// The same review found the latch: `pending` was cleared only by a line opening a brace,
/// so `#[cfg(test)]` on a braceless item — `mod tests;` after the tests move to a file of
/// their own, or a `#[cfg(test)] use` — blanked the whole rest of the file and left the
/// `integrity-check` rule reporting success having read almost nothing. A braceless item
/// now ends at its semicolon.
///
/// Lines are replaced rather than removed so that anything reported against this text still
/// lines up with the file.
#[must_use]
pub(crate) fn without_test_modules(code: &str) -> String {
    let mut kept = String::with_capacity(code.len());
    let mut depth: i32 = 0;
    let mut test_block: Option<i32> = None;
    let mut pending = false;

    for line in code.lines() {
        let trimmed = line.trim();
        let opens = i32::try_from(trimmed.matches('{').count()).unwrap_or(0);
        let closes = i32::try_from(trimmed.matches('}').count()).unwrap_or(0);

        if trimmed.contains("#[cfg(test)]") {
            pending = true;
        }
        let mut ends_a_braceless_item = false;
        if pending {
            if opens > 0 {
                test_block = Some(depth);
                pending = false;
            } else if trimmed.ends_with(';') {
                // `#[cfg(test)] mod tests;` — the item is one line and ends here.
                pending = false;
                ends_a_braceless_item = true;
            }
        }

        if test_block.is_none() && !pending && !ends_a_braceless_item {
            kept.push_str(line);
        }
        kept.push('\n');

        depth += opens - closes;
        if let Some(started_at) = test_block
            && depth <= started_at
        {
            test_block = None;
        }
    }

    kept
}

/// The body both surface pins share: `path` declares exactly `pinned`, no more and no less.
///
/// One function rather than two near-copies, because the failure this guards against is a
/// pin that quietly stops checking — and two implementations is exactly how one of them
/// ends up with the fails-closed branch and the other without it.
///
/// Scanned with the same reader `size-probe-reach` uses, so `#[cfg(test)]` helpers are
/// skipped and a trait method counts even without `pub` on it.
#[must_use]
fn check_pinned_surface(
    rule: &'static str,
    subject: &str,
    path: &str,
    pinned: &[&str],
    sources: &[crate::size::LayerSource],
    purpose: &str,
) -> Vec<Violation> {
    let Some(source) = sources
        .iter()
        .find(|source| source.path.replace('\\', "/").ends_with(path))
    else {
        return vec![Violation::new(
            rule,
            subject.to_owned(),
            format!(
                "no {path} in the workspace, so the pinned surface is checking nothing; \
                 {purpose}"
            ),
        )];
    };

    let mut declarations: Vec<String> =
        crate::size::public_functions(core::slice::from_ref(source))
            .into_iter()
            .map(|function| function.name)
            .collect();
    declarations.sort_unstable();

    let mut violations = Vec::new();

    // Counted, not merely collected. A set would fold a *second* declaration of a pinned
    // name into the first, so `pub fn advance(id: EffectId) -> Option<RecordRef<'_>>`
    // alongside `ReplayCursor::advance` would leave this rule reporting no difference at
    // all — and `size-probe-reach` matches calls by name too, so the new body would be
    // dead-stripped with both gates green. The pin is a list of names, so a name used twice
    // is a name the pin can no longer speak about, whatever the second one turns out to be.
    for (index, name) in declarations.iter().enumerate() {
        if declarations.get(index.wrapping_add(1)) == Some(name) {
            violations.push(Violation::new(
                rule,
                subject.to_owned(),
                format!(
                    "{} declares `{name}` more than once: the pin is a list of names, so a \
                     second declaration under a name already on it is invisible to this rule \
                     and to `size-probe-reach`; give it a name of its own",
                    source.path
                ),
            ));
        }
    }

    let declared: BTreeSet<String> = declarations.into_iter().collect();
    let pinned: BTreeSet<String> = pinned.iter().map(|name| (*name).to_owned()).collect();

    for added in declared.difference(&pinned) {
        violations.push(Violation::new(
            rule,
            subject.to_owned(),
            format!(
                "{} declares `{added}`, which the pinned surface does not list: {purpose}",
                source.path
            ),
        ));
    }
    for missing in pinned.difference(&declared) {
        violations.push(Violation::new(
            rule,
            subject.to_owned(),
            format!(
                "the pinned surface lists `{missing}`, which {} no longer declares; a pin \
                 nothing matches is a pin that has stopped checking",
                source.path
            ),
        ));
    }
    violations
}

/// Whether `line` — an `impl` header with its whitespace and lifetimes already removed —
/// implements exactly the trait `needle`.
///
/// Matched against the *start* of the trait position rather than anywhere in the line, for
/// two reasons. `TryFrom<&[u8]>` contains `From<&[u8]>`, so a substring test reports one
/// `impl` as two violations and tells a reader that a single line broke two rules — which is
/// the kind of noise that gets a gate switched off. And a trait named in a `where` clause or
/// a doc link is not an implementation of it.
fn implements_trait(line: &str, needle: &str) -> bool {
    let Some(rest) = line.strip_prefix("impl") else {
        return false;
    };
    // An optional generic parameter list sits between `impl` and the trait. Skipped by
    // depth rather than by finding the first `>`, so that `impl<T:Into<u8>>` does not stop
    // at the inner one.
    let after_generics = if rest.starts_with('<') {
        let mut depth = 0_usize;
        let mut end = None;
        for (index, character) in rest.char_indices() {
            match character {
                '<' => depth = depth.saturating_add(1),
                '>' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(index.saturating_add(1));
                        break;
                    }
                }
                _ => {}
            }
        }
        match end.and_then(|at| rest.get(at..)) {
            Some(after) => after,
            None => return false,
        }
    } else {
        rest
    };
    strip_path_qualifiers(after_generics).starts_with(needle)
}

/// The code in `contents`, with every comment and every literal's contents blanked out.
///
/// One pass, character by character, tracking whether it is inside a line comment, a block
/// comment (which nests in Rust), a string, a raw string, or a character literal.
/// Newlines survive so a reader can still count lines; everything else that is not code is
/// dropped.
///
/// # Why a real lexical pass rather than a substring scan
///
/// Because the two rules below decide whether the kernel may grow a decoder, and a gate a
/// comment can switch off is worse than no gate. Each of these was a live defect in a
/// simpler version of this function, and each has a test:
///
/// * `// see for example: /*` — an unmatched block-comment opener inside a *line* comment.
///   Stripping block comments first swallowed the rest of the file, so a `from_le_bytes`
///   after it was not reported. A false negative in a rule, which is the direction that
///   does not announce itself.
/// * `"impl TryFrom<&[u8]> for RecordRef {"` in a diagnostic string. Scanning flattened
///   text found an implementation header inside a literal and failed the build for one.
/// * `'"'` — a character literal holding a quote, which opened a string that never closed.
/// * `/* /* */ */` — Rust block comments nest, and a scan that stopped at the first `*/`
///   would treat the tail as code.
fn code_only(contents: &str) -> String {
    let source: Vec<char> = contents.chars().collect();
    let mut code = String::with_capacity(contents.len());
    let mut at = 0;
    let mut block_depth: usize = 0;

    while let Some(current) = source.get(at).copied() {
        let next = source.get(at.saturating_add(1)).copied();

        if block_depth > 0 {
            if current == '*' && next == Some('/') {
                block_depth = block_depth.saturating_sub(1);
                at = at.saturating_add(2);
            } else if current == '/' && next == Some('*') {
                block_depth = block_depth.saturating_add(1);
                at = at.saturating_add(2);
            } else {
                if current == '\n' {
                    code.push('\n');
                }
                at = at.saturating_add(1);
            }
            continue;
        }

        // A line comment reaches the newline and no further, so a `/*` inside one opens
        // nothing.
        if current == '/' && next == Some('/') {
            while source.get(at).copied().is_some_and(|c| c != '\n') {
                at = at.saturating_add(1);
            }
            continue;
        }
        if current == '/' && next == Some('*') {
            block_depth = 1;
            at = at.saturating_add(2);
            continue;
        }
        if let Some(after) = raw_string_end(&source, at) {
            at = after;
            continue;
        }
        if current == '"' {
            at = string_end(&source, at.saturating_add(1));
            continue;
        }
        if current == '\'' && is_character_literal(&source, at) {
            at = character_literal_end(&source, at.saturating_add(1));
            continue;
        }

        code.push(current);
        at = at.saturating_add(1);
    }
    code
}

/// The index just past a raw string starting at `at`, or [`None`] if one does not.
///
/// Recognises `r"…"` and `r#"…"#` with any number of hashes, and both prefixed forms Rust
/// has: `br"…"` for a byte string and `cr"…"` for a C string. The `r` must start a token:
/// the one in `for` is not a raw string.
///
/// The `c` form is not hypothetical tidiness. Missing it means `cr#"a " /*"#` is lexed as an
/// ordinary string that ends at the quote inside it, leaving a `/*` that opens a block
/// comment and swallows the rest of the file — the same false negative, arrived at from a
/// literal form that has been stable since Rust 1.77.
fn raw_string_end(source: &[char], at: usize) -> Option<usize> {
    let mut cursor = at;
    if matches!(source.get(cursor).copied(), Some('b' | 'c')) {
        cursor = cursor.saturating_add(1);
    }
    if source.get(cursor).copied() != Some('r') {
        return None;
    }
    let starts_token = at
        .checked_sub(1)
        .and_then(|before| source.get(before).copied())
        .is_none_or(|character| !character.is_alphanumeric() && character != '_');
    if !starts_token {
        return None;
    }

    cursor = cursor.saturating_add(1);
    let mut hashes: usize = 0;
    while source.get(cursor).copied() == Some('#') {
        hashes = hashes.saturating_add(1);
        cursor = cursor.saturating_add(1);
    }
    if source.get(cursor).copied() != Some('"') {
        return None;
    }
    cursor = cursor.saturating_add(1);

    // A raw string has no escapes: it ends at the first quote followed by as many hashes as
    // it opened with.
    while let Some(character) = source.get(cursor).copied() {
        cursor = cursor.saturating_add(1);
        if character != '"' {
            continue;
        }
        let closed = (0..hashes)
            .all(|offset| source.get(cursor.saturating_add(offset)).copied() == Some('#'));
        if closed {
            return Some(cursor.saturating_add(hashes));
        }
    }
    Some(cursor)
}

/// The index just past an ordinary string whose opening quote was before `at`.
fn string_end(source: &[char], mut at: usize) -> usize {
    while let Some(character) = source.get(at).copied() {
        at = at.saturating_add(1);
        match character {
            '\\' => at = at.saturating_add(1),
            '"' => return at,
            _ => {}
        }
    }
    at
}

/// Whether the quote at `at` opens a character literal rather than a lifetime.
///
/// `'a'` is a literal and `'a` is a lifetime, and the difference matters: `'"'` is ordinary
/// Rust, and reading it as a lifetime opens a string that never closes.
fn is_character_literal(source: &[char], at: usize) -> bool {
    match source.get(at.saturating_add(1)).copied() {
        // An escape is always a literal: no lifetime begins with a backslash.
        Some('\\') => true,
        // Otherwise it is a literal exactly when a closing quote follows the single
        // character — `'a'` — and a lifetime when an identifier continues instead.
        Some(_) => source.get(at.saturating_add(2)).copied() == Some('\''),
        None => false,
    }
}

/// The index just past a character literal whose opening quote was before `at`.
fn character_literal_end(source: &[char], mut at: usize) -> usize {
    while let Some(character) = source.get(at).copied() {
        at = at.saturating_add(1);
        match character {
            '\\' => at = at.saturating_add(1),
            '\'' => return at,
            _ => {}
        }
    }
    at
}

/// Every `impl` header in `code`, flattened to one line each.
///
/// `rustfmt` breaks a long header across lines — `impl<'a>` on one and
/// `TryFrom<&'a [u8]> for RecordRef<'a>` on the next — and a scan that looked at lines
/// individually would find no line carrying both `impl` and the trait. So the whole file is
/// flattened first and each header is taken from its `impl` token to the `{` or `;` that
/// ends it. Ordinary formatting cannot hide a header from this.
fn impl_headers(code: &str) -> Vec<String> {
    let flattened = code.split_whitespace().collect::<Vec<&str>>().join(" ");
    let mut headers = Vec::new();
    let mut rest = flattened.as_str();

    while let Some(at) = rest.find("impl") {
        let preceded_by_identifier = rest
            .get(..at)
            .and_then(|before| before.chars().next_back())
            .is_some_and(|character| character.is_alphanumeric() || character == '_');
        let from_impl = rest.get(at..).unwrap_or_default();
        if !preceded_by_identifier {
            let brace = from_impl.find('{').unwrap_or(from_impl.len());
            let end = from_impl.find(';').map_or(brace, |semi| semi.min(brace));
            headers.push(from_impl.get(..end).unwrap_or_default().to_owned());
        }
        rest = from_impl.get("impl".len()..).unwrap_or_default();
    }
    headers
}

/// Path qualifiers that a trait may be written with and that mean nothing to this scan.
///
/// `impl core::convert::TryFrom<&[u8]> for RecordRef<'_>` is the ordinary fully qualified
/// spelling of the very thing the rule rejects, and a bare `starts_with` would not see it.
const TRAIT_PATH_PREFIXES: &[&str] = &["::", "core::", "std::", "convert::"];

/// Drops leading path qualification from a whitespace-free trait position.
fn strip_path_qualifiers(mut trait_position: &str) -> &str {
    let mut stripped = true;
    while stripped {
        stripped = false;
        for prefix in TRAIT_PATH_PREFIXES {
            if let Some(rest) = trait_position.strip_prefix(prefix) {
                trait_position = rest;
                stripped = true;
            }
        }
    }
    trait_position
}

/// Removes `'a`-style lifetimes so an `impl` header can be matched without them.
fn erase_lifetimes(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find('\'') {
        out.push_str(rest.get(..at).unwrap_or_default());
        let after = rest.get(at.saturating_add(1)..).unwrap_or_default();
        let end = after
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(after.len());
        rest = after.get(end..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A façade file set with one codec module and one that is not.
    fn facade_sources(decode: &str, ctx: &str) -> Vec<crate::size::LayerSource> {
        vec![
            crate::size::LayerSource {
                crate_name: CODEC_CRATE.to_owned(),
                path: format!("crates/{CODEC_PATH}"),
                contents: decode.to_owned(),
            },
            crate::size::LayerSource {
                crate_name: CODEC_CRATE.to_owned(),
                path: format!("crates/{CTX_FACADE_PATH}"),
                contents: ctx.to_owned(),
            },
        ]
    }

    fn codec_manifests(contents: &str) -> Vec<(String, String)> {
        vec![(CODEC_CRATE.to_owned(), contents.to_owned())]
    }

    #[test]
    fn a_facade_that_keeps_its_codec_optional_is_accepted() {
        let violations = check_codec_is_optional(
            &facade_sources(&tests_support::clean_codec_module(), "pub struct Ctx;\n"),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_codec_named_outside_the_codec_module_is_reported() {
        // The failure §02 decision 4 is actually about: not a dependency, a *bound*. Every
        // workflow carries the codec whatever the manifest says.
        let violations = check_codec_is_optional(
            &facade_sources(
                &tests_support::clean_codec_module(),
                "pub fn activity<T: DeserializeOwned>() {}\n",
            ),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.rule == "codec-is-optional"
                    && violation.detail.contains("DeserializeOwned")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_codec_item_that_is_not_behind_a_feature_is_reported() {
        let violations = check_codec_is_optional(
            &facade_sources(
                "pub trait Decode: Sized {}\npub struct Postcard;\n",
                "pub struct Ctx;\n",
            ),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("Postcard")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_codec_in_an_items_body_is_reported_though_its_first_line_is_clean() {
        // Review of this change wrote exactly this: a declaration line naming no codec
        // word, with the codec in a field below it. A line-based scan reads the first line
        // and skips the rest as members of an item the attribute above already decided.
        let violations = check_codec_is_optional(
            &facade_sources(
                "pub trait Decode: Sized {}\npub struct Bridge {\n    inner: Postcard,\n}\n",
                "pub struct Ctx;\n",
            ),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("Postcard")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_gated_item_that_spans_lines_is_accepted() {
        let module = format!(
            "{}#[cfg(feature = \"postcard\")]\npub struct Wide {{\n    inner: Postcard,\n}}\n",
            tests_support::clean_codec_module()
        );

        let violations = check_codec_is_optional(
            &facade_sources(&module, "pub struct Ctx;\n"),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_codec_behind_a_compound_cfg_is_reported() {
        // Conservative on purpose: `any(feature = "postcard", unix)` is true in a default
        // build on a host, so a scan that read every `#[cfg(` as gating would accept an
        // item that really is in the default build.
        let module = format!(
            "{}#[cfg(any(feature = \"postcard\", unix))]\npub struct Loose(Postcard);\n",
            tests_support::clean_codec_module()
        );

        let violations = check_codec_is_optional(
            &facade_sources(&module, "pub struct Ctx;\n"),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("Loose")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_attribute_between_the_gate_and_the_item_does_not_ungate_it() {
        let module = format!(
            "{}#[cfg(feature = \"postcard\")]\n#[derive(Debug)]\npub struct Derived(Postcard);\n",
            tests_support::clean_codec_module()
        );

        let violations = check_codec_is_optional(
            &facade_sources(&module, "pub struct Ctx;\n"),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_gate_above_one_item_does_not_carry_to_the_next() {
        let module = format!(
            "{}#[cfg(feature = \"postcard\")]\npub struct First(Postcard);\npub struct Second(Postcard);\n",
            tests_support::clean_codec_module()
        );

        let violations = check_codec_is_optional(
            &facade_sources(&module, "pub struct Ctx;\n"),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("Second")),
            "{violations:?}"
        );
        assert!(
            !violations
                .iter()
                .any(|violation| violation.detail.contains("First")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_decode_trait_hidden_behind_a_feature_is_reported() {
        // The other direction, and the one that breaks the default build: the trait every
        // workflow names must be there when no feature is.
        let violations = check_codec_is_optional(
            &facade_sources(
                "#[cfg(feature = \"serde\")]\npub trait Decode: Sized {}\n",
                "pub struct Ctx;\n",
            ),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("Decode")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_decode_trait_behind_a_compound_cfg_is_reported() {
        // Review of this change ran this and watched the gate stay green: the trait branch
        // reported only when the item was classified as gated, and a compound `cfg` is not.
        // So `Decode` left the default build with no violation, while the same spelling on
        // a codec item was reported. The two halves now fail in the same direction.
        let violations = check_codec_is_optional(
            &facade_sources(
                "#[cfg(all(feature = \"serde\"))]\npub trait Decode: Sized {}\n",
                "pub struct Ctx;\n",
            ),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("Decode")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_renamed_trait_does_not_satisfy_the_pin() {
        // `starts_with` alone reads `pub trait Decoder` as the pinned trait, so the
        // "declares no `pub trait Decode`" branch was satisfied by a trait that is not it.
        let violations = check_codec_is_optional(
            &facade_sources("pub trait Decoder: Sized {}\n", "pub struct Ctx;\n"),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("declares no")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_codec_named_in_an_attribute_is_reported() {
        // A scan that skipped attribute lines read this as an item naming no codec.
        let violations = check_codec_is_optional(
            &facade_sources(
                "pub trait Decode: Sized {}\n\
                 #[cfg_attr(feature = \"serde\", derive(serde::Deserialize))]\n\
                 pub struct Answer(u8);\n",
                "pub struct Ctx;\n",
            ),
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("Answer")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_missing_codec_module_is_reported() {
        let violations = check_codec_is_optional(
            &[],
            &codec_manifests(&tests_support::clean_codec_manifest()),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains(CODEC_PATH)),
            "{violations:?}"
        );
    }

    #[test]
    fn a_codec_dependency_that_is_not_optional_is_reported() {
        let manifest = tests_support::clean_codec_manifest().replace(
            "postcard = { version = \"1\", optional = true, default-features = false }",
            "postcard = { version = \"1\", default-features = false }",
        );

        let violations = check_codec_is_optional(
            &facade_sources(&tests_support::clean_codec_module(), "pub struct Ctx;\n"),
            &codec_manifests(&manifest),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("postcard")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_codec_feature_that_enables_the_wrong_thing_is_reported() {
        let manifest = tests_support::clean_codec_manifest()
            .replace("postcard = [\"dep:postcard\", \"serde\"]", "postcard = []");

        let violations = check_codec_is_optional(
            &facade_sources(&tests_support::clean_codec_module(), "pub struct Ctx;\n"),
            &codec_manifests(&manifest),
        );

        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("dep:postcard")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_facade_manifest_the_gate_cannot_read_is_reported() {
        let violations = check_codec_is_optional(
            &facade_sources(&tests_support::clean_codec_module(), "pub struct Ctx;\n"),
            &[],
        );

        assert!(
            !violations.is_empty(),
            "a manifest nobody read checks nothing"
        );
    }

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
    fn a_nested_block_comment_stays_open_until_its_outer_close() {
        // Rust nests block comments. A scanner tracking a boolean rather than a depth
        // reopens at the inner `*/`, and reads the attribute below it as live code.
        let nested = "/* off:\n/* note */\n#![warn(missing_docs)]\n*/\n#![no_std]\n";
        assert_eq!(inner_attributes(nested), ["#![no_std]"]);
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

    /// The replay module as the pin expects it, optionally with a line appended.
    fn replay_source(extra: &str) -> Vec<crate::size::LayerSource> {
        vec![crate::size::LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: format!("crates/{REPLAY_SURFACE_PATH}"),
            contents: format!("{}{extra}", tests_support::clean_replay_surface()),
        }]
    }

    #[test]
    fn the_pinned_replay_surface_passes() {
        assert!(check_replay_cursor_surface(&replay_source("")).is_empty());
    }

    #[test]
    fn a_lookup_by_effect_id_is_rejected_by_name() {
        // The shape the rule exists for: it breaks no layering rule, needs no dependency,
        // and turns a sequential cursor into one that seeks.
        let violations = check_replay_cursor_surface(&replay_source(
            "pub fn record_at(&self, id: EffectId) -> Option<RecordRef<'_>> { None }\n",
        ));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "replay-cursor-surface");
        assert!(
            violations[0].detail.contains("record_at"),
            "{}",
            violations[0].detail
        );
    }

    #[test]
    fn a_second_declaration_under_a_pinned_name_is_rejected() {
        // The hole a set would leave: a lookup by effect id, named after a function already
        // on the pin. Set difference sees nothing added and nothing missing, and
        // `size-probe-reach` credits the probe's existing `advance(` call, so both gates
        // stay green while the cursor grows the one method it must never have.
        let violations = check_replay_cursor_surface(&replay_source(
            "pub fn advance(id: EffectId) -> Option<RecordRef<'static>> { None }\n",
        ));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "replay-cursor-surface");
        assert!(
            violations[0].detail.contains("more than once"),
            "{}",
            violations[0].detail
        );
    }

    #[test]
    fn a_pinned_name_the_module_no_longer_declares_is_rejected() {
        // The direction that matters more: a pin nothing matches has stopped checking.
        let thinned = tests_support::clean_replay_surface().replace("pub fn advance() {}\n", "");
        let violations = check_replay_cursor_surface(&[crate::size::LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: format!("crates/{REPLAY_SURFACE_PATH}"),
            contents: thinned,
        }]);
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].detail.contains("advance"),
            "{}",
            violations[0].detail
        );
    }

    #[test]
    fn a_workspace_with_no_replay_module_fails_closed() {
        // Renamed or deleted, the pin checks nothing — so the gate refuses rather than
        // reporting success it did not establish.
        let violations = check_replay_cursor_surface(&kernel_source("pub fn nothing() {}\n"));
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].detail.contains("checking nothing"),
            "{}",
            violations[0].detail
        );
    }

    /// One transition-module source, for the second surface pin.
    fn transition_source(extra: &str) -> Vec<crate::size::LayerSource> {
        vec![crate::size::LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: format!("crates/{TRANSITION_SURFACE_PATH}"),
            contents: format!("{}{extra}", tests_support::clean_transition_surface()),
        }]
    }

    #[test]
    fn the_pinned_transition_surface_passes() {
        assert!(check_transition_surface(&transition_source("")).is_empty());
    }

    #[test]
    fn a_way_out_of_a_divergence_is_rejected_by_name() {
        // The shape the rule exists for: it breaks no layering rule, needs no dependency,
        // and turns design document §08's "stop, never guess" into a suggestion.
        for escape in [
            "pub fn clear_divergence(&mut self) {}\n",
            "pub fn reset(&mut self) {}\n",
            "pub fn resume(&mut self) {}\n",
        ] {
            let violations = check_transition_surface(&transition_source(escape));
            assert_eq!(violations.len(), 1, "{escape}");
            assert_eq!(violations[0].rule, "transition-surface");
            assert!(
                violations[0].detail.contains("stop, never guess"),
                "{}",
                violations[0].detail
            );
        }
    }

    #[test]
    fn a_pinned_transition_name_the_module_no_longer_declares_is_rejected() {
        // The direction that matters more: a pin nothing matches has stopped checking, and
        // a machine whose refusal nobody guards is a machine that will grow a way out.
        let thinned =
            tests_support::clean_transition_surface().replace("pub fn diverged() {}\n", "");
        let violations = check_transition_surface(&[crate::size::LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: format!("crates/{TRANSITION_SURFACE_PATH}"),
            contents: thinned,
        }]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "transition-surface");
        assert!(
            violations[0].detail.contains("diverged"),
            "{}",
            violations[0].detail
        );
    }

    #[test]
    fn a_workspace_with_no_transition_module_fails_closed() {
        let violations = check_transition_surface(&kernel_source("pub fn nothing() {}\n"));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "transition-surface");
        assert!(
            violations[0].detail.contains("checking nothing"),
            "{}",
            violations[0].detail
        );
    }

    fn storage_source(extra: &str) -> Vec<crate::size::LayerSource> {
        vec![crate::size::LayerSource {
            crate_name: "waymaker-flash".to_owned(),
            path: format!("crates/{STORAGE_CONTRACT_PATH}"),
            contents: format!("{}{extra}", tests_support::clean_storage_contract()),
        }]
    }

    #[test]
    fn the_pinned_storage_contract_passes() {
        assert!(check_storage_contract(&storage_source("")).is_empty());
    }

    #[test]
    fn a_host_convenience_on_the_storage_contract_is_rejected_by_name() {
        // The shape the rule exists for. Design document §05: a host adapter "must not
        // expand the firmware traits to accommodate host conveniences". None of these
        // breaks a layering rule, needs a dependency, or fails any other gate.
        for convenience in [
            "pub fn read_all(&mut self) -> Vec<u8> { Vec::new() }\n",
            "pub fn flush(&mut self) {}\n",
            "pub fn write_at(&mut self, offset: u32) {}\n",
        ] {
            let violations = check_storage_contract(&storage_source(convenience));
            assert_eq!(violations.len(), 1, "{convenience}");
            assert_eq!(violations[0].rule, "storage-contract");
            assert_eq!(violations[0].subject, "waymaker-flash");
        }
    }

    #[test]
    fn a_storage_operation_that_disappeared_is_reported_too() {
        let thinned = tests_support::clean_storage_contract().replace("pub fn barrier() {}\n", "");
        let violations = check_storage_contract(&[crate::size::LayerSource {
            crate_name: "waymaker-flash".to_owned(),
            path: format!("crates/{STORAGE_CONTRACT_PATH}"),
            contents: thinned,
        }]);
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].detail.contains("barrier"),
            "{}",
            violations[0].detail
        );
    }

    #[test]
    fn a_workspace_with_no_storage_module_fails_closed() {
        let violations = check_storage_contract(&kernel_source("pub fn nothing() {}\n"));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "storage-contract");
        assert_eq!(violations[0].subject, "waymaker-flash");
        assert!(
            violations[0].detail.contains("checking nothing"),
            "{}",
            violations[0].detail
        );
    }

    #[test]
    fn no_surface_pin_reads_another_pin_s_module() {
        // The hazard of one shared body behind three rule ids: a pin pointed at the wrong
        // file would report another module's functions and look, from a green build,
        // exactly like a pin that is working. Each rule is handed only the others' sources,
        // and each has to fail closed rather than find something it can compare.
        let replay_only = vec![crate::size::LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: format!("crates/{REPLAY_SURFACE_PATH}"),
            contents: tests_support::clean_replay_surface(),
        }];
        let transition_only = transition_source("");

        assert!(check_replay_cursor_surface(&replay_only).is_empty());
        assert!(check_transition_surface(&transition_only).is_empty());

        let missing_transition = check_transition_surface(&replay_only);
        assert_eq!(missing_transition.len(), 1);
        assert_eq!(missing_transition[0].rule, "transition-surface");
        assert!(
            missing_transition[0].detail.contains("checking nothing"),
            "{}",
            missing_transition[0].detail
        );
        let missing_replay = check_replay_cursor_surface(&transition_only);
        assert_eq!(missing_replay.len(), 1);
        assert_eq!(missing_replay[0].rule, "replay-cursor-surface");
        assert!(
            missing_replay[0].detail.contains("checking nothing"),
            "{}",
            missing_replay[0].detail
        );

        // And the third pin, which reads a different crate's module and must not be
        // satisfied by either of the kernel's.
        let storage_only = storage_source("");
        assert!(check_storage_contract(&storage_only).is_empty());
        for handed in [&replay_only, &transition_only] {
            let missing_storage = check_storage_contract(handed);
            assert_eq!(missing_storage.len(), 1);
            assert_eq!(missing_storage[0].rule, "storage-contract");
            assert_eq!(missing_storage[0].subject, "waymaker-flash");
            assert!(
                missing_storage[0].detail.contains("checking nothing"),
                "{}",
                missing_storage[0].detail
            );
        }
        for other in [
            check_replay_cursor_surface(&storage_only),
            check_transition_surface(&storage_only),
        ] {
            assert_eq!(other.len(), 1);
            assert!(other[0].detail.contains("checking nothing"), "{other:?}");
        }

        // And the fourth, which reads a second module of the same crate as the third — the
        // pair most likely to be satisfied by each other if a path were ever wrong.
        let recovery_only = recovery_source("");
        assert!(check_recovery_surface(&recovery_only).is_empty());
        for handed in [&replay_only, &transition_only, &storage_only] {
            let missing_recovery = check_recovery_surface(handed);
            assert_eq!(missing_recovery.len(), 1);
            assert_eq!(missing_recovery[0].rule, "recovery-surface");
            assert_eq!(missing_recovery[0].subject, "waymaker-flash");
            assert!(
                missing_recovery[0].detail.contains("checking nothing"),
                "{}",
                missing_recovery[0].detail
            );
        }
        let missing_storage = check_storage_contract(&recovery_only);
        assert_eq!(missing_storage.len(), 1);
        assert!(
            missing_storage[0].detail.contains("checking nothing"),
            "{}",
            missing_storage[0].detail
        );
    }

    fn recovery_source(extra: &str) -> Vec<crate::size::LayerSource> {
        vec![crate::size::LayerSource {
            crate_name: "waymaker-flash".to_owned(),
            path: format!("crates/{RECOVERY_SURFACE_PATH}"),
            contents: format!("{}{extra}", tests_support::clean_recovery_surface()),
        }]
    }

    #[test]
    fn the_pinned_recovery_surface_passes() {
        assert!(check_recovery_surface(&recovery_source("")).is_empty());
    }

    #[test]
    fn a_seek_or_a_second_route_to_an_offset_is_rejected_by_name() {
        // The four shapes the pin exists for. The first three turn a forward scan whose RAM
        // is one page into one that seeks or indexes, which is what §02 decision 2 rules
        // out. The fourth is the dangerous one: an offset that comes back whatever the
        // ending points at cells a program cycle has already cleared, and on NOR a bank
        // written there never boots again — `waymaker-fault`'s sweep is what demonstrates
        // that, and this is what stops the door being reopened.
        for door in [
            "pub fn seek(&mut self, offset: u32) {}
",
            "pub fn resume_at(&mut self, offset: u32) {}
",
            "pub fn read_all(&mut self) -> Vec<u8> { Vec::new() }
",
            "pub fn stopping_offset(&self) -> u32 { 0 }
",
        ] {
            let violations = check_recovery_surface(&recovery_source(door));
            assert_eq!(violations.len(), 1, "{door}");
            assert_eq!(violations[0].rule, "recovery-surface");
            assert_eq!(violations[0].subject, "waymaker-flash");
        }
    }

    #[test]
    fn a_recovery_function_that_disappeared_is_reported_too() {
        // The direction that matters more: `append_offset` gone means the module was
        // renamed or rewritten and the pin has stopped checking anything.
        let thinned =
            tests_support::clean_recovery_surface().replace("pub fn append_offset() {}\n", "");
        let violations = check_recovery_surface(&[crate::size::LayerSource {
            crate_name: "waymaker-flash".to_owned(),
            path: format!("crates/{RECOVERY_SURFACE_PATH}"),
            contents: thinned,
        }]);
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].detail.contains("append_offset"),
            "{}",
            violations[0].detail
        );
    }

    #[test]
    fn a_workspace_with_no_recovery_module_fails_closed() {
        let violations = check_recovery_surface(&kernel_source("pub fn nothing() {}\n"));
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "recovery-surface");
        assert_eq!(violations[0].subject, "waymaker-flash");
        assert!(
            violations[0].detail.contains("checking nothing"),
            "{}",
            violations[0].detail
        );
    }

    #[test]
    fn a_windows_path_separator_still_finds_the_recovery_module() {
        let violations = check_recovery_surface(&[crate::size::LayerSource {
            crate_name: "waymaker-flash".to_owned(),
            path: RECOVERY_SURFACE_PATH.replace('/', "\\"),
            contents: tests_support::clean_recovery_surface(),
        }]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_windows_path_separator_still_finds_the_transition_module() {
        let violations = check_transition_surface(&[crate::size::LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: "crates\\waymaker-core\\src\\transition.rs".to_owned(),
            contents: tests_support::clean_transition_surface(),
        }]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_windows_path_separator_still_finds_the_storage_module() {
        let violations = check_storage_contract(&[crate::size::LayerSource {
            crate_name: "waymaker-flash".to_owned(),
            path: STORAGE_CONTRACT_PATH.replace('/', "\\"),
            contents: tests_support::clean_storage_contract(),
        }]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_test_support_crate_must_forbid_unsafe_code_but_need_not_be_no_std() {
        // The exemption a test-support crate has is narrow and specific: it is host code,
        // so `#![no_std]` would be wrong for it. `#![forbid(unsafe_code)]` is not — a
        // harness the layers are tested against is the last place an unreviewed `unsafe`
        // block should be able to appear.
        let clean = sources("waymaker-fault", "//! Docs.\n#![forbid(unsafe_code)]\n");
        assert!(check_crate_attributes(&clean).is_empty());

        let unforbidden = check_crate_attributes(&sources("waymaker-fault", "//! Docs.\n"));
        assert_eq!(unforbidden.len(), 1);
        assert_eq!(unforbidden[0].rule, "crate-attributes");
        assert_eq!(unforbidden[0].subject, "waymaker-fault");
        assert!(
            unforbidden[0].detail.contains("forbid(unsafe_code)"),
            "{}",
            unforbidden[0].detail
        );

        let allowed = check_crate_attributes(&sources(
            "waymaker-fault",
            "//! Docs.\n#![forbid(unsafe_code)]\n#![allow(unsafe_code)]\n",
        ));
        assert!(
            allowed
                .iter()
                .any(|violation| violation.detail.contains("allows unsafe code")),
            "{allowed:?}"
        );
    }

    #[test]
    fn a_windows_path_separator_still_finds_the_replay_module() {
        let violations = check_replay_cursor_surface(&[crate::size::LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: "crates\\waymaker-core\\src\\replay.rs".to_owned(),
            contents: tests_support::clean_replay_surface(),
        }]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    /// One kernel source file, for the encoding rule.
    fn kernel_source(contents: &str) -> Vec<crate::size::LayerSource> {
        vec![crate::size::LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: "crates/waymaker-core/src/record.rs".to_owned(),
            contents: contents.to_owned(),
        }]
    }

    #[test]
    fn a_kernel_that_reads_no_bytes_passes_the_encoding_rule() {
        // What the kernel actually looks like: numeric kinds, borrowed slices, and not one
        // conversion between the two.
        let clean = kernel_source(
            "pub struct RecordKind(pub u8);
             pub enum RecordRef<'a> { RunCompleted { result: &'a [u8] } }
",
        );
        assert!(check_kernel_owns_no_encoding(&clean).is_empty());
    }

    #[test]
    fn every_endianness_conversion_in_the_kernel_is_reported() {
        // All six, individually: a rule that listed five would let the sixth through, and
        // `to_ne_bytes` is the one that is not even a wire format.
        for (construct, _) in KERNEL_FORBIDDEN_CONSTRUCTS {
            let source = kernel_source(&format!(
                "fn read(bytes: [u8; 4]) -> u32 {{ u32::{construct}(bytes) }}
"
            ));
            let violations = check_kernel_owns_no_encoding(&source);
            assert_eq!(violations.len(), 1, "{construct}");
            assert_eq!(violations[0].rule, "kernel-owns-no-encoding");
            assert!(violations[0].detail.contains(construct), "{construct}");
        }
    }

    #[test]
    fn prose_about_a_construct_is_not_the_construct() {
        // `waymaker-core` explains at length what it does *not* do, and its record module
        // names `from_le_bytes` in a comment saying the kernel never calls it. A scan that
        // read comments would report the crate for documenting its own rule.
        let commented = kernel_source(
            "// Nothing here calls from_le_bytes: the wire format is one layer up.
             pub struct RecordKind(pub u8); // not to_le_bytes either
",
        );
        assert!(check_kernel_owns_no_encoding(&commented).is_empty());
    }

    #[test]
    fn a_byte_decoding_trait_impl_in_the_kernel_is_reported() {
        // The cheapest way in: no dependency, no `pub`, and `size-probe-reach` already
        // credits `try_from` from the probe's `usize::try_from`.
        for marker in KERNEL_FORBIDDEN_IMPL_MARKERS {
            let source = kernel_source(&format!(
                "impl<'a> {marker} for RecordRef<'a> {{
    type Error = ();
}}
"
            ));
            let violations = check_kernel_owns_no_encoding(&source);
            assert_eq!(violations.len(), 1, "{marker}");
            assert!(violations[0].detail.contains(marker), "{marker}");
        }

        // The lifetime-erased form is what the scan matches, so the elided spelling is
        // caught too.
        let elided = kernel_source(
            "impl TryFrom<&[u8]> for RecordKind {
    type Error = ();
}
",
        );
        assert_eq!(check_kernel_owns_no_encoding(&elided).len(), 1);
    }

    #[test]
    fn a_qualified_trait_path_is_still_the_trait() {
        // `impl core::convert::TryFrom<&[u8]> for RecordRef<'_>` is the ordinary fully
        // qualified spelling of the exact thing this rule rejects. A decoder written that
        // way need contain none of the endianness names above — it only has to wrap the
        // borrowed slice — so a scan that compared the trait position literally would let
        // the whole rule through.
        for qualified in [
            "impl core::convert::TryFrom<&[u8]> for RecordRef<'_> {",
            "impl ::core::convert::TryFrom<&[u8]> for RecordRef<'_> {",
            "impl std::convert::From<&[u8]> for RecordKind {",
            "impl convert::From<&[u8]> for RecordKind {",
        ] {
            assert_eq!(
                check_kernel_owns_no_encoding(&kernel_source(qualified)).len(),
                1,
                "{qualified}"
            );
        }
    }

    #[test]
    fn a_header_rustfmt_split_across_lines_is_still_one_header() {
        // `rustfmt` breaks a long header, and neither line then carries both `impl` and the
        // trait. Ordinary formatting must not be a way past the rule.
        let split = kernel_source(
            "impl<'a>\n    TryFrom<&'a [u8]>\n    for RecordRef<'a>\n{\n    type Error = ();\n}\n",
        );
        assert_eq!(check_kernel_owns_no_encoding(&split).len(), 1);

        let split_and_qualified = kernel_source(
            "impl<'a>\n    core::convert::TryFrom<&'a [u8]>\n    for RecordRef<'a>\n{\n}\n",
        );
        assert_eq!(check_kernel_owns_no_encoding(&split_and_qualified).len(), 1);
    }

    #[test]
    fn a_block_comment_about_a_construct_is_not_the_construct() {
        // The same rule as for `//`, and the same reason: a rule that failed a build over a
        // migration note is a rule somebody writes an `allow` next to.
        let noted = kernel_source(
            "/* the old code used u32::from_le_bytes here */\npub struct RecordKind(pub u8);\n",
        );
        assert!(check_kernel_owns_no_encoding(&noted).is_empty());

        let multi_line = kernel_source(
            "/*\n * A decoder would need to_le_bytes, and lives one layer up.\n */\npub struct K(pub u8);\n",
        );
        assert!(check_kernel_owns_no_encoding(&multi_line).is_empty());

        // A `//` inside a block comment does not end it, and a `/*` inside a line comment
        // opens nothing — so the two passes have to happen in that order.
        let nested = kernel_source("/* // from_le_bytes */\npub struct K(pub u8);\n");
        assert!(check_kernel_owns_no_encoding(&nested).is_empty());
        let mentioned = kernel_source("// a /* from_le_bytes */ in a line comment\n");
        assert!(check_kernel_owns_no_encoding(&mentioned).is_empty());

        // And a construct in real code beside a comment is still reported.
        let both =
            kernel_source("/* not code */\nfn read(b: [u8; 4]) -> u32 { u32::from_le_bytes(b) }\n");
        assert_eq!(check_kernel_owns_no_encoding(&both).len(), 1);
    }

    #[test]
    fn a_block_opener_inside_a_line_comment_opens_nothing() {
        // The defect this rule had, and the direction that does not announce itself. An
        // unmatched `/*` inside a line comment used to swallow the rest of the file, so
        // real encoding code after it went unreported — a gate switched off by a comment.
        let sneaky = kernel_source(
            "// see for example: /*\nfn read(b: [u8; 4]) -> u32 { u32::from_le_bytes(b) }\n",
        );
        assert_eq!(check_kernel_owns_no_encoding(&sneaky).len(), 1);

        // The same shape one line further down, and with a `//` inside a block comment,
        // which likewise opens nothing.
        let mixed = kernel_source(
            "/* a // inside a block */\n// and a /* inside a line\nfn f(b: [u8; 2]) -> u16 { u16::from_le_bytes(b) }\n",
        );
        assert_eq!(check_kernel_owns_no_encoding(&mixed).len(), 1);
    }

    #[test]
    fn a_construct_inside_a_literal_is_not_a_construct() {
        // The other direction: a diagnostic string that quotes the very thing the rule
        // rejects must not fail a build. The kernel's own documentation quotes both.
        let quoted = kernel_source(
            "pub const HELP: &str = \"impl TryFrom<&[u8]> for RecordRef {\";\npub struct K(pub u8);\n",
        );
        assert!(check_kernel_owns_no_encoding(&quoted).is_empty());

        let named = kernel_source("pub const WHY: &str = \"never from_le_bytes here\";\n");
        assert!(check_kernel_owns_no_encoding(&named).is_empty());

        // Every raw form Rust has, including the `c` prefix a simpler version of the
        // scanner missed: `cr#\"a \" /*\"#` lexed as an ordinary string ending at its inner
        // quote, leaving a `/*` that swallowed the rest of the file.
        for literal in [
            "r\"impl From<&[u8]> for K\"",
            "br\"impl From<&[u8]> for K\"",
            "cr\"impl From<&[u8]> for K\"",
            "cr#\"a \" /* impl From<&[u8]> for K\"#",
            "br##\"a \"# /* from_le_bytes\"##",
        ] {
            let with_literal = kernel_source(&format!(
                "pub const L: &str = {literal};\nfn f(b: [u8; 2]) -> u16 {{ u16::from_le_bytes(b) }}\n"
            ));
            assert_eq!(
                check_kernel_owns_no_encoding(&with_literal).len(),
                1,
                "the call after {literal} must still be found, and the literal must not be a finding"
            );
        }

        // A raw string, which has no escapes and ends only at its matching hashes.
        let raw = kernel_source(
            "pub const R: &str = r#\"impl From<&[u8]> for K { \"quoted\" }\"#;\npub struct K(pub u8);\n",
        );
        assert!(check_kernel_owns_no_encoding(&raw).is_empty());

        // And real code after every one of them is still reported.
        let after = kernel_source(
            "pub const R: &str = r\"impl From<&[u8]> for K\";\nfn f(b: [u8; 2]) -> u16 { u16::from_le_bytes(b) }\n",
        );
        assert_eq!(check_kernel_owns_no_encoding(&after).len(), 1);
    }

    #[test]
    fn a_character_literal_holding_a_quote_does_not_open_a_string() {
        // `'\"'` is ordinary Rust — `matches!(c, '\"')` — and reading its quote as the start
        // of a string swallows the rest of the file, which is the same false negative as
        // the line-comment case.
        let quote_char = kernel_source(
            "fn is_quote(c: char) -> bool { c == '\"' }\nfn f(b: [u8; 2]) -> u16 { u16::from_le_bytes(b) }\n",
        );
        assert_eq!(check_kernel_owns_no_encoding(&quote_char).len(), 1);

        // A lifetime is not a character literal, and an escape always is.
        let lifetime =
            kernel_source("pub enum RecordRef<'a> { RunCompleted { result: &'a [u8] } }\n");
        assert!(check_kernel_owns_no_encoding(&lifetime).is_empty());
        let escaped = kernel_source(
            "fn nl(c: char) -> bool { c == '\\n' }\nfn f(b: [u8; 2]) -> u16 { u16::from_le_bytes(b) }\n",
        );
        assert_eq!(check_kernel_owns_no_encoding(&escaped).len(), 1);
    }

    #[test]
    fn block_comments_nest_the_way_rust_says_they_do() {
        // A scan that stopped at the first `*/` would read the tail of a nested comment as
        // code, and report a construct nobody wrote.
        let nested = kernel_source(
            "/* outer /* inner */ still a comment: from_le_bytes */\npub struct K(pub u8);\n",
        );
        assert!(check_kernel_owns_no_encoding(&nested).is_empty());

        // And the code after the outer close is code again.
        let after = kernel_source(
            "/* outer /* inner */ */\nfn f(b: [u8; 2]) -> u16 { u16::from_le_bytes(b) }\n",
        );
        assert_eq!(check_kernel_owns_no_encoding(&after).len(), 1);
    }

    #[test]
    fn code_only_keeps_the_code_and_the_line_count() {
        // The pass itself, so its behaviour is pinned where it is easiest to read.
        assert_eq!(
            code_only("let x = 1; // note\nlet y = 2;"),
            "let x = 1; \nlet y = 2;"
        );
        assert_eq!(code_only("a /* b */ c"), "a  c");
        assert_eq!(code_only("a /* b\nc */ d"), "a \n d");
        assert_eq!(code_only("let s = \"text\";"), "let s = ;");
        assert_eq!(code_only("let c = 'x';"), "let c = ;");
        assert_eq!(
            code_only("fn f<'a>(x: &'a u8) {}"),
            "fn f<'a>(x: &'a u8) {}"
        );
        // An unterminated block comment does not compile, so nothing after it is code.
        assert_eq!(code_only("code /* and then nothing"), "code ");
    }

    #[test]
    fn an_impl_that_is_not_at_a_token_boundary_is_not_an_impl() {
        // `impl_headers` scans a flattened file, so it has to tell the keyword from a word
        // that ends in it.
        let headers = impl_headers("fn reimpl() {} impl Foo for Bar {}");
        assert_eq!(headers, ["impl Foo for Bar "]);
        assert!(impl_headers("struct NoImplHere;").is_empty());
        // A header ended by `;` rather than `{` — a trait impl cannot be written that way,
        // but the scan must not swallow the rest of the file looking for a brace.
        assert_eq!(
            impl_headers("impl Foo for Bar; fn f() {}"),
            ["impl Foo for Bar"]
        );
    }

    #[test]
    fn a_conversion_outside_the_kernel_is_not_the_kernels_problem() {
        // `waymaker-flash` owns the wire format, so `from_le_bytes` is what it is *for*.
        let adapter = vec![crate::size::LayerSource {
            crate_name: "waymaker-flash".to_owned(),
            path: "crates/waymaker-flash/src/frame.rs".to_owned(),
            contents: "fn read(b: [u8; 2]) -> u16 { u16::from_le_bytes(b) }
"
            .to_owned(),
        }];
        assert!(check_kernel_owns_no_encoding(&adapter).is_empty());
    }

    #[test]
    fn a_mention_of_a_trait_that_is_not_an_impl_header_is_not_reported() {
        // A doc link or a where-clause bound naming the trait is not an implementation of
        // it, and reporting one would train a reader to reach for an allow.
        let mention = kernel_source(
            "pub fn takes<T>(_value: T) where T: Sized {}
             pub struct Holder(pub u8);
",
        );
        assert!(check_kernel_owns_no_encoding(&mention).is_empty());
    }

    #[test]
    fn a_longer_trait_name_does_not_report_the_shorter_one_it_contains() {
        // `TryFrom<&[u8]>` contains `From<&[u8]>`. Reporting one `impl` twice would tell a
        // reader that one line broke two rules, which is the kind of noise that gets a gate
        // switched off.
        let source = kernel_source("impl TryFrom<&[u8]> for RecordKind {}\n");
        assert_eq!(check_kernel_owns_no_encoding(&source).len(), 1);
        assert!(implements_trait("implTryFrom<&[u8]>forT", "TryFrom<&[u8]>"));
        assert!(!implements_trait("implTryFrom<&[u8]>forT", "From<&[u8]>"));
        assert!(implements_trait("implFrom<&[u8]>forT", "From<&[u8]>"));
        // A generic parameter list is skipped by depth, so a nested `>` does not end it.
        assert!(implements_trait(
            "impl<T:Into<u8>>From<&[u8]>forT",
            "From<&[u8]>"
        ));
        assert!(implements_trait(
            "impl<>TryFrom<&[u8]>forT",
            "TryFrom<&[u8]>"
        ));
        // A trait named anywhere but the trait position is not an implementation of it.
        assert!(!implements_trait(
            "implTforUwhereT:From<&[u8]>",
            "From<&[u8]>"
        ));
        assert!(!implements_trait("fnf()->From<&[u8]>", "From<&[u8]>"));
        // An unbalanced header is not credited as an impl of anything.
        assert!(!implements_trait("impl<T", "From<&[u8]>"));
    }

    #[test]
    fn lifetimes_are_erased_without_losing_the_rest_of_the_line() {
        assert_eq!(
            erase_lifetimes("impl<'a> Foo<&'a [u8]> for Bar"),
            "impl<> Foo<& [u8]> for Bar"
        );
        assert_eq!(erase_lifetimes("no lifetimes here"), "no lifetimes here");
        assert_eq!(erase_lifetimes(""), "");
    }
}

/// The two pins that hold issue #16's answers: ADR 0010's checksum and ADR 0011's metadata.
///
/// A module of its own rather than more cases in `mod tests`, because these are the only
/// tests here that read a *named* file in the workspace — `crc.rs` and `record.rs` — and
/// two of them run against the real one. Grouping them keeps that property visible.
#[cfg(test)]
mod deferred_answer_pins {
    use super::*;

    fn layer(path: &str, contents: &str) -> crate::size::LayerSource {
        crate::size::LayerSource {
            crate_name: path.split('/').next().unwrap_or("waymaker-core").to_owned(),
            path: format!("crates/{path}"),
            contents: contents.to_owned(),
        }
    }

    // `effect-protocol`: design document §07's seven steps, pinned.

    fn effect_sources(contents: &str) -> Vec<crate::size::LayerSource> {
        vec![layer(EFFECT_PROTOCOL_PATH, contents)]
    }

    /// The three layer files `timer-capability` reads, with one of them replaced.
    fn timer_sources(path: &str, contents: &str) -> Vec<crate::size::LayerSource> {
        let clean: [(&str, String); 3] = [
            (TIMER_SEMANTICS_PATH, tests_support::clean_timer_module()),
            (CLOCK_CAPABILITY_PATH, tests_support::clean_clock_module()),
            (
                "waymaker-core/src/lib.rs",
                tests_support::clean_kernel_root(),
            ),
        ];
        clean
            .into_iter()
            .map(|(at, body)| layer(at, if at == path { contents } else { body.as_str() }))
            .collect()
    }

    /// The two board clock files the same rule reads, with one of them replaced.
    fn board_clock_sources(path: &str, contents: &str) -> Vec<crate::size::LayerSource> {
        BOARD_CLOCK_MODULES
            .iter()
            .map(|clock| {
                let body = tests_support::clean_board_clock(clock);
                layer(
                    clock.path,
                    if clock.path == path { contents } else { &body },
                )
            })
            .collect()
    }

    /// The clean fixture for `path`, so a mutation test starts from what the pin accepts.
    fn clean_board_clock(path: &str) -> String {
        let Some(clock) = BOARD_CLOCK_MODULES.iter().find(|clock| clock.path == path) else {
            unreachable!("{path} is a board clock module")
        };
        tests_support::clean_board_clock(clock)
    }

    /// Every violation the rule emits when the layer file `path` holds `contents`.
    fn timer_details(path: &str, contents: &str) -> Vec<String> {
        check_timer_capability(&timer_sources(path, contents), &board_clock_sources("", ""))
            .into_iter()
            .map(|violation| violation.detail)
            .collect()
    }

    /// Every violation the rule emits when the board file `path` holds `contents`.
    fn board_clock_details(path: &str, contents: &str) -> Vec<String> {
        check_timer_capability(&timer_sources("", ""), &board_clock_sources(path, contents))
            .into_iter()
            .map(|violation| violation.detail)
            .collect()
    }

    /// The two façade files `ctx-facade` reads, with one of them replaced.
    fn facade_sources(path: &str, contents: &str) -> Vec<crate::size::LayerSource> {
        [
            (CTX_FACADE_PATH, tests_support::clean_ctx_facade()),
            (CTX_JOURNAL_PATH, tests_support::clean_ctx_journal()),
        ]
        .into_iter()
        .map(|(at, body)| layer(at, if at == path { contents } else { body.as_str() }))
        .collect()
    }

    /// Driver modules outside the façade edge, with one of them replaced.
    fn facade_free_driver_sources(path: &str, contents: &str) -> Vec<crate::size::LayerSource> {
        [
            "waymaker-drive/src/drive.rs",
            "waymaker-drive/src/boundary.rs",
        ]
        .into_iter()
        .map(|at| {
            let body = tests_support::clean_facade_free_driver_module();
            layer(at, if at == path { contents } else { &body })
        })
        .collect()
    }

    /// Every violation the rule emits when the façade file `path` holds `contents`.
    fn facade_details(path: &str, contents: &str) -> Vec<String> {
        check_ctx_facade(
            &facade_sources(path, contents),
            &facade_free_driver_sources("", ""),
        )
        .into_iter()
        .map(|violation| violation.detail)
        .collect()
    }

    /// The two wiring files `dispatch-wiring` reads, with one of them replaced.
    fn wiring_sources(path: &str, contents: &str) -> Vec<crate::size::LayerSource> {
        [
            (DISPATCH_PATH, tests_support::clean_dispatch_module()),
            (WIRING_PATH, tests_support::clean_wiring_module()),
        ]
        .into_iter()
        .map(|(at, body)| layer(at, if at == path { contents } else { body.as_str() }))
        .collect()
    }

    /// Every violation the rule emits when the wiring file `path` holds `contents`.
    fn wiring_details(path: &str, contents: &str) -> Vec<String> {
        check_dispatch_wiring(&wiring_sources(path, contents))
            .into_iter()
            .map(|violation| violation.detail)
            .collect()
    }

    #[test]
    fn the_clean_wiring_is_accepted() {
        assert!(check_dispatch_wiring(&wiring_sources("", "")).is_empty());
    }

    #[test]
    fn a_missing_wiring_module_is_reported() {
        // A pin whose file is gone is a pin checking nothing, which is the failure mode
        // every surface rule here fails closed on.
        for path in [DISPATCH_PATH, WIRING_PATH] {
            let sources: Vec<crate::size::LayerSource> = wiring_sources("", "")
                .into_iter()
                .filter(|source| !source.path.ends_with(path))
                .collect();
            let details: Vec<String> = check_dispatch_wiring(&sources)
                .into_iter()
                .map(|violation| violation.detail)
                .collect();
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("checking nothing")),
                "{path}: {details:?}"
            );
        }
    }

    #[test]
    fn a_lookup_by_name_is_reported() {
        // Issue #36's string-addressed activity registry, as the addition it would be.
        let module = format!(
            "{}\npub fn by_name(label: &str) -> usize {{ label.len() }}\n",
            tests_support::clean_wiring_module()
        );
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("`by_name`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_second_trait_method_is_reported() {
        let module = format!(
            "{}\npub fn poll_cancel() {{}}\n",
            tests_support::clean_dispatch_module()
        );
        let details = wiring_details(DISPATCH_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("`poll_cancel`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_pinned_function_that_went_away_is_reported() {
        let details = wiring_details(DISPATCH_PATH, "//! Nothing here.\n");
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("no longer declares")),
            "{details:?}"
        );
    }

    #[test]
    fn a_pinned_function_declared_twice_is_reported() {
        // A pin that is a list of names cannot speak about a name declared twice.
        let module = format!(
            "{}\npub fn name_of() {{}}\n",
            tests_support::clean_wiring_module()
        );
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("more than once")),
            "{details:?}"
        );
    }

    #[test]
    fn a_private_lookup_is_reported() {
        // The defeat `timer-capability`, `effect-protocol` and `ctx-facade` each record: a
        // surface pin counts `pub ` and not `pub(`, and this crate is the one that would
        // call the escape hatch.
        let module = tests_support::clean_wiring_module().replace(
            "impl Table {",
            "impl Table {\n    pub(crate) fn by_name(&self) {}",
        );
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("by_name")),
            "{details:?}"
        );
    }

    #[test]
    fn a_public_row_field_is_reported() {
        // A caller that can write the rows can build a table at run time, which is issue
        // #36's dynamic-loading non-goal reached without adding a function.
        let module =
            tests_support::clean_wiring_module().replace("    rows: u16,", "    pub rows: u16,");
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("public field")),
            "{details:?}"
        );
    }

    #[test]
    fn a_selection_body_that_reads_a_label_is_reported() {
        let module = tests_support::clean_wiring_module().replace(
            "fn row(&self) { let _ = self.kind; }",
            "fn row(&self) { let _ = self.name; }",
        );
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("`row` names `name`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_decoy_selection_body_is_reported() {
        // `braced_body` takes the first match, so a decoy above the real one is what a
        // first-match scan reads. `effect-protocol` records the same defeat.
        let module = format!(
            "//! A decoy.\nfn row() {{ let _ = 1; }}\n{}",
            tests_support::clean_wiring_module()
        );
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("other than exactly once")),
            "{details:?}"
        );
    }

    #[test]
    fn a_free_lookup_at_module_scope_is_reported() {
        // The sharpest of the evasions review found. `public_functions` reads `pub` and
        // `inherent_impl_bodies` reads the two `impl` blocks, so a free
        // `pub(crate) fn by_name` at module scope was in neither reader's field of view —
        // and the label ban did not fire either, because `names_identifier` reads `by_name`
        // as one identifier.
        for declaration in [
            "pub(crate) fn by_name(wanted: &str) -> usize { wanted.len() }",
            "fn label_for(kind: u16) -> &'static str { let _ = kind; \"x\" }",
            "pub(crate) fn register(at: usize) -> bool { at == 0 }",
        ] {
            let module = format!("{}\n{declaration}\n", tests_support::clean_wiring_module());
            let details = wiring_details(WIRING_PATH, &module);
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("read anywhere in the file")),
                "{declaration}: {details:?}"
            );
        }
    }

    #[test]
    fn a_renamed_label_field_is_reported() {
        // The label ban is one identifier deep: a `name` renamed to `label`, with
        // `pub const fn name(&self) -> &'static str { self.label }` left in place, keeps
        // both the surface and the method pins intact and frees a selection body to compare
        // `row.label`.
        let module =
            tests_support::clean_wiring_module().replace("    name: u16,", "    label: u16,");
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares the fields")),
            "{details:?}"
        );
    }

    #[test]
    fn a_decoy_type_above_the_real_one_is_reported() {
        // `braced_body` reads the first declaration, so a decoy above the real one is the
        // body the public-field scan reads. Review of this change put a
        // `mod shim { pub struct Table {} }` above the real `Table`, made both of its fields
        // public, and watched a version without this stay green.
        let module = format!(
            "//! A decoy.\npub struct Table {{}}\n{}",
            tests_support::clean_wiring_module()
        );
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("2 times rather than once")),
            "{details:?}"
        );
    }

    #[test]
    fn a_submodule_is_reported() {
        // `effect-protocol`'s reason: `inherent_impl_bodies` reads `impl` at column zero, so
        // an `impl` inside a submodule of this same file is indented and invisible to it.
        let module = format!(
            "{}\nmod shim {{\n    impl Table {{\n        pub fn by_name() {{}}\n    }}\n}}\n",
            tests_support::clean_wiring_module()
        );
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares a module")),
            "{details:?}"
        );
    }

    #[test]
    fn a_wiring_type_that_lost_its_impl_is_reported() {
        let module =
            tests_support::clean_wiring_module().replace("impl Activity {", "impl Other {");
        let details = wiring_details(WIRING_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("no inherent `impl` for `Activity`")),
            "{details:?}"
        );
    }

    /// Every violation the rule emits when the driver file `path` holds `contents`.
    fn facade_driver_details(path: &str, contents: &str) -> Vec<String> {
        check_ctx_facade(
            &facade_sources("", ""),
            &facade_free_driver_sources(path, contents),
        )
        .into_iter()
        .map(|violation| violation.detail)
        .collect()
    }

    #[test]
    fn the_clean_facade_passes() {
        assert!(facade_details("", "").is_empty());
    }

    #[test]
    fn a_facade_that_reaches_the_device_is_reported() {
        // The sharpest mutation the rule exists for. `waymaker-embassy` may depend on
        // `waymaker-flash`, so nothing else stops the façade programming a record itself.
        let module = format!(
            "{}\npub fn record(storage: &mut impl StableStorage) {{ let _ = storage; }}\n",
            tests_support::clean_ctx_facade()
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("StableStorage")),
            "{details:?}"
        );
    }

    #[test]
    fn a_facade_that_holds_a_static_is_reported() {
        // The other half of the must-not-own cell. A buffer two runs share is the failure.
        for declaration in [
            "static SHARED: [u8; 8] = [0; 8];",
            "pub static SHARED: [u8; 8] = [0; 8];",
            "pub(crate) static SHARED: [u8; 8] = [0; 8];",
        ] {
            let module = format!("{}\n{declaration}\n", tests_support::clean_ctx_facade());
            let details = facade_details(CTX_FACADE_PATH, &module);
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("hidden global state")),
                "{declaration}: {details:?}"
            );
        }
    }

    #[test]
    fn a_static_lifetime_is_not_hidden_state() {
        // Issue #36's activity names are `&'static str`, which is what compile-time metadata
        // is spelled as. The rule is about a `static` *item*, and an item is `static NAME:`.
        for declaration in [
            "pub const fn name(&self) -> &'static str { self.name }",
            "    name: &'static str,",
            "pub fn name_of(&self) -> Option<&'static str> { None }",
        ] {
            let module = format!("{}\n{declaration}\n", tests_support::clean_ctx_facade());
            let details = facade_details(CTX_FACADE_PATH, &module);
            assert!(
                !details
                    .iter()
                    .any(|detail| detail.contains("hidden global state")),
                "{declaration}: {details:?}"
            );
        }
    }

    #[test]
    fn a_static_item_beside_a_static_lifetime_is_still_reported() {
        // The exemption removes the lifetime and nothing else, so a line holding both is
        // still a line declaring an item.
        let module = format!(
            "{}\nstatic SHARED: &'static [u8; 8] = &[0; 8];\n",
            tests_support::clean_ctx_facade()
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("hidden global state")),
            "{details:?}"
        );
    }

    #[test]
    fn a_const_is_not_hidden_state() {
        let module = format!(
            "{}\npub const WIDTH: usize = 8;\n",
            tests_support::clean_ctx_facade()
        );
        assert!(facade_details(CTX_FACADE_PATH, &module).is_empty());
    }

    #[test]
    fn a_fifth_future_is_reported() {
        // The surface pin sets `poll` aside, so this is the only thing that counts them.
        let module = format!(
            "{}\npub struct SignalFuture;\nimpl SignalFuture {{\n    fn poll() {{}}\n}}\n",
            tests_support::clean_ctx_facade()
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("`fn poll`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_facade_function_the_pin_does_not_list_is_reported() {
        let module = format!(
            "{}\npub fn record() {{}}\n",
            tests_support::clean_ctx_facade()
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("`record`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_facade_function_declared_twice_is_reported() {
        // A pin that is a list of names cannot speak about a name used twice.
        let module = format!(
            "{}\npub fn payload() {{}}\n",
            tests_support::clean_ctx_facade()
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("more than once")),
            "{details:?}"
        );
    }

    #[test]
    fn a_missing_facade_function_is_reported() {
        let module = tests_support::clean_ctx_facade().replace("pub fn payload()", "fn payload()");
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("no longer declares `payload`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_fifth_journal_method_is_reported() {
        let module = format!(
            "{}\npub fn append() {{}}\n",
            tests_support::clean_ctx_journal()
        );
        let details = facade_details(CTX_JOURNAL_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("`append`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_decoy_future_above_the_real_one_is_reported() {
        // `braced_body` and every scan below it read the first declaration, so a decoy above
        // the real one is what they would check. `integrity-check` and `effect-protocol`
        // each carry a named test for this; without one, the declaration count is a branch
        // no test reaches.
        let module = format!(
            "//! The façade.\npub struct ActivityFuture;\n{}",
            tests_support::clean_ctx_facade()
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("`pub struct ActivityFuture` 2 times")),
            "{details:?}"
        );
    }

    /// The names `public_functions` collects from one file's text.
    fn counted(contents: &str) -> Vec<String> {
        crate::size::public_functions(&[crate::size::LayerSource {
            crate_name: "waymaker-flash".to_owned(),
            path: "waymaker-flash/src/bank.rs".to_owned(),
            contents: contents.to_owned(),
        }])
        .into_iter()
        .map(|function| function.name)
        .collect()
    }

    #[test]
    fn an_attribute_on_the_same_line_hides_nothing() {
        // Codex round 3. Round 1's finding was about `#[rustfmt::skip] impl X { pub fn y()
        // {} }` — one line — and the fix's own fixture put the attribute on a line of its
        // own, so it never exercised the form the mutation used. Every classifier in the
        // reader tests the *start* of a line, so an attribute in front of the item hid the
        // `impl` and the `pub` alike. rustfmt leaves that line exactly as written.
        for modifier in ["", "const ", "async ", "unsafe "] {
            let inline = format!("#[rustfmt::skip] impl Bank {{ pub {modifier}fn raw() {{}} }}\n");
            assert!(
                counted(&inline).contains(&"raw".to_owned()),
                "same-line attribute hid `pub {modifier}fn`"
            );
        }
        // And an attribute in front of an ordinary declaration, which is the same shape one
        // nesting level down.
        assert!(counted("#[inline] pub fn raw() {}\n").contains(&"raw".to_owned()));
        // Two of them, and one carrying a bracket of its own.
        let stacked = "#[cfg(all(a, b))] #[rustfmt::skip] impl Bank { pub fn raw() {} }\n";
        assert!(counted(stacked).contains(&"raw".to_owned()));
    }

    #[test]
    fn a_bracket_inside_an_attributes_string_is_not_the_end_of_the_attribute() {
        // Codex round 4, and the finding is against round 3's own fix rather than against
        // the reader it replaced: matching brackets counted every `]` as syntax, so
        // `reason = "]"` ended the attribute early and left the classifier standing on
        // `")]` rather than on the item. `#[expect(..)]` needs a reason under this
        // workspace's lints, so the string is the ordinary form and not a contrivance.
        let quoted =
            "#[expect(lint, reason = \"]\")] #[rustfmt::skip] impl Bank { pub fn raw() {} }\n";
        assert!(counted(quoted).contains(&"raw".to_owned()), "{quoted}");
        // An escaped quote inside the reason, so the scan cannot simply toggle on `\"`.
        let escaped = "#[expect(lint, reason = \"a \\\"]\\\" here\")] pub fn raw() {}\n";
        assert!(counted(escaped).contains(&"raw".to_owned()), "{escaped}");
        // And the direction that under-reports rather than over-reports: an attribute the
        // scan cannot finish leaves the item unclassified, never counted as public.
        assert!(!counted("#[expect(lint, reason = \"open").contains(&"raw".to_owned()));
    }

    #[test]
    fn a_same_line_attribute_does_not_hide_an_impl_block_from_the_method_pin() {
        // The reader beside `public_functions` had the same blindness, and it is what
        // `ctx-facade` pins `Ctx`'s methods with — so a `pub(crate)` escape hatch behind a
        // same-line attribute was invisible to both.
        let module = format!(
            "{}\n#[rustfmt::skip] impl {CTX_TYPE} {{ pub(crate) fn commit_raw() {{}} }}\n",
            tests_support::clean_ctx_facade()
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("commit_raw")),
            "{details:?}"
        );
    }

    #[test]
    fn an_inline_public_method_is_counted_whatever_modifier_follows_pub() {
        // Codex round 1. The prefix before ` fn ` ends in the *modifier* rather than in
        // `pub`, so `impl Bank { pub const fn raw() {} }` read as private and nine surface
        // pins and `size-probe-reach` stayed blind to the same one-line escape this change
        // exists to close. Driven against the reader itself: `ctx-facade`'s method pin
        // catches such a line on `Ctx` for a different reason, so a test that went through
        // the rule would pass with the reader still broken.
        for modifier in ["", "const ", "async ", "unsafe ", "extern \"C\" "] {
            let inline = format!("#[rustfmt::skip]\nimpl Bank {{ pub {modifier}fn raw() {{}} }}\n");
            assert!(
                counted(&inline).contains(&"raw".to_owned()),
                "pub {modifier}fn was not counted"
            );
        }
    }

    #[test]
    fn an_inline_pub_crate_method_is_not_counted_as_public() {
        // The two paths must agree on what `pub` means: the non-inline one reads
        // `starts_with("pub ")`, so a `pub(crate)` member is not public there and must not
        // become public here — `size-probe-reach` would then demand a probe call for it.
        let inline = "#[rustfmt::skip]\nimpl Bank { pub(crate) fn raw() {} }\n";
        assert!(!counted(inline).contains(&"raw".to_owned()));
        // And a private member of an inline block is still private.
        let private = "#[rustfmt::skip]\nimpl Bank { fn raw() {} }\n";
        assert!(!counted(private).contains(&"raw".to_owned()));
    }

    #[test]
    fn a_pub_crate_method_on_the_context_is_reported() {
        // The defeat CLAUDE.md records against `timer-capability` and `effect-protocol`,
        // met here: a surface pin counts `pub ` and not `pub(`, and this crate is one crate.
        let module = tests_support::clean_ctx_facade().replace(
            "    fn ending() {}",
            "    fn ending() {}\n    pub(crate) fn commit_raw() {}",
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("commit_raw")),
            "{details:?}"
        );
    }

    #[test]
    fn an_associated_constant_on_the_context_is_reported() {
        // The other half of that defeat: the pin reads `fn`, so a `const` is a value every
        // caller reaches without changing a surface.
        let module = tests_support::clean_ctx_facade().replace(
            "    fn ending() {}",
            "    fn ending() {}\n    pub const AUTHORITY: usize = 1;",
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("AUTHORITY")),
            "{details:?}"
        );
    }

    #[test]
    fn a_static_behind_a_leading_attribute_is_reported() {
        // Reading the start of a line after one `pub` prefix let this through, and it
        // survives `cargo fmt`. Review of this change landed exactly it.
        let module = format!(
            "{}\n#[allow(dead_code)] static SHARED_PAGE: [u8; 8] = [0; 8];\n",
            tests_support::clean_ctx_facade()
        );
        let details = facade_details(CTX_FACADE_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("hidden global state")),
            "{details:?}"
        );
    }

    #[test]
    fn a_macro_in_the_facade_crate_is_reported() {
        // A scanner cannot expand a macro, so it refuses the construct — the answer
        // `effect-protocol` gives to a closure and a short-circuit. Review of this change
        // expanded a tenth public method into `impl Ctx` from a sibling module.
        let module = format!(
            "{}\nmacro_rules! escape_hatch {{ () => {{}} }}\n",
            tests_support::clean_ctx_journal()
        );
        let details = facade_details(CTX_JOURNAL_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("macro_rules")),
            "{details:?}"
        );
    }

    #[test]
    fn the_journal_side_is_held_to_the_same_two_bans() {
        // The vocabulary ban and the `static` ban read every file of the crate, so the
        // second path is exercised rather than assumed.
        for (mutation, expected) in [
            (
                "pub use waymaker_flash::storage::StableStorage as Device;",
                "StableStorage",
            ),
            ("static SHARED: usize = 0;", "hidden global state"),
        ] {
            let module = format!("{}\n{mutation}\n", tests_support::clean_ctx_journal());
            let details = facade_details(CTX_JOURNAL_PATH, &module);
            assert!(
                details.iter().any(|detail| detail.contains(expected)),
                "{mutation}: {details:?}"
            );
        }
    }

    #[test]
    fn a_fifth_future_in_a_sibling_module_is_reported() {
        // The future *count* reads `ctx.rs`; the future *set* reads every file of the
        // crate. Review of this change declared one in `dispatch.rs`, where the count
        // cannot see it.
        let module = format!(
            "{}\npub struct SignalFuture;\nimpl core::future::Future for SignalFuture {{}}\n",
            tests_support::clean_ctx_journal()
        );
        let details = facade_details(CTX_JOURNAL_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("SignalFuture")),
            "{details:?}"
        );
    }

    #[test]
    fn a_fifth_future_behind_a_leading_attribute_is_reported() {
        // Codex round 4. The set scan tested `starts_with("impl")` on the raw line while
        // every classifier beside it stripped attributes first, so the one spelling
        // `cargo fmt` preserves walked past the only reader that looks outside `ctx.rs`.
        for mutation in [
            "#[rustfmt::skip] impl core::future::Future for SignalFuture {}",
            "#[cfg(all(a, b))] #[rustfmt::skip] impl core::future::Future for SignalFuture {}",
            "#[expect(lint, reason = \"]\")] impl core::future::Future for SignalFuture {}",
        ] {
            let module = format!(
                "{}\npub struct SignalFuture;\n{mutation}\n",
                tests_support::clean_ctx_journal()
            );
            let details = facade_details(CTX_JOURNAL_PATH, &module);
            assert!(
                details.iter().any(|detail| detail.contains("SignalFuture")),
                "{mutation}: {details:?}"
            );
        }
    }

    #[test]
    fn the_pinned_futures_are_not_reported_by_the_crate_wide_set() {
        // The clean fixture implements `Future` for all four, so a set pin that could not
        // read the spelling `impl core::future::Future for X` would report every one of
        // them and this test would be the one that noticed.
        assert!(facade_details("", "").is_empty());
    }

    #[test]
    fn a_driver_module_that_routes_through_the_bridge_is_reported() {
        // Naming no crate and reaching the façade anyway. Review of this change added
        // exactly this line to `drive.rs` and watched a ban on the crate name stay green.
        for mutation in [
            "use crate::facade::Bridge;",
            "use crate::Bridge;",
            "use crate::ota::Ota;",
        ] {
            let module = format!("//! A driver module.\n{mutation}\n");
            let details = facade_driver_details("waymaker-drive/src/drive.rs", &module);
            assert!(!details.is_empty(), "{mutation}: {details:?}");
        }
    }

    #[test]
    fn a_facade_module_that_is_gone_is_reported() {
        // Fails closed, for `timer-capability`'s reason: a pin that cannot find its file is
        // a pin that has stopped checking.
        let details: Vec<String> = check_ctx_facade(&[], &[])
            .into_iter()
            .map(|violation| violation.detail)
            .collect();
        assert!(
            details
                .iter()
                .filter(|detail| detail.contains("in the workspace"))
                .count()
                >= 2,
            "{details:?}"
        );
    }

    #[test]
    fn a_driver_module_that_names_the_facade_is_reported() {
        // Issue #35's second "done when": removing the façade must leave the protocol
        // usable, so the edge belongs in the two files that exist to hold it.
        let module = "//! A driver module.\nuse waymaker_embassy::Journal;\n";
        let details = facade_driver_details("waymaker-drive/src/drive.rs", module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("waymaker_embassy")),
            "{details:?}"
        );
    }

    #[test]
    fn a_driver_module_that_only_mentions_the_facade_in_a_comment_is_not_reported() {
        let module = "//! See waymaker_embassy for the façade.\n// waymaker_embassy again.\n";
        assert!(
            facade_driver_details("waymaker-drive/src/drive.rs", module).is_empty(),
            "prose is not a dependency"
        );
    }

    #[test]
    fn the_clean_timer_capability_passes() {
        assert!(
            timer_details(TIMER_SEMANTICS_PATH, &tests_support::clean_timer_module()).is_empty()
        );
    }

    #[test]
    fn a_spec_whose_name_merely_starts_with_the_pinned_one_is_reported() {
        // Codex round 3: `starts_with` accepted `TimerSpec::AtPersistentTimeFallback`, and an
        // associated constant of that name — invisible to a method pin that reads `fn` — can
        // be the boot spec. A prefix is not a name.
        let module = tests_support::clean_clock_module().replace(
            CLOCK_SPEC_CONSTRUCTION,
            "TimerSpec::AtPersistentTimeFallback",
        );
        let details = timer_details(CLOCK_CAPABILITY_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("names a `TimerSpec` other than")),
            "{details:?}"
        );
    }

    #[test]
    fn a_clock_module_naming_no_spec_is_reported() {
        // A pin that matches nothing checks nothing.
        let module = tests_support::clean_clock_module()
            .lines()
            .filter(|line| !line.contains("TimerSpec"))
            .collect::<Vec<&str>>()
            .join("\n");
        let details = timer_details(CLOCK_CAPABILITY_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("names no `TimerSpec`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_re_export_that_renames_another_type_to_a_pinned_name_is_reported() {
        // Codex round 3: `pub use timer::TimerPolicy as TimerSpec;` mentions the identifier,
        // so a check that only asked whether the root named it left the decoy in place. What
        // is compared is the source name.
        let root = "//! A kernel crate root.\npub mod timer;\n\
                    pub use timer::TimerPolicy as TimerSpec;\n\
                    pub use timer::{ClockCapability, Deadline, Timer};\n";
        let details = timer_details("waymaker-core/src/lib.rs", root);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("does not re-export `timer::TimerSpec`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_re_export_reaching_through_a_module_is_not_the_pinned_type() {
        // `pub use timer::compat::TimerSpec` re-exports whatever the rename left in `compat`,
        // which is the decoy rather than the type the member pin read.
        let root = "//! A kernel crate root.\npub mod timer;\n\
                    pub use timer::compat::TimerSpec;\n\
                    pub use timer::{ClockCapability, Deadline, Timer};\n";
        let details = timer_details("waymaker-core/src/lib.rs", root);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("does not re-export `timer::TimerSpec`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_pub_crate_downgrade_on_the_timer_is_reported() {
        // A surface pin counts `pub ` and not `pub(`, and `pub(crate)` is reach enough for
        // rung 0.4's `Ctx`, which lands in this crate.
        let module = tests_support::clean_timer_module().replace(
            "impl Timer {",
            "impl Timer {\n    pub(crate) fn arm_or_downgrade() {}",
        );
        let details = timer_details(TIMER_SEMANTICS_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("arm_or_downgrade")),
            "{details:?}"
        );
    }

    #[test]
    fn a_public_field_on_the_timer_is_reported() {
        let module = tests_support::clean_timer_module()
            .replace("    spec: TimerSpec,", "    pub spec: TimerSpec,");
        let details = timer_details(TIMER_SEMANTICS_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares a public field")),
            "{details:?}"
        );
    }

    #[test]
    fn a_missing_timer_module_fails_closed() {
        let details: Vec<String> = check_timer_capability(&[], &[])
            .into_iter()
            .map(|violation| violation.detail)
            .collect();
        assert!(
            details.iter().any(|detail| detail.contains("timer.rs")),
            "{details:?}"
        );
        assert!(
            details.iter().any(|detail| detail.contains("clock.rs")),
            "{details:?}"
        );
        assert!(
            details.iter().any(|detail| detail.contains("lib.rs")),
            "{details:?}"
        );
        for clock in BOARD_CLOCK_MODULES {
            assert!(
                details.iter().any(|detail| detail.contains(clock.path)),
                "{} is not reported: {details:?}",
                clock.path
            );
        }
    }

    #[test]
    fn the_clean_board_clocks_pass() {
        assert!(board_clock_details(RIG_RTC_PATH, &clean_board_clock(RIG_RTC_PATH)).is_empty());
    }

    #[test]
    fn a_board_clock_that_hands_out_an_unvouched_reading_is_reported() {
        // The mutation this pin exists for: an accessor beside `now` that returns the raw
        // counter. It breaks no layering rule and needs no dependency, and a backup domain
        // that lost power reads zero — which fires every persistent deadline at once.
        let module = format!(
            "{}pub fn counter_unchecked() {{}}\n",
            clean_board_clock(RIG_RTC_PATH)
        );
        let details = board_clock_details(RIG_RTC_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("counter_unchecked")),
            "{details:?}"
        );
    }

    #[test]
    fn a_board_clock_that_could_not_be_found_is_reported() {
        // Fails closed, for the two halves above's reason: a module the pin cannot find is
        // a pin that has stopped checking.
        let details = board_clock_details(RIG_EPOCH_PATH, "//! A module with no surface.\n");
        assert!(
            details.iter().any(|detail| detail.contains("awaiting")),
            "{details:?}"
        );
    }

    #[test]
    fn a_board_clock_naming_the_boot_vocabulary_is_reported() {
        for (forbidden, _) in CLOCK_FORBIDDEN_VOCABULARY {
            let module = format!(
                "{}pub fn extra() {{ let _ = {forbidden}; }}\n",
                clean_board_clock(RIG_RTC_PATH)
            );
            let details = board_clock_details(RIG_RTC_PATH, &module);
            assert!(
                details.iter().any(|detail| detail.contains(forbidden)),
                "{forbidden} is not refused: {details:?}"
            );
        }
    }

    #[test]
    fn a_public_field_on_a_board_clock_driver_is_reported() {
        // A public field is a constructor. `pub registers: R` lets every caller reach
        // `rtc.registers.counter()` and skip the continuity check, adding no function.
        let module =
            clean_board_clock(RIG_RTC_PATH).replace("    registers: u8,", "    pub registers: u8,");
        let details = board_clock_details(RIG_RTC_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares a public field")),
            "{details:?}"
        );
    }

    #[test]
    fn a_pub_crate_accessor_on_a_board_clock_driver_is_reported() {
        // A surface pin counts `pub ` and not `pub(`, and this crate is the one the drivers
        // were written for, so `pub(crate)` is reach enough.
        let module = clean_board_clock(RIG_RTC_PATH).replace(
            "impl Rtc {",
            "impl Rtc {\n    pub(crate) fn counter_unchecked() {}",
        );
        let details = board_clock_details(RIG_RTC_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("counter_unchecked")),
            "{details:?}"
        );
    }

    #[test]
    fn a_driver_with_no_inherent_impl_is_reported() {
        // Fails closed: a method pin with nothing to read is a pin that has stopped checking.
        let module =
            clean_board_clock(RIG_EPOCH_PATH).replace("impl RestoredEpoch {", "impl Other {");
        let details = board_clock_details(RIG_EPOCH_PATH, &module);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares no inherent `impl`")),
            "{details:?}"
        );
    }

    #[test]
    fn an_associated_constant_on_a_board_clock_module_is_reported() {
        // The door every other half of this rule walks past: it adds no function, changes no
        // member, and hands a caller a continuity the hardware never reported. Declared on a
        // *second* type, which is why the scan reads the module rather than one impl body.
        let module = format!(
            "{}impl Continuity {{\n    pub const ASSUME_HELD: Self = Self::Held;\n}}\n",
            clean_board_clock(RIG_RTC_PATH)
        );
        let details = board_clock_details(RIG_RTC_PATH, &module);
        assert!(
            details.iter().any(|detail| detail.contains("ASSUME_HELD")),
            "{details:?}"
        );
    }

    #[test]
    fn a_const_fn_on_a_board_clock_module_is_not_a_constant() {
        // The two real modules declare `pub const fn over` and `pub const fn awaiting`, so a
        // scan that read `const fn` as a constant would fail the workspace it is written for.
        let module = clean_board_clock(RIG_RTC_PATH)
            .replace("    pub fn over() {}", "    pub const fn over() {}");
        assert!(board_clock_details(RIG_RTC_PATH, &module).is_empty());
    }

    #[test]
    fn a_board_clock_naming_a_deadline_policy_is_reported() {
        // A driver reports a reading. A driver that named a `TimerSpec` or a
        // `ClockCapability` would be deciding §02 decision 8 where neither pin above looks.
        for (forbidden, _) in BOARD_CLOCK_FORBIDDEN_VOCABULARY {
            let module = format!(
                "{}pub fn extra() {{ let _ = {forbidden}; }}\n",
                clean_board_clock(RIG_EPOCH_PATH)
            );
            let details = board_clock_details(RIG_EPOCH_PATH, &module);
            assert!(
                details.iter().any(|detail| detail.contains(forbidden)),
                "{forbidden} is not refused: {details:?}"
            );
        }
    }

    /// Every violation the rule emits for `source`, so a test cannot pass on another half's
    /// message.
    fn effect_details(source: &str) -> Vec<String> {
        check_effect_protocol(&effect_sources(source))
            .into_iter()
            .map(|violation| violation.detail)
            .collect()
    }

    #[test]
    fn the_clean_effect_protocol_passes() {
        assert!(effect_details(&tests_support::clean_effect_module()).is_empty());
    }

    #[test]
    fn a_missing_effect_protocol_fails_closed() {
        let details: Vec<String> = check_effect_protocol(&[])
            .into_iter()
            .map(|violation| violation.detail)
            .collect();
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("checking nothing")),
            "{details:?}"
        );
    }

    #[test]
    fn a_public_function_the_effect_protocol_pin_does_not_list_is_reported() {
        let source = tests_support::clean_effect_module() + "pub fn dispatch_now() {}\n";
        let details = effect_details(&source);
        assert!(
            details.iter().any(|detail| detail.contains("dispatch_now")),
            "{details:?}"
        );
    }

    #[test]
    fn a_method_added_at_any_visibility_is_reported() {
        // The hole review found: `public_functions` counts `pub ` and not `pub(`, so a
        // `pub(crate)` forge was invisible to the surface pin — and `waymaker-drive` is the
        // crate that would call it. Both spellings are checked, because only one of them was.
        for visibility in ["pub", "pub(crate)", ""] {
            let source = tests_support::clean_effect_module().replace(
                "    /// The identity step 4 dispatches under.",
                &format!(
                    "    {visibility} const fn forge(id: EffectId) -> Self {{\n\
                     \x20       Self {{ id }}\n    }}\n\n\
                     \x20   /// The identity step 4 dispatches under."
                ),
            );
            let details = effect_details(&source);
            assert!(
                details.iter().any(|detail| detail.contains("rather than")),
                "{visibility}: {details:?}"
            );
        }
    }

    #[test]
    fn a_proof_built_as_a_self_literal_is_reported() {
        // `struct_literals` counts the type's *name*, so a `Self { .. }` inside the type's
        // own `impl` is a construction it cannot see.
        let source = tests_support::clean_effect_module().replace(
            "    pub const fn id(self) -> EffectId {\n        self.id\n    }",
            "    pub const fn id(self) -> EffectId {\n        let _ = Self { id: self.id };\n\
             \x20       self.id\n    }",
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("builds a `Self`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_submodule_in_the_pinned_file_is_reported() {
        // `inherent_impl_bodies` reads `impl` at column zero, so an indented one in a nested
        // module escapes every method pin above.
        let source = tests_support::clean_effect_module()
            + "mod ext {\n    impl super::Dispatchable {\n\
               \x20       pub(crate) fn abandon(self) -> u32 {\n            0\n        }\n    }\n}\n";
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("one flat module")),
            "{details:?}"
        );
    }

    #[test]
    fn a_decoy_declaration_above_the_real_one_is_reported() {
        for opaque in EFFECT_TYPE_METHODS.map(|(name, _)| name) {
            let source = format!(
                "pub struct {opaque} {{\n    _decoy: (),\n}}\n{}",
                tests_support::clean_effect_module()
            );
            let details = effect_details(&source);
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("is declared 2 times")),
                "{opaque}: {details:?}"
            );
        }
    }

    #[test]
    fn a_public_field_on_a_proof_of_durable_intent_is_reported() {
        // A public field is a constructor, whichever of the three carries it.
        for (opaque, first) in [
            ("DurableIntent", "    id: EffectId,"),
            ("Dispatchable", "    intent: DurableIntent,"),
            ("Effect", "    run: RunId,"),
        ] {
            let source = tests_support::clean_effect_module().replacen(
                first,
                &format!("    pub {}", first.trim_start()),
                1,
            );
            let details = effect_details(&source);
            assert!(
                details.iter().any(|detail| detail.contains("public field")),
                "{opaque}: {details:?}"
            );
        }
    }

    #[test]
    fn a_proof_declared_as_a_tuple_struct_is_reported() {
        // `braced_body` looks for the next `{`, and a tuple struct has none — so the field
        // scan would read the *impl* block below it and report on the wrong text.
        let source = tests_support::clean_effect_module().replace(
            "pub struct DurableIntent {\n    id: EffectId,\n}",
            "pub struct DurableIntent(pub EffectId);",
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("is not a braced struct")),
            "{details:?}"
        );
    }

    #[test]
    fn a_missing_opaque_type_is_reported() {
        let source = tests_support::clean_effect_module()
            .replace("pub struct DurableIntent {", "struct DurableIntent {");
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares no `pub struct DurableIntent`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_type_with_no_inherent_impl_is_reported() {
        let source = tests_support::clean_effect_module()
            .replace("impl DurableIntent {", "impl Elsewhere {");
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares no inherent `impl` for `DurableIntent`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_durable_intent_built_outside_a_barrier_is_reported() {
        let source = tests_support::clean_effect_module()
            + "pub fn forge() -> DurableIntent {\n    DurableIntent { id: 2 }\n}\n";
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("from nowhere else")),
            "{details:?}"
        );
    }

    #[test]
    fn a_body_that_stops_building_the_proof_is_reported() {
        let source = tests_support::clean_effect_module().replace(
            "    pub(crate) const fn redelivering(self, seq: EffectSeq) -> Dispatchable<C> {\n\
             \x20       Dispatchable {\n            intent: DurableIntent {\n\
             \x20               id: EffectId { run: self.run, seq },\n            },\n\
             \x20           writer: self.writer,\n        }\n    }",
            "    pub(crate) const fn redelivering(self, seq: EffectSeq) -> Dispatchable<C> {\n\
             \x20       todo!()\n    }",
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("is not built inside `redelivering`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_step_body_that_skips_a_barrier_is_reported() {
        // The exact line each step is on, semicolon included: `commit` ends the chain.
        for (step, line) in [
            ("stage", "            .stage(storage, &record, page)\n"),
            ("payload_barrier", "            .payload_barrier(storage)\n"),
            ("commit", "            .commit(storage);\n"),
        ] {
            let source = tests_support::clean_effect_module().replacen(line, "", 1);
            let details = effect_details(&source);
            assert!(
                details.iter().any(|detail| detail.contains("exactly once")),
                "{step}: {details:?}"
            );
        }
    }

    #[test]
    fn a_step_body_that_seals_before_its_payload_barrier_is_reported() {
        let source = tests_support::clean_effect_module().replacen(
            "            .payload_barrier(storage)\n            .commit(storage);",
            "            .commit(storage)\n            .payload_barrier(storage);",
            1,
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("the order is the guarantee")),
            "{details:?}"
        );
    }

    #[test]
    fn a_local_named_like_a_step_does_not_satisfy_the_pin() {
        // The count and the position disagreed once: `count_tokens(body, "commit")` counted a
        // binding and `str::find` located it, so a body that sealed before it barriered
        // passed. The steps are method calls now, and both halves read one tightened copy.
        let source = tests_support::clean_effect_module().replacen(
            "            .commit(storage);",
            "            .payload_barrier(storage);\n        let commit = 0;\n        let _ = commit;",
            1,
        );
        let details = effect_details(&source);
        assert!(
            details.iter().any(|detail| detail.contains("exactly once")),
            "{details:?}"
        );
    }

    #[test]
    fn a_step_body_that_is_gone_is_reported() {
        let source = tests_support::clean_effect_module()
            .replace("pub fn schedule<S>", "pub fn was_schedule<S>");
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares no `fn schedule`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_decoy_step_body_outside_the_owning_impl_is_ignored() {
        // Review of this change wrote a private free `fn resolve` that took all three steps,
        // above a real `Dispatchable::resolve` that stopped at the payload barrier — and the
        // pin read the decoy. The body is read out of the type's own `impl` blocks now.
        let source = tests_support::clean_effect_module()
            .replacen(
                "            .payload_barrier(storage)\n            .commit(storage);\n        Effect {",
                "            .payload_barrier(storage);\n        Effect {",
                1,
            )
            .replace(
                "/// An effect whose intent is durable.",
                "fn resolve(writer: &mut u32) {\n    writer\n        .stage(0, &0, 0)\n\
                 \x20       .payload_barrier(0)\n        .commit(0);\n}\n\n\
                 /// An effect whose intent is durable.",
            );
        let details = effect_details(&source);
        assert!(
            details.iter().any(|detail| detail.contains("exactly once")),
            "{details:?}"
        );
    }

    #[test]
    fn a_proof_built_before_the_commit_barrier_is_reported() {
        // Codex round 1. The construction and the step order were checked separately, so an
        // early return that built a `Dispatchable` before `.stage(` left both halves green.
        let source = tests_support::clean_effect_module().replacen(
            "        let record = RecordRef::EffectScheduled { seq };",
            "        if false {\n            return Dispatchable {\n\
             \x20               intent: DurableIntent { id: 9 },\n                writer: self.writer,\n\
             \x20           };\n        }\n\
             \x20       let record = RecordRef::EffectScheduled { seq };",
            1,
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("before the barrier that earns it")),
            "{details:?}"
        );
    }

    #[test]
    fn a_step_body_whose_barriers_are_conditional_is_reported() {
        // The other half of the same hole: three calls in the right order, on a branch.
        let source = tests_support::clean_effect_module().replacen(
            "        self.writer\n            .stage(storage, &record, page)\n\
             \x20           .payload_barrier(storage)\n            .commit(storage);",
            "        if guard() {\n            self.writer\n                .stage(storage, &record, page)\n\
             \x20               .payload_barrier(storage)\n                .commit(storage);\n        }",
            1,
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("inside a block")),
            "{details:?}"
        );
    }

    #[test]
    fn a_redelivery_that_writes_a_second_schedule_record_is_reported() {
        let source = tests_support::clean_effect_module().replace(
            "    pub(crate) const fn redelivering(self, seq: EffectSeq) -> Dispatchable<C> {\n",
            "    pub(crate) const fn redelivering(self, seq: EffectSeq) -> Dispatchable<C> {\n\
             \x20       let _ = program();\n",
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("writes a second one")),
            "{details:?}"
        );
    }

    #[test]
    fn a_missing_redelivery_path_is_reported() {
        let source = tests_support::clean_effect_module().replace(
            "pub(crate) const fn redelivering",
            "pub(crate) const fn was_redelivering",
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares no `fn redelivering`")),
            "{details:?}"
        );
    }

    #[test]
    fn a_trait_impl_on_a_proof_type_is_reported() {
        // Codex round 2. The surface pin already caught this — `public_functions` counts a
        // trait method as callable — so this asserts the *direct* refusal, which is what
        // stops the guarantee resting on another pin's side effect.
        for proof in EFFECT_NO_SELF_LITERAL {
            let source = tests_support::clean_effect_module()
                + &format!(
                    "impl From<EffectId> for {proof} {{\n\
                     \x20   fn from(id: EffectId) -> Self {{\n        Self {{ id }}\n    }}\n}}\n"
                );
            let details = effect_details(&source);
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("implements a trait")),
                "{proof}: {details:?}"
            );
        }
    }

    #[test]
    fn a_step_body_whose_barriers_are_inside_a_closure_is_reported() {
        // Codex round 2, the other half. Codex's own example: no braces anywhere in the
        // closure, so brace depth is zero at every one of the three calls.
        let source = tests_support::clean_effect_module().replacen(
            "        self.writer\n            .stage(storage, &record, page)\n\
             \x20           .payload_barrier(storage)\n            .commit(storage);",
            "        let _ = false.then(|| self.writer\n\
             \x20           .stage(storage, &record, page)\n\
             \x20           .payload_barrier(storage)\n            .commit(storage));",
            1,
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("inside a block")),
            "{details:?}"
        );
    }

    #[test]
    fn a_step_body_that_declares_a_closure_is_reported() {
        // And a closure bound to a name sidesteps every depth: the calls are at the body's
        // own nesting, and nothing but the `|` says they are not what the body does.
        let source = tests_support::clean_effect_module().replacen(
            "        self.writer\n            .stage(storage, &record, page)\n\
             \x20           .payload_barrier(storage)\n            .commit(storage);",
            "        let seal = || self.writer\n            .stage(storage, &record, page)\n\
             \x20           .payload_barrier(storage)\n            .commit(storage);\n\
             \x20       let _ = seal;",
            1,
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("declares a closure")),
            "{details:?}"
        );
    }

    #[test]
    fn a_step_body_that_short_circuits_its_barriers_is_reported() {
        // Codex round 3. `false && self.writer.stage(..)?.payload_barrier(..)?.commit(..)?`
        // puts all three calls once, in order, at nesting depth zero, in a right-hand side
        // that never runs. `||` was already refused by the closure ban; `&&` was not.
        let source = tests_support::clean_effect_module().replacen(
            "        self.writer\n            .stage(storage, &record, page)\n\
             \x20           .payload_barrier(storage)\n            .commit(storage);",
            "        let _ = false\n            && self.writer\n\
             \x20               .stage(storage, &record, page)\n\
             \x20               .payload_barrier(storage)\n                .commit(storage);",
            1,
        );
        let details = effect_details(&source);
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("short-circuits")),
            "{details:?}"
        );
    }

    // `effect-scheduled-fields`: ADR 0011's answer, pinned.

    fn record_source(variant_body: &str) -> Vec<crate::size::LayerSource> {
        vec![layer(
            EFFECT_SCHEDULED_PATH,
            &format!(
                "pub enum RecordRef<'a> {{\n    RunStarted {{ input: &'a [u8] }},\n    \
                 EffectScheduled {{{variant_body}}},\n    RunFailed {{ error: &'a [u8] }},\n}}\n"
            ),
        )]
    }

    /// The pinned field set, rendered as a variant body.
    fn pinned_body() -> String {
        EFFECT_SCHEDULED_FIELDS
            .iter()
            .map(|field| format!(" {field}: u32,"))
            .collect::<Vec<String>>()
            .concat()
    }

    #[test]
    fn the_pinned_field_set_is_sorted_and_free_of_duplicates() {
        let mut sorted = EFFECT_SCHEDULED_FIELDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, EFFECT_SCHEDULED_FIELDS);
    }

    #[test]
    fn neither_pin_pins_nothing() {
        // Internal review of PR #58: emptying `INTEGRITY_CHECK_PARAMETERS` left all 415
        // tests green, because every test that reads it iterates it. An empty table is a
        // rule that reports success having compared nothing, which is the failure mode
        // CLAUDE.md names first — "a measurement that did not happen is not a measurement
        // that passed". Asserted here rather than left to the sweeps above, which cannot
        // see it by construction.
        assert!(!INTEGRITY_CHECK_PARAMETERS.is_empty());
        assert!(!EFFECT_SCHEDULED_FIELDS.is_empty());
    }

    #[test]
    fn the_real_record_module_matches_the_pin() {
        // The pin against the workspace it pins. Every other test here builds a fixture; if
        // only those existed, the pin could describe a record that does not exist.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(EFFECT_SCHEDULED_PATH);
        let contents = std::fs::read_to_string(&path).expect("the record module should exist");
        let violations = check_effect_scheduled_fields(&[layer(EFFECT_SCHEDULED_PATH, &contents)]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_variant_declaring_exactly_the_pinned_fields_passes() {
        assert!(
            check_effect_scheduled_fields(&record_source(&pinned_body())).is_empty(),
            "the pinned set must be accepted"
        );
    }

    #[test]
    fn a_field_added_to_the_variant_is_reported() {
        // §16: "every extra field is paid per effect, per record, in flash and in write
        // amplification". That is the whole reason this is pinned.
        let body = format!("{} deadline_ms: u32,", pinned_body());
        let violations = check_effect_scheduled_fields(&record_source(&body));
        assert!(
            violations
                .iter()
                .any(|v| v.rule == "effect-scheduled-fields" && v.detail.contains("deadline_ms")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_field_removed_from_the_variant_is_reported() {
        let Some(first) = EFFECT_SCHEDULED_FIELDS.first() else {
            return;
        };
        let body = pinned_body().replace(&format!(" {first}: u32,"), "");
        let violations = check_effect_scheduled_fields(&record_source(&body));
        assert!(
            violations.iter().any(|v| v.detail.contains(*first)),
            "{violations:?}"
        );
    }

    #[test]
    fn a_missing_record_module_fails_closed() {
        let violations = check_effect_scheduled_fields(&[]);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].rule, "effect-scheduled-fields");
    }

    #[test]
    fn a_record_module_with_no_enum_fails_closed() {
        // The pin checking nothing is the failure every rule here is written to avoid.
        let violations =
            check_effect_scheduled_fields(&[layer(EFFECT_SCHEDULED_PATH, "pub struct Nothing;\n")]);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].detail.contains("RecordRef"), "{violations:?}");
    }

    #[test]
    fn an_enum_without_the_variant_fails_closed() {
        let violations = check_effect_scheduled_fields(&[layer(
            EFFECT_SCHEDULED_PATH,
            "pub enum RecordRef<'a> {\n    RunStarted { input: &'a [u8] },\n}\n",
        )]);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].detail.contains("EffectScheduled"),
            "{violations:?}"
        );
    }

    #[test]
    fn a_field_named_only_in_a_comment_or_a_string_does_not_count() {
        // Every scan in this module reads `code_only`, and this is why: a doc comment on a
        // field is full of colons, and a rule that read them would pin the prose.
        let body = format!(
            "{} /* deadline_ms: u32, */ \n /// deadline_ms: u32,\n",
            pinned_body()
        );
        assert!(
            check_effect_scheduled_fields(&record_source(&body)).is_empty(),
            "a commented-out field is not a field"
        );
    }

    #[test]
    fn a_pattern_match_on_the_variant_is_not_read_as_its_declaration() {
        // `RecordRef::EffectScheduled { .. }` appears in every `match` over the enum. The
        // scan has to find the declaration, not the first mention.
        let source = format!(
            "pub enum RecordRef<'a> {{\n    EffectScheduled {{{}}},\n}}\n\
             impl RecordRef<'_> {{\n    pub const fn kind(&self) -> u8 {{\n        \
             match self {{ Self::EffectScheduled {{ .. }} => 2 }}\n    }}\n}}\n",
            pinned_body()
        );
        assert!(
            check_effect_scheduled_fields(&[layer(EFFECT_SCHEDULED_PATH, &source)]).is_empty(),
            "a match arm is not a declaration"
        );
    }

    #[test]
    fn a_path_qualified_field_type_does_not_add_a_field() {
        // `seq: crate::id::EffectSeq` must not be read as a field called `crate`.
        let body = EFFECT_SCHEDULED_FIELDS
            .iter()
            .map(|field| format!(" {field}: crate::id::Thing,"))
            .collect::<Vec<String>>()
            .concat();
        assert!(
            check_effect_scheduled_fields(&record_source(&body)).is_empty(),
            "a path-qualified type is not a field"
        );
    }

    // `integrity-check`: ADR 0010's answer, pinned.

    #[test]
    fn the_real_integrity_binding_matches_the_pin() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(INTEGRITY_BINDING_PATH);
        let contents = std::fs::read_to_string(&path).expect("the binding module should exist");
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &contents)]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_clean_integrity_binding_passes() {
        assert!(
            check_integrity_binding(&[layer(
                INTEGRITY_BINDING_PATH,
                &tests_support::clean_integrity_binding()
            )])
            .is_empty()
        );
    }

    #[test]
    fn a_missing_integrity_binding_fails_closed() {
        let violations = check_integrity_binding(&[]);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].rule, "integrity-check");
    }

    #[test]
    fn a_widened_seal_is_reported() {
        // The widths are what issue #17 settles alongside the algorithm. A `header_check`
        // that returned a `u32` would be a different frame: two bytes more per record, on
        // media, for the life of the format.
        let source = tests_support::clean_integrity_binding().replace(
            "fn header_check(bytes: &[u8]) -> u16;",
            "fn header_check(bytes: &[u8]) -> u32;",
        );
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("u16")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_rebound_shipped_check_is_reported() {
        // The whole point of the binding: the trait may be implemented by anything, and
        // the type this firmware ships must still be ADR 0010's two functions.
        let source =
            tests_support::clean_integrity_binding().replace("crc32(bytes)", "crc32c(bytes)");
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("crc32")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_binding_without_the_trait_fails_closed() {
        let source =
            tests_support::clean_integrity_binding().replace("trait IntegrityCheck", "trait Other");
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(!violations.is_empty(), "a trait that is gone pins nothing");
    }

    #[test]
    fn a_commented_out_binding_does_not_satisfy_the_pin() {
        // The rule's own documentation says it scans code rather than prose. Without this
        // test that claim was unwitnessed: replacing `code_only` with the raw contents broke
        // none of the tests here, and this module's documentation names both functions
        // repeatedly.
        let mut commented = String::new();
        for line in tests_support::clean_integrity_binding().lines() {
            commented.push_str("// ");
            commented.push_str(line);
            commented.push('\n');
        }
        let violations = check_integrity_binding(&[layer(
            INTEGRITY_BINDING_PATH,
            &format!("//! d\n{commented}"),
        )]);
        assert!(!violations.is_empty(), "prose is not a binding");
    }

    #[test]
    fn a_binding_that_exists_only_under_cfg_test_does_not_satisfy_the_pin() {
        // Firmware does not link a test module, so a binding that lives in one binds
        // nothing that ships.
        let source = format!(
            "//! d\n\n#[cfg(test)]\nmod tests {{\n{}\n}}\n",
            tests_support::clean_integrity_binding()
        );
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(!violations.is_empty(), "a test-only binding ships nothing");
    }

    #[test]
    fn a_decoy_declaration_is_reported_rather_than_shadowing_the_real_one() {
        // Review of this change defeated the first version of this rule exactly this way: a
        // conforming `mod legacy` above the real declaration, and `braced_body`'s
        // first-match-wins made the real one unreadable while it drifted.
        for header in [INTEGRITY_TRAIT, INTEGRITY_SHIPPED_IMPL] {
            let clean = tests_support::clean_integrity_binding();
            let decoy = format!("//! d\nmod legacy {{\n{clean}\n}}\n{clean}");
            let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &decoy)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(header)
                        && violation.detail.contains("2 times")),
                "a second `{header}` went unreported: {violations:?}"
            );
        }
    }

    #[test]
    fn a_path_qualified_delegation_is_reported() {
        // `count_tokens(body, "crc32") == 1` is satisfied by `fast::crc32(bytes)` calling a
        // Castagnoli loop in a sibling module, with `crc.rs` untouched so the other half of
        // the rule passes too. That is the whole failure this pin exists to stop.
        let source =
            tests_support::clean_integrity_binding().replace("crc32(bytes)", "fast::crc32(bytes)");
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("path-qualified")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_shadowed_delegation_is_reported() {
        // Codex, PR #60: `{ let crc32 = |_| 0_u32; crc32(bytes) }` makes exactly one
        // unqualified call to something named `crc32`, and it is a closure returning zero. A
        // name resolves against what is in scope and a scanner does not resolve names, so the
        // body must be the call and nothing else.
        let source = tests_support::clean_integrity_binding().replace(
            "{ crc32(bytes) }",
            "{ let crc32 = |_| 0_u32; crc32(bytes) }",
        );
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("does more than delegate")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_delegation_whose_name_is_not_imported_from_the_checksum_module_is_reported() {
        // The other half of the same hole: with the body pinned to one call, the name still
        // has to be the checksum module's. A `use` pointed somewhere else, or aliased, is
        // where that would change.
        for source in [
            tests_support::clean_integrity_binding()
                .replace(&format!("use {CHECKSUM_MODULE}::"), "use other::"),
            tests_support::clean_integrity_binding()
                .replace("crc16, crc32", "crc16, forged as crc32"),
        ] {
            let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains("does not import")),
                "{violations:?}"
            );
        }
    }

    #[test]
    fn a_cfg_disabled_import_does_not_vouch_for_a_delegation() {
        // Codex, PR #60 round 2: `#[cfg(any())] use crate::crc::{crc16, crc32};` is an import
        // that never exists, and beside a local `fn crc16` it is a textual proof of a
        // resolution that does not happen. Both spellings — the attribute on its own line and
        // on the same line — because a formatter chooses between them.
        for disabled in [
            format!("#[cfg(any())]\nuse {CHECKSUM_MODULE}::"),
            format!("#[cfg(any())] use {CHECKSUM_MODULE}::"),
        ] {
            let source = tests_support::clean_integrity_binding()
                .replace(&format!("use {CHECKSUM_MODULE}::"), &disabled);
            let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains("does not import")),
                "a disabled import vouched for the delegation: {violations:?}"
            );
        }
    }

    #[test]
    fn a_local_definition_shadowing_the_checksum_is_reported() {
        // The other half: the import can be real and unconditional, and a local item of the
        // same name in the same file is still what a call resolves to.
        for seal in SEAL_BINDINGS {
            let source = format!(
                "{}\nconst fn {}(bytes: &[u8]) -> u32 {{ 0 }}\n",
                tests_support::clean_integrity_binding(),
                seal.delegates_to
            );
            let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains("shadows the import")),
                "a local `{}` went unreported: {violations:?}",
                seal.delegates_to
            );
        }
    }

    #[test]
    fn a_seal_whose_answer_is_discarded_is_reported() {
        // Codex, PR #60 round 2, and a genuinely new case rather than the dead reference from
        // round 1: `let _ = C::header_check(&sealed_header);` is a real call to the right
        // function whose answer goes nowhere, with the stored seal computed by something else.
        for seal in SEAL_BINDINGS {
            let source = tests_support::clean_integrity_routing().replace(
                &format!(
                    "let seal_{} = C::{}(bytes).to_le_bytes();",
                    seal.method, seal.method
                ),
                &format!(
                    "let _ = C::{}(bytes); let seal = forged(bytes);",
                    seal.method
                ),
            );
            let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains("throws the answer away")),
                "a discarded {} went unreported: {violations:?}",
                seal.method
            );
        }
    }

    #[test]
    fn a_seal_compared_or_bound_by_name_is_a_route_to_it() {
        // The two shapes the real codec uses, so the pin above cannot pass by refusing
        // everything: `encode_with` turns the seal into bytes, `decode_with` compares it.
        for used in [
            // Every shape the real codec uses, so this rule cannot become "refuse
            // everything": bytes from a method call, a comparison, a `match` scrutinee, a
            // binding the compiler will hold you to, and a tail expression.
            "let seal_header_check = C::header_check(bytes).to_le_bytes();",
            "if C::header_check(bytes) != stored { return None; }",
            "let header = C::header_check(bytes);",
            "match C::header_check(bytes) { _ => 0 };",
            "keep(C::header_check(bytes));",
        ] {
            let source = tests_support::clean_integrity_routing().replace(
                "let seal_header_check = C::header_check(bytes).to_le_bytes();",
                used,
            );
            let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
            assert!(
                !violations
                    .iter()
                    .any(|violation| violation.detail.contains("header_check")),
                "`{used}` was refused: {violations:?}"
            );
        }
    }

    #[test]
    fn an_underscore_binding_is_not_a_route_to_a_seal() {
        // Codex, PR #60 round 3: `let _selected = C::header_check(bytes);` binds to a name,
        // and the leading underscore silences the unused-variable warning that would
        // otherwise be the thing catching it — so the seal can come from anywhere. A plain
        // `let header = ...` is fine for exactly the reason this is not: the compiler holds
        // you to it, and this workspace builds with `-D warnings`.
        for seal in SEAL_BINDINGS {
            let source = tests_support::clean_integrity_routing().replace(
                &format!(
                    "let seal_{} = C::{}(bytes).to_le_bytes();",
                    seal.method, seal.method
                ),
                &format!(
                    "let _selected = C::{}(bytes); let seal = forged(bytes);",
                    seal.method
                ),
            );
            let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains("throws the answer away")),
                "an underscore binding of {} went unreported: {violations:?}",
                seal.method
            );
        }
    }

    #[test]
    fn a_checksum_import_inside_a_nested_module_does_not_vouch_for_a_delegation() {
        // Codex, PR #60 round 3: the shipped `impl` is at file scope and cannot see
        // `mod inner { use crate::crc::{..}; }`, so an import at depth is not the import
        // the call resolves through.
        let source = tests_support::clean_integrity_binding().replace(
            &format!("use {CHECKSUM_MODULE}::"),
            &format!(
                "use crate::forged::{{crc16, crc32}};\nmod inner {{\n    use {CHECKSUM_MODULE}::"
            ),
        ) + "\n}\n";
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("does not import")),
            "a nested import vouched for the delegation: {violations:?}"
        );
    }

    #[test]
    fn a_digest_or_a_scan_step_that_only_mentions_its_callee_is_reported() {
        // The same hole the sealing functions had, in the two checks that still counted
        // tokens: `let _ = crc32; 0` is valid in a `const fn`, and `let _ = decode_with::<C>;`
        // beside `decode(rest)` makes every scan verify with the default check.
        for (function, callee, replacement) in [
            (DIGEST_FUNCTION.0, DIGEST_FUNCTION.1, "{ let _ = crc32; 0 }"),
            (
                SCAN_STEP.0,
                SCAN_STEP.1,
                "{ let _ = decode_with::<C>; decode(rest) }",
            ),
        ] {
            let clean = tests_support::clean_integrity_routing();
            let body = clean
                .lines()
                .find(|line| line.contains(&format!("fn {function}")))
                .unwrap_or_default();
            let broken = body
                .split_once('{')
                .map(|(head, _)| format!("{head}{replacement}"))
                .unwrap_or_default();
            let source = clean.replace(body, &broken);
            let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(function)),
                "`{function}` mentioning `{callee}` went unreported: {violations:?}"
            );
        }
    }

    #[test]
    fn a_mention_of_the_trait_method_is_not_a_route_to_it() {
        // Codex, PR #60: `let _ = C::header_check;` left beside a seal some other helper now
        // computes satisfied a token count, and the checksum-name check does not see a helper
        // called anything else.
        for seal in SEAL_BINDINGS {
            let source = tests_support::clean_integrity_routing().replace(
                &format!("C::{}(bytes)", seal.method),
                &format!("C::{}; forged(bytes)", seal.method),
            );
            let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains("is not a call")),
                "a dead reference to {} passed as a route: {violations:?}",
                seal.method
            );
        }
    }

    #[test]
    fn a_delegation_beside_a_mention_is_reported() {
        // `{ let _ = crc32; forged(bytes) }` also satisfies a token count of one.
        let source = tests_support::clean_integrity_binding()
            .replace("{ crc32(bytes) }", "{ let _ = crc32; forged(bytes) }");
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("forged")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_array_parameter_is_not_reported_as_a_changed_width() {
        // `fn header_check(bytes: &[u8; 10])` is a plausible refactor — the seal covers
        // exactly ten bytes — and a scan that cut the signature at the first `;` truncated
        // it mid-parameter and then blamed the width. A rule whose message names the wrong
        // cause is worse than one that says nothing.
        let source = tests_support::clean_integrity_binding().replace(
            "fn header_check(bytes: &[u8]) -> u16;",
            "fn header_check(bytes: &[u8; 10]) -> u16;",
        );
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_where_clause_is_not_reported_as_a_changed_width() {
        let source = tests_support::clean_integrity_binding().replace(
            "fn frame_check(bytes: &[u8]) -> u32;",
            "fn frame_check(bytes: &[u8]) -> u32 where Self: Sized;",
        );
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    // `integrity-check`, third half: the codec still reaches its seals through the trait.

    #[test]
    fn the_real_codec_routes_through_the_trait() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(INTEGRITY_ROUTING_PATH);
        let contents = std::fs::read_to_string(&path).expect("the codec should exist");
        let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &contents)]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_clean_routing_fixture_passes_and_a_missing_codec_fails_closed() {
        assert!(
            check_integrity_routing(&[layer(
                INTEGRITY_ROUTING_PATH,
                &tests_support::clean_integrity_routing()
            )])
            .is_empty()
        );
        assert_eq!(check_integrity_routing(&[]).len(), 1);
    }

    #[test]
    fn a_codec_that_seals_around_the_trait_is_reported() {
        // The mutation that passed all 34 rules before this half existed: `integrity.rs`
        // perfect, and the codec hard-wired straight back to the checksum module, so the
        // type parameter selects nothing.
        for seal in SEAL_BINDINGS {
            let source = tests_support::clean_integrity_routing().replace(
                &format!("C::{}(bytes)", seal.method),
                &format!("{}(bytes)", seal.delegates_to),
            );
            let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(&format!("C::{}", seal.method))),
                "{} went around the trait unreported: {violations:?}",
                seal.method
            );
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains("goes around the trait")),
                "{violations:?}"
            );
        }
    }

    #[test]
    fn a_scan_that_walks_with_the_default_check_is_reported() {
        let source = tests_support::clean_integrity_routing()
            .replace(&format!("{}::<C>(bytes)", SCAN_STEP.1), "decode(bytes)");
        let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("its caller asked for")),
            "{violations:?}"
        );
    }

    #[test]
    fn the_generic_scan_finds_every_generic_codec_function() {
        // A derived scan that found nothing would pass every test below while checking
        // nothing at all, which is the one direction a gate must not fail in. So the real
        // file's answer is compared against the table, in both directions.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(INTEGRITY_ROUTING_PATH);
        let contents = std::fs::read_to_string(&path).expect("the codec should exist");
        let mut found = integrity_generic_functions(&without_test_modules(&code_only(&contents)));
        found.sort();
        let mut pinned: Vec<String> = SEALING_FUNCTIONS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        pinned.sort();
        assert_eq!(found, pinned);
        assert!(
            !found.is_empty(),
            "a scan that finds nothing checks nothing"
        );
    }

    #[test]
    fn the_generic_scan_sees_the_shapes_that_once_escaped_it() {
        // Each of these reached a seal through the check and passed the whole gate, because
        // the scan read one line at a time. A `where` clause puts the bound on a line with no
        // `fn`; a method in a generic `impl` block has a `fn` on a line with no bound; and a
        // wrapped signature splits the two.
        let shapes = [
            "fn shadow_where<C>(bytes: &[u8]) -> u16\nwhere\n    C: IntegrityCheck,\n{\n    0\n}\n",
            "impl<'a, C: IntegrityCheck> Scan<'a, C> {\n    fn shadow_method(&self) -> u16 {\n        0\n    }\n}\n",
            "fn shadow_wrapped<\n    C: IntegrityCheck,\n>(bytes: &[u8]) -> u16 {\n    0\n}\n",
            // Codex round 4's class, met a third time. An attribute in front of the header
            // left it unjoined, so the block registered no depth and `shadow_behind` — a
            // method whose own signature never names `C` — was found by nothing.
            "#[rustfmt::skip] impl<C: IntegrityCheck> Scan<C> {\n    fn shadow_behind(&self) -> u16 {\n        0\n    }\n}\n",
        ];
        let expected = [
            "shadow_where",
            "shadow_method",
            "shadow_wrapped",
            "shadow_behind",
        ];
        for (shape, name) in shapes.iter().zip(expected) {
            let found = integrity_generic_functions(shape);
            assert!(
                found.iter().any(|declared| declared == name),
                "{name} escaped the scan: {found:?}"
            );
        }
    }

    #[test]
    fn a_generic_codec_function_no_row_pins_is_reported() {
        let source = tests_support::clean_integrity_routing()
            + "pub fn sneak_with<C: IntegrityCheck>() -> u32 { 0 }\n";
        let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("no row of `SEALING_FUNCTIONS`")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_helper_that_names_a_checksum_outside_the_digest_is_reported() {
        // The hole a per-body ban leaves: a new helper beside the pinned bodies, calling the
        // checksum module directly, and called from one of them. It takes no `C` at all, so
        // the scan above cannot see it by construction — this is what does.
        for seal in SEAL_BINDINGS {
            let source = format!(
                "{}fn shadow(bytes: &[u8]) -> u32 {{ {}(bytes) }}\n",
                tests_support::clean_integrity_routing(),
                seal.delegates_to
            );
            let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains("outside")),
                "{}: {violations:?}",
                seal.delegates_to
            );
        }
    }

    #[test]
    fn the_digest_may_still_name_its_own_checksum() {
        // And the exception is a real one, not a rule nobody can satisfy: the clean fixture
        // has `input_digest` calling `crc32`, and it passes.
        let violations = check_integrity_routing(&[layer(
            INTEGRITY_ROUTING_PATH,
            &tests_support::clean_integrity_routing(),
        )]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_decoder_that_reads_a_header_without_verifying_it_is_reported() {
        // `SEALING_FUNCTIONS` says `decode_with` computes no header seal of its own. Without
        // these two pins that would be satisfied by a decoder which skipped the header check
        // altogether and read `payload_len` out of bytes nothing verified — §09's first
        // checksum undone.
        for (owner, callee) in [HEADER_STEP, FRAME_LEN_STEP] {
            let source = tests_support::clean_integrity_routing()
                .replace(&format!("let routed = {callee}::<C>(bytes);"), "");
            let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(callee)),
                "{owner}: {violations:?}"
            );
        }
    }

    // `integrity-check`, fourth half: the recovery reader's route to the codec.

    #[test]
    fn the_real_recovery_reader_routes_through_the_check_its_caller_chose() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(RECOVERY_ROUTING_PATH);
        let contents = std::fs::read_to_string(&path).expect("the reader should exist");
        let violations = check_recovery_routing(&[layer(RECOVERY_ROUTING_PATH, &contents)]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    // -----------------------------------------------------------------------------------
    // `commit-discipline` and the append routing
    // -----------------------------------------------------------------------------------

    /// The real writer, read off disk, so a rule that only the fixture satisfies is caught.
    fn real_append_module() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(APPEND_SURFACE_PATH);
        std::fs::read_to_string(&path).expect("the writer should exist")
    }

    #[test]
    fn the_real_writer_satisfies_the_commit_discipline_it_is_pinned_by() {
        let contents = real_append_module();
        let violations = check_commit_discipline(&[layer(APPEND_SURFACE_PATH, &contents)]);
        assert!(violations.is_empty(), "{violations:?}");
        let routing = check_append_routing(&[layer(APPEND_SURFACE_PATH, &contents)]);
        assert!(routing.is_empty(), "{routing:?}");
    }

    // -----------------------------------------------------------------------------------
    // `swap-discipline` and the swap routing
    // -----------------------------------------------------------------------------------

    /// The real swap, read off disk, so a rule that only the fixture satisfies is caught.
    ///
    /// The fixture is rendered from the pins, so it can only ever prove that the pins agree
    /// with themselves. Review of issue #26 measured what that is worth: with the fixture as
    /// the only witness, `let _ = storage.barrier();` in `Sealable::commit` passed the gate,
    /// every test in the workspace, and both crash sweeps.
    fn real_swap_module() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(SWAP_SURFACE_PATH);
        std::fs::read_to_string(&path).expect("the swap should exist")
    }

    fn swap_violations(contents: &str) -> Vec<Violation> {
        check_swap_discipline(&[layer(SWAP_SURFACE_PATH, contents)])
    }

    #[test]
    fn the_real_swap_satisfies_the_discipline_it_is_pinned_by() {
        let contents = real_swap_module();
        let violations = swap_violations(&contents);
        assert!(violations.is_empty(), "{violations:?}");
        let routing = check_swap_routing(&[layer(SWAP_SURFACE_PATH, &contents)]);
        assert!(routing.is_empty(), "{routing:?}");
    }

    #[test]
    fn a_swallowed_barrier_is_reported_wherever_the_protocol_puts_one() {
        // The four barriers of \u{a7}10, each swallowed in turn. The `commit` one is the
        // regression this test exists for: it is \u{a7}10 step 6 itself, it was pinned by
        // nothing, and it reached a commit on this branch before a review caught it.
        let contents = real_swap_module();
        let swallowed = "let _ = storage.barrier();";
        let occurrences = contents.matches(SWAP_BARRIER_CALL).count();
        assert_eq!(
            occurrences, 4,
            "\u{a7}10 has four barriers; the mutants below assume the file spells all four"
        );
        for index in 0..occurrences {
            let mut mutant = String::new();
            let mut rest = contents.as_str();
            for seen in 0..=index {
                let Some(at) = rest.find(SWAP_BARRIER_CALL) else {
                    unreachable!("the count above says there are {occurrences}")
                };
                let (before, after) = rest.split_at(at);
                mutant.push_str(before);
                if seen == index {
                    mutant.push_str(swallowed);
                }
                rest = after.get(SWAP_BARRIER_CALL.len()..).unwrap_or_default();
                if seen != index {
                    mutant.push_str(SWAP_BARRIER_CALL);
                }
            }
            mutant.push_str(rest);
            assert!(
                !swap_violations(&mutant).is_empty(),
                "barrier {index} could be swallowed with the rule silent"
            );
        }
    }

    #[test]
    fn a_step_that_touches_the_other_bank_is_reported() {
        // Which bank a swap erases or seals is derived from the authority the device booted.
        // A step that reached for the other one would erase the run it is executing, or seal
        // the bank it is replacing.
        // The arguments rather than the whole pinned call: rustfmt breaks these bodies over
        // three lines, so the pin's own spelling is not a substring of the file. The
        // `assert!` below is what keeps that from turning into a mutant nobody applied.
        const MUTATIONS: [(&str, &str, &str); 3] = [
            (
                "prepare",
                ".erase(self.plan.installing.base(), self.plan.installing.bytes())",
                ".erase(self.plan.retiring.base(), self.plan.retiring.bytes())",
            ),
            (
                "reclaim",
                ".erase(self.plan.retiring.base(), self.plan.retiring.bytes())",
                ".erase(self.plan.installing.base(), self.plan.installing.bytes())",
            ),
            (
                "commit",
                ".program(self.plan.installing.seal_offset(), self.seal)",
                ".program(self.plan.retiring.seal_offset(), self.seal)",
            ),
        ];
        for (step, call, instead) in MUTATIONS {
            let contents = real_swap_module();
            assert!(
                contents.contains(call),
                "{step}: the pin should be findable"
            );
            let violations = swap_violations(&contents.replace(call, instead));
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(step)),
                "{step} reached for the other bank with the rule silent: {violations:?}"
            );
        }
    }

    #[test]
    fn a_state_that_grows_a_second_method_is_reported() {
        // Every state \u{a7}10 puts a barrier in front of may do exactly one thing. A
        // `Prepared::commit` skipping the header, or a `Staged::seal_now` skipping the
        // payload barrier, is a step order given back.
        const ANCHOR: &str = "/// Whether `region` lies inside `bank`'s payload.";
        for (state, step) in SWAP_TYPESTATE {
            let contents = real_swap_module();
            assert!(
                contents.contains(ANCHOR),
                "the mutant needs somewhere to go"
            );
            let contents = contents.replace(
                ANCHOR,
                &format!(
                    "impl<C: IntegrityCheck> {state}<'_, C> {{ pub fn shortcut(self) {{}} }}\n\n                     {ANCHOR}"
                ),
            );
            let violations = swap_violations(&contents);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(state)),
                "{state} grew a second method beside `{step}` with the rule silent: \
                 {violations:?}"
            );
        }
    }

    #[test]
    fn a_second_construction_of_a_post_barrier_value_is_reported() {
        // `Sealable` is the only value that can program a generation seal and `Installed` the
        // only one that can erase the retired bank. A second construction of either is a
        // second route to a step \u{a7}10 puts after a barrier.
        for (value, from) in SWAP_CONSTRUCTIONS {
            let contents = real_swap_module();
            assert!(
                contents.contains("/// Whether `region` lies inside `bank`'s payload."),
                "the mutant needs somewhere to go"
            );
            let contents = contents.replace(
                "/// Whether `region` lies inside `bank`'s payload.",
                &format!(
                    "fn back_door(plan: Plan) -> {value} {{ {value} {{ plan }} }}\n\n\
                     /// Whether `region` lies inside `bank`'s payload."
                ),
            );
            let violations = swap_violations(&contents);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(value)),
                "{value} was built outside `{from}` with the rule silent: {violations:?}"
            );
        }
    }

    #[test]
    fn a_swap_that_seals_with_the_shipped_check_is_reported() {
        // The routing half. A device whose two banks were sealed by two algorithms is a
        // device only half of which boots.
        for (_, entries) in SWAP_ROUTING_STEPS {
            for through in *entries {
                let contents = real_swap_module().replace(&format!("{through}::<C>"), through);
                let violations = check_swap_routing(&[layer(SWAP_SURFACE_PATH, &contents)]);
                assert!(
                    violations
                        .iter()
                        .any(|violation| violation.detail.contains(through)),
                    "{through} lost its turbofish with the rule silent: {violations:?}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------------------
    // `capacity-reserve`
    // -----------------------------------------------------------------------------------

    /// The real reserve, read off disk, so a rule that only the fixture satisfies is caught.
    fn real_capacity_module() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(CAPACITY_SURFACE_PATH);
        std::fs::read_to_string(&path).expect("the reserve should exist")
    }

    /// The admission as the real module spells it, for a mutant to remove or move.
    fn real_admission() -> String {
        "        self.reserve\n            .admits(record, self.journal.room())\n            \
         .map_err(ReservedError::Capacity)?;\n"
            .to_owned()
    }

    /// The real module with its admission replaced by `instead`.
    fn capacity_module_with(instead: &str) -> String {
        let contents = real_capacity_module();
        let admission = real_admission();
        assert!(
            contents.contains(&admission),
            "the mutants below rewrite the admission, so they have to be able to find it"
        );
        contents.replace(&admission, instead)
    }

    // `kernel-boundary`: design document §06's boundary, and the driver that decides from it.

    /// The real `transition.rs`, so a mutant is a mutation of what ships.
    fn real_transition_module() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(KERNEL_BOUNDARY_PATH);
        std::fs::read_to_string(&path).expect("the transition module should exist")
    }

    /// The real driver, for the routing half.
    fn real_driver_module() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(DRIVER_PATH);
        std::fs::read_to_string(&path).expect("the driver should exist")
    }

    fn boundary_violations(kernel: &str, driver: &str) -> Vec<Violation> {
        check_kernel_boundary(
            &[layer(KERNEL_BOUNDARY_PATH, kernel)],
            &[layer(DRIVER_PATH, driver)],
        )
    }

    #[test]
    fn the_real_boundary_and_driver_satisfy_the_rule_they_are_pinned_by() {
        let violations = boundary_violations(&real_transition_module(), &real_driver_module());
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_missing_boundary_fails_closed() {
        let violations = check_kernel_boundary(&[], &[layer(DRIVER_PATH, &real_driver_module())]);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("checking nothing"))
        );
    }

    #[test]
    fn a_missing_driver_fails_closed() {
        let violations = check_kernel_boundary(
            &[layer(KERNEL_BOUNDARY_PATH, &real_transition_module())],
            &[],
        );
        assert_eq!(violations.len(), 1, "{violations:?}");
    }

    #[test]
    fn a_variant_added_to_the_boundary_is_rejected() {
        // The shape issue #28's second "done when" forbids: §09's first reserved record kind
        // arriving as a boundary variant rather than as a record.
        let mutant = real_transition_module().replace(
            "    Redeliver {",
            "    TimerFired { id: EffectId },\n    Redeliver {",
        );
        let violations = boundary_violations(&mutant, &real_driver_module());
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("`TimerFired`")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_variant_removed_from_the_boundary_is_rejected() {
        let mutant = real_transition_module().replace("    EndOfHistory,", "");
        let violations = boundary_violations(&mutant, &real_driver_module());
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("no longer declares `EndOfHistory`")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_field_added_to_the_request_is_rejected() {
        let mutant = real_transition_module().replace(
            "    pub input_crc: u32,",
            "    pub input_crc: u32,\n    pub record_kind: u8,",
        );
        let violations = boundary_violations(&mutant, &real_driver_module());
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("`record_kind`")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_decoy_declaration_above_the_real_one_is_rejected() {
        // `braced_body` reads the first declaration, so a second one leaves the pin
        // comparing a type nobody ships. `integrity-check` fails over the same shape.
        let mutant = real_transition_module().replace(
            "pub enum Resolve<'a> {",
            "mod decoy {\n    pub enum Resolve { Replayed, Redeliver }\n}\npub enum Resolve<'a> {",
        );
        let violations = boundary_violations(&mutant, &real_driver_module());
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("2 times, not once")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_boundary_type_renamed_away_is_rejected() {
        let mutant =
            real_transition_module().replace("pub enum Resolve<'a>", "pub enum Resolution<'a>");
        let violations = boundary_violations(&mutant, &real_driver_module());
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("0 times, not once")),
            "{violations:?}"
        );
    }

    #[test]
    fn every_declaration_form_the_boundary_uses_is_read() {
        // Unit, tuple and struct variants, and a lifetime parameter on the header — the
        // three forms `transition.rs` really declares, in one fixture so the hand-rolled
        // scanner is exercised on all of them rather than incidentally.
        let source = "pub enum Next<'a> {\n    Record(RecordRef<'a>),\n    EndOfHistory,\n}\n\
                      pub enum Resolve<'a> {\n    Replayed { id: EffectId, outcome: Outcome<'a> },\n\
                      \x20   Redeliver { id: EffectId },\n}\n";
        assert_eq!(
            variant_names(braced_body(source, "pub enum Next").expect("a body")),
            ["EndOfHistory", "Record"]
        );
        assert_eq!(
            variant_names(braced_body(source, "pub enum Resolve").expect("a body")),
            ["Redeliver", "Replayed"]
        );
    }

    #[test]
    fn a_driver_that_stops_deciding_from_a_row_of_the_table_is_rejected() {
        let mutant = real_driver_module().replace("Intent::Finished", "Something::Else");
        let violations = boundary_violations(&real_transition_module(), &mutant);
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("names no `Intent::Finished`")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_longer_path_ending_in_a_decision_does_not_vouch_for_it() {
        // What a `contains` would have accepted: the arm is gone and a longer path that ends
        // in the same two segments is all that is left.
        let mutant = real_driver_module().replace("Intent::Finished", "SomeIntent::Finished");
        let violations = boundary_violations(&real_transition_module(), &mutant);
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("names no `Intent::Finished`")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_driver_that_could_mint_an_identity_is_rejected() {
        // Issue #30: a redelivered effect keeps the identity its schedule record committed.
        // A driver holding an allocator can hand it a fresh one, which every downstream
        // system reads as a second effect.
        let mutant = format!(
            "{}\nuse waymaker_core::EffectIdAllocator;\n",
            real_driver_module()
        );
        let violations = boundary_violations(&real_transition_module(), &mutant);
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("mint an effect identity")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_driver_that_names_the_record_vocabulary_is_rejected() {
        for mutant in [
            format!("{}\nuse waymaker_core::RecordKind;\n", real_driver_module()),
            // The spellings a `contains("Step::")` ban would have missed.
            format!(
                "{}\nuse waymaker_core::replay::Step as S;\n",
                real_driver_module()
            ),
            format!(
                "{}\nfn f(step: Step) -> Step {{ step }}\n",
                real_driver_module()
            ),
        ] {
            let violations = boundary_violations(&real_transition_module(), &mutant);
            assert!(
                violations
                    .iter()
                    .any(|one| one.detail.contains("record vocabulary")),
                "{violations:?}"
            );
        }
    }

    #[test]
    fn a_decision_named_only_under_cfg_test_discharges_nothing() {
        let mutant = real_driver_module().replace("Intent::Finished", "Something::Else")
            + "\n#[cfg(test)]\nmod t {\n    fn f() { let _ = Intent::Finished; }\n}\n";
        let violations = boundary_violations(&real_transition_module(), &mutant);
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("names no `Intent::Finished`")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_unrelated_identifier_ending_in_step_is_not_the_record_vocabulary() {
        let mutant = format!(
            "{}\nfn f() {{ let _ = BootStep::First; }}\n",
            real_driver_module()
        );
        let violations = boundary_violations(&real_transition_module(), &mutant);
        assert!(violations.is_empty(), "{violations:?}");
    }

    fn capacity_violations(contents: &str) -> Vec<Violation> {
        check_capacity_reserve(&[layer(CAPACITY_SURFACE_PATH, contents)])
    }

    #[test]
    fn the_real_reserve_satisfies_the_rule_it_is_pinned_by() {
        let violations = capacity_violations(&real_capacity_module());
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_missing_reserve_fails_closed() {
        assert_eq!(check_capacity_reserve(&[]).len(), 1);
    }

    #[test]
    fn the_pinned_admission_and_delegation_are_not_empty() {
        // `"anything".find("")` is `Some(0)` and `starts_with("")` is `true`, so an emptied
        // pin would leave every mutant below passing with the gate green. That is the
        // failure `gate-broken` exists for, and this is where it is caught for these two.
        assert!(!CAPACITY_ADMISSION_CALL.trim().is_empty());
        assert!(!CAPACITY_DELEGATION.trim().is_empty());
        assert!(!CAPACITY_GATE.trim().is_empty());
    }

    #[test]
    fn a_gate_that_never_admits_is_reported() {
        let violations = capacity_violations(&capacity_module_with(""));
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("does not open with")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_admission_whose_answer_is_dropped_is_reported() {
        let violations = capacity_violations(&capacity_module_with(
            "        let _ = self.reserve.admits(record, self.journal.room());\n",
        ));
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("does not open with")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_admission_taken_after_the_delegation_is_reported() {
        // The refusal that arrives once the frame body is already on media.
        let moved = format!(
            "        let staged = self.journal.stage(storage, record, page);\n{}        staged\n",
            real_admission()
        );
        let violations = capacity_violations(&capacity_module_with(&moved));
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("does not open with")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_admission_the_body_can_skip_is_reported() {
        // The plausible diff: guard the gate so only some record kinds reach it, and
        // *ordinary* effect scheduling — the one thing \u{a7}10 is about — goes ungated. A rule
        // that only asked whether the admission occurred before the delegation said nothing.
        let guarded = format!(
            "        if matches!(record, RecordRef::RunStarted {{ .. }}) {{\n{}        }}\n",
            real_admission()
        );
        let violations = capacity_violations(&capacity_module_with(&guarded));
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("does not open with")),
            "{violations:?}"
        );
        // And the same admission inside a closure nothing calls.
        let deferred = format!(
            "        let _unused = || {{\n{}        }};\n",
            real_admission()
        );
        let violations = capacity_violations(&capacity_module_with(&deferred));
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("does not open with")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_private_stage_decoy_does_not_stand_in_for_the_real_gate() {
        // The surface half counts only *public* functions, so a private `fn stage` above the
        // real one is invisible to it — and a rule that took the first `fn stage` in the file
        // read the decoy's body and reported nothing. Both halves are needed.
        let gutted = capacity_module_with("");
        let decoy = format!(
            "struct Decoy;\nimpl Decoy {{\n    fn stage(&mut self) {{\n{}    }}\n}}\n{gutted}",
            real_admission()
        );
        let violations = capacity_violations(&decoy);
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("does not open with")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_second_gate_method_named_stage_is_reported() {
        let contents = format!(
            "{}\nimpl {CAPACITY_GATE} {{\n    fn stage(&mut self) {{}}\n}}\n",
            real_capacity_module()
        );
        let violations = capacity_violations(&contents);
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("declares `stage` 2 time(s)")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_ungated_writer_added_to_the_reserve_is_reported() {
        let contents = format!(
            "{}\nimpl {CAPACITY_GATE} {{\n    pub fn stage_unchecked(&mut self) {{}}\n}}\n",
            real_capacity_module()
        );
        let violations = capacity_violations(&contents);
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("`stage_unchecked`")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_reserve_with_no_gate_type_is_reported() {
        let contents = real_capacity_module().replace(
            &format!("impl<C: IntegrityCheck> {CAPACITY_GATE}<C> {{"),
            "impl<C: IntegrityCheck> Elsewhere<C> {",
        );
        let violations = capacity_violations(&contents);
        assert!(
            violations
                .iter()
                .any(|one| one.detail.contains("declares no inherent `impl`")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_pinned_expression_survives_rustfmt_joining_the_line() {
        // `squeezed` collapses runs of whitespace and so survives rustfmt *breaking* a chain
        // but not rustfmt *joining* it. These two pins are compared with whitespace removed
        // instead, so an unrelated rename that shortens the line cannot turn the gate red.
        let joined = capacity_module_with(
            "        self.reserve.admits(record, self.journal.room()).map_err(ReservedError::Capacity)?;\n",
        );
        let violations = capacity_violations(&joined);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_missing_writer_fails_both_of_its_rules_closed() {
        // Two rules, two refusals: the pin cannot compare a surface that is not there, and
        // the routing cannot say the writer seals with the check its caller chose.
        assert_eq!(check_commit_discipline(&[]).len(), 1);
        assert_eq!(check_append_routing(&[]).len(), 1);
    }

    #[test]
    fn a_staged_frame_that_can_program_is_reported() {
        let contents = real_append_module().replace(
            "        self.journal.written = self.journal.written.barriering();",
            "        let _ = storage.program(self.seal_at, self.seal);",
        );
        let violations = check_commit_discipline(&[layer(APPEND_SURFACE_PATH, &contents)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("names `program`")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_second_route_to_the_sealable_frame_is_reported() {
        // Both spellings, because review of this change evaded the first one by qualifying
        // the path: `self::Sealable { .. }` builds the same value the bare name does.
        for spelling in ["Sealable {", "self::Sealable {"] {
            let contents = real_append_module().replace(
                "impl<C: IntegrityCheck> Sealable<'_, '_, C> {",
                &format!(
                    "impl<'journal, 'page, C: IntegrityCheck> Staged<'journal, 'page, C> {{\n\
                     fn assume(self) -> Sealable<'journal, 'page, C> {{ {spelling} \
                     journal: self.journal, seal: self.seal, seal_at: self.seal_at, \
                     stride: self.stride, record: self.record }} }} }}\n\
                     impl<C: IntegrityCheck> Sealable<'_, '_, C> {{"
                ),
            );
            let violations = check_commit_discipline(&[layer(APPEND_SURFACE_PATH, &contents)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains("is constructed")),
                "{spelling}: {violations:?}"
            );
        }
    }

    #[test]
    fn a_second_impl_block_does_not_hide_a_method_from_the_pin() {
        // The rule reads *every* inherent block for the type, not the first: a second
        // `impl Staged` further down the file was invisible until review said so.
        let contents = format!(
            "{}\nimpl<'journal, 'page, C: IntegrityCheck> Staged<'journal, 'page, C> {{\n\
             fn sneak(&self) {{}}\n}}\n",
            real_append_module()
        );
        let violations = check_commit_discipline(&[layer(APPEND_SURFACE_PATH, &contents)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("sneak")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_payload_barrier_that_swallows_its_failure_is_reported() {
        // The mutation review wrote: a barrier whose answer is bound and never propagated
        // satisfies "called once and used", and hands back a value that may program a seal
        // over a frame no barrier ever ordered.
        let contents = real_append_module().replace(
            APPEND_BARRIER_CALL,
            "let ordered = storage.barrier();\n        let _ = ordered.is_ok()",
        );
        let violations = check_commit_discipline(&[layer(APPEND_SURFACE_PATH, &contents)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("does not take the barrier")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_writer_that_seals_with_the_default_check_is_reported() {
        let contents = real_append_module();
        for (step, through) in APPEND_ROUTING_STEPS {
            let stripped = contents.replace(&format!("{through}::<C>"), through);
            let violations = check_append_routing(&[layer(APPEND_SURFACE_PATH, &stripped)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(step)),
                "{step}: {violations:?}"
            );
        }
    }

    #[test]
    fn a_writer_that_computes_a_seal_of_its_own_is_reported() {
        for seal in SEAL_BINDINGS {
            for named in [seal.delegates_to, seal.method] {
                let source = format!(
                    "//! Writer.\nfn stage() {{ frame::encode_with::<C>(r, a, p) }}\n\
                     fn shadow(b: &[u8]) {{ {named}(b); }}\n"
                );
                let violations = check_append_routing(&[layer(APPEND_SURFACE_PATH, &source)]);
                assert!(
                    violations
                        .iter()
                        .any(|violation| violation.detail.contains(named)),
                    "{named}: {violations:?}"
                );
            }
        }
    }

    #[test]
    fn an_impl_header_names_the_type_rather_than_its_last_generic_parameter() {
        // `impl<'a, C: IntegrityCheck> Sealable<'a, C>` is a block for `Sealable`, and the
        // naive reading — the last whitespace-separated word — takes `C>` out of it.
        assert_eq!(
            implemented_type("impl<'journal, 'page, C: IntegrityCheck> Sealable<'journal, C> ")
                .as_deref(),
            Some("Sealable")
        );
        assert_eq!(
            implemented_type("impl Journal ").as_deref(),
            Some("Journal")
        );
        assert_eq!(
            implemented_type("impl<C> Journal<C> ").as_deref(),
            Some("Journal")
        );
        assert_eq!(implemented_type("impl ").as_deref(), None);
        assert_eq!(implemented_type("struct Journal ").as_deref(), None);
    }

    #[test]
    fn a_declaration_and_a_return_type_are_not_constructions() {
        // The three shapes that put a brace where a literal does, and the two that are one.
        assert_eq!(
            struct_literals("pub struct Seal {\n    a: u8,\n}\n", "Seal"),
            0
        );
        assert_eq!(
            struct_literals("impl Seal {\n    fn f() {}\n}\n", "Seal"),
            0
        );
        assert_eq!(
            struct_literals("fn f(self) -> Seal {\n    x\n}\n", "Seal"),
            0
        );
        assert_eq!(struct_literals("fn f() { Seal { a: 1 } }\n", "Seal"), 1);
        assert_eq!(
            struct_literals("fn f() { self::Seal { a: 1 } }\n", "Seal"),
            1
        );
        assert_eq!(struct_literals("fn f() { Sealed { a: 1 } }\n", "Seal"), 0);
    }

    #[test]
    fn a_missing_recovery_reader_fails_closed() {
        assert_eq!(check_recovery_routing(&[]).len(), 1);
    }

    #[test]
    fn a_recovery_that_walks_with_the_default_check_is_reported() {
        // The mutation review demonstrated: both turbofishes dropped, so every recovery
        // verifies with the shipped check whatever its caller chose. It passed all 38 rules
        // and the whole suite before this rule existed.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(RECOVERY_ROUTING_PATH);
        let contents = std::fs::read_to_string(&path).expect("the reader should exist");
        for (step, through) in RECOVERY_ROUTING_STEPS {
            let stripped = contents.replace(&format!("{through}::<C>"), through);
            let violations = check_recovery_routing(&[layer(RECOVERY_ROUTING_PATH, &stripped)]);
            assert!(
                violations
                    .iter()
                    .any(|violation| violation.detail.contains(step)),
                "{step}: {violations:?}"
            );
        }
    }

    #[test]
    fn a_recovery_that_computes_a_seal_of_its_own_is_reported() {
        // Both ways around the type parameter: the checksum module directly, and the shipped
        // implementation's method directly. Neither needs a `C`, so neither is visible to the
        // generic-function scan.
        for seal in SEAL_BINDINGS {
            for named in [seal.delegates_to, seal.method] {
                let source = format!(
                    "//! Reader.\nfn next() {{ frame::decode_with::<C>(b) }}\n                     fn stage() {{ frame::frame_len_of_with::<C>(h) }}\n                     fn shadow(b: &[u8]) {{ {named}(b); }}\n"
                );
                let violations = check_recovery_routing(&[layer(RECOVERY_ROUTING_PATH, &source)]);
                assert!(
                    violations
                        .iter()
                        .any(|violation| violation.detail.contains(named)),
                    "{named}: {violations:?}"
                );
            }
        }
    }

    #[test]
    fn a_digest_that_is_no_longer_the_frames_own_seal_is_reported() {
        let source = tests_support::clean_integrity_routing()
            .replace(&format!("{{ {}(input) }}", DIGEST_FUNCTION.1), "{ 0 }");
        let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("no replay can reproduce")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_binding_without_the_shipped_impl_fails_closed() {
        let source = tests_support::clean_integrity_binding().replace(
            "impl IntegrityCheck for Catalogued",
            "impl IntegrityCheck for Other",
        );
        let violations = check_integrity_binding(&[layer(INTEGRITY_BINDING_PATH, &source)]);
        assert!(!violations.is_empty(), "an impl that is gone pins nothing");
    }

    #[test]
    fn the_real_checksum_module_matches_the_pin() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("crates")
            .join(INTEGRITY_CHECK_PATH);
        let contents = std::fs::read_to_string(&path).expect("the checksum module should exist");
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &contents)]);
        assert!(violations.is_empty(), "{violations:?}");
    }

    #[test]
    fn a_table_free_checksum_module_passes() {
        assert!(
            check_integrity_check(&[layer(
                INTEGRITY_CHECK_PATH,
                &tests_support::clean_checksum_module()
            )])
            .is_empty()
        );
    }

    #[test]
    fn a_missing_checksum_module_fails_closed() {
        let violations = check_integrity_check(&[]);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].rule, "integrity-check");
    }

    #[test]
    fn a_lookup_table_is_reported() {
        // ADR 0010's measured decision: a 256-entry table is 1024 B of rodata against an
        // 8 KiB incremental budget, and a nibble table 64 B. Either is a decision, not an
        // optimisation somebody slips in.
        let source = format!(
            "{}static TABLE: [u32; 256] = [0; 256];\n",
            tests_support::clean_checksum_module()
        );
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
        assert!(
            violations.iter().any(|v| v.detail.contains("TABLE")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_const_lookup_table_is_reported_too() {
        let source = format!(
            "{}const NIBBLE: [u32; 16] = [0; 16];\n",
            tests_support::clean_checksum_module()
        );
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
        assert!(
            violations.iter().any(|v| v.detail.contains("NIBBLE")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_table_in_a_test_module_is_not_a_lookup_table() {
        // `crc.rs` already holds a `const MESSAGE: [u8; 12]` fixture for the bit-flip
        // sweep. A test fixture is not code the firmware links, and a rule that could not
        // tell the difference would be a rule that punishes testing.
        let source = format!(
            "{}#[cfg(test)]\nmod tests {{\n    const MESSAGE: [u8; 12] = [0; 12];\n}}\n",
            tests_support::clean_checksum_module()
        );
        assert!(
            check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]).is_empty(),
            "a test fixture is not a lookup table"
        );
    }

    #[test]
    fn a_restricted_visibility_lookup_table_is_reported() {
        // Codex review, PR #58. `strip_prefix("pub ")` left `pub(crate) const TABLE: [u32; 16]`
        // untouched, so the rule permitted exactly the production lookup table it claims to
        // reject — and `pub(crate)` is this module's own idiom, so it is the spelling a
        // contributor would reach for first.
        for visibility in [
            "pub(crate) ",
            "pub(super) ",
            "pub(in crate::frame) ",
            "pub ",
            "",
        ] {
            let source = format!(
                "{}{visibility}const TABLE: [u32; 16] = [0; 16];\n",
                tests_support::clean_checksum_module()
            );
            let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
            assert!(
                violations.iter().any(|v| v.detail.contains("TABLE")),
                "visibility `{visibility}` evaded the rule: {violations:?}"
            );
        }
    }

    #[test]
    fn a_lookup_table_whose_type_is_on_the_next_line_is_reported() {
        // `rustfmt` wraps a long item exactly this way, so a line-at-a-time scan is a scan
        // a formatter can defeat.
        let source = format!(
            "{}static TABLE:\n    [u32; 256] = [0; 256];\n",
            tests_support::clean_checksum_module()
        );
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
        assert!(
            violations.iter().any(|v| v.detail.contains("TABLE")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_borrowed_lookup_table_is_reported() {
        // `&[u32; 16]` and `&'static [u32]` are lookup tables with a reference in front.
        for declaration in [
            "const TABLE: &[u32; 16] = &[0; 16];",
            "const TABLE: &'static [u32] = &[0; 16];",
            "static mut TABLE: [u32; 16] = [0; 16];",
        ] {
            let source = format!("{}{declaration}\n", tests_support::clean_checksum_module());
            let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
            assert!(
                violations.iter().any(|v| v.detail.contains("TABLE")),
                "`{declaration}` evaded the rule: {violations:?}"
            );
        }
    }

    #[test]
    fn a_const_fn_and_a_const_generic_are_not_lookup_tables() {
        // The real module is all `pub(crate) const fn`. A rule that read those as tables
        // would fail the workspace it is supposed to pass.
        let source = format!(
            "{}pub(crate) const fn f() -> u32 {{ 0 }}\n\
             const _: () = assert!(true);\n\
             fn g<const N: usize>() -> usize {{ N }}\n",
            tests_support::clean_checksum_module()
        );
        assert!(
            check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]).is_empty(),
            "a const fn is not a table"
        );
    }

    #[test]
    fn every_algorithm_parameter_can_actually_be_lost() {
        // Internal review of PR #58. The rule matched each literal as a bare substring, so
        // `0xFFFF` — CRC-16/CCITT-FALSE's initial value — was vouched for by CRC-32's
        // `0xFFFF_FFFF` two functions further down. Changing `let mut crc: u16 = 0xFFFF` to
        // `0x0000` passed the gate. A pin that cannot fail is not a pin, and the old test
        // could not see it because it only exercised `.first()`.
        //
        // Swept over every parameter rather than one, which is the whole lesson.
        for parameter in INTEGRITY_CHECK_PARAMETERS {
            // One occurrence removed, not all of them: for CRC-32's `0xFFFF_FFFF` that is
            // the case Codex raised — change the initial value and the final xor still
            // satisfies a presence check.
            let clean = tests_support::clean_checksum_module();
            let without = clean.replacen(&format!(" {}", parameter.literal), " 0xDEAD_BEEF", 1);
            assert_ne!(
                without, clean,
                "the fixture should carry `{}`",
                parameter.literal
            );
            let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &without)]);
            assert!(
                violations.iter().any(|v| v.detail.contains(parameter.role)),
                "losing one `{}` from `{}` was not reported: {violations:?}",
                parameter.literal,
                parameter.function
            );
        }
    }

    #[test]
    fn a_longer_literal_does_not_vouch_for_a_shorter_one() {
        // The specific shape of the bug above, stated on its own so a future rewrite of the
        // matcher has to keep it: `0xFFFF` must not be found inside `0xFFFF_FFFF`.
        let source = tests_support::clean_checksum_module().replace(" 0xFFFF ", " 0x0000 ");
        assert!(
            source.contains("0xFFFF_FFFF"),
            "the longer literal has to survive, or the test proves nothing"
        );
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("CRC-16/CCITT-FALSE initial value")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_braceless_test_item_does_not_hide_the_rest_of_the_file() {
        // Internal review of PR #58, and the worst kind of hole: fail-open. `#[cfg(test)]`
        // set a flag that only a `{` could clear, so a braceless item — `mod tests;`, or a
        // `#[cfg(test)] use` at the top — swallowed every line after it, lookup table
        // included, and the rule reported success having read almost nothing.
        for braceless in ["#[cfg(test)]\nmod tests;", "#[cfg(test)]\nuse core::fmt;"] {
            let source = format!(
                "{braceless}\n{}static TABLE: [u32; 16] = [0; 16];\n",
                tests_support::clean_checksum_module()
            );
            let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
            assert!(
                violations.iter().any(|v| v.detail.contains("TABLE")),
                "`{braceless}` hid the rest of the file: {violations:?}"
            );
            assert!(
                !violations
                    .iter()
                    .any(|v| v.detail.contains("no longer contains")),
                "`{braceless}` also ate the parameters: {violations:?}"
            );
        }
    }

    #[test]
    fn a_commented_out_test_attribute_does_not_start_a_test_module() {
        // The `//` guard in `without_test_modules`, which no fixture reached. A line
        // *mentioning* `#[cfg(test)]` in a comment is prose, and prose must not be able to
        // switch the scanner off for the rest of the file.
        let source = format!(
            "// a table here would be rejected even under #[cfg(test)]\n\
             {}static TABLE: [u32; 16] = [0; 16];\n",
            tests_support::clean_checksum_module()
        );
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
        assert!(
            violations.iter().any(|v| v.detail.contains("TABLE")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_windows_path_separator_still_finds_both_pinned_modules() {
        // Both new rules fail closed when they cannot find their module, so a lookup that
        // missed on a `\` separator would turn the whole pin into a fail-closed error that
        // says the module is absent — noisy, but about the wrong thing.
        let record = crate::size::LayerSource {
            crate_name: "waymaker-core".to_owned(),
            path: format!("crates\\{}", EFFECT_SCHEDULED_PATH.replace('/', "\\")),
            contents: tests_support::clean_record_module(),
        };
        assert!(check_effect_scheduled_fields(&[record]).is_empty());

        let checksums = crate::size::LayerSource {
            crate_name: "waymaker-flash".to_owned(),
            path: format!("crates\\{}", INTEGRITY_CHECK_PATH.replace('/', "\\")),
            contents: tests_support::clean_checksum_module(),
        };
        assert!(check_integrity_check(&[checksums]).is_empty());
    }

    #[test]
    fn a_local_lookup_table_is_reported_too() {
        // The first version of `array_items` excused a `let`, on the reasoning that "a
        // table has to outlive the call to be a table". Internal review of PR #58 compiled
        // one for this target at `opt-level = "z"` and found it emitted as constant-pool
        // words inside `.text`, plus a stack copy: the same ~64 B ADR 0010 measured and
        // rejected, and small enough that `cargo xtask size` would not notice either. The
        // reasoning was wrong, so the exemption is gone.
        for local in [
            "let table = [0_u32; 16];",
            "let table: [u32; 16] = [0; 16];",
        ] {
            let source = format!(
                "{}pub(crate) fn f() -> u32 {{ {local} table[0] }}\n",
                tests_support::clean_checksum_module()
            );
            let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
            assert!(
                violations.iter().any(|v| v.detail.contains("table")),
                "`{local}` evaded the rule: {violations:?}"
            );
        }
    }

    #[test]
    fn a_table_hidden_behind_a_type_alias_is_reported() {
        // `type Nibbles = [u32; 16]; const NIBBLE: Nibbles = ...` is a lookup table with a
        // name in front of it, and a rule that only reads the item's own type cannot see
        // the array at all.
        let source = format!(
            "{}type Nibbles = [u32; 16];\npub(crate) const NIBBLE: Nibbles = [0; 16];\n",
            tests_support::clean_checksum_module()
        );
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
        assert!(
            violations.iter().any(|v| v.detail.contains("Nibbles")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_brace_inside_a_string_does_not_unbalance_the_test_scan() {
        // The scan ran on raw text, so a `{{` in a format string inside `mod tests` left
        // the brace depth permanently short and everything after the test module was
        // dropped. `crc.rs`'s own `"byte {index} bit {bit}"` happens to balance, which is
        // luck rather than design.
        let source = format!(
            "{}#[cfg(test)]\nmod tests {{\n    fn f() {{ assert!(true, \"a brace {{{{\"); }}\n}}\n\
             static TABLE: [u32; 16] = [0; 16];\n",
            tests_support::clean_checksum_module()
        );
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
        assert!(
            violations.iter().any(|v| v.detail.contains("TABLE")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_test_attribute_inside_a_block_comment_does_not_start_a_test_module() {
        let source = format!(
            "{}/* an example: #[cfg(test)] */\nstatic TABLE: [u32; 16] = [0; 16];\n",
            tests_support::clean_checksum_module()
        );
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
        assert!(
            violations.iter().any(|v| v.detail.contains("TABLE")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_variant_whose_name_extends_the_pinned_one_does_not_hijack_the_pin() {
        // Internal review of PR #58: `split_once("EffectScheduled")` matched
        // `EffectScheduledV1` declared before it, so the pin read the decoy's field list
        // and the real variant grew a fifth field unseen. The same held for
        // `enum RecordRefV2`. This module already settled the convention — `impl_headers`
        // checks a token boundary — and the pin was not following it.
        let decoyed = format!(
            "pub enum RecordRef<'a> {{\n    EffectScheduledV1 {{{}}},\n    \
             EffectScheduled {{{} deadline_ms: u32,}},\n}}\n",
            pinned_body(),
            pinned_body()
        );
        let violations = check_effect_scheduled_fields(&[layer(EFFECT_SCHEDULED_PATH, &decoyed)]);
        assert!(
            violations.iter().any(|v| v.detail.contains("deadline_ms")),
            "a prefix-named decoy hijacked the pin: {violations:?}"
        );

        let decoyed_enum = format!(
            "pub enum RecordRefV2<'a> {{\n    EffectScheduled {{{}}},\n}}\n\
             pub enum RecordRef<'a> {{\n    EffectScheduled {{{} deadline_ms: u32,}},\n}}\n",
            pinned_body(),
            pinned_body()
        );
        let violations =
            check_effect_scheduled_fields(&[layer(EFFECT_SCHEDULED_PATH, &decoyed_enum)]);
        assert!(
            violations.iter().any(|v| v.detail.contains("deadline_ms")),
            "a prefix-named enum hijacked the pin: {violations:?}"
        );
    }

    #[test]
    fn a_field_named_only_in_a_string_literal_does_not_count() {
        // The other half of the comment test: `code_only` erases string literals, so a
        // message that quotes a field name cannot add one.
        let body = format!("{} /* prose */ ", pinned_body());
        let source = format!(
            "pub enum RecordRef<'a> {{\n    EffectScheduled {{{body}}},\n}}\n\
             pub const HELP: &str = \"deadline_ms: u32, priority: u8,\";\n"
        );
        assert!(
            check_effect_scheduled_fields(&[layer(EFFECT_SCHEDULED_PATH, &source)]).is_empty(),
            "a field name inside a string literal is not a field"
        );
    }

    #[test]
    fn a_generic_field_type_does_not_add_a_field() {
        // `Option<Thing>` and a function-pointer type both put angle brackets and arrows in
        // a field list; neither declares a field.
        let body = EFFECT_SCHEDULED_FIELDS
            .iter()
            .map(|field| format!(" {field}: Option<Thing>,"))
            .collect::<Vec<String>>()
            .concat();
        assert!(
            check_effect_scheduled_fields(&record_source(&body)).is_empty(),
            "a generic type is not a field"
        );
    }

    #[test]
    fn a_parameter_named_only_in_a_comment_does_not_count() {
        let Some(parameter) = INTEGRITY_CHECK_PARAMETERS.first() else {
            return;
        };
        let source = tests_support::clean_checksum_module().replace(
            &format!(" {}", parameter.literal),
            &format!(" /* {} */ 0xDEAD_BEEF", parameter.literal),
        );
        assert!(
            !check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]).is_empty(),
            "a polynomial in a comment is not a polynomial"
        );
    }

    #[test]
    fn a_parameter_outside_its_own_function_does_not_count() {
        // Codex, PR #58: the pin was a presence check over the whole file, so CRC-32's
        // initial value vouched for its own final xor, and any unrelated constant could
        // keep a literal alive after its real use changed.
        let source = format!(
            "{}\npub(crate) const LEFTOVER: u32 = 0xEDB8_8320;\n",
            tests_support::clean_checksum_module().replace(" 0xEDB8_8320", " 0xDEAD_BEEF")
        );
        let violations = check_integrity_check(&[layer(INTEGRITY_CHECK_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("reflected polynomial")),
            "a literal parked outside the function satisfied the pin: {violations:?}"
        );
    }

    #[test]
    fn a_missing_checksum_function_is_reported() {
        // The fail-closed direction: no `fn crc32` means the parameters are pinned against
        // nothing, and that has to be loud rather than green.
        let violations = check_integrity_check(&[layer(
            INTEGRITY_CHECK_PATH,
            "//! Nothing here.\npub(crate) const fn crc16(b: &[u8]) -> u16 { 0x1021 0xFFFF }\n",
        )]);
        assert!(
            violations.iter().any(|v| v.detail.contains("fn crc32")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_test_fixture_in_an_out_of_line_test_module_is_not_a_lookup_table() {
        // Codex, PR #58 round 3. Moving the inline tests to `crc/tests.rs` behind
        // `#[cfg(test)] mod tests;` is an ordinary refactor, and the submodule scan added
        // in the round before would have reported the bit-flip sweep's
        // `const MESSAGE: [u8; 12]` as a production table. A rule that rejects a test-only
        // refactor is a rule contributors learn to work around.
        for path in [
            "crates/waymaker-flash/src/crc/tests.rs",
            "crates/waymaker-flash/src/crc/tests/mod.rs",
        ] {
            let sources = vec![
                layer(
                    INTEGRITY_CHECK_PATH,
                    &format!(
                        "{}#[cfg(test)]\nmod tests;\n",
                        tests_support::clean_checksum_module()
                    ),
                ),
                crate::size::LayerSource {
                    crate_name: "waymaker-flash".to_owned(),
                    path: path.to_owned(),
                    contents: "const MESSAGE: [u8; 12] = [0; 12];\n".to_owned(),
                },
            ];
            assert!(
                check_integrity_check(&sources).is_empty(),
                "`{path}` was read as production code"
            );
        }
    }

    #[test]
    fn an_ungated_submodule_is_still_scanned_when_a_sibling_is_test_only() {
        // The exemption has to be per module, not "there is a cfg(test) mod somewhere".
        let sources = vec![
            layer(
                INTEGRITY_CHECK_PATH,
                &format!(
                    "{}#[cfg(test)]\nmod tests;\nmod table;\n",
                    tests_support::clean_checksum_module()
                ),
            ),
            crate::size::LayerSource {
                crate_name: "waymaker-flash".to_owned(),
                path: "crates/waymaker-flash/src/crc/tests.rs".to_owned(),
                contents: "const MESSAGE: [u8; 12] = [0; 12];\n".to_owned(),
            },
            crate::size::LayerSource {
                crate_name: "waymaker-flash".to_owned(),
                path: "crates/waymaker-flash/src/crc/table.rs".to_owned(),
                contents: "pub(crate) const NIBBLE: [u32; 16] = [0; 16];\n".to_owned(),
            },
        ];
        let violations = check_integrity_check(&sources);
        assert!(
            violations.iter().any(|v| v.detail.contains("NIBBLE")),
            "{violations:?}"
        );
        assert!(
            !violations.iter().any(|v| v.detail.contains("MESSAGE")),
            "{violations:?}"
        );
    }

    #[test]
    fn a_lookup_table_in_a_checksum_submodule_is_reported() {
        // Codex, PR #58. Splitting `crc.rs` into `crc/mod.rs` and `crc/table.rs` is an
        // ordinary refactor, and it was how a table would arrive with the rule none the
        // wiser — it read one path and called everything else absent.
        let sources = vec![
            layer(
                INTEGRITY_CHECK_PATH,
                &tests_support::clean_checksum_module(),
            ),
            crate::size::LayerSource {
                crate_name: "waymaker-flash".to_owned(),
                path: "crates/waymaker-flash/src/crc/table.rs".to_owned(),
                contents: "pub(crate) const NIBBLE: [u32; 16] = [0; 16];\n".to_owned(),
            },
        ];
        let violations = check_integrity_check(&sources);
        assert!(
            violations
                .iter()
                .any(|v| v.detail.contains("NIBBLE") && v.detail.contains("crc/table.rs")),
            "a table in a checksum submodule went unseen: {violations:?}"
        );
    }
}

/// Fixtures describing a replay module that does not exist on disk.
#[cfg(test)]
pub mod tests_support {
    use std::collections::BTreeSet;
    use std::fmt::Write as _;

    use super::{
        APPEND_BARRIER_CALL, APPEND_BARRIER_STEP, APPEND_COMMIT_CALL, APPEND_COMMIT_STEP,
        APPEND_ROUTING_STEPS, APPEND_SURFACE, APPEND_TYPESTATE, BANK_SEALING_FUNCTIONS,
        BOUNDARY_DECISIONS, BOUNDARY_TYPES, CAPACITY_ADMISSION_CALL, CAPACITY_DELEGATION,
        CAPACITY_GATE, CAPACITY_SURFACE, CHECKSUM_MODULE, CLOCK_SPEC_CONSTRUCTION, CLOCK_SURFACE,
        CTX_FUTURES, CTX_JOURNAL_SURFACE, CTX_PRIVATE_METHODS, CTX_SURFACE, CTX_TYPE,
        DIGEST_FUNCTION, DISPATCH_SURFACE, EFFECT_SCHEDULED_FIELDS, FRAME_LEN_STEP, HEADER_STEP,
        INTEGRITY_CHECK_PARAMETERS, RECOVERY_ROUTING_STEPS, RECOVERY_SURFACE, REPLAY_SURFACE,
        SCAN_STEP, SEAL_BINDINGS, SEALING_FUNCTIONS, STORAGE_CONTRACT_SURFACE, SWAP_BARRIER_CALL,
        SWAP_COMMIT_STEP, SWAP_CONSTRUCTIONS, SWAP_ERASE_CALLS, SWAP_ROUTING_STEPS, SWAP_SURFACE,
        SWAP_TYPESTATE, TIMER_BRACED_STRUCTS, TIMER_RECORD_FIELDS, TIMER_SURFACE,
        TIMER_TYPE_METHODS, TIMER_TYPES, TRANSITION_SURFACE, WIRING_SELECTION_BODIES,
        WIRING_SURFACE, WIRING_TYPE_FIELDS, WIRING_TYPE_METHODS,
    };

    /// A module declaring exactly `pinned` and nothing else.
    ///
    /// Rendered from the pin rather than written out, so that a name added to a pin without
    /// the real module gaining it fails against the real workspace — where it should —
    /// rather than here, where it would look like a fixture problem.
    fn surface(title: &str, pinned: &[&str]) -> String {
        let mut source = format!("//! {title}\n");
        for name in pinned {
            source.push_str("pub fn ");
            source.push_str(name);
            source.push_str("() {}\n");
        }
        source
    }

    /// A replay module declaring exactly [`REPLAY_SURFACE`] and nothing else.
    #[must_use]
    pub fn clean_replay_surface() -> String {
        surface("A replay module.", REPLAY_SURFACE)
    }

    /// A transition module declaring exactly [`TRANSITION_SURFACE`] and [`BOUNDARY_TYPES`].
    ///
    /// Both pins read this one file, so a fixture carrying only the function surface would
    /// describe a workspace `kernel-boundary` rejects for a reason no test here is about.
    #[must_use]
    pub fn clean_transition_surface() -> String {
        use std::fmt::Write as _;

        let mut source = surface("A transition module.", TRANSITION_SURFACE);
        for pinned in BOUNDARY_TYPES {
            let _ = writeln!(source, "{} {{", pinned.header);
            for member in pinned.members {
                if pinned.header.starts_with("pub struct") {
                    let _ = writeln!(source, "    pub {member}: u32,");
                } else {
                    let _ = writeln!(source, "    {member},");
                }
            }
            source.push_str("}\n");
        }
        source
    }

    /// A timer module satisfying every half of `timer-capability`'s kernel side.
    ///
    /// Rendered from the pins rather than written out, so a name added to a pin without the
    /// real module gaining it fails against the real workspace, where it should. The methods
    /// are rendered inside their `impl` blocks rather than as free functions: the surface pin
    /// reports a name declared twice, so a fixture that did both would describe a workspace
    /// the gate rejects for a reason no test here is about.
    #[must_use]
    pub fn clean_timer_module() -> String {
        use std::fmt::Write as _;

        let mut source = String::from("//! A timer module.\n");
        for pinned in TIMER_TYPES {
            let _ = writeln!(source, "{} {{", pinned.header);
            for member in pinned.members {
                let _ = writeln!(source, "    {member},");
            }
            source.push_str("}\n");
        }
        for name in TIMER_BRACED_STRUCTS {
            let _ = writeln!(source, "pub struct {name} {{\n    spec: TimerSpec,\n}}");
        }
        for (name, methods) in TIMER_TYPE_METHODS {
            let _ = writeln!(source, "impl {name} {{");
            for method in *methods {
                let _ = writeln!(source, "    pub fn {method}() {{}}");
            }
            source.push_str("}\n");
        }
        source
    }

    /// A kernel crate root re-exporting every type `timer-capability` pins.
    #[must_use]
    pub fn clean_kernel_root() -> String {
        let exported: Vec<&str> = TIMER_TYPES
            .iter()
            .map(|pinned| pinned.header)
            .chain(TIMER_BRACED_STRUCTS.iter().copied())
            .filter_map(|header| header.rsplit(' ').next())
            .collect();
        format!(
            "//! A kernel crate root.\npub mod timer;\npub use timer::{{{}}};\n",
            exported.join(", ")
        )
    }

    /// A façade module declaring exactly [`CTX_SURFACE`] and [`CTX_FUTURES`], naming no
    /// authority and holding no `static`.
    ///
    /// The futures implement `Future` rather than declaring an inherent `impl`. That is the
    /// shape the `poll` exemption exists for: a trait impl's method is public whether or
    /// not it says so, so a fixture built from inherent `impl`s would let the exemption be
    /// deleted with every test still green. Review of this change measured exactly that.
    #[must_use]
    pub fn clean_ctx_facade() -> String {
        let mut source = format!("//! The façade.\npub struct {CTX_TYPE};\nimpl {CTX_TYPE} {{\n");
        for name in CTX_SURFACE {
            let _ = writeln!(source, "    pub fn {name}() {{}}");
        }
        for name in CTX_PRIVATE_METHODS {
            let _ = writeln!(source, "    fn {name}() {{}}");
        }
        source.push_str("}\n");
        for future in CTX_FUTURES {
            let _ = writeln!(source, "pub struct {future};");
            let _ = writeln!(
                source,
                "impl core::future::Future for {future} {{\n    fn poll() {{}}\n}}"
            );
        }
        source
    }

    /// A durable half declaring exactly [`CTX_JOURNAL_SURFACE`].
    #[must_use]
    pub fn clean_ctx_journal() -> String {
        surface("The durable half.", CTX_JOURNAL_SURFACE)
    }

    /// A dispatcher module declaring exactly [`DISPATCH_SURFACE`].
    #[must_use]
    pub fn clean_dispatch_module() -> String {
        surface("The world's half.", DISPATCH_SURFACE)
    }

    /// A wiring module declaring exactly [`WIRING_SURFACE`], with the two types
    /// [`WIRING_TYPE_METHODS`] pins and selection bodies that name no label.
    ///
    /// Rendered from the pins rather than written out, for [`surface`]'s reason.
    #[must_use]
    pub fn clean_wiring_module() -> String {
        let mut source = String::from("//! The dispatch wiring.\n");
        for (type_name, methods) in WIRING_TYPE_METHODS {
            let fields: String = WIRING_TYPE_FIELDS
                .iter()
                .find(|(named, _)| named == type_name)
                .map(|(_, fields)| {
                    let mut rendered = String::new();
                    for field in *fields {
                        let _ = writeln!(&mut rendered, "    {field}: u16,");
                    }
                    rendered
                })
                .unwrap_or_default();
            let _ = writeln!(&mut source, "pub struct {type_name} {{\n{fields}}}");
            let _ = writeln!(&mut source, "impl {type_name} {{");
            for method in *methods {
                if WIRING_SELECTION_BODIES.contains(method) {
                    let _ = writeln!(
                        &mut source,
                        "    fn {method}(&self) {{ let _ = self.kind; }}"
                    );
                } else {
                    let _ = writeln!(&mut source, "    pub fn {method}(&self) {{}}");
                }
            }
            let _ = writeln!(&mut source, "}}");
        }
        // The trait method, which lives in a trait `impl` rather than an inherent one.
        let _ = writeln!(
            &mut source,
            "impl Dispatcher for Table {{\n    pub fn poll_dispatch(&self) {{ let _ = self.row(); }}\n}}"
        );
        source
    }

    /// A driver module that names no façade type.
    #[must_use]
    pub fn clean_facade_free_driver_module() -> String {
        "//! A driver module that knows nothing about the façade.\n".to_owned()
    }

    /// A clock module declaring exactly [`CLOCK_SURFACE`], naming no boot clock, and
    /// building the one spec [`CLOCK_SPEC_CONSTRUCTION`] permits.
    #[must_use]
    pub fn clean_clock_module() -> String {
        let mut source = surface("A persistent-clock module.", CLOCK_SURFACE);
        let _ = writeln!(
            &mut source,
            "const SPEC: () = {{ let _ = {CLOCK_SPEC_CONSTRUCTION}; }};"
        );
        source
    }

    /// A driver module deciding from exactly [`BOUNDARY_DECISIONS`] and naming none of
    /// [`DRIVER_FORBIDDEN_VOCABULARY`].
    #[must_use]
    pub fn clean_driver_module() -> String {
        use std::fmt::Write as _;

        let mut source = String::from("//! A synchronous driver.\npub fn drive() {\n");
        for decision in BOUNDARY_DECISIONS {
            let _ = writeln!(source, "    let _ = {decision};");
        }
        source.push_str("}\n");
        source
    }

    /// An effect protocol `effect-protocol` accepts.
    ///
    /// Shaped like the file it models rather than minimally: generics on every type, the
    /// steps as a chained method call on the writer, a doc comment carrying a struct literal,
    /// and a `#[cfg(test)]` module. Each of those exercises a helper the real file depends on
    /// — `implemented_type`, `tightened`, `code_only`, `without_test_modules` — and a fixture
    /// without them leaves those helpers pinned by nothing.
    #[must_use]
    pub fn clean_effect_module() -> String {
        String::from(
            r"//! §07's protocol.

use waymaker_core::{EffectId, EffectSeq, RecordRef, RunId};
use waymaker_flash::capacity::Reserved;
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};

/// A proof that step 3 completed.
///
/// ```compile_fail,E0451
/// let forged = DurableIntent { id: EffectId { run: RunId(1), seq: EffectSeq(0) } };
/// ```
pub struct DurableIntent {
    id: EffectId,
}

impl DurableIntent {
    /// The identity step 4 dispatches under.
    pub const fn id(self) -> EffectId {
        self.id
    }
}

/// The protocol between effects.
pub struct Effect<C: IntegrityCheck = Catalogued> {
    run: RunId,
    writer: Reserved<C>,
}

impl<C: IntegrityCheck> Effect<C> {
    /// The protocol over a writer.
    pub const fn over(run: RunId, writer: Reserved<C>) -> Self {
        Self { run, writer }
    }

    /// The writer back.
    pub const fn into_writer(self) -> Reserved<C> {
        self.writer
    }

    /// Steps 1, 2 and 3.
    pub fn schedule<S>(mut self, storage: &mut S, seq: EffectSeq, page: &mut [u8]) -> Dispatchable<C> {
        let record = RecordRef::EffectScheduled { seq };
        self.writer
            .stage(storage, &record, page)
            .payload_barrier(storage)
            .commit(storage);
        Dispatchable {
            intent: DurableIntent {
                id: EffectId { run: self.run, seq },
            },
            writer: self.writer,
        }
    }

    /// An intent committed before this boot.
    pub(crate) const fn redelivering(self, seq: EffectSeq) -> Dispatchable<C> {
        Dispatchable {
            intent: DurableIntent {
                id: EffectId { run: self.run, seq },
            },
            writer: self.writer,
        }
    }
}

/// An effect whose intent is durable.
pub struct Dispatchable<C: IntegrityCheck = Catalogued> {
    intent: DurableIntent,
    writer: Reserved<C>,
}

impl<C: IntegrityCheck> Dispatchable<C> {
    /// What step 4 dispatches under.
    pub const fn intent(&self) -> DurableIntent {
        self.intent
    }

    /// Steps 5, 6 and 7.
    pub fn resolve<S>(mut self, storage: &mut S, page: &mut [u8]) -> Effect<C> {
        let record = RecordRef::EffectCompleted { seq: self.intent.id.seq };
        self.writer
            .stage(storage, &record, page)
            .payload_barrier(storage)
            .commit(storage);
        Effect {
            run: self.intent.id.run,
            writer: self.writer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_module_is_not_read_by_these_pins() {
        let _ = DurableIntent { id: EffectId { run: RunId(0), seq: EffectSeq(0) } };
    }
}
",
        )
    }

    /// A codec module every half of the rule accepts.
    #[must_use]
    pub fn clean_codec_module() -> String {
        String::from(
            "pub trait Decode: Sized {\n    type Error;\n}\n\
             impl Decode for () {\n    type Error = ();\n}\n\
             #[cfg(feature = \"serde\")]\npub use serde;\n\
             #[cfg(feature = \"serde\")]\npub trait Format {}\n\
             #[cfg(feature = \"serde\")]\npub struct Coded<F, T>(F, T);\n\
             #[cfg(feature = \"postcard\")]\npub struct Postcard;\n\
             #[cfg(feature = \"postcard\")]\npub type FromPostcard<T> = Coded<Postcard, T>;\n",
        )
    }

    /// A façade manifest every half of the rule accepts.
    #[must_use]
    pub fn clean_codec_manifest() -> String {
        String::from(
            "[package]\nname = \"waymaker-embassy\"\n\n\
             [dependencies]\n\
             serde = { version = \"1\", optional = true, default-features = false }\n\
             postcard = { version = \"1\", optional = true, default-features = false }\n\n\
             [features]\ndefault = []\nserde = [\"dep:serde\"]\n\
             postcard = [\"dep:postcard\", \"serde\"]\n\n\
             [lints]\nworkspace = true\n",
        )
    }

    /// A storage module declaring exactly [`STORAGE_CONTRACT_SURFACE`] and nothing else.
    #[must_use]
    pub fn clean_storage_contract() -> String {
        surface("A storage module.", STORAGE_CONTRACT_SURFACE)
    }

    /// A recovery module declaring exactly [`RECOVERY_SURFACE`] and nothing else.
    #[must_use]
    pub fn clean_recovery_surface() -> String {
        surface("A recovery module.", RECOVERY_SURFACE)
    }

    /// A `waymaker-rig` oracle whose public surface is exactly the pin.
    #[must_use]
    pub fn clean_rig_audit() -> String {
        surface("A rig oracle.", super::RIG_AUDIT_SURFACE)
    }

    /// A `waymaker-rig` census whose public surface is exactly the pin.
    #[must_use]
    pub fn clean_rig_census() -> String {
        surface("A rig census.", super::RIG_CENSUS_SURFACE)
    }

    /// A `waymaker-rig` runner whose public surface is exactly the pin.
    #[must_use]
    pub fn clean_rig_run() -> String {
        surface("A rig runner.", super::RIG_RUN_SURFACE)
    }

    /// A `waymaker-rig` failure matrix whose public surface is exactly the pin.
    #[must_use]
    pub fn clean_rig_matrix() -> String {
        surface("A failure matrix.", super::RIG_MATRIX_SURFACE)
    }

    /// A board clock module satisfying every half of `timer-capability`'s board side.
    ///
    /// Rendered from the pin for [`clean_timer_module`]'s reason, and with each name emitted
    /// once: the surface pin reports a name declared twice, so a fixture that put a pinned
    /// method in the `impl` *and* beside it would describe a workspace the gate rejects for a
    /// reason no test here is about.
    #[must_use]
    pub fn clean_board_clock(clock: &super::BoardClock) -> String {
        use std::fmt::Write as _;

        let mut source = format!("//! A board clock.\npub struct {} {{\n", clock.driver);
        source.push_str("    registers: u8,\n}\n");
        let _ = writeln!(source, "impl {} {{", clock.driver);
        for method in clock.methods {
            let visibility = if clock.surface.contains(method) {
                "pub "
            } else {
                ""
            };
            let _ = writeln!(source, "    {visibility}fn {method}() {{}}");
        }
        source.push_str("}\n");
        for name in clock.surface {
            if !clock.methods.contains(name) {
                let _ = writeln!(source, "pub fn {name}() {{}}");
            }
        }
        source
    }

    /// A capacity module the `capacity-reserve` rule accepts whole.
    ///
    /// The surface is rendered from the pin for the reason every surface above is, and the
    /// `stage` body is the pinned admission and delegation in the order the rule requires —
    /// so a fixture cannot be the reason the shape half passes.
    #[must_use]
    pub fn clean_capacity_reserve() -> String {
        // Every pinned name but `stage`, which the `impl` below declares: the pin refuses a
        // name declared twice, so a fixture that rendered it in both places would fail the
        // surface half for a reason no rule is about.
        let others: Vec<&str> = CAPACITY_SURFACE
            .iter()
            .copied()
            .filter(|name| *name != "stage")
            .collect();
        let mut source = surface("A capacity module.", &others);
        source.push('\n');
        source.push_str("impl ");
        source.push_str(CAPACITY_GATE);
        source.push_str(" {\n");
        source.push_str("    pub fn stage(&mut self) -> Result<(), ()> {\n        ");
        source.push_str(CAPACITY_ADMISSION_CALL);
        source.push_str("\n        ");
        source.push_str(CAPACITY_DELEGATION);
        source.push_str("\n    }\n}\n");
        source
    }

    /// A record module declaring exactly [`EFFECT_SCHEDULED_FIELDS`] and nothing else.
    ///
    /// Rendered from the pin for the same reason the surfaces above are: a field added to
    /// the pin without the real record gaining it should fail against the real workspace,
    /// not here.
    #[must_use]
    pub fn clean_record_module() -> String {
        use std::fmt::Write as _;

        let mut variants = String::new();
        let _ = writeln!(variants, "    EffectScheduled {{");
        for field in EFFECT_SCHEDULED_FIELDS {
            let _ = writeln!(variants, "        {field}: u32,");
        }
        let _ = writeln!(variants, "    }},");
        // The timer bodies too, because `timer-record-fields` fails closed when the variant
        // it pins is absent — a fixture without them describes a workspace the gate rejects
        // for a reason no test here is about.
        for (variant, fields) in TIMER_RECORD_FIELDS {
            let _ = writeln!(variants, "    {variant} {{");
            for field in *fields {
                let _ = writeln!(variants, "        {field}: u32,");
            }
            let _ = writeln!(variants, "    }},");
        }
        format!("//! A record module.\npub enum RecordRef<'a> {{\n{variants}}}\n")
    }

    /// A checksum module carrying every pinned parameter and declaring no lookup table.
    ///
    /// `pub(crate)`, like the real one, so that `size-probe-reach` does not demand a probe
    /// call for a fixture.
    #[must_use]
    pub fn clean_checksum_module() -> String {
        use std::collections::BTreeMap;
        use std::fmt::Write as _;

        // Grouped by function and repeated as many times as the pin expects, because the
        // pin is now a count inside one body rather than a presence check over the file.
        let mut bodies: BTreeMap<&str, String> = BTreeMap::new();
        for parameter in INTEGRITY_CHECK_PARAMETERS {
            let body = bodies.entry(parameter.function).or_default();
            for _ in 0..parameter.occurrences {
                let _ = write!(body, " {}", parameter.literal);
            }
        }

        let mut source = String::from("//! Two checksums.\n");
        for (function, body) in bodies {
            let _ = writeln!(
                source,
                "pub(crate) const fn {function}(bytes: &[u8]) -> u32 {{{body} }}"
            );
        }
        source
    }

    /// A binding module the pin accepts, for tests about everything else.
    ///
    /// Rendered from [`SEAL_BINDINGS`] rather than written out, so a seal added to the table
    /// arrives in the fixture too and a fixture cannot pass a pin the real module fails.
    #[must_use]
    pub fn clean_integrity_binding() -> String {
        use std::fmt::Write as _;

        let mut source = String::from("//! The swap point.\n\n");
        let _ = writeln!(
            source,
            "use {CHECKSUM_MODULE}::{{{}}};",
            SEAL_BINDINGS
                .iter()
                .map(|seal| seal.delegates_to)
                .collect::<Vec<&str>>()
                .join(", ")
        );
        source.push_str("\npub trait IntegrityCheck {\n");
        for seal in SEAL_BINDINGS {
            let _ = writeln!(
                source,
                "    fn {}(bytes: &[u8]) -> {};",
                seal.method, seal.width
            );
        }
        source.push_str("}\n\npub struct Catalogued;\n\nimpl IntegrityCheck for Catalogued {\n");
        for seal in SEAL_BINDINGS {
            let _ = writeln!(
                source,
                "    fn {}(bytes: &[u8]) -> {} {{ {}(bytes) }}",
                seal.method, seal.width, seal.delegates_to
            );
        }
        source.push_str("}\n");
        source
    }

    /// A codec whose routing the pin accepts, for tests about everything else.
    ///
    /// Rendered from the same tables the rule reads, so a seal or a sealing function added
    /// to a pin arrives here too — a fixture written out by hand is a fixture that passes a
    /// pin the real codec fails.
    #[must_use]
    pub fn clean_integrity_routing() -> String {
        use std::fmt::Write as _;

        let mut source = String::from("//! The codec.\n");
        for (function, methods) in SEALING_FUNCTIONS {
            let _ = writeln!(source, "pub fn {function}<C: IntegrityCheck>() -> u32 {{");
            for method in *methods {
                let _ = writeln!(
                    source,
                    "    let seal_{method} = C::{method}(bytes).to_le_bytes();"
                );
            }
            // The routed calls each body owes, rendered from the same pins the rule reads —
            // including the scan's, so `next` is declared once rather than twice.
            for (owner, callee) in [HEADER_STEP, FRAME_LEN_STEP, SCAN_STEP] {
                if owner == *function {
                    let _ = writeln!(source, "    let routed = {callee}::<C>(bytes);");
                }
            }
            source.push_str("    0\n}\n");
        }
        let _ = writeln!(
            source,
            "pub const fn {}(input: &[u8]) -> u32 {{ {}(input) }}",
            DIGEST_FUNCTION.0, DIGEST_FUNCTION.1
        );
        source
    }

    /// A whole recovery module both of its pins accept: the surface *and* the routing.
    ///
    /// One fixture rather than two concatenated, because `next` is on both lists and two
    /// declarations of it would make `braced_body` read whichever came first — which is the
    /// decoy `sealing_function_violations` refuses in the real codec, and a fixture is no
    /// place to demonstrate it accidentally.
    ///
    /// Rendered from [`RECOVERY_SURFACE`] and [`RECOVERY_ROUTING_STEPS`] for the reason every
    /// other clean fixture is rendered from its table: one written out by hand is one that
    /// passes a pin the real reader fails.
    #[must_use]
    pub fn clean_recovery_routing() -> String {
        use std::fmt::Write as _;

        let mut source = String::from("//! A recovery module.\n");
        for name in RECOVERY_SURFACE {
            let _ = match RECOVERY_ROUTING_STEPS.iter().find(|(step, _)| step == name) {
                Some((_, through)) => writeln!(
                    source,
                    "pub fn {name}<C: IntegrityCheck>() -> u32 {{ {through}::<C>(bytes) }}"
                ),
                None => writeln!(source, "pub fn {name}() {{}}"),
            };
        }
        // The private steps, which the surface pin does not see and this one does.
        for (step, through) in RECOVERY_ROUTING_STEPS {
            if !RECOVERY_SURFACE.contains(step) {
                let _ = writeln!(
                    source,
                    "fn {step}<C: IntegrityCheck>() -> u32 {{ {through}::<C>(bytes) }}"
                );
            }
        }
        source
    }

    /// A writer whose surface, typestate and routing all three pins accept.
    ///
    /// Rendered from [`APPEND_SURFACE`], [`APPEND_TYPESTATE`] and [`APPEND_ROUTING_STEPS`]
    /// rather than written out, for the reason [`clean_integrity_routing`] is: a fixture
    /// written by hand is a fixture that passes a pin the real writer fails.
    #[must_use]
    pub fn clean_append_module() -> String {
        use std::fmt::Write as _;

        let [staged, sealable] = APPEND_TYPESTATE;
        let mut source = String::from("//! A writer.\n");
        let _ = writeln!(source, "pub struct {staged};\npub struct {sealable};");

        // Everything the surface pin lists that the typestate does not own, on a `Journal`
        // that is otherwise not modelled.
        let _ = writeln!(source, "impl Journal {{");
        for name in APPEND_SURFACE {
            if *name == APPEND_BARRIER_STEP || *name == APPEND_COMMIT_STEP {
                continue;
            }
            match APPEND_ROUTING_STEPS.iter().find(|(step, _)| step == name) {
                Some((_, through)) => {
                    let _ = writeln!(
                        source,
                        "    pub fn {name}<C: IntegrityCheck>() -> u32 {{ {through}::<C>(r) }}"
                    );
                }
                None => {
                    let _ = writeln!(source, "    pub fn {name}() {{}}");
                }
            }
        }
        source.push_str("}\n");

        let _ = writeln!(
            source,
            "impl {staged} {{\n    pub fn {APPEND_BARRIER_STEP}(self) -> {sealable} {{\n        {APPEND_BARRIER_CALL};\n        {sealable} {{}}\n    }}\n}}"
        );
        let _ = writeln!(
            source,
            "impl {sealable} {{\n    pub fn {APPEND_COMMIT_STEP}(self) {{\n                     {APPEND_COMMIT_CALL};\n        {APPEND_BARRIER_CALL};\n    }}\n}}"
        );
        source
    }

    /// A bank codec whose routing the pin accepts, for tests about everything else.
    ///
    /// Rendered from [`BANK_SEALING_FUNCTIONS`] for the reason
    /// [`clean_integrity_routing`] is rendered from its own table: a fixture written out by
    /// hand is a fixture that passes a pin the real codec fails.
    #[must_use]
    pub fn clean_bank_routing() -> String {
        use std::fmt::Write as _;

        let mut source = String::from("//! The bank codec.\n");
        for (function, methods) in BANK_SEALING_FUNCTIONS {
            let _ = writeln!(source, "pub fn {function}<C: IntegrityCheck>() -> u32 {{");
            for method in *methods {
                let _ = writeln!(source, "    let seal_{method} = C::{method}(bytes);");
            }
            source.push_str("    0\n}\n");
        }
        source
    }

    /// A swap whose surface, step order and routing all three pins accept.
    ///
    /// Rendered from [`SWAP_SURFACE`], [`SWAP_TYPESTATE`], [`SWAP_CONSTRUCTIONS`],
    /// [`SWAP_ERASE_CALLS`] and [`SWAP_ROUTING_STEPS`] rather than written out, for the
    /// reason [`clean_append_module`] is: a fixture written by hand is a fixture that passes
    /// a pin the real swap fails.
    #[must_use]
    pub fn clean_swap_module() -> String {
        use std::fmt::Write as _;

        let mut source = String::from("//! A swap.\n");
        for (state, _) in SWAP_TYPESTATE {
            let _ = writeln!(source, "pub struct {state};");
        }
        for (value, _) in SWAP_CONSTRUCTIONS {
            if !SWAP_TYPESTATE.iter().any(|(state, _)| *state == value) {
                let _ = writeln!(source, "pub struct {value};");
            }
        }

        // The steps, each in the state that owns it.
        for (state, step) in SWAP_TYPESTATE {
            let produces = SWAP_CONSTRUCTIONS
                .iter()
                .find(|(_, from)| *from == step)
                .map(|(value, _)| *value);
            let _ = writeln!(source, "impl {state} {{\n    pub fn {step}() {{");
            if step == "payload_barrier" {
                let _ = writeln!(source, "        {SWAP_BARRIER_CALL};");
            }
            if step == SWAP_COMMIT_STEP.0 {
                let _ = writeln!(source, "        {};", SWAP_COMMIT_STEP.1);
                let _ = writeln!(source, "        {SWAP_BARRIER_CALL};");
            }
            for (routed, entries) in SWAP_ROUTING_STEPS {
                if routed != &step {
                    continue;
                }
                for (index, through) in entries.iter().enumerate() {
                    let _ = writeln!(source, "        let seal{index} = {through}::<C>(bytes);");
                }
            }
            if let Some(value) = produces {
                let _ = writeln!(source, "        {value} {{}};");
            }
            source.push_str("    }\n}\n");
        }

        // The two erases, and everything else the surface pin lists.
        let owned: Vec<&str> = SWAP_TYPESTATE.iter().map(|(_, step)| *step).collect();
        let _ = writeln!(source, "impl Swap {{");
        for name in SWAP_SURFACE {
            if owned.contains(name) {
                continue;
            }
            match SWAP_ERASE_CALLS.iter().find(|(step, _, _)| step == name) {
                Some((_, erase, _)) => {
                    let _ = writeln!(
                        source,
                        "    pub fn {name}() {{\n        {erase};\n        \
                         {SWAP_BARRIER_CALL};\n    }}"
                    );
                }
                None => {
                    let _ = writeln!(source, "    pub fn {name}() {{}}");
                }
            }
        }
        source.push_str("}\n");
        source
    }

    /// Probe source calling every name both pins list, for the clean-workspace fixture.
    ///
    /// `size-probe-reach` demands a call for every public function a layer declares, and the
    /// two clean surfaces above declare thirteen distinct names between them — so a fixture
    /// that supplied one without the other would describe a workspace the gate rejects for a
    /// reason that has nothing to do with what is being tested.
    ///
    /// Deduplicated, because the pins share five names — `new`, `run`, `position`,
    /// `pending` and `advance` — and a second call to one would be a second identical line
    /// rather than a second reachable function.
    #[must_use]
    pub fn clean_probe_calls() -> String {
        // The seal methods too: `size-probe-reach` counts a trait's methods as public
        // whether or not they carry `pub`, so a fixture binding module the probe fixture
        // does not call is a clean workspace the gate rejects.
        let seals = SEAL_BINDINGS.iter().map(|seal| &seal.method);
        let names: BTreeSet<&&str> = REPLAY_SURFACE
            .iter()
            .chain(TRANSITION_SURFACE)
            .chain(STORAGE_CONTRACT_SURFACE)
            .chain(RECOVERY_SURFACE)
            .chain(seals)
            .chain(SEALING_FUNCTIONS.iter().map(|(name, _)| name))
            .chain([&DIGEST_FUNCTION.0])
            .chain(BANK_SEALING_FUNCTIONS.iter().map(|(name, _)| name))
            .chain(APPEND_SURFACE)
            .chain(CAPACITY_SURFACE)
            .chain(SWAP_SURFACE)
            .chain(TIMER_SURFACE)
            .chain(CLOCK_SURFACE)
            .chain(CTX_SURFACE)
            .chain(CTX_JOURNAL_SURFACE)
            .chain(DISPATCH_SURFACE)
            .chain(WIRING_SURFACE)
            .collect();
        let mut source = String::from("\nfn reaches_the_pinned_surfaces() {\n");
        for name in names {
            source.push_str("    ");
            source.push_str(name);
            source.push_str("();\n");
        }
        source.push_str("}\n");
        source
    }
}
