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

/// Whether `attrs` carries a `#[cfg(..)]` or `#[cfg_attr(.., cfg(..))]` that is
/// guaranteed false whenever `test` is — the bare `#[cfg(test)]`, a compound `cfg`
/// predicate built from it, or an equivalent spelled through `cfg_attr`.
///
/// The textual `without_test_modules` blanked on the substring `#[cfg(test)]` alone, and
/// this function used to match only that exact shape — `path_is_ident(attr.path(),
/// "cfg")` with a bare `test` identifier as its whole argument, so `#[cfg(any(test))]`
/// and `#[cfg(all(test, feature = "x"))]` fell through `parse_args::<syn::Ident>()`
/// unparsed and answered `false`.
///
/// Found by Codex review of this change (PR #143), round 35: both compounds are exactly
/// as test-only as the bare form — an `all(..)` naming `test` among its conjuncts can
/// never hold without it, whatever else it also asks for, and an `any(..)` every one of
/// whose branches is itself guaranteed test-only can only be satisfied under test — so a
/// reached file gated with either spelling was still walked as production-reachable.
/// [`attribute_requires_test`] is the recursive predicate; `#[cfg(any(test, other))]` is
/// deliberately *not* recognized, and must not be, because it is satisfiable under
/// `other` alone — treating it as test-only would hide production-reachable code from
/// every rule that reads this function's answer as "unreachable in a shipped build".
///
/// Round 36 widened it once more: `#![cfg_attr(not(test), cfg(test))]` is exactly as
/// test-only as a bare `#![cfg(test)]`, because rustc's own rewrite of a `cfg_attr`
/// leaves nothing else it could mean — expanding to `cfg(test)` in exactly the builds
/// where `not(test)` holds (every non-test one) and to no attribute at all in every
/// build where it does not (every test one, where the guarded `cfg(test)` would have
/// held anyway) — but this function's own outer filter read only an attribute whose
/// *path* was `cfg`, so a `cfg_attr`-spelled equivalent never reached the predicate at
/// all. [`attribute_requires_test`] now reads the attribute's own path instead of
/// requiring the caller to have already stripped it.
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attribute_requires_test(&attr.meta))
}

/// [`has_cfg_test`]'s recursive half, over one attribute's parsed [`syn::Meta`] — its
/// own path included, so a `#[cfg(..)]` and a `#[cfg_attr(.., ..)]` are told apart here
/// rather than by the caller.
///
/// A `cfg(..)` predicate is handed to [`meta_requires_test`], the same recursive
/// predicate a `cfg_attr`'s own condition is evaluated against — the two questions are
/// dual, not the same, which is what [`meta_holds_without_test`] answers instead.
///
/// A `cfg_attr(condition, injected..)` requires `test` exactly when its own expansion
/// does: rustc replaces the whole attribute with every `injected` item when `condition`
/// holds, and removes it entirely otherwise — so the item's presence, considering only
/// this one attribute, is `!condition || (every injected item's own presence)`. That is
/// false whenever `test` is false exactly when both `condition` is *guaranteed true*
/// whenever `test` is false (so the `!condition` branch is not what lets it through) and
/// at least one `injected` item is itself [`attribute_requires_test`] (so the branch
/// that does run still requires it) — the same "any disjunct must hold" shape
/// [`meta_requires_test`]'s own `any(..)` arm already uses, because `!condition ||
/// injected` is exactly that shape with two disjuncts. Recursing into each `injected`
/// item through this same function rather than assuming it is a bare `cfg(..)` is what
/// lets `cfg_attr(.., cfg_attr(.., cfg(test)))` chain arbitrarily deep, the same reach
/// [`attr_introduces_cfg`]/[`meta_introduces_cfg`] already give a *reached* `cfg`.
fn attribute_requires_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::List(list) if path_is_ident(&list.path, "cfg") => list
            .parse_args::<syn::Meta>()
            .is_ok_and(|inner| meta_requires_test(&inner)),
        syn::Meta::List(list) if path_is_ident(&list.path, "cfg_attr") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|metas| {
                metas.first().is_some_and(meta_holds_without_test)
                    && metas.iter().skip(1).any(attribute_requires_test)
            }),
        _ => false,
    }
}

/// [`attribute_requires_test`]'s half over one already-unwrapped `cfg` predicate — the
/// argument of a `cfg(..)`, or one operand of an `all(..)`/`any(..)`/`not(..)` — rather
/// than over a whole attribute.
///
/// A bare `test` is the base case. `all(..)` is true only when every one of its
/// conjuncts is, so naming a predicate this function already recognizes as test-only
/// anywhere in the list makes the whole `all(..)` test-only too, whatever the other
/// conjuncts are. `any(..)` is true when at least one of its disjuncts is, so it is
/// test-only only when *every* disjunct is — one branch this function cannot vouch for
/// (a bare `feature = ".."`, or anything [`meta_holds_without_test`] cannot prove either)
/// is a branch that can fire without `test`, and an empty `any()` is never true at all
/// so it is not test-only either. `not(..)` is test-only exactly when its own argument
/// is guaranteed to *hold* whenever `test` is false — see [`meta_holds_without_test`],
/// its dual — and an unrecognized shape is read as unable to prove, matching this scan's
/// own rule that guessing is how a broken input talks a check out of testing it.
fn meta_requires_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path_is_ident(path, "test"),
        syn::Meta::List(list) if path_is_ident(&list.path, "all") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|metas| metas.iter().any(meta_requires_test)),
        syn::Meta::List(list) if path_is_ident(&list.path, "any") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|metas| !metas.is_empty() && metas.iter().all(meta_requires_test)),
        syn::Meta::List(list) if path_is_ident(&list.path, "not") => list
            .parse_args::<syn::Meta>()
            .is_ok_and(|inner| meta_holds_without_test(&inner)),
        _ => false,
    }
}

/// [`meta_requires_test`]'s dual: whether `meta` is guaranteed **true** whenever `test`
/// is false, over the same predicate grammar.
///
/// No bare identifier or key-value pair is ever recognized here, `test` itself
/// included — `test` is false exactly when `test` is false, never guaranteed *true*
/// then, and nothing else (`debug_assertions`, `feature = ".."`) is a fact this scan can
/// assume about an unrelated flag. `not(P)` holds whenever `test` is false exactly when
/// `P` is guaranteed false whenever `test` is false, which is [`meta_requires_test`]
/// applied to `P` — the two functions call each other rather than duplicating one
/// another's cases. `all(..)` holds whenever `test` is false only if *every* conjunct
/// does, and `any(..)` only needs *one* disjunct that does, matching each connective's
/// own truth table rather than [`meta_requires_test`]'s (which asks the opposite
/// question of the opposite condition).
fn meta_holds_without_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::List(list) if path_is_ident(&list.path, "not") => list
            .parse_args::<syn::Meta>()
            .is_ok_and(|inner| meta_requires_test(&inner)),
        syn::Meta::List(list) if path_is_ident(&list.path, "all") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|metas| !metas.is_empty() && metas.iter().all(meta_holds_without_test)),
        syn::Meta::List(list) if path_is_ident(&list.path, "any") => list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|metas| metas.iter().any(meta_holds_without_test)),
        _ => false,
    }
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

/// The attributes on a trait member, whatever kind of member it is.
fn trait_item_attrs(item: &syn::TraitItem) -> &[syn::Attribute] {
    match item {
        syn::TraitItem::Const(item) => &item.attrs,
        syn::TraitItem::Fn(item) => &item.attrs,
        syn::TraitItem::Type(item) => &item.attrs,
        syn::TraitItem::Macro(item) => &item.attrs,
        _ => &[],
    }
}

/// The attributes on a foreign item, whatever kind of item it is.
fn foreign_item_attrs(item: &syn::ForeignItem) -> &[syn::Attribute] {
    match item {
        syn::ForeignItem::Fn(item) => &item.attrs,
        syn::ForeignItem::Static(item) => &item.attrs,
        syn::ForeignItem::Type(item) => &item.attrs,
        syn::ForeignItem::Macro(item) => &item.attrs,
        _ => &[],
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

/// Whether `contents` carries a top-level `#![cfg(test)]`, exactly as `has_cfg_test`
/// reads it on an item's own attributes.
///
/// A file reached through an unconditional `mod name;` is exactly as test-only as one
/// the parent gated with `#[cfg(test)] mod name;` when the file's own inner attribute
/// says so — the attribute lands on the module the `mod` item names either way, only
/// spelled where the module's own file can carry it instead of where it is declared.
/// Found by Codex review of this change (PR #143), round 33: `module_tree` read only
/// the parent declaration's own gating and never a reached file's own inner attribute,
/// so a file that is entirely `#![cfg(test)]` was walked as production-reachable and a
/// `Clone` impl or a macro invocation inside it — never shipped — rejected valid code.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn crate_root_is_cfg_test_gated(contents: &str) -> Result<bool, syn::Error> {
    let file = parse_rust(contents)?;
    Ok(has_cfg_test(&file.attrs))
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

/// Every `mod <name>` declaration in `contents`: inline or out-of-line, at any
/// nesting depth.
///
/// A local module can shadow an external crate of the same name at the point it
/// is declared, so a caller judging that needs every name this file declares, not
/// only its top-level ones. Items under exactly `#[cfg(test)]` are skipped,
/// structurally — test code declares no module a shipped build sees.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn declared_module_names(contents: &str) -> Result<Vec<String>, syn::Error> {
    let file = parse_rust(contents)?;
    let mut names = Vec::new();
    collect_module_names(&file.items, &mut names);
    Ok(names)
}

fn collect_module_names(items: &[syn::Item], names: &mut Vec<String>) {
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        if let syn::Item::Mod(module) = item {
            names.push(ident_name(&module.ident));
            if let Some((_, nested)) = module.content.as_ref() {
                collect_module_names(nested, names);
            }
        }
    }
}

/// One `use` binding: the name it introduces and the path it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseAlias {
    /// The name the importing file actually writes, e.g. `Pollable`.
    pub local: String,
    /// The path it stands for, as written, e.g. `["core", "future", "Future"]`.
    pub target: Vec<String>,
    /// Whether `target` was written `use ::a::b as c;`. A leading `::` reaches
    /// the extern prelude directly, past every local scope on purpose — so
    /// `target`'s first segment must never be looked up as a local alias
    /// (Codex review, PR #160, round 8).
    pub absolute: bool,
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
            syn::Item::Use(use_item) => collect_tree_aliases(
                &use_item.tree,
                use_item.leading_colon.is_some(),
                prefix,
                aliases,
            ),
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    collect_item_aliases(nested, prefix, aliases);
                }
            }
            _ => {}
        }
    }
}

/// The `use` bindings `items` declares directly, at its own level only.
///
/// Unlike [`collect_item_aliases`], this does not recurse into a nested `mod`.
/// Real Rust scopes a `use` binding to the module that declares it: an inner
/// module does not inherit an outer one's aliases, and a sibling module's
/// aliases are not visible either. A resolver that read every alias in the
/// file as one flat list could chain a name through an unrelated module's
/// rename and report a real, correct `impl` as a fifth future (issue #109
/// review). Each caller that walks into a nested module must call this again
/// on that module's own items, so every scope stays its own.
fn own_aliases(items: &[syn::Item]) -> Vec<UseAlias> {
    let mut aliases = Vec::new();
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        if let syn::Item::Use(use_item) = item {
            collect_tree_aliases(
                &use_item.tree,
                use_item.leading_colon.is_some(),
                &mut Vec::new(),
                &mut aliases,
            );
        }
    }
    aliases
}

/// The inline sibling modules `items` declares directly, at its own level only:
/// `mod name { .. }` and the items inside it.
///
/// Issue #169: a plain relative path can name a sibling module instead of a
/// `use` alias, e.g. `traits::Pollable` beside `mod traits { .. }`.
/// [`resolve_segments`] uses this to step into that module and keep
/// resolving there. An out-of-line declaration (`mod name;`) has no body
/// here to step into, and a `#[cfg(test)]` module is skipped — the same
/// reason `own_aliases` skips one (issue #51).
fn own_modules(items: &[syn::Item]) -> Vec<(String, &[syn::Item])> {
    items
        .iter()
        .filter(|item| !has_cfg_test(item_attrs(item)))
        .filter_map(|item| match item {
            syn::Item::Mod(module) => module
                .content
                .as_ref()
                .map(|(_, nested)| (ident_name(&module.ident), nested.as_slice())),
            _ => None,
        })
        .collect()
}

/// Every item anywhere in `items`, at any nesting depth, `mod` blocks
/// included.
///
/// Half of [`resolve_segments`]'s loop bound: a module descent (issue #169)
/// always moves to a strictly smaller, physically nested item subtree, so
/// it can never need more steps than there are items in the file.
/// [`total_alias_count`] is the other half, for alias hops.
fn item_count(items: &[syn::Item]) -> usize {
    items
        .iter()
        .map(|item| match item {
            syn::Item::Mod(module) => {
                1 + module
                    .content
                    .as_ref()
                    .map_or(0, |(_, nested)| item_count(nested))
            }
            _ => 1,
        })
        .sum()
}

/// Every `use` alias anywhere in `items`, at any nesting depth — every leaf
/// of every group, unlike [`own_aliases`], which reads one scope's own
/// direct declarations only.
///
/// [`resolve_segments`]'s loop bound needs this count, not [`item_count`]'s:
/// one `use` item can declare many chained aliases in a single group
/// (`use m::{a as b, b as c, c as d};`), so counting items alone undercounts
/// how many alias hops a real chain may need (Codex review, PR #176).
fn total_alias_count(items: &[syn::Item]) -> usize {
    let mut aliases = Vec::new();
    collect_item_aliases(items, &mut Vec::new(), &mut aliases);
    aliases.len()
}

fn collect_tree_aliases(
    tree: &syn::UseTree,
    absolute: bool,
    prefix: &mut Vec<String>,
    aliases: &mut Vec<UseAlias>,
) {
    // `.unraw()` throughout: `r#Klon` names the same local binding a plain `Klon` would
    // when `Klon` is not a keyword, and a later comparison against the plain spelling
    // must not miss the raw one (issues #68/#90's reasoning for `extern_crate_names`).
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(ident_name(&path.ident));
            collect_tree_aliases(&path.tree, absolute, prefix, aliases);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            // A raw identifier cannot spell `self`. This check needs no `ident_is`.
            if name.ident != "self" {
                aliases.push(UseAlias {
                    local: ident_name(&name.ident),
                    target: [prefix.clone(), vec![ident_name(&name.ident)]].concat(),
                    absolute,
                });
            }
        }
        syn::UseTree::Rename(rename) => {
            aliases.push(UseAlias {
                local: ident_name(&rename.rename),
                target: [prefix.clone(), vec![ident_name(&rename.ident)]].concat(),
                absolute,
            });
        }
        syn::UseTree::Glob(_) => {}
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                collect_tree_aliases(tree, absolute, prefix, aliases);
            }
        }
    }
}

/// Whether `tree` names a glob anywhere in it — `use a::*;` directly, or nested inside a
/// group such as `use a::{b, c::*};` — the same shape [`collect_tree_aliases`] walks, but
/// answering "is one here at all" instead of collecting the aliases the non-glob branches
/// name.
fn tree_has_glob(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Glob(_) => true,
        syn::UseTree::Path(path) => tree_has_glob(&path.tree),
        syn::UseTree::Group(group) => group.items.iter().any(tree_has_glob),
        syn::UseTree::Name(_) | syn::UseTree::Rename(_) => false,
    }
}

/// A synthetic [`GLOB_IMPORT_MARKER`] alias, present exactly when `items` directly
/// declares a non-`#[cfg(test)]`-gated `use` naming a glob anywhere in its tree.
///
/// See [`GLOB_IMPORT_MARKER`] for why this is registered and where it is consulted. A
/// `#[cfg(test)]`-gated glob is excluded for [`direct_scope_aliases`]'s reason: code
/// reachable only from a shipped struct could never reach a name a test-only import
/// brought in. A glob gated by any *other* `cfg` is still included, matching
/// [`direct_scope_aliases`]'s own conservative reading of an alias it cannot evaluate the
/// condition of.
fn glob_marker_alias<'a>(items: impl IntoIterator<Item = &'a syn::Item>) -> Vec<UseAlias> {
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        if let syn::Item::Use(use_item) = item {
            if tree_has_glob(&use_item.tree) {
                return vec![UseAlias {
                    local: GLOB_IMPORT_MARKER.to_owned(),
                    target: Vec::new(),
                    absolute: false,
                }];
            }
        }
    }
    Vec::new()
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
    struct PathVisitor<'ast> {
        stack: Vec<&'ast [syn::Item]>,
        paths: Vec<ResolvedPath>,
    }

    impl<'ast> syn::visit::Visit<'ast> for PathVisitor<'ast> {
        fn visit_item_use(&mut self, _use: &'ast syn::ItemUse) {}

        fn visit_item(&mut self, item: &'ast syn::Item) {
            // Test code is not shipped code: a path named only under `#[cfg(test)]`
            // constructs nothing the gate pins (issue #51).
            if has_cfg_test(item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
            // A nested module's own item list goes on top of the stack
            // (issue #109 review): pushed while walking it, popped
            // afterward, so `super::`/`crate::` can still reach an ancestor
            // scope while the module's own scope never inherits it
            // implicitly. An out-of-line declaration (`mod x;`) has no body
            // to push, but its own name and attributes must still be
            // visited the default way — an early return here had skipped
            // them (Codex review, PR #160), hiding a banned identifier
            // spelled as a module name.
            let pushed = node.content.is_some();
            if let Some((_, items)) = node.content.as_ref() {
                self.stack.push(items);
            }
            syn::visit::visit_item_mod(self, node);
            if pushed {
                self.stack.pop();
            }
        }

        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.paths.push(ResolvedPath {
                segments: resolve_segments(path, &self.stack),
            });
            syn::visit::visit_path(self, path);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = PathVisitor {
        stack: vec![&file.items],
        paths: Vec::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.paths)
}

/// `path`'s segments, resolved against `stack` — the item lists of the
/// lexical scopes from the file this scan read (index 0) to the current
/// module (the last one). Each scope's own `use` aliases and sibling `mod`
/// blocks are read from its item list on demand.
///
/// Real Rust does not let a nested module inherit an outer one's aliases
/// just by being written inside it (issue #109 review), but `self::` and
/// `super::` are not inheritance — each names a scope explicitly, the same
/// way regardless of nesting depth: `self` is the current scope and `super`
/// is one level up (repeatable: `super::super::X`), bounded at the file
/// this scan read. `crate::` is a residual limit rather than index 0 of
/// this stack: this function sees one file, never the crate, so it has no
/// way to tell whether that file is really the crate root (Codex review,
/// PR #160, round 6). Each is consumed before every lookup, because an
/// alias's own target can itself start with one, e.g.
/// `pub use super::Pollable as Awaitable;` (Codex review, PR #160).
///
/// A plain relative path may also name a sibling `mod` block declared in
/// the same scope, e.g. `traits::Pollable` beside `mod traits { .. }`
/// (issue #169). Stepping into that module this way leaves the lexical
/// ancestor stack behind: a module reached by name has no ancestor this
/// per-file scan can identify past the point it was entered from, so
/// `self::` still resolves inside it but `super::` does not. That is the
/// same residual-limit shape as `crate::` and a top-level `super::` above.
/// A path of one segment never steps into a module: `impl Future for X`
/// names a trait or type called `Future`, not the module block, even when
/// one exists by that name in the same scope (Codex review, PR #176) —
/// there is nothing left to resolve inside it, so descending would only
/// throw the name away.
fn resolve_segments(path: &syn::Path, stack: &[&[syn::Item]]) -> Vec<String> {
    let mut segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    if path.leading_colon.is_some() {
        return segments;
    }
    let mut scope = stack.len().saturating_sub(1);
    // `None` while resolution is still on the lexical ancestor stack;
    // `Some(items)` once it has stepped into a sibling module by name
    // (issue #169), at which point `scope` stops tracking it.
    let mut entered: Option<&[syn::Item]> = None;
    // Module names consumed by descent since the last real alias
    // substitution (issue #169; Codex review, PR #176). Descent alone
    // decides only which scope to search next — it must not remove a name
    // from the answer unless an alias actually substituted for it. `self`
    // and `super` are safe to drop unconditionally because `self::X` and
    // `X` name the same thing; a module name is not: `TimerSpec::BestEffort`
    // with no alias anywhere in `TimerSpec` names a real item by that whole
    // path, and returning bare `BestEffort` would silently rewrite it.
    // Cleared on every alias hit, because a substitution is a legitimate
    // answer standing for everything read to reach it.
    let mut descended_prefix: Vec<String> = Vec::new();
    // A renamed re-export chains one alias to another, and a plain
    // relative path can step into a sibling module (issue #169), possibly
    // more than once. Bounded by the whole file's own alias count plus its
    // item count: enough for any real chain or descent, and it stops a
    // crafted alias cycle (`use a as b; use b as a;`) from looping forever.
    // The item count alone is not enough (Codex review, PR #176): one `use`
    // item can pack many chained hops into a single group,
    // `use m::{a as b, b as c, ...};`, so counting items undercounts how
    // many hops a real, acyclic chain may need. Module descent cannot
    // cycle on its own, since each step moves to a strictly smaller,
    // physically nested subtree — the item count alone bounds that half.
    let bound = stack
        .first()
        .map_or(0, |items| item_count(items) + total_alias_count(items))
        + 1;
    for _ in 0..=bound {
        let items = if let Some(items) = entered {
            consume_self_prefix(&mut segments);
            items
        } else {
            consume_scope_prefix(&mut segments, &mut scope);
            stack.get(scope).copied().unwrap_or_default()
        };
        let Some(first) = segments.first().cloned() else {
            break;
        };
        if let Some(alias) = own_aliases(items)
            .iter()
            .find(|candidate| candidate.local == first)
        {
            let mut resolved = alias.target.clone();
            resolved.extend(segments.drain(1..));
            segments = resolved;
            descended_prefix.clear();
            // `use ::a::b as c;` reaches the extern prelude directly, past
            // every local scope on purpose (Codex review, PR #160, round
            // 8): `a` is never a local alias, whatever else in this file
            // happens to share its spelling. Stop the chain here rather
            // than looking `a` up.
            if alias.absolute {
                return segments;
            }
            continue;
        }
        // A one-segment path names an item, not a module to step into
        // (Codex review, PR #176): `own_modules` is not even consulted
        // once `segments` has nothing left past the head.
        if segments.len() > 1 {
            if let Some((_, module_items)) = own_modules(items)
                .into_iter()
                .find(|(name, _)| *name == first)
            {
                descended_prefix.push(segments.remove(0));
                entered = Some(module_items);
                continue;
            }
        }
        break;
    }
    match entered {
        Some(_) => consume_self_prefix(&mut segments),
        None => consume_scope_prefix(&mut segments, &mut scope),
    }
    // Nothing resolved past the last module entered by name: give the
    // names descent consumed back, so the answer is the path as written
    // rather than a name silently thrown away (Codex review, PR #176).
    descended_prefix.append(&mut segments);
    descended_prefix
}

/// Consumes a leading `self` segment, if there is one. The half of
/// [`consume_scope_prefix`] that still applies once resolution has stepped
/// into a module by name (issue #169): `self` still names that module, but
/// there is no ancestor scope left here for `super` to step to.
fn consume_self_prefix(segments: &mut Vec<String>) {
    while segments.first().map(String::as_str) == Some("self") {
        segments.remove(0);
    }
}

/// Consumes leading `self`/`super` segments, moving `scope` — an index
/// into the alias stack — to match: `self` leaves it where it is, and
/// `super` moves it one level toward the file this scan read. A `super`
/// consumed while `scope` is already at that file's own top level (index
/// 0) would need to step *above* the file — the module that declared it
/// as `mod child;`, which this per-file scan never sees — so it is left
/// in place instead, the same reason a leading `crate` is (Codex review,
/// PR #160, round 7: `scope`'s floor at 0 had silently stood in for that
/// unknown outer module rather than refusing to answer).
fn consume_scope_prefix(segments: &mut Vec<String>, scope: &mut usize) {
    loop {
        match segments.first().map(String::as_str) {
            Some("self") => {
                segments.remove(0);
            }
            Some("super") if *scope > 0 => {
                segments.remove(0);
                *scope -= 1;
            }
            _ => break,
        }
    }
}

/// The self types of every `impl <path ending in Future> for T` in `contents`.
///
/// The check matches the trait by its resolved last segment. So `Future`
/// still identifies the implementor when the file imports it under an
/// alias, or renames it through a chain of aliases (issue #109). An
/// `impl` of a different trait that also ends in `Future` keeps the old
/// textual check's verdict. Only the `Future` trait matters here — a
/// workflow can `.await` only that trait. The four pinned futures are all
/// bare `impl Future for ...`.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn future_trait_implementors(contents: &str) -> Result<Vec<String>, syn::Error> {
    let file = parse_rust(contents)?;
    let mut stack = vec![file.items.as_slice()];
    let mut implementors = Vec::new();
    collect_future_implementors(&file.items, &mut stack, &mut implementors);
    Ok(implementors)
}

fn collect_future_implementors<'ast>(
    items: &'ast [syn::Item],
    stack: &mut Vec<&'ast [syn::Item]>,
    implementors: &mut Vec<String>,
) {
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Impl(implementation) => {
                if let Some((_, trait_path, _)) = implementation.trait_.as_ref() {
                    let resolved = resolve_segments(trait_path, stack);
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
                    // The nested module's own item list goes on top of the
                    // stack (issue #109 review): pushed for the recursion,
                    // popped after, so `super::`/`crate::` inside it can
                    // still reach an ancestor scope on purpose without this
                    // scope's own chain resolving through one of its
                    // renames by accident.
                    stack.push(nested);
                    collect_future_implementors(nested, stack, implementors);
                    stack.pop();
                }
            }
            _ => {}
        }
    }
}

/// The identifier a further alias lookup has to match against `segments`, and what
/// follows it — stripping a leading `self`, because a module qualifier is not itself an
/// aliasable name: `self::C` means "this module's own `C`", whether that qualifier opens
/// a derive path directly (`#[derive(self::C)]`) or shows up partway through, in an
/// alias's own target (`use self::C as Klon;`).
///
/// `crate` is deliberately *not* stripped here, unlike `self`. Found by Codex review of
/// this change (PR #143), round 25: `self::C` really does mean "this file's own module
/// scope", which is exactly the table every caller of this function builds — but a
/// *bare* `crate::C` means the *crate root's* own scope, a different file this scan
/// never reads (every function here parses one file's `contents` alone, and none of
/// the files `recovery-surface` walks is ever the crate root). Treating the two as
/// interchangeable let `impl crate::C for Recovery` resolve `C` against this file's own
/// table as if `crate::` were `self::`, silently missing that the real `C` lives in
/// `lib.rs`. `every_resolution` fails every `crate`-qualified path closed the same way
/// it already fails a `super`-qualified path — round 25 only closed the bare,
/// two-segment `crate::NAME` shape, and round 32 closed the rest once `every_resolution`
/// stopped being shared with `future_trait_implementors`'s scan (see `every_resolution`'s
/// own doc for why that made the narrower fix's reasoning obsolete).
fn lookup_candidate(segments: &[String]) -> Option<(&str, &[String])> {
    match strip_self_prefix(segments) {
        [first, tail @ ..] => Some((first, tail)),
        [] => None,
    }
}

/// `segments`, with a leading `self` qualifier stripped off when at least one more
/// segment follows it — a lone `self` with nothing after it is left alone, the same
/// edge case [`lookup_candidate`]'s own second arm already leaves unchanged.
fn strip_self_prefix(segments: &[String]) -> &[String] {
    match segments {
        [first, rest @ ..] if first == "self" && !rest.is_empty() => rest,
        _ => segments,
    }
}

/// [`lookup_candidate`]'s multi-segment twin: every joined-prefix name a qualified path
/// through nested inline modules could match against
/// [`direct_scope_module_aliases`]'s synthetic `mod_name::exported_name` (or
/// `outer::inner::exported_name`, for a module nested inside a module) aliases, longest
/// prefix first, each paired with what follows it. Empty when fewer than two segments
/// remain after stripping a leading `self`, the same way [`lookup_candidate`] answers
/// [`None`] only when none remain at all.
///
/// Found by Codex review of this change (PR #143), round 28: `traits::C` is an
/// ordinary identifier (`traits`) followed by another, and [`lookup_candidate`] alone
/// only ever looks up the first — so a path qualified by a sibling module's own name
/// had no alias to match against at all, and fell through to reporting the bare,
/// still-aliased last segment. Round 31 found the fixed two-segment join was itself too
/// narrow: `traits::nested::C`, qualified through a module nested *inside* another
/// nested module, joins its first three segments rather than two, and
/// `direct_scope_module_aliases` gained a matching recursive registration on the same
/// round — every prefix length is tried now, rather than only the shortest that could
/// possibly qualify a directly nested module's own export.
fn qualified_candidates(segments: &[String]) -> Vec<(String, &[String])> {
    let stripped = strip_self_prefix(segments);
    let mut candidates = Vec::new();
    if stripped.len() < 2 {
        return candidates;
    }
    for split in (2..=stripped.len()).rev() {
        let Some((prefix, tail)) = stripped.get(..split).zip(stripped.get(split..)) else {
            continue;
        };
        candidates.push((prefix.join("::"), tail));
    }
    candidates
}

/// Sentinel a derive-path resolution emits in place of a name it could not pin down,
/// rather than guessing one.
///
/// Three things earn it: a path (the derive path itself, or an alias reached partway
/// through resolving it) that opens with `super`, whose meaning depends on the *parent*
/// module's own bindings; a *bare* two-segment `crate::NAME`, whose meaning depends on
/// the *crate root's* own bindings (round 25) — a file this module never reads either
/// way, since every function here parses one file's `contents` alone — and a candidate
/// this scan gave up chasing once its bound on how many it will explore was reached. A
/// `crate`-qualified path of any length is this shape too: see `lookup_candidate`'s own
/// doc, and `every_resolution`'s own doc below, for why round 25 closed only the bare,
/// two-segment form and round 32 closed the rest.
///
/// A caller checking for one specific trait name must treat this the same as a match:
/// "this scan could not run it down" is not evidence that it is not `Clone`, and reading
/// it as a plain identifier that merely fails to equal `"Clone"` is exactly the bypass
/// Codex found on PR #143's eighth review round — `super::C` resolved to the harmless-
/// looking name `"C"` because there was no local alias named `super` to fail the lookup
/// against, and a dropped 65th candidate resolved to its own still-aliased name (`"Klon"`)
/// rather than to anything that could be compared against `"Clone"` at all. This sentinel
/// is the same fail-closed shape [`struct_derives`] already uses for a file that will not
/// parse or a struct that is not declared: an answer this scan cannot stand behind is
/// reported as such, not folded into "resolved, and not a match".
pub const UNRESOLVED_DERIVE: &str = "<unresolved derive>";

/// Sentinel [`every_resolution`] emits for a name that resolves, unambiguously, to a
/// struct, enum or union declared locally in the very scope a reference to it appears
/// in — a *different* declaration than the one an enclosing scope's identically-named
/// item would mean, so it must never be folded into whatever that enclosing name would
/// compare equal to. Unlike [`UNRESOLVED_DERIVE`], this is not "this scan gave up": it
/// is confident the name means something, and confident that something is not whatever
/// is being searched for in another scope entirely. See
/// [`shadow_aliases_for_local_types`] for where this is registered and
/// [`trait_implementors_for_pinned_type`] for why.
const LOCAL_SHADOWED_TYPE: &str = "<locally shadowed type>";

/// Reserved [`UseAlias::local`] value marking that a scope contains an unresolved glob
/// `use` — never a real Rust identifier, so it can never collide with one a lookup would
/// otherwise search for.
///
/// Found by Codex review of this change (PR #143), round 30: `mod traits { pub use
/// core::clone::Clone as C; } use traits::*; #[derive(C)] struct Recovery;` is legal
/// Rust, and [`collect_tree_aliases`] deliberately drops `UseTree::Glob` — this scan does
/// not perform name resolution, so it has no way to know what a glob import actually
/// brings into scope (see the module's "What is not checked" limit on glob imports).
/// Silently treating `C` as an ordinary, unaliased identifier let the derive resolve to
/// the harmless-looking bare name `C` instead of `Clone`. [`glob_marker_alias`] registers
/// this sentinel for a scope that contains one, and [`every_resolution`]'s "no matching
/// alias" fallback fails closed to [`UNRESOLVED_DERIVE`] rather than trusting the bare
/// name whenever it is present — the same fail-closed shape as an unresolvable `super`-
/// or `crate`-qualified path, because a name a glob *might* have rebound is exactly as
/// unaccountable as one this scan gave up chasing.
const GLOB_IMPORT_MARKER: &str = "*";

/// Every name `path` could ultimately mean, considering every alias that could bind any
/// step along the way — not just the one [`resolve_segments`] would pick by taking the
/// first match at the first step alone.
///
/// Two things this branches for, both because this scan does not evaluate which of
/// several possibilities is real. Mutually exclusive `cfg`s can validly bind one local
/// name to two different targets: `#[cfg(any())] use core::fmt::Debug as Klon;
/// #[cfg(all())] use core::clone::Clone as Klon;` derives `Clone` under the condition
/// that always holds, and resolving only whichever `Klon` happened to be declared first
/// would miss it whenever that one loses the race. And a resolved target can itself need
/// another hop — `use core::clone::Clone as C; use self::C as Klon;` needs two — which
/// [`lookup_candidate`]'s `self` stripping makes visible at every step, not only the
/// first.
///
/// Bounded twice over, so neither an adversarial pile of aliases nor a cycle spelled by
/// hand (`use A as B; use B as A;` — not something real Rust name resolution could
/// produce, but something a text file can still spell) can make this loop unbounded: at
/// most `aliases.len()` hops, and at most `MAX_CANDIDATES` names explored in total. Both
/// bounds, a `super`-qualified path, and a `crate`-qualified path met at any hop,
/// contribute [`UNRESOLVED_DERIVE`] rather than the segment sequence a bound or a
/// missing qualifier happened to stop resolution at — so nothing this scan stopped
/// chasing early, and nothing it could never chase in the first place, is silently
/// treated as a plain name that simply is not the one being looked for.
///
/// The `crate`-qualified case used to be narrower — round 25 only closed a bare,
/// two-segment `crate::NAME`, because this function was shared with
/// `future_trait_implementors`'s own scan of every file the crate has, and failing
/// closed on a longer `crate::a::b::NAME` there rejected `waymaker-embassy/src/
/// wiring.rs`'s own `use crate::dispatch::ActivityDispatcher;` — a real, unaliased
/// import that scan has to read correctly. That sharing ended when
/// `future_trait_implementors` moved to its own lexical-scope `resolve_segments`: this
/// function is `recovery-surface`'s alone now, over a reachable tree that never
/// includes `wiring.rs` or any file outside `waymaker-flash`, so nothing here still
/// needs the narrower carve-out. Round 32 found the gap it left: `impl
/// crate::traits::C for Recovery`, naming a crate-root re-export two segments down
/// (`mod traits { pub use core::clone::Clone as C; }`), resolved to the bare,
/// harmless-looking name `C` exactly the way a bare `crate::C` used to — every
/// `crate`-qualified path fails closed now, regardless of length.
fn every_resolution(path: &syn::Path, aliases: &[UseAlias]) -> Vec<String> {
    if path.leading_colon.is_some() {
        // Round 34: an absolute path (`::dep::C`) reaches the extern prelude — which,
        // through `extern crate self as dep;`, can be this very crate under another
        // name — so its root can be exactly as aliased as any local one, and this
        // per-file scan has no crate-level view to resolve it with. Trusting the last
        // segment (`C`) as a plain name let `impl ::dep::C for Recovery`, with `dep`
        // renaming this crate and `C` a re-exported alias for `Clone`, read as an
        // unrelated trait; failing closed matches `crate::`-qualified paths of any
        // length, which fail the same way for the same reason.
        return vec![UNRESOLVED_DERIVE.to_owned()];
    }

    // Round 34: a segment that is itself an aliased local name and also carries
    // generic arguments can resolve to something this module cannot compute —
    // `type Identity<T> = T; impl Clone for Identity<Recovery> { .. }` is legal Rust
    // that implements `Clone` for `Recovery`, because substituting `Recovery` for `T`
    // makes `Identity<Recovery>` the type `Recovery` itself. Resolving the alias here
    // means substituting its target for the segment, which is real type-checking this
    // module does not do; it reads identifiers only and drops every generic argument
    // along the way (`segments`, below), so reporting a name for a segment shaped like
    // this would be reporting a guess. Failing closed matches every other substitution
    // this module cannot perform, from a projected associated type to a widened alias
    // target.
    //
    // A [`LOCAL_SHADOWED_TYPE`] entry is excluded: it names a struct, enum or union
    // declared right here, so the segment already names the real type directly — a
    // generic parameter on `Wrapper<T>` where `Wrapper` is that local declaration is
    // not a substitution this module has to compute, only a real generic type using
    // its own real name.
    if path.segments.iter().any(|segment| {
        !matches!(segment.arguments, syn::PathArguments::None)
            && aliases.iter().any(|alias| {
                alias.local == ident_name(&segment.ident)
                    && alias.target != [LOCAL_SHADOWED_TYPE.to_owned()]
            })
    }) {
        return vec![UNRESOLVED_DERIVE.to_owned()];
    }

    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    resolve_segment_chain(segments, aliases)
}

/// [`every_resolution`]'s own hop-chasing BFS, factored out over an already-extracted
/// segment list rather than a [`syn::Path`] — everything past `every_resolution`'s own
/// path-shaped pre-checks (a leading `::`, a generic argument on an aliased segment),
/// which have no equivalent once a target is already a plain `Vec<String>`.
///
/// [`direct_scope_module_aliases`] is the other caller, and the reason this split
/// exists: round 37 of Codex review on this change (PR #143) found that a *chained*
/// export inside a nested module — `mod traits { pub use core::clone::Clone as C; pub
/// use self::C as D; }` — registered `traits::D`'s synthetic alias with the target
/// `self::C` copied verbatim, never resolved against `traits`' own scope where `C` is
/// declared. The next hop then looked `self::C` up in the *outer* file's alias table,
/// which only has `traits::C` — the qualified name — not the bare `C` `traits`'s own
/// scope would resolve it through, so the lookup fell through to the harmless-looking
/// last segment, `C`, instead of chasing the second hop to `Clone`. Calling this same
/// function against the nested module's *own* alias list, exactly as `every_resolution`
/// already would if `self::C` had been written where `every_resolution` could see it, is
/// what closes it — the same resolution a real compiler performs, run once more before
/// the qualifying prefix is added, rather than a second, narrower implementation of it.
fn resolve_segment_chain(segments: Vec<String>, aliases: &[UseAlias]) -> Vec<String> {
    const MAX_CANDIDATES: usize = 64;

    let mut frontier = vec![segments];
    let mut finished: Vec<String> = Vec::new();
    for _ in 0..=aliases.len() {
        if frontier.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for current in frontier {
            // A `super`-qualified path names something in the parent module's own
            // namespace, which no function in this module reads — see
            // `UNRESOLVED_DERIVE`.
            if current.first().is_some_and(|first| first == "super") {
                finished.push(UNRESOLVED_DERIVE.to_owned());
                continue;
            }
            // Round 29: a candidate that has already resolved to `UNRESOLVED_DERIVE` at
            // an earlier hop — through `direct_scope_opaque_module_aliases`, an
            // out-of-line `mod name;` registered as a synthetic alias to this same
            // sentinel — can still carry a trailing tail (`traits::C` resolving one hop
            // to `["<unresolved derive>", "C"]`). Without this guard the tail survives:
            // it matches neither `qualified_candidates` nor `lookup_candidate`, so the
            // "no matching alias, take the last segment" branch below silently reports
            // the harmless-looking `"C"` rather than propagating that the hop it went
            // through could not be resolved at all.
            if current
                .first()
                .is_some_and(|first| first == UNRESOLVED_DERIVE)
            {
                finished.push(UNRESOLVED_DERIVE.to_owned());
                continue;
            }
            // Round 29: the companion sentinel for a name shadowed by a local
            // struct/enum/union declaration (see `LOCAL_SHADOWED_TYPE`) propagates the
            // same way, for the same reason — a qualified reference through it
            // (`local_shadow_alias::C`, if anything ever chased a further hop through
            // one) must not silently resolve to its own trailing segment either.
            if current
                .first()
                .is_some_and(|first| first == LOCAL_SHADOWED_TYPE)
            {
                finished.push(LOCAL_SHADOWED_TYPE.to_owned());
                continue;
            }
            // A `crate::`-qualified path — of any length — names something in the
            // crate root's own scope, which this scan never reads: every function
            // here parses one file's `contents` alone, and none of the files
            // `recovery-surface` walks is ever the crate root. Round 25 fixed only
            // the bare, two-segment `crate::NAME` shape, reasoning that a longer
            // `crate::a::b::NAME` names another module's own real declaration rather
            // than asking for an identifier lookup in a table this scan does not
            // have — but that reasoning rested on `every_resolution` being shared
            // with `future_trait_implementors`'s scan of every file in the crate,
            // `waymaker-embassy/src/wiring.rs`'s own `use crate::dispatch::
            // ActivityDispatcher;` included, where failing closed on the longer shape
            // really did reject an ordinary, unaliased import. The two scans no
            // longer share this function — `future_trait_implementors` now resolves
            // through its own lexical-scope `resolve_segments` — so `every_resolution`
            // is recovery-surface's alone, and round 32 found the gap the narrower
            // fix left open: `impl crate::traits::C for Recovery`, where the crate
            // root's own `mod traits { pub use core::clone::Clone as C; }` re-exports
            // `Clone` two segments down, resolved to the bare, harmless-looking name
            // `C` exactly the way a bare `crate::C` used to. Every `crate`-qualified
            // path now fails closed the same way a `super`-qualified one already does.
            if current.first().is_some_and(|first| first == "crate") {
                finished.push(UNRESOLVED_DERIVE.to_owned());
                continue;
            }
            if current.is_empty() {
                continue;
            }
            // Round 28: a qualified candidate is tried *before* the plain
            // single-segment one, because `traits::C` — where `traits` is a
            // sibling inline module registering `C` as one of its own aliases via
            // `direct_scope_module_aliases` — has to be looked up as the joined
            // name `traits::C`, not as the bare identifier `traits` (which no
            // ordinary `use` or `type` alias is ever named after). Round 31
            // generalized this to every prefix length, longest first, for a path
            // qualified through more than one level of nested inline module
            // (`traits::nested::C`) — see `qualified_candidates`. Every one is
            // tried rather than the first alone, exactly as this function already
            // tries every alias sharing one local name: a plain single-segment
            // alias and a qualified one could both exist for unrelated reasons,
            // and either resolving to the trait being searched for is enough.
            let mut matched = false;
            for (joined, tail) in qualified_candidates(&current) {
                for alias in aliases.iter().filter(|alias| alias.local == joined) {
                    matched = true;
                    if finished.len() + next.len() >= MAX_CANDIDATES {
                        finished.push(UNRESOLVED_DERIVE.to_owned());
                        continue;
                    }
                    let mut resolved = alias.target.clone();
                    resolved.extend(tail.iter().cloned());
                    next.push(resolved);
                }
            }
            if let Some((candidate, tail)) = lookup_candidate(&current) {
                for alias in aliases.iter().filter(|alias| alias.local == candidate) {
                    matched = true;
                    if finished.len() + next.len() >= MAX_CANDIDATES {
                        finished.push(UNRESOLVED_DERIVE.to_owned());
                        continue;
                    }
                    let mut resolved = alias.target.clone();
                    resolved.extend(tail.iter().cloned());
                    next.push(resolved);
                }
            }
            if !matched {
                // Round 30: a glob import in the ambient scope could have bound this
                // very name to anything, and this scan does not perform name
                // resolution — see `GLOB_IMPORT_MARKER`. Round 39 generalizes it to a
                // *qualified* candidate: `traits::C` is exactly as uncertain when
                // `traits` itself has a glob nobody chased (`direct_scope_module_
                // aliases`'s own `"traits::*"` marker) as a bare `C` is when the
                // ambient scope does, even though no plain alias named `traits::C`
                // was ever registered for either lookup above to match.
                let stripped = strip_self_prefix(&current);
                let glob_could_have_bound_this = aliases
                    .iter()
                    .any(|alias| alias.local == GLOB_IMPORT_MARKER)
                    || (1..stripped.len()).any(|split| {
                        let Some(prefix) = stripped.get(..split) else {
                            return false;
                        };
                        let prefix = prefix.join("::");
                        aliases
                            .iter()
                            .any(|alias| alias.local == format!("{prefix}::{GLOB_IMPORT_MARKER}"))
                    });
                if glob_could_have_bound_this {
                    finished.push(UNRESOLVED_DERIVE.to_owned());
                } else if let Some(last) = current.last() {
                    finished.push(last.clone());
                }
            }
        }
        frontier = next;
    }
    // Any candidate still in flight when the hop bound above is reached can only be a
    // cycle a real compiler could never produce (`use A as B; use B as A;`) — a genuine
    // acyclic chain resolves within `aliases.len()` hops. Reporting it unresolved rather
    // than taking its still-aliased name is the same fail-closed shape as the two bounds
    // above, for the reason `UNRESOLVED_DERIVE` gives.
    finished.extend(frontier.into_iter().map(|_| UNRESOLVED_DERIVE.to_owned()));
    finished
}

/// The self types of every `impl` of `trait_name` found in `contents`, however the trait
/// path ends.
///
/// Issue #77 needs this for `Clone`: a handwritten `impl Clone for Recovery` is caught
/// the same way a `#[derive(Clone)]` is, and an alias on the trait name cannot hide the
/// implementor. This is its own implementation rather than a generalization of
/// [`future_trait_implementors`] — the two diverged once each was hardened against a
/// different set of findings (`every_resolution`'s alias-chasing and its sentinels here,
/// `resolve_segments`'s lexical-scope stack there), and re-unifying them would mean
/// carrying every fix either accumulated across onto ground the other never needed it on.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn trait_implementors(contents: &str, trait_name: &str) -> Result<Vec<String>, syn::Error> {
    let file = parse_rust(contents)?;
    // File scope is a module scope like any other — see `module_scope_aliases`, which
    // is also what a nested `mod` gets its own table from during the walk below.
    let aliases = module_scope_aliases(&file.items);
    let mut implementors = Vec::new();
    collect_trait_implementors(&file.items, &aliases, trait_name, false, &mut implementors);
    Ok(implementors)
}

/// [`trait_implementors`], refined to tell a genuine implementor of one pinned,
/// singular type from an unrelated, identically-named type declared somewhere else the
/// module tree reaches.
///
/// Issue #77's rule is not "no file in the module tree implements `Clone` for a type
/// *named* `Recovery`" — it is "no file implements `Clone` for *the* `Recovery`", and
/// those are different questions once two declarations share a name. A
/// production-reachable child file, or an inline module nested anywhere the tree
/// reaches, is free to declare its own, wholly unrelated `struct Recovery` and hand it
/// a `Clone` impl with nothing to do with the pinned type — real Rust name resolution
/// has an unqualified `Recovery` written there mean the *local* declaration, exactly as
/// [`struct_derives`] already reads only a *top-level* declaration in the pinned
/// type's own file as the one that counts for a derive. This extends that same
/// restriction to a handwritten `impl`: every directly nested inline module's own
/// struct, enum or union shadows its name for an unqualified reference inside that
/// module (via `shadow_aliases_for_local_types`, folded in wherever
/// `collect_trait_implementors` builds a nested module's own scope) — and, when
/// `is_pinned_type_file` is `false`, so does one declared at the scanned file's own top
/// level, because from the whole reachable tree's point of view a child file reached
/// through `mod name;` is exactly as nested as `mod name { .. }` would have been had
/// its contents been written inline instead.
///
/// `is_pinned_type_file` must be `true` only for the one file whose *top-level* scope
/// is where the pinned type is actually declared — every other reachable file, however
/// it is reached, is nested with respect to that declaration and gets its own top
/// level shadowed the same way an inline module's would be.
///
/// Found by Codex review of this change (PR #143), round 29.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn trait_implementors_for_pinned_type(
    contents: &str,
    trait_name: &str,
    is_pinned_type_file: bool,
) -> Result<Vec<String>, syn::Error> {
    let file = parse_rust(contents)?;
    let mut aliases = module_scope_aliases(&file.items);
    if !is_pinned_type_file {
        aliases.extend(shadow_aliases_for_local_types(file.items.iter()));
    }
    aliases.extend(glob_marker_alias(&file.items));
    let mut implementors = Vec::new();
    collect_trait_implementors(&file.items, &aliases, trait_name, true, &mut implementors);
    Ok(implementors)
}

/// The `use` aliases and plain-path `type NAME = TARGET;` aliases declared *directly*
/// in `items`, with no recursion into any nested scope at all — not a nested
/// `Item::Mod`, and not a nested body (`Fn`/`Impl`/`Trait`/`Const`/`Static`/`Enum`/
/// `Type`/`Struct`/`Union`) either.
///
/// Found by Codex review of this change (PR #143), round 13 (the `type` half) and
/// round 20 (the `use` half, named `collect_item_aliases` there): a self-type or a
/// trait path read straight off an `impl`, with no alias resolution at all, missed
/// `impl Clone for R` where `R` is a local alias of the real type — the same shape of
/// miss `every_resolution` already closes for a derive path. `unwrap_type_parens` on
/// `TARGET`: round 16 found `#[allow(unused_parens)] type R = (super::Recovery);` is
/// legal Rust whose target is `Type::Paren` rather than `Type::Path`, on the *alias
/// declaration* side of the same parenthesizing round 14 had already closed on the
/// self-type side.
///
/// This function itself never recurses — see [`module_scope_aliases`] and
/// [`collect_trait_implementors`] for why, and where the recursion that used to live
/// here moved to instead.
fn direct_scope_aliases<'a>(items: impl IntoIterator<Item = &'a syn::Item>) -> Vec<UseAlias> {
    let mut aliases = Vec::new();
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Use(use_item) => {
                collect_tree_aliases(
                    &use_item.tree,
                    use_item.leading_colon.is_some(),
                    &mut Vec::new(),
                    &mut aliases,
                );
            }
            syn::Item::Type(type_item) => {
                if let syn::Type::Path(target) = unwrap_type_parens(type_item.ty.as_ref()) {
                    let resolved_target = if target.qself.is_some() {
                        // Round 24 of Codex review on this change (PR #143): `type R =
                        // <() as Alias>::Target;`, after a reached file binds `Target`
                        // to `Recovery`, is legal Rust whose target is a projected
                        // associated type — `syn` stores the qualified self and the
                        // trait separately from `path`, which here is only `Target`.
                        // Reading `path` alone resolved `R` to the unqualified name
                        // `Target` instead of to `Recovery` or to a sentinel this
                        // module fails closed on, so `impl Clone for R` went neither
                        // matched nor flagged as unresolved. This is round 23's
                        // self-type finding one hop earlier, in the alias a self-type
                        // can be chased through rather than in the self-type itself,
                        // and it fails closed the same way: an alias nobody can chase
                        // to a real name resolves to `UNRESOLVED_DERIVE` rather than to
                        // the qualifier-stripped associated type name.
                        vec![UNRESOLVED_DERIVE.to_owned()]
                    } else if target
                        .path
                        .segments
                        .iter()
                        .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
                    {
                        // Round 28 of Codex review on this change (PR #143): `type
                        // Identity<T> = T; type R = Identity<super::Recovery>;` is legal
                        // Rust whose target names a real generic alias with an argument
                        // substituted in — reading only the segments' own identifiers and
                        // discarding `<super::Recovery>` resolved `R` to `Identity`'s own
                        // declared target, `T`, rather than to the type actually
                        // substituted in. This module does not perform generic
                        // substitution — that is real type-checking, not parsing — so an
                        // alias target carrying a generic argument anywhere along its path
                        // fails closed to `UNRESOLVED_DERIVE` the same way a projected
                        // associated type already does, rather than silently resolving
                        // through the unparameterized definition.
                        vec![UNRESOLVED_DERIVE.to_owned()]
                    } else {
                        target
                            .path
                            .segments
                            .iter()
                            .map(|segment| ident_name(&segment.ident))
                            .collect()
                    };
                    aliases.push(UseAlias {
                        local: ident_name(&type_item.ident),
                        target: resolved_target,
                        absolute: false,
                    });
                }
            }
            _ => {}
        }
    }
    aliases
}

/// The `use` aliases and plain-path type aliases visible in one module's own scope —
/// `items` — without crossing into a nested `Item::Mod`. Just [`direct_scope_aliases`]
/// under a name that says which scope it is being asked for: [`trait_implementors`]
/// calls this for file scope, and [`collect_trait_implementors`]'s own `Item::Mod` arm
/// calls it afresh for each nested module's own scope instead of inheriting the
/// caller's table.
///
/// Found by Codex review of this change (PR #143), round 20: this used to recurse
/// through every inline module, accumulating everything into one table shared across
/// the whole file, so an unrelated nested module's own `use core::clone::Clone as C;`
/// — which real Rust scopes strictly to that `mod { .. }` block, never letting it leak
/// to a sibling scope or its parent — could resolve an unrelated, identically-named
/// alias used by a completely different `impl` elsewhere in the file. That is a false
/// *positive* rather than one of this scanner's usual false negatives: a root-scope
/// `use self::Harmless as C; impl C for Recovery {}` was reported as implementing
/// `Clone`, purely because some other module anywhere in the same file happened to
/// alias an unrelated `Clone` under the same local name. Round 22 found that round 21's
/// own fix for a *function*-body alias — needed because a local `use` or `type` alias
/// is visible to a sibling item in the very same body — had reintroduced exactly this
/// shape of false positive one level down: it recursed into every function body in the
/// scope and accumulated all of *their* aliases into this same shared table too, so an
/// unrelated function's own local `use core::clone::Clone as C;` could resolve an
/// unrelated `impl C for Recovery` in a *different* function, or at the module's own
/// top level. This function no longer recurses into any body at all — see
/// [`collect_trait_implementors`] for where a body's own local aliases are computed
/// and threaded through instead, extending rather than replacing the ambient table
/// (unlike a `mod { .. }` block, a function, `impl`, `const`, `enum`, `struct`, `union`
/// or `type` body is not a scope boundary in Rust: an item declared inside one still
/// resolves names through the enclosing module's own imports).
fn module_scope_aliases<'a>(items: impl IntoIterator<Item = &'a syn::Item>) -> Vec<UseAlias> {
    let items: Vec<&syn::Item> = items.into_iter().collect();
    direct_scope_aliases(items.iter().copied())
        .into_iter()
        .chain(direct_scope_module_aliases(items.iter().copied()))
        .chain(direct_scope_opaque_module_aliases(items.iter().copied()))
        .collect()
}

/// A synthetic, fail-closed alias to [`UNRESOLVED_DERIVE`] for every directly nested
/// *out-of-line* module — `mod name;`, with its content in a sibling file this function
/// never reads — so a qualified path through its name has something to resolve against
/// rather than falling through to "no matching alias, take the last segment".
///
/// [`direct_scope_module_aliases`] is this same idea for an *inline* `mod name { .. }`,
/// which can be resolved precisely because its content is right here in `items`. An
/// out-of-line module's content lives in another file this per-file scan does not open,
/// so nothing here can say what a name qualified by it actually means — the honest
/// answer is the same one a `super`-qualified path already gets, not a guess and not a
/// silent skip.
///
/// Found by Codex review of this change (PR #143), round 29: `mod traits; impl
/// traits::C for super::Recovery { .. }`, where `traits`'s content lives in
/// `recovery/traits.rs` and binds `C` to `Clone`, had no alias for `traits::C` to
/// resolve against at all — the same gap round 28 closed for an inline module, one
/// level out.
fn direct_scope_opaque_module_aliases<'a>(
    items: impl IntoIterator<Item = &'a syn::Item>,
) -> Vec<UseAlias> {
    let mut aliases = Vec::new();
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        let syn::Item::Mod(module) = item else {
            continue;
        };
        if module.content.is_some() {
            // `direct_scope_module_aliases`'s to resolve, precisely.
            continue;
        }
        aliases.push(UseAlias {
            local: ident_name(&module.ident),
            target: vec![UNRESOLVED_DERIVE.to_owned()],
            absolute: false,
        });
    }
    aliases
}

/// A synthetic, self-referential alias for every struct, enum or union directly
/// declared in `items`, each resolving to [`LOCAL_SHADOWED_TYPE`] rather than to its
/// own name.
///
/// [`trait_implementors_for_pinned_type`] registers this for every scope that is not
/// the pinned type's own file-level declaration site, because a locally declared
/// item's name always resolves to that local declaration before it resolves to
/// anything an enclosing scope or an import could mean by the same identifier — the
/// same rule [`direct_scope_aliases`]'s own alias shadowing already applies to a `use`
/// or `type` alias redeclared in a nested scope (round 24), extended here to a struct,
/// enum or union item, which registers no alias of its own and so was invisible to
/// that mechanism entirely.
///
/// Found by Codex review of this change (PR #143), round 29: a production-reachable
/// child file, or an inline module nested anywhere the module tree reaches, declaring
/// its own unrelated `struct Recovery` and a handwritten, unqualified `impl Clone for
/// Recovery` resolved to the bare name `Recovery` exactly as a genuine implementor of
/// the pinned type would — nothing distinguished "the name `Recovery`, resolved with no
/// alias in play" from "the name `Recovery`, resolved to a *different* declaration of
/// that name local to this very scope".
fn shadow_aliases_for_local_types<'a>(
    items: impl IntoIterator<Item = &'a syn::Item>,
) -> Vec<UseAlias> {
    let mut aliases = Vec::new();
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        let name = match item {
            syn::Item::Struct(declared) => ident_name(&declared.ident),
            syn::Item::Enum(declared) => ident_name(&declared.ident),
            syn::Item::Union(declared) => ident_name(&declared.ident),
            _ => continue,
        };
        aliases.push(UseAlias {
            local: name,
            target: vec![LOCAL_SHADOWED_TYPE.to_owned()],
            absolute: false,
        });
    }
    aliases
}

/// Every alias reachable through one or more levels of qualification by a directly
/// nested inline module's own name, recursing into a module nested inside that module
/// in turn.
///
/// Found by Codex review of this change (PR #143), round 28: `mod traits { pub use
/// core::clone::Clone as C; } impl traits::C for super::Recovery { .. }` is legal
/// Rust, and a qualified trait path naming `traits::C` had no alias to resolve
/// against — `every_resolution` only ever looked up a *single* segment as a
/// candidate, so `traits` (an ordinary identifier, not `self`/`crate`/`super`) fell
/// through to the "no matching alias, take the last segment" branch and reported the
/// bare, still-aliased name `C` rather than the real trait. This registers `traits::C`
/// as a synthetic alias for whatever `C` itself resolves to inside `traits`' own
/// scope, so a path qualified by a sibling module's name can be looked up the same
/// way an unqualified one already is.
///
/// Round 31 found that stopping at one level was itself the same shape of gap: `mod
/// traits { pub mod nested { pub use core::clone::Clone as C; } }
/// #[derive(traits::nested::C)] pub struct Recovery;` is legal Rust, and this function
/// read only `traits`' own *direct* aliases, never `traits::nested`'s — so
/// `traits::nested::C` had nothing to resolve against either, one level further out
/// than round 28 closed. It now recurses into each nested module's own content the
/// same way, and prefixes whatever comes back — a direct alias, or one already
/// qualified by a module nested inside this one — with this module's own name, so a
/// path qualified through any number of nested inline modules resolves the same way a
/// single level already did. [`qualified_candidates`] is the matching generalization
/// on the lookup side: a fixed two-segment join could never have matched a
/// three-or-more-segment registration this recursion produces.
fn direct_scope_module_aliases<'a>(
    items: impl IntoIterator<Item = &'a syn::Item>,
) -> Vec<UseAlias> {
    let mut aliases = Vec::new();
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        let syn::Item::Mod(module) = item else {
            continue;
        };
        let Some((_, nested)) = module.content.as_ref() else {
            continue;
        };
        let name = ident_name(&module.ident);
        let nested_items: Vec<&syn::Item> = nested.iter().collect();
        let nested_qualified = direct_scope_module_aliases(nested_items.iter().copied());
        let scope: Vec<UseAlias> = direct_scope_aliases(nested_items.iter().copied())
            .into_iter()
            .chain(
                nested_qualified
                    .iter()
                    .filter(|alias| !is_glob_marker_name(&alias.local))
                    .cloned(),
            )
            .collect();
        // Round 37: `alias.target` used to be copied straight onto the qualified
        // entry, which is right for an ordinary re-export (`pub use core::clone::Clone
        // as C;`, target `["core", "clone", "Clone"]`, nothing further to chase) and
        // wrong for a *chained* one (`pub use self::C as D;`, target `["self", "C"]`)
        // — `self::C` names `C` in *this* nested module's own scope, which `scope`
        // already holds, but was never resolved against it before being prefixed and
        // handed to the outer caller. `resolve_segment_chain` is `every_resolution`'s
        // own hop-chasing, run here against `scope` before qualifying rather than
        // left for a second hop the outer alias table has no way to complete.
        for alias in &scope {
            for resolved in resolve_segment_chain(alias.target.clone(), &scope) {
                aliases.push(UseAlias {
                    local: format!("{name}::{}", alias.local),
                    target: vec![resolved],
                    absolute: false,
                });
            }
        }
        // Round 39 of Codex review on this change (PR #143): `mod traits { mod
        // nested { pub use core::clone::Clone as C; } pub use nested::*; }` makes
        // `traits::C` a legal, qualified reference to `Clone` through `traits`' own
        // glob re-export — but this function's synthetic scope above is built only
        // from `traits`' own explicit aliases and its nested modules' own qualified
        // *aliases*, never from a glob any of them declares, so `traits::C` had
        // nothing registered to resolve against and fell through to the
        // harmless-looking bare name `C`. A glob anywhere in `nested_items` — this
        // module's own direct glob, or one a nested module already reduced to its
        // own `nested::*` marker — is exactly as uncertain one level of
        // qualification up, so it is re-registered under `name` the same way an
        // ordinary qualified alias already is; `resolve_segment_chain`'s own
        // fallback is what a qualified candidate check against this marker.
        for marker in glob_marker_alias(nested_items.iter().copied())
            .into_iter()
            .chain(
                nested_qualified
                    .into_iter()
                    .filter(|alias| is_glob_marker_name(&alias.local)),
            )
        {
            aliases.push(UseAlias {
                local: format!("{name}::{}", marker.local),
                target: Vec::new(),
                absolute: false,
            });
        }
    }
    aliases
}

/// Whether `local` is a [`GLOB_IMPORT_MARKER`] — either the bare marker itself, or one
/// qualified by [`direct_scope_module_aliases`] onto a chain of nested module names
/// (`"traits::*"`, `"traits::nested::*"`).
fn is_glob_marker_name(local: &str) -> bool {
    local == GLOB_IMPORT_MARKER
        || local
            .rsplit_once("::")
            .is_some_and(|(_, last)| last == GLOB_IMPORT_MARKER)
}

/// Strips any number of redundant `(..)` wrappers from a type, so `(Recovery)` and
/// `((Recovery))` read the same as `Recovery`.
///
/// `#[allow(unused_parens)] impl Clone for (Recovery) { .. }` is legal Rust — `syn`
/// parses the parenthesized form as `Type::Paren`, never `Type::Path` — so a scan that
/// only matched `Type::Path` directly would silently skip it (round 14 of Codex review
/// on this change, PR #143).
fn unwrap_type_parens(mut ty: &syn::Type) -> &syn::Type {
    while let syn::Type::Paren(paren) = ty {
        ty = paren.elem.as_ref();
    }
    ty
}

fn collect_trait_implementors<'a>(
    items: impl IntoIterator<Item = &'a syn::Item>,
    aliases: &[UseAlias],
    trait_name: &str,
    shadow_locals: bool,
    implementors: &mut Vec<String>,
) {
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Impl(implementation) => {
                if let Some((_, trait_path, _)) = implementation.trait_.as_ref() {
                    // `every_resolution`, not `resolve_segments`: a handwritten impl's
                    // trait name can be aliased through the same chains and cfg-gated
                    // duplicates a derive path can (round 13's second finding), and an
                    // `UNRESOLVED_DERIVE` here is treated as a match — this scan cannot
                    // rule out that the alias it gave up chasing is the trait being
                    // searched for.
                    let resolved_trait = every_resolution(trait_path, aliases);
                    let names_trait = resolved_trait
                        .iter()
                        .any(|name| name == trait_name || name == UNRESOLVED_DERIVE);
                    if names_trait {
                        // `unwrap_type_parens` first: `impl Clone for (Recovery)` is
                        // legal Rust under `#[allow(unused_parens)]` (round 14's
                        // finding), and `syn` parses the parenthesized form as
                        // `Type::Paren`, not `Type::Path` — a bare `if let` on the
                        // unwrapped variant alone would silently skip it.
                        let self_ty = unwrap_type_parens(implementation.self_ty.as_ref());
                        if let syn::Type::Path(self_type) = self_ty {
                            if self_type.qself.is_some() {
                                // Round 23: `impl Clone for <() as Alias>::Target` is
                                // legal Rust whose self-type is a projected
                                // associated type — `syn` stores the qualified self
                                // and the trait separately from `path`, which here
                                // is only `Target`, so resolving `path` alone
                                // through this module's alias table can miss that
                                // rustc would normalize the projection to
                                // `Recovery`. This module does not resolve trait
                                // bindings, so it fails closed the same way a
                                // `super`-qualified path already does rather than
                                // silently comparing the unqualified associated
                                // type name.
                                implementors.push(UNRESOLVED_DERIVE.to_owned());
                            } else {
                                // `every_resolution` again: the self-type can be a
                                // local type alias (round 13's first finding), and
                                // an `UNRESOLVED_DERIVE` here is pushed through
                                // unchanged so the caller can fail closed on it the
                                // same way `struct_derives`'s caller already does.
                                implementors.extend(every_resolution(&self_type.path, aliases));
                            }
                        } else if matches!(self_ty, syn::Type::Macro(_)) {
                            // Round 16: `impl Clone for identity_ty!(super::Recovery)`
                            // is legal Rust whose self-type is a macro invocation this
                            // module cannot expand — `declares_item_macro` fails the
                            // whole file closed on a type-position macro too (see its
                            // own `visit_type_macro`), and this is the second half of
                            // the same finding: even if that check were bypassed, a
                            // self-type this scan cannot resolve must not be silently
                            // dropped from `implementors`.
                            implementors.push(UNRESOLVED_DERIVE.to_owned());
                        }
                    }
                }
                // The `impl` block's own methods, associated consts and associated
                // types can themselves bury a further `impl` (round 15's, round 17's
                // and round 23's findings) — an `impl` block is not only a place a
                // trait implementation is checked, it is also a place a body, an
                // initializer or a type starts. `implementation.attrs` is not
                // re-checked: the loop's own top-of-body `has_cfg_test` already
                // excluded this arm entirely when the `impl` itself is `#[cfg(test)]`.
                collect_trait_implementors_in_item_body(
                    item,
                    aliases,
                    trait_name,
                    shadow_locals,
                    implementors,
                );
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    // Round 20: a `mod { .. }` block is a real scope boundary in Rust,
                    // so the caller's `aliases` must not apply inside it, and this
                    // module's own aliases must not leak back out to the caller either
                    // — each nested module gets a table built fresh from its own scope
                    // alone, the same way `trait_implementors` builds the file-root one.
                    let mut module_aliases = module_scope_aliases(nested);
                    if shadow_locals {
                        // Round 29: a nested inline module's own struct, enum or union
                        // shadows its name for any unqualified reference inside this
                        // same module, exactly as a `use` or `type` alias redeclared
                        // here already shadows an ambient one (round 24) — see
                        // `shadow_aliases_for_local_types` and
                        // `trait_implementors_for_pinned_type`.
                        module_aliases.extend(shadow_aliases_for_local_types(nested.iter()));
                        // Round 30: a glob import inside this module is exactly as
                        // opaque to this scan as one at file scope — see
                        // `GLOB_IMPORT_MARKER`.
                        module_aliases.extend(glob_marker_alias(nested.iter()));
                    }
                    collect_trait_implementors(
                        nested,
                        &module_aliases,
                        trait_name,
                        shadow_locals,
                        implementors,
                    );
                }
            }
            // Round 15 of Codex review on this change (PR #143) found an `impl`
            // declared as a local item inside a function body —
            // `#[allow(non_local_definitions)] fn install() { impl Clone for
            // super::Recovery { .. } }` — which Rust's own `non_local_definitions`
            // lint documents as never actually scoped to the function, however it
            // looks written down; round 17 found the same non-local shape reachable
            // through a `const`/`static` initializer's own block too, and through a
            // trait's own default method bodies and default associated consts; round
            // 18 found it reachable through an enum variant's discriminant and
            // through a block buried inside a type alias's own type (an array length
            // or a const generic argument); round 19 found the same type-bearing shape
            // reachable through a struct's own field types; round 22 through a
            // union's own field types too. `item.attrs` is not re-checked, for the
            // reason given above.
            syn::Item::Fn(_)
            | syn::Item::Const(_)
            | syn::Item::Static(_)
            | syn::Item::Trait(_)
            | syn::Item::Enum(_)
            | syn::Item::Type(_)
            | syn::Item::Struct(_)
            | syn::Item::Union(_)
            | syn::Item::ForeignMod(_) => {
                collect_trait_implementors_in_item_body(
                    item,
                    aliases,
                    trait_name,
                    shadow_locals,
                    implementors,
                );
            }
            _ => {}
        }
    }
}

/// Every scope-root block [`direct_blocks_in_signature`], [`direct_blocks_in_expr`],
/// [`direct_blocks_in_type`] or [`direct_blocks_in_generics`] can find nested inside
/// `item`'s own body, signature, initializer, member types, or generics, walked with
/// [`collect_trait_implementors_in_block`] under `aliases` as the ambient scope — the
/// block-aware replacement for what used to be a single flat item list shared by the
/// whole item, retired in round 23 (see [`collect_trait_implementors_in_block`] for
/// why).
///
/// Covers every shape [`collect_trait_implementors`] recurses into for a body: a free
/// function's signature and block; an `impl` block's own generics, its methods
/// (signature and body), associated consts' initializers, and associated types' own
/// types; a trait's own generics, its default method bodies (and every method's
/// signature, default or not), and default associated consts; a free `const` or
/// `static`'s initializer; an enum's own generics and its variants' discriminants and
/// fields' types; a type alias's own generics and type; and a struct's or union's own
/// generics and field types.
///
/// Round 24 of Codex review on this change (PR #143) found the generics gap: round 21
/// added [`direct_blocks_in_signature`] for a function's or method's own parameter and
/// return types, and round 22 chained [`direct_blocks_in_generics`] into it for that
/// signature's own type parameters and `where` clause — but `Item::Impl`,
/// `Item::Trait`, `Item::Enum`, `Item::Type`, `Item::Struct` and `Item::Union` each
/// declare their *own* [`syn::Generics`] too (`struct Holder<T = Wrapper<{ impl Clone
/// for super::Recovery { .. }; 0 }>>(T);` is the reported case), and none of those six
/// arms read it at all.
fn collect_trait_implementors_in_item_body<'a>(
    item: &'a syn::Item,
    aliases: &[UseAlias],
    trait_name: &str,
    shadow_locals: bool,
    implementors: &mut Vec<String>,
) {
    let mut roots: Vec<&'a syn::Block> = Vec::new();
    match item {
        syn::Item::Fn(function) => {
            roots.extend(direct_blocks_in_signature(&function.sig));
            roots.push(&function.block);
        }
        syn::Item::Impl(implementation) => {
            roots.extend(direct_blocks_in_generics(&implementation.generics));
            // Round 25: the impl header's own trait path and self type can each bury a
            // block through a const generic argument, exactly the way the impl's own
            // generic declarations already could — `impl Marker<{ impl Clone for
            // super::Recovery { .. }; 0 }> for Holder {}` reached neither before.
            if let Some((_, trait_path, _)) = implementation.trait_.as_ref() {
                roots.extend(direct_blocks_in_path(trait_path));
            }
            roots.extend(direct_blocks_in_type(&implementation.self_ty));
            roots.extend(impl_member_scope_roots(implementation));
        }
        syn::Item::Trait(trait_item) => {
            roots.extend(direct_blocks_in_generics(&trait_item.generics));
            // Round 26: a trait's own supertrait bounds (`trait Outer: Marker<{ .. }>
            // {}`) can bury a block through a const generic argument exactly the way
            // its generics' own bounds already could, and this arm never read
            // `trait_item.supertraits` at all.
            roots.extend(direct_blocks_in_bounds(&trait_item.supertraits));
            roots.extend(trait_member_scope_roots(trait_item));
        }
        syn::Item::Const(constant) => {
            roots.extend(direct_blocks_in_type(&constant.ty));
            roots.extend(direct_blocks_in_expr(&constant.expr));
        }
        syn::Item::Static(statik) => {
            roots.extend(direct_blocks_in_type(&statik.ty));
            roots.extend(direct_blocks_in_expr(&statik.expr));
        }
        syn::Item::Enum(enum_item) => {
            roots.extend(direct_blocks_in_generics(&enum_item.generics));
            for variant in enum_item
                .variants
                .iter()
                .filter(|v| !has_cfg_test(&v.attrs))
            {
                if let Some((_, expr)) = variant.discriminant.as_ref() {
                    roots.extend(direct_blocks_in_expr(expr));
                }
                for field in variant.fields.iter().filter(|f| !has_cfg_test(&f.attrs)) {
                    roots.extend(direct_blocks_in_type(&field.ty));
                }
            }
        }
        syn::Item::Type(type_item) => {
            roots.extend(direct_blocks_in_generics(&type_item.generics));
            roots.extend(direct_blocks_in_type(&type_item.ty));
        }
        syn::Item::Struct(struct_item) => {
            roots.extend(direct_blocks_in_generics(&struct_item.generics));
            for field in struct_item
                .fields
                .iter()
                .filter(|f| !has_cfg_test(&f.attrs))
            {
                roots.extend(direct_blocks_in_type(&field.ty));
            }
        }
        syn::Item::Union(union_item) => {
            roots.extend(direct_blocks_in_generics(&union_item.generics));
            for field in union_item
                .fields
                .named
                .iter()
                .filter(|f| !has_cfg_test(&f.attrs))
            {
                roots.extend(direct_blocks_in_type(&field.ty));
            }
        }
        // Round 26: `extern "C" { fn hidden(_: [(); { impl Clone for super::Recovery
        // { .. }; 0 }]); }` is legal Rust, and a foreign function's own signature or a
        // foreign static's own declared type can bury a block exactly the way an
        // ordinary one's already could — this scan never read `Item::ForeignMod` at
        // all, so its members reached the fallback arm below.
        syn::Item::ForeignMod(foreign_mod) => {
            for foreign_item in &foreign_mod.items {
                match foreign_item {
                    syn::ForeignItem::Fn(function) if !has_cfg_test(&function.attrs) => {
                        roots.extend(direct_blocks_in_signature(&function.sig));
                    }
                    syn::ForeignItem::Static(statik) if !has_cfg_test(&statik.attrs) => {
                        roots.extend(direct_blocks_in_type(&statik.ty));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    for root in roots {
        collect_trait_implementors_in_block(root, aliases, trait_name, shadow_locals, implementors);
    }
}

/// [`collect_trait_implementors_in_item_body`]'s `Item::Impl` arm, over one `impl`
/// block's own members — split out to keep that function under this file's own
/// line-count lint once round 25's impl-header roots joined it.
fn impl_member_scope_roots(implementation: &syn::ItemImpl) -> Vec<&syn::Block> {
    let mut roots = Vec::new();
    for member in &implementation.items {
        match member {
            syn::ImplItem::Fn(method) if !has_cfg_test(&method.attrs) => {
                roots.extend(direct_blocks_in_signature(&method.sig));
                roots.push(&method.block);
            }
            // Round 25: an associated const's own *declared type* can bury a
            // block exactly the way its initializer already could — `const N:
            // [(); { impl Clone for super::Recovery { .. }; 0 }] = [];` — and
            // this arm read only `constant.expr`, never `constant.ty`.
            syn::ImplItem::Const(constant) if !has_cfg_test(&constant.attrs) => {
                roots.extend(direct_blocks_in_type(&constant.ty));
                roots.extend(direct_blocks_in_expr(&constant.expr));
            }
            // Round 23: an associated type's own type can bury a block the
            // same way a type alias's or a struct field's can — `impl T for X
            // { type A = [(); { impl Clone for Recovery { .. }; 0 }]; }` —
            // and this member was dropped on the floor before. Round 27: a
            // generic associated type's own generics — a type parameter's
            // bounds, exactly the shape round 24 already reads on the impl's
            // own generics — can bury one too: `impl T for X { type A<U:
            // Marker<{ impl Clone for Recovery { .. }; 0 }>> = (); }` reached
            // `assoc_type.ty` and never `assoc_type.generics`.
            syn::ImplItem::Type(assoc_type) if !has_cfg_test(&assoc_type.attrs) => {
                roots.extend(direct_blocks_in_generics(&assoc_type.generics));
                roots.extend(direct_blocks_in_type(&assoc_type.ty));
            }
            _ => {}
        }
    }
    roots
}

/// [`collect_trait_implementors_in_item_body`]'s `Item::Trait` arm, over one trait's own
/// members — split out for the same reason [`impl_member_scope_roots`] is.
fn trait_member_scope_roots(trait_item: &syn::ItemTrait) -> Vec<&syn::Block> {
    let mut roots = Vec::new();
    for member in &trait_item.items {
        match member {
            syn::TraitItem::Fn(method) if !has_cfg_test(&method.attrs) => {
                roots.extend(direct_blocks_in_signature(&method.sig));
                if let Some(default) = method.default.as_ref() {
                    roots.push(default);
                }
            }
            syn::TraitItem::Const(constant) if !has_cfg_test(&constant.attrs) => {
                roots.extend(direct_blocks_in_type(&constant.ty));
                if let Some((_, expr)) = constant.default.as_ref() {
                    roots.extend(direct_blocks_in_expr(expr));
                }
            }
            // Round 26: a trait's own associated type *declaration* — as opposed to
            // an impl's associated type, which round 23 already covers — can be a
            // GAT with its own generics (whose `where` clause bounds can bury a
            // block), its own trait bounds, and a default type, and this arm read
            // none of the three: `trait Outer { type A<T> where T: Marker<{ .. }>; }`
            // reached neither this function nor the module-tree walk's twin.
            syn::TraitItem::Type(assoc_type) if !has_cfg_test(&assoc_type.attrs) => {
                roots.extend(direct_blocks_in_generics(&assoc_type.generics));
                roots.extend(direct_blocks_in_bounds(&assoc_type.bounds));
                if let Some((_, ty)) = assoc_type.default.as_ref() {
                    roots.extend(direct_blocks_in_type(ty));
                }
            }
            _ => {}
        }
    }
    roots
}

/// Walks one [`syn::Block`] the way real Rust scopes it: an item declared directly in
/// `block.stmts` — a `use`, a local `type` alias, a nested `impl`, and so on — is
/// visible throughout this whole block (Rust hoists item declarations, so order does
/// not matter) and in every block nested inside it, but never outside it and never in a
/// sibling block.
///
/// Round 23 of Codex review on this change (PR #143) found that
/// [`collect_trait_implementors`] was getting this wrong at a finer grain than round
/// 20's or round 22's fix reached: both flattened *every* item a body could reach,
/// however deeply nested in control flow, into one item list and computed one shared
/// alias table for the whole thing — so `use self::Harmless as C; fn install() { if
/// false { use core::clone::Clone as C; } impl C for Recovery {} }` resolved the
/// `impl`'s `C` through the inner `use`, even though that `use` is scoped only to the
/// `if false { .. }` block it is declared in and is not visible in the function body
/// around it. This function replaces the flatten-then-extend design with one that walks
/// exactly one block at a time: it takes only the items declared *directly* in this
/// block (not through [`block_items`]'s recursive descent) to build this block's own
/// scope, processes them under it, and then recurses into every block nested directly
/// in one of this block's own statements — found by [`direct_child_blocks_of_block`],
/// which does not cross into a nested item's own body, since
/// [`collect_trait_implementors_in_item_body`] already handles that separately — under
/// this block's scope as the new ambient, one level at a time, so a sibling block's own
/// items are never on the table a block did not declare them in.
fn collect_trait_implementors_in_block(
    block: &syn::Block,
    aliases: &[UseAlias],
    trait_name: &str,
    shadow_locals: bool,
    implementors: &mut Vec<String>,
) {
    let direct_items: Vec<&syn::Item> = block
        .stmts
        .iter()
        .filter_map(|stmt| match stmt {
            syn::Stmt::Item(item) => Some(item),
            _ => None,
        })
        .collect();
    let scoped_aliases = extend_with_local_scope(aliases, &direct_items, shadow_locals);
    collect_trait_implementors(
        direct_items.iter().copied(),
        &scoped_aliases,
        trait_name,
        shadow_locals,
        implementors,
    );
    for nested in direct_child_blocks_of_block(block) {
        collect_trait_implementors_in_block(
            nested,
            &scoped_aliases,
            trait_name,
            shadow_locals,
            implementors,
        );
    }
}

/// `aliases`, extended with the `use` and plain-path type aliases declared directly in
/// `local_items` — the items declared directly in one [`syn::Block`]'s own statements.
///
/// A body is not a scope boundary in Rust, so its own local aliases join the ambient
/// table rather than replacing it (unlike [`module_scope_aliases`]'s `Item::Mod` case).
/// But joining is not the same as appending: real Rust *shadows* an outer binding with
/// an inner one of the same name, for the rest of the inner scope, rather than keeping
/// both reachable under it. Round 24 of Codex review on this change (PR #143) found
/// that this used to append unconditionally — module scope importing
/// `use core::clone::Clone as C;` beside a function body that imports a harmless local
/// trait as `use self::Harmless as C;` before `impl C for Recovery {}` resolves the
/// `impl` to the block-local `Harmless` in real Rust, but appending kept the ambient
/// `Clone` binding reachable too, so `every_resolution`'s search still found it and
/// rejected a `Recovery` that never implements `Clone`. Every ambient alias whose local
/// name is *unconditionally* redeclared here is dropped before the new ones are added,
/// so a shadowed name resolves only through its innermost declaration, the way every
/// other alias lookup in this module already treats "the nearest binding wins" for a
/// *single* hop — this is that same rule applied to which binding is on the table at
/// all.
///
/// "Unconditionally" is round 25's own correction: round 24's fix dropped an ambient
/// alias whenever *any* local declaration of the same name existed, including one
/// behind a `#[cfg(any())]` that can never actually compile — module scope importing
/// `use core::clone::Clone as C;`, beside a function body's `#[cfg(any())] use
/// self::Harmless as C; impl C for Recovery { .. }`, resolves the `impl` through the
/// *ambient* `Clone` in the only configuration that ever ships, because the `cfg`-gated
/// local declaration never exists in it. This module does not evaluate a `cfg`'s
/// condition (see the module doc's residual limits), so it cannot tell that `any()`
/// never holds; the correct, fail-closed answer is to treat the shadowing itself as
/// conditional and keep the ambient alias reachable alongside the local one, the same
/// way [`struct_derives`] reads past an unevaluated `cfg` rather than trusting either
/// branch alone. Only a local declaration with no `#[cfg(..)]` at all — checked with
/// [`has_any_cfg`], not [`has_cfg_test`], since any condition leaves the ambient
/// binding possibly still live — shadows the ambient alias it redeclares.
///
/// Found by Codex review of this change (PR #143), round 30: a function is just as free
/// to declare its own local `struct Recovery` as an inline module is (round 29's
/// finding) — `fn install() { struct Recovery; impl Clone for Recovery { .. } }` is
/// legal Rust whose unqualified `Recovery` means the block-local declaration — but this
/// function only ever extended the table with [`direct_scope_aliases`], which reads
/// `use` and `type` items alone, so a block-local struct, enum or union never earned the
/// [`LOCAL_SHADOWED_TYPE`] marker [`shadow_aliases_for_local_types`] registers for the
/// identical shape at module scope. `shadow_locals` gates it the same way it gates every
/// other caller of this function: `trait_implementors`'s unrelated
/// `future_trait_implementors` scan never asks for it.
fn extend_with_local_scope(
    ambient: &[UseAlias],
    local_items: &[&syn::Item],
    shadow_locals: bool,
) -> Vec<UseAlias> {
    let mut local = direct_scope_aliases(local_items.iter().copied());
    let mut unconditionally_shadowed: Vec<String> = local_items
        .iter()
        .filter(|item| !has_cfg_test(item_attrs(item)) && !has_any_cfg(item_attrs(item)))
        .flat_map(|item| direct_scope_aliases(std::iter::once(*item)))
        .map(|alias| alias.local)
        .collect();
    if shadow_locals {
        let unconditional_shadows: Vec<UseAlias> = local_items
            .iter()
            .filter(|item| !has_cfg_test(item_attrs(item)) && !has_any_cfg(item_attrs(item)))
            .flat_map(|item| shadow_aliases_for_local_types(std::iter::once(*item)))
            .collect();
        unconditionally_shadowed.extend(
            unconditional_shadows
                .iter()
                .map(|alias| alias.local.clone()),
        );
        local.extend(unconditional_shadows);
        local.extend(glob_marker_alias(local_items.iter().copied()));
    }
    let mut extended: Vec<UseAlias> = ambient
        .iter()
        .filter(|alias| !unconditionally_shadowed.contains(&alias.local))
        .cloned()
        .collect();
    extended.extend(local);
    extended
}

/// The derive traits named on the struct `name` in `contents`, or [`None`] when `contents`
/// declares no struct of that name.
///
/// [`Some`] may still be empty: a declared struct that derives nothing is not the same as
/// no struct at all, and a caller pinning "this struct must not derive `Clone`" needs to
/// tell the two apart the way every surface pin in this file already says "or the module
/// is gone, so the pin checks nothing" rather than reading a rename as a clean pass.
///
/// Two things a naive `#[derive(..)]` scan misses, both closed here. A derive path
/// resolves through the file's `use` aliases and keeps its last segment, so
/// `use core::clone::Clone as Klon; #[derive(Klon)]` reports `Clone`, the same way
/// [`trait_implementors`] resolves the trait name of a handwritten `impl`. And a derive
/// named inside `#[cfg_attr(.., derive(..))]` is read too, however deeply `cfg_attr`
/// nests, and whatever every condition on the way down is: a `Clone` that only applies
/// under one build is still a `Clone` under that build, and reading past a condition
/// rather than evaluating it is what `codec-is-optional` already does for a compound
/// `cfg`, for the same reason.
///
/// The alias an unconditional derive resolves through may itself be behind any `cfg`
/// except `cfg(test)` — a `#[cfg(feature = "x")] use Y as Klon;` still means
/// `#[derive(Klon)]` is `Y` under that feature, so excluding it would be the false
/// negative, the opposite mistake from the one two paragraphs up. Only `cfg(test)` is
/// excluded, because a name defined solely under it could never be reached by a
/// `#[derive(..)]` on a struct that ships.
///
/// Only a struct declared at the top level of `contents` is `name`. A private struct of
/// the same name nested inside a `mod` is a different declaration, not a second sighting
/// of the one the crate's public API exports under that name — reading it as one is
/// exactly how a rename survives this pin: a real `Recovery` renamed to `Scan` and
/// re-exported, sitting beside an unrelated inner `struct Recovery` that derives nothing,
/// would let the decoy answer "declared, not `Clone`" for a struct nobody exported by
/// that name.
///
/// Two more things follow from that same top-level-only reading, both found by review of
/// this change (issue #77, PR #143). Only a top-level declaration with **no** `#[cfg(..)]`
/// at all may answer for `name`: this module does not evaluate a `cfg`'s condition, so a
/// `#[cfg(any())] struct Recovery;` that never compiles would otherwise sit beside a real,
/// `Clone`-deriving type exported under that name and answer "declared, not `Clone`" for
/// it. And the aliases a derive resolves through are collected at the top level only — via
/// `module_scope_aliases`, the same function [`trait_implementors`] builds its own file
/// scope from — rather than by recursing into a nested module: an inner module's `use X as
/// Klon;`, read before a top-level `use core::clone::Clone as Klon;` because it happens to
/// sit earlier in the file, would otherwise resolve `#[derive(Klon)]` to `X` instead — and
/// a nested module cannot shadow a name in the scope the pinned struct is declared in, so
/// reading only the top level is the correct resolution here, not merely a narrower one.
///
/// A returned name can also be [`UNRESOLVED_DERIVE`], found on the same review round as
/// the two paragraphs above: a `super`-qualified derive path, or one buried past the pile
/// this scan's alias resolution will chase, cannot be pinned to a name this scan can
/// compare — and a caller checking for one specific trait has to treat that sentinel as a
/// match, the same way it already treats [`None`] here as "cannot say" rather than as
/// "not `Clone`".
///
/// Found by Codex review of this change (PR #143), round 29: this used to collect its
/// aliases with a hand-rolled loop over `Item::Use` alone, which — unlike
/// `module_scope_aliases` — read neither a plain-path `type` alias
/// (`type Klon = core::clone::Clone; #[derive(Klon)]`) nor an alias exported one level
/// through an inline module's own name (`mod traits { pub use core::clone::Clone as C; }
/// #[derive(traits::C)]`), so a derive resolved through either bypassed the scan entirely.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn struct_derives(contents: &str, name: &str) -> Result<Option<Vec<String>>, syn::Error> {
    let file = parse_rust(contents)?;
    let mut aliases = module_scope_aliases(&file.items);
    aliases.extend(glob_marker_alias(&file.items));
    let mut derives = Vec::new();
    let mut declared = false;
    for item in &file.items {
        if has_any_cfg(item_attrs(item)) {
            continue;
        }
        if let syn::Item::Struct(found) = item {
            // `ident_is`: `pub struct r#Recovery` declares the same item `Recovery` would.
            if ident_is(&found.ident, name) {
                declared = true;
                for attr in &found.attrs {
                    collect_derive_names_from_meta(&attr.meta, &aliases, &mut derives);
                }
            }
        }
    }
    Ok(declared.then_some(derives))
}

/// Whether any struct, enum or union `contents` declares derives something this
/// module cannot rule out as one of Rust's own nine derivable traits.
///
/// Checked at module scope and at any depth of inline-module nesting. Found by
/// Codex review of this change (PR #143), round 40: a procedural derive
/// macro is not obliged to emit an implementation only for the trait its own name
/// suggests, or only for the type it is attached to — `#[derive(Evil)] struct
/// Helper;` anywhere in a production-reachable file can expand to `impl Clone for
/// crate::recovery::Recovery` exactly as freely as a derive placed directly on
/// `Recovery` itself, because a derive macro receives the whole item as input and
/// emits whatever tokens it likes. [`struct_derives`] only ever validates the
/// *pinned* type's own derive list, in the one file it is declared in; this walks
/// every other struct, enum and union the same file declares and asks the same
/// question of each, with the same `push_resolved_names` logic and the same
/// fail-closed answer for a name this scan cannot vouch for.
///
/// Scoped to module-level and inline-module-nested declarations, matching
/// `collect_trait_implementors`'s own module-boundary alias threading — an
/// out-of-line child module is a separate file this function's own caller calls it
/// on again, exactly as `trait_implementors_for_pinned_type` already is. A
/// block-local struct, enum or union (declared inside a function body) is not
/// walked; closing that narrower residual needs the same block-scoped descent
/// `collect_trait_implementors_in_block` carries for a handwritten `impl`, which
/// this module-scoped check does not yet share.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn unresolved_derive_elsewhere(contents: &str) -> Result<bool, syn::Error> {
    let file = parse_rust(contents)?;
    let mut aliases = module_scope_aliases(&file.items);
    aliases.extend(glob_marker_alias(&file.items));
    Ok(any_unresolved_derive_in_scope(&file.items, &aliases))
}

fn any_unresolved_derive_in_scope(items: &[syn::Item], aliases: &[UseAlias]) -> bool {
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Struct(found) => {
                if any_unresolved_derive(&found.attrs, aliases) {
                    return true;
                }
            }
            syn::Item::Enum(found) => {
                if any_unresolved_derive(&found.attrs, aliases) {
                    return true;
                }
            }
            syn::Item::Union(found) => {
                if any_unresolved_derive(&found.attrs, aliases) {
                    return true;
                }
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    // Round 20's own reason: a `mod { .. }` block is a real scope
                    // boundary, so this module's aliases are built fresh from its own
                    // scope rather than inherited from the caller's.
                    let mut module_aliases = module_scope_aliases(nested);
                    module_aliases.extend(glob_marker_alias(nested.iter()));
                    if any_unresolved_derive_in_scope(nested, &module_aliases) {
                        return true;
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn any_unresolved_derive(attrs: &[syn::Attribute], aliases: &[UseAlias]) -> bool {
    let mut derives = Vec::new();
    for attr in attrs {
        collect_derive_names_from_meta(&attr.meta, aliases, &mut derives);
    }
    derives.iter().any(|name| name == UNRESOLVED_DERIVE)
}

/// Whether `contents` invokes a macro anywhere it could expand to an item, at any
/// nesting depth.
///
/// Two shapes count, both because Rust's grammar — not this scan's judgement — is what
/// lets a macro produce a new item there. `Item::Macro` covers a `macro_rules!`
/// definition and an item-position invocation alike, at the top of the file, inside a
/// nested `mod` (however deep), or inside an `impl` or `trait` body. `Stmt::Macro` covers
/// the other position the reference grants item expansion: a bare `path!(..);` standing
/// alone in a function or method body, as its own statement rather than bound to a `let`
/// or read as a value. A path in either position can still name a type outside the file
/// it is written in — `generate_clone_impl!(super::Recovery)` inside `mod hidden { .. }`,
/// or inside a method body, still expands to an `impl Clone for Recovery` at the crate's
/// real type — so both are read at every depth this scan reaches, matching the
/// [`syn::visit::Visit`] traversal [`resolved_path_uses`] already uses for the same
/// reason.
///
/// A macro used anywhere else — an argument, a condition, a `let` binding, a tail
/// expression — parses as `Expr::Macro` instead, and the reference does not let an
/// expression position expand to an item: `assert!(a == b)` and `matches!(x, Some(_))`
/// stay expressions everywhere this crate already uses them, so reading only the two
/// item-granting shapes is precise rather than merely convenient — a version that also
/// flagged `Expr::Macro` would reject `recovery.rs`'s own compile-time assertions.
///
/// A third shape counts too, in a different part of the grammar: `Type::Macro` is a
/// macro invoked where a type is expected, and `impl Clone for identity_ty!(super::
/// Recovery)` is legal Rust whose self-type this scan cannot expand — round 16 of
/// Codex review on this change found it, right after [`trait_implementors`]'s own
/// self-type match, which reads what a macro in that position would otherwise expand
/// to. Flagging it fails the whole file closed the same way an item- or
/// statement-position macro already does, which is simpler than resolving what a type
/// macro expands to and correct for the same reason: this module cannot expand a
/// macro at all.
///
/// This module cannot expand a macro (see the module doc's residual limits), so any of
/// the three shapes could expand to anything — a `#[derive(Clone)]`, a handwritten `impl
/// Clone`, or nothing at all — and neither [`struct_derives`] nor [`trait_implementors`]
/// can tell which. Found by Codex review of this change (PR #143): round 9 added
/// `Item::Macro` at the top level, round 10 corrected it to recurse into nested `mod`s
/// the way a declaration cannot, round 11 added `Stmt::Macro`, the position a
/// hand-rolled recursion over `syn::Item` alone cannot reach at all — a visitor is what
/// closes it rather than a fourth case bolted onto the same recursion — and round 16
/// added `Type::Macro`. This generalizes the ban `names_identifier(&code,
/// "macro_rules")` already places on a **declared** macro elsewhere in this file to any
/// invocation, because the macro doing the expanding does not have to be declared in the
/// file it expands into.
///
/// A fourth shape is not an invocation at all, syntactically, and round 31 found it: an
/// **attribute** macro. `#[a_transform] struct Anything;` compiles today with nothing
/// here able to say what it expands to — unlike a derive, an attribute macro may rewrite
/// or replace the very item it decorates, or splice an unrelated item in beside it, so
/// `#[a_transform] struct Anything;` could just as well expand to `struct Anything; impl
/// Clone for Recovery { .. }`. `visit_attribute` reaches every attribute the visitor's
/// existing traversal reaches — file level, item level, and every member, field, variant
/// and foreign item beneath a level `has_cfg_test` has not already excluded — and
/// `meta_is_unresolved_attribute_macro` is what tells a builtin the compiler interprets
/// itself, and a namespace rustc treats as opaque to a named tool, from everything else.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn declares_item_macro(contents: &str) -> Result<bool, syn::Error> {
    struct MacroVisitor {
        found: bool,
        shadowed_expression_macro_names: std::collections::HashSet<String>,
    }

    impl<'ast> syn::visit::Visit<'ast> for MacroVisitor {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            // Test code is not shipped: `#[cfg(test)]` removes the item, and anything
            // inside it, before any expansion that could reach the real `Recovery` runs.
            if has_cfg_test(item_attrs(item)) {
                return;
            }
            syn::visit::visit_item(self, item);
        }

        // Round 27: an item's own `#[cfg(test)]` was the only gate this visitor read,
        // but a *member* of an otherwise-production `impl` or `trait` can carry its own
        // `#[cfg(test)]` too — `#[cfg(test)] fn helper() { generate_clone!(); }` inside
        // a production `impl` does not exist in a shipped build, and the default
        // traversal `visit_item`'s override does not reach past the enclosing `impl`
        // or `trait` to see it, so a macro inside such a member reached
        // `visit_item_macro`/`visit_stmt_macro` regardless and rejected valid
        // production code.
        fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
            if has_cfg_test(impl_item_attrs(item)) {
                return;
            }
            syn::visit::visit_impl_item(self, item);
        }

        fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
            if has_cfg_test(trait_item_attrs(item)) {
                return;
            }
            syn::visit::visit_trait_item(self, item);
        }

        // Round 28: the same gap as round 27's, one level of subitem further —
        // `#[cfg(test)] field: imported_type_macro!()` on a production struct's or
        // union's field, a `#[cfg(test)]`-gated enum variant, and a `#[cfg(test)]`
        // foreign item each carry their own gate that the default traversal walks
        // straight past, since `syn::visit::Visit` dispatches each through its own
        // method (`visit_field`, `visit_variant`, `visit_foreign_item`) rather than
        // back through `visit_item` or `visit_impl_item`/`visit_trait_item`.
        fn visit_field(&mut self, field: &'ast syn::Field) {
            if has_cfg_test(&field.attrs) {
                return;
            }
            syn::visit::visit_field(self, field);
        }

        fn visit_variant(&mut self, variant: &'ast syn::Variant) {
            if has_cfg_test(&variant.attrs) {
                return;
            }
            syn::visit::visit_variant(self, variant);
        }

        fn visit_foreign_item(&mut self, item: &'ast syn::ForeignItem) {
            if has_cfg_test(foreign_item_attrs(item)) {
                return;
            }
            syn::visit::visit_foreign_item(self, item);
        }

        fn visit_item_macro(&mut self, _node: &'ast syn::ItemMacro) {
            self.found = true;
        }

        // Round 32: a statement-position macro invocation carries its own attributes
        // the same way an item does, and `#[cfg(test)] generate_clone!();` inside an
        // otherwise-production function body does not exist in a shipped build — but
        // this override read only the fact that a `StmtMacro` node was reached, never
        // its own `attrs`, so a test-only macro statement failed the whole file closed
        // over code that ships with nothing generated at all.
        fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
            if has_cfg_test(&node.attrs) {
                return;
            }
            self.found = true;
        }

        fn visit_type_macro(&mut self, _node: &'ast syn::TypeMacro) {
            self.found = true;
        }

        // Round 33: `const _: () = make_clone!();` is legal Rust whose macro sits in
        // *expression* position, and this module's own doc reasoned that shape needs
        // no case here because "the reference does not let an expression position
        // expand to an item" — true of substituting the invocation itself for an item,
        // but not of what the invocation can expand *into*: a block is a legal
        // expression, and Rust's block grammar admits item statements inside one
        // (`non_local_definitions`, the same construct round 15 already found reaching
        // an `impl` through a function body), so an arbitrary macro can expand to `{
        // impl Clone for Recovery { .. }; }` and still type as `()`. `visit_expr_macro`
        // is the last of the four positions a macro invocation can occupy — item,
        // statement, type and now expression — that this module cannot expand, but
        // unlike the other three this position is where `waymaker-flash` itself
        // already calls `assert!`, `matches!`, `panic!`, `unreachable!` and `write!`
        // throughout its production code, so flagging every expression-position
        // invocation would reject the very file this rule exists to protect. Every one
        // of those is a compiler-builtin or standard-library macro with a fixed,
        // fully-specified expansion that never emits a freestanding item — a promise a
        // third-party or local `macro_rules!` invocation cannot make — so only a
        // macro that is not one of them fails closed.
        //
        // Round 35: naming a safe macro is not the same as vouching for its
        // *arguments* — `syn` never parses a macro invocation's own tokens into
        // structured syntax, they are an opaque `TokenStream`, and `#[allow(
        // non_local_definitions)] const _: () = assert!({ impl Clone for
        // super::Recovery { .. } true });` puts a real, globally-applying `impl`
        // inside `assert!`'s own condition — a block is still a block whichever
        // macro's arguments it sits inside, and this visitor never reaches inside one
        // to see it. Reparsing every whitelisted macro's own grammar is not this
        // module's to do — `assert!`, `matches!` and `write!` each take a different
        // shape of arguments, one of them a pattern rather than an expression at all
        // — so instead of trying, [`token_stream_hides_a_possible_item`] refuses the
        // one thing every one of them shares: only a brace-delimited group can open a
        // block, and only a block can carry an item statement, so a whitelisted
        // macro's own tokens are trusted only when they carry no brace group at all,
        // at any depth.
        //
        // Round 40: a brace is not the only way one of these tokens can reach an item
        // this scan cannot see. `#[allow(non_local_definitions)] const _: () =
        // assert!(evil!());`, where `evil!` is an ordinary (non-whitelisted) macro
        // that expands to `{ impl Clone for super::Recovery { .. } true }`, carries no
        // brace anywhere in `assert!`'s own tokens — only `evil`, `!` and an empty
        // `(..)` group, since `evil!()`'s own expansion is exactly as opaque to `syn`
        // as any other macro's, and this scan never expands it to see the brace one
        // level down. `token_stream_hides_a_possible_item` now also refuses any
        // further macro invocation nested in a whitelisted macro's own tokens, at any
        // depth: an identifier immediately followed by `!` and a delimited group is
        // one, whichever delimiter it uses, and a nested invocation this scan does not
        // itself recognize as safe is exactly as untrustworthy as a bare brace.
        // Round 36: a macro used as a *tail* expression still carries its own
        // attributes on this node — `fn helper() { #[cfg(test)] make_clone!() }` is
        // legal Rust whose macro is removed from every non-test build exactly like a
        // gated statement already is — but this override read only the macro's path,
        // never `node.attrs`, so a test-gated expression-position invocation failed
        // the whole file closed over a macro that never ships. `visit_stmt_macro`
        // already gained this same check for the statement-position shape.
        fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
            if has_cfg_test(&node.attrs) {
                return;
            }
            // Round 36: a name on the whitelist is only trusted when nothing in this
            // file has rebound it — see `shadowed_expression_macro_names`.
            let shadowed = node.mac.path.get_ident().is_some_and(|ident| {
                self.shadowed_expression_macro_names
                    .contains(&ident_name(ident))
            });
            if shadowed
                || !is_known_safe_expression_macro(&node.mac.path)
                || token_stream_hides_a_possible_item(node.mac.tokens.clone())
            {
                self.found = true;
            }
        }

        // Round 31: every attribute the traversal above still reaches — because
        // nothing upstream of it has already excluded the item, member, field,
        // variant or foreign item it sits on — might be an attribute macro this
        // module cannot expand, and `visit_attribute` is where `syn`'s own generated
        // traversal calls back for every one of them, at any nesting depth, without
        // a case added here for each new place an attribute can appear.
        fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
            if meta_is_unresolved_attribute_macro(&attr.meta) {
                self.found = true;
            }
            syn::visit::visit_attribute(self, attr);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = MacroVisitor {
        found: false,
        shadowed_expression_macro_names: shadowed_expression_macro_names(&file),
    };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// Whether `meta` names an attribute [`declares_item_macro`] cannot rule out as a macro
/// invocation: not a builtin the compiler interprets itself, not an attribute in a
/// namespace rustc treats as opaque to any tool but the one it names, and not a
/// `#[cfg_attr(.., ..)]` whose own arguments — read at any depth, for
/// [`collect_derive_names_from_meta`]'s reason — are every one of those two.
///
/// A `cfg_attr` this module cannot parse into a condition and its arguments answers
/// `true`, matching [`attr_introduces_cfg`]'s own rule for the identical shape of
/// unreadable content: a `cfg_attr` this scan cannot read is not evidence that everything
/// it names is safe.
fn meta_is_unresolved_attribute_macro(meta: &syn::Meta) -> bool {
    if path_is_ident(meta.path(), "cfg_attr") {
        let syn::Meta::List(list) = meta else {
            return true;
        };
        let Ok(metas) = list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        ) else {
            return true;
        };
        return metas.iter().skip(1).any(meta_is_unresolved_attribute_macro);
    }
    !is_known_safe_attribute_path(meta.path())
}

/// Whether `path` is a builtin attribute the compiler interprets itself, or names a
/// namespace rustc treats as opaque to a tool of its own rather than expanding — the two
/// shapes an attribute can take without being a macro invocation this module cannot see
/// through. `path.segments` is empty for no attribute `syn` can produce, so an empty path
/// answers `false` rather than assuming safety of something that cannot occur.
///
/// The builtin list is every attribute the reference gives the compiler itself, so a
/// crate that starts using one it does not use today does not newly fail this rule; the
/// tool list is `rustfmt`, `clippy`, `rust_analyzer` and `miri` — the tool namespaces the
/// reference names — plus `diagnostic`, which the reference documents as a namespace
/// reserved for the compiler's own diagnostics rather than for any external tool, but
/// which behaves the same way here: neither expands to new code.
fn is_known_safe_attribute_path(path: &syn::Path) -> bool {
    const KNOWN_TOOLS: &[&str] = &["clippy", "diagnostic", "miri", "rust_analyzer", "rustfmt"];
    const BUILTIN: &[&str] = &[
        "allow",
        "automatically_derived",
        "bench",
        "cfg",
        "cfg_attr",
        "cold",
        "crate_name",
        "crate_type",
        "debugger_visualizer",
        "deny",
        "deprecated",
        "derive",
        "doc",
        "expect",
        "export_name",
        "feature",
        "forbid",
        "global_allocator",
        "ignore",
        "inline",
        "instruction_set",
        "link",
        "link_name",
        "link_ordinal",
        "link_section",
        "macro_export",
        "macro_use",
        "must_use",
        "naked",
        "no_builtins",
        "no_implicit_prelude",
        "no_link",
        "no_main",
        "no_mangle",
        "no_std",
        "non_exhaustive",
        "panic_handler",
        "path",
        "proc_macro",
        "proc_macro_attribute",
        "proc_macro_derive",
        "recursion_limit",
        "repr",
        "should_panic",
        "start",
        "target_feature",
        "test",
        "track_caller",
        "type_length_limit",
        "used",
        "warn",
        "windows_subsystem",
    ];
    let Some(first) = path.segments.first() else {
        return false;
    };
    let first_name = ident_name(&first.ident);
    if path.segments.len() == 1 {
        return BUILTIN.contains(&first_name.as_str());
    }
    KNOWN_TOOLS.contains(&first_name.as_str())
}

/// Whether `path` names one of the compiler-builtin or standard-library macros usable
/// in expression position whose expansion is fixed and fully specified by the
/// reference — never a third-party or local `macro_rules!` invocation, which this
/// module cannot expand and which round 33 found could expand to a block containing a
/// freestanding item.
///
/// Matched as a single, unqualified identifier rather than resolved through an alias
/// table: every name here is a language or standard-library macro a workspace has no
/// ordinary reason to re-export under another name, and a path of more than one
/// segment (`core::assert!` written out, say) is not how any of them are invoked in
/// practice — read conservatively, so a genuinely unusual spelling fails closed rather
/// than being resolved away.
///
/// Being one of these names is necessary but not sufficient: [`declares_item_macro`]'s
/// own caller also asks whether the name is [`shadowed_expression_macro_names`] before
/// trusting it, because a *local* `use` can rebind any of them — see that function's own
/// documentation for the round 36 finding this split exists to close.
///
/// `include` is deliberately absent, unlike its two siblings. Codex review of this
/// change (PR #143), round 39: `include_str!` and `include_bytes!` can only ever
/// produce a string or byte-string literal — their result is never parsed as Rust at
/// all — but `include!` splices the *named file's own tokens* in as Rust source, and
/// [`token_stream_hides_a_possible_item`]'s scan reads only the invocation's own
/// arguments, which for `include!("clone.inc")` is a single string literal with no
/// brace or nested macro invocation in it anywhere. The file `"clone.inc"` names is
/// never opened by this per-file scan — the same blind spot an out-of-line `mod name;`
/// has — so
/// `const _: () = include!("clone.inc");` in a production-reachable file, where
/// `clone.inc` holds `{ impl Clone for Recovery { .. }; 0 }`, walked past this check
/// unseen. No source in this workspace calls bare `include!` in expression position, so
/// removing it costs no accepted file anything; it now falls to the same
/// cannot-rule-out refusal every other unrecognized macro invocation already gets.
const KNOWN_SAFE_EXPRESSION_MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "cfg",
    "column",
    "compile_error",
    "concat",
    "dbg",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "env",
    "eprint",
    "eprintln",
    "file",
    "format",
    "format_args",
    "include_bytes",
    "include_str",
    "line",
    "matches",
    "module_path",
    "option_env",
    "panic",
    "print",
    "println",
    "stringify",
    "todo",
    "unimplemented",
    "unreachable",
    "vec",
    "write",
    "writeln",
];

fn is_known_safe_expression_macro(path: &syn::Path) -> bool {
    path.get_ident()
        .is_some_and(|ident| KNOWN_SAFE_EXPRESSION_MACROS.contains(&ident_name(ident).as_str()))
}

/// Every local name a `use` item anywhere in `file` binds to one of
/// [`KNOWN_SAFE_EXPRESSION_MACROS`], at any nesting depth — file scope, a nested module,
/// or a function body, since `use` is legal in all three.
///
/// Found by Codex review of this change (PR #143), round 36: `use crate::make_clone as
/// assert; const _: () = assert!();` is legal Rust whose `assert!` invocation is not
/// `core::assert!` at all — a local `use` rebinds the name in the macro namespace the
/// same way it would in the value or type namespace, and [`is_known_safe_expression_
/// macro`] matched the spelled name alone, so a locally-imported macro wearing a
/// whitelisted name walked past the one check built to stop an unexpandable macro.
///
/// This does not resolve *which* invocation a given `use` shadows — that needs the same
/// scope-stack machinery [`resolve_segments`] and [`every_resolution`] each carry for
/// their own callers, which this visitor does not have — so a name found anywhere in the
/// file is poisoned for the whole file, not only the scope the shadowing `use` sits in.
/// Coarser than precise shadowing would be, and safe in the direction that matters: a
/// whitelisted name that is never rebound anywhere keeps trusting the real builtin
/// exactly as before, and a name rebound *anywhere* stops being trusted *everywhere*,
/// which can only reject more than a precise version would, never less.
fn shadowed_expression_macro_names(file: &syn::File) -> std::collections::HashSet<String> {
    struct ShadowVisitor {
        shadowed: std::collections::HashSet<String>,
    }

    impl<'ast> syn::visit::Visit<'ast> for ShadowVisitor {
        fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
            let mut aliases = Vec::new();
            collect_tree_aliases(
                &node.tree,
                node.leading_colon.is_some(),
                &mut Vec::new(),
                &mut aliases,
            );
            for alias in aliases {
                if KNOWN_SAFE_EXPRESSION_MACROS.contains(&alias.local.as_str()) {
                    self.shadowed.insert(alias.local);
                }
            }
            syn::visit::visit_item_use(self, node);
        }
    }

    let mut visitor = ShadowVisitor {
        shadowed: std::collections::HashSet::new(),
    };
    visitor.visit_file(file);
    visitor.shadowed
}

/// Whether `tokens` contains a brace-delimited group, or what looks like a nested
/// macro invocation, anywhere at any depth.
///
/// A macro invocation's own arguments are an opaque [`proc_macro2::TokenStream`] to
/// `syn`'s visitor — it is never parsed into structured syntax the way an item, a
/// statement or an ordinary expression is — so this is what this module can still
/// check about a whitelisted expression macro's own tokens without reparsing each
/// macro's own grammar: only a `{ .. }` group can open a block, and only a block can
/// carry an item statement, so tokens with no brace group anywhere cannot hide one
/// directly (round 35 of Codex review on this change, PR #143).
///
/// Round 40 found the same hole one level of expansion down: an identifier
/// immediately followed by `!` and a delimited group — `evil!()`, `evil!{}` or
/// `evil![]`, whichever delimiter it uses — is a nested macro invocation, and its own
/// expansion is exactly as opaque to `syn` as the outer, whitelisted macro's is.
/// `assert!(evil!())` carries no brace anywhere in `assert!`'s own tokens at all, only
/// in whatever `evil!`'s unexpandable expansion would occupy, so the brace scan alone
/// passed a real `impl Clone for Recovery` hidden one macro deeper. A path of more
/// than one segment (`crate::evil!()`) is still caught: the check only needs the last
/// identifier before the `!`, whatever qualifies it.
fn token_stream_hides_a_possible_item(tokens: proc_macro2::TokenStream) -> bool {
    let trees: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    trees.iter().enumerate().any(|(index, tree)| match tree {
        proc_macro2::TokenTree::Group(group) => {
            group.delimiter() == proc_macro2::Delimiter::Brace
                || token_stream_hides_a_possible_item(group.stream())
        }
        proc_macro2::TokenTree::Ident(_) => matches!(
            (trees.get(index + 1), trees.get(index + 2)),
            (
                Some(proc_macro2::TokenTree::Punct(bang)),
                Some(proc_macro2::TokenTree::Group(_)),
            ) if bang.as_char() == '!',
        ),
        proc_macro2::TokenTree::Punct(_) | proc_macro2::TokenTree::Literal(_) => false,
    })
}

/// Whether `attrs` carries an `#[cfg(..)]` at all, whatever its condition, including one
/// reached only by expanding a `#[cfg_attr(.., cfg(..))]` however many levels deep.
///
/// Broader than [`has_cfg_test`]: this module does not evaluate a `cfg`'s condition (see
/// the module doc's residual limits), so an item behind *any* `cfg` — not only
/// `cfg(test)` — might not be the declaration that ships, in either direction. A pin that
/// has to answer for one specific, unconditional type needs an unconditional declaration
/// to point at; reading past an unevaluated condition either way is the shape of mistake
/// [`struct_derives`] exists to catch, not something it can also fall into. The recursion
/// mirrors [`collect_derive_names_from_meta`]'s: `#[cfg_attr(all(), cfg(any()))]` is valid
/// Rust that removes the item exactly as a bare `#[cfg(any())]` would, and a check that
/// only read the outer attribute's own path would call it unconditional.
fn has_any_cfg(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(attr_introduces_cfg)
}

/// Whether `attr` is a `#[cfg(..)]`, or a `#[cfg_attr(.., ..)]` that expands to one at any
/// depth. Unreadable `cfg_attr` arguments answer `true`: an attribute this scan cannot
/// read is not evidence of an unconditional declaration.
fn attr_introduces_cfg(attr: &syn::Attribute) -> bool {
    if path_is_ident(attr.path(), "cfg") {
        return true;
    }
    if !path_is_ident(attr.path(), "cfg_attr") {
        return false;
    }
    let Ok(metas) = attr.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return true;
    };
    metas.iter().skip(1).any(meta_introduces_cfg)
}

/// [`attr_introduces_cfg`], over one argument of a `cfg_attr` rather than over a whole
/// attribute.
fn meta_introduces_cfg(meta: &syn::Meta) -> bool {
    let syn::Meta::List(list) = meta else {
        return false;
    };
    if path_is_ident(&list.path, "cfg") {
        return true;
    }
    if !path_is_ident(&list.path, "cfg_attr") {
        return false;
    }
    let Ok(nested) = list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return true;
    };
    nested.iter().skip(1).any(meta_introduces_cfg)
}

/// The trait names `meta` derives, resolved through `aliases`: from a plain
/// `derive(..)`, or from every `derive(..)` reachable by expanding `cfg_attr(.., ..)`
/// however many levels deep — `cfg_attr(a, cfg_attr(b, derive(Clone)))` is valid Rust,
/// and rustc derives `Clone` from it exactly as it would from a bare `#[derive(Clone)]`,
/// so a scan that only looked one level in would miss it.
fn collect_derive_names_from_meta(
    meta: &syn::Meta,
    aliases: &[UseAlias],
    derives: &mut Vec<String>,
) {
    let syn::Meta::List(list) = meta else {
        return;
    };
    if path_is_ident(&list.path, "derive") {
        if let Ok(paths) = list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        ) {
            push_resolved_names(&paths, aliases, derives);
        }
        return;
    }
    if !path_is_ident(&list.path, "cfg_attr") {
        return;
    }
    // `cfg_attr(condition, attr, attr, ..)`: the first argument is the condition and
    // every argument after it applies when the condition holds. Every one is read
    // regardless of what the condition is, for the reason the doc comment above gives —
    // including one that is itself a `cfg_attr`, which is why this recurses.
    let Ok(metas) = list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return;
    };
    for nested in metas.into_iter().skip(1) {
        collect_derive_names_from_meta(&nested, aliases, derives);
    }
}

/// Pushes every name each path in `paths` could resolve to onto `derives` — a name this
/// module recognizes as one of Rust's own derivable traits pushed as itself, and
/// anything else pushed as [`UNRESOLVED_DERIVE`].
///
/// Found by Codex review of this change (PR #143), round 33: `#[derive(MakeClone)]`,
/// where `MakeClone` is a procedural derive macro, is legal Rust whose expansion this
/// module cannot see — a derive macro is not bound to generate an implementation only
/// for the trait its own name suggests, so `MakeClone` could just as well expand to
/// `impl Clone for Recovery` beside whatever else it derives. Recording the resolved
/// name literally let it through as an ordinary, harmless-looking derive that simply is
/// not `"Clone"`, the same bypass `UNRESOLVED_DERIVE` exists to close for an alias this
/// scan gave up chasing rather than one it chased all the way to a name it cannot
/// expand. `DERIVABLE_BUILTIN_TRAITS` is every trait `derive` can name without a
/// third-party macro; anything else fails closed the same way.
///
/// Round 39 found the same bypass one level earlier: `use custom::MakeClone as Debug;
/// #[derive(Debug)]` — or the un-renamed `use custom::Debug;`, since a plain import
/// shadows a name exactly as a `use .. as` one does — resolves `Debug` through that
/// alias to `custom::MakeClone` (or `custom::Debug`), and when neither the crate root
/// nor a real one names a further alias for it, `resolve_segment_chain`'s own "no
/// matching alias, take the last segment" fallback reports whatever that path's last
/// segment happens to spell, `"Debug"` included if the imported item shares the name.
/// The whitelist then read the resolved string alone and trusted it as the concrete
/// builtin, with no way to tell "the literal, never-rebound identifier `Debug`" from
/// "a fallback guess that happens to read `Debug`". `Clone` is exempt from the new
/// check: resolving *to* `Clone` through an alias is the intended detection round 13's
/// own `Klon` test already relies on, so distrusting it here would reopen that gap
/// rather than close this one — the risk is specific to the eight names this scan
/// otherwise discards as harmless.
fn push_resolved_names(
    paths: &syn::punctuated::Punctuated<syn::Path, syn::Token![,]>,
    aliases: &[UseAlias],
    derives: &mut Vec<String>,
) {
    const DERIVABLE_BUILTIN_TRAITS: &[&str] = &[
        "Clone",
        "Copy",
        "Debug",
        "Default",
        "Eq",
        "Hash",
        "Ord",
        "PartialEq",
        "PartialOrd",
    ];
    for path in paths {
        // The path's own first segment, exactly as written — not the name resolution
        // eventually lands on — because what matters here is whether *this* derive
        // path was ever handed off to an alias at all, not what came back.
        let locally_rebound = path.segments.first().is_some_and(|segment| {
            let first = ident_name(&segment.ident);
            aliases.iter().any(|alias| alias.local == first)
        });
        for name in every_resolution(path, aliases) {
            let trusted = name == UNRESOLVED_DERIVE
                || name == LOCAL_SHADOWED_TYPE
                || name == "Clone"
                || (!locally_rebound && DERIVABLE_BUILTIN_TRAITS.contains(&name.as_str()));
            if trusted {
                derives.push(name);
            } else {
                derives.push(UNRESOLVED_DERIVE.to_owned());
            }
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
    struct Literals<'ast> {
        stack: Vec<&'ast [syn::Item]>,
        name: String,
        count: usize,
    }

    impl<'ast> syn::visit::Visit<'ast> for Literals<'ast> {
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

        fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
            // A nested module's own item list goes on top of the stack
            // (issue #109 review): pushed while walking it, popped
            // afterward, so `super::`/`crate::` can still reach an ancestor
            // scope while the module's own scope never inherits it
            // implicitly. An out-of-line declaration (`mod x;`) has no body
            // to push, but its own name and attributes must still be
            // visited the default way — an early return here had skipped
            // them (Codex review, PR #160), hiding a banned identifier
            // spelled as a module name.
            let pushed = node.content.is_some();
            if let Some((_, items)) = node.content.as_ref() {
                self.stack.push(items);
            }
            syn::visit::visit_item_mod(self, node);
            if pushed {
                self.stack.pop();
            }
        }

        fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
            let resolved = resolve_segments(&node.path, &self.stack);
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

    let mut total = Literals {
        stack: vec![&file.items],
        name: name.to_owned(),
        count: 0,
    };
    total.visit_file(&file);

    let mut inside_count = 0_usize;
    for target in inside_targets(&file, &inside) {
        let mut visitor = Literals {
            stack: target.stack().to_vec(),
            name: name.to_owned(),
            count: 0,
        };
        match &target {
            InsideTarget::Block(block, _) => visitor.visit_block(block),
            InsideTarget::Impl(implementation, _) => visitor.visit_item_impl(implementation),
        }
        inside_count = inside_count.saturating_add(visitor.count);
    }

    Ok(LiteralCounts {
        total: total.count,
        inside: inside_count,
    })
}

/// Something [`struct_literal_counts`] can count literals inside of, with the
/// aliases in scope at the point it was found (issue #109 review: an inner
/// module's own, not inherited from where the search started).
enum InsideTarget<'a> {
    /// A function body.
    Block(&'a syn::Block, Vec<&'a [syn::Item]>),
    /// An `impl` block, visited whole.
    Impl(&'a syn::ItemImpl, Vec<&'a [syn::Item]>),
}

impl<'a> InsideTarget<'a> {
    fn stack(&self) -> &[&'a [syn::Item]] {
        match self {
            Self::Block(_, stack) | Self::Impl(_, stack) => stack,
        }
    }
}

/// The bodies [`FnScope`] selects, in source order.
fn inside_targets<'a>(file: &'a syn::File, scope: &FnScope<'a>) -> Vec<InsideTarget<'a>> {
    let root_stack = vec![file.items.as_slice()];
    match *scope {
        FnScope::None => Vec::new(),
        FnScope::FirstFn(name) => {
            let mut blocks = Vec::new();
            fn_blocks(&file.items, &root_stack, name, &mut blocks);
            blocks.truncate(1);
            blocks
                .into_iter()
                .map(|(block, stack)| InsideTarget::Block(block, stack))
                .collect()
        }
        FnScope::InherentFns { ty, name } => {
            let mut blocks = Vec::new();
            for (implementation, stack) in inherent_impls(&file.items, &root_stack, ty) {
                for item in &implementation.items {
                    if has_cfg_test(impl_item_attrs(item)) {
                        continue;
                    }
                    if let syn::ImplItem::Fn(function) = item {
                        if ident_is(&function.sig.ident, name) {
                            blocks.push(InsideTarget::Block(&function.block, stack.clone()));
                        }
                    }
                }
            }
            blocks
        }
        FnScope::InherentImpls(ty) => inherent_impls(&file.items, &root_stack, ty)
            .into_iter()
            .map(|(implementation, stack)| InsideTarget::Impl(implementation, stack))
            .collect(),
    }
}

/// The bodies of every `fn name`, in source order through inline modules and `impl`
/// blocks, skipping `#[cfg(test)]`. Each body carries the alias stack visible where
/// it was found: `stack` plus an inline module's own on top of it (issue #109
/// review), so `super::`/`crate::` inside the body can still reach an ancestor scope.
fn fn_blocks<'a>(
    items: &'a [syn::Item],
    stack: &[&'a [syn::Item]],
    name: &str,
    blocks: &mut Vec<(&'a syn::Block, Vec<&'a [syn::Item]>)>,
) {
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Fn(function) if ident_is(&function.sig.ident, name) => {
                blocks.push((&function.block, stack.to_vec()));
            }
            syn::Item::Impl(implementation) => {
                for impl_item in &implementation.items {
                    if has_cfg_test(impl_item_attrs(impl_item)) {
                        continue;
                    }
                    if let syn::ImplItem::Fn(function) = impl_item {
                        if ident_is(&function.sig.ident, name) {
                            blocks.push((&function.block, stack.to_vec()));
                        }
                    }
                }
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    let mut nested_stack = stack.to_vec();
                    nested_stack.push(nested);
                    fn_blocks(nested, &nested_stack, name, blocks);
                }
            }
            _ => {}
        }
    }
}

/// The inherent `impl` blocks for `ty`, in source order through inline modules,
/// skipping `#[cfg(test)]`. Each carries the alias stack visible where it was
/// found: `stack` plus an inline module's own on top of it (issue #109 review).
fn inherent_impls<'a>(
    items: &'a [syn::Item],
    stack: &[&'a [syn::Item]],
    ty: &str,
) -> Vec<(&'a syn::ItemImpl, Vec<&'a [syn::Item]>)> {
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
                found.push((implementation, stack.to_vec()));
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    let mut nested_stack = stack.to_vec();
                    nested_stack.push(nested);
                    found.extend(inherent_impls(nested, &nested_stack, ty));
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
    struct Names<'ast> {
        stack: Vec<&'ast [syn::Item]>,
        idents: Vec<String>,
        paths: Vec<ResolvedPath>,
    }

    impl<'ast> syn::visit::Visit<'ast> for Names<'ast> {
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

        fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
            // A nested module's own item list goes on top of the stack
            // (issue #109 review): pushed while walking it, popped
            // afterward, so `super::`/`crate::` can still reach an ancestor
            // scope while the module's own scope never inherits it
            // implicitly. An out-of-line declaration (`mod x;`) has no body
            // to push, but its own name and attributes must still be
            // visited the default way — an early return here had skipped
            // them (Codex review, PR #160), hiding a banned identifier
            // spelled as a module name.
            let pushed = node.content.is_some();
            if let Some((_, items)) = node.content.as_ref() {
                self.stack.push(items);
            }
            syn::visit::visit_item_mod(self, node);
            if pushed {
                self.stack.pop();
            }
        }

        fn visit_ident(&mut self, node: &'ast syn::Ident) {
            self.idents.push(ident_name(node));
        }

        fn visit_path(&mut self, node: &'ast syn::Path) {
            self.paths.push(ResolvedPath {
                segments: resolve_segments(node, &self.stack),
            });
            syn::visit::visit_path(self, node);
        }
    }

    let file = parse_rust(contents)?;

    let mut names = Names {
        stack: vec![&file.items],
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
    /// The 1-indexed source line the `fn` keyword itself sits on — not the identifier's
    /// line, which a comment between `fn` and the name (legal Rust) can separate from it
    /// (issue #97, Codex review round 7): a caller checking whether an *item* sits inside
    /// a line range means the whole item, starting at its own keyword.
    ///
    /// Read off the parsed item's own span rather than found again by a second,
    /// independent text search: two searches for "the same" declaration can each answer
    /// about a different one when a name is declared more than once, which is exactly
    /// the ambiguity a caller matching attributes to a position must not have (issue
    /// #97, Codex review round 5).
    pub line: usize,
    /// The 1-indexed source line of the function's closing brace.
    ///
    /// A caller must check both ends of an item, not only [`line`](Self::line). An
    /// anchor can end between the `fn` keyword and the body. Then the start line does
    /// not prove the page shows the whole function (issue #165, Codex review round 8 of
    /// issue #97).
    pub end_line: usize,
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
pub(crate) fn fns_matching(contents: &str, name: &str, include_test_gated: bool) -> Vec<NamedFn> {
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
                    line: function.sig.fn_token.span.start().line,
                    end_line: function.block.brace_token.span.close().start().line,
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
                                line: method.sig.fn_token.span.start().line,
                                end_line: method.block.brace_token.span.close().start().line,
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
    /// Candidate workspace-relative paths for the child's file, grouped by which
    /// build selects them — in `rustc`'s probe order within a group (see
    /// [`child_modules`]).
    ///
    /// A caller resolves each group independently to at most one scanned file
    /// (ambiguous only *within* a group — the shape `rustc` itself rejects, such as
    /// both `name.rs` and `name/mod.rs` present at once) and scans every group's
    /// resolution, rather than requiring exactly one match across the whole thing.
    /// Round 17 of Codex review on this change (PR #143) found why the two cannot be
    /// merged into one flat list: a natural `name.rs` and a `#[cfg_attr(feature =
    /// "x", path = "alt.rs")] mod name;` target can both legally exist in the source
    /// tree at once — two different builds pick different files, which `rustc` does
    /// not consider ambiguous — and a flat candidate list that saw both present would
    /// misreport a real, legal layout as `ModuleTreeError::Ambiguous`.
    pub candidates: Vec<Vec<String>>,
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
/// A `#[path]` attribute names exactly the file `rustc` reads (issue #59): an
/// unconditional one offers no natural directory fallback, because a fallback would
/// scan a file the compiler never reads. Each candidate is lexically normalized, so
/// `#[path = "../shared.rs"]` matches the `shared.rs` beside the parent directory.
///
/// A `#[cfg_attr(.., path = "...")]` is different: this module does not evaluate a
/// `cfg`'s condition, so a build under which the condition is false really does resolve
/// the module naturally, and one under which it is true really does load the named
/// file instead. Round 16 of Codex review on this change (PR #143) found exactly that
/// pair — a harmless natural `recovery/child.rs` sitting beside a
/// `#[cfg_attr(all(), path = "recovery/clone_impl.rs")]`, with `rustc` loading the
/// latter — so both the natural pair and every `cfg_attr`-nested `path` are scanned as
/// candidates, at any nesting depth of `cfg_attr`.
///
/// Inline modules have no file and are not returned, but the walk descends into them: a
/// `mod data;` inside `mod tests { ... }` lives under `tests/`, and inherits the outer
/// module's test-gating. The walk also descends into a function, method, or default
/// trait-method body, at any nesting depth of `if`, `match`, a loop, or a bare block —
/// not only the body's own immediate statements — because a `mod` declared as a local
/// item anywhere in there resolves to a file the same way a module-scope one does
/// (issue #77's PR #143: round 14 found one written directly in an ordinary method's
/// body, and round 15 found one a control-flow block deeper still).
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
    // Round 38 of Codex review on this change (PR #143): `recovery-surface`'s
    // Clone-detection scan now walks the module tree from `waymaker-flash`'s crate root
    // rather than from `recovery.rs` (see `RECOVERY_ADAPTER_ROOT_PATH`), and `lib.rs` —
    // like `main.rs` for a binary crate — resolves a `mod` declared in it the same way
    // `mod.rs` resolves one: beside itself, in the directory it is already in, never
    // under a `lib/` or `main/` subdirectory. Every prior caller of this function walked
    // from a non-root file, so this shape had never been exercised before.
    let is_crate_root = matches!(parent_path, "lib.rs" | "main.rs")
        || parent_path.ends_with("/lib.rs")
        || parent_path.ends_with("/main.rs");
    let child_dir = if parent_path == "mod.rs" || parent_path.ends_with("/mod.rs") || is_crate_root
    {
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

/// The items a block declares as local items — `Stmt::Item`, the shape `mod`, `fn` and
/// `struct` all take when written inside a function or method body — reached at any
/// nesting depth of `if`, `else`, `match`, `loop`, `while`, `for`, a bare `{ .. }`, or a
/// closure body, not only the block's own immediate statements.
///
/// Round 15 of Codex review on this change (PR #143) found
/// `if true { #[path = "recovery/clone_impl.rs"] mod clone_impl; }` written inside an
/// ordinary method: a local item one control-flow block deeper than the method's own
/// body, which an earlier version of this function — reading only `block.stmts`
/// directly — could not see, exactly the residual limit this function's doc used to
/// state as "legal Rust, and ... not a shape review of this change found a working
/// example of". A [`syn::visit::Visit`] is what closes it, the same way
/// [`resolved_path_uses`] and [`declares_item_macro`] already use one to reach a nesting
/// depth a hand-rolled recursion over one enum's variants cannot enumerate ahead of
/// time: the default `visit_block` walks into every expression's own nested blocks for
/// free, so this only needs to say what to do with a `Stmt::Item` once it is reached.
///
/// The traversal stops at a `Stmt::Item` rather than descending into it — a nested
/// `fn`'s own body, for instance — because every caller of this function already
/// recurses into a found `Item::Fn`/`Item::Impl`/`Item::Trait` itself, calling this
/// function again on its body; visiting through the item here as well would walk that
/// same nested body twice.
fn block_items(block: &syn::Block) -> Vec<&syn::Item> {
    let mut visitor = BlockItemVisitor { items: Vec::new() };
    visitor.visit_block(block);
    visitor.items
}

/// [`block_items`], starting from an expression rather than a block — a `const` or
/// `static` initializer, or an associated const's default, is an [`syn::Expr`] rather
/// than a [`syn::Block`], and round 17 of Codex review on this change (PR #143) found
/// `const _: () = { impl Clone for super::Recovery { .. } };` reaching none of the
/// scanners that already recurse into a function or method body: the initializer *can*
/// itself be exactly such a block, and this is the same walk one layer up.
fn expr_items(expr: &syn::Expr) -> Vec<&syn::Item> {
    let mut visitor = BlockItemVisitor { items: Vec::new() };
    visitor.visit_expr(expr);
    visitor.items
}

/// [`block_items`], starting from a type rather than a block or an expression — an
/// array type's length (`[T; N]`, where `N` is a const expression) or a const generic
/// argument (`Foo<{ N }>`) can each be a block, and round 18 of Codex review on this
/// change (PR #143) found `type R = [(); { impl Clone for super::Recovery { .. }; 1
/// }];` reaching neither scanner: the visitor here walks the whole type looking for an
/// embedded expression the same way [`expr_items`] walks one expression looking for a
/// nested block.
fn type_items(ty: &syn::Type) -> Vec<&syn::Item> {
    let mut visitor = BlockItemVisitor { items: Vec::new() };
    visitor.visit_type(ty);
    visitor.items
}

/// [`block_items`], starting from a path rather than a block, an expression or a type —
/// a path segment's own generic arguments can carry a const generic argument the same
/// way a type's can. Round 25 of Codex review on this change (PR #143) found an impl's
/// own trait path reaching neither scanner: `impl Marker<{ impl Clone for
/// super::Recovery { .. }; 0 }> for Holder {}` names a trait path this walk never read.
fn path_items(path: &syn::Path) -> Vec<&syn::Item> {
    let mut visitor = BlockItemVisitor { items: Vec::new() };
    visitor.visit_path(path);
    visitor.items
}

/// [`block_items`], starting from a set of generics rather than a block, an expression
/// or a type — a type parameter's own bounds and default, and a `where` clause
/// predicate's bounded type and bounds, can each carry a buried block the same way a
/// parameter or return type can, and round 22 of Codex review on this change (PR #143)
/// found `where Wrapper<{ impl Clone for super::Recovery { .. }; 0 }>: Trait` reaching
/// neither scanner: [`fn_signature_type_items`] read only `signature.inputs` and
/// `signature.output`, never `signature.generics`.
fn generics_items(generics: &syn::Generics) -> Vec<&syn::Item> {
    let mut visitor = BlockItemVisitor { items: Vec::new() };
    visitor.visit_generics(generics);
    visitor.items
}

/// Every [`type_items`] finds in a function or method signature's own parameter types,
/// return type, and generics (type parameter bounds and defaults, and `where` clause
/// predicates), in addition to whatever its body carries.
///
/// Round 21 of Codex review on this change (PR #143) found `fn hidden(_: [(); { impl
/// Clone for super::Recovery { .. }; 0 }]) {}` reaching neither scanner: every prior
/// round that walked a function or method descended only into its *body*
/// ([`block_items`]), never its signature, and a parameter type — or a return type — is
/// exactly as capable of burying a block as a type alias's, a struct field's, or an
/// enum variant field's own type already proved. Round 22 found the same gap in the
/// signature's own generics, closed by [`generics_items`].
fn fn_signature_type_items(signature: &syn::Signature) -> Vec<&syn::Item> {
    signature
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            syn::FnArg::Typed(typed) => Some(type_items(&typed.ty)),
            syn::FnArg::Receiver(_) => None,
        })
        .flatten()
        .chain(match &signature.output {
            syn::ReturnType::Type(_, ty) => type_items(ty),
            syn::ReturnType::Default => Vec::new(),
        })
        .chain(generics_items(&signature.generics))
        .collect()
}

/// The shared walk [`block_items`], [`expr_items`] and [`type_items`] each drive: every
/// `Stmt::Item` at any nesting depth of control flow, stopping at the item itself
/// rather than descending into it — every caller of any of the three already recurses
/// into a found `Item::Fn`/`Item::Impl`/`Item::Trait`/`Item::Const`/`Item::Static`/
/// `Item::Enum`/`Item::Type` itself, so walking through the item here too would walk
/// its own body twice.
struct BlockItemVisitor<'ast> {
    items: Vec<&'ast syn::Item>,
}

impl<'ast> syn::visit::Visit<'ast> for BlockItemVisitor<'ast> {
    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if let syn::Stmt::Item(item) = stmt {
            self.items.push(item);
            return;
        }
        syn::visit::visit_stmt(self, stmt);
    }
}

/// The shared walk [`direct_blocks_in_expr`], [`direct_blocks_in_type`] and
/// [`direct_blocks_in_generics`] each drive: every [`syn::Block`] reachable without
/// crossing into a nested item's own body, captured rather than descended into further
/// — a caller that wants what is nested *inside* a captured block recurses into it
/// itself, at its own scope, which is exactly what
/// [`collect_trait_implementors_in_block`] does.
///
/// Retired in round 23 of Codex review on this change (PR #143): this file used to
/// have a `BlockItemVisitor` that flattened every `Stmt::Item` at any nesting depth
/// into one list, which is what let a `use` or `type` alias declared inside one
/// control-flow block reach an unrelated `impl` in a sibling block or the enclosing
/// one — see [`collect_trait_implementors_in_block`]'s own doc for the finding. This
/// visitor captures *blocks* instead of items, and stops at each one rather than
/// flattening through it, so a caller can recurse block by block and keep every
/// block's own scope separate.
struct DirectChildBlockVisitor<'ast> {
    blocks: Vec<&'ast syn::Block>,
}

impl<'ast> syn::visit::Visit<'ast> for DirectChildBlockVisitor<'ast> {
    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.blocks.push(block);
    }

    fn visit_item(&mut self, _item: &'ast syn::Item) {
        // Stop at a nested item's own boundary: its body is
        // `collect_trait_implementors_in_item_body`'s concern, not this visitor's.
    }
}

/// Every block [`direct_child_blocks_of_block`] can reach starting from an expression
/// rather than a block's own statements — a `const` or `static` initializer, an enum
/// variant's discriminant, or an associated const's default is a [`syn::Expr`] rather
/// than a [`syn::Block`] directly, and this is the same one-level capture one layer up.
fn direct_blocks_in_expr(expr: &syn::Expr) -> Vec<&syn::Block> {
    let mut visitor = DirectChildBlockVisitor { blocks: Vec::new() };
    visitor.visit_expr(expr);
    visitor.blocks
}

/// [`direct_blocks_in_expr`], starting from a type rather than an expression — an array
/// type's length or a const generic argument can each embed a block the same way an
/// expression can.
fn direct_blocks_in_type(ty: &syn::Type) -> Vec<&syn::Block> {
    let mut visitor = DirectChildBlockVisitor { blocks: Vec::new() };
    visitor.visit_type(ty);
    visitor.blocks
}

/// [`direct_blocks_in_expr`], starting from a path rather than an expression — a path
/// segment's own generic arguments can carry a const generic argument the same way a
/// type's can, since a type is often spelled as exactly such a path
/// (`Wrapper<{ .. }>`). Round 25 of Codex review on this change (PR #143) found this
/// gap in an `impl` header specifically: `impl Marker<{ impl Clone for super::Recovery
/// { .. }; 0 }> for Holder {}` is legal Rust with `non_local_definitions` allowed, and
/// [`collect_trait_implementors_in_item_body`]'s `Item::Impl` arm walked the impl's own
/// generic *declarations* and its members, but never the trait path it implements.
fn direct_blocks_in_path(path: &syn::Path) -> Vec<&syn::Block> {
    let mut visitor = DirectChildBlockVisitor { blocks: Vec::new() };
    visitor.visit_path(path);
    visitor.blocks
}

/// [`direct_blocks_in_expr`], starting from a set of generics — a type parameter's own
/// bounds and default, and a `where` clause predicate's bounded type and bounds, can
/// each embed a block the same way a parameter or return type can.
fn direct_blocks_in_generics(generics: &syn::Generics) -> Vec<&syn::Block> {
    let mut visitor = DirectChildBlockVisitor { blocks: Vec::new() };
    visitor.visit_generics(generics);
    visitor.blocks
}

/// [`direct_blocks_in_expr`], starting from a bound list — a trait's own supertraits
/// (`trait Outer: Marker<{ .. }> {}`) and an associated type's own trait bounds
/// (`type A: Marker<{ .. }>;`) are each a `Punctuated<TypeParamBound, Token![+]>`
/// rather than a type or a set of generics, and a bound's own trait path can carry a
/// const generic argument the same way any other path can. Round 26 of Codex review on
/// this change (PR #143) found neither read at all: a trait's generics and its members
/// were walked, never its supertrait list.
fn direct_blocks_in_bounds(
    bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, syn::Token![+]>,
) -> Vec<&syn::Block> {
    let mut visitor = DirectChildBlockVisitor { blocks: Vec::new() };
    for bound in bounds {
        visitor.visit_type_param_bound(bound);
    }
    visitor.blocks
}

/// [`direct_blocks_in_bounds`]'s [`block_items`]-flavoured twin, for the module-tree
/// walk's own use — mirrors how [`type_items`] pairs with [`direct_blocks_in_type`].
fn bound_items(
    bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, syn::Token![+]>,
) -> Vec<&syn::Item> {
    let mut visitor = BlockItemVisitor { items: Vec::new() };
    for bound in bounds {
        visitor.visit_type_param_bound(bound);
    }
    visitor.items
}

/// Every [`direct_blocks_in_type`] finds in a function or method signature's own
/// parameter types, return type, and generics, mirroring [`fn_signature_type_items`]
/// for [`collect_trait_implementors_in_item_body`]'s own use.
fn direct_blocks_in_signature(signature: &syn::Signature) -> Vec<&syn::Block> {
    signature
        .inputs
        .iter()
        .filter_map(|argument| match argument {
            syn::FnArg::Typed(typed) => Some(direct_blocks_in_type(&typed.ty)),
            syn::FnArg::Receiver(_) => None,
        })
        .flatten()
        .chain(match &signature.output {
            syn::ReturnType::Type(_, ty) => direct_blocks_in_type(ty),
            syn::ReturnType::Default => Vec::new(),
        })
        .chain(direct_blocks_in_generics(&signature.generics))
        .collect()
}

/// Every block nested directly in `block`'s own statements — an `if`, `while`, `for`,
/// `loop`, `match` arm, or bare block expression each introduce a block that is a real
/// Rust scope of its own — without crossing into a nested item's own body and without
/// capturing `block` itself.
///
/// This is [`collect_trait_implementors_in_block`]'s other half: it finds the blocks
/// that function should recurse into next, one level at a time, so a deeply nested
/// block's own scope is built up the same way real Rust builds it — by walking outward
/// in, through every enclosing block in order — rather than by flattening every block
/// at every depth into one list up front.
fn direct_child_blocks_of_block(block: &syn::Block) -> Vec<&syn::Block> {
    let mut visitor = DirectChildBlockVisitor { blocks: Vec::new() };
    for stmt in &block.stmts {
        visitor.visit_stmt(stmt);
    }
    visitor.blocks
}

/// The candidate groups a plain `mod name;` or a `#[path]`-attributed one resolves to,
/// from the declaring module's attributes and its would-be name.
///
/// Split out of [`collect_child_modules`] to keep that function under the line count
/// this file's own `too_many_lines` lint holds every function to; the logic is
/// unchanged from when it lived inline.
///
/// No unconditional `#[path]`: a `#[cfg_attr(.., path = "...")]` may still choose one
/// under some build — round 16 found `#[cfg_attr(all(), path =
/// "recovery/clone_impl.rs")] mod child;` beside a harmless natural
/// `recovery/child.rs`, where `rustc` loads the `cfg_attr` target and the old scan,
/// reading only a direct `#[path]`, found neither: it fell back to the natural pair
/// and saw only the harmless decoy. This module does not evaluate a `cfg`'s condition,
/// so every build's candidate — the natural pair *and* every path a `cfg_attr` could
/// select, at any nesting depth — is scanned. Each is its own group (see
/// [`ChildModule`]'s own doc): round 17 found that merging them into one flat list
/// made a real, legal layout — the natural file *and* the `cfg_attr` target both
/// present, for two different builds — misreport as `Ambiguous`.
///
/// An unconditional `#[path = "..."]`: `rustc` consults exactly this one path, so no
/// fallback.
fn mod_candidates(
    module: &syn::ItemMod,
    parent_dir: &str,
    child_dir: &str,
    name: &str,
) -> Vec<Vec<String>> {
    module.attrs.iter().find_map(path_attr_value).map_or_else(
        || {
            let mut groups = vec![vec![
                format!("{child_dir}{name}.rs"),
                format!("{child_dir}{name}/mod.rs"),
            ]];
            for attr in &module.attrs {
                for path in cfg_attr_path_values(attr) {
                    groups.push(vec![normalize_path(&format!("{parent_dir}{path}"))]);
                }
            }
            groups
        },
        |path| vec![vec![normalize_path(&format!("{parent_dir}{path}"))]],
    )
}

/// Every `(gated, items)` pair `collect_child_modules` should recurse into for one
/// `impl` block's members — its methods' bodies, its associated consts'
/// initializers, and its associated types' own types — split out for the reason
/// [`mod_candidates`] is.
///
/// Round 23 of Codex review on this change (PR #143) found the associated-type
/// case missing: `impl T for X { type A = [(); { impl Clone for Recovery { .. };
/// 0 }]; }` buries a non-local `impl` inside an associated type's own type exactly
/// the way a type alias's or a struct field's type already could, and this match
/// dropped `ImplItem::Type` on the floor instead of walking it with [`type_items`].
/// Round 25 found the same gap one shape over: an associated const's own *declared
/// type* — `const N: [(); { impl Clone for Recovery { .. }; 0 }] = [];` — can bury a
/// block exactly the way its initializer already could, and this arm read only
/// `constant.expr`, never `constant.ty`.
fn impl_member_bodies(
    implementation: &syn::ItemImpl,
    impl_gated: bool,
) -> Vec<(bool, Vec<&syn::Item>)> {
    implementation
        .items
        .iter()
        .filter_map(|member| match member {
            syn::ImplItem::Fn(method) => Some((
                impl_gated || has_cfg_test(&method.attrs),
                block_items(&method.block)
                    .into_iter()
                    .chain(fn_signature_type_items(&method.sig))
                    .collect(),
            )),
            syn::ImplItem::Const(constant) => Some((
                impl_gated || has_cfg_test(&constant.attrs),
                type_items(&constant.ty)
                    .into_iter()
                    .chain(expr_items(&constant.expr))
                    .collect(),
            )),
            syn::ImplItem::Type(assoc_type) => Some((
                impl_gated || has_cfg_test(&assoc_type.attrs),
                generics_items(&assoc_type.generics)
                    .into_iter()
                    .chain(type_items(&assoc_type.ty))
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

/// [`impl_member_bodies`], for a struct's own field types — split out for the same
/// reason and to keep [`collect_child_modules`] under this file's own line-count lint
/// once round 19's `Item::Struct` arm joined it.
fn struct_field_bodies(
    struct_item: &syn::ItemStruct,
    struct_gated: bool,
) -> Vec<(bool, Vec<&syn::Item>)> {
    struct_item
        .fields
        .iter()
        .map(|field| {
            (
                struct_gated || has_cfg_test(&field.attrs),
                type_items(&field.ty),
            )
        })
        .collect()
}

/// [`struct_field_bodies`], for a `union`'s own field types — round 22 of Codex review
/// on this change (PR #143) found the identical field-type shape a `union` declares
/// too, with neither this walk nor the trait-implementor scan's own handling of it
/// reading `Item::Union` at all.
fn union_field_bodies(
    union_item: &syn::ItemUnion,
    union_gated: bool,
) -> Vec<(bool, Vec<&syn::Item>)> {
    union_item
        .fields
        .named
        .iter()
        .map(|field| {
            (
                union_gated || has_cfg_test(&field.attrs),
                type_items(&field.ty),
            )
        })
        .collect()
}

/// [`struct_field_bodies`], for an enum's own variants — each variant's discriminant
/// expression is gated by the variant's own `#[cfg(test)]` alone, and each of its
/// fields is gated by its own `#[cfg(test)]` on top of the variant's, one entry apiece.
///
/// Round 20 of Codex review on this change (PR #143) found that a variant's *fields*,
/// not only its discriminant, can carry a buried block the same way a struct's own
/// field types can (`enum E { V([(); { impl Clone for super::Recovery { .. }; 0 }]) }`)
/// — a `mod` hidden inside one was never reached by this walk, and neither was an
/// `impl` hidden inside one by the trait-implementor scan's own enum handling, which
/// round 20 also closed the same way. Round 26 found that fix had combined every
/// field's items into one entry gated by the *variant* alone, unlike
/// [`struct_field_bodies`] and [`union_field_bodies`], which each gate a field by its
/// own attribute in addition to the enclosing item's — so a field carrying its own
/// `#[cfg(test)]` inside a variant that carries none was misclassified as reachable in
/// production, and one inside a variant that itself carries `#[cfg(test)]` could never
/// be told apart from a field with no gate of its own. Each field is now its own
/// `(bool, Vec<&syn::Item>)` entry, gated by `variant_gated || has_cfg_test(field)`,
/// matching the two sibling helpers exactly; the discriminant has no analogous
/// per-part gate to combine with, so it keeps its own single entry at `variant_gated`.
fn enum_variant_bodies(
    enum_item: &syn::ItemEnum,
    enum_gated: bool,
) -> Vec<(bool, Vec<&syn::Item>)> {
    enum_item
        .variants
        .iter()
        .flat_map(|variant| {
            let variant_gated = enum_gated || has_cfg_test(&variant.attrs);
            let discriminant_entry = variant
                .discriminant
                .as_ref()
                .map(|(_, expr)| (variant_gated, expr_items(expr)));
            let field_entries = variant.fields.iter().map(move |field| {
                (
                    variant_gated || has_cfg_test(&field.attrs),
                    type_items(&field.ty),
                )
            });
            discriminant_entry.into_iter().chain(field_entries)
        })
        .collect()
}

/// [`impl_member_bodies`], for a trait's own default method bodies and default
/// associated consts.
///
/// Every declared method contributes its signature's own [`fn_signature_type_items`]
/// regardless of whether it has a default body — round 21's finding applies just as
/// much to a trait method with none, since the signature is parsed either way. Round 25
/// found the same standing applies to a trait const's own *declared type*: it exists
/// whether or not the const has a default value, exactly as a method's signature exists
/// whether or not it has a default body, so every declared trait const contributes its
/// type regardless.
fn trait_member_bodies(
    trait_item: &syn::ItemTrait,
    trait_gated: bool,
) -> Vec<(bool, Vec<&syn::Item>)> {
    trait_item
        .items
        .iter()
        .filter_map(|member| match member {
            syn::TraitItem::Fn(method) => {
                let body_items = method.default.as_ref().map_or_else(Vec::new, block_items);
                Some((
                    trait_gated || has_cfg_test(&method.attrs),
                    body_items
                        .into_iter()
                        .chain(fn_signature_type_items(&method.sig))
                        .collect(),
                ))
            }
            syn::TraitItem::Const(constant) => {
                let default_items = constant
                    .default
                    .as_ref()
                    .map_or_else(Vec::new, |(_, expr)| expr_items(expr));
                Some((
                    trait_gated || has_cfg_test(&constant.attrs),
                    type_items(&constant.ty)
                        .into_iter()
                        .chain(default_items)
                        .collect(),
                ))
            }
            // Round 26: the module-tree walk's twin of
            // [`trait_member_scope_roots`]'s own `TraitItem::Type` arm — a trait's own
            // associated type declaration can be a GAT with generics, bounds and a
            // default type, none of which this walk read before.
            syn::TraitItem::Type(assoc_type) => {
                let default_items = assoc_type
                    .default
                    .as_ref()
                    .map_or_else(Vec::new, |(_, ty)| type_items(ty));
                Some((
                    trait_gated || has_cfg_test(&assoc_type.attrs),
                    generics_items(&assoc_type.generics)
                        .into_iter()
                        .chain(bound_items(&assoc_type.bounds))
                        .chain(default_items)
                        .collect(),
                ))
            }
            _ => None,
        })
        .collect()
}

/// The out-of-line `mod`s in `items`, appending to `found` in source order.
///
/// `parent_dir` is the declaring file's directory and `child_dir` the directory its
/// children live in; `gated` is whether an enclosing inline module is `#[cfg(test)]`.
///
/// Every shape a block-bearing body or an initializer comes in is gated the same way
/// an inline module is — an enclosing `#[cfg(test)]`, on the item itself or inherited
/// from `gated`, marks whatever `mod` it declares as test-only rather than hiding it
/// from the walk entirely, for the reason the module doc gives. Round 18 of Codex
/// review on this change (PR #143): the trait-implementor scan already reaches a
/// `mod` — through its `Clone`-detecting eyes, an `impl`, whether handwritten or
/// macro-generated — inside a `const`/`static` initializer, an enum variant's
/// discriminant, or a block buried inside a type alias's own type; this walk needs
/// the same shapes, so a `mod` declared inside one of them is not merely unresolved
/// as a self-type but never even reached as a file at all. Round 19 found the same
/// type-bearing shape one level over, in a struct's own field types. Round 24 found it
/// one shape further still: `Item::Impl`, `Item::Trait`, `Item::Enum`, `Item::Type`,
/// `Item::Struct` and `Item::Union` each declare their own [`syn::Generics`] — a type
/// parameter's bounds and default, and a `where` clause predicate — which this walk
/// read for none of them (a function's or method's signature is the one shape that was
/// already covered, through [`fn_signature_type_items`]).
fn collect_child_modules<'a>(
    items: impl IntoIterator<Item = &'a syn::Item>,
    parent_dir: &str,
    child_dir: &str,
    gated: bool,
    found: &mut Vec<ChildModule>,
) {
    for item in items {
        let syn::Item::Mod(module) = item else {
            for (nested_gated, nested_items) in nested_item_bodies_for_child_modules(item, gated) {
                collect_child_modules(nested_items, parent_dir, child_dir, nested_gated, found);
            }
            continue;
        };
        let name = ident_name(&module.ident);
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
            found.push(ChildModule {
                candidates: mod_candidates(module, parent_dir, child_dir, &name),
                name,
                test_gated: item_gated,
            });
        }
    }
}

/// Every `(gated, items)` pair [`collect_child_modules`] should recurse into for one
/// item that is not itself a `syn::Item::Mod` — split out to keep that function under
/// this file's own line-count lint once round 24's generics arms joined it.
///
/// Shaped like [`collect_trait_implementors_in_item_body`], but keeps this walk's own
/// `test_gated` tracking, which that function throws away — see the module doc for
/// why the two scans stay separate. An `impl`, `trait`, `enum`, `struct` or `union`
/// item's own generics — a type parameter's bounds and default, and a `where` clause
/// predicate — can bury a `mod` the same way its members' or fields' types can, and
/// round 24 of Codex review on this change (PR #143) found that neither this walk nor
/// the per-member helpers below read it; each arm's own gate now covers the generics
/// pass alongside the member-body pass. Round 25 found the same gap in an `impl`
/// header's own trait path and self type, which can each bury a `mod` through a const
/// generic argument the same way the impl's own generic declarations already could.
fn nested_item_bodies_for_child_modules(
    item: &syn::Item,
    gated: bool,
) -> Vec<(bool, Vec<&syn::Item>)> {
    match item {
        syn::Item::Fn(function) => {
            let item_gated = gated || has_cfg_test(&function.attrs);
            vec![(
                item_gated,
                block_items(&function.block)
                    .into_iter()
                    .chain(fn_signature_type_items(&function.sig))
                    .collect(),
            )]
        }
        syn::Item::Impl(implementation) => {
            let impl_gated = gated || has_cfg_test(&implementation.attrs);
            let header_items: Vec<&syn::Item> = generics_items(&implementation.generics)
                .into_iter()
                .chain(
                    implementation
                        .trait_
                        .as_ref()
                        .map_or_else(Vec::new, |(_, path, _)| path_items(path)),
                )
                .chain(type_items(&implementation.self_ty))
                .collect();
            std::iter::once((impl_gated, header_items))
                .chain(impl_member_bodies(implementation, impl_gated))
                .collect()
        }
        syn::Item::Trait(trait_item) => {
            let trait_gated = gated || has_cfg_test(&trait_item.attrs);
            let header_items: Vec<&syn::Item> = generics_items(&trait_item.generics)
                .into_iter()
                .chain(bound_items(&trait_item.supertraits))
                .collect();
            std::iter::once((trait_gated, header_items))
                .chain(trait_member_bodies(trait_item, trait_gated))
                .collect()
        }
        syn::Item::Const(constant) => {
            let item_gated = gated || has_cfg_test(&constant.attrs);
            vec![(
                item_gated,
                type_items(&constant.ty)
                    .into_iter()
                    .chain(expr_items(&constant.expr))
                    .collect(),
            )]
        }
        syn::Item::Static(statik) => {
            let item_gated = gated || has_cfg_test(&statik.attrs);
            vec![(
                item_gated,
                type_items(&statik.ty)
                    .into_iter()
                    .chain(expr_items(&statik.expr))
                    .collect(),
            )]
        }
        syn::Item::Enum(enum_item) => {
            let enum_gated = gated || has_cfg_test(&enum_item.attrs);
            std::iter::once((enum_gated, generics_items(&enum_item.generics)))
                .chain(enum_variant_bodies(enum_item, enum_gated))
                .collect()
        }
        syn::Item::Type(type_item) => {
            let item_gated = gated || has_cfg_test(&type_item.attrs);
            vec![(
                item_gated,
                generics_items(&type_item.generics)
                    .into_iter()
                    .chain(type_items(&type_item.ty))
                    .collect(),
            )]
        }
        syn::Item::Struct(struct_item) => {
            let struct_gated = gated || has_cfg_test(&struct_item.attrs);
            std::iter::once((struct_gated, generics_items(&struct_item.generics)))
                .chain(struct_field_bodies(struct_item, struct_gated))
                .collect()
        }
        syn::Item::Union(union_item) => {
            let union_gated = gated || has_cfg_test(&union_item.attrs);
            std::iter::once((union_gated, generics_items(&union_item.generics)))
                .chain(union_field_bodies(union_item, union_gated))
                .collect()
        }
        // Round 26: the module-tree walk's twin of the trait-implementor scan's own
        // `Item::ForeignMod` arm — a `mod` hidden inside a foreign function's
        // signature or a foreign static's declared type was never even reached as a
        // file, the same standing every other shape in this function already records.
        syn::Item::ForeignMod(foreign_mod) => foreign_mod_bodies(foreign_mod, gated),
        _ => Vec::new(),
    }
}

/// [`nested_item_bodies_for_child_modules`]'s `Item::ForeignMod` arm — split out to
/// keep that function under this file's own line-count lint once round 26's arm
/// joined it, the same way [`impl_member_scope_roots`] was split from
/// [`collect_trait_implementors_in_item_body`].
fn foreign_mod_bodies(
    foreign_mod: &syn::ItemForeignMod,
    gated: bool,
) -> Vec<(bool, Vec<&syn::Item>)> {
    let mod_gated = gated || has_cfg_test(&foreign_mod.attrs);
    foreign_mod
        .items
        .iter()
        .filter_map(|foreign_item| match foreign_item {
            syn::ForeignItem::Fn(function) => Some((
                mod_gated || has_cfg_test(&function.attrs),
                fn_signature_type_items(&function.sig),
            )),
            syn::ForeignItem::Static(statik) => Some((
                mod_gated || has_cfg_test(&statik.attrs),
                type_items(&statik.ty),
            )),
            _ => None,
        })
        .collect()
}

/// The string value of a `#[path = "..."]` attribute, if present.
fn path_attr_value(attr: &syn::Attribute) -> Option<String> {
    path_meta_value(&attr.meta)
}

/// [`path_attr_value`], over one `syn::Meta` rather than a whole `syn::Attribute` — the
/// shape a `cfg_attr`'s own arguments come in.
fn path_meta_value(meta: &syn::Meta) -> Option<String> {
    let syn::Meta::NameValue(named) = meta else {
        return None;
    };
    if !path_is_ident(&named.path, "path") {
        return None;
    }
    let syn::Expr::Lit(lit) = &named.value else {
        return None;
    };
    let syn::Lit::Str(value) = &lit.lit else {
        return None;
    };
    Some(value.value())
}

/// Every `path = "..."` value reachable by expanding `attr` as a
/// `#[cfg_attr(.., path = "...")]`, however many levels deep, regardless of what the
/// condition at each level is.
///
/// Round 16 of Codex review on this change (PR #143): this module does not evaluate a
/// `cfg`'s condition (see the module doc's residual limits), so a `#[cfg_attr(all(),
/// path = "recovery/clone_impl.rs")] mod child;` beside a harmless natural
/// `recovery/child.rs` is a case where `rustc` really does load `clone_impl.rs` under
/// that build, and [`collect_child_modules`] has to scan it as a candidate rather than
/// only the natural pair a version reading only a direct `#[path]` would fall back to.
/// The recursion mirrors [`meta_introduces_cfg`]'s: `#[cfg_attr(a, cfg_attr(b, path =
/// "..."))]` is valid Rust that reaches its `path` two levels down.
fn cfg_attr_path_values(attr: &syn::Attribute) -> Vec<String> {
    if !path_is_ident(attr.path(), "cfg_attr") {
        return Vec::new();
    }
    let Ok(metas) = attr.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return Vec::new();
    };
    metas.iter().skip(1).flat_map(meta_path_values).collect()
}

/// [`cfg_attr_path_values`], over one `cfg_attr` argument rather than over a whole
/// attribute — a `path = "..."` directly, or another `cfg_attr` nested inside it.
fn meta_path_values(meta: &syn::Meta) -> Vec<String> {
    if let Some(value) = path_meta_value(meta) {
        return vec![value];
    }
    let syn::Meta::List(list) = meta else {
        return Vec::new();
    };
    if !path_is_ident(&list.path, "cfg_attr") {
        return Vec::new();
    }
    let Ok(nested) = list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return Vec::new();
    };
    nested.iter().skip(1).flat_map(meta_path_values).collect()
}

/// True if `text` is a string, byte-string, or C-string literal.
///
/// The six forms this function matches:
/// - `"..."` — a string
/// - `r"..."` or `r#"..."#` — a raw string
/// - `b"..."` — a byte string
/// - `br"..."` or `br#"..."#` — a raw byte string
/// - `c"..."` — a C string (stable since Rust 1.77)
/// - `cr"..."` or `cr#"..."#` — a raw C string
///
/// A char literal (`'x'`) or a byte literal (`b'x'`) holds one character. It cannot
/// spell a callee name. This function does not match these two forms.
fn is_string_literal(text: &str) -> bool {
    let text = text
        .strip_prefix('b')
        .or_else(|| text.strip_prefix('c'))
        .unwrap_or(text);
    text.strip_prefix('r').map_or_else(
        || text.starts_with('"'),
        |rest| rest.trim_start_matches('#').starts_with('"'),
    )
}

/// Replaces each string or byte-string literal in `stream` with an empty one.
///
/// Every other token stays the same. This includes a literal's own quote marks.
///
/// A literal token's rendered text keeps the exact text from the source file (issue
/// #158). The text `"route via crc32(input)"` still shows the callee's name after
/// rendering. A text scan cannot tell this mention from a real call to `crc32`. But to
/// `rustc`, a string's content is data, not a call. This function removes the content
/// so the scan cannot see it.
fn blank_string_literals(stream: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    stream.into_iter().map(blank_string_literal_tree).collect()
}

/// The one-token step for [`blank_string_literals`].
///
/// A group keeps its own delimiters and span. Only the tokens inside it change.
fn blank_string_literal_tree(tree: proc_macro2::TokenTree) -> proc_macro2::TokenTree {
    match tree {
        proc_macro2::TokenTree::Group(group) => {
            let mut replaced =
                proc_macro2::Group::new(group.delimiter(), blank_string_literals(group.stream()));
            replaced.set_span(group.span());
            proc_macro2::TokenTree::Group(replaced)
        }
        proc_macro2::TokenTree::Literal(literal) if is_string_literal(&literal.to_string()) => {
            let mut blanked = proc_macro2::Literal::string("");
            blanked.set_span(literal.span());
            proc_macro2::TokenTree::Literal(blanked)
        }
        other => other,
    }
}

/// True if `tree` is the identifier `macro_rules`.
fn is_macro_rules_keyword(tree: &proc_macro2::TokenTree) -> bool {
    matches!(tree, proc_macro2::TokenTree::Ident(ident) if ident == "macro_rules")
}

/// Drops every macro invocation's argument tokens, and every local macro definition's
/// body, from `stream`.
///
/// `syn` does not expand macros (this module's own header states the limit). Neither a
/// macro argument nor a macro definition's body runs as code on its own:
/// `stringify!(crc32(input))` does not call `crc32`, and a local
/// `macro_rules! spoof { () => { crc32(input) } }` does not either unless something
/// invokes `spoof!()`. A call-boundary scan cannot tell either case from a real call, so
/// this function drops both instead of rendering them. A macro's own name, and its `!`,
/// stay — so a real call right after a macro in the same statement still renders at its
/// own token boundary.
///
/// Both shapes are matched by their tokens alone, and each is the only construct Rust
/// grammar has with that shape:
///
/// - **A macro invocation**: an identifier, then `!`, then a delimited group. A bare `!`
///   never follows an identifier with nothing between them except as a macro call — the
///   logical-not `!` is a prefix operator and always needs an operator, a delimiter, or
///   the start of an expression before it, never an identifier.
/// - **A macro definition**: the identifier `macro_rules`, then `!`, then the macro's
///   own name, then a delimited group holding its rules.
fn blank_macro_arguments(stream: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
    let tokens: Vec<proc_macro2::TokenTree> = stream.into_iter().collect();
    let mut kept: Vec<proc_macro2::TokenTree> = Vec::with_capacity(tokens.len());
    let mut rest: &[proc_macro2::TokenTree] = &tokens;
    loop {
        if let [
            keyword,
            proc_macro2::TokenTree::Punct(bang),
            name @ proc_macro2::TokenTree::Ident(_),
            proc_macro2::TokenTree::Group(group),
            after @ ..,
        ] = rest
            && bang.as_char() == '!'
            && is_macro_rules_keyword(keyword)
        {
            kept.push(keyword.clone());
            kept.push(proc_macro2::TokenTree::Punct(bang.clone()));
            kept.push(name.clone());
            kept.push(blanked_group(group));
            rest = after;
            continue;
        }
        if let [
            name @ proc_macro2::TokenTree::Ident(_),
            proc_macro2::TokenTree::Punct(bang),
            proc_macro2::TokenTree::Group(group),
            after @ ..,
        ] = rest
            && bang.as_char() == '!'
        {
            kept.push(name.clone());
            kept.push(proc_macro2::TokenTree::Punct(bang.clone()));
            kept.push(blanked_group(group));
            rest = after;
            continue;
        }
        let Some((first, after)) = rest.split_first() else {
            break;
        };
        kept.push(blank_macro_argument_tree(first.clone()));
        rest = after;
    }
    kept.into_iter().collect()
}

/// An empty group with `group`'s own delimiter and span.
fn blanked_group(group: &proc_macro2::Group) -> proc_macro2::TokenTree {
    let mut emptied = proc_macro2::Group::new(group.delimiter(), proc_macro2::TokenStream::new());
    emptied.set_span(group.span());
    proc_macro2::TokenTree::Group(emptied)
}

/// [`blank_macro_arguments`], one token at a time, for a token that does not start a
/// macro invocation. A group recurses, so a macro call nested inside an `if` or a
/// block is still found.
fn blank_macro_argument_tree(tree: proc_macro2::TokenTree) -> proc_macro2::TokenTree {
    match tree {
        proc_macro2::TokenTree::Group(group) => {
            let mut replaced =
                proc_macro2::Group::new(group.delimiter(), blank_macro_arguments(group.stream()));
            replaced.set_span(group.span());
            proc_macro2::TokenTree::Group(replaced)
        }
        other => other,
    }
}

/// The body of a function or method block as text the token-based scans understand.
///
/// The statements rendered without the outer braces — the way `braced_body` returned
/// them — with `quote`'s spaces around `::` collapsed again: the call scans look for
/// `C::name(` and `name::<`, and the spaced rendering would hide both. String and
/// byte-string literals are blanked first (issue #158), and so is every macro
/// invocation's argument list. A literal or a macro argument is the only rendered text
/// that can spell a callee's name without a real call to it. What the scans do with the
/// text is otherwise unchanged; this is only the bridge from the resolved item back to
/// the textual analyses.
///
/// A raw marker is not stripped here (issue #90). It does not need to be: every
/// consumer matches a substring at a token boundary, and `#` is such a boundary, so
/// `r#stage(` still matches a scan for `stage(`. Strip it anyway if a future consumer
/// starts comparing this text for exact equality.
fn block_text(block: &syn::Block) -> String {
    let mut body = String::new();
    for stmt in &block.stmts {
        let blanked = blank_macro_arguments(blank_string_literals(stmt.to_token_stream()));
        body.push_str(&blanked.to_string());
        body.push(' ');
    }
    body.replace(" :: ", "::")
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
                .flatten()
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

    #[test]
    fn a_module_declared_in_the_crate_root_resolves_beside_it() {
        // Round 38 of Codex review on this change (PR #143): a `mod` declared in
        // `lib.rs` resolves in the same directory as `lib.rs` itself, exactly as one
        // declared in `mod.rs` does — never under a `lib/` subdirectory.
        let modules =
            child_modules("waymaker-flash/src/lib.rs", "mod append;").expect("the fixture parses");
        assert_eq!(modules.len(), 1, "{}", modules.len());
        assert_eq!(modules[0].name, "append");
        assert!(
            modules[0].candidates.iter().flatten().any(|candidate| {
                candidate == "waymaker-flash/src/append.rs"
                    || candidate == "waymaker-flash/src/append/mod.rs"
            }),
            "{:?}",
            modules[0].candidates
        );
        assert!(
            !modules[0]
                .candidates
                .iter()
                .flatten()
                .any(|candidate| candidate.contains("/lib/")),
            "a crate-root module must not resolve under a `lib/` subdirectory: {:?}",
            modules[0].candidates
        );
    }

    #[test]
    fn a_module_declared_in_a_binary_crate_root_resolves_beside_it() {
        let modules =
            child_modules("xtask/src/main.rs", "mod pipeline;").expect("the fixture parses");
        assert!(
            modules[0].candidates.iter().flatten().any(|candidate| {
                candidate == "xtask/src/pipeline.rs" || candidate == "xtask/src/pipeline/mod.rs"
            }),
            "{:?}",
            modules[0].candidates
        );
    }
}

#[cfg(test)]
mod alias_scope_tests {
    //! Codex review, issue #109: a `use` alias is scoped to its own module. It
    //! is not visible in a sibling module, and a sibling module's alias must
    //! not resolve a chain that starts here.
    use super::{future_trait_implementors, name_uses, resolved_path_uses};

    #[test]
    fn an_unrelated_trait_in_a_sibling_module_is_not_a_fifth_future() {
        // Module `a` renames `Future` to `Awaitable` through a chain. Module
        // `b` renames its own, unrelated trait to the same local name,
        // `Awaitable`, and implements it. A flat, unscoped alias table would
        // let `b`'s impl resolve through `a`'s chain and report `Innocent`
        // as a fifth future.
        let code = "mod a {\n\
             use core::future::Future as Pollable;\n\
             pub use Pollable as Awaitable;\n\
             }\n\
             mod b {\n\
             trait Unrelated {}\n\
             use Unrelated as Awaitable;\n\
             struct Innocent;\n\
             impl Awaitable for Innocent {}\n\
             }\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert!(
            !implementors.contains(&"Innocent".to_owned()),
            "an unrelated trait in a sibling module was reported as a future: {implementors:?}"
        );
    }

    #[test]
    fn a_chain_within_one_module_still_resolves() {
        // The scoping fix must not lose the same-module chain issue #109
        // itself asks for: `Awaitable` still means `Future` when both
        // aliases are declared in the same module.
        let code = "mod a {\n\
             use core::future::Future as Pollable;\n\
             pub use Pollable as Awaitable;\n\
             struct Real;\n\
             impl Awaitable for Real {}\n\
             }\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert_eq!(implementors, ["Real"], "{implementors:?}");
    }

    #[test]
    fn a_sibling_modules_alias_does_not_leak_into_name_uses() {
        // `name_uses` shares `resolve_segments`. A path in module `b` must not
        // resolve through module `a`'s alias of the same local name.
        let code = "mod a {\n\
             use core::future::Future as Marker;\n\
             }\n\
             mod b {\n\
             fn f() { let _ = Marker::x; }\n\
             }\n";
        let uses = name_uses(code).expect("the fixture parses");
        assert!(
            !uses
                .paths
                .iter()
                .any(|path| path.segments == ["core", "future", "Future", "x"]),
            "{:?}",
            uses.paths
        );
    }

    #[test]
    fn a_sibling_modules_alias_does_not_leak_into_resolved_path_uses() {
        let code = "mod a {\n\
             use core::future::Future as Marker;\n\
             }\n\
             mod b {\n\
             fn f() { let _ = Marker::x; }\n\
             }\n";
        let paths = resolved_path_uses(code).expect("the fixture parses");
        assert!(
            paths.iter().any(|path| path.segments == ["Marker", "x"]),
            "module b's own, unaliased path went missing: {paths:?}"
        );
    }

    #[test]
    fn a_self_qualified_hop_still_resolves_the_chain() {
        // Codex review of the scoping fix (PR #160): `pub use self::Pollable
        // as Awaitable;` chains through a `self::`-qualified target. The
        // first substitution produces `self::Pollable`; without stripping
        // `self`, the next lookup searches for an alias named `self`, finds
        // none, and the chain stops one hop short of `Future`.
        let code = "pub use core::future::Future as Pollable;\n\
             pub use self::Pollable as Awaitable;\n\
             struct Sneaky;\n\
             impl Awaitable for Sneaky {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert_eq!(implementors, ["Sneaky"], "{implementors:?}");
    }

    #[test]
    fn a_directly_self_qualified_path_resolves_to_its_full_name() {
        // The same stripping must apply to a path written with `self::`
        // directly, not only to an alias target that produced one: `self`
        // names this scope, so `self::Marker` and `Marker` resolve alike.
        // `future_trait_implementors` cannot tell them apart — it reads only
        // the last segment, which `self` never is — so this checks the full
        // resolved path instead.
        let code = "use core::future::Future as Marker;\nfn f() { let _ = self::Marker::x; }\n";
        let paths = resolved_path_uses(code).expect("the fixture parses");
        assert!(
            paths
                .iter()
                .any(|path| path.segments == ["core", "future", "Future", "x"]),
            "{paths:?}"
        );
    }

    #[test]
    fn an_out_of_line_module_declaration_still_names_its_own_identifier() {
        // Codex review of the scoping fix (PR #160): `mod Step;` (no inline
        // body) has nothing to re-scope, but the declaration's own name must
        // still be visited the ordinary way. An early return on no content
        // had skipped it, so `check_kernel_boundary`'s
        // `names_word("Step")` would miss a banned identifier spelled as an
        // out-of-line module name.
        let uses = name_uses("#[allow(non_snake_case)]\nmod Step;\n").expect("the fixture parses");
        assert!(uses.names_word("Step"), "{uses:?}");
    }

    #[test]
    fn a_super_qualified_alias_still_reaches_future() {
        // Codex review of the scoping fix (PR #160): a nested module can
        // explicitly reach an ancestor's alias with `super::`, which is not
        // inheritance — Rust resolves it the same way regardless of
        // nesting. A per-module scope with no memory of its ancestors
        // cannot follow it, so `Sneaky` went unreported.
        let code = "use core::future::Future as Pollable;\nmod child {\n    use super::Pollable \
             as Awaitable;\n    struct Sneaky;\n    impl Awaitable for Sneaky {}\n}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert_eq!(implementors, ["Sneaky"], "{implementors:?}");
    }

    #[test]
    fn a_crate_qualified_path_does_not_falsely_resolve_through_this_files_own_top() {
        // Codex review, round 6: this scanner reads one file at a time and
        // never learns whether that file is the crate root. Treating the
        // parsed file's own top-level aliases as `crate`'s target is a
        // guess that is right only when the scanned file happens to be
        // `lib.rs` — for any other file, `crate::Pollable` names a
        // *different* file's top level, one this scan never sees. A file
        // that locally aliases `Future` to `Pollable` and separately
        // implements an unrelated `crate::Pollable` (some other, real trait
        // at the true crate root) must not have that unrelated impl
        // reported as a fifth future.
        let code = "use core::future::Future as Pollable;\nstruct Innocent;\nimpl crate::Pollable \
             for Innocent {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert!(
            !implementors.contains(&"Innocent".to_owned()),
            "a `crate`-qualified path resolved through this file's own aliases, as though this \
             file were necessarily the crate root: {implementors:?}"
        );
    }

    #[test]
    fn a_top_level_super_does_not_falsely_resolve_through_this_files_own_top() {
        // Codex review, round 7: `super` at the very top of the scanned
        // file (no enclosing `mod {}` written in this file) steps one
        // level above the file's own top-level scope — the module that
        // declared this file as `mod child;`, which this per-file scan
        // never sees, the same reason `crate` is left unresolved.
        // `saturating_sub` had clamped that step at scope 0 instead,
        // silently resolving `super::X` against this file's own aliases
        // as though scope 0 stood in for that unknown outer module.
        let code = "use core::future::Future as Pollable;\nstruct Innocent;\nimpl super::Pollable \
             for Innocent {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert!(
            !implementors.contains(&"Innocent".to_owned()),
            "a top-level `super`-qualified path resolved through this file's own aliases, as \
             though scope 0 were the module above this file: {implementors:?}"
        );
    }

    #[test]
    fn an_absolute_alias_target_does_not_chain_through_a_same_spelled_local_one() {
        // Codex review, round 8: `use ::A as B;` names the external crate
        // `A` from the extern prelude — a leading `::` reaches past every
        // local scope on purpose. `own_aliases` had dropped that marker
        // when recording `B`'s target, so a *separate*, local
        // `use core::future::Future as A;` in the same file let the chain
        // loop treat `B`'s `A` as that local alias and walk straight into
        // `Future`, even though the two `A`s name unrelated things.
        let code = "use core::future::Future as A;\nuse ::A as B;\nstruct Sneaky;\nimpl B for \
             Sneaky {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert!(
            !implementors.contains(&"Sneaky".to_owned()),
            "an absolute alias target chained through a same-spelled local alias: \
             {implementors:?}"
        );
    }

    #[test]
    fn a_plain_relative_path_follows_a_sibling_modules_alias() {
        // Issue #169 (Codex review, PR #160, round 9): `traits` is a sibling
        // `mod` block in the same scope as the `use` statement that names
        // it, with no `crate`/`super`/`self` prefix at all. Nothing outside
        // this file is needed to resolve `traits::Pollable` — unlike
        // `crate::traits::Pollable`, which stays a residual limit.
        let code = "mod traits {\n    pub use core::future::Future as Pollable;\n}\nuse \
             traits::Pollable as Awaitable;\nstruct Sneaky;\nimpl Awaitable for Sneaky {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert_eq!(implementors, ["Sneaky"], "{implementors:?}");
    }

    #[test]
    fn a_plain_relative_path_follows_a_chain_of_nested_sibling_modules() {
        // A path may name more than one level of sibling module —
        // `a::b::Pollable` — not only a single hop.
        let code = "mod a {\n    mod b {\n        pub use core::future::Future as Pollable;\n    \
             }\n}\nuse a::b::Pollable as Awaitable;\nstruct Sneaky;\nimpl Awaitable for Sneaky \
             {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert_eq!(implementors, ["Sneaky"], "{implementors:?}");
    }

    #[test]
    fn an_out_of_line_sibling_module_declaration_is_left_unresolved() {
        // `mod traits;` (issue #169's own out-of-line case) has no body in
        // this file to step into — the declaring module lives in another
        // file this per-file scan never reads. An honest miss, not a
        // guessed match: `Innocent` must not be reported.
        let code = "mod traits;\nuse traits::Pollable as Awaitable;\nstruct Innocent;\nimpl \
             Awaitable for Innocent {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert!(
            !implementors.contains(&"Innocent".to_owned()),
            "an out-of-line module declaration was treated as though its body were known: \
             {implementors:?}"
        );
    }

    #[test]
    fn a_cfg_test_sibling_module_is_not_stepped_into() {
        // Test code is not shipped code (issue #51): a `mod` gated on
        // exactly `#[cfg(test)]` must not be treated as a real sibling
        // module to step into, the same way `own_aliases` already skips a
        // `#[cfg(test)]` `use` item.
        let code = "#[cfg(test)]\nmod traits {\n    pub use core::future::Future as Pollable;\n\
             }\nuse traits::Pollable as Awaitable;\nstruct Innocent;\nimpl Awaitable for \
             Innocent {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert!(
            !implementors.contains(&"Innocent".to_owned()),
            "a #[cfg(test)] module was stepped into as though it shipped: {implementors:?}"
        );
    }

    #[test]
    fn a_self_qualified_alias_still_resolves_inside_an_entered_module() {
        // `self::` still names the module resolution just stepped into
        // (issue #169's own doc comment): the chain must reach `Future`
        // through a `self::`-qualified re-export declared inside `traits`.
        let code = "mod traits {\n    pub use core::future::Future as Pollable;\n    pub use \
             self::Pollable as Awaitable;\n}\nuse traits::Awaitable as Reexported;\nstruct \
             Sneaky;\nimpl Reexported for Sneaky {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert_eq!(implementors, ["Sneaky"], "{implementors:?}");
    }

    #[test]
    fn a_super_qualified_alias_inside_an_entered_module_is_left_unresolved() {
        // Once resolution has stepped into `traits` by name it is off the
        // lexical ancestor stack, so `super::Pollable` there is left as
        // written rather than guessed at — the same residual-limit shape
        // as `crate::` and a top-level `super::` (rounds 6/7). An honest
        // miss: `Innocent` must not be reported, even though `Pollable`
        // really is `Future` one file-scope up.
        let code = "use core::future::Future as Pollable;\nmod traits {\n    pub use \
             super::Pollable as Awaitable;\n}\nuse traits::Awaitable as Reexported;\nstruct \
             Innocent;\nimpl Reexported for Innocent {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert!(
            !implementors.contains(&"Innocent".to_owned()),
            "a `super`-qualified alias inside an entered module resolved as though it were \
             still on the lexical ancestor stack: {implementors:?}"
        );
    }

    #[test]
    fn a_grouped_use_chain_longer_than_the_item_count_still_resolves() {
        // Codex review, PR #176: one `use` item can pack many chained
        // renames into a single group, so an item count alone undercounts
        // the hops a real chain may need. `I` chains through eight renames
        // declared in one group, plus one more item, to reach `Future` —
        // nine hops from four syntax items, more hops than the old,
        // item-count-only bound allowed.
        let code = "use core::future::Future as A;\nuse self::{A as B, B as C, C as D, D as E, \
             E as F, F as G, G as H, H as I};\nstruct Sneaky;\nimpl I for Sneaky {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert_eq!(implementors, ["Sneaky"], "{implementors:?}");
    }

    #[test]
    fn a_bare_path_naming_an_item_is_not_swallowed_by_a_same_named_sibling_module() {
        // Codex review, PR #176: `impl Future for RealFuture` names a
        // trait called `Future`, not a module — even when `mod Future`
        // also exists in the same scope. A one-segment path has nothing
        // left to resolve once it names the module, so stepping into it
        // would only throw the name away and drop a genuine impl.
        let code = "mod Future {\n    pub struct Whatever;\n}\nstruct RealFuture;\nimpl Future \
             for RealFuture {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert_eq!(implementors, ["RealFuture"], "{implementors:?}");
    }

    #[test]
    fn a_crafted_alias_cycle_terminates_without_reporting_a_false_match() {
        // `a` and `b` rename each other with no real target anywhere. The
        // loop bound must stop this rather than loop forever, and the
        // cycle must not somehow read as `Future`.
        let code = "use a as b;\nuse b as a;\nstruct Sneaky;\nimpl b for Sneaky {}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert!(
            !implementors.contains(&"Sneaky".to_owned()),
            "a crafted alias cycle resolved to a false match: {implementors:?}"
        );
    }

    #[test]
    fn a_plain_relative_path_follows_a_sibling_modules_alias_in_every_caller() {
        // The stack representation changed for all five callers that share
        // `resolve_segments`, not just `future_trait_implementors`. Each
        // one must follow the same sibling-module descent on its own path,
        // not only inherit it by accident through a shared helper.
        let code = "mod traits {\n    pub use core::future::Future as Pollable;\n}\nfn f() {\n    \
             let _ = traits::Pollable::x;\n}\n";
        let resolved = resolved_path_uses(code).expect("the fixture parses");
        assert!(
            resolved
                .iter()
                .any(|path| path.segments == ["core", "future", "Future", "x"]),
            "resolved_path_uses missed the sibling-module descent: {resolved:?}"
        );
        let names = name_uses(code).expect("the fixture parses");
        assert!(
            names
                .paths
                .iter()
                .any(|path| path.segments == ["core", "future", "Future", "x"]),
            "name_uses missed the sibling-module descent: {:?}",
            names.paths
        );
    }

    #[test]
    fn a_same_named_sibling_module_in_a_different_branch_does_not_leak_across_scopes() {
        // Module `a` and module `b` each declare their own `mod traits`,
        // one re-exporting the real `Future` and one an unrelated trait
        // under the same local name. `b`'s own module must not resolve
        // through `a`'s, the module-descent counterpart of
        // `an_unrelated_trait_in_a_sibling_module_is_not_a_fifth_future`.
        let code = "mod a {\n    mod traits {\n        pub use core::future::Future as \
             Pollable;\n    }\n    pub use traits::Pollable as Awaitable;\n    struct Real;\n    \
             impl Awaitable for Real {}\n}\nmod b {\n    mod traits {\n        trait Unrelated \
             {}\n        pub use Unrelated as Pollable;\n    }\n    use traits::Pollable as \
             Awaitable;\n    struct Innocent;\n    impl Awaitable for Innocent {}\n}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert_eq!(implementors, ["Real"], "{implementors:?}");
    }

    #[test]
    fn an_unresolvable_tail_after_module_descent_keeps_its_module_prefix() {
        // Codex review, PR #176: `TimerSpec::BestEffort` names a real item
        // through a real module, with no `use` alias anywhere in
        // `TimerSpec`. Descending into `TimerSpec` to look for one must not
        // throw the module's own name away when the look fails — a
        // suffix check like `check_clock_spec_construction`'s needs the
        // whole path back, not a bare `BestEffort` that looks unqualified.
        let code = "mod TimerSpec {\n    pub struct BestEffort;\n}\nfn f() {\n    let _ = \
             TimerSpec::BestEffort;\n}\n";
        let paths = resolved_path_uses(code).expect("the fixture parses");
        assert!(
            paths
                .iter()
                .any(|path| path.segments == ["TimerSpec", "BestEffort"]),
            "module descent dropped its own prefix when nothing further resolved: {paths:?}"
        );
    }

    #[test]
    fn a_sibling_modules_alias_is_still_invisible_through_super() {
        // `super::` reaches the immediate parent only, not a sibling of the
        // current module. Module `b`'s own `Awaitable` must not resolve
        // through `a`'s chain just because both are one level under root.
        let code = "mod a {\n    use core::future::Future as Pollable;\n    pub use Pollable as \
             Awaitable;\n}\nmod b {\n    trait Unrelated {}\n    use Unrelated as Awaitable;\n    \
             struct Innocent;\n    impl Awaitable for Innocent {}\n}\n";
        let implementors = future_trait_implementors(code).expect("the fixture parses");
        assert!(
            !implementors.contains(&"Innocent".to_owned()),
            "a sibling module's alias was reachable through an unrelated `super::` path: \
             {implementors:?}"
        );
    }
}

#[cfg(test)]
mod named_fn_span_tests {
    //! Issue #165: a caller must check the whole span of an item, not only its start
    //! line. `collect_fns_named` has two branches that build a [`NamedFn`] — one for a
    //! free function, one for a method in an `impl` block. Both must record the real
    //! end line, not only the start line.
    use super::fns_matching;

    #[test]
    fn a_free_functions_end_line_is_its_own_closing_brace() {
        let found = fns_matching("fn it_works() {\n    assert!(true);\n}\n", "it_works", true);
        assert_eq!(found.len(), 1, "found {} functions", found.len());
        assert_eq!(found[0].line, 1, "the `fn` keyword sits on line 1");
        assert_eq!(found[0].end_line, 3, "the closing brace sits on line 3");
    }

    #[test]
    fn an_impl_methods_end_line_is_its_own_closing_brace() {
        let sample =
            "impl Fixture {\n    #[test]\n    fn it_works() {\n        assert!(true);\n    }\n}\n";
        let found = fns_matching(sample, "it_works", true);
        assert_eq!(found.len(), 1, "found {} functions", found.len());
        assert_eq!(found[0].line, 3, "the `fn` keyword sits on line 3");
        assert_eq!(found[0].end_line, 5, "the closing brace sits on line 5");
    }
}
