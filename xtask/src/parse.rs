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
    // The non-rendering elements (`<script>`, `<style>` or `<template>`) currently
    // open, innermost last — a stack rather than one tag, named rather than a bare flag
    // (Codex, pull request #138, round 30) so a matching close is required: a
    // `<script>` body containing the literal text `</style>` (a JavaScript string, say)
    // does not end HTML parsing of the script, and closing on any of the three would
    // resume visibility while a browser is still in script-data state. A real stack
    // since round 33: unlike `<script>`/`<style>`, `<template>` content is parsed as
    // ordinary HTML, so a nested `<template>` genuinely opens a second context that
    // needs its own close first, and a `<script>` or `<style>` can itself nest inside a
    // `<template>` and needs its own level tracked rather than scanned as more
    // `<template>` content.
    let mut open_non_rendering_tag: Vec<&'static str> = Vec::new();
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
            Event::End(TagEnd::List(_)) => {
                ordered_lists.pop();
            }
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
                    out.push('`');
                    out.push_str(&code);
                    out.push('`');
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
                    for _ in 0..level as usize {
                        out.push('#');
                    }
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
                        out.push(marker);
                        out.push(' ');
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
            // any of the three would resume visibility too early.
            Event::Html(html) => {
                for range in
                    visible_html_ranges(&html, &mut in_html_comment, &mut open_non_rendering_tag)
                {
                    if !container_hidden {
                        out.push_str(&html[range]);
                    }
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
            Event::InlineHtml(html) => {
                track_non_rendering_html(&html, &mut open_non_rendering_tag);
            }
            _ => {}
        }
    }
    out
}

/// Whether `tag` (`"script"`, `"style"` or `"template"`) can genuinely nest (Codex, pull
/// request #138, round 31, finding 3).
///
/// `<template>` content is parsed as ordinary HTML, so
/// `<template><template>inner</template>more</template>` really does open a second
/// template context, and `more` stays inert until the *outer* close is found. `<script>`
/// and `<style>` are raw-text elements: a browser is in raw-text parsing mode once
/// either opens, so a further `<script` inside one is literal text — a JavaScript
/// string, say (round 30) — and never opens a second level.
fn non_rendering_element_nests(tag: &str) -> bool {
    tag == "template"
}

/// The byte range of the first well-formed opening tag for `tag` at or after `from` in
/// `line`, case-insensitively — from `<` through the tag's own closing `>` (or the end
/// of `line`, if the tag is not closed on this line).
///
/// Matched anywhere in `line`, not only at its start, since a nested element can open
/// partway through an enclosing `HtmlBlock`'s own `Event::Html` line; only where the tag
/// name ends right there rather than continuing into a longer one (`<scriptx>` does not
/// match).
fn find_opening_tag(line: &str, from: usize, tag: &str) -> Option<(usize, usize)> {
    let lower = line.to_ascii_lowercase();
    let marker = format!("<{tag}");
    lower
        .get(from..)?
        .match_indices(&marker)
        .find_map(|(rel, _)| {
            let start = from + rel;
            let after = start + marker.len();
            let boundary = lower
                .as_bytes()
                .get(after)
                .is_none_or(|&byte| matches!(byte, b'>' | b' ' | b'\t' | b'\n' | b'/'));
            if !boundary {
                return None;
            }
            let end = lower[start..]
                .find('>')
                .map_or(line.len(), |offset| start + offset + 1);
            Some((start, end))
        })
}

/// The byte range of the first closing tag for `tag` at or after `from` in `line`,
/// case-insensitively — `</tag>` or `</tag >` alike, with any whitespace HTML permits
/// between the tag name and `>` (Codex, pull request #138, round 32, finding 2): an
/// exact `</tag>` match left `open_non_rendering_tag` set forever against a real,
/// legally spelled `</script >` or `</template\t>`, hiding every line after it.
fn find_closing_tag(line: &str, from: usize, tag: &str) -> Option<(usize, usize)> {
    let lower = line.to_ascii_lowercase();
    let marker = format!("</{tag}");
    lower
        .get(from..)?
        .match_indices(&marker)
        .find_map(|(rel, _)| {
            let start = from + rel;
            let after = start + marker.len();
            let rest = lower.get(after..)?;
            let gt = rest.find('>')?;
            rest.get(..gt)?
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
                .then_some((start, after + gt + 1))
        })
}

/// The earliest opening tag, at or after `from` in `line`, among the three non-rendering
/// elements, with the tag name it matched.
///
/// These are the three HTML elements whose body a browser never renders as visible text
/// (Codex, pull request #138, rounds 27, 28 and 29 — `<template>`'s content is inert DOM
/// meant for cloning by script, not display); every other tag this module keeps verbatim
/// because a reader does see it.
fn find_any_opening_tag(line: &str, from: usize) -> Option<(usize, usize, &'static str)> {
    ["script", "style", "template"]
        .into_iter()
        .filter_map(|tag| find_opening_tag(line, from, tag).map(|(start, end)| (start, end, tag)))
        .min_by_key(|&(start, _, _)| start)
}

/// Whether `line` contains a closing tag for `tag` (`"script"`, `"style"` or
/// `"template"`) anywhere in it, case-insensitively.
fn closes_non_rendering_element(line: &str, tag: &str) -> bool {
    find_closing_tag(line, 0, tag).is_some()
}

/// The tag name (`"script"`, `"style"` or `"template"`) of an opening non-rendering tag
/// found anywhere in `line`, case-insensitively.
fn opens_non_rendering_element(line: &str) -> Option<&'static str> {
    find_any_opening_tag(line, 0).map(|(_, _, tag)| tag)
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
fn track_non_rendering_html(html: &str, stack: &mut Vec<&'static str>) -> bool {
    if html.starts_with("<!--") {
        return true;
    }
    match stack.last().copied() {
        Some(top) if non_rendering_element_nests(top) => {
            if let Some(tag) = opens_non_rendering_element(html) {
                stack.push(tag);
            } else if closes_non_rendering_element(html, top) {
                stack.pop();
            }
            true
        }
        Some(top) => {
            if closes_non_rendering_element(html, top) {
                stack.pop();
            }
            true
        }
        None => opens_non_rendering_element(html).is_some_and(|tag| {
            stack.push(tag);
            true
        }),
    }
}

/// One place in a line that stops content from being visible: a comment opener, a
/// non-rendering element's opening tag, or an ordinary tag's own markup.
enum HidingMarker {
    /// The byte offset of a `<!--`.
    Comment(usize),
    /// The byte range and name of a non-rendering element's opening tag.
    Tag(usize, usize, &'static str),
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
    let mut cursor = from;
    loop {
        let start = cursor + line.get(cursor..)?.find('<')?;
        if line[start..].starts_with("<!--") {
            cursor = start + "<!--".len();
            continue;
        }
        let bytes = line.as_bytes();
        let mut quote: Option<u8> = None;
        let mut index = start + 1;
        while let Some(&byte) = bytes.get(index) {
            match quote {
                Some(open) if byte == open => quote = None,
                None if byte == b'"' || byte == b'\'' => quote = Some(byte),
                None if byte == b'>' => return Some((start, index + 1)),
                Some(_) | None => {}
            }
            index += 1;
        }
        return None;
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
/// to change tracked state, which excluding it as plain markup would not do.
fn next_hiding_marker(line: &str, from: usize) -> Option<HidingMarker> {
    let mut candidates: Vec<(usize, HidingMarker)> = Vec::new();
    if let Some(start) = line[from..].find("<!--").map(|offset| from + offset) {
        candidates.push((start, HidingMarker::Comment(start)));
    }
    if let Some((start, end, tag)) = find_any_opening_tag(line, from) {
        candidates.push((start, HidingMarker::Tag(start, end, tag)));
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
    /// (Codex, round 33, finding 2).
    Open(usize, &'static str),
    /// The innermost open element closed.
    Close(usize),
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
/// keeps `hidden` inert until the real, final close. For a raw-text element (`<script>`,
/// `<style>`), only its own close: a browser never parses anything else inside one,
/// comment or nested element included.
fn next_non_rendering_marker(line: &str, cursor: usize, top: &str) -> Option<NonRenderingAdvance> {
    let close = find_closing_tag(line, cursor, top)
        .map(|(start, end)| (start, NonRenderingAdvance::Close(end)));
    if !non_rendering_element_nests(top) {
        return close.map(|(_, marker)| marker);
    }
    let open = find_any_opening_tag(line, cursor)
        .map(|(start, end, tag)| (start, NonRenderingAdvance::Open(end, tag)));
    let comment = line[cursor..].find("<!--").map(|offset| {
        (
            cursor + offset,
            NonRenderingAdvance::Comment(cursor + offset),
        )
    });
    [close, open, comment]
        .into_iter()
        .flatten()
        .min_by_key(|&(start, _)| start)
        .map(|(_, marker)| marker)
}

/// Advances past one close, one nested open, or one nested comment, relative to the
/// innermost element `stack` already carries — returning the new cursor, or `None` when
/// nothing more is found before the end of `line`, meaning the rest of the line stays
/// hidden and `stack` (and `in_html_comment`, if a comment was left open) carry into the
/// next line unchanged.
fn advance_past_non_rendering(
    line: &str,
    cursor: usize,
    stack: &mut Vec<&'static str>,
    in_html_comment: &mut bool,
) -> Option<usize> {
    let top = *stack.last()?;
    match next_non_rendering_marker(line, cursor, top)? {
        NonRenderingAdvance::Comment(start) => {
            let Some(offset) = line[start..].find("-->") else {
                *in_html_comment = true;
                return None;
            };
            Some(start + offset + "-->".len())
        }
        NonRenderingAdvance::Open(end, tag) => {
            stack.push(tag);
            Some(end)
        }
        NonRenderingAdvance::Close(end) => {
            stack.pop();
            Some(end)
        }
    }
}

/// The visible byte ranges of one `Event::Html` line — real block-level HTML
/// passthrough, one source line per event — with HTML comments and the content of
/// non-rendering elements (`<script>`, `<style>`, `<template>`) excluded. Carries
/// `in_html_comment` and the open non-rendering element across calls the way either
/// already has to be: a multi-line comment or a `<script>` that outlives its own
/// `HtmlBlock` (rounds 20 and 29) is tracked one line at a time, not judged per event.
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
/// text.
fn visible_html_ranges(
    line: &str,
    in_html_comment: &mut bool,
    open_non_rendering: &mut Vec<&'static str>,
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut cursor = 0usize;
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
            match advance_past_non_rendering(line, cursor, open_non_rendering, in_html_comment) {
                Some(end) => {
                    cursor = end;
                    continue;
                }
                None => break,
            }
        }
        match next_hiding_marker(line, cursor) {
            None => {
                ranges.push(cursor..line.len());
                break;
            }
            Some(HidingMarker::Comment(start)) => {
                ranges.push(cursor..start);
                let Some(offset) = line[start..].find("-->") else {
                    *in_html_comment = true;
                    break;
                };
                cursor = start + offset + "-->".len();
            }
            Some(HidingMarker::Tag(start, end, tag)) => {
                ranges.push(cursor..start);
                open_non_rendering.push(tag);
                cursor = end;
            }
            Some(HidingMarker::Markup(start, end)) => {
                ranges.push(cursor..start);
                cursor = end;
            }
        }
    }
    ranges
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
/// be absent or HTML whitespace (Codex, pull request #138, round 36, finding 2): a bare
/// substring search for `href=` also matches inside `data-href=`, so `<a
/// data-href="tests/spine.rs">elsewhere</a>` — an anchor with no link destination at all
/// — returned that unrelated attribute's value as if it were the real one. A rejected
/// candidate resumes the search past it rather than giving up, since a real `href` can
/// still follow a decoy one in the same tag.
fn anchor_href(html: &str) -> Option<&str> {
    find_opening_tag(html, 0, "a")?;
    let lower = html.to_ascii_lowercase();
    let marker = "href=";
    let mut search_from = 0;
    loop {
        let start = search_from + lower.get(search_from..)?.find(marker)?;
        let fresh_attribute = start == 0
            || lower
                .as_bytes()
                .get(start - 1)
                .is_some_and(u8::is_ascii_whitespace);
        if !fresh_attribute {
            search_from = start + marker.len();
            continue;
        }
        let value_start = start + marker.len();
        let quote = *html.as_bytes().get(value_start)?;
        if quote != b'"' && quote != b'\'' {
            search_from = value_start;
            continue;
        }
        let value_start = value_start + 1;
        let end = value_start + html.get(value_start..)?.find(quote as char)?;
        return Some(&html[value_start..end]);
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
#[must_use]
pub fn visible_source(contents: &str) -> String {
    use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

    let mut hidden: Vec<(usize, usize)> = Vec::new();
    let mut fence_start: Option<usize> = None;
    let mut quote_start: Option<usize> = None;
    let mut quote_depth: u32 = 0;
    let mut html_block_start: Option<usize> = None;
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
                    let mut cursor = start;
                    while let Some(open) = contents[cursor..range.end].find("<!--") {
                        let open = cursor + open;
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
            _ => {}
        }
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
    // Whether an HTML comment opened earlier is still open (Codex, pull request #138,
    // round 20): `pulldown-cmark` ends an `HtmlBlock` at a blank line even when a
    // comment inside it never closed, so an item appearing right after reads as an
    // ordinary, visible one — structurally separate from the comment, but still
    // inside it by real HTML rules until an actual `-->` appears. `append_visible_html_line`
    // is reused for the state transition alone: called with `hidden: true`, it never
    // writes anywhere; called for the state transition alone.
    let mut in_html_comment = false;
    // The non-rendering elements (`<script>`, `<style>` or `<template>`) currently open
    // at block level, innermost last, if any (Codex, pull request #138, round 30):
    // `<div>\n<script>\n\n- Status: accepted\n\n</script>\n</div>` outlives its own
    // `HtmlBlock` across the blank line the same way an unterminated comment does —
    // `pulldown-cmark` ends the block there and resumes the decoy item as an ordinary,
    // structurally separate one, even though a browser is still in script-data state
    // until the real `</script>` two lines later. Folded into `hidden` below so
    // `Start(Tag::Item)` never begins collecting such an item at all, the same
    // disqualification a fence or blockquote already gets. A real stack since round 33:
    // a nested `<template>` genuinely opens a second context, unlike
    // `<script>`/`<style>`'s raw-text parsing, and either of those can itself nest
    // inside a `<template>` and needs its own level tracked.
    let mut open_non_rendering_tag: Vec<&'static str> = Vec::new();

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
                visible_html_ranges(&html, &mut in_html_comment, &mut open_non_rendering_tag);
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
                    item.clear();
                }
            }
            Event::End(TagEnd::Paragraph) if collecting => paragraph_closed = true,
            Event::End(TagEnd::Item) => {
                if collecting {
                    collecting = false;
                    if let Some(value) = item.strip_prefix(prefix) {
                        return Some(value.trim().to_owned());
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
            // A non-rendering element — `<script>`, `<style>`, `<template>` —
            // disqualifies the item too (Codex, round 30): `- Status:
            // <script>accepted</script>` has a real, visible `Status: ` prefix ahead
            // of the tag, so merely *excluding* the hidden value (rather than
            // disqualifying the item) would leave `item` at `"Status: "`, which still
            // strips and trims to an empty value that still trivially matches —
            // exactly the failure round 15 already closed for a nested fence or
            // blockquote. Disqualifying outright, the same way `<br>` and a nested
            // fence or blockquote already do, is what actually closes it: `collecting`
            // drops before `End(TagEnd::Item)` can try matching an empty prefix.
            // Checked by the tag *opening* alone — a stray, unmatched close needs no
            // separate case, since the item is already disqualified by then.
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
            Event::InlineHtml(html)
                if collecting
                    && if html.starts_with("<!--") {
                        html.contains('\n')
                    } else {
                        is_line_break_tag(&html) || opens_non_rendering_element(&html).is_some()
                    } =>
            {
                collecting = false;
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
    let mut open_non_rendering_tag: Vec<&'static str> = Vec::new();
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
                visible_html_ranges(&html, &mut in_html_comment, &mut open_non_rendering_tag);
            }
            Event::InlineHtml(html) => {
                track_non_rendering_html(&html, &mut open_non_rendering_tag);
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
    // The non-rendering elements (`<script>`, `<style>` or `<template>`) currently open
    // inside a cell, innermost last, if any (Codex, pull request #138, round 30): this
    // parser is independent of `markdown_prose`'s own non-rendering handling (rounds
    // 27-29), and a required value placed inside one of these three elements is
    // invisible to a reader the same way a comment is, but was still copied into the
    // cell verbatim. A real stack since round 33: a nested `<template>` genuinely opens
    // a second context, unlike `<script>`/`<style>`'s raw-text parsing, and either of
    // those can itself nest inside a `<template>` and needs its own level tracked.
    let mut open_non_rendering_tag: Vec<&'static str> = Vec::new();

    for event in Parser::new_ext(contents, Options::ENABLE_TABLES) {
        let hidden = in_fence
            || blockquote_depth > 0
            || in_html_comment
            || !open_non_rendering_tag.is_empty();
        match event {
            Event::Html(html) => {
                visible_html_ranges(&html, &mut in_html_comment, &mut open_non_rendering_tag);
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
            Event::End(TagEnd::TableHead | TagEnd::TableRow) if !hidden => {
                if in_row {
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
            // `<script>`, `<style>`, `<template>` — is excluded the same way (Codex,
            // round 30), and its own open/close tags update `open_non_rendering_tag`
            // instead of joining the cell's text: a required value placed inside one is
            // invisible to a reader.
            Event::InlineHtml(html)
                if !track_non_rendering_html(&html, &mut open_non_rendering_tag)
                    && in_row
                    && !html.starts_with("<!--") =>
            {
                if let Some(href) = anchor_href(&html) {
                    cell.push_str(href);
                    cell.push(' ');
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
