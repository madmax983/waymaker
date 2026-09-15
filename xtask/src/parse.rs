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
    // The non-rendering elements (`<script>`, `<style>`, `<title>` or `<template>`)
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
            Event::Start(Tag::Item) => {
                if !hidden {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    if ordered_lists.last().copied().unwrap_or(false) {
                        // An ordered item renders as `1. ` so it can never be
                        // mistaken for the marker a field or claim scan matches on.
                        out.push_str("1. ");
                    } else {
                        // CommonMark allows `-`, `*` or `+` for an unordered item,
                        // and normalizing every one of them to `-` would make an
                        // example written with a different marker indistinguishable
                        // from the real bullet a scan matches on. The item's own
                        // range starts at its marker, so read the real one back from
                        // the source rather than guessing.
                        let marker = contents[range.start..].chars().next().unwrap_or('-');
                        out.extend([marker, ' ']);
                    }
                }
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
                    &mut in_html_comment,
                    &mut open_non_rendering_tag,
                    &mut pending_tag,
                    &mut pending_raw_text_close,
                    &mut foreign_content,
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

/// Whether `tag` can genuinely nest (Codex, pull request #138, round 31, finding 3;
/// widened round 42, finding 3).
///
/// `<template>` content is parsed as ordinary HTML, so
/// `<template><template>inner</template>more</template>` really does open a second
/// template context, and `more` stays inert until the *outer* close is found. `<script>`,
/// `<style>` and `<title>` are raw-text/RCDATA elements: a browser is in raw-text (or
/// RCDATA) parsing mode once one opens, so a further `<script`/`<title` inside one is
/// literal text — a JavaScript string, or a literal document title containing the
/// characters `<title>`, say (round 30) — and never opens a second level. Every other
/// tag nests too, which matters now that the non-rendering stack can hold an *arbitrary*
/// tag name suppressed by its own `hidden` attribute rather than only the fixed ones: a
/// `<div hidden>` still parses its children as ordinary HTML, exactly like `<template>`,
/// and only `<script>`/`<style>`/`<title>`'s raw-text parsing is the exception.
fn non_rendering_element_nests(tag: &str) -> bool {
    !matches!(tag, "script" | "style" | "title")
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
                | "details"
                | "div"
                | "dl"
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
                | "hr"
                | "main"
                | "menu"
                | "nav"
                | "ol"
                | "p"
                | "pre"
                | "section"
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

/// The byte range of the next raw-text end tag for `tag` (`"script"`, `"style"` or
/// `"title"`) at or after `from` in `line` — matched the way an HTML5 parser matches one
/// inside raw-text (or RCDATA) content: a literal, case-insensitive `</tag` sequence,
/// wherever it falls, with no regard for anything around it that merely *looks*
/// tag-shaped (Codex, pull request #138, round 40, finding 3; extended to `<title>`,
/// round 48, finding 2).
///
/// Once a `<script>`, `<style>` or `<title>` opens, a browser is not parsing tags or
/// quotes at all — every byte up to the literal closing sequence is opaque text —
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
/// title is metadata for the browser chrome, never page prose); every other tag this
/// module keeps verbatim because a reader does see it.
fn find_any_opening_tag(line: &str, from: usize) -> Option<(usize, usize, &'static str)> {
    ["script", "style", "title", "template"]
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
                    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'=' | b'/' | b'>')
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

/// Updates `foreign_content` — the currently open foreign-content roots and HTML
/// integration points, innermost last — from one already-consumed tag's own markup
/// (Codex, pull request #138, round 49, "Avoid pushing self-closing scripts in
/// foreign content"; carried across lines round 50, "Carry foreign-content depth
/// across raw HTML lines"; widened to a real stack round 51, "Exit foreign mode at
/// HTML integration points").
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
                        .is_none_or(|&byte| matches!(byte, b'/' | b' ' | b'\t' | b'\n' | b'\r'));
                    let after_ok = bytes.get(index + 6).is_none_or(|&byte| {
                        matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'=' | b'/' | b'>')
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
fn track_non_rendering_html(
    html: &str,
    stack: &mut Vec<String>,
    foreign_content: &mut Vec<ForeignFrame>,
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
/// A `<` only counts here when a syntactically plausible tag could follow it — an
/// ASCII letter (an opening tag's own name) or `/` (a closing tag) — not any `<`
/// whatsoever (Codex, pull request #138, round 47, "Distinguish literal less-than
/// signs from tag starts"): a browser tokenizes `<` as the start of markup only in
/// those two cases, and `2 < 3` is ordinary visible text whose `<` is not one of
/// them. Every caller of this function treats its `Some` as "an incomplete tag
/// starts here, carry it across the line break" — reading `2 < 3`'s `<` that way
/// swallowed everything from there to the next unrelated `>` anywhere later in the
/// document as if it were that tag's own markup.
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
            .is_some_and(|&byte| byte.is_ascii_alphabetic() || byte == b'/');
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
        } else if implicitly_closed_by(top, &name) {
            return Some(NonRenderingAdvance::Close(start));
        } else if matches!(name.as_str(), "script" | "style" | "title" | "template") || name == top
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
        }
        // Not relevant to `top` — carry its own namespace effect forward (a no-op for
        // anything that is not a foreign-content root or an HTML integration point)
        // and keep walking.
        track_foreign_content_depth(span, foreign_content);
        cursor = end;
    }
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
    foreign_content: &mut Vec<ForeignFrame>,
) -> Option<usize> {
    // Cloned rather than borrowed (Codex, pull request #138, round 42, finding 3):
    // the stack widened from `Vec<&'static str>` to `Vec<String>` so it can hold an
    // arbitrary `hidden`-suppressed name, and a borrow of its last element would
    // still be live across the `stack.push`/`stack.pop` calls below.
    let top = stack.last()?.clone();
    match next_non_rendering_marker(line, cursor, &top, foreign_content) {
        Some(NonRenderingAdvance::Comment(start)) => {
            let Some(offset) = line[start..].find("-->") else {
                *in_html_comment = true;
                return None;
            };
            Some(start + offset + "-->".len())
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
/// non-rendering elements (`<script>`, `<style>`, `<title>`, `<template>`) excluded. Carries
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
        if matches!(name.as_str(), "script" | "style" | "title" | "template") || reopens_top {
            open_non_rendering.push(name);
        } else if !is_void_element(&name) {
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

fn visible_html_ranges(
    line: &str,
    in_html_comment: &mut bool,
    open_non_rendering: &mut Vec<String>,
    pending_tag: &mut Option<PendingTag>,
    pending_raw_text_close: &mut Option<PendingRawTextClose>,
    foreign_content: &mut Vec<ForeignFrame>,
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
        if *in_html_comment {
            match line[cursor..].find("-->") {
                Some(offset) => {
                    cursor += offset + "-->".len();
                    *in_html_comment = false;
                    continue;
                }
                None => break,
            }
        }
        if !open_non_rendering.is_empty() {
            match advance_past_non_rendering(
                line,
                cursor,
                open_non_rendering,
                in_html_comment,
                pending_tag,
                pending_raw_text_close,
                foreign_content,
            ) {
                Some(end) => {
                    cursor = end;
                    continue;
                }
                None => break,
            }
        }
        match next_hiding_marker(line, cursor, foreign_content) {
            None => {
                // `next_hiding_marker` finding nothing is ambiguous on its own: either
                // there is no more `<` at all (the remainder really is visible text),
                // or there is one whose own tag never closes before this line runs out
                // (Codex, round 41, finding 1) — `find_any_tag` commits to the first
                // `<` it finds and gives up entirely rather than searching past it, so
                // an unclosed tag anywhere in the remainder reads identically to no
                // tag at all unless checked for separately.
                if let Some(start) = next_tag_start(line, cursor) {
                    spans.push(VisibleHtmlSpan::Text(cursor..start));
                    let closing = line[start..].starts_with("</");
                    let name = markup_tag_name(&line[start..]).to_ascii_lowercase();
                    // The quote state this line's own scan reached is captured, not
                    // assumed empty (Codex, round 42, finding 2): a tag whose quoted
                    // attribute value itself crosses the line —
                    // `<div title="first\nsecond">All 6 recovery invariants</div>` —
                    // left this line still inside that quote, and starting the next
                    // line's resumed scan from `quote: None` read its own closing
                    // quote as a fresh *opening* one, so the tag never resolved and
                    // the visible text after it was discarded along with it.
                    let mut quote: Option<u8> = None;
                    scan_tag_close(line, start + 1, &mut quote);
                    *pending_tag = Some(PendingTag {
                        name,
                        closing,
                        quote,
                        text: line[start..].to_owned(),
                    });
                    break;
                }
                spans.push(VisibleHtmlSpan::Text(cursor..line.len()));
                break;
            }
            Some(HidingMarker::Comment(start)) => {
                spans.push(VisibleHtmlSpan::Text(cursor..start));
                let Some(offset) = line[start..].find("-->") else {
                    *in_html_comment = true;
                    break;
                };
                cursor = start + offset + "-->".len();
            }
            Some(HidingMarker::Tag(start, end, tag)) => {
                spans.push(VisibleHtmlSpan::Text(cursor..start));
                open_non_rendering.push(tag.to_owned());
                cursor = end;
            }
            Some(HidingMarker::Hidden(start, end, name)) => {
                spans.push(VisibleHtmlSpan::Text(cursor..start));
                open_non_rendering.push(name);
                cursor = end;
            }
            Some(HidingMarker::Markup(start, end)) => {
                spans.push(VisibleHtmlSpan::Text(cursor..start));
                let span = &line[start..end];
                if is_html_block_tag(span) || is_line_break_tag(span) {
                    spans.push(VisibleHtmlSpan::Break);
                }
                track_foreign_content_depth(span, foreign_content);
                cursor = end;
            }
        }
    }
    spans
}

/// One visible byte range of an `Event::Html` line, or a forced line break a stripped
/// block tag's own markup leaves behind — see [`visible_html_ranges`].
enum VisibleHtmlSpan {
    /// A visible byte range into the original line.
    Text(std::ops::Range<usize>),
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

/// `value` with every HTML character reference it carries resolved to the character
/// it names, the way a browser resolves an attribute value before using it (Codex,
/// pull request #138, round 44, finding 3): a raw anchor's destination can itself
/// encode part of its path as a reference — `tests&#47;spine.rs` and `tests/spine.rs`
/// are the same destination to a reader's click — and comparing the undecoded source
/// text against a real repository path finds neither. Decodes the five XML entities
/// (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`) and a numeric reference, decimal
/// (`&#47;`) or hexadecimal (`&#x2F;`/`&#X2F;`); any other named entity, or a `&` with
/// no terminating `;` at all, is left exactly as written — a narrower scope than a
/// full HTML5 decoder, stated rather than solved.
fn decode_character_references(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(offset) = rest.find('&') {
        out.push_str(&rest[..offset]);
        let after_amp = &rest[offset + 1..];
        let Some(semicolon) = after_amp.find(';') else {
            out.push('&');
            rest = after_amp;
            continue;
        };
        let entity = &after_amp[..semicolon];
        if let Some(character) = decode_one_character_reference(entity) {
            out.push(character);
            rest = &after_amp[semicolon + 1..];
        } else {
            out.push('&');
            rest = after_amp;
        }
    }
    out.push_str(rest);
    out
}

/// One HTML character reference's own name or digits (the text between `&` and `;`,
/// exclusive of both), resolved to the character it names — see
/// [`decode_character_references`] for which references this recognizes.
fn decode_one_character_reference(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let digits = entity.strip_prefix('#')?;
            let value = match digits
                .strip_prefix('x')
                .or_else(|| digits.strip_prefix('X'))
            {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse::<u32>().ok()?,
            };
            char::from_u32(value)
        }
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
/// A non-rendering (`<script>`, `<style>`, `<title>`, `<template>`) or `hidden`-suppressed
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
                    // A comment nested inside a block that opens with a real tag —
                    // `<div>\n<!-- [x](y) -->\n</div>` — is still a comment, and the
                    // whole block does not start with `<!--` for that reason (Codex,
                    // pull request #138, round 13). One `HtmlBlock` event covers the
                    // whole block, real tags and nested comments alike, so each
                    // `<!--` ... `-->` span inside it is hidden on its own rather than
                    // requiring the block to be nothing but a comment.
                    //
                    // The close is searched for from the opener to the end of the whole
                    // document, not only to the end of this block (Codex, round 14): an
                    // unterminated `<!--` is not rendered, so everything after it is as
                    // hidden as a matched comment's body, matching the fail-closed
                    // behaviour `without_html_comments` already had for this case. HTML
                    // comments do not nest, so the first `-->` found always closes the
                    // `<!--` before it.
                    //
                    // Found through `find_comment_opener`, not a raw substring search
                    // (Codex, pull request #138, round 40, finding 2): a real, complete
                    // tag inside the block whose own quoted attribute merely spells
                    // `<!--` — `<div title="<!--">note</div>` — is text a browser
                    // renders as an attribute value, not a comment, and a raw search
                    // would hide everything from there to end of document over a
                    // decoy that was never a real opener. Bounded to `range.end` the
                    // same way the old search was, by slicing the search text there.
                    let mut cursor = start;
                    while let Some(open) = find_comment_opener(&contents[..range.end], cursor) {
                        let end = contents[open..]
                            .find("-->")
                            .map_or(contents.len(), |close| open + close + "-->".len());
                        hidden.push((open, end));
                        if end >= range.end {
                            break;
                        }
                        cursor = end;
                    }
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
    hidden: &mut Vec<(usize, usize)>,
) {
    let was_open = !open_non_rendering.is_empty();
    track_non_rendering_html(html, open_non_rendering, foreign_content);
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
        if !open_non_rendering.is_empty() {
            match advance_past_non_rendering(
                html,
                cursor,
                open_non_rendering,
                in_template_comment,
                pending_tag,
                pending_raw_text_close,
                foreign_content,
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
            Some(HidingMarker::Comment(start)) => match html[start..].find("-->") {
                Some(offset) => cursor = start + offset + "-->".len(),
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
                cursor = end;
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
    // The non-rendering elements (`<script>`, `<style>`, `<title>` or `<template>`)
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
                    &mut in_html_comment,
                    &mut open_non_rendering_tag,
                    &mut pending_tag,
                    &mut pending_raw_text_close,
                    &mut foreign_content,
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
            // A non-rendering element — `<script>`, `<style>`, `<title>`, `<template>` —
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
            Event::InlineHtml(html) => {
                let opened = track_non_rendering_html(
                    &html,
                    &mut open_non_rendering_tag,
                    &mut foreign_content,
                );
                if opened {
                    hidden_markup_seen = true;
                }
                if collecting
                    && if html.starts_with("<!--") {
                        html.contains('\n')
                    } else {
                        is_line_break_tag(&html)
                    }
                {
                    collecting = false;
                }
            }
            _ => {}
        }
    }
    None
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
                    &mut in_html_comment,
                    &mut open_non_rendering_tag,
                    &mut pending_tag,
                    &mut pending_raw_text_close,
                    &mut foreign_content,
                );
            }
            Event::InlineHtml(html) => {
                track_non_rendering_html(&html, &mut open_non_rendering_tag, &mut foreign_content);
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
    // The non-rendering elements (`<script>`, `<style>`, `<title>` or `<template>`)
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

    for event in Parser::new_ext(contents, Options::ENABLE_TABLES) {
        let hidden = in_fence
            || blockquote_depth > 0
            || in_html_comment
            || !open_non_rendering_tag.is_empty();
        match event {
            Event::Html(html) => {
                visible_html_ranges(
                    &html,
                    &mut in_html_comment,
                    &mut open_non_rendering_tag,
                    &mut pending_tag,
                    &mut pending_raw_text_close,
                    &mut foreign_content,
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
            // `<script>`, `<style>`, `<title>`, `<template>` — is excluded the same way (Codex,
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
