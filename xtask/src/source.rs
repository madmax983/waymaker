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
        let code = strip_comments(&source.contents);
        for (construct, why) in KERNEL_FORBIDDEN_CONSTRUCTS {
            if code.contains(construct) {
                violations.push(Violation::new(
                    "kernel-owns-no-encoding",
                    KERNEL,
                    format!("{} uses `{construct}`: {why}", source.path),
                ));
            }
        }
        for marker in KERNEL_FORBIDDEN_IMPL_MARKERS {
            let needle = marker.replace(' ', "");
            let implemented = code.lines().any(|line| {
                let line = erase_lifetimes(line).replace(' ', "");
                implements_trait(&line, &needle)
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
    const KERNEL: &str = "waymaker-core";

    let Some(source) = sources.iter().find(|source| {
        source
            .path
            .replace('\\', "/")
            .ends_with(REPLAY_SURFACE_PATH)
    }) else {
        return vec![Violation::new(
            "replay-cursor-surface",
            KERNEL,
            format!(
                "no {REPLAY_SURFACE_PATH} in the workspace, so the pinned replay surface is \
                 checking nothing; the cursor's public API is where design document \u{a7}02 \
                 decision 2 is enforced"
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
                "replay-cursor-surface",
                KERNEL,
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
    let pinned: BTreeSet<String> = REPLAY_SURFACE
        .iter()
        .map(|name| (*name).to_owned())
        .collect();

    for added in declared.difference(&pinned) {
        violations.push(Violation::new(
            "replay-cursor-surface",
            KERNEL,
            format!(
                "{} declares `{added}`, which is not in `source::REPLAY_SURFACE`: the \
                 cursor's public API is pinned so that a lookup by effect id cannot be added \
                 without a reviewer writing it down — see design document \u{a7}02 decision 2",
                source.path
            ),
        ));
    }
    for missing in pinned.difference(&declared) {
        violations.push(Violation::new(
            "replay-cursor-surface",
            KERNEL,
            format!(
                "`source::REPLAY_SURFACE` pins `{missing}`, which {} no longer declares; a \
                 pin nothing matches is a pin that has stopped checking",
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
    after_generics.starts_with(needle)
}

/// Drops `//` comments so that prose describing a construct is not read as the construct.
fn strip_comments(contents: &str) -> String {
    contents
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<&str>>()
        .join("\n")
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

/// Fixtures describing a replay module that does not exist on disk.
#[cfg(test)]
pub mod tests_support {
    use super::REPLAY_SURFACE;

    /// A replay module declaring exactly the pinned surface and nothing else.
    ///
    /// Rendered from [`REPLAY_SURFACE`] rather than written out, so that a name added to the
    /// pin without the real module gaining it fails against the real workspace — where it
    /// should — rather than here, where it would look like a fixture problem.
    #[must_use]
    pub fn clean_replay_surface() -> String {
        let mut source = String::from("//! A replay module.\n");
        for name in REPLAY_SURFACE {
            source.push_str("pub fn ");
            source.push_str(name);
            source.push_str("() {}\n");
        }
        source
    }

    /// Probe source calling every name in [`REPLAY_SURFACE`], for the clean-workspace
    /// fixture.
    ///
    /// `size-probe-reach` demands a call for every public function a layer declares, and
    /// [`clean_replay_surface`] declares eight of them — so a fixture that supplied one
    /// without the other would describe a workspace the gate rejects for a reason that has
    /// nothing to do with what is being tested.
    #[must_use]
    pub fn clean_probe_calls() -> String {
        let mut source = String::from("\nfn reaches_the_replay_surface() {\n");
        for name in REPLAY_SURFACE {
            source.push_str("    ");
            source.push_str(name);
            source.push_str("();\n");
        }
        source.push_str("}\n");
        source
    }
}
