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
/// A test-support crate is deliberately held to less: it is host code, so `#![no_std]` and
/// the `extern crate std` scan would be wrong for it. `#![forbid(unsafe_code)]` is not —
/// nothing about modelling media in a `Vec<u8>` needs it, and a harness the layers are
/// tested against is the last place an unreviewed `unsafe` block should be able to appear.
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

/// Rule: no source file of any layer re-admits `std` or `alloc`.
/// Rule: no source file of any layer re-admits `std` or `alloc`.
///
/// The other half of [`check_crate_attributes`]'s `extern crate` scan, which reads crate
/// roots. A nested module may declare `extern crate alloc;` perfectly legally, and nothing
/// about the crate root would change — so a firmware crate could allocate with `#![no_std]`
/// above it, `cargo build --target thumbv6m-none-eabi` still green (`alloc` cross-compiles),
/// and `waymaker-spec`'s `bounded-decoding` row still claiming allocation-freedom is
/// structural. It is structural only because of this.
///
/// Fires under `crate-attributes` rather than under an id of its own: it is the same rule
/// about the same thing, read over more files.
#[must_use]
pub fn check_layer_sources_are_bare_metal(sources: &[crate::size::LayerSource]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for source in sources {
        if crate::policy::layer(&source.crate_name).is_none() {
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
/// Sorted, so that the comparison below can be a set comparison and the list can be read.
pub const REPLAY_SURFACE: &[&str] = &[
    "advance",
    "is_terminal",
    "new",
    "next_effect_id",
    "next_seq",
    "pending",
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
    "position",
    "run",
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

/// The two codec functions whose bodies compute a seal.
///
/// A binding rule that reads only `integrity.rs` pins a trait nothing is obliged to call.
/// Review of this change found exactly that: a codec re-hard-wired to `crc16` and `crc32`,
/// with `integrity.rs` left perfectly intact, passed every rule. So the routing is pinned
/// too — these two bodies must name `C::header_check` and `C::frame_check`, and must not
/// name a checksum function at all.
pub const SEALING_FUNCTIONS: &[&str] = &["encode_with", "decode_with"];

/// The one function permitted to call the checksum module from the codec, and what it may
/// call.
///
/// [`crate::size`] cannot see this and neither can a type: `input_digest` is a `const fn`,
/// a trait method cannot be one, so ADR 0011's digest reaches `crc32` directly. That is the
/// single documented exception, and naming it here is what stops it from becoming a habit.
pub const DIGEST_FUNCTION: (&str, &str) = ("input_digest", "crc32");

/// The scan's step, and the entry point it must walk a journal with.
///
/// `decode` rather than `decode_with::<C>` here would make [`crate::size`]'s type parameter
/// decorative: every scan would verify with the shipped check whatever its caller asked
/// for, which is a reader silently disagreeing with the writer.
pub const SCAN_STEP: (&str, &str) = ("next", "decode_with");

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

    for function in SEALING_FUNCTIONS {
        violations.extend(sealing_function_violations(function, &code, &checksums));
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
fn sealing_function_violations(function: &str, code: &str, checksums: &[&str]) -> Vec<Violation> {
    const RULE: &str = "integrity-check";
    const ADAPTER: &str = "waymaker-flash";

    let mut violations = Vec::new();
    let Some(body) = braced_body(code, &format!("fn {function}")) else {
        violations.push(Violation::new(
            RULE,
            ADAPTER,
            format!(
                "{INTEGRITY_ROUTING_PATH} declares no `fn {function}`, so the codec's \
                 route to its seals is pinned against nothing"
            ),
        ));
        return violations;
    };
    for seal in SEAL_BINDINGS {
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

    let code = code_only(&source.contents);
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
fn braced_body<'a>(code: &'a str, header: &str) -> Option<&'a str> {
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
fn without_test_modules(code: &str) -> String {
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
            .replace(&format!("{}::<C>(rest)", SCAN_STEP.1), "decode(rest)");
        let violations = check_integrity_routing(&[layer(INTEGRITY_ROUTING_PATH, &source)]);
        assert!(
            violations
                .iter()
                .any(|violation| violation.detail.contains("its caller asked for")),
            "{violations:?}"
        );
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

    use super::{
        CHECKSUM_MODULE, DIGEST_FUNCTION, EFFECT_SCHEDULED_FIELDS, INTEGRITY_CHECK_PARAMETERS,
        REPLAY_SURFACE, SCAN_STEP, SEAL_BINDINGS, SEALING_FUNCTIONS, STORAGE_CONTRACT_SURFACE,
        TRANSITION_SURFACE,
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

    /// A transition module declaring exactly [`TRANSITION_SURFACE`] and nothing else.
    #[must_use]
    pub fn clean_transition_surface() -> String {
        surface("A transition module.", TRANSITION_SURFACE)
    }

    /// A storage module declaring exactly [`STORAGE_CONTRACT_SURFACE`] and nothing else.
    #[must_use]
    pub fn clean_storage_contract() -> String {
        surface("A storage module.", STORAGE_CONTRACT_SURFACE)
    }

    /// A record module declaring exactly [`EFFECT_SCHEDULED_FIELDS`] and nothing else.
    ///
    /// Rendered from the pin for the same reason the surfaces above are: a field added to
    /// the pin without the real record gaining it should fail against the real workspace,
    /// not here.
    #[must_use]
    pub fn clean_record_module() -> String {
        use std::fmt::Write as _;

        let mut fields = String::new();
        for field in EFFECT_SCHEDULED_FIELDS {
            let _ = writeln!(fields, "        {field}: u32,");
        }
        format!(
            "//! A record module.\npub enum RecordRef<'a> {{\n    EffectScheduled {{\n{fields}\
             \x20   }},\n}}\n"
        )
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
        for function in SEALING_FUNCTIONS {
            let _ = writeln!(source, "pub fn {function}<C: IntegrityCheck>() -> u32 {{");
            for seal in SEAL_BINDINGS {
                let _ = writeln!(
                    source,
                    "    let seal_{} = C::{}(bytes).to_le_bytes();",
                    seal.method, seal.method
                );
            }
            source.push_str("    0\n}\n");
        }
        let _ = writeln!(
            source,
            "pub const fn {}(input: &[u8]) -> u32 {{ {}(input) }}",
            DIGEST_FUNCTION.0, DIGEST_FUNCTION.1
        );
        let _ = writeln!(
            source,
            "fn {}(&mut self) -> Option<u32> {{ {}::<C>(rest) }}",
            SCAN_STEP.0, SCAN_STEP.1
        );
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
            .chain(seals)
            .chain(SEALING_FUNCTIONS)
            .chain([&DIGEST_FUNCTION.0])
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
