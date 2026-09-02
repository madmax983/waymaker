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
