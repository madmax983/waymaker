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

/// Whether `attrs` — the *whole* attribute list on one item — is guaranteed absent
/// whenever `test` is false, considering every `#[cfg(..)]` and `#[cfg_attr(.., ..)]`
/// on it *together*.
///
/// The textual `without_test_modules` blanked on the substring `#[cfg(test)]` alone, and
/// rounds 35 through 37 widened a single attribute's own predicate to recognize
/// `#[cfg(any(test))]`, `#[cfg(all(test, feature = "x"))]` and a `cfg_attr`-spelled
/// equivalent — each still a *per-attribute* recursive rule, combined across several
/// attributes with `.any()`. Found by Codex review of this change (PR #143), round 43:
/// that combination is sound only in the direction it was built for — a single
/// attribute that alone proves the item test-only is enough to prove the whole item
/// test-only too — but several `#[cfg(..)]` attributes on one item are conjunctive,
/// exactly like `all(..)`'s own arguments, and a per-attribute rule that treats each
/// one as an isolated question cannot see a combination that is test-only only because
/// two attributes *correlate* through a flag neither one alone pins down.
/// `#[cfg(any(test, feature = "x"))]` `#[cfg(not(feature = "x"))]` is exactly that: read
/// together the two admit only `test && !x`, which requires `test`, but neither
/// attribute alone does — `any(test, x)` is satisfiable under `x` with no `test`
/// anywhere, and `not(x)` is satisfiable under `!x` the same way, and even a recursive
/// rule that folded every attribute's own formula into one `all(..)` and then asked
/// "does any conjunct alone require test" would still miss it, since neither conjunct
/// does — only the pair does, through the shared `x`.
///
/// [`Cfg`] is the fix: every attribute is parsed into one formula with
/// [`attribute_cfg`] and the whole list is joined with [`Cfg::All`], exactly the way
/// several bare `#[cfg(..)]`s combine in real Rust — but [`Cfg::requires_test`] then
/// answers the combination by *enumeration* over the flags actually named in it rather
/// than by a recursive per-node rule, which is what lets a flag occurring twice, once
/// under `any` and once negated under a sibling attribute, correlate correctly instead
/// of being treated as two independent unknowns.
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs_cfg(attrs).requires_test()
}

/// `attrs` — the whole attribute list on one item — as one [`Cfg`] formula: every
/// `#[cfg(..)]` and `#[cfg_attr(.., ..)]` on it parsed with [`attribute_cfg`] and
/// joined with [`Cfg::All`], exactly the way [`has_cfg_test`] combines them before
/// asking [`Cfg::requires_test`] of the result. Split out so a caller that needs the
/// formula itself — rather than only whether it requires `test` — can combine it with
/// an *enclosing* scope's own formula before asking, which [`has_cfg_test`] alone
/// cannot do: see `declares_item_macro`'s `MacroVisitor` for why that combination
/// matters.
fn attrs_cfg(attrs: &[syn::Attribute]) -> Cfg {
    Cfg::All(
        attrs
            .iter()
            .filter_map(|attr| attribute_cfg(&attr.meta))
            .collect(),
    )
}

/// A `cfg` predicate's truth value, abstracted from `syn`'s parsed tokens into a small
/// tree of its own so several attributes' formulas can be combined with [`Cfg::All`]
/// and then answered by [`Cfg::requires_test`]'s exhaustive enumeration — a bare
/// [`syn::Meta`] cannot be combined this way at all, since two attributes are two
/// separate parse trees with no operator joining them.
///
/// [`Cfg::Atom`] is the leaf for anything this scan cannot read as `test`, `all`,
/// `any` or `not` — an ordinary flag such as `feature = ".."`, or a shape this parser
/// gave up on — carrying the exact tokens it was built from (via [`cfg_atom_text`]) as
/// its identity. Two atoms with identical rendered text are the *same* flag and are
/// held to the same value in every enumerated assignment [`Cfg::requires_test`] tries;
/// two atoms with different text are treated as independent, which is this scan's
/// residual limit rather than a guess — it cannot know that `feature = "a"` and
/// `feature = "b"` are mutually exclusive, only that two occurrences of the identical
/// text name the same flag.
#[derive(Clone)]
enum Cfg {
    /// The bare identifier `test`.
    Test,
    /// A named flag this scan cannot evaluate, identified by its own rendered tokens.
    Atom(String),
    /// A conjunction: true only when every member is.
    All(Vec<Self>),
    /// A disjunction: true when at least one member is.
    Any(Vec<Self>),
    /// A negation of one predicate.
    Not(Box<Self>),
}

/// The most distinct flags [`Cfg::requires_test`] will enumerate every combination of
/// before giving up and answering `false` — a real, reviewed `cfg` predicate names a
/// small handful at most, and this bound keeps a pathological input's cost bounded
/// rather than turning this scan into a denial of service over its own gate.
const MAX_CFG_ATOMS: usize = 20;

impl Cfg {
    /// Whether this predicate is guaranteed **false** whenever `test` is — i.e.,
    /// whether `self && !test` is unsatisfiable — decided by enumerating every
    /// assignment of the distinct flags [`Cfg::atoms`] finds in it, with `test` fixed
    /// to `false`, and requiring [`Cfg::eval`] to answer `false` under every one.
    ///
    /// This is what a purely recursive per-node rule (asking each `all`/`any`/`not`
    /// the same question of its own children in isolation) cannot do: two attributes
    /// naming the identical flag in different places are the *same* variable across
    /// the whole formula, and only trying every joint assignment can see that a
    /// combination is unsatisfiable when no single piece of it is. A formula naming
    /// more than [`MAX_CFG_ATOMS`] distinct flags is answered `false` — unable to
    /// prove test-only — rather than paying for `2^n` assignments.
    fn requires_test(&self) -> bool {
        let mut atoms = Vec::new();
        self.atoms(&mut atoms);
        atoms.sort();
        atoms.dedup();
        if atoms.len() > MAX_CFG_ATOMS {
            return false;
        }
        let assignments = 1usize << atoms.len();
        (0..assignments).all(|mask| {
            let true_atoms: std::collections::HashSet<&str> = atoms
                .iter()
                .enumerate()
                .filter_map(|(index, name)| (mask & (1 << index) != 0).then_some(name.as_str()))
                .collect();
            !self.eval(false, &true_atoms)
        })
    }

    /// Collects the distinct [`Cfg::Atom`] names reachable from this formula into
    /// `out`, for [`Cfg::requires_test`]'s enumeration.
    fn atoms(&self, out: &mut Vec<String>) {
        match self {
            Self::Test => {}
            Self::Atom(name) => out.push(name.clone()),
            Self::All(children) | Self::Any(children) => {
                for child in children {
                    child.atoms(out);
                }
            }
            Self::Not(inner) => inner.atoms(out),
        }
    }

    /// This formula's truth value under `test` and one assignment of atom names to
    /// `true` (every name absent from `true_atoms` is `false`).
    fn eval(&self, test: bool, true_atoms: &std::collections::HashSet<&str>) -> bool {
        match self {
            Self::Test => test,
            Self::Atom(name) => true_atoms.contains(name.as_str()),
            Self::All(children) => children.iter().all(|child| child.eval(test, true_atoms)),
            Self::Any(children) => children.iter().any(|child| child.eval(test, true_atoms)),
            Self::Not(inner) => !inner.eval(test, true_atoms),
        }
    }
}

/// Renders `tokens` as [`Cfg::Atom`]'s own identity: the same whitespace- and
/// raw-marker-free text [`attribute_text`] already uses, so two occurrences of the
/// identical flag — spelled identically — compare equal.
fn cfg_atom_text(tokens: impl quote::ToTokens) -> String {
    unraw_tokens(tokens.to_token_stream())
        .to_string()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

/// Parses one already-unwrapped `cfg` predicate — the argument of a `cfg(..)`, or one
/// operand of an `all(..)`/`any(..)`/`not(..)` — into a [`Cfg`]. A list that fails to
/// parse becomes a single [`Cfg::Atom`] of its own unparsed tokens rather than an empty
/// list, so a malformed `all(..)`/`any(..)` is at least as opaque as an ordinary flag
/// this scan cannot evaluate, never mistaken for an empty one.
fn parse_cfg_meta(meta: &syn::Meta) -> Cfg {
    match meta {
        syn::Meta::Path(path) if path_is_ident(path, "test") => Cfg::Test,
        syn::Meta::List(list) if path_is_ident(&list.path, "all") => Cfg::All(parse_cfg_list(list)),
        syn::Meta::List(list) if path_is_ident(&list.path, "any") => Cfg::Any(parse_cfg_list(list)),
        syn::Meta::List(list) if path_is_ident(&list.path, "not") => {
            Cfg::Not(Box::new(list.parse_args::<syn::Meta>().map_or_else(
                |_| Cfg::Atom(cfg_atom_text(list)),
                |inner| parse_cfg_meta(&inner),
            )))
        }
        _ => Cfg::Atom(cfg_atom_text(meta)),
    }
}

/// [`parse_cfg_meta`]'s helper over one `all(..)`/`any(..)`'s own comma-separated
/// argument list.
fn parse_cfg_list(list: &syn::MetaList) -> Vec<Cfg> {
    list.parse_args_with(syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        .map_or_else(
            |_| vec![Cfg::Atom(cfg_atom_text(list))],
            |metas| metas.iter().map(parse_cfg_meta).collect(),
        )
}

/// Parses one whole attribute's [`syn::Meta`] — its own path included, so a
/// `#[cfg(..)]` and a `#[cfg_attr(.., ..)]` are told apart here rather than by the
/// caller — into the [`Cfg`] formula that must hold for the item carrying it to
/// survive, considering only this one attribute. [`None`] means the attribute is
/// unrelated to `cfg` entirely, which [`has_cfg_test`] drops rather than folding in a
/// formula that could never affect the combination's satisfiability.
///
/// A `cfg(P)` survives exactly when `P` does, so its formula is `P` itself, parsed with
/// [`parse_cfg_meta`]. A `cfg_attr(condition, injected..)` requires `test` exactly when
/// its own expansion does: rustc replaces the whole attribute with every `injected`
/// item when `condition` holds, and removes it entirely otherwise — so the item's
/// presence, considering only this one attribute, is `!condition || (every injected
/// item's own presence)`, which is [`Cfg::Any`] of the negated condition and an
/// [`Cfg::All`] over each injected item's own recursively parsed formula (an injected
/// item unrelated to `cfg` contributes nothing, exactly as at the top level). Recursing
/// through this same function rather than assuming an injected item is a bare
/// `cfg(..)` is what lets `cfg_attr(.., cfg_attr(.., cfg(test)))` chain arbitrarily
/// deep, the same reach `attr_introduces_cfg`/`meta_introduces_cfg` already give a
/// *reached* `cfg`.
fn attribute_cfg(meta: &syn::Meta) -> Option<Cfg> {
    match meta {
        syn::Meta::List(list) if path_is_ident(&list.path, "cfg") => {
            Some(list.parse_args::<syn::Meta>().map_or_else(
                |_| Cfg::Atom(cfg_atom_text(list)),
                |inner| parse_cfg_meta(&inner),
            ))
        }
        syn::Meta::List(list) if path_is_ident(&list.path, "cfg_attr") => Some(
            list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .map_or_else(
                |_| Cfg::Atom(cfg_atom_text(list)),
                |metas| {
                    let mut rest = metas.iter();
                    let condition = rest
                        .next()
                        .map_or_else(|| Cfg::Atom(cfg_atom_text(list)), parse_cfg_meta);
                    let injected: Vec<Cfg> = rest.filter_map(attribute_cfg).collect();
                    Cfg::Any(vec![Cfg::Not(Box::new(condition)), Cfg::All(injected)])
                },
            ),
        ),
        _ => None,
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

/// Render a path the same whitespace-free, raw-marker-stripped way [`attribute_text`]
/// renders a whole attribute, for naming a `#[derive(..)]` entry in a violation message.
fn path_text(path: &syn::Path) -> String {
    unraw_tokens(path.to_token_stream())
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

/// Every `use` binding and `type` alias declared by `items`, recursing into inline modules.
///
/// A `use` alias and a `type` alias resolve the same way: both map a local name to the path
/// it stands for, so `use Sealable as S;` followed by `S { .. }` and
/// `type Unchecked<'a> = CheckedDispatch<'a>;` followed by `Unchecked { .. }` are one shape
/// to every caller that resolves through this list — `struct_literal_counts`,
/// `resolved_path_uses` and `future_trait_implementors` all do. Codex found the type-alias
/// gap on a third round of review of issue #92's construction-site pin: a `type` alias
/// forwarded to `CheckedDispatch`, and a literal spelled through the alias's name was
/// invisible to a scan that resolved only `use` bindings.
///
/// A `type` alias's right-hand side counts only when it is a plain type path — generics on
/// either side are not part of a struct literal's path and are dropped. A right-hand side
/// that is not a type path (a tuple, a reference, a trait object, a qualified
/// `<T as Trait>::Type`) introduces no alias: there is no single final segment for a struct
/// literal to be counted against.
///
/// `items` need not be a whole file's items: [`struct_literal_counts`] calls this once more
/// per block, over the items declared directly in that block's own statements, to resolve a
/// function-local alias in the scope it is actually visible in (issue #92, Codex's fifth
/// round: a `type` alias declared inside a function body is invisible to a scan that walks
/// only file items and inline modules).
fn collect_item_aliases<'a>(
    items: impl IntoIterator<Item = &'a syn::Item>,
    prefix: &mut Vec<String>,
    aliases: &mut Vec<UseAlias>,
) {
    for item in items {
        // A `#[cfg(test)]` declaration is not in the shipped code, so it resolves nothing —
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
            syn::Item::Type(type_item) => {
                if let Some(target) = type_alias_target(&type_item.ty) {
                    aliases.push(UseAlias {
                        local: ident_name(&type_item.ident),
                        target,
                        absolute: false,
                    });
                }
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    collect_item_aliases(nested, prefix, aliases);
                }
            }
            _ => {}
        }
    }
}

/// `ty`'s segments, if `ty` is a plain type path with no `<T as Trait>::` qualifier.
fn type_alias_target(ty: &syn::Type) -> Option<Vec<String>> {
    match ty {
        syn::Type::Path(type_path) if type_path.qself.is_none() => Some(
            type_path
                .path
                .segments
                .iter()
                .map(|segment| ident_name(&segment.ident))
                .collect(),
        ),
        // `(CheckedDispatch<'a>)` is valid Rust — `#[allow(unused_parens)]` even lets it
        // through `-D warnings` — and `syn` keeps the parens as their own node rather than
        // discarding them, so the path underneath is invisible without unwrapping one more
        // layer. `Type::Group` is the same shape, for a macro's own hygiene grouping. Both
        // recurse, so `((CheckedDispatch))` unwraps to the same target in two hops.
        syn::Type::Paren(inner) => type_alias_target(&inner.elem),
        syn::Type::Group(inner) => type_alias_target(&inner.elem),
        _ => None,
    }
}

/// `ty`, or a type it wraps in parens or a macro's hygiene grouping, names an associated-type
/// projection this scanner cannot resolve: `<T as Trait>::Assoc`, `<T>::Assoc` with no `as`,
/// or a bare `T::Assoc` where `T` is one of `type_params` — the alias's own declared generic
/// type parameters.
///
/// The third shape is issue #171's finding 2. `T::Dispatch` has no `qself`: no `<`, no `as`,
/// nothing but an ordinary multi-segment path, so [`type_alias_target`] resolves it to
/// `["T", "Dispatch"]` and a lookup chases `T` as if it were a real local alias — which it is
/// not, so the chain dead-ends at `Dispatch` and never reaches whatever `T::Dispatch` really
/// names. Restricting the match to the alias's *own* parameters is what tells this apart from
/// a module-qualified path to some other, already-defined type (`type Foo =
/// some_module::Bar;`): only a name the alias itself introduced as generic can be bound to
/// anything a caller chooses, the same unresolvable shape as `<T as Trait>::Assoc`. A
/// UFCS-style projection through a *concrete* type (`type Foo = Via::Dispatch;`, where `Via`
/// is some other type implementing an associated-type-bearing trait) is not caught here —
/// telling that apart from an ordinary qualified path needs real name resolution, which is
/// out of scope for a syntactic scanner and left as a residual limit.
fn type_is_qself_projection(ty: &syn::Type, type_params: &[String]) -> bool {
    match ty {
        syn::Type::Path(type_path) => {
            type_path.qself.is_some()
                || type_path.path.segments.len() > 1
                    && type_path.path.segments.first().is_some_and(|first| {
                        type_params
                            .iter()
                            .any(|param| ident_is(&first.ident, param.as_str()))
                    })
        }
        syn::Type::Paren(inner) => type_is_qself_projection(&inner.elem, type_params),
        syn::Type::Group(inner) => type_is_qself_projection(&inner.elem, type_params),
        _ => false,
    }
}

/// Every `type` alias `contents` declares whose right-hand side is an associated-type
/// projection [`syn`] cannot resolve, outside `#[cfg(test)]`.
///
/// At file scope, in an inline module, or inside a function body. See
/// `type_is_qself_projection` for the three shapes a projection can take. Such a
/// projection can name any struct the trait's `impl` chooses — `CheckedDispatch`
/// included — and following it needs type inference `syn` does not have. A plain type alias
/// already resolves a plain path and one wrapped in parens; a projection is the one shape it
/// cannot safely treat as "not an alias" the way it treats a
/// tuple, a reference or a trait object, because unlike those a projection genuinely can
/// resolve to a struct usable in `Name { .. }` position. So this reports the alias's own
/// name instead of silently skipping it, for a caller to refuse the file outright rather
/// than resolve what it cannot see.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn qself_type_alias_names(contents: &str) -> Result<Vec<String>, syn::Error> {
    struct QSelfAliases {
        found: Vec<String>,
    }

    impl<'ast> syn::visit::Visit<'ast> for QSelfAliases {
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

        fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
            let type_params: Vec<String> = node
                .generics
                .type_params()
                .map(|param| ident_name(&param.ident))
                .collect();
            if type_is_qself_projection(&node.ty, &type_params) {
                self.found.push(ident_name(&node.ident));
            }
            syn::visit::visit_item_type(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = QSelfAliases { found: Vec::new() };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// The `use` and `type` aliases `block` declares directly in its own statements — not in a
/// nested block, which gets its own scope when [`struct_literal_counts`]'s visitor reaches it.
/// Chases `name` through the `use`/`type` aliases declared directly in `items` — the
/// function-local declarations of every block enclosing the point a lookup started from
/// (issue #92, Codex's fifth round), accumulated flat rather than as separate per-block
/// scopes, since a block inherits its enclosing scope's aliases in real Rust where a module
/// does not (issue #109 review). Searched from the end, so a more deeply nested block's own
/// declaration shadows a same-named one further out.
///
/// Scoped to a block's own item declarations only: a `mod` block declared inside a function
/// body is not descended into by name the way [`resolve_segments`] does for a file's own
/// sibling modules (issue #169) — not tested, and not needed for the shapes a function-local
/// alias is actually written in.
/// Returns `Some((segments, absolute))` when `name` chains through at least one block-local
/// alias, where `absolute` says whether the chain ended on a `use ::a::b as c;`-style
/// absolute alias (in which case `segments` is fully resolved) or simply ran out of
/// block-local aliases to try next (in which case `segments`' first element may itself be a
/// *module*-level alias, still to be resolved — issue #92, Codex's post-merge review: the
/// caller used to treat a block-local chain's leftover head as final rather than feeding it
/// on to the module resolver, so `type Inner = Outer;` beside a module-level
/// `type Outer = Foo;` left `Inner {}` resolved only as far as `Outer`).
fn resolve_local_alias_chain(items: &[&syn::Item], name: &str) -> Option<(Vec<String>, bool)> {
    // `own_aliases`, not `collect_item_aliases`: a block's own declarations are exactly
    // one scope, the same as a module's, and reading through a `mod` nested in this block
    // would let that inner module's private alias shadow the outer, real one (Codex
    // review) — a bare `S {}` outside `mod hidden { type S = Other; }` still means
    // whatever `S` resolves to in the enclosing block, never `hidden`'s own.
    let aliases = own_aliases(items.iter().copied());
    let mut segments = vec![name.to_owned()];
    let mut resolved_any = false;
    let mut absolute = false;
    let bound = aliases.len().saturating_add(1);
    for _ in 0..=bound {
        let Some(first) = segments.first().cloned() else {
            break;
        };
        let Some(alias) = aliases
            .iter()
            .rev()
            .find(|candidate| candidate.local == first)
        else {
            break;
        };
        let mut resolved = alias.target.clone();
        resolved.extend(segments.drain(1..));
        segments = resolved;
        resolved_any = true;
        // Same short-circuit as `resolve_segments`'s own absolute-alias check: past this
        // point the path names the extern prelude directly, not another block-local name.
        if alias.absolute {
            absolute = true;
            break;
        }
    }
    resolved_any.then_some((segments, absolute))
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
/// on that module's own items, so every scope stays its own. A `type` alias
/// is scoped the same way and resolves the same way a `use` alias does (issue
/// #92, Codex's third round), so it is collected here too.
///
/// Generic over the item source rather than pinned to `&[syn::Item]`, so
/// [`resolve_local_alias_chain`] can hand it a block's own `&syn::Item` references
/// directly — the block-local case has exactly the same non-recursive-into-`mod`
/// requirement a module-level lookup does, and reusing this rather than
/// [`collect_item_aliases`] is what makes that true rather than assumed (Codex review).
fn own_aliases<'a>(items: impl IntoIterator<Item = &'a syn::Item>) -> Vec<UseAlias> {
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
                if let Some(target) = type_alias_target(&type_item.ty) {
                    aliases.push(UseAlias {
                        local: ident_name(&type_item.ident),
                        target,
                        absolute: false,
                    });
                }
            }
            _ => {}
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

/// Every path written in `contents`, with the file's `use` and `type` aliases resolved.
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
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    if path.leading_colon.is_some() {
        return segments;
    }
    resolve_segments_from(segments, stack)
}

/// [`resolve_segments`]'s own resolution loop, over a segment list that is already known to
/// be relative — reused by [`struct_literal_counts`] to continue resolving a block-local
/// alias chain's leftover head against the enclosing module stack, since a name a block's own
/// aliases could not finish resolving may itself be a module-level alias (issue #92, Codex's
/// post-merge review).
fn resolve_segments_from(mut segments: Vec<String>, stack: &[&[syn::Item]]) -> Vec<String> {
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
    for hop in 0..=aliases.len() {
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
                // Round 43: on the very first hop — the path exactly as the source
                // wrote it, never yet substituted through any alias this scan can
                // see — a qualified path (more than one segment) whose own leading
                // segment names nothing in this file's own alias table can still be
                // exactly as aliased as a local one, through a route this per-file
                // scan has no way to read: `extern crate self as dep; pub use
                // core::clone::Clone as C;` at the crate root makes `dep::C` name
                // `Clone` in a reachable sibling this scan visits separately, with
                // no `dep` in that sibling's own alias table at all — trusting the
                // last segment (`C`) read it as an unrelated trait instead. Once a
                // candidate has already been substituted through at least one local
                // alias (`hop > 0`), its remaining segments are the literal path
                // that alias's own `use` target named — a real, fully-written path
                // this scan did see, `core::clone::Clone` included — and continuing
                // to trust *its* last segment is what lets an ordinary `use
                // core::clone::Clone as C;` resolve to `Clone` at all; narrowing
                // this rule to the first hop alone is what keeps that working.
                let unqualified_at_the_source = hop == 0 && stripped.len() > 1;
                if glob_could_have_bound_this || unqualified_at_the_source {
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
/// Found by Codex review of this change (PR #143), round 29. Round 43 found the pinned
/// file's own exemption was wider than it needed to be: skipping *every* top-level
/// shadow there — not only the pinned type's own — meant a second, unrelated
/// declaration at that same top level, sharing a name with something `trait_name`
/// could otherwise mean, was read as if it did not exist. `trait Clone { .. } impl
/// Clone for Recovery { .. }` inside the pinned file itself implements only that local,
/// unrelated trait — round 41's own finding, one scope further in — but with the whole
/// top level unshadowed, the bare `Clone` resolved past the local declaration to the
/// real `core::clone::Clone` and rejected code that never implements it. Only the
/// entry named `pinned_type_name` is now dropped from the pinned file's own shadow
/// list, so every *other* local declaration there — the pinned file is an ordinary
/// module scope for anything it is not the pinned type's own name — still shadows the
/// way round 29 and round 41 already say a local declaration must.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn trait_implementors_for_pinned_type(
    contents: &str,
    trait_name: &str,
    pinned_type_name: &str,
    is_pinned_type_file: bool,
) -> Result<Vec<String>, syn::Error> {
    let file = parse_rust(contents)?;
    let mut aliases = module_scope_aliases(&file.items);
    aliases.extend(
        shadow_aliases_for_local_types(file.items.iter())
            .into_iter()
            .filter(|alias| !is_pinned_type_file || alias.local != pinned_type_name),
    );
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

/// A synthetic, self-referential alias for every struct, enum, union or trait
/// directly declared in `items`, each resolving to [`LOCAL_SHADOWED_TYPE`] rather than
/// to its own name.
///
/// [`trait_implementors_for_pinned_type`] registers this for every scope that is not
/// the pinned type's own file-level declaration site, because a locally declared
/// item's name always resolves to that local declaration before it resolves to
/// anything an enclosing scope or an import could mean by the same identifier — the
/// same rule [`direct_scope_aliases`]'s own alias shadowing already applies to a `use`
/// or `type` alias redeclared in a nested scope (round 24), extended here to an item
/// that registers no alias of its own and so was invisible to that mechanism entirely.
/// A trait declaration is in the same namespace a struct, enum or union name occupies
/// — both are items of the *type* namespace, which is what the sentinel's own name
/// means — so `LOCAL_SHADOWED_TYPE` already says the right thing about either.
///
/// Found by Codex review of this change (PR #143), round 29: a production-reachable
/// child file, or an inline module nested anywhere the module tree reaches, declaring
/// its own unrelated `struct Recovery` and a handwritten, unqualified `impl Clone for
/// Recovery` resolved to the bare name `Recovery` exactly as a genuine implementor of
/// the pinned type would — nothing distinguished "the name `Recovery`, resolved with no
/// alias in play" from "the name `Recovery`, resolved to a *different* declaration of
/// that name local to this very scope". Round 41 found the identical shape on the
/// *trait* side: `trait Clone { fn conjure() -> Self; } impl Clone for Recovery { .. }`
/// is legal Rust whose `Clone` is this local, unrelated trait — Rust resolves an
/// unqualified name to the nearest declaration in scope, and a trait declared right
/// here shadows `core::clone::Clone` for every unqualified reference inside this same
/// scope exactly as a local struct already shadows an imported type — but nothing
/// registered a trait declaration's own name here, so a bare `Clone` resolved as
/// though no local declaration existed and was rejected as implementing the real
/// trait it does not.
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
            syn::Item::Trait(declared) => ident_name(&declared.ident),
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
fn collect_trait_implementors_in_item_body(
    item: &syn::Item,
    aliases: &[UseAlias],
    trait_name: &str,
    shadow_locals: bool,
    implementors: &mut Vec<String>,
) {
    for root in scope_root_blocks_of_item(item) {
        collect_trait_implementors_in_block(root, aliases, trait_name, shadow_locals, implementors);
    }
}

/// The `roots` computation [`collect_trait_implementors_in_item_body`] used to inline
/// before round 42 of Codex review on this change (PR #143) split it out: derive
/// checking (see [`any_unresolved_derive_in_item_body`]) needs exactly the same set of
/// scope-root blocks a handwritten `impl` can hide behind, since a block-local struct,
/// enum or union carrying a derive is buried the identical way a block-local `impl` is.
fn scope_root_blocks_of_item(item: &syn::Item) -> Vec<&syn::Block> {
    let mut roots: Vec<&syn::Block> = Vec::new();
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
    roots
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
/// Checked at module scope, at any depth of inline-module nesting, and — since round 42
/// of Codex review on this change (PR #143) — at any depth of block nesting a function
/// body, a method, a trait's default method, or any of the other scopes
/// `scope_root_blocks_of_item` already finds for Clone-impl detection can carry. Found
/// by Codex review of this change (PR #143), round 40: a procedural derive
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
/// Round 40 scoped this to module-level and inline-module-nested declarations alone,
/// matching `collect_trait_implementors`'s own module-boundary alias threading, and its
/// own doc comment named the gap round 42 closes: "a production function containing
/// `#[derive(Evil)] struct Helper;`" reached neither `declares_item_macro`'s trust of the
/// outer `derive` nor this scan's own module-scoped walk, so a procedural derive on that
/// local item could emit a non-local `impl Clone for crate::recovery::Recovery` with
/// `recovery-surface` still passing. `any_unresolved_derive_in_item_body` closes it by
/// reusing exactly the scope-root blocks a handwritten `impl` can hide behind, and
/// `any_unresolved_derive_in_block` walks one block at a time the way
/// `collect_trait_implementors_in_block` does, so a block-local `use` or `type` alias
/// resolves a nested derive's name under its own lexical scope rather than a sibling
/// block's. An out-of-line child module is still a separate file this function's own
/// caller calls it on again, exactly as `trait_implementors_for_pinned_type` already is.
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

fn any_unresolved_derive_in_scope<'a>(
    items: impl IntoIterator<Item = &'a syn::Item>,
    aliases: &[UseAlias],
) -> bool {
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
        // Round 42: every scope-root block a handwritten `impl` can hide behind —
        // a function or method body, a trait's default method, a const/static
        // initializer, or a field, variant or generic bound's own type — can just
        // as well hide a block-local struct, enum or union carrying a derive this
        // scan has not yet resolved. `scope_root_blocks_of_item` returns an empty
        // list for an item with no such scope (an ordinary `use`, for instance),
        // so this costs nothing beyond the walk every other item already gets.
        if any_unresolved_derive_in_item_body(item, aliases) {
            return true;
        }
    }
    false
}

/// [`any_unresolved_derive_in_scope`]'s descent into `item`'s own scope-root blocks —
/// the derive-checking twin of [`collect_trait_implementors_in_item_body`], sharing its
/// [`scope_root_blocks_of_item`] rather than duplicating the per-item-kind block search.
fn any_unresolved_derive_in_item_body(item: &syn::Item, aliases: &[UseAlias]) -> bool {
    scope_root_blocks_of_item(item)
        .into_iter()
        .any(|root| any_unresolved_derive_in_block(root, aliases))
}

/// Walks one [`syn::Block`] at a time, the derive-checking twin of
/// [`collect_trait_implementors_in_block`]: an item declared directly in this block's own
/// statements extends the ambient alias table for this block and every block nested
/// inside it, but never a sibling block, so a local `use` or `type` alias two blocks over
/// cannot resolve a derive found here. `shadow_locals` is `false` throughout — this
/// function asks only whether a derive name resolves, never whether a local declaration
/// shadows the pinned type, so [`shadow_aliases_for_local_types`]'s `LOCAL_SHADOWED_TYPE`
/// marker has no part to play here.
fn any_unresolved_derive_in_block(block: &syn::Block, aliases: &[UseAlias]) -> bool {
    let direct_items: Vec<&syn::Item> = block
        .stmts
        .iter()
        .filter_map(|stmt| match stmt {
            syn::Stmt::Item(item) => Some(item),
            _ => None,
        })
        .collect();
    let scoped_aliases = extend_with_local_scope(aliases, &direct_items, false);
    if any_unresolved_derive_in_scope(direct_items.iter().copied(), &scoped_aliases) {
        return true;
    }
    direct_child_blocks_of_block(block)
        .into_iter()
        .any(|nested| any_unresolved_derive_in_block(nested, &scoped_aliases))
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
        /// Every enclosing item's own `cfg`, joined with [`Cfg::All`] as the walk
        /// descends and restored on the way back out — [`Cfg::All(Vec::new())`] at the
        /// file's own top level, which [`Cfg::eval`] holds vacuously true regardless of
        /// `test`.
        ///
        /// Found by Codex review of this change (PR #143), round 46: every override
        /// below used to ask `has_cfg_test` of one item's own attributes alone,
        /// discarding what an *enclosing* item's own `cfg` had already narrowed down —
        /// `#[cfg(any(test, feature = "x"))] mod parent { #[cfg(not(feature = "x"))] fn
        /// helper() { evil!(); } }` can never include `helper` in a non-test build, the
        /// two conditions correlating through the shared flag exactly the way
        /// [`has_cfg_test`] itself closed for several attributes on *one* item in round
        /// 43 — but neither `parent`'s nor `helper`'s own condition alone requires
        /// `test`, so a per-node check that compared each in isolation read past both
        /// and reached `evil!()`. This field is what lets a descendant be checked
        /// against everything that has to hold for it to be reached at all, rather than
        /// reducing every level to its own separate boolean.
        enclosing_cfg: Cfg,
    }

    impl MacroVisitor {
        /// Whether `attrs`, combined with [`Self::enclosing_cfg`], can only hold under
        /// `test`.
        fn requires_test_here(&self, attrs: &[syn::Attribute]) -> bool {
            Cfg::All(vec![self.enclosing_cfg.clone(), attrs_cfg(attrs)]).requires_test()
        }

        /// [`Self::requires_test_here`] followed by a descent: when `attrs` combined
        /// with the accumulated enclosing formula is not provably test-only,
        /// [`Self::enclosing_cfg`] is updated to include `attrs` for the span of
        /// `visit` and restored afterward, so a still-deeper descendant is checked
        /// against this item's own `cfg` as well as every one of its ancestors'.
        fn descend_gated<F: FnOnce(&mut Self)>(&mut self, attrs: &[syn::Attribute], visit: F) {
            let combined = Cfg::All(vec![self.enclosing_cfg.clone(), attrs_cfg(attrs)]);
            if combined.requires_test() {
                return;
            }
            let previous = std::mem::replace(&mut self.enclosing_cfg, combined);
            visit(self);
            self.enclosing_cfg = previous;
        }
    }

    impl<'ast> syn::visit::Visit<'ast> for MacroVisitor {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            // Test code is not shipped: `#[cfg(test)]` removes the item, and anything
            // inside it, before any expansion that could reach the real `Recovery` runs.
            self.descend_gated(item_attrs(item), |visitor| {
                syn::visit::visit_item(visitor, item);
            });
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
            self.descend_gated(impl_item_attrs(item), |visitor| {
                syn::visit::visit_impl_item(visitor, item);
            });
        }

        fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
            self.descend_gated(trait_item_attrs(item), |visitor| {
                syn::visit::visit_trait_item(visitor, item);
            });
        }

        // Round 28: the same gap as round 27's, one level of subitem further —
        // `#[cfg(test)] field: imported_type_macro!()` on a production struct's or
        // union's field, a `#[cfg(test)]`-gated enum variant, and a `#[cfg(test)]`
        // foreign item each carry their own gate that the default traversal walks
        // straight past, since `syn::visit::Visit` dispatches each through its own
        // method (`visit_field`, `visit_variant`, `visit_foreign_item`) rather than
        // back through `visit_item` or `visit_impl_item`/`visit_trait_item`.
        fn visit_field(&mut self, field: &'ast syn::Field) {
            self.descend_gated(&field.attrs, |visitor| {
                syn::visit::visit_field(visitor, field);
            });
        }

        fn visit_variant(&mut self, variant: &'ast syn::Variant) {
            self.descend_gated(&variant.attrs, |visitor| {
                syn::visit::visit_variant(visitor, variant);
            });
        }

        fn visit_foreign_item(&mut self, item: &'ast syn::ForeignItem) {
            self.descend_gated(foreign_item_attrs(item), |visitor| {
                syn::visit::visit_foreign_item(visitor, item);
            });
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
            if self.requires_test_here(&node.attrs) {
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
            if self.requires_test_here(&node.attrs) {
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
        enclosing_cfg: Cfg::All(Vec::new()),
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
///
/// Round 45 of Codex review on this change (PR #143) found the reverse gap: this
/// recursion read every injected attribute of a `cfg_attr` without ever asking whether
/// the `cfg_attr`'s own *condition* could hold in a production build at all.
/// `#[cfg_attr(test, Evil)]` on an otherwise ordinary production item only ever injects
/// `Evil` under `cfg(test)` — rustc removes the whole attribute in every other build —
/// so a production build never sees it, yet this function walked into it exactly as it
/// would a condition that might hold in production and reported the item as macro-
/// generated. [`Cfg::requires_test`] is the same predicate [`has_cfg_test`] builds every
/// attribute's formula with, over the condition [`parse_cfg_meta`] parses from `metas`'
/// own first entry: a condition it proves can never hold outside `test` makes the whole
/// `cfg_attr` contribute nothing to a production build, so its injected attributes are
/// skipped rather than read. A condition this scan cannot *prove* test-only — `feature =
/// ".."`, or anything unrecognized — still reads its injected attributes exactly as
/// before, matching the fail-closed default every other use of `Cfg::requires_test`
/// keeps.
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
        if metas
            .first()
            .is_some_and(|condition| parse_cfg_meta(condition).requires_test())
        {
            return false;
        }
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
///
/// Found on this pull request's own merge with `main`: `matches!(decoded,
/// RecordRef::EffectCompleted { .. } | RecordRef::EffectFailed { .. } | ..)` is
/// ordinary, production `waymaker-flash` code (`frame.rs`'s `redeliverable_kind`) whose
/// second argument is a *pattern*, and `Type::Variant { .. }` is how a struct- or
/// tuple-variant pattern spells "match this variant, ignore its fields" — a brace group
/// this scan cannot tell from a block's, since a macro's own tokens carry no such
/// distinction. `matches!`'s whole second argument is a pattern by the macro's own
/// fixed grammar, and pattern grammar has no bare block anywhere in it; a variant
/// pattern's own `{ .. }` can only ever hold `..`, never a statement. [`is_opaque_variant_pattern_brace`]
/// recognizes exactly that minimal shape — a brace immediately qualified by `path::` and
/// containing nothing but `..` — and lets it through without itself counting as hiding
/// an item, though its (empty, by construction) contents are still scanned like any
/// other group. Anything wider — a named field, a nested sub-pattern, a bare
/// unqualified `Foo { .. }` — is still flagged exactly as before: this closes the
/// concrete case a real, shipping pattern needs, not variant-pattern recognition in
/// general.
fn token_stream_hides_a_possible_item(tokens: proc_macro2::TokenStream) -> bool {
    let trees: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    trees.iter().enumerate().any(|(index, tree)| match tree {
        proc_macro2::TokenTree::Group(group) => {
            if group.delimiter() == proc_macro2::Delimiter::Brace
                && !is_opaque_variant_pattern_brace(&trees, index, group)
            {
                return true;
            }
            token_stream_hides_a_possible_item(group.stream())
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

/// Whether the brace group at `trees[index]` is `path::Segment { .. }` — a qualified
/// path immediately followed by a group whose own contents are exactly the rest-pattern
/// `..` and nothing else. Both halves matter: requiring `::` immediately before the
/// identifier rules out a bare keyword that can open a real block directly (`loop {`,
/// `unsafe {`, a lone `{` with nothing before it at all), since no Rust keyword can
/// itself be a path segment before `::`; and requiring the contents to be bare `..`
/// rules out a named field, whose *value* — in a struct literal, though never in a
/// pattern — could still be an arbitrary block expression this function has no business
/// trusting.
fn is_opaque_variant_pattern_brace(
    trees: &[proc_macro2::TokenTree],
    index: usize,
    group: &proc_macro2::Group,
) -> bool {
    let qualified = matches!(
        (trees.get(index.wrapping_sub(1)), trees.get(index.wrapping_sub(2)), trees.get(index.wrapping_sub(3))),
        (
            Some(proc_macro2::TokenTree::Ident(_)),
            Some(proc_macro2::TokenTree::Punct(second_colon)),
            Some(proc_macro2::TokenTree::Punct(first_colon)),
        ) if second_colon.as_char() == ':' && first_colon.as_char() == ':',
    );
    if !qualified {
        return false;
    }
    let contents: Vec<proc_macro2::TokenTree> = group.stream().into_iter().collect();
    matches!(
        contents.as_slice(),
        [proc_macro2::TokenTree::Punct(first), proc_macro2::TokenTree::Punct(second)]
            if first.as_char() == '.' && second.as_char() == '.',
    )
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
///
/// A `cfg_attr` whose own condition [`Cfg::requires_test`] can prove never holds outside
/// `test` contributes nothing: rustc removes the whole attribute, injected `derive` and
/// all, in every build that ships. Found by Codex review of this change (PR #143), round
/// 46: this used to recurse into every injected attribute regardless of the condition,
/// so `#[cfg_attr(test, derive(Clone))]` on `Recovery` read as an unconditional `Clone`
/// derive and `recovery-surface` rejected a crate whose production build never carries
/// one — the same gap round 45 closed for [`meta_is_unresolved_attribute_macro`]'s
/// unrelated scan, left open here. A condition this scan cannot *prove* test-only —
/// `feature = ".."`, or anything unrecognized — still reads its injected attributes
/// exactly as before, matching the fail-closed default every other use of
/// [`Cfg::requires_test`] keeps.
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
    // unless the condition is provably test-only — including one that is itself a
    // `cfg_attr`, which is why this recurses.
    let Ok(metas) = list.parse_args_with(
        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
    ) else {
        return;
    };
    if metas
        .first()
        .is_some_and(|condition| parse_cfg_meta(condition).requires_test())
    {
        return;
    }
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
/// "a fallback guess that happens to read `Debug`".
///
/// Round 45 of Codex review on this change (PR #143) found that `Clone` had been
/// carved out of this same check with an unconditional `name == "Clone"` exemption,
/// reasoning that resolving *to* `Clone` through an alias is the intended detection
/// round 13's own `Klon` test relies on — but `use evil::Clone; #[derive(Clone)] struct
/// Helper;` is exactly round 39's bypass with `Clone` as the shadowed name instead of
/// `Debug`: an explicitly imported procedural derive macro can share the name `Clone`
/// on purpose, and the unconditional exemption trusted it regardless of
/// `locally_rebound`. `Clone` is already the first entry of `DERIVABLE_BUILTIN_TRAITS`,
/// so the separate exemption is redundant rather than load-bearing once removed: the
/// existing `!locally_rebound` guard on the whitelist clause covers the un-rebound case
/// on its own, and round 13's own `Klon` alias case — `locally_rebound` is `true`
/// there — now reports `UNRESOLVED_DERIVE` instead of the literal name `Clone`, which
/// still names the pinned type's own violation (the fail-closed message names `Clone`
/// by text) and still flags an unrelated struct's derive through the same alias as
/// worth a human's review, exactly as an unrelated struct's `MakeClone` or aliased
/// `Debug` already does.
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
                || (!locally_rebound && DERIVABLE_BUILTIN_TRAITS.contains(&name.as_str()));
            if trusted {
                derives.push(name);
            } else {
                derives.push(UNRESOLVED_DERIVE.to_owned());
            }
        }
    }
}

/// Whether `op` rewrites its left operand in place: one of the ten compound-assignment
/// operators (`+=`, `^=`, and the rest), each of which `syn` parses as a `BinOp` on an
/// `Expr::Binary` rather than as an `Expr::Assign` — `ExprAssign` is `=` alone.
const fn is_compound_assign(op: &syn::BinOp) -> bool {
    matches!(
        op,
        syn::BinOp::AddAssign(_)
            | syn::BinOp::SubAssign(_)
            | syn::BinOp::MulAssign(_)
            | syn::BinOp::DivAssign(_)
            | syn::BinOp::RemAssign(_)
            | syn::BinOp::BitXorAssign(_)
            | syn::BinOp::BitAndAssign(_)
            | syn::BinOp::BitOrAssign(_)
            | syn::BinOp::ShlAssign(_)
            | syn::BinOp::ShrAssign(_)
    )
}

/// Whether `pat` is a bare `ref mut` identifier binding — the whole `let` pattern, not one
/// field of a struct pattern.
///
/// `let ref mut slot = x.field;` borrows `x.field`'s place directly through the pattern —
/// no `Expr::Assign`, no `Expr::Reference` and no method call anywhere, so none of
/// [`mutated_field_names`]'s other routes sees it, and there is no struct pattern here for
/// `visit_field_pat` to read either (Codex review, PR #183). `ref` and `mut` are explicit
/// keywords here, not a scrutinee-dependent default binding mode the way a struct pattern's
/// field is, so the check is exact rather than over-broad. A nested correspondence — a tuple
/// pattern's own `ref mut` element matched against a tuple initializer's own element — is a
/// residual limit stated rather than closed, the same standing this file already states for
/// other syntactic scans.
const fn is_bare_ref_mut_ident(pat: &syn::Pat) -> bool {
    matches!(
        pat,
        syn::Pat::Ident(pat_ident) if pat_ident.by_ref.is_some() && pat_ident.mutability.is_some()
    )
}

/// Whether `pat`, or any sub-pattern it contains, binds a name at all.
///
/// Issue #171's finding 1: `syn` sees a pattern's syntax, never the type of the value it
/// matches, and Rust's match ergonomics (RFC 2005) let that type decide what a *bare*
/// binding means. `let Foo { field, .. } = dispatch;` moves or copies `field` when
/// `dispatch` is owned — a read, safe to ignore — but aliases it as `&mut field`'s type
/// when `dispatch: &mut Foo`, with no `ref`, `mut` or `&mut` written anywhere to tell the
/// two apart. An explicit `ref mut` is only the loudest of several spellings that reach a
/// mutable alias; a bare name and a plain `mut` reach it too, under a `&mut` scrutinee,
/// and nothing here can rule that scrutinee out. So every named binding on a guarded
/// field is reported, not only an explicit `ref mut` — the sound answer without type
/// inference, in the same spirit as this family's other over-broad refusals.
///
/// Walked with a nested [`syn::visit::Visit`] rather than matched by hand over every
/// [`syn::Pat`] variant, so a binding nested inside a struct, tuple, tuple-struct, slice
/// or paren pattern is found the same way regardless of how deep it sits — the traversal
/// is `syn`'s own, only the question asked at each identifier is new. A wildcard (`_`)
/// binds no name and is not reported: nothing reaches it to alias or read back.
fn pattern_binds_a_name(pat: &syn::Pat) -> bool {
    struct NamedBinding(bool);

    impl<'ast> syn::visit::Visit<'ast> for NamedBinding {
        fn visit_pat_ident(&mut self, node: &'ast syn::PatIdent) {
            self.0 = true;
            syn::visit::visit_pat_ident(self, node);
        }
    }

    let mut visitor = NamedBinding(false);
    visitor.visit_pat(pat);
    visitor.0
}

/// Every name in `names` that `contents` writes to as a struct field, outside
/// `#[cfg(test)]` — in any of the six ways a field's value can be rewritten in place
/// rather than rebuilt.
///
/// A plain assignment, `x.field = value;`, is one route. A compound assignment —
/// `x.field += value;`, and the other nine arithmetic and bitwise operators with their own
/// `=` — is a second, and a separate one from `syn`'s own point of view: every
/// compound-assignment operator parses as a `BinOp` on an `Expr::Binary`, never as
/// `Expr::Assign`, which is `=` alone. A *destructuring* assignment is a third, and a
/// different kind of gap: `(x.field,) = (value,);` is still an `Expr::Assign`, but its left
/// side is a tuple, an array or a struct literal of places rather than a bare field access,
/// so a field buried inside one is invisible to a walk that only recognises `Expr::Field` at
/// the top. Recursing into each element of a tuple or array, and each field's value in a
/// struct literal, is what finds it — arbitrarily nested, since a tuple can hold another
/// tuple. A `&mut` reference taken to the field is the fourth —
/// `std::mem::swap(&mut x.field, &mut y.field)`, `std::mem::replace(&mut x.field, value)`,
/// and passing the reference to an arbitrary function that takes `&mut T` are all routes to
/// the same rewrite that spell no `=` at all, and every one of them needs a `&mut` to the
/// field first, which is the shape this refuses. A *method* call on the field is the fifth,
/// and the one that needs neither: `x.field.
/// clone_from(&other)` autorefs `&mut x.field` implicitly, with no `&mut` token written
/// anywhere — so every method call on a guarded field is refused outright, since telling a
/// mutating method from a read-only one needs type inference `syn` does not have. A method
/// called on the whole *value* (`x.field()`, an accessor) is unaffected: its receiver is a
/// plain path, not a field access. A named binding in a struct pattern is the sixth:
/// `let Foo { field: ref mut slot, .. } = x;` borrows `field` mutably through the pattern
/// itself, with no assignment, no `&mut` expression and no method call anywhere for the
/// other five routes to see — and so, under match ergonomics, does a bare `field` or a
/// by-value `field: mut slot` once `x` is itself a `&mut` reference, with no `ref`, `mut` or
/// `&mut` written anywhere in the pattern to tell that apart from a harmless move or copy
/// (issue #171's finding 1). `syn` sees the pattern, never `x`'s type, so it cannot rule the
/// reference case out — every named binding on a guarded field is refused, not only an
/// explicit `ref mut`. Only a wildcard (`_`) binds no name and is left alone. A `let ref mut`
/// binding of the field chain itself is the seventh (Codex review, PR #183):
/// `let ref mut slot = x.field;` borrows `x.field` directly through the `let` pattern, with
/// no struct pattern for the sixth route to read and no `=`, `&mut` expression or method call
/// for the first five. `ref` and `mut` are explicit keywords here rather than a
/// scrutinee-dependent default binding mode, so this one check is exact: only a `let` whose
/// whole pattern is one `ref mut` identifier, binding directly to the initializer.
///
/// A name is matched on the field member alone, not on the receiver's type — `syn` sees
/// syntax, not types, so `x.bytes = value` is refused for any `x` once `"bytes"` is in
/// `names`, whatever `x` turns out to be. That is deliberately broader than exact: a
/// coincidental field of the same name elsewhere in the file becomes a review question
/// rather than a silent gap, the same standing `wire-format`'s literal comparison and
/// `effect-scheduled-fields`'s name comparison already have.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn mutated_field_names(contents: &str, names: &[&str]) -> Result<Vec<String>, syn::Error> {
    struct Mutations<'a> {
        names: &'a [&'a str],
        found: Vec<String>,
    }

    impl Mutations<'_> {
        fn note(&mut self, expr: &syn::Expr) {
            note_mutation(self.names, &mut self.found, expr);
        }
    }

    impl<'ast> syn::visit::Visit<'ast> for Mutations<'_> {
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

        fn visit_expr_assign(&mut self, node: &'ast syn::ExprAssign) {
            self.note(&node.left);
            syn::visit::visit_expr_assign(self, node);
        }

        fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
            // `dispatch.intent.request.kind ^= 1;` rewrites `kind` in place, and
            // `visit_expr_assign` above never sees it — see `is_compound_assign`.
            if is_compound_assign(&node.op) {
                self.note(&node.left);
            }
            syn::visit::visit_expr_binary(self, node);
        }

        fn visit_local(&mut self, node: &'ast syn::Local) {
            // `let ref mut slot = x.field;` borrows the initializer's place directly through
            // the whole pattern, with no route above seeing it — see `is_bare_ref_mut_ident`.
            if is_bare_ref_mut_ident(&node.pat)
                && let Some(init) = &node.init
            {
                self.note(&init.expr);
            }
            syn::visit::visit_local(self, node);
        }

        fn visit_expr_reference(&mut self, node: &'ast syn::ExprReference) {
            if node.mutability.is_some() {
                self.note(&node.expr);
            }
            syn::visit::visit_expr_reference(self, node);
        }

        fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
            // `x.field.clone_from(&other)` reassigns `field` through an *implicit* `&mut
            // self` autoref: nothing in the source spells `=` or `&mut`, so this is a third
            // route neither `visit_expr_assign` nor `visit_expr_reference` can see. Which
            // method is called, and whether it really takes `&mut self`, needs type
            // inference `syn` does not have — so every method call on a guarded field is
            // refused, not only the ones a reviewer could confirm are mutating. A method
            // called on the whole *value* (`dispatch.bytes()`, the accessor every legitimate
            // caller already uses) has a receiver that is a plain path, not a field access,
            // so it is unaffected.
            self.note(&node.receiver);
            syn::visit::visit_expr_method_call(self, node);
        }

        fn visit_field_pat(&mut self, node: &'ast syn::FieldPat) {
            // `let Foo { field: ref mut slot, .. } = x;` borrows `field` through the pattern
            // itself, with no route above seeing it — see `pattern_binds_a_name` for why
            // every named binding is reported, not only an explicit `ref mut`.
            if let syn::Member::Named(ident) = &node.member {
                let name = ident_name(ident);
                if self.names.contains(&name.as_str()) && pattern_binds_a_name(&node.pat) {
                    self.found.push(name);
                }
            }
            syn::visit::visit_field_pat(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = Mutations {
        names,
        found: Vec::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// [`mutated_field_names`]'s own `note`: every name in `names` that `expr` names as a field
/// of a chain, a destructuring target, or a parenthesized wrapper of either, pushed to
/// `found`.
///
/// Walks the whole chain of field accesses, not only the outermost one:
/// `dispatch.intent.request.kind = x;` assigns to `kind`, but `intent` and `request` are
/// guarded *ancestors* in the same chain, and rewriting through either is the rewrite this
/// whole family of checks exists to catch (issue #92, Codex's tenth round). A parenthesized
/// ancestor — `(dispatch.intent.request).kind = x;` — is unwrapped rather than stopping the
/// walk (Codex's thirteenth round): `.base` there is an `Expr::Paren`, not the `Expr::Field`
/// a plain `while let` only matched, so `intent` and `request` were invisible to it. Stops at
/// the first expression that is neither a field access nor a paren/group wrapper, which is
/// the root the chain is built on.
///
/// `(dispatch.bytes,) = (replacement,);` is a destructuring assignment: the left side is a
/// tuple, array or struct literal of *places*, each of which can itself be, or contain, a
/// guarded field chain (issue #92, Codex's eleventh round). Recursing into each element/field
/// is what lets the field-chain case above see one nested inside.
fn note_mutation(names: &[&str], found: &mut Vec<String>, expr: &syn::Expr) {
    match expr {
        syn::Expr::Field(_) => {
            let mut current = expr;
            loop {
                match current {
                    syn::Expr::Field(field) => {
                        if let syn::Member::Named(ident) = &field.member {
                            let name = ident_name(ident);
                            if names.contains(&name.as_str()) {
                                found.push(name);
                            }
                        }
                        current = &field.base;
                    }
                    syn::Expr::Paren(paren) => current = &paren.expr,
                    syn::Expr::Group(group) => current = &group.expr,
                    _ => break,
                }
            }
        }
        syn::Expr::Tuple(tuple) => {
            for elem in &tuple.elems {
                note_mutation(names, found, elem);
            }
        }
        syn::Expr::Array(array) => {
            for elem in &array.elems {
                note_mutation(names, found, elem);
            }
        }
        syn::Expr::Struct(strukt) => {
            for field in &strukt.fields {
                note_mutation(names, found, &field.expr);
            }
        }
        syn::Expr::Paren(paren) => note_mutation(names, found, &paren.expr),
        _ => {}
    }
}

/// Whether `contents` invokes any macro at all, outside `#[cfg(test)]`.
///
/// `syn::Visit` treats a macro's token body as opaque — it is exactly the shape a
/// `macro_rules!` definition already has to be refused for, and it is also the shape of
/// a plain *invocation*: `emit!(CheckedDispatch { intent, bytes })`, calling a macro
/// defined anywhere else in the crate, builds the same construction site under tokens no
/// scan built on [`struct_literal_counts`] or [`mutated_field_names`] can read (issue #92,
/// Codex's fifteenth round — a passthrough invocation is the gap a ban on `macro_rules!`
/// alone leaves open, since that ban reads a definition's own identifier and an invocation
/// spells no such thing). `syn::Macro` is the one type every invocation site shares —
/// `ItemMacro`, `StmtMacro`, `ExprMacro`, `TypeMacro` and `PatMacro` each carry one — so a
/// single override of `visit_macro` catches all five, `macro_rules!` included, without
/// naming any of them individually.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn invokes_any_macro(contents: &str) -> Result<bool, syn::Error> {
    struct AnyMacro {
        found: bool,
    }

    impl<'ast> syn::visit::Visit<'ast> for AnyMacro {
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

        fn visit_macro(&mut self, _node: &'ast syn::Macro) {
            self.found = true;
            // The body is an opaque token stream, so there is nothing further to descend
            // into; `syn::visit::visit_macro` would only walk `node.path`, which is not a
            // construction site.
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = AnyMacro { found: false };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// Every attribute name `contents` carries, outside `#[cfg(test)]`, that is not
/// `allowed_attributes` or a `#[derive(..)]` naming only `allowed_derives`.
///
/// Also every name in `allowed_derives` that a `use` in `contents` rebinds.
///
/// Each is returned as the bare name (`"forge"`), for a rejected derive as
/// `"derive(Path)"`, or for a rebound name as `"Clone (rebound by a use)"`. A
/// procedural attribute macro or a custom derive is a macro surface neither
/// [`struct_literal_counts`]'s item walk nor [`invokes_any_macro`]'s `visit_macro` override
/// ever sees: it is not a `syn::Macro` invocation at all, and its expansion runs in its own
/// defining crate, invisible to a scan that only reads this file's *unexpanded* tokens
/// (issue #92 — Codex found this the round after `invokes_any_macro` closed every
/// invocation shape, because an attribute is represented as `syn::Attribute`, a wrapper
/// `visit_macro` is never called for). `#[forge]` on a method, or `#[derive(Forge)]` on a
/// struct, could rewrite a checked body or emit an unchecked construction with nothing here
/// able to read what it expands to. Rather than try to resolve what a name expands to —
/// which needs a compiler, not a scanner, exactly like resolving a qualified associated-type
/// projection does — every attribute is required to be one of a fixed set the compiler
/// itself interprets with no macro behind it at all; a derive is checked further, since
/// `#[derive(A, B)]` can mix an inert compiler derive with a custom one in the same
/// attribute.
///
/// Issue #186 closes two more gaps a fourth review round found.
///
/// First: the scan read an item's own attributes and an `impl` member's, but never a
/// *trait* member's. A trait method's own declaration — its default body included —
/// is a `syn::TraitItem`. An attribute macro there stayed invisible.
///
/// Second: an allowed derive name is only a name. `#[derive(Clone)]` resolves however
/// `Clone` names in scope at that point. `use forge::Anything as Clone;` rebinds that
/// name, the same way it would rebind a type or a value. This scanner cannot resolve
/// what a `use` really targets — the same limit this file's qualified
/// associated-type-projection check already accepts — so it reports any
/// `use` that rebinds an allowed derive's name, whatever it targets. This is the same
/// hard refusal this function already uses for a macro or an attribute it does not
/// recognize.
///
/// # Errors
///
/// Returns [`syn::Error`] when `contents` does not parse as Rust.
pub fn unaudited_attributes(
    contents: &str,
    allowed_attributes: &[&str],
    allowed_derives: &[&str],
) -> Result<Vec<String>, syn::Error> {
    fn note(attrs: &[syn::Attribute], allowed: &[&str], derives: &[&str], found: &mut Vec<String>) {
        for attr in attrs {
            let Some(name) = attr.path().get_ident().map(ident_name) else {
                found.push(path_text(attr.path()));
                continue;
            };
            if name == "derive" {
                match attr.parse_args_with(
                    syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                ) {
                    Ok(paths) => {
                        for path in &paths {
                            let is_allowed = path
                                .get_ident()
                                .is_some_and(|ident| derives.contains(&ident_name(ident).as_str()));
                            if !is_allowed {
                                found.push(format!("derive({})", path_text(path)));
                            }
                        }
                    }
                    Err(_) => found.push("derive(..)".to_string()),
                }
                continue;
            }
            if !allowed.contains(&name.as_str()) {
                found.push(name);
            }
        }
    }

    struct Attrs<'a> {
        allowed: &'a [&'a str],
        derives: &'a [&'a str],
        found: Vec<String>,
    }

    impl<'ast> syn::visit::Visit<'ast> for Attrs<'_> {
        fn visit_item(&mut self, node: &'ast syn::Item) {
            let attrs = item_attrs(node);
            if has_cfg_test(attrs) {
                return;
            }
            note(attrs, self.allowed, self.derives, &mut self.found);
            syn::visit::visit_item(self, node);
        }

        fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
            let attrs = impl_item_attrs(node);
            if has_cfg_test(attrs) {
                return;
            }
            note(attrs, self.allowed, self.derives, &mut self.found);
            syn::visit::visit_impl_item(self, node);
        }

        // Issue #186, finding 1: a trait method's own declaration, default body
        // included, is a `syn::TraitItem`, not a `syn::Item` or a `syn::ImplItem` — an
        // attribute macro there reached neither override above.
        fn visit_trait_item(&mut self, node: &'ast syn::TraitItem) {
            let attrs = trait_item_attrs(node);
            if has_cfg_test(attrs) {
                return;
            }
            note(attrs, self.allowed, self.derives, &mut self.found);
            syn::visit::visit_trait_item(self, node);
        }
    }

    let file = parse_rust(contents)?;
    let mut visitor = Attrs {
        allowed: allowed_attributes,
        derives: allowed_derives,
        found: Vec::new(),
    };
    visitor.visit_file(&file);
    let mut found = visitor.found;
    shadowed_derive_names(&file.items, allowed_derives, &mut found);
    Ok(found)
}

/// Every name in `derives` that a `use` in `items` rebinds, outside `#[cfg(test)]`.
///
/// Pushed onto `found` as `"Name (rebound by a use)"`; or, for a glob import, every
/// name in `derives` at once, as `"Name (glob import may rebind it)"`.
///
/// "Rebound" applies with or without a rename. Without a `use`, `Clone` in scope is
/// the compiler's own derive. `use forge::Clone;` — no `as` — rebinds it exactly as
/// `use forge::Anything as Clone;` would, so both are reported.
///
/// See [`unaudited_attributes`]'s own doc comment for why this does not resolve what
/// a named `use` actually targets. A `type` alias is deliberately not read here the
/// way [`own_aliases`] reads one: it occupies the type namespace, not the macro
/// namespace a derive name is resolved in, so it cannot rebind what `#[derive(..)]`
/// sees. Item order does not matter: real Rust resolves every item in a module
/// together, so a `use` declared after the derive it shadows still shadows it. This
/// recurses into a nested `mod` on the same terms `own_aliases`'s own callers do,
/// even though `effect-protocol`'s own module ban means `effect.rs` itself never has
/// one.
///
/// A glob import (`use forge::*;`) is checked with [`tree_has_glob`] instead of
/// [`collect_tree_aliases`], which contributes no alias for one at all: this scanner
/// cannot see what a glob exports, so it cannot rule out an item named `Clone` among
/// them. Adversarial review of this fix's own first round found the gap: a glob is
/// exactly the shape `trait_implementors`'s own `GLOB_IMPORT_MARKER` already fails
/// closed over for a handwritten `impl` (issue #109), one this function had not
/// reused (issue #186).
fn shadowed_derive_names(items: &[syn::Item], derives: &[&str], found: &mut Vec<String>) {
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Use(use_item) => {
                if tree_has_glob(&use_item.tree) {
                    for derive in derives {
                        found.push(format!("{derive} (glob import may rebind it)"));
                    }
                }
                let mut aliases = Vec::new();
                collect_tree_aliases(
                    &use_item.tree,
                    use_item.leading_colon.is_some(),
                    &mut Vec::new(),
                    &mut aliases,
                );
                for alias in aliases {
                    if derives.contains(&alias.local.as_str()) {
                        found.push(format!("{} (rebound by a use)", alias.local));
                    }
                }
            }
            syn::Item::Mod(module) => {
                if let Some((_, nested)) = module.content.as_ref() {
                    shadowed_derive_names(nested, derives, found);
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
/// resolving the file's `use` and `type` aliases — in total and inside a function body.
///
/// `use Sealable as S;` followed by `S { .. }` counts (issue #99): the literal's path
/// resolves through the alias to the segments of `Sealable`'s import path, so the final
/// segment is `Sealable` whatever the construction site spells. `type Unchecked<'a> =
/// CheckedDispatch<'a>;` followed by `Unchecked { .. }` counts the same way (issue #92,
/// Codex's third round): a `type` alias is resolved exactly like a `use` alias, chased
/// through a chain of either. Items under exactly `#[cfg(test)]` are skipped, structurally —
/// the old textual pipeline blanked them after lexing comments and strings out, and `syn`
/// sees attributes directly (issue #51).
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
        // The function-local `use`/`type` aliases of every block enclosing the current
        // point, flat and cumulative rather than a stack of separate scopes — a block
        // inherits its enclosing scope's aliases in real Rust, unlike a module (issue #92,
        // Codex's fifth round; issue #109 review).
        block_items: Vec<&'ast syn::Item>,
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
            // The enclosing block's own local aliases are not visible inside a module
            // nested within it either — a module inherits nothing from its lexical
            // surroundings, whether that surrounding is another module or a function body
            // (Codex review) — so `block_items` is set aside for the module's own
            // traversal and restored once it is done, the same way `self.stack` is.
            let enclosing_block_items = core::mem::take(&mut self.block_items);
            syn::visit::visit_item_mod(self, node);
            self.block_items = enclosing_block_items;
            if pushed {
                self.stack.pop();
            }
        }

        fn visit_block(&mut self, node: &'ast syn::Block) {
            // A function-local `use` or `type` alias is visible only inside the block
            // that declares it, and inherited by anything nested within it (issue #92,
            // Codex's fifth round) — unlike a module, which never inherits an outer
            // scope's aliases just by being written inside it. `block_items` therefore
            // stays one flat, growing list: this block's own item declarations are
            // appended so they are searched first (and so shadow a same-named one
            // further out — see `resolve_local_alias_chain`), and exactly that many are
            // truncated back off on the way out, restoring the parent's view for a
            // sibling block.
            let own_items: Vec<&'ast syn::Item> = node
                .stmts
                .iter()
                .filter_map(|stmt| match stmt {
                    syn::Stmt::Item(item) => Some(item),
                    _ => None,
                })
                .collect();
            let pushed = own_items.len();
            self.block_items.extend(own_items);
            syn::visit::visit_block(self, node);
            self.block_items.truncate(self.block_items.len() - pushed);
        }

        fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
            // A block-local alias is innermost, so it is tried first — and only for a
            // bare, single-segment path, the only shape a function-local `type`/`use`
            // alias is ever written against; a multi-segment path and module descent stay
            // `resolve_segments`'s own job over the file's item-slice stack.
            let local = (node.path.leading_colon.is_none() && node.path.segments.len() == 1)
                .then(|| node.path.segments.first())
                .flatten()
                .map(|segment| ident_name(&segment.ident))
                .and_then(|first| resolve_local_alias_chain(&self.block_items, &first));
            let resolved = match local {
                // The chain ended on an absolute alias (`use ::a::b as c;`): already fully
                // resolved, the same as `resolve_segments`'s own leading-colon short-circuit.
                Some((segments, true)) => segments,
                // Ran out of block-local aliases: the leftover head may itself be a
                // module-level alias — `resolve_segments_from` is a no-op if it is not.
                Some((segments, false)) => resolve_segments_from(segments, &self.stack),
                None => resolve_segments(&node.path, &self.stack),
            };
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
        block_items: Vec::new(),
        name: name.to_owned(),
        count: 0,
    };
    total.visit_file(&file);

    let mut inside_count = 0_usize;
    for target in inside_targets(&file, &inside) {
        let mut visitor = Literals {
            stack: target.stack().to_vec(),
            block_items: Vec::new(),
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
#[expect(
    clippy::too_many_lines,
    reason = "one already-reviewed per-event dispatch; bundling round 64's persisted \
              non-rendering descendants into `NestedHtmlContext` pushed the `Event::Html` \
              arm three lines past the limit"
)]
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
    // Codex, round 64: per-level non-rendering descendants — see `NestedHtmlContext`.
    let mut non_rendering_descendants: Vec<Vec<String>> = Vec::new();
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
                    &mut NestedHtmlContext {
                        foreign_content: &mut foreign_content,
                        ancestors: &mut ancestors,
                        descendants: &mut non_rendering_descendants,
                    },
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
                    &mut non_rendering_descendants,
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

/// The HTML5 formatting elements: the fixed set whose own end tag runs the
/// "adoption agency algorithm" rather than an ordinary close, when the element is
/// misnested with something opened after it still on the stack (Codex, pull
/// request #138, round 73, "Preserve hidden formatting after adoption-agency
/// closes"). No other element gets this treatment — an ordinary misnested `<div>`
/// or `<span>` just runs HTML5's plain "any other end tag" algorithm, which this
/// scanner's own `descendants`/`ancestors` truncate-through-match logic already
/// models correctly.
const FORMATTING_ELEMENTS: &[&str] = &[
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];

/// Whether `name` is one of [`FORMATTING_ELEMENTS`], case-insensitively.
fn is_formatting_element(name: &str) -> bool {
    FORMATTING_ELEMENTS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

/// The HTML5 "special" category (Codex, pull request #138, round 75, "Require a
/// real furthest block before preserving formatting"): the adoption agency
/// algorithm's furthest-block search only ever promotes a node from this category —
/// an ordinary phrasing/inline descendant like `<span>` or `<mark>` never qualifies,
/// however deeply the misnesting reaches. When no descendant opened after a
/// misnested formatting element is one of these, there is no furthest block at all,
/// and HTML5's own "no furthest block" case simply pops every node from the current
/// one down through the formatting element — the same as an ordinary close, with no
/// clone and nothing preserved past it. `<b hidden><span>ignored</b>All 6 recovery
/// invariants</span>` has `span` as `b`'s only descendant; `span` is not special, so
/// `</b>` closes both `span` and `b` together and the suffix is visible immediately,
/// not latched open until `</span>`.
///
/// Deliberately not [`HTML_BLOCK_TAG_NAMES`]: that list is `CommonMark`'s own type-6
/// block-tag categorization, built for a different question (does a bare tag name
/// start a raw HTML *block*) and neither a subset nor a superset of this one —
/// `legend`, `optgroup`, `option`, `search` and `dialog` are on that list and not in
/// HTML5's special category, while `br`, `button`, `img`, `input`, `object`,
/// `pre`, `script`, `select`, `style`, `template`, `textarea` and several others are
/// special and never appear there.
const SPECIAL_ELEMENTS: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
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
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "keygen",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];

/// Whether `name` is one of [`SPECIAL_ELEMENTS`], case-insensitively.
fn is_special_element(name: &str) -> bool {
    SPECIAL_ELEMENTS
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
    /// Ordinary (non-foreign, non-void, non-self-closing) elements opened directly
    /// under this frame while it is the innermost one and not yet closed — tracked
    /// only to answer "is `<mglyph>`/`<malignmark>` still a genuine *direct* child of
    /// an open `MathML` text integration point" (Codex, pull request #138, round 61,
    /// "Require `mglyph` to be a direct integration-point child"): `<mtext><span>
    /// <mglyph>...` has `mglyph` nested inside `span`, an ordinary HTML element, not
    /// directly inside `mtext` — the adjusted current node is `span`, an HTML
    /// element, so WHATWG's `mglyph`/`malignmark` exception does not apply there,
    /// only when nothing else genuinely open sits between the two. Pushed and popped
    /// the same "close only what actually opened" discipline every other tracked
    /// stack in this module already follows, so `<mtext><b>x</b><mglyph>...` — a
    /// sibling opened and properly closed before `mglyph`, not an ancestor — still
    /// finds it empty and lets the exception apply.
    ordinary_descendants: Vec<String>,
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
///
/// The `encoding` value is decoded for character references before the comparison
/// (Codex, pull request #138, round 76, "Decode the annotation encoding before
/// namespace switching"): a browser resolves an attribute's value — references
/// included — before comparing it against anything, the same way [`anchor_href`]'s
/// own destination already has to be (round 44 on). `encoding="text&#47;html"` names
/// `text/html` exactly as much as the literal spelling does, and comparing the raw,
/// undecoded source text found no match, wrongly leaving `MathML` parsing (and its
/// self-closing-honoring) active over what is really HTML integration-point content.
fn is_html_integration_point(span: &str, name: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "foreignobject" | "desc" => true,
        "annotation-xml" => attribute_value(span, "encoding").is_some_and(|value| {
            let decoded = decode_character_references(value);
            decoded.eq_ignore_ascii_case("text/html")
                || decoded.eq_ignore_ascii_case("application/xhtml+xml")
        }),
        name => is_mathml_text_integration_point(name),
    }
}

/// Whether `name` is one of the two integration points [`is_html_integration_point`]
/// recognizes that belong to the *SVG* namespace specifically — `foreignObject` and
/// `desc` — as opposed to `annotation-xml` (with a matching encoding) or one of
/// `MathML`'s five text integration points, both `MathML`-only.
fn is_svg_integration_point(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "foreignobject" | "desc")
}

/// Whether `foreign_content`'s innermost frame is genuinely being parsed in the SVG
/// namespace, as opposed to `MathML`'s — `false` when nothing is open at all, which
/// is `MathML`-or-neither's own safe default here, since every caller of this
/// already requires `foreign_content` to be non-empty before asking. Classifies by
/// the frame's own tracked name: `svg` and its two SVG-only integration points
/// (`foreignObject`, `desc`) are SVG; a plain `math` root, `mglyph`/`malignmark`
/// (`MathML`-only per WHATWG's own exception), `annotation-xml` and `MathML`'s five
/// text integration points are all `MathML`.
fn innermost_foreign_namespace_is_svg(foreign_content: &[ForeignFrame]) -> bool {
    foreign_content.last().is_some_and(|frame| {
        frame.name.eq_ignore_ascii_case("svg") || is_svg_integration_point(&frame.name)
    })
}

/// Whether `name` is one of `MathML`'s five fixed "text integration points" — `mi`,
/// `mo`, `mn`, `ms`, `mtext` — WHATWG's own term for the elements that admit ordinary
/// HTML content the same way [`is_html_integration_point`] already checks. Factored
/// out so [`track_foreign_content_depth`]'s `mglyph`/`malignmark` exception (Codex,
/// pull request #138, round 60, "Preserve `MathML` parsing for mglyph children") can
/// ask the same question of an already-open [`ForeignFrame`]'s own tracked name, not
/// only a fresh tag's `span`.
fn is_mathml_text_integration_point(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "mi" | "mo" | "mn" | "ms" | "mtext"
    )
}

/// Whether a well-formed opening tag's own markup `span` carries `attribute` at all,
/// with or without a value — [`has_hidden_attribute`]'s generalization to an arbitrary
/// attribute name (Codex, pull request #138, round 60, "Recognize valueless font
/// breakout attributes"): [`is_foreign_breakout_tag`]'s `font` check needs *presence*,
/// not [`attribute_value`], because HTML5's own breakout condition is "carries a
/// `color`, `face` or `size` attribute" full stop — a boolean attribute with no
/// `=value` at all, `<font color>`, still counts, the same as `hidden` does for
/// [`has_hidden_attribute`]. Quote-tracked the same way that function already is, so a
/// name that merely *appears* inside another attribute's own quoted value is never
/// mistaken for a real one.
fn has_attribute(span: &str, attribute: &str) -> bool {
    let bytes = span.as_bytes();
    let attribute_bytes = attribute.as_bytes();
    let mut quote: Option<u8> = None;
    let mut index = 1 + markup_tag_name(span).len();
    while let Some(&byte) = bytes.get(index) {
        match quote {
            Some(open) if byte == open => quote = None,
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None => {
                let spelled_here = bytes
                    .get(index..)
                    .and_then(|rest| rest.get(..attribute_bytes.len()))
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(attribute_bytes));
                if spelled_here {
                    let before_ok = index
                        .checked_sub(1)
                        .and_then(|before| bytes.get(before))
                        .is_none_or(|&byte| byte == b'/' || byte.is_ascii_whitespace());
                    let after_ok = bytes
                        .get(index + attribute_bytes.len())
                        .is_none_or(|&byte| {
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

/// Whether `character` is one of HTML5's own five "ASCII whitespace" bytes — tab, line
/// feed, form feed, carriage return, space — the fixed set its tokenizer's attribute
/// states (and every other whitespace-sensitive state) test against, not Rust's
/// `char::is_whitespace`'s full Unicode notion (Codex, round 70, "Treat only ASCII
/// bytes as HTML attribute whitespace"): a non-breaking space (U+00A0) is Unicode
/// whitespace but not one of these five, so a browser's unquoted-attribute-value state
/// does not end on one — `<a href=tests/spine.rs\u{A0}junk>` has `\u{A0}junk` as part
/// of the value a reader's click follows, not a separator before a second, discarded
/// token. [`anchor_href`] and [`attribute_value`] both use this in place of
/// `char::is_whitespace` everywhere they skip or scan for HTML whitespace, so neither
/// reads past a non-breaking space early nor reports the wrong destination.
const fn is_html_whitespace(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\x0C' | '\r' | ' ')
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
                    .find(|character: char| !is_html_whitespace(character))
                    .map_or(span.len(), |offset| after_name + offset);
                if bytes.get(after_name_whitespace) != Some(&b'=') {
                    index = after_name;
                    continue;
                }
                let after_equals = after_name_whitespace + 1;
                let value_start = span
                    .get(after_equals..)?
                    .find(|character: char| !is_html_whitespace(character))
                    .map_or(span.len(), |offset| after_equals + offset);
                let value_quote = *bytes.get(value_start)?;
                if value_quote != b'"' && value_quote != b'\'' {
                    let end = span
                        .get(value_start..)?
                        .find(|character: char| is_html_whitespace(character) || character == '>')
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
                .any(|attribute| has_attribute(span, attribute)))
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
        let lower = name.to_ascii_lowercase();
        // The innermost frame's own tracked descendants are searched *before* the
        // frame stack itself (Codex, pull request #138, round 72, "Distinguish
        // nested foreign elements from namespace frames"): a same-named element
        // genuinely nested inside a foreign root or integration point is a real,
        // separately open element — HTML5's own open-element stack has a distinct
        // entry for it — and its own close has to unwind *that* entry rather than
        // being read as the enclosing frame's close just because the two share a
        // name. `<svg><svg></svg><script /><text>...` has the inner, ordinary
        // `<svg>` recorded as a descendant of the outer root; checking frame names
        // first found the outer root's own frame by that same name and popped it —
        // ending foreign content two elements early — rather than closing only the
        // inner element and leaving the outer root, and the still-honored
        // self-closing rule that follows from it, genuinely open.
        if let Some(top) = foreign_content.last_mut()
            && let Some(pos) = top
                .ordinary_descendants
                .iter()
                .rposition(|open| *open == lower)
        {
            top.ordinary_descendants.truncate(pos);
            return;
        }
        // A closing tag that mismatches the innermost frame still closes a real,
        // currently open *ancestor* frame — not only the innermost one (Codex,
        // round 63, "Pop through matching foreign-content ancestors"): HTML5's
        // foreign-content end-tag handling searches the whole stack of open
        // elements outward from the current node, and popping a match takes
        // everything nested inside it along, however many frames deep. `<svg>
        // <foreignObject><math></foreignObject></svg><script />...` has
        // `</foreignObject>` mismatch the innermost `math` frame, but `foreignObject`
        // is still a real ancestor frame two levels out — a real browser pops both
        // `math` and `foreignObject` off, and `</svg>` then pops `svg` the same
        // way, leaving foreign content empty for the `<script />` that follows.
        // Checking only `foreign_content.last()` left the stale `math` frame
        // behind, so `<script />` was misread as still inside foreign content and
        // wrongly acknowledged as self-closing rather than the real, unclosed
        // HTML script a browser reads it as.
        if let Some(pos) = foreign_content
            .iter()
            .rposition(|frame| frame.name.eq_ignore_ascii_case(name))
        {
            foreign_content.truncate(pos);
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
    // A self-closing slash is only ever honored by the *innermost* frame's own rule
    // (Codex, round 62, "Honor HTML slashes while tracking direct children"): under
    // an HTML integration point the slash is ignored and the tag opens for real, so
    // returning here unconditionally skipped tracking it as an ordinary descendant.
    // `<math><mtext><span/><mglyph>...` left `span` unrecorded, so `mglyph`'s
    // `ordinary_descendants.is_empty()` direct-child check saw nothing between it and
    // `mtext` and wrongly re-entered MathML.
    if ends_with_self_closing_slash(span) && honors_self_closing_now(foreign_content) {
        return;
    }
    // WHATWG's one named exception to "an HTML integration point's descendants parse
    // under ordinary HTML rules" (Codex, pull request #138, round 60, "Preserve
    // MathML parsing for mglyph children"): a `<mglyph>` or `<malignmark>` opened
    // directly inside a MathML text integration point (`mi`/`mo`/`mn`/`ms`/`mtext`) is
    // itself still processed under the *foreign*-content rules, not the HTML ones the
    // integration point otherwise switches to — so it reopens real MathML parsing for
    // its own descendants, self-closing acknowledged again, rather than leaving the
    // enclosing integration point's frame (and its `honors_self_closing: false`) as
    // the innermost one. `<math><mtext><mglyph><script /></mglyph>All 6 recovery
    // invariants</mtext></math>` keeps the `<script />` bodyless this way; without it,
    // the still-innermost `mtext` frame read the slash as ignored, opening a real,
    // unclosed `<script>` that swallowed everything after it to end of document.
    //
    // Only while genuinely a *direct* child of the integration point, its own
    // `ordinary_descendants` empty (Codex, round 61, "Require `mglyph` to be a
    // direct integration-point child"): `<mtext><span><mglyph>...` has `mglyph`
    // nested inside the ordinary `span`, not directly inside `mtext` — the adjusted
    // current node there is `span`, an HTML element, so the exception does not
    // apply, and the round-60 fix (which read only the innermost *frame*'s own
    // name, blind to any ordinary element opened beneath it) wrongly re-entered
    // MathML anyway.
    let is_mglyph_exception = !honors_self_closing_now(foreign_content)
        && (name.eq_ignore_ascii_case("mglyph") || name.eq_ignore_ascii_case("malignmark"))
        && foreign_content.last().is_some_and(|frame| {
            is_mathml_text_integration_point(&frame.name) && frame.ordinary_descendants.is_empty()
        });
    // Any element being inserted into the SVG or MathML namespace acknowledges its
    // own self-closing flag immediately, whatever the ambient context was a moment
    // before (Codex, round 67, "Skip self-closing foreign roots when tracking
    // namespaces") — unlike an *ordinary* HTML element's self-closing slash, which
    // is only ever honored while parsing already inside foreign content
    // (`honors_self_closing_now`, checked above and everywhere else in this
    // function). `<svg/><script />decision-id headline</script>` has `<svg/>`
    // acknowledged and immediately popped back off before `<script />` is ever
    // reached, returning to plain HTML — where a self-closing slash is *not*
    // honored, so `<script />` opens for real. Pushing a frame for `<svg/>`
    // unconditionally left a stale, still-open `honors_self_closing: true` frame
    // behind, wrongly reading the following `<script />` as bodyless.
    let self_closes_immediately = ends_with_self_closing_slash(span);
    // A foreign root genuinely starts new foreign content only while HTML's own "in
    // body" insertion mode is the one in effect — nothing (top level) or an open
    // HTML integration point, where descendants parse under ordinary HTML rules
    // again (Codex, round 70, "Keep nested foreign roots in the current
    // namespace"): a `<math>` or `<svg>` reached *inside already-open* raw foreign
    // content is not special there at all, and is inserted as an ordinary element
    // of the *current* namespace, the same as any other tag foreign content's own
    // rules do not name. `<svg><math><mtext><script /></mtext></math><text>...`
    // never leaves the SVG namespace a real browser reads it in — `math` and
    // `mtext` are just unrecognized SVG-namespaced elements, self-closing stays
    // honored throughout, and `<script />` is bodyless — but pushing a frame for
    // `math` unconditionally opened a genuinely new (and wrong) MathML root, whose
    // `mtext` was then read as a real integration point switching to HTML rules,
    // leaving `<script />` a real, unclosed script that swallowed the rest of the
    // document.
    if is_mglyph_exception
        || (is_foreign_content_root(name) && !honors_self_closing_now(foreign_content))
    {
        if !self_closes_immediately {
            foreign_content.push(ForeignFrame {
                name: name.to_ascii_lowercase(),
                honors_self_closing: true,
                ordinary_descendants: Vec::new(),
            });
        }
    } else if !foreign_content.is_empty()
        && is_html_integration_point(span, name)
        && is_svg_integration_point(name) == innermost_foreign_namespace_is_svg(foreign_content)
        && honors_self_closing_now(foreign_content)
    {
        // An HTML integration point is only ever real while it is genuinely being
        // inserted into the SVG or MathML namespace (Codex, round 68, "Require a
        // foreign namespace before opening integration frames") — `is_html_integration_point`
        // matches on the tag's own name and attributes alone, blind to whether any
        // foreign root is open at all. `<mtext><mglyph><script
        // />decision-id headline</script></mglyph></mtext>` with no enclosing
        // `<math>` has `mtext` as nothing more than an unrecognized ordinary HTML
        // element — self-closing never honored, `<script />` opening for real and
        // hiding its own body — but opening an integration-point frame for it
        // regardless let the following `mglyph` exception wrongly re-enter
        // "MathML" that was never really open, treating `<script />` as bodyless.
        //
        // Namespace-matched, not merely non-empty (Codex, round 69, "Match
        // integration points to their foreign namespace"): `foreignObject` is an
        // integration point only in SVG, so `<math><foreignObject><script
        // /></foreignObject></math>...` keeps real MathML parsing throughout —
        // `foreignObject` is just an unrecognized MathML-namespaced element there,
        // never switching to HTML rules — and the still-honored self-closing slash
        // on `<script />` leaves it bodyless. Accepting any nonempty foreign stack
        // wrongly opened an integration-point frame for it anyway, switching to
        // HTML rules that read the same `<script />` as a real, unclosed script.
        //
        // Genuinely still in foreign content, not merely somewhere under an open
        // root (Codex, round 72, "Require foreign parsing before opening
        // integration frames"): an integration point switches parsing of its own
        // descendants *from* foreign-content rules *to* HTML ones, which is a
        // transition that can only happen once — an integration-point-shaped tag
        // reached while already inside HTML content (the innermost open frame's
        // own `honors_self_closing` already false) is itself just an ordinary,
        // unrecognized HTML element, not a second switch into HTML rules.
        // `<math><mtext><mtext><mglyph><script />...` has the inner `mtext`,
        // `mglyph` and `script` all parsed as ordinary HTML — the slash on
        // `script` ignored, its body real and hidden — but matching on namespace
        // alone let the inner `mtext` reopen an integration-point frame, and the
        // `mglyph` exception after it wrongly re-entered MathML, exposing the
        // script body the same two-step way round 68's own fix closed for a
        // *plain* HTML ancestor with no enclosing root at all.
        if !self_closes_immediately {
            foreign_content.push(ForeignFrame {
                name: name.to_ascii_lowercase(),
                honors_self_closing: false,
                ordinary_descendants: Vec::new(),
            });
        }
    } else if honors_self_closing_now(foreign_content) {
        // A tag reached here while genuinely still inside foreign content is not a
        // root, an `mglyph`/`malignmark` exception, or a real integration-point
        // switch — it is an ordinary, unrecognized element of the *current*
        // namespace (Codex, round 72, finding 1's open-tag half, closing the other
        // side of "Distinguish nested foreign elements from namespace frames"):
        // self-closing has already returned above, so it stays genuinely open and
        // needs the same record an HTML-mode descendant gets, or its own later
        // close (handled by the closing-tag arm's own descendant search above) has
        // nothing to match and falls through to closing the enclosing root by
        // name instead. Foreign content has no optional-tag or void-element rules
        // of its own — every non-self-closing tag opens for real, `is_void_element`
        // included, unlike the ordinary-HTML branch just below — so this is
        // recorded unconditionally rather than filtered the way that branch is.
        if let Some(top) = foreign_content.last_mut() {
            top.ordinary_descendants.push(name.to_ascii_lowercase());
        }
    } else if !is_void_element(&name.to_ascii_lowercase()) {
        // Otherwise `foreign_content` is empty or the innermost frame is an
        // integration point — an ordinary tag opened under an HTML integration
        // point's "in html content" rules, or at the document's own top level —
        // recorded on the innermost frame alone (Codex, round 61), so a later
        // `mglyph`/`malignmark` can tell it is no longer a direct child.
        if let Some(top) = foreign_content.last_mut() {
            let lower = name.to_ascii_lowercase();
            // Applies its own implicit closes to the frame's own descendant stack
            // first (Codex, round 68, "Drop implicitly closed integration-point
            // descendants"), the same discipline round 66 gave the plain
            // `ancestors`/`descendants` stacks: `<mtext><p><div></div><mglyph>...`
            // has `div` implicitly close the open `p`, so a browser recovers with
            // `mglyph` once again a direct child of `mtext` — but appending `div`
            // without first popping the stale `p` left `ordinary_descendants`
            // nonempty, wrongly failing the `mglyph` exception's direct-child check.
            //
            // Searched and truncated through the whole stack, not only its top
            // (Codex, round 69, "Search through implicitly closed integration
            // descendants") — the same "any other end tag"-shaped gap round 67
            // found in `track_ordinary_ancestor` and round 68 found in the plain
            // `descendants` stack: `<p><span><div>` has `div` implicitly close `p`
            // two levels down, taking the intervening `span` with it, but checking
            // only `.last()` (`span`) never finds the `p` at all.
            if let Some(pos) = top
                .ordinary_descendants
                .iter()
                .rposition(|open| implicitly_closed_by(open, &lower))
            {
                top.ordinary_descendants.truncate(pos);
            }
            top.ordinary_descendants.push(lower);
        }
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
            // An opening tag applies its own implicit closes to the real ancestor
            // stack *before* it is recorded as open (Codex, round 62, "Remove
            // implicitly closed ancestors before reusing them"): otherwise a `<p>` a
            // sibling `<div>` had already closed stayed in `ancestors` as a stale
            // entry, and a later, unrelated opening tag found it there and was
            // misread as closing a real outer `<p>` that no longer existed.
            //
            // Searched and truncated through the whole stack, not only its top
            // (Codex, round 67, "Truncate through implicitly closed ancestors"):
            // HTML5's "close a p element" rule pops everything nested inside the
            // closed `<p>` too, however many ordinary elements deep — `<p><span>
            // <div>` has the `<div>` close the `<p>` and take the intervening
            // `<span>` down with it, but checking only `ancestors.last()` (`span`)
            // never found the `<p>` two levels down at all, leaving both stale.
            if let Some(pos) = ancestors
                .iter()
                .rposition(|open| implicitly_closed_by(open, &name))
            {
                ancestors.truncate(pos);
            }
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
///
/// [`has_attribute`]'s own fixed case (Codex, round 60): the two used to duplicate this
/// same byte-scanning algorithm, one hardcoded to `hidden`'s own length and one
/// parameterized, until `has_attribute`'s own presence check — built for
/// [`is_foreign_breakout_tag`]'s `font` — was recognized as exactly this function
/// generalized rather than a new one.
fn has_hidden_attribute(span: &str) -> bool {
    has_attribute(span, "hidden")
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
///
/// That unwind is gated by `ancestors_are_in_scope`, `next_non_rendering_marker`'s
/// own template-scope check (Codex, pull request #138, round 75, "Keep inline
/// template content isolated from outer ancestors") — round 72 added it there but
/// not here, leaving this function's own copy of the same unwind reachable through
/// `template` regardless: `<template>` content is HTML5's one exception, parsed on
/// its own, wholly separate stack of open elements a real outer ancestor cannot
/// reach in and close, any more than something inside can reach out.
///
/// `text <div><template>ignored</div>decision-id headline</template></div>` has the leading text force every tag through `Event::InlineHtml`, and its stray inner `</div>` matched the real, outer `div` still in `ancestors` and force-closed `template` early, exposing the marker `</template>` should have kept inert.
fn track_non_rendering_html(
    html: &str,
    stack: &mut Vec<String>,
    foreign_content: &mut Vec<ForeignFrame>,
    ancestors: &mut Vec<String>,
    descendants_stack: &mut Vec<Vec<String>>,
) -> bool {
    if html.starts_with("<!--") {
        return true;
    }
    track_foreign_content_depth(html, foreign_content);
    let self_closing_in_foreign_content =
        honors_self_closing_now(foreign_content) && ends_with_self_closing_slash(html);
    // Resynced to `stack`'s own depth on every call, the same way
    // `advance_past_non_rendering` resyncs `NestedHtmlContext::descendants` (Codex,
    // round 65, "Track ordinary descendants in inline hidden markup") — see that
    // field's own doc comment.
    while descendants_stack.len() < stack.len() {
        descendants_stack.push(Vec::new());
    }
    descendants_stack.truncate(stack.len());
    // Cloned rather than borrowed, for the reason `advance_past_non_rendering` now
    // does the same (Codex, pull request #138, round 42, finding 3): `stack` holds
    // owned names since it can carry an arbitrary `hidden`-suppressed one, not only
    // the fixed `&'static str`s, so a borrow of its top would still be live
    // across the `push`/`pop` calls below.
    match stack.last().cloned() {
        Some(top) if non_rendering_element_nests(&top) => {
            let ancestors_are_in_scope = !top.eq_ignore_ascii_case("template");
            let Some(descendants) = descendants_stack.last_mut() else {
                return true;
            };
            if let Some(tag) = opens_non_rendering_element(html) {
                if !self_closing_in_foreign_content {
                    stack.push(tag.to_owned());
                }
            } else if closes_non_rendering_element(html, &top)
                && (descendants.is_empty() || !is_formatting_element(&top))
            {
                stack.pop();
            } else if closes_non_rendering_element(html, &top) {
                // A *formatting* element's own end tag does not close it while
                // something opened after it is still open (Codex, pull request
                // #138, round 73, "Preserve hidden formatting after
                // adoption-agency closes") — but the clone HTML5's adoption
                // agency algorithm builds is not left open forever either
                // (Codex, round 74, "Pop adopted formatting content when its
                // block closes"): it is reparented as a new child of the
                // "furthest block" — the outermost element opened between the
                // formatting element and the point of misnesting — so closing
                // *that* element closes the clone right along with it, the
                // same way closing any other element closes whatever is
                // nested inside it. `<b hidden><div>ignored</b></div>All 6
                // recovery invariants` has the clone's own suppression end at
                // `</div>`, not run to end of document.
                //
                // Not every nonempty descendant list has a *real* furthest
                // block, though (Codex, pull request #138, round 75, "Require
                // a real furthest block before preserving formatting"): HTML5
                // only promotes a descendant from [`SPECIAL_ELEMENTS`], the
                // outermost one there is if any qualifies, discarding
                // whatever ordinary, non-special descendants came before it —
                // they were never going to keep the clone alive on their own.
                // When none of them qualify there is no furthest block at
                // all, and the whole misnested run (every tracked descendant
                // included) simply closes here, the same as the unconditional
                // branch above.
                //
                // `top` is left exactly as it was — still `b`, the formatting
                // element's own name, never overwritten with the furthest
                // block's (Codex, round 76, "Keep the adopted formatting
                // clone addressable by its end tag"): an earlier version
                // promoted the furthest block into `top`'s place, which read
                // as though the *furthest block itself* were the hidden
                // thing, and discarded the clone's own tag identity in the
                // process — so a second, legitimate `</b>` closing the clone
                // (nested inside the furthest block, the same way any other
                // descendant closes) matched neither `top` (now the furthest
                // block's own name) nor any tracked descendant (cleared),
                // and was left inert, wrongly keeping content hidden past the
                // point the clone itself closed. `text <b hidden><div>ignored
                // </b>still hidden</b>All 6 recovery invariants</div>` has
                // the clone opened by the first `</b>` still genuinely hidden
                // (`still hidden` is its own text, a child of the clone) —
                // but the *second* `</b>` is the clone's own real close, and
                // everything after it, up to the furthest block's own
                // `</div>`, is ordinary, visible content: `div` was never
                // itself marked `hidden`, only the clone nested inside it
                // was.
                //
                // The furthest block is instead pushed onto `ancestors` —
                // what it genuinely becomes the moment the clone opens: a
                // real, ordinary, currently-open element the document's own
                // stack holds, exactly like anything `track_ordinary_ancestor`
                // already records. Its own end tag then closes the clone (and
                // everything nested inside it) through the *existing*
                // "unwind through a matching real ancestor" branch below —
                // round 57's mechanism, met here rather than reimplemented —
                // and the clone's own end tag closes through the ordinary
                // `top`-matches-`closes_non_rendering_element` branch above,
                // unconditionally, since a fresh clone starts with nothing
                // nested inside it (`descendants` cleared along with the
                // promotion) and so is never itself mistaken for still having
                // *its own* misnested formatting exception to apply.
                if let Some(pos) = descendants.iter().position(|name| is_special_element(name)) {
                    let furthest_block = descendants.remove(pos);
                    descendants.clear();
                    ancestors.push(furthest_block);
                } else {
                    stack.pop();
                }
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
                } else if !self_closing_in_foreign_content && !is_void_element(&next_tag) {
                    // An ordinary element genuinely nested inside `top`, tracked so
                    // its own later close (below) is told apart from an untracked
                    // ancestor's — `next_non_rendering_marker`'s own `descendants`,
                    // met here for a self-contained inline construct (Codex, round
                    // 65, "Track ordinary descendants in inline hidden markup"):
                    // `<span hidden><mark>ignored</mark>decision-id headline</span>`
                    // had this construct's `<mark>` recorded nowhere at all, so its
                    // own `</mark>` later matched neither `top` nor any known
                    // descendant and fell through to the `ancestors` check, where an
                    // *outer* `<mark>` happened to share its name.
                    //
                    // Applies its own implicit closes to the descendant stack first
                    // (Codex, round 66, "Apply implicit closes to tracked hidden
                    // descendants"), met here for a self-contained inline construct —
                    // see `next_non_rendering_marker`'s own twin fix for the reasoning.
                    // Searched and truncated through the whole stack, not only its
                    // top (Codex, round 68, "Truncate through implicitly closed
                    // hidden descendants") — see that fix's own twin for the
                    // reasoning.
                    if let Some(pos) = descendants
                        .iter()
                        .rposition(|open| implicitly_closed_by(open, &next_tag))
                    {
                        descendants.truncate(pos);
                    }
                    descendants.push(next_tag);
                }
            } else if let Some(name) = html
                .starts_with("</")
                .then(|| markup_tag_name(html).to_ascii_lowercase())
            {
                if let Some(pos) = descendants.iter().rposition(|open| *open == name) {
                    // Searched and truncated through, not just checked at the top
                    // (Codex, round 65, "Truncate through matching nested
                    // descendants") — see `next_non_rendering_marker`'s own twin
                    // fix for the reasoning.
                    descendants.truncate(pos);
                } else if ancestors_are_in_scope && ancestors.contains(&name) {
                    // Round 57's "Unwind all elements through a matching ancestor",
                    // met here for a self-contained inline construct rather than a
                    // byte-by-byte walk: a closing tag matching neither `top` nor
                    // anything this construct itself opens, but matching *some*
                    // element genuinely known to be open outside it, force-closes
                    // everything nested inside — the whole tracked stack, not only
                    // `top` — the same "pop everything nested inside a closing
                    // ancestor's end tag" a real HTML5 parser does. Only while
                    // `top` is itself part of that same, real stack (round 75) —
                    // see `ancestors_are_in_scope`'s own doc comment above.
                    stack.clear();
                    descendants.clear();
                    while ancestors.pop().as_deref() != Some(name.as_str()) {}
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
///
/// HTML5's own five-byte ASCII whitespace set, not [`char::is_whitespace`] (Codex, pull
/// request #138, round 74, "Use HTML whitespace when delimiting tag names"): the
/// tokenizer's "tag name state" only ends a name on tab, LF, FF, CR or space — a
/// non-breaking space (U+00A0) is Unicode whitespace but not one of the five, so it
/// stays *part of* the name. `<div\u{A0} hidden>` (the gap after `div` is a non-breaking
/// space followed by a real one) therefore opens an element whose real tag name is
/// `div\u{A0}`, which a later, literal `</div>` never matches — a browser leaves it
/// open, and its content stays hidden until a `</div\u{A0}>` this scanner will almost
/// certainly never see. Reading the name with `char::is_whitespace` stopped at the
/// non-breaking space instead, truncating it to plain `div` — a name the later
/// `</div>` *does* match, wrongly ending the hidden region and exposing the marker
/// after it.
fn markup_tag_name(span: &str) -> &str {
    let rest = span.strip_prefix('<').unwrap_or(span);
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let end = rest
        .find(|character: char| {
            is_html_whitespace(character) || character == '/' || character == '>'
        })
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
    descendants: &mut Vec<String>,
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
    // Whether `ancestors` — the document's own, real element stack, maintained
    // outside `top` entirely — is reachable from inside `top` at all (Codex, pull
    // request #138, round 72, "Respect HTML scope boundaries when unwinding hidden
    // content"). `top` here is either an ordinary element the `hidden` attribute
    // suppressed — ordinary content, fully part of the document's own stack, where
    // a real outer ancestor's end tag legitimately reaches in and closes it exactly
    // as it would any other nested element — or `template`, whose content HTML5
    // parses on its own, wholly separate stack of open elements, starting empty.
    // Only the second creates a real scope boundary: a `<template>` element's own
    // ancestors are not reachable from inside its content, by name or otherwise,
    // any more than something inside could reach out and close one of them.
    // `<div><template>ignored</div>decision-id headline</template></div>` has the
    // inner `</div>` match nothing on template content's own stack — stray, left
    // inert — so `top` stays open until its own literal `</template>`; treating it
    // as reaching the real, outer `div` (as an earlier version did, with no
    // exception for `template`) read that as evidence `top` itself must have
    // closed too, ending the hidden region early and exposing the marker after it.
    let ancestors_are_in_scope = !top.eq_ignore_ascii_case("template");
    let mut cursor = cursor;
    // Ordinary elements opened *during this walk*, nested inside `top` (Codex, pull
    // request #138, round 55, "Unwind hidden descendants when an ancestor closes") —
    // an ordinary `<div>` an untracked ancestor already opened *before* `top` itself
    // never appears here at all, since consuming its own opening tag happened at the
    // top level, before `top` was ever pushed; only a child opened after `top`, while
    // this very walk is what is scanning past it, is recorded. That is exactly the
    // distinction the closing-tag arm below needs to draw. Owned by the caller and
    // resynced to `top`'s own level rather than declared fresh here (Codex, round 64,
    // "Persist hidden descendants across raw-HTML lines") — a child opened on one
    // `Event::Html` line and closed on the next used to be forgotten the moment this
    // function returned, so its own later close fell through to the `ancestors` check
    // below and could be misread as a real outer element genuinely closing instead.
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
    // positive evidence of a real, currently-open outer element — now closes `top`,
    // and only while `ancestors_are_in_scope`; anything else is left inert, matching
    // HTML5's own "any other end tag" algorithm, which ignores a token with no
    // matching open element anywhere on the (in-scope) stack rather than guessing.
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
                // `top` itself may own a namespace change the Hidden/Tag opener
                // pushed (Codex, round 62, "Track namespace changes on hidden
                // openers"), and every other path back out of a tracked frame
                // already calls this on its own closing tag — the safe no-op this
                // is when `top` never pushed one, so the frame it did push does
                // not otherwise outlive the element that opened it.
                //
                // Never a *formatting* element's own misnested close here (Codex,
                // pull request #138, round 73, "Preserve hidden formatting after
                // adoption-agency closes"): `top` reaches this walk only by being
                // one of the fixed non-rendering elements, or an arbitrary
                // `hidden`-suppressed element recognized while `stack` was
                // otherwise empty — either way, the line that opened it had to be
                // one `pulldown-cmark` itself classifies as an HTML *block*
                // (CommonMark's fixed "type 6" tag-name list), and no formatting
                // element (`is_formatting_element`'s own fixed list — `b`, `em`,
                // `i`, and the rest) is ever on it. A nested `<b hidden>` reached
                // *while already scanning* a block-level `top` is tracked here
                // only as an ordinary `descendants` entry, exactly like any other
                // nested tag with no `hidden` of its own — this walk never
                // promotes it to a `top` in its own right, so it is `track_non_rendering_html`'s
                // self-contained-`Event::InlineHtml` twin, not this function,
                // that ever meets a formatting element as `top` and needs the
                // adoption-agency exception; see that function's own fix.
                track_foreign_content_depth(span, foreign_content);
                return Some(NonRenderingAdvance::Close(end));
            }
            if let Some(pos) = descendants.iter().rposition(|open| *open == name) {
                // A genuine nested child, opened after `top` and closed properly —
                // not relevant to `top`'s own state. Searched and truncated through
                // rather than checked only at the top (Codex, round 65, "Truncate
                // through matching nested descendants"): HTML5 pops everything
                // nested inside a closing descendant's own end tag too, however many
                // layers deep, the same "any other end tag" unwind `ancestors`
                // already gets (round 57) — `<div><section></div>` has `</div>` pop
                // both `section` and `div`, and checking only `descendants.last()`
                // (`section`) left the stale `div` behind, where a *later*,
                // unrelated close matching it by name could be misread as a genuine
                // nested child closing when it was really the real outer ancestor
                // sharing that div's name.
                descendants.truncate(pos);
            } else if ancestors_are_in_scope
                && !descendants.contains(&name)
                && ancestors.contains(&name)
            {
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
                //
                // Only while `top` is itself part of that same, real stack (Codex,
                // round 72, "Respect HTML scope boundaries when unwinding hidden
                // content"): `template` content is HTML5's one exception, parsed
                // on its own separate stack a real outer ancestor cannot reach —
                // see `ancestors_are_in_scope`'s own doc comment above.
                while ancestors.pop().as_deref() != Some(name.as_str()) {}
                return Some(NonRenderingAdvance::Close(end));
            }
            // Otherwise `name` matches nothing this scanner has positive evidence
            // is genuinely open — an out-of-order close inside a genuine nested
            // child, or a wholly unmatched stray tag — and is left inert, the same
            // as HTML5's own tokenizer does for an end tag with no matching open
            // element anywhere on the (in-scope) stack.
        } else if implicitly_closed_by(top, &name) {
            return Some(NonRenderingAdvance::Close(start));
        } else if ancestors_are_in_scope
            && let Some(closed_ancestor) = ancestors
                .iter()
                .rev()
                .find(|ancestor| implicitly_closed_by(ancestor, &name))
                .cloned()
        {
            // An opening tag can implicitly close an *ancestor* genuinely open
            // outside `top`, not only `top` itself (Codex, pull request #138,
            // round 61, "Apply implicit closes through hidden descendants"):
            // `<p><span hidden>ignored<div>All 6 recovery invariants</div>` has
            // `div` implicitly close the ancestor `p` — HTML5's own "close a p
            // element" rule — which pops everything nested inside `p`, the
            // hidden `span` included, off the stack right along with it. Checking
            // only `implicitly_closed_by(top, &name)` missed this, since `span`
            // has no optional-tag rules of its own naming `div`, so the incoming
            // tag was recorded as an ordinary nested child instead, latching the
            // hidden stack open through the ancestor's own implicit close. The
            // search walks `ancestors` innermost first — the same "nearest open
            // instance" a real stack-based algorithm finds — and truncates
            // through the match the same way an *explicit* ancestor close
            // already does (round 57). Gated by `ancestors_are_in_scope` for the
            // same reason the explicit close above is (round 72): an opening tag
            // inside `template` content cannot reach out and implicitly close a
            // real ancestor of the `<template>` element any more than an
            // explicit end tag inside it can.
            while ancestors.pop().as_deref() != Some(closed_ancestor.as_str()) {}
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
        } else if !(is_void_element(&name)
            || ends_with_self_closing_slash(span) && honors_self_closing_now(foreign_content))
        {
            // An ordinary element genuinely nested inside `top`, opened during this
            // same walk — tracked so its own later close (above) is told apart from
            // an untracked ancestor's. A self-closing slash is skipped here only
            // when the active namespace actually honors it (Codex, round 65,
            // "Record slash-terminated HTML descendants"): ordinary HTML ignores
            // the slash, so `<section/>` inside a hidden `top` with no foreign
            // content open still opens for real and needs its own close tracked —
            // skipping it unconditionally left that close unmatched by anything,
            // falling through to the `ancestors` check where an outer element could
            // share its name and be mistaken for it.
            //
            // Applies its own implicit closes to the descendant stack first (Codex,
            // round 66, "Apply implicit closes to tracked hidden descendants"), the
            // same discipline `track_ordinary_ancestor` already applies to
            // `ancestors` (round 62): `<span hidden><p><div></p></div>...` has the
            // inner `<div>` implicitly close the open `<p>` — HTML5's own "close a p
            // element" rule — so a browser recovers from the stray `</p>` that
            // follows without it affecting anything, and only the later `</div>`
            // closes the real, surviving `div`. Appending `div` without first
            // popping the stale `p` left both on the stack, so the stray `</p>`
            // matched `p` and, via `rposition`'s search, truncated the *real* `div`
            // away with it — leaving the following `</div>` to match nothing here
            // and fall through to `ancestors`, where the outer element happened to
            // share its name.
            //
            // Searched and truncated through the whole stack, not only its top
            // (Codex, round 68, "Truncate through implicitly closed hidden
            // descendants"): the same "any other end tag"-shaped gap round 67 found
            // in `track_ordinary_ancestor` — `<p><span><div>` has `div` implicitly
            // close `p` two levels down, taking the intervening `span` with it, but
            // checking only `descendants.last()` (`span`) never found the `p` at
            // all, leaving both stale for a later stray close to wrongly match.
            if let Some(pos) = descendants
                .iter()
                .rposition(|open| implicitly_closed_by(open, &name))
            {
                descendants.truncate(pos);
            }
            descendants.push(name);
        }
        // Not relevant to `top` — carry its own namespace effect forward (a no-op for
        // anything that is not a foreign-content root or an HTML integration point)
        // and keep walking.
        track_foreign_content_depth(span, foreign_content);
        cursor = end;
    }
}

/// `foreign_content`, `ancestors` and `descendants` bundled into one parameter, only to
/// keep [`advance_past_non_rendering`] (and, since round 64, [`visible_html_ranges`])
/// under clippy's parameter-count limit (Codex, pull request #138, round 56) — the
/// three are otherwise independent state, each documented at its own declaration site
/// (`track_foreign_content_depth`, `track_ordinary_ancestor`,
/// [`next_non_rendering_marker`]'s own doc comment).
struct NestedHtmlContext<'a> {
    foreign_content: &'a mut Vec<ForeignFrame>,
    ancestors: &'a mut Vec<String>,
    /// One entry per currently open non-rendering level (`open_non_rendering`'s own
    /// depth), each the ordinary elements opened directly under *that* level — the
    /// per-line-call-local `descendants` [`next_non_rendering_marker`] used to declare
    /// for itself, persisted across `Event::Html` lines the same way `foreign_content`
    /// and `ancestors` already are (Codex, round 64, "Persist hidden descendants
    /// across raw-HTML lines"): `<div><span hidden><div>` split across a line break
    /// from its own `</div>decision-id headline</span></div>` had the inner `<div>`'s
    /// own open forgotten the moment its line's call returned, so the closing `</div>`
    /// on the next line found no evidence it was a real nested child and fell through
    /// to `ancestors`, where the *outer* `<div>` happened to share its name — reading
    /// a nested child's own close as the real outer ancestor's, and ending the hidden
    /// `span` two levels early. Resynced to `open_non_rendering`'s current length at
    /// the top of every [`advance_past_non_rendering`] call rather than mirrored at
    /// every individual push/pop site: a level newly opened (by this scan or by an
    /// interleaved `Event::InlineHtml` construct sharing the same stack) starts with
    /// no known descendants, which is the same safe default an empty local `descendants`
    /// already was, and a level that has closed is simply not read again.
    descendants: &'a mut Vec<Vec<String>>,
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
    // Resynced to `stack`'s own depth on every call rather than mirrored at every
    // individual push/pop site (Codex, round 64, "Persist hidden descendants across
    // raw-HTML lines") — a level `stack` gained since the last call (whether pushed by
    // this same scan or by an interleaved `Event::InlineHtml` construct sharing the
    // stack) starts with no known descendants, the same safe default a fresh local
    // `Vec::new()` already was, and a level `stack` lost is simply dropped.
    while context.descendants.len() < stack.len() {
        context.descendants.push(Vec::new());
    }
    context.descendants.truncate(stack.len());
    let descendants = context.descendants.last_mut()?;
    match next_non_rendering_marker(
        line,
        cursor,
        &top,
        context.foreign_content,
        context.ancestors,
        descendants,
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
    context: &mut NestedHtmlContext<'_>,
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
        match resolve_pending_tag(line, open_non_rendering, context.foreign_content, pending) {
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
                    foreign_content: context.foreign_content,
                    ancestors: context.ancestors,
                    descendants: context.descendants,
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
                foreign_content: context.foreign_content,
                ancestors: context.ancestors,
                descendants: context.descendants,
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
            // The hidden opener's own namespace matters to what is scanned beneath
            // it (Codex, round 62, "Track namespace changes on hidden openers"):
            // `<svg hidden>` never reached `track_foreign_content_depth` before, so
            // a self-closing `<script />` inside it found an empty foreign-content
            // stack, read its own slash as unhonored HTML, and opened as a real,
            // unclosed script that swallowed the rest of the document.
            track_foreign_content_depth(&line[start..end], nested.foreign_content);
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
                    .find(|character: char| !is_html_whitespace(character))
                    .map_or(html.len(), |offset| after_name + offset);
                if bytes.get(after_name_whitespace) != Some(&b'=') {
                    index = after_name;
                    continue;
                }
                let after_equals = after_name_whitespace + 1;
                let value_start = html
                    .get(after_equals..)?
                    .find(|character: char| !is_html_whitespace(character))
                    .map_or(html.len(), |offset| after_equals + offset);
                let value_quote = *bytes.get(value_start)?;
                if value_quote != b'"' && value_quote != b'\'' {
                    // An unquoted HTML attribute value (Codex, pull request #138, round
                    // 38, finding 2): `<a href=tests/spine.rs>recovery proof</a>` is
                    // real, valid HTML — a browser follows it exactly as it would a
                    // quoted `href` — so `href=tests/spine.rs` must not be read as an
                    // empty or absent value. It runs to the next HTML whitespace or the
                    // tag's own closing `>`, whichever comes first; neither character is
                    // legal inside an unquoted value. HTML whitespace specifically
                    // (Codex, round 70, "Treat only ASCII bytes as HTML attribute
                    // whitespace"), not `char::is_whitespace`'s full Unicode notion — see
                    // `is_html_whitespace`'s own doc comment.
                    let end = html
                        .get(value_start..)?
                        .find(|character: char| is_html_whitespace(character) || character == '>')
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
    // One entry per currently open non-rendering level, carried across `Event::Html`
    // lines the same way `foreign_content` and `ancestors` already are (Codex, round
    // 64, "Persist hidden descendants across raw-HTML lines") — see
    // `NestedHtmlContext`'s own doc comment.
    let mut non_rendering_descendants: Vec<Vec<String>> = Vec::new();
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
                    &mut non_rendering_descendants,
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
                &mut non_rendering_descendants,
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
#[expect(
    clippy::too_many_arguments,
    reason = "one flag per piece of state visible_source already carries across lines \
              for its own top-level comment search; bundling them loses the ability to \
              read each mutation at its own call site"
)]
fn hide_non_rendering_in_inline_html(
    html: &str,
    range: std::ops::Range<usize>,
    open_non_rendering: &mut Vec<String>,
    non_rendering_start: &mut Option<usize>,
    foreign_content: &mut Vec<ForeignFrame>,
    ancestors: &mut Vec<String>,
    descendants_stack: &mut Vec<Vec<String>>,
    hidden: &mut Vec<(usize, usize)>,
) {
    let was_open = !open_non_rendering.is_empty();
    track_non_rendering_html(
        html,
        open_non_rendering,
        foreign_content,
        ancestors,
        descendants_stack,
    );
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
    non_rendering_descendants: &mut Vec<Vec<String>>,
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
                    descendants: non_rendering_descendants,
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
                track_foreign_content_depth(&html[start..end], foreign_content);
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
    // One entry per currently open non-rendering level (Codex, round 64, "Persist
    // hidden descendants across raw-HTML lines") — see `NestedHtmlContext`'s own doc
    // comment.
    let mut non_rendering_descendants: Vec<Vec<String>> = Vec::new();

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
                    &mut NestedHtmlContext {
                        foreign_content: &mut foreign_content,
                        ancestors: &mut ancestors,
                        descendants: &mut non_rendering_descendants,
                    },
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
                    &mut non_rendering_descendants,
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
    descendants_stack: &mut Vec<Vec<String>>,
    hidden_markup_seen: &mut bool,
) -> bool {
    let opened = track_non_rendering_html(
        html,
        open_non_rendering_tag,
        foreign_content,
        ancestors,
        descendants_stack,
    );
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
    // One entry per currently open non-rendering level (Codex, round 64, "Persist
    // hidden descendants across raw-HTML lines") — see `NestedHtmlContext`'s own doc
    // comment.
    let mut non_rendering_descendants: Vec<Vec<String>> = Vec::new();
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
                    &mut NestedHtmlContext {
                        foreign_content: &mut foreign_content,
                        ancestors: &mut ancestors,
                        descendants: &mut non_rendering_descendants,
                    },
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
                    &mut non_rendering_descendants,
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
#[expect(
    clippy::too_many_lines,
    reason = "one already-reviewed per-event dispatch; bundling round 64's persisted \
              non-rendering descendants into `NestedHtmlContext` pushed the `Event::Html` \
              arm three lines past the limit"
)]
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
    // Codex, round 64: per-level non-rendering descendants — see `NestedHtmlContext`.
    let mut non_rendering_descendants: Vec<Vec<String>> = Vec::new();

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
                    &mut NestedHtmlContext {
                        foreign_content: &mut foreign_content,
                        ancestors: &mut ancestors,
                        descendants: &mut non_rendering_descendants,
                    },
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
                    &mut non_rendering_descendants,
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
mod tests {
    use super::{anchor_href, attribute_value};

    // Codex, pull request #138, round 70, finding "Treat only ASCII bytes as HTML
    // attribute whitespace": every other behavior this module is responsible for is
    // exercised indirectly, through a `docs.rs` check that reads a full document —
    // but the check that consumes an unquoted `href` value only ever tests whether a
    // *substring* of it is present (`table_rows`'s own callers all read a table row
    // with `str::contains`, not equality), which cannot observe a value that grew
    // *longer* than the correct one: a string that starts with the required substring
    // still contains it however much more text is appended after it, so no
    // downstream check can tell an unquoted value that stopped at a non-breaking
    // space apart from one that correctly kept reading past it. Tested directly here
    // instead, against the one thing that changed: what the function itself returns.
    //
    // A non-breaking space (U+00A0) is Unicode whitespace but not one of HTML5's own
    // five ASCII whitespace bytes (tab, line feed, form feed, carriage return, space)
    // — the fixed set its tokenizer's unquoted-attribute-value state actually ends
    // on — so a browser reads `\u{A0}junk` as part of the value, not a separator
    // before a second, discarded token. `<a href=tests/spine.rs\u{A0}junk>proof</a>`
    // has the real destination a reader's click follows as the whole
    // `tests/spine.rs\u{A0}junk`; reporting `tests/spine.rs` alone would tell a
    // caller the link points where it does not.
    #[test]
    fn anchor_href_keeps_text_past_a_non_breaking_space_in_an_unquoted_value() {
        let html = "<a href=tests/spine.rs\u{A0}junk>recovery proof</a>";
        assert_eq!(anchor_href(html), Some("tests/spine.rs\u{A0}junk"));
    }

    // `attribute_value` is `anchor_href`'s own generalization to an arbitrary
    // attribute name (used to read `<annotation-xml>`'s `encoding`), built from the
    // same three whitespace-scanning sites — fixed the same way, for the same
    // reason.
    #[test]
    fn attribute_value_keeps_text_past_a_non_breaking_space_in_an_unquoted_value() {
        let span = "<annotation-xml encoding=text/html\u{A0}junk>";
        assert_eq!(
            attribute_value(span, "encoding"),
            Some("text/html\u{A0}junk")
        );
    }
}

#[cfg(test)]
mod raw_identifier_tests {
    //! A raw identifier and its plain spelling name the same item (issue #90).
    //! `extern_crate_names` already strips the `r#` marker. This module tests
    //! every other parser in this file against the same rule.
    use super::{
        FnScope, child_modules, declares_test, fn_declaration_count, future_trait_implementors,
        inner_attributes, mutated_field_names, name_uses, qself_type_alias_names,
        resolved_path_uses, struct_literal_counts, trait_impls, use_aliases,
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
    fn a_type_alias_resolves_to_the_type_it_stands_for() {
        // Codex, issue #92's third round: a `type` alias must resolve the same way a `use`
        // alias does, or `Unchecked { .. }` hides a `CheckedDispatch` construction from a
        // scan that only chased `use` bindings.
        let counts = struct_literal_counts(
            "type Unchecked<'a> = Foo<'a>; fn forge() -> Unchecked<'static> { Unchecked {} }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_chained_type_alias_still_resolves() {
        // `type A = B; type B = Foo;` is two aliases, and `A { .. }` has to reach `Foo`
        // through both — a one-hop resolver would stop at `B` and count nothing.
        let counts = struct_literal_counts(
            "type A = B; type B = Foo; fn forge() -> A { A {} }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_type_alias_of_a_use_alias_still_resolves() {
        // A chain need not be all one kind: `use Sealable as S; type Unchecked = S;` mixes a
        // `use` alias and a `type` alias, and both have to be chased to reach `Sealable`.
        let counts = struct_literal_counts(
            "use Sealable as S; type Unchecked = S; fn forge() -> Unchecked { Unchecked {} }",
            "Sealable",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_function_local_type_alias_still_resolves() {
        // Codex, issue #92's fifth round: a `type` alias declared *inside* a function body
        // is legal Rust and was invisible to a scan that only walked file items and inline
        // modules. `Unchecked` here exists only within `forge`'s block.
        let counts = struct_literal_counts(
            "fn forge() -> u8 {\n\
             \x20   type Unchecked = Foo;\n\
             \x20   let _ = Unchecked {};\n\
             \x20   0\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_type_alias_in_a_nested_block_still_resolves() {
        // The alias need not be at the top of the function body: an `if` arm's own block is
        // a block too, and gets its own scope when the visitor reaches it.
        let counts = struct_literal_counts(
            "fn forge(flag: bool) -> u8 {\n\
             \x20   if flag {\n\
             \x20       type Unchecked = Foo;\n\
             \x20       let _ = Unchecked {};\n\
             \x20   }\n\
             \x20   0\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_local_alias_does_not_leak_into_a_sibling_scope() {
        // A block-local alias shadows only its own scope. `Unchecked` in `other` means
        // something else, so its literal must not be counted as `Foo`.
        let counts = struct_literal_counts(
            "fn forge() -> u8 {\n\
             \x20   type Unchecked = Foo;\n\
             \x20   let _ = Unchecked {};\n\
             \x20   0\n\
             }\n\
             fn other() -> u8 {\n\
             \x20   type Unchecked = Bar;\n\
             \x20   let _ = Unchecked {};\n\
             \x20   0\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_function_local_alias_of_a_module_level_alias_still_resolves() {
        // Codex, issue #92's post-merge review: a block-local `type Inner = Outer;` where
        // `Outer` is itself a *module-level* alias for `Foo`. `resolve_local_alias_chain`
        // only searches the block's own aliases, so it correctly stops at `Outer` — but the
        // caller used to take that partial result as final instead of feeding it back
        // through the module-level resolver, so `Inner {}` was never counted as `Foo`.
        let counts = struct_literal_counts(
            "type Outer = Foo;\n\
             fn forge() -> u8 {\n\
             \x20   type Inner = Outer;\n\
             \x20   let _ = Inner {};\n\
             \x20   0\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_parenthesized_type_alias_still_resolves() {
        // Codex, issue #92's sixth round: `(Foo)` is valid Rust on a `type` alias's
        // right-hand side, `#[allow(unused_parens)]` lets it through `-D warnings`, and
        // `syn` keeps the parens as their own `Type::Paren` node rather than discarding
        // them — so the path underneath was invisible without unwrapping one more layer.
        let counts = struct_literal_counts(
            "#[allow(unused_parens)]\n\
             type Unchecked<'a> = (Foo<'a>);\n\
             fn forge() -> Unchecked<'static> { Unchecked {} }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_doubly_parenthesized_type_alias_still_resolves() {
        // Nested parens unwrap in more than one hop.
        let counts = struct_literal_counts(
            "#[allow(unused_parens)]\n\
             type Unchecked<'a> = ((Foo<'a>));\n\
             fn forge() -> Unchecked<'static> { Unchecked {} }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_plain_field_assignment_is_reported() {
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo) -> Foo {\n\
             \x20   dispatch.bytes = other;\n\
             \x20   dispatch\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_compound_assignment_to_a_field_is_reported() {
        // `^=` and its nine siblings parse as `Expr::Binary`, never `Expr::Assign` — a
        // separate route from a plain `=` in `syn`'s own grammar, not only in the source.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo) -> Foo {\n\
             \x20   dispatch.bytes ^= 1;\n\
             \x20   dispatch\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn an_ordinary_binary_expression_is_not_reported() {
        // `x.field + 1` reads `field` and rewrites nothing; only the ten assignment
        // operators name a mutation.
        let found = mutated_field_names(
            "fn read(dispatch: &Foo) -> i32 {\n\
             \x20   dispatch.bytes + 1\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_tuple_destructuring_assignment_to_a_field_is_reported() {
        // `(dispatch.bytes,) = (replacement,);` puts the field access inside a tuple on
        // the left of `=`, which the plain `Expr::Field` walk never enters on its own.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, replacement: Bytes) {\n\
             \x20   (dispatch.bytes,) = (replacement,);\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_struct_destructuring_assignment_to_a_field_is_reported() {
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, other: Bytes, id: Id) {\n\
             \x20   Foo { bytes: dispatch.bytes, id } = Foo { bytes: other, id };\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_nested_tuple_destructuring_assignment_to_a_field_is_reported() {
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, other: Bytes, x: i32) {\n\
             \x20   (x, (dispatch.bytes,)) = (x, (other,));\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_mutable_reference_to_a_field_is_reported() {
        // `mem::swap`/`mem::replace`/an arbitrary `&mut`-taking call all start here, and
        // none of them spells `=`.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, other: &mut Bytes) {\n\
             \x20   core::mem::swap(&mut dispatch.bytes, other);\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_shared_reference_to_a_field_is_not_reported() {
        // `&x.field` cannot mutate anything, so it is not a rewrite route.
        let found = mutated_field_names(
            "fn read(dispatch: &Foo) -> &Bytes {\n\
             \x20   &dispatch.bytes\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn an_unnamed_field_name_is_not_reported() {
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo) {\n\
             \x20   dispatch.0 = other;\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_field_write_under_cfg_test_is_not_reported() {
        let found = mutated_field_names(
            "#[cfg(test)]\n\
             mod tests {\n\
             \x20   fn tamper(mut dispatch: super::Foo) {\n\
             \x20       dispatch.bytes = other;\n\
             \x20   }\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_mutating_method_call_on_a_field_is_reported() {
        // Codex, issue #92's eighth round: `x.field.clone_from(&other)` reassigns the field
        // through an *implicit* `&mut self` autoref — no `=` and no explicit `&mut` anywhere
        // in the source, so neither of the other two routes sees it.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, other: &Bytes) {\n\
             \x20   dispatch.bytes.clone_from(other);\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_method_call_on_the_whole_value_is_not_reported() {
        // `dispatch.bytes()` calls a method *named* `bytes` on `dispatch` — the receiver is
        // `dispatch`, not a field access — which must stay legal: it is how every accessor in
        // this file is called.
        let found = mutated_field_names(
            "fn read(dispatch: Foo) -> Bytes {\n\
             \x20   dispatch.bytes()\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_ref_mut_struct_pattern_binding_is_reported() {
        // Codex, issue #92's ninth round: `field: ref mut slot` borrows the field mutably
        // through the pattern itself — no `=`, no `&mut` expression, no method call
        // anywhere, so none of the first three routes sees it.
        let found = mutated_field_names(
            "fn tamper(dispatch: Foo) {\n\
             \x20   let Foo { bytes: ref mut slot, .. } = dispatch;\n\
             \x20   *slot = other;\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_nested_ref_mut_struct_pattern_binding_is_reported() {
        // The binding can sit arbitrarily deep, e.g. behind a second guarded field.
        let found = mutated_field_names(
            "fn tamper(dispatch: Foo) {\n\
             \x20   let Foo { intent: Bar { id: ref mut slot, .. }, .. } = dispatch;\n\
             \x20   *slot = other;\n}",
            &["id"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["id"], "{found:?}");
    }

    #[test]
    fn a_mut_by_value_struct_pattern_binding_is_reported() {
        // Issue #171's finding 1: `field: mut slot` looks like it moves or copies the value
        // into a fresh local, and does — *if* the scrutinee is owned. `syn` sees only the
        // pattern, never the scrutinee's type, so it cannot tell that case apart from the one
        // below, where the identical pattern aliases the field instead. The sound answer
        // without type inference is to report every named binding on a guarded field, not
        // only the ones a by-value scrutinee would make safe.
        let found = mutated_field_names(
            "fn read(dispatch: Foo) {\n\
             \x20   let Foo { bytes: mut slot, .. } = dispatch;\n\
             \x20   slot = other;\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_bare_binding_under_match_ergonomics_is_reported() {
        // Issue #171's finding 1: no `ref`, `mut` or `&mut` is written anywhere here, and
        // `syn`'s parse of the bare identifier pattern `bytes` is identical whether `dispatch`
        // is owned or `&mut`. RFC 2005's match ergonomics mean the *default binding mode* — a
        // fact about `dispatch`'s type, not about this pattern's syntax — decides whether
        // `bytes` is a fresh copy or `&mut &'a [u8]` aliasing the original field. A `&mut`
        // scrutinee is exactly the case `Dispatchable::perform` and `CheckedDispatch::bytes`
        // both take `self`/`&self` through, so it is not a hypothetical.
        let found = mutated_field_names(
            "fn read(dispatch: &mut Foo) {\n\
             \x20   let Foo { bytes, .. } = dispatch;\n\
             \x20   *bytes = other;\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_ref_mut_let_binding_of_a_field_chain_is_reported() {
        // Codex, PR #183's own review: `let ref mut slot = dispatch.intent.request;` borrows
        // the initializer's place directly through the pattern, with no struct pattern for
        // `visit_field_pat` to read and no `=`/`&mut`/method call for the other routes.
        let found = mutated_field_names(
            "fn tamper(dispatch: &mut Foo) {\n\
             \x20   let ref mut slot = dispatch.intent.request;\n\
             \x20   consume(slot);\n}",
            &["intent"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["intent"], "{found:?}");
    }

    #[test]
    fn a_plain_ref_let_binding_of_a_field_chain_is_not_reported() {
        // `ref` with no `mut` borrows immutably — `&T`, not `&mut T` — so it cannot alias a
        // guarded field for writing and must stay unreported, the control beside the
        // positive case above.
        let found = mutated_field_names(
            "fn read(dispatch: &Foo) {\n\
             \x20   let ref slot = dispatch.intent;\n\
             \x20   consume(slot);\n}",
            &["intent"],
        )
        .expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_by_value_let_binding_of_a_field_chain_is_not_reported() {
        // `let mut slot = dispatch.intent;` with no `ref` moves or copies the value into a
        // fresh local rather than aliasing the original place — a read, and the control that
        // shows the new check is scoped to `ref mut` rather than to every `let`.
        let found = mutated_field_names(
            "fn read(dispatch: Foo) {\n\
             \x20   let mut slot = dispatch.intent;\n\
             \x20   consume(slot);\n}",
            &["intent"],
        )
        .expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_qself_type_alias_is_reported() {
        // Codex, issue #92's ninth round: `<T as Trait>::Assoc` can name any struct the
        // trait's `impl` chooses, and following it needs type inference `syn` does not have.
        let found = qself_type_alias_names("type Unchecked = <Via as Alias>::Dispatch;")
            .expect("the fixture parses");
        assert_eq!(found, ["Unchecked"], "{found:?}");
    }

    #[test]
    fn a_qself_type_alias_with_no_trait_is_reported() {
        // `<T>::Assoc`, with no `as Trait`, is the same projection shape.
        let found = qself_type_alias_names("type Unchecked = <Via>::Dispatch;")
            .expect("the fixture parses");
        assert_eq!(found, ["Unchecked"], "{found:?}");
    }

    #[test]
    fn a_plain_type_alias_is_not_a_qself_projection() {
        let found = qself_type_alias_names("type Unchecked = Foo;").expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_parenthesized_qself_type_alias_is_reported() {
        let found = qself_type_alias_names(
            "#[allow(unused_parens)]\ntype Unchecked = (<Via as Alias>::Dispatch);",
        )
        .expect("the fixture parses");
        assert_eq!(found, ["Unchecked"], "{found:?}");
    }

    #[test]
    fn a_qself_type_alias_under_cfg_test_is_not_reported() {
        let found = qself_type_alias_names(
            "#[cfg(test)]\nmod tests {\n    type Unchecked = <Via as Alias>::Dispatch;\n}",
        )
        .expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_generic_type_alias_projecting_through_its_own_parameter_is_reported() {
        // Issue #171's finding 2: `T::Dispatch` has no `qself` — no `<`, no `as` — so it
        // parses as an ordinary path, `["T", "Dispatch"]`. Chased as one, it resolves to
        // `Dispatch`, never `CheckedDispatch`, so a construction spelled through `Unchecked`
        // was invisible to the pin. But `T` is a generic parameter of `Unchecked` itself, so
        // whatever it names at each call site is exactly as unresolvable as
        // `<T as Trait>::Assoc` — the same danger, spelled without the disambiguating syntax.
        let found = qself_type_alias_names(
            "#[allow(type_alias_bounds)]\ntype Unchecked<T: Alias> = T::Dispatch;",
        )
        .expect("the fixture parses");
        assert_eq!(found, ["Unchecked"], "{found:?}");
    }

    #[test]
    fn a_parenthesized_generic_projection_is_reported() {
        let found = qself_type_alias_names(
            "#[allow(type_alias_bounds, unused_parens)]\n\
             type Unchecked<T: Alias> = (T::Dispatch);",
        )
        .expect("the fixture parses");
        assert_eq!(found, ["Unchecked"], "{found:?}");
    }

    #[test]
    fn a_bare_generic_parameter_with_no_projection_is_not_reported() {
        // `T` alone is a plain path `type_alias_target` already resolves; nothing is
        // projected through it.
        let found = qself_type_alias_names("type Same<T> = T;").expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_qualified_path_through_a_concrete_type_is_not_reported() {
        // `Via` here is an ordinary, already-defined type — not one of `Unchecked`'s own
        // generic parameters — so this is a module-qualified path (`type Foo =
        // some_module::Bar;`'s shape), not a projection through a trait bound. Telling a
        // UFCS-style projection through a *concrete* type apart from that needs real name
        // resolution, which this scanner does not have; the issue that adds this check scopes
        // it to the alias's own declared parameters rather than guessing further.
        let found =
            qself_type_alias_names("type Unchecked = Via::Dispatch;").expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn an_assignment_beneath_a_guarded_ancestor_field_is_reported() {
        // Codex, issue #92's tenth round: `dispatch.intent.request.kind = x;` assigns to
        // `kind`, not to a guarded name directly — but `intent` and `request` are both
        // guarded ancestors in the chain, and rewriting through either is the same rewrite
        // this whole family of checks exists to catch.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, x: u8) {\n\
             \x20   dispatch.intent.request.kind = x;\n}",
            &["intent", "request"],
        )
        .expect("the fixture parses");
        let mut sorted = found;
        sorted.sort_unstable();
        assert_eq!(sorted, ["intent", "request"], "{sorted:?}");
    }

    #[test]
    fn a_mutable_reference_beneath_a_guarded_ancestor_field_is_reported() {
        let found = mutated_field_names(
            "fn tamper(dispatch: Foo) {\n\
             \x20   let r = &mut dispatch.intent.id.seq;\n}",
            &["intent", "id"],
        )
        .expect("the fixture parses");
        let mut sorted = found;
        sorted.sort_unstable();
        assert_eq!(sorted, ["id", "intent"], "{sorted:?}");
    }

    #[test]
    fn a_method_call_beneath_a_guarded_ancestor_field_is_reported() {
        let found = mutated_field_names(
            "fn tamper(dispatch: Foo, other: u8) {\n\
             \x20   dispatch.intent.request.kind.clone_from(&other);\n}",
            &["intent", "request"],
        )
        .expect("the fixture parses");
        let mut sorted = found;
        sorted.sort_unstable();
        assert_eq!(sorted, ["intent", "request"], "{sorted:?}");
    }

    #[test]
    fn a_blocks_local_alias_does_not_leak_into_a_nested_module() {
        // Codex: the inverse leak from the next test below. A block-local `type S = Foo;`
        // is not visible inside a `mod` nested in that same block — real Rust never lets a
        // module inherit an enclosing function's local items — so `S {}` inside
        // `hidden::make` names `hidden`'s own `S`, not `Foo`.
        let counts = struct_literal_counts(
            "fn forge() -> u8 {\n\
             \x20   type S = Foo;\n\
             \x20   mod hidden {\n\
             \x20       pub struct S;\n\
             \x20       fn make() -> S { S {} }\n\
             \x20   }\n\
             \x20   0\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 0, "{counts:?}");
    }

    #[test]
    fn a_nested_modules_alias_does_not_leak_into_the_enclosing_blocks_lookup() {
        // Codex: block-local alias lookup must not descend into a nested `mod`'s own
        // aliases. `type S = Foo;` declared directly in the block is what a bare `S {}`
        // resolves to there; `mod hidden { type S = Bar; }` declared alongside it is a
        // separate scope, invisible outside `hidden` — the same rule `own_aliases`
        // already enforces for module-level lookups, not consulted here before this fix.
        let counts = struct_literal_counts(
            "fn forge() -> u8 {\n\
             \x20   type S = Foo;\n\
             \x20   mod hidden {\n\
             \x20       type S = Bar;\n\
             \x20   }\n\
             \x20   let _ = S {};\n\
             \x20   0\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_parenthesized_ancestor_in_the_field_chain_is_reported() {
        // Codex, issue #92's thirteenth round: `(dispatch.intent.request).kind = x;` puts
        // the guarded ancestors behind an `Expr::Paren`, so `note`'s chain walk — a
        // `while let Expr::Field` loop over `field.base` — stopped the moment it met the
        // parenthesized prefix rather than seeing `intent` and `request` inside it.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, x: u8) {\n\
             \x20   (dispatch.intent.request).kind = x;\n}",
            &["intent", "request"],
        )
        .expect("the fixture parses");
        let mut sorted = found;
        sorted.sort_unstable();
        assert_eq!(sorted, ["intent", "request"], "{sorted:?}");
    }

    #[test]
    fn a_doubly_parenthesized_ancestor_in_the_field_chain_is_reported() {
        // Nested parens unwrap in more than one hop, the same shape as a doubly
        // parenthesized type alias.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, x: u8) {\n\
             \x20   ((dispatch.intent).request).kind = x;\n}",
            &["intent", "request"],
        )
        .expect("the fixture parses");
        let mut sorted = found;
        sorted.sort_unstable();
        assert_eq!(sorted, ["intent", "request"], "{sorted:?}");
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
