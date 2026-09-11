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
