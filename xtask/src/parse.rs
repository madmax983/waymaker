//! Syntactic parsing for the layering gate.
//!
//! Issue #51's decision: the gate answers questions about program syntax, so it parses
//! syntax instead of scanning text. Hand-written lexers drift out of sync with the
//! language (raw strings, nested attributes, either fence character, `use` renames);
//! [`syn`] and `pulldown-cmark` are the parsers the language already has, and they stay
//! in sync for us.
//!
//! The contract this module keeps with the rule checks:
//!
//! * Parse once, read structurally. A rule that needs an attribute reads
//!   [`syn::File::attrs`]; a rule that needs a declaration reads the item it lives on.
//!   Nothing here re-implements lexing.
//! * Fail closed. Every function that parses returns `Result<_, syn::Error>`; a file the
//!   parser cannot read is a file the gate cannot see, and the caller reports that as a
//!   violation rather than as an absence.
//! * Residual limits are documented where they bite. `syn` parses one file's syntax:
//!   it does not resolve names across crates, expand macros, or evaluate `cfg`s.
//!   `pulldown-cmark` reads Markdown structure: it does not validate links or judge
//!   prose.
//!
//! This module is host-only, like all of `xtask`: it never ships in firmware.

use quote::ToTokens as _;
use syn::ext::IdentExt as _;
use syn::visit::Visit as _;

/// Parse Rust source into `syn`'s syntax tree.
///
/// Every structural query in this module starts here so that there is exactly one
/// place where "the file would not parse" becomes an error the caller must handle.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn parse_rust(contents: &str) -> Result<syn::File, syn::Error> {
    syn::parse_file(contents)
}

/// The name of `ident`. Strips a leading `r#` marker.
///
/// `r#alloc` and `alloc` name the same crate (issues #68, #90). Use this function,
/// or [`ident_is`], for every name comparison in this module.
fn ident_name(ident: &syn::Ident) -> String {
    ident.unraw().to_string()
}

/// True if `ident` has the name `name`. Ignores a raw marker on `ident`.
fn ident_is(ident: &syn::Ident, name: &str) -> bool {
    ident.unraw() == name
}

/// True if `path` is one identifier with the name `name`. Ignores a raw marker.
///
/// [`syn::Path::is_ident`] keeps the raw marker. Without this function,
/// `#[r#cfg(test)]` would not match `"cfg"` (issue #90).
fn path_is_ident(path: &syn::Path, name: &str) -> bool {
    path.get_ident().is_some_and(|ident| ident_is(ident, name))
}

/// Whether `attrs` carries exactly `#[cfg(test)]`.
///
/// The textual `without_test_modules` blanked on the substring `#[cfg(test)]`; the
/// structural equivalent matches the attribute: path `cfg` with the single identifier
/// `test` as its argument. `#[cfg(any(test, ...))]` is not the exact spelling and is not
/// skipped — the textual version did not blank on it either.
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        path_is_ident(attr.path(), "cfg")
            && attr
                .parse_args::<syn::Ident>()
                .is_ok_and(|ident| ident_is(&ident, "test"))
    })
}

/// The attributes on an item, whatever kind of item it is.
fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// The attributes on an `impl` member, whatever kind of member it is.
fn impl_item_attrs(item: &syn::ImplItem) -> &[syn::Attribute] {
    match item {
        syn::ImplItem::Const(item) => &item.attrs,
        syn::ImplItem::Fn(item) => &item.attrs,
        syn::ImplItem::Type(item) => &item.attrs,
        syn::ImplItem::Macro(item) => &item.attrs,
        _ => &[],
    }
}

/// The attributes on an expression, for the variants an expression *statement* can
/// carry its own `#[cfg(test)]` on directly — `syn::Expr` is `#[non_exhaustive]` and has
/// no one accessor every variant shares, so this covers the named shapes and falls back
/// to none for anything else (`Verbatim` carries none at all, and a future variant this
/// scan does not yet know about is the same standing).
///
/// Codex's finding: `syn::Expr::RawAddr` — `&raw const place` / `&raw mut place` — has an
/// `attrs` field like every other named variant here, and the fallback dropped it anyway. A
/// legal `#[cfg(test)] &raw const array[..];` statement was read as production code, so
/// `stmt_is_cfg_test` never excluded it and every visitor below walked into its nested
/// expression as if `rustc` had shipped it.
fn expr_attrs(expr: &syn::Expr) -> &[syn::Attribute] {
    match expr {
        syn::Expr::Array(e) => &e.attrs,
        syn::Expr::Assign(e) => &e.attrs,
        syn::Expr::Async(e) => &e.attrs,
        syn::Expr::Await(e) => &e.attrs,
        syn::Expr::Binary(e) => &e.attrs,
        syn::Expr::Block(e) => &e.attrs,
        syn::Expr::Break(e) => &e.attrs,
        syn::Expr::Call(e) => &e.attrs,
        syn::Expr::Cast(e) => &e.attrs,
        syn::Expr::Closure(e) => &e.attrs,
        syn::Expr::Const(e) => &e.attrs,
        syn::Expr::Continue(e) => &e.attrs,
        syn::Expr::Field(e) => &e.attrs,
        syn::Expr::ForLoop(e) => &e.attrs,
        syn::Expr::Group(e) => &e.attrs,
        syn::Expr::If(e) => &e.attrs,
        syn::Expr::Index(e) => &e.attrs,
        syn::Expr::Infer(e) => &e.attrs,
        syn::Expr::Let(e) => &e.attrs,
        syn::Expr::Lit(e) => &e.attrs,
        syn::Expr::Loop(e) => &e.attrs,
        syn::Expr::Macro(e) => &e.attrs,
        syn::Expr::Match(e) => &e.attrs,
        syn::Expr::MethodCall(e) => &e.attrs,
        syn::Expr::Paren(e) => &e.attrs,
        syn::Expr::Path(e) => &e.attrs,
        syn::Expr::Range(e) => &e.attrs,
        syn::Expr::RawAddr(e) => &e.attrs,
        syn::Expr::Reference(e) => &e.attrs,
        syn::Expr::Repeat(e) => &e.attrs,
        syn::Expr::Return(e) => &e.attrs,
        syn::Expr::Struct(e) => &e.attrs,
        syn::Expr::Try(e) => &e.attrs,
        syn::Expr::TryBlock(e) => &e.attrs,
        syn::Expr::Tuple(e) => &e.attrs,
        syn::Expr::Unary(e) => &e.attrs,
        syn::Expr::Unsafe(e) => &e.attrs,
        syn::Expr::While(e) => &e.attrs,
        syn::Expr::Yield(e) => &e.attrs,
        _ => &[],
    }
}

/// Whether `stmt` carries exactly `#[cfg(test)]` on its own attributes — a `let`, a bare
/// expression, or a statement-position macro invocation, whichever of the three it is.
///
/// Codex's finding, against two different scans: `rustc` strips a `#[cfg(test)]`
/// statement from shipped code whichever of these three shapes it is, and the earlier
/// fix for this only ever checked [`syn::Stmt::Local`] — a bare `#[cfg(test)] match
/// nibble { .. };` expression statement, or a `#[cfg(test)] some_macro!();` statement,
/// were each still walked into and read as production code. An item statement is not
/// checked here at all: [`syn::Item`] is already excluded by every visitor's own
/// `visit_item` override, which this function's callers reach through the ordinary
/// per-statement dispatch for anything this returns `false` for.
fn stmt_is_cfg_test(stmt: &syn::Stmt) -> bool {
    match stmt {
        syn::Stmt::Local(local) => has_cfg_test(&local.attrs),
        syn::Stmt::Expr(expr, _) => has_cfg_test(expr_attrs(expr)),
        syn::Stmt::Macro(mac) => has_cfg_test(&mac.attrs),
        syn::Stmt::Item(_) => false,
    }
}

/// Whether `stmt` is a local item [`evaluate_block`]'s own collectors never bind a name
/// from — a type alias, a local `fn`, `struct`, `enum`, `trait`, `impl`, `use`, `mod` or
/// `static`, every local item kind but a `const`, which [`block_const_exprs`] already
/// reads. `rustc` allows every one of them inside a block, and `syn` reads a local `const`
/// as the identical `syn::Stmt::Item` shape as the rest, so `evaluate_block`'s own
/// statement count has to tell the two apart rather than treating every item as absent the
/// way [`stmt_is_cfg_test`] does.
///
/// Codex's finding: `{ type Value = u8; let value: Value = 0; value }` counted the type
/// alias against `rest` — `production_stmts` kept every item statement — while neither
/// `block_const_exprs` nor `block_let_exprs`/`block_let_statement_count` counted it at all,
/// since it binds no value either collector tracks. The mismatch made `evaluate_block`
/// refuse a block that was otherwise fully resolvable, and refusing is not a conservative
/// answer here: an unresolved pattern constant is exactly what lets a dense-table arm's own
/// `pattern_literal` come back empty, which is the shape the whole `integrity-check` scan
/// exists to catch rather than one it can afford to wave through. A transparent item is
/// filtered out of `production_stmts` the same way a `#[cfg(test)]` statement already is,
/// so the count it is compared against never counted it either.
const fn stmt_is_transparent_item(stmt: &syn::Stmt) -> bool {
    matches!(stmt, syn::Stmt::Item(item) if !matches!(item, syn::Item::Const(_)))
}

/// Strips a raw marker from every identifier in `stream`.
///
/// `#![allow(r#missing_docs)]` renders as `allow(r#missing_docs)` through
/// [`quote::ToTokens`] unless this runs first. That text does not equal a rule's
/// plain literal, so a raw marker used only to dodge a keyword would still silence
/// the lint it names (issue #90).
fn unraw_tokens(stream: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    stream.into_iter().map(unraw_token_tree).collect()
}

/// [`unraw_tokens`], one token at a time. A group's own delimiters keep their span;
/// only its contents are rewritten.
fn unraw_token_tree(tree: proc_macro2::TokenTree) -> proc_macro2::TokenTree {
    match tree {
        proc_macro2::TokenTree::Group(group) => {
            let mut replaced =
                proc_macro2::Group::new(group.delimiter(), unraw_tokens(group.stream()));
            replaced.set_span(group.span());
            proc_macro2::TokenTree::Group(replaced)
        }
        proc_macro2::TokenTree::Ident(ident) => proc_macro2::TokenTree::Ident(ident.unraw()),
        other => other,
    }
}

/// Render one attribute exactly as the old line scanner would have seen it.
///
/// The historical checks compared attributes as whitespace-free text
/// (`#![forbid(unsafe_code)]`); rendering the parsed attribute back to that spelling
/// keeps those comparisons meaningful without re-scanning source lines. A string
/// literal's interior keeps its characters but loses insignificant spacing, the same
/// way the old comment stripper treated it. Every identifier's raw marker is stripped
/// first, so `#[cfg(r#test)]` reads the same as `#[cfg(test)]` (issue #90).
fn attribute_text(attribute: &syn::Attribute) -> String {
    unraw_tokens(attribute.to_token_stream())
        .to_string()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// The crate-root inner attributes of `contents`, in source order.
///
/// Only `syn::File::attrs` is read: attributes on nested modules, functions, or
/// items are items' business, not the crate root's. A `#![allow]` buried in a
/// submodule therefore cannot satisfy a rule that asks about the crate (issue #51),
/// and a `reason = "/*"` string cannot swallow the attribute that follows it
/// (issue #108), because no comment stripping happens at all.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn inner_attributes(contents: &str) -> Result<Vec<String>, syn::Error> {
    let file = parse_rust(contents)?;
    Ok(file.attrs.iter().map(attribute_text).collect())
}

/// The `extern crate` declarations of `contents`, in source order.
///
/// The identifier is `syn`'s spelling, so `extern crate r#alloc;` reports `alloc`:
/// a raw identifier is the same crate under a different hat (issue #68/#90).
/// Visibility and attributes ride on the item, not the name, so neither can hide it.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn extern_crate_names(contents: &str) -> Result<Vec<String>, syn::Error> {
    let file = parse_rust(contents)?;
    Ok(file
        .items
        .iter()
        .filter_map(|item| match item {
            // `extern crate r#alloc;` is the same crate as `extern crate alloc;`
            // (issues #68/#90).
            syn::Item::ExternCrate(declaration) => Some(ident_name(&declaration.ident)),
            _ => None,
        })
        .collect())
}

/// One `use` binding: the name it introduces and the path it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseAlias {
    /// The name the importing file actually writes, e.g. `Pollable`.
    pub local: String,
    /// The path it stands for, as written, e.g. `["core", "future", "Future"]`.
    pub target: Vec<String>,
}

/// Every `use` binding in `contents`, file scope and inline modules alike.
///
/// Nested `use` trees are flattened: `use a::{b, c as d};` yields `b -> [a, b]` and
/// `d -> [a, c]`. Glob imports bind no name and contribute nothing; resolving a
/// name through a glob would need the exporting crate's item list, which `syn`
/// cannot see (residual limit, documented on [`resolved_path_uses`]).
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn use_aliases(contents: &str) -> Result<Vec<UseAlias>, syn::Error> {
    let file = parse_rust(contents)?;
    let mut aliases = Vec::new();
    collect_item_aliases(&file.items, &mut Vec::new(), &mut aliases);
    Ok(aliases)
}

fn collect_item_aliases(
    items: &[syn::Item],
    prefix: &mut Vec<String>,
    aliases: &mut Vec<UseAlias>,
) {
    for item in items {
        // A `#[cfg(test)]` import is not in the shipped code, so it resolves nothing —
        // the structural half of what `without_test_modules` did textually (issue #51).
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Use(use_item) => collect_tree_aliases(&use_item.tree, prefix, aliases),
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    collect_item_aliases(nested, prefix, aliases);
                }
            }
            _ => {}
        }
    }
}

fn collect_tree_aliases(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    aliases: &mut Vec<UseAlias>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(ident_name(&path.ident));
            collect_tree_aliases(&path.tree, prefix, aliases);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            // A raw identifier cannot spell `self`. This check needs no `ident_is`.
            if name.ident != "self" {
                aliases.push(UseAlias {
                    local: ident_name(&name.ident),
                    target: [prefix.clone(), vec![ident_name(&name.ident)]].concat(),
                });
            }
        }
        syn::UseTree::Rename(rename) => {
            aliases.push(UseAlias {
                local: ident_name(&rename.rename),
                target: [prefix.clone(), vec![ident_name(&rename.ident)]].concat(),
            });
        }
        syn::UseTree::Glob(_) => {}
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                collect_tree_aliases(tree, prefix, aliases);
            }
        }
    }
}

/// One path as written in the code, with `use` aliases resolved.
///
/// `segments` is the canonical spelling: `use core::future::Future as Pollable;`
/// followed by `impl Pollable for X` yields `["core", "future", "Future"]`.
/// Absolute paths (`::core::...`) and multi-segment heads the file never imported
/// are left as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// The path segments after alias resolution.
    pub segments: Vec<String>,
}

impl ResolvedPath {
    /// The last segment, i.e. the name the code actually refers to.
    #[must_use]
    pub fn last(&self) -> Option<&str> {
        self.segments.last().map(String::as_str)
    }
}

/// Every path written in `contents`, with the file's `use` aliases resolved.
///
/// The visitor skips `use` items themselves: importing a name is not using it.
/// Paths inside macro invocations are invisible to `syn`'s visitor, so a pinned
/// name spelled only inside a macro body is not reported (residual limit; the
/// current tree has no such case, and `cargo xtask check-layering` would fail
/// closed on the diff if one appeared).
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn resolved_path_uses(contents: &str) -> Result<Vec<ResolvedPath>, syn::Error> {
    struct PathVisitor<'aliases> {
        aliases: &'aliases [UseAlias],
        paths: Vec<ResolvedPath>,
    }

    impl<'ast> syn::visit::Visit<'ast> for PathVisitor<'_> {
        fn visit_item_use(&mut self, _use: &'ast syn::ItemUse) {}

        fn visit_item(&mut self, item: &'ast syn::Item) {
            // Test code is not shipped code: a path named only under `#[cfg(test)]`
            // constructs nothing the gate pins (issue #51).
            if has_cfg_test(item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.paths.push(ResolvedPath {
                segments: resolve_segments(path, self.aliases),
            });
            syn::visit::visit_path(self, path);
        }
    }

    let file = parse_rust(contents)?;
    let aliases = {
        let mut collected = Vec::new();
        collect_item_aliases(&file.items, &mut Vec::new(), &mut collected);
        collected
    };
    let mut visitor = PathVisitor {
        aliases: &aliases,
        paths: Vec::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.paths)
}

fn resolve_segments(path: &syn::Path, aliases: &[UseAlias]) -> Vec<String> {
    let mut segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    if path.leading_colon.is_some() {
        return segments;
    }
    if let Some(first) = segments.first() {
        if let Some(alias) = aliases.iter().find(|candidate| candidate.local == *first) {
            let mut resolved = alias.target.clone();
            resolved.extend(segments.drain(1..));
            return resolved;
        }
    }
    segments
}

/// The self types of every `impl <path ending in Future> for T` in `contents`.
///
/// The trait is matched on its resolved last segment, so `Future` imported under
/// any alias still identifies the implementor (issue #109). Implementations of a
/// different trait that merely ends in `Future` keep the old textual check's
/// verdict; only the `Future` that can be `.await`ed matters to the facade rule,
/// and the four pinned futures are all bare `impl Future for ...`.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn future_trait_implementors(contents: &str) -> Result<Vec<String>, syn::Error> {
    let file = parse_rust(contents)?;
    let mut aliases = Vec::new();
    collect_item_aliases(&file.items, &mut Vec::new(), &mut aliases);
    let mut implementors = Vec::new();
    collect_future_implementors(&file.items, &aliases, &mut implementors);
    Ok(implementors)
}

fn collect_future_implementors(
    items: &[syn::Item],
    aliases: &[UseAlias],
    implementors: &mut Vec<String>,
) {
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Impl(implementation) => {
                if let Some((_, trait_path, _)) = implementation.trait_.as_ref() {
                    let resolved = resolve_segments(trait_path, aliases);
                    if resolved.last().is_some_and(|last| last == "Future") {
                        if let syn::Type::Path(self_type) = implementation.self_ty.as_ref() {
                            if let Some(name) = self_type.path.segments.last() {
                                implementors.push(ident_name(&name.ident));
                            }
                        }
                    }
                }
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    collect_future_implementors(nested, aliases, implementors);
                }
            }
            _ => {}
        }
    }
}

/// How many `fn name` items `contents` declares, at any nesting depth.
///
/// Free functions, trait declarations, trait method defaults, and inherent methods
/// all count: the old `count_tokens(code, "fn name")` scan counted them all too,
/// and narrowing the count would silently un-count a declaration the gate used to
/// see. What changes is disambiguation: two declarations are ambiguity, not a
/// first-match-wins tiebreak a decoy can exploit (issue #62).
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn fn_declaration_count(contents: &str, name: &str) -> Result<usize, syn::Error> {
    let file = parse_rust(contents)?;
    Ok(count_fn_declarations(&file.items, name))
}

fn count_fn_declarations(items: &[syn::Item], name: &str) -> usize {
    items
        .iter()
        .map(|item| {
            match item {
            syn::Item::Fn(function) => usize::from(ident_is(&function.sig.ident, name)),
            syn::Item::Mod(module) => module
                .content
                .as_ref()
                .map_or(0, |(_, nested)| count_fn_declarations(nested, name)),
            syn::Item::Trait(trait_item) => trait_item
                .items
                .iter()
                .filter(|member| {
                    matches!(member, syn::TraitItem::Fn(function) if ident_is(&function.sig.ident, name))
                })
                .count(),
            syn::Item::Impl(implementation) => implementation
                .items
                .iter()
                .filter(|member| {
                    matches!(member, syn::ImplItem::Fn(function) if ident_is(&function.sig.ident, name))
                })
                .count(),
            _ => 0,
        }
        })
        .sum()
}

/// Whether `contents` declares `fn name` as a test the gate may count.
///
/// The function must carry `#[test]` itself, and `#[ignore]`, any `#[cfg(..)]`,
/// and any `#[cfg_attr(..)]` disqualify it: a test the compiler skips, or could
/// skip under some configuration, vouches for nothing (issue #97). Attributes are
/// read off the parsed item, so a doc comment between `#[test]` and the `fn`
/// no longer breaks the association the way the old line-above scan did.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn declares_countable_test(contents: &str, name: &str) -> Result<bool, syn::Error> {
    let file = parse_rust(contents)?;
    Ok(items_declare_countable_test(&file.items, name))
}

fn items_declare_countable_test(items: &[syn::Item], name: &str) -> bool {
    items.iter().any(|item| match item {
        syn::Item::Fn(function) => {
            ident_is(&function.sig.ident, name) && is_countable_test(&function.attrs)
        }
        syn::Item::Mod(module) => module
            .content
            .as_ref()
            .is_some_and(|(_, nested)| items_declare_countable_test(nested, name)),
        _ => false,
    })
}

fn is_countable_test(attributes: &[syn::Attribute]) -> bool {
    let mut tested = false;
    for attribute in attributes {
        let path = attribute.path();
        if path_is_ident(path, "test") {
            tested = true;
        }
        if path_is_ident(path, "ignore")
            || path_is_ident(path, "cfg")
            || path_is_ident(path, "cfg_attr")
        {
            return false;
        }
    }
    tested
}

/// Where the "inside" count of [`struct_literal_counts`] is taken.
#[derive(Debug, Clone, Copy)]
pub enum FnScope<'a> {
    /// No inside count; `inside` is 0.
    None,
    /// The first `fn` named `0` in source order, searching through inline modules and
    /// `impl` blocks. The old textual scan read the first text match the same way; the
    /// structural search cannot be fooled by the name appearing in a comment or string.
    FirstFn(&'a str),
    /// Every `fn` named `name` in the inherent `impl` blocks for `ty`, summed.
    InherentFns {
        /// The type whose inherent `impl` blocks are searched, e.g. `"Effect"`.
        ty: &'a str,
        /// The method name, e.g. `"schedule"`.
        name: &'a str,
    },
    /// Every inherent `impl` block for `ty`, summed.
    InherentImpls(&'a str),
}

/// How many struct literals name a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiteralCounts {
    /// Literals anywhere in the file.
    pub total: usize,
    /// Literals inside the [`FnScope`]'s function body or bodies.
    pub inside: usize,
}

/// Counts the struct literals in `contents` whose path's final segment is `name` — after
/// resolving the file's `use` aliases — in total and inside a function body.
///
/// `use Sealable as S;` followed by `S { .. }` counts (issue #99): the literal's path
/// resolves through the alias to the segments of `Sealable`'s import path, so the final
/// segment is `Sealable` whatever the construction site spells. Items under exactly
/// `#[cfg(test)]` are skipped, structurally — the old textual pipeline blanked them
/// after lexing comments and strings out, and `syn` sees attributes directly (issue #51).
///
/// Struct literals, not declarations or patterns: `syn` reads [`syn::ExprStruct`], so
/// `struct Sealable {`, `impl Sealable {`, `fn barrier(self) -> Sealable {`, and
/// `let Sealable { .. } = value` never count, where the old textual scan needed a
/// per-line exclusion list for the first three and counted the fourth.
///
/// A file the parser cannot read is an error, not zeros: the construction pins fail
/// closed on it, because a pin that cannot see is a pin that approves.
///
/// `Self` is not mapped to the implementing type: `Self { .. }` counts only when the
/// caller asks for `"Self"` by name. That mapping needs the surrounding `impl`'s type,
/// which is name resolution by another name (see the limits above).
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn struct_literal_counts(
    contents: &str,
    name: &str,
    inside: FnScope<'_>,
) -> Result<LiteralCounts, syn::Error> {
    struct Literals<'aliases> {
        aliases: &'aliases [UseAlias],
        name: String,
        count: usize,
    }

    impl<'ast> syn::visit::Visit<'ast> for Literals<'_> {
        fn visit_item(&mut self, node: &'ast syn::Item) {
            if has_cfg_test(item_attrs(node)) {
                return;
            }
            syn::visit::visit_item(self, node);
        }

        fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
            if has_cfg_test(impl_item_attrs(node)) {
                return;
            }
            syn::visit::visit_impl_item(self, node);
        }

        fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
            let resolved = resolve_segments(&node.path, self.aliases);
            if resolved
                .last()
                .is_some_and(|last| last.as_str() == self.name)
            {
                self.count = self.count.saturating_add(1);
            }
            syn::visit::visit_expr_struct(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut aliases = Vec::new();
    collect_item_aliases(&file.items, &mut Vec::new(), &mut aliases);

    let mut total = Literals {
        aliases: &aliases,
        name: name.to_owned(),
        count: 0,
    };
    total.visit_file(&file);

    let mut inside_count = 0_usize;
    for target in inside_targets(&file, &inside) {
        let mut visitor = Literals {
            aliases: &aliases,
            name: name.to_owned(),
            count: 0,
        };
        match target {
            InsideTarget::Block(block) => visitor.visit_block(block),
            InsideTarget::Impl(implementation) => visitor.visit_item_impl(implementation),
        }
        inside_count = inside_count.saturating_add(visitor.count);
    }

    Ok(LiteralCounts {
        total: total.count,
        inside: inside_count,
    })
}

/// Something [`struct_literal_counts`] can count literals inside of.
enum InsideTarget<'a> {
    /// A function body.
    Block(&'a syn::Block),
    /// An `impl` block, visited whole.
    Impl(&'a syn::ItemImpl),
}

/// The bodies [`FnScope`] selects, in source order.
fn inside_targets<'a>(file: &'a syn::File, scope: &FnScope<'a>) -> Vec<InsideTarget<'a>> {
    match *scope {
        FnScope::None => Vec::new(),
        FnScope::FirstFn(name) => {
            let mut blocks = Vec::new();
            fn_blocks(&file.items, name, &mut blocks);
            blocks.truncate(1);
            blocks.into_iter().map(InsideTarget::Block).collect()
        }
        FnScope::InherentFns { ty, name } => {
            let mut blocks = Vec::new();
            for implementation in inherent_impls(&file.items, ty) {
                for item in &implementation.items {
                    if has_cfg_test(impl_item_attrs(item)) {
                        continue;
                    }
                    if let syn::ImplItem::Fn(function) = item {
                        if ident_is(&function.sig.ident, name) {
                            blocks.push(InsideTarget::Block(&function.block));
                        }
                    }
                }
            }
            blocks
        }
        FnScope::InherentImpls(ty) => inherent_impls(&file.items, ty)
            .into_iter()
            .map(InsideTarget::Impl)
            .collect(),
    }
}

/// The bodies of every `fn name`, in source order through inline modules and `impl`
/// blocks, skipping `#[cfg(test)]`.
fn fn_blocks<'a>(items: &'a [syn::Item], name: &str, blocks: &mut Vec<&'a syn::Block>) {
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Fn(function) if ident_is(&function.sig.ident, name) => {
                blocks.push(&function.block);
            }
            syn::Item::Impl(implementation) => {
                for impl_item in &implementation.items {
                    if has_cfg_test(impl_item_attrs(impl_item)) {
                        continue;
                    }
                    if let syn::ImplItem::Fn(function) = impl_item {
                        if ident_is(&function.sig.ident, name) {
                            blocks.push(&function.block);
                        }
                    }
                }
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    fn_blocks(nested, name, blocks);
                }
            }
            _ => {}
        }
    }
}

/// The inherent `impl` blocks for `ty`, in source order through inline modules,
/// skipping `#[cfg(test)]`.
fn inherent_impls<'a>(items: &'a [syn::Item], ty: &str) -> Vec<&'a syn::ItemImpl> {
    let mut found = Vec::new();
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Impl(implementation)
                if implementation.trait_.is_none()
                    && self_ty_names(&implementation.self_ty, ty) =>
            {
                found.push(implementation);
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    found.extend(inherent_impls(nested, ty));
                }
            }
            _ => {}
        }
    }
    found
}

/// Whether `self_ty` is the bare named type, ignoring generics: `Sealable<C>` counts.
fn self_ty_names(self_ty: &syn::Type, ty: &str) -> bool {
    match self_ty {
        syn::Type::Path(typed) => typed
            .path
            .segments
            .last()
            .is_some_and(|segment| ident_is(&segment.ident, ty)),
        _ => false,
    }
}
/// A trait implementation: `impl [...] Trait for SelfType`.
///
/// The trait is `syn`'s path, so `impl core::convert::TryFrom<&[u8]> for RecordRef<'_>`
/// reports the full path and the caller decides what a qualifier means — no textual
/// prefix list (issue #51). A `const C: char = 'a'` generic is a const argument, not a
/// lifetime, because `syn` parses rather than scanning for quotes (issue #108).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitImpl {
    /// The trait path segments, e.g. `["core", "convert", "TryFrom"]`.
    pub trait_segments: Vec<String>,
    /// The trait's generic arguments on its final segment, rendered without spaces and
    /// with lifetimes erased, e.g. `<&[u8]>` for `TryFrom<&'a [u8]>`. Empty when the
    /// trait takes no generics. Lifetimes are incidental to the encoding comparison
    /// (issue #108's char-literal rule), so they are removed structurally.
    pub trait_generics: String,
    /// The self type as written, e.g. `RecordRef<'_>`.
    pub self_ty: String,
}

/// Erases lifetimes from a type: `&'a [u8]` becomes `&[u8]`.
///
/// The kernel-encoding rule compares trait generics against markers like
/// `TryFrom<&[u8]>`; a lifetime on the reference is incidental to whether the impl
/// converts bytes, and `rustfmt`'s multi-line headers keep the lifetime spelled out.
/// Done on the parsed syntax — lifetimes are removed structurally — so a character
/// literal (`'a'` as a const generic default) is never mistaken for one. That
/// distinction is exactly what the textual `erase_lifetimes` this replaces got wrong:
/// it ate the `'a` of the char literal and left the closing quote behind (issue #108).
struct EraseLifetimes;

impl syn::visit_mut::VisitMut for EraseLifetimes {
    fn visit_type_reference_mut(&mut self, node: &mut syn::TypeReference) {
        node.lifetime = None;
        syn::visit_mut::visit_type_reference_mut(self, node);
    }
}

/// Strips a raw marker from every identifier in a syntax tree, before it is rendered
/// with [`quote::quote!`].
///
/// `TryFrom<&[r#u8]>` renders its argument as `<&[r#u8]>` unless this runs first, and
/// that text does not equal the plain spelling [`check_kernel_owns_no_encoding`] looks
/// for — a raw marker used only to dodge a keyword would still hide the argument
/// (issue #90).
///
/// [`check_kernel_owns_no_encoding`]: crate::source::check_kernel_owns_no_encoding
struct UnrawIdents;

impl syn::visit_mut::VisitMut for UnrawIdents {
    fn visit_ident_mut(&mut self, ident: &mut syn::Ident) {
        *ident = ident.unraw();
    }
}

/// Every `impl Trait for Type` in `contents`, in source order.
///
/// Inherent impls (`impl Foo { ... }`) are not trait implementations and are skipped.
/// Items under exactly `#[cfg(test)]` are skipped, structurally — test code is not
/// shipped code.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn trait_impls(contents: &str) -> Result<Vec<TraitImpl>, syn::Error> {
    struct Impls {
        found: Vec<TraitImpl>,
    }

    impl<'ast> syn::visit::Visit<'ast> for Impls {
        fn visit_item(&mut self, node: &'ast syn::Item) {
            if has_cfg_test(item_attrs(node)) {
                return;
            }
            syn::visit::visit_item(self, node);
        }

        fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
            let Some((_, trait_path, _)) = &node.trait_ else {
                return;
            };
            let Some(last) = trait_path.segments.last() else {
                return;
            };
            let trait_segments: Vec<String> = trait_path
                .segments
                .iter()
                .map(|segment| ident_name(&segment.ident))
                .collect();
            let trait_generics = match &last.arguments {
                syn::PathArguments::AngleBracketed(args) => {
                    let rendered: Vec<String> = args
                        .args
                        .iter()
                        .filter_map(|arg| {
                            // A bare lifetime argument (`Foo<'a>`) is incidental to the
                            // comparison; dropping it keeps the rendering honest.
                            if matches!(arg, syn::GenericArgument::Lifetime(_)) {
                                return None;
                            }
                            let mut erased = arg.clone();
                            syn::visit_mut::VisitMut::visit_generic_argument_mut(
                                &mut EraseLifetimes,
                                &mut erased,
                            );
                            syn::visit_mut::VisitMut::visit_generic_argument_mut(
                                &mut UnrawIdents,
                                &mut erased,
                            );
                            Some(quote::quote!(#erased).to_string())
                        })
                        .collect();
                    format!("<{}>", rendered.join(",")).replace(' ', "")
                }
                syn::PathArguments::None | syn::PathArguments::Parenthesized(_) => String::new(),
            };
            let mut self_ty = (*node.self_ty).clone();
            syn::visit_mut::VisitMut::visit_type_mut(&mut UnrawIdents, &mut self_ty);
            self.found.push(TraitImpl {
                trait_segments,
                trait_generics,
                self_ty: quote::quote!(#self_ty).to_string().replace(' ', ""),
            });
            syn::visit::visit_item_impl(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = Impls { found: Vec::new() };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// Every macro `contents` invokes or defines, named by the path it uses, in source order.
///
/// Covers a `macro_rules!` definition, an item-position invocation (`foo! { ... }`), a
/// statement- or expression-position invocation (`foo!(...)`), or one in any other
/// position `syn` reaches. Items and `impl` members under exactly `#[cfg(test)]` are
/// skipped, the same structural exclusion [`trait_impls`] makes.
///
/// Codex's finding, against the checksum module's dense-match scan: nothing in this
/// module expands a macro, and neither does `syn` — this module's own contract says so.
/// A macro that expands to a dense `match` or to an array is therefore invisible to
/// [`match_expressions`] and to the textual array ban beside it, whatever it expands to,
/// because both read the syntax a macro invocation *is* rather than the syntax it
/// produces. This function does not try to see through one; the caller refuses every
/// macro the checksum module's own tree names, so a table hidden behind one is a build
/// failure rather than a gap nobody had reasoned about.
///
/// A single override of [`syn::visit::Visit::visit_macro`] reaches every position a
/// macro can appear in — item, statement, expression, and a `macro_rules!` definition's
/// own body — because each of `syn`'s per-position wrappers (`ItemMacro`, `StmtMacro`,
/// `ExprMacro`, and so on) carries one [`syn::Macro`] and the default visit for each
/// forwards to it.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn macro_uses(contents: &str) -> Result<Vec<String>, syn::Error> {
    struct Macros {
        found: Vec<String>,
    }

    impl<'ast> syn::visit::Visit<'ast> for Macros {
        fn visit_item(&mut self, node: &'ast syn::Item) {
            if has_cfg_test(item_attrs(node)) {
                return;
            }
            syn::visit::visit_item(self, node);
        }

        fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
            if has_cfg_test(impl_item_attrs(node)) {
                return;
            }
            syn::visit::visit_impl_item(self, node);
        }

        // Codex's finding: the item and `impl`-member exclusions above say nothing
        // about a *local* statement, so a `#[cfg(test)] assert_eq!(...);` — or any
        // other cfg-gated local, `rustc` strips the whole statement from shipped code
        // — inside an otherwise ordinary function was still walked into and its macro
        // reported as a second production one. [`stmt_is_cfg_test`] is the same check
        // [`MatchVisitor::visit_block`] makes, reused here rather than duplicated.
        fn visit_block(&mut self, node: &'ast syn::Block) {
            for stmt in &node.stmts {
                if stmt_is_cfg_test(stmt) {
                    continue;
                }
                self.visit_stmt(stmt);
            }
        }

        fn visit_macro(&mut self, node: &'ast syn::Macro) {
            let segments: Vec<String> = node
                .path
                .segments
                .iter()
                .map(|segment| ident_name(&segment.ident))
                .collect();
            self.found.push(segments.join("::"));
            syn::visit::visit_macro(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = Macros { found: Vec::new() };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// Every crate-anchored pattern `contents` writes — a path of two or more segments,
/// starting with `crate`, with no qualified-self half — in source order.
///
/// Items and `impl` members under exactly `#[cfg(test)]` are skipped, the same
/// structural exclusion [`macro_uses`] makes.
///
/// Codex's thirty-fifth-round finding: `resolve_anchored_single_segment` resolves
/// `crate::NAME` as always-unresolved, on purpose — the checksum module is itself
/// `crate::crc`, one submodule below the crate's true root, so `crate::NAME` names a
/// constant this scan's own tree never reaches. But `missing_value` and
/// `fully_dense_arm_patterns` (in `crate::source`) both fail an *entire* match as "not
/// dense" the moment any one arm's pattern is unresolved — correct for a stray
/// unresolvable pattern, and exactly the gap a real table could be built behind: sixteen
/// arms reading `crate::P0` through `crate::P15`, naming real constants this scan simply
/// cannot see the value of, pass every dense-match check here by never being recognised
/// as dense at all. A checksum module has no legitimate reason to pattern-match on its
/// own crate's root — every constant this scan needs to resolve lives inside the tree it
/// already reads — so rather than reasoning about what such a pattern might resolve to,
/// `check_checksum_module_crate_root_patterns` (also in `crate::source`) refuses its mere
/// presence outright, the same way [`macro_uses`]'s caller refuses a macro rather than
/// trying to see through it.
///
/// Codex's next-round finding: the original refusal only caught the *shortest* such
/// spelling — exactly `crate::NAME` — leaving a longer chain (`crate::indices::P0`) free
/// to name a constant just as far outside this scan's own tree, silently unresolved for
/// the identical reason. Widened to every crate-anchored path of two or more segments,
/// which is deliberately wider than strictly necessary: `crate::crc::indices::P0`,
/// explicitly re-stating the checksum module's own real prefix, is the one shape that
/// can legitimately resolve through the ordinary qualified-path lookup, and this refuses
/// it too rather than trying to tell the two apart — idiomatic Rust has no reason to
/// spell a path that way from inside the very module it names, and reasoning about which
/// crate-anchored chains are "real" is exactly the kind of resolving-instead-of-refusing
/// this function exists to avoid.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn crate_root_pattern_uses(contents: &str) -> Result<Vec<String>, syn::Error> {
    struct CrateRootPatterns {
        found: Vec<String>,
    }

    /// Every crate-anchored path (two or more segments, no qualified self, headed by
    /// `crate`) anywhere within `expr`'s own tree — the identical shape and width
    /// `visit_pat` below refuses in pattern position, found here in *expression* position
    /// instead.
    ///
    /// Codex's next-round finding: `const P0: u8 = crate::R0;` names a crate-root constant
    /// this scan cannot see the value of, exactly as much as a pattern spelled the same way
    /// does — but nothing here had ever looked at a `const` item's own *initializer*, only
    /// at where a path is *matched against*. `literal_or_const_value` already declines to
    /// resolve such a path (`resolve_anchored_single_segment` answers `crate::NAME` as
    /// always-unresolved, on purpose), so the constant simply stayed silently unresolved —
    /// and `missing_value`/`fully_dense_arm_patterns` fail a match's *entire* dense-table
    /// check the moment any one arm is unresolved, the identical gap
    /// [`const_call_initializer_uses`] already closes for a call. Refused outright here
    /// too, independent of whether anything ever pattern-matches on the constant: the
    /// whole expression tree is searched, not only its own top level, for the same reason
    /// `const_call_initializer_uses`'s own `contains_call` does.
    fn crate_anchored_paths(expr: &syn::Expr) -> Vec<String> {
        struct FindCrateAnchoredPaths {
            found: Vec<String>,
        }
        impl<'ast> syn::visit::Visit<'ast> for FindCrateAnchoredPaths {
            fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
                if node.qself.is_none() {
                    let segments: Vec<String> = node
                        .path
                        .segments
                        .iter()
                        .map(|segment| ident_name(&segment.ident))
                        .collect();
                    if segments.len() >= 2 && segments.first().map(String::as_str) == Some("crate")
                    {
                        self.found.push(segments.join("::"));
                    }
                }
                syn::visit::visit_expr_path(self, node);
            }
        }
        let mut finder = FindCrateAnchoredPaths { found: Vec::new() };
        finder.visit_expr(expr);
        finder.found
    }

    impl<'ast> syn::visit::Visit<'ast> for CrateRootPatterns {
        fn visit_item(&mut self, node: &'ast syn::Item) {
            if has_cfg_test(item_attrs(node)) {
                return;
            }
            if let syn::Item::Const(constant) = node {
                self.found.extend(crate_anchored_paths(&constant.expr));
            }
            syn::visit::visit_item(self, node);
        }

        fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
            if has_cfg_test(impl_item_attrs(node)) {
                return;
            }
            if let syn::ImplItem::Const(constant) = node {
                self.found.extend(crate_anchored_paths(&constant.expr));
            }
            syn::visit::visit_impl_item(self, node);
        }

        fn visit_block(&mut self, node: &'ast syn::Block) {
            for stmt in &node.stmts {
                if stmt_is_cfg_test(stmt) {
                    continue;
                }
                self.visit_stmt(stmt);
            }
        }

        // `syn` gives a path pattern (`Color::Red`) and a path *expression*
        // (`Color::Red` used as a value) the identical underlying type — `PatPath` is a
        // type alias for `ExprPath` — so `Pat::Path`'s own generated dispatch calls
        // `visit_expr_path` rather than a pattern-specific method, and overriding that
        // method would catch an ordinary `crate::foo()` call just as readily as a match
        // arm. `visit_pat` is overridden instead, matched on the `Path` variant
        // specifically, so only a path that is actually written in pattern position is
        // ever reported; the default recursion still runs afterward, unconditionally, so
        // a `crate::NAME` nested inside an or-pattern or a reference pattern is still
        // found by the same walk that reaches it.
        fn visit_pat(&mut self, node: &'ast syn::Pat) {
            if let syn::Pat::Path(path_expr) = node {
                if path_expr.qself.is_none() {
                    let segments: Vec<String> = path_expr
                        .path
                        .segments
                        .iter()
                        .map(|segment| ident_name(&segment.ident))
                        .collect();
                    // Codex's next-round finding: a chain of three or more segments
                    // (`crate::indices::P0`) is anchored at the crate root exactly as
                    // much as the bare two-segment form is, and is just as capable of
                    // naming a constant outside this scan's own tree — `crate::crc::X`,
                    // explicitly re-stating the tree's own real prefix, is the one shape
                    // that can legitimately resolve through the ordinary qualified-path
                    // lookup, and idiomatic Rust has no reason to spell a path that way
                    // from inside the very module it names. Widened from exactly two
                    // segments to two or more, so every crate-anchored pattern is refused
                    // outright rather than only its shortest spelling.
                    if segments.len() >= 2 && segments.first().map(String::as_str) == Some("crate")
                    {
                        self.found.push(segments.join("::"));
                    }
                }
            }
            syn::visit::visit_pat(self, node);
        }

        // Codex's next-round finding: `_ if crate::ALWAYS => value, _ => fallback` names a
        // crate-root path in a match *guard*, which is neither a `const` initializer
        // `visit_item`/`visit_impl_item` reach through `crate_anchored_paths`, nor a
        // pattern `visit_pat` reaches — a guard is `syn::Arm`'s own `guard` field, a plain
        // expression sitting beside the pattern rather than inside it. `resolve_pattern_path`
        // already declines a `crate::` path the identical way `resolve_anchored_single_segment`
        // does for one in pattern position, so the guard resolves to nothing and the arm it
        // guards reads as neither provably dead nor the real wildcard — but that only stops
        // *this* scan from mistaking a hidden table for a dense one, it does not report the
        // hidden path the way a pattern or a `const` initializer spelled the same way already
        // does. Every arm's own guard is searched the identical way a `const`'s initializer
        // already is, closing the one expression position within a `match` this visitor had
        // not reached yet.
        fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
            for arm in &node.arms {
                if has_cfg_test(&arm.attrs) {
                    continue;
                }
                if let Some((_, guard_expr)) = &arm.guard {
                    self.found.extend(crate_anchored_paths(guard_expr));
                }
            }
            syn::visit::visit_expr_match(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = CrateRootPatterns { found: Vec::new() };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// Every `const` `contents` declares whose initializer is a call expression, named by
/// the constant's own name, in source order.
///
/// Reaches any level `syn` reaches: file, module, `impl`, or a block's own local item.
/// Items and `impl` members under exactly `#[cfg(test)]` are skipped, the same
/// structural exclusion [`macro_uses`] makes.
///
/// Codex's thirty-ninth-round finding: `const P0: u8 = index(0);` through `P14` is
/// `Expr::Call`, which `literal_or_const_value` does not evaluate — deliberately, since
/// doing that in general means interpreting an arbitrary function body, which this scan
/// does not attempt — so `resolve_scope_consts` folds none of them and a table built this
/// way reads as unresolved on every arm, passing every dense-match check here by never
/// being recognised as dense at all. The same shape of gap [`crate_root_pattern_uses`]
/// closes for a crate-anchored pattern: rather than reasoning about what a call might
/// evaluate to, or which calls are safe to interpret, a `const` whose own initializer is
/// a call is refused outright, independent of whether anything ever pattern-matches on it.
///
/// Codex's next-round finding: the first version of this only looked for a call at the
/// initializer's own top level, seen through a `Paren` or a `Group` — the same two
/// wrappers `literal_or_const_value` sees through for every other shape it resolves —
/// but `const P0: u8 = { let value = index(0); value };` buries the call one level
/// deeper, inside a `let` statement `literal_or_const_value`'s own block handling does
/// not understand either (it only follows a block whose every leading statement is a
/// local `const` item), so neither function saw the call at all and the constant simply
/// stayed silently unresolved. The initializer's *whole* expression tree is now searched
/// for a call wherever it sits, not only at the top, which is the more conservative
/// answer named as the alternative to evaluating one: a `const` referencing a call
/// anywhere in how it computes its own value is refused, rather than this scan trying to
/// decide which levels of nesting are safe to see through and which are not.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn const_call_initializer_uses(contents: &str) -> Result<Vec<String>, syn::Error> {
    struct ConstCallInitializers {
        found: Vec<String>,
    }

    /// Whether `expr`'s own tree contains a call anywhere within it — a plain
    /// `f(..)`/`Type::method(..)` `Expr::Call`, or a `receiver.method(..)` `Expr::MethodCall`.
    ///
    /// Codex's next-round finding: `syn` gives a dot-call its own node kind rather than
    /// lowering it to `Expr::Call`, so `0u8.wrapping_add(0)` — a real, `const`-evaluable
    /// method call `rustc` folds before the match it feeds ever lowers, exactly like the
    /// free-function call this function already refuses to interpret — walked straight
    /// past the original `visit_expr_call`-only override. The same conservative answer this
    /// function already gives a free call is given to a method call too: refuse the
    /// initializer outright rather than deciding which method calls are safe to interpret.
    fn contains_call(expr: &syn::Expr) -> bool {
        struct FindCall {
            found: bool,
        }
        impl<'ast> syn::visit::Visit<'ast> for FindCall {
            fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
                self.found = true;
                syn::visit::visit_expr_call(self, node);
            }

            fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
                self.found = true;
                syn::visit::visit_expr_method_call(self, node);
            }
        }
        let mut finder = FindCall { found: false };
        finder.visit_expr(expr);
        finder.found
    }

    impl<'ast> syn::visit::Visit<'ast> for ConstCallInitializers {
        fn visit_item(&mut self, node: &'ast syn::Item) {
            if has_cfg_test(item_attrs(node)) {
                return;
            }
            if let syn::Item::Const(constant) = node {
                if contains_call(&constant.expr) {
                    self.found.push(ident_name(&constant.ident));
                }
            }
            syn::visit::visit_item(self, node);
        }

        fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
            if has_cfg_test(impl_item_attrs(node)) {
                return;
            }
            if let syn::ImplItem::Const(constant) = node {
                if contains_call(&constant.expr) {
                    self.found.push(ident_name(&constant.ident));
                }
            }
            syn::visit::visit_impl_item(self, node);
        }

        fn visit_block(&mut self, node: &'ast syn::Block) {
            for stmt in &node.stmts {
                if stmt_is_cfg_test(stmt) {
                    continue;
                }
                self.visit_stmt(stmt);
            }
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = ConstCallInitializers { found: Vec::new() };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// Every name a file uses: all identifiers in source order, and all paths with `use`
/// aliases resolved.
///
/// The kernel-boundary pin asks two different questions (issue #99), so it gets two
/// views. Its decisions are dotted paths (`Intent::Finished`): the caller matches them
/// by suffix against [`NameUses::paths`], so `crate::Intent::Finished` counts, and so
/// does `use kernel::Intent as I;` followed by `I::Finished` — the alias resolves to the
/// kernel's path. Its forbidden vocabulary is single words (`RecordKind`): the caller
/// matches those against [`NameUses::idents`], because a binding, a declaration, or an
/// import names the word as much as a path does, and an alias cannot hide the word —
/// `use kernel::RecordKind as RK;` spells it in the import itself, which the old textual
/// scan also saw.
///
/// Unlike [`resolved_path_uses`], paths inside `use` items are included: importing
/// `Intent::Finished` names the decision the same way writing it does, and the old
/// textual scan counted the import's text too. Items under exactly `#[cfg(test)]` are
/// skipped, structurally (issue #51).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameUses {
    /// Every identifier in the file, in source order.
    pub idents: Vec<String>,
    /// Every path in the file, with `use` aliases resolved.
    pub paths: Vec<ResolvedPath>,
}

impl NameUses {
    /// Whether any resolved path ends in the `::`-separated `decision`'s segments.
    ///
    /// A longer path ending in the same segments counts: `crate::Intent::Finished`
    /// decides from `Intent::Finished`, and the boundary the old textual scan drew was
    /// the same suffix.
    #[must_use]
    pub fn names_decision(&self, decision: &str) -> bool {
        let wanted: Vec<&str> = decision.split("::").collect();
        self.paths.iter().any(|path| {
            path.segments.len() >= wanted.len()
                && wanted
                    .iter()
                    .rev()
                    .zip(path.segments.iter().rev())
                    .all(|(want, have)| have == want)
        })
    }

    /// Whether `word` appears as an identifier anywhere in the file.
    #[must_use]
    pub fn names_word(&self, word: &str) -> bool {
        self.idents.iter().any(|ident| ident == word)
    }
}

/// [`NameUses`] for `contents`: every identifier and every alias-resolved path.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn name_uses(contents: &str) -> Result<NameUses, syn::Error> {
    struct Names<'aliases> {
        aliases: &'aliases [UseAlias],
        idents: Vec<String>,
        paths: Vec<ResolvedPath>,
    }

    impl<'ast> syn::visit::Visit<'ast> for Names<'_> {
        fn visit_item(&mut self, node: &'ast syn::Item) {
            if has_cfg_test(item_attrs(node)) {
                return;
            }
            syn::visit::visit_item(self, node);
        }

        fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
            if has_cfg_test(impl_item_attrs(node)) {
                return;
            }
            syn::visit::visit_impl_item(self, node);
        }

        fn visit_ident(&mut self, node: &'ast syn::Ident) {
            self.idents.push(ident_name(node));
        }

        fn visit_path(&mut self, node: &'ast syn::Path) {
            self.paths.push(ResolvedPath {
                segments: resolve_segments(node, self.aliases),
            });
            syn::visit::visit_path(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut aliases = Vec::new();
    collect_item_aliases(&file.items, &mut Vec::new(), &mut aliases);

    let mut names = Names {
        aliases: &aliases,
        idents: Vec::new(),
        paths: Vec::new(),
    };
    names.visit_file(&file);
    Ok(NameUses {
        idents: names.idents,
        paths: names.paths,
    })
}

/// Whether [`markdown_prose`] keeps the text of inline code spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineCode {
    /// Keep: `` `the-question-id` `` renders as `the-question-id`. Marker claims
    /// backtick their id, so claim scans keep the spans.
    Keep,
    /// Drop: `` `# Title` `` is literal text, not a heading. Structure scans drop
    /// the spans so a fenced example cannot satisfy a rule about the document.
    Drop,
}

/// The Markdown `contents` as the text a reader sees: fenced code blocks removed.
///
/// Parsed with `pulldown-cmark`, so both fence characters open and close blocks,
/// a fence inside a longer fence stays inside, and an indented fence is literal
/// text — the cases a line scan gets wrong (issues #51c, #51d). Structure the
/// rules read is preserved: headings keep their `#` markers and list items their
/// `- ` markers, so the ADR field and heading scans run unchanged on the result.
/// Inline code spans are kept or dropped per [`InlineCode`].
///
/// HTML comments are NOT stripped here: the call sites that need them gone strip
/// them first, exactly as before, because comment stripping has its own
/// fusion-avoiding semantics the Markdown parser must not second-guess.
#[must_use]
pub fn markdown_prose(contents: &str, inline_code: InlineCode) -> String {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

    let parser = Parser::new_ext(contents, Options::empty());
    let mut out = String::new();
    let mut in_fence = false;
    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                // Only fenced blocks are dropped: the old line scan never removed
                // indented code blocks, and narrowing what counts as code would
                // newly blind the gate to prose it used to read.
                if matches!(kind, CodeBlockKind::Fenced(_)) {
                    in_fence = true;
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if in_fence {
                    in_fence = false;
                    // A fence is a paragraph break; without it the text on either
                    // side fuses into one line the field scans would misread.
                    out.push('\n');
                }
            }
            Event::Code(code) => {
                if !in_fence && matches!(inline_code, InlineCode::Keep) {
                    out.push_str(&code);
                }
            }
            Event::Text(text) => {
                if !in_fence {
                    out.push_str(&text);
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                if !in_fence {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    for _ in 0..level as usize {
                        out.push('#');
                    }
                    out.push(' ');
                }
            }
            Event::Start(Tag::Item) => {
                if !in_fence {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("- ");
                }
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item)
            | Event::SoftBreak
            | Event::HardBreak
                if !in_fence =>
            {
                out.push('\n');
            }
            _ => {}
        }
    }
    out
}

/// Every `marker` claim in `contents`, in source order.
///
/// The shared reader for the marker lines the rules track: `Settles deferred
/// question:` and `Attests hardware target:`. Fenced code blocks are not claims —
/// `docs/adr/README.md` shows the marker syntax in a fenced example, and a rule
/// that read it as a claim would count documentation as evidence (issue #82).
/// Inline code spans are kept because the id itself is backticked. HTML comments are
/// dropped by the Markdown parser itself — block and inline alike never become prose —
/// so `contents` may be raw: a marker a reader cannot see claims nothing either way.
#[must_use]
pub fn claims_in(contents: &str, marker: &str) -> Vec<String> {
    markdown_prose(contents, InlineCode::Keep)
        .lines()
        .filter_map(|line| line.split_once(marker))
        .map(|(_, rest)| rest.trim().trim_matches('`').trim().to_owned())
        .filter(|claim| !claim.is_empty())
        .collect()
}

/// A function found by name: a free `fn` or a method in an `impl` block.
///
/// `used_call` pins bodies by name, and the `Scan::next` it pins is an `Iterator`
/// method, not a free function. The old textual scan found `fn name` wherever it sat;
/// a search that only sees `Item::Fn` reports "declares no `fn next`" for a method the
/// file does declare. Both shapes carry what callers need: the attributes (for
/// `#[cfg(test)]` and `#[test]` inspection) and the body as text (for call inspection).
#[derive(Clone)]
pub struct NamedFn {
    /// The attributes on the function or method.
    pub attrs: Vec<syn::Attribute>,
    /// The body rendered as text, with `::` normalized (see `block_text`).
    pub body: String,
}

/// Every `fn name` in `contents`, outside `#[cfg(test)]`, in source order.
///
/// The structural replacement for a first-match textual `fn name` lookup (issue #62): a
/// decoy declaration above the real one no longer decides the verdict while the real
/// body drifts unexamined. Inline modules are searched too — the old scan read the whole
/// file — and so are methods in `impl` blocks: the old scan found `fn next` wherever it
/// sat, and the `Scan::next` the routing pin reads is an `Iterator` method. `#[cfg(test)]`
/// items are skipped the way `without_test_modules` blanked them. A file the parser
/// cannot read declares nothing, which callers treat as a missing declaration: the
/// fail-closed direction.
///
/// Owned, because the parsed file is local: returning references into it does not
/// compile, and cloning the found items keeps the caller's choice of which body to
/// inspect.
#[must_use]
pub fn fns_named(contents: &str, name: &str) -> Vec<NamedFn> {
    fns_matching(contents, name, false)
}

/// The `fn name`s in `contents`, test-gated or not according to `include_test_gated`.
///
/// [`fns_named`] is the production view. [`declares_test`] passes `true`: a `#[cfg(test)]`
/// on the enclosing module must not disqualify a test declaration, exactly as the old
/// scan read only the attribute block above the `fn`.
fn fns_matching(contents: &str, name: &str, include_test_gated: bool) -> Vec<NamedFn> {
    let Ok(file) = parse_rust(contents) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    collect_fns_named(&file.items, name, include_test_gated, &mut found);
    found
}

/// The `fn name`s in `items`, through inline modules and `impl` blocks, in source order.
fn collect_fns_named(
    items: &[syn::Item],
    name: &str,
    include_test_gated: bool,
    found: &mut Vec<NamedFn>,
) {
    for item in items {
        match item {
            syn::Item::Fn(function) if ident_is(&function.sig.ident, name) => {
                if !include_test_gated && has_cfg_test(&function.attrs) {
                    continue;
                }
                found.push(NamedFn {
                    attrs: function.attrs.clone(),
                    body: block_text(&function.block),
                });
            }
            syn::Item::Impl(implementation) => {
                if !include_test_gated && has_cfg_test(&implementation.attrs) {
                    continue;
                }
                for inner in &implementation.items {
                    if let syn::ImplItem::Fn(method) = inner {
                        if ident_is(&method.sig.ident, name)
                            && (include_test_gated || !has_cfg_test(&method.attrs))
                        {
                            found.push(NamedFn {
                                attrs: method.attrs.clone(),
                                body: block_text(&method.block),
                            });
                        }
                    }
                }
            }
            syn::Item::Mod(module) => {
                if !include_test_gated && has_cfg_test(&module.attrs) {
                    continue;
                }
                if let Some((_, nested)) = module.content.as_ref() {
                    collect_fns_named(nested, name, include_test_gated, found);
                }
            }
            _ => {}
        }
    }
}

/// Whether `contents` declares a `fn name` that is a test nothing can skip.
///
/// A test carries `#[test]` and no attribute that can skip it: `#[ignore]`, `#[cfg(..)]`,
/// or `#[cfg_attr(.., ..)]` (issue #97). The attributes are the function's own — `syn`
/// attaches them to the item, so the old "contiguous `#[...]` block above the `fn`"
/// heuristic is gone, along with the comment, blank line, or doc attribute that could
/// break it.
///
/// Only the fn's own attributes count, exactly as the old scan read only the block above
/// the `fn`: a `#[cfg(test)]` on the enclosing module does not disqualify, and neither
/// does an `#[ignore]` on the test next door.
///
/// A file the parser cannot read declares no test: the failure-matrix check then reports
/// the row unvouched, which is the fail-closed direction.
#[must_use]
pub fn declares_test(contents: &str, name: &str) -> bool {
    // Every declaration, test-gated or not: the old scan read the attribute block above
    // the `fn` wherever the `fn` sat, and a `#[cfg(test)]` on the enclosing module must
    // not disqualify the declaration it gates.
    fns_matching(contents, name, true).iter().any(|function| {
        let mut tested = false;
        let mut skippable = false;
        for attr in &function.attrs {
            let Some(first) = attr.path().segments.first() else {
                continue;
            };
            if attr.path().segments.len() == 1 && ident_is(&first.ident, "test") {
                tested = true;
            }
            if ident_is(&first.ident, "ignore")
                || ident_is(&first.ident, "cfg")
                || ident_is(&first.ident, "cfg_attr")
            {
                skippable = true;
            }
        }
        tested && !skippable
    })
}

/// A module declared out of line, with the files it may live in.
pub struct ChildModule {
    /// The declared `mod` name.
    pub name: String,
    /// The names of every *inline* `mod { ... }` the declaring file nests this
    /// declaration inside, outermost first — empty for one declared at the file's own
    /// top level. `crc.rs`'s `mod outer { mod inner; }` gives `inner`'s own
    /// [`ChildModule`] an `inline_ancestors` of `["outer"]`, which is what lets a caller
    /// building this child's own module path write `outer::inner` rather than losing
    /// `outer` the way reading `name` alone would (Codex's finding).
    pub inline_ancestors: Vec<String>,
    /// Candidate workspace-relative paths for the child's file, in `rustc`'s probe
    /// order (see [`child_modules`]).
    pub candidates: Vec<String>,
    /// Whether the declaration is test-only: it carries exactly `#[cfg(test)]` itself,
    /// or it sits inside an inline module that does.
    pub test_gated: bool,
}

/// Lexically normalizes a `/`-separated path, resolving `.` and `..`.
///
/// `#[path = "../shared.rs"]` in `src/crc.rs` means `src/shared.rs`: without this, the
/// candidate `src/../shared.rs` never matches a `LayerSource` path and the module is
/// silently unscanned (issue #59). Purely lexical — no filesystem access — so `..`
/// above the root is dropped rather than escaping.
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                _ = parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// The out-of-line `mod`s declared in the file at `parent_path`, in source order.
///
/// `parent_path` is the declaring file's workspace-relative path. A plain `mod name;`
/// lives in the child directory — beside the parent when the parent is `mod.rs`,
/// otherwise in the directory named after the parent's file stem — as `name.rs` or
/// `name/mod.rs`, in `rustc`'s probe order. A `#[path = "..."]` attribute replaces the
/// file name, and at the top level `rustc` resolves it against the declaring file's own
/// directory: verified by compiling a nested probe (`lib.rs` → `mod crc;` → `crc.rs` →
/// `#[path = "tbl.rs"] mod table;` reads the sibling `src/tbl.rs`; `src/crc/tbl.rs` is
/// never consulted — the build fails without the sibling). A `#[path]` with directory
/// components is relative the same way, so `#[path = "crc/tbl.rs"]` in `src/crc.rs` reads
/// `src/crc/tbl.rs`.
///
/// That directory is not the declaring *file's* directory once the declaration sits
/// inside an inline `mod outer { ... }` — Codex's finding: `rustc` then resolves the
/// attribute against `outer`'s own directory (`src/crc/outer/`, the same one an
/// unattributed `mod table;` there would use) rather than against `crc.rs`'s. The two
/// only coincide at the top level, which is why `parent_dir` was ever the right answer to
/// begin with.
///
/// A `#[path]` attribute names exactly the file `rustc` reads (issue #59): no natural
/// directory fallback is offered, because a fallback would scan a file the compiler
/// never reads. Each candidate is lexically normalized, so `#[path = "../shared.rs"]`
/// matches the `shared.rs` beside the parent directory.
///
/// Inline modules have no file and are not returned, but the walk descends into them: a
/// `mod data;` inside `mod tests { ... }` lives under `tests/`, and inherits the outer
/// module's test-gating.
///
/// Residual limit: `mod` declarations inside function bodies are not descended into.
/// They are legal Rust but vanishingly rare, and the enclosing file's own array scan
/// still reads whatever they declare.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust. The caller fails
/// closed on this: an unreadable module tree is a violation, not an empty tree, because
/// a checksum that cannot see a file is a checksum that approves it unseen.
pub fn child_modules(parent_path: &str, contents: &str) -> Result<Vec<ChildModule>, syn::Error> {
    let file = parse_rust(contents)?;
    let parent_dir = parent_path
        .rsplit_once('/')
        .map(|(dir, _)| format!("{dir}/"))
        .unwrap_or_default();
    // Codex's finding: a crate root — `lib.rs` or `main.rs` — is the *other* file name
    // `rustc` gives this same-directory treatment, not only `mod.rs`. `collect_dependency_qualified_constants`
    // is this function's first caller ever to hand it a crate root rather than an ordinary
    // submodule file, and without this a dependency crate's own `pub mod transition;` in
    // `lib.rs` produced the candidate `src/lib/transition.rs` — a directory `rustc` never
    // creates for a crate root at all — so the declaration always resolved to
    // `ModuleTreeError::Missing` and the whole tree walk failed closed before it read a
    // single one of that crate's own enum declarations.
    let is_same_directory_root = parent_path == "mod.rs"
        || parent_path.ends_with("/mod.rs")
        || parent_path == "lib.rs"
        || parent_path.ends_with("/lib.rs")
        || parent_path == "main.rs"
        || parent_path.ends_with("/main.rs");
    let child_dir = if is_same_directory_root {
        parent_dir.clone()
    } else {
        let stem = parent_path
            .rsplit_once('/')
            .map_or(parent_path, |(_, file)| file)
            .trim_end_matches(".rs");
        format!("{parent_dir}{stem}/")
    };
    let mut collector = ChildModuleCollector {
        parent_dir: &parent_dir,
        child_dir,
        gated: false,
        inline_path: Vec::new(),
        found: Vec::new(),
    };
    for item in &file.items {
        collector.visit_item(item);
    }
    Ok(collector.found)
}

/// A [`syn::visit::Visit`] walk collecting every out-of-line `mod` reachable from `items`
/// — at item level, inside an inline `mod { ... }`, or nested arbitrarily deep inside a
/// function or method body (a block, an `if`, a `match` arm, a closure — anywhere `syn`'s
/// own grammar allows an item statement).
///
/// Codex's findings, across two rounds: the first version of this walk only read
/// `file.items` and never entered a function body at all. A second version added a
/// non-recursive scan of a function body's own top-level statements, and that version
/// had two more gaps Codex found in the same round: it never descended into a block
/// nested *inside* that body (an `if`, a `match` arm, a closure), so a `mod` declared
/// there was still unseen; and it passed the enclosing item's own gating into a function
/// body unconditionally, so a `#[cfg(test)] fn fixture() { .. }`'s own local modules were
/// read as shipping code. A `syn::visit::Visit` walk closes all three at once: its own
/// default recursion already reaches every nested block the grammar allows (an `if`'s
/// arms, a `match`'s, a closure's body, and so on, transitively), and overriding
/// `visit_item` once — generically, for every item kind rather than only `mod` and `fn` —
/// folds a cfg-tested item's own gating into everything beneath it before recursing.
struct ChildModuleCollector<'a> {
    parent_dir: &'a str,
    child_dir: String,
    gated: bool,
    inline_path: Vec<String>,
    found: Vec<ChildModule>,
}

impl<'ast> syn::visit::Visit<'ast> for ChildModuleCollector<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let was_gated = self.gated;
        self.gated = self.gated || has_cfg_test(item_attrs(item));
        syn::visit::visit_item(self, item);
        self.gated = was_gated;
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        let name = ident_name(&module.ident);
        if module.content.is_some() {
            // Inline: no file of its own, but its out-of-line children live under it.
            // `self.gated` already carries this module's own `#[cfg(test)]`, folded in by
            // `visit_item` above before it dispatched here.
            let nested_child_dir = format!("{}{name}/", self.child_dir);
            let saved_child_dir = std::mem::replace(&mut self.child_dir, nested_child_dir);
            self.inline_path.push(name);
            syn::visit::visit_item_mod(self, module);
            self.inline_path.pop();
            self.child_dir = saved_child_dir;
        } else {
            // A `#[path]` attribute resolves against the *declaring file's* own
            // directory at the top level (`parent_dir`), but stops being true the
            // moment the declaration sits inside an inline `mod { ... }`: `rustc` then
            // resolves it against that inline module's own directory instead, which is
            // `self.child_dir` by the time this is reached — extended with each inline
            // module's own name on the way in. `self.inline_path` being non-empty is
            // exactly "this declaration is nested inside at least one inline module" —
            // a function or a further-nested block carries the same `child_dir` and
            // `inline_path` its surrounding item-level module already had, since
            // neither is a directory-owning scope of its own, so this reads correctly
            // for a `mod` declared arbitrarily deep inside one.
            let path_base = if self.inline_path.is_empty() {
                self.parent_dir
            } else {
                self.child_dir.as_str()
            };
            let candidates = module.attrs.iter().find_map(path_attr_value).map_or_else(
                || {
                    vec![
                        format!("{}{name}.rs", self.child_dir),
                        format!("{}{name}/mod.rs", self.child_dir),
                    ]
                },
                // `rustc` consults exactly this one path (see above): no fallback.
                |path| vec![normalize_path(&format!("{path_base}{path}"))],
            );
            self.found.push(ChildModule {
                name,
                inline_ancestors: self.inline_path.clone(),
                candidates,
                test_gated: self.gated,
            });
        }
    }
}

/// The string value of a `#[path = "..."]` attribute, if present.
fn path_attr_value(attr: &syn::Attribute) -> Option<String> {
    if !path_is_ident(attr.path(), "path") {
        return None;
    }
    let syn::Meta::NameValue(named) = &attr.meta else {
        return None;
    };
    let syn::Expr::Lit(lit) = &named.value else {
        return None;
    };
    let syn::Lit::Str(value) = &lit.lit else {
        return None;
    };
    Some(value.value())
}

/// The body of a function or method block as text the token-based scans understand.
///
/// The statements rendered without the outer braces — the way `braced_body` returned
/// them — with `quote`'s spaces around `::` collapsed again: the call scans look for
/// `C::name(` and `name::<`, and the spaced rendering would hide both. What the scans
/// do with the text is unchanged; this is only the bridge from the resolved item back
/// to the textual analyses.
///
/// A raw marker is not stripped here (issue #90). It does not need to be: every
/// consumer matches a substring at a token boundary, and `#` is such a boundary, so
/// `r#stage(` still matches a scan for `stage(`. Strip it anyway if a future consumer
/// starts comparing this text for exact equality.
fn block_text(block: &syn::Block) -> String {
    let mut body = String::new();
    for stmt in &block.stmts {
        body.push_str(&stmt.to_token_stream().to_string());
        body.push(' ');
    }
    body.replace(" :: ", "::")
}

/// One `match` expression `match_expressions` found.
///
/// Wherever in the file it sits — a function body, a closure, a `const` initializer,
/// another match's own scrutinee or arm, anywhere an expression can appear.
pub struct FoundMatch {
    /// The scrutinee, rendered as token text.
    pub selector: String,
    /// Each arm, in source order.
    pub arms: Vec<FoundArm>,
}

/// One arm of a [`FoundMatch`].
pub struct FoundArm {
    /// Every integer value the arm's pattern covers — a literal, however based or
    /// suffixed, a path resolving to a `const` this same scan found declared with a
    /// literal initializer (`literal_or_const_value`'s reach), or more than one value
    /// when the pattern is an or-pattern (`0 | 1 => ..`) whose every alternative
    /// resolves. Empty for anything else, `_` included, or for an or-pattern with even
    /// one unresolved alternative.
    pub pattern: Vec<i128>,
    /// Whether the pattern is irrefutable the way a dense table's final arm needs to be —
    /// `_`, or an unguarded binding naming no known constant — since a plain binding
    /// matches everything a wildcard does and compiles to the identical lookup table.
    pub is_wild: bool,
    /// The callee name and the resolved argument value, when the arm's whole value is a
    /// call with exactly one argument. `None` for anything else, including a call whose
    /// argument this scan cannot resolve to a value.
    pub call: Option<(String, Option<i128>)>,
    /// Whether the arm's own pattern is confirmed to name an unsigned value — a literal
    /// suffixed `u8`..`u128`, or a bare path `resolve.unsigned` answers for — the same
    /// standing `is_definitely_unsigned` already gives an expression. `false` for
    /// anything this scan cannot make that determination for, ranges and or-patterns
    /// included, which is the safe default: it only ever *widens* what a window search
    /// is allowed to try, never what it accepts.
    ///
    /// Codex's finding: `window_layout`'s own wrapping search tries every value in
    /// turn as a candidate base, which is sound only because `lit_value`'s own
    /// two's-complement reinterpretation of a `u128` literal at or above `2^127` makes
    /// the wrap a real fact about the unsigned domain the literal came from — not a
    /// property of `i128` arithmetic in general. A match whose patterns are genuinely
    /// signed and merely happen to sit near both ends of `i128` (`i128::MIN`, a `i128`
    /// constant near `i128::MAX`, and `i128::MAX` itself) is not such a domain: nothing
    /// about those three values is adjacent, and treating them as a dense four-slot
    /// window the way a real `u128` wrap would be read a match `rustc` still lowers to
    /// ordinary comparisons as a lookup table. This field is what lets the window search
    /// tell the two apart before it decides whether wrapping past `i128::MIN` means
    /// anything at all.
    pub unsigned: bool,
}

/// Every `match` expression `contents` declares, anywhere one can appear, outside
/// `#[cfg(test)]`.
///
/// The structural replacement for a hand-rolled brace-and-comma scan (issue's own history
/// on pull request #154: by the time this replaced it, that scan had accumulated distinct
/// bypasses for a comma-less block arm, a block scrutinee, a postfixed block value, a cast,
/// an `else`, a scrutinee holding its own nested `match`, a scrutinee with its own
/// `if`/`else`, and a comma inside a turbofish — eight shapes `rustc`'s own grammar
/// disambiguates for free and no text scan built one rule at a time ever closes for good).
/// `syn` parses the real grammar, so every one of those shapes is handled without a rule of
/// its own, and one more besides: a constant pattern is resolved the same way a constant
/// argument is, against every `const` visible at the match's own position, rather than read
/// as "not a literal" and let through unclassified.
///
/// `contents` is walked *twice*. Codex's finding: a single pass populates `qualified` only
/// as it *reaches* each module, so a match textually before the `mod` it qualifies a
/// pattern against read every one of that module's constants as unresolved — Rust's own
/// item lookup does not care about declaration order, and a single left-to-right walk
/// cannot honour that. The first pass exists only to finish `qualified`, its own matches
/// discarded; the second is the real one, with every module-qualified constant `contents`
/// declares already in view regardless of where in the file it sits. `external_qualified`
/// seeds both passes, so a caller can fold in constants declared in another file of the
/// same checksum module tree — a qualified reference is not confined to the file that
/// declares the module it names.
///
/// Equivalent to [`match_expressions_with_prefix`] with an empty prefix — see that
/// function for what a non-empty one is for and why one file's own two-pass walk still
/// needs it.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust — the caller fails closed
/// on this, the same as every other structural query in this module.
#[allow(
    clippy::implicit_hasher,
    reason = "this crate never receives a caller-chosen hasher; every map it builds and \
              passes is `std::collections::HashMap`'s default, so generalising the \
              parameter buys no caller anything and only widens the signature"
)]
pub fn match_expressions(
    contents: &str,
    external_qualified: &std::collections::HashMap<String, i128>,
    external_qualified_unsigned: &std::collections::HashMap<String, bool>,
    external_qualified_types: &std::collections::HashMap<String, String>,
) -> Result<Vec<FoundMatch>, syn::Error> {
    match_expressions_with_prefix(
        contents,
        external_qualified,
        external_qualified_unsigned,
        external_qualified_types,
        &[],
    )
}

/// [`match_expressions`], with `prefix` seeded as the module path `contents`' own file sits
/// at.
///
/// The segments an out-of-line `mod name;` declared in some *other* file gives it, empty
/// for a file nothing declares this way — the same `prefix`
/// [`qualified_constants_with_prefix`] takes, for the same reason. Codex's finding: a
/// match's own patterns can be `super`- or module-relative too, and a
/// visitor that always started `module_path` at the file's own root had no way to answer
/// that correctly for a file that is not, itself, at the tree's root — an out-of-line
/// `crc/outer/inner.rs` using `super::indices::P0` needs `module_path` to start at
/// `["outer", "inner"]`, the position this file sits at in the tree, not at `[]`, or a
/// leading `super` pops from a path that was never there and resolves against nothing. The
/// constant-collection half of this already took a prefix; the match-visiting half is the
/// same fix applied to the same gap in the other reader of the same file.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust — the caller fails closed
/// on this, the same as every other structural query in this module.
#[allow(
    clippy::implicit_hasher,
    reason = "this crate never receives a caller-chosen hasher; every map it builds and \
              passes is `std::collections::HashMap`'s default, so generalising the \
              parameter buys no caller anything and only widens the signature"
)]
pub fn match_expressions_with_prefix(
    contents: &str,
    external_qualified: &std::collections::HashMap<String, i128>,
    external_qualified_unsigned: &std::collections::HashMap<String, bool>,
    external_qualified_types: &std::collections::HashMap<String, String>,
    prefix: &[String],
) -> Result<Vec<FoundMatch>, syn::Error> {
    let file = parse_rust(contents)?;
    let base = resolve_scope_consts(
        &OwnConsts {
            exprs: &item_const_exprs(&file.items),
            unsigned: &item_const_unsigned(&file.items),
            types: &item_const_types(&file.items),
        },
        &OuterScopes {
            values: &ConstScopes(Vec::new()),
            unsigned: &UnsignedConstScopes::default(),
        },
        external_qualified,
        external_qualified_unsigned,
        prefix,
        &[],
        &[],
    );
    let mut visitor = MatchVisitor {
        scopes: ConstScopes(vec![base]),
        scopes_unsigned: UnsignedConstScopes(vec![item_const_unsigned(&file.items)]),
        scopes_types: ConstTypeScopes(vec![item_const_types(&file.items)]),
        use_scopes: UseScopes(vec![item_use_imports(&file.items)]),
        module_path: prefix.to_vec(),
        module_scope_depths: Vec::new(),
        function_path: Vec::new(),
        block_path: Vec::new(),
        next_block_id: 0,
        self_type_path: Vec::new(),
        trait_defaults: std::collections::HashMap::new(),
        qualified: external_qualified.clone(),
        // Codex's next-round finding: this used to be seeded empty unconditionally, on the
        // reasoning that cross-file qualified constants carry no unsignedness of their own
        // — but `check_integrity_check_module_tree` already collects one, alongside
        // `qualified` itself, from every file of the tree before any file is checked for a
        // dense match; nothing here ever threaded it through. `bounds::HI` in one file,
        // `bounds::ZERO` in another, and `_ if bounds::HI < bounds::ZERO => ..` in a third
        // needed `HI`'s own `u128` declaration to reach the third file's own guard, and
        // this parameter is what carries it there — the identical map
        // `qualified_constants_with_prefix` now returns instead of discarding.
        qualified_unsigned: external_qualified_unsigned.clone(),
        // Codex's next-round finding: `path_declared_type` answered `None` for every
        // qualified reference, because nothing here tracked a qualified constant's own
        // declared type at all — only `qualified_unsigned`'s own bool did. Seeded empty at
        // first, the same first-round way `qualified_unsigned` itself once was — but
        // `check_integrity_check_module_tree` already collects one, alongside `qualified`
        // and `qualified_unsigned`, from every file of the tree before any file is checked
        // for a dense match; `bounds::OFF` declared in one file and referenced as
        // `!bounds::OFF` in a guard written in another needed `OFF`'s own `bool` declaration
        // to reach that guard, and this parameter is what carries it there — the identical
        // map `qualified_constants_with_prefix` now returns instead of discarding.
        qualified_types: external_qualified_types.clone(),
        found: Vec::new(),
    };
    visitor.visit_file(&file);
    visitor.found.clear();
    // The block-id counter is reset between passes so the second pass assigns the
    // identical ids to the identical blocks the first pass did — without this, the
    // second pass's own local-module keys would never match the first pass's, since
    // `next_block_id` would have kept climbing rather than reproducing the same walk.
    visitor.next_block_id = 0;
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// Every module-qualified `const` `contents` declares on its own, independent of whether any
/// match anywhere references it.
///
/// The half of [`match_expressions`]'s own two-pass resolution a caller needs *before*
/// scanning any file of a multi-file checksum module tree: Codex's finding is that a
/// qualified reference is not confined to the file that declares the module it names, so a
/// single file's own two-pass walk is not enough on its own — a caller combines every
/// file's own [`qualified_constants`] first and passes the result to [`match_expressions`]
/// as `external_qualified` for every file in the tree.
///
/// Equivalent to [`qualified_constants_with_prefix`] with an empty prefix and no
/// cross-file seed: for a file that is not itself the out-of-line body of a `mod` declared
/// somewhere else, and whose own constants reference nothing outside this file, that is the
/// whole answer, because every constant this function can see either sits at that file's
/// own root — with no module of its own to be qualified under — or inside a `mod { ... }`
/// this file declares inline, which `qualified_constants_with_prefix` already walks into
/// either way.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
#[allow(
    clippy::type_complexity,
    reason = "the value map and its unsignedness and declared-type mirrors, returned \
              together the same way they are threaded together through every caller — a \
              type alias would name the trio once more than the signature already does"
)]
pub fn qualified_constants(
    contents: &str,
) -> Result<
    (
        std::collections::HashMap<String, i128>,
        std::collections::HashMap<String, bool>,
        std::collections::HashMap<String, String>,
    ),
    syn::Error,
> {
    qualified_constants_with_prefix(
        contents,
        &[],
        &std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
    )
}

/// [`qualified_constants`], with two seeds a lone file cannot supply on its own.
///
/// `prefix` is the module path `contents`' own file sits at, and `external_qualified` is
/// every module-qualified constant already known from the rest of the tree — the
/// tree-wide accumulation `check_checksum_module_dense_matches`'s own caller builds.
///
/// `prefix` is the segments an out-of-line `mod name;` declared in some *other* file
/// gives it, empty for a file nothing declares this way. Codex's finding: an out-of-line
/// declaration's content lives in a different file than the `mod` keyword that names it,
/// so nothing before this ever told `contents`'s own root scope what module path it
/// answers to — `visit_item_mod` only records a module's constants when it can see that
/// module's content, and this file's root items are never inside a `mod { ... }` node *of
/// their own*. Seeding `prefix` closes that: the visitor's module path starts there
/// instead of empty, and this file's own top-level constants are recorded under it
/// explicitly, the one thing a walk of `contents` alone has no way to know on its own.
///
/// Codex's forty-fourth-round finding: this function's own doc comment claimed every
/// constant it can see "sits at that file's own root ... or inside a `mod { ... }` this
/// file declares inline" — true of where a constant is *declared*, and false of what its
/// *initializer* can reference. `pub const P0: u8 = super::base::BASE + 0;`, in an
/// out-of-line `indices.rs` whose sibling `base` module lives in a *different* file
/// entirely, names a constant this single-file walk has no way to see, because this
/// function used to seed `resolve_scope_consts` and the visitor's own `qualified` map from
/// nothing — no amount of file-traversal order fixed that, since each file was scanned in
/// isolation. `external_qualified` is now threaded through both, exactly the way
/// [`match_expressions_with_prefix`] already takes one, so a caller that has already
/// collected the rest of the tree's constants (or is doing so at a fixed point, since one
/// file's own dependency on another does not respect any particular scan order either) can
/// hand them to this pass too.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
#[allow(
    clippy::implicit_hasher,
    reason = "this crate never receives a caller-chosen hasher; every map it builds and \
              passes is `std::collections::HashMap`'s default, so generalising the \
              parameter buys no caller anything and only widens the signature"
)]
#[allow(
    clippy::type_complexity,
    reason = "the value map and its unsignedness and declared-type mirrors, returned \
              together the same way they are threaded together through every caller — a \
              type alias would name the trio once more than the signature already does"
)]
pub fn qualified_constants_with_prefix(
    contents: &str,
    prefix: &[String],
    external_qualified: &std::collections::HashMap<String, i128>,
    external_qualified_unsigned: &std::collections::HashMap<String, bool>,
    external_qualified_types: &std::collections::HashMap<String, String>,
) -> Result<
    (
        std::collections::HashMap<String, i128>,
        std::collections::HashMap<String, bool>,
        std::collections::HashMap<String, String>,
    ),
    syn::Error,
> {
    let file = parse_rust(contents)?;
    let item_unsigned = item_const_unsigned(&file.items);
    let item_types = item_const_types(&file.items);
    let base = resolve_scope_consts(
        &OwnConsts {
            exprs: &item_const_exprs(&file.items),
            unsigned: &item_unsigned,
            types: &item_types,
        },
        &OuterScopes {
            values: &ConstScopes(Vec::new()),
            unsigned: &UnsignedConstScopes::default(),
        },
        external_qualified,
        external_qualified_unsigned,
        prefix,
        &[],
        &[],
    );
    let mut qualified = external_qualified.clone();
    // Codex's next-round finding: `qualified_unsigned` used to be seeded empty
    // unconditionally here too, for the identical reason `match_expressions_with_prefix`'s
    // own seed was — but this is the function that *produces* the map
    // `check_integrity_check_module_tree` now collects across the whole tree, so declining
    // to record a locally-declared constant's own unsignedness here is what left every
    // caller with nothing to thread through in the first place. Inserted at the identical
    // key `qualified` itself gains, from the identical `item_unsigned` map `base`'s own
    // values were resolved against.
    let mut qualified_unsigned = external_qualified_unsigned.clone();
    // Codex's next-round finding: this still seeded `qualified_types` empty even once
    // `qualified_unsigned` was threaded through, because a declared type is not an
    // unsignedness — `bounds.rs` declaring `OFF: bool = false;` and a guard in `crc.rs`
    // spelled `!bounds::OFF` needs `OFF`'s own `bool` ascription to reach that guard's own
    // file, and only this function ever sees `bounds.rs`'s own `item_types`. Inserted at the
    // identical key `qualified` and `qualified_unsigned` themselves gain, from the identical
    // `item_types` map `base`'s own values were resolved against.
    let mut qualified_types = external_qualified_types.clone();
    if !prefix.is_empty() {
        for (name, value) in &base {
            qualified.insert(format!("{}::{name}", prefix.join("::")), *value);
            qualified_unsigned.insert(
                format!("{}::{name}", prefix.join("::")),
                item_unsigned.get(name).copied().unwrap_or(false),
            );
            if let Some(type_name) = item_types.get(name) {
                qualified_types.insert(format!("{}::{name}", prefix.join("::")), type_name.clone());
            }
        }
    }
    let mut visitor = MatchVisitor {
        scopes: ConstScopes(vec![base]),
        scopes_unsigned: UnsignedConstScopes(vec![item_unsigned]),
        scopes_types: ConstTypeScopes(vec![item_types]),
        use_scopes: UseScopes(vec![item_use_imports(&file.items)]),
        module_path: prefix.to_vec(),
        module_scope_depths: Vec::new(),
        function_path: Vec::new(),
        block_path: Vec::new(),
        next_block_id: 0,
        self_type_path: Vec::new(),
        trait_defaults: std::collections::HashMap::new(),
        qualified,
        qualified_unsigned,
        qualified_types,
        found: Vec::new(),
    };
    visitor.visit_file(&file);
    Ok((
        visitor.qualified,
        visitor.qualified_unsigned,
        visitor.qualified_types,
    ))
}

/// A stack of constant scopes, outermost first, each mapping a `const` name declared
/// directly in that scope to its resolved integer value.
///
/// Codex's finding: a single file-wide map lets a second module's or function's own local
/// `const P0 = 0;` be silently overwritten by an unrelated `const P0 = 99;` declared
/// elsewhere in the same file, so the first table's patterns resolve to the *wrong* file's
/// values — either hiding a real dense table behind values that no longer look dense, or the
/// reverse. Resolution has to respect the same lexical scoping `rustc` gives these names:
/// innermost declaration wins, and a name invisible from a given point (declared in a
/// sibling module or a different function) must not be resolved from there at all.
struct ConstScopes(Vec<std::collections::HashMap<String, i128>>);

impl ConstScopes {
    /// `name`'s value at the innermost scope that declares it, searching outward.
    fn resolve(&self, name: &str) -> Option<i128> {
        self.0
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    /// `name`'s value at the innermost scope within the first `depth` entries of the
    /// stack, searching outward from there — [`resolve`](Self::resolve) truncated to a
    /// specific ancestor rather than the whole live stack.
    ///
    /// Codex's finding: `super::P0` was resolved with a plain [`resolve`](Self::resolve),
    /// which searches the *entire* current stack — including scopes nested more deeply
    /// than the ancestor `super` actually names. A child module that shadows its parent's
    /// `P0` with a non-dense value of its own made a match in that child reading
    /// `super::P0` find the child's own shadowing value instead of the parent's, because
    /// the child's scope is innermost and a plain search never learns to skip past it.
    /// Truncating the stack to `depth` — the length it had right after the target ancestor
    /// module's own scope was pushed — excludes every scope nested inside that ancestor,
    /// so the search can only find that ancestor's own constant or one further outward.
    fn resolve_from(&self, name: &str, depth: usize) -> Option<i128> {
        self.0
            .get(..depth)?
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }
}

/// Mirrors [`ConstScopes`]'s own stack of scopes, one level for one, but records only
/// whether the name at that level was declared with an explicit unsigned integer type — the
/// one fact [`is_definitely_unsigned`] needs for a bare `Expr::Path` operand and
/// [`ConstScopes`]'s own value map does not carry, since every other reader of a resolved
/// constant wants only its value. Pushed and popped at the identical points `ConstScopes`
/// is, from the identical declarations ([`item_const_unsigned`] beside [`item_const_exprs`],
/// [`block_const_unsigned`] beside [`block_const_exprs`]), so a name present in one stack's
/// scope at a given index is present in the other's scope at the same index too — shadowing
/// agrees between the two stacks because both are built from the same declarations in the
/// same order.
#[derive(Default)]
struct UnsignedConstScopes(Vec<std::collections::HashMap<String, bool>>);

impl UnsignedConstScopes {
    /// `name`'s own declared-unsigned fact at the innermost scope that declares it,
    /// searching outward exactly as [`ConstScopes::resolve`] does — `false` once nothing
    /// records it, which only ever declines to fold an ordering guard rather than guessing
    /// one is unsigned when it might not be.
    fn resolve(&self, name: &str) -> bool {
        self.0
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .unwrap_or(false)
    }
}

/// [`UnsignedConstScopes`]'s own mirror for a bare name's declared *type name* rather than
/// only whether it is unsigned — [`ConstScopes`]'s own twin for the fact
/// [`evaluate_bitwise_not`] needs a match arm's own guard or pattern to answer too, not only
/// a `const`'s own sibling initializer.
///
/// Codex's next-round finding: `const OFF: bool = false; .. _ if !OFF => value, _ =>
/// fallback` names a guard `evaluate_bitwise_not` cannot fold — `OFF` is a bare path, not a
/// literal, and the `resolve.width` a match arm's own guard is resolved with had always been
/// a stub answering `None` unconditionally, on the reasoning that only a `const`'s own
/// initializer (resolved by [`resolve_scope_consts`]) ever needed a referenced constant's
/// declared width. That reasoning held for an *integer* width, where guessing wrong from a
/// value alone risks masking to the wrong number of bits, but excluded the one type this
/// scan already represents unambiguously either way: a `bool` is `0` or `1` in this scan's
/// own storage regardless of which way `!` is read, so the only thing missing was knowing
/// `OFF` is a `bool` at all. Pushed and popped at the identical points `scopes_unsigned` is,
/// from the identical declarations — [`item_const_types`] beside [`item_const_unsigned`],
/// [`block_const_types`] beside [`block_const_unsigned`].
#[derive(Default)]
struct ConstTypeScopes(Vec<std::collections::HashMap<String, String>>);

impl ConstTypeScopes {
    /// `name`'s own declared type name at the innermost scope that declares it, searching
    /// outward exactly as [`ConstScopes::resolve`] does — `None` once nothing records it,
    /// which only ever declines to fold a `!` operand rather than guessing its width.
    fn resolve(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).map(String::as_str))
    }
}

/// One scope level's own `use` declarations: the names they bind directly — after any `as`
/// rename — to the full path segments each names, and the prefixes any glob import in this
/// scope names.
#[derive(Default)]
struct UseScope {
    named: std::collections::HashMap<String, Vec<String>>,
    globs: Vec<Vec<String>>,
}

/// A stack of [`UseScope`]s, mirroring [`ConstScopes`].
///
/// Codex's finding: `use indices::{P0, P1};` brings `indices::P0` and `indices::P1` into
/// scope under the bare names `P0` and `P1`, and Rust resolves a pattern spelled that way
/// exactly as if the full path had been written out — but nothing here had ever read a
/// `use` item at all, so an imported constant used bare read as an unresolved binding.
struct UseScopes(Vec<UseScope>);

impl UseScopes {
    /// `name`'s imported target, at the innermost scope that binds it directly, searching
    /// outward — the same shadowing [`ConstScopes::resolve`] gives a bare constant, and the
    /// same approximation: a `use` is really only visible in the module that declares it,
    /// not automatically in every module nested inside it, but this searches the whole
    /// enclosing chain the way a bare constant reference already does here.
    fn resolve(&self, name: &str) -> Option<&[String]> {
        self.0
            .iter()
            .rev()
            .find_map(|scope| scope.named.get(name).map(Vec::as_slice))
    }

    /// Every glob import's own prefix visible from here, innermost scope first — the same
    /// shadowing order [`resolve`] searches a named import in.
    ///
    /// Codex's finding: `use indices::*;` was recorded nowhere at all, so a numbered
    /// pattern reached only through a glob read as an unresolved binding exactly as an
    /// unimported one would, and a match built entirely of glob-imported constants was
    /// invisible to the dense-match scan.
    fn glob_prefixes(&self) -> impl Iterator<Item = &[String]> {
        self.0
            .iter()
            .rev()
            .flat_map(|scope| scope.globs.iter().map(Vec::as_slice))
    }
}

/// The unevaluated initializer of every `const` declared *directly* in `items` — not
/// recursing into a nested `mod` or `fn`, each of which is its own scope and resolved
/// separately when [`MatchVisitor`] descends into it.
fn item_const_exprs(items: &[syn::Item]) -> std::collections::HashMap<String, syn::Expr> {
    items
        .iter()
        .filter_map(|item| {
            let syn::Item::Const(constant) = item else {
                return None;
            };
            (!has_cfg_test(&constant.attrs))
                .then(|| (ident_name(&constant.ident), (*constant.expr).clone()))
        })
        .collect()
}

/// [`item_const_exprs`]'s own mirror for [`UnsignedConstScopes`]: every `const` declared
/// *directly* in `items`, by name, against whether its own type ascription names one of the
/// five unsigned fixed-width integer types — not against the value it initializes to, which
/// this function never reads.
fn item_const_unsigned(items: &[syn::Item]) -> std::collections::HashMap<String, bool> {
    items
        .iter()
        .filter_map(|item| {
            let syn::Item::Const(constant) = item else {
                return None;
            };
            (!has_cfg_test(&constant.attrs)).then(|| {
                (
                    ident_name(&constant.ident),
                    declared_type_is_unsigned(&constant.ty),
                )
            })
        })
        .collect()
}

/// [`item_const_exprs`]'s own mirror for a *width*-aware sibling of [`UnsignedConstScopes`]:
/// every `const` declared *directly* in `items`, by name, against its own type ascription's
/// single-segment name — `"u8"`, `"i32"`, and so on — wherever it is one, not against the
/// value it initializes to.
///
/// Codex's next-round finding: `const Q0: u8 = 255; const P0: u8 = !Q0;` names an operand
/// [`is_definitely_unsigned`] already answers `true` for (`Q0`'s own declaration is
/// `u8`), but the `!` case in [`literal_or_const_value`] still required a suffixed literal
/// through [`as_suffixed_int_literal`] to learn a width to mask against, and a bare path has
/// no suffix of its own to read. Confirming a path is unsigned answers a different question
/// than knowing *how many bits* its own type has, and only the second is enough to negate it
/// correctly — `!255u8` and `!255u16` are different values. This is that width, read off the
/// identical declaration [`item_const_unsigned`] already reads a bare `true`/`false` out of,
/// scoped identically narrowly: only a sibling declared directly in the same item list, not a
/// name reached through an outer scope or a qualified path, which decline rather than guess.
fn item_const_types(items: &[syn::Item]) -> std::collections::HashMap<String, String> {
    items
        .iter()
        .filter_map(|item| {
            let syn::Item::Const(constant) = item else {
                return None;
            };
            if has_cfg_test(&constant.attrs) {
                return None;
            }
            single_segment_type_name(&constant.ty).map(|name| (ident_name(&constant.ident), name))
        })
        .collect()
}

/// Every fieldless enum variant `items` declares directly, by its own module-qualified name
/// under `prefix` — [`item_const_exprs`]'s own enum-variant twin, flat over `items` the
/// identical way, needed by `collect_dependency_qualified_constants` to resolve a match over
/// a *dependency* crate's own enum without running that crate's full match-expression scan,
/// which is what `MatchVisitor::visit_item_enum` already does for the checksum module's own
/// tree but has no reason to be asked of a crate this scan otherwise never reads at all.
///
/// A variant's own discriminant is either explicit — resolved only as far as a bare integer
/// literal or arithmetic over one, through [`literal_or_const_value`] handed a resolver that
/// answers nothing for any named path — or implicit: one more than the previous variant's
/// own value, `0` for the first. Scoped this narrowly on purpose: a dependency's own
/// discriminant that itself names one of that crate's other constants is a real gap this
/// leaves open rather than one closed by threading this scan's own cross-file `Resolve`
/// machinery across a crate boundary it was never designed to cross, and declining is the
/// same safe answer every other case this scan cannot confirm already gets. A variant
/// carrying fields is skipped, since a pattern can never name one this way; the running
/// counter still advances past it, matching how `rustc` numbers a mixed enum's own fieldless
/// variants. An unresolvable explicit discriminant stops the enum's own count rather than
/// guessing a wrong running value for every variant after it — `#[cfg(test)]` on the enum
/// item or on one variant is skipped exactly as `item_const_exprs` already skips one.
pub(crate) fn item_enum_variant_constants(
    items: &[syn::Item],
    prefix: &[String],
) -> std::collections::HashMap<String, i128> {
    let no_value = |_: &syn::Path| None;
    let not_unsigned = |_: &syn::Path| false;
    let no_width = |_: &syn::Path| None;
    let resolve = Resolve {
        value: &no_value,
        unsigned: &not_unsigned,
        width: &no_width,
    };
    let mut found = std::collections::HashMap::new();
    for item in items {
        let syn::Item::Enum(item_enum) = item else {
            continue;
        };
        if has_cfg_test(&item_enum.attrs) {
            continue;
        }
        let mut enum_path = prefix.to_vec();
        enum_path.push(ident_name(&item_enum.ident));
        let mut next: i128 = 0;
        for variant in &item_enum.variants {
            if has_cfg_test(&variant.attrs) {
                continue;
            }
            let Some(value) = (match &variant.discriminant {
                Some((_, expr)) => literal_or_const_value(expr, &resolve),
                None => Some(next),
            }) else {
                break;
            };
            if matches!(variant.fields, syn::Fields::Unit) {
                found.insert(
                    format!("{}::{}", enum_path.join("::"), ident_name(&variant.ident)),
                    value,
                );
            }
            let Some(successor) = value.checked_add(1) else {
                break;
            };
            next = successor;
        }
    }
    found
}

/// Every plainly-typed parameter `sig` declares, by name, against its own declared type's
/// single-segment name — the identical shape [`item_const_types`] and [`block_const_types`]
/// already answer for a `const` and a `let`, extended to a function's own signature so a
/// bare, unsuffixed literal pattern matched against a named parameter can inherit *its*
/// declared type the same way real Rust type inference gives it one.
///
/// Codex's next-round finding: a scrutinee declared `nibble: u128` settles what an
/// unsuffixed integer *pattern* against it means — `170141183460469231731687303715884105726`
/// with no suffix at all is a `u128` literal precisely because nothing else could type-check
/// against a `u128` scrutinee — but nothing here had ever read a function's own parameter
/// list, so every such pattern's own unsignedness answered `false` regardless of the
/// scrutinee it was matched against. Scoped narrowly, the way every declared-type map in
/// this file is: a parameter pattern that is not a bare identifier (`&self`, a destructured
/// tuple) answers for no name here, and a type this scan cannot reduce to one path segment
/// answers for none either.
fn fn_param_types(sig: &syn::Signature) -> std::collections::HashMap<String, String> {
    sig.inputs
        .iter()
        .filter_map(|arg| {
            let syn::FnArg::Typed(pat_type) = arg else {
                return None;
            };
            let syn::Pat::Ident(ident) = pat_type.pat.as_ref() else {
                return None;
            };
            single_segment_type_name(&pat_type.ty).map(|name| (ident_name(&ident.ident), name))
        })
        .collect()
}

/// [`fn_param_types`]'s own unsigned-only mirror, the identical shape
/// [`item_const_unsigned`] already answers for a `const`: every plainly-typed parameter
/// whose declared type is one of the five unsigned primitives, by name.
fn fn_param_unsigned(sig: &syn::Signature) -> std::collections::HashMap<String, bool> {
    fn_param_types(sig)
        .into_iter()
        .filter_map(|(name, type_name)| is_unsigned_type_name(&type_name).then_some((name, true)))
        .collect()
}

/// The unevaluated initializer of every associated `const` declared directly in an
/// inherent `impl`'s own item list — `impl Indices { const P0: u8 = 0; ... }` — mirroring
/// [`item_const_exprs`] for the other place a scannable constant is declared.
///
/// Codex's finding: `Indices::P0` is spelled exactly like a module-qualified constant and
/// resolves the same way in Rust, but nothing here had ever looked at an
/// [`syn::ImplItem::Const`] — every visited `impl` block's own consts went unrecorded, so
/// a dense table keyed on associated constants read as unresolved on every arm.
fn impl_const_exprs(items: &[syn::ImplItem]) -> std::collections::HashMap<String, syn::Expr> {
    items
        .iter()
        .filter_map(|item| {
            let syn::ImplItem::Const(constant) = item else {
                return None;
            };
            (!has_cfg_test(impl_item_attrs(item)))
                .then(|| (ident_name(&constant.ident), constant.expr.clone()))
        })
        .collect()
}

/// The unevaluated initializer of every associated `const` a `trait`'s own body gives a
/// *default* value — `trait Indices { const P0: u8 = 0; .. }` — mirroring
/// [`impl_const_exprs`] for the other place an associated constant's value can come from.
///
/// Codex's finding: `impl Indices for u8 {}`, implementing a trait every one of whose
/// constants already carries a default, redeclares none of them — legal Rust, and the
/// unqualified spelling `u8::P0` (or the qualified `<u8 as Indices>::P0`) still names the
/// trait's own default value `0` — but `impl_const_exprs` reads only what an impl's own
/// item list *redeclares*, so an impl that overrides nothing indexed nothing at all, and
/// every arm of a table keyed this way read as unresolved. `MatchVisitor::visit_item_trait`
/// is what indexes these, once per trait, so `MatchVisitor::visit_item_impl` can start a
/// trait impl's own scope from the trait's defaults and let the impl's own redeclarations
/// (if any) override them. A constant the trait leaves with no default at all
/// (`const P0: u8;`, `constant.default` is `None`) is not this function's to guess a value
/// for; it stays unresolved unless the impl itself supplies one, exactly as before.
fn trait_const_exprs(items: &[syn::TraitItem]) -> std::collections::HashMap<String, syn::Expr> {
    items
        .iter()
        .filter_map(|item| {
            let syn::TraitItem::Const(constant) = item else {
                return None;
            };
            let (_, default) = constant.default.as_ref()?;
            (!has_cfg_test(&constant.attrs)).then(|| (ident_name(&constant.ident), default.clone()))
        })
        .collect()
}

/// The unevaluated initializer of every `const` declared *directly* as a local item
/// statement in `block` — a function body's own `const P0: u8 = 0;`, which Codex found the
/// first version of this scan missed entirely by only ever walking `syn::Item::Mod`.
fn block_const_exprs(block: &syn::Block) -> std::collections::HashMap<String, syn::Expr> {
    block
        .stmts
        .iter()
        .filter_map(|stmt| {
            let syn::Stmt::Item(syn::Item::Const(constant)) = stmt else {
                return None;
            };
            (!has_cfg_test(&constant.attrs))
                .then(|| (ident_name(&constant.ident), (*constant.expr).clone()))
        })
        .collect()
}

/// [`block_const_exprs`]'s own mirror for [`UnsignedConstScopes`], the same way
/// [`item_const_unsigned`] mirrors [`item_const_exprs`].
fn block_const_unsigned(block: &syn::Block) -> std::collections::HashMap<String, bool> {
    block
        .stmts
        .iter()
        .filter_map(|stmt| {
            let syn::Stmt::Item(syn::Item::Const(constant)) = stmt else {
                return None;
            };
            (!has_cfg_test(&constant.attrs)).then(|| {
                (
                    ident_name(&constant.ident),
                    declared_type_is_unsigned(&constant.ty),
                )
            })
        })
        .collect()
}

/// [`block_const_exprs`]'s own mirror for the width-aware [`item_const_types`], the same way
/// [`block_const_unsigned`] mirrors [`item_const_unsigned`].
fn block_const_types(block: &syn::Block) -> std::collections::HashMap<String, String> {
    block
        .stmts
        .iter()
        .filter_map(|stmt| {
            let syn::Stmt::Item(syn::Item::Const(constant)) = stmt else {
                return None;
            };
            if has_cfg_test(&constant.attrs) {
                return None;
            }
            single_segment_type_name(&constant.ty).map(|name| (ident_name(&constant.ident), name))
        })
        .collect()
}

/// The unevaluated initializer of every name a plain `let PATTERN = EXPR;` declared
/// *directly* as a statement in `block` binds — every name [`destructured_binding`] finds in
/// `PATTERN`, which may be more than one for an `@` binding.
///
/// Codex's next-round finding: `const P0: u8 = { let value = 0; value };` is exactly as
/// resolvable as the local-`const` block [`evaluate_block`] already folds, but a `let`
/// statement is not a local *item* at all (`syn::Stmt::Local`, not `syn::Stmt::Item`), so
/// [`block_const_exprs`] never saw it and the block stayed unresolved — the const-call
/// backstop does not catch it either, since a bare `let` is neither a call. The initializer
/// must be a plain `= EXPR` with no `let-else` diverge arm, whose value depends on a branch
/// this scan does not evaluate. A `let mut` is not excluded — nothing in this narrow shape
/// lets it be reassigned, since a bare assignment statement is neither this nor a local
/// `const` item, and [`evaluate_block`]'s own statement-count check already refuses a block
/// holding one.
///
/// Codex's next-round finding: an `@` binding such as `let whole @ _ignored = 0u8;` binds
/// *two* names to the identical value, and only one of them can be the single entry this
/// function's own `filter_map` used to keep — whichever [`destructured_binding`] happened to
/// return. Flattened instead: every name a pattern legally introduces is inserted, so a
/// caller reading either the outer name or a name bound inside its sub-pattern finds it.
fn block_let_exprs(block: &syn::Block) -> std::collections::HashMap<String, syn::Expr> {
    block
        .stmts
        .iter()
        .flat_map(|stmt| {
            let syn::Stmt::Local(local) = stmt else {
                return Vec::new();
            };
            if has_cfg_test(&local.attrs) {
                return Vec::new();
            }
            let Some(init) = local.init.as_ref() else {
                return Vec::new();
            };
            if init.diverge.is_some() {
                return Vec::new();
            }
            destructured_binding(&local.pat, &init.expr)
        })
        .collect()
}

/// The count of every plain `let PATTERN = EXPR;` statement declared *directly* in `block`
/// whose pattern [`destructured_binding`] finds at least one name in — the statement-counted
/// twin of [`block_let_exprs`]'s own flattened, name-counted map.
///
/// Codex's next-round finding: once [`destructured_binding`] could return more than one name
/// for a single `@`-bound statement, [`block_let_exprs`]'s own `len()` stopped being a
/// statement count — a `let whole @ _ignored = 0u8;` contributes two entries from one
/// statement, and [`evaluate_block`]'s own `rest.len()` check compares against the number of
/// *statements* `block.stmts` actually holds. This is that count instead, scoped the
/// identical way `block_let_exprs` itself is filtered, so the two stay in lock-step with
/// what each one is really counting.
fn block_let_statement_count(block: &syn::Block) -> usize {
    block
        .stmts
        .iter()
        .filter(|stmt| {
            let syn::Stmt::Local(local) = stmt else {
                return false;
            };
            if has_cfg_test(&local.attrs) {
                return false;
            }
            let Some(init) = local.init.as_ref() else {
                return false;
            };
            if init.diverge.is_some() {
                return false;
            }
            !destructured_binding(&local.pat, &init.expr).is_empty()
        })
        .count()
}

/// Every name `pat` binds, each paired with the value `expr` initializes it to — unwrapping
/// a type ascription (`Pat::Type`), an `@` sub-pattern (`Pat::Ident` with `subpat: Some(..)`)
/// and a tuple destructure (`Pat::Tuple`) of any arity, element-wise, the same way
/// [`literal_or_const_value`]'s own `Expr::Tuple` case and [`match_arm_matches_constant`]'s
/// own `Pat::Tuple` case already do for a scrutinee and a pattern — no bindings at all for
/// `_` or an unrecognised shape, since neither binds a name this scan could later resolve a
/// reference to.
///
/// Codex's next-round finding: `const P0: u8 = { let (x,) = (0u8,); x };` names a `let` whose
/// pattern is `Pat::Tuple` rather than `Pat::Ident`, which the version of this function
/// scoped to exactly `Pat::Type`-then-`Pat::Ident` fell through to `_ => None` for — not
/// "declined and left the block's statement count refusing it", which would still be safe,
/// but silently *excluded from the map entirely* while [`evaluate_block`]'s own statement
/// count still expected one fewer bound name than the block actually declared, so the whole
/// block read as unresolved and the constants it feeds stayed empty rather than folding.
/// A one-element tuple pattern over a one-element tuple *initializer* destructures to
/// exactly the inner pattern and the inner expression, recursed through the identical way a
/// scrutinee or an arm pattern already is; an initializer that is not itself a literal
/// one-element tuple expression is declined rather than guessed at, since this scan folds
/// no other tuple shape into a value it could hand back here.
///
/// Codex's next-round finding: `let _whole @ (x,) = (0u8,); x;` is `Pat::Ident` naming
/// `_whole` with a `subpat` of `(x,)` — an irrefutable `@` binding, where the outer name and
/// the sub-pattern both bind against the identical value. The guard above was written to
/// decline a sub-pattern outright rather than to recurse into it, which fell through to
/// `_ => None` the same way an unhandled `Pat::Tuple` used to: not merely leaving `x`
/// unresolved, but dropping the whole statement from [`block_let_exprs`]'s map while
/// [`block_ignored_let_count`] does not count it either — an `@` binding is not the wildcard
/// `is_wildcard` looks for — so the statement went uncounted anywhere and the whole block
/// read as unresolved. This function now recurses into the sub-pattern against the same
/// `expr` an outer bare identifier would have bound to, the same way it already recurses
/// into a one-element tuple's own inner pattern.
///
/// Codex's next-round finding: `let whole @ _ignored = 0u8; whole;` names the identical `@`
/// shape with the *outer* name referenced instead of the inner one — this function recorded
/// only the sub-pattern's own binding (`_ignored`), on the reasoning that a name meant to be
/// discarded is the far more common shape of an `@` binding used only to destructure, but
/// Rust does not care which of the two a caller goes on to read, and the outer name is a
/// real binding exactly as much as the inner one is. Every binding a pattern legally
/// introduces is now returned, not one: an `@` pattern yields the outer name *and* whatever
/// its sub-pattern yields, and a pattern binding no name at all (`_`, or an unrecognised
/// shape) yields none. [`block_let_exprs`] flattens every call's own bindings into its map
/// instead of keeping one entry per statement, and [`block_let_statement_count`] is the
/// statement-counting half [`evaluate_block`]'s own `rest.len()` check now reads instead of
/// counting binding names, since one statement can bind more than one name.
///
/// Codex's next-round finding: `let (x, _) = (0u8, ()); x;` names a *two*-element tuple
/// pattern over a two-element tuple initializer, which the version of this function scoped
/// to exactly one element fell through to `_ => Vec::new()` for — the identical shape of
/// gap the one-element case itself closed, one arity wider. Generalised to any arity: a
/// tuple pattern and a tuple expression of the identical length are paired element-wise,
/// each pair recursed through the identical way a one-element tuple's own single pair
/// already was, and every element's own bindings are flattened together — `_` contributing
/// none of its own, the same way it already does outside a tuple. An arity mismatch, or an
/// initializer that is not itself a literal tuple expression, is declined rather than
/// guessed at, since this scan folds no other tuple shape into a value it could hand back
/// here.
fn destructured_binding(pat: &syn::Pat, expr: &syn::Expr) -> Vec<(String, syn::Expr)> {
    match pat {
        syn::Pat::Type(pat_type) => destructured_binding(&pat_type.pat, expr),
        syn::Pat::Ident(ident) => {
            let mut bindings = vec![(ident_name(&ident.ident), expr.clone())];
            if let Some((_, subpat)) = &ident.subpat {
                bindings.extend(destructured_binding(subpat, expr));
            }
            bindings
        }
        syn::Pat::Tuple(pat_tuple) => {
            let syn::Expr::Tuple(expr_tuple) = strip_parens(expr) else {
                return Vec::new();
            };
            if pat_tuple.elems.len() != expr_tuple.elems.len() {
                return Vec::new();
            }
            pat_tuple
                .elems
                .iter()
                .zip(expr_tuple.elems.iter())
                .flat_map(|(inner_pat, inner_expr)| destructured_binding(inner_pat, inner_expr))
                .collect()
        }
        _ => Vec::new(),
    }
}

/// `local`'s own name and declared type, when it binds a single, bare name with no tuple and
/// no `@` sub-pattern — `None` for anything else, which [`block_let_exprs`]'s own
/// `destructured_binding` accepts (a tuple or `@` pattern among them) but this cannot answer
/// a width for. The type itself comes from an explicit `let NAME: TYPE = EXPR;` ascription
/// when `local` carries one, and otherwise from `init`'s own initializer expression — a
/// suffixed literal's suffix or a cast's destination type, the identical fallback
/// [`expr_declared_width`] already gives an arm-bound name matched against such an
/// expression, read here for a `let`-bound one instead.
///
/// Codex's next-round finding: `const P0: u8 = { let q: u8 = 255; !q };` names an operand
/// [`evaluate_block`]'s own `local_resolve_width`/`block_resolve_width` closures declined
/// unconditionally for any name already resolved as one of the block's own locals, on the
/// reasoning that this function never read a declared type for one — true before this
/// existed, since [`block_let_exprs`]'s own `destructured_binding` unwraps and discards
/// `Pat::Type` rather than keeping it. `q`'s own ascription states the width
/// `evaluate_bitwise_not` needs exactly as plainly as a local `const`'s own type does.
///
/// Codex's next finding: a whole-block collector answering this once per *name*, the way an
/// earlier version of this function did, is sound for a block declaring each name once and
/// unsound for one that shadows a name — `let mut x: u8 = 128 + n; x <<= 1; let x: u16 = if
/// x < 100 { n } else { 10 * n + 1 }; x` has two declarations of `x` with two different
/// widths, and folding both into one `HashMap` entry keyed by name let whichever declaration
/// [`std::iter::Iterator::collect`] visited *last* answer for both — the second, wider `x:
/// u16` was credited to the *first* `x <<= 1`, which really shifts an 8-bit value and
/// truncates where a 16-bit shift would not. [`resolve_block_sequential`] now calls this once
/// per `let` statement and updates its own `local_types` map at exactly the point the
/// statement executes, so a read before the shadow sees the old width and a read after it
/// sees the new one — the identical ordering fix already applied to *values*, extended to
/// the type metadata a value's own arithmetic is read against.
///
/// Codex's next finding after that: `let mut x = 128u8; x <<= 1; ..` names no ascription at
/// all, only a suffixed initializer — this function answered `None` for it, so
/// `apply_compound_assignment` folded the shift with no declared type, which
/// [`plain_assign_operator_text`]'s own caller passes straight through as an *unsuffixed*
/// synthetic literal. Rust performs that shift at `x`'s real, inferred width and truncates
/// accordingly; an unsuffixed synthetic literal has no width for
/// [`evaluate_shl_op`]/[`evaluate_shr_op`] to truncate against, so the fold produces the
/// *untruncated* value where a suffix or an ascription would have produced the real one —
/// wrong rather than merely unresolved, and past the finding this doc comment already
/// records for a fully unascribed shadow, which stays `None` because nothing here can name a
/// width for it either. `expr_declared_width` is what closes it: called on `init`'s own
/// expression whenever `local`'s own pattern carries no ascription, so a suffixed literal or
/// a cast still states the width `stmt_let_type` could not otherwise see.
fn stmt_let_type(
    local: &syn::Local,
    init: &syn::LocalInit,
    resolve: &Resolve<'_>,
) -> Option<(String, String)> {
    let (pat, ascribed) = match &local.pat {
        syn::Pat::Type(pat_type) => (
            pat_type.pat.as_ref(),
            single_segment_type_name(&pat_type.ty),
        ),
        other => (other, None),
    };
    let syn::Pat::Ident(ident) = pat else {
        return None;
    };
    if ident.subpat.is_some() {
        return None;
    }
    let ty = ascribed.or_else(|| expr_declared_width(&init.expr, resolve))?;
    Some((ident_name(&ident.ident), ty))
}

/// The count of every plain `let _ = EXPR;` declared *directly* as a statement in `block` —
/// a value-discarding binding, counted rather than resolved, since nothing later in the
/// block can reference a name a wildcard pattern never bound.
///
/// Codex's next-round finding: `const P0: u8 = { let _ = core::marker::PhantomData::<()>; 0
/// };` holds a statement [`block_let_exprs`] correctly leaves out of its own map — its
/// pattern is `_`, not a name that function's own binding rule can bind — but
/// [`evaluate_block`]'s own statement-count check has no way to tell "a statement this scan
/// cannot fold" from "a statement that folds to nothing on purpose", so the whole block was
/// refused rather than only a block genuinely holding the former. Counted here instead of
/// resolved: a real `let _ = EXPR;` never reads `EXPR`'s own value again, so this scan does
/// not need to fold it either — only to know the statement was legitimately accounted for.
/// Scoped the same way [`block_let_exprs`] is: no `#[cfg(test)]` statement, and no
/// `let-else` diverge arm, whose reachability this scan does not decide.
fn block_ignored_let_count(block: &syn::Block) -> usize {
    fn is_wildcard(pat: &syn::Pat) -> bool {
        match pat {
            syn::Pat::Type(pat_type) => is_wildcard(&pat_type.pat),
            syn::Pat::Wild(_) => true,
            _ => false,
        }
    }

    block
        .stmts
        .iter()
        .filter(|stmt| {
            let syn::Stmt::Local(local) = stmt else {
                return false;
            };
            if has_cfg_test(&local.attrs) {
                return false;
            }
            let Some(init) = local.init.as_ref() else {
                return false;
            };
            if init.diverge.is_some() {
                return false;
            }
            is_wildcard(&local.pat)
        })
        .count()
}

/// The count of every `let PATTERN = EXPR else { DIVERGE };` statement declared *directly*
/// in `block` — counted here regardless of whether [`resolve_let_else_binding`] can actually
/// judge the pattern, the same way [`block_while_statement_count`] and
/// [`block_if_statement_count`] count every loop and conditional statement structurally
/// before either scan attempts to fold it. A block genuinely holding one this scan cannot
/// resolve still refuses — through `resolve_let_else_binding`'s own `None`, propagated by
/// `resolve_block_sequential`'s `?` — but it refuses *there*, on what the pattern and its
/// scrutinee actually are, rather than here, on the mere shape of the statement.
fn block_let_else_statement_count(block: &syn::Block) -> usize {
    block
        .stmts
        .iter()
        .filter(|stmt| {
            let syn::Stmt::Local(local) = stmt else {
                return false;
            };
            if has_cfg_test(&local.attrs) {
                return false;
            }
            let Some(init) = local.init.as_ref() else {
                return false;
            };
            init.diverge.is_some()
        })
        .count()
}

/// The plain, non-assigning operator `assign_op` desugars to (`AddAssign` to `+`, and so on)
/// — one of the ten compound-assignment [`syn::BinOp`] variants, spelled as the source text
/// [`apply_compound_assignment`] parses back into a real [`syn::BinOp`] to build a synthetic
/// binary expression from, rather than this scan reimplementing ten operators' worth of
/// arithmetic a second time.
const fn plain_assign_operator_text(assign_op: &syn::BinOp) -> Option<&'static str> {
    match assign_op {
        syn::BinOp::AddAssign(_) => Some("+"),
        syn::BinOp::SubAssign(_) => Some("-"),
        syn::BinOp::MulAssign(_) => Some("*"),
        syn::BinOp::DivAssign(_) => Some("/"),
        syn::BinOp::RemAssign(_) => Some("%"),
        syn::BinOp::BitXorAssign(_) => Some("^"),
        syn::BinOp::BitAndAssign(_) => Some("&"),
        syn::BinOp::BitOrAssign(_) => Some("|"),
        syn::BinOp::ShlAssign(_) => Some("<<"),
        syn::BinOp::ShrAssign(_) => Some(">>"),
        _ => None,
    }
}

/// `current_value assign_op= rhs_expr`'s own new value — a local's own compound assignment
/// (`x += 1;`), folded by reusing [`literal_or_const_value`]'s own operator dispatch rather
/// than duplicating any of it: a synthetic `syn::Expr::Binary` is built from `current_value`
/// re-spelled as a literal (carrying `declared_type`'s own suffix when one is known, so a
/// sign- or width-sensitive operator downstream reads the identical type information the
/// real assignment's left-hand side would have had) and `rhs_expr` reused unchanged, joined
/// by the plain operator [`plain_assign_operator_text`] names, then evaluated exactly as any
/// other binary expression in this scan already would be.
///
/// Codex's finding: `let mut x = 0; x += 1; x - 1` names a mutation
/// [`evaluate_block`]'s own local-resolution loop had no representation for at all — that
/// loop treats each local as one static initializer expression, which is sound for a `let`
/// or a `const` and says nothing about a statement that changes one afterward. Refusing
/// silently would have been the safe answer if there were no way to fold the mutation at
/// all, but there is one, and a `None` here reaches the identical failure mode the whole
/// scan exists to catch: an unresolved pattern constant lets a dense-table arm's own
/// `pattern_literal` come back empty, missed rather than reported.
fn apply_compound_assignment(
    current_value: i128,
    declared_type: Option<&str>,
    assign_op: &syn::BinOp,
    rhs_expr: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    let op_text = plain_assign_operator_text(assign_op)?;
    let op: syn::BinOp = syn::parse_str(op_text).ok()?;
    let left_text = declared_type.map_or_else(
        || current_value.to_string(),
        |ty| format!("{current_value}{ty}"),
    );
    let left_expr: syn::Expr = syn::parse_str(&left_text).ok()?;
    let synthetic = syn::Expr::Binary(syn::ExprBinary {
        attrs: Vec::new(),
        left: Box::new(left_expr),
        op,
        right: Box::new(rhs_expr.clone()),
    });
    literal_or_const_value(&synthetic, resolve)
}

/// `expr`'s own assignment target and right-hand side, when it is one — a bare,
/// single-segment path on the left of a plain `=` or one of the ten compound-assignment
/// operators [`plain_assign_operator_text`] recognises, seen through any nesting of
/// parentheses. The operator is `None` for a plain `=` (`syn::Expr::Assign`) and `Some` for
/// a compound one (`syn::Expr::Binary`, one of the ten `*Assign` variants) — two distinct
/// `syn` shapes unified here because both replace a single local's value in source order,
/// which is all [`resolve_block_sequential`] needs to treat them identically. `None` for
/// anything else: an assignment to a field, an index, or a dereference names no single local
/// this scan tracks at all.
///
/// Codex's finding: `x = x - 100;` is `syn::Expr::Assign`, a shape this function used to
/// decline outright — its own previous doc comment named it explicitly as "a different
/// question this function does not answer", which stopped being true the moment a plain
/// reassignment needed answering too. A plain assignment's own new value needs no
/// synthesised binary the way a compound one does: its right-hand side is evaluated against
/// the scope exactly as any other expression here already is, and a self-reference on that
/// right-hand side (`x = x - 100`) reads the *pre-assignment* value of `x` correctly for the
/// identical reason a compound assignment's own right-hand side does — [`resolve_block_sequential`]
/// resolves both against `resolved` before this statement's own write lands in it.
fn mutation_target(expr: &syn::Expr) -> Option<(String, Option<syn::BinOp>, syn::Expr)> {
    match strip_parens(expr) {
        syn::Expr::Assign(assign) => {
            let syn::Expr::Path(path) = strip_parens(&assign.left) else {
                return None;
            };
            let ident = path.path.get_ident()?;
            Some((ident_name(ident), None, (*assign.right).clone()))
        }
        syn::Expr::Binary(binary) => {
            plain_assign_operator_text(&binary.op)?;
            let syn::Expr::Path(path) = strip_parens(&binary.left) else {
                return None;
            };
            let ident = path.path.get_ident()?;
            Some((ident_name(ident), Some(binary.op), (*binary.right).clone()))
        }
        _ => None,
    }
}

/// Every assignment statement `block` declares directly, in source order — the
/// mutation-statement twin of [`block_let_exprs`]/[`block_const_exprs`], read the identical
/// way: a `#[cfg(test)]`-gated one is skipped, matching every other collector here and
/// `MatchVisitor::visit_block`'s own production walk.
fn block_mutation_stmts(block: &syn::Block) -> Vec<(String, Option<syn::BinOp>, syn::Expr)> {
    block
        .stmts
        .iter()
        .filter(|stmt| !stmt_is_cfg_test(stmt))
        .filter_map(|stmt| {
            let syn::Stmt::Expr(expr, Some(_)) = stmt else {
                return None;
            };
            mutation_target(expr)
        })
        .collect()
}

/// [`block_mutation_stmts`]'s own statement count — [`block_let_statement_count`]'s twin,
/// for the identical reason `evaluate_block`'s own `rest.len()` invariant needs one: every
/// compound-assignment statement `rest` counts also has to be counted on the side answering
/// for it, or a block holding one refuses as unresolved regardless of whether the mutation
/// itself folds.
fn block_mutation_statement_count(block: &syn::Block) -> usize {
    block_mutation_stmts(block).len()
}

/// `block`'s own production statements minus its last one — the block's own value-tail,
/// which `evaluate_block` resolves separately through `literal_or_const_value` rather than
/// through any of the three scanners beside this one.
///
/// Codex's finding: `{ let mut x = 128u8; x <<= 1; if x == 0 { n } else { n * 10 } }` names a
/// block whose own *tail* is a value-producing `if`/`else` — a legitimate expression, not a
/// statement, since it is the block's last one and carries no semicolon. `block_if_stmts`'s
/// own scanner (and `block_while_stmts`'s and `block_nested_block_stmts`'s beside it, sharing
/// the identical shape) used to scan `block.stmts` whole, so a tail that happened to share
/// one of these three forms was counted as a *statement* on top of being resolved separately
/// as the tail — one more than `rest.len()` could ever match, refusing a block that resolves
/// cleanly. Excluding the tail here, the same way `evaluate_block`'s own `rest` already does,
/// is what keeps the two sides counting the identical set of statements.
fn block_non_tail_stmts(block: &syn::Block) -> Vec<&syn::Stmt> {
    match production_stmts(block).split_last() {
        Some((_, rest)) => rest.to_vec(),
        None => Vec::new(),
    }
}

/// Every `while` loop statement `block` declares directly, in source order —
/// [`block_mutation_stmts`]'s own loop twin, read the identical way: a `#[cfg(test)]`-gated
/// one is skipped. A `while` loop needs no trailing semicolon to stand as a statement (it is
/// one of the handful of expression forms `rustc` accepts bare in statement position), so
/// both `Stmt::Expr` shapes are matched here rather than only the `Some(_)` one
/// `block_mutation_stmts` requires of an assignment.
fn block_while_stmts(block: &syn::Block) -> Vec<&syn::ExprWhile> {
    block_non_tail_stmts(block)
        .into_iter()
        .filter_map(|stmt| {
            let syn::Stmt::Expr(expr, _) = stmt else {
                return None;
            };
            let syn::Expr::While(while_expr) = strip_parens(expr) else {
                return None;
            };
            Some(while_expr)
        })
        .collect()
}

/// [`block_while_stmts`]'s own statement count — `evaluate_block`'s `rest.len()` invariant's
/// missing term. Codex's finding: a block whose only statements are `const`/`let`
/// declarations, a `while` loop mutating one of them, and a tail expression was refused
/// outright, because nothing on either side of that invariant ever counted the loop
/// statement — the block always looked one statement longer than its own name-counting
/// terms could account for, whatever the loop's own condition and body folded to.
fn block_while_statement_count(block: &syn::Block) -> usize {
    block_while_stmts(block).len()
}

/// Every bare, unlabelled `{ .. }` block statement `block` declares directly, in source
/// order — [`block_while_stmts`]'s own nested-scope twin. A labelled block is excluded, the
/// same way [`resolve_block_sequential`]'s own arm is: a `break 'a value;` inside one can
/// produce a value from a control-flow path this scan does not trace.
fn block_nested_block_stmts(block: &syn::Block) -> Vec<&syn::Block> {
    block_non_tail_stmts(block)
        .into_iter()
        .filter_map(|stmt| {
            let syn::Stmt::Expr(expr, _) = stmt else {
                return None;
            };
            let syn::Expr::Block(nested) = strip_parens(expr) else {
                return None;
            };
            nested.label.is_none().then_some(&nested.block)
        })
        .collect()
}

/// [`block_nested_block_stmts`]'s own statement count — `evaluate_block`'s `rest.len()`
/// invariant's other missing term. Codex's finding: `{ let mut x = 100; { x -= 100; } x }`
/// names a block whose statements are a `let` and a bare nested block — the identical shape
/// `block_while_statement_count` was added for, one syntax over: nothing on either side of
/// the invariant ever counted a nested block statement, so a block holding one refused
/// outright regardless of what running it would have computed.
fn block_nested_block_statement_count(block: &syn::Block) -> usize {
    block_nested_block_stmts(block).len()
}

/// Every `if`/`else` statement `block` declares directly, in source order —
/// [`block_while_stmts`]'s own conditional-scope twin.
fn block_if_stmts(block: &syn::Block) -> Vec<&syn::ExprIf> {
    block_non_tail_stmts(block)
        .into_iter()
        .filter_map(|stmt| {
            let syn::Stmt::Expr(expr, _) = stmt else {
                return None;
            };
            let syn::Expr::If(if_expr) = strip_parens(expr) else {
                return None;
            };
            Some(if_expr)
        })
        .collect()
}

/// [`block_if_stmts`]'s own statement count — `evaluate_block`'s `rest.len()` invariant's
/// third missing term. Codex's finding: `{ let mut x: u8 = n * 10; if x > 0 { x /= 10; } x }`
/// names a block whose statements are a `let` and a top-level `if` with no `else` — the
/// identical shape `block_while_statement_count` and `block_nested_block_statement_count`
/// were each added for, one syntax further over: nothing on either side of the invariant
/// ever counted the conditional statement, so a block holding one refused outright
/// regardless of which branch running it would have taken.
fn block_if_statement_count(block: &syn::Block) -> usize {
    block_if_stmts(block).len()
}

/// Every `use` declared *directly* in `items`, flattened into one [`UseScope`] — not
/// recursing into a nested `mod` or `fn`, each of which is its own scope, the same split
/// [`item_const_exprs`] makes for a `const`.
fn item_use_imports(items: &[syn::Item]) -> UseScope {
    let mut scope = UseScope::default();
    for item in items {
        let syn::Item::Use(use_item) = item else {
            continue;
        };
        if has_cfg_test(&use_item.attrs) {
            continue;
        }
        flatten_use_tree(&use_item.tree, &mut Vec::new(), &mut scope);
    }
    scope
}

/// Every `use` declared *directly* as a local item statement in `block` — a function
/// body's own `use indices::P0;` — the same split [`block_const_exprs`] makes for a
/// `const`.
fn block_use_imports(block: &syn::Block) -> UseScope {
    let mut scope = UseScope::default();
    for stmt in &block.stmts {
        let syn::Stmt::Item(syn::Item::Use(use_item)) = stmt else {
            continue;
        };
        if has_cfg_test(&use_item.attrs) {
            continue;
        }
        flatten_use_tree(&use_item.tree, &mut Vec::new(), &mut scope);
    }
    scope
}

/// Walks one `use` declaration's tree, recording every leaf it binds into `scope` —
/// `prefix` is the path segments accumulated so far.
///
/// A glob (`use indices::*;`) binds no *name* here: expanding one to the names it actually
/// exports needs the whole tree's own collected constants, which is [`resolve_pattern_path`]'s
/// job once it has both a bare name to look up and this glob's own prefix — recording only
/// the prefix here keeps this function a plain syntactic walk, the same as every other case
/// in it.
fn flatten_use_tree(tree: &syn::UseTree, prefix: &mut Vec<String>, scope: &mut UseScope) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(ident_name(&path.ident));
            flatten_use_tree(&path.tree, prefix, scope);
            prefix.pop();
        }
        // Codex's next-round finding: `self` in this position — `use indices::{self};` —
        // is not another path segment naming a child of `indices`, it is Rust's own way of
        // binding the *containing* module itself, so `full` must stay exactly `prefix`
        // rather than gaining a literal `"self"` segment, and the bound name is `prefix`'s
        // own last segment (`indices`) rather than the word `"self"`. The unqualified
        // `use indices;` a caller would otherwise have to write is what this is equivalent
        // to, and it is what `resolve_qualified_path`'s own `self`-skipping already
        // expects a scope's stored path to look like.
        syn::UseTree::Name(name) if name.ident == "self" => {
            if let Some(bound_name) = prefix.last().cloned() {
                scope.named.insert(bound_name, prefix.clone());
            }
        }
        syn::UseTree::Name(name) => {
            let bound_name = ident_name(&name.ident);
            let mut full = prefix.clone();
            full.push(bound_name.clone());
            scope.named.insert(bound_name, full);
        }
        // [`flatten_use_tree`]'s `Name` arm above holds the rationale: `self` renamed —
        // `use indices::{self as idx};` — binds `idx` to `indices` itself, so `full` is
        // `prefix` unchanged rather than `prefix` plus a literal `"self"` segment.
        syn::UseTree::Rename(rename) if rename.ident == "self" => {
            scope
                .named
                .insert(ident_name(&rename.rename), prefix.clone());
        }
        syn::UseTree::Rename(rename) => {
            let mut full = prefix.clone();
            full.push(ident_name(&rename.ident));
            scope.named.insert(ident_name(&rename.rename), full);
        }
        syn::UseTree::Group(group) => {
            for member in &group.items {
                flatten_use_tree(member, prefix, scope);
            }
        }
        syn::UseTree::Glob(_) => scope.globs.push(prefix.clone()),
    }
}

/// `own`'s own constant initializers alongside which of them are declared unsigned —
/// [`item_const_exprs`]/[`item_const_unsigned`] or [`block_const_exprs`]/[`block_const_unsigned`],
/// bundled the same reason [`Resolve`] bundles a value and an unsigned closure: reducing
/// [`resolve_scope_consts`]'s own parameter count back under `clippy::too_many_arguments`
/// once threading unsignedness through it gave it an eighth.
struct OwnConsts<'a> {
    exprs: &'a std::collections::HashMap<String, syn::Expr>,
    unsigned: &'a std::collections::HashMap<String, bool>,
    // Codex's next-round finding: `!Q0` over a bare sibling reference needs `Q0`'s own
    // declared *width*, not only whether it is unsigned — [`item_const_types`]/
    // [`block_const_types`] are that mirror, scoped only as narrowly as this struct's own
    // `unsigned` field already is: a sibling declared directly in the same item list.
    types: &'a std::collections::HashMap<String, String>,
}

/// [`ConstScopes`] alongside its own [`UnsignedConstScopes`] mirror — [`OwnConsts`]'s own
/// twin for the *outer* scope stack [`resolve_scope_consts`] searches outward through.
struct OuterScopes<'a> {
    values: &'a ConstScopes,
    unsigned: &'a UnsignedConstScopes,
}

/// `own`'s constants, each resolved to an integer where its initializer allows — directly,
/// through a chain of references to other constants `own` itself declares, through one
/// already visible in `outer`, or through a module-qualified path already recorded in
/// `qualified` (searched at every depth [`resolve_qualified_path_at_any_depth`] tries,
/// relative to the scope `own` itself sits in — `module_path`, `function_path` and
/// `block_path`, exactly as a match arm's own qualified pattern is). Declaration order
/// within `own` does not matter: Rust's own name resolution does not require one, so a
/// fixed-point pass (bounded, since a real dependency chain among a handful of constants in
/// one scope is shallow) is what a single top-to-bottom scan would get wrong for a constant
/// that names a later one.
///
/// Codex's forty-second-round finding: a qualified initializer such as
/// `const P0: u8 = base::BASE + 0;`, naming a sibling module's constant already indexed in
/// `qualified` by an earlier-visited part of the same walk, was rejected outright — the
/// `resolve` closure below only ever accepted `path.get_ident()`, a single bare segment,
/// so every multi-segment path answered `None` regardless of whether `qualified` already
/// held it.
///
/// Codex's next-round finding: the first fix asked only one fixed combination —
/// `module_path + function_path + block_path`, the scope `own` itself sits in — which
/// answers for a module declared local to that exact function or block, but not for one
/// declared at ordinary, shallower scope (a sibling of the function, or of the whole file),
/// which is the far more common shape and the one Codex's own repro used. Widened to
/// [`resolve_qualified_path_at_any_depth`]'s full most-specific-first search, the same one
/// [`resolve_pattern_path`] already runs for a pattern.
fn resolve_scope_consts(
    own: &OwnConsts<'_>,
    outer: &OuterScopes<'_>,
    qualified: &std::collections::HashMap<String, i128>,
    qualified_unsigned: &std::collections::HashMap<String, bool>,
    module_path: &[String],
    function_path: &[String],
    block_path: &[String],
) -> std::collections::HashMap<String, i128> {
    let mut resolved: std::collections::HashMap<String, i128> = std::collections::HashMap::new();
    for _ in 0..own.exprs.len().max(1) {
        let mut progressed = false;
        for (name, expr) in own.exprs {
            if resolved.contains_key(name) {
                continue;
            }
            let resolve = |path: &syn::Path| {
                if let Some(ident) = path.get_ident() {
                    let candidate = ident_name(ident);
                    return resolved
                        .get(&candidate)
                        .copied()
                        .or_else(|| outer.values.resolve(&candidate));
                }
                resolve_qualified_path_at_any_depth(
                    path,
                    qualified,
                    module_path,
                    function_path,
                    block_path,
                )
            };
            // Codex's next-round finding: an initializer such as `const P1: u8 = P0 + 1;`
            // where `P0` is one of `own`'s own siblings needs no unsignedness of its own to
            // fold (arithmetic does not care), but a *sibling's* own ordering comparison —
            // `const P1: bool = P0 < OTHER;` — would, and a bare reference to `own`'s own
            // declared-unsigned map answers that the same way the value lookup above
            // answers a bare value reference.
            //
            // Codex's next-round finding: `const Q: u128 = u128::MAX >> 127;` — a qualified,
            // well-known-bound reference — declined unconditionally here, on the reasoning
            // that this was the same scope [`path_is_definitely_unsigned`] itself declines
            // beyond a bare name; that reasoning was wrong about what that function actually
            // covers, once [`qualified_path_is_declared_at_any_depth`]'s own finding is
            // accounted for — it resolves a qualified local constant and a well-known
            // primitive bound alike, not only a bare name. Widened to match: a qualified
            // reference already recorded in `qualified_unsigned` (a sibling module's own
            // constant, at the identical most-specific-first depth the value resolver above
            // already searches), then, failing that, a well-known bound not shadowed by any
            // real declaration at any of those depths — the identical two-step fallback
            // [`path_is_definitely_unsigned`] uses, so a local constant's own initializer and
            // a match arm's own guard or pattern answer the identical question the identical
            // way.
            let resolve_unsigned = |path: &syn::Path| {
                if let Some(ident) = path.get_ident() {
                    let candidate = ident_name(ident);
                    return own
                        .unsigned
                        .get(&candidate)
                        .copied()
                        .unwrap_or_else(|| outer.unsigned.resolve(&candidate));
                }
                if resolve_qualified_unsigned_at_any_depth(
                    path,
                    qualified,
                    qualified_unsigned,
                    module_path,
                    function_path,
                    block_path,
                ) {
                    return true;
                }
                let segments: Vec<String> = path
                    .segments
                    .iter()
                    .map(|segment| ident_name(&segment.ident))
                    .collect();
                let Some((type_name, member)) = well_known_bound_segments(&segments) else {
                    return false;
                };
                if !matches!(type_name, "u8" | "u16" | "u32" | "u64" | "u128") {
                    return false;
                }
                if qualified_path_is_declared_at_any_depth(
                    path,
                    qualified,
                    module_path,
                    function_path,
                    block_path,
                ) {
                    return false;
                }
                well_known_integer_bound(type_name, member).is_some()
            };
            // [`evaluate_bitwise_not`]'s own doc comment holds the rationale: a bare
            // sibling's own declared width, answered only as narrowly as `own.types` itself
            // is scoped — a name reached through an outer scope or a qualified path
            // declines, which is always sound for a fact this pass does not track that
            // widely.
            let resolve_width = |path: &syn::Path| -> Option<&str> {
                let ident = path.get_ident()?;
                own.types.get(&ident_name(ident)).map(String::as_str)
            };
            let bundled = Resolve {
                value: &resolve,
                unsigned: &resolve_unsigned,
                width: &resolve_width,
            };
            let declared_type = own.types.get(name).map(String::as_str);
            if let Some(value) = resolve_declared_initializer(expr, declared_type, &bundled) {
                resolved.insert(name.clone(), value);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    resolved
}

/// A literal's own integer value: an integer of any base or suffix, a byte literal
/// (`b'\0'`), or a `char` literal (`'\0'`, by its scalar value) — each exactly as numeric
/// a singleton as a bare `0` is and compiling to the identical lookup-table entry.
/// Codex's finding, twice over: neither [`literal_or_const_value`] nor [`pattern_literal`]
/// had ever looked past `syn::Lit::Int`, and a `char`'s own discriminant is no less a
/// number than a byte's — `rustc` lowers a dense `char` match to the same indexed `.rodata`
/// a `u8` one gets.
///
/// Codex's next-round finding: a `u128` literal at or above `2^127` — half of that type's
/// own range — has no representation in this scan's own `i128`, so
/// `base10_parse::<i128>()` fails and every arm of a table spelled with such literals
/// stayed unresolved. Reinterpreted as its own two's-complement bit pattern instead — the
/// same cast [`apply_integer_cast`]'s own widest-width branch already performs for a
/// `u128 as i128` cast — which preserves both the relative order and the unit spacing of
/// every value in `u128`'s upper half, so a window of consecutive `u128` literals up there
/// is still a window of consecutive `i128` values to every check downstream, just one
/// that wraps around through `i128::MIN`.
fn lit_value(lit: &syn::Lit) -> Option<i128> {
    match lit {
        syn::Lit::Int(int) => int.base10_parse::<i128>().ok().or_else(|| {
            let unsigned = int.base10_parse::<u128>().ok()?;
            #[allow(
                clippy::cast_possible_wrap,
                reason = "deliberate two's-complement bit reinterpretation of a value \
                          base10_parse::<i128> already rejected as out of range, not a \
                          value conversion"
            )]
            let reinterpreted = unsigned as i128;
            Some(reinterpreted)
        }),
        syn::Lit::Byte(byte) => Some(i128::from(byte.value())),
        syn::Lit::Char(char) => Some(i128::from(u32::from(char.value()))),
        // Codex's forty-fifth-round finding: a bare `true`/`false` reaching this function
        // — by way of `Expr::If`'s own condition, since a `bool` is the only type Rust
        // permits one to be — had no representation here at all. `0`/`1` is the same
        // representation the const-folded machine code itself uses for a `bool` (`rustc`
        // never allocates a `bool` a byte wider than that), so treating `false` and `true`
        // as `0` and `1` in this scan's own `i128` domain costs nothing and needs no
        // separate boolean domain of its own.
        syn::Lit::Bool(boolean) => Some(i128::from(boolean.value)),
        _ => None,
    }
}

/// The name of `ty`, if it is a plain, unqualified single-segment type path (`u8`, `i32`,
/// and so on, with no generic arguments) — the shape [`apply_integer_cast`] acts on — or
/// the identical primitive spelled out in full as `core::primitive::u8` or
/// `std::primitive::u8`, the same two canonical prefixes [`well_known_bound_segments`]
/// already recognises for a bound path rather than a type.
///
/// Codex's next-round finding: `const HI: core::primitive::u128 = 1u128 << 127;` names its
/// type this way rather than bare, which is ordinary, MSRV-legal Rust — `core::primitive`
/// (and `std::primitive`) are real modules re-exporting every primitive under its own
/// name — but this function answered `None` for any path longer than one segment, so
/// `declared_type_is_unsigned` never recorded `HI` as unsigned and a guard built from it
/// stayed unresolved. Scoped to exactly the three-segment canonical spelling, the same way
/// [`well_known_bound_segments`]'s own four-segment case is scoped to it rather than to any
/// path ending in the right two segments.
fn single_segment_type_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    if let Some(ident) = type_path.path.get_ident() {
        return Some(ident_name(ident));
    }
    let segments: Vec<String> = type_path
        .path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    match segments.as_slice() {
        [root, primitive, name]
            if (root == "core" || root == "std") && primitive == "primitive" =>
        {
            Some(name.clone())
        }
        _ => None,
    }
}

/// Whether `name` is one of the five unsigned fixed-width integer type names — `u8`, `u16`,
/// `u32`, `u64`, `u128` — factored out of [`declared_type_is_unsigned`] so a caller already
/// holding a type *name* (rather than a `syn::Type` to read one from) does not repeat this
/// five-way match of its own; every one of those callers checks the identical five names
/// [`is_definitely_unsigned`]'s own suffix and cast cases already accept.
fn is_unsigned_type_name(name: &str) -> bool {
    matches!(name, "u8" | "u16" | "u32" | "u64" | "u128")
}

/// [`is_unsigned_type_name`]'s own signed twin — `i8`, `i16`, `i32`, `i64`, `i128` — for
/// [`is_definitely_signed`]'s three cases in the identical shape.
fn is_signed_type_name(name: &str) -> bool {
    matches!(name, "i8" | "i16" | "i32" | "i64" | "i128")
}

/// Whether `ty` is a plain, unqualified path naming one of the five unsigned fixed-width
/// integer types — the same shape [`single_segment_type_name`] already recognises.
/// [`item_const_unsigned`] and [`block_const_unsigned`] are what read it off a `const`
/// item's own type ascription rather than a cast.
fn declared_type_is_unsigned(ty: &syn::Type) -> bool {
    single_segment_type_name(ty).is_some_and(|name| is_unsigned_type_name(&name))
}

/// The full, dotted name of `ty`, if it is a plain, unqualified type path of any length —
/// `u8`, `Key`, or `defs::Key` — with any generic argument on any segment ignored the same
/// way `ident_name` already ignores one for every other path this scan reads. The shape a
/// `<Type as Trait>::NAME` pattern's own `Type` needs to be for
/// [`resolve_qself_associated_const`] to find what it was implemented for.
///
/// Codex's next-round finding: `single_segment_type_name`'s own one-segment restriction
/// refused a *qualified* `Self` type outright — `<defs::Key as Indices>::P0`'s own
/// `qself.ty` is `defs::Key`, two segments — before `resolve_qself_associated_const` ever
/// got to consult the impl-constant index at all, even though `MatchVisitor::visit_item_impl`
/// already indexes exactly this spelling (`defs::Key::P0`) under its own third key, for the
/// identical reason a qualified constant reference needs one. This is that restriction
/// dropped: the type name this function returns can itself be multi-segment, and every
/// synthetic path and suffix `resolve_qself_associated_const` builds from it already reads
/// correctly whichever way — `defs::Key::P0` parses as a three-segment path exactly as
/// `Key::P0` parses as a two-segment one, and a suffix ending `::defs::Key::P0` is matched
/// the same substring way a suffix ending `::Key::P0` already was.
fn type_path_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() || type_path.path.segments.is_empty() {
        return None;
    }
    Some(
        type_path
            .path
            .segments
            .iter()
            .map(|segment| ident_name(&segment.ident))
            .collect::<Vec<_>>()
            .join("::"),
    )
}

/// `value`'s own bit pattern, reinterpreted the way Rust's `as` operator casts one
/// fixed-width integer type into another: truncated to the destination's own width, then
/// sign-extended if the destination is signed and the truncated value's own high bit is
/// set.
///
/// Codex's finding: `248u8 as i8` and `255u8 as i8` are `-8` and `-1`, not `248` and
/// `255` — a `const` whose numbered arms cover `-8..=6` by casting a run of `u8` literals
/// compiles to the identical offset-indexed table an `i8` literal sequence would, but the
/// cast's own destination type used to be discarded entirely, passing the unsigned
/// operand straight through.
///
/// Scoped to the ten fixed-width integer types (`u8`..`u128`, `i8`..`i128`) plus one
/// narrow exception for `usize`/`isize`, which are otherwise platform-width and have no
/// target here to measure against — so a cast to either stays unresolved rather than
/// guessed, the same standing a call to a user-defined `const fn` already has here. A
/// destination this scan does not resolve returns `None`, never the operand unchanged,
/// because passing an unevaluated cast through is exactly the bug being fixed.
///
/// Codex's next-round finding: `0u8 as usize` was refused by that same platform-width
/// rule, even though nothing about *this* cast's answer depends on which target it runs
/// on — Rust's own reference guarantees `usize`/`isize` are at least 16 bits wide on
/// every target, so a value already inside that guaranteed range casts to the identical
/// `usize`/`isize` value everywhere. Scoped to exactly that width and no wider: a value
/// needing more than 16 bits genuinely would depend on the target, which this scan still
/// has no way to know.
fn apply_integer_cast(value: i128, ty: &syn::Type) -> Option<i128> {
    let name = single_segment_type_name(ty)?;
    match name.as_str() {
        "usize" => return (0..=i128::from(u16::MAX)).contains(&value).then_some(value),
        "isize" => {
            return (i128::from(i16::MIN)..=i128::from(i16::MAX))
                .contains(&value)
                .then_some(value);
        }
        _ => {}
    }
    let (width, signed): (u32, bool) = match name.as_str() {
        "u8" => (8, false),
        "u16" => (16, false),
        "u32" => (32, false),
        "u64" => (64, false),
        "u128" => (128, false),
        "i8" => (8, true),
        "i16" => (16, true),
        "i32" => (32, true),
        "i64" => (64, true),
        "i128" => (128, true),
        _ => return None,
    };
    // Both casts are the deliberate two's-complement bit reinterpretation Rust's own
    // `as` gives an integer-to-integer cast of the same width — not a value conversion,
    // which is what `checked_sub`/`try_from` below are for.
    #[allow(
        clippy::cast_sign_loss,
        reason = "the widest-width branch below reinterprets this pattern, not converts it"
    )]
    let bits = value as u128;
    if width >= 128 {
        // Codex's next-round finding: the `!signed` (`u128` destination) arm used
        // `i128::try_from(bits).ok()`, which fails closed the moment `bits` exceeds
        // `i128::MAX` — exactly the upper half of `u128`'s own range, and exactly the
        // domain `lit_value`'s own `Lit::Int` case already stores as a wrapped, negative
        // `i128` for a `u128` literal too wide for `i128` to parse directly. A cast
        // *landing* in that same upper half (`(-15i128) as u128`, whose 128-bit pattern is
        // a large positive `u128` no `i128` can hold as a positive value) was refused
        // instead of reinterpreted the identical way. `i128` and `u128` share one 128-bit
        // storage representation in this scan regardless of which one Rust's own type
        // system would call it, so both destinations reinterpret the same bit pattern
        // rather than converting a value — there is nothing left for `signed` to decide at
        // this width.
        #[allow(
            clippy::cast_possible_wrap,
            reason = "u128 to i128 at equal width is the same bit-pattern reinterpretation, not a value conversion"
        )]
        let reinterpreted = bits as i128;
        return Some(reinterpreted);
    }
    let masked = bits & ((1_u128 << width) - 1);
    if signed && masked & (1_u128 << (width - 1)) != 0 {
        i128::try_from(masked).ok()?.checked_sub(1_i128 << width)
    } else {
        i128::try_from(masked).ok()
    }
}

/// `TypeName::MIN` or `TypeName::MAX`'s own well-known value, for the ten fixed-width
/// integer types this scan already tracks a width and a signedness for in
/// [`apply_integer_cast`] — a fact about the language rather than about anything the
/// scanned source tree declares, so no amount of collecting local, qualified or
/// trait-default constants would ever find it. Read straight off Rust's own associated
/// constants rather than re-derived from `width`/`signed` by hand, so this can never
/// disagree with the values `apply_integer_cast`'s own casts already produce.
///
/// `u128::MAX` is the one case with no positive `i128` to hold it — `2^128 - 1`, every bit
/// set — so it answers `-1`, the identical all-ones bit pattern [`apply_integer_cast`]'s
/// own upper-half `u128` reinterpretation already uses; `u128::MIN` is `0`, which needs no
/// such reinterpretation at all.
fn well_known_integer_bound(type_name: &str, member: &str) -> Option<i128> {
    match (type_name, member) {
        ("u8", "MIN") => Some(i128::from(u8::MIN)),
        ("u8", "MAX") => Some(i128::from(u8::MAX)),
        ("u16", "MIN") => Some(i128::from(u16::MIN)),
        ("u16", "MAX") => Some(i128::from(u16::MAX)),
        ("u32", "MIN") => Some(i128::from(u32::MIN)),
        ("u32", "MAX") => Some(i128::from(u32::MAX)),
        ("u64", "MIN") => Some(i128::from(u64::MIN)),
        ("u64", "MAX") => Some(i128::from(u64::MAX)),
        ("u128", "MIN") => Some(0),
        ("u128", "MAX") => Some(-1),
        ("i8", "MIN") => Some(i128::from(i8::MIN)),
        ("i8", "MAX") => Some(i128::from(i8::MAX)),
        ("i16", "MIN") => Some(i128::from(i16::MIN)),
        ("i16", "MAX") => Some(i128::from(i16::MAX)),
        ("i32", "MIN") => Some(i128::from(i32::MIN)),
        ("i32", "MAX") => Some(i128::from(i32::MAX)),
        ("i64", "MIN") => Some(i128::from(i64::MIN)),
        ("i64", "MAX") => Some(i128::from(i64::MAX)),
        ("i128", "MIN") => Some(i128::MIN),
        ("i128", "MAX") => Some(i128::MAX),
        _ => None,
    }
}

/// The `(type_name, member)` [`well_known_integer_bound`] answers for, read out of a path's
/// own segments in either of the two shapes Rust resolves a primitive's associated bound
/// through: the bare `TypeName::MIN` two segments name directly, or the canonical
/// `core::primitive::TypeName::MIN` (or `std::primitive::TypeName::MIN`) four segments spell
/// out in full — `core::primitive` and `std::primitive` both being real modules that
/// re-export every primitive type under its own name, not a shape this scan invents.
///
/// Codex's next-round finding: `resolve_qualified_path_at_any_depth`'s own well-known
/// fallback matched only the bare two-segment shape, so `const P0: u8 =
/// core::primitive::u8::MIN;` — legal, unambiguous Rust naming the identical constant a bare
/// `u8::MIN` would — read as unresolved on every arm. Scoped to exactly these two shapes: a
/// path headed by anything else four segments long (a real module a source tree happens to
/// call `primitive`, say) is not this, and stays unresolved rather than guessed at — the
/// qualified-map search this fallback follows is what would recognise a local shadowing
/// module by that name instead.
fn well_known_bound_segments(segments: &[String]) -> Option<(&str, &str)> {
    match segments {
        [type_name, member] => Some((type_name.as_str(), member.as_str())),
        [root, primitive, type_name, member]
            if (root == "core" || root == "std") && primitive == "primitive" =>
        {
            Some((type_name.as_str(), member.as_str()))
        }
        _ => None,
    }
}

/// `expr`'s own integer literal, if it is one carrying an explicit suffix (`255u8`, never
/// a bare `255`) — seen through any nesting of parentheses or brace groups, the same two
/// wrappers every other literal-reading function here sees through.
///
/// The suffix is what [`literal_or_const_value`]'s own bitwise-NOT case needs and a bare
/// literal or an already-resolved constant cannot supply: the *width* `!` is meant to flip
/// every bit of, which this scan has no way to learn except from the source spelling.
fn as_suffixed_int_literal(expr: &syn::Expr) -> Option<&syn::LitInt> {
    match expr {
        syn::Expr::Paren(paren) => as_suffixed_int_literal(&paren.expr),
        syn::Expr::Group(group) => as_suffixed_int_literal(&group.expr),
        syn::Expr::Lit(literal) => match &literal.lit {
            syn::Lit::Int(int) if !int.suffix().is_empty() => Some(int),
            _ => None,
        },
        _ => None,
    }
}

/// `expr`'s own boolean literal — seen through any nesting of parentheses or brace groups,
/// the same two wrappers [`as_suffixed_int_literal`] sees through for an integer literal.
///
/// Codex's next-round finding: `!true` and `!false` are [`syn::UnOp::Not`] over a
/// [`syn::Lit::Bool`], exactly the same node kind an integer bitwise-NOT wears — but
/// `!true` is logical negation to `false`, not a width-dependent bitwise flip, and has no
/// suffix for [`as_suffixed_int_literal`] to find (`true`/`false` carry no width at all).
/// A guard spelled `_ if !true => ..` therefore resolved to `None` rather than the `0` a
/// dead-guard check elsewhere needs to prove the arm unreachable. This is tried first, so a
/// boolean operand is read as logical negation and only a non-boolean operand falls through
/// to the width-aware bitwise path.
fn as_bool_literal(expr: &syn::Expr) -> Option<bool> {
    match expr {
        syn::Expr::Paren(paren) => as_bool_literal(&paren.expr),
        syn::Expr::Group(group) => as_bool_literal(&group.expr),
        syn::Expr::Lit(literal) => match &literal.lit {
            syn::Lit::Bool(boolean) => Some(boolean.value),
            _ => None,
        },
        _ => None,
    }
}

/// `block`'s own value as a constant-initializer expression: its own tail expression,
/// once any local `const` declaration or plain `let` binding feeding that tail is resolved
/// first — a block-scoped mirror of `resolve_scope_consts`'s own fixed point, self-contained
/// here since this function carries no `ConstScopes` of its own, only the caller's flat
/// `resolve`. Scoped narrowly: every statement but the last must be a local `const` item
/// (`block_const_exprs` is what recognises one) or a plain `let` binding
/// (`block_let_exprs`), and the last must be a semicolon-less tail expression — a block
/// holding a loop, an assignment, or any other statement shape stays unresolved rather than
/// guessed at, and so does a labelled block (`'a: { .. }`), whose tail a `break 'a value;`
/// elsewhere in the block could also supply — a labelled block is the caller's to refuse,
/// since a `syn::Block` carries no label of its own to check. A `const` and a `let` sharing
/// one name — shadowing either way — is refused the same way: the two maps merge into one
/// by name, so a collision silently drops an entry and the statement count no longer
/// matches, which is caught below exactly as an unrecognised statement shape is.
///
/// Codex's next-round finding: `const P0: u8 = { let value = 0; value };` is exactly as
/// resolvable as a local `const` already folded here, but a `let` is a different statement
/// kind (`syn::Stmt::Local`) that nothing here had ever read. [`block_let_exprs`] is that
/// reading, narrowed to a bare identifier binding with no destructuring and no `let-else`.
///
/// Factored out of [`literal_or_const_value`]'s own `Expr::Block` case so `Expr::If`'s
/// `then` branch — itself a plain `syn::Block` — can be evaluated the identical way,
/// rather than duplicating the local-binding fixed point a second time.
/// `path`'s own name, when it is a bare identifier already resolved as one of a block's own
/// locals — factored out of [`evaluate_block`]'s four near-identical resolver closures to
/// keep that function under clippy's line count.
fn resolved_local_name(
    path: &syn::Path,
    resolved: &std::collections::HashMap<String, i128>,
) -> Option<String> {
    let ident = path.get_ident()?;
    let candidate = ident_name(ident);
    resolved.contains_key(&candidate).then_some(candidate)
}

/// `locals`' own fixed-point resolution — factored out of [`evaluate_block`] to keep that
/// function under clippy's line count: each local's own initializer is retried against the
/// scope's own partial progress until nothing more resolves, so a local declared in terms of
/// another declared after it in source order (`let a = b; let b = 1;` is not legal Rust, but
/// two names each usable in the other's initializer inside one `const` block's worth of
/// mutually-referencing constants is) does not depend on declaration order.
///
/// Codex's ordering finding: this fixed point is sound for `locals` filled with a block's own
/// `const` items alone — order-independent statics, exactly like the top-level constants the
/// doc comment above already describes — and unsound for a plain `let` statement, which
/// executes in source order and can be reassigned by a mutation between its own declaration
/// and a later statement that reads it. `evaluate_block` now calls this only with `block`'s
/// `const` items; every `let` and every mutation statement is [`resolve_block_sequential`]'s,
/// walked together in the one order they actually run in rather than folded into this
/// fixed point.
fn resolve_block_locals(
    locals: &std::collections::HashMap<String, syn::Expr>,
    local_types: &std::collections::HashMap<String, String>,
    resolve: &Resolve<'_>,
) -> std::collections::HashMap<String, i128> {
    let mut resolved: std::collections::HashMap<String, i128> = std::collections::HashMap::new();
    for _ in 0..locals.len().max(1) {
        let mut progressed = false;
        for (name, local_expr) in locals {
            if resolved.contains_key(name) {
                continue;
            }
            let local_resolve_value = |path: &syn::Path| {
                path.get_ident()
                    .map(ident_name)
                    .and_then(|candidate| resolved.get(&candidate).copied())
                    .or_else(|| (resolve.value)(path))
            };
            let local_resolve_unsigned = |path: &syn::Path| {
                resolved_local_name(path, &resolved).map_or_else(
                    || (resolve.unsigned)(path),
                    |candidate| {
                        local_types
                            .get(&candidate)
                            .is_some_and(|name| is_unsigned_type_name(name))
                    },
                )
            };
            let local_resolve_width = |path: &syn::Path| {
                resolved_local_name(path, &resolved).map_or_else(
                    || (resolve.width)(path),
                    |candidate| local_types.get(&candidate).map(String::as_str),
                )
            };
            let local_resolve = Resolve {
                value: &local_resolve_value,
                unsigned: &local_resolve_unsigned,
                width: &local_resolve_width,
            };
            let declared_type = local_types.get(name).map(String::as_str);
            if let Some(value) =
                resolve_declared_initializer(local_expr, declared_type, &local_resolve)
            {
                resolved.insert(name.clone(), value);
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    resolved
}

/// Walks `block`'s own `let` and compound-assignment statements together, in source order,
/// updating `resolved` as each one is reached — factored out of [`evaluate_block`] to keep
/// that function under clippy's line count.
///
/// Codex's finding: `let mut x = 0; x += 1; x - 1` names a mutation the previous version of
/// this scan had no way to represent — a fixed point over every `let`'s own static
/// initializer, run to completion, *then* every mutation applied afterward in a second pass.
/// That second pass was itself real progress over resolving no mutation at all, but it kept
/// the two kinds of statement in two different passes rather than one order, and Codex's
/// next-round finding is what that costs: `{ let mut x: u8 = n * 10; x /= 10; let y = x; y
/// }` evaluates to `n` in real Rust, because `y` binds to whatever `x` holds *after* the
/// division that precedes it — but the old two-pass shape resolved every `let` first, so `y`
/// bound to `x`'s freshly *declared* value (`n * 10`) before the mutation pass ever ran, and
/// the division that should have applied to `y`'s own view of `x` never touched it. This
/// function is the fix: a single walk over `block.stmts`, in order, threading one `resolved`
/// map through both statement kinds, so a `let` sees exactly the mutations that precede it
/// in source order and none that follow — the identical thing `rustc` does when it executes
/// the block one statement at a time.
///
/// `resolved` on entry already holds `block`'s own resolved `const` items from
/// [`resolve_block_locals`]'s own fixed point: a local `const` is an order-independent
/// static rather than a statement this walk executes, so two mutually-referencing constants
/// keep the fixed point that finding local `const`s at all needed in the first place, and
/// this walk never lets a `let` or a mutation feed back into it.
///
/// A `let` whose initializer this scan cannot resolve is left out of `resolved` rather than
/// failing the whole walk — the same leniency every other unresolved local in this scan
/// gets, since a later reference to it will simply fail to resolve in its own turn rather
/// than being credited with a wrong answer. A mutation naming a local with no resolved value
/// yet is refused outright instead, for the reason [`apply_compound_assignment`]'s own doc
/// comment already gives for skipping being worse than refusing: silently leaving that local
/// at a value `rustc` would never have produced there would credit every later read of it
/// with an answer rather than with no answer at all.
/// [`resolve_block_sequential`]'s own `let`-statement half, factored out to keep that
/// function under clippy's line count: resolves every name `local` binds against the scope
/// as it stands *before* this statement, then updates `local_types` to match.
///
/// Codex's finding: a name this statement (re)binds has to shed whatever type an *earlier*
/// declaration of the same name left in `local_types` before this statement's own
/// initializer is read against it — `stmt_let_type` is asked first, before either map
/// changes, so the initializer's own right-hand side still sees the *old* binding's type (a
/// self-referential shadow, `let x: u16 = x as u16 + 1;`, needs the outer `x`'s width to
/// fold correctly), and the type update lands only after every name this statement binds has
/// had its own chance to resolve against the scope as it stood before the shadow. Every name
/// bound sheds whatever type an earlier declaration left behind — a bare `let x = ..;`
/// shadowing a typed `x` has to lose that type exactly as much as a typed shadow has to
/// replace it, since a shadow this scan cannot ascribe a type to is a shadow whose type is
/// unknown, not one that inherits its predecessor's.
fn resolve_sequential_let(
    local: &syn::Local,
    init: &syn::LocalInit,
    resolve: &Resolve<'_>,
    local_types: &mut std::collections::HashMap<String, String>,
    resolved: &mut std::collections::HashMap<String, i128>,
    shadow_snapshot: &mut ShadowSnapshot,
) {
    let ascribed_type = stmt_let_type(local, init, resolve);
    let mut bound_names = Vec::new();
    for (name, expr) in destructured_binding(&local.pat, &init.expr) {
        // Codex's finding, met here: a name this statement binds may already answer for an
        // *outer* scope — the body of a loop this statement sits inside, most sharply — and
        // that outer answer is what has to come back once this call's own scope ends, not
        // whatever the last shadow inside it left behind. Captured once per name, before
        // either map changes for it, so a second shadow of the same name later in this same
        // call sees the entry already there and leaves it alone: the value one level up is
        // the value the *first* shadow overwrote, and every later scope must reveal that one.
        shadow_snapshot.entry(name.clone()).or_insert_with(|| {
            (
                resolved.get(&name).copied(),
                local_types.get(&name).cloned(),
            )
        });
        let scoped_resolve_value = |path: &syn::Path| {
            path.get_ident()
                .map(ident_name)
                .and_then(|candidate| resolved.get(&candidate).copied())
                .or_else(|| (resolve.value)(path))
        };
        let scoped_resolve_unsigned = |path: &syn::Path| {
            resolved_local_name(path, resolved).map_or_else(
                || (resolve.unsigned)(path),
                |candidate| {
                    local_types
                        .get(&candidate)
                        .is_some_and(|name| is_unsigned_type_name(name))
                },
            )
        };
        let scoped_resolve_width = |path: &syn::Path| {
            resolved_local_name(path, resolved).map_or_else(
                || (resolve.width)(path),
                |candidate| local_types.get(&candidate).map(String::as_str),
            )
        };
        let scoped_resolve = Resolve {
            value: &scoped_resolve_value,
            unsigned: &scoped_resolve_unsigned,
            width: &scoped_resolve_width,
        };
        let declared_type = ascribed_type
            .as_ref()
            .filter(|(ascribed_name, _)| *ascribed_name == name)
            .map(|(_, ty)| ty.as_str());
        if let Some(value) = resolve_declared_initializer(&expr, declared_type, &scoped_resolve) {
            resolved.insert(name.clone(), value);
        }
        bound_names.push(name);
    }
    for name in &bound_names {
        local_types.remove(name);
    }
    if let Some((name, ty)) = ascribed_type {
        local_types.insert(name, ty);
    }
}

/// A `let PATTERN = EXPR else { DIVERGE };` statement whose pattern provably matches —
/// resolved and bound into `resolved`/`local_types` exactly as [`resolve_sequential_let`]
/// binds an ordinary `let`, since a let-else's own bindings are visible in the *enclosing*
/// scope from this point on, not confined to a nested branch the way an `if let`'s are.
///
/// Codex's finding: `let x @ 0..=254 = 0u8 else { loop {} }; x` names a refutable pattern
/// this scan could already check — [`match_arm_matches_constant`] is exactly what an `if
/// let`/`while let` condition already resolves through — but `resolve_block_sequential`
/// unconditionally skipped every `let`-`else` statement (`init.diverge.is_some()`), leaving
/// `x` unbound even when the pattern is provably satisfied and the diverging `else` provably
/// never runs.
///
/// A pattern that provably does *not* match is refused (`None`) rather than skipped: the
/// diverging `else` branch would run in that case, and this scan has no way to represent
/// control leaving the block here — continuing as though the statements after it still
/// executed normally would be a wrong answer, not merely an unresolved one. A pattern this
/// scan cannot judge either way is refused for the identical reason `resolve_sequential_let`
/// refuses an initializer it cannot fold: an unresolved name, never a guessed value.
///
/// Every name [`pattern_bindings`] finds is bound to the identical scrutinee value — the
/// same simplification [`evaluate_if_let`]'s own `bound` map already makes, sound for the
/// single-name-or-uniform-range shape a let-else's pattern practically has here and not
/// attempting to destructure a compound one further.
fn resolve_let_else_binding(
    local: &syn::Local,
    init: &syn::LocalInit,
    resolve: &Resolve<'_>,
    local_types: &mut std::collections::HashMap<String, String>,
    resolved: &mut std::collections::HashMap<String, i128>,
    shadow_snapshot: &mut ShadowSnapshot,
) -> Option<()> {
    let scoped_resolve_value = |path: &syn::Path| {
        path.get_ident()
            .map(ident_name)
            .and_then(|candidate| resolved.get(&candidate).copied())
            .or_else(|| (resolve.value)(path))
    };
    let scoped_resolve_unsigned = |path: &syn::Path| {
        resolved_local_name(path, resolved).map_or_else(
            || (resolve.unsigned)(path),
            |candidate| {
                local_types
                    .get(&candidate)
                    .is_some_and(|name| is_unsigned_type_name(name))
            },
        )
    };
    let scoped_resolve_width = |path: &syn::Path| {
        resolved_local_name(path, resolved).map_or_else(
            || (resolve.width)(path),
            |candidate| local_types.get(&candidate).map(String::as_str),
        )
    };
    let scoped_resolve = Resolve {
        value: &scoped_resolve_value,
        unsigned: &scoped_resolve_unsigned,
        width: &scoped_resolve_width,
    };
    let scrutinee = literal_or_const_value(&init.expr, &scoped_resolve)?;
    if !match_arm_matches_constant(&local.pat, scrutinee, &scoped_resolve)? {
        return None;
    }
    let declared_type = expr_declared_width(&init.expr, &scoped_resolve);
    for ident in pattern_bindings(&local.pat, &scoped_resolve) {
        let name = ident_name(ident);
        shadow_snapshot.entry(name.clone()).or_insert_with(|| {
            (
                resolved.get(&name).copied(),
                local_types.get(&name).cloned(),
            )
        });
        resolved.insert(name.clone(), scrutinee);
        local_types.remove(&name);
        if let Some(ty) = &declared_type {
            local_types.insert(name, ty.clone());
        }
    }
    Some(())
}

/// A bound on how many times [`evaluate_while_loop`] will run one loop's body before
/// refusing rather than resolving. A source loop with no discoverable termination is a
/// hazard this scan must never inherit — an unbounded interpreter here would make a single
/// pull request able to hang `cargo xtask check-layering` itself — so a loop that has not
/// finished within this many iterations is refused the identical way an unresolvable
/// condition already is, rather than guessed at or silently accepted as never-terminating.
const MAX_WHILE_LOOP_ITERATIONS: u32 = 4096;

/// Runs `while_expr` to its own completion against `resolved`/`local_types` — the loop twin
/// of [`resolve_sequential_let`] and the mutation arm of [`resolve_block_sequential`] beside
/// it, called from that function's own per-statement walk. Each iteration folds the
/// condition against the scope as it stands and, while it is non-zero, walks the loop
/// body's own statements with [`resolve_block_sequential`] before folding the condition
/// again — so a `let` or a mutation inside the body is resolved the identical way one
/// outside it would be, against the same two maps, in the order it actually executes.
///
/// `resolve_block_sequential` already refuses with `None` rather than guess at a statement
/// it cannot resolve, so a body this scan cannot fold all the way through refuses the whole
/// loop the identical way; a condition that never folds to a known value, or a loop that has
/// not finished within [`MAX_WHILE_LOOP_ITERATIONS`] iterations, refuses it too. Never by
/// returning as though the loop had ended — [`resolve_block_sequential`]'s own caller,
/// `evaluate_block`, would otherwise resolve the rest of the block from a `resolved` map the
/// loop never actually finished mutating, which is a wrong answer rather than an unresolved
/// one.
///
/// Codex's finding: a name the body's own `let` statements bind directly is that body's
/// own lexical scope, which ends when the body does — but `resolve_block_sequential` was
/// handed the *outer* `resolved`/`local_types` maps directly, with nothing to end that
/// scope at the body's own closing brace. A body that shadows an outer mutable local (`let
/// mut x = 1u8; while x > 0 { x -= 1; let x = 1u8; let _ = x; }`) left the shadow's value
/// sitting in `resolved` under the *outer* name after the statement that declared it should
/// have gone out of scope, so every later iteration's condition read the shadow instead of
/// the mutation right before it — turning a loop real Rust runs once into one this function
/// could never see converge.
///
/// A snapshot taken once *before* the whole body runs and restored once after is not this
/// fix, and was tried and rejected: it cannot tell a shadow's overwrite apart from a
/// mutation's, since both write through the identical map entry, so restoring to the
/// pre-body value undoes the mutation (`x -= 1`) as well as the shadow — the reproduction
/// above needs the mutation to survive and the shadow not to. What is threaded through
/// [`resolve_block_sequential`] and [`resolve_sequential_let`] instead is a `shadow_snapshot`
/// map that each `let` statement writes to, once per name, only the *first* time that name
/// is bound during this one call — capturing the value a mutation earlier in this same body
/// left behind, immediately before the shadow overwrites it. Restoring from that map after
/// the body returns reveals exactly that value, however many times the name was re-shadowed
/// afterward, and a name no statement in the body ever binds is never in it at all.
fn evaluate_while_loop(
    while_expr: &syn::ExprWhile,
    resolve: &Resolve<'_>,
    local_types: &mut std::collections::HashMap<String, String>,
    resolved: &mut std::collections::HashMap<String, i128>,
) -> Option<()> {
    for _ in 0..MAX_WHILE_LOOP_ITERATIONS {
        let condition_resolve_value = |path: &syn::Path| {
            path.get_ident()
                .map(ident_name)
                .and_then(|candidate| resolved.get(&candidate).copied())
                .or_else(|| (resolve.value)(path))
        };
        let condition_resolve_unsigned = |path: &syn::Path| {
            resolved_local_name(path, resolved).map_or_else(
                || (resolve.unsigned)(path),
                |candidate| {
                    local_types
                        .get(&candidate)
                        .is_some_and(|name| is_unsigned_type_name(name))
                },
            )
        };
        let condition_resolve_width = |path: &syn::Path| {
            resolved_local_name(path, resolved).map_or_else(
                || (resolve.width)(path),
                |candidate| local_types.get(&candidate).map(String::as_str),
            )
        };
        let condition_resolve = Resolve {
            value: &condition_resolve_value,
            unsigned: &condition_resolve_unsigned,
            width: &condition_resolve_width,
        };
        // Codex's finding: `while let x @ 1 = y { .. }` reduced its own condition to a
        // boolean through `literal_or_const_value`'s existing `Expr::Let` case — correct as
        // far as it goes, but that case only ever answers "did it match," with nowhere to
        // put the pattern's own bound name once it has. The body then ran against the plain
        // `resolve`, exactly as if the pattern had bound nothing, so a body reading `x` at
        // all left the whole loop unresolved. `evaluate_if_let`'s own fix for the identical
        // gap is reused here rather than duplicated blind: the scrutinee and the match are
        // both decided through `condition_resolve` (so `y`'s own current, possibly-mutated
        // value is what the pattern is checked against, exactly as the plain condition below
        // already is), and the bound name is answered by a resolver whose *fallback* is the
        // plain `resolve` — composing with `resolve_block_sequential`'s own priority of
        // `resolved`/`local_types` first, this resolver second, the same way the non-`let`
        // branch below already relies on.
        if let syn::Expr::Let(let_expr) = strip_parens(&while_expr.cond) {
            let scrutinee = literal_or_const_value(&let_expr.expr, &condition_resolve)?;
            if !match_arm_matches_constant(&let_expr.pat, scrutinee, &condition_resolve)? {
                return Some(());
            }
            let bound = pattern_bindings(&let_expr.pat, &condition_resolve);
            let is_bound = |path: &syn::Path| -> bool {
                path.get_ident().is_some_and(|ident| bound.contains(&ident))
            };
            let bound_unsigned_value =
                !bound.is_empty() && is_definitely_unsigned(&let_expr.expr, &condition_resolve);
            let bound_width_value = (!bound.is_empty())
                .then(|| expr_declared_width(&let_expr.expr, &condition_resolve))
                .flatten();
            let bound_value = |path: &syn::Path| -> Option<i128> {
                if is_bound(path) {
                    return Some(scrutinee);
                }
                (resolve.value)(path)
            };
            let bound_unsigned = |path: &syn::Path| -> bool {
                if is_bound(path) {
                    return bound_unsigned_value;
                }
                (resolve.unsigned)(path)
            };
            let bound_width = |path: &syn::Path| -> Option<&str> {
                if is_bound(path) {
                    return bound_width_value.as_deref();
                }
                (resolve.width)(path)
            };
            let let_resolve = Resolve {
                value: &bound_value,
                unsigned: &bound_unsigned,
                width: &bound_width,
            };
            let mut shadow_snapshot = std::collections::HashMap::new();
            resolve_block_sequential(
                &production_stmts(&while_expr.body),
                &let_resolve,
                local_types,
                resolved,
                &mut shadow_snapshot,
            )?;
            restore_shadow_snapshot(local_types, resolved, shadow_snapshot);
            continue;
        }
        let condition = literal_or_const_value(&while_expr.cond, &condition_resolve)?;
        if condition == 0 {
            return Some(());
        }
        let mut shadow_snapshot = std::collections::HashMap::new();
        resolve_block_sequential(
            &production_stmts(&while_expr.body),
            resolve,
            local_types,
            resolved,
            &mut shadow_snapshot,
        )?;
        restore_shadow_snapshot(local_types, resolved, shadow_snapshot);
    }
    None
}

/// Runs `if_expr` — an `if`/`else` used as a *statement*, never as a value — against
/// `resolved`/`local_types`: the branch taken opens a nested lexical scope of its own,
/// exactly as a `while` body or a bare block statement does, restored via its own
/// `shadow_snapshot` once that branch finishes.
///
/// Codex's finding: `{ let mut x: u8 = n * 10; if x > 0 { x /= 10; } x }` names a top-level
/// `if` with no trailing semicolon and no `else` — `evaluate_block`'s own statement-count
/// invariant had no term at all for it (unlike the `while` and nested-block statements
/// beside it), so the block always looked one statement longer than its own name-counting
/// terms could account for, refusing every constant built this way regardless of what
/// running the conditional would have computed. [`block_if_statement_count`] is that
/// missing term; this function is what actually runs the chosen branch, the same way
/// [`evaluate_while_loop`] runs a loop's body rather than merely being counted for.
///
/// An `if let` condition is handled the identical way [`evaluate_if_let`] and
/// [`evaluate_while_loop`]'s own while-let arm already do: the scrutinee and the match are
/// decided through `condition_resolve`, and a match extends the branch's own resolver with
/// the pattern's bound name(s), falling back to the plain `resolve` for everything else.
fn evaluate_if_statement(
    if_expr: &syn::ExprIf,
    resolve: &Resolve<'_>,
    local_types: &mut std::collections::HashMap<String, String>,
    resolved: &mut std::collections::HashMap<String, i128>,
) -> Option<()> {
    let condition_resolve_value = |path: &syn::Path| {
        path.get_ident()
            .map(ident_name)
            .and_then(|candidate| resolved.get(&candidate).copied())
            .or_else(|| (resolve.value)(path))
    };
    let condition_resolve_unsigned = |path: &syn::Path| {
        resolved_local_name(path, resolved).map_or_else(
            || (resolve.unsigned)(path),
            |candidate| {
                local_types
                    .get(&candidate)
                    .is_some_and(|name| is_unsigned_type_name(name))
            },
        )
    };
    let condition_resolve_width = |path: &syn::Path| {
        resolved_local_name(path, resolved).map_or_else(
            || (resolve.width)(path),
            |candidate| local_types.get(&candidate).map(String::as_str),
        )
    };
    let condition_resolve = Resolve {
        value: &condition_resolve_value,
        unsigned: &condition_resolve_unsigned,
        width: &condition_resolve_width,
    };
    if let syn::Expr::Let(let_expr) = strip_parens(&if_expr.cond) {
        let scrutinee = literal_or_const_value(&let_expr.expr, &condition_resolve)?;
        if !match_arm_matches_constant(&let_expr.pat, scrutinee, &condition_resolve)? {
            return evaluate_else_branch(
                if_expr.else_branch.as_ref(),
                resolve,
                local_types,
                resolved,
            );
        }
        let bound = pattern_bindings(&let_expr.pat, &condition_resolve);
        let is_bound = |path: &syn::Path| -> bool {
            path.get_ident().is_some_and(|ident| bound.contains(&ident))
        };
        let bound_unsigned_value =
            !bound.is_empty() && is_definitely_unsigned(&let_expr.expr, &condition_resolve);
        let bound_width_value = (!bound.is_empty())
            .then(|| expr_declared_width(&let_expr.expr, &condition_resolve))
            .flatten();
        let bound_value = |path: &syn::Path| -> Option<i128> {
            if is_bound(path) {
                return Some(scrutinee);
            }
            (resolve.value)(path)
        };
        let bound_unsigned = |path: &syn::Path| -> bool {
            if is_bound(path) {
                return bound_unsigned_value;
            }
            (resolve.unsigned)(path)
        };
        let bound_width = |path: &syn::Path| -> Option<&str> {
            if is_bound(path) {
                return bound_width_value.as_deref();
            }
            (resolve.width)(path)
        };
        let let_resolve = Resolve {
            value: &bound_value,
            unsigned: &bound_unsigned,
            width: &bound_width,
        };
        let mut shadow_snapshot = ShadowSnapshot::new();
        resolve_block_sequential(
            &production_stmts(&if_expr.then_branch),
            &let_resolve,
            local_types,
            resolved,
            &mut shadow_snapshot,
        )?;
        restore_shadow_snapshot(local_types, resolved, shadow_snapshot);
        return Some(());
    }
    let condition = literal_or_const_value(&if_expr.cond, &condition_resolve)?;
    if condition != 0 {
        let mut shadow_snapshot = ShadowSnapshot::new();
        resolve_block_sequential(
            &production_stmts(&if_expr.then_branch),
            resolve,
            local_types,
            resolved,
            &mut shadow_snapshot,
        )?;
        restore_shadow_snapshot(local_types, resolved, shadow_snapshot);
        return Some(());
    }
    evaluate_else_branch(if_expr.else_branch.as_ref(), resolve, local_types, resolved)
}

/// [`evaluate_if_statement`]'s own `else` half: nothing when there is no `else` at all — the
/// statement simply did nothing — a further `if`/`else if` chain recursed into the identical
/// way, or a plain `else { .. }` block run as its own nested scope. The grammar guarantees
/// `else_branch`'s expression is always one of the first two; there is no third shape to
/// refuse.
fn evaluate_else_branch(
    else_branch: Option<&(syn::token::Else, Box<syn::Expr>)>,
    resolve: &Resolve<'_>,
    local_types: &mut std::collections::HashMap<String, String>,
    resolved: &mut std::collections::HashMap<String, i128>,
) -> Option<()> {
    let Some((_, else_expr)) = else_branch else {
        return Some(());
    };
    match strip_parens(else_expr) {
        syn::Expr::If(nested_if) => {
            evaluate_if_statement(nested_if, resolve, local_types, resolved)
        }
        syn::Expr::Block(else_block) => {
            let mut shadow_snapshot = ShadowSnapshot::new();
            resolve_block_sequential(
                &production_stmts(&else_block.block),
                resolve,
                local_types,
                resolved,
                &mut shadow_snapshot,
            )?;
            restore_shadow_snapshot(local_types, resolved, shadow_snapshot);
            Some(())
        }
        _ => None,
    }
}

/// `resolve_block_sequential`'s own `shadow_snapshot` — every name a `let` statement in the
/// walked block binds, mapped to the `(value, type)` it answered with the moment *before*
/// this call's first shadow of that name, so a caller whose own scope ends where the call's
/// block does — [`evaluate_while_loop`]'s body, or a bare nested block statement — can undo
/// exactly that block's own `let`s once its lexical scope ends. A name no `let` in the block
/// binds is never a key here, which is what lets [`evaluate_block`]'s own top-level call
/// discard this map entirely: nothing it could name would ever need undoing.
type ShadowSnapshot = std::collections::HashMap<String, (Option<i128>, Option<String>)>;

/// Undoes exactly what `shadow_snapshot` recorded: every name in it goes back to the value
/// and type it held immediately before the block that produced this snapshot shadowed it —
/// removed outright if it held none, the standing a name only that block's own `let`s ever
/// introduced already has. Factored out because [`evaluate_while_loop`] applies this once
/// per iteration and a bare nested block statement applies it once for the one time it runs.
fn restore_shadow_snapshot(
    local_types: &mut std::collections::HashMap<String, String>,
    resolved: &mut std::collections::HashMap<String, i128>,
    shadow_snapshot: ShadowSnapshot,
) {
    for (name, (value, ty)) in shadow_snapshot {
        match value {
            Some(value) => {
                resolved.insert(name.clone(), value);
            }
            None => {
                resolved.remove(&name);
            }
        }
        match ty {
            Some(ty) => {
                local_types.insert(name, ty);
            }
            None => {
                local_types.remove(&name);
            }
        }
    }
}

/// Walks `stmts` — every statement a caller has already reduced to what it must interpret,
/// via [`production_stmts`] and, for [`evaluate_block`]'s own top-level call, a further
/// split that holds the block's own value-tail out of this walk entirely.
///
/// Codex's finding, at its root: an `Expr::If` (or a `match`, a `for`/`loop`, an unsafe or
/// labelled block, a bare literal, a method call — anything this function does not
/// specifically interpret) used to fall through every check below and reach a `continue`,
/// treated as a no-op regardless of what running it would really have done. `evaluate_block`
/// itself is protected from an *outer* case of this same shape by its own statement-count
/// invariant, but that invariant is checked once, for the outermost block alone — a nested
/// scope this function recurses into (a `while` body, a bare block statement) had no
/// equivalent guard of its own, so an unrecognised statement inside *one of those* was
/// silently dropped with nothing to notice. Every unrecognised [`syn::Stmt::Expr`] now
/// refuses the whole call instead, the identical answer this function already gives a
/// mutation target it cannot resolve (`resolved.get(&name)?` below) — sound because refusing
/// is always a safe answer here, and never silently substitutes "unresolved" for "wrong."
///
/// Two statement kinds are still let through as harmless. A local item —
/// [`stmt_is_transparent_item`]'s vocabulary, plus a nested `const` — carries no runtime
/// mutation this scan tracks: a `const` is resolved separately by
/// [`resolve_block_locals`]/[`resolve_scope_consts`] before this function ever runs, and
/// every other item kind (a nested `fn`, `type`, `struct`, and so on) has no value to lose.
/// A `let` with no initializer, or a `let`-`else` whose `else` diverges, binds nothing this
/// function can compute — but that is *sound* rather than merely convenient: a later read of
/// the unbound name fails through `resolved.get(&name)?`/`?` on `literal_or_const_value`
/// exactly as it would for any other unresolvable name, so skipping these two never invents
/// a value, it only ever leaves one absent.
fn resolve_block_sequential(
    stmts: &[&syn::Stmt],
    resolve: &Resolve<'_>,
    local_types: &mut std::collections::HashMap<String, String>,
    resolved: &mut std::collections::HashMap<String, i128>,
    shadow_snapshot: &mut ShadowSnapshot,
) -> Option<()> {
    for &stmt in stmts {
        let expr = match stmt {
            syn::Stmt::Local(local) => {
                let Some(init) = local.init.as_ref() else {
                    continue;
                };
                if init.diverge.is_some() {
                    resolve_let_else_binding(
                        local,
                        init,
                        resolve,
                        local_types,
                        resolved,
                        shadow_snapshot,
                    )?;
                    continue;
                }
                resolve_sequential_let(
                    local,
                    init,
                    resolve,
                    local_types,
                    resolved,
                    shadow_snapshot,
                );
                continue;
            }
            syn::Stmt::Item(_) => continue,
            syn::Stmt::Expr(expr, _) => expr,
            syn::Stmt::Macro(_) => return None,
        };
        // A `while` loop and a bare, unlabelled `{ .. }` block statement each need no
        // trailing semicolon, and each opens a nested lexical scope of its own — a `let`
        // inside either must not survive past its own closing brace. A labelled block
        // (`'a: { .. }`) is refused below rather than specially handled, since a
        // `break 'a value;` inside it can produce a value from a control-flow path this
        // scan does not trace.
        if let syn::Expr::While(while_expr) = strip_parens(expr) {
            evaluate_while_loop(while_expr, resolve, local_types, resolved)?;
            continue;
        }
        if let syn::Expr::Block(nested) = strip_parens(expr) {
            if nested.label.is_none() {
                let mut nested_snapshot = ShadowSnapshot::new();
                resolve_block_sequential(
                    &production_stmts(&nested.block),
                    resolve,
                    local_types,
                    resolved,
                    &mut nested_snapshot,
                )?;
                restore_shadow_snapshot(local_types, resolved, nested_snapshot);
                continue;
            }
        }
        // An `if`/`else` statement needs no trailing semicolon either, and each branch it
        // can take opens a nested lexical scope of its own the identical way a `while` body
        // or a bare block statement does.
        if let syn::Expr::If(if_expr) = strip_parens(expr) {
            evaluate_if_statement(if_expr, resolve, local_types, resolved)?;
            continue;
        }
        let (name, op, rhs_expr) = mutation_target(expr)?;
        let &current_value = resolved.get(&name)?;
        let mutation_resolve_value = |path: &syn::Path| {
            path.get_ident()
                .map(ident_name)
                .and_then(|candidate| resolved.get(&candidate).copied())
                .or_else(|| (resolve.value)(path))
        };
        let mutation_resolve_unsigned = |path: &syn::Path| {
            resolved_local_name(path, resolved).map_or_else(
                || (resolve.unsigned)(path),
                |candidate| {
                    local_types
                        .get(&candidate)
                        .is_some_and(|name| is_unsigned_type_name(name))
                },
            )
        };
        let mutation_resolve_width = |path: &syn::Path| {
            resolved_local_name(path, resolved).map_or_else(
                || (resolve.width)(path),
                |candidate| local_types.get(&candidate).map(String::as_str),
            )
        };
        let mutation_resolve = Resolve {
            value: &mutation_resolve_value,
            unsigned: &mutation_resolve_unsigned,
            width: &mutation_resolve_width,
        };
        let declared_type = local_types.get(&name).map(String::as_str);
        // Codex's finding: `x = x - 100;` is `syn::Expr::Assign` rather than one of the ten
        // compound-assignment `syn::Expr::Binary` shapes, so `op` is `None` here exactly
        // when `mutation_target` recognised a plain `=` — its new value is `rhs_expr`'s own,
        // evaluated against the scope as it stands *before* this statement's write, with no
        // synthetic binary needed at all; `current_value` above still gates it on the target
        // already being a local this scan resolved, the identical refusal a compound
        // assignment to an untracked name already gets, for the identical reason.
        //
        // Codex's next-round finding: the plain-assignment arm called `literal_or_const_value`
        // directly, discarding the same `declared_type` the compound-assignment arm beside it
        // already threads through — but Rust uses the *target's* own declared type as the
        // RHS's expected type for a plain assignment exactly as it does for a typed `let`, so
        // `x = !255 + n;` against a `let mut x: u8 = ..;` needs it the identical way
        // `resolve_declared_initializer` already serves a `let`'s own initializer.
        let updated = match &op {
            Some(assign_op) => apply_compound_assignment(
                current_value,
                declared_type,
                assign_op,
                &rhs_expr,
                &mutation_resolve,
            )?,
            None => resolve_declared_initializer(&rhs_expr, declared_type, &mutation_resolve)?,
        };
        resolved.insert(name, updated);
    }
    Some(())
}

/// Every statement `block` declares that a non-test build actually ships: a `#[cfg(test)]`
/// statement stripped, and a transparent local item ([`stmt_is_transparent_item`]) filtered
/// the identical way. Factored out of [`evaluate_block`]'s own inline computation so a
/// nested scope [`resolve_block_sequential`] recurses into — a `while` body, a bare nested
/// block statement — filters its own statements the same way before walking them, rather
/// than seeing a `#[cfg(test)]` statement or a local item declaration as something to be
/// interpreted or refused.
fn production_stmts(block: &syn::Block) -> Vec<&syn::Stmt> {
    block
        .stmts
        .iter()
        .filter(|stmt| !stmt_is_cfg_test(stmt) && !stmt_is_transparent_item(stmt))
        .collect()
}

fn evaluate_block(block: &syn::Block, resolve: &Resolve<'_>) -> Option<i128> {
    let const_locals = block_const_exprs(block);
    let const_item_count = const_locals.len();
    let lets = block_let_exprs(block);
    let combined_len = const_locals.len() + lets.len();
    // Codex's ordering finding is about *resolution*, not about this collision check: a
    // `let` name and a `const` item name sharing one block still have to be counted as two
    // separate declarations rather than one silently overwriting the other in this map, so
    // the check stays exactly what it always compared — it is only the merged map's role in
    // *resolving* values that `resolve_block_sequential` below takes over instead.
    let mut name_collision_check = const_locals.clone();
    for (name, expr) in &lets {
        name_collision_check.insert(name.clone(), expr.clone());
    }
    // Codex's shadowing finding: a block's own `let` types are no longer merged in here.
    // Two declarations of one name folded into a single flat entry answered for *both* —
    // whichever `resolve_block_sequential` below is not free to un-collapse once collection
    // has already discarded which declaration a given read actually fell under. Starting
    // from `const` types alone and letting the sequential walk maintain this map itself,
    // one `let` at a time, is what keeps a read between two shadows seeing the type that
    // was really in scope for it.
    let mut local_types = block_const_types(block);
    if name_collision_check.len() != combined_len {
        return None;
    }
    let ignored_lets = block_ignored_let_count(block);
    // Codex's next-round finding: `block_const_exprs`, `block_let_exprs` and
    // `block_ignored_let_count` each already skip a `#[cfg(test)]`-gated statement, the
    // same way `MatchVisitor::visit_block`'s own production walk does — but `rest` here
    // was still `block.stmts`' own *raw* slice, so a block holding one of those alongside
    // an otherwise-complete set of local declarations counted one statement more than the
    // filtered collectors ever could, and the whole block read as unresolved. Filtering
    // the statements the identical way before splitting off the tail is not just the count
    // fix: a cfg-gated statement written *last* in source order would otherwise be taken
    // for the block's own tail value, when `rustc` would really return whatever the last
    // *production* statement is.
    let stmts = production_stmts(block);
    let (tail, rest) = stmts.split_last()?;
    // Codex's next-round finding: `locals.len()` used to double as both "how many names are
    // bound" and "how many statements bound them", which `destructured_binding` returning
    // more than one name for a single `@`-bound `let` statement broke — a `let
    // whole @ _ignored = 0u8;` is one statement contributing two entries, so comparing
    // `rest.len()` (a statement count) against `locals.len()` (a name count) would refuse a
    // block this shape appears in even though every statement is accounted for.
    // `block_let_statement_count` is the statement count `block_let_exprs`'s own name count
    // stopped being; `const_item_count` is unaffected, since a `const` item never binds more
    // than one name. `block_mutation_statement_count` is `apply_compound_assignment`'s own
    // half of the identical invariant: a compound-assignment statement (`x += 1;`) binds no
    // new name at all, so it belongs on neither side of the name-counting terms, but it is
    // still one production statement `rest` counts and one this scan now folds.
    // `block_while_statement_count` is Codex's next-round finding's own term: a `while` loop
    // binds no new name either, and until it was added here `rest` counted a loop statement
    // that neither side of this sum accounted for at all, refusing the whole block regardless
    // of whether `resolve_block_sequential` below could actually run the loop.
    // `block_nested_block_statement_count` is the identical gap one syntax over: a bare
    // `{ .. }` statement binds no name at the outer scope either, and was counted by neither
    // side of this sum until now. `block_if_statement_count` is the same gap a third time: an
    // `if`/`else` statement binds no name at the outer scope either. `block_let_else_statement_count`
    // is `block_ignored_let_count`'s own twin for a `let`-`else`: it is a name-binding
    // statement, but one this sum counts structurally rather than through
    // `block_let_statement_count`, since a let-else needs its pattern actually checked
    // against its scrutinee before anyone can say whether it bound a name at all.
    if rest.len()
        != const_item_count
            + block_let_statement_count(block)
            + block_mutation_statement_count(block)
            + block_while_statement_count(block)
            + block_nested_block_statement_count(block)
            + block_if_statement_count(block)
            + block_let_else_statement_count(block)
            + ignored_lets
    {
        return None;
    }
    let syn::Stmt::Expr(tail_expr, None) = tail else {
        return None;
    };
    let mut resolved = resolve_block_locals(&const_locals, &local_types, resolve);
    // [`resolve_block_sequential`]'s own doc comment holds the rationale: a `let` and a
    // mutation are both facts about *sequence*, which `resolve_block_locals`'s own
    // order-independent fixed point (correct for `const` items alone) has no room for, so
    // the two statement kinds are walked together afterward, in the order they actually
    // execute.
    //
    // The shadow snapshot is discarded here on purpose: `block` is this call's own whole
    // scope, so a shadow it introduces is meant to stay in effect for `tail_expr` below,
    // which sits inside the same block. Only a re-executed nested scope — a `while` body,
    // in `evaluate_while_loop` — needs the snapshot back to undo it once that scope ends.
    //
    // `rest` rather than `stmts`: `tail` is `evaluate_block`'s own value expression,
    // resolved separately below through `literal_or_const_value`, and it must never be
    // handed to `resolve_block_sequential` — that function now refuses anything it does not
    // specifically recognise, and a bare value-returning tail (typically just a name) is
    // never one of those shapes.
    resolve_block_sequential(
        rest,
        resolve,
        &mut local_types,
        &mut resolved,
        &mut ShadowSnapshot::new(),
    )?;
    let block_resolve_value = |path: &syn::Path| {
        path.get_ident()
            .map(ident_name)
            .and_then(|candidate| resolved.get(&candidate).copied())
            .or_else(|| (resolve.value)(path))
    };
    let block_resolve_unsigned = |path: &syn::Path| {
        resolved_local_name(path, &resolved).map_or_else(
            || (resolve.unsigned)(path),
            |candidate| {
                local_types
                    .get(&candidate)
                    .is_some_and(|name| is_unsigned_type_name(name))
            },
        )
    };
    let block_resolve_width = |path: &syn::Path| {
        resolved_local_name(path, &resolved).map_or_else(
            || (resolve.width)(path),
            |candidate| local_types.get(&candidate).map(String::as_str),
        )
    };
    let block_resolve = Resolve {
        value: &block_resolve_value,
        unsigned: &block_resolve_unsigned,
        width: &block_resolve_width,
    };
    literal_or_const_value(tail_expr, &block_resolve)
}

/// `left op right`'s own value, for every [`syn::BinOp`] this scan folds — factored out of
/// [`literal_or_const_value`]'s own `Expr::Binary` case to keep that function under
/// clippy's line count, not because the two arithmetic and comparison halves are otherwise
/// unrelated: a `const` initializer that is real, MSRV-legal arithmetic over literals or
/// other constants (`BASE + 1`) is evaluated by `rustc` before the match it feeds ever
/// lowers, and compiles to the identical table a literal would. Checked throughout, so
/// overflow, a shift wider than the value's own bits, or division and remainder by zero
/// each fail closed to `None` rather than wrapping to a value `rustc` itself would have
/// rejected at a different one. A call to a user-defined `const fn` (`index(1)`) is not
/// evaluated — doing that in general means interpreting an arbitrary function body, which
/// this scan does not attempt — so a table whose numbered arms are spelled that way stays
/// unresolved.
///
/// Codex's next-round finding: a guard this scan can prove is always `false` is dropped
/// before `visit_expr_match` ever records the arm — but that check resolves the guard
/// through this same function, and a comparison (`_ if 1 == 0 => ..`) is `Expr::Binary`
/// with a comparison operator this `match` had no arm for at all, so it fell to the
/// wildcard `_ => None` below and the guard stayed unresolved rather than provably `0`. A
/// comparison between two values this scan already resolved is exactly as sound to fold as
/// an arithmetic one — `bool`'s own `0`/`1` representation, the identical one
/// `lit_value`'s `Lit::Bool` case and `Expr::If`'s own condition already use.
///
/// `Eq`/`Ne` are folded here because bit-pattern equality does not care which of
/// `i128`/`u128` a value is really meant as; `Lt`/`Le`/`Gt`/`Ge` are not, for the reason
/// [`evaluate_ordering_op`] states, and are folded there instead — the same split `&&`/`||`
/// already have with [`evaluate_short_circuit_op`].
///
/// Codex's next-round finding: `Div`/`Rem` are not sound here for the identical reason
/// ordering is not — `left.checked_div(right)` divides the shared 128-bit storage as a
/// *signed* `i128`, and an upper-half `u128` value that storage reinterprets as negative
/// divides to a completely different quotient than the unsigned division `rustc` performs.
/// [`evaluate_division_op`] is where the two operators live now, needing the operand
/// *expressions* [`is_definitely_unsigned`] reads, exactly as [`evaluate_ordering_op`]
/// already does.
///
/// Codex's next-round finding: `Add`/`Sub`/`Mul` are not sound *checked* here either, for a
/// third reason beside ordering's and division's — not because either operand's own bit
/// pattern is ambiguous (`(1u128 << 126) + (1u128 << 126)`'s two operands are each `2^126`,
/// an ordinary non-negative `i128` value either domain agrees on), but because the *result*
/// — `2^127` — overflows `i128::MAX` while fitting `u128` with room to spare. `checked_add`
/// answers `None` for a sum `rustc` computes without complaint, and every constant built
/// from it stays unresolved. [`evaluate_additive_op`] is where the three operators live now,
/// retrying in the `u128` domain — once [`is_definitely_unsigned`] confirms that is what the
/// operation means — exactly when the `i128` domain's own checked arithmetic already failed.
///
/// Codex's next-round finding: `Shr` shares division's and ordering's ambiguity and not
/// addition's — `i128::checked_shr` performs an *arithmetic* shift, sign-extending a
/// negative value's own top bits, where the unsigned `u128` shift `rustc` performs for an
/// upper-half value zero-fills instead, so `u128::MAX >> 127` folds to `-1` here rather than
/// the `1` a real unsigned shift gives. [`evaluate_shift_op`] is where `Shr` lives now,
/// folded the same way [`evaluate_ordering_op`] folds ordering.
///
/// Codex's next-round finding: `Shl` was kept here on the reasoning that a left shift's own
/// bit pattern is identical whichever domain it is read in — true, and not the question:
/// this function is never handed the operand *expressions*, only their already-resolved
/// values, so it has no way to learn how wide the shifted operand's own type is and to
/// truncate the result to it the way real Rust does. `128u8 << 1` is `0u8`, not the `256`
/// this used to answer. [`evaluate_shl_op`] is where `Shl` lives now, needing the left
/// operand's own expression for exactly that reason.
fn evaluate_binary_op(op: syn::BinOp, left: i128, right: i128) -> Option<i128> {
    match op {
        syn::BinOp::BitAnd(_) => Some(left & right),
        syn::BinOp::BitOr(_) => Some(left | right),
        syn::BinOp::BitXor(_) => Some(left ^ right),
        syn::BinOp::Eq(_) => Some(i128::from(left == right)),
        syn::BinOp::Ne(_) => Some(i128::from(left != right)),
        // `&&`/`||`, the four ordering comparisons, `Div`/`Rem`, `Add`/`Sub`/`Mul`, `Shr` and
        // `Shl` are not folded here at all — [`literal_or_const_value`]'s own `Expr::Binary`
        // case handles each in a match arm of its own, before this function's caller would
        // otherwise require both operands to resolve (for `&&`/`||`) or lose the operand
        // expressions this function never sees (for every other one of them).
        _ => None,
    }
}

/// Bundles the two things every constant-expression evaluator here needs about a
/// `syn::Path` — the value it resolves to, and whether its own declaration names an
/// unsigned integer type — behind one reference, the way [`ResolutionContext`] already
/// bundles the loose lookup parameters a path resolver was once passed positionally.
///
/// Codex's next-round finding: [`is_definitely_unsigned`] recognised a suffixed literal or a
/// cast, but not a bare `Expr::Path` referring to an already-typed constant — `const HI:
/// u128 = 1u128 << 127; const ZERO: u128 = 0; _ if HI < ZERO => ..` resolves both operands
/// to values, and `HI`'s value is negative in this scan's own `i128` storage, but neither
/// operand expression is itself a literal or a cast for that function's existing cases to
/// read. Every caller of [`literal_or_const_value`] already threads a value resolver
/// through every recursive call; this is that same resolver widened to carry a second
/// closure alongside it; rather than adding a second loose parameter everywhere the first
/// one is threaded, which would touch the signature of a function like
/// [`is_catchall_pattern`] that has no use for the second closure at all, every place that
/// only ever *forwards* `resolve` on to a further call needs no change beyond the type this
/// struct gives it.
struct Resolve<'a> {
    /// `path`'s own value, when it resolves to one.
    value: &'a dyn Fn(&syn::Path) -> Option<i128>,
    /// Whether `path`'s own declaration names an unsigned integer type — `false` once
    /// nothing here can tell, which is always the sound answer for a fact this scan cannot
    /// yet confirm, never the sound answer for one it could confirm and got wrong.
    unsigned: &'a dyn Fn(&syn::Path) -> bool,
    /// `path`'s own declared integer type name (`"u8"`, `"i32"`, ..), when its declaration
    /// is a bare `const` this scan can see one for — `None` once nothing here can tell,
    /// the identical safe decline `unsigned` gives for the same reason: this is a narrower
    /// question `unsigned` cannot answer (a width, not only a sign), needed only by the `!`
    /// case in [`literal_or_const_value`], so every other caller of this struct declines it
    /// unconditionally rather than reasoning about a fact it has no use for.
    width: &'a dyn Fn(&syn::Path) -> Option<&'a str>,
}

/// Whether `expr` is written with an explicit unsigned integer type — a suffixed literal
/// (`0u128`), a cast to one (`x as u128`), a bare path whose own declaration names one
/// (`HI`, where `const HI: u128 = ..;` is in scope), or an arithmetic or bitwise expression
/// either of whose own operands already is — seen through any nesting of parentheses or
/// brace groups. The only way [`evaluate_ordering_op`] can know an operand's own *type*
/// rather than only its resolved bit pattern, which is what lets it tell a genuinely
/// negative value apart from an upper-half `u128` one wrapped around.
///
/// Codex's next-round finding: `(1u128 << 126) + (1u128 << 126)` names two operands neither
/// of which this function recognised — each is `Expr::Binary(Shl)`, not a literal, a cast or
/// a bare path — even though Rust requires `+`'s two operands to share one type, so a
/// suffixed `u128` on either side of the shift settles what the *whole* addition is typed
/// as. `Add`/`Sub`/`Mul`/`BitAnd`/`BitOr`/`BitXor` all share that same-type requirement on
/// both sides, so confirming *either* side is unsigned confirms the operator's own result
/// is too. `Shl`/`Shr` do not: Rust lets the shift amount be a different, narrower integer
/// type than the value being shifted (`huge_u128 << 5u32` is ordinary, legal Rust), so only
/// the left — the value actually being shifted, and the one whose type the result shares —
/// is asked; confirming the shift *amount*'s own type would say nothing about the operand
/// that matters. `Eq`/`Ne`/the four ordering comparisons/`&&`/`||` are excluded on purpose:
/// each answers `bool`, a type of its own that shares nothing with its operands'.
fn is_definitely_unsigned(expr: &syn::Expr, resolve: &Resolve<'_>) -> bool {
    match strip_parens(expr) {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(int),
            ..
        }) => is_unsigned_type_name(int.suffix()),
        syn::Expr::Cast(cast) => {
            single_segment_type_name(&cast.ty).is_some_and(|name| is_unsigned_type_name(&name))
        }
        syn::Expr::Path(path) if path.qself.is_none() => (resolve.unsigned)(&path.path),
        syn::Expr::Binary(binary)
            if matches!(
                binary.op,
                syn::BinOp::Add(_)
                    | syn::BinOp::Sub(_)
                    | syn::BinOp::Mul(_)
                    | syn::BinOp::BitAnd(_)
                    | syn::BinOp::BitOr(_)
                    | syn::BinOp::BitXor(_)
            ) =>
        {
            is_definitely_unsigned(&binary.left, resolve)
                || is_definitely_unsigned(&binary.right, resolve)
        }
        syn::Expr::Binary(binary)
            if matches!(binary.op, syn::BinOp::Shl(_) | syn::BinOp::Shr(_)) =>
        {
            is_definitely_unsigned(&binary.left, resolve)
        }
        // Codex's next-round finding: `!x` — bitwise negation — flips every bit of `x`
        // without changing its type, so `(!0u128) >> 127` names an operand this function
        // did not recognise even though nothing about it is ambiguous: the operand's type
        // is exactly its inner expression's own. Recursing through `!` costs nothing when
        // the inner expression is a `bool` rather than an integer — every arm above answers
        // `false` for a `bool` literal or path just as it already would have for one wrapped
        // in nothing at all.
        syn::Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Not(_)) => {
            is_definitely_unsigned(&unary.expr, resolve)
        }
        // Codex's next-round finding: `(const { u128::MAX }) >> 127` names an inline
        // const block — `Expr::Const` — whose own tail expression is exactly as unsigned
        // as `literal_or_const_value`'s identical `Expr::Const` case already evaluates it
        // as a *value*, but nothing here had ever asked the block anything at all. Scoped
        // to the one shape this scan resolves a block's own unsignedness for without
        // guessing: a block holding nothing but a bare tail expression, no local `let` or
        // `const` beside it — recursed into with the same `resolve`, since such a block
        // introduces no scope of its own for a further name to shadow. A block with any
        // other statement in it declines, the same standing every other shape this
        // function cannot answer for already has.
        syn::Expr::Const(expr_const) => match expr_const.block.stmts.as_slice() {
            [syn::Stmt::Expr(tail, None)] => is_definitely_unsigned(tail, resolve),
            _ => false,
        },
        _ => false,
    }
}

/// [`is_definitely_unsigned`]'s own signed twin, needed only by [`evaluate_shift_op`]'s
/// `Shr` fold: a suffixed literal, a negation of one (`-128i8`, which reaches `Shr` as
/// `Expr::Unary(Neg, ..)` over the *positive* literal `128i8`, so the suffix lives one
/// level deeper than the operand `Shr` itself sees), a cast, or a bare path whose
/// declaration `resolve.width` names one of the five signed fixed-width types.
///
/// Codex's finding: `(-128i8 >> 7) + 1` evaluates to `0` in real Rust because `>>` on a
/// signed operand is an arithmetic shift — floor division by a power of two, which
/// [`i128::checked_shr`] already performs natively on the true, sign-extended value this
/// scan stores (negating a literal here computes the real mathematical value rather than a
/// narrower bit pattern, so no width reinterpretation is needed the way a *left* shift's
/// own truncation is) — but [`evaluate_shift_op`] refused every negative operand it could
/// not confirm as unsigned, leaving `BASE` through `BASE + 14` unresolved and the dense
/// match built from them unrecognised.
fn is_definitely_signed(expr: &syn::Expr, resolve: &Resolve<'_>) -> bool {
    match strip_parens(expr) {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(int),
            ..
        }) => is_signed_type_name(int.suffix()),
        syn::Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Neg(_)) => {
            is_definitely_signed(&unary.expr, resolve)
        }
        syn::Expr::Cast(cast) => {
            single_segment_type_name(&cast.ty).is_some_and(|name| is_signed_type_name(&name))
        }
        syn::Expr::Path(path) if path.qself.is_none() => {
            (resolve.width)(&path.path).is_some_and(is_signed_type_name)
        }
        _ => false,
    }
}

/// `pattern`'s own [`is_definitely_unsigned`], for the two arm-pattern shapes
/// [`FoundArm::unsigned`] needs it for: a suffixed literal, and a bare path
/// `resolve.unsigned` answers for. Unwraps a reference pattern (`&0u8`) and a
/// parenthesized one the identical way [`pattern_literal`] already does before it looks at
/// either shape; declines — `false`, the safe default — for every other pattern this scan
/// resolves a value from, ranges and or-patterns included, since a range or an or-pattern
/// mixing operands of different confirmed signedness has no one answer this function could
/// give without guessing which operand decided it.
fn pattern_is_definitely_unsigned(pattern: &syn::Pat, resolve: &Resolve<'_>) -> bool {
    match pattern {
        syn::Pat::Type(pat_type) => pattern_is_definitely_unsigned(&pat_type.pat, resolve),
        syn::Pat::Paren(paren) => pattern_is_definitely_unsigned(&paren.pat, resolve),
        syn::Pat::Reference(reference) => pattern_is_definitely_unsigned(&reference.pat, resolve),
        syn::Pat::Lit(syn::PatLit {
            lit: syn::Lit::Int(int),
            ..
        }) => is_unsigned_type_name(int.suffix()),
        syn::Pat::Path(path) if path.qself.is_none() => (resolve.unsigned)(&path.path),
        syn::Pat::Ident(named) if named.subpat.is_none() && named.by_ref.is_none() => {
            (resolve.unsigned)(&syn::Path::from(named.ident.clone()))
        }
        _ => false,
    }
}

/// `left_expr op right_expr`'s own value, for the four ordering comparisons
/// (`Lt`/`Le`/`Gt`/`Ge`) — factored out of [`evaluate_binary_op`] because these, unlike
/// every other operator that function folds, need the operand *expressions* themselves,
/// not only their resolved values, to answer every case this scan can soundly decide.
///
/// Codex's next-round finding: `Eq`/`Ne` are sound at any value this domain holds, because
/// bit-pattern equality does not care which of `i128`/`u128` a value is really meant as —
/// but ordering is not. `lit_value` and `apply_integer_cast` both store a `u128` value
/// above `i128::MAX` as its own two's-complement bit pattern reinterpreted as a *negative*
/// `i128` — the identical storage a genuinely negative `i8`..`i128` value already uses —
/// so a negative value in this domain is ambiguous between "really negative" and "a large
/// unsigned value wrapped around", and ordering the two ways disagrees whenever either
/// operand is negative. A first version of this fix folded ordering only when both
/// operands were non-negative, refusing every ambiguous case rather than guessing — sound,
/// but overbroad: `0x80000000000000000000000000000000u128 < 0u128` is written with an
/// explicit `u128` suffix on *both* sides, so nothing is actually ambiguous about which
/// domain it means, and the same signed-only rule left this comparison — and the guard
/// spelled with it — unresolved too, when `rustc` folds it to `false` outright.
///
/// Codex's next-round finding after that: only an operand that is itself negative in this
/// domain needs its type *confirmed* by [`is_definitely_unsigned`] before its bit pattern
/// is reinterpreted as `u128` for ordering — a non-negative operand's value is identical
/// whichever domain it is really meant as, so requiring proof of *its* type as well would
/// refuse cases (`huge_u128 < 0`, the right side an ordinary, unsuffixed `0`) that are not
/// actually ambiguous either.
///
/// Codex's next-round finding after that: `is_definitely_unsigned` is not the only proof
/// that resolves the ambiguity — [`is_definitely_signed`] resolves it the other way. A
/// negative operand confirmed signed is not a large `u128` value wrapped around at all; the
/// stored `i128` bit pattern already *is* its true value, and reinterpreting it as `u128`
/// would compute the wrong ordering rather than merely fail to compute one. So a negative
/// operand is ambiguous, and this function refuses, only when it is confirmed as *neither* —
/// and the `u128` reinterpretation is applied only when some negative operand is confirmed
/// unsigned, never merely because one is present, so a comparison with every negative
/// operand confirmed signed (and none confirmed unsigned) is folded as an ordinary signed
/// comparison instead.
fn evaluate_ordering_op(
    op: syn::BinOp,
    left_expr: &syn::Expr,
    right_expr: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    let left = literal_or_const_value(left_expr, resolve)?;
    let right = literal_or_const_value(right_expr, resolve)?;
    let left_ambiguous = left < 0
        && !is_definitely_unsigned(left_expr, resolve)
        && !is_definitely_signed(left_expr, resolve);
    let right_ambiguous = right < 0
        && !is_definitely_unsigned(right_expr, resolve)
        && !is_definitely_signed(right_expr, resolve);
    if left_ambiguous || right_ambiguous {
        return None;
    }
    let reinterpret_as_unsigned = (left < 0 && is_definitely_unsigned(left_expr, resolve))
        || (right < 0 && is_definitely_unsigned(right_expr, resolve));
    let ordering = if reinterpret_as_unsigned {
        #[allow(
            clippy::cast_sign_loss,
            reason = "reinterpreting the shared 128-bit storage as unsigned, once an \
                      explicit suffix or cast has confirmed that is what a negative \
                      operand means, not converting a value"
        )]
        let unsigned_ordering = (left as u128).cmp(&(right as u128));
        unsigned_ordering
    } else {
        left.cmp(&right)
    };
    Some(i128::from(match op {
        syn::BinOp::Lt(_) => ordering.is_lt(),
        syn::BinOp::Le(_) => ordering.is_le(),
        syn::BinOp::Gt(_) => ordering.is_gt(),
        syn::BinOp::Ge(_) => ordering.is_ge(),
        _ => return None,
    }))
}

/// `left_expr op right_expr`'s own value, for `Div`/`Rem` — [`evaluate_ordering_op`]'s own
/// reasoning, applied to the other pair of operators this domain's shared storage makes
/// ambiguous. Factored out of [`evaluate_binary_op`] for the identical reason: division
/// needs the operand *expressions* themselves, not only their resolved values, to tell a
/// genuinely negative operand apart from an upper-half `u128` one wrapped around.
///
/// Codex's next-round finding: `0xffffffffffffffffffffffffffffffffu128 / 2` is a real
/// `u128` division `rustc` performs unsigned, landing well inside the positive half of
/// `u128`'s own range — but the dividend's stored `i128` bit pattern is negative, and
/// `i128::checked_div` divides that negative value as itself, landing on a completely
/// different (and much smaller in magnitude) quotient. Exactly [`evaluate_ordering_op`]'s
/// own ambiguity, resolved the same way: an operand negative in this domain needs its type
/// confirmed unsigned before its bit pattern is reinterpreted as `u128` for the arithmetic,
/// and both operands non-negative divide identically whichever domain they are really meant
/// as, so plain `i128` division still answers that case. A `u128` division whose own
/// *quotient* lands back in the upper half is reinterpreted as its own two's-complement bit
/// pattern on the way out, the same convention [`lit_value`] and [`apply_integer_cast`]
/// already use for a `u128` value at or above `2^127`.
fn evaluate_division_op(
    op: syn::BinOp,
    left_expr: &syn::Expr,
    right_expr: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    let left = literal_or_const_value(left_expr, resolve)?;
    let right = literal_or_const_value(right_expr, resolve)?;
    if (left < 0 && !is_definitely_unsigned(left_expr, resolve))
        || (right < 0 && !is_definitely_unsigned(right_expr, resolve))
    {
        return None;
    }
    if left >= 0 && right >= 0 {
        return match op {
            syn::BinOp::Div(_) => left.checked_div(right),
            syn::BinOp::Rem(_) => left.checked_rem(right),
            _ => None,
        };
    }
    #[allow(
        clippy::cast_sign_loss,
        reason = "reinterpreting the shared 128-bit storage as unsigned, once an explicit \
                  suffix or cast has confirmed that is what a negative operand means, not \
                  converting a value"
    )]
    let (left_unsigned, right_unsigned) = (left as u128, right as u128);
    if right_unsigned == 0 {
        return None;
    }
    let result = match op {
        syn::BinOp::Div(_) => left_unsigned / right_unsigned,
        syn::BinOp::Rem(_) => left_unsigned % right_unsigned,
        _ => return None,
    };
    #[allow(
        clippy::cast_possible_wrap,
        reason = "deliberate two's-complement bit reinterpretation of a `u128` result at or \
                  above 2^127, the same convention every other u128-domain value in this \
                  scan uses, not a value conversion"
    )]
    let reinterpreted = result as i128;
    Some(reinterpreted)
}

/// Whether `op` is one of the seven operators [`evaluate_additive_or_shift_op`] routes —
/// `Div`/`Rem`/`Add`/`Sub`/`Mul`/`Shr`/`Shl` — factored into its own small function so the
/// guard naming them in [`literal_or_const_value`]'s own `match` stays one line, the same
/// reason every other arm in that function is kept under clippy's per-function line count.
///
/// Codex's next-round finding: `Shl` used to stay in [`evaluate_binary_op`] on the
/// reasoning that a left shift's own *bit pattern* does not depend on which domain it is
/// read in — true, and beside the point [`evaluate_shl_op`]'s own doc comment states: which
/// bits *survive* the shift depends on the operand's own width, which `evaluate_binary_op`
/// has no way to ask for since it is never handed the operand expressions at all. Routed
/// here instead, the identical way `Shr` already is, for the identical reason.
const fn is_additive_or_shift_or_division_op(op: syn::BinOp) -> bool {
    matches!(
        op,
        syn::BinOp::Div(_)
            | syn::BinOp::Rem(_)
            | syn::BinOp::Add(_)
            | syn::BinOp::Sub(_)
            | syn::BinOp::Mul(_)
            | syn::BinOp::Shr(_)
            | syn::BinOp::Shl(_)
    )
}

/// Routes `Div`/`Rem` to [`evaluate_division_op`], `Shr` to [`evaluate_shift_op`], `Shl` to
/// [`evaluate_shl_op`], and `Add`/`Sub`/`Mul` to [`evaluate_additive_op`] — factored out of
/// [`literal_or_const_value`]'s own `Expr::Binary` case to keep that function under
/// clippy's line count, the same reason the ordering case is factored the way it is.
fn evaluate_additive_or_shift_op(
    op: syn::BinOp,
    left_expr: &syn::Expr,
    right_expr: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    match op {
        syn::BinOp::Div(_) | syn::BinOp::Rem(_) => {
            evaluate_division_op(op, left_expr, right_expr, resolve)
        }
        syn::BinOp::Shr(_) => evaluate_shift_op(left_expr, right_expr, resolve),
        syn::BinOp::Shl(_) => evaluate_shl_op(left_expr, right_expr, resolve),
        _ => evaluate_additive_op(op, left_expr, right_expr, resolve),
    }
}

/// `left_expr op right_expr`'s own value, for `Add`/`Sub`/`Mul` — needing the operand
/// *expressions* for a different reason than [`evaluate_ordering_op`] and
/// [`evaluate_division_op`] do: not because either operand's own bit pattern is ambiguous,
/// but because `rustc`'s own `u128` arithmetic can cross `i128::MAX` without overflowing at
/// all, and this scan's plain `i128`-checked arithmetic has no way to tell that crossing
/// apart from a genuine overflow.
///
/// Codex's next-round finding: `(1u128 << 126) + (1u128 << 126)` is `2^127` — a real `u128`
/// sum, well inside that type's own range — but `i128::checked_add` answers `None`, since
/// `2^127` is one past `i128::MAX`. Retried in the `u128` domain once
/// [`is_definitely_unsigned`] confirms *either* operand's own type — `Add`, `Sub` and `Mul`
/// all require both sides to share one type in real Rust, so confirming either one confirms
/// the whole operation — but only when the plain `i128`-checked arithmetic has already
/// failed: an ordinary result both domains agree on is returned directly, without needing
/// either operand's type confirmed at all, the same shape [`evaluate_ordering_op`]'s own
/// non-negative case already has.
fn evaluate_additive_op(
    op: syn::BinOp,
    left_expr: &syn::Expr,
    right_expr: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    let left = literal_or_const_value(left_expr, resolve)?;
    let right = literal_or_const_value(right_expr, resolve)?;
    let signed_result = match op {
        syn::BinOp::Add(_) => left.checked_add(right),
        syn::BinOp::Sub(_) => left.checked_sub(right),
        syn::BinOp::Mul(_) => left.checked_mul(right),
        _ => None,
    };
    if signed_result.is_some() {
        return signed_result;
    }
    if !is_definitely_unsigned(left_expr, resolve) && !is_definitely_unsigned(right_expr, resolve) {
        return None;
    }
    #[allow(
        clippy::cast_sign_loss,
        reason = "reinterpreting the shared 128-bit storage as unsigned, once an explicit \
                  suffix or cast has confirmed that is what one of the two operands means, \
                  not converting a value"
    )]
    let (left_unsigned, right_unsigned) = (left as u128, right as u128);
    let result = match op {
        syn::BinOp::Add(_) => left_unsigned.checked_add(right_unsigned),
        syn::BinOp::Sub(_) => left_unsigned.checked_sub(right_unsigned),
        syn::BinOp::Mul(_) => left_unsigned.checked_mul(right_unsigned),
        _ => None,
    }?;
    #[allow(
        clippy::cast_possible_wrap,
        reason = "deliberate two's-complement bit reinterpretation of a `u128` result at or \
                  above 2^127, the same convention every other u128-domain value in this \
                  scan uses, not a value conversion"
    )]
    let reinterpreted = result as i128;
    Some(reinterpreted)
}

/// `left_expr >> right_expr`'s own value — needing the shifted operand's own *expression*
/// for the identical reason [`evaluate_ordering_op`] and [`evaluate_division_op`] need
/// theirs: `i128::checked_shr` performs an arithmetic shift, sign-extending a negative
/// value's own top bits, where the unsigned `u128` shift `rustc` performs for an upper-half
/// value zero-fills instead. Only the left operand — the value being shifted, whose type the
/// result shares — is asked; the shift amount on the right may legally be a different,
/// narrower type in real Rust (`huge_u128 >> 5u32`), so confirming *its* type would say
/// nothing about the one that matters. [`evaluate_shl_op`] needs the identical expression for
/// a different reason of its own — see that function's doc comment.
fn evaluate_shift_op(
    left_expr: &syn::Expr,
    right_expr: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    let left = literal_or_const_value(left_expr, resolve)?;
    let right = literal_or_const_value(right_expr, resolve)?;
    let shift = u32::try_from(right).ok()?;
    if left < 0 {
        // [`is_definitely_signed`]'s own doc comment holds the rationale: an arithmetic
        // shift of a confirmed-signed operand is floor division by a power of two, which
        // `checked_shr` already performs natively on the true value this scan stores —
        // width-independent, so no further reinterpretation is needed the way the unsigned
        // path below needs one.
        if is_definitely_signed(left_expr, resolve) {
            return left.checked_shr(shift);
        }
        if !is_definitely_unsigned(left_expr, resolve) {
            return None;
        }
    }
    #[allow(
        clippy::cast_sign_loss,
        reason = "reinterpreting the shared 128-bit storage as unsigned, once an explicit \
                  suffix or cast has confirmed that is what a negative operand means, or \
                  because the value was already non-negative, not converting a value"
    )]
    let left_unsigned = left as u128;
    let result = left_unsigned.checked_shr(shift)?;
    #[allow(
        clippy::cast_possible_wrap,
        reason = "deliberate two's-complement bit reinterpretation of a `u128` result at or \
                  above 2^127, the same convention every other u128-domain value in this \
                  scan uses, not a value conversion"
    )]
    let reinterpreted = result as i128;
    Some(reinterpreted)
}

/// `left_expr << right_expr`'s own value, truncated to the *shifted* operand's own declared
/// width — needing the left operand's own expression for a different reason than
/// [`evaluate_shift_op`]'s own doc comment states for `Shr`: a left shift's own bit pattern
/// really is identical whichever domain it is read in, since it only ever moves bits toward
/// the top and drops what falls off — but *which* bits fall off depends on how wide the
/// operand's own type is, and this scan's shared `i128` storage has no width of its own to
/// drop them from. `128u8 << 1` is `0u8` in real Rust, not `256`, because the ninth bit the
/// full-width shift would keep has nowhere to live in an eight-bit register.
///
/// Codex's next-round finding: the version of this fold that lived in
/// [`evaluate_binary_op`] computed `left.checked_shl(shift)` and returned that raw,
/// full-width result directly, so `if (128u8 << 1) == 0 { .. } else { .. }` read the shift
/// as `256` and took the wrong branch of every constant built from it. The operand's own
/// width is read the identical narrow way [`evaluate_bitwise_not`]'s own operand search
/// already does, in the same order: a literal's own suffix first, then a cast's own
/// destination type, then a bare path's declared type through `resolve.width` — `None` for
/// anything this scan cannot confirm a width for, which declines to fold rather than
/// guessing one and getting it wrong the way the unwidth-aware version did.
fn evaluate_shl_op(
    left_expr: &syn::Expr,
    right_expr: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    let left = literal_or_const_value(left_expr, resolve)?;
    let right = literal_or_const_value(right_expr, resolve)?;
    let shift = u32::try_from(right).ok()?;
    let shifted = left.checked_shl(shift)?;
    if let Some(int) = as_suffixed_int_literal(left_expr) {
        let ty = syn::parse_str::<syn::Type>(int.suffix()).ok()?;
        return apply_integer_cast(shifted, &ty);
    }
    if let syn::Expr::Cast(cast) = strip_parens(left_expr) {
        return apply_integer_cast(shifted, &cast.ty);
    }
    let syn::Expr::Path(path) = strip_parens(left_expr) else {
        return None;
    };
    if path.qself.is_some() {
        return None;
    }
    let width_name = (resolve.width)(&path.path)?;
    let ty = syn::parse_str::<syn::Type>(width_name).ok()?;
    apply_integer_cast(shifted, &ty)
}

/// `left && right` or `left || right`'s own value (`is_and` selects which), evaluated
/// with real short-circuit semantics: `right` is resolved only when `left` alone does
/// not already decide the whole expression's answer. Factored out of
/// [`literal_or_const_value`]'s own `Expr::Binary` case to keep that function under
/// clippy's line count.
///
/// Codex's next-round finding: `false && opaque()` — a decisive left operand beside a
/// right one this scan cannot resolve (a call, here) — stayed unresolved under an eager
/// version of this case that required *both* sides to resolve before
/// [`evaluate_binary_op`] was even called, exactly the difference from `rustc`'s real
/// short-circuit behaviour that function's own doc comment had already flagged. `rustc`
/// never evaluates `opaque()` at all once `false` has already decided `&&`'s answer, and
/// this scan can match that without interpreting arbitrary control flow: only `left` is
/// required to resolve; if it alone decides the result (`false` for `&&`, `true` for
/// `||`), that answer is returned directly and `right` is never asked to resolve at all —
/// the identical shape of "never reached" this scan already extends to every unresolved
/// right-hand call. Otherwise `left` was the non-deciding value (`true` for `&&`, `false`
/// for `||`), so `right` is what the whole expression's value actually is, and it alone
/// is resolved.
fn evaluate_short_circuit_op(
    is_and: bool,
    left: &syn::Expr,
    right: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    let left_value = literal_or_const_value(left, resolve)?;
    if is_and && left_value == 0 {
        return Some(0);
    }
    if !is_and && left_value != 0 {
        return Some(1);
    }
    let right_value = literal_or_const_value(right, resolve)?;
    Some(i128::from(right_value != 0))
}

/// `indexed`'s own value — `expr[index]` — for every indexable literal shape this scan
/// folds. Factored out of [`literal_or_const_value`]'s own `Expr::Index` case to keep that
/// function under clippy's line count.
///
/// Codex's next-round finding: `const P0: u8 = [0u8][0];` is `Expr::Index` over an array
/// *literal*, built solely so the indexing yields a constant — which fell to the wildcard
/// `_ => None` case in `literal_or_const_value` and left every such arm unresolved, the
/// const-call and array-vocabulary backstops both included, since each declared
/// constant's own type is a plain `u8` and neither backstop reads an initializer's
/// *shape*. Scoped to exactly that shape, the same way the tuple- and struct-literal
/// `Expr::Field` case is: the indexed expression must itself be one of the shapes below —
/// nothing reaches outside this expression for a value, so indexing a path or a slice
/// reference is not attempted — and the index itself resolved through this same pipeline;
/// an index outside the literal's own bounds, or one this scan cannot resolve to a value,
/// stays unresolved rather than guessed at.
///
/// Codex's next-round finding: `[0u8; 1][0]` — a *repeat* array literal (`syn`'s
/// `Expr::Repeat`, the `[value; count]` grammar), rather than the bracketed-list
/// `Expr::Array` this function first handled — fell straight through that arm's own
/// refusal, for the same reason the tuple/struct split needed a second match arm two
/// rounds earlier. Every element of a repeat literal is definitionally the same
/// expression, so this resolves the index only far enough to bounds-check it against the
/// (separately resolved) repeat count, then evaluates that one shared element expression
/// rather than looking anything up positionally.
///
/// Codex's next-round finding: `b"\x00"[0]` indexes a byte-string *literal* (`Expr::Lit`
/// wrapping `syn::Lit::ByteStr`), a third indexable shape beside the two array literals
/// above and one this case's own bounds check does not apply to the same way — a byte
/// string carries its own length in its bytes rather than in a separate `count`
/// expression, so there is nothing to resolve before indexing, only a bounds check
/// against that length.
fn evaluate_index(indexed: &syn::ExprIndex, resolve: &Resolve<'_>) -> Option<i128> {
    let index = usize::try_from(literal_or_const_value(&indexed.index, resolve)?).ok()?;
    match indexed.expr.as_ref() {
        syn::Expr::Array(array) => literal_or_const_value(array.elems.get(index)?, resolve),
        syn::Expr::Repeat(repeat) => {
            let len = usize::try_from(literal_or_const_value(&repeat.len, resolve)?).ok()?;
            (index < len).then(|| literal_or_const_value(&repeat.expr, resolve))?
        }
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::ByteStr(byte_str),
            ..
        }) => byte_str.value().get(index).map(|&byte| i128::from(byte)),
        _ => None,
    }
}

/// `!operand`'s own value — bitwise NOT — for every shape this scan can find a *width* for.
/// Factored out of [`literal_or_const_value`]'s own `Expr::Unary(Not)` case to keep that
/// function under clippy's line count.
///
/// Codex's forty-third-round finding: `!255u8` — bitwise NOT of a literal — is
/// `Expr::Unary(Not, ..)`, which fell to the wildcard `_ => None` case, so a table whose
/// numbered arms were spelled `!255u8` through `!241u8` read as unresolved on every arm.
/// Rust's `!` flips every bit *within the operand's own width* — `!255u8` is `0`, not the
/// all-ones `i128` a naive `!raw_value` would give — so this needs that width, and a literal
/// carrying its own suffix (through any nesting of parentheses or brace groups) is the first
/// place to find one. `!value` at full `i128` width, then [`apply_integer_cast`]'s own
/// truncating mask down to the suffix's width, is the same answer a width-aware NOT would
/// give directly: masking to the low `N` bits after a full-width NOT is bit-for-bit identical
/// to NOT-ing those `N` bits alone.
///
/// Codex's next-round finding: `const Q0: u8 = 255; const P0: u8 = !Q0;` names an operand
/// with no suffix of its own to read — a bare path to an already-typed sibling constant —
/// and stayed unresolved even though `Q0`'s own declaration states the width this needs just
/// as plainly as a literal's suffix would. `resolve.width` is that declaration, read the same
/// narrow, safe-to-decline way [`is_definitely_unsigned`]'s own `Expr::Path` case reads
/// `resolve.unsigned`: `None` for anything this scan cannot confirm a width for — a qualified
/// path, one reached only through an outer scope, or a name with no declared width at all —
/// stays unresolved rather than guessed at.
///
/// Codex's next-round finding: `!(255 as u8)` carries its width in the cast's own
/// destination type rather than in a literal's suffix or a path's declaration —
/// `as_suffixed_int_literal` only reads a `Lit::Int` through parentheses or a brace group,
/// never a `Cast`, so this fell straight through to the path-only case and declined a shape
/// that names its width just as plainly as `!255u8` does. [`literal_or_const_value`]'s own
/// `Expr::Cast` case already evaluates the cast for real — recursing into the operand and
/// truncating to the destination type — so the same masked-then-truncated answer
/// [`apply_integer_cast`] gives the suffixed-literal case above is reached here by asking for
/// `operand`'s own value pre-negation, then negating within the cast's own destination width.
fn evaluate_bitwise_not(operand: &syn::Expr, resolve: &Resolve<'_>) -> Option<i128> {
    if let Some(boolean) = as_bool_literal(operand) {
        return Some(i128::from(!boolean));
    }
    if let Some(int) = as_suffixed_int_literal(operand) {
        let ty = syn::parse_str::<syn::Type>(int.suffix()).ok()?;
        let raw = lit_value(&syn::Lit::Int(int.clone()))?;
        return apply_integer_cast(!raw, &ty);
    }
    if let syn::Expr::Cast(cast) = strip_parens(operand) {
        let truncated = literal_or_const_value(operand, resolve)?;
        return apply_integer_cast(!truncated, &cast.ty);
    }
    let syn::Expr::Path(path) = strip_parens(operand) else {
        return None;
    };
    if path.qself.is_some() {
        return None;
    }
    let width_name = (resolve.width)(&path.path)?;
    let raw = literal_or_const_value(operand, resolve)?;
    // Codex's next-round finding: `const OFF: bool = false; .. !OFF` names an operand whose
    // declared type is `bool`, not an integer — `apply_integer_cast` has no width for it and
    // declines, which is sound but not the answer Rust gives: a `bool` negation flips `0`
    // and `1` exactly, the same values this scan already stores `false`/`true` as
    // ([`lit_value`]'s own `Lit::Bool` case), with no masking of any kind involved.
    if width_name == "bool" {
        return Some(i128::from(raw == 0));
    }
    let ty = syn::parse_str::<syn::Type>(width_name).ok()?;
    apply_integer_cast(!raw, &ty)
}

/// A named local or `const`'s own initializer, evaluated through [`literal_or_const_value`]
/// first and, only when that declines, one more shape it cannot reach: a bare, unsuffixed
/// bitwise-NOT whose width is written nowhere in the initializer at all — only in the
/// declaration this *is* the initializer of.
///
/// Codex's finding: `const P0: u8 = !255;` names an operand with no suffix, no cast and no
/// path to read a width from — [`evaluate_bitwise_not`]'s three fallbacks all correctly
/// decline it, because none of them is lying: `255`'s own width really is nowhere in `!255`
/// itself, the same way any other unsuffixed integer literal in Rust takes its type from the
/// position it is used in rather than carrying one of its own. `P0`'s own declared type
/// states it just as plainly as a literal's suffix would, but nothing here had ever read a
/// name's *own* declaration as a width for its *own* initializer — `evaluate_bitwise_not`'s
/// width search is scoped to what the operand names, and an initializer is not its own
/// operand.
///
/// Called only where that declaration is already in view: [`resolve_scope_consts`]'s fixed
/// point, [`resolve_block_locals`]'s, and [`resolve_sequential_let`], each of which already
/// holds the name's own declared type before it ever asks for the initializer's value — the
/// same three positions [`stmt_let_type`]'s own sequential fix already threads a declared
/// type through. Scoped narrowly on purpose: a *suffixed* literal, a cast, or a path already
/// have their own way to state a width and are declined here exactly as
/// `evaluate_bitwise_not` already declines redoing work it already did — this is only for
/// the one shape that has no width to find anywhere but the declaration.
fn resolve_declared_initializer(
    expr: &syn::Expr,
    declared_type: Option<&str>,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    if let Some(value) = literal_or_const_value(expr, resolve) {
        return Some(value);
    }
    let width = declared_type?;
    if let Some(rewritten) = cast_bare_negation_to_width(expr, width) {
        return literal_or_const_value(&rewritten, resolve);
    }
    // Codex's finding: `const Pn: u8 = !255 + n;` names a bare negation that is not the
    // *whole* initializer but one operand of a binary expression wrapping it — the shape
    // above alone still declines it, because `expr` itself is `Expr::Binary`, not
    // `Expr::Unary`. Real Rust propagates the declaration's own expected type into *both*
    // operands of an arithmetic binary the identical way it does for the initializer as a
    // whole, so each operand gets the same rewrite tried on it independently; an operand
    // that already resolves on its own (`n`, a literal or a path) is left untouched, and the
    // reconstructed binary is handed back to `literal_or_const_value`'s own `Expr::Binary`
    // dispatch rather than evaluated here, so `Add`/`Sub`/`Mul` and every other operator
    // still go through the identical width- and sign-aware evaluator every other binary
    // expression does.
    let syn::Expr::Binary(binary) = strip_parens(expr) else {
        return None;
    };
    let left = cast_bare_negation_to_width(&binary.left, width);
    let right = cast_bare_negation_to_width(&binary.right, width);
    if left.is_none() && right.is_none() {
        return None;
    }
    let mut rewritten = binary.clone();
    if let Some(left) = left {
        rewritten.left = Box::new(left);
    }
    if let Some(right) = right {
        rewritten.right = Box::new(right);
    }
    literal_or_const_value(&syn::Expr::Binary(rewritten), resolve)
}

/// Rewrites a bare, unsuffixed bitwise-NOT — `!255`, with no cast and no path of its own to
/// name a width — into `!(255 as width)`, the one shape [`evaluate_bitwise_not`] already
/// resolves on its own (a cast as the operand being negated) but could never reach unaided,
/// because the width really is nowhere in `!255` itself: only in the declaration this is
/// part of the initializer of. `None` for anything else — a suffixed literal, a cast, or a
/// path already have their own way to state a width and need no rewriting, and a caller
/// combining this with an operand that already resolves on its own leaves that operand
/// untouched.
fn cast_bare_negation_to_width(expr: &syn::Expr, width: &str) -> Option<syn::Expr> {
    let syn::Expr::Unary(unary) = strip_parens(expr) else {
        return None;
    };
    if !matches!(unary.op, syn::UnOp::Not(_)) {
        return None;
    }
    let operand = strip_parens(&unary.expr);
    let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Int(int),
        ..
    }) = operand
    else {
        return None;
    };
    if !int.suffix().is_empty() {
        return None;
    }
    let ty: syn::Type = syn::parse_str(width).ok()?;
    let cast_operand = syn::Expr::Cast(syn::ExprCast {
        attrs: Vec::new(),
        expr: Box::new(operand.clone()),
        as_token: syn::Token![as](proc_macro2::Span::call_site()),
        ty: Box::new(ty),
    });
    Some(syn::Expr::Unary(syn::ExprUnary {
        attrs: unary.attrs.clone(),
        op: unary.op,
        expr: Box::new(cast_operand),
    }))
}

/// `expr`'s own integer value: a bare literal, however based or suffixed, seen through a
/// cast, a set of parentheses or a brace group; or a path that `resolve` answers for — the
/// constant-pattern half of both [`FoundArm::pattern`] and a call argument's own value.
fn literal_or_const_value(expr: &syn::Expr, resolve: &Resolve<'_>) -> Option<i128> {
    match expr {
        syn::Expr::Lit(literal) => lit_value(&literal.lit),
        // Codex's finding: `248u8 as i8` is `-8`, not `248` — this used to discard the
        // cast's own destination type and pass the operand straight through, so a
        // `const` whose numbered arms cover `-8..=6` by casting a run of `u8` literals
        // read as `248..=255, 0..=6` and was never recognised as the contiguous window
        // it really is. [`apply_integer_cast`] applies the cast for real.
        syn::Expr::Cast(cast) => {
            apply_integer_cast(literal_or_const_value(&cast.expr, resolve)?, &cast.ty)
        }
        syn::Expr::Paren(paren) => literal_or_const_value(&paren.expr, resolve),
        syn::Expr::Group(group) => literal_or_const_value(&group.expr, resolve),
        // Codex's next-round finding: `match (0u8,) { (0,) => 0, _ => 100 }` — a one-element
        // tuple scrutinee over a one-element tuple pattern — fell to the wildcard `_ => None`
        // case below, so `evaluate_match` could never even resolve the *scrutinee*, and every
        // arm of a numbered constant built this way stayed unresolved regardless of whether
        // the pattern side could have matched it. A one-element tuple carries exactly its own
        // element's value and nothing else a dense integer table could ever be keyed on, so
        // this recurses into it the same way a parenthesized or grouped expression already
        // does; a tuple of any other arity is not a shape this scan's own `i128` domain can
        // represent at all and stays unresolved.
        syn::Expr::Tuple(tuple) if tuple.elems.len() == 1 => {
            literal_or_const_value(tuple.elems.first()?, resolve)
        }
        syn::Expr::Path(path) => (resolve.value)(&path.path),
        // [`evaluate_short_circuit_op`] holds the rationale for why `&&`/`||` are not
        // folded through `evaluate_binary_op` like every other operator.
        syn::Expr::Binary(binary)
            if matches!(binary.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) =>
        {
            evaluate_short_circuit_op(
                matches!(binary.op, syn::BinOp::And(_)),
                &binary.left,
                &binary.right,
                resolve,
            )
        }
        // [`evaluate_ordering_op`] holds the rationale for why the four ordering
        // comparisons are not folded through `evaluate_binary_op` like `Eq`/`Ne` are.
        syn::Expr::Binary(binary)
            if matches!(
                binary.op,
                syn::BinOp::Lt(_) | syn::BinOp::Le(_) | syn::BinOp::Gt(_) | syn::BinOp::Ge(_)
            ) =>
        {
            evaluate_ordering_op(binary.op, &binary.left, &binary.right, resolve)
        }
        // [`evaluate_additive_or_shift_op`] holds the rationale for this arm.
        syn::Expr::Binary(binary) if is_additive_or_shift_or_division_op(binary.op) => {
            evaluate_additive_or_shift_op(binary.op, &binary.left, &binary.right, resolve)
        }
        // [`evaluate_binary_op`] holds the rationale for every operator this folds,
        // arithmetic and comparison alike, since both are one decision rather than two.
        syn::Expr::Binary(binary) => evaluate_binary_op(
            binary.op,
            literal_or_const_value(&binary.left, resolve)?,
            literal_or_const_value(&binary.right, resolve)?,
        ),
        // Codex's finding: a negative *range endpoint* (`-8..=6`) is a full expression,
        // not the special negative-literal-pattern grammar a bare `-8` pattern parses
        // through — `syn::Pat::Range`'s own `start`/`end` are `Expr`s, so `-8` there is
        // `Expr::Unary(Neg, Expr::Lit(8))` and never reached `lit_value` at all. Checked,
        // like every other fold here: negating `i128::MIN` has no representable positive
        // counterpart and fails closed rather than wrapping.
        syn::Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Neg(_)) => {
            literal_or_const_value(&unary.expr, resolve)?.checked_neg()
        }
        // [`evaluate_bitwise_not`] holds the rationale for every shape this folds.
        syn::Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Not(_)) => {
            evaluate_bitwise_not(&unary.expr, resolve)
        }
        // Codex's next-round finding: `*&0u8` — dereferencing a reference taken in the
        // same expression — is `Expr::Unary(Deref, Expr::Reference(..))`, which fell to
        // the wildcard `_ => None` case below and left every such arm unresolved, the
        // const-call and array backstops included, since a dereference is neither a call
        // nor an array index. `*&X` is definitionally `X` for any `X`, whatever `X` turns
        // out to be, so this recurses into the reference's own inner expression through
        // the identical pipeline rather than evaluating anything new. Scoped to exactly
        // that shape — the operand must itself be a freshly taken `&`-reference — since
        // dereferencing anything else (a raw pointer, a path naming some other
        // reference-typed value) is a question about what that reference points *at*,
        // which this scan has no way to answer without guessing.
        syn::Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Deref(_)) => {
            let syn::Expr::Reference(reference) = unary.expr.as_ref() else {
                return None;
            };
            literal_or_const_value(&reference.expr, resolve)
        }
        // Codex's finding: `const P0: u8 = { const N: u8 = 0; N };` is `Expr::Block` — a
        // block used as an expression, most often to give an initializer a scope of its
        // own — and was not unwrapped at all, so a table whose numbered arms are spelled
        // this way read as unresolved on every arm. A block is exactly as evaluable as
        // its own tail expression, once any local `const` declarations feeding that tail
        // are resolved first — a small, block-scoped mirror of `resolve_scope_consts`'s
        // own fixed point, self-contained here since this function carries no
        // `ConstScopes` of its own, only the caller's flat `resolve`. Scoped narrowly:
        // every statement but the last must be a local `const` item (`block_const_exprs`
        // is what recognises one), and the last must be a semicolon-less tail
        // expression — a block holding a `let`, a loop, or any other statement shape
        // stays unresolved rather than guessed at, and so does a labelled block
        // (`'a: { .. }`) reaching here, whose tail a `break 'a value;` elsewhere in the
        // block could also supply — [`evaluate_labelled_block`] is that narrower shape,
        // tried below rather than folded into this arm's own guard.
        syn::Expr::Block(block_expr) if block_expr.label.is_none() => {
            evaluate_block(&block_expr.block, resolve)
        }
        // [`evaluate_labelled_block`] holds the rationale for the one labelled-block shape
        // this folds — [`evaluate_loop`]'s own self-targeted-break reasoning, applied to
        // the other Rust construct a labelled `break` can exit.
        syn::Expr::Block(block_expr) => evaluate_labelled_block(block_expr, resolve),
        // Codex's forty-fifth-round finding: `const P0: u8 = if SELECT_FIRST { 0 } else {
        // 100 };` is `Expr::If`, which fell to the wildcard `_ => None` case below and
        // left every such arm unresolved — and the const-call backstop
        // (`const_call_initializer_uses`) does not catch it either, since an `if` is not
        // a call. A `bool` is the only type Rust permits an `if`'s own condition to be,
        // so evaluating it through this same `i128` pipeline (`0` for `false`, anything
        // else for `true`, by way of [`lit_value`]'s own `Lit::Bool` case) and then
        // evaluating the chosen branch is exactly as sound as `Expr::Block`'s own
        // handling, which the `then` branch shares directly since it is the identical
        // `syn::Block` shape. An `if` with no `else` cannot type-check as an integer
        // value in real Rust — the missing branch would have to produce `()` — so one is
        // required rather than assumed; the `else` branch recurses through this same
        // function, which is what lets an `else if` chain resolve without a case of its
        // own.
        syn::Expr::If(if_expr) => {
            let (_, else_branch) = if_expr.else_branch.as_ref()?;
            // Codex's finding: `if let x @ 0 = 0u8 { x } else { 100 }` selects the `then`
            // branch, whose own body reads `x` — a name only `if_expr.cond`'s own
            // `Expr::Let` pattern binds. The generic path below folds `if_expr.cond` down
            // to a bare `0`/`1` through the same pipeline every other condition shape
            // uses, discarding whatever the condition bound along the way — sound for a
            // plain boolean, which introduces no binding, and unsound the moment the
            // condition is `Expr::Let`, which can introduce several. [`evaluate_if_let`]
            // answers the match and the binding together, the identical two things
            // `evaluate_match`'s own arm resolver computes for a `match` arm, and hands
            // the chosen branch a resolver that answers for them before the generic path
            // ever sees the condition.
            if let syn::Expr::Let(let_expr) = strip_parens(&if_expr.cond) {
                return evaluate_if_let(let_expr, if_expr, else_branch, resolve);
            }
            let condition = literal_or_const_value(&if_expr.cond, resolve)?;
            if condition == 0 {
                literal_or_const_value(else_branch, resolve)
            } else {
                evaluate_block(&if_expr.then_branch, resolve)
            }
        }
        // Codex's next-round finding: `if_expr.cond` is evaluated through this same
        // function above, but `if let 0 = 0u8 { 0 } else { 100 }`'s own condition is
        // `Expr::Let` — Rust's grammar permits a `let` only in an `if`'s or a `while`'s own
        // condition position, never as a general value expression, but this function had no
        // case for the node kind regardless, so it fell to the wildcard `_ => None` case
        // below and every such `if`'s own condition stayed unresolved. Evaluated the same
        // way a `match` arm's own pattern already is: the scrutinee resolved through this
        // same pipeline, then [`match_arm_matches_constant`] answers whether the pattern
        // matches it — `1` for a match and `0` for none, the identical `i128` encoding
        // [`lit_value`]'s own `Lit::Bool` case already uses for a plain boolean, so the
        // `Expr::If` case above needs no case of its own to read it.
        syn::Expr::Let(let_expr) => {
            let scrutinee = literal_or_const_value(&let_expr.expr, resolve)?;
            let matches = match_arm_matches_constant(&let_expr.pat, scrutinee, resolve)?;
            Some(i128::from(matches))
        }
        // Codex's forty-seventh-round finding: `const P0: u8 = match true { true => 0,
        // false => 100 };` is `Expr::Match`, which fell to the wildcard `_ => None` case
        // below — and the const-call backstop does not catch it either, since a `match`
        // is not a call. Scoped to what a constant match plausibly needs: the scrutinee
        // resolved through this same pipeline, then the *first* arm whose pattern names
        // that value evaluated in turn. [`match_arm_matches_constant`] is deliberately not
        // [`pattern_literal`] reused: a guard, or a pattern shape it does not recognise,
        // has to stop the search rather than being treated as "does not match" and
        // silently falling through to a later arm that might answer differently from what
        // `rustc` itself would choose.
        syn::Expr::Match(expr_match) => evaluate_match(expr_match, resolve),
        // Codex's forty-ninth-round finding: `const P0: u8 = (0u8,).0;` is `Expr::Field` —
        // a field projection on a tuple *literal*, built solely so the projection yields a
        // constant — which fell to the wildcard `_ => None` case below and left every such
        // arm unresolved, the const-call and array backstops included, since a field
        // projection is neither a call nor an array index. Scoped to exactly that shape:
        // the base must itself be an `Expr::Tuple` or `Expr::Struct` literal — nothing
        // reaches outside this expression for a value, so a projection off a path is not
        // attempted — and the member the corresponding unnamed index or named field of
        // that literal; the picked element is then evaluated through this same pipeline.
        //
        // Codex's next-round finding: a *named*-field struct literal
        // (`Cell { value: 0 }.value`) is `Expr::Struct`, a different node kind from the
        // tuple literal this case first handled, and fell straight through to the tuple
        // arm's own refusal — the const-call and array backstops do not catch it either,
        // for the same reason a tuple projection escaped them. A struct literal carrying a
        // `..rest` base is refused outright: a field this scan does not see named among
        // the literal's own fields might still come from `rest`, and guessing its value
        // from nothing written here would be exactly the kind of guess every other case in
        // this function already declines to make.
        syn::Expr::Field(field) => match field.base.as_ref() {
            syn::Expr::Tuple(tuple) => {
                let syn::Member::Unnamed(index) = &field.member else {
                    return None;
                };
                let element = tuple.elems.get(usize::try_from(index.index).ok()?)?;
                literal_or_const_value(element, resolve)
            }
            syn::Expr::Struct(struct_literal) => {
                let syn::Member::Named(name) = &field.member else {
                    return None;
                };
                if struct_literal.rest.is_some() {
                    return None;
                }
                let field_value = struct_literal.fields.iter().find(|candidate| {
                    matches!(&candidate.member, syn::Member::Named(candidate_name) if candidate_name == name)
                })?;
                literal_or_const_value(&field_value.expr, resolve)
            }
            _ => None,
        },
        // Codex's next-round finding: `const P0: u8 = const { 0u8 };` is `Expr::Const` — an
        // inline const block, MSRV-legal and evaluated by `rustc` before the match it feeds
        // ever lowers — which fell to the wildcard `_ => None` case below, the const-call
        // and array backstops included, since an inline const block is neither a call nor
        // an array index. Its own body is a plain `syn::Block`, the identical shape
        // `Expr::Block`'s own case already evaluates through `evaluate_block`, so this case
        // is nothing more than routing that body to the same function — no local `const` or
        // `let` fixed point of its own to invent.
        syn::Expr::Const(expr_const) => evaluate_block(&expr_const.block, resolve),
        // [`evaluate_index`] holds the rationale for every indexable shape this folds.
        syn::Expr::Index(indexed) => evaluate_index(indexed, resolve),
        // [`evaluate_loop`] holds the rationale for the one loop shape this folds.
        syn::Expr::Loop(expr_loop) => evaluate_loop(expr_loop, resolve),
        _ => None,
    }
}

/// `expr_loop`'s own value, for the one shape this scan folds without interpreting control
/// flow at all — factored out of [`literal_or_const_value`]'s own `Expr::Loop` case to keep
/// that function under clippy's line count.
///
/// Codex's next-round finding: `const P0: u8 = loop { break 0 };` is `Expr::Loop` — an
/// unusual but MSRV-legal way to spell a plain value inside a position that must itself be
/// an expression — which fell to the wildcard `_ => None` case, the const-call, array and
/// macro backstops all included, since a loop is none of those. Scoped to exactly the one
/// shape that is knowably total without interpreting control flow at all: the body must hold
/// exactly one statement, an unlabelled `break` expression carrying a value. Anything else —
/// a conditional break, a loop that never breaks, one breaking more than once, a labelled
/// break aimed at an outer loop — stays unresolved rather than guessed at, since deciding
/// which of several possible breaks would fire first is exactly the kind of control-flow
/// interpretation this scan does not attempt.
///
/// Codex's next-round finding: refusing *every* labelled break is stricter than the
/// control-flow question this case actually needs to decide. `'done: loop { break 'done 0
/// };` labels both the loop and its own break with the identical name — the break targets
/// *this* loop, not some outer one this scan would have to interpret control flow to find —
/// so it is exactly as total as the unlabelled shape, and `rustc` folds it to the same `0`.
/// Only a break naming a *different* label (an outer loop's) is the real ambiguity this case
/// exists to decline; compared by the label's own identifier, the same way two lifetimes are
/// compared everywhere else in this scan.
fn evaluate_loop(expr_loop: &syn::ExprLoop, resolve: &Resolve<'_>) -> Option<i128> {
    let [syn::Stmt::Expr(syn::Expr::Break(break_expr), _)] = expr_loop.body.stmts.as_slice() else {
        return None;
    };
    let targets_this_loop = match (&break_expr.label, expr_loop.label.as_ref()) {
        (None, _) => true,
        (Some(break_label), Some(loop_label)) => break_label.ident == loop_label.name.ident,
        (Some(_), None) => false,
    };
    if !targets_this_loop {
        return None;
    }
    literal_or_const_value(break_expr.expr.as_ref()?, resolve)
}

/// `block_expr`'s own value, for the one labelled-block shape this scan folds —
/// [`evaluate_loop`]'s own reasoning, applied to the other Rust construct a labelled
/// `break` can exit: a *labelled block* (`'a: { .. }`, stable since Rust 1.65), whose body
/// is exactly one statement, an unlabelled-target-free `break` naming the block's own label
/// and carrying a value.
///
/// Codex's next-round finding: `const P0: u8 = 'value: { break 'value 0 };` is exactly as
/// resolvable as `'done: loop { break 'done 0 }` — the break targets this block itself, not
/// some other construct this scan would have to interpret control flow to find — but the
/// labelled-loop fix only ever touched `Expr::Loop`; `Expr::Block`'s own guard refuses
/// *every* labelled block unconditionally, `evaluate_block` included, so every one of a
/// table's numbered arms spelled this way stayed unresolved. Scoped identically to
/// `evaluate_loop`: any other body shape — more than one statement, an unlabelled break, one
/// naming a different label — stays unresolved rather than guessed at.
fn evaluate_labelled_block(block_expr: &syn::ExprBlock, resolve: &Resolve<'_>) -> Option<i128> {
    let label = block_expr.label.as_ref()?;
    let [syn::Stmt::Expr(syn::Expr::Break(break_expr), _)] = block_expr.block.stmts.as_slice()
    else {
        return None;
    };
    let break_label = break_expr.label.as_ref()?;
    if break_label.ident != label.name.ident {
        return None;
    }
    literal_or_const_value(break_expr.expr.as_ref()?, resolve)
}

/// [`literal_or_const_value`]'s own value for `expr_match`, once its scrutinee resolves
/// to a constant: the body of the first arm whose pattern [`match_arm_matches_constant`]
/// confirms matches that value, evaluated the same way any other expression here is.
/// `None` the moment a guard appears, a pattern's own match-or-not cannot be determined,
/// or no arm matches at all — every one of those means guessing which arm `rustc` would
/// have chosen, which this scan does not do.
///
/// Codex's forty-eighth-round finding: an arm can carry its own `#[cfg(test)]`, the same
/// way a `match` [`MatchVisitor::visit_expr_match`] scans directly can, and `rustc` strips
/// such an arm from a production build. This function had read every arm regardless, so a
/// constant initialized by a match whose *production* arms are dense and whose *test-only*
/// arms are not — or the reverse — could be resolved to a value only a test build would
/// produce, feeding a wrong constant into the density check the same
/// [`has_cfg_test`]-filtered exclusion already protects `visit_expr_match` itself from.
///
/// Codex's next-round finding: a guarded arm made this bail out unconditionally, even when
/// the guard itself resolves to a compile-time constant — `_ if false => 100, _ => 0` names
/// exactly `0`, the second arm, because a guard `rustc` can prove always false is dead code
/// it eliminates before the match this scan is trying to fold ever lowers, the identical
/// pruning [`MatchVisitor::visit_expr_match`] already does for a numbered table's own outer
/// arms. Folded the same way here: an arm whose pattern does not match the scrutinee is
/// skipped regardless of its guard, since the guard is never even evaluated for it: an arm
/// whose pattern *does* match is selected outright when it carries no guard or one that
/// resolves to a nonzero (`true`) value, skipped in favour of a later arm when its guard
/// resolves to exactly `0` (`false`), and this whole function still bails out — rather than
/// guessing which of two matching arms `rustc` would pick — the moment a matching arm's own
/// guard cannot be resolved to a compile-time value at all.
/// `expr`'s own declared width name (`"u8"`, `"i32"`, ..), when this scan can confirm one —
/// the identical three-case search [`evaluate_shl_op`]'s own left operand already runs
/// (a suffixed literal's own suffix, a cast's own destination type, a bare path's declared
/// type through `resolve.width`), widened with a fourth: a well-known bound (`u128::MAX`)
/// names its own type as the path's first segment, with no scope to search at all — the
/// identical fallback [`path_is_definitely_unsigned`] itself falls back to. Answers with an
/// owned name rather than a borrowed one, since a cast's own type name
/// ([`single_segment_type_name`]) is never borrowed from anywhere this scan's own AST holds
/// long enough to return as a reference on its own.
///
/// [`evaluate_match`] and [`evaluate_tuple_match`] are this function's only callers, needing
/// an arm-bound name's own width derived from the scrutinee expression it was matched
/// against — see [`pattern_binding`]'s own doc comment for the shape of the gap this closes.
fn expr_declared_width(expr: &syn::Expr, resolve: &Resolve<'_>) -> Option<String> {
    if let Some(int) = as_suffixed_int_literal(expr) {
        let suffix = int.suffix();
        return (!suffix.is_empty()).then(|| suffix.to_string());
    }
    if let syn::Expr::Cast(cast) = strip_parens(expr) {
        return single_segment_type_name(&cast.ty);
    }
    let syn::Expr::Path(path) = strip_parens(expr) else {
        return None;
    };
    if path.qself.is_some() {
        return None;
    }
    if let Some(width) = (resolve.width)(&path.path) {
        return Some(width.to_string());
    }
    let segments: Vec<String> = path
        .path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    let (type_name, _member) = well_known_bound_segments(&segments)?;
    (is_unsigned_type_name(type_name) || is_signed_type_name(type_name))
        .then(|| type_name.to_string())
}

/// [`literal_or_const_value`]'s own `Expr::If`-over-`Expr::Let` half, factored out to keep
/// that function under clippy's line count: resolves `let_expr`'s own scrutinee, selects
/// `if_expr`'s `then` branch or `else_branch` by whether `let_expr`'s pattern matches it, and
/// — when it does — evaluates the `then` branch with every name that pattern binds mapped to
/// the scrutinee, the identical two-part answer [`evaluate_match`]'s own arm resolver
/// computes for a `match` arm's pattern and body.
fn evaluate_if_let(
    let_expr: &syn::ExprLet,
    if_expr: &syn::ExprIf,
    else_branch: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    let scrutinee = literal_or_const_value(&let_expr.expr, resolve)?;
    if !match_arm_matches_constant(&let_expr.pat, scrutinee, resolve)? {
        return literal_or_const_value(else_branch, resolve);
    }
    let bound = pattern_bindings(&let_expr.pat, resolve);
    let is_bound =
        |path: &syn::Path| -> bool { path.get_ident().is_some_and(|ident| bound.contains(&ident)) };
    let bound_unsigned_value = !bound.is_empty() && is_definitely_unsigned(&let_expr.expr, resolve);
    let bound_width_value = (!bound.is_empty())
        .then(|| expr_declared_width(&let_expr.expr, resolve))
        .flatten();
    let bound_value = |path: &syn::Path| -> Option<i128> {
        if is_bound(path) {
            return Some(scrutinee);
        }
        (resolve.value)(path)
    };
    let bound_unsigned = |path: &syn::Path| -> bool {
        if is_bound(path) {
            return bound_unsigned_value;
        }
        (resolve.unsigned)(path)
    };
    let bound_width = |path: &syn::Path| -> Option<&str> {
        if is_bound(path) {
            return bound_width_value.as_deref();
        }
        (resolve.width)(path)
    };
    let let_resolve = Resolve {
        value: &bound_value,
        unsigned: &bound_unsigned,
        width: &bound_width,
    };
    evaluate_block(&if_expr.then_branch, &let_resolve)
}

fn evaluate_match(expr_match: &syn::ExprMatch, resolve: &Resolve<'_>) -> Option<i128> {
    // Codex's next-round finding: `match (0u8, 1u8) { (0, 1) => 0, _ => 100 }` names a
    // scrutinee this scan's own `i128` domain has no room for — [`literal_or_const_value`]'s
    // own `Expr::Tuple` case only ever reduces a *one*-element tuple to its single inner
    // value, so a genuinely multi-element tuple scrutinee fell to the wildcard `_ => None`
    // case there and stayed unresolved, taking every constant built from it with it. Tried
    // first, and unconditionally, is the ordinary single-value path every other scrutinee
    // shape already uses; [`evaluate_tuple_match`] is what is tried next, and only when the
    // scrutinee is itself `Expr::Tuple` with more than one element — a tuple this scan can
    // still answer for component-wise, matched against a tuple pattern of the identical
    // arity, without ever needing to name the whole scrutinee as one `i128`.
    if let Some(scrutinee) = literal_or_const_value(&expr_match.expr, resolve) {
        for arm in expr_match
            .arms
            .iter()
            .filter(|arm| !has_cfg_test(&arm.attrs))
        {
            if !match_arm_matches_constant(&arm.pat, scrutinee, resolve)? {
                continue;
            }
            // Codex's finding: an arm that binds its own scrutinee (`x @ 0 => x`) has to be
            // evaluated — guard and body alike — through a resolver that answers for that
            // binding, not only through the outer one `resolve` already is. `unsigned` and
            // `width` are wrapped in fresh closures rather than copied straight from
            // `resolve` — the identical shape every other local `Resolve` built in this
            // module already uses (`block_resolve_width` and its neighbours, above) — because
            // a field copied unchanged carries its own already-fixed lifetime into the new
            // struct literal and forces every other field to match it exactly, where a fresh
            // closure lets inference pick the one lifetime this whole local value actually
            // needs: this block's own.
            //
            // Codex's next-round finding: `x @ u128::MAX => (x >> 127) as u8` resolves `x`'s
            // own *value* through the fix above, but the two closures directly below still
            // discarded its *type* — a bare pass-through of `resolve.unsigned`/`resolve.width`
            // has never heard of an arm-local name, so `is_definitely_unsigned(x, ..)` asked
            // the outer scope and answered `false` regardless of what `x` was actually bound
            // to, and a shift or a cast built from it stayed unresolved. The scrutinee
            // expression `x` was matched against is what its type really is, so `bound`'s own
            // unsignedness and width are computed from *that* expression once, through
            // `is_definitely_unsigned` and `expr_declared_width` exactly as any other operand
            // of that shape already would be, and handed back for the bound name alone.
            let bound = pattern_bindings(&arm.pat, resolve);
            let is_bound = |path: &syn::Path| -> bool {
                path.get_ident().is_some_and(|ident| bound.contains(&ident))
            };
            let bound_unsigned_value =
                !bound.is_empty() && is_definitely_unsigned(&expr_match.expr, resolve);
            let bound_width_value = (!bound.is_empty())
                .then(|| expr_declared_width(&expr_match.expr, resolve))
                .flatten();
            let bound_value = |path: &syn::Path| -> Option<i128> {
                if is_bound(path) {
                    return Some(scrutinee);
                }
                (resolve.value)(path)
            };
            let bound_unsigned = |path: &syn::Path| -> bool {
                if is_bound(path) {
                    return bound_unsigned_value;
                }
                (resolve.unsigned)(path)
            };
            let bound_width = |path: &syn::Path| -> Option<&str> {
                if is_bound(path) {
                    return bound_width_value.as_deref();
                }
                (resolve.width)(path)
            };
            let arm_resolve = Resolve {
                value: &bound_value,
                unsigned: &bound_unsigned,
                width: &bound_width,
            };
            if let Some((_, guard_expr)) = arm.guard.as_ref() {
                match literal_or_const_value(guard_expr, &arm_resolve) {
                    Some(0) => continue,
                    Some(_) => {}
                    None => return None,
                }
            }
            return literal_or_const_value(&arm.body, &arm_resolve);
        }
        return None;
    }
    let syn::Expr::Tuple(tuple) = strip_parens(&expr_match.expr) else {
        return None;
    };
    if tuple.elems.len() < 2 {
        return None;
    }
    evaluate_tuple_match(expr_match, tuple, resolve)
}

/// [`evaluate_match`]'s own tuple-scrutinee half: `scrutinee`'s own elements, each resolved
/// through the identical single-value pipeline, matched component-wise against the first
/// arm whose own pattern is a tuple of the identical arity (or an irrefutable catch-all —
/// `_` or a bare, unguarded binding, the same two shapes [`match_arm_matches_constant`]
/// already treats as matching anything). Every other half of `evaluate_match`'s own
/// contract holds unchanged: a `#[cfg(test)]` arm is skipped, a guard is resolved and
/// applied the identical way, and an arm whose own shape this function does not recognise
/// stops the whole search rather than being treated as "does not match".
fn evaluate_tuple_match(
    expr_match: &syn::ExprMatch,
    scrutinee: &syn::ExprTuple,
    resolve: &Resolve<'_>,
) -> Option<i128> {
    let values: Vec<i128> = scrutinee
        .elems
        .iter()
        .map(|elem| literal_or_const_value(elem, resolve))
        .collect::<Option<_>>()?;
    for arm in expr_match
        .arms
        .iter()
        .filter(|arm| !has_cfg_test(&arm.attrs))
    {
        if !tuple_pattern_matches_constant(&arm.pat, &values, resolve)? {
            continue;
        }
        // [`evaluate_match`]'s own fix, carried here: an arm that binds one of the
        // scrutinee's own elements (`(x, _) => x`) has to be evaluated through a resolver
        // that answers for it too — value, unsignedness and width alike, each derived from
        // the scrutinee *element* a binding was matched against, the identical way
        // `evaluate_match`'s own scalar fix derives them from the whole scrutinee.
        let bindings = tuple_pattern_bindings(&arm.pat, &values, &scrutinee.elems, resolve);
        let bound_value = |path: &syn::Path| -> Option<i128> {
            if let Some(ident) = path.get_ident() {
                if let Some((_, value, ..)) = bindings.iter().find(|(name, ..)| ident == *name) {
                    return Some(*value);
                }
            }
            (resolve.value)(path)
        };
        let bound_unsigned = |path: &syn::Path| -> bool {
            if let Some(ident) = path.get_ident() {
                if let Some((_, _, unsigned, _)) = bindings.iter().find(|(name, ..)| ident == *name)
                {
                    return *unsigned;
                }
            }
            (resolve.unsigned)(path)
        };
        let bound_width = |path: &syn::Path| -> Option<&str> {
            if let Some(ident) = path.get_ident() {
                if let Some((_, _, _, width)) = bindings.iter().find(|(name, ..)| ident == *name) {
                    return width.as_deref();
                }
            }
            (resolve.width)(path)
        };
        let arm_resolve = Resolve {
            value: &bound_value,
            unsigned: &bound_unsigned,
            width: &bound_width,
        };
        if let Some((_, guard_expr)) = arm.guard.as_ref() {
            match literal_or_const_value(guard_expr, &arm_resolve) {
                Some(0) => continue,
                Some(_) => {}
                None => return None,
            }
        }
        return literal_or_const_value(&arm.body, &arm_resolve);
    }
    None
}

/// The name-to-value bindings `pattern` introduces when it matches `values`, component-wise
/// — [`pattern_binding`]'s own tuple-arity twin, for the identical reason
/// [`tuple_pattern_matches_constant`] is [`match_arm_matches_constant`]'s: a tuple pattern
/// (`(x, _) => x`) can bind one of the scrutinee's own elements rather than the whole thing,
/// and [`evaluate_tuple_match`] needs every such binding to evaluate an arm's guard and body
/// correctly. Element positions that bind nothing are simply absent from the result, rather
/// than the whole call answering `None` — unlike the match functions beside it, a tuple that
/// binds nothing at all is not a failure to resolve, it is a pattern with no bindings in it.
///
/// Each binding carries its own unsignedness and width alongside its value, both derived
/// from `scrutinee_elems`' own matching element — [`evaluate_match`]'s scalar-binding fix
/// carried one level deeper, for the identical reason: a bound name's *type* is the
/// expression it was matched against, not a fact the outer scope has ever heard of it.
fn tuple_pattern_bindings<'a>(
    pattern: &'a syn::Pat,
    values: &[i128],
    scrutinee_elems: &'a syn::punctuated::Punctuated<syn::Expr, syn::Token![,]>,
    resolve: &Resolve<'_>,
) -> Vec<(&'a syn::Ident, i128, bool, Option<String>)> {
    match pattern {
        syn::Pat::Paren(paren) => {
            tuple_pattern_bindings(&paren.pat, values, scrutinee_elems, resolve)
        }
        syn::Pat::Tuple(tuple_pat)
            if tuple_pat.elems.len() == values.len()
                && tuple_pat.elems.len() == scrutinee_elems.len() =>
        {
            tuple_pat
                .elems
                .iter()
                .zip(values)
                .zip(scrutinee_elems)
                .flat_map(|((sub_pattern, value), elem_expr)| {
                    pattern_bindings(sub_pattern, resolve)
                        .into_iter()
                        .map(|name| {
                            (
                                name,
                                *value,
                                is_definitely_unsigned(elem_expr, resolve),
                                expr_declared_width(elem_expr, resolve),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Whether `pattern` matches the constant tuple `values`, component-wise — the tuple-arity
/// twin of [`match_arm_matches_constant`], reusing that function for each element rather
/// than reimplementing what a single element's own pattern can match. `None` for anything
/// this cannot answer, for the identical reason [`match_arm_matches_constant`]'s own doc
/// comment states: a caller that treated an unrecognised shape as "does not match" could
/// silently try the wrong later arm.
fn tuple_pattern_matches_constant(
    pattern: &syn::Pat,
    values: &[i128],
    resolve: &Resolve<'_>,
) -> Option<bool> {
    match pattern {
        syn::Pat::Wild(_) => Some(true),
        syn::Pat::Ident(named) if named.subpat.is_none() && named.by_ref.is_none() => Some(true),
        syn::Pat::Paren(paren) => tuple_pattern_matches_constant(&paren.pat, values, resolve),
        syn::Pat::Tuple(tuple_pat) if tuple_pat.elems.len() == values.len() => {
            for (sub_pattern, value) in tuple_pat.elems.iter().zip(values) {
                if !match_arm_matches_constant(sub_pattern, *value, resolve)? {
                    return Some(false);
                }
            }
            Some(true)
        }
        _ => None,
    }
}

/// Every identifier `pattern` binds when it matches a value, for the same narrow set of
/// shapes [`match_arm_matches_constant`] recognises — empty when the pattern introduces no
/// binding at all (a wildcard, a literal, a range) or when the shape is not one this scan
/// supports. Every name returned binds to the identical value `pattern` as a whole matched,
/// unlike [`destructured_binding`]'s own per-name expressions for a `let`'s tuple pattern,
/// because none of the shapes here destructures a value into parts.
///
/// Codex's finding: `match 0u8 { x @ 0 => x, _ => 100 }` — a constant initializer that
/// returns the value its own selected arm bound — confirmed the arm matched through
/// [`match_arm_matches_constant`] and then evaluated `x` with only the *outer* resolver,
/// where the arm-local binding does not exist, so `x` stayed unresolved and the whole
/// match fell to `None`. [`evaluate_match`] and [`evaluate_tuple_match`] are what call
/// this to find out which name, if any, an arm's own pattern bound, so they can hand the
/// arm's guard and body a resolver that answers for it too. An at-binding (`x @ subpat`)
/// always binds `x`, regardless of what `subpat` itself matches; a bare identifier binds
/// only when it does not already resolve as a known constant — the identical distinction
/// [`match_arm_matches_constant`]'s own bare-`Pat::Ident` arm draws between a value match
/// and an irrefutable catch-all — since a constant's own name is not a fresh binding. An
/// or-pattern's alternatives are required by `rustc` to bind the same names, so the first
/// alternative that names any speaks for all of them.
///
/// Codex's next-round finding: `match 0u8 { _outer @ inner => inner }` binds *two* names —
/// `_outer`, from the at-pattern itself, and `inner`, from the sub-pattern an at-pattern's
/// own bare-identifier arm returned unconditionally without ever looking inside — and this
/// function used to answer only the first of them, `Option`-shaped rather than list-shaped,
/// the same gap [`destructured_binding`] closed for a `let`'s own pattern before this did
/// for a match arm's. `_outer @ inner` now recurses into the sub-pattern exactly as
/// `destructured_binding`'s own `Pat::Ident` case already does, and every caller reads a
/// list rather than a single name.
fn pattern_bindings<'a>(pattern: &'a syn::Pat, resolve: &Resolve<'_>) -> Vec<&'a syn::Ident> {
    match pattern {
        syn::Pat::Ident(named) if named.by_ref.is_none() => {
            if let Some((_, subpat)) = &named.subpat {
                let mut bound = vec![&named.ident];
                bound.extend(pattern_bindings(subpat, resolve));
                bound
            } else if (resolve.value)(&syn::Path::from(named.ident.clone())).is_none() {
                vec![&named.ident]
            } else {
                Vec::new()
            }
        }
        syn::Pat::Paren(paren) => pattern_bindings(&paren.pat, resolve),
        syn::Pat::Tuple(tuple) if tuple.elems.len() == 1 => tuple
            .elems
            .first()
            .map_or_else(Vec::new, |inner| pattern_bindings(inner, resolve)),
        syn::Pat::Or(or_pattern) => or_pattern
            .cases
            .iter()
            .find_map(|case| {
                let names = pattern_bindings(case, resolve);
                (!names.is_empty()).then_some(names)
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Whether `pattern` matches the constant `value`, for the narrow set of shapes
/// [`evaluate_match`] needs: a literal (through [`lit_value`], `true`/`false` included), an
/// unguarded bare binding (a known constant's own name if `resolve` answers for it,
/// otherwise an irrefutable catch-all exactly as [`is_catchall_pattern`] already treats
/// one), an or-pattern combining either, or a set of parentheses around any of them.
/// `None` for anything else — a range, a tuple, a struct pattern — because this function's
/// only caller stops the whole search on `None` rather than treating an unrecognised
/// pattern as "does not match" and silently trying the next arm, which could answer from
/// the wrong one.
fn match_arm_matches_constant(
    pattern: &syn::Pat,
    value: i128,
    resolve: &Resolve<'_>,
) -> Option<bool> {
    match pattern {
        syn::Pat::Wild(_) => Some(true),
        syn::Pat::Lit(literal) => Some(lit_value(&literal.lit)? == value),
        syn::Pat::Ident(named) if named.subpat.is_none() && named.by_ref.is_none() => Some(
            (resolve.value)(&syn::Path::from(named.ident.clone()))
                .is_none_or(|resolved| resolved == value),
        ),
        // Codex's next-round finding: `_x @ 0 => 0, _ => 100` — an at-binding over a match
        // this function itself is asked to evaluate — fell through the guard above (which
        // requires `subpat.is_none()`) straight to the wildcard `_ => None` case below and
        // stopped the whole search, even though [`pattern_literal`]'s own at-binding case
        // already answers the identical question for a numbered table's own outer
        // patterns: the binding name is incidental to the value the arm matches, and this
        // recurses into the subpattern the same way.
        syn::Pat::Ident(named) if named.by_ref.is_none() => {
            let (_, subpat) = named.subpat.as_ref()?;
            match_arm_matches_constant(subpat, value, resolve)
        }
        syn::Pat::Paren(paren) => match_arm_matches_constant(&paren.pat, value, resolve),
        // Codex's next-round finding: `x @ u128::MAX => ..` — a *qualified* constant path
        // as the subpattern of an at-binding, over a match this function itself is asked to
        // evaluate — parses as `Pat::Path` rather than the bare-identifier `Pat::Ident` the
        // arm above already handles, and fell to the wildcard `_ => None` case below,
        // stopping the whole search: `evaluate_match`'s own `?` on this function's answer
        // means a single unresolved arm bails the entire match rather than only that arm,
        // so `x`'s own value binding never had a chance to matter. `resolve.value` is what
        // already answers the identical question for a bare path in `Expr::Path` position —
        // `u128::MAX` included, through the well-known-bound fallback every qualified value
        // lookup already falls back to — so this asks it the same way rather than
        // reimplementing that resolution.
        syn::Pat::Path(path) if path.qself.is_none() => Some((resolve.value)(&path.path)? == value),
        // [`literal_or_const_value`]'s own `Expr::Tuple` case holds the rationale: a
        // one-element tuple pattern names exactly its own element's value, the pattern-side
        // twin of that scrutinee-side fold — `(0,) => 0` over a `(0u8,)` scrutinee is
        // otherwise indistinguishable from `0 => 0` over `0u8` to this scan's own `i128`
        // domain. A tuple of any other arity is not a shape that domain can represent.
        syn::Pat::Tuple(tuple) if tuple.elems.len() == 1 => {
            match_arm_matches_constant(tuple.elems.first()?, value, resolve)
        }
        syn::Pat::Or(or_pattern) => {
            let mut matched = false;
            for case in &or_pattern.cases {
                if match_arm_matches_constant(case, value, resolve)? {
                    matched = true;
                }
            }
            Some(matched)
        }
        // Codex's next-round finding: `match 0u8 { 0..=14 => 0, _ => 100 }` — a range
        // pattern over a match this function itself is asked to evaluate — fell to the
        // wildcard `_ => None` case below and stopped the whole search, so a numbered
        // constant initialized this way stayed unresolved regardless of whether the
        // *outer* dense-match check would ever have recognised it. [`pattern_literal`]'s
        // own `Pat::Range` case already answers the wider question of *every* value a
        // range names, bounded so it cannot be asked to enumerate an unreasonably wide
        // one; this only needs to know whether `value` is one of them, which is the
        // identical forward-distance-from-`start` arithmetic that answers "is this inside"
        // without enumerating anything at all — sound over the same range width
        // `pattern_literal`'s own bound exists to guard, and over any wider one too, since
        // a containment check allocates nothing regardless of how far `start` sits from
        // `end`.
        syn::Pat::Range(range) => {
            let start = range
                .start
                .as_deref()
                .and_then(|expr| literal_or_const_value(expr, resolve))?;
            let end = range
                .end
                .as_deref()
                .and_then(|expr| literal_or_const_value(expr, resolve))?;
            #[allow(
                clippy::cast_sign_loss,
                reason = "wrapping_sub's own bit pattern, reinterpreted as the unsigned \
                          forward distance from start, not a value conversion — the same \
                          reasoning pattern_literal's own Pat::Range case already uses"
            )]
            let span = end.wrapping_sub(start) as u128;
            #[allow(
                clippy::cast_sign_loss,
                reason = "wrapping_sub's own bit pattern, reinterpreted as the unsigned \
                          forward distance from start, not a value conversion"
            )]
            let offset = value.wrapping_sub(start) as u128;
            Some(if matches!(range.limits, syn::RangeLimits::Closed(_)) {
                offset <= span
            } else {
                offset < span
            })
        }
        _ => None,
    }
}

/// `pattern`'s own integer value, the pattern half of [`literal_or_const_value`]: a bare
/// literal, a path — a single plain identifier parses as a binding rather than a path,
/// since `syn` cannot tell one from a constant of the same name without resolving it, so it
/// is turned into a one-segment [`syn::Path`] before asking `resolve` the same question a
/// multi-segment, qualified pattern (`module::P0`) would be asked — or an inclusive range
/// whose two ends resolve to the same value.
///
/// Codex's finding on the last: `0..=0` names exactly the value `0` and nothing else, so a
/// table that spelled its patterns that way — indistinguishable from `0` at the value level,
/// and identical to it in every value `rustc`'s exhaustiveness check accepts — folds into
/// the same lookup table a bare-literal version would, and was read as a range (which is
/// genuinely not one of the shapes a dense table's *patterns* take, hence not accepted for
/// any width wider than one value) rather than as the single integer it singles out. A
/// half-open range (`0..1`) is a singleton by exactly the same reasoning — it names one
/// fewer value than its own closed twin `0..=1` — and a later round widened both closed
/// and half-open ranges to their full span rather than singletons alone, so this is now
/// the single-value case of that wider handling rather than a case of its own.
///
/// An at-binding (`_p0 @ 0`) resolves to whatever its own subpattern does, recursively —
/// Codex's finding: the binding name is incidental to the value the arm matches, and MSRV
/// Rust accepts the form with no warning, so a table spelled that way is exactly as dense
/// as one without the bindings and was read as unresolved on every arm instead.
///
/// `None` for a wildcard, a wider range, a tuple, or anything else a dense table's patterns
/// are not, and for a binding that names no known constant — that is
/// [`is_catchall_pattern`]'s question, not this one's.
/// `<Type as Trait>::NAME`'s own value, when `Type` is a plain single-segment type this
/// scan has indexed a trait-associated (or inherent) constant under — the qualified-self
/// half of [`pattern_literal`]'s `Pat::Path` case, for a pattern `syn` gives a `qself`.
///
/// Codex's finding: a `syn::PatPath`'s own `qself` was never read at all, so a
/// `<u8 as Indices>::P0`-style pattern's `path` field (`Indices::P0` — the *trait's* own
/// path, plus the member) was resolved as though it were an ordinary module-qualified
/// reference, looking for a module literally named `Indices`. `visit_item_impl` indexes
/// a trait impl's own associated constants the same way it already does an inherent
/// impl's — under the *implementing type's* own name (`u8::P0`), which is also the
/// unqualified spelling Rust itself accepts when the trait is unambiguous — so this
/// builds that same key from the qself's own type and the path's last segment, and asks
/// `resolve` the ordinary multi-segment question of it.
///
/// Codex's next-round finding: that key is built with no module prefix at all, which
/// only ever matched an impl declared at the scanned tree's own top level. A trait
/// referenced by its own qualified path — `<u8 as defs::Indices>::P0`, naming the module
/// `defs` the trait (and, in practice, the impl of it) is declared in — carries exactly
/// the prefix this was missing, in `path`'s own leading segments (everything before the
/// trait's own bare name and the member). Tried first, since it is the more specific of
/// the two candidates; the bare, unprefixed form stays the fallback for the common case
/// where the trait is referenced bare (`<u8 as Indices>::P0`) and both scan the same
/// tree's own top level.
///
/// Codex's next-round finding after that: the trait's own declared module is not
/// necessarily the *impl's* module at all — `impl traits::Indices for u8 { .. }` can sit
/// in a sibling `mod implementations` of the module the trait itself is declared in, and
/// [`MatchVisitor::visit_item_impl`] indexes a trait impl's constants under the *impl's*
/// own lexical module, never the trait's. Neither candidate above names that module (this
/// function only ever sees the *pattern's* own path, never where the impl actually sits),
/// so both failed and the arm stayed unresolved. A fallback searches every key `qualified`
/// holds for the one ending in exactly `type_name::member` — the suffix every
/// [`MatchVisitor::visit_item_impl`] insertion carries regardless of which module recorded
/// it — and answers only when that search is unambiguous, the same standing every other
/// unsupported shape in this scan already has: a second impl of the same type existing
/// somewhere else in the tree leaves this unresolved rather than guessing between them.
///
/// Codex's next-round finding after *that*: "unambiguous" broke the moment two different
/// traits are each implemented for the same type in two different modules — `traits::Dense`
/// and `traits::Noise`, say, both implemented for `u8` — because both impls' constants end
/// in the identical `u8::P0` suffix, and the bare-type search above cannot tell them apart
/// even though the pattern's own trait path (`<u8 as traits::Dense>::P0`) names the intended
/// impl exactly. [`MatchVisitor::visit_item_impl`] now indexes a trait impl's constants a
/// second way too, with the trait's own bare name ahead of the type
/// (`impl_module::Dense::u8::P0`), and the search below tries that shape — keyed on the
/// pattern's own trait segment, `path.segments[segment_count - 2]`, which is always the
/// trait's bare name whether the pattern referenced it bare or through a longer qualified
/// path — before falling back to the type-only suffix, which stays for an impl this second
/// key cannot reach (an inherent impl, where there is no trait to disambiguate with).
///
/// Codex's next-round finding after *that*: a bare last segment is *still* ambiguous the
/// moment two different traits sharing that one bare name — `traits_a::Indices` and
/// `traits_b::Indices`, each implemented for `u8` in its own sibling module — exist
/// anywhere in the scanned tree, because `visit_item_impl`'s second key now carries the
/// trait's full *resolved* scope path (`traits_a::Indices`, `traits_b::Indices`), but this
/// search still built its own query suffix from the single segment right before the
/// member, discarding the rest of what the pattern itself wrote. The suffix is now built
/// from every segment of the pattern's own trait path (`path.segments[..segment_count -
/// 1]`, everything before the member) rather than the one immediately preceding it, so
/// `<u8 as traits_a::Indices>::P0` searches for `::traits_a::Indices::u8::P0` and no longer
/// shares a candidate with `traits_b::Indices`'s own key. A trait referenced bare
/// (`<u8 as Indices>::P0`) still searches the identical single-segment suffix it always
/// has, since there is nothing more written to widen it with.
fn resolve_qself_associated_const(
    qself: &syn::QSelf,
    path: &syn::Path,
    resolve: &Resolve<'_>,
    qualified: &std::collections::HashMap<String, i128>,
) -> Option<i128> {
    let type_name = type_path_name(&qself.ty)?;
    let member = ident_name(&path.segments.last()?.ident);
    let segment_count = path.segments.len();
    if segment_count >= 3 {
        let trait_module: Vec<String> = path
            .segments
            .iter()
            .take(segment_count - 2)
            .map(|segment| ident_name(&segment.ident))
            .collect();
        let mut scoped = trait_module;
        scoped.push(type_name.clone());
        scoped.push(member.clone());
        if let Ok(synthetic) = syn::parse_str::<syn::Path>(&scoped.join("::")) {
            if let Some(value) = (resolve.value)(&synthetic) {
                return Some(value);
            }
        }
    }
    let synthetic = syn::parse_str::<syn::Path>(&format!("{type_name}::{member}")).ok()?;
    if let Some(value) = (resolve.value)(&synthetic) {
        return Some(value);
    }
    if let Some(trait_path_len) = segment_count.checked_sub(1) {
        if trait_path_len > 0 {
            let trait_path_segments: Vec<String> = path
                .segments
                .iter()
                .take(trait_path_len)
                .map(|segment| ident_name(&segment.ident))
                .collect();
            let trait_suffix = format!(
                "::{}::{type_name}::{member}",
                trait_path_segments.join("::")
            );
            let mut trait_candidates = qualified.keys().filter(|key| key.ends_with(&trait_suffix));
            if let Some(unique) = trait_candidates.next() {
                if trait_candidates.next().is_none() {
                    return qualified.get(unique).copied();
                }
            }
        }
    }
    let suffix = format!("::{type_name}::{member}");
    let mut candidates = qualified.keys().filter(|key| key.ends_with(&suffix));
    let unique = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    qualified.get(unique).copied()
}

/// The most values [`pattern_literal`]'s `Pat::Range` case will ever expand a single
/// range pattern into, checked before any value is generated rather than after.
///
/// Chosen with headroom rather than tightly: [ADR 0010] measures the widest checksum
/// table ever considered here, a 256-entry byte-indexed CRC table, at 1024 B of `rodata`
/// against an 8 KiB code-flash budget — sixteen times that is still a few dozen
/// kilobytes of `i128`s, not the gigabytes a syntactically ordinary but astronomically
/// wide range (`0..=1_000_000_000u64`, which fits comfortably in a `usize` on a 64-bit
/// host) would otherwise force this scan to allocate before the match around it has even
/// been checked for its arm count or its density.
///
/// [ADR 0010]: https://github.com/madmax983/waymaker/blob/main/docs/adr/0010-the-integrity-check-is-catalogued-and-table-free.md
const MAX_RANGE_PATTERN_VALUES: usize = 4096;

/// The single field of `elems` that is not itself irrefutable — `_`, or an unguarded
/// binding naming no known constant, via the exact same test [`is_catchall_pattern`]
/// applies to a whole arm's own pattern — or `None` when zero or more than one field
/// qualifies.
///
/// A tuple, tuple-struct, named-field struct, or slice pattern with any number of
/// fields, all but one of them a catch-all, is exactly as dense a table row as that one
/// field alone: `rustc` still indexes on the one field that actually varies and ignores
/// every field that always matches — a field's own *name*, where it has one, plays no
/// part in this, only whether its subpattern is irrefutable. Two or more non-catch-all
/// fields is genuinely ambiguous — nothing here says which one a table would be keyed on
/// — and is refused the same as zero, rather than guessing.
fn single_discriminating_field<'a>(
    elems: impl IntoIterator<Item = &'a syn::Pat>,
    resolve: &Resolve<'_>,
) -> Option<&'a syn::Pat> {
    let mut found: Option<&syn::Pat> = None;
    for elem in elems {
        if is_catchall_pattern(elem, false, resolve) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(elem);
    }
    found
}

fn pattern_literal(
    pattern: &syn::Pat,
    resolve: &Resolve<'_>,
    qualified: &std::collections::HashMap<String, i128>,
) -> Vec<i128> {
    match pattern {
        syn::Pat::Lit(literal) => lit_value(&literal.lit).into_iter().collect(),
        syn::Pat::Ident(named) => match &named.subpat {
            // Codex's finding: an at-binding (`_p0 @ 0`) is exactly as singleton a pattern
            // as its own subpattern is, and MSRV-legal, unwarned Rust — the binding name
            // is incidental to the value the arm matches, so the subpattern is resolved
            // the same way any other pattern here is, recursively.
            //
            // Codex's next-round finding: the recursion was gated on `named.by_ref.is_none()`
            // for the whole arm, so an at-binding with an explicit binding mode
            // (`ref _p0 @ 0`, MSRV-legal and unwarned) fell through to `_ => Vec::new()`
            // instead — but `ref`, `mut` and `ref mut` change only how the match binds the
            // value, never which value the pattern matches, so the subpattern is resolved
            // the same way regardless of the binding's own mode.
            Some((_, subpat)) => pattern_literal(subpat, resolve, qualified),
            // A bare identifier with no subpattern is the one shape where the mode still
            // matters: `ref`/`mut` are legal only on a genuine new binding, never on
            // Rust's own path-pattern spelling of a constant, so a bare `ref name` can
            // never be the constant `resolve` would otherwise answer for — refused here
            // rather than asked of `resolve`, which could otherwise answer from an
            // unrelated same-named constant it has no business naming.
            None if named.by_ref.is_none() => {
                (resolve.value)(&syn::Path::from(named.ident.clone()))
                    .into_iter()
                    .collect()
            }
            None => Vec::new(),
        },
        // Codex's finding: `<u8 as Indices>::P0` is a trait-associated constant, and
        // this arm used to resolve `path.path` alone — `Indices::P0`, the *trait's* own
        // path plus the member — as though it were an ordinary module-qualified
        // reference, ignoring `path.qself` (the `<u8 as ..>` half) entirely. A module
        // named `Indices` is never what this scan indexes, so every such arm read as
        // unresolved. [`resolve_qself_associated_const`] is the qualified-self half.
        syn::Pat::Path(path) => path.qself.as_ref().map_or_else(
            || (resolve.value)(&path.path).into_iter().collect(),
            |qself| {
                resolve_qself_associated_const(qself, &path.path, resolve, qualified)
                    .into_iter()
                    .collect()
            },
        ),
        // Codex's finding: `0..=1` covers two values, not one, and `rustc` still lowers
        // a match spelling a range that way to the identical indexed table a
        // one-value-per-arm spelling gets — this used to accept only a *singleton*
        // closed range (`start == end`) and return nothing for any wider one, on the
        // reasoning that a half-open range's own singleton-ness needs the pattern's real
        // type's successor function to check. Widening a *closed* range needs no such
        // thing: `start..=end` is exactly the consecutive integers from `start` to `end`
        // in this scan's own `i128` representation, regardless of what the pattern's
        // real type is, so every value in that span is generated directly.
        //
        // Codex's next-round finding: the same reasoning applies just as directly to a
        // *half-open* range, and the earlier caution about needing the pattern's real
        // type's own successor function does not actually hold either of them — the
        // successor structure this scan already relies on for a closed range's own
        // interior values (`start`, `start + 1`, ..., `end`) is exactly the one integer
        // ranges have, and `0..2` names precisely the two values `0..=1` does, with no
        // further knowledge of the pattern's real type needed than the closed case
        // already assumes. A half-open range therefore widens to the closed range one
        // step short of its own exclusive end (`checked_sub(1)`, so an empty range like
        // `0..0` correctly resolves to no values rather than a negative-width one), and
        // is folded into the identical closed-range handling below rather than kept as a
        // second, narrower case.
        syn::Pat::Range(range) => {
            let Some(start) = range
                .start
                .as_deref()
                .and_then(|expr| literal_or_const_value(expr, resolve))
            else {
                return Vec::new();
            };
            let Some(end) = range
                .end
                .as_deref()
                .and_then(|expr| literal_or_const_value(expr, resolve))
            else {
                return Vec::new();
            };
            // Bounded before it is generated: a span too wide to fit a `usize`, or one
            // whose width cannot even be computed, is not a dense table any real
            // integer pattern could name, and is refused rather than attempted.
            //
            // Codex's finding: fitting in a `usize` is not the same as being small — on a
            // 64-bit host, a syntactically ordinary range such as `0..=1_000_000_000u64`
            // passes that check and then eagerly collects roughly a billion `i128`
            // values, before the match around it has even been checked for its arm count
            // or its density, which is an out-of-memory failure one added line could
            // trigger. `MAX_RANGE_PATTERN_VALUES` bounds the span *before* any value is
            // generated, wide enough for any table this workspace's own budgets could
            // ever justify — ADR 0010 measures the widest one ever considered, a 256-entry
            // byte-indexed CRC table, at 1024 B against an 8 KiB code-flash budget — and
            // narrow enough that even the worst case allocates kilobytes, not gigabytes.
            //
            // Codex's forty-third-round finding: a range whose `start` and `end` straddle
            // the point [`lit_value`]'s own two's-complement reinterpretation of a `u128`
            // literal at or above `2^127` wraps through — `2^127-2..=2^127`, say — has
            // `start` sitting near `i128::MAX` and `end` near `i128::MIN` after that
            // reinterpretation, so `end.checked_sub(start)` (by way of `inclusive_end`)
            // always overflowed and this whole arm answered `None`, even though the range
            // is still exactly as dense a window as any other. The span is now the
            // *forward* distance from `start` to `end`, computed as `end.wrapping_sub
            // (start)`'s own bit pattern read as `u128` — [`window_layout`]'s own
            // reasoning, that this bit pattern is the circular distance mod `2^128`
            // regardless of which half of `i128` either endpoint sits in — plus one for a
            // closed range, and the values are generated the same way: `wrapping_add(1)`
            // from `start`, which is the identical successor a plain increment gives for
            // every range that does not straddle the boundary and the correct one for a
            // range that does.
            #[allow(
                clippy::cast_sign_loss,
                reason = "wrapping_sub's own bit pattern, reinterpreted as the unsigned \
                          forward distance from start to end, not a value conversion"
            )]
            let mut count = end.wrapping_sub(start) as u128;
            if matches!(range.limits, syn::RangeLimits::Closed(_)) {
                let Some(bumped) = count.checked_add(1) else {
                    return Vec::new();
                };
                count = bumped;
            }
            let Ok(max_values) = u128::try_from(MAX_RANGE_PATTERN_VALUES) else {
                return Vec::new();
            };
            if count == 0 || count > max_values {
                return Vec::new();
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "count was just bounded above by MAX_RANGE_PATTERN_VALUES, itself \
                          a usize, so it fits"
            )]
            let count = count as usize;
            let mut values = Vec::with_capacity(count);
            let mut current = start;
            for _ in 0..count {
                values.push(current);
                current = current.wrapping_add(1);
            }
            values
        }
        // Codex's finding: a reference pattern (`&0`) is exactly as singleton a value as
        // its own referent, over a scrutinee that is itself a reference — a shape a dense
        // table's own selector can be, and one `rustc` lowers to the identical indexed
        // table a by-value match would.
        syn::Pat::Reference(reference) => pattern_literal(&reference.pat, resolve, qualified),
        // Codex's forty-fourth-round finding: `(0)` through `(14)` — a pattern wrapped in
        // parentheses, legal and warning-free even with nothing to disambiguate — is
        // `Pat::Paren`, which fell to the wildcard `_ => Vec::new()` case and left every
        // such arm unresolved, even though a parenthesized pattern names exactly the value
        // its own interior does and `rustc` lowers a match built from it to the identical
        // indexed table an unparenthesized one gets.
        syn::Pat::Paren(paren) => pattern_literal(&paren.pat, resolve, qualified),
        // Codex's finding: `Some(0)` through `Some(14)` over an `Option<u8>` scrutinee is
        // exactly as dense as its bare-integer twin — `rustc` lowers a single-field
        // tuple-struct constructor pattern to the identical indexed table a plain integer
        // pattern gets — but every such arm is a `Pat::TupleStruct`, which fell to the
        // wildcard `_ => Vec::new()` case below regardless of which constructor it named.
        //
        // Codex's next-round finding: scoping this to exactly one field left a second
        // shape open — `(0, _)` through `(14, _)` over a `(u8, bool)` scrutinee is
        // exactly as dense again, since `rustc` still indexes on the one field that
        // varies and ignores the one that is always a catch-all, but a tuple struct with
        // *two* fields failed the `elems.len() == 1` guard outright. Generalised to
        // [`single_discriminating_field`]: any number of fields, exactly one of which is
        // not itself irrefutable, is exactly as dense as that one field alone — two or
        // more non-catch-all fields is genuinely ambiguous (which one indexes the table?)
        // and stays unresolved, the same standing a multi-field pattern already had.
        syn::Pat::TupleStruct(tuple_struct) => {
            single_discriminating_field(&tuple_struct.elems, resolve)
                .map_or_else(Vec::new, |elem| pattern_literal(elem, resolve, qualified))
        }
        // Codex's next-round finding: a plain one-tuple pattern (`(0,)` through `(14,)`)
        // is `Pat::Tuple` rather than `Pat::TupleStruct` — no constructor name, just a
        // single parenthesized, comma-terminated field — and `rustc` lowers a match built
        // from it to the identical indexed table the tuple-struct form gets. The identical
        // discriminating-field scoping applies for the identical reason, generalised the
        // same way the tuple-struct case above was.
        syn::Pat::Tuple(tuple) => single_discriminating_field(&tuple.elems, resolve)
            .map_or_else(Vec::new, |elem| pattern_literal(elem, resolve, qualified)),
        // Codex's next-round finding: `Key { n: 0, ignored: _ }` through
        // `Key { n: 14, ignored: _ }` is exactly as dense once more, since `rustc`
        // indexes on the one named field that varies and ignores a field that is
        // always a catch-all — a named-field struct pattern is the third shape this
        // reasoning applies to, so it gets the identical treatment: the field name
        // itself plays no part (a dense table could be built keyed on any one field,
        // whichever one actually varies), only which single field's own subpattern is
        // not irrefutable.
        syn::Pat::Struct(pat_struct) => single_discriminating_field(
            pat_struct.fields.iter().map(|field| field.pat.as_ref()),
            resolve,
        )
        .map_or_else(Vec::new, |elem| pattern_literal(elem, resolve, qualified)),
        // Codex's next-round finding: `[0]` through `[14]` over a `[u8; 1]` scrutinee,
        // or `[0, _]` through `[14, _]` over a wider slice, is exactly as dense once
        // more — a slice pattern is a fourth shape the discriminating-field reasoning
        // applies to, over its own explicitly-listed elements the same way a tuple's
        // are.
        //
        // Codex's next-round finding: `[0, ..]` through `[14, ..]` still evaded it — `..`
        // is `Pat::Rest`, which `is_catchall_pattern` did not recognise, so it counted as
        // a *second* discriminating field beside the literal and made every such arm
        // read as ambiguous. `Pat::Rest` matches every remaining element and names no
        // value of its own, exactly as irrefutable as a wildcard; `is_catchall_pattern`
        // now treats it as one.
        syn::Pat::Slice(pat_slice) => single_discriminating_field(&pat_slice.elems, resolve)
            .map_or_else(Vec::new, |elem| pattern_literal(elem, resolve, qualified)),
        // Codex's finding: `0 | 1 => VALUE` covers two values in a single arm, and
        // `rustc` still lowers a match built this way to the identical indexed table a
        // one-value-per-arm spelling gets — this fell to the `_ => Vec::new()` case
        // below and read as unresolved on every arm that used it. Every alternative is
        // resolved and flattened into one list; if any alternative does not resolve, the
        // whole arm is treated as unresolved rather than silently counting only the
        // alternatives that did — a partial count could make a real table look sparser
        // than it is, in either direction.
        syn::Pat::Or(or_pattern) => {
            let mut values = Vec::with_capacity(or_pattern.cases.len());
            for case in &or_pattern.cases {
                let case_values = pattern_literal(case, resolve, qualified);
                if case_values.is_empty() {
                    return Vec::new();
                }
                values.extend(case_values);
            }
            values
        }
        _ => Vec::new(),
    }
}

/// Whether `pattern` is irrefutable the way a dense table's final arm needs to be — `_`, or
/// a plain, unguarded binding that names no constant [`pattern_literal`] could have resolved
/// it to instead.
///
/// Codex's finding: `_ => VALUE15` and `other => VALUE15` are equally irrefutable and
/// compile to the identical lookup table, since an ordinary binding matches everything a
/// wildcard does — `has_dense_arm_patterns` asking specifically for `_` on the last arm was
/// a spelling requirement `rustc`'s own exhaustiveness check does not share. `guarded` is the
/// caller's to pass, since a guard (`other if cond => ..`) makes even a binding pattern
/// refutable and this function sees only the pattern, not the arm it belongs to.
///
/// Codex's next-round finding: `ref other => VALUE15` is exactly as irrefutable as `other`
/// itself — `ref` changes only how the match binds the value, never whether the pattern
/// matches — but the whole arm was gated on `named.by_ref.is_none()`, so a catch-all
/// spelled this way was not recognised as one, and a dense-looking table ending in it
/// failed both density checks (no `_`, and `missing_value`'s own numbered-arms-plus-one
/// window one arm short) rather than being reported. `mut other` never needed the
/// equivalent fix: `named.mutability` was never part of this guard to begin with.
#[must_use]
fn is_catchall_pattern(pattern: &syn::Pat, guarded: bool, resolve: &Resolve<'_>) -> bool {
    if guarded {
        return false;
    }
    match pattern {
        // Codex's forty-second-round finding: `[0, ..]` through `[14, ..]` over a
        // `[u8; 2]` scrutinee left `..` — `Pat::Rest`, matching every remaining element
        // and never itself a discriminating value — falling to the `_ => false` arm, so
        // `single_discriminating_field` saw *two* fields it could not rule out (the
        // literal and the rest) and answered `None` for every arm. `Pat::Rest` carries no
        // value of its own to discriminate on, exactly like a wildcard, which is why the
        // two share one arm here.
        syn::Pat::Wild(_) | syn::Pat::Rest(_) => true,
        syn::Pat::Ident(named) if named.subpat.is_none() => {
            (resolve.value)(&syn::Path::from(named.ident.clone())).is_none()
        }
        _ => false,
    }
}

/// The callee name and resolved argument value when `expr` is a call with exactly one
/// argument — seen through a set of parentheses or a brace group holding one tail
/// expression, since a block-valued arm (`0 => { helper(0) }`) is exactly as much a call as
/// an unwrapped one once a real parser is reading it.
fn call_shape_of(expr: &syn::Expr, resolve: &Resolve<'_>) -> Option<(String, Option<i128>)> {
    match expr {
        syn::Expr::Paren(paren) => call_shape_of(&paren.expr, resolve),
        syn::Expr::Group(group) => call_shape_of(&group.expr, resolve),
        syn::Expr::Block(block) if block.block.stmts.len() == 1 => {
            match block.block.stmts.first()? {
                syn::Stmt::Expr(inner, None) => call_shape_of(inner, resolve),
                _ => None,
            }
        }
        syn::Expr::Call(call) => {
            let syn::Expr::Path(callee) = call.func.as_ref() else {
                return None;
            };
            let callee = callee.path.get_ident()?;
            if call.args.len() != 1 {
                return None;
            }
            let argument = call.args.first()?;
            Some((
                ident_name(callee),
                literal_or_const_value(argument, resolve),
            ))
        }
        _ => None,
    }
}

/// `block`, wrapped as the plain, unlabelled `Expr::Block` [`call_shape_of`] already knows
/// how to see through — the shape an `if`/`else if` chain's own arm bodies are, which
/// [`extract_if_chain`] needs to ask [`call_shape_of`] the same question a real match arm's
/// body already gets asked.
fn block_as_expr(block: &syn::Block) -> syn::Expr {
    syn::Expr::Block(syn::ExprBlock {
        attrs: Vec::new(),
        label: None,
        block: block.clone(),
    })
}

/// `cond`'s own scrutinee and the value it is compared against, when `cond` is (seen
/// through any nesting of parentheses or brace groups) `left == right` and exactly one
/// side resolves to a constant — the shape ADR 0044's `crc32_nibble`/`crc16_nibble` never
/// need but a hand-written `if`/`else if` chain over a checksum table plausibly does.
///
/// The scrutinee is returned as its own token text rather than a `syn::Expr`, because
/// [`extract_if_chain`]'s whole job is confirming every link of a chain compares the
/// *identical* scrutinee, and comparing token text is the same structural-identity check
/// [`FoundMatch::selector`] already uses for a real match's own scrutinee — `syn::Expr` has
/// no `PartialEq` this workspace's own `syn` build enables.
///
/// Codex's forty-seventh-round finding: a hand-written `if x == 0 { .. } else if x == 1 {
/// .. } else { .. }` chain compiles to the identical indexed table a `match` over the same
/// arms would, but nothing here had ever looked at an `Expr::If` as anything but a
/// constant-initializer's own conditional. Which side is "the scrutinee" is not fixed by
/// position — `0 == x` is exactly as meaningful as `x == 0` — so whichever side fails to
/// resolve as a constant is taken to be it; if both sides resolve, or neither does, this
/// chain is not a shape this scan can tell apart from an ordinary comparison, and it stays
/// unrecognised rather than guessed at.
///
/// Codex's next-round finding: the scrutinee's own token text was read straight from
/// whichever side did not resolve, with no normalisation of *that* side — so `x == 0`
/// alongside a later link's `(x) == 1` compared "x" against "(x)" and never matched, even
/// though `(x)` names the identical scrutinee `rustc` sees straight through. [`strip_parens`]
/// is applied to the unresolved side before its token text is taken, the same normalisation
/// every other literal- or constant-reading function here already applies before it looks
/// at an expression's own shape.
fn if_chain_condition_value(
    cond: &syn::Expr,
    resolve: &Resolve<'_>,
) -> Option<(String, i128, bool)> {
    let cond = strip_parens(cond);
    let syn::Expr::Binary(binary) = cond else {
        return None;
    };
    if !matches!(binary.op, syn::BinOp::Eq(_)) {
        return None;
    }
    let left_value = literal_or_const_value(&binary.left, resolve);
    let right_value = literal_or_const_value(&binary.right, resolve);
    match (left_value, right_value) {
        (None, Some(value)) => Some((
            strip_parens(&binary.left).to_token_stream().to_string(),
            value,
            is_definitely_unsigned(&binary.right, resolve),
        )),
        (Some(value), None) => Some((
            strip_parens(&binary.right).to_token_stream().to_string(),
            value,
            is_definitely_unsigned(&binary.left, resolve),
        )),
        _ => None,
    }
}

/// `expr`, seen through any nesting of parentheses or brace groups — the same
/// normalisation most literal- and constant-reading functions in this file already apply
/// before they look at an expression's own shape, factored out here so
/// [`if_chain_condition_value`] can apply it to a *sub*-expression rather than only to
/// the top-level one it is handed.
fn strip_parens(mut expr: &syn::Expr) -> &syn::Expr {
    loop {
        expr = match expr {
            syn::Expr::Paren(paren) => &paren.expr,
            syn::Expr::Group(group) => &group.expr,
            other => return other,
        };
    }
}

/// `node`'s own scrutinee and arms, read as the identical [`FoundMatch`] shape a real
/// `match` over the same values already produces — so the density check downstream never
/// has to know which syntax the source used. `None` when `node` is not, structurally, a
/// chain of `scrutinee == literal` comparisons over one consistent scrutinee ending in an
/// unconditional `else`: a chain with no final `else` cannot type-check as an integer value
/// in real Rust (the missing branch would have to produce `()`, the same reasoning
/// [`literal_or_const_value`]'s own `Expr::If` case already rests on), a link whose
/// condition is not `scrutinee == literal` is not this shape at all, and a link naming a
/// *different* scrutinee than the chain's first link is two unrelated comparisons that
/// happen to share an `else if`, not one dense table.
fn extract_if_chain(node: &syn::ExprIf, resolve: &Resolve<'_>) -> Option<(String, Vec<FoundArm>)> {
    let (scrutinee, first_value, first_unsigned) = if_chain_condition_value(&node.cond, resolve)?;
    let mut arms = vec![FoundArm {
        pattern: vec![first_value],
        is_wild: false,
        call: call_shape_of(&block_as_expr(&node.then_branch), resolve),
        unsigned: first_unsigned,
    }];
    let mut current = node;
    loop {
        let (_, else_expr) = current.else_branch.as_ref()?;
        match else_expr.as_ref() {
            syn::Expr::If(next_if) => {
                let (next_scrutinee, next_value, next_unsigned) =
                    if_chain_condition_value(&next_if.cond, resolve)?;
                if next_scrutinee != scrutinee {
                    return None;
                }
                arms.push(FoundArm {
                    pattern: vec![next_value],
                    is_wild: false,
                    call: call_shape_of(&block_as_expr(&next_if.then_branch), resolve),
                    unsigned: next_unsigned,
                });
                current = next_if;
            }
            syn::Expr::Block(else_block) if else_block.label.is_none() => {
                arms.push(FoundArm {
                    pattern: Vec::new(),
                    is_wild: true,
                    call: call_shape_of(&block_as_expr(&else_block.block), resolve),
                    unsigned: false,
                });
                break;
            }
            _ => return None,
        }
    }
    Some((scrutinee, arms))
}

/// `path`'s value as a *qualified* reference (`module::P0`, or a longer chain reaching one),
/// resolved against every module-qualified constant [`MatchVisitor`] has recorded so far.
///
/// Codex's finding: [`ConstScopes`] alone only ever resolves a bare, single-segment name
/// against the scopes lexically enclosing a match — exactly right for `P0`, and exactly
/// wrong for `indices::P0`, which `Pat::Path`/`Expr::Path` parse as a *multi*-segment path
/// that a single-identifier lookup (`path.get_ident()`) simply refuses to look at, silently
/// treating a qualified constant pattern as unresolved rather than as the value it names. A
/// leading `crate`/`self` is stripped before joining, since it names no module of its own.
///
/// Codex's next-round finding: a *relative* reference is not always the file-root path it
/// happens to share a spelling with. `indices::P0` written inside `mod outer` resolves in
/// Rust against `outer`'s own scope — `outer::indices::P0` — not against a top-level
/// `mod indices` of the same name, because plain module-relative resolution consults the
/// current module's own items rather than the file root. So the stripped chain is tried
/// first with `current_module` — [`MatchVisitor::module_path`] at the match's own position
/// — prepended, which is what a bare or `self`-relative path actually resolves against.
///
/// Codex's finding after that: trying the plain, unprefixed chain *first* still answers
/// wrong when a same-named path exists at both the file root and the current module — a
/// root `mod indices` and an `outer::indices` both declaring `P0` means the root lookup
/// would find something and return before the relative one is ever tried, even though
/// Rust resolves `indices::P0` written inside `outer` to `outer`'s own `indices`
/// exclusively. So the relative lookup goes first and the plain chain is the fallback, not
/// the other way around.
///
/// Codex's finding after *that*: a leading `super` was left in the chain rather than
/// resolved, so it never matched `current_module` (which holds plain module names, `super`
/// among none of them) and fell all the way to the last-two-segments heuristic — silently
/// right when that heuristic's guess happened to agree, silently wrong whenever a
/// same-named module also existed at the file root. Each leading `super` now steps one
/// level up from `current_module` before the relative lookup runs, the same way `rustc`
/// itself resolves it — `super::indices::P0` written inside `outer::inner` is tried against
/// `outer::indices::P0`, not against `outer::inner`'s own scope or the file root. A chain
/// with more `super`s than `current_module` has levels clamps to the file root, since there
/// is nowhere higher to step to. What matches neither the relative form nor the plain chain
/// still falls back to its last two segments (`module::name`).
///
/// Codex's finding after *that*: a leading `crate` was stripped the same way `self` is,
/// which throws away the one thing distinguishing them — `crate::indices::P0` names the
/// crate root and nothing else, in real Rust, whichever module the reference sits in, but
/// the stripped chain still tried the current-module-relative form *first*, so a same-named
/// `outer::indices` could answer for a reference that explicitly asked to skip past it.
/// `crate::` now records that it was absolute before the shared strip erases the word, and
/// skips the relative attempt outright — going straight to the plain chain, which is what
/// lets `crate::indices::P0` keep finding a `mod indices` recorded relative to the file root.
///
/// This is a pure map lookup with no well-known-bound fallback of its own — deliberately,
/// since [`resolve_qualified_path_at_any_depth`] calls this once per depth it tries, and a
/// fallback embedded here would answer from the *first*, most deeply nested depth a real
/// declaration might not sit at, before the outer depths where it actually does are ever
/// tried. [`resolve_qualified_path_at_any_depth`]'s own doc comment has the finding that
/// taught this.
fn resolve_qualified_path(
    path: &syn::Path,
    qualified: &std::collections::HashMap<String, i128>,
    current_module: &[String],
) -> Option<i128> {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    let is_crate_absolute = segments.first().map(String::as_str) == Some("crate");
    let relevant: Vec<&str> = segments
        .iter()
        .map(String::as_str)
        .skip_while(|segment| *segment == "crate" || *segment == "self")
        .collect();
    if relevant.len() < 2 {
        return None;
    }
    let super_count = relevant
        .iter()
        .take_while(|segment| **segment == "super")
        .count();
    let rest = relevant.get(super_count..)?;
    if rest.len() < 2 {
        return None;
    }
    let joined = rest.join("::");
    if !is_crate_absolute {
        let pop = super_count.min(current_module.len());
        let effective_module = current_module.get(..current_module.len() - pop)?;
        if !effective_module.is_empty() {
            let relative = format!("{}::{joined}", effective_module.join("::"));
            if let Some(value) = qualified.get(&relative) {
                return Some(*value);
            }
        }
    }
    if let Some(value) = qualified.get(&joined) {
        return Some(*value);
    }
    let tail_start = rest.len().saturating_sub(2);
    let tail = rest.get(tail_start..)?;
    qualified.get(&tail.join("::")).copied()
}

/// [`resolve_qualified_path`]'s own answer for whether the entry it found — the *same* one,
/// at the *same* key, searched in the identical relative-then-joined-then-tail order — is
/// one `qualified_unsigned` marks unsigned. Every `qualified.get` this mirrors is gated on
/// the identical `qualified.contains_key` here, so this never reports on a key
/// `resolve_qualified_path` itself would not have used to answer the value: a `relative` key
/// present only in `qualified_unsigned` and not in `qualified` — which never happens given
/// the two are inserted together, but this reads defensively rather than trusting that — is
/// not consulted ahead of a `joined` key `resolve_qualified_path` would have found first.
fn resolve_qualified_unsigned(
    path: &syn::Path,
    qualified: &std::collections::HashMap<String, i128>,
    qualified_unsigned: &std::collections::HashMap<String, bool>,
    current_module: &[String],
) -> bool {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    let is_crate_absolute = segments.first().map(String::as_str) == Some("crate");
    let relevant: Vec<&str> = segments
        .iter()
        .map(String::as_str)
        .skip_while(|segment| *segment == "crate" || *segment == "self")
        .collect();
    let Some(super_count) = (relevant.len() >= 2).then(|| {
        relevant
            .iter()
            .take_while(|segment| **segment == "super")
            .count()
    }) else {
        return false;
    };
    let Some(rest) = relevant.get(super_count..) else {
        return false;
    };
    if rest.len() < 2 {
        return false;
    }
    let joined = rest.join("::");
    if !is_crate_absolute {
        let pop = super_count.min(current_module.len());
        if let Some(effective_module) = current_module.get(..current_module.len() - pop) {
            if !effective_module.is_empty() {
                let relative = format!("{}::{joined}", effective_module.join("::"));
                if qualified.contains_key(&relative) {
                    return qualified_unsigned.get(&relative).copied().unwrap_or(false);
                }
            }
        }
    }
    if qualified.contains_key(&joined) {
        return qualified_unsigned.get(&joined).copied().unwrap_or(false);
    }
    let tail_start = rest.len().saturating_sub(2);
    let Some(tail) = rest.get(tail_start..) else {
        return false;
    };
    let tail_key = tail.join("::");
    qualified.contains_key(&tail_key) && qualified_unsigned.get(&tail_key).copied().unwrap_or(false)
}

/// [`resolve_qualified_unsigned`]'s own [`resolve_qualified_path_at_any_depth`]: the same
/// depth search, re-run to find which depth's `combined_module` is the one
/// [`resolve_qualified_path`] itself would answer from, and [`resolve_qualified_unsigned`]'s
/// answer at that exact depth — never a different one, which is what would happen were this
/// to search `qualified_unsigned` independently rather than re-deriving the depth the value
/// search itself settled on.
fn resolve_qualified_unsigned_at_any_depth(
    path: &syn::Path,
    qualified: &std::collections::HashMap<String, i128>,
    qualified_unsigned: &std::collections::HashMap<String, bool>,
    module_path: &[String],
    function_path: &[String],
    block_path: &[String],
) -> bool {
    for depth in (0..=block_path.len()).rev() {
        let mut combined_module = module_path.to_vec();
        combined_module.extend(function_path.iter().cloned());
        if let Some(prefix) = block_path.get(..depth) {
            combined_module.extend(prefix.iter().cloned());
        }
        if resolve_qualified_path(path, qualified, &combined_module).is_some() {
            return resolve_qualified_unsigned(
                path,
                qualified,
                qualified_unsigned,
                &combined_module,
            );
        }
    }
    if !(function_path.is_empty() && block_path.is_empty())
        && resolve_qualified_path(path, qualified, module_path).is_some()
    {
        return resolve_qualified_unsigned(path, qualified, qualified_unsigned, module_path);
    }
    false
}

/// [`resolve_qualified_unsigned`]'s own mirror for a declared *type name* rather than only
/// whether one is unsigned — [`resolve_qualified_path`]'s own answer for whether the entry
/// it found is one `qualified_types` records a type name for. Every `qualified.get` this
/// mirrors is gated on the identical `qualified.contains_key` here, for the identical reason
/// `resolve_qualified_unsigned` already states: a `relative` key present only in
/// `qualified_types` and not in `qualified` never happens given the two are inserted
/// together, but this reads defensively rather than trusting that.
///
/// Codex's next-round finding: `path_declared_type` answered `None` for every *qualified*
/// reference unconditionally, because `qualified` carried no declared-type counterpart at
/// all — only [`ConstTypeScopes`] did, for the bare-name case. `const OFF: bool = false;` in
/// an inline `mod bounds { .. }` this file itself declares, referenced as `!bounds::OFF` in a
/// match guard, therefore stayed unresolved the identical way a qualified `bounds::HI`'s own
/// unsignedness once did before [`resolve_qualified_unsigned`] existed. `MatchVisitor::
/// qualified_types` is `qualified`'s own type-name mirror, inserted at the identical key
/// everywhere `qualified` itself gains one, the same way `qualified_unsigned` already is.
fn resolve_qualified_type<'a>(
    path: &syn::Path,
    qualified: &std::collections::HashMap<String, i128>,
    qualified_types: &'a std::collections::HashMap<String, String>,
    current_module: &[String],
) -> Option<&'a str> {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    let is_crate_absolute = segments.first().map(String::as_str) == Some("crate");
    let relevant: Vec<&str> = segments
        .iter()
        .map(String::as_str)
        .skip_while(|segment| *segment == "crate" || *segment == "self")
        .collect();
    let super_count = (relevant.len() >= 2).then(|| {
        relevant
            .iter()
            .take_while(|segment| **segment == "super")
            .count()
    })?;
    let rest = relevant.get(super_count..)?;
    if rest.len() < 2 {
        return None;
    }
    let joined = rest.join("::");
    if !is_crate_absolute {
        let pop = super_count.min(current_module.len());
        if let Some(effective_module) = current_module.get(..current_module.len() - pop) {
            if !effective_module.is_empty() {
                let relative = format!("{}::{joined}", effective_module.join("::"));
                if qualified.contains_key(&relative) {
                    return qualified_types.get(&relative).map(String::as_str);
                }
            }
        }
    }
    if qualified.contains_key(&joined) {
        return qualified_types.get(&joined).map(String::as_str);
    }
    let tail_start = rest.len().saturating_sub(2);
    let tail = rest.get(tail_start..)?;
    let tail_key = tail.join("::");
    qualified
        .contains_key(&tail_key)
        .then(|| qualified_types.get(&tail_key).map(String::as_str))
        .flatten()
}

/// [`resolve_qualified_type`]'s own [`resolve_qualified_unsigned_at_any_depth`]: the same
/// depth search, re-run to find which depth's `combined_module` is the one
/// [`resolve_qualified_path`] itself would answer from, and [`resolve_qualified_type`]'s
/// answer at that exact depth — never a different one, for the identical reason
/// [`resolve_qualified_unsigned_at_any_depth`] already states.
fn resolve_qualified_type_at_any_depth<'a>(
    path: &syn::Path,
    qualified: &std::collections::HashMap<String, i128>,
    qualified_types: &'a std::collections::HashMap<String, String>,
    module_path: &[String],
    function_path: &[String],
    block_path: &[String],
) -> Option<&'a str> {
    for depth in (0..=block_path.len()).rev() {
        let mut combined_module = module_path.to_vec();
        combined_module.extend(function_path.iter().cloned());
        if let Some(prefix) = block_path.get(..depth) {
            combined_module.extend(prefix.iter().cloned());
        }
        if resolve_qualified_path(path, qualified, &combined_module).is_some() {
            return resolve_qualified_type(path, qualified, qualified_types, &combined_module);
        }
    }
    if !(function_path.is_empty() && block_path.is_empty())
        && resolve_qualified_path(path, qualified, module_path).is_some()
    {
        return resolve_qualified_type(path, qualified, qualified_types, module_path);
    }
    None
}

/// `name`'s value at the module `levels_up` ancestors above the current one — `0` for the
/// current module (`self::NAME`), `1` for its parent (`super::NAME`), and so on.
///
/// Three cases, in the order they are checked. `levels_up` short of
/// `module_scope_depths.len()` names a module *this file's own walk entered* — resolved
/// against the scope stack, truncated to the depth [`MatchVisitor::visit_item_mod`]
/// recorded for it. `levels_up` exactly `module_scope_depths.len()` names this file's own
/// root — depth `1`, the one scope every walk starts with regardless of how many modules
/// it later enters.
///
/// Codex's finding is the third case: `levels_up` *past* that range names a module
/// [`MatchVisitor::module_path`]'s own seeded prefix knows about but this file's scope
/// stack was never given a scope for at all, because that module's `mod { ... }` body lives
/// in a *different* file of the same out-of-line tree (round 19's `crc/outer.rs` declaring
/// `mod inner;`, with the match itself inside `inner.rs`, reading `super::P0` for a `P0`
/// `outer.rs` declares). Such an ancestor's own constants are exactly what `qualified`
/// already holds, under that ancestor's own dotted module path (`module_path`'s first
/// `index + 1` segments), because `visit_item_mod` records a module's constants there
/// regardless of which file declared it. An ancestor past even the seeded prefix's own
/// length names a module no file this scan has read declares at all, and resolves to
/// nothing rather than guessing.
fn resolve_ancestor_single_segment(
    module_path: &[String],
    module_scope_depths: &[usize],
    scopes: &ConstScopes,
    qualified: &std::collections::HashMap<String, i128>,
    levels_up: usize,
    name: &str,
) -> Option<i128> {
    let local_count = module_scope_depths.len();
    if levels_up < local_count {
        let depth = module_scope_depths
            .get(local_count - 1 - levels_up)
            .copied()?;
        return scopes.resolve_from(name, depth);
    }
    if levels_up == local_count {
        return scopes.resolve_from(name, 1);
    }
    let index = module_path.len().checked_sub(levels_up + 1)?;
    let ancestor = module_path.get(..=index)?;
    qualified
        .get(&format!("{}::{name}", ancestor.join("::")))
        .copied()
}

/// `refs`' own value when it is `crate`, `self`, or one or more `super`s, followed by
/// exactly one more segment — the value that segment names, resolved against the ancestor
/// the anchor names rather than [`ConstScopes::resolve`]'s unrestricted outward search.
/// `None` when `refs` is not one of these three shapes at all, so the caller falls through
/// to its own unrelated handling for anything else — a module-qualified chain
/// (`super::indices::P0`) among them, which stays [`resolve_qualified_path`]'s.
///
/// Codex's finding: `super::P0`, resolved by a plain [`ConstScopes::resolve`], searches the
/// *entire* live stack, including a scope nested more deeply than the ancestor `super`
/// actually names — a child module shadowing its parent's `P0` with a non-dense value made
/// a match inside that child reading `super::P0` find the child's own value instead of the
/// parent's, since the child's own scope is innermost and an unrestricted search never
/// learns to stop before it. `self::P0` has the identical shape of gap (`levels_up: 0`),
/// solved the same way [`resolve_ancestor_single_segment`] solves `super`'s.
///
/// `crate::P0` is not the same question at all, and Codex's next finding is that the
/// earlier version answered it as though it were: it resolved `crate::NAME` against this
/// *file's* own root (depth `1`), on the reasoning that a bare `crate::NAME` names
/// something at the crate's true root and this scan cannot see past its own top scope —
/// true of the second half and exactly backwards on the first. The scanned file
/// (`crc.rs`) is itself one submodule of `waymaker-flash`, `crate::crc`, not the crate's
/// own root `crate` — so `crate::P0` names a constant declared in `waymaker-flash`'s
/// `lib.rs`, which is outside the checksum module's own tree this scan ever reads, and
/// answering from this file's root was answering a different, unasked question
/// (`crc::P0` in Rust's own naming, which a bare unanchored `P0` or a `self::P0` already
/// reach correctly). Resolved as always-unresolved instead: `AnchoredLookup::Resolved` is
/// still returned unconditionally, so a `crate::P0` this scan cannot see behind still
/// fails closed to "not dense" rather than falling through to `resolve_qualified_path`'s
/// own unrelated handling, which could otherwise resolve a same-named `crc::P0` by
/// coincidence and answer with the wrong module's value.
///
/// [`resolve_anchored_single_segment`]'s answer: either the shape does not apply and the
/// caller's own unrelated handling decides, or it does, with whatever value (`None`
/// included) the anchor's own scope resolved the remaining name to.
enum AnchoredLookup {
    /// Not `crate`/`self`/`super`-anchored with exactly one segment left, or not anchored
    /// at all.
    NotApplicable,
    /// Anchored; this is what the anchor's own scope resolved the name to.
    Resolved(Option<i128>),
}

fn resolve_anchored_single_segment(
    refs: &[&str],
    scopes: &ConstScopes,
    module_path: &[String],
    module_scope_depths: &[usize],
    qualified: &std::collections::HashMap<String, i128>,
) -> AnchoredLookup {
    let Some((&first, rest)) = refs.split_first() else {
        return AnchoredLookup::NotApplicable;
    };
    if first == "crate" {
        let (true, Some(_)) = (rest.len() == 1, rest.first()) else {
            return AnchoredLookup::NotApplicable;
        };
        // A bare `crate::NAME` names the crate's true root, `waymaker-flash/src/lib.rs`
        // — outside the checksum module's own tree this scan ever reads, and never this
        // scanned *file's* own root the way the removed `scopes.resolve_from(name, 1)`
        // answered it. Anchored and always unresolved, so the caller fails closed rather
        // than falling through to `resolve_qualified_path`, which could otherwise answer
        // with a same-named constant from the wrong module entirely.
        return AnchoredLookup::Resolved(None);
    }
    if first == "self" {
        let (true, Some(&name)) = (rest.len() == 1, rest.first()) else {
            return AnchoredLookup::NotApplicable;
        };
        return AnchoredLookup::Resolved(resolve_ancestor_single_segment(
            module_path,
            module_scope_depths,
            scopes,
            qualified,
            0,
            name,
        ));
    }
    if first == "super" {
        let super_count = refs
            .iter()
            .take_while(|segment| **segment == "super")
            .count();
        let Some(after_super) = refs.get(super_count..) else {
            return AnchoredLookup::NotApplicable;
        };
        let (true, Some(&name)) = (after_super.len() == 1, after_super.first()) else {
            return AnchoredLookup::NotApplicable;
        };
        return AnchoredLookup::Resolved(resolve_ancestor_single_segment(
            module_path,
            module_scope_depths,
            scopes,
            qualified,
            super_count,
            name,
        ));
    }
    AnchoredLookup::NotApplicable
}

/// `path`'s own value, the whole of what a [`MatchVisitor`] arm's `resolve` closure asks —
/// factored out to a plain function, rather than left inline in the closure, so that
/// resolving a `use`-imported bare name can recurse into this same question over the
/// import's own target path.
///
/// Codex's finding: `use indices::{P0, P1};` brings `indices::P0` into scope under the
/// bare name `P0`, and Rust resolves a pattern spelled `P0` exactly as if the full path
/// `indices::P0` had been written out — but a bare identifier here only ever consulted
/// `scopes` (locally declared constants), never `use_scopes`. A single-segment path now
/// falls back to the import scope when the constant scope has nothing, and — found there —
/// is resolved the same way any other qualified pattern is, by building a fresh
/// [`syn::Path`] from the import's own target and asking this same question of it. The
/// recursion terminates because a `use` target is never itself the name it was imported
/// under (Rust rejects `use self::P0 as P0;` as importing nothing new), so it cannot loop.
/// Everything [`resolve_pattern_path`] needs to answer one path, bundled behind a single
/// reference rather than left as an ever-growing list of positional parameters — the
/// scan environment picked up a ninth dimension (`self_type_path`) the same round
/// `clippy::too_many_arguments` would have started flagging the loose form.
#[derive(Clone, Copy)]
struct ResolutionContext<'a> {
    scopes: &'a ConstScopes,
    use_scopes: &'a UseScopes,
    qualified: &'a std::collections::HashMap<String, i128>,
    module_path: &'a [String],
    module_scope_depths: &'a [usize],
    function_path: &'a [String],
    block_path: &'a [String],
    /// The stack of concrete types `Self` currently names, innermost last —
    /// [`MatchVisitor`]'s own `self_type_path`, pushed by `visit_item_impl` and consulted
    /// here for a leading `Self` segment the same way `crate`/`self`/`super` are already
    /// special-cased rather than searched for by name.
    self_type_path: &'a [String],
    /// [`UnsignedConstScopes`]'s own mirror of `scopes` — [`path_is_definitely_unsigned`]'s
    /// bare-identifier case, the way `scopes` is [`resolve_pattern_path`]'s.
    scopes_unsigned: &'a UnsignedConstScopes,
    /// `qualified`'s own unsignedness mirror — [`path_is_definitely_unsigned`]'s qualified
    /// case, the way `scopes_unsigned` is its bare-identifier one.
    qualified_unsigned: &'a std::collections::HashMap<String, bool>,
    /// [`ConstTypeScopes`]'s own mirror of `scopes` — [`path_declared_type`]'s
    /// bare-identifier case, the way `scopes_unsigned` is [`path_is_definitely_unsigned`]'s.
    scopes_types: &'a ConstTypeScopes,
    /// `qualified`'s own declared-type mirror — [`path_declared_type`]'s qualified case, the
    /// way `qualified_unsigned` is [`path_is_definitely_unsigned`]'s.
    qualified_types: &'a std::collections::HashMap<String, String>,
}

/// `path`'s own value against `qualified`, searched at every depth a bare or qualified
/// reference written inside `module_path`/`function_path`/`block_path` could resolve at:
/// the full combination first (a module local to this exact function or block), each
/// trailing `block_path` segment peeled off in turn, then `function_path` dropped
/// entirely, and finally `module_path` alone (a module declared at ordinary, file-level
/// scope) — the same most-specific-first order [`resolve_pattern_path`]'s own fallback
/// uses, factored out so a second caller can search identically rather than fixing one
/// combination and missing every declaration not keyed at exactly that depth.
///
/// Codex's forty-second-round finding: [`resolve_scope_consts`]'s own qualified-initializer
/// lookup did exactly that — one fixed `module_path + function_path + block_path`
/// combination — so `const P0: u8 = base::BASE + 0;`, written inside a function whose
/// enclosing module is where `mod base` actually sits, searched only
/// `enclosing_module::this_function::base::BASE`, never `enclosing_module::base::BASE`
/// itself, and the whole initializer stayed unresolved even though the qualified map
/// already held it.
///
/// Codex's next-round finding: `u8::MIN` is a real, well-known associated constant of a
/// language primitive, tried once every depth above has already failed — but a *local*
/// `mod u8 { pub const MIN: u8 = 0; .. }` shadows it, and an earlier version of this fix put
/// the well-known fallback inside [`resolve_qualified_path`] itself, which this function
/// calls once per depth: the fallback then answered from the first, most deeply nested depth
/// tried (`module_path` plus `function_path` plus every prefix of `block_path`), before the
/// *outer* depth where the shadowing module's own declaration was actually recorded —
/// ordinarily `module_path` alone — was ever reached, so a real declaration lost to the
/// primitive whenever a reference sat inside a function or a block. The fallback is tried
/// here instead, exactly once, only after every depth above — the full search this function
/// exists to make — has already failed, so a real declaration at *any* depth always wins and
/// the primitive answers only when nothing anywhere shadows it.
fn resolve_qualified_path_at_any_depth(
    path: &syn::Path,
    qualified: &std::collections::HashMap<String, i128>,
    module_path: &[String],
    function_path: &[String],
    block_path: &[String],
) -> Option<i128> {
    for depth in (0..=block_path.len()).rev() {
        let mut combined_module = module_path.to_vec();
        combined_module.extend(function_path.iter().cloned());
        if let Some(prefix) = block_path.get(..depth) {
            combined_module.extend(prefix.iter().cloned());
        }
        if let Some(value) = resolve_qualified_path(path, qualified, &combined_module) {
            return Some(value);
        }
    }
    if !(function_path.is_empty() && block_path.is_empty()) {
        if let Some(value) = resolve_qualified_path(path, qualified, module_path) {
            return Some(value);
        }
    }
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    let (type_name, member) = well_known_bound_segments(&segments)?;
    well_known_integer_bound(type_name, member)
}

/// Whether some real, already-scanned entry in `qualified` answers `path` — the identical
/// depth-loop-then-plain-module-path search [`resolve_qualified_path_at_any_depth`] itself
/// runs before its own well-known-primitive-bound fallback, with that fallback excluded.
///
/// Codex's next-round finding: [`path_is_definitely_unsigned`]'s own "a real declaration
/// wins over the primitive's own bound" guard called [`resolve_qualified_path_at_any_depth`]
/// directly and treated any `Some` answer as proof one existed — but that function's own
/// last line answers `Some` for a path like `u128::MAX` from its *own* well-known-bound
/// fallback whenever nothing shadows it, which is exactly the case this guard exists to tell
/// apart from a real one, not a confirmation of one. The guard therefore fired on every
/// path `well_known_bound_segments` itself would also recognise — since the fallback that
/// answers `well_known_integer_bound(type_name, member)` for the guard's own local check is
/// the identical call the value resolver just made two lines above it — so
/// `well_known_integer_bound(type_name, member).is_some()` on the line after the guard was
/// unreachable: every well-known bound this scan resolves at all read as signed, `u128::MAX`
/// included, however it was confirmed unsigned via `Expr::Lit`'s own suffix elsewhere. This
/// is the value resolver's identical two-step search with the tail cut off, so a real
/// declaration is told apart from the fallback that stands in for one only when nothing real
/// answered first.
fn qualified_path_is_declared_at_any_depth(
    path: &syn::Path,
    qualified: &std::collections::HashMap<String, i128>,
    module_path: &[String],
    function_path: &[String],
    block_path: &[String],
) -> bool {
    for depth in (0..=block_path.len()).rev() {
        let mut combined_module = module_path.to_vec();
        combined_module.extend(function_path.iter().cloned());
        if let Some(prefix) = block_path.get(..depth) {
            combined_module.extend(prefix.iter().cloned());
        }
        if resolve_qualified_path(path, qualified, &combined_module).is_some() {
            return true;
        }
    }
    !(function_path.is_empty() && block_path.is_empty())
        && resolve_qualified_path(path, qualified, module_path).is_some()
}

fn resolve_pattern_path(path: &syn::Path, ctx: &ResolutionContext<'_>) -> Option<i128> {
    if let Some(ident) = path.get_ident() {
        let name = ident_name(ident);
        if let Some(value) = ctx.scopes.resolve(&name) {
            return Some(value);
        }
        if let Some(target) = ctx.use_scopes.resolve(&name) {
            let synthetic = syn::parse_str::<syn::Path>(&target.join("::")).ok()?;
            return resolve_pattern_path(&synthetic, ctx);
        }
        // Codex's finding: a name reached only through `use indices::*;` was recorded
        // nowhere, so it fell through to here and returned `None` exactly as an
        // unimported name would. Each glob's own prefix is tried in turn, innermost
        // scope first — the same shadowing order a named import already gets, above —
        // by building `prefix::name` and asking this same question of it, which lets the
        // multi-segment branch below resolve it exactly as a fully spelled-out
        // `indices::P0` already would.
        for prefix in ctx.use_scopes.glob_prefixes() {
            let mut full = prefix.to_vec();
            full.push(name.clone());
            let Ok(synthetic) = syn::parse_str::<syn::Path>(&full.join("::")) else {
                continue;
            };
            if let Some(value) = resolve_pattern_path(&synthetic, ctx) {
                return Some(value);
            }
        }
        return None;
    }
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    // Codex's finding: `Self::P0`, written inside the very impl that declares `P0`,
    // names no module `resolve_qualified_path`'s map could ever hold under the literal
    // spelling `Self` — `MatchVisitor::visit_item_impl` indexes the constant under the
    // *concrete* implementing type's own name, exactly as it already does for `u8::P0`
    // reached through `<u8 as Indices>::P0`. A leading `Self` segment is substituted for
    // the innermost enclosing impl's own type and the question asked again of the
    // rewritten path — recursing rather than rewriting `segments` in place, because
    // every lookup below this point re-derives its own segments from `path` itself, and
    // a `Vec<String>` edited only here would never reach them. Left unsubstituted, and so
    // left to resolve to nothing exactly as before, when no enclosing impl's type is one
    // this scan could index in the first place (a generic, qualified, or multi-segment
    // `Self` type never is).
    if segments.first().map(String::as_str) == Some("Self") {
        if let Some(current_self) = ctx.self_type_path.last() {
            let substituted = std::iter::once(current_self.clone())
                .chain(segments.iter().skip(1).cloned())
                .collect::<Vec<_>>()
                .join("::");
            if let Ok(synthetic) = syn::parse_str::<syn::Path>(&substituted) {
                return resolve_pattern_path(&synthetic, ctx);
            }
        }
    }
    // Codex's finding: `use indices as idx;` binds `idx` to `indices` exactly the way
    // `use indices::P0;` binds `P0` to `indices::P0`, above, but only the single-segment
    // branch ever consulted `use_scopes` — a multi-segment path headed by a module alias
    // (`idx::P0`) went straight to the anchored and qualified lookups below, which know
    // the real module by its own name (`indices::P0` in the tree) and nothing by the
    // alias. `crate`, `self` and `super` can never be an alias's own bound name (they are
    // reserved words, not identifiers a `use` can rename to), so trying this first cannot
    // shadow the anchor handling below.
    if let Some((first, rest)) = segments.split_first() {
        if let Some(target) = ctx.use_scopes.resolve(first) {
            let mut full = target.to_vec();
            full.extend(rest.iter().cloned());
            if let Ok(synthetic) = syn::parse_str::<syn::Path>(&full.join("::")) {
                if let Some(value) = resolve_pattern_path(&synthetic, ctx) {
                    return Some(value);
                }
            }
        }
    }
    let refs: Vec<&str> = segments.iter().map(String::as_str).collect();
    // `crate`/`self`/`super` followed by exactly one more segment names a plain
    // constant declared directly in a specific ancestor, not a further-qualified
    // `module::name` chain — `resolve_qualified_path`'s map has no entry for a
    // constant that was never itself nested in a named module of its own. Tried
    // before anything else strips these words, because the anchor decides *which*
    // scope the remaining name is looked up against.
    if let AnchoredLookup::Resolved(value) = resolve_anchored_single_segment(
        &refs,
        ctx.scopes,
        ctx.module_path,
        ctx.module_scope_depths,
        ctx.qualified,
    ) {
        return value;
    }
    // A leading `crate` or `self` names no module of its own, so a path that is
    // only that plus one more segment (`crate::P0`) is still a bare, ambiently
    // resolved name once it is stripped — the qualified map only holds an actual
    // module's own constants. (The anchored form above already caught this shape;
    // reaching here means `refs` had more than one segment left after `crate`/
    // `self`, so this is `crate::module::P0` rather than `crate::P0`.)
    let relevant: Vec<&str> = refs
        .iter()
        .copied()
        .skip_while(|segment| *segment == "crate" || *segment == "self")
        .collect();
    if relevant.len() == 1 {
        return relevant.first().and_then(|name| ctx.scopes.resolve(name));
    }
    // Codex's finding: two different functions each declaring their own local
    // `mod indices { .. }` recorded their constants under the identical key
    // `module_path::indices::P0`, because nothing distinguished which function a
    // function-local module sat inside — the later-visited function's values silently
    // overwrote the earlier one's. `function_path` is appended after `module_path` here,
    // for this lookup only, mirroring how `MatchVisitor`'s own insertion sites now build
    // the matching key; `resolve_anchored_single_segment`, above, deliberately keeps
    // receiving the unmodified `module_path` instead, since a function is not a module
    // and `self`/`super`/`crate` arithmetic must not treat it as one.
    //
    // Codex's next-round finding is the identical shape one level finer: two *sibling
    // blocks in the same function* — an `if`/`else`'s two arms, two bare `{ .. }`
    // expressions, or any other nesting `rustc` treats as its own scope — each declaring
    // their own local `mod indices` collided exactly as the two functions did, because
    // `function_path` alone cannot tell two blocks of one function apart. `block_path` is
    // `MatchVisitor`'s own count of blocks entered, appended after `function_path` the
    // same way, so a module declared inside one block is never read from another.
    //
    // The most-qualified attempt is tried first and not exclusively: a block can
    // reference a `mod` declared in an *enclosing* block or function, or at ordinary
    // module scope, exactly as freely as code outside any block or function can — a
    // nested `if` reading a module its enclosing function declared, say — and that is
    // the common case in this scan's own fixtures. Requiring the full `block_path` to
    // match would break every one of those, so each of its own trailing segments is
    // peeled off in turn, most specific first, before the search drops `function_path`
    // too and finally tries the plain `module_path` alone — the same order Rust's own
    // name resolution would use: the innermost declaration in scope wins, and only a
    // real collision between two same-named, equally-nested local modules is what this
    // fallback cannot silently paper over, because each was keyed under its own block's
    // place in the walk and neither can be reached from the other's.
    // Codex's next-round finding: `u8::MIN` is a real, well-known associated constant of
    // a language primitive — not anything the scanned source tree ever declares — so no
    // amount of collecting local, qualified or trait-default constants would ever find
    // it, and a table whose numbered arms are spelled `u8::MIN` through `u8::MIN + 14`
    // read as unresolved on every arm. [`resolve_qualified_path_at_any_depth`] is where
    // that fallback lives now, tried only once every depth of the search below has already
    // failed — its own doc comment has the finding that moved it there, off a version of
    // this function that tried it too early and let it answer ahead of a local `mod u8`
    // shadowing the primitive.
    resolve_qualified_path_at_any_depth(
        path,
        ctx.qualified,
        ctx.module_path,
        ctx.function_path,
        ctx.block_path,
    )
}

/// Whether `path`, if it resolves to a constant at all, was declared with an explicit
/// unsigned integer type — [`is_definitely_unsigned`]'s own answer for a bare or
/// two-segment `Expr::Path` operand, mirroring the value resolution
/// [`resolve_pattern_path`] performs but answering the question that function cannot: not
/// what the constant equals, but which domain its own declaration names.
///
/// Scoped to the shapes this scan can answer without guessing: a bare name, searched through
/// [`ConstScopes`]'s own shadowing order via [`UnsignedConstScopes`]'s identical stack; a
/// module-qualified name declared in this file's own tree, searched the identical way
/// [`resolve_qualified_path_at_any_depth`] searches for its value, via
/// [`resolve_qualified_unsigned_at_any_depth`]'s mirror; and a well-known associated bound
/// (`u128::MAX`), whose declaring type is the path's own first segment and needs no scope at
/// all — tried last, and only once the qualified search has found no real declaration, the
/// identical ordering [`resolve_qualified_path_at_any_depth`] itself uses for the same
/// reason. A qualified reference resolved from *outside* this file's own tree — a cross-file
/// `external_qualified` seed neither entry point threads unsignedness through — is not
/// attempted, and answers `false`, the same safe default an unresolved bare name gets:
/// declining to fold an ordering guard is always sound, where guessing it is unsigned when
/// it might not be is not.
///
/// Codex's next-round finding: a qualified local constant (`bounds::HI`, `bounds` an inline
/// `mod` this file itself declares) answered `false` unconditionally, because `qualified`
/// carried a value for it but nothing here had a way to ask whether that value's own
/// declaration was unsigned — `path.get_ident()` is `None` for a multi-segment path, so
/// every qualified reference fell straight to the well-known-bound check, which answers only
/// for an *unqualified* primitive member. `MatchVisitor::qualified_unsigned` is `qualified`'s
/// own mirror now, inserted at the identical key everywhere `qualified` itself gains one.
fn path_is_definitely_unsigned(path: &syn::Path, ctx: &ResolutionContext<'_>) -> bool {
    if let Some(ident) = path.get_ident() {
        return ctx.scopes_unsigned.resolve(&ident_name(ident));
    }
    if resolve_qualified_unsigned_at_any_depth(
        path,
        ctx.qualified,
        ctx.qualified_unsigned,
        ctx.module_path,
        ctx.function_path,
        ctx.block_path,
    ) {
        return true;
    }
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    let Some((type_name, member)) = well_known_bound_segments(&segments) else {
        return false;
    };
    if !matches!(type_name, "u8" | "u16" | "u32" | "u64" | "u128") {
        return false;
    }
    // A real declaration anywhere in the depth search above — even one that turned out to
    // be signed, or unsigned but already answered `true` and returned before here — must
    // still win over the primitive's own bound. [`qualified_path_is_declared_at_any_depth`]
    // is that search alone, deliberately not [`resolve_qualified_path_at_any_depth`]'s own
    // fallback-carrying answer: that function's own last line resolves `path` from the
    // identical well-known bound this check is about to fall back to, so treating *its*
    // `Some` as evidence of a real declaration made this guard fire on every well-known
    // bound this scan can resolve at all, and the line below unreachable.
    if qualified_path_is_declared_at_any_depth(
        path,
        ctx.qualified,
        ctx.module_path,
        ctx.function_path,
        ctx.block_path,
    ) {
        return false;
    }
    well_known_integer_bound(type_name, member).is_some()
}

/// `path`'s own declared type name (`"u8"`, `"bool"`, ..) — a bare identifier naming a
/// `const` whose own type ascription [`ConstTypeScopes`] recorded, or a qualified reference
/// to one `ctx.qualified_types` records the identical way `ctx.qualified_unsigned` records an
/// unsignedness for the identical key. `None` for a well-known bound, which declines rather
/// than guesses, the identical narrowing [`Resolve::width`]'s own doc comment states.
///
/// Codex's next-round finding: `const OFF: bool = false; .. _ if !OFF => value, _ =>
/// fallback` named a guard [`evaluate_bitwise_not`] could not fold, because every match
/// arm's own `resolve.width` had been a stub answering `None` unconditionally — the
/// reasoning that only a `const`'s own initializer ever needed one held for an *integer*
/// width, where a value alone cannot say how many bits to mask to, but not for `bool`,
/// which this scan already represents as exactly `0` or `1` regardless of which way `!` is
/// read: the only fact missing was that `OFF` is a `bool` at all. This is that fact,
/// answered the identical bare-name-only way [`ConstTypeScopes::resolve`] already carries
/// it — a qualified reference used to stay undeclined by design, the same narrowing
/// `resolve_scope_consts`'s own `resolve_width` closure still applies for the identical
/// reason there: a wrong guess about which bits a qualified `!` operand's width covers is
/// not a risk worth taking to fold a case that pass can simply decline.
///
/// Codex's next-round finding: that same narrowing left a *qualified* `bool` unresolved too
/// — `const OFF: bool = false;` in an inline `mod bounds { .. }`, referenced as
/// `!bounds::OFF`, is not a guess about an integer's width, it is the identical fact
/// [`path_is_definitely_unsigned`] already resolves for a qualified reference through
/// `qualified_unsigned`. Widened to match: a qualified path now searches
/// [`resolve_qualified_type_at_any_depth`] the same most-specific-first way, before falling
/// back to `None` for anything neither map records.
fn path_declared_type<'a>(path: &syn::Path, ctx: &ResolutionContext<'a>) -> Option<&'a str> {
    if let Some(ident) = path.get_ident() {
        return ctx.scopes_types.resolve(&ident_name(ident));
    }
    resolve_qualified_type_at_any_depth(
        path,
        ctx.qualified,
        ctx.qualified_types,
        ctx.module_path,
        ctx.function_path,
        ctx.block_path,
    )
}

/// `trait_name`'s own default associated constants, found by the identical
/// most-specific-first search [`resolve_pattern_path`]'s own fallback already makes over
/// `module_path`, `function_path` and `block_path` — mirroring it exactly, rather than
/// inventing a second search order, because a trait impl's bare, unqualified reference to
/// its trait resolves under the same lexical-scoping rules as any other bare name.
///
/// Codex's finding: keying `trait_defaults` by the trait's bare name alone let two
/// *different* traits of the same name, declared in two different modules, collide — the
/// first module's `impl Indices for u8 {}` looked up "Indices" and could just as easily
/// find the second module's own same-named trait's defaults, non-dense ones included.
/// `visit_item_trait` now records each trait's defaults under its own full scope path,
/// the same way every other declaration here already is, and this is `visit_item_impl`'s
/// side of that: try the trait impl's own full scope first, peeling `block_path` down to
/// nothing, then drop `function_path` and try the bare `module_path` — never falling all
/// the way to a bare, unscoped trait name, which is exactly the collision this closes.
/// Returns the canonical scope path that answered, `path` and all, alongside the defaults
/// map itself — Codex's next-round finding is exactly why: a *caller* keying its own index
/// by the trait's bare name alone (rather than the full scope path that actually resolved)
/// reproduces the identical ambiguity this function exists to close, one call site later.
fn lookup_trait_defaults<'a>(
    trait_defaults: &'a std::collections::HashMap<String, std::collections::HashMap<String, i128>>,
    module_path: &[String],
    function_path: &[String],
    block_path: &[String],
    trait_name: &str,
) -> Option<(Vec<String>, &'a std::collections::HashMap<String, i128>)> {
    for depth in (0..=block_path.len()).rev() {
        let mut combined = module_path.to_vec();
        combined.extend(function_path.iter().cloned());
        if let Some(prefix) = block_path.get(..depth) {
            combined.extend(prefix.iter().cloned());
        }
        combined.push(trait_name.to_string());
        if let Some(defaults) = trait_defaults.get(&combined.join("::")) {
            return Some((combined, defaults));
        }
    }
    if function_path.is_empty() {
        return None;
    }
    let mut bare = module_path.to_vec();
    bare.push(trait_name.to_string());
    trait_defaults
        .get(&bare.join("::"))
        .map(|defaults| (bare, defaults))
}

/// `trait_path`'s own default associated constants, when `trait_path` is *qualified* — two
/// or more segments, `super::traits::Indices` or `crate::traits::Indices` or
/// `traits::Indices` rather than a bare `Indices` — resolved the identical way
/// [`resolve_qualified_path`] resolves a qualified *constant* reference against `qualified`:
/// `crate`, `self` and `super` read the same way, the current-module-relative form tried
/// before the plain chain, and a same-named trait declared at both the file root and the
/// current module told apart the same way a same-named constant already is. `current_module`
/// is the impl's own position, exactly as [`resolve_qualified_path`]'s own caller supplies —
/// so the search agrees with `resolve_pattern_path`'s peel-down over `module_path`,
/// `function_path` and `block_path` rather than fixing one combination.
///
/// Codex's next-round finding: [`lookup_trait_defaults`] resolves only a *bare* trait name,
/// searched against the trait *impl's own* lexical scope — right for `impl Indices for u8`
/// written beside `trait Indices`, and wrong the moment the impl names its trait through a
/// path of its own. `mod traits { trait Indices { .. } }` beside `mod implementations {
/// impl super::traits::Indices for u8 {} }` indexes the trait's defaults under
/// `"traits::Indices"` (`visit_item_trait`'s own full declaration scope), but the impl's
/// bare-name search only ever tried scopes built from `implementations` — the impl's *own*
/// module — and never consulted the qualified path (`super::traits::Indices`) the impl
/// itself wrote, which names the trait's real declaration exactly the way a qualified
/// constant path already does. `visit_item_impl` now tries this resolution first, against
/// the impl's full written trait path, before falling back to [`lookup_trait_defaults`]'s
/// bare-name search for a trait referenced without qualification.
/// Returns the canonical scope path that answered alongside the defaults map, the same
/// reason [`lookup_trait_defaults`] does: a caller indexing its own lookup under the
/// trait's bare last segment alone reproduces the ambiguity this whole search exists to
/// close, one call site later — see [`lookup_trait_defaults`]'s own next-round finding,
/// which this function shares in full.
fn resolve_qualified_trait_defaults<'a>(
    trait_defaults: &'a std::collections::HashMap<String, std::collections::HashMap<String, i128>>,
    trait_path: &syn::Path,
    current_module: &[String],
) -> Option<(Vec<String>, &'a std::collections::HashMap<String, i128>)> {
    let segments: Vec<String> = trait_path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    let is_crate_absolute = segments.first().map(String::as_str) == Some("crate");
    let relevant: Vec<&str> = segments
        .iter()
        .map(String::as_str)
        .skip_while(|segment| *segment == "crate" || *segment == "self")
        .collect();
    if relevant.len() < 2 {
        return None;
    }
    let super_count = relevant
        .iter()
        .take_while(|segment| **segment == "super")
        .count();
    let rest = relevant.get(super_count..)?;
    if rest.len() < 2 {
        return None;
    }
    let joined = rest.join("::");
    if !is_crate_absolute {
        let pop = super_count.min(current_module.len());
        let effective_module = current_module.get(..current_module.len() - pop)?;
        if !effective_module.is_empty() {
            let relative = format!("{}::{joined}", effective_module.join("::"));
            if let Some(defaults) = trait_defaults.get(&relative) {
                let mut resolved = effective_module.to_vec();
                resolved.extend(rest.iter().map(ToString::to_string));
                return Some((resolved, defaults));
            }
        }
    }
    if let Some(defaults) = trait_defaults.get(&joined) {
        return Some((rest.iter().map(ToString::to_string).collect(), defaults));
    }
    let tail_start = rest.len().saturating_sub(2);
    let tail = rest.get(tail_start..)?;
    trait_defaults
        .get(&tail.join("::"))
        .map(|defaults| (tail.iter().map(ToString::to_string).collect(), defaults))
}

/// [`resolve_qualified_trait_defaults`], peeled down over `module_path`, `function_path`
/// and `block_path` the identical way [`resolve_qualified_path_at_any_depth`] peels a
/// qualified constant reference — a trait impl can sit inside a function or a block, not
/// only directly inside a module, and a single fixed combination would miss a trait path
/// resolvable only at a shallower one.
fn resolve_qualified_trait_defaults_at_any_depth<'a>(
    trait_defaults: &'a std::collections::HashMap<String, std::collections::HashMap<String, i128>>,
    trait_path: &syn::Path,
    module_path: &[String],
    function_path: &[String],
    block_path: &[String],
) -> Option<(Vec<String>, &'a std::collections::HashMap<String, i128>)> {
    for depth in (0..=block_path.len()).rev() {
        let mut combined_module = module_path.to_vec();
        combined_module.extend(function_path.iter().cloned());
        if let Some(prefix) = block_path.get(..depth) {
            combined_module.extend(prefix.iter().cloned());
        }
        if let Some(found) =
            resolve_qualified_trait_defaults(trait_defaults, trait_path, &combined_module)
        {
            return Some(found);
        }
    }
    if function_path.is_empty() && block_path.is_empty() {
        return None;
    }
    resolve_qualified_trait_defaults(trait_defaults, trait_path, module_path)
}

/// Walks a parsed file collecting every [`FoundMatch`], skipping anything declared under
/// `#[cfg(test)]` — an item, an `impl` member, or an inline module's contents — the
/// structural equivalent of `without_test_modules` blanking the same text.
///
/// `scopes` grows and shrinks as the walk enters and leaves a module or a block (a
/// function's body among them): each carries only the constants declared directly in it, so
/// resolving a bare name at any point searches the live stack from the innermost scope
/// outward — the same shadowing a real name lookup gives these declarations, and what stops
/// one module's or function's own constants from being read through another's of the same
/// name. `qualified` is the other half, for a path a bare-name search cannot answer at all:
/// every module's own constants, recorded once under that module's file-root-relative path
/// (`module_path` is the stack of module names currently entered) as the walk resolves each
/// module's scope, so a later `module::P0` reference — from anywhere the walk has since
/// moved on to — still finds it. `module_scope_depths` is `scopes`'s own depth recorded
/// once per entry of `module_path`, right after that module's scope was pushed — what lets
/// `super::P0`, `self::P0` and `crate::P0` truncate the live stack to the ancestor they
/// each name instead of searching all of it (Codex's finding: an unrestricted search can
/// return a *shadowing* constant declared at a level the anchor explicitly steps past).
struct MatchVisitor {
    scopes: ConstScopes,
    /// [`UnsignedConstScopes`]'s own mirror of `scopes`, pushed and popped at the identical
    /// points, from the identical declarations — [`ResolutionContext::scopes_unsigned`]'s
    /// source.
    scopes_unsigned: UnsignedConstScopes,
    /// [`ConstTypeScopes`]'s own mirror of `scopes`, pushed and popped at the identical
    /// points `scopes_unsigned` is — [`ResolutionContext::scopes_types`]'s source.
    scopes_types: ConstTypeScopes,
    use_scopes: UseScopes,
    module_path: Vec<String>,
    module_scope_depths: Vec<usize>,
    /// The stack of enclosing `fn`/method names entered so far — pushed and popped by
    /// `visit_item_fn`/`visit_impl_item_fn` the same way `module_path` is by
    /// `visit_item_mod`, but never mixed into `module_path` or `module_scope_depths`
    /// themselves. Codex's finding: two *different* functions each declaring their own
    /// local `mod indices { .. }` had their constants collide under one identical key
    /// (`crc::indices::P0`), since neither `module_path` nor anything else recorded
    /// which function a local module sat inside — the later-visited function's own
    /// values silently overwrote the earlier one's, so a match inside the *first*
    /// function could read the *second* function's values. This field is what makes the
    /// two distinguishable, appended after `module_path` wherever a local declaration is
    /// indexed or a bare qualified reference is looked up — but never handed to
    /// `resolve_anchored_single_segment`, whose `self`/`super`/`crate` arithmetic is
    /// stated purely in terms of *module* nesting and must not learn that a function
    /// scope is a module boundary, which it is not in real Rust: `super::` inside a
    /// function still names the function's *enclosing module*, not the function itself.
    function_path: Vec<String>,
    /// The stack of blocks entered so far, each named by the order [`Self::visit_block`]
    /// entered it in (`next_block_id`, which only ever climbs) — `function_path`'s own
    /// twin one level finer. Codex's next-round finding: two *sibling blocks in the same
    /// function* — an `if`/`else`'s two arms, or any other nesting `rustc` treats as its
    /// own scope — each declaring their own local `mod indices { .. }` collided exactly
    /// the way two functions did, because `function_path` alone cannot tell two blocks of
    /// one function apart. Appended after `function_path` everywhere that field already
    /// is, and — like `function_path` — never handed to `resolve_anchored_single_segment`,
    /// since a block is not a module either.
    block_path: Vec<String>,
    /// The next id [`Self::visit_block`] will assign — strictly increasing across the
    /// whole walk, so no two blocks, however deeply nested or however far apart in the
    /// file, are ever assigned the same one. Reset between the two passes
    /// [`match_expressions_with_prefix`] makes over one file, so the second pass
    /// reproduces the first pass's own key space rather than drifting past it.
    next_block_id: usize,
    /// The stack of concrete types `Self` currently names, innermost last — pushed and
    /// popped by `visit_item_impl` around the impl's own body, for exactly the impls
    /// whose associated constants it can index in the first place (a plain, single-
    /// segment `Self` type). Codex's finding: `Self::P0`, written inside the very impl
    /// that declares `const P0 = 0;`, named no module `resolve_pattern_path` could ever
    /// find — the concrete type's own name was recorded for `u8::P0`, but nothing
    /// recorded that `Self` currently *meant* `u8` while that impl's body was being
    /// walked, so every such arm read as unresolved.
    self_type_path: Vec<String>,
    /// Every trait's own default associated-constant values seen so far, keyed by the
    /// trait's own name — populated by `visit_item_trait`, consulted by `visit_item_impl`
    /// as the starting point for a trait impl's own scope, before that impl's own
    /// redeclarations (if any) are laid over it. Codex's finding: `impl Indices for u8 {}`
    /// redeclares none of `Indices`'s own constants, all of which already carry a default,
    /// and indexed nothing at all — `u8::P0` named the trait's own default value `0`, but
    /// `impl_const_exprs` only ever read what the impl's own item list redeclared.
    trait_defaults: std::collections::HashMap<String, std::collections::HashMap<String, i128>>,
    qualified: std::collections::HashMap<String, i128>,
    /// [`qualified`]'s own mirror of which of its entries are declared unsigned — the
    /// module-qualified twin of [`Self::scopes_unsigned`], inserted at the identical key
    /// every time `qualified` itself gains one.
    qualified_unsigned: std::collections::HashMap<String, bool>,
    /// [`qualified`]'s own mirror of each entry's declared *type name* — the
    /// module-qualified twin of `scopes_types`, inserted at the identical key every time
    /// `qualified` itself gains one, the same way `qualified_unsigned` already is.
    qualified_types: std::collections::HashMap<String, String>,
    found: Vec<FoundMatch>,
}

impl<'ast> syn::visit::Visit<'ast> for MatchVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if has_cfg_test(item_attrs(item)) {
            return;
        }
        syn::visit::visit_item(self, item);
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        if has_cfg_test(impl_item_attrs(item)) {
            return;
        }
        syn::visit::visit_impl_item(self, item);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        // Pushed and popped the same way `visit_item_mod` maintains `module_path`, so a
        // local module declared in this function's body — and any bare or qualified
        // reference to its constants written inside the same function — is keyed and
        // resolved against a path that names *this* function, distinguishing it from an
        // unrelated function elsewhere that happens to declare a same-named local module.
        self.function_path.push(ident_name(&node.sig.ident));
        // Codex's next-round finding: a parameter's own declared type is what an
        // unsuffixed literal *pattern* matched against it means, the same way a `let`'s
        // own ascription already feeds `scopes_types`/`scopes_unsigned` — pushed here,
        // ahead of `visit_item_fn`'s own recursion into the body, so it is visible for
        // the whole function and pops on the way back out exactly as `function_path` does.
        self.scopes_types.0.push(fn_param_types(&node.sig));
        self.scopes_unsigned.0.push(fn_param_unsigned(&node.sig));
        syn::visit::visit_item_fn(self, node);
        self.scopes_unsigned.0.pop();
        self.scopes_types.0.pop();
        self.function_path.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.function_path.push(ident_name(&node.sig.ident));
        self.scopes_types.0.push(fn_param_types(&node.sig));
        self.scopes_unsigned.0.push(fn_param_unsigned(&node.sig));
        syn::visit::visit_impl_item_fn(self, node);
        self.scopes_unsigned.0.pop();
        self.scopes_types.0.pop();
        self.function_path.pop();
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let Some((_, items)) = &node.content else {
            syn::visit::visit_item_mod(self, node);
            return;
        };
        // Codex's finding: two different functions each declaring their own local
        // `mod indices { .. }` are both real modules named `indices` under the same
        // ancestor `module_path`, so keying solely on `module_path` (below) let the
        // later-visited function's constants silently overwrite the earlier one's under
        // one identical string. `function_path` — empty outside any function body — is
        // spliced in ahead of this module's own freshly pushed name so the two stay
        // distinct, without changing `module_path` itself, which `visit_item_mod`'s own
        // recursive walk and `resolve_ancestor_single_segment`'s ancestor arithmetic still
        // need to see as pure module nesting.
        let mut key_path = self.module_path.clone();
        key_path.extend(self.function_path.iter().cloned());
        key_path.extend(self.block_path.iter().cloned());
        let unsigned_names = item_const_unsigned(items);
        let types_names = item_const_types(items);
        let scope = resolve_scope_consts(
            &OwnConsts {
                exprs: &item_const_exprs(items),
                unsigned: &unsigned_names,
                types: &types_names,
            },
            &OuterScopes {
                values: &self.scopes,
                unsigned: &self.scopes_unsigned,
            },
            &self.qualified,
            &self.qualified_unsigned,
            &self.module_path,
            &self.function_path,
            &self.block_path,
        );
        key_path.push(ident_name(&node.ident));
        self.module_path.push(ident_name(&node.ident));
        for (name, value) in &scope {
            let key = format!("{}::{name}", key_path.join("::"));
            self.qualified.insert(key.clone(), *value);
            // Codex's next-round finding: `path_is_definitely_unsigned` answered `false`
            // for every *qualified* reference, a local module's own constant
            // (`bounds::HI`) included, because `qualified` carried no unsignedness
            // counterpart at all — only `scopes_unsigned` did, for the bare-name case.
            // Inserted at the identical key `qualified` itself just gained, so
            // [`resolve_qualified_unsigned`] can answer the same question `resolve_qualified_path`
            // does, for the same declaration.
            self.qualified_unsigned.insert(
                key.clone(),
                unsigned_names.get(name).copied().unwrap_or(false),
            );
            // Codex's next-round finding: `path_declared_type` answered `None` for every
            // *qualified* reference the same way `path_is_definitely_unsigned` used to,
            // because `qualified` carried no declared-type counterpart either — only
            // `scopes_types` did, for the bare-name case. Inserted at the identical key,
            // from the identical `types_names` map `scopes_types` itself is pushed from
            // below, so [`resolve_qualified_type_at_any_depth`] can answer the same
            // question `resolve_qualified_path` does, for the same declaration.
            if let Some(type_name) = types_names.get(name) {
                self.qualified_types.insert(key, type_name.clone());
            }
        }
        self.scopes.0.push(scope);
        self.scopes_unsigned.0.push(unsigned_names);
        self.scopes_types.0.push(types_names);
        self.use_scopes.0.push(item_use_imports(items));
        self.module_scope_depths.push(self.scopes.0.len());
        syn::visit::visit_item_mod(self, node);
        self.module_scope_depths.pop();
        self.use_scopes.0.pop();
        self.scopes_types.0.pop();
        self.scopes_unsigned.0.pop();
        self.scopes.0.pop();
        self.module_path.pop();
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        // Codex's finding: `impl Indices for u8 {}`, implementing a trait every one of
        // whose constants already carries a default, redeclares none of them — legal
        // Rust — but `visit_item_impl` only ever read an impl's own item list, so a trait
        // impl that overrode nothing indexed nothing. Every default this trait declares is
        // recorded here, once, under the trait's own name, for `visit_item_impl` to start
        // a trait impl's own scope from before the impl's own redeclarations (if any) are
        // laid over it.
        //
        // Codex's next-round finding: keyed by the trait's bare name alone, two different
        // traits named `Indices` in two different modules collided under one key, and the
        // later-visited trait's own defaults silently answered for the earlier one's
        // impls. Keyed by the trait's full scope path instead — `module_path` plus
        // `function_path` plus `block_path`, exactly the way every other declaration this
        // file indexes already is — so `lookup_trait_defaults` can tell the two apart the
        // same way `resolve_pattern_path` already tells two same-named local modules
        // apart.
        if !has_cfg_test(&node.attrs) {
            let mut key_path = self.module_path.clone();
            key_path.extend(self.function_path.iter().cloned());
            key_path.extend(self.block_path.iter().cloned());
            let scope = resolve_scope_consts(
                &OwnConsts {
                    exprs: &trait_const_exprs(&node.items),
                    unsigned: &std::collections::HashMap::new(),
                    types: &std::collections::HashMap::new(),
                },
                &OuterScopes {
                    values: &self.scopes,
                    unsigned: &self.scopes_unsigned,
                },
                &self.qualified,
                &self.qualified_unsigned,
                &self.module_path,
                &self.function_path,
                &self.block_path,
            );
            key_path.push(ident_name(&node.ident));
            self.trait_defaults.insert(key_path.join("::"), scope);
        }
        syn::visit::visit_item_trait(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        // Codex's finding: `Indices::P0` is an associated constant, not a module-qualified
        // one, and nothing here had ever read an `impl` block's own `const` items. Scoped
        // to a plain, single-segment `Self` type: `impl some::Path { .. }` names no known
        // type this way and is left unrecorded rather than guessed at.
        //
        // Codex's forty-sixth-round finding: that scoping excluded a real shape —
        // `impl self::Key { .. }` and `impl defs::Key { .. }` both name a perfectly
        // ordinary type, qualified rather than bare, and every constant they declare was
        // silently dropped. Every reference to those constants this scan can already
        // resolve — a bare `Key::P0` where `Key` is in scope, or a qualified
        // `defs::Key::P0` matched by [`resolve_qualified_path`]'s own last-two-segments
        // fallback — is keyed on the type's own *last* segment, the identical bare name a
        // single-segment `Self` type already indexes under; a qualifying prefix (`self`,
        // `defs`, or anything else) changes nothing about that key. So the length
        // restriction is dropped and the type's own last segment is read instead of its
        // first, which for the single-segment case already in use are the same segment.
        //
        // Codex's next-round finding: `impl<T> Indices<T> { .. }` was excluded by requiring
        // the one segment to carry no generic arguments at all, even though a caller
        // referencing `Indices::<u8>::P0` names the base type the same way — the arguments
        // are on the *caller's* path and are already ignored by every reader of a
        // `syn::Path` here (`ident_name` reads a segment's identifier, never its
        // arguments), so a `Self` type's own generics are no reason to skip its constants.
        //
        // Codex's next finding after that: a *trait* impl's own constants (`impl Indices
        // for u8 { const P0 = 0; .. }`) were skipped outright, on the reasoning that a
        // const the impl does not redeclare could be the trait's own default, which this
        // scan had no notion of. Indexed under the *implementing type's* own name
        // (`u8::P0`) exactly like an inherent impl's, since that is also the unqualified
        // spelling Rust itself accepts when the trait is unambiguous, and it is what
        // [`resolve_qself_associated_const`] looks up for the qualified
        // `<u8 as Indices>::P0` spelling too.
        //
        // Codex's next-round finding is the case the round before it left standing:
        // `impl Indices for u8 {}`, redeclaring *none* of a trait all of whose constants
        // already carry a default, indexed nothing at all, even though `u8::P0` still
        // names the trait's own default value. `self.trait_defaults`, populated by
        // `visit_item_trait`, is consulted here by the trait path's own last segment,
        // seeded as the starting scope for a trait impl before the impl's own
        // redeclarations are layered over it, so an override still wins where one exists.
        //
        // Codex's next-round finding after that: the trait's name alone is *not*
        // unambiguous — two different modules can each declare their own trait named
        // `Indices`, and a bare name lookup could answer from either one's defaults.
        // `lookup_trait_defaults` is `resolve_pattern_path`'s own most-specific-first
        // search, reused rather than reinvented, since this impl's own bare reference to
        // its trait resolves under the identical lexical-scoping rules any other bare
        // name here does.
        // Codex's next-round finding: `Self::P0`, written inside this very impl, named no
        // module `resolve_pattern_path` could ever find — the constant below is indexed
        // under the concrete type's own name, but nothing recorded that `Self` currently
        // *means* that type while this impl's body is being walked. `self_type_path`
        // makes that substitution possible; pushed only when a name was actually indexed
        // above; popped only when it was pushed, so a `Self` reached from *outside* any
        // indexable impl still resolves to nothing rather than to a stale, unrelated one.
        let mut self_type_name = None;
        if let syn::Type::Path(type_path) = node.self_ty.as_ref() {
            if type_path.qself.is_none() {
                if let Some(segment) = type_path.path.segments.last() {
                    let trait_path = node.trait_.as_ref().map(|(_, trait_path, _)| trait_path);
                    let trait_name = trait_path
                        .and_then(|trait_path| trait_path.segments.last())
                        .map(|segment| ident_name(&segment.ident));
                    // Codex's next-round finding: a *qualified* trait reference
                    // (`impl super::traits::Indices for u8 {}`) is tried first, against
                    // the impl's own full written trait path, since it names the trait's
                    // real declaration exactly the way a qualified constant path already
                    // does. Only when that fails — a bare, unqualified trait name, or a
                    // qualified one this scan cannot resolve — does the bare-name,
                    // lexically-scoped search run, exactly as before. Both searches now
                    // return the *canonical scope path* that actually answered, alongside
                    // the defaults themselves, which is what the disambiguating key below
                    // needs.
                    let resolved_trait = trait_path
                        .and_then(|trait_path| {
                            resolve_qualified_trait_defaults_at_any_depth(
                                &self.trait_defaults,
                                trait_path,
                                &self.module_path,
                                &self.function_path,
                                &self.block_path,
                            )
                        })
                        .or_else(|| {
                            trait_name.as_ref().and_then(|trait_name| {
                                lookup_trait_defaults(
                                    &self.trait_defaults,
                                    &self.module_path,
                                    &self.function_path,
                                    &self.block_path,
                                    trait_name,
                                )
                            })
                        });
                    let mut scope = resolved_trait
                        .as_ref()
                        .map(|(_, defaults)| (*defaults).clone())
                        .unwrap_or_default();
                    // Codex's next-round finding after that: a trait this scan *could not
                    // resolve* (an external trait, or one outside the scanned tree) whose
                    // impl still redeclares every constant itself has no canonical scope
                    // path to fall back to — the bare trait name, ambiguous as it is, is
                    // still what the forty-third round's fix relied on for that case, and
                    // dropping it outright would regress the impls that fix already covers.
                    let trait_scope_path: Option<Vec<String>> = resolved_trait
                        .map(|(scope_path, _)| scope_path)
                        .or_else(|| trait_name.as_ref().map(|name| vec![name.clone()]));
                    let mut path = self.module_path.clone();
                    path.extend(self.function_path.iter().cloned());
                    path.extend(self.block_path.iter().cloned());
                    scope.extend(resolve_scope_consts(
                        &OwnConsts {
                            exprs: &impl_const_exprs(&node.items),
                            unsigned: &std::collections::HashMap::new(),
                            types: &std::collections::HashMap::new(),
                        },
                        &OuterScopes {
                            values: &self.scopes,
                            unsigned: &self.scopes_unsigned,
                        },
                        &self.qualified,
                        &self.qualified_unsigned,
                        &self.module_path,
                        &self.function_path,
                        &self.block_path,
                    ));
                    let name = ident_name(&segment.ident);
                    path.push(name.clone());
                    // Codex's forty-third-round finding: two sibling modules each
                    // implementing a *different* trait for the same type (`u8`, say) both
                    // indexed their constants under the identical suffix `u8::P0`, so
                    // `resolve_qself_associated_const`'s suffix search — which exists
                    // precisely because the impl's own module can differ from the trait's —
                    // found two candidates for `<u8 as traits::Dense>::P0` and answered
                    // neither, even though the pattern's own trait path names the impl
                    // unambiguously. A second key, carrying the trait's own scope path ahead
                    // of the type, is inserted alongside the existing one whenever this is a
                    // trait impl, so a suffix search keyed on *both* the trait and the type
                    // finds exactly one candidate again.
                    //
                    // Codex's next-round finding: that scope path was the trait's bare
                    // *last segment* alone, which is exactly the collision this key exists
                    // to close, met one level up — `traits_a::Indices` and
                    // `traits_b::Indices`, each implemented for `u8` in a sibling module,
                    // both inserted a key ending in `Indices::u8::P0`, and the suffix search
                    // found two candidates again. `trait_scope_path` carries the trait's
                    // *full* resolved scope — the same canonical path
                    // `resolve_qualified_trait_defaults`/`lookup_trait_defaults` themselves
                    // found it under — so `traits_a::Indices::u8::P0` and
                    // `traits_b::Indices::u8::P0` no longer share a suffix at all.
                    let mut trait_qualified_path = path.clone();
                    if let Some(trait_scope_path) = &trait_scope_path {
                        let insertion_point = trait_qualified_path.len() - 1;
                        trait_qualified_path.splice(
                            insertion_point..insertion_point,
                            trait_scope_path.iter().cloned(),
                        );
                    }
                    // Codex's forty-sixth-round finding: `impl defs::Key { .. }`, written
                    // outside `defs`, indexes its constants under the *impl's* own lexical
                    // module (`crc::Key::P0` for an impl at this scanned tree's own root,
                    // since `crc` is `INTEGRITY_CHECK_PATH`'s own seeded prefix) — not
                    // under `defs`, which this scan has no reason to believe coincides
                    // with wherever the impl itself happens to be written. A reference
                    // spelled `defs::Key::P0`, matching the self type's own qualified
                    // spelling exactly, does not share that suffix with the impl-module
                    // key at all once the impl's own module is more than empty, so
                    // `resolve_qualified_path`'s bare-chain lookup — an exact match on the
                    // reference's own full path — is what has to find it. A third key,
                    // built from the self type's own segments as written rather than from
                    // the impl's position, is inserted whenever the self type carries more
                    // than the one segment the existing keys already cover.
                    let self_type_segments: Vec<String> = type_path
                        .path
                        .segments
                        .iter()
                        .map(|segment| ident_name(&segment.ident))
                        .collect();
                    for (const_name, value) in &scope {
                        self.qualified
                            .insert(format!("{}::{const_name}", path.join("::")), *value);
                        if trait_scope_path.is_some() {
                            self.qualified.insert(
                                format!("{}::{const_name}", trait_qualified_path.join("::")),
                                *value,
                            );
                        }
                        if self_type_segments.len() > 1 {
                            self.qualified.insert(
                                format!("{}::{const_name}", self_type_segments.join("::")),
                                *value,
                            );
                        }
                    }
                    self_type_name = Some(name);
                }
            }
        }
        if let Some(name) = self_type_name.clone() {
            self.self_type_path.push(name);
        }
        syn::visit::visit_item_impl(self, node);
        if self_type_name.is_some() {
            self.self_type_path.pop();
        }
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        // Codex's finding: `Indices::P0` names a fieldless enum variant exactly the way
        // `Pat::Path` spells a module-qualified constant, and `rustc` compiles a dense
        // match over sequential variant discriminants into the identical indexed table —
        // but nothing here had ever read an `enum`'s own variants, so every arm of such a
        // table read as unresolved. Each unit variant's discriminant is recorded as
        // `EnumName::VariantName`, the same key shape an associated constant gets above,
        // so [`resolve_pattern_path`] finds it by the same route.
        //
        // A variant's own discriminant is either explicit (`Variant = EXPR`) — evaluated
        // through the same constant folding a `const`'s initializer gets, so a discriminant
        // spelled as arithmetic over an earlier constant still resolves — or implicit: one
        // more than the previous variant's own value, `0` for the first. A variant carrying
        // fields is skipped, since `Pat::Path` cannot name one; the running counter still
        // advances past it, matching how `rustc` numbers a mixed enum's fieldless variants.
        // An unresolvable explicit discriminant stops the count rather than guessing a
        // wrong running value for every variant after it.
        if !has_cfg_test(&node.attrs) {
            let qualified_snapshot = self.qualified.clone();
            let ctx = ResolutionContext {
                scopes: &self.scopes,
                use_scopes: &self.use_scopes,
                qualified: &qualified_snapshot,
                module_path: &self.module_path,
                module_scope_depths: &self.module_scope_depths,
                function_path: &self.function_path,
                block_path: &self.block_path,
                self_type_path: &self.self_type_path,
                scopes_unsigned: &self.scopes_unsigned,
                qualified_unsigned: &self.qualified_unsigned,
                scopes_types: &self.scopes_types,
                qualified_types: &self.qualified_types,
            };
            let resolve_value = |path: &syn::Path| resolve_pattern_path(path, &ctx);
            let resolve_unsigned = |path: &syn::Path| path_is_definitely_unsigned(path, &ctx);
            let resolve_width = |path: &syn::Path| path_declared_type(path, &ctx);
            let resolve = Resolve {
                value: &resolve_value,
                unsigned: &resolve_unsigned,
                width: &resolve_width,
            };
            let mut enum_path = self.module_path.clone();
            enum_path.extend(self.function_path.iter().cloned());
            enum_path.extend(self.block_path.iter().cloned());
            enum_path.push(ident_name(&node.ident));
            let mut next: i128 = 0;
            let mut found = Vec::new();
            for variant in &node.variants {
                if has_cfg_test(&variant.attrs) {
                    continue;
                }
                let Some(value) = (match &variant.discriminant {
                    Some((_, expr)) => literal_or_const_value(expr, &resolve),
                    None => Some(next),
                }) else {
                    break;
                };
                if matches!(variant.fields, syn::Fields::Unit) {
                    found.push((
                        format!("{}::{}", enum_path.join("::"), ident_name(&variant.ident)),
                        value,
                    ));
                }
                let Some(successor) = value.checked_add(1) else {
                    break;
                };
                next = successor;
            }
            for (key, value) in found {
                self.qualified.insert(key, value);
            }
        }
        syn::visit::visit_item_enum(self, node);
    }

    fn visit_block(&mut self, node: &'ast syn::Block) {
        let scope = resolve_scope_consts(
            &OwnConsts {
                exprs: &block_const_exprs(node),
                unsigned: &block_const_unsigned(node),
                types: &block_const_types(node),
            },
            &OuterScopes {
                values: &self.scopes,
                unsigned: &self.scopes_unsigned,
            },
            &self.qualified,
            &self.qualified_unsigned,
            &self.module_path,
            &self.function_path,
            &self.block_path,
        );
        self.scopes.0.push(scope);
        self.scopes_unsigned.0.push(block_const_unsigned(node));
        self.scopes_types.0.push(block_const_types(node));
        self.use_scopes.0.push(block_use_imports(node));
        // Codex's finding: two sibling blocks of one function each declaring their own
        // local `mod indices { .. }` collided under `function_path`'s own key exactly the
        // way two sibling functions did, since nothing recorded which *block* a
        // function-local module sat in. Every block entered is given the next never-
        // repeated id and pushed onto `block_path`, popped on the way back out, so a
        // module declared inside one block is keyed and resolved distinctly from one an
        // unrelated sibling block declares under the identical name.
        let block_id = self.next_block_id;
        self.next_block_id = self.next_block_id.saturating_add(1);
        self.block_path.push(format!("{{block {block_id}}}"));
        // Codex's finding, in two rounds: `syn::visit::visit_block`'s default walk
        // descends into every statement unconditionally, so a `#[cfg(test)]`-gated
        // statement — a `let`, a bare expression such as a `match`, or a
        // statement-position macro invocation, all of which `rustc` strips from shipped
        // code — was still walked into and read as production code. Only items and
        // `impl` members were ever checked for `#[cfg(test)]` (`visit_item` and
        // `visit_impl_item`, above); [`stmt_is_cfg_test`] is what now reads a statement's
        // own attributes whichever of the three shapes it is.
        for stmt in &node.stmts {
            if stmt_is_cfg_test(stmt) {
                continue;
            }
            self.visit_stmt(stmt);
        }
        self.block_path.pop();
        self.use_scopes.0.pop();
        self.scopes_types.0.pop();
        self.scopes_unsigned.0.pop();
        self.scopes.0.pop();
    }

    fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
        let ctx = ResolutionContext {
            scopes: &self.scopes,
            use_scopes: &self.use_scopes,
            qualified: &self.qualified,
            module_path: &self.module_path,
            module_scope_depths: &self.module_scope_depths,
            function_path: &self.function_path,
            block_path: &self.block_path,
            self_type_path: &self.self_type_path,
            scopes_unsigned: &self.scopes_unsigned,
            qualified_unsigned: &self.qualified_unsigned,
            scopes_types: &self.scopes_types,
            qualified_types: &self.qualified_types,
        };
        let resolve_value = move |path: &syn::Path| resolve_pattern_path(path, &ctx);
        let resolve_unsigned = move |path: &syn::Path| path_is_definitely_unsigned(path, &ctx);
        let resolve_width = move |path: &syn::Path| path_declared_type(path, &ctx);
        let resolve = Resolve {
            value: &resolve_value,
            unsigned: &resolve_unsigned,
            width: &resolve_width,
        };
        let selector = node.expr.to_token_stream().to_string();
        // Codex's next-round finding: an unsuffixed integer pattern's own unsignedness is
        // not a fact about the pattern alone — `170141183460469231731687303715884105726`
        // with no suffix at all means `u128` only because the scrutinee it is matched
        // against does, the same way any other unsuffixed integer literal in Rust takes
        // its type from context rather than carrying one of its own. Resolved once per
        // match, through the identical `is_definitely_unsigned` every other expression in
        // this scan already goes through, and folded into every arm below rather than
        // asked of the pattern alone.
        let scrutinee_unsigned = is_definitely_unsigned(&node.expr, &resolve);
        // Codex's finding: an individual arm can carry its own `#[cfg(test)]`
        // (`syn::Arm` has its own `attrs`, the same as an item or a statement does),
        // and `rustc` strips such an arm from a production build exactly as it does a
        // gated item — but nothing here had ever read an arm's own attributes, so a
        // table whose numbered arms are all test-only and whose one production arm is
        // an unrelated wildcard was read as though every arm shipped, undercounting
        // what the real, shipped match looks like.
        // Codex's forty-sixth-round finding: `_ if false => 999` immediately before the
        // real wildcard is dead code `rustc` eliminates entirely, but this scan recorded
        // it as an ordinary arm — `is_catchall_pattern` refuses every guarded arm
        // unconditionally, so it was neither the wildcard nor a value-bearing numbered
        // arm, and its `Pat::Wild` pattern resolves to no values at all. `missing_value`
        // requires every arm in its own numbered prefix to have a non-empty pattern, so
        // one dead arm anywhere in that prefix silently disqualified an otherwise dense
        // match. A guard this scan can prove is always `false` — resolving through the
        // identical `i128` pipeline every other constant expression here does, down to
        // exactly `0` — names an arm that never runs, so it is dropped before `FoundArm`
        // is ever built, the same way `rustc`'s own dead-code elimination would drop it.
        // A guard this scan cannot resolve, or that resolves to anything but `0`, is left
        // exactly as before: not provably dead, so not excluded.
        //
        // Codex's next-round finding: the mirror image of a provably dead guard is a
        // provably *true* one — `_ if true => 15` unconditionally matches every input a
        // guard evaluates at all, exactly as an unguarded `_ => 15` would, but
        // `is_catchall_pattern` was still handed `arm.guard.is_some()` unconditionally, so
        // this arm read as an ordinary guarded, non-wild arm rather than the real
        // terminating wildcard `rustc` treats it as — and a genuine trailing `_ => 16`
        // after it, required only for exhaustiveness against a guard `rustc` cannot prove
        // total at compile time, is dead code this scan had no way to discount. A guard
        // this scan resolves to anything but `0` is now treated as absent for exactly the
        // purpose `is_catchall_pattern`'s own `guarded` flag serves: an always-true guard
        // over a wildcard or an unbound, unconstrained binding reads as the real wildcard.
        // The moment that happens, every arm still to come is exactly the dead code a
        // provably-false guard already drops — unreachable, because this one already
        // claims every value — so the scan stops there instead of recording them.
        let mut arms = Vec::new();
        for arm in node.arms.iter().filter(|arm| !has_cfg_test(&arm.attrs)) {
            let guard_value = arm
                .guard
                .as_ref()
                .and_then(|(_, guard_expr)| literal_or_const_value(guard_expr, &resolve));
            if guard_value == Some(0) {
                continue;
            }
            let effectively_guarded = arm.guard.is_some() && guard_value.is_none();
            let is_wild = is_catchall_pattern(&arm.pat, effectively_guarded, &resolve);
            arms.push(FoundArm {
                pattern: pattern_literal(&arm.pat, &resolve, &self.qualified),
                is_wild,
                call: call_shape_of(&arm.body, &resolve),
                unsigned: scrutinee_unsigned || pattern_is_definitely_unsigned(&arm.pat, &resolve),
            });
            if is_wild {
                break;
            }
        }
        self.found.push(FoundMatch { selector, arms });
        syn::visit::visit_expr_match(self, node);
    }

    // Codex's forty-seventh-round finding: this scan only ever looked at `match`
    // expressions, but a hand-written `if x == 0 { .. } else if x == 1 { .. } else { .. }`
    // chain over one consistent scrutinee compiles to the identical indexed table a
    // `match` over the same arms would, and nothing here had ever read an `Expr::If` this
    // way. [`extract_if_chain`] recognises the shape and answers in the identical
    // `FoundMatch` shape [`visit_expr_match`] already produces, so the density check
    // downstream runs over it unchanged — this override's only job is finding the chain
    // and not double-counting it.
    //
    // A recognised chain is *not* also walked by the default visitor: `syn::visit`'s own
    // walk would descend into `node`'s `else_branch`, reaching each `else if` link as its
    // own, separate `Expr::If` node and re-extracting the same chain a second time,
    // shorter by one link each time. Every arm body is still visited by hand instead —
    // `self.visit_block`, once per link — so a match or a nested `if` chain written
    // *inside* one arm's own body is still found. A chain this function does not
    // recognise (no `else`, an inconsistent scrutinee, a condition that is not `scrutinee
    // == literal`) falls through to the ordinary default walk, which is what lets a
    // genuine chain nested inside an unrelated `if`'s own branches still be reached on its
    // own later visit.
    //
    // Codex's forty-eighth-round finding: the manual traversal visited only branch
    // *bodies*, so a condition itself — `consume(match x { 0 => A, .. }) == 0` is still an
    // `Expr::Binary` this function reads as `scrutinee == literal`, but the `match` buried
    // inside `consume(..)`'s argument is a separate, nested dense-match shape of its own —
    // was never handed to `self.visit_expr` and so never reached this visitor at all. Every
    // link's condition is now visited by hand too, the same way every link's body already
    // was, so a construct nested inside a condition is found exactly as one nested inside a
    // body already is.
    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        let ctx = ResolutionContext {
            scopes: &self.scopes,
            use_scopes: &self.use_scopes,
            qualified: &self.qualified,
            module_path: &self.module_path,
            module_scope_depths: &self.module_scope_depths,
            function_path: &self.function_path,
            block_path: &self.block_path,
            self_type_path: &self.self_type_path,
            scopes_unsigned: &self.scopes_unsigned,
            qualified_unsigned: &self.qualified_unsigned,
            scopes_types: &self.scopes_types,
            qualified_types: &self.qualified_types,
        };
        let resolve_value = move |path: &syn::Path| resolve_pattern_path(path, &ctx);
        let resolve_unsigned = move |path: &syn::Path| path_is_definitely_unsigned(path, &ctx);
        let resolve_width = move |path: &syn::Path| path_declared_type(path, &ctx);
        let resolve = Resolve {
            value: &resolve_value,
            unsigned: &resolve_unsigned,
            width: &resolve_width,
        };
        if let Some((selector, arms)) = extract_if_chain(node, &resolve) {
            self.found.push(FoundMatch { selector, arms });
            self.visit_expr(&node.cond);
            self.visit_block(&node.then_branch);
            let mut current = node;
            while let Some((_, else_expr)) = &current.else_branch {
                match else_expr.as_ref() {
                    syn::Expr::If(next_if) => {
                        self.visit_expr(&next_if.cond);
                        self.visit_block(&next_if.then_branch);
                        current = next_if;
                    }
                    syn::Expr::Block(else_block) => {
                        self.visit_block(&else_block.block);
                        break;
                    }
                    _ => break,
                }
            }
            return;
        }
        syn::visit::visit_expr_if(self, node);
    }
}

#[cfg(test)]
mod raw_identifier_tests {
    //! A raw identifier and its plain spelling name the same item (issue #90).
    //! `extern_crate_names` already strips the `r#` marker. This module tests
    //! every other parser in this file against the same rule.
    use super::{
        FnScope, child_modules, declares_test, fn_declaration_count, future_trait_implementors,
        inner_attributes, name_uses, resolved_path_uses, struct_literal_counts, trait_impls,
        use_aliases,
    };

    #[test]
    fn a_raw_trait_name_is_still_the_trait() {
        let impls = trait_impls("impl r#TryFrom<&[u8]> for Foo {}").expect("the fixture parses");
        assert_eq!(impls.len(), 1, "{impls:?}");
        assert_eq!(impls[0].trait_segments, ["TryFrom"]);
    }

    #[test]
    fn a_raw_generic_argument_is_still_the_plain_argument() {
        let impls = trait_impls("impl TryFrom<&[r#u8]> for Foo {}").expect("the fixture parses");
        assert_eq!(impls.len(), 1, "{impls:?}");
        assert_eq!(impls[0].trait_generics, "<&[u8]>");
    }

    #[test]
    fn a_raw_self_type_is_still_the_plain_type() {
        let impls = trait_impls("impl TryFrom<&[u8]> for r#Foo {}").expect("the fixture parses");
        assert_eq!(impls.len(), 1, "{impls:?}");
        assert_eq!(impls[0].self_ty, "Foo");
    }

    #[test]
    fn a_raw_attribute_argument_still_silences_the_lint() {
        let attributes = inner_attributes("#![allow(r#missing_docs)]").expect("the fixture parses");
        assert_eq!(attributes, ["#![allow(missing_docs)]"]);
    }

    #[test]
    fn a_raw_future_implementor_is_still_reported() {
        let implementors = future_trait_implementors("impl core::future::r#Future for Fifth {}")
            .expect("the fixture parses");
        assert_eq!(implementors, ["Fifth"]);
    }

    #[test]
    fn a_raw_use_segment_resolves_to_its_plain_name() {
        let aliases = use_aliases("use r#serde::Deserialize;").expect("the fixture parses");
        assert_eq!(aliases.len(), 1, "{aliases:?}");
        assert_eq!(aliases[0].target, ["serde", "Deserialize"]);
    }

    #[test]
    fn a_raw_use_rename_target_resolves_to_its_plain_name() {
        let aliases = use_aliases("use serde::r#Deserialize as D;").expect("the fixture parses");
        assert_eq!(aliases.len(), 1, "{aliases:?}");
        assert_eq!(aliases[0].local, "D");
        assert_eq!(aliases[0].target, ["serde", "Deserialize"]);
    }

    #[test]
    fn a_raw_path_segment_resolves_to_its_plain_name() {
        let paths =
            resolved_path_uses("fn f() { r#serde::Deserialize; }").expect("the fixture parses");
        assert!(
            paths
                .iter()
                .any(|path| path.segments == ["serde", "Deserialize"]),
            "{paths:?}"
        );
    }

    #[test]
    fn a_raw_fn_name_is_still_counted() {
        assert_eq!(
            fn_declaration_count("fn r#new() {}", "new").expect("the fixture parses"),
            1
        );
    }

    #[test]
    fn a_raw_test_attribute_is_still_a_test() {
        assert!(declares_test("#[r#test]\nfn r#it_works() {}", "it_works"));
    }

    #[test]
    fn a_raw_cfg_test_marker_still_excludes_the_item() {
        // `has_cfg_test` controls the visitor in `trait_impls`. A `#[cfg(r#test)]`
        // module must skip, like a `#[cfg(test)]` module.
        let impls =
            trait_impls("#[cfg(r#test)]\nmod tests {\n    impl TryFrom<&[u8]> for Foo {}\n}\n")
                .expect("the fixture parses");
        assert!(impls.is_empty(), "{impls:?}");
    }

    #[test]
    fn a_raw_inherent_type_name_is_still_matched() {
        // `self_ty_names` must read `impl r#Foo` as "Foo". Else `FnScope::InherentImpls`
        // finds no body to search.
        let counts = struct_literal_counts(
            "impl r#Foo { fn build() -> Foo { Foo {} } }",
            "Foo",
            FnScope::InherentImpls("Foo"),
        )
        .expect("the fixture parses");
        assert_eq!(counts.inside, 1, "{counts:?}");
    }

    #[test]
    fn a_raw_module_name_resolves_to_its_plain_file() {
        let modules = child_modules("src/crc.rs", "mod r#type;").expect("the fixture parses");
        assert_eq!(modules.len(), 1, "{}", modules.len());
        assert_eq!(modules[0].name, "type");
        assert!(
            modules[0]
                .candidates
                .iter()
                .any(|candidate| candidate.ends_with("type.rs")),
            "{:?}",
            modules[0].candidates
        );
    }

    #[test]
    fn a_raw_ident_use_is_still_named() {
        let uses = name_uses("fn f() { let r#alloc = 1; }").expect("the fixture parses");
        assert!(uses.names_word("alloc"), "{uses:?}");
    }
}
