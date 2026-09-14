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

/// Every `crate::NAME` pattern `contents` writes — a bare two-segment path, anchored at
/// the crate root, with no qualified-self half — in source order.
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
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn crate_root_pattern_uses(contents: &str) -> Result<Vec<String>, syn::Error> {
    struct CrateRootPatterns {
        found: Vec<String>,
    }

    impl<'ast> syn::visit::Visit<'ast> for CrateRootPatterns {
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
                    if segments.len() == 2 && segments.first().map(String::as_str) == Some("crate")
                    {
                        self.found.push(segments.join("::"));
                    }
                }
            }
            syn::visit::visit_pat(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = CrateRootPatterns { found: Vec::new() };
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
    let child_dir = if parent_path == "mod.rs" || parent_path.ends_with("/mod.rs") {
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
) -> Result<Vec<FoundMatch>, syn::Error> {
    match_expressions_with_prefix(contents, external_qualified, &[])
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
    prefix: &[String],
) -> Result<Vec<FoundMatch>, syn::Error> {
    let file = parse_rust(contents)?;
    let base = resolve_scope_consts(&item_const_exprs(&file.items), &ConstScopes(Vec::new()));
    let mut visitor = MatchVisitor {
        scopes: ConstScopes(vec![base]),
        use_scopes: UseScopes(vec![item_use_imports(&file.items)]),
        module_path: prefix.to_vec(),
        module_scope_depths: Vec::new(),
        function_path: Vec::new(),
        block_path: Vec::new(),
        next_block_id: 0,
        self_type_path: Vec::new(),
        qualified: external_qualified.clone(),
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
/// Equivalent to [`qualified_constants_with_prefix`] with an empty prefix: for a file that
/// is not itself the out-of-line body of a `mod` declared somewhere else, that is the whole
/// answer, because every constant this function can see either sits at that file's own
/// root — with no module of its own to be qualified under — or inside a `mod { ... }` this
/// file declares inline, which `qualified_constants_with_prefix` already walks into either
/// way.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn qualified_constants(
    contents: &str,
) -> Result<std::collections::HashMap<String, i128>, syn::Error> {
    qualified_constants_with_prefix(contents, &[])
}

/// [`qualified_constants`], with `prefix` seeded as the module path `contents`' own file
/// sits at.
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
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn qualified_constants_with_prefix(
    contents: &str,
    prefix: &[String],
) -> Result<std::collections::HashMap<String, i128>, syn::Error> {
    let file = parse_rust(contents)?;
    let base = resolve_scope_consts(&item_const_exprs(&file.items), &ConstScopes(Vec::new()));
    let mut qualified = std::collections::HashMap::new();
    if !prefix.is_empty() {
        for (name, value) in &base {
            qualified.insert(format!("{}::{name}", prefix.join("::")), *value);
        }
    }
    let mut visitor = MatchVisitor {
        scopes: ConstScopes(vec![base]),
        use_scopes: UseScopes(vec![item_use_imports(&file.items)]),
        module_path: prefix.to_vec(),
        module_scope_depths: Vec::new(),
        function_path: Vec::new(),
        block_path: Vec::new(),
        next_block_id: 0,
        self_type_path: Vec::new(),
        qualified,
        found: Vec::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.qualified)
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
        syn::UseTree::Name(name) => {
            let bound_name = ident_name(&name.ident);
            let mut full = prefix.clone();
            full.push(bound_name.clone());
            scope.named.insert(bound_name, full);
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

/// `own`'s constants, each resolved to an integer where its initializer allows — directly,
/// through a chain of references to other constants `own` itself declares, or through one
/// already visible in `outer`. Declaration order within `own` does not matter: Rust's own
/// name resolution does not require one, so a fixed-point pass (bounded, since a real
/// dependency chain among a handful of constants in one scope is shallow) is what a single
/// top-to-bottom scan would get wrong for a constant that names a later one.
fn resolve_scope_consts(
    own: &std::collections::HashMap<String, syn::Expr>,
    outer: &ConstScopes,
) -> std::collections::HashMap<String, i128> {
    let mut resolved: std::collections::HashMap<String, i128> = std::collections::HashMap::new();
    for _ in 0..own.len().max(1) {
        let mut progressed = false;
        for (name, expr) in own {
            if resolved.contains_key(name) {
                continue;
            }
            // Bare names only: a same-scope or an outer-scope reference, never a qualified
            // path. Qualifying a constant's own initializer with a module path is a far
            // deeper reach than the finding this scope machinery closes, and unsupported
            // here means "not resolved" rather than "resolved wrongly".
            let resolve = |path: &syn::Path| {
                let candidate = ident_name(path.get_ident()?);
                resolved
                    .get(&candidate)
                    .copied()
                    .or_else(|| outer.resolve(&candidate))
            };
            if let Some(value) = literal_or_const_value(expr, &resolve) {
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
fn lit_value(lit: &syn::Lit) -> Option<i128> {
    match lit {
        syn::Lit::Int(int) => int.base10_parse::<i128>().ok(),
        syn::Lit::Byte(byte) => Some(i128::from(byte.value())),
        syn::Lit::Char(char) => Some(i128::from(u32::from(char.value()))),
        _ => None,
    }
}

/// The name of `ty`, if it is a plain, unqualified single-segment type path (`u8`, `i32`,
/// and so on, with no generic arguments) — the shape [`apply_integer_cast`] acts on, and
/// the shape a `<Type as Trait>::NAME` pattern's own `Type` needs to be for
/// [`resolve_qself_associated_const`] to find what it was implemented for.
fn single_segment_type_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    type_path.path.get_ident().map(ident_name)
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
/// Scoped to the ten fixed-width integer types (`u8`..`u128`, `i8`..`i128`): `usize` and
/// `isize` are platform-width, which this scan has no target to measure against, so a
/// cast to either stays unresolved rather than guessed — the same standing a call to a
/// user-defined `const fn` already has here. A destination this scan does not resolve
/// returns `None`, never the operand unchanged, because passing an unevaluated cast
/// through is exactly the bug being fixed.
fn apply_integer_cast(value: i128, ty: &syn::Type) -> Option<i128> {
    let name = single_segment_type_name(ty)?;
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
        return if signed {
            #[allow(
                clippy::cast_possible_wrap,
                reason = "u128 to i128 at equal width is the same reinterpretation, not a value conversion"
            )]
            let reinterpreted = bits as i128;
            Some(reinterpreted)
        } else {
            i128::try_from(bits).ok()
        };
    }
    let masked = bits & ((1_u128 << width) - 1);
    if signed && masked & (1_u128 << (width - 1)) != 0 {
        i128::try_from(masked).ok()?.checked_sub(1_i128 << width)
    } else {
        i128::try_from(masked).ok()
    }
}

/// `expr`'s own integer value: a bare literal, however based or suffixed, seen through a
/// cast, a set of parentheses or a brace group; or a path that `resolve` answers for — the
/// constant-pattern half of both [`FoundArm::pattern`] and a call argument's own value.
fn literal_or_const_value(
    expr: &syn::Expr,
    resolve: &dyn Fn(&syn::Path) -> Option<i128>,
) -> Option<i128> {
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
        syn::Expr::Path(path) => resolve(&path.path),
        // Codex's finding: a `const` initializer that is real, MSRV-legal arithmetic over
        // literals or other constants (`BASE + 1`) is evaluated by `rustc` before the match
        // it feeds ever lowers, and compiles to the identical table a literal would — this
        // was refusing every such initializer as unresolved rather than doing the same
        // constant folding. Checked throughout, so overflow, a shift wider than the value's
        // own bits, or division and remainder by zero each fail closed to `None` rather
        // than wrapping to a value `rustc` itself would have rejected at a different one.
        // A call to a user-defined `const fn` (`index(1)`) is not evaluated — doing that in
        // general means interpreting an arbitrary function body, which this scan does not
        // attempt — so a table whose numbered arms are spelled that way stays unresolved.
        syn::Expr::Binary(binary) => {
            let left = literal_or_const_value(&binary.left, resolve)?;
            let right = literal_or_const_value(&binary.right, resolve)?;
            match binary.op {
                syn::BinOp::Add(_) => left.checked_add(right),
                syn::BinOp::Sub(_) => left.checked_sub(right),
                syn::BinOp::Mul(_) => left.checked_mul(right),
                syn::BinOp::Div(_) => left.checked_div(right),
                syn::BinOp::Rem(_) => left.checked_rem(right),
                syn::BinOp::BitAnd(_) => Some(left & right),
                syn::BinOp::BitOr(_) => Some(left | right),
                syn::BinOp::BitXor(_) => Some(left ^ right),
                syn::BinOp::Shl(_) => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shl(shift)),
                syn::BinOp::Shr(_) => u32::try_from(right)
                    .ok()
                    .and_then(|shift| left.checked_shr(shift)),
                _ => None,
            }
        }
        // Codex's finding: a negative *range endpoint* (`-8..=6`) is a full expression,
        // not the special negative-literal-pattern grammar a bare `-8` pattern parses
        // through — `syn::Pat::Range`'s own `start`/`end` are `Expr`s, so `-8` there is
        // `Expr::Unary(Neg, Expr::Lit(8))` and never reached `lit_value` at all. Checked,
        // like every other fold here: negating `i128::MIN` has no representable positive
        // counterpart and fails closed rather than wrapping.
        syn::Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Neg(_)) => {
            literal_or_const_value(&unary.expr, resolve)?.checked_neg()
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
fn resolve_qself_associated_const(
    qself: &syn::QSelf,
    path: &syn::Path,
    resolve: &dyn Fn(&syn::Path) -> Option<i128>,
) -> Option<i128> {
    let type_name = single_segment_type_name(&qself.ty)?;
    let member = ident_name(&path.segments.last()?.ident);
    let synthetic = syn::parse_str::<syn::Path>(&format!("{type_name}::{member}")).ok()?;
    resolve(&synthetic)
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

fn pattern_literal(pattern: &syn::Pat, resolve: &dyn Fn(&syn::Path) -> Option<i128>) -> Vec<i128> {
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
            Some((_, subpat)) => pattern_literal(subpat, resolve),
            // A bare identifier with no subpattern is the one shape where the mode still
            // matters: `ref`/`mut` are legal only on a genuine new binding, never on
            // Rust's own path-pattern spelling of a constant, so a bare `ref name` can
            // never be the constant `resolve` would otherwise answer for — refused here
            // rather than asked of `resolve`, which could otherwise answer from an
            // unrelated same-named constant it has no business naming.
            None if named.by_ref.is_none() => resolve(&syn::Path::from(named.ident.clone()))
                .into_iter()
                .collect(),
            None => Vec::new(),
        },
        // Codex's finding: `<u8 as Indices>::P0` is a trait-associated constant, and
        // this arm used to resolve `path.path` alone — `Indices::P0`, the *trait's* own
        // path plus the member — as though it were an ordinary module-qualified
        // reference, ignoring `path.qself` (the `<u8 as ..>` half) entirely. A module
        // named `Indices` is never what this scan indexes, so every such arm read as
        // unresolved. [`resolve_qself_associated_const`] is the qualified-self half.
        syn::Pat::Path(path) => path.qself.as_ref().map_or_else(
            || resolve(&path.path).into_iter().collect(),
            |qself| {
                resolve_qself_associated_const(qself, &path.path, resolve)
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
            let inclusive_end = match range.limits {
                syn::RangeLimits::Closed(_) => Some(end),
                syn::RangeLimits::HalfOpen(_) => end.checked_sub(1),
            };
            let Some(inclusive_end) = inclusive_end else {
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
            let count = inclusive_end
                .checked_sub(start)
                .and_then(|span| usize::try_from(span).ok())
                .and_then(|span| span.checked_add(1));
            let Some(count) = count else {
                return Vec::new();
            };
            if start > inclusive_end || count > MAX_RANGE_PATTERN_VALUES {
                return Vec::new();
            }
            (start..=inclusive_end).collect()
        }
        // Codex's finding: a reference pattern (`&0`) is exactly as singleton a value as
        // its own referent, over a scrutinee that is itself a reference — a shape a dense
        // table's own selector can be, and one `rustc` lowers to the identical indexed
        // table a by-value match would.
        syn::Pat::Reference(reference) => pattern_literal(&reference.pat, resolve),
        // Codex's finding: `Some(0)` through `Some(14)` over an `Option<u8>` scrutinee is
        // exactly as dense as its bare-integer twin — `rustc` lowers a single-field
        // tuple-struct constructor pattern to the identical indexed table a plain integer
        // pattern gets — but every such arm is a `Pat::TupleStruct`, which fell to the
        // wildcard `_ => Vec::new()` case below regardless of which constructor it named.
        // Scoped to exactly one field: a multi-field tuple struct has no single value
        // this scan could point a table row at, and is left unresolved rather than
        // guessed at.
        syn::Pat::TupleStruct(tuple_struct) if tuple_struct.elems.len() == 1 => tuple_struct
            .elems
            .first()
            .map_or_else(Vec::new, |elem| pattern_literal(elem, resolve)),
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
                let case_values = pattern_literal(case, resolve);
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
#[must_use]
fn is_catchall_pattern(
    pattern: &syn::Pat,
    guarded: bool,
    resolve: &dyn Fn(&syn::Path) -> Option<i128>,
) -> bool {
    if guarded {
        return false;
    }
    match pattern {
        syn::Pat::Wild(_) => true,
        syn::Pat::Ident(named) if named.by_ref.is_none() && named.subpat.is_none() => {
            resolve(&syn::Path::from(named.ident.clone())).is_none()
        }
        _ => false,
    }
}

/// The callee name and resolved argument value when `expr` is a call with exactly one
/// argument — seen through a set of parentheses or a brace group holding one tail
/// expression, since a block-valued arm (`0 => { helper(0) }`) is exactly as much a call as
/// an unwrapped one once a real parser is reading it.
fn call_shape_of(
    expr: &syn::Expr,
    resolve: &dyn Fn(&syn::Path) -> Option<i128>,
) -> Option<(String, Option<i128>)> {
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
    // Codex's finding: stripping `crate` the same way `self` is stripped, below, loses
    // the one fact that made it worth reading — `crate::indices::P0` names the crate
    // root exclusively, in real Rust, and never the current module, however deep a nested
    // `mod outer` sits. Recorded here, before the shared strip below throws the
    // distinction away, so `crate::` can skip the current-module-relative attempt
    // entirely rather than racing it the way a `self`- or `super`-anchored chain does.
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
    for depth in (0..=ctx.block_path.len()).rev() {
        let mut combined_module = ctx.module_path.to_vec();
        combined_module.extend(ctx.function_path.iter().cloned());
        if let Some(prefix) = ctx.block_path.get(..depth) {
            combined_module.extend(prefix.iter().cloned());
        }
        if let Some(value) = resolve_qualified_path(path, ctx.qualified, &combined_module) {
            return Some(value);
        }
    }
    if ctx.function_path.is_empty() && ctx.block_path.is_empty() {
        return None;
    }
    resolve_qualified_path(path, ctx.qualified, ctx.module_path)
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
    qualified: std::collections::HashMap<String, i128>,
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
        syn::visit::visit_item_fn(self, node);
        self.function_path.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.function_path.push(ident_name(&node.sig.ident));
        syn::visit::visit_impl_item_fn(self, node);
        self.function_path.pop();
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let Some((_, items)) = &node.content else {
            syn::visit::visit_item_mod(self, node);
            return;
        };
        let scope = resolve_scope_consts(&item_const_exprs(items), &self.scopes);
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
        key_path.push(ident_name(&node.ident));
        self.module_path.push(ident_name(&node.ident));
        for (name, value) in &scope {
            self.qualified
                .insert(format!("{}::{name}", key_path.join("::")), *value);
        }
        self.scopes.0.push(scope);
        self.use_scopes.0.push(item_use_imports(items));
        self.module_scope_depths.push(self.scopes.0.len());
        syn::visit::visit_item_mod(self, node);
        self.module_scope_depths.pop();
        self.use_scopes.0.pop();
        self.scopes.0.pop();
        self.module_path.pop();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        // Codex's finding: `Indices::P0` is an associated constant, not a module-qualified
        // one, and nothing here had ever read an `impl` block's own `const` items. Scoped
        // to a plain, single-segment `Self` type: `impl some::Path { .. }` names no known
        // type this way and is left unrecorded rather than guessed at.
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
        // scan has no notion of — true, and still the standing for that one case, but it
        // does not justify skipping the constants a trait impl *does* declare. Indexed
        // under the *implementing type's* own name (`u8::P0`) exactly like an inherent
        // impl's, since that is also the unqualified spelling Rust itself accepts when
        // the trait is unambiguous, and it is what [`resolve_qself_associated_const`]
        // looks up for the qualified `<u8 as Indices>::P0` spelling too.
        // Codex's next-round finding: `Self::P0`, written inside this very impl, named no
        // module `resolve_pattern_path` could ever find — the constant below is indexed
        // under the concrete type's own name, but nothing recorded that `Self` currently
        // *means* that type while this impl's body is being walked. `self_type_path`
        // makes that substitution possible; pushed only when a name was actually indexed
        // above; popped only when it was pushed, so a `Self` reached from *outside* any
        // indexable impl still resolves to nothing rather than to a stale, unrelated one.
        let mut self_type_name = None;
        if let syn::Type::Path(type_path) = node.self_ty.as_ref() {
            if type_path.qself.is_none() && type_path.path.segments.len() == 1 {
                if let Some(segment) = type_path.path.segments.first() {
                    let scope = resolve_scope_consts(&impl_const_exprs(&node.items), &self.scopes);
                    let mut path = self.module_path.clone();
                    path.extend(self.function_path.iter().cloned());
                    path.extend(self.block_path.iter().cloned());
                    let name = ident_name(&segment.ident);
                    path.push(name.clone());
                    for (const_name, value) in &scope {
                        self.qualified
                            .insert(format!("{}::{const_name}", path.join("::")), *value);
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
            };
            let resolve = |path: &syn::Path| resolve_pattern_path(path, &ctx);
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
        let scope = resolve_scope_consts(&block_const_exprs(node), &self.scopes);
        self.scopes.0.push(scope);
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
        };
        let resolve = move |path: &syn::Path| resolve_pattern_path(path, &ctx);
        let selector = node.expr.to_token_stream().to_string();
        let arms = node
            .arms
            .iter()
            .map(|arm| FoundArm {
                pattern: pattern_literal(&arm.pat, &resolve),
                is_wild: is_catchall_pattern(&arm.pat, arm.guard.is_some(), &resolve),
                call: call_shape_of(&arm.body, &resolve),
            })
            .collect();
        self.found.push(FoundMatch { selector, arms });
        syn::visit::visit_expr_match(self, node);
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
