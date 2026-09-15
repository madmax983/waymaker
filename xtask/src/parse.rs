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

/// Whether `attrs` carries exactly `#[cfg(test)]`.
///
/// The textual `without_test_modules` blanked on the substring `#[cfg(test)]`; the
/// structural equivalent matches the attribute: path `cfg` with the single identifier
/// `test` as its argument. `#[cfg(any(test, ...))]` is not the exact spelling and is not
/// skipped — the textual version did not blank on it either.
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Ident>()
                .is_ok_and(|ident| ident == "test")
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

/// Render one attribute exactly as the old line scanner would have seen it.
///
/// The historical checks compared attributes as whitespace-free text
/// (`#![forbid(unsafe_code)]`); rendering the parsed attribute back to that spelling
/// keeps those comparisons meaningful without re-scanning source lines. A string
/// literal's interior keeps its characters but loses insignificant spacing, the same
/// way the old comment stripper treated it.
fn attribute_text(attribute: &syn::Attribute) -> String {
    attribute
        .to_token_stream()
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
            // `unraw()` strips the `r#` prefix: `extern crate r#alloc;` is the same
            // crate as `extern crate alloc;` (issues #68/#90).
            syn::Item::ExternCrate(declaration) => Some(declaration.ident.unraw().to_string()),
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
            prefix.push(path.ident.to_string());
            collect_tree_aliases(&path.tree, prefix, aliases);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            if name.ident != "self" {
                aliases.push(UseAlias {
                    local: name.ident.to_string(),
                    target: [prefix.clone(), vec![name.ident.to_string()]].concat(),
                });
            }
        }
        syn::UseTree::Rename(rename) => {
            aliases.push(UseAlias {
                local: rename.rename.to_string(),
                target: [prefix.clone(), vec![rename.ident.to_string()]].concat(),
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
        .map(|segment| segment.ident.to_string())
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
                                implementors.push(name.ident.to_string());
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
            syn::Item::Fn(function) => usize::from(function.sig.ident == name),
            syn::Item::Mod(module) => module
                .content
                .as_ref()
                .map_or(0, |(_, nested)| count_fn_declarations(nested, name)),
            syn::Item::Trait(trait_item) => trait_item
                .items
                .iter()
                .filter(|member| {
                    matches!(member, syn::TraitItem::Fn(function) if function.sig.ident == name)
                })
                .count(),
            syn::Item::Impl(implementation) => implementation
                .items
                .iter()
                .filter(|member| {
                    matches!(member, syn::ImplItem::Fn(function) if function.sig.ident == name)
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
        syn::Item::Fn(function) => function.sig.ident == name && is_countable_test(&function.attrs),
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
        if path.is_ident("test") {
            tested = true;
        }
        if path.is_ident("ignore") || path.is_ident("cfg") || path.is_ident("cfg_attr") {
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
                        if function.sig.ident == name {
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
            syn::Item::Fn(function) if function.sig.ident == name => {
                blocks.push(&function.block);
            }
            syn::Item::Impl(implementation) => {
                for impl_item in &implementation.items {
                    if has_cfg_test(impl_item_attrs(impl_item)) {
                        continue;
                    }
                    if let syn::ImplItem::Fn(function) = impl_item {
                        if function.sig.ident == name {
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
            .is_some_and(|segment| segment.ident == ty),
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
                .map(|segment| segment.ident.to_string())
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
                            Some(quote::quote!(#erased).to_string())
                        })
                        .collect();
                    format!("<{}>", rendered.join(",")).replace(' ', "")
                }
                syn::PathArguments::None | syn::PathArguments::Parenthesized(_) => String::new(),
            };
            self.found.push(TraitImpl {
                trait_segments,
                trait_generics,
                self_ty: quote::quote!(#node.self_ty).to_string().replace(' ', ""),
            });
            syn::visit::visit_item_impl(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = Impls { found: Vec::new() };
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
            self.idents.push(node.to_string());
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
    /// Keep: `` `the-question-id` `` renders as `` `the-question-id` ``, backticks
    /// and all. Marker claims and table rows match on a backtick-delimited id, so
    /// the delimiters have to survive along with the text.
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
/// An HTML comment is dropped, block or inline, so `contents` may be raw. Real,
/// non-comment HTML is kept verbatim (Codex, pull request #138, round 18): a decision
/// recorded inside `<div>...</div>` is visible to a reader and a renderer alike, and
/// dropping it would fail a build over content that renders fine. A caller may still
/// pre-strip comments for a narrower reason of its own (`check_adr_structure` does, to
/// avoid two fields fusing across a same-line comment); this function does not
/// require it.
#[must_use]
pub fn markdown_prose(contents: &str, inline_code: InlineCode) -> String {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
    use std::fmt::Write as _;

    let parser = Parser::new_ext(contents, Options::empty()).into_offset_iter();
    let mut out = String::new();
    let mut in_fence = false;
    // A quoted example is an example: `> - Status: accepted` shown as a worked case
    // must not read as the real thing, so nothing inside a blockquote is emitted,
    // at any nesting depth — the same treatment a fence already gets.
    let mut blockquote_depth: u32 = 0;
    // Whether each list currently open is ordered, innermost last. `1. Status:
    // accepted` is not `- Status: accepted`: a field or claim scan matches on the
    // `- ` marker, and reconstructing every item with it regardless of list kind
    // would let an ordered-list example stand in for the real bullet.
    let mut ordered_lists: Vec<bool> = Vec::new();
    // Whether a multi-line HTML comment, opened by an earlier `Event::Html` line, is
    // still open (Codex, pull request #138, round 18): real block-level HTML is raw
    // passthrough with no separate `Event::Text` for its content, one `Event::Html` per
    // source line, so a comment's own lines have to be tracked the way a fence's are
    // rather than judged one event at a time. Real, non-comment HTML is kept — a
    // decision recorded inside `<div>...</div>` is still visible to a reader, unlike a
    // comment, and dropping it would fail a build over content that renders fine.
    let mut in_html_comment = false;
    let mut in_cdata = false; // Codex, round 57: carried the same way, see `OpenConstructState`.
    // The non-rendering elements (`<script>`, `<style>`, `<title>`, `<template>` or `<iframe>`)
    // currently open, innermost last — a stack rather than one tag, named rather than a
    // bare flag (Codex, pull request #138, round 30) so a matching close is required: a
    // `<script>` body containing the literal text `</style>` (a JavaScript string, say)
    // does not end HTML parsing of the script, and closing on any of them would
    // resume visibility while a browser is still in script-data state. A real stack
    // since round 33: unlike `<script>`/`<style>`/`<title>`, `<template>` content is
    // parsed as ordinary HTML, so a nested `<template>` genuinely opens a second context
    // that needs its own close first, and a `<script>`, `<style>` or `<title>` can
    // itself nest inside a `<template>` and needs its own level tracked rather than
    // scanned as more `<template>` content.
    let mut open_non_rendering_tag: Vec<String> = Vec::new();
    // A tag whose own closing `>` had not yet appeared when its line ran out
    // (Codex, pull request #138, round 41, finding 1), carried across `Event::Html`
    // lines the same way `in_html_comment` and `open_non_rendering_tag` already are.
    let mut pending_tag: Option<PendingTag> = None;
    // A raw-text element's own close tag name matched but its terminating `>` had not
    // yet appeared (Codex, pull request #138, round 45, "Finish multiline raw-text
    // close tags before popping"), carried the same way `pending_tag` is.
    let mut pending_raw_text_close: Option<PendingRawTextClose> = None;
    // Foreign-content depth for self-closing scripts (Codex, round 49) — see
    // `track_non_rendering_html`'s own doc comment.
    let mut foreign_content: Vec<ForeignFrame> = Vec::new();
    let mut ancestors: Vec<String> = Vec::new(); // Codex, round 56: `track_ordinary_ancestor`.
    for (event, range) in parser {
        // `in_html_comment` as well (Codex, pull request #138, round 20): `pulldown-cmark`
        // ends an `HtmlBlock` at a blank line even when a comment inside it never closed,
        // so ordinary `Text`/`Item`/`Heading` events resume right after — structurally
        // separate from the comment, but still inside it by real HTML rules, until an
        // actual `-->` appears. Without this, a decoy placed after the blank line reads as
        // ordinary visible prose. A non-rendering element outlives its own `HtmlBlock` the
        // same way (Codex, round 30) — `open_non_rendering_tag` folds in here too, or a
        // decision after the blank line inside a still-open `<script>` reads as prose.
        //
        // Kept apart from the container half (Codex, pull request #138, round 21): a
        // comment closing *within* one `Event::Html` or `Event::Text` leaves a visible
        // suffix in that same event — `-->decision-id` — and passing the blanket `hidden`
        // (which is still true from before the close) into the text-scanning helper for
        // that event would suppress the suffix along with the comment. Only fence and
        // blockquote containment says nothing about *this* event's own text, so only that
        // half is passed to the helper; the full `hidden` still gates every event that has
        // no text of its own to scan for a close, such as a heading or item marker.
        let container_hidden = in_fence || blockquote_depth > 0;
        let hidden = container_hidden || in_html_comment || !open_non_rendering_tag.is_empty();
        match event {
            Event::Start(Tag::List(kind)) => ordered_lists.push(kind.is_some()),
            Event::End(TagEnd::List(_)) => drop(ordered_lists.pop()),
            Event::Start(Tag::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_add(1);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_sub(1);
                if blockquote_depth == 0 {
                    // A blockquote is a paragraph break too, for the same reason a
                    // fence is: without it, text on either side fuses into one line.
                    out.push('\n');
                }
            }
            // Only fenced blocks are dropped: the old line scan never removed indented
            // code blocks, and narrowing what counts as code would newly blind the
            // gate to prose it used to read.
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => {
                in_fence = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                if in_fence {
                    in_fence = false;
                    // A fence is a paragraph break; without it the text on either
                    // side fuses into one line the field scans would misread. Only
                    // while a blockquote does not already hide it, or a fence closing
                    // inside a quoted example leaks a blank line into hidden output.
                    if blockquote_depth == 0 {
                        out.push('\n');
                    }
                }
            }
            Event::Code(code) => {
                if !hidden && matches!(inline_code, InlineCode::Keep) {
                    // Backticks kept, not just the content: row and claim scans match
                    // on a backtick-delimited id, and a bare id could be a substring
                    // of a longer one.
                    let _ = write!(out, "`{code}`");
                }
            }
            // Never scanned for a comment marker of either kind, opener or closer
            // (Codex, pull request #138, round 28, correcting rounds 21/22/26): ordinary
            // Markdown text is always HTML-escaped when rendered, so a literal `-->`
            // reaching `Event::Text` never survives as three unescaped bytes in the
            // rendered HTML a browser parses — verified with a throwaway `pulldown-cmark`
            // render, where `--> visible suffix` renders to `--&gt; visible suffix`, and
            // a browser's still-open comment (from an earlier, genuinely unterminated
            // `<!--`) never sees a real close there, so it — and everything after —
            // stays exactly as hidden as if the paragraph had never been there.
            // `Event::Text` therefore can never open *or* close a comment; only
            // `Event::Html` and `Event::InlineHtml` are ever rendered unescaped, and are
            // the only events this module trusts to change `in_html_comment`.
            Event::Text(text) if !hidden => out.push_str(&text),
            Event::Start(Tag::Heading { level, .. }) => {
                if !hidden {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(&"#".repeat(level as usize));
                    out.push(' ');
                }
            }
            // CommonMark allows `-`, `*` or `+` for an unordered item, and
            // normalizing every one of them to `-` would make an example written
            // with a different marker indistinguishable from the real bullet a
            // scan matches on — the item's own range starts at its marker, so
            // `push_list_item_marker` reads the real one back from the source
            // rather than guessing, unless the list is ordered.
            Event::Start(Tag::Item) if !hidden => {
                let ordered = ordered_lists.last().copied().unwrap_or(false);
                let marker = contents[range.start..].chars().next().unwrap_or('-');
                push_list_item_marker(&mut out, ordered, marker);
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item)
            | Event::SoftBreak
            | Event::HardBreak
                if !hidden =>
            {
                out.push('\n');
            }
            // Tracked per line rather than only at the block's own start (Codex, pull
            // request #138, round 29): `<div>\n<script>\n...\n</script>\n</div>` is one
            // `HtmlBlock` whose *nested* `<script>` opens partway through it, on its own
            // `Event::Html` line — classifying only the block's first line missed a
            // non-rendering element that does not open the block itself. A top-level
            // `<script>...</script>` block is simply the case where the opening line is
            // its own first line, so one check handles both. Never reset at the block's
            // own end, matching an HTML comment's own fail-closed handling: a `<script>`
            // whose close is never found hides everything after it the way a browser's
            // script-data parsing state would.
            //
            // The close must name the *same* tag that opened (Codex, round 30): a
            // `<script>` body can contain the literal text `</style>` — a JavaScript
            // string, say — without ending HTML parsing of the script, so closing on
            // any of them would resume visibility too early.
            Event::Html(html) => {
                let spans = visible_html_ranges(
                    &html,
                    &mut OpenConstructState {
                        in_html_comment: &mut in_html_comment,
                        in_cdata: &mut in_cdata,
                    },
                    &mut open_non_rendering_tag,
                    &mut pending_tag,
                    &mut pending_raw_text_close,
                    &mut foreign_content,
                    &mut ancestors,
                );
                if !container_hidden {
                    append_visible_html(&mut out, &html, spans);
                }
            }
            // Never pushes the construct's own raw text (Codex, round 33, finding 1): a
            // reader does not see `<span>`/`</span>` as characters, only the text they
            // wrap — which already reaches `out` on its own, separately, as this
            // element's `Event::Text`. Keeping the tag markup verbatim (as every
            // round-18-through-31 fix here did) was only ever a stand-in for stripping
            // it properly, and it corrupts a line-based reader like `claims_in`: a
            // legitimate `Settles deferred question: <span>\`id\`</span>` was returned
            // with the `<span>` tags still attached, matching no real question id. The
            // tracker call still runs unconditionally, for every construct including a
            // comment, so a non-rendering element's open/close (and, since round 33,
            // finding 3, a comment never being mistaken for either) is still recorded.
            //
            // `<br>` is the one inline tag whose absence *does* change what a reader
            // sees (Codex, pull request #138, round 37, finding 1): it renders as a
            // real line break, so `decision-id head<br>line` shows as two separate
            // lines, `head` and `line`, and joining them with no separator at all fused
            // them into `headline` — a literal substring a `.contains` scan could match
            // even though no reader ever sees those characters run together. A real
            // line break is pushed for it instead, the same as `Event::SoftBreak` and
            // `Event::HardBreak` already get — but only when the tag was not itself
            // swallowed by an open non-rendering element (`!consumed`) and nothing else
            // is already hiding this text, since a `<br>` inside a hidden span renders
            // no break a reader would see either.
            Event::InlineHtml(html) => {
                let consumed = track_non_rendering_html(
                    &html,
                    &mut open_non_rendering_tag,
                    &mut foreign_content,
                    &mut ancestors,
                );
                if !consumed && !hidden && is_line_break_tag(&html) {
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    out
}

/// Pushes one list item's own marker onto `out` — `"1. "` for an ordered item, or
/// `marker` (whatever real bullet character the source used) followed by a space for
/// an unordered one — starting a fresh line first if `out` does not already end on
/// one. Extracted from [`markdown_prose`] only to keep it under clippy's line limit
/// (Codex, pull request #138, round 56, the same reason [`resolve_pending_tag`] and
/// [`track_non_rendering_html`] were pulled out at rounds 45 and 33).
fn push_list_item_marker(out: &mut String, ordered: bool, marker: char) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if ordered {
        // An ordered item renders as `1. ` so it can never be mistaken for the
        // marker a field or claim scan matches on.
        out.push_str("1. ");
    } else {
        out.extend([marker, ' ']);
    }
}

/// Whether `tag` can genuinely nest (Codex, pull request #138, round 31, finding 3;
/// widened round 42, finding 3).
///
/// `<template>` content is parsed as ordinary HTML, so
/// `<template><template>inner</template>more</template>` really does open a second
/// template context, and `more` stays inert until the *outer* close is found. `<script>`,
/// `<style>`, `<title>` and `<iframe>` (round 52, "Suppress iframe fallback content") are
/// raw-text/RCDATA elements: a browser is in raw-text (or RCDATA) parsing mode once one
/// opens, so a further `<script`/`<title`/`<iframe` inside one is literal text — a
/// JavaScript string, a literal document title containing the characters `<title>`
/// (round 30), or an iframe's own legacy fallback content nesting the spelling of
/// another — and never opens a second level. Every other tag nests too, which matters
/// now that the non-rendering stack can hold an *arbitrary* tag name suppressed by its
/// own `hidden` attribute rather than only the fixed ones: a `<div hidden>` still parses
/// its children as ordinary HTML, exactly like `<template>`, and only
/// `<script>`/`<style>`/`<title>`/`<iframe>`'s raw-text parsing is the exception.
fn non_rendering_element_nests(tag: &str) -> bool {
    !matches!(tag, "script" | "style" | "title" | "iframe")
}

/// Whether opening `next_tag` while `top` is already the innermost open non-rendering
/// element implicitly closes `top` rather than nesting inside it — HTML5's per-element
/// list of optional-end-tag rules (Codex, pull request #138, round 48, "Honor implicit
/// closes for optional-end-tag elements"; widened round 50, "Honor implicit closes
/// triggered by different tag names").
///
/// `<li>`'s own end tag may be omitted immediately before another `<li>`, so
/// `<ul><li hidden>hidden<li>All 6 recovery invariants</li></ul>` is not two nested `li`
/// elements sharing one `</li>` — the second `<li>` silently ends the first, the same way
/// a browser's own parser would. Treating it as a further open (as every non-raw-text
/// same-name opener used to be) left the hidden stack open past the visible item's own
/// single close tag, hiding the item itself and everything the document says after it.
///
/// A same-name reopen is not the only trigger (round 50): `<dt>` and `<dd>` close each
/// other, not only themselves, and `<p>` closes before almost any block-starting tag at
/// all, not only another `<p>` — `<p hidden>ignored<div>All 6 recovery
/// invariants</div>` has its `<div>` closing the hidden `<p>` just as surely as a second
/// `<p>` would, and checking only for `top`'s own name back left that hidden past its
/// own implicit close, exposing nothing after it — HTML5's rules per element, not one
/// list shared by all of them, are what `top`'s own match arm names.
///
/// `<p>`'s own arm is the complete list the specification's "in body" insertion mode
/// gives — every start tag whose own clause says "close a p element" before doing
/// anything else (Codex, pull request #138, round 53, "Include every element that
/// implicitly closes a hidden p"; round 50's own list only carried the first, largest
/// group of that spec text and missed nine more scattered through the rest of it, `<dialog>`,
/// `<hgroup>`, `<search>` among them — `<p hidden>ignored<dialog>All 6 recovery
/// invariants</dialog>` was one of the ones round 50 still got wrong).
///
/// `<template>` and an arbitrary `hidden`-suppressed element are deliberately not
/// covered by any arm here (falling through to `false`): both genuinely nest, and a
/// repeated `<template>` (or a `<div hidden>` nested inside another) really does open a
/// second level a browser keeps separately open.
fn implicitly_closed_by(top: &str, next_tag: &str) -> bool {
    match top {
        "li" => next_tag == "li",
        "dt" | "dd" => matches!(next_tag, "dt" | "dd"),
        "option" => matches!(next_tag, "option" | "optgroup"),
        "optgroup" => next_tag == "optgroup",
        "rt" | "rp" => matches!(next_tag, "rt" | "rp"),
        "thead" | "tbody" => matches!(next_tag, "tbody" | "tfoot"),
        "tfoot" => next_tag == "tbody",
        "tr" => matches!(next_tag, "tr" | "tbody" | "thead" | "tfoot"),
        "td" | "th" => matches!(next_tag, "td" | "th" | "tr"),
        "p" => matches!(
            next_tag,
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "center"
                | "dd"
                | "details"
                | "dialog"
                | "dir"
                | "div"
                | "dl"
                | "dt"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "header"
                | "hgroup"
                | "hr"
                | "li"
                | "listing"
                | "main"
                | "menu"
                | "nav"
                | "ol"
                | "p"
                | "plaintext"
                | "pre"
                | "search"
                | "section"
                | "summary"
                | "table"
                | "ul"
        ),
        _ => false,
    }
}

/// The byte range of the first well-formed opening tag for `tag` at or after `from` in
/// `line`, case-insensitively — from `<` through the tag's own unquoted closing `>` (or
/// the end of `line`, if the tag is not closed on this line).
///
/// Matched anywhere in `line`, not only at its start, since a nested element can open
/// partway through an enclosing `HtmlBlock`'s own `Event::Html` line; only where the tag
/// name ends right there rather than continuing into a longer one (`<scriptx>` does not
/// match).
///
/// Tokenized through [`find_any_tag`] rather than searched for `<tag` as a raw substring
/// (Codex, pull request #138, round 39, finding 1, generalizing round 37's fix for a
/// `>` inside this *same* tag's own quoted value to a quoted value inside an
/// *intervening* one): `<span title="</template>">hidden marker</span>` sits between a
/// search's `from` and a real `<template>`'s close, and a raw substring search for
/// `</template` finds that name inside the `<span>`'s own quoted attribute value — text
/// a browser never treats as a tag at all. Walking tag by tag, checking each whole
/// span's own name, skips straight over an unrelated tag's quoted content exactly the
/// way `find_any_tag` already skips over one *quote's* contents, so a spelling trapped
/// inside either can never stand in for a real tag.
fn find_opening_tag(line: &str, from: usize, tag: &str) -> Option<(usize, usize)> {
    let mut cursor = from;
    loop {
        let (start, end) = find_any_tag(line, cursor)?;
        let span = &line[start..end];
        if !span.starts_with("</") && markup_tag_name(span).eq_ignore_ascii_case(tag) {
            return Some((start, end));
        }
        cursor = end;
    }
}

/// The byte range of the first closing tag for `tag` at or after `from` in `line`,
/// case-insensitively — `</tag>` or `</tag >` alike, with any whitespace HTML permits
/// between the tag name and `>` (Codex, pull request #138, round 32, finding 2): an
/// exact `</tag>` match left `open_non_rendering_tag` set forever against a real,
/// legally spelled `</script >` or `</template\t>`, hiding every line after it.
/// Tokenized the same way [`find_opening_tag`] now is, for the same round-39 reason.
fn find_closing_tag(line: &str, from: usize, tag: &str) -> Option<(usize, usize)> {
    let mut cursor = from;
    loop {
        let (start, end) = find_any_tag(line, cursor)?;
        let span = &line[start..end];
        if span.starts_with("</") && markup_tag_name(span).eq_ignore_ascii_case(tag) {
            return Some((start, end));
        }
        cursor = end;
    }
}

/// The byte range of the next raw-text end tag for `tag` (`"script"`, `"style"`,
/// `"title"` or `"iframe"`) at or after `from` in `line` — matched the way an HTML5
/// parser matches one inside raw-text (or RCDATA) content: a literal, case-insensitive
/// `</tag` sequence, wherever it falls, with no regard for anything around it that
/// merely *looks* tag-shaped (Codex, pull request #138, round 40, finding 3; extended to
/// `<title>`, round 48, finding 2; extended to `<iframe>`, round 52, "Suppress iframe
/// fallback content").
///
/// Once a `<script>`, `<style>`, `<title>` or `<iframe>` opens, a browser is not parsing
/// tags or quotes at all — every byte up to the literal closing sequence is opaque text —
/// so `<span title="x</script>y">`, appearing inside one, is not a `<span>` tag whose
/// attribute happens to quote a closer; it is plain text containing the real close.
/// [`find_closing_tag`]'s quote-aware tokenization, built for `<template>`'s genuinely
/// parsed content, misreads that quoted-looking span as an ordinary tag and skips
/// straight over the closer "trapped" inside it, leaving the non-rendering stack open
/// for the rest of the document; this function is what the raw-text branch of
/// [`next_non_rendering_marker`] uses instead.
///
/// The tag name having matched is not the whole answer (Codex, pull request #138,
/// round 45, "Finish multiline raw-text close tags before popping"): a browser keeps
/// consuming the end tag's own markup through to its own `>`, wherever that falls, the
/// same way it does an ordinary tag's — `</script\n data-note="decision-id
/// headline">` really is one close tag split by `pulldown-cmark` into two `Event::Html`
/// lines, not a closed script followed by an ordinary paragraph. [`RawTextClose::Pending`]
/// is what says the name matched but the terminating `>` did not yet appear, so a
/// caller carries the wait into the next line instead of popping early.
fn find_raw_text_closing_tag(line: &str, from: usize, tag: &str) -> Option<(usize, RawTextClose)> {
    let lower = line.to_ascii_lowercase();
    let needle = format!("</{tag}");
    let bytes = line.as_bytes();
    let mut cursor = from;
    loop {
        let start = cursor + lower.get(cursor..)?.find(needle.as_str())?;
        let after_name = start + needle.len();
        let terminates = bytes
            .get(after_name)
            .is_none_or(|&byte| byte.is_ascii_whitespace() || byte == b'/' || byte == b'>');
        if terminates {
            // Quote-aware (Codex, pull request #138, round 46, "Scan raw-text end
            // tags through an unquoted delimiter"): HTML5's tokenizer keeps parsing
            // an end tag's own (bogus, but real) attribute-like text quote-aware
            // exactly like an opening tag's, so `</script data-note=">decision-id
            // headline">` closes at the *second* `>` — the first sits inside the
            // quoted attribute value, and a blind search for it stops early,
            // exposing the rest of that value as ordinary text. The same scan
            // [`find_any_tag`]'s own markup search already uses.
            let mut quote: Option<u8> = None;
            let close = scan_tag_close(line, after_name, &mut quote)
                .map_or(RawTextClose::Pending(quote), RawTextClose::Whole);
            return Some((start, close));
        }
        cursor = start + 1;
    }
}

/// Whether [`find_raw_text_closing_tag`] found the closing tag's own terminating `>`
/// on the same line as its name, or only the name.
enum RawTextClose {
    /// The tag closes at this byte offset, `>` included.
    Whole(usize),
    /// The tag's name matched, but its own `>` was not found before the line ran
    /// out — not resolved yet, carrying the quote state the scan left off in.
    Pending(Option<u8>),
}

/// The earliest opening tag, at or after `from` in `line`, among the fixed non-rendering
/// elements, with the tag name it matched.
///
/// These are the HTML elements whose body a browser never renders as visible text (Codex,
/// pull request #138, rounds 27, 28 and 29 — `<template>`'s content is inert DOM meant
/// for cloning by script, not display; `<title>` added round 48, finding 2 — a document's
/// title is metadata for the browser chrome, never page prose; `<iframe>` added round 52,
/// "Suppress iframe fallback content" — its body is legacy fallback content for a
/// browser that cannot embed the frame at all, never rendered by one that can); every
/// other tag this module keeps verbatim because a reader does see it.
fn find_any_opening_tag(line: &str, from: usize) -> Option<(usize, usize, &'static str)> {
    ["script", "style", "title", "template", "iframe"]
        .into_iter()
        .filter_map(|tag| find_opening_tag(line, from, tag).map(|(start, end)| (start, end, tag)))
        .min_by_key(|&(start, _, _)| start)
}

/// The HTML5 void elements: tags with no content and no closing tag of their own.
/// `hidden` on one of these suppresses nothing beyond the tag's own markup, which
/// [`is_html_block_tag`]'s sibling handling already excludes — there is no body to
/// track a close for, and treating one as an opener would wait forever for a
/// `</...>` no well-formed document ever writes.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Whether `name` is one of [`VOID_ELEMENTS`], case-insensitively.
fn is_void_element(name: &str) -> bool {
    VOID_ELEMENTS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

/// Whether `name` is one of the two HTML5 foreign-content namespace roots — SVG and
/// `MathML` — the only place ordinary HTML still honors a trailing `/` in
/// `<tag ... />` as bodyless, XML-style self-closing syntax (Codex, pull request
/// #138, round 46, "Honor self-closing syntax only in foreign content"). Everywhere
/// else in an HTML document the slash is ignored outright: `<div hidden />` is an
/// ordinary, unclosed opening tag whose element still needs a real `</div>`, exactly
/// as if the slash were not there at all — treating it as bodyless the way
/// [`is_self_closing_tag`] used to, unconditionally, read `<div hidden
/// />decision-id headline</div>` as carrying nothing after the tag's own markup,
/// when the decision is really still inside the (still-open) `hidden` element.
///
/// [`is_self_closing_tag`], built directly on this, is narrower than real foreign
/// content on its own — self-closing honored only for the two root names, not for
/// any element nested inside one — but [`find_any_hidden_opening_tag`]'s own two
/// callers each guard it with the caller-carried `foreign_content` depth
/// (round 49, "Avoid pushing self-closing scripts in foreign content"; round 50,
/// "Skip self-closing hidden elements inside foreign content"), so an arbitrary
/// element like `<g hidden />` nested inside an open `<svg>` is still recognized as
/// bodyless where it matters, without this narrower root-only check needing to know
/// about it itself.
fn is_foreign_content_root(name: &str) -> bool {
    name.eq_ignore_ascii_case("svg") || name.eq_ignore_ascii_case("math")
}

/// Whether `span` — a complete, well-formed opening tag's own markup, ending at its
/// own unquoted `>` — carries a self-closing `/` immediately before that `>`,
/// outside any quoted attribute value (Codex, pull request #138, round 44, finding
/// 2). Whether that slash is *honored* — bodyless XML-style syntax, rather than
/// ignored the way ordinary HTML ignores it everywhere outside foreign content — is
/// the caller's question, answered by [`is_self_closing_tag`] (a fixed tag name) or
/// a caller-carried `foreign_content` depth (an ambient parsing context, tracked by
/// [`track_foreign_content_depth`]) depending on which one needs it.
fn ends_with_self_closing_slash(span: &str) -> bool {
    span.len()
        .checked_sub(2)
        .and_then(|index| span.as_bytes().get(index))
        .is_some_and(|&byte| byte == b'/')
}

/// Whether `span` — a complete, well-formed opening tag's own markup for the element
/// named `name` — is self-closing: [`ends_with_self_closing_slash`], and only for
/// [`is_foreign_content_root`] (round 46): `<svg hidden />` has no body and no
/// `</svg>` a document ever writes, so [`find_any_hidden_opening_tag`] must not wait
/// forever for one, the same way it already never does for a [`VOID_ELEMENTS`]
/// member — but `<div hidden />` is not SVG or `MathML`, and a browser gives its
/// `div` a real, still-open body regardless of the trailing slash.
fn is_self_closing_tag(span: &str, name: &str) -> bool {
    is_foreign_content_root(name) && ends_with_self_closing_slash(span)
}

/// One namespace-changing element currently open, in the order opened — a genuine LIFO
/// stack, closed by a matching name the same way the non-rendering `stack: Vec<String>`
/// already is, rather than a bare depth (Codex, pull request #138, round 51, finding 1:
/// "Exit foreign mode at HTML integration points"). A foreign-content root (`<svg>`,
/// `<math>`) and an HTML integration point opened inside one (`<foreignObject>`,
/// `<desc>`, a `<annotation-xml>` carrying a matching `encoding`, or a `MathML` text
/// integration point) are tracked on the same stack, because an integration point's own
/// descendants are parsed under ordinary HTML rules — a self-closing `/` ignored, the
/// same as anywhere else in an HTML document — even while the enclosing root is still
/// open, and only a further foreign root opened *inside* the integration point
/// re-enters foreign content for its own descendants
/// (`<foreignObject><svg><script /></svg></foreignObject>`). A depth alone cannot tell
/// which state a matching close should restore once nesting like that is possible.
struct ForeignFrame {
    name: String,
    /// Whether HTML5 honors a self-closing `/` on a descendant tag while this frame is
    /// the innermost open one — true for a foreign-content root, false for an HTML
    /// integration point.
    honors_self_closing: bool,
}

/// Whether a self-closing `/` is currently honored — the innermost open [`ForeignFrame`]
/// is a foreign-content root rather than an HTML integration point, or the stack is
/// simply empty (never honored outside foreign content at all).
fn honors_self_closing_now(foreign_content: &[ForeignFrame]) -> bool {
    foreign_content
        .last()
        .is_some_and(|frame| frame.honors_self_closing)
}

/// Whether `name` is one of the HTML5 elements that switches parsing of its own
/// descendants back to ordinary HTML rules while nested inside open foreign content
/// (Codex, pull request #138, round 51, finding 1) — the two SVG integration points
/// whose own name is never one of the fixed non-rendering elements
/// (`<title>`, SVG's third integration point, is already tracked as raw-text RCDATA by
/// [`find_any_opening_tag`], which takes priority before this ever runs), a `MathML`
/// `<annotation-xml>` carrying an `encoding` attribute matching `text/html` or
/// `application/xhtml+xml` case-insensitively (checked against `span`, the tag's own
/// complete markup, rather than trusted from the name alone — any other encoding, or
/// none, leaves its content ordinary `MathML`), and the `MathML` text integration
/// points, which admit HTML content the same way.
fn is_html_integration_point(span: &str, name: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "foreignobject" | "desc" | "mi" | "mo" | "mn" | "ms" | "mtext" => true,
        "annotation-xml" => attribute_value(span, "encoding").is_some_and(|value| {
            value.eq_ignore_ascii_case("text/html")
                || value.eq_ignore_ascii_case("application/xhtml+xml")
        }),
        _ => false,
    }
}

/// The value of `attribute` on a well-formed opening tag's own markup `span`, if it
/// carries one — [`anchor_href`]'s generalization to an arbitrary attribute name, used
/// here only to read `<annotation-xml>`'s `encoding` (Codex, pull request #138, round
/// 51, finding 1). Quote-tracked the same way [`anchor_href`] already is, so a value
/// that merely *contains* `attribute=` inside another attribute's own quoted value is
/// never mistaken for the real one.
fn attribute_value<'a>(span: &'a str, attribute: &str) -> Option<&'a str> {
    let bytes = span.as_bytes();
    let lower = span.to_ascii_lowercase();
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        match quote {
            Some(open) if byte == open => quote = None,
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if lower.as_bytes().get(index..index + attribute.len())
                == Some(attribute.as_bytes())
                && index.checked_sub(1).is_none_or(|before| {
                    bytes.get(before).is_some_and(u8::is_ascii_whitespace)
                }) =>
            {
                let after_name = index + attribute.len();
                if !bytes.get(after_name).is_none_or(|&byte| {
                    byte.is_ascii_whitespace() || matches!(byte, b'=' | b'/' | b'>')
                }) {
                    index = after_name;
                    continue;
                }
                let after_name_whitespace = span
                    .get(after_name..)?
                    .find(|character: char| !character.is_whitespace())
                    .map_or(span.len(), |offset| after_name + offset);
                if bytes.get(after_name_whitespace) != Some(&b'=') {
                    index = after_name;
                    continue;
                }
                let after_equals = after_name_whitespace + 1;
                let value_start = span
                    .get(after_equals..)?
                    .find(|character: char| !character.is_whitespace())
                    .map_or(span.len(), |offset| after_equals + offset);
                let value_quote = *bytes.get(value_start)?;
                if value_quote != b'"' && value_quote != b'\'' {
                    let end = span
                        .get(value_start..)?
                        .find(|character: char| character.is_whitespace() || character == '>')
                        .map_or(span.len(), |offset| value_start + offset);
                    return Some(&span[value_start..end]);
                }
                let value_start = value_start + 1;
                let end = value_start + span.get(value_start..)?.find(value_quote as char)?;
                return Some(&span[value_start..end]);
            }
            Some(_) | None => {}
        }
        index += 1;
    }
    None
}

/// HTML5's fixed list of "foreign content breakout" element names (WHATWG §13.2.6.5,
/// the "Any other start tag" case of the "in foreign content" insertion mode) —
/// reaching one of these as a start tag while genuinely inside foreign content pops
/// back out to ordinary HTML rules rather than opening a foreign-namespace child
/// (Codex, pull request #138, round 57, "Exit foreign mode on HTML breakout tags").
/// `font` is deliberately absent: WHATWG lists it separately, breaking out only when
/// it carries a `color`, `face` or `size` attribute, checked by
/// [`is_foreign_breakout_tag`] rather than by name alone.
const HTML_FOREIGN_BREAKOUT_TAGS: &[&str] = &[
    "b",
    "big",
    "blockquote",
    "body",
    "br",
    "center",
    "code",
    "dd",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "i",
    "img",
    "li",
    "listing",
    "menu",
    "meta",
    "nobr",
    "ol",
    "p",
    "pre",
    "ruby",
    "s",
    "small",
    "span",
    "strong",
    "strike",
    "sub",
    "sup",
    "table",
    "tt",
    "u",
    "ul",
    "var",
];

/// Whether opening the element named `name` — with markup `span` — while genuinely
/// inside foreign content breaks back out to ordinary HTML parsing: [`name`] is one of
/// [`HTML_FOREIGN_BREAKOUT_TAGS`], or it is `font` carrying a `color`, `face` or
/// `size` attribute, the one name WHATWG lists as breaking out conditionally rather
/// than unconditionally.
fn is_foreign_breakout_tag(span: &str, name: &str) -> bool {
    HTML_FOREIGN_BREAKOUT_TAGS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
        || (name.eq_ignore_ascii_case("font")
            && ["color", "face", "size"]
                .iter()
                .any(|attribute| attribute_value(span, attribute).is_some()))
}

/// Updates `foreign_content` — the currently open foreign-content roots and HTML
/// integration points, innermost last — from one already-consumed tag's own markup
/// (Codex, pull request #138, round 49, "Avoid pushing self-closing scripts in
/// foreign content"; carried across lines round 50, "Carry foreign-content depth
/// across raw HTML lines"; widened to a real stack round 51, "Exit foreign mode at
/// HTML integration points"; round 57, "Exit foreign mode on HTML breakout tags").
///
/// A start tag named in [`HTML_FOREIGN_BREAKOUT_TAGS`] (or `font` per
/// [`is_foreign_breakout_tag`]), reached while [`honors_self_closing_now`] is true —
/// genuinely inside a foreign-content root rather than at an HTML integration point —
/// pops every consecutive root frame off the top of the stack and reprocesses as
/// ordinary HTML, per WHATWG's "in foreign content" insertion mode. Reaching this
/// before the self-closing check matters: `<svg><p><script /></svg>` breaks out at
/// `p` regardless of any self-closing slash a breakout tag itself might carry, and
/// once broken out, a later self-closing tag like `<script />` is read under
/// ordinary HTML rules rather than foreign-content ones.
///
/// HTML5 acknowledges the self-closing flag on *any* start tag while the innermost open
/// frame [`honors_self_closing_now`] — a root's own descendants, but not an integration
/// point's — so a self-closing tag of either kind still never opens a frame: `<svg
/// /></svg>` (likewise `<foreignObject/>`) has no body. A close only pops when its name
/// matches the innermost open frame, the same "close only what actually opened"
/// discipline the non-rendering stack already applies — a close for anything else,
/// including one that was never tracked here at all (an ordinary `</title>` at the
/// document's own top level, say — SVG's `<title>` integration point is not on this
/// function's own list, since [`find_any_opening_tag`]'s fixed non-rendering handling
/// already intercepts it first, every place this function is ever reached from), is a
/// silent no-op.
///
/// Every caller that consumes an ordinary tag's markup calls this on it, the same way
/// `open_non_rendering`/`open_non_rendering_tag` is threaded and updated across
/// `Event::Html` lines and `Event::InlineHtml` constructs alike — an `<svg>` opened on
/// one line (or in one construct) and a self-closing child reached on a later one
/// (or in a later construct) are the same running document, not two separate
/// searches with no memory of each other.
fn track_foreign_content_depth(span: &str, foreign_content: &mut Vec<ForeignFrame>) {
    let name = markup_tag_name(span);
    if span.starts_with("</") {
        if foreign_content
            .last()
            .is_some_and(|frame| frame.name.eq_ignore_ascii_case(name))
        {
            foreign_content.pop();
        }
        return;
    }
    if honors_self_closing_now(foreign_content) && is_foreign_breakout_tag(span, name) {
        while foreign_content
            .last()
            .is_some_and(|frame| frame.honors_self_closing)
        {
            foreign_content.pop();
        }
        return;
    }
    if ends_with_self_closing_slash(span) {
        return;
    }
    if is_foreign_content_root(name) {
        foreign_content.push(ForeignFrame {
            name: name.to_ascii_lowercase(),
            honors_self_closing: true,
        });
    } else if is_html_integration_point(span, name) {
        foreign_content.push(ForeignFrame {
            name: name.to_ascii_lowercase(),
            honors_self_closing: false,
        });
    }
}

/// Tracks `ancestors`, the persistent record of ordinary elements known to be
/// genuinely open at the top level — outside any tracked non-rendering/hidden
/// element — as an ordinary tag's own markup is consumed there (Codex, pull request
/// #138, round 56, "Ignore closes that do not match a real ancestor"). This is what
/// lets [`next_non_rendering_marker`]'s own closing-tag arm tell a real ancestor's
/// close (positive evidence: the name is genuinely open, here) apart from a stray,
/// wholly unmatched closing tag or one belonging to something opened *inside* the
/// hidden element it is scanning, neither of which should unwind anything.
///
/// A closing tag truncates the whole stack through its match rather than only
/// popping the top (Codex, round 59, "Truncate ordinary ancestors on matching outer
/// closes"): `<div><span></div><em hidden>ignored</span>decision-id headline</em>`
/// has `</div>` close both `span` and `div` at once, per HTML5's "any other end tag"
/// stack-popping algorithm — checking only `ancestors.last()` left the never-closed
/// `span` recorded forever, so the later, genuinely stray `</span>` (its own
/// ancestor already closed out from under it) matched `ancestors` as if it were
/// still real evidence, wrongly force-closing the hidden `em`.
///
/// An opening tag is pushed unless it is void, or self-closing *and* the innermost
/// open foreign-content frame [`honors_self_closing_now`] (Codex, round 59, "Ignore
/// self-closing slashes on ordinary HTML ancestors"): HTML5 only honors a trailing
/// `/` as bodyless syntax inside foreign content (SVG/MathML) — `<span/>` in
/// ordinary HTML is a real, open `span` with the slash ignored, exactly the same
/// distinction [`is_foreign_content_root`] and [`ends_with_self_closing_slash`]'s
/// other callers already draw, so `<span/><em hidden>ignored</span>All 6 recovery
/// invariants</em>` needs `span` recorded to let its later, real close unwind `em`.
fn track_ordinary_ancestor(
    span: &str,
    foreign_content: &[ForeignFrame],
    ancestors: &mut Vec<String>,
) {
    let name = markup_tag_name(span).to_ascii_lowercase();
    if span.starts_with("</") {
        if ancestors.contains(&name) {
            while ancestors.pop().as_deref() != Some(name.as_str()) {}
        }
    } else {
        let self_closing_in_foreign_content =
            ends_with_self_closing_slash(span) && honors_self_closing_now(foreign_content);
        if !is_void_element(&name) && !self_closing_in_foreign_content {
            ancestors.push(name);
        }
    }
}

/// Whether `span` — a complete, well-formed opening tag's own markup — carries the
/// HTML boolean `hidden` attribute as an attribute *name* (Codex, pull request #138,
/// round 42, finding 3): bare `hidden`, or `hidden=...` with any value, at a position
/// preceded only by whitespace or `/` and followed only by whitespace, `=`, `/`
/// or `>`. Quote-tracked so a *value* that merely spells the word — `<div
/// title="hidden">` — is never mistaken for the attribute itself, the same discipline
/// [`anchor_href`] already applies to `href`.
///
/// The scan starts after the tag's own name, not at byte `0` (Codex, pull request
/// #138, round 47, "Skip the element name when scanning for hidden attributes"): an
/// attribute can never appear before it, so this used to accept `<` as a boundary
/// only to admit the case where the name itself happened to sit at the scan's own
/// start — but that same allowance let the *name* be spelled `hidden` and mistaken
/// for the attribute, `<hidden>visible documentation</hidden>` chief among them,
/// which is an element named `hidden`, not a `hidden` attribute on some other one.
///
/// Both boundary checks read the byte's own [`u8::is_ascii_whitespace`] rather than a
/// hand-picked list of four (Codex, pull request #138, round 54, "Accept form feed as
/// HTML attribute whitespace"): HTML treats U+000C FORM FEED as attribute whitespace
/// too, and the hand-picked list — space, tab, line feed, carriage return — had left it
/// out, so `<span hidden\u{c}>` read as an ordinary, unsuppressed tag whose own name
/// happened to continue past `hidden` rather than a real boolean attribute.
fn has_hidden_attribute(span: &str) -> bool {
    let bytes = span.as_bytes();
    let mut quote: Option<u8> = None;
    let mut index = 1 + markup_tag_name(span).len();
    while let Some(&byte) = bytes.get(index) {
        match quote {
            Some(open) if byte == open => quote = None,
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None => {
                let spelled_here = bytes
                    .get(index..)
                    .and_then(|rest| rest.get(..6))
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(b"hidden"));
                if spelled_here {
                    let before_ok = index
                        .checked_sub(1)
                        .and_then(|before| bytes.get(before))
                        .is_none_or(|&byte| byte == b'/' || byte.is_ascii_whitespace());
                    let after_ok = bytes.get(index + 6).is_none_or(|&byte| {
                        byte.is_ascii_whitespace() || matches!(byte, b'=' | b'/' | b'>')
                    });
                    if before_ok && after_ok {
                        return true;
                    }
                }
            }
            Some(_) => {}
        }
        index += 1;
    }
    false
}

/// The byte range and lowercase name of the earliest opening tag, at or after `from`
/// in `line`, that carries [`has_hidden_attribute`] and is neither a [`VOID_ELEMENTS`]
/// member nor [`is_self_closing_tag`] (Codex, pull request #138, round 42, finding 3;
/// round 44, finding 2 — a self-closing foreign element such as `<svg hidden />` has
/// no body and writes no `</svg>` either, so it must not be tracked as an opener any
/// more than a genuine void element already is not).
///
/// A browser never renders such an element or anything inside it, the same as the
/// three explicitly tracked non-rendering elements — but `hidden` can appear on *any*
/// tag, not a fixed set of names, so this returns the name it actually finds rather
/// than choosing from a short list the way [`find_any_opening_tag`] does. A tag
/// nested *inside* the hidden element that happens to share its exact name is not
/// itself detected as a further open the way a `<template>` reopen is — the
/// non-rendering stack still closes on that inner tag's own close, one narrower limit
/// than the fixed elements get, since only `<template>`'s reopen tracking is
/// built to recognize a specific name rather than any of a short list.
fn find_any_hidden_opening_tag(line: &str, from: usize) -> Option<(usize, usize, String)> {
    let mut cursor = from;
    loop {
        let (start, end) = find_any_tag(line, cursor)?;
        let span = &line[start..end];
        if !span.starts_with("</") {
            let name = markup_tag_name(span);
            if !is_void_element(name)
                && !is_self_closing_tag(span, name)
                && has_hidden_attribute(span)
            {
                return Some((start, end, name.to_ascii_lowercase()));
            }
        }
        cursor = end;
    }
}

/// Whether `line` *itself is* a closing tag for `tag` (`"script"`, `"style"`, `"title"`
/// or `"template"`), case-insensitively.
///
/// Required to start at byte `0` — the whole of `line`, not merely contained somewhere
/// in it (Codex, pull request #138, round 38, finding 1, the closing-side twin of
/// `opens_non_rendering_element`'s own fix, applied preemptively for the same reason):
/// the callers below (`track_non_rendering_html`, `unordered_list_item_value`'s
/// disqualify guard) only ever pass one self-contained `Event::InlineHtml` construct,
/// never a longer line a tag can open or close partway through — that block-level case
/// is what `find_closing_tag`'s own direct callers (`advance_past_non_rendering`) are
/// for, searching from a cursor rather than requiring position `0`.
fn closes_non_rendering_element(line: &str, tag: &str) -> bool {
    find_closing_tag(line, 0, tag).is_some_and(|(start, _)| start == 0)
}

/// The lowercase name of a well-formed opening tag, if `line` *itself is* one,
/// case-insensitively — [`closes_non_rendering_element`]'s opening twin, for a name
/// that is not one of the fixed non-rendering ones (Codex, pull request #138, round 45,
/// "Preserve inline nesting for hidden elements"; generalized round 50, "Honor implicit
/// closes triggered by different tag names", from checking one fixed name at a time to
/// returning whatever name the construct actually opens): the block-level nesting
/// search already reopens an arbitrary `hidden`-suppressed element by its own name
/// (`find_opening_tag(line, cursor, top)`, round 43, finding 1), but
/// `track_non_rendering_html`'s inline twin only ever checked
/// [`opens_non_rendering_element`]'s fixed names, so an ordinary same-named child of a
/// `hidden`-suppressed element reaching *inline* Markdown —
/// `<span hidden><span>x</span>decision-id headline</span>` — was never pushed, and its
/// own close popped the outer element early, exposing text still really inside it. The
/// caller now also uses the returned name to decide whether it is one of a *different*
/// name [`implicitly_closed_by`] lists for `top` — `<dt hidden>x<dd>visible` — which a
/// same-name-only check could never recognize.
fn opens_any_tag(line: &str) -> Option<String> {
    let (start, end) = find_any_tag(line, 0)?;
    (start == 0 && !line[start..end].starts_with("</"))
        .then(|| markup_tag_name(&line[start..end]).to_ascii_lowercase())
}

/// The tag name (`"script"`, `"style"`, `"title"` or `"template"`) of an opening
/// non-rendering tag, if `line` *itself is* one, case-insensitively.
///
/// Required to start at byte `0` — the whole of `line`, not merely contained somewhere
/// in it (Codex, pull request #138, round 38, finding 1): both callers below pass one
/// self-contained `Event::InlineHtml` construct, and a self-contained inline construct
/// can carry a quoted attribute whose *value* merely spells a tag opener —
/// `<span title="<script>">decision-id headline</span>` is real, visible inline
/// formatting a reader sees rendered as `<script>` literal text inside the title
/// tooltip, not a genuine `<script>` element — and searching the whole string for the
/// spelling anywhere, the way the block-level scan legitimately does across a longer
/// line, read the browser-invisible attribute text as a real opener, suppressing the
/// label and everything after it as though a script had truly begun.
fn opens_non_rendering_element(line: &str) -> Option<&'static str> {
    find_any_opening_tag(line, 0).and_then(|(start, _, tag)| (start == 0).then_some(tag))
}

/// The name of an arbitrary element's opening tag, suppressed by its own `hidden`
/// attribute rather than by name, if `line` *itself is* one (Codex, pull request #138,
/// round 43, finding 2) — the inline twin of [`opens_non_rendering_element`], required
/// to start at byte `0` for the same reason.
fn opens_hidden_element(line: &str) -> Option<String> {
    find_any_hidden_opening_tag(line, 0).and_then(|(start, _, name)| (start == 0).then_some(name))
}

/// Opens or closes `stack` from one self-contained `Event::InlineHtml` construct —
/// exactly one tag, since `CommonMark`'s inline HTML grammar matches one open tag, one
/// close tag, or one comment per event, never a run of surrounding text — returning
/// whether the construct was consumed as such. A caller has nothing further to do with a
/// consumed construct: a non-rendering element's own open and close tags are not visible
/// text either, and neither is anything else `pulldown-cmark` matched while the stack was
/// already open, since a real, uncommented `<script>` or `<style>` puts a browser in
/// raw-text parsing mode until its own close (round 30) — no other inline construct is
/// real HTML there, whatever `pulldown-cmark` (blind to that state) parsed it as.
///
/// A comment never opens, closes, or reopens anything, checked before any other case
/// (Codex, round 33, finding 3): a self-contained inline comment — `<!-- <script> -->`
/// — can contain the literal text of an opening *or* closing tag, and reaching for
/// `opens_non_rendering_element`/`closes_non_rendering_element` without first asking
/// whether the construct is itself a comment would read either as real.
///
/// `<template>` nests, and can nest a *different* non-rendering element inside it
/// (round 31, finding 3; round 33, finding 2): unlike the raw-text `<script>`/`<style>`,
/// its content is parsed as ordinary HTML, so `stack` is a real stack rather than one
/// tag and a depth — a further open pushes whatever tag it names (the same tag, for
/// plain nesting, or a different one, for a `<script>` or `<style>` nested inside a
/// `<template>`), and only a close matching the *top* of the stack pops it, the same
/// "close only what actually opened" discipline round 30 already applies one level up.
///
/// `foreign_content` is a second, independent stack — `<svg>`/`<math>` are ordinary,
/// visible elements, never hidden by this alone, but HTML5 acknowledges the
/// self-closing flag on *any* start tag while the innermost open frame
/// [`honors_self_closing_now`] (Codex, pull request #138, round 49, "Avoid pushing
/// self-closing scripts in foreign content"; round 51, "Exit foreign mode at HTML
/// integration points"): `<svg><script /></svg>` never opens a genuinely unclosed
/// `<script>` the way a bare `<script />` does outside one, where the slash is ignored
/// and a real `</script>` is still needed — but `<svg><foreignObject><script
/// /></foreignObject></svg>` does, because an HTML integration point's own descendants
/// are parsed under ordinary HTML rules regardless of the still-open `<svg>` around it.
/// Each self-contained `Event::InlineHtml` construct arrives with no memory of the ones
/// around it, unlike a block-level line this module can re-scan from its own start, so
/// this stack has to be carried the same way `stack` already is, tracked here
/// unconditionally so every caller gets it for free.
///
/// `ancestors` is threaded through the same way `foreign_content` is (Codex, pull
/// request #138, round 58, "Track ordinary ancestors across inline HTML events"):
/// the inline twin of `visible_html_ranges`' own top-level `Markup` handling records
/// an ordinary tag reached while nothing is tracked (the `None` arm below) the same
/// way, and — round 57's own whole-stack ancestor unwind, met here for the nesting
/// arm — a closing tag matching neither `top` nor anything `opens_any_tag` finds is
/// checked against `ancestors` before being left inert: `<span><em
/// hidden>ignored</span>decision-id headline` never gives `em` its own end tag, but
/// a browser still force-closes it the moment its ancestor `span` closes, and
/// `</span>` reaching this function as its own self-contained inline construct
/// matches neither `opens_non_rendering_element` nor `closes_non_rendering_element`
/// nor `opens_any_tag` (all three read "opens", and a close is never an open) — so
/// without this, `stack` stayed at `["em"]` through end of document.
fn track_non_rendering_html(
    html: &str,
    stack: &mut Vec<String>,
    foreign_content: &mut Vec<ForeignFrame>,
    ancestors: &mut Vec<String>,
) -> bool {
    if html.starts_with("<!--") {
        return true;
    }
    track_foreign_content_depth(html, foreign_content);
    let self_closing_in_foreign_content =
        honors_self_closing_now(foreign_content) && ends_with_self_closing_slash(html);
    // Cloned rather than borrowed, for the reason `advance_past_non_rendering` now
    // does the same (Codex, pull request #138, round 42, finding 3): `stack` holds
    // owned names since it can carry an arbitrary `hidden`-suppressed one, not only
    // the fixed `&'static str`s, so a borrow of its top would still be live
    // across the `push`/`pop` calls below.
    match stack.last().cloned() {
        Some(top) if non_rendering_element_nests(&top) => {
            if let Some(tag) = opens_non_rendering_element(html) {
                if !self_closing_in_foreign_content {
                    stack.push(tag.to_owned());
                }
            } else if closes_non_rendering_element(html, &top) {
                stack.pop();
            } else if let Some(next_tag) = opens_any_tag(html) {
                if implicitly_closed_by(&top, &next_tag) {
                    // Codex, pull request #138, round 48, "Honor implicit closes
                    // for optional-end-tag elements", widened round 50, "Honor
                    // implicit closes triggered by different tag names": `top`
                    // does not nest on a reopen of one of the tags HTML5 lists as
                    // implicitly closing it — its own end tag may be omitted
                    // before one, whether or not it shares `top`'s own name — so
                    // this construct ends `top` rather than opening a second one.
                    stack.pop();
                } else if next_tag == top {
                    // Codex, pull request #138, round 45 ("Preserve inline
                    // nesting for hidden elements"): a same-named ordinary child of
                    // the `hidden`-suppressed `top` reopens it, the same way the
                    // block-level scan's `top_reopen` already does.
                    stack.push(top);
                }
            } else if let Some(name) = html
                .starts_with("</")
                .then(|| markup_tag_name(html).to_ascii_lowercase())
                && ancestors.contains(&name)
            {
                // Round 57's "Unwind all elements through a matching ancestor",
                // met here for a self-contained inline construct rather than a
                // byte-by-byte walk: a closing tag matching neither `top` nor
                // anything this construct itself opens, but matching *some*
                // element genuinely known to be open outside it, force-closes
                // everything nested inside — the whole tracked stack, not only
                // `top` — the same "pop everything nested inside a closing
                // ancestor's end tag" a real HTML5 parser does.
                stack.clear();
                while ancestors.pop().as_deref() != Some(name.as_str()) {}
            }
            true
        }
        Some(top) => {
            if closes_non_rendering_element(html, &top) {
                stack.pop();
            }
            true
        }
        // An arbitrary element carrying its own `hidden` attribute is checked after
        // the fixed names, not instead of them (Codex, pull request #138, round
        // 43, finding 2): `find_any_hidden_opening_tag`'s own doc comment already
        // covers a tag that is both (`<template hidden>`), and the fixed check is
        // cheaper to try first. `<span hidden>` reaching `Event::InlineHtml` —
        // `Settles deferred question: <span hidden>\`id\`</span>` — never reached
        // either check before this, so its own `Event::Text` body read as ordinary
        // visible documentation evidence even though no browser ever displays it.
        None => {
            if let Some(tag) = opens_non_rendering_element(html) {
                if self_closing_in_foreign_content {
                    false
                } else {
                    stack.push(tag.to_owned());
                    true
                }
            } else if let Some(name) = opens_hidden_element(html) {
                // Self-closing inside foreign content is not an opener here
                // either (Codex, pull request #138, round 50, "Skip self-closing
                // hidden elements inside foreign content"): `<svg><g
                // hidden /></svg>` has no body and no `</g>` a document ever
                // writes, the same as a self-closing `<script>` or `<style>`
                // just above — the check was only ever applied to the fixed
                // list, leaving an arbitrary `hidden`-suppressed element to wait
                // forever for a close that never comes.
                if self_closing_in_foreign_content {
                    false
                } else {
                    stack.push(name);
                    true
                }
            } else {
                // An ordinary tag, reached while nothing is tracked, updates
                // `ancestors` the same way `visible_html_ranges`' own top-level
                // `Markup` handling does (Codex, pull request #138, round 58,
                // "Track ordinary ancestors across inline HTML events") — without
                // this, an inline `<span>` opened outside any hidden element was
                // invisible to the ancestor-unwind check in the nesting arm above,
                // since only the block-level scan ever recorded one.
                track_ordinary_ancestor(html, foreign_content, ancestors);
                false
            }
        }
    }
}

/// Every `CommonMark` §4.6 "type 6" HTML block tag name, lowercase — the fixed list whose
/// mere presence at a line's start begins a raw HTML block, because a browser always
/// renders each one as its own block rather than running inline with neighboring text.
const HTML_BLOCK_TAG_NAMES: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

/// The tag name an opening or closing tag span (`<div ...>` or `</div>`) names, without
/// the angle brackets, slash or any attributes.
fn markup_tag_name(span: &str) -> &str {
    let rest = span.strip_prefix('<').unwrap_or(span);
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let end = rest
        .find(|character: char| character.is_whitespace() || character == '/' || character == '>')
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Whether `span` — an opening or closing tag's own markup — names one of
/// [`HTML_BLOCK_TAG_NAMES`], or `<pre>`, case-insensitively.
///
/// `<pre>` is `CommonMark` §4.6 type 1, not type 6 — it is ended by its own matching
/// close tag rather than by a blank line, which is why [`HTML_BLOCK_TAG_NAMES`] itself
/// stays exactly the type-6 list its own doc comment claims — but a browser still
/// always starts it on a line of its own (Codex, pull request #138, round 40, finding
/// 4): `<pre>head</pre><pre>line</pre>` renders as two separate blocks, `head` and
/// `line`, not one running word, exactly like the type-6 tags already handled here, and
/// stripping its markup with no separator fused the two into a literal `headline` a
/// `.contains` scan could match.
fn is_html_block_tag(span: &str) -> bool {
    let name = markup_tag_name(span);
    name.eq_ignore_ascii_case("pre")
        || HTML_BLOCK_TAG_NAMES
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

/// One place in a line that stops content from being visible: a comment opener, a
/// non-rendering element's opening tag, or an ordinary tag's own markup.
enum HidingMarker {
    /// The byte offset of a `<!--`.
    Comment(usize),
    /// The byte range and name of one of the fixed non-rendering elements'
    /// opening tag.
    Tag(usize, usize, &'static str),
    /// The byte range and name of an arbitrary element's opening tag, suppressed by
    /// its own `hidden` attribute rather than by name (Codex, pull request #138,
    /// round 42, finding 3).
    Hidden(usize, usize, String),
    /// The byte range of an ordinary tag's own markup — the angle brackets, the name
    /// and any attributes, excluded without changing any tracked state.
    Markup(usize, usize),
    /// A CDATA section inside foreign content (Codex, pull request #138, round 56,
    /// "Preserve CDATA text while parsing foreign content"): `<![CDATA[` and `]]>`
    /// are invisible markup delimiters, exactly like an ordinary tag's own markup,
    /// but the payload between them is genuine character data a browser renders —
    /// unlike a bogus comment (what `<![CDATA[` degrades to *outside* foreign
    /// content, and what round 55 correctly treats it as there), which suppresses
    /// its own content too. Carries the opening delimiter's start, the payload's
    /// start (= the opening delimiter's own end), the payload's end (= the closing
    /// delimiter's own start), and the closing delimiter's end, in that order.
    Cdata(usize, usize, usize, usize),
    /// A CDATA section whose opener was found on this line but whose closing `]]>`
    /// was not (Codex, pull request #138, round 57, "Carry foreign CDATA across
    /// lines") — carries the opening delimiter's start and the payload's start, so
    /// the caller can emit the rest of the line as visible text and carry the open
    /// state into the next line's scan, the same way an unterminated comment
    /// already does via `in_html_comment`.
    CdataOpen(usize, usize),
}

/// The byte range of the next HTML tag — opening or closing, any name — at or after
/// `from` in `line`, from `<` through the next unquoted `>`. A comment opener (`<!--`)
/// is not a tag and is skipped, since comments are tracked separately with their own
/// semantics and can span lines a naive "next `>`" search would close early against.
///
/// A `>` inside a quoted attribute value does not end the tag (Codex, pull request
/// #138, round 36, finding 1): `<div title="ends here>decision-id headline">other</div>`
/// is one tag whose attribute value happens to contain the character, and closing on it
/// exposed the rest of the (still-quoted) attribute text as visible prose — a value no
/// browser ever renders. Byte-scanned rather than searched, tracking whichever quote
/// character (`"` or `'`) is currently open so a `>` inside one is skipped and the
/// matching close quote is what re-arms the search.
fn find_any_tag(line: &str, from: usize) -> Option<(usize, usize)> {
    let start = next_tag_start(line, from)?;
    // A markup declaration (`<!ignored ...>`) or processing instruction (`<?...?>`)
    // is not a real tag at all — HTML5 tokenizes both into "bogus comment state",
    // which tracks no quotes whatsoever and ends at the very first `>` it meets
    // (Codex, pull request #138, round 55, "End bogus comments at the first
    // greater-than sign"): `<!ignored title=">decision-id headline">` closes right
    // after `title="`, not at the matching close-quote's own trailing `>` the
    // quote-aware scan below would find instead — which would swallow the whole
    // quoted-looking remainder, ordinary visible text a reader sees plainly, as
    // though it were still markup. `next_tag_start` never yields this `start` for a
    // real `<!--` comment (intercepted separately, above it), so a `!` or `?`
    // reaching here always means one of these two constructs.
    if matches!(line.as_bytes().get(start + 1), Some(b'!' | b'?')) {
        let offset = line.get(start + 1..)?.find('>')?;
        return Some((start, start + 1 + offset + 1));
    }
    let mut quote: Option<u8> = None;
    let end = scan_tag_close(line, start + 1, &mut quote)?;
    Some((start, end))
}

/// Scans `line` from byte `from` for a tag's own closing, unquoted `>`, continuing
/// whichever quote state `quote` already carries in (`None` for a fresh tag, or
/// whatever a previous line's own scan left off in, for one resumed across a line
/// break). Returns the byte offset just past the `>` if `line` supplies it, updating
/// `quote` either way — shared by [`find_any_tag`]'s own single-line scan and by
/// [`visible_html_ranges`]'s cross-line [`PendingTag`] resolution (Codex, pull request
/// #138, round 42, finding 2), so a tag's quote state is tracked exactly one way
/// rather than by two scans that could drift apart.
fn scan_tag_close(line: &str, from: usize, quote: &mut Option<u8>) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut index = from;
    while let Some(&byte) = bytes.get(index) {
        match *quote {
            Some(open) if byte == open => *quote = None,
            None if byte == b'"' || byte == b'\'' => *quote = Some(byte),
            None if byte == b'>' => return Some(index + 1),
            Some(_) | None => {}
        }
        index += 1;
    }
    None
}

/// The byte offset of the next tag-starting `<` — one that is not a comment opener
/// (`<!--`, tracked separately) — at or after `from` in `line`. Split out of
/// [`find_any_tag`] so a caller can tell "no tag starts here at all" apart from "a tag
/// starts here but this line ran out before its own `>`" (Codex, pull request #138,
/// round 41, finding 1) — the latter is [`find_any_tag`] returning `None` too, and only
/// this function's own `Some` distinguishes the two.
///
/// A `<` only counts here when a syntactically plausible tag *or markup declaration*
/// could follow it — an ASCII letter (an opening tag's own name), `/` (a closing tag),
/// `!` or `?` (Codex, pull request #138, round 47, "Distinguish literal less-than
/// signs from tag starts"; widened round 54, "Exclude markup declarations from visible
/// prose") — not any `<` whatsoever: a browser tokenizes `<` as the start of markup
/// only in those cases, and `2 < 3` is ordinary visible text whose `<` is none of them.
/// `!` not followed by `--` (a real comment, intercepted separately above) or `?` both
/// put an HTML5 tokenizer into "bogus comment state" — `<!ignored decision-id
/// headline>` and `<?processing instruction?>` alike are parsed as an inert comment
/// running to the next `>`, never displayed, even though neither spells a real tag —
/// and a browser-visible `<!DOCTYPE html>` sits in the same family; the fixed-name
/// checks elsewhere in this module never match a name starting `!` or `?`, so such a
/// span falls through to the ordinary tag-markup path and is excised the same way any
/// other tag's own markup already is, with no name it could collide with. Every caller
/// of this function treats its `Some` as "an incomplete tag starts here, carry it
/// across the line break" — reading `2 < 3`'s `<` that way swallowed everything from
/// there to the next unrelated `>` anywhere later in the document as if it were that
/// tag's own markup.
fn next_tag_start(line: &str, from: usize) -> Option<usize> {
    let mut cursor = from;
    loop {
        let start = cursor + line.get(cursor..)?.find('<')?;
        if line[start..].starts_with("<!--") {
            cursor = start + "<!--".len();
            continue;
        }
        let plausible = line
            .as_bytes()
            .get(start + 1)
            .is_some_and(|&byte| byte.is_ascii_alphabetic() || matches!(byte, b'/' | b'!' | b'?'));
        if plausible {
            return Some(start);
        }
        cursor = start + 1;
    }
}

/// The byte offset of the next real comment opener (`<!--`) at or after `from` in
/// `line` — one that begins a genuine construct of its own, never one trapped inside an
/// ordinary tag's quoted attribute value (Codex, pull request #138, round 40, finding
/// 1). `<span title="<!--">hidden</span>` is one complete, well-formed tag whose
/// attribute value happens to spell a comment opener — a browser parses it as ordinary
/// attribute text, never as the start of a comment — and a raw substring search for
/// `<!--` anywhere in the line reads it as a real one regardless, latching a tracked
/// "still inside a comment" flag for the rest of the document once no closing `-->` is
/// ever found for it. Tokenizes past every ordinary tag's own span via [`find_any_tag`],
/// the same way [`find_opening_tag`]/[`find_closing_tag`] already do, so only a `<!--`
/// that truly starts outside any tag's markup is ever returned.
fn find_comment_opener(line: &str, from: usize) -> Option<usize> {
    let mut cursor = from;
    loop {
        let start = cursor + line.get(cursor..)?.find('<')?;
        if line[start..].starts_with("<!--") {
            return Some(start);
        }
        let (_, end) = find_any_tag(line, start)?;
        cursor = end;
    }
}

/// The byte offset just past the next HTML comment closer at or after `from` in
/// `text` — the standard `-->`, or the end-bang parse-error recovery spelling `--!>` a
/// browser's tokenizer also accepts as a genuine close (Codex, pull request #138, round
/// 54, "Recognize the HTML comment end-bang close"): `<!-- note --!>All 6 recovery
/// invariants` closes there, not at whatever `-->` a blind, single-spelling search kept
/// waiting for — leaving the visible suffix on the same line, or every line after it,
/// read as still inside the comment. Whichever spelling starts first wins; the two can
/// never both match at the same position, since `-->`'s third byte is `-` and
/// `--!>`'s is `!`.
fn find_comment_close(text: &str, from: usize) -> Option<usize> {
    let rest = text.get(from..)?;
    let arrow = rest.find("-->").map(|start| (start, 3));
    let bang = rest.find("--!>").map(|start| (start, 4));
    let (start, len) = match (arrow, bang) {
        (Some(a), Some(b)) if b.0 < a.0 => b,
        (Some(a), _) => a,
        (None, Some(b)) => b,
        (None, None) => return None,
    };
    Some(from + start + len)
}

/// The byte offset of the next `<![CDATA[` opener at or after `from` in `line`,
/// case-sensitive as HTML5's tokenizer requires — only that exact spelling switches
/// to CDATA-section handling inside foreign content; any other case reads as an
/// ordinary bogus comment even there (Codex, pull request #138, round 56, "Preserve
/// CDATA text while parsing foreign content").
fn find_cdata_opener(line: &str, from: usize) -> Option<usize> {
    line.get(from..)?
        .find("<![CDATA[")
        .map(|offset| from + offset)
}

/// The earliest of a comment opener (`<!--`), a non-rendering element's opening tag, or
/// any other tag's own markup, at or after `from` in `line`.
///
/// A tag's markup is never visible text (Codex, pull request #138, round 34): a browser
/// renders `<div data-note="All 6 recovery invariants">` as nothing at all, not as those
/// characters, so keeping the whole line verbatim — as every fix from round 18 on did —
/// let an attribute value neither reader nor round-18's own "what a reader sees"
/// justification ever meant to expose satisfy a `.contains()` check. `Tag` (an opening
/// non-rendering tag) is preferred over `Markup` at the same starting position, since a
/// `<script>`/`<style>`/`<template>` open is both — an ordinary tag *and* one that has
/// to change tracked state, which excluding it as plain markup would not do. `Hidden`
/// (an arbitrary element carrying its own `hidden` attribute, round 42, finding 3) is
/// checked after `Tag` — a tie between the two would mean a fixed non-rendering
/// element also carries `hidden`, and its own, more specific handling is what should
/// win — but still ahead of `Markup`, for the same reason `Tag` is.
fn next_hiding_marker(
    line: &str,
    from: usize,
    foreign_content: &[ForeignFrame],
) -> Option<HidingMarker> {
    let mut candidates: Vec<(usize, HidingMarker)> = Vec::new();
    // Tokenized via `find_comment_opener`, not a raw substring search (Codex, pull
    // request #138, round 47, "Ignore comment markers inside pending tag
    // attributes"): a still-incomplete tag's own quoted attribute value can itself
    // contain `<!--` — `<div title="<!--\ncontinued">All 6 recovery
    // invariants</div>` has no complete tag on this line at all — and a raw search
    // read that as a genuine comment opener, latching `in_html_comment` for the
    // rest of the document once no real `-->` is ever found for it. The
    // quote-aware, tag-tokenizing search correctly finds nothing here instead,
    // falling through to the "no marker found" case, which is what carries an
    // incomplete tag like this across the line break as a `PendingTag`.
    if let Some(start) = find_comment_opener(line, from) {
        candidates.push((start, HidingMarker::Comment(start)));
    }
    if let Some((start, end, tag)) = find_any_opening_tag(line, from) {
        // Self-closing inside foreign content is not an opener at all (Codex, pull
        // request #138, round 49, "Avoid pushing self-closing scripts in foreign
        // content"): `<svg><script /></svg>` has no body and no `</script>` a
        // document ever writes, so treating it as one waited forever for a close
        // that hid everything after it to end of document. Omitted here rather
        // than pushed as `Markup` directly, so the ordinary `find_any_tag` search
        // below still finds and strips its markup the normal way. `foreign_content`
        // is the caller's own running depth, not a re-scan of `line` (Codex, round
        // 50, "Carry foreign-content depth across raw HTML lines"): an `<svg>`
        // opened on an earlier `Event::Html` line is invisible to anything that
        // only reads this one.
        let span = &line[start..end];
        if !(ends_with_self_closing_slash(span) && honors_self_closing_now(foreign_content)) {
            candidates.push((start, HidingMarker::Tag(start, end, tag)));
        }
    }
    if let Some((start, end, name)) = find_any_hidden_opening_tag(line, from) {
        // The same self-closing-in-foreign-content exemption as the fixed list just
        // above, applied to an arbitrary `hidden`-suppressed element (Codex, round
        // 50, "Skip self-closing hidden elements inside foreign content"): `<g
        // hidden />` inside an `<svg>` has no body either, whatever name it carries.
        let span = &line[start..end];
        if !(ends_with_self_closing_slash(span) && honors_self_closing_now(foreign_content)) {
            candidates.push((start, HidingMarker::Hidden(start, end, name)));
        }
    }
    // Checked ahead of `Markup` (Codex, pull request #138, round 56): `<![CDATA[`
    // starts a plausible tag by `next_tag_start`'s own rules (the byte after `<` is
    // `!`), so `find_any_tag` below always finds a competing `Markup` candidate at
    // this exact same `start` too — one that (correctly, outside foreign content)
    // treats it as a bogus comment ending at the first `>`, which would also
    // swallow the CDATA payload itself as invisible markup.
    //
    // Only recognized while the innermost open frame genuinely honors foreign
    // parsing rules (Codex, round 57, "Restrict CDATA to active foreign
    // namespaces") — `honors_self_closing_now`, the same test the self-closing
    // exemption already uses, rather than a bare "some foreign frame is open":
    // `<svg><foreignObject><![CDATA[...]]></foreignObject></svg>` has switched
    // back to ordinary HTML parsing inside the integration point, where
    // `<![CDATA[` is once again a bogus comment whose payload a browser never
    // renders, even though `foreign_content` is still non-empty there.
    //
    // If this line does not also supply the closing `]]>`, a `CdataOpen`
    // candidate carries the open state across lines (round 57, "Carry foreign
    // CDATA across lines") the same way an HTML comment already does — falling
    // through to the bogus-comment/`PendingTag` path instead, as an earlier
    // version of this fix did, read every payload line up to the next unrelated
    // `>` as markup, discarding real visible text a multi-line CDATA section
    // legitimately spans.
    if honors_self_closing_now(foreign_content)
        && let Some(start) = find_cdata_opener(line, from)
    {
        let payload_start = start + "<![CDATA[".len();
        match line.get(payload_start..).and_then(|rest| rest.find("]]>")) {
            Some(offset) => {
                let payload_end = payload_start + offset;
                let close_end = payload_end + "]]>".len();
                candidates.push((
                    start,
                    HidingMarker::Cdata(start, payload_start, payload_end, close_end),
                ));
            }
            None => {
                candidates.push((start, HidingMarker::CdataOpen(start, payload_start)));
            }
        }
    }
    if let Some((start, end)) = find_any_tag(line, from) {
        candidates.push((start, HidingMarker::Markup(start, end)));
    }
    candidates.sort_by_key(|&(start, _)| start);
    candidates.into_iter().next().map(|(_, marker)| marker)
}

/// What [`next_non_rendering_marker`] found next while a non-rendering element was open.
enum NonRenderingAdvance {
    /// A nested comment opened — only possible for a nesting element like `<template>`,
    /// whose content is real, parsed HTML (Codex, round 32, finding 1). Not a real open
    /// or close of anything, and content-agnostic: everything here is already hidden
    /// regardless of what the comment contains.
    Comment(usize),
    /// A non-rendering element opened — the same tag, for plain nesting, or a
    /// *different* one, for a `<script>` or `<style>` nested inside a `<template>`
    /// (Codex, round 33, finding 2). Owned rather than `&'static str` since round 42
    /// widened the stack to hold an arbitrary `hidden`-suppressed name too, though
    /// this candidate itself is still only ever one of the fixed names — see
    /// [`find_any_hidden_opening_tag`]'s own doc comment for the one thing that
    /// leaves unfound.
    Open(usize, String),
    /// The innermost open element closed.
    Close(usize),
    /// The innermost open element's raw-text close tag name matched, but its own `>`
    /// was not found before the line ran out (Codex, pull request #138, round 45,
    /// "Finish multiline raw-text close tags before popping") — not resolved yet,
    /// carrying the quote state the scan left off in (round 46).
    PendingClose(Option<u8>),
    /// Nothing relevant to `top` was found anywhere in the rest of `line` — carrying
    /// the cursor the internal walk actually reached, past every complete tag it
    /// already consumed and tracked along the way (Codex, pull request #138, round 51,
    /// finding 2: "Update foreign context while scanning hidden content"). A caller
    /// that fell back to its *own*, un-advanced cursor to look for a trailing
    /// incomplete tag would re-walk that same ground — and, since `foreign_content`
    /// is mutated as the walk passes over it, re-evaluate a self-closing tag's
    /// exemption against namespace state the first, complete pass had already moved
    /// past, exactly the way `<template><svg><script /></svg></template>` did before
    /// this variant existed.
    Exhausted(usize),
}

/// The next thing relevant to the innermost currently-open non-rendering element `top`,
/// at or after `cursor` in `line`.
///
/// For a nesting element (`<template>`, round 31, finding 3), the earliest of a comment
/// opener, a further open of *any* non-rendering element (round 33, finding 2 — a
/// `<script>` or `<style>` can nest inside a `<template>`, and its raw-text body must be
/// tracked as its own level rather than scanned for `</template>`-looking text), or
/// `top`'s own close — a close tag written *inside* a comment inside the template is not
/// a real close (round 32, finding 1): `<template><!-- </template> -->hidden</template>`
/// keeps `hidden` inert until the real, final close. The comment opener is found through
/// [`find_comment_opener`], not a raw substring search (Codex, round 40, finding 1): an
/// ordinary child of the template whose own quoted attribute merely spells `<!--` —
/// `<span title="<!--">hidden</span>` — is real, visible text a browser renders as
/// attribute content, not a comment, and a raw search over the whole remaining line
/// would find it regardless of the tag it sits inside, latching `in_html_comment` for
/// the rest of the document once no real close ever follows.
///
/// For a raw-text element (`<script>`, `<style>`), only its own close, found through
/// [`find_raw_text_closing_tag`] rather than [`find_closing_tag`] (Codex, round 40,
/// finding 3): a browser is not parsing tags at all inside one, so a quoted-looking
/// closer trapped inside what merely *looks* like a nested tag — `<span title="x
/// </script>y">` — really is the close, and the quote-aware tokenizer built for
/// `<template>`'s genuinely parsed content would skip straight over it.
///
/// The "further open" half also checks for a reopen of `top` *by name*, not only
/// among the fixed non-rendering elements (Codex, pull request #138, round 43,
/// finding 1): `<div hidden><div>x</div>decision-id headline</div>` has an ordinary,
/// unsuppressed `<div>` nested inside the `hidden`-tracked outer one, and reading only
/// `find_any_opening_tag` — blind to any name outside the fixed ones — never saw it,
/// so the *inner* `</div>` was read as the outer element's own close, exposing
/// everything after it (still really inside the hidden container) as visible prose.
/// `<template>`'s own reopen was always covered this way already, since it is one of
/// the fixed ones; this closes the same gap for an arbitrary `hidden`-suppressed name.
///
/// A reopen [`implicitly_closed_by`] `top` (Codex, pull request #138, round 48, "Honor
/// implicit closes for optional-end-tag elements"; widened round 50, "Honor implicit
/// closes triggered by different tag names") is not an `Open` at all: `<li>`'s own end
/// tag may be omitted immediately before another `<li>`, and `<dt>`/`<dd>` before either
/// of the pair, so such a reopen silently ends `top` rather than nesting inside it, and
/// the marker this returns for it is a `Close` at the reopening tag's own *start* — not
/// its end, the way an explicit close tag resumes past itself — so the reopening tag's
/// own markup is left for the caller's normal, unhidden processing, the same as any
/// other ordinary tag.
///
/// Walked one tag at a time from `cursor`, rather than combining several independent
/// searches that could each start further ahead (Codex, pull request #138, round 51,
/// finding 2: "Update foreign context while scanning hidden content") — every tag this
/// walk passes over on the way to whatever it finds relevant updates `foreign_content`
/// the same way an ordinary tag's markup does at the top level, so a foreign-content
/// root opened between `cursor` and the returned marker is not invisible to it:
/// `<template><svg><script /></svg></template>` used to jump straight from the
/// template's own opener to the fixed `<script>` open without ever consuming the
/// intervening `<svg>`, leaving `foreign_content` at whatever it already was and
/// wrongly reading the self-closing `<script />` as though no foreign content were open
/// at all — pushed as a genuinely unclosed raw-text element that `</template>` could
/// never pop.
fn next_non_rendering_marker(
    line: &str,
    cursor: usize,
    top: &str,
    foreign_content: &mut Vec<ForeignFrame>,
    ancestors: &mut Vec<String>,
) -> Option<NonRenderingAdvance> {
    if !non_rendering_element_nests(top) {
        // A raw-text element's body is opaque to tag parsing altogether — a browser is
        // not looking for `<svg>` or anything else in there, only for its own literal
        // close — so no foreign-content tracking runs on this path.
        return find_raw_text_closing_tag(line, cursor, top).map(|(_, close)| match close {
            RawTextClose::Whole(end) => NonRenderingAdvance::Close(end),
            RawTextClose::Pending(quote) => NonRenderingAdvance::PendingClose(quote),
        });
    }
    let mut cursor = cursor;
    // Ordinary elements opened *during this walk*, nested inside `top` (Codex, pull
    // request #138, round 55, "Unwind hidden descendants when an ancestor closes") —
    // an ordinary `<div>` an untracked ancestor already opened *before* `top` itself
    // never appears here at all, since consuming its own opening tag happened at the
    // top level, before `top` was ever pushed; only a child opened after `top`, while
    // this very walk is what is scanning past it, is recorded. That is exactly the
    // distinction the closing-tag arm below needs to draw.
    let mut descendants: Vec<String> = Vec::new();
    // `ancestors` is `visible_html_ranges`' (or `hide_non_rendering_in_html_line`'s)
    // own persistent record of which ordinary elements are genuinely still open
    // *outside* `top` — maintained at the top level as their markup is consumed,
    // long before `top` was ever pushed. Round 55 had no such record and treated
    // *any* closing tag matching neither `top` nor a `descendants` entry as proof of
    // an ancestor's close by elimination — but a stray, wholly unmatched closing tag
    // (no `<bogus>` ever opened) or a genuine descendant whose own opening tag was
    // consumed on an *earlier* line (so this call's fresh, empty `descendants` never
    // saw it) is exactly as unmatched by that test, and elimination wrongly closed
    // `top` for those too (Codex, pull request #138, round 56, "Ignore closes that
    // do not match a real ancestor"). Only a name found at the top of `ancestors` —
    // positive evidence of a real, currently-open outer element — now closes `top`;
    // anything else is left inert, matching HTML5's own "any other end tag"
    // algorithm, which ignores a token with no matching open element anywhere on the
    // stack rather than guessing.
    loop {
        let comment = find_comment_opener(line, cursor);
        let Some((start, end)) = find_any_tag(line, cursor) else {
            return Some(comment.map_or(
                NonRenderingAdvance::Exhausted(cursor),
                NonRenderingAdvance::Comment,
            ));
        };
        if let Some(comment_start) = comment
            && comment_start < start
        {
            return Some(NonRenderingAdvance::Comment(comment_start));
        }
        let span = &line[start..end];
        let closing = span.starts_with("</");
        let name = markup_tag_name(span).to_ascii_lowercase();
        if closing {
            if name == top {
                return Some(NonRenderingAdvance::Close(end));
            }
            if descendants.last().is_some_and(|open| *open == name) {
                // A genuine nested child, opened after `top` and closed properly —
                // not relevant to `top`'s own state.
                descendants.pop();
            } else if !descendants.contains(&name) && ancestors.contains(&name) {
                // Matches neither `top` nor anything this walk watched open inside
                // it, but does match *some* element genuinely known to be open
                // *outside* it — not necessarily the nearest one, since a real
                // HTML5 parser searches its whole stack of open elements for a
                // matching end tag, not only its topmost entry (Codex, pull
                // request #138, round 57, "Unwind all elements through a matching
                // ancestor"): `<div><section><span hidden>ignored</div>All 6
                // recovery invariants` has `section` nested between `div` and the
                // hidden `span`, so checking only `ancestors.last()` (`section`)
                // never matched `</div>` at all, leaving `span` latched open
                // through end of document. A real parser pops everything nested
                // inside a closing ancestor's end tag, however many layers deep,
                // which is why every entry through the match — not just the
                // matched one — is popped here, and `top` closes along with them.
                while ancestors.pop().as_deref() != Some(name.as_str()) {}
                return Some(NonRenderingAdvance::Close(end));
            }
            // Otherwise `name` matches nothing this scanner has positive evidence
            // is genuinely open — an out-of-order close inside a genuine nested
            // child, or a wholly unmatched stray tag — and is left inert, the same
            // as HTML5's own tokenizer does for an end tag with no matching open
            // element anywhere on the stack.
        } else if implicitly_closed_by(top, &name) {
            return Some(NonRenderingAdvance::Close(start));
        } else if matches!(
            name.as_str(),
            "script" | "style" | "title" | "template" | "iframe"
        ) || name == top
        {
            // Self-closing while `foreign_content` currently honors it is not a
            // further open either (Codex, pull request #138, round 49; round 51,
            // "Exit foreign mode at HTML integration points" — `foreign_content` has
            // already been carried forward through every tag walked past above, so
            // an HTML integration point opened earlier on this same walk correctly
            // stops this from being exempted too).
            if !(ends_with_self_closing_slash(span) && honors_self_closing_now(foreign_content)) {
                return Some(NonRenderingAdvance::Open(end, name));
            }
        } else if !ends_with_self_closing_slash(span) && !is_void_element(&name) {
            // An ordinary element genuinely nested inside `top`, opened during this
            // same walk — tracked so its own later close (above) is told apart from
            // an untracked ancestor's.
            descendants.push(name);
        }
        // Not relevant to `top` — carry its own namespace effect forward (a no-op for
        // anything that is not a foreign-content root or an HTML integration point)
        // and keep walking.
        track_foreign_content_depth(span, foreign_content);
        cursor = end;
    }
}

/// `foreign_content` and `ancestors` bundled into one parameter, only to keep
/// [`advance_past_non_rendering`] under clippy's parameter-count limit (Codex, pull
/// request #138, round 56) — the two are otherwise independent state, each documented
/// at its own declaration site (`track_foreign_content_depth`,
/// `track_ordinary_ancestor`).
struct NestedHtmlContext<'a> {
    foreign_content: &'a mut Vec<ForeignFrame>,
    ancestors: &'a mut Vec<String>,
}

/// `in_html_comment` and `in_cdata` bundled into one parameter, only to keep
/// [`visible_html_ranges`] (and [`hide_non_rendering_in_html_line`]) under clippy's
/// parameter-count limit (Codex, pull request #138, round 57) — the two are
/// otherwise independent, mutually-exclusive mid-construct flags, each carried
/// across `Event::Html` lines the same way: an unterminated HTML comment or an
/// unterminated foreign-content CDATA section leaves the rest of that line's text
/// visible and resumes the search for its own close at the start of the next line.
struct OpenConstructState<'a> {
    in_html_comment: &'a mut bool,
    in_cdata: &'a mut bool,
}

/// Advances past one close, one nested open, or one nested comment, relative to the
/// innermost element `stack` already carries — returning the new cursor, or `None` when
/// nothing more is found before the end of `line`, meaning the rest of the line stays
/// hidden and `stack` (and `in_html_comment`, if a comment was left open) carry into the
/// next line unchanged.
///
/// `pending_tag` and `pending_raw_text_close` carry the same two kinds of unresolved
/// tag `visible_html_ranges` already tracks at the top level, extended to this nested
/// context (Codex, pull request #138, round 45). A raw-text element's own close tag
/// name matching with no `>` yet on this line (`NonRenderingAdvance::PendingClose`,
/// "Finish multiline raw-text close tags before popping") sets `pending_raw_text_close`
/// rather than popping early. And when nothing at all is found — not even a complete
/// open or close — the remainder is not necessarily plain hidden text the way it would
/// be at the top level: an incomplete same-name reopen or an incomplete close of a
/// *nesting* element (`<template>`, or an arbitrary `hidden`-suppressed one) whose own
/// `>` lands on a later line ("Carry nested multiline tags through hidden blocks") is
/// otherwise invisible to [`next_non_rendering_marker`], which only recognizes a
/// complete tag — so an unclosed tag start found via [`next_tag_start`] is captured the
/// same way the top-level fallback already does, and resolved by the same pending-tag
/// logic in [`visible_html_ranges`], which now also recognizes a resolved name matching
/// the current stack top as a reopen or a real close of it.
fn advance_past_non_rendering(
    line: &str,
    cursor: usize,
    stack: &mut Vec<String>,
    in_html_comment: &mut bool,
    pending_tag: &mut Option<PendingTag>,
    pending_raw_text_close: &mut Option<PendingRawTextClose>,
    context: &mut NestedHtmlContext<'_>,
) -> Option<usize> {
    // Cloned rather than borrowed (Codex, pull request #138, round 42, finding 3):
    // the stack widened from `Vec<&'static str>` to `Vec<String>` so it can hold an
    // arbitrary `hidden`-suppressed name, and a borrow of its last element would
    // still be live across the `stack.push`/`stack.pop` calls below.
    let top = stack.last()?.clone();
    match next_non_rendering_marker(
        line,
        cursor,
        &top,
        context.foreign_content,
        context.ancestors,
    ) {
        Some(NonRenderingAdvance::Comment(start)) => {
            let Some(end) = find_comment_close(line, start) else {
                *in_html_comment = true;
                return None;
            };
            Some(end)
        }
        Some(NonRenderingAdvance::Open(end, tag)) => {
            stack.push(tag);
            Some(end)
        }
        Some(NonRenderingAdvance::Close(end)) => {
            stack.pop();
            Some(end)
        }
        Some(NonRenderingAdvance::PendingClose(quote)) => {
            *pending_raw_text_close = Some(PendingRawTextClose { quote });
            None
        }
        // Resumed from the cursor the walk itself reached, not the one this call
        // started from (Codex, pull request #138, round 51, finding 2): every
        // complete tag up to `end` was already consumed and tracked by
        // `next_non_rendering_marker`'s own walk, so re-deriving it here from
        // `cursor` would re-mutate `foreign_content` for ground already covered.
        // Only a trailing *incomplete* tag — one whose own `>` lands on a later
        // line — can remain at `end`, the same "carry it across the line break"
        // case the top-level scan already handles ("Carry nested multiline tags
        // through hidden blocks", round 45).
        Some(NonRenderingAdvance::Exhausted(end)) => {
            let start = next_tag_start(line, end)?;
            let mut quote: Option<u8> = None;
            scan_tag_close(line, start + 1, &mut quote);
            let closing = line[start..].starts_with("</");
            let name = markup_tag_name(&line[start..]).to_ascii_lowercase();
            *pending_tag = Some(PendingTag {
                name,
                closing,
                quote,
                text: line[start..].to_owned(),
            });
            None
        }
        // Reached only for a raw-text element (`<script>`, `<style>`, `<title>`),
        // whose opaque body needs no incomplete-tag capture at all — a split
        // *close* tag is `PendingClose` instead, handled above.
        None => None,
    }
}

/// An HTML tag whose opening `<` or `</` was seen but whose own closing, unquoted `>`
/// had not yet appeared when the line it started on ran out (Codex, pull request #138,
/// round 41, finding 1): `<script\n type="text/javascript">` is one legal tag split by
/// `pulldown-cmark` into two `Event::Html` lines, `"<script\n"` and `"
/// type=\"text/javascript\">\n"`, and every scan in this module works one line at a
/// time — so the first line's search for the tag's close found nothing, and (before
/// this) the whole unclosed remainder read as ordinary visible text, with the second
/// line never even recognized as a continuation of anything, let alone as the very
/// tag that should have opened the non-rendering stack. Carried the way
/// `in_html_comment` and the non-rendering stack already are.
struct PendingTag {
    /// The tag's own name, already fully known from the partial text this line held —
    /// an HTML5 tag name state never survives a line break (the same whitespace that
    /// ends this line's text is what a browser would use to end the name too), so
    /// nothing a later line contributes can extend it.
    name: String,
    /// Whether this is a closing tag (`</name`), which never opens anything.
    closing: bool,
    /// The quote character (`"` or `'`) still open where this line's own scan left
    /// off, if any — an attribute value can itself carry a tag's closing search past
    /// more than one line.
    quote: Option<u8>,
    /// The tag's own raw text seen so far, across every line read while it stayed
    /// unresolved (Codex, pull request #138, round 43, finding 3): whether it opens
    /// an arbitrary `hidden`-suppressed element cannot be decided from its name
    /// alone the way opening one of the fixed elements can, since the
    /// attribute itself may sit on a line after the one the tag started on —
    /// `<div\n hidden>decision-id headline</div>` — and [`has_hidden_attribute`]
    /// needs the tag's complete markup to answer that.
    text: String,
}

/// A raw-text element's own end tag, whose name matched but whose own terminating
/// `>` had not yet appeared when the line it started on ran out (Codex, pull
/// request #138, round 46, "Scan raw-text end tags through an unquoted delimiter") —
/// [`PendingTag`]'s twin for [`find_raw_text_closing_tag`]'s own close search, which
/// scans quote-aware exactly like an ordinary tag's markup does, so the quote state
/// its scan left off in has to carry into the next line the same way.
struct PendingRawTextClose {
    /// The quote character (`"` or `'`) still open where this line's own scan left
    /// off, if any.
    quote: Option<u8>,
}

/// The visible byte ranges of one `Event::Html` line — real block-level HTML
/// passthrough, one source line per event — with HTML comments and the content of
/// non-rendering elements (`<script>`, `<style>`, `<title>`, `<template>` or `<iframe>`) excluded. Carries
/// `in_html_comment`, the open non-rendering element and a tag still awaiting its own
/// close across calls the way each already has to be: a multi-line comment, a
/// `<script>` that outlives its own `HtmlBlock` (rounds 20 and 29), or a tag whose
/// closing `>` is on a later line (round 41, finding 1) is tracked one line at a time,
/// not judged per event.
///
/// Comments and non-rendering elements are scanned in one left-to-right pass rather
/// than two independent ones (Codex, round 31, finding 4): a `<script>` written
/// *inside* a comment — `<div><!-- <script> --></div>` — is not a real opening tag at
/// all, and looking for non-rendering tags first, blind to comment spans, would open on
/// it and then never find a real close, hiding every line after. Comment scanning stops
/// once inside a real, uncommented `<script>` or `<style>`, for round 30's own reason: a
/// browser is in raw-text mode there, so a `<!--` inside one is just text, exactly like
/// a nested `<script>`'s literal `</style>`. Nothing inside `<template>` needs
/// comment-stripping either, because its whole content is already invisible regardless
/// of what is inside it.
///
/// Only the non-rendering element's own subrange is excluded from a line, not the whole
/// event (Codex, round 31, finding 1): `<div>decision-id headline<script>hidden</script>
/// visible suffix</div>` keeps its real prefix and suffix.
///
/// `open_non_rendering` is a stack rather than one tag (Codex, round 33, finding 2): a
/// `<script>` or `<style>` can nest inside a `<template>`, and its raw-text body has to
/// be tracked as its own level — closed only by its own matching close — rather than
/// scanned for a `</template>`-looking substring that is really just JavaScript or CSS
/// text. Holds owned names since round 42, finding 3, not only the fixed
/// `&'static str`s, so an arbitrary element suppressed by its own `hidden` attribute
/// can be tracked the same way.
///
/// A block tag's own markup (`Markup`, stripped in either direction) is followed by a
/// [`VisibleHtmlSpan::Break`] (Codex, pull request #138, round 39, finding 3): a
/// browser always starts a block element — `<div>`, `<p>`, and the rest of
/// [`HTML_BLOCK_TAG_NAMES`] — on a line of its own, so `<div>head</div><div>line</div>`
/// renders as two separate lines, `head` and `line`, not one running word. Excluding
/// the tags' own markup with nothing between them — as every fix through round 38 did —
/// fused the two into the literal contiguous run `headline`, matching a `.contains`
/// scan no reader would. An inline tag (`<span>`, `<em>`, and anything else not on that
/// list) forces no such break, so it gets none: `<div>Some <em>emphasized</em>
/// text</div>` must still read as one running line — except `<br>` (Codex, round 42,
/// finding 1), which forces one for a different reason than a block tag does: it is a
/// genuine, void line-break element wherever it appears, block context or not, exactly
/// the way `Event::InlineHtml`'s own `<br>` handling (round 37) already treats it.
///
/// Extracted from `visible_html_ranges` itself only to stay under clippy's line limit
/// (Codex, pull request #138, round 45, the same reason `append_visible_html` and
/// `track_non_rendering_html` were pulled out at rounds 30 and 33). Resolving a
/// `PendingTag` against `line` either finds its own close and returns the cursor just
/// past it, updating `open_non_rendering` on the way, or re-arms it (carrying `line`'s
/// text onto what it already held) and hands it back unresolved.
///
/// A resolved tag that reopens or closes the element already on top of
/// `open_non_rendering` (Codex, round 45, "Carry nested multiline tags through hidden
/// blocks") pushes or pops even with no `hidden` attribute of its own — a reopen of a
/// *nesting* element (`<template>`, or an arbitrary `hidden`-suppressed one) by its own
/// name is still a reopen, and a matching close is still a close, neither of which the
/// two checks below (written for the top-level, empty-stack case) know how to do.
///
/// A resolved tag [`implicitly_closed_by`] the open one (Codex, pull request #138,
/// round 48, "Honor implicit closes for optional-end-tag elements"; widened round 50,
/// "Honor implicit closes triggered by different tag names") pops the open one before
/// anything else decides the resolved tag's own fate — a cross-line `<li\n class="x">`
/// reopening a hidden `<li>`, or a cross-line `<div\n class="x">` closing a hidden `<p>`,
/// is a *sibling*, not a child nested inside the hidden one, so it is then judged purely
/// on its own `hidden` attribute below, the same as any other tag.
fn resolve_pending_tag(
    line: &str,
    open_non_rendering: &mut Vec<String>,
    foreign_content: &mut Vec<ForeignFrame>,
    pending: PendingTag,
) -> Result<usize, PendingTag> {
    let PendingTag {
        name,
        closing,
        mut quote,
        text,
    } = pending;
    let Some(end) = scan_tag_close(line, 0, &mut quote) else {
        let mut text = text;
        text.push_str(line);
        return Err(PendingTag {
            name,
            closing,
            quote,
            text,
        });
    };
    // The full tag text, not just this line's own portion (Codex, round 43, finding
    // 3): `hidden` may sit on any line the tag spans, not only the last one — and
    // (round 50) so can the `/` of a foreign-content root's own cross-line
    // self-closing form, `<svg\n/>`. Delegated to `track_foreign_content_depth` itself
    // (round 51) rather than a hand-written open/close pair, so a cross-line HTML
    // integration point (`<foreignObject\n>`) is carried the same way a cross-line
    // foreign-content root already was.
    let full_text = text + &line[..end];
    track_foreign_content_depth(&full_text, foreign_content);
    let open_top = open_non_rendering.last().cloned();
    let open_top_nests = open_top.as_deref().is_some_and(non_rendering_element_nests);
    let matches_open_top = open_top.as_deref() == Some(name.as_str()) && open_top_nests;
    if closing {
        if matches_open_top {
            open_non_rendering.pop();
        }
    } else {
        let implicit_close = open_top_nests
            && open_top
                .as_deref()
                .is_some_and(|top| implicitly_closed_by(top, &name));
        if implicit_close {
            open_non_rendering.pop();
        }
        let reopens_top = matches_open_top && !implicit_close;
        let is_fixed_name = matches!(
            name.as_str(),
            "script" | "style" | "title" | "template" | "iframe"
        );
        // The ambient foreign-content self-closing exemption, applied here too and
        // not only in the single-line paths (`next_hiding_marker`'s `Tag` handling,
        // `next_non_rendering_marker`'s `Open` handling) that already carry it
        // (Codex, pull request #138, round 57, "Honor multiline self-closing
        // scripts in SVG"): `<svg>\n<script\n />\n<text>All 6 recovery
        // invariants</text>\n</svg>` has SVG honoring the trailing slash on a
        // *fixed* non-rendering name the same way it would any other descendant,
        // so `<script>` has no body and needs no matching close — pushing it here
        // regardless waited forever for a `</script>` this document never writes,
        // hiding everything after it to end of document.
        let self_closing_in_foreign_content =
            ends_with_self_closing_slash(&full_text) && honors_self_closing_now(foreign_content);
        if (is_fixed_name || reopens_top) && !self_closing_in_foreign_content {
            open_non_rendering.push(name);
        } else if !is_fixed_name && !reopens_top && !is_void_element(&name) {
            // Self-closing foreign content checked here too, not only in
            // `find_any_hidden_opening_tag`'s own same-line search (Codex, round 46,
            // "Skip multiline self-closing foreign hidden tags"): `<svg\n hidden />`
            // takes this cross-line path, and pushing it regardless left the
            // tracked state waiting for a `</svg>` a document never writes, hiding
            // everything after it to end of document.
            if !is_self_closing_tag(&full_text, &name) && has_hidden_attribute(&full_text) {
                open_non_rendering.push(name);
            }
        }
    }
    Ok(end)
}

/// Handles [`next_hiding_marker`] finding nothing at `cursor` in `line`, which is
/// ambiguous on its own: either there is no more `<` at all (the remainder really
/// is visible text), or there is one whose own tag never closes before this line
/// runs out (Codex, round 41, finding 1) — `find_any_tag` commits to the first `<`
/// it finds and gives up entirely rather than searching past it, so an unclosed tag
/// anywhere in the remainder reads identically to no tag at all unless checked for
/// separately. Always ends the calling loop; extracted from [`visible_html_ranges`]
/// only to keep it under clippy's line limit (Codex, pull request #138, round 56,
/// the same reason [`resolve_pending_tag`] and [`track_non_rendering_html`] were
/// pulled out at rounds 45 and 33).
fn push_remaining_or_pending_tag(
    line: &str,
    cursor: usize,
    pending_tag: &mut Option<PendingTag>,
    spans: &mut Vec<VisibleHtmlSpan>,
) {
    if let Some(start) = next_tag_start(line, cursor) {
        spans.push(VisibleHtmlSpan::Text(cursor..start));
        let closing = line[start..].starts_with("</");
        let name = markup_tag_name(&line[start..]).to_ascii_lowercase();
        // The quote state this line's own scan reached is captured, not assumed
        // empty (Codex, round 42, finding 2): a tag whose quoted attribute value
        // itself crosses the line — `<div title="first\nsecond">All 6 recovery
        // invariants</div>` — left this line still inside that quote, and
        // starting the next line's resumed scan from `quote: None` read its own
        // closing quote as a fresh *opening* one, so the tag never resolved and
        // the visible text after it was discarded along with it.
        let mut quote: Option<u8> = None;
        scan_tag_close(line, start + 1, &mut quote);
        *pending_tag = Some(PendingTag {
            name,
            closing,
            quote,
            text: line[start..].to_owned(),
        });
        return;
    }
    spans.push(VisibleHtmlSpan::Text(cursor..line.len()));
}

/// Consumes CDATA payload text a foreign-content section left open at the end of an
/// earlier line (Codex, pull request #138, round 57, "Carry foreign CDATA across
/// lines"): either this line supplies the closing `]]>`, ending the section and
/// returning the cursor just past it, or the whole line is payload and nothing
/// remains open for a caller to resume past. Extracted from `visible_html_ranges`
/// itself only to stay under clippy's line limit, the same reason `push_list_item_marker`
/// and `push_remaining_or_pending_tag` were pulled out at round 56.
fn advance_past_cdata(
    line: &str,
    cursor: usize,
    in_cdata: &mut bool,
    spans: &mut Vec<VisibleHtmlSpan>,
) -> Option<usize> {
    if let Some(offset) = line.get(cursor..).and_then(|rest| rest.find("]]>")) {
        let payload_end = cursor + offset;
        spans.push(VisibleHtmlSpan::RawText(cursor..payload_end));
        *in_cdata = false;
        return Some(payload_end + "]]>".len());
    }
    spans.push(VisibleHtmlSpan::RawText(cursor..line.len()));
    None
}

fn visible_html_ranges(
    line: &str,
    open_construct: &mut OpenConstructState<'_>,
    open_non_rendering: &mut Vec<String>,
    pending_tag: &mut Option<PendingTag>,
    pending_raw_text_close: &mut Option<PendingRawTextClose>,
    foreign_content: &mut Vec<ForeignFrame>,
    ancestors: &mut Vec<String>,
) -> Vec<VisibleHtmlSpan> {
    let mut spans = Vec::new();
    // Resolved before anything else, ahead of even `pending_tag` (Codex, pull request
    // #138, round 45, "Finish multiline raw-text close tags before popping"): the two
    // never hold at once, since a raw-text element's own end tag is not scanned like an
    // ordinary one at all — but this is the more specific of the two mid-token states,
    // and the more clearly resolved one first is the same ordering `in_html_comment`
    // already gets below. Quote-aware, not a bare search (Codex, round 46, "Scan
    // raw-text end tags through an unquoted delimiter"): a browser still tracks quotes
    // while consuming a raw-text close tag's own trailing markup, so the delimiter can
    // sit past a quoted attribute value that itself contains an earlier `>`.
    let mut cursor = if let Some(pending) = pending_raw_text_close.take() {
        let mut quote = pending.quote;
        let Some(end) = scan_tag_close(line, 0, &mut quote) else {
            *pending_raw_text_close = Some(PendingRawTextClose { quote });
            return spans;
        };
        open_non_rendering.pop();
        end
    } else {
        0
    };
    // A tag whose own close is still missing is, by construction, not inside a comment
    // or a non-rendering element yet — those are states a *closed* tag can open — and
    // everything up to its resolution (this line's own bytes) is markup the same way
    // any other tag's is, never visible text (Codex, round 41, finding 1).
    if let Some(pending) = pending_tag.take() {
        match resolve_pending_tag(line, open_non_rendering, foreign_content, pending) {
            Ok(end) => cursor = end,
            Err(unresolved) => {
                *pending_tag = Some(unresolved);
                return spans;
            }
        }
    }
    loop {
        // Checked before the open element (Codex, round 32, finding 1): a comment
        // nested inside an open `<template>` and a comment at the top level share one
        // flag, since the two never hold at once — resolving it first is what lets a
        // `</template>` written *inside* such a comment be skipped rather than read as
        // the tracked element's real close.
        if *open_construct.in_html_comment {
            match find_comment_close(line, cursor) {
                Some(end) => {
                    cursor = end;
                    *open_construct.in_html_comment = false;
                    continue;
                }
                None => break,
            }
        }
        // A CDATA section left open by an earlier line carries the same way an
        // unterminated comment does (Codex, pull request #138, round 57, "Carry
        // foreign CDATA across lines"): the payload up to `]]>` — or the rest of
        // the line, if it is not here either — is visible text, never markup.
        if *open_construct.in_cdata {
            if let Some(next) =
                advance_past_cdata(line, cursor, open_construct.in_cdata, &mut spans)
            {
                cursor = next;
                continue;
            }
            break;
        }
        if !open_non_rendering.is_empty() {
            match advance_past_non_rendering(
                line,
                cursor,
                open_non_rendering,
                open_construct.in_html_comment,
                pending_tag,
                pending_raw_text_close,
                &mut NestedHtmlContext {
                    foreign_content,
                    ancestors,
                },
            ) {
                Some(end) => {
                    cursor = end;
                    continue;
                }
                None => break,
            }
        }
        match advance_via_hiding_marker(
            line,
            cursor,
            open_non_rendering,
            open_construct,
            pending_tag,
            &mut NestedHtmlContext {
                foreign_content,
                ancestors,
            },
            &mut spans,
        ) {
            Some(next) => cursor = next,
            None => break,
        }
    }
    spans
}

/// Applies whatever [`next_hiding_marker`] finds at or after `cursor`, returning the
/// cursor to resume `visible_html_ranges`'s own loop from, or `None` when nothing
/// more can be resolved on this line — an unterminated comment or an unclosed CDATA
/// section carries its open state across the line break instead, through
/// `open_construct`, and a marker of `None` itself means there was nothing left to
/// find at all. Extracted from `visible_html_ranges` only to stay under clippy's
/// line limit, the same reason [`advance_past_cdata`] was; bundled onto
/// [`OpenConstructState`] and [`NestedHtmlContext`], the same reason those two exist,
/// to stay under its parameter-count limit as well.
fn advance_via_hiding_marker(
    line: &str,
    cursor: usize,
    open_non_rendering: &mut Vec<String>,
    open_construct: &mut OpenConstructState<'_>,
    pending_tag: &mut Option<PendingTag>,
    nested: &mut NestedHtmlContext<'_>,
    spans: &mut Vec<VisibleHtmlSpan>,
) -> Option<usize> {
    match next_hiding_marker(line, cursor, nested.foreign_content) {
        None => {
            push_remaining_or_pending_tag(line, cursor, pending_tag, spans);
            None
        }
        Some(HidingMarker::Comment(start)) => {
            spans.push(VisibleHtmlSpan::Text(cursor..start));
            let Some(end) = find_comment_close(line, start) else {
                *open_construct.in_html_comment = true;
                return None;
            };
            Some(end)
        }
        Some(HidingMarker::Tag(start, end, tag)) => {
            spans.push(VisibleHtmlSpan::Text(cursor..start));
            open_non_rendering.push(tag.to_owned());
            Some(end)
        }
        Some(HidingMarker::Hidden(start, end, name)) => {
            spans.push(VisibleHtmlSpan::Text(cursor..start));
            open_non_rendering.push(name);
            Some(end)
        }
        Some(HidingMarker::Markup(start, end)) => {
            spans.push(VisibleHtmlSpan::Text(cursor..start));
            let span = &line[start..end];
            if is_html_block_tag(span) || is_line_break_tag(span) {
                spans.push(VisibleHtmlSpan::Break);
            }
            track_foreign_content_depth(span, nested.foreign_content);
            track_ordinary_ancestor(span, nested.foreign_content, nested.ancestors);
            Some(end)
        }
        Some(HidingMarker::Cdata(open_start, payload_start, payload_end, close_end)) => {
            spans.push(VisibleHtmlSpan::Text(cursor..open_start));
            spans.push(VisibleHtmlSpan::RawText(payload_start..payload_end));
            Some(close_end)
        }
        Some(HidingMarker::CdataOpen(open_start, payload_start)) => {
            spans.push(VisibleHtmlSpan::Text(cursor..open_start));
            spans.push(VisibleHtmlSpan::RawText(payload_start..line.len()));
            *open_construct.in_cdata = true;
            None
        }
    }
}

/// One visible byte range of an `Event::Html` line, or a forced line break a stripped
/// block tag's own markup leaves behind — see [`visible_html_ranges`].
enum VisibleHtmlSpan {
    /// A visible byte range into the original line, decoded for character references
    /// before a reader sees it — ordinary HTML text content, the way a browser renders
    /// it.
    Text(std::ops::Range<usize>),
    /// A foreign-content CDATA section's own payload (Codex, pull request #138, round
    /// 59, "Preserve character references inside foreign CDATA"): visible the same way
    /// [`Text`](VisibleHtmlSpan::Text) is, but never decoded — HTML5's CDATA section
    /// tokenizer state emits every byte between `<![CDATA[` and `]]>` literally, with
    /// no character-reference processing at all, unlike ordinary text content. `All
    /// &#54; recovery invariants` inside one renders exactly that way, the literal
    /// six-character sequence `&#54;` and all, never as `All 6 recovery invariants`.
    RawText(std::ops::Range<usize>),
    /// A line break a browser renders here that no byte range can carry, because no
    /// byte of the source is one.
    Break,
}

/// Appends `spans` (from `visible_html_ranges` over `html`) to `out`, extracted from
/// `markdown_prose` itself only to stay under clippy's line limit (Codex, pull request
/// #138, round 30, the same reason `track_non_rendering_html` was pulled out).
///
/// Each text span is decoded, not kept as raw source bytes (Codex, pull request #138,
/// round 45, "Decode entities in visible raw-HTML text"): a browser resolves
/// `<div>All &#54; recovery invariants</div>`
/// to `All 6 recovery invariants` before a reader ever sees it, the same as it resolves
/// an anchor's `href` before following it — [`decode_character_references`] is what
/// [`anchor_href`]'s own caller already uses for that reason, reused here for visible
/// block and inline HTML text.
fn append_visible_html(out: &mut String, html: &str, spans: Vec<VisibleHtmlSpan>) {
    for span in spans {
        match span {
            VisibleHtmlSpan::Text(range) => {
                out.push_str(&decode_character_references(&html[range]));
            }
            VisibleHtmlSpan::RawText(range) => out.push_str(&html[range]),
            VisibleHtmlSpan::Break => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
}

/// Whether an `Event::InlineHtml`'s text is some spelling of the `<br>` tag — the one
/// inline HTML element that renders as a line break rather than inline.
///
/// Matched case-insensitively on the tag *name* alone, extracted up to the first
/// whitespace or self-closing slash, so an attribute changes nothing: `<br>`, `<br/>`,
/// `<br />`, `<BR>` and `<br class="x">` all admit (Codex, pull request #138, rounds 26
/// and 27) — HTML tag names, attributes, spacing and the self-closing slash are all
/// parts a browser (and `pulldown-cmark`) ignores when deciding this is a line break.
/// A leading `/` is stripped first, too (Codex, round 28): `<br>` has no closing tag in
/// real HTML, so `</br>` is a parse error a browser recovers from by treating it as a
/// line break anyway, the same as the opening spelling. Any other tag name is ordinary
/// inline formatting that renders with no break at all.
fn is_line_break_tag(html: &str) -> bool {
    let Some(rest) = html.trim().strip_prefix('<') else {
        return false;
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let rest = rest.strip_suffix('>').unwrap_or(rest);
    let name_end = rest
        .find(|character: char| character.is_whitespace() || character == '/')
        .unwrap_or(rest.len());
    rest[..name_end].eq_ignore_ascii_case("br")
}

/// An anchor tag's `href` attribute value, if `html` is a well-formed `<a ...>` opening
/// tag carrying one.
///
/// A reader sees a link's destination — `<a href="tests/spine.rs">recovery proof</a>`
/// renders as a clickable label a browser resolves against `href` — but does not see any
/// other attribute an anchor or any other inline tag carries. `table_rows`'s
/// `Event::InlineHtml` arm used to keep such a tag's raw text verbatim so `href`'s value
/// reached the row as a substring (round 15), but a decoy attribute unrelated to the
/// link destination — `<span title="clause-id headline proof">` — rode along with it
/// just as verbatim, letting an otherwise empty cell satisfy a documentation check from
/// text no reader ever sees (Codex, pull request #138, round 35, finding 3). This
/// extracts only the one attribute a reader's own click already exposes, and only from a
/// genuine `<a` open tag — every other tag, and an anchor's own `</a>` close, carries
/// nothing this function returns.
///
/// The match is required to start a fresh attribute name — the byte right before it must
/// be absent or HTML whitespace, and it must sit outside any quoted attribute value
/// currently open (Codex, pull request #138, rounds 36 and 37, finding 2 of each). Round
/// 36: a bare substring search for `href=` also matched inside `data-href=`, so `<a
/// data-href="tests/spine.rs">elsewhere</a>` — an anchor with no link destination at all
/// — returned that unrelated attribute's value as if it were the real one; a leading
/// whitespace check closed that. Round 37: whitespace alone is not a real attribute
/// boundary — `<a title=" href='tests/spine.rs'">proof</a>` has no link destination
/// either, but the `href=` inside `title`'s own quoted value is *preceded* by
/// whitespace too (the space right after `title`'s opening quote), so the whitespace
/// check alone accepted it. The scan now tracks whichever quote character is currently
/// open, the same way `find_any_tag` and `find_opening_tag` do, and only tests for
/// `href=` while no attribute value is open — text inside one is never a fresh
/// attribute name, whatever byte precedes it.
///
/// The returned slice is the raw source text, not yet resolved the way a browser
/// resolves an attribute value before following it — [`decode_character_references`]
/// is the caller's job, kept separate so this function stays about finding the right
/// bytes rather than about decoding them.
fn anchor_href(html: &str) -> Option<&str> {
    find_opening_tag(html, 0, "a")?;
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let marker = "href";
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        match quote {
            Some(open) if byte == open => quote = None,
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if lower.as_bytes().get(index..index + marker.len())
                == Some(marker.as_bytes())
                && index.checked_sub(1).is_none_or(|before| {
                    bytes.get(before).is_some_and(u8::is_ascii_whitespace)
                }) =>
            {
                let after_name = index + marker.len();
                // The name must end right there, not continue into a longer attribute
                // (Codex, pull request #138, round 39, finding 2, guarded against while
                // fixing that same round's own ask): `hreflang="en"` starts with
                // `href`, and without this boundary check its own `l` would be read as
                // the start of the `=` HTML permits whitespace around — matching
                // `hreflang`'s value as though it were `href`'s.
                if !bytes
                    .get(after_name)
                    .is_none_or(|&b| b.is_ascii_whitespace() || b == b'=')
                {
                    index = after_name;
                    continue;
                }
                // HTML permits whitespace on both sides of the `=` — `href = "..."`
                // and `href= "..."` are as legal as `href="..."` — but a bare,
                // contiguous `href=` search rejected anything else (Codex, round 39,
                // finding 2): `table_rows` then lost the destination of a raw anchor
                // written with any of that legal spacing.
                let after_name_whitespace = html
                    .get(after_name..)?
                    .find(|character: char| !character.is_whitespace())
                    .map_or(html.len(), |offset| after_name + offset);
                if bytes.get(after_name_whitespace) != Some(&b'=') {
                    index = after_name;
                    continue;
                }
                let after_equals = after_name_whitespace + 1;
                let value_start = html
                    .get(after_equals..)?
                    .find(|character: char| !character.is_whitespace())
                    .map_or(html.len(), |offset| after_equals + offset);
                let value_quote = *bytes.get(value_start)?;
                if value_quote != b'"' && value_quote != b'\'' {
                    // An unquoted HTML attribute value (Codex, pull request #138, round
                    // 38, finding 2): `<a href=tests/spine.rs>recovery proof</a>` is
                    // real, valid HTML — a browser follows it exactly as it would a
                    // quoted `href` — so `href=tests/spine.rs` must not be read as an
                    // empty or absent value. It runs to the next HTML whitespace or the
                    // tag's own closing `>`, whichever comes first; neither character is
                    // legal inside an unquoted value.
                    let end = html
                        .get(value_start..)?
                        .find(|character: char| character.is_whitespace() || character == '>')
                        .map_or(html.len(), |offset| value_start + offset);
                    return Some(&html[value_start..end]);
                }
                let value_start = value_start + 1;
                let end = value_start + html.get(value_start..)?.find(value_quote as char)?;
                return Some(&html[value_start..end]);
            }
            Some(_) | None => {}
        }
        index += 1;
    }
    None
}

/// `value` with every HTML character reference it carries resolved to the text it
/// names, the way a browser resolves an attribute value before using it (Codex, pull
/// request #138, round 44, finding 3; widened to the full named-reference set round 52,
/// "Decode the full HTML named-reference set"; widened to a semicolon-optional numeric
/// reference round 53, "Decode numeric references without semicolons"): a raw anchor's
/// destination can itself encode part of its path as a reference — `tests&#47;spine.rs`,
/// `tests&#47spine.rs` and `tests&sol;spine.rs` are all the same destination,
/// `tests/spine.rs`, as a reader's click — and comparing the undecoded source text
/// against a real repository path finds none of them.
///
/// A numeric reference is tried first and does not need a terminating `;` at all
/// (`numeric_character_reference_at`'s own doc comment says why); only once that fails
/// does a semicolon-terminated named reference get a turn, since — unlike a numeric
/// one — the specification's own semicolon-optional form for a named reference is a
/// second, fixed legacy list [`NAMED_CHARACTER_REFERENCES`] deliberately does not carry
/// (that table's own doc comment says why). A name that table does not list, or a `&`
/// that is neither, is left exactly as written.
fn decode_character_references(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(offset) = rest.find('&') {
        out.push_str(&rest[..offset]);
        let after_amp = &rest[offset + 1..];
        if let Some((character, consumed)) = numeric_character_reference_at(after_amp) {
            out.push(character);
            rest = &after_amp[consumed..];
            continue;
        }
        let Some(semicolon) = after_amp.find(';') else {
            out.push('&');
            rest = after_amp;
            continue;
        };
        let entity = &after_amp[..semicolon];
        if let Some(named) = named_character_reference(entity) {
            out.push_str(named);
            rest = &after_amp[semicolon + 1..];
        } else {
            out.push('&');
            rest = after_amp;
        }
    }
    out.push_str(rest);
    out
}

/// The complete WHATWG HTML5 set of *semicolon-terminated* named character
/// references, name to resolved text, sorted by name for [`named_character_reference`]'s
/// binary search — generated from the specification's own machine-readable
/// `entities.json` (Codex, pull request #138, round 52, "Decode the full HTML
/// named-reference set"): `decode_character_references` used to recognize only the five
/// XML entities, so a raw anchor's destination written with any other standard named
/// reference — `tests&sol;spine.rs`, say, which a browser resolves to `tests/spine.rs`
/// — was left undecoded and never matched the real repository path a reader's click (or
/// a comparison) expects. Deliberately narrower than the full HTML5 tokenizer's own
/// character-reference state in one respect: this table holds only the semicolon-
/// terminated spelling of each name, never the legacy semicolon-optional forms
/// (`&nbsp` alongside `&nbsp;`) the spec keeps for web compatibility with pre-HTML5
/// content — recognizing those needs the tokenizer's own longest-match-without-a-
/// terminator algorithm, which does not fit the `&...;`-bounded scan
/// [`decode_character_references`] already does for a numeric reference, and no
/// construct this module reads for was ever written without the semicolon in the first
/// place.
const NAMED_CHARACTER_REFERENCES: &[(&str, &str)] = &[
    ("AElig", "\u{c6}"),
    ("AMP", "&"),
    ("Aacute", "\u{c1}"),
    ("Abreve", "\u{102}"),
    ("Acirc", "\u{c2}"),
    ("Acy", "\u{410}"),
    ("Afr", "\u{1d504}"),
    ("Agrave", "\u{c0}"),
    ("Alpha", "\u{391}"),
    ("Amacr", "\u{100}"),
    ("And", "\u{2a53}"),
    ("Aogon", "\u{104}"),
    ("Aopf", "\u{1d538}"),
    ("ApplyFunction", "\u{2061}"),
    ("Aring", "\u{c5}"),
    ("Ascr", "\u{1d49c}"),
    ("Assign", "\u{2254}"),
    ("Atilde", "\u{c3}"),
    ("Auml", "\u{c4}"),
    ("Backslash", "\u{2216}"),
    ("Barv", "\u{2ae7}"),
    ("Barwed", "\u{2306}"),
    ("Bcy", "\u{411}"),
    ("Because", "\u{2235}"),
    ("Bernoullis", "\u{212c}"),
    ("Beta", "\u{392}"),
    ("Bfr", "\u{1d505}"),
    ("Bopf", "\u{1d539}"),
    ("Breve", "\u{2d8}"),
    ("Bscr", "\u{212c}"),
    ("Bumpeq", "\u{224e}"),
    ("CHcy", "\u{427}"),
    ("COPY", "\u{a9}"),
    ("Cacute", "\u{106}"),
    ("Cap", "\u{22d2}"),
    ("CapitalDifferentialD", "\u{2145}"),
    ("Cayleys", "\u{212d}"),
    ("Ccaron", "\u{10c}"),
    ("Ccedil", "\u{c7}"),
    ("Ccirc", "\u{108}"),
    ("Cconint", "\u{2230}"),
    ("Cdot", "\u{10a}"),
    ("Cedilla", "\u{b8}"),
    ("CenterDot", "\u{b7}"),
    ("Cfr", "\u{212d}"),
    ("Chi", "\u{3a7}"),
    ("CircleDot", "\u{2299}"),
    ("CircleMinus", "\u{2296}"),
    ("CirclePlus", "\u{2295}"),
    ("CircleTimes", "\u{2297}"),
    ("ClockwiseContourIntegral", "\u{2232}"),
    ("CloseCurlyDoubleQuote", "\u{201d}"),
    ("CloseCurlyQuote", "\u{2019}"),
    ("Colon", "\u{2237}"),
    ("Colone", "\u{2a74}"),
    ("Congruent", "\u{2261}"),
    ("Conint", "\u{222f}"),
    ("ContourIntegral", "\u{222e}"),
    ("Copf", "\u{2102}"),
    ("Coproduct", "\u{2210}"),
    ("CounterClockwiseContourIntegral", "\u{2233}"),
    ("Cross", "\u{2a2f}"),
    ("Cscr", "\u{1d49e}"),
    ("Cup", "\u{22d3}"),
    ("CupCap", "\u{224d}"),
    ("DD", "\u{2145}"),
    ("DDotrahd", "\u{2911}"),
    ("DJcy", "\u{402}"),
    ("DScy", "\u{405}"),
    ("DZcy", "\u{40f}"),
    ("Dagger", "\u{2021}"),
    ("Darr", "\u{21a1}"),
    ("Dashv", "\u{2ae4}"),
    ("Dcaron", "\u{10e}"),
    ("Dcy", "\u{414}"),
    ("Del", "\u{2207}"),
    ("Delta", "\u{394}"),
    ("Dfr", "\u{1d507}"),
    ("DiacriticalAcute", "\u{b4}"),
    ("DiacriticalDot", "\u{2d9}"),
    ("DiacriticalDoubleAcute", "\u{2dd}"),
    ("DiacriticalGrave", "`"),
    ("DiacriticalTilde", "\u{2dc}"),
    ("Diamond", "\u{22c4}"),
    ("DifferentialD", "\u{2146}"),
    ("Dopf", "\u{1d53b}"),
    ("Dot", "\u{a8}"),
    ("DotDot", "\u{20dc}"),
    ("DotEqual", "\u{2250}"),
    ("DoubleContourIntegral", "\u{222f}"),
    ("DoubleDot", "\u{a8}"),
    ("DoubleDownArrow", "\u{21d3}"),
    ("DoubleLeftArrow", "\u{21d0}"),
    ("DoubleLeftRightArrow", "\u{21d4}"),
    ("DoubleLeftTee", "\u{2ae4}"),
    ("DoubleLongLeftArrow", "\u{27f8}"),
    ("DoubleLongLeftRightArrow", "\u{27fa}"),
    ("DoubleLongRightArrow", "\u{27f9}"),
    ("DoubleRightArrow", "\u{21d2}"),
    ("DoubleRightTee", "\u{22a8}"),
    ("DoubleUpArrow", "\u{21d1}"),
    ("DoubleUpDownArrow", "\u{21d5}"),
    ("DoubleVerticalBar", "\u{2225}"),
    ("DownArrow", "\u{2193}"),
    ("DownArrowBar", "\u{2913}"),
    ("DownArrowUpArrow", "\u{21f5}"),
    ("DownBreve", "\u{311}"),
    ("DownLeftRightVector", "\u{2950}"),
    ("DownLeftTeeVector", "\u{295e}"),
    ("DownLeftVector", "\u{21bd}"),
    ("DownLeftVectorBar", "\u{2956}"),
    ("DownRightTeeVector", "\u{295f}"),
    ("DownRightVector", "\u{21c1}"),
    ("DownRightVectorBar", "\u{2957}"),
    ("DownTee", "\u{22a4}"),
    ("DownTeeArrow", "\u{21a7}"),
    ("Downarrow", "\u{21d3}"),
    ("Dscr", "\u{1d49f}"),
    ("Dstrok", "\u{110}"),
    ("ENG", "\u{14a}"),
    ("ETH", "\u{d0}"),
    ("Eacute", "\u{c9}"),
    ("Ecaron", "\u{11a}"),
    ("Ecirc", "\u{ca}"),
    ("Ecy", "\u{42d}"),
    ("Edot", "\u{116}"),
    ("Efr", "\u{1d508}"),
    ("Egrave", "\u{c8}"),
    ("Element", "\u{2208}"),
    ("Emacr", "\u{112}"),
    ("EmptySmallSquare", "\u{25fb}"),
    ("EmptyVerySmallSquare", "\u{25ab}"),
    ("Eogon", "\u{118}"),
    ("Eopf", "\u{1d53c}"),
    ("Epsilon", "\u{395}"),
    ("Equal", "\u{2a75}"),
    ("EqualTilde", "\u{2242}"),
    ("Equilibrium", "\u{21cc}"),
    ("Escr", "\u{2130}"),
    ("Esim", "\u{2a73}"),
    ("Eta", "\u{397}"),
    ("Euml", "\u{cb}"),
    ("Exists", "\u{2203}"),
    ("ExponentialE", "\u{2147}"),
    ("Fcy", "\u{424}"),
    ("Ffr", "\u{1d509}"),
    ("FilledSmallSquare", "\u{25fc}"),
    ("FilledVerySmallSquare", "\u{25aa}"),
    ("Fopf", "\u{1d53d}"),
    ("ForAll", "\u{2200}"),
    ("Fouriertrf", "\u{2131}"),
    ("Fscr", "\u{2131}"),
    ("GJcy", "\u{403}"),
    ("GT", ">"),
    ("Gamma", "\u{393}"),
    ("Gammad", "\u{3dc}"),
    ("Gbreve", "\u{11e}"),
    ("Gcedil", "\u{122}"),
    ("Gcirc", "\u{11c}"),
    ("Gcy", "\u{413}"),
    ("Gdot", "\u{120}"),
    ("Gfr", "\u{1d50a}"),
    ("Gg", "\u{22d9}"),
    ("Gopf", "\u{1d53e}"),
    ("GreaterEqual", "\u{2265}"),
    ("GreaterEqualLess", "\u{22db}"),
    ("GreaterFullEqual", "\u{2267}"),
    ("GreaterGreater", "\u{2aa2}"),
    ("GreaterLess", "\u{2277}"),
    ("GreaterSlantEqual", "\u{2a7e}"),
    ("GreaterTilde", "\u{2273}"),
    ("Gscr", "\u{1d4a2}"),
    ("Gt", "\u{226b}"),
    ("HARDcy", "\u{42a}"),
    ("Hacek", "\u{2c7}"),
    ("Hat", "^"),
    ("Hcirc", "\u{124}"),
    ("Hfr", "\u{210c}"),
    ("HilbertSpace", "\u{210b}"),
    ("Hopf", "\u{210d}"),
    ("HorizontalLine", "\u{2500}"),
    ("Hscr", "\u{210b}"),
    ("Hstrok", "\u{126}"),
    ("HumpDownHump", "\u{224e}"),
    ("HumpEqual", "\u{224f}"),
    ("IEcy", "\u{415}"),
    ("IJlig", "\u{132}"),
    ("IOcy", "\u{401}"),
    ("Iacute", "\u{cd}"),
    ("Icirc", "\u{ce}"),
    ("Icy", "\u{418}"),
    ("Idot", "\u{130}"),
    ("Ifr", "\u{2111}"),
    ("Igrave", "\u{cc}"),
    ("Im", "\u{2111}"),
    ("Imacr", "\u{12a}"),
    ("ImaginaryI", "\u{2148}"),
    ("Implies", "\u{21d2}"),
    ("Int", "\u{222c}"),
    ("Integral", "\u{222b}"),
    ("Intersection", "\u{22c2}"),
    ("InvisibleComma", "\u{2063}"),
    ("InvisibleTimes", "\u{2062}"),
    ("Iogon", "\u{12e}"),
    ("Iopf", "\u{1d540}"),
    ("Iota", "\u{399}"),
    ("Iscr", "\u{2110}"),
    ("Itilde", "\u{128}"),
    ("Iukcy", "\u{406}"),
    ("Iuml", "\u{cf}"),
    ("Jcirc", "\u{134}"),
    ("Jcy", "\u{419}"),
    ("Jfr", "\u{1d50d}"),
    ("Jopf", "\u{1d541}"),
    ("Jscr", "\u{1d4a5}"),
    ("Jsercy", "\u{408}"),
    ("Jukcy", "\u{404}"),
    ("KHcy", "\u{425}"),
    ("KJcy", "\u{40c}"),
    ("Kappa", "\u{39a}"),
    ("Kcedil", "\u{136}"),
    ("Kcy", "\u{41a}"),
    ("Kfr", "\u{1d50e}"),
    ("Kopf", "\u{1d542}"),
    ("Kscr", "\u{1d4a6}"),
    ("LJcy", "\u{409}"),
    ("LT", "<"),
    ("Lacute", "\u{139}"),
    ("Lambda", "\u{39b}"),
    ("Lang", "\u{27ea}"),
    ("Laplacetrf", "\u{2112}"),
    ("Larr", "\u{219e}"),
    ("Lcaron", "\u{13d}"),
    ("Lcedil", "\u{13b}"),
    ("Lcy", "\u{41b}"),
    ("LeftAngleBracket", "\u{27e8}"),
    ("LeftArrow", "\u{2190}"),
    ("LeftArrowBar", "\u{21e4}"),
    ("LeftArrowRightArrow", "\u{21c6}"),
    ("LeftCeiling", "\u{2308}"),
    ("LeftDoubleBracket", "\u{27e6}"),
    ("LeftDownTeeVector", "\u{2961}"),
    ("LeftDownVector", "\u{21c3}"),
    ("LeftDownVectorBar", "\u{2959}"),
    ("LeftFloor", "\u{230a}"),
    ("LeftRightArrow", "\u{2194}"),
    ("LeftRightVector", "\u{294e}"),
    ("LeftTee", "\u{22a3}"),
    ("LeftTeeArrow", "\u{21a4}"),
    ("LeftTeeVector", "\u{295a}"),
    ("LeftTriangle", "\u{22b2}"),
    ("LeftTriangleBar", "\u{29cf}"),
    ("LeftTriangleEqual", "\u{22b4}"),
    ("LeftUpDownVector", "\u{2951}"),
    ("LeftUpTeeVector", "\u{2960}"),
    ("LeftUpVector", "\u{21bf}"),
    ("LeftUpVectorBar", "\u{2958}"),
    ("LeftVector", "\u{21bc}"),
    ("LeftVectorBar", "\u{2952}"),
    ("Leftarrow", "\u{21d0}"),
    ("Leftrightarrow", "\u{21d4}"),
    ("LessEqualGreater", "\u{22da}"),
    ("LessFullEqual", "\u{2266}"),
    ("LessGreater", "\u{2276}"),
    ("LessLess", "\u{2aa1}"),
    ("LessSlantEqual", "\u{2a7d}"),
    ("LessTilde", "\u{2272}"),
    ("Lfr", "\u{1d50f}"),
    ("Ll", "\u{22d8}"),
    ("Lleftarrow", "\u{21da}"),
    ("Lmidot", "\u{13f}"),
    ("LongLeftArrow", "\u{27f5}"),
    ("LongLeftRightArrow", "\u{27f7}"),
    ("LongRightArrow", "\u{27f6}"),
    ("Longleftarrow", "\u{27f8}"),
    ("Longleftrightarrow", "\u{27fa}"),
    ("Longrightarrow", "\u{27f9}"),
    ("Lopf", "\u{1d543}"),
    ("LowerLeftArrow", "\u{2199}"),
    ("LowerRightArrow", "\u{2198}"),
    ("Lscr", "\u{2112}"),
    ("Lsh", "\u{21b0}"),
    ("Lstrok", "\u{141}"),
    ("Lt", "\u{226a}"),
    ("Map", "\u{2905}"),
    ("Mcy", "\u{41c}"),
    ("MediumSpace", "\u{205f}"),
    ("Mellintrf", "\u{2133}"),
    ("Mfr", "\u{1d510}"),
    ("MinusPlus", "\u{2213}"),
    ("Mopf", "\u{1d544}"),
    ("Mscr", "\u{2133}"),
    ("Mu", "\u{39c}"),
    ("NJcy", "\u{40a}"),
    ("Nacute", "\u{143}"),
    ("Ncaron", "\u{147}"),
    ("Ncedil", "\u{145}"),
    ("Ncy", "\u{41d}"),
    ("NegativeMediumSpace", "\u{200b}"),
    ("NegativeThickSpace", "\u{200b}"),
    ("NegativeThinSpace", "\u{200b}"),
    ("NegativeVeryThinSpace", "\u{200b}"),
    ("NestedGreaterGreater", "\u{226b}"),
    ("NestedLessLess", "\u{226a}"),
    ("NewLine", "\n"),
    ("Nfr", "\u{1d511}"),
    ("NoBreak", "\u{2060}"),
    ("NonBreakingSpace", "\u{a0}"),
    ("Nopf", "\u{2115}"),
    ("Not", "\u{2aec}"),
    ("NotCongruent", "\u{2262}"),
    ("NotCupCap", "\u{226d}"),
    ("NotDoubleVerticalBar", "\u{2226}"),
    ("NotElement", "\u{2209}"),
    ("NotEqual", "\u{2260}"),
    ("NotEqualTilde", "\u{2242}\u{338}"),
    ("NotExists", "\u{2204}"),
    ("NotGreater", "\u{226f}"),
    ("NotGreaterEqual", "\u{2271}"),
    ("NotGreaterFullEqual", "\u{2267}\u{338}"),
    ("NotGreaterGreater", "\u{226b}\u{338}"),
    ("NotGreaterLess", "\u{2279}"),
    ("NotGreaterSlantEqual", "\u{2a7e}\u{338}"),
    ("NotGreaterTilde", "\u{2275}"),
    ("NotHumpDownHump", "\u{224e}\u{338}"),
    ("NotHumpEqual", "\u{224f}\u{338}"),
    ("NotLeftTriangle", "\u{22ea}"),
    ("NotLeftTriangleBar", "\u{29cf}\u{338}"),
    ("NotLeftTriangleEqual", "\u{22ec}"),
    ("NotLess", "\u{226e}"),
    ("NotLessEqual", "\u{2270}"),
    ("NotLessGreater", "\u{2278}"),
    ("NotLessLess", "\u{226a}\u{338}"),
    ("NotLessSlantEqual", "\u{2a7d}\u{338}"),
    ("NotLessTilde", "\u{2274}"),
    ("NotNestedGreaterGreater", "\u{2aa2}\u{338}"),
    ("NotNestedLessLess", "\u{2aa1}\u{338}"),
    ("NotPrecedes", "\u{2280}"),
    ("NotPrecedesEqual", "\u{2aaf}\u{338}"),
    ("NotPrecedesSlantEqual", "\u{22e0}"),
    ("NotReverseElement", "\u{220c}"),
    ("NotRightTriangle", "\u{22eb}"),
    ("NotRightTriangleBar", "\u{29d0}\u{338}"),
    ("NotRightTriangleEqual", "\u{22ed}"),
    ("NotSquareSubset", "\u{228f}\u{338}"),
    ("NotSquareSubsetEqual", "\u{22e2}"),
    ("NotSquareSuperset", "\u{2290}\u{338}"),
    ("NotSquareSupersetEqual", "\u{22e3}"),
    ("NotSubset", "\u{2282}\u{20d2}"),
    ("NotSubsetEqual", "\u{2288}"),
    ("NotSucceeds", "\u{2281}"),
    ("NotSucceedsEqual", "\u{2ab0}\u{338}"),
    ("NotSucceedsSlantEqual", "\u{22e1}"),
    ("NotSucceedsTilde", "\u{227f}\u{338}"),
    ("NotSuperset", "\u{2283}\u{20d2}"),
    ("NotSupersetEqual", "\u{2289}"),
    ("NotTilde", "\u{2241}"),
    ("NotTildeEqual", "\u{2244}"),
    ("NotTildeFullEqual", "\u{2247}"),
    ("NotTildeTilde", "\u{2249}"),
    ("NotVerticalBar", "\u{2224}"),
    ("Nscr", "\u{1d4a9}"),
    ("Ntilde", "\u{d1}"),
    ("Nu", "\u{39d}"),
    ("OElig", "\u{152}"),
    ("Oacute", "\u{d3}"),
    ("Ocirc", "\u{d4}"),
    ("Ocy", "\u{41e}"),
    ("Odblac", "\u{150}"),
    ("Ofr", "\u{1d512}"),
    ("Ograve", "\u{d2}"),
    ("Omacr", "\u{14c}"),
    ("Omega", "\u{3a9}"),
    ("Omicron", "\u{39f}"),
    ("Oopf", "\u{1d546}"),
    ("OpenCurlyDoubleQuote", "\u{201c}"),
    ("OpenCurlyQuote", "\u{2018}"),
    ("Or", "\u{2a54}"),
    ("Oscr", "\u{1d4aa}"),
    ("Oslash", "\u{d8}"),
    ("Otilde", "\u{d5}"),
    ("Otimes", "\u{2a37}"),
    ("Ouml", "\u{d6}"),
    ("OverBar", "\u{203e}"),
    ("OverBrace", "\u{23de}"),
    ("OverBracket", "\u{23b4}"),
    ("OverParenthesis", "\u{23dc}"),
    ("PartialD", "\u{2202}"),
    ("Pcy", "\u{41f}"),
    ("Pfr", "\u{1d513}"),
    ("Phi", "\u{3a6}"),
    ("Pi", "\u{3a0}"),
    ("PlusMinus", "\u{b1}"),
    ("Poincareplane", "\u{210c}"),
    ("Popf", "\u{2119}"),
    ("Pr", "\u{2abb}"),
    ("Precedes", "\u{227a}"),
    ("PrecedesEqual", "\u{2aaf}"),
    ("PrecedesSlantEqual", "\u{227c}"),
    ("PrecedesTilde", "\u{227e}"),
    ("Prime", "\u{2033}"),
    ("Product", "\u{220f}"),
    ("Proportion", "\u{2237}"),
    ("Proportional", "\u{221d}"),
    ("Pscr", "\u{1d4ab}"),
    ("Psi", "\u{3a8}"),
    ("QUOT", "\""),
    ("Qfr", "\u{1d514}"),
    ("Qopf", "\u{211a}"),
    ("Qscr", "\u{1d4ac}"),
    ("RBarr", "\u{2910}"),
    ("REG", "\u{ae}"),
    ("Racute", "\u{154}"),
    ("Rang", "\u{27eb}"),
    ("Rarr", "\u{21a0}"),
    ("Rarrtl", "\u{2916}"),
    ("Rcaron", "\u{158}"),
    ("Rcedil", "\u{156}"),
    ("Rcy", "\u{420}"),
    ("Re", "\u{211c}"),
    ("ReverseElement", "\u{220b}"),
    ("ReverseEquilibrium", "\u{21cb}"),
    ("ReverseUpEquilibrium", "\u{296f}"),
    ("Rfr", "\u{211c}"),
    ("Rho", "\u{3a1}"),
    ("RightAngleBracket", "\u{27e9}"),
    ("RightArrow", "\u{2192}"),
    ("RightArrowBar", "\u{21e5}"),
    ("RightArrowLeftArrow", "\u{21c4}"),
    ("RightCeiling", "\u{2309}"),
    ("RightDoubleBracket", "\u{27e7}"),
    ("RightDownTeeVector", "\u{295d}"),
    ("RightDownVector", "\u{21c2}"),
    ("RightDownVectorBar", "\u{2955}"),
    ("RightFloor", "\u{230b}"),
    ("RightTee", "\u{22a2}"),
    ("RightTeeArrow", "\u{21a6}"),
    ("RightTeeVector", "\u{295b}"),
    ("RightTriangle", "\u{22b3}"),
    ("RightTriangleBar", "\u{29d0}"),
    ("RightTriangleEqual", "\u{22b5}"),
    ("RightUpDownVector", "\u{294f}"),
    ("RightUpTeeVector", "\u{295c}"),
    ("RightUpVector", "\u{21be}"),
    ("RightUpVectorBar", "\u{2954}"),
    ("RightVector", "\u{21c0}"),
    ("RightVectorBar", "\u{2953}"),
    ("Rightarrow", "\u{21d2}"),
    ("Ropf", "\u{211d}"),
    ("RoundImplies", "\u{2970}"),
    ("Rrightarrow", "\u{21db}"),
    ("Rscr", "\u{211b}"),
    ("Rsh", "\u{21b1}"),
    ("RuleDelayed", "\u{29f4}"),
    ("SHCHcy", "\u{429}"),
    ("SHcy", "\u{428}"),
    ("SOFTcy", "\u{42c}"),
    ("Sacute", "\u{15a}"),
    ("Sc", "\u{2abc}"),
    ("Scaron", "\u{160}"),
    ("Scedil", "\u{15e}"),
    ("Scirc", "\u{15c}"),
    ("Scy", "\u{421}"),
    ("Sfr", "\u{1d516}"),
    ("ShortDownArrow", "\u{2193}"),
    ("ShortLeftArrow", "\u{2190}"),
    ("ShortRightArrow", "\u{2192}"),
    ("ShortUpArrow", "\u{2191}"),
    ("Sigma", "\u{3a3}"),
    ("SmallCircle", "\u{2218}"),
    ("Sopf", "\u{1d54a}"),
    ("Sqrt", "\u{221a}"),
    ("Square", "\u{25a1}"),
    ("SquareIntersection", "\u{2293}"),
    ("SquareSubset", "\u{228f}"),
    ("SquareSubsetEqual", "\u{2291}"),
    ("SquareSuperset", "\u{2290}"),
    ("SquareSupersetEqual", "\u{2292}"),
    ("SquareUnion", "\u{2294}"),
    ("Sscr", "\u{1d4ae}"),
    ("Star", "\u{22c6}"),
    ("Sub", "\u{22d0}"),
    ("Subset", "\u{22d0}"),
    ("SubsetEqual", "\u{2286}"),
    ("Succeeds", "\u{227b}"),
    ("SucceedsEqual", "\u{2ab0}"),
    ("SucceedsSlantEqual", "\u{227d}"),
    ("SucceedsTilde", "\u{227f}"),
    ("SuchThat", "\u{220b}"),
    ("Sum", "\u{2211}"),
    ("Sup", "\u{22d1}"),
    ("Superset", "\u{2283}"),
    ("SupersetEqual", "\u{2287}"),
    ("Supset", "\u{22d1}"),
    ("THORN", "\u{de}"),
    ("TRADE", "\u{2122}"),
    ("TSHcy", "\u{40b}"),
    ("TScy", "\u{426}"),
    ("Tab", "\t"),
    ("Tau", "\u{3a4}"),
    ("Tcaron", "\u{164}"),
    ("Tcedil", "\u{162}"),
    ("Tcy", "\u{422}"),
    ("Tfr", "\u{1d517}"),
    ("Therefore", "\u{2234}"),
    ("Theta", "\u{398}"),
    ("ThickSpace", "\u{205f}\u{200a}"),
    ("ThinSpace", "\u{2009}"),
    ("Tilde", "\u{223c}"),
    ("TildeEqual", "\u{2243}"),
    ("TildeFullEqual", "\u{2245}"),
    ("TildeTilde", "\u{2248}"),
    ("Topf", "\u{1d54b}"),
    ("TripleDot", "\u{20db}"),
    ("Tscr", "\u{1d4af}"),
    ("Tstrok", "\u{166}"),
    ("Uacute", "\u{da}"),
    ("Uarr", "\u{219f}"),
    ("Uarrocir", "\u{2949}"),
    ("Ubrcy", "\u{40e}"),
    ("Ubreve", "\u{16c}"),
    ("Ucirc", "\u{db}"),
    ("Ucy", "\u{423}"),
    ("Udblac", "\u{170}"),
    ("Ufr", "\u{1d518}"),
    ("Ugrave", "\u{d9}"),
    ("Umacr", "\u{16a}"),
    ("UnderBar", "_"),
    ("UnderBrace", "\u{23df}"),
    ("UnderBracket", "\u{23b5}"),
    ("UnderParenthesis", "\u{23dd}"),
    ("Union", "\u{22c3}"),
    ("UnionPlus", "\u{228e}"),
    ("Uogon", "\u{172}"),
    ("Uopf", "\u{1d54c}"),
    ("UpArrow", "\u{2191}"),
    ("UpArrowBar", "\u{2912}"),
    ("UpArrowDownArrow", "\u{21c5}"),
    ("UpDownArrow", "\u{2195}"),
    ("UpEquilibrium", "\u{296e}"),
    ("UpTee", "\u{22a5}"),
    ("UpTeeArrow", "\u{21a5}"),
    ("Uparrow", "\u{21d1}"),
    ("Updownarrow", "\u{21d5}"),
    ("UpperLeftArrow", "\u{2196}"),
    ("UpperRightArrow", "\u{2197}"),
    ("Upsi", "\u{3d2}"),
    ("Upsilon", "\u{3a5}"),
    ("Uring", "\u{16e}"),
    ("Uscr", "\u{1d4b0}"),
    ("Utilde", "\u{168}"),
    ("Uuml", "\u{dc}"),
    ("VDash", "\u{22ab}"),
    ("Vbar", "\u{2aeb}"),
    ("Vcy", "\u{412}"),
    ("Vdash", "\u{22a9}"),
    ("Vdashl", "\u{2ae6}"),
    ("Vee", "\u{22c1}"),
    ("Verbar", "\u{2016}"),
    ("Vert", "\u{2016}"),
    ("VerticalBar", "\u{2223}"),
    ("VerticalLine", "|"),
    ("VerticalSeparator", "\u{2758}"),
    ("VerticalTilde", "\u{2240}"),
    ("VeryThinSpace", "\u{200a}"),
    ("Vfr", "\u{1d519}"),
    ("Vopf", "\u{1d54d}"),
    ("Vscr", "\u{1d4b1}"),
    ("Vvdash", "\u{22aa}"),
    ("Wcirc", "\u{174}"),
    ("Wedge", "\u{22c0}"),
    ("Wfr", "\u{1d51a}"),
    ("Wopf", "\u{1d54e}"),
    ("Wscr", "\u{1d4b2}"),
    ("Xfr", "\u{1d51b}"),
    ("Xi", "\u{39e}"),
    ("Xopf", "\u{1d54f}"),
    ("Xscr", "\u{1d4b3}"),
    ("YAcy", "\u{42f}"),
    ("YIcy", "\u{407}"),
    ("YUcy", "\u{42e}"),
    ("Yacute", "\u{dd}"),
    ("Ycirc", "\u{176}"),
    ("Ycy", "\u{42b}"),
    ("Yfr", "\u{1d51c}"),
    ("Yopf", "\u{1d550}"),
    ("Yscr", "\u{1d4b4}"),
    ("Yuml", "\u{178}"),
    ("ZHcy", "\u{416}"),
    ("Zacute", "\u{179}"),
    ("Zcaron", "\u{17d}"),
    ("Zcy", "\u{417}"),
    ("Zdot", "\u{17b}"),
    ("ZeroWidthSpace", "\u{200b}"),
    ("Zeta", "\u{396}"),
    ("Zfr", "\u{2128}"),
    ("Zopf", "\u{2124}"),
    ("Zscr", "\u{1d4b5}"),
    ("aacute", "\u{e1}"),
    ("abreve", "\u{103}"),
    ("ac", "\u{223e}"),
    ("acE", "\u{223e}\u{333}"),
    ("acd", "\u{223f}"),
    ("acirc", "\u{e2}"),
    ("acute", "\u{b4}"),
    ("acy", "\u{430}"),
    ("aelig", "\u{e6}"),
    ("af", "\u{2061}"),
    ("afr", "\u{1d51e}"),
    ("agrave", "\u{e0}"),
    ("alefsym", "\u{2135}"),
    ("aleph", "\u{2135}"),
    ("alpha", "\u{3b1}"),
    ("amacr", "\u{101}"),
    ("amalg", "\u{2a3f}"),
    ("amp", "&"),
    ("and", "\u{2227}"),
    ("andand", "\u{2a55}"),
    ("andd", "\u{2a5c}"),
    ("andslope", "\u{2a58}"),
    ("andv", "\u{2a5a}"),
    ("ang", "\u{2220}"),
    ("ange", "\u{29a4}"),
    ("angle", "\u{2220}"),
    ("angmsd", "\u{2221}"),
    ("angmsdaa", "\u{29a8}"),
    ("angmsdab", "\u{29a9}"),
    ("angmsdac", "\u{29aa}"),
    ("angmsdad", "\u{29ab}"),
    ("angmsdae", "\u{29ac}"),
    ("angmsdaf", "\u{29ad}"),
    ("angmsdag", "\u{29ae}"),
    ("angmsdah", "\u{29af}"),
    ("angrt", "\u{221f}"),
    ("angrtvb", "\u{22be}"),
    ("angrtvbd", "\u{299d}"),
    ("angsph", "\u{2222}"),
    ("angst", "\u{c5}"),
    ("angzarr", "\u{237c}"),
    ("aogon", "\u{105}"),
    ("aopf", "\u{1d552}"),
    ("ap", "\u{2248}"),
    ("apE", "\u{2a70}"),
    ("apacir", "\u{2a6f}"),
    ("ape", "\u{224a}"),
    ("apid", "\u{224b}"),
    ("apos", "'"),
    ("approx", "\u{2248}"),
    ("approxeq", "\u{224a}"),
    ("aring", "\u{e5}"),
    ("ascr", "\u{1d4b6}"),
    ("ast", "*"),
    ("asymp", "\u{2248}"),
    ("asympeq", "\u{224d}"),
    ("atilde", "\u{e3}"),
    ("auml", "\u{e4}"),
    ("awconint", "\u{2233}"),
    ("awint", "\u{2a11}"),
    ("bNot", "\u{2aed}"),
    ("backcong", "\u{224c}"),
    ("backepsilon", "\u{3f6}"),
    ("backprime", "\u{2035}"),
    ("backsim", "\u{223d}"),
    ("backsimeq", "\u{22cd}"),
    ("barvee", "\u{22bd}"),
    ("barwed", "\u{2305}"),
    ("barwedge", "\u{2305}"),
    ("bbrk", "\u{23b5}"),
    ("bbrktbrk", "\u{23b6}"),
    ("bcong", "\u{224c}"),
    ("bcy", "\u{431}"),
    ("bdquo", "\u{201e}"),
    ("becaus", "\u{2235}"),
    ("because", "\u{2235}"),
    ("bemptyv", "\u{29b0}"),
    ("bepsi", "\u{3f6}"),
    ("bernou", "\u{212c}"),
    ("beta", "\u{3b2}"),
    ("beth", "\u{2136}"),
    ("between", "\u{226c}"),
    ("bfr", "\u{1d51f}"),
    ("bigcap", "\u{22c2}"),
    ("bigcirc", "\u{25ef}"),
    ("bigcup", "\u{22c3}"),
    ("bigodot", "\u{2a00}"),
    ("bigoplus", "\u{2a01}"),
    ("bigotimes", "\u{2a02}"),
    ("bigsqcup", "\u{2a06}"),
    ("bigstar", "\u{2605}"),
    ("bigtriangledown", "\u{25bd}"),
    ("bigtriangleup", "\u{25b3}"),
    ("biguplus", "\u{2a04}"),
    ("bigvee", "\u{22c1}"),
    ("bigwedge", "\u{22c0}"),
    ("bkarow", "\u{290d}"),
    ("blacklozenge", "\u{29eb}"),
    ("blacksquare", "\u{25aa}"),
    ("blacktriangle", "\u{25b4}"),
    ("blacktriangledown", "\u{25be}"),
    ("blacktriangleleft", "\u{25c2}"),
    ("blacktriangleright", "\u{25b8}"),
    ("blank", "\u{2423}"),
    ("blk12", "\u{2592}"),
    ("blk14", "\u{2591}"),
    ("blk34", "\u{2593}"),
    ("block", "\u{2588}"),
    ("bne", "=\u{20e5}"),
    ("bnequiv", "\u{2261}\u{20e5}"),
    ("bnot", "\u{2310}"),
    ("bopf", "\u{1d553}"),
    ("bot", "\u{22a5}"),
    ("bottom", "\u{22a5}"),
    ("bowtie", "\u{22c8}"),
    ("boxDL", "\u{2557}"),
    ("boxDR", "\u{2554}"),
    ("boxDl", "\u{2556}"),
    ("boxDr", "\u{2553}"),
    ("boxH", "\u{2550}"),
    ("boxHD", "\u{2566}"),
    ("boxHU", "\u{2569}"),
    ("boxHd", "\u{2564}"),
    ("boxHu", "\u{2567}"),
    ("boxUL", "\u{255d}"),
    ("boxUR", "\u{255a}"),
    ("boxUl", "\u{255c}"),
    ("boxUr", "\u{2559}"),
    ("boxV", "\u{2551}"),
    ("boxVH", "\u{256c}"),
    ("boxVL", "\u{2563}"),
    ("boxVR", "\u{2560}"),
    ("boxVh", "\u{256b}"),
    ("boxVl", "\u{2562}"),
    ("boxVr", "\u{255f}"),
    ("boxbox", "\u{29c9}"),
    ("boxdL", "\u{2555}"),
    ("boxdR", "\u{2552}"),
    ("boxdl", "\u{2510}"),
    ("boxdr", "\u{250c}"),
    ("boxh", "\u{2500}"),
    ("boxhD", "\u{2565}"),
    ("boxhU", "\u{2568}"),
    ("boxhd", "\u{252c}"),
    ("boxhu", "\u{2534}"),
    ("boxminus", "\u{229f}"),
    ("boxplus", "\u{229e}"),
    ("boxtimes", "\u{22a0}"),
    ("boxuL", "\u{255b}"),
    ("boxuR", "\u{2558}"),
    ("boxul", "\u{2518}"),
    ("boxur", "\u{2514}"),
    ("boxv", "\u{2502}"),
    ("boxvH", "\u{256a}"),
    ("boxvL", "\u{2561}"),
    ("boxvR", "\u{255e}"),
    ("boxvh", "\u{253c}"),
    ("boxvl", "\u{2524}"),
    ("boxvr", "\u{251c}"),
    ("bprime", "\u{2035}"),
    ("breve", "\u{2d8}"),
    ("brvbar", "\u{a6}"),
    ("bscr", "\u{1d4b7}"),
    ("bsemi", "\u{204f}"),
    ("bsim", "\u{223d}"),
    ("bsime", "\u{22cd}"),
    ("bsol", "\\"),
    ("bsolb", "\u{29c5}"),
    ("bsolhsub", "\u{27c8}"),
    ("bull", "\u{2022}"),
    ("bullet", "\u{2022}"),
    ("bump", "\u{224e}"),
    ("bumpE", "\u{2aae}"),
    ("bumpe", "\u{224f}"),
    ("bumpeq", "\u{224f}"),
    ("cacute", "\u{107}"),
    ("cap", "\u{2229}"),
    ("capand", "\u{2a44}"),
    ("capbrcup", "\u{2a49}"),
    ("capcap", "\u{2a4b}"),
    ("capcup", "\u{2a47}"),
    ("capdot", "\u{2a40}"),
    ("caps", "\u{2229}\u{fe00}"),
    ("caret", "\u{2041}"),
    ("caron", "\u{2c7}"),
    ("ccaps", "\u{2a4d}"),
    ("ccaron", "\u{10d}"),
    ("ccedil", "\u{e7}"),
    ("ccirc", "\u{109}"),
    ("ccups", "\u{2a4c}"),
    ("ccupssm", "\u{2a50}"),
    ("cdot", "\u{10b}"),
    ("cedil", "\u{b8}"),
    ("cemptyv", "\u{29b2}"),
    ("cent", "\u{a2}"),
    ("centerdot", "\u{b7}"),
    ("cfr", "\u{1d520}"),
    ("chcy", "\u{447}"),
    ("check", "\u{2713}"),
    ("checkmark", "\u{2713}"),
    ("chi", "\u{3c7}"),
    ("cir", "\u{25cb}"),
    ("cirE", "\u{29c3}"),
    ("circ", "\u{2c6}"),
    ("circeq", "\u{2257}"),
    ("circlearrowleft", "\u{21ba}"),
    ("circlearrowright", "\u{21bb}"),
    ("circledR", "\u{ae}"),
    ("circledS", "\u{24c8}"),
    ("circledast", "\u{229b}"),
    ("circledcirc", "\u{229a}"),
    ("circleddash", "\u{229d}"),
    ("cire", "\u{2257}"),
    ("cirfnint", "\u{2a10}"),
    ("cirmid", "\u{2aef}"),
    ("cirscir", "\u{29c2}"),
    ("clubs", "\u{2663}"),
    ("clubsuit", "\u{2663}"),
    ("colon", ":"),
    ("colone", "\u{2254}"),
    ("coloneq", "\u{2254}"),
    ("comma", ","),
    ("commat", "@"),
    ("comp", "\u{2201}"),
    ("compfn", "\u{2218}"),
    ("complement", "\u{2201}"),
    ("complexes", "\u{2102}"),
    ("cong", "\u{2245}"),
    ("congdot", "\u{2a6d}"),
    ("conint", "\u{222e}"),
    ("copf", "\u{1d554}"),
    ("coprod", "\u{2210}"),
    ("copy", "\u{a9}"),
    ("copysr", "\u{2117}"),
    ("crarr", "\u{21b5}"),
    ("cross", "\u{2717}"),
    ("cscr", "\u{1d4b8}"),
    ("csub", "\u{2acf}"),
    ("csube", "\u{2ad1}"),
    ("csup", "\u{2ad0}"),
    ("csupe", "\u{2ad2}"),
    ("ctdot", "\u{22ef}"),
    ("cudarrl", "\u{2938}"),
    ("cudarrr", "\u{2935}"),
    ("cuepr", "\u{22de}"),
    ("cuesc", "\u{22df}"),
    ("cularr", "\u{21b6}"),
    ("cularrp", "\u{293d}"),
    ("cup", "\u{222a}"),
    ("cupbrcap", "\u{2a48}"),
    ("cupcap", "\u{2a46}"),
    ("cupcup", "\u{2a4a}"),
    ("cupdot", "\u{228d}"),
    ("cupor", "\u{2a45}"),
    ("cups", "\u{222a}\u{fe00}"),
    ("curarr", "\u{21b7}"),
    ("curarrm", "\u{293c}"),
    ("curlyeqprec", "\u{22de}"),
    ("curlyeqsucc", "\u{22df}"),
    ("curlyvee", "\u{22ce}"),
    ("curlywedge", "\u{22cf}"),
    ("curren", "\u{a4}"),
    ("curvearrowleft", "\u{21b6}"),
    ("curvearrowright", "\u{21b7}"),
    ("cuvee", "\u{22ce}"),
    ("cuwed", "\u{22cf}"),
    ("cwconint", "\u{2232}"),
    ("cwint", "\u{2231}"),
    ("cylcty", "\u{232d}"),
    ("dArr", "\u{21d3}"),
    ("dHar", "\u{2965}"),
    ("dagger", "\u{2020}"),
    ("daleth", "\u{2138}"),
    ("darr", "\u{2193}"),
    ("dash", "\u{2010}"),
    ("dashv", "\u{22a3}"),
    ("dbkarow", "\u{290f}"),
    ("dblac", "\u{2dd}"),
    ("dcaron", "\u{10f}"),
    ("dcy", "\u{434}"),
    ("dd", "\u{2146}"),
    ("ddagger", "\u{2021}"),
    ("ddarr", "\u{21ca}"),
    ("ddotseq", "\u{2a77}"),
    ("deg", "\u{b0}"),
    ("delta", "\u{3b4}"),
    ("demptyv", "\u{29b1}"),
    ("dfisht", "\u{297f}"),
    ("dfr", "\u{1d521}"),
    ("dharl", "\u{21c3}"),
    ("dharr", "\u{21c2}"),
    ("diam", "\u{22c4}"),
    ("diamond", "\u{22c4}"),
    ("diamondsuit", "\u{2666}"),
    ("diams", "\u{2666}"),
    ("die", "\u{a8}"),
    ("digamma", "\u{3dd}"),
    ("disin", "\u{22f2}"),
    ("div", "\u{f7}"),
    ("divide", "\u{f7}"),
    ("divideontimes", "\u{22c7}"),
    ("divonx", "\u{22c7}"),
    ("djcy", "\u{452}"),
    ("dlcorn", "\u{231e}"),
    ("dlcrop", "\u{230d}"),
    ("dollar", "$"),
    ("dopf", "\u{1d555}"),
    ("dot", "\u{2d9}"),
    ("doteq", "\u{2250}"),
    ("doteqdot", "\u{2251}"),
    ("dotminus", "\u{2238}"),
    ("dotplus", "\u{2214}"),
    ("dotsquare", "\u{22a1}"),
    ("doublebarwedge", "\u{2306}"),
    ("downarrow", "\u{2193}"),
    ("downdownarrows", "\u{21ca}"),
    ("downharpoonleft", "\u{21c3}"),
    ("downharpoonright", "\u{21c2}"),
    ("drbkarow", "\u{2910}"),
    ("drcorn", "\u{231f}"),
    ("drcrop", "\u{230c}"),
    ("dscr", "\u{1d4b9}"),
    ("dscy", "\u{455}"),
    ("dsol", "\u{29f6}"),
    ("dstrok", "\u{111}"),
    ("dtdot", "\u{22f1}"),
    ("dtri", "\u{25bf}"),
    ("dtrif", "\u{25be}"),
    ("duarr", "\u{21f5}"),
    ("duhar", "\u{296f}"),
    ("dwangle", "\u{29a6}"),
    ("dzcy", "\u{45f}"),
    ("dzigrarr", "\u{27ff}"),
    ("eDDot", "\u{2a77}"),
    ("eDot", "\u{2251}"),
    ("eacute", "\u{e9}"),
    ("easter", "\u{2a6e}"),
    ("ecaron", "\u{11b}"),
    ("ecir", "\u{2256}"),
    ("ecirc", "\u{ea}"),
    ("ecolon", "\u{2255}"),
    ("ecy", "\u{44d}"),
    ("edot", "\u{117}"),
    ("ee", "\u{2147}"),
    ("efDot", "\u{2252}"),
    ("efr", "\u{1d522}"),
    ("eg", "\u{2a9a}"),
    ("egrave", "\u{e8}"),
    ("egs", "\u{2a96}"),
    ("egsdot", "\u{2a98}"),
    ("el", "\u{2a99}"),
    ("elinters", "\u{23e7}"),
    ("ell", "\u{2113}"),
    ("els", "\u{2a95}"),
    ("elsdot", "\u{2a97}"),
    ("emacr", "\u{113}"),
    ("empty", "\u{2205}"),
    ("emptyset", "\u{2205}"),
    ("emptyv", "\u{2205}"),
    ("emsp", "\u{2003}"),
    ("emsp13", "\u{2004}"),
    ("emsp14", "\u{2005}"),
    ("eng", "\u{14b}"),
    ("ensp", "\u{2002}"),
    ("eogon", "\u{119}"),
    ("eopf", "\u{1d556}"),
    ("epar", "\u{22d5}"),
    ("eparsl", "\u{29e3}"),
    ("eplus", "\u{2a71}"),
    ("epsi", "\u{3b5}"),
    ("epsilon", "\u{3b5}"),
    ("epsiv", "\u{3f5}"),
    ("eqcirc", "\u{2256}"),
    ("eqcolon", "\u{2255}"),
    ("eqsim", "\u{2242}"),
    ("eqslantgtr", "\u{2a96}"),
    ("eqslantless", "\u{2a95}"),
    ("equals", "="),
    ("equest", "\u{225f}"),
    ("equiv", "\u{2261}"),
    ("equivDD", "\u{2a78}"),
    ("eqvparsl", "\u{29e5}"),
    ("erDot", "\u{2253}"),
    ("erarr", "\u{2971}"),
    ("escr", "\u{212f}"),
    ("esdot", "\u{2250}"),
    ("esim", "\u{2242}"),
    ("eta", "\u{3b7}"),
    ("eth", "\u{f0}"),
    ("euml", "\u{eb}"),
    ("euro", "\u{20ac}"),
    ("excl", "!"),
    ("exist", "\u{2203}"),
    ("expectation", "\u{2130}"),
    ("exponentiale", "\u{2147}"),
    ("fallingdotseq", "\u{2252}"),
    ("fcy", "\u{444}"),
    ("female", "\u{2640}"),
    ("ffilig", "\u{fb03}"),
    ("fflig", "\u{fb00}"),
    ("ffllig", "\u{fb04}"),
    ("ffr", "\u{1d523}"),
    ("filig", "\u{fb01}"),
    ("fjlig", "fj"),
    ("flat", "\u{266d}"),
    ("fllig", "\u{fb02}"),
    ("fltns", "\u{25b1}"),
    ("fnof", "\u{192}"),
    ("fopf", "\u{1d557}"),
    ("forall", "\u{2200}"),
    ("fork", "\u{22d4}"),
    ("forkv", "\u{2ad9}"),
    ("fpartint", "\u{2a0d}"),
    ("frac12", "\u{bd}"),
    ("frac13", "\u{2153}"),
    ("frac14", "\u{bc}"),
    ("frac15", "\u{2155}"),
    ("frac16", "\u{2159}"),
    ("frac18", "\u{215b}"),
    ("frac23", "\u{2154}"),
    ("frac25", "\u{2156}"),
    ("frac34", "\u{be}"),
    ("frac35", "\u{2157}"),
    ("frac38", "\u{215c}"),
    ("frac45", "\u{2158}"),
    ("frac56", "\u{215a}"),
    ("frac58", "\u{215d}"),
    ("frac78", "\u{215e}"),
    ("frasl", "\u{2044}"),
    ("frown", "\u{2322}"),
    ("fscr", "\u{1d4bb}"),
    ("gE", "\u{2267}"),
    ("gEl", "\u{2a8c}"),
    ("gacute", "\u{1f5}"),
    ("gamma", "\u{3b3}"),
    ("gammad", "\u{3dd}"),
    ("gap", "\u{2a86}"),
    ("gbreve", "\u{11f}"),
    ("gcirc", "\u{11d}"),
    ("gcy", "\u{433}"),
    ("gdot", "\u{121}"),
    ("ge", "\u{2265}"),
    ("gel", "\u{22db}"),
    ("geq", "\u{2265}"),
    ("geqq", "\u{2267}"),
    ("geqslant", "\u{2a7e}"),
    ("ges", "\u{2a7e}"),
    ("gescc", "\u{2aa9}"),
    ("gesdot", "\u{2a80}"),
    ("gesdoto", "\u{2a82}"),
    ("gesdotol", "\u{2a84}"),
    ("gesl", "\u{22db}\u{fe00}"),
    ("gesles", "\u{2a94}"),
    ("gfr", "\u{1d524}"),
    ("gg", "\u{226b}"),
    ("ggg", "\u{22d9}"),
    ("gimel", "\u{2137}"),
    ("gjcy", "\u{453}"),
    ("gl", "\u{2277}"),
    ("glE", "\u{2a92}"),
    ("gla", "\u{2aa5}"),
    ("glj", "\u{2aa4}"),
    ("gnE", "\u{2269}"),
    ("gnap", "\u{2a8a}"),
    ("gnapprox", "\u{2a8a}"),
    ("gne", "\u{2a88}"),
    ("gneq", "\u{2a88}"),
    ("gneqq", "\u{2269}"),
    ("gnsim", "\u{22e7}"),
    ("gopf", "\u{1d558}"),
    ("grave", "`"),
    ("gscr", "\u{210a}"),
    ("gsim", "\u{2273}"),
    ("gsime", "\u{2a8e}"),
    ("gsiml", "\u{2a90}"),
    ("gt", ">"),
    ("gtcc", "\u{2aa7}"),
    ("gtcir", "\u{2a7a}"),
    ("gtdot", "\u{22d7}"),
    ("gtlPar", "\u{2995}"),
    ("gtquest", "\u{2a7c}"),
    ("gtrapprox", "\u{2a86}"),
    ("gtrarr", "\u{2978}"),
    ("gtrdot", "\u{22d7}"),
    ("gtreqless", "\u{22db}"),
    ("gtreqqless", "\u{2a8c}"),
    ("gtrless", "\u{2277}"),
    ("gtrsim", "\u{2273}"),
    ("gvertneqq", "\u{2269}\u{fe00}"),
    ("gvnE", "\u{2269}\u{fe00}"),
    ("hArr", "\u{21d4}"),
    ("hairsp", "\u{200a}"),
    ("half", "\u{bd}"),
    ("hamilt", "\u{210b}"),
    ("hardcy", "\u{44a}"),
    ("harr", "\u{2194}"),
    ("harrcir", "\u{2948}"),
    ("harrw", "\u{21ad}"),
    ("hbar", "\u{210f}"),
    ("hcirc", "\u{125}"),
    ("hearts", "\u{2665}"),
    ("heartsuit", "\u{2665}"),
    ("hellip", "\u{2026}"),
    ("hercon", "\u{22b9}"),
    ("hfr", "\u{1d525}"),
    ("hksearow", "\u{2925}"),
    ("hkswarow", "\u{2926}"),
    ("hoarr", "\u{21ff}"),
    ("homtht", "\u{223b}"),
    ("hookleftarrow", "\u{21a9}"),
    ("hookrightarrow", "\u{21aa}"),
    ("hopf", "\u{1d559}"),
    ("horbar", "\u{2015}"),
    ("hscr", "\u{1d4bd}"),
    ("hslash", "\u{210f}"),
    ("hstrok", "\u{127}"),
    ("hybull", "\u{2043}"),
    ("hyphen", "\u{2010}"),
    ("iacute", "\u{ed}"),
    ("ic", "\u{2063}"),
    ("icirc", "\u{ee}"),
    ("icy", "\u{438}"),
    ("iecy", "\u{435}"),
    ("iexcl", "\u{a1}"),
    ("iff", "\u{21d4}"),
    ("ifr", "\u{1d526}"),
    ("igrave", "\u{ec}"),
    ("ii", "\u{2148}"),
    ("iiiint", "\u{2a0c}"),
    ("iiint", "\u{222d}"),
    ("iinfin", "\u{29dc}"),
    ("iiota", "\u{2129}"),
    ("ijlig", "\u{133}"),
    ("imacr", "\u{12b}"),
    ("image", "\u{2111}"),
    ("imagline", "\u{2110}"),
    ("imagpart", "\u{2111}"),
    ("imath", "\u{131}"),
    ("imof", "\u{22b7}"),
    ("imped", "\u{1b5}"),
    ("in", "\u{2208}"),
    ("incare", "\u{2105}"),
    ("infin", "\u{221e}"),
    ("infintie", "\u{29dd}"),
    ("inodot", "\u{131}"),
    ("int", "\u{222b}"),
    ("intcal", "\u{22ba}"),
    ("integers", "\u{2124}"),
    ("intercal", "\u{22ba}"),
    ("intlarhk", "\u{2a17}"),
    ("intprod", "\u{2a3c}"),
    ("iocy", "\u{451}"),
    ("iogon", "\u{12f}"),
    ("iopf", "\u{1d55a}"),
    ("iota", "\u{3b9}"),
    ("iprod", "\u{2a3c}"),
    ("iquest", "\u{bf}"),
    ("iscr", "\u{1d4be}"),
    ("isin", "\u{2208}"),
    ("isinE", "\u{22f9}"),
    ("isindot", "\u{22f5}"),
    ("isins", "\u{22f4}"),
    ("isinsv", "\u{22f3}"),
    ("isinv", "\u{2208}"),
    ("it", "\u{2062}"),
    ("itilde", "\u{129}"),
    ("iukcy", "\u{456}"),
    ("iuml", "\u{ef}"),
    ("jcirc", "\u{135}"),
    ("jcy", "\u{439}"),
    ("jfr", "\u{1d527}"),
    ("jmath", "\u{237}"),
    ("jopf", "\u{1d55b}"),
    ("jscr", "\u{1d4bf}"),
    ("jsercy", "\u{458}"),
    ("jukcy", "\u{454}"),
    ("kappa", "\u{3ba}"),
    ("kappav", "\u{3f0}"),
    ("kcedil", "\u{137}"),
    ("kcy", "\u{43a}"),
    ("kfr", "\u{1d528}"),
    ("kgreen", "\u{138}"),
    ("khcy", "\u{445}"),
    ("kjcy", "\u{45c}"),
    ("kopf", "\u{1d55c}"),
    ("kscr", "\u{1d4c0}"),
    ("lAarr", "\u{21da}"),
    ("lArr", "\u{21d0}"),
    ("lAtail", "\u{291b}"),
    ("lBarr", "\u{290e}"),
    ("lE", "\u{2266}"),
    ("lEg", "\u{2a8b}"),
    ("lHar", "\u{2962}"),
    ("lacute", "\u{13a}"),
    ("laemptyv", "\u{29b4}"),
    ("lagran", "\u{2112}"),
    ("lambda", "\u{3bb}"),
    ("lang", "\u{27e8}"),
    ("langd", "\u{2991}"),
    ("langle", "\u{27e8}"),
    ("lap", "\u{2a85}"),
    ("laquo", "\u{ab}"),
    ("larr", "\u{2190}"),
    ("larrb", "\u{21e4}"),
    ("larrbfs", "\u{291f}"),
    ("larrfs", "\u{291d}"),
    ("larrhk", "\u{21a9}"),
    ("larrlp", "\u{21ab}"),
    ("larrpl", "\u{2939}"),
    ("larrsim", "\u{2973}"),
    ("larrtl", "\u{21a2}"),
    ("lat", "\u{2aab}"),
    ("latail", "\u{2919}"),
    ("late", "\u{2aad}"),
    ("lates", "\u{2aad}\u{fe00}"),
    ("lbarr", "\u{290c}"),
    ("lbbrk", "\u{2772}"),
    ("lbrace", "{"),
    ("lbrack", "["),
    ("lbrke", "\u{298b}"),
    ("lbrksld", "\u{298f}"),
    ("lbrkslu", "\u{298d}"),
    ("lcaron", "\u{13e}"),
    ("lcedil", "\u{13c}"),
    ("lceil", "\u{2308}"),
    ("lcub", "{"),
    ("lcy", "\u{43b}"),
    ("ldca", "\u{2936}"),
    ("ldquo", "\u{201c}"),
    ("ldquor", "\u{201e}"),
    ("ldrdhar", "\u{2967}"),
    ("ldrushar", "\u{294b}"),
    ("ldsh", "\u{21b2}"),
    ("le", "\u{2264}"),
    ("leftarrow", "\u{2190}"),
    ("leftarrowtail", "\u{21a2}"),
    ("leftharpoondown", "\u{21bd}"),
    ("leftharpoonup", "\u{21bc}"),
    ("leftleftarrows", "\u{21c7}"),
    ("leftrightarrow", "\u{2194}"),
    ("leftrightarrows", "\u{21c6}"),
    ("leftrightharpoons", "\u{21cb}"),
    ("leftrightsquigarrow", "\u{21ad}"),
    ("leftthreetimes", "\u{22cb}"),
    ("leg", "\u{22da}"),
    ("leq", "\u{2264}"),
    ("leqq", "\u{2266}"),
    ("leqslant", "\u{2a7d}"),
    ("les", "\u{2a7d}"),
    ("lescc", "\u{2aa8}"),
    ("lesdot", "\u{2a7f}"),
    ("lesdoto", "\u{2a81}"),
    ("lesdotor", "\u{2a83}"),
    ("lesg", "\u{22da}\u{fe00}"),
    ("lesges", "\u{2a93}"),
    ("lessapprox", "\u{2a85}"),
    ("lessdot", "\u{22d6}"),
    ("lesseqgtr", "\u{22da}"),
    ("lesseqqgtr", "\u{2a8b}"),
    ("lessgtr", "\u{2276}"),
    ("lesssim", "\u{2272}"),
    ("lfisht", "\u{297c}"),
    ("lfloor", "\u{230a}"),
    ("lfr", "\u{1d529}"),
    ("lg", "\u{2276}"),
    ("lgE", "\u{2a91}"),
    ("lhard", "\u{21bd}"),
    ("lharu", "\u{21bc}"),
    ("lharul", "\u{296a}"),
    ("lhblk", "\u{2584}"),
    ("ljcy", "\u{459}"),
    ("ll", "\u{226a}"),
    ("llarr", "\u{21c7}"),
    ("llcorner", "\u{231e}"),
    ("llhard", "\u{296b}"),
    ("lltri", "\u{25fa}"),
    ("lmidot", "\u{140}"),
    ("lmoust", "\u{23b0}"),
    ("lmoustache", "\u{23b0}"),
    ("lnE", "\u{2268}"),
    ("lnap", "\u{2a89}"),
    ("lnapprox", "\u{2a89}"),
    ("lne", "\u{2a87}"),
    ("lneq", "\u{2a87}"),
    ("lneqq", "\u{2268}"),
    ("lnsim", "\u{22e6}"),
    ("loang", "\u{27ec}"),
    ("loarr", "\u{21fd}"),
    ("lobrk", "\u{27e6}"),
    ("longleftarrow", "\u{27f5}"),
    ("longleftrightarrow", "\u{27f7}"),
    ("longmapsto", "\u{27fc}"),
    ("longrightarrow", "\u{27f6}"),
    ("looparrowleft", "\u{21ab}"),
    ("looparrowright", "\u{21ac}"),
    ("lopar", "\u{2985}"),
    ("lopf", "\u{1d55d}"),
    ("loplus", "\u{2a2d}"),
    ("lotimes", "\u{2a34}"),
    ("lowast", "\u{2217}"),
    ("lowbar", "_"),
    ("loz", "\u{25ca}"),
    ("lozenge", "\u{25ca}"),
    ("lozf", "\u{29eb}"),
    ("lpar", "("),
    ("lparlt", "\u{2993}"),
    ("lrarr", "\u{21c6}"),
    ("lrcorner", "\u{231f}"),
    ("lrhar", "\u{21cb}"),
    ("lrhard", "\u{296d}"),
    ("lrm", "\u{200e}"),
    ("lrtri", "\u{22bf}"),
    ("lsaquo", "\u{2039}"),
    ("lscr", "\u{1d4c1}"),
    ("lsh", "\u{21b0}"),
    ("lsim", "\u{2272}"),
    ("lsime", "\u{2a8d}"),
    ("lsimg", "\u{2a8f}"),
    ("lsqb", "["),
    ("lsquo", "\u{2018}"),
    ("lsquor", "\u{201a}"),
    ("lstrok", "\u{142}"),
    ("lt", "<"),
    ("ltcc", "\u{2aa6}"),
    ("ltcir", "\u{2a79}"),
    ("ltdot", "\u{22d6}"),
    ("lthree", "\u{22cb}"),
    ("ltimes", "\u{22c9}"),
    ("ltlarr", "\u{2976}"),
    ("ltquest", "\u{2a7b}"),
    ("ltrPar", "\u{2996}"),
    ("ltri", "\u{25c3}"),
    ("ltrie", "\u{22b4}"),
    ("ltrif", "\u{25c2}"),
    ("lurdshar", "\u{294a}"),
    ("luruhar", "\u{2966}"),
    ("lvertneqq", "\u{2268}\u{fe00}"),
    ("lvnE", "\u{2268}\u{fe00}"),
    ("mDDot", "\u{223a}"),
    ("macr", "\u{af}"),
    ("male", "\u{2642}"),
    ("malt", "\u{2720}"),
    ("maltese", "\u{2720}"),
    ("map", "\u{21a6}"),
    ("mapsto", "\u{21a6}"),
    ("mapstodown", "\u{21a7}"),
    ("mapstoleft", "\u{21a4}"),
    ("mapstoup", "\u{21a5}"),
    ("marker", "\u{25ae}"),
    ("mcomma", "\u{2a29}"),
    ("mcy", "\u{43c}"),
    ("mdash", "\u{2014}"),
    ("measuredangle", "\u{2221}"),
    ("mfr", "\u{1d52a}"),
    ("mho", "\u{2127}"),
    ("micro", "\u{b5}"),
    ("mid", "\u{2223}"),
    ("midast", "*"),
    ("midcir", "\u{2af0}"),
    ("middot", "\u{b7}"),
    ("minus", "\u{2212}"),
    ("minusb", "\u{229f}"),
    ("minusd", "\u{2238}"),
    ("minusdu", "\u{2a2a}"),
    ("mlcp", "\u{2adb}"),
    ("mldr", "\u{2026}"),
    ("mnplus", "\u{2213}"),
    ("models", "\u{22a7}"),
    ("mopf", "\u{1d55e}"),
    ("mp", "\u{2213}"),
    ("mscr", "\u{1d4c2}"),
    ("mstpos", "\u{223e}"),
    ("mu", "\u{3bc}"),
    ("multimap", "\u{22b8}"),
    ("mumap", "\u{22b8}"),
    ("nGg", "\u{22d9}\u{338}"),
    ("nGt", "\u{226b}\u{20d2}"),
    ("nGtv", "\u{226b}\u{338}"),
    ("nLeftarrow", "\u{21cd}"),
    ("nLeftrightarrow", "\u{21ce}"),
    ("nLl", "\u{22d8}\u{338}"),
    ("nLt", "\u{226a}\u{20d2}"),
    ("nLtv", "\u{226a}\u{338}"),
    ("nRightarrow", "\u{21cf}"),
    ("nVDash", "\u{22af}"),
    ("nVdash", "\u{22ae}"),
    ("nabla", "\u{2207}"),
    ("nacute", "\u{144}"),
    ("nang", "\u{2220}\u{20d2}"),
    ("nap", "\u{2249}"),
    ("napE", "\u{2a70}\u{338}"),
    ("napid", "\u{224b}\u{338}"),
    ("napos", "\u{149}"),
    ("napprox", "\u{2249}"),
    ("natur", "\u{266e}"),
    ("natural", "\u{266e}"),
    ("naturals", "\u{2115}"),
    ("nbsp", "\u{a0}"),
    ("nbump", "\u{224e}\u{338}"),
    ("nbumpe", "\u{224f}\u{338}"),
    ("ncap", "\u{2a43}"),
    ("ncaron", "\u{148}"),
    ("ncedil", "\u{146}"),
    ("ncong", "\u{2247}"),
    ("ncongdot", "\u{2a6d}\u{338}"),
    ("ncup", "\u{2a42}"),
    ("ncy", "\u{43d}"),
    ("ndash", "\u{2013}"),
    ("ne", "\u{2260}"),
    ("neArr", "\u{21d7}"),
    ("nearhk", "\u{2924}"),
    ("nearr", "\u{2197}"),
    ("nearrow", "\u{2197}"),
    ("nedot", "\u{2250}\u{338}"),
    ("nequiv", "\u{2262}"),
    ("nesear", "\u{2928}"),
    ("nesim", "\u{2242}\u{338}"),
    ("nexist", "\u{2204}"),
    ("nexists", "\u{2204}"),
    ("nfr", "\u{1d52b}"),
    ("ngE", "\u{2267}\u{338}"),
    ("nge", "\u{2271}"),
    ("ngeq", "\u{2271}"),
    ("ngeqq", "\u{2267}\u{338}"),
    ("ngeqslant", "\u{2a7e}\u{338}"),
    ("nges", "\u{2a7e}\u{338}"),
    ("ngsim", "\u{2275}"),
    ("ngt", "\u{226f}"),
    ("ngtr", "\u{226f}"),
    ("nhArr", "\u{21ce}"),
    ("nharr", "\u{21ae}"),
    ("nhpar", "\u{2af2}"),
    ("ni", "\u{220b}"),
    ("nis", "\u{22fc}"),
    ("nisd", "\u{22fa}"),
    ("niv", "\u{220b}"),
    ("njcy", "\u{45a}"),
    ("nlArr", "\u{21cd}"),
    ("nlE", "\u{2266}\u{338}"),
    ("nlarr", "\u{219a}"),
    ("nldr", "\u{2025}"),
    ("nle", "\u{2270}"),
    ("nleftarrow", "\u{219a}"),
    ("nleftrightarrow", "\u{21ae}"),
    ("nleq", "\u{2270}"),
    ("nleqq", "\u{2266}\u{338}"),
    ("nleqslant", "\u{2a7d}\u{338}"),
    ("nles", "\u{2a7d}\u{338}"),
    ("nless", "\u{226e}"),
    ("nlsim", "\u{2274}"),
    ("nlt", "\u{226e}"),
    ("nltri", "\u{22ea}"),
    ("nltrie", "\u{22ec}"),
    ("nmid", "\u{2224}"),
    ("nopf", "\u{1d55f}"),
    ("not", "\u{ac}"),
    ("notin", "\u{2209}"),
    ("notinE", "\u{22f9}\u{338}"),
    ("notindot", "\u{22f5}\u{338}"),
    ("notinva", "\u{2209}"),
    ("notinvb", "\u{22f7}"),
    ("notinvc", "\u{22f6}"),
    ("notni", "\u{220c}"),
    ("notniva", "\u{220c}"),
    ("notnivb", "\u{22fe}"),
    ("notnivc", "\u{22fd}"),
    ("npar", "\u{2226}"),
    ("nparallel", "\u{2226}"),
    ("nparsl", "\u{2afd}\u{20e5}"),
    ("npart", "\u{2202}\u{338}"),
    ("npolint", "\u{2a14}"),
    ("npr", "\u{2280}"),
    ("nprcue", "\u{22e0}"),
    ("npre", "\u{2aaf}\u{338}"),
    ("nprec", "\u{2280}"),
    ("npreceq", "\u{2aaf}\u{338}"),
    ("nrArr", "\u{21cf}"),
    ("nrarr", "\u{219b}"),
    ("nrarrc", "\u{2933}\u{338}"),
    ("nrarrw", "\u{219d}\u{338}"),
    ("nrightarrow", "\u{219b}"),
    ("nrtri", "\u{22eb}"),
    ("nrtrie", "\u{22ed}"),
    ("nsc", "\u{2281}"),
    ("nsccue", "\u{22e1}"),
    ("nsce", "\u{2ab0}\u{338}"),
    ("nscr", "\u{1d4c3}"),
    ("nshortmid", "\u{2224}"),
    ("nshortparallel", "\u{2226}"),
    ("nsim", "\u{2241}"),
    ("nsime", "\u{2244}"),
    ("nsimeq", "\u{2244}"),
    ("nsmid", "\u{2224}"),
    ("nspar", "\u{2226}"),
    ("nsqsube", "\u{22e2}"),
    ("nsqsupe", "\u{22e3}"),
    ("nsub", "\u{2284}"),
    ("nsubE", "\u{2ac5}\u{338}"),
    ("nsube", "\u{2288}"),
    ("nsubset", "\u{2282}\u{20d2}"),
    ("nsubseteq", "\u{2288}"),
    ("nsubseteqq", "\u{2ac5}\u{338}"),
    ("nsucc", "\u{2281}"),
    ("nsucceq", "\u{2ab0}\u{338}"),
    ("nsup", "\u{2285}"),
    ("nsupE", "\u{2ac6}\u{338}"),
    ("nsupe", "\u{2289}"),
    ("nsupset", "\u{2283}\u{20d2}"),
    ("nsupseteq", "\u{2289}"),
    ("nsupseteqq", "\u{2ac6}\u{338}"),
    ("ntgl", "\u{2279}"),
    ("ntilde", "\u{f1}"),
    ("ntlg", "\u{2278}"),
    ("ntriangleleft", "\u{22ea}"),
    ("ntrianglelefteq", "\u{22ec}"),
    ("ntriangleright", "\u{22eb}"),
    ("ntrianglerighteq", "\u{22ed}"),
    ("nu", "\u{3bd}"),
    ("num", "#"),
    ("numero", "\u{2116}"),
    ("numsp", "\u{2007}"),
    ("nvDash", "\u{22ad}"),
    ("nvHarr", "\u{2904}"),
    ("nvap", "\u{224d}\u{20d2}"),
    ("nvdash", "\u{22ac}"),
    ("nvge", "\u{2265}\u{20d2}"),
    ("nvgt", ">\u{20d2}"),
    ("nvinfin", "\u{29de}"),
    ("nvlArr", "\u{2902}"),
    ("nvle", "\u{2264}\u{20d2}"),
    ("nvlt", "<\u{20d2}"),
    ("nvltrie", "\u{22b4}\u{20d2}"),
    ("nvrArr", "\u{2903}"),
    ("nvrtrie", "\u{22b5}\u{20d2}"),
    ("nvsim", "\u{223c}\u{20d2}"),
    ("nwArr", "\u{21d6}"),
    ("nwarhk", "\u{2923}"),
    ("nwarr", "\u{2196}"),
    ("nwarrow", "\u{2196}"),
    ("nwnear", "\u{2927}"),
    ("oS", "\u{24c8}"),
    ("oacute", "\u{f3}"),
    ("oast", "\u{229b}"),
    ("ocir", "\u{229a}"),
    ("ocirc", "\u{f4}"),
    ("ocy", "\u{43e}"),
    ("odash", "\u{229d}"),
    ("odblac", "\u{151}"),
    ("odiv", "\u{2a38}"),
    ("odot", "\u{2299}"),
    ("odsold", "\u{29bc}"),
    ("oelig", "\u{153}"),
    ("ofcir", "\u{29bf}"),
    ("ofr", "\u{1d52c}"),
    ("ogon", "\u{2db}"),
    ("ograve", "\u{f2}"),
    ("ogt", "\u{29c1}"),
    ("ohbar", "\u{29b5}"),
    ("ohm", "\u{3a9}"),
    ("oint", "\u{222e}"),
    ("olarr", "\u{21ba}"),
    ("olcir", "\u{29be}"),
    ("olcross", "\u{29bb}"),
    ("oline", "\u{203e}"),
    ("olt", "\u{29c0}"),
    ("omacr", "\u{14d}"),
    ("omega", "\u{3c9}"),
    ("omicron", "\u{3bf}"),
    ("omid", "\u{29b6}"),
    ("ominus", "\u{2296}"),
    ("oopf", "\u{1d560}"),
    ("opar", "\u{29b7}"),
    ("operp", "\u{29b9}"),
    ("oplus", "\u{2295}"),
    ("or", "\u{2228}"),
    ("orarr", "\u{21bb}"),
    ("ord", "\u{2a5d}"),
    ("order", "\u{2134}"),
    ("orderof", "\u{2134}"),
    ("ordf", "\u{aa}"),
    ("ordm", "\u{ba}"),
    ("origof", "\u{22b6}"),
    ("oror", "\u{2a56}"),
    ("orslope", "\u{2a57}"),
    ("orv", "\u{2a5b}"),
    ("oscr", "\u{2134}"),
    ("oslash", "\u{f8}"),
    ("osol", "\u{2298}"),
    ("otilde", "\u{f5}"),
    ("otimes", "\u{2297}"),
    ("otimesas", "\u{2a36}"),
    ("ouml", "\u{f6}"),
    ("ovbar", "\u{233d}"),
    ("par", "\u{2225}"),
    ("para", "\u{b6}"),
    ("parallel", "\u{2225}"),
    ("parsim", "\u{2af3}"),
    ("parsl", "\u{2afd}"),
    ("part", "\u{2202}"),
    ("pcy", "\u{43f}"),
    ("percnt", "%"),
    ("period", "."),
    ("permil", "\u{2030}"),
    ("perp", "\u{22a5}"),
    ("pertenk", "\u{2031}"),
    ("pfr", "\u{1d52d}"),
    ("phi", "\u{3c6}"),
    ("phiv", "\u{3d5}"),
    ("phmmat", "\u{2133}"),
    ("phone", "\u{260e}"),
    ("pi", "\u{3c0}"),
    ("pitchfork", "\u{22d4}"),
    ("piv", "\u{3d6}"),
    ("planck", "\u{210f}"),
    ("planckh", "\u{210e}"),
    ("plankv", "\u{210f}"),
    ("plus", "+"),
    ("plusacir", "\u{2a23}"),
    ("plusb", "\u{229e}"),
    ("pluscir", "\u{2a22}"),
    ("plusdo", "\u{2214}"),
    ("plusdu", "\u{2a25}"),
    ("pluse", "\u{2a72}"),
    ("plusmn", "\u{b1}"),
    ("plussim", "\u{2a26}"),
    ("plustwo", "\u{2a27}"),
    ("pm", "\u{b1}"),
    ("pointint", "\u{2a15}"),
    ("popf", "\u{1d561}"),
    ("pound", "\u{a3}"),
    ("pr", "\u{227a}"),
    ("prE", "\u{2ab3}"),
    ("prap", "\u{2ab7}"),
    ("prcue", "\u{227c}"),
    ("pre", "\u{2aaf}"),
    ("prec", "\u{227a}"),
    ("precapprox", "\u{2ab7}"),
    ("preccurlyeq", "\u{227c}"),
    ("preceq", "\u{2aaf}"),
    ("precnapprox", "\u{2ab9}"),
    ("precneqq", "\u{2ab5}"),
    ("precnsim", "\u{22e8}"),
    ("precsim", "\u{227e}"),
    ("prime", "\u{2032}"),
    ("primes", "\u{2119}"),
    ("prnE", "\u{2ab5}"),
    ("prnap", "\u{2ab9}"),
    ("prnsim", "\u{22e8}"),
    ("prod", "\u{220f}"),
    ("profalar", "\u{232e}"),
    ("profline", "\u{2312}"),
    ("profsurf", "\u{2313}"),
    ("prop", "\u{221d}"),
    ("propto", "\u{221d}"),
    ("prsim", "\u{227e}"),
    ("prurel", "\u{22b0}"),
    ("pscr", "\u{1d4c5}"),
    ("psi", "\u{3c8}"),
    ("puncsp", "\u{2008}"),
    ("qfr", "\u{1d52e}"),
    ("qint", "\u{2a0c}"),
    ("qopf", "\u{1d562}"),
    ("qprime", "\u{2057}"),
    ("qscr", "\u{1d4c6}"),
    ("quaternions", "\u{210d}"),
    ("quatint", "\u{2a16}"),
    ("quest", "?"),
    ("questeq", "\u{225f}"),
    ("quot", "\""),
    ("rAarr", "\u{21db}"),
    ("rArr", "\u{21d2}"),
    ("rAtail", "\u{291c}"),
    ("rBarr", "\u{290f}"),
    ("rHar", "\u{2964}"),
    ("race", "\u{223d}\u{331}"),
    ("racute", "\u{155}"),
    ("radic", "\u{221a}"),
    ("raemptyv", "\u{29b3}"),
    ("rang", "\u{27e9}"),
    ("rangd", "\u{2992}"),
    ("range", "\u{29a5}"),
    ("rangle", "\u{27e9}"),
    ("raquo", "\u{bb}"),
    ("rarr", "\u{2192}"),
    ("rarrap", "\u{2975}"),
    ("rarrb", "\u{21e5}"),
    ("rarrbfs", "\u{2920}"),
    ("rarrc", "\u{2933}"),
    ("rarrfs", "\u{291e}"),
    ("rarrhk", "\u{21aa}"),
    ("rarrlp", "\u{21ac}"),
    ("rarrpl", "\u{2945}"),
    ("rarrsim", "\u{2974}"),
    ("rarrtl", "\u{21a3}"),
    ("rarrw", "\u{219d}"),
    ("ratail", "\u{291a}"),
    ("ratio", "\u{2236}"),
    ("rationals", "\u{211a}"),
    ("rbarr", "\u{290d}"),
    ("rbbrk", "\u{2773}"),
    ("rbrace", "}"),
    ("rbrack", "]"),
    ("rbrke", "\u{298c}"),
    ("rbrksld", "\u{298e}"),
    ("rbrkslu", "\u{2990}"),
    ("rcaron", "\u{159}"),
    ("rcedil", "\u{157}"),
    ("rceil", "\u{2309}"),
    ("rcub", "}"),
    ("rcy", "\u{440}"),
    ("rdca", "\u{2937}"),
    ("rdldhar", "\u{2969}"),
    ("rdquo", "\u{201d}"),
    ("rdquor", "\u{201d}"),
    ("rdsh", "\u{21b3}"),
    ("real", "\u{211c}"),
    ("realine", "\u{211b}"),
    ("realpart", "\u{211c}"),
    ("reals", "\u{211d}"),
    ("rect", "\u{25ad}"),
    ("reg", "\u{ae}"),
    ("rfisht", "\u{297d}"),
    ("rfloor", "\u{230b}"),
    ("rfr", "\u{1d52f}"),
    ("rhard", "\u{21c1}"),
    ("rharu", "\u{21c0}"),
    ("rharul", "\u{296c}"),
    ("rho", "\u{3c1}"),
    ("rhov", "\u{3f1}"),
    ("rightarrow", "\u{2192}"),
    ("rightarrowtail", "\u{21a3}"),
    ("rightharpoondown", "\u{21c1}"),
    ("rightharpoonup", "\u{21c0}"),
    ("rightleftarrows", "\u{21c4}"),
    ("rightleftharpoons", "\u{21cc}"),
    ("rightrightarrows", "\u{21c9}"),
    ("rightsquigarrow", "\u{219d}"),
    ("rightthreetimes", "\u{22cc}"),
    ("ring", "\u{2da}"),
    ("risingdotseq", "\u{2253}"),
    ("rlarr", "\u{21c4}"),
    ("rlhar", "\u{21cc}"),
    ("rlm", "\u{200f}"),
    ("rmoust", "\u{23b1}"),
    ("rmoustache", "\u{23b1}"),
    ("rnmid", "\u{2aee}"),
    ("roang", "\u{27ed}"),
    ("roarr", "\u{21fe}"),
    ("robrk", "\u{27e7}"),
    ("ropar", "\u{2986}"),
    ("ropf", "\u{1d563}"),
    ("roplus", "\u{2a2e}"),
    ("rotimes", "\u{2a35}"),
    ("rpar", ")"),
    ("rpargt", "\u{2994}"),
    ("rppolint", "\u{2a12}"),
    ("rrarr", "\u{21c9}"),
    ("rsaquo", "\u{203a}"),
    ("rscr", "\u{1d4c7}"),
    ("rsh", "\u{21b1}"),
    ("rsqb", "]"),
    ("rsquo", "\u{2019}"),
    ("rsquor", "\u{2019}"),
    ("rthree", "\u{22cc}"),
    ("rtimes", "\u{22ca}"),
    ("rtri", "\u{25b9}"),
    ("rtrie", "\u{22b5}"),
    ("rtrif", "\u{25b8}"),
    ("rtriltri", "\u{29ce}"),
    ("ruluhar", "\u{2968}"),
    ("rx", "\u{211e}"),
    ("sacute", "\u{15b}"),
    ("sbquo", "\u{201a}"),
    ("sc", "\u{227b}"),
    ("scE", "\u{2ab4}"),
    ("scap", "\u{2ab8}"),
    ("scaron", "\u{161}"),
    ("sccue", "\u{227d}"),
    ("sce", "\u{2ab0}"),
    ("scedil", "\u{15f}"),
    ("scirc", "\u{15d}"),
    ("scnE", "\u{2ab6}"),
    ("scnap", "\u{2aba}"),
    ("scnsim", "\u{22e9}"),
    ("scpolint", "\u{2a13}"),
    ("scsim", "\u{227f}"),
    ("scy", "\u{441}"),
    ("sdot", "\u{22c5}"),
    ("sdotb", "\u{22a1}"),
    ("sdote", "\u{2a66}"),
    ("seArr", "\u{21d8}"),
    ("searhk", "\u{2925}"),
    ("searr", "\u{2198}"),
    ("searrow", "\u{2198}"),
    ("sect", "\u{a7}"),
    ("semi", ";"),
    ("seswar", "\u{2929}"),
    ("setminus", "\u{2216}"),
    ("setmn", "\u{2216}"),
    ("sext", "\u{2736}"),
    ("sfr", "\u{1d530}"),
    ("sfrown", "\u{2322}"),
    ("sharp", "\u{266f}"),
    ("shchcy", "\u{449}"),
    ("shcy", "\u{448}"),
    ("shortmid", "\u{2223}"),
    ("shortparallel", "\u{2225}"),
    ("shy", "\u{ad}"),
    ("sigma", "\u{3c3}"),
    ("sigmaf", "\u{3c2}"),
    ("sigmav", "\u{3c2}"),
    ("sim", "\u{223c}"),
    ("simdot", "\u{2a6a}"),
    ("sime", "\u{2243}"),
    ("simeq", "\u{2243}"),
    ("simg", "\u{2a9e}"),
    ("simgE", "\u{2aa0}"),
    ("siml", "\u{2a9d}"),
    ("simlE", "\u{2a9f}"),
    ("simne", "\u{2246}"),
    ("simplus", "\u{2a24}"),
    ("simrarr", "\u{2972}"),
    ("slarr", "\u{2190}"),
    ("smallsetminus", "\u{2216}"),
    ("smashp", "\u{2a33}"),
    ("smeparsl", "\u{29e4}"),
    ("smid", "\u{2223}"),
    ("smile", "\u{2323}"),
    ("smt", "\u{2aaa}"),
    ("smte", "\u{2aac}"),
    ("smtes", "\u{2aac}\u{fe00}"),
    ("softcy", "\u{44c}"),
    ("sol", "/"),
    ("solb", "\u{29c4}"),
    ("solbar", "\u{233f}"),
    ("sopf", "\u{1d564}"),
    ("spades", "\u{2660}"),
    ("spadesuit", "\u{2660}"),
    ("spar", "\u{2225}"),
    ("sqcap", "\u{2293}"),
    ("sqcaps", "\u{2293}\u{fe00}"),
    ("sqcup", "\u{2294}"),
    ("sqcups", "\u{2294}\u{fe00}"),
    ("sqsub", "\u{228f}"),
    ("sqsube", "\u{2291}"),
    ("sqsubset", "\u{228f}"),
    ("sqsubseteq", "\u{2291}"),
    ("sqsup", "\u{2290}"),
    ("sqsupe", "\u{2292}"),
    ("sqsupset", "\u{2290}"),
    ("sqsupseteq", "\u{2292}"),
    ("squ", "\u{25a1}"),
    ("square", "\u{25a1}"),
    ("squarf", "\u{25aa}"),
    ("squf", "\u{25aa}"),
    ("srarr", "\u{2192}"),
    ("sscr", "\u{1d4c8}"),
    ("ssetmn", "\u{2216}"),
    ("ssmile", "\u{2323}"),
    ("sstarf", "\u{22c6}"),
    ("star", "\u{2606}"),
    ("starf", "\u{2605}"),
    ("straightepsilon", "\u{3f5}"),
    ("straightphi", "\u{3d5}"),
    ("strns", "\u{af}"),
    ("sub", "\u{2282}"),
    ("subE", "\u{2ac5}"),
    ("subdot", "\u{2abd}"),
    ("sube", "\u{2286}"),
    ("subedot", "\u{2ac3}"),
    ("submult", "\u{2ac1}"),
    ("subnE", "\u{2acb}"),
    ("subne", "\u{228a}"),
    ("subplus", "\u{2abf}"),
    ("subrarr", "\u{2979}"),
    ("subset", "\u{2282}"),
    ("subseteq", "\u{2286}"),
    ("subseteqq", "\u{2ac5}"),
    ("subsetneq", "\u{228a}"),
    ("subsetneqq", "\u{2acb}"),
    ("subsim", "\u{2ac7}"),
    ("subsub", "\u{2ad5}"),
    ("subsup", "\u{2ad3}"),
    ("succ", "\u{227b}"),
    ("succapprox", "\u{2ab8}"),
    ("succcurlyeq", "\u{227d}"),
    ("succeq", "\u{2ab0}"),
    ("succnapprox", "\u{2aba}"),
    ("succneqq", "\u{2ab6}"),
    ("succnsim", "\u{22e9}"),
    ("succsim", "\u{227f}"),
    ("sum", "\u{2211}"),
    ("sung", "\u{266a}"),
    ("sup", "\u{2283}"),
    ("sup1", "\u{b9}"),
    ("sup2", "\u{b2}"),
    ("sup3", "\u{b3}"),
    ("supE", "\u{2ac6}"),
    ("supdot", "\u{2abe}"),
    ("supdsub", "\u{2ad8}"),
    ("supe", "\u{2287}"),
    ("supedot", "\u{2ac4}"),
    ("suphsol", "\u{27c9}"),
    ("suphsub", "\u{2ad7}"),
    ("suplarr", "\u{297b}"),
    ("supmult", "\u{2ac2}"),
    ("supnE", "\u{2acc}"),
    ("supne", "\u{228b}"),
    ("supplus", "\u{2ac0}"),
    ("supset", "\u{2283}"),
    ("supseteq", "\u{2287}"),
    ("supseteqq", "\u{2ac6}"),
    ("supsetneq", "\u{228b}"),
    ("supsetneqq", "\u{2acc}"),
    ("supsim", "\u{2ac8}"),
    ("supsub", "\u{2ad4}"),
    ("supsup", "\u{2ad6}"),
    ("swArr", "\u{21d9}"),
    ("swarhk", "\u{2926}"),
    ("swarr", "\u{2199}"),
    ("swarrow", "\u{2199}"),
    ("swnwar", "\u{292a}"),
    ("szlig", "\u{df}"),
    ("target", "\u{2316}"),
    ("tau", "\u{3c4}"),
    ("tbrk", "\u{23b4}"),
    ("tcaron", "\u{165}"),
    ("tcedil", "\u{163}"),
    ("tcy", "\u{442}"),
    ("tdot", "\u{20db}"),
    ("telrec", "\u{2315}"),
    ("tfr", "\u{1d531}"),
    ("there4", "\u{2234}"),
    ("therefore", "\u{2234}"),
    ("theta", "\u{3b8}"),
    ("thetasym", "\u{3d1}"),
    ("thetav", "\u{3d1}"),
    ("thickapprox", "\u{2248}"),
    ("thicksim", "\u{223c}"),
    ("thinsp", "\u{2009}"),
    ("thkap", "\u{2248}"),
    ("thksim", "\u{223c}"),
    ("thorn", "\u{fe}"),
    ("tilde", "\u{2dc}"),
    ("times", "\u{d7}"),
    ("timesb", "\u{22a0}"),
    ("timesbar", "\u{2a31}"),
    ("timesd", "\u{2a30}"),
    ("tint", "\u{222d}"),
    ("toea", "\u{2928}"),
    ("top", "\u{22a4}"),
    ("topbot", "\u{2336}"),
    ("topcir", "\u{2af1}"),
    ("topf", "\u{1d565}"),
    ("topfork", "\u{2ada}"),
    ("tosa", "\u{2929}"),
    ("tprime", "\u{2034}"),
    ("trade", "\u{2122}"),
    ("triangle", "\u{25b5}"),
    ("triangledown", "\u{25bf}"),
    ("triangleleft", "\u{25c3}"),
    ("trianglelefteq", "\u{22b4}"),
    ("triangleq", "\u{225c}"),
    ("triangleright", "\u{25b9}"),
    ("trianglerighteq", "\u{22b5}"),
    ("tridot", "\u{25ec}"),
    ("trie", "\u{225c}"),
    ("triminus", "\u{2a3a}"),
    ("triplus", "\u{2a39}"),
    ("trisb", "\u{29cd}"),
    ("tritime", "\u{2a3b}"),
    ("trpezium", "\u{23e2}"),
    ("tscr", "\u{1d4c9}"),
    ("tscy", "\u{446}"),
    ("tshcy", "\u{45b}"),
    ("tstrok", "\u{167}"),
    ("twixt", "\u{226c}"),
    ("twoheadleftarrow", "\u{219e}"),
    ("twoheadrightarrow", "\u{21a0}"),
    ("uArr", "\u{21d1}"),
    ("uHar", "\u{2963}"),
    ("uacute", "\u{fa}"),
    ("uarr", "\u{2191}"),
    ("ubrcy", "\u{45e}"),
    ("ubreve", "\u{16d}"),
    ("ucirc", "\u{fb}"),
    ("ucy", "\u{443}"),
    ("udarr", "\u{21c5}"),
    ("udblac", "\u{171}"),
    ("udhar", "\u{296e}"),
    ("ufisht", "\u{297e}"),
    ("ufr", "\u{1d532}"),
    ("ugrave", "\u{f9}"),
    ("uharl", "\u{21bf}"),
    ("uharr", "\u{21be}"),
    ("uhblk", "\u{2580}"),
    ("ulcorn", "\u{231c}"),
    ("ulcorner", "\u{231c}"),
    ("ulcrop", "\u{230f}"),
    ("ultri", "\u{25f8}"),
    ("umacr", "\u{16b}"),
    ("uml", "\u{a8}"),
    ("uogon", "\u{173}"),
    ("uopf", "\u{1d566}"),
    ("uparrow", "\u{2191}"),
    ("updownarrow", "\u{2195}"),
    ("upharpoonleft", "\u{21bf}"),
    ("upharpoonright", "\u{21be}"),
    ("uplus", "\u{228e}"),
    ("upsi", "\u{3c5}"),
    ("upsih", "\u{3d2}"),
    ("upsilon", "\u{3c5}"),
    ("upuparrows", "\u{21c8}"),
    ("urcorn", "\u{231d}"),
    ("urcorner", "\u{231d}"),
    ("urcrop", "\u{230e}"),
    ("uring", "\u{16f}"),
    ("urtri", "\u{25f9}"),
    ("uscr", "\u{1d4ca}"),
    ("utdot", "\u{22f0}"),
    ("utilde", "\u{169}"),
    ("utri", "\u{25b5}"),
    ("utrif", "\u{25b4}"),
    ("uuarr", "\u{21c8}"),
    ("uuml", "\u{fc}"),
    ("uwangle", "\u{29a7}"),
    ("vArr", "\u{21d5}"),
    ("vBar", "\u{2ae8}"),
    ("vBarv", "\u{2ae9}"),
    ("vDash", "\u{22a8}"),
    ("vangrt", "\u{299c}"),
    ("varepsilon", "\u{3f5}"),
    ("varkappa", "\u{3f0}"),
    ("varnothing", "\u{2205}"),
    ("varphi", "\u{3d5}"),
    ("varpi", "\u{3d6}"),
    ("varpropto", "\u{221d}"),
    ("varr", "\u{2195}"),
    ("varrho", "\u{3f1}"),
    ("varsigma", "\u{3c2}"),
    ("varsubsetneq", "\u{228a}\u{fe00}"),
    ("varsubsetneqq", "\u{2acb}\u{fe00}"),
    ("varsupsetneq", "\u{228b}\u{fe00}"),
    ("varsupsetneqq", "\u{2acc}\u{fe00}"),
    ("vartheta", "\u{3d1}"),
    ("vartriangleleft", "\u{22b2}"),
    ("vartriangleright", "\u{22b3}"),
    ("vcy", "\u{432}"),
    ("vdash", "\u{22a2}"),
    ("vee", "\u{2228}"),
    ("veebar", "\u{22bb}"),
    ("veeeq", "\u{225a}"),
    ("vellip", "\u{22ee}"),
    ("verbar", "|"),
    ("vert", "|"),
    ("vfr", "\u{1d533}"),
    ("vltri", "\u{22b2}"),
    ("vnsub", "\u{2282}\u{20d2}"),
    ("vnsup", "\u{2283}\u{20d2}"),
    ("vopf", "\u{1d567}"),
    ("vprop", "\u{221d}"),
    ("vrtri", "\u{22b3}"),
    ("vscr", "\u{1d4cb}"),
    ("vsubnE", "\u{2acb}\u{fe00}"),
    ("vsubne", "\u{228a}\u{fe00}"),
    ("vsupnE", "\u{2acc}\u{fe00}"),
    ("vsupne", "\u{228b}\u{fe00}"),
    ("vzigzag", "\u{299a}"),
    ("wcirc", "\u{175}"),
    ("wedbar", "\u{2a5f}"),
    ("wedge", "\u{2227}"),
    ("wedgeq", "\u{2259}"),
    ("weierp", "\u{2118}"),
    ("wfr", "\u{1d534}"),
    ("wopf", "\u{1d568}"),
    ("wp", "\u{2118}"),
    ("wr", "\u{2240}"),
    ("wreath", "\u{2240}"),
    ("wscr", "\u{1d4cc}"),
    ("xcap", "\u{22c2}"),
    ("xcirc", "\u{25ef}"),
    ("xcup", "\u{22c3}"),
    ("xdtri", "\u{25bd}"),
    ("xfr", "\u{1d535}"),
    ("xhArr", "\u{27fa}"),
    ("xharr", "\u{27f7}"),
    ("xi", "\u{3be}"),
    ("xlArr", "\u{27f8}"),
    ("xlarr", "\u{27f5}"),
    ("xmap", "\u{27fc}"),
    ("xnis", "\u{22fb}"),
    ("xodot", "\u{2a00}"),
    ("xopf", "\u{1d569}"),
    ("xoplus", "\u{2a01}"),
    ("xotime", "\u{2a02}"),
    ("xrArr", "\u{27f9}"),
    ("xrarr", "\u{27f6}"),
    ("xscr", "\u{1d4cd}"),
    ("xsqcup", "\u{2a06}"),
    ("xuplus", "\u{2a04}"),
    ("xutri", "\u{25b3}"),
    ("xvee", "\u{22c1}"),
    ("xwedge", "\u{22c0}"),
    ("yacute", "\u{fd}"),
    ("yacy", "\u{44f}"),
    ("ycirc", "\u{177}"),
    ("ycy", "\u{44b}"),
    ("yen", "\u{a5}"),
    ("yfr", "\u{1d536}"),
    ("yicy", "\u{457}"),
    ("yopf", "\u{1d56a}"),
    ("yscr", "\u{1d4ce}"),
    ("yucy", "\u{44e}"),
    ("yuml", "\u{ff}"),
    ("zacute", "\u{17a}"),
    ("zcaron", "\u{17e}"),
    ("zcy", "\u{437}"),
    ("zdot", "\u{17c}"),
    ("zeetrf", "\u{2128}"),
    ("zeta", "\u{3b6}"),
    ("zfr", "\u{1d537}"),
    ("zhcy", "\u{436}"),
    ("zigrarr", "\u{21dd}"),
    ("zopf", "\u{1d56b}"),
    ("zscr", "\u{1d4cf}"),
    ("zwj", "\u{200d}"),
    ("zwnj", "\u{200c}"),
];

/// The text a named HTML character reference (the entity name between `&` and `;`,
/// exclusive of both) resolves to, if [`NAMED_CHARACTER_REFERENCES`] lists it.
fn named_character_reference(entity: &str) -> Option<&'static str> {
    let index = NAMED_CHARACTER_REFERENCES
        .binary_search_by_key(&entity, |&(name, _)| name)
        .ok()?;
    NAMED_CHARACTER_REFERENCES
        .get(index)
        .map(|&(_, value)| value)
}

/// The character a numeric HTML character reference starting at `after_amp` (the text
/// immediately following the `&`) resolves to, along with how many of `after_amp`'s own
/// bytes it consumes — the `#` and any `x`/`X` prefix, the digits themselves, and a
/// terminating `;` only if one is actually there.
///
/// HTML5's own tokenizer never requires the semicolon on a numeric reference the way it
/// does on a named one (Codex, pull request #138, round 53, "Decode numeric references
/// without semicolons"): the "Numeric character reference end state" resolves and emits
/// a character as soon as a non-digit byte (or the end of input) is reached, semicolon
/// or not — omitting one is merely a parse error a browser still recovers from, not a
/// reason to leave the reference undecoded. `tests&#47spine.rs` therefore still resolves
/// to `tests/spine.rs`, the digits `47` ending where the non-digit `s` begins. A *named*
/// reference's own semicolon-optional form is a different, narrower thing —
/// [`NAMED_CHARACTER_REFERENCES`]'s own doc comment says why this decoder does not carry
/// it — and this function's own scan for digits never mistakes one for the other, since
/// a numeric reference always starts with a literal `#`.
fn numeric_character_reference_at(after_amp: &str) -> Option<(char, usize)> {
    let rest = after_amp.strip_prefix('#')?;
    let (hex, digits) = rest
        .strip_prefix('x')
        .or_else(|| rest.strip_prefix('X'))
        .map_or((false, rest), |hex_digits| (true, hex_digits));
    let digit_len = digits
        .find(|character: char| {
            !(if hex {
                character.is_ascii_hexdigit()
            } else {
                character.is_ascii_digit()
            })
        })
        .unwrap_or(digits.len());
    if digit_len == 0 {
        return None;
    }
    let value = if hex {
        u32::from_str_radix(&digits[..digit_len], 16).ok()?
    } else {
        digits[..digit_len].parse::<u32>().ok()?
    };
    let character = char::from_u32(value)?;
    let mut consumed = after_amp.len() - digits.len() + digit_len;
    if digits[digit_len..].starts_with(';') {
        consumed += 1;
    }
    Some((character, consumed))
}

/// Hides every `<!--` ... `-->` span inside one `Tag::HtmlBlock` spanning `start` to
/// `block_end`, into `hidden`. Extracted from [`visible_source`] only to stay under
/// clippy's line limit, the same reason [`hide_non_rendering_in_html_line`] and
/// [`hide_non_rendering_in_inline_html`] were.
///
/// A comment nested inside a block that opens with a real tag —
/// `<div>\n<!-- [x](y) -->\n</div>` — is still a comment, and the whole block does
/// not start with `<!--` for that reason (Codex, pull request #138, round 13). One
/// `HtmlBlock` event covers the whole block, real tags and nested comments alike, so
/// each `<!--` ... `-->` span inside it is hidden on its own rather than requiring
/// the block to be nothing but a comment.
///
/// The close is searched for from the opener to the end of the whole document, not
/// only to the end of this block (Codex, round 14): an unterminated `<!--` is not
/// rendered, so everything after it is as hidden as a matched comment's body,
/// matching the fail-closed behaviour `without_html_comments` already had for this
/// case. HTML comments do not nest, so the first `-->` found always closes the
/// `<!--` before it.
///
/// Found through `find_comment_opener`, not a raw substring search (Codex, pull
/// request #138, round 40, finding 2): a real, complete tag inside the block whose
/// own quoted attribute merely spells `<!--` — `<div title="<!--">note</div>` — is
/// text a browser renders as an attribute value, not a comment, and a raw search
/// would hide everything from there to end of document over a decoy that was never
/// a real opener. Bounded to `block_end` the same way the old search was, by
/// slicing the search text there.
fn hide_html_block_comments(
    contents: &str,
    start: usize,
    block_end: usize,
    hidden: &mut Vec<(usize, usize)>,
) {
    let mut cursor = start;
    while let Some(open) = find_comment_opener(&contents[..block_end], cursor) {
        let end = find_comment_close(contents, open).unwrap_or(contents.len());
        hidden.push((open, end));
        if end >= block_end {
            break;
        }
        cursor = end;
    }
}

/// `contents` with every fenced code block, blockquote and HTML comment removed,
/// keeping the exact source bytes of everything else — link syntax, code span
/// backticks, real (non-comment) HTML, and all.
///
/// [`markdown_prose`] answers "what does a reader see", which is the wrong question
/// for a rule that needs raw Markdown syntax rather than rendered text: a link's
/// destination, `[label](target)`, is exactly what rendered prose strips down to its
/// visible label. This answers "what raw source is not inside one of these three
/// containers" instead, by finding their spans structurally — with `pulldown-cmark`'s
/// own byte offsets, not by re-deriving them from rendered output — and cutting
/// those spans out of the original text. A span nested inside another already-hidden
/// span is skipped rather than double-hidden, so a fence inside a blockquote (or the
/// reverse) removes it once. Structural, not textual: nesting any of the three inside
/// any other cannot defeat this the way composing two independent line scanners
/// could, because there is one parse producing one consistent set of spans rather
/// than two scans that disagree about what is inside what.
///
/// Real HTML is not one of the three: `<a href="target">label</a>` is a link a
/// reader (and a renderer) sees, not an example, so only HTML that is actually a
/// comment is hidden.
///
/// A non-rendering (`<script>`, `<style>`, `<title>`, `<template>` or `<iframe>`) or `hidden`-suppressed
/// element's body is a fourth thing hidden (Codex, pull request #138, round 45,
/// "Suppress non-rendering blocks in `visible_source`"): a Markdown link written
/// inside `<script>[one](0001-one.md)</script>`, or a wire-format path named inside a
/// `hidden` container, is never rendered, but this function's callers read raw syntax
/// rather than rendered output and could not otherwise tell the difference. Tracked
/// per `Event::Html` line by `hide_non_rendering_in_html_line`, below, which keeps an
/// ordinary tag's own markup untouched — `<a href="target">` included — for the same
/// reason only a genuine comment is hidden above: the raw `href=` syntax is exactly
/// what a caller like `linked_markdown_files` or the wire-format path check reads for.
#[must_use]
pub fn visible_source(contents: &str) -> String {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

    let mut hidden: Vec<(usize, usize)> = Vec::new();
    let mut fence_start: Option<usize> = None;
    let mut quote_start: Option<usize> = None;
    let mut quote_depth: u32 = 0;
    let mut html_block_start: Option<usize> = None;
    // The non-rendering/`hidden` elements open, and where the outermost began — one
    // span per outermost element, deferred until its matching close resolves. See this
    // function's own doc comment for why.
    let mut open_non_rendering: Vec<String> = Vec::new();
    let mut non_rendering_start: Option<usize> = None;
    let mut pending_tag: Option<PendingTag> = None;
    let mut pending_raw_text_close: Option<PendingRawTextClose> = None;
    // How many `<svg>`/`<math>` foreign-content roots are currently open, carried
    // across both `Event::Html` lines and `Event::InlineHtml` constructs the same way
    // `open_non_rendering` is (Codex, pull request #138, round 49, "Avoid pushing
    // self-closing scripts in foreign content"; round 50, "Carry foreign-content depth
    // across raw HTML lines") — see `track_non_rendering_html`'s own doc comment.
    let mut foreign_content: Vec<ForeignFrame> = Vec::new();
    // Ordinary elements genuinely known to be open outside any tracked non-rendering
    // element, carried across `Event::Html` lines the same way `open_non_rendering`
    // and `foreign_content` are (Codex, pull request #138, round 56, "Ignore closes
    // that do not match a real ancestor").
    let mut ancestors: Vec<String> = Vec::new();
    let mut in_cdata = false; // Codex, round 57: carried the same way, see `OpenConstructState`.
    // A comment nested inside an open `<template>`, kept apart from the block-comment
    // search below: that is a document-wide search over already-*closed* blocks, not a
    // state a currently open nesting element carries across lines.
    let mut in_template_comment = false;
    for (event, range) in Parser::new_ext(contents, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                // Only fenced blocks, matching markdown_prose's own distinction: an
                // indented block is not a construct either of these rules quotes an
                // example in, and hiding one would blind the scan to prose it reads.
                if matches!(kind, CodeBlockKind::Fenced(_)) {
                    fence_start.get_or_insert(range.start);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = fence_start.take() {
                    hidden.push((start, range.end));
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                if quote_depth == 0 {
                    quote_start = Some(range.start);
                }
                quote_depth = quote_depth.saturating_add(1);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                quote_depth = quote_depth.saturating_sub(1);
                if quote_depth == 0
                    && let Some(start) = quote_start.take()
                {
                    hidden.push((start, range.end));
                }
            }
            // A block-level HTML comment is a `Tag::HtmlBlock` whose span covers the
            // whole thing; the leaf `Event::Html`s inside it are one per source line
            // and are not read directly, the same way a fence's inner lines are not.
            // Only a *comment* is hidden — `<a href="...">label</a>` is real, visible
            // HTML a reader (and a renderer) sees, not an example, and hiding it would
            // remove a legitimate link's destination along with its label.
            Event::Start(Tag::HtmlBlock) => {
                html_block_start.get_or_insert(range.start);
            }
            Event::End(TagEnd::HtmlBlock) => {
                if let Some(start) = html_block_start.take() {
                    hide_html_block_comments(contents, start, range.end, &mut hidden);
                }
            }
            // Inline HTML has no enclosing tag, so a self-contained inline comment,
            // `text <!-- ... --> text`, is judged from its own event text instead.
            Event::InlineHtml(html) if html.starts_with("<!--") => {
                hidden.push((range.start, range.end));
            }
            // One raw HTML line — real block-level HTML passthrough, `pulldown-cmark`
            // firing one `Event::Html` per source line, the same as every other
            // function in this module reads it.
            Event::Html(html) => {
                hide_non_rendering_in_html_line(
                    &html,
                    range.start,
                    &mut open_non_rendering,
                    &mut non_rendering_start,
                    &mut pending_tag,
                    &mut pending_raw_text_close,
                    &mut in_template_comment,
                    &mut foreign_content,
                    &mut ancestors,
                    &mut in_cdata,
                    &mut hidden,
                );
            }
            // A non-rendering or `hidden`-suppressed element can open or close
            // inline, mid-paragraph, too — sharing the same stack a block-level
            // `<script>` or `<template>` already pushed onto, or pushing one of its
            // own for a later block-level close to find.
            Event::InlineHtml(html) => hide_non_rendering_in_inline_html(
                &html,
                range,
                &mut open_non_rendering,
                &mut non_rendering_start,
                &mut foreign_content,
                &mut ancestors,
                &mut hidden,
            ),
            _ => {}
        }
    }
    // A non-rendering or `hidden` element that never closes hides everything after it,
    // matching this function's own unterminated-comment handling above: a browser
    // never leaves script-data (or template, or `hidden`) parsing once it enters it
    // with nothing left in the document to end it.
    if let Some(start) = non_rendering_start {
        hidden.push((start, contents.len()));
    }
    hidden.sort_unstable();

    let mut out = String::with_capacity(contents.len());
    let mut cursor = 0usize;
    for (start, end) in hidden {
        if start < cursor {
            // Overlapping, not necessarily nested (Codex, pull request #138, round 17):
            // a comment opened inside a blockquote and closed after it — or never
            // closed at all — reaches past the blockquote's own span, and merely
            // skipping it here would forget that reach and let the quote's own
            // (shorter) end stand in as the cursor, exposing everything from there to
            // this span's real end. The two are merged instead: the span itself is
            // already covered, so nothing new is cut out of `out`, but the end still
            // advances the cursor if it reaches further.
            cursor = end.max(cursor);
            continue;
        }
        out.push_str(&contents[cursor..start]);
        // A fusion guard, for the reason `without_html_comments` inserts one: text
        // on either side of a removed span must not read as one token.
        out.push(' ');
        cursor = end.max(cursor);
    }
    out.push_str(&contents[cursor..]);
    out
}

/// [`visible_source`]'s twin of [`track_non_rendering_html`] — the inline construct
/// can open or close a non-rendering/`hidden` element mid-paragraph the same way a
/// block-level tag does, and this is where that transition becomes a hidden span:
/// `range` is hidden in full when the construct closes the outermost element (or is
/// itself the whole thing, opening and closing in one self-contained construct), and
/// `non_rendering_start` is armed rather than pushed yet when it only opens one.
/// Extracted from [`visible_source`] for the same clippy-line-limit reason
/// [`hide_non_rendering_in_html_line`] was.
fn hide_non_rendering_in_inline_html(
    html: &str,
    range: std::ops::Range<usize>,
    open_non_rendering: &mut Vec<String>,
    non_rendering_start: &mut Option<usize>,
    foreign_content: &mut Vec<ForeignFrame>,
    ancestors: &mut Vec<String>,
    hidden: &mut Vec<(usize, usize)>,
) {
    let was_open = !open_non_rendering.is_empty();
    track_non_rendering_html(html, open_non_rendering, foreign_content, ancestors);
    let now_open = !open_non_rendering.is_empty();
    if !was_open && now_open {
        non_rendering_start.get_or_insert(range.start);
    } else if was_open
        && !now_open
        && let Some(start) = non_rendering_start.take()
    {
        hidden.push((start, range.end));
    }
}

/// Hides a non-rendering or `hidden`-suppressed element's body from `line`, extracted
/// from [`visible_source`] itself only to stay under clippy's line limit (Codex, pull
/// request #138, round 45, the same reason `append_visible_html`,
/// `track_non_rendering_html` and `resolve_pending_tag` were pulled out). Reuses the
/// block-level tracking every other function in this module shares
/// (`next_hiding_marker`, `advance_past_non_rendering`, `resolve_pending_tag`) but only
/// for the *span* — an ordinary tag's own markup is left untouched, `href="..."`
/// included, unlike [`visible_html_ranges`], because a real raw anchor's destination is
/// exactly the syntax [`visible_source`]'s callers read for.
#[expect(
    clippy::too_many_arguments,
    reason = "one flag per piece of state visible_source already carries across lines \
              for its own top-level comment search; bundling them loses the ability to \
              read each mutation at its own call site"
)]
fn hide_non_rendering_in_html_line(
    html: &str,
    line_start: usize,
    open_non_rendering: &mut Vec<String>,
    non_rendering_start: &mut Option<usize>,
    pending_tag: &mut Option<PendingTag>,
    pending_raw_text_close: &mut Option<PendingRawTextClose>,
    in_template_comment: &mut bool,
    foreign_content: &mut Vec<ForeignFrame>,
    ancestors: &mut Vec<String>,
    in_cdata: &mut bool,
    hidden: &mut Vec<(usize, usize)>,
) {
    let mut cursor = if let Some(pending) = pending_raw_text_close.take() {
        let mut quote = pending.quote;
        let Some(end) = scan_tag_close(html, 0, &mut quote) else {
            *pending_raw_text_close = Some(PendingRawTextClose { quote });
            return;
        };
        open_non_rendering.pop();
        end
    } else {
        0
    };
    if let Some(pending) = pending_tag.take() {
        match resolve_pending_tag(html, open_non_rendering, foreign_content, pending) {
            Ok(end) => cursor = end,
            Err(unresolved) => {
                *pending_tag = Some(unresolved);
                return;
            }
        }
    }
    if open_non_rendering.is_empty()
        && let Some(start) = non_rendering_start.take()
    {
        hidden.push((start, line_start + cursor));
    }
    loop {
        // A CDATA section left open by an earlier line (Codex, pull request #138,
        // round 57, "Carry foreign CDATA across lines") is opaque character data,
        // never real markup — consumed rather than scanned, so a stray construct
        // inside its payload is never misread as a real comment or tag.
        if *in_cdata {
            match html.get(cursor..).and_then(|rest| rest.find("]]>")) {
                Some(offset) => {
                    cursor += offset + "]]>".len();
                    *in_cdata = false;
                    continue;
                }
                None => break,
            }
        }
        if !open_non_rendering.is_empty() {
            match advance_past_non_rendering(
                html,
                cursor,
                open_non_rendering,
                in_template_comment,
                pending_tag,
                pending_raw_text_close,
                &mut NestedHtmlContext {
                    foreign_content,
                    ancestors,
                },
            ) {
                Some(end) => {
                    if open_non_rendering.is_empty()
                        && let Some(start) = non_rendering_start.take()
                    {
                        hidden.push((start, line_start + end));
                    }
                    cursor = end;
                    continue;
                }
                None => break,
            }
        }
        match next_hiding_marker(html, cursor, foreign_content) {
            None => break,
            // Skipped rather than hidden here: this function's caller has its own
            // block-comment search, covering every comment in the block from its own
            // opener to its real close (or to end of document), so this only has to
            // step past it correctly rather than re-decide it.
            Some(HidingMarker::Comment(start)) => match find_comment_close(html, start) {
                Some(end) => cursor = end,
                None => break,
            },
            Some(HidingMarker::Tag(start, end, tag)) => {
                non_rendering_start.get_or_insert(line_start + start);
                open_non_rendering.push(tag.to_owned());
                cursor = end;
            }
            Some(HidingMarker::Hidden(start, end, name)) => {
                non_rendering_start.get_or_insert(line_start + start);
                open_non_rendering.push(name);
                cursor = end;
            }
            // An ordinary tag's own markup — kept verbatim by this function's
            // caller, `href="..."` included — so there is nothing to hide here, only
            // to step past.
            Some(HidingMarker::Markup(start, end)) => {
                track_foreign_content_depth(&html[start..end], foreign_content);
                track_ordinary_ancestor(&html[start..end], foreign_content, ancestors);
                cursor = end;
            }
            // A CDATA section's payload is real, reader-visible text, exactly like
            // an ordinary tag's own markup is kept verbatim by this function's
            // caller — nothing here to hide either, only to step past.
            Some(HidingMarker::Cdata(_, _, _, close_end)) => {
                cursor = close_end;
            }
            Some(HidingMarker::CdataOpen(..)) => {
                *in_cdata = true;
                break;
            }
        }
    }
}

/// The text after `prefix` in the first non-hidden list item marked with a literal `-`.
///
/// `unordered_list_item_value(contents, "Status:")` reads an ADR's `- Status: accepted`
/// as `"accepted"`. Reads the parser's own `Tag::Item` events rather than scanning
/// [`markdown_prose`]'s
/// rendered lines, for the limit stated there: an escaped marker like `\- Status: accepted`
/// unescapes to text that reads like a bullet once rendered, and a line scan over that
/// output cannot tell it from a real list item — this can, because the escaped line
/// produces no `Tag::Item` event at all, only paragraph text (issue #82's continuation).
/// Fenced code blocks and blockquotes are hidden, and HTML — a comment included — is
/// dropped by the parser itself, for [`markdown_prose`]'s reason: a decoy value shown as a
/// worked example, or hidden away, must not out-rank the real one.
///
/// The marker itself is read back from the source at the item's own range, the way
/// [`markdown_prose`] reads it, rather than assumed: an ADR field is always written `- `,
/// so an example deliberately marked `*` or `1.` to read as a worked case rather than the
/// real field stays a decoy, exactly as it would once rendered.
///
/// Only the item's own text is read, so a value split across a hard break or held in a
/// nested block is not reconstructed — an ADR field is one line, and that is what this is
/// for.
#[must_use]
pub fn unordered_list_item_value(contents: &str, prefix: &str) -> Option<String> {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

    let mut in_fence = false;
    let mut blockquote_depth: u32 = 0;
    let mut collecting = false;
    // Whether the item's one legitimate paragraph has already closed (Codex, pull
    // request #138, round 16): a loose item — `- Status:` followed by a blank line and
    // an indented `accepted` paragraph — is two `Tag::Paragraph`s with no `SoftBreak`
    // between them at all, so neither the line-break nor the hidden-container check
    // catches it. An ADR field is one paragraph, so anything opening after the first
    // one closes disqualifies the item, whatever kind of block it is.
    let mut paragraph_closed = false;
    let mut item = String::new();
    // Whether a non-rendering or `hidden`-suppressed element opened somewhere in the
    // item being collected (Codex, pull request #138, round 49, "Preserve complete
    // fields before trailing hidden markup"): unlike a real line break or a fenced
    // block, such an element's own span carries no text a reader ever sees, so its
    // own opening alone says nothing about whether the field is still complete —
    // only whether the stripped value comes out empty once it is excluded does, and
    // that can only be answered once the item's own text has all been gathered.
    let mut hidden_markup_seen = false;
    // Whether an HTML comment opened earlier is still open (Codex, pull request #138,
    // round 20): `pulldown-cmark` ends an `HtmlBlock` at a blank line even when a
    // comment inside it never closed, so an item appearing right after reads as an
    // ordinary, visible one — structurally separate from the comment, but still
    // inside it by real HTML rules until an actual `-->` appears. `append_visible_html_line`
    // is reused for the state transition alone: called with `hidden: true`, it never
    // writes anywhere; called for the state transition alone.
    let mut in_html_comment = false;
    let mut in_cdata = false; // Codex, round 57: carried the same way, see `OpenConstructState`.
    // The non-rendering elements (`<script>`, `<style>`, `<title>`, `<template>` or `<iframe>`)
    // currently open at block level, innermost last, if any (Codex, pull request #138,
    // round 30): `<div>\n<script>\n\n- Status: accepted\n\n</script>\n</div>` outlives
    // its own `HtmlBlock` across the blank line the same way an unterminated comment
    // does — `pulldown-cmark` ends the block there and resumes the decoy item as an
    // ordinary, structurally separate one, even though a browser is still in
    // script-data state until the real `</script>` two lines later. Folded into
    // `hidden` below so `Start(Tag::Item)` never begins collecting such an item at
    // all, the same disqualification a fence or blockquote already gets. A real stack
    // since round 33: a nested `<template>` genuinely opens a second context, unlike
    // `<script>`/`<style>`/`<title>`'s raw-text parsing, and either of those can itself
    // nest inside a `<template>` and needs its own level tracked.
    let mut open_non_rendering_tag: Vec<String> = Vec::new();
    // A tag whose own closing `>` had not yet appeared when its line ran out
    // (Codex, pull request #138, round 41, finding 1), carried across `Event::Html`
    // lines the same way `in_html_comment` and `open_non_rendering_tag` already are.
    let mut pending_tag: Option<PendingTag> = None;
    // A raw-text element's own close tag name matched but its terminating `>` had not
    // yet appeared (Codex, pull request #138, round 45, "Finish multiline raw-text
    // close tags before popping"), carried the same way `pending_tag` is.
    let mut pending_raw_text_close: Option<PendingRawTextClose> = None;
    // How many `<svg>`/`<math>` foreign-content roots are currently open, carried
    // across `Event::InlineHtml` constructs the same way `open_non_rendering_tag` is
    // (Codex, pull request #138, round 49, "Avoid pushing self-closing scripts in
    // foreign content") — see `track_non_rendering_html`'s own doc comment.
    let mut foreign_content: Vec<ForeignFrame> = Vec::new();
    // Ordinary elements genuinely known to be open outside any tracked non-rendering
    // element (Codex, pull request #138, round 56, "Ignore closes that do not match
    // a real ancestor") — see `track_ordinary_ancestor`'s own doc comment.
    let mut ancestors: Vec<String> = Vec::new();

    for (event, range) in Parser::new_ext(contents, Options::empty()).into_offset_iter() {
        let hidden = in_fence
            || blockquote_depth > 0
            || in_html_comment
            || !open_non_rendering_tag.is_empty();
        if collecting && paragraph_closed && matches!(event, Event::Start(_)) {
            collecting = false;
        }
        match event {
            Event::Html(html) => {
                visible_html_ranges(
                    &html,
                    &mut OpenConstructState {
                        in_html_comment: &mut in_html_comment,
                        in_cdata: &mut in_cdata,
                    },
                    &mut open_non_rendering_tag,
                    &mut pending_tag,
                    &mut pending_raw_text_close,
                    &mut foreign_content,
                    &mut ancestors,
                );
            }
            // A fenced block or blockquote opening while an item is being collected
            // disqualifies it, the same way a line break does (Codex, pull request
            // #138, round 15): `- Status:` followed by a nested fenced or quoted
            // `accepted` is hidden content standing in for the field's value, and
            // simply skipping the hidden text (rather than dropping the item) would
            // leave `item` holding just `Status:`, which still strips to an empty —
            // and therefore still matching — value.
            Event::Start(Tag::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_add(1);
                collecting = false;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_sub(1);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                if matches!(kind, CodeBlockKind::Fenced(_)) {
                    in_fence = true;
                    collecting = false;
                }
            }
            Event::End(TagEnd::CodeBlock) => in_fence = false,
            Event::Start(Tag::Item) if !hidden => {
                let marker = contents[range.start..].chars().next();
                if marker == Some('-') {
                    collecting = true;
                    paragraph_closed = false;
                    hidden_markup_seen = false;
                    item.clear();
                }
            }
            Event::End(TagEnd::Paragraph) if collecting => paragraph_closed = true,
            Event::End(TagEnd::Item) => {
                if collecting {
                    collecting = false;
                    if let Some(value) = item.strip_prefix(prefix) {
                        let value = value.trim();
                        // A hidden element's own span never contributes text a
                        // reader sees, so its opening alone does not disqualify a
                        // field that was already complete before it (Codex, round
                        // 49) — only an *empty* result does, the same failure
                        // round 30 closed by disqualifying outright: `- Status:
                        // <script>accepted</script>` strips to an empty value that
                        // would otherwise still trivially match.
                        if !(hidden_markup_seen && value.is_empty()) {
                            return Some(value.to_owned());
                        }
                    }
                }
            }
            // Never scanned for a comment marker (Codex, pull request #138, round 28,
            // correcting rounds 21/22): ordinary Markdown text is always HTML-escaped
            // when rendered, so a `-->` reaching `Event::Text` never survives as three
            // unescaped bytes in the rendered HTML a browser parses — verified with a
            // throwaway render, where `-->` in prose renders to `--&gt;`. A comment a
            // genuine, unescaped `<!--` opened therefore stays open through any amount
            // of later `Event::Text`, exactly as it would in a browser; only
            // `Event::Html` can close it, which `hidden` (folding in `in_html_comment`)
            // already reflects here.
            Event::Text(text) if collecting && !hidden => item.push_str(&text),
            Event::Code(code) if collecting && !hidden => {
                item.push('`');
                item.push_str(&code);
                item.push('`');
            }
            // A line break inside the item disqualifies it (Codex, pull request #138,
            // round 13): the text before and after one are two separate `Event::Text`
            // events, and concatenating them bare reconstructs a one-line-looking field
            // out of a value split across a line on purpose — `- Sta\n  tus: accepted`
            // becomes `Status: accepted` with the break silently dropped. An ADR field
            // is one line, so a break means this item is not one; `collecting` drops
            // rather than being kept for `End(TagEnd::Item)` to still try matching what
            // was gathered before the break.
            Event::SoftBreak | Event::HardBreak if collecting => collecting = false,
            // A tag that itself renders as a line break disqualifies the item (Codex,
            // round 26, narrowing round 25): `<br>` is not invisible the way a comment
            // is, so `- Sta<br>tus: accepted` renders as two lines even though its
            // *source* is one, and concatenating the surrounding `Text` fragments bare
            // reconstructs `Status: accepted` out of a field a reader never sees as one
            // line. Ordinary inline formatting — `<span>`, `<time>`, `<b>`, their
            // closing tags — renders inline with no break at all and is *not*
            // disqualifying: `- Status: <span>accepted</span>` is a real, complete,
            // one-line field, and round 25's blanket "any non-comment tag" rule
            // rejected it along with every other harmless tag.
            //
            // A same-line inline comment carries no newline of its own and is not
            // disqualifying either (Codex, round 24): `- Status: accepted <!--
            // rationale -->` is a real, complete, one-line field with a trailing note,
            // and disqualifying it discarded a value that had already been fully
            // collected before the comment appeared. A comment that itself spans more
            // than one source line still disqualifies (Codex, round 23):
            // `pulldown-cmark` collapses a multi-line comment into a single
            // `InlineHtml` event with no `SoftBreak` around it at all, so `- Sta<!--\n
            // -->tus: accepted` reaches `Event::Text` as two fragments with nothing
            // between them to say the source ever broke there. The comment's own text
            // is never appended to `item` in any of these cases, matching how a
            // comment is invisible everywhere else in this module — only whether it
            // disqualifies the item differs. `Html` block, appearing only for a
            // comment that outlived its own block, is excluded because it is handled
            // separately above and cannot occur while a paragraph is still open.
            //
            // A non-rendering element — `<script>`, `<style>`, `<title>`, `<template>` or `<iframe>` —
            // marks the item rather than disqualifying it outright (Codex, round 30,
            // narrowed round 49, "Preserve complete fields before trailing hidden
            // markup"): `- Status: <script>accepted</script>` has a real, visible
            // `Status: ` prefix ahead of the tag, so merely *excluding* the hidden
            // value (rather than recording that one was excluded) would leave `item`
            // at `"Status: "`, which still strips and trims to an empty value that
            // still trivially matches — exactly the failure round 15 already closed
            // for a nested fence or blockquote. But `- Status: accepted <span
            // hidden></span>` is a real, complete, one-line field with nothing but an
            // empty, trailing hidden element after it — round 30's own fix
            // disqualified this too, discarding a value that had already been fully
            // collected, the identical shape of overreach round 24 corrected for a
            // trailing comment. `hidden_markup_seen` records that a hidden element
            // was involved without dropping `collecting` outright; `End(TagEnd::Item)`
            // is what tells complete from incomplete, since only it has the item's
            // whole text to strip and check for emptiness. Checked by the tag
            // *opening* alone — a stray, unmatched close needs no separate case,
            // since the flag is sticky for the rest of the item either way.
            //
            // A comment is checked first and exclusively (Codex, round 35, finding 2):
            // `opens_non_rendering_element`/`is_line_break_tag` read `html`'s raw text
            // for a tag *spelling*, not a real tag, so a self-contained inline comment
            // whose own text merely contains one — `- Status: accepted <!-- <script>
            // example -->` — matched `opens_non_rendering_element` on the comment's
            // contents and disqualified a real, complete field, the same class of bug
            // `track_non_rendering_html` was given a comment-first check to close
            // (round 33, finding 3). A comment can only ever disqualify by spanning a
            // line break; it is never itself a real opening tag or a `<br>`.
            //
            // `track_non_rendering_html` is now called unconditionally, the way
            // `markdown_prose`, `heading_lines` and `table_rows` already call it for
            // every `Event::InlineHtml` (Codex, pull request #138, round 41, finding
            // 2): this function used to decide disqualification from
            // `opens_non_rendering_element(&html)` alone, without ever pushing the tag
            // onto `open_non_rendering_tag` — so `- Note: <script>`, opened inline and
            // left unclosed across a blank line into a second item, disqualified only
            // *that* item, and the next one, `- Status: accepted`, began collecting
            // fresh with the stack still empty, even though a browser is still in
            // script-data state until the real `</script>` two items later. The
            // returned `opened` is `true` exactly when this construct just pushed a
            // genuine non-rendering opener — the same condition
            // `opens_non_rendering_element(&html).is_some()` checked before, now with
            // the side effect that actually closes the gap. It sets
            // `hidden_markup_seen` rather than dropping `collecting` directly (round
            // 49): unlike `<br>` and a spanning comment, which are real, rendered
            // breaks nothing can undo, a non-rendering element's own opening says
            // nothing about completeness on its own.
            Event::InlineHtml(html)
                if advance_list_item_inline_html(
                    &html,
                    collecting,
                    &mut open_non_rendering_tag,
                    &mut foreign_content,
                    &mut ancestors,
                    &mut hidden_markup_seen,
                ) =>
            {
                collecting = false;
            }
            _ => {}
        }
    }
    None
}

/// Applies one `Event::InlineHtml` construct to a list item [`unordered_list_item_value`]
/// is collecting, returning whether it disqualifies the item (a spanning comment or a
/// tag that itself renders as a line break, exactly as that function's own
/// `Event::InlineHtml` handling already documented). Extracted only to stay under
/// clippy's line limit, the same reason `hide_html_block_comments` was pulled out of
/// `visible_source`.
fn advance_list_item_inline_html(
    html: &str,
    collecting: bool,
    open_non_rendering_tag: &mut Vec<String>,
    foreign_content: &mut Vec<ForeignFrame>,
    ancestors: &mut Vec<String>,
    hidden_markup_seen: &mut bool,
) -> bool {
    let opened = track_non_rendering_html(html, open_non_rendering_tag, foreign_content, ancestors);
    if opened {
        *hidden_markup_seen = true;
    }
    collecting
        && if html.starts_with("<!--") {
            html.contains('\n')
        } else {
            is_line_break_tag(html)
        }
}

/// Every real, visible Markdown heading in `contents`, rendered `"#".repeat(level) + "
/// " + text` — the same convention [`markdown_prose`] renders one with — in source
/// order.
///
/// Reads the parser's own heading events rather than scanning `markdown_prose`'s
/// rendered output for a line starting with `#` (Codex, pull request #138, round 34):
/// that is how a real heading renders, but it is also exactly how a literal `# Decoy`
/// line *inside a raw HTML block* renders, since `markdown_prose` preserves real,
/// non-comment HTML content unchanged (round 18) — and a browser shows that line as a
/// literal hash character, not a semantic `<h1>` or `<h2>`. Reading the event a real ATX
/// heading produces, rather than text-matching the convention `markdown_prose` happens
/// to render it with, cannot be fooled by the collision; comparing a caller's wanted
/// line (`"# Title"`, `"## Context"`) against this list rather than against
/// `markdown_prose`'s raw output is what closes it, at every level a heading can be
/// written at, not only the first. Fenced code blocks, blockquotes, HTML comments and
/// non-rendering elements are all hidden, for the reasons `markdown_prose` already
/// hides each — inline ones (`## <script>Context</script>`) included (Codex, pull
/// request #138, round 35, finding 1): unlike every other collector in this module,
/// the first version of this function had no `Event::InlineHtml` arm at all, so an
/// inline non-rendering tag never reached `open_non_rendering_tag`, and the `Context`
/// text between its open and close tags — ordinary `Event::Text`, invisible to a reader
/// — was collected as if the heading had rendered it.
#[must_use]
pub fn heading_lines(contents: &str) -> Vec<String> {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

    let mut in_fence = false;
    let mut blockquote_depth: u32 = 0;
    let mut in_html_comment = false;
    let mut in_cdata = false; // Codex, round 57: carried the same way, see `OpenConstructState`.
    let mut open_non_rendering_tag: Vec<String> = Vec::new();
    // A tag whose own closing `>` had not yet appeared when its line ran out
    // (Codex, pull request #138, round 41, finding 1), carried across `Event::Html`
    // lines the same way `in_html_comment` and `open_non_rendering_tag` already are.
    let mut pending_tag: Option<PendingTag> = None;
    // A raw-text element's own close tag name matched but its terminating `>` had not
    // yet appeared (Codex, pull request #138, round 45, "Finish multiline raw-text
    // close tags before popping"), carried the same way `pending_tag` is.
    let mut pending_raw_text_close: Option<PendingRawTextClose> = None;
    // How many `<svg>`/`<math>` foreign-content roots are currently open, carried
    // across `Event::InlineHtml` constructs the same way `open_non_rendering_tag` is
    // (Codex, pull request #138, round 49, "Avoid pushing self-closing scripts in
    // foreign content") — see `track_non_rendering_html`'s own doc comment.
    let mut foreign_content: Vec<ForeignFrame> = Vec::new();
    // Ordinary elements genuinely known to be open outside any tracked non-rendering
    // element (Codex, pull request #138, round 56, "Ignore closes that do not match
    // a real ancestor") — see `track_ordinary_ancestor`'s own doc comment.
    let mut ancestors: Vec<String> = Vec::new();
    let mut collecting = false;
    let mut current = String::new();
    let mut lines = Vec::new();

    for event in Parser::new_ext(contents, Options::empty()) {
        let hidden = in_fence
            || blockquote_depth > 0
            || in_html_comment
            || !open_non_rendering_tag.is_empty();
        match event {
            Event::Html(html) => {
                visible_html_ranges(
                    &html,
                    &mut OpenConstructState {
                        in_html_comment: &mut in_html_comment,
                        in_cdata: &mut in_cdata,
                    },
                    &mut open_non_rendering_tag,
                    &mut pending_tag,
                    &mut pending_raw_text_close,
                    &mut foreign_content,
                    &mut ancestors,
                );
            }
            // `<br>` is a real line break, this collector's own twin of
            // `markdown_prose`'s round-37 fix (Codex, pull request #138, round 58,
            // "Preserve inline breaks while collecting heading text"): the fix
            // there never reached this independent collector, so `## Con<br>text`
            // still fused into `Context`, a literal substring `check_adr_structure`
            // could match even though no reader ever sees those words run
            // together. Pushed only when the tag was not itself swallowed by an
            // open non-rendering element and nothing else is already hiding this
            // text, matching every other collector's own guard.
            Event::InlineHtml(html) => {
                let consumed = track_non_rendering_html(
                    &html,
                    &mut open_non_rendering_tag,
                    &mut foreign_content,
                    &mut ancestors,
                );
                if collecting && !consumed && !hidden && is_line_break_tag(&html) {
                    current.push('\n');
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_add(1);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_sub(1);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                if matches!(kind, CodeBlockKind::Fenced(_)) {
                    in_fence = true;
                }
            }
            Event::End(TagEnd::CodeBlock) => in_fence = false,
            Event::Start(Tag::Heading { level, .. }) if !hidden => {
                collecting = true;
                current.clear();
                for _ in 0..level as usize {
                    current.push('#');
                }
                current.push(' ');
            }
            Event::End(TagEnd::Heading(_)) if collecting => {
                collecting = false;
                lines.push(std::mem::take(&mut current));
            }
            Event::Text(text) if collecting && !hidden => current.push_str(&text),
            Event::Code(code) if collecting && !hidden => {
                current.push('`');
                current.push_str(&code);
                current.push('`');
            }
            _ => {}
        }
    }
    lines
}

/// Every real Markdown table row in `contents`, rendered as `| cell | cell | ... |` with
/// inline code backticks kept, in source order.
///
/// Enables `pulldown-cmark`'s GFM table extension, so a row exists only where the source
/// forms a real table — a header line followed by its delimiter row of dashes. A line that
/// merely starts with `|` is not one: `markdown_prose`'s line scan could not tell a real row
/// from an escaped example written to *show* the row syntax, `\| id \| headline \| proof \|`,
/// because unescaping and reconstructing prose happen before either line reaches a reader —
/// the backslash is gone and the two are the same text. A table walk is structural instead:
/// an escaped example forms no `Tag::TableRow` at all, table syntax or not, so it is invisible
/// here rather than merely dropped after being read as one (issue #82's continuation).
///
/// Fenced code blocks and blockquotes are hidden, for [`markdown_prose`]'s reason: a real
/// table quoted inside either must not stand in for the document's own.
#[must_use]
pub fn table_rows(contents: &str) -> Vec<String> {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

    let mut rows = Vec::new();
    let mut in_fence = false;
    let mut blockquote_depth: u32 = 0;
    let mut in_row = false;
    let mut row = String::new();
    let mut cell = String::new();
    // Whether an HTML comment opened earlier is still open (Codex, pull request #138,
    // round 20): `pulldown-cmark` ends an `HtmlBlock` at a blank line even when a
    // comment inside it never closed, so a table appearing right after reads as an
    // ordinary, visible one — structurally separate from the comment, but still inside
    // it by real HTML rules until an actual `-->` appears. `visible_html_ranges` is
    // called for the state transition alone; nothing between cells is ever read.
    let mut in_html_comment = false;
    let mut in_cdata = false; // Codex, round 57: carried the same way, see `OpenConstructState`.
    // The non-rendering elements (`<script>`, `<style>`, `<title>`, `<template>` or `<iframe>`)
    // currently open inside a cell, innermost last, if any (Codex, pull request #138,
    // round 30): this parser is independent of `markdown_prose`'s own non-rendering
    // handling (rounds 27-29), and a required value placed inside one of these
    // elements is invisible to a reader the same way a comment is, but was still
    // copied into the cell verbatim. A real stack since round 33: a nested `<template>`
    // genuinely opens a second context, unlike `<script>`/`<style>`/`<title>`'s
    // raw-text parsing, and either of those can itself nest inside a `<template>` and
    // needs its own level tracked.
    let mut open_non_rendering_tag: Vec<String> = Vec::new();
    // A tag whose own closing `>` had not yet appeared when its line ran out
    // (Codex, pull request #138, round 41, finding 1), carried across `Event::Html`
    // lines the same way `in_html_comment` and `open_non_rendering_tag` already are.
    let mut pending_tag: Option<PendingTag> = None;
    // A raw-text element's own close tag name matched but its terminating `>` had not
    // yet appeared (Codex, pull request #138, round 45, "Finish multiline raw-text
    // close tags before popping"), carried the same way `pending_tag` is.
    let mut pending_raw_text_close: Option<PendingRawTextClose> = None;
    // How many `<svg>`/`<math>` foreign-content roots are currently open, carried
    // across `Event::InlineHtml` constructs the same way `open_non_rendering_tag` is
    // (Codex, pull request #138, round 49, "Avoid pushing self-closing scripts in
    // foreign content") — see `track_non_rendering_html`'s own doc comment.
    let mut foreign_content: Vec<ForeignFrame> = Vec::new();
    // Ordinary elements genuinely known to be open outside any tracked non-rendering
    // element (Codex, pull request #138, round 56, "Ignore closes that do not match
    // a real ancestor") — see `track_ordinary_ancestor`'s own doc comment.
    let mut ancestors: Vec<String> = Vec::new();

    for event in Parser::new_ext(contents, Options::ENABLE_TABLES) {
        let hidden = in_fence
            || blockquote_depth > 0
            || in_html_comment
            || !open_non_rendering_tag.is_empty();
        match event {
            Event::Html(html) => {
                visible_html_ranges(
                    &html,
                    &mut OpenConstructState {
                        in_html_comment: &mut in_html_comment,
                        in_cdata: &mut in_cdata,
                    },
                    &mut open_non_rendering_tag,
                    &mut pending_tag,
                    &mut pending_raw_text_close,
                    &mut foreign_content,
                    &mut ancestors,
                );
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                if matches!(kind, CodeBlockKind::Fenced(_)) {
                    in_fence = true;
                }
            }
            Event::End(TagEnd::CodeBlock) => in_fence = false,
            Event::Start(Tag::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_add(1);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                blockquote_depth = blockquote_depth.saturating_sub(1);
            }
            Event::Start(Tag::TableHead | Tag::TableRow) if !hidden => {
                in_row = true;
                row.clear();
                row.push('|');
            }
            // Never guarded by `!hidden` (Codex, pull request #138, round 44, finding
            // 1): an inline non-rendering element opened partway through this row and
            // left open past its own end — `open_non_rendering_tag` still non-empty
            // right at `End(TableRow)` — used to skip this whole arm, so neither
            // `rows.push` nor `in_row = false` ever ran; `in_row` and this row's
            // partial text then carried straight into the *next* row, whose own
            // content (after the element's real close, later in that row) was
            // appended onto it, synthesizing one visible-looking row out of two a
            // reader never sees combined. The row still ends here regardless — only
            // whether it is *kept* depends on `hidden`, and `in_row` always resets.
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                if in_row && !hidden {
                    rows.push(row.clone());
                }
                in_row = false;
            }
            Event::Start(Tag::TableCell) if in_row => {
                cell.clear();
            }
            Event::End(TagEnd::TableCell) if in_row => {
                row.push(' ');
                row.push_str(cell.trim());
                row.push_str(" |");
            }
            // Never scanned for a comment closer (Codex, pull request #138, round 28,
            // correcting round 22): ordinary Markdown text is always HTML-escaped when
            // rendered, so a `-->` reaching `Event::Text` never survives as three
            // unescaped bytes in the rendered HTML a browser parses. A comment a
            // genuine, unescaped `<!--` opened stays open through any amount of later
            // `Event::Text`; only `Event::Html`/`Event::InlineHtml` can close it, which
            // is why `open_non_rendering_tag` (round 30) is checked explicitly here
            // rather than folded into `in_row`: it can open *partway through* a row
            // already started, from an inline `<script>` in this very cell.
            Event::Text(text) if in_row && open_non_rendering_tag.is_empty() => {
                cell.push_str(&text);
            }
            Event::Code(code) if in_row && open_non_rendering_tag.is_empty() => {
                cell.push('`');
                cell.push_str(&code);
                cell.push('`');
            }
            // A descriptive link, `[recovery proof](tests/spine.rs)`, renders only its
            // label through `Event::Text`; the destination is the `Tag::Link` this cell
            // is now inside (Codex, pull request #138, round 14). The old row scan read
            // raw source text and saw the destination along with the label, so a cell
            // whose required value is the link target rather than its label must still
            // carry that value for `.contains` to find.
            Event::Start(Tag::Link { dest_url, .. })
                if in_row && open_non_rendering_tag.is_empty() =>
            {
                cell.push_str(&dest_url);
                cell.push(' ');
            }
            // Raw HTML, `<a href="tests/spine.rs">recovery proof</a>`, is not a
            // `Tag::Link` at all — it is two `InlineHtml` events around the label's own
            // `Event::Text` (Codex, pull request #138, round 15) — so the destination is
            // extracted explicitly rather than kept as the tag's raw text (Codex, round
            // 35, finding 3, correcting round 15's own fix): a reader sees a link's
            // `href`, resolved as the destination a click follows, but not any other
            // attribute an anchor or any other inline tag carries — `<span
            // title="\`clause-id\` headline proof">` kept verbatim just as readily and let
            // an otherwise empty cell satisfy a check from text no reader ever sees.
            // `anchor_href` returns only that one value, and only from a genuine `<a`
            // open tag; every other tag — including an anchor's own `</a>` close —
            // contributes nothing here, matching what a reader actually sees beyond the
            // label `Event::Text` already carried into the cell. An inline comment is
            // excluded (Codex, round 16), for `visible_source`'s reason: `| <!--
            // \`id\` --> |` is a hidden decoy, not a real cell, and keeping its text
            // would let it stand in for the real one. A non-rendering element —
            // `<script>`, `<style>`, `<title>`, `<template>` or `<iframe>` — is excluded the same way (Codex,
            // round 30), and its own open/close tags update `open_non_rendering_tag`
            // instead of joining the cell's text: a required value placed inside one is
            // invisible to a reader. `<br>` is a real line break, this collector's own
            // twin of `markdown_prose`'s round-37 fix (Codex, round 38, finding 3): the
            // fix there did not reach this independent collector, so `head<br>line`
            // still fused into `headline` here, a literal substring a `.contains` scan
            // could match though no reader ever sees it run together.
            Event::InlineHtml(html)
                if !track_non_rendering_html(
                    &html,
                    &mut open_non_rendering_tag,
                    &mut foreign_content,
                    &mut ancestors,
                ) && in_row
                    && !html.starts_with("<!--") =>
            {
                if let Some(href) = anchor_href(&html) {
                    // Decoded, not kept as raw source text (Codex, pull request #138,
                    // round 44, finding 3): a destination can itself encode part of
                    // its path as an HTML character reference, and only the resolved
                    // value is what a reader's click — or a comparison against a
                    // real repository path — ever sees.
                    cell.push_str(&decode_character_references(href));
                    cell.push(' ');
                } else if is_line_break_tag(&html) {
                    cell.push('\n');
                }
            }
            _ => {}
        }
    }
    rows
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
            syn::Item::Fn(function) if function.sig.ident == name => {
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
                        if method.sig.ident == name
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
            if attr.path().segments.len() == 1 && first.ident == "test" {
                tested = true;
            }
            if first.ident == "ignore" || first.ident == "cfg" || first.ident == "cfg_attr" {
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
/// file name, and `rustc` resolves it against the declaring file's directory: verified
/// by compiling a nested probe (`lib.rs` → `mod crc;` → `crc.rs` → `#[path = "tbl.rs"]
/// mod table;` reads the sibling `src/tbl.rs`; `src/crc/tbl.rs` is never consulted —
/// the build fails without the sibling). A `#[path]` with directory components is
/// relative the same way, so `#[path = "crc/tbl.rs"]` in `src/crc.rs` reads
/// `src/crc/tbl.rs`.
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
    let mut found = Vec::new();
    collect_child_modules(&file.items, &parent_dir, &child_dir, false, &mut found);
    Ok(found)
}

/// The out-of-line `mod`s in `items`, appending to `found` in source order.
///
/// `parent_dir` is the declaring file's directory and `child_dir` the directory its
/// children live in; `gated` is whether an enclosing inline module is `#[cfg(test)]`.
fn collect_child_modules(
    items: &[syn::Item],
    parent_dir: &str,
    child_dir: &str,
    gated: bool,
    found: &mut Vec<ChildModule>,
) {
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        let name = module.ident.to_string();
        let item_gated = gated || has_cfg_test(&module.attrs);
        if let Some((_, nested)) = module.content.as_ref() {
            // Inline: no file of its own, but its out-of-line children live under it.
            collect_child_modules(
                nested,
                parent_dir,
                &format!("{child_dir}{name}/"),
                item_gated,
                found,
            );
        } else {
            let candidates = module.attrs.iter().find_map(path_attr_value).map_or_else(
                || {
                    vec![
                        format!("{child_dir}{name}.rs"),
                        format!("{child_dir}{name}/mod.rs"),
                    ]
                },
                // `rustc` consults exactly this one path (see above): no fallback.
                |path| vec![normalize_path(&format!("{parent_dir}{path}"))],
            );
            found.push(ChildModule {
                name,
                candidates,
                test_gated: item_gated,
            });
        }
    }
}

/// The string value of a `#[path = "..."]` attribute, if present.
fn path_attr_value(attr: &syn::Attribute) -> Option<String> {
    if !attr.path().is_ident("path") {
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
fn block_text(block: &syn::Block) -> String {
    let mut body = String::new();
    for stmt in &block.stmts {
        body.push_str(&stmt.to_token_stream().to_string());
        body.push(' ');
    }
    body.replace(" :: ", "::")
}
