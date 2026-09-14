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

/// The `use` and `type` bindings `items` declares directly, at its own level only.
///
/// Unlike [`collect_item_aliases`], this does not recurse into a nested `mod`.
/// Real Rust scopes a `use` binding to the module that declares it: an inner
/// module does not inherit an outer one's aliases, and a sibling module's
/// aliases are not visible either. A resolver that read every alias in the
/// file as one flat list could chain a name through an unrelated module's
/// rename and report a real, correct `impl` as a fifth future (issue #109
/// review). Each caller that walks into a nested module must call this again
/// on that module's own items, so every scope stays its own.
///
/// A `type` alias is scoped the same way — a module-local `type Unchecked = Foo;` is no
/// more visible outside its module than a `use` rename is, so it is collected here rather
/// than only at [`block_own_aliases`]'s function-body granularity.
///
/// Takes an iterator, not a slice, so [`block_own_aliases`] can filter a block's statements
/// down to its item-statements and hand them here directly — a `&[syn::Item]` moves the same
/// way, since a slice's iterator yields `&syn::Item` too.
fn own_aliases<'a>(items: impl IntoIterator<Item = &'a syn::Item>) -> Vec<UseAlias> {
    let mut aliases = Vec::new();
    for item in items {
        if has_cfg_test(item_attrs(item)) {
            continue;
        }
        match item {
            syn::Item::Use(use_item) => collect_tree_aliases(
                &use_item.tree,
                use_item.leading_colon.is_some(),
                &mut Vec::new(),
                &mut aliases,
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
            _ => {}
        }
    }
    aliases
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
/// The alias may be declared at file scope, in an inline module, or inside a function body.
/// See `type_is_qself_projection` for the three shapes a projection can take. Such a
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
/// nested block, which gets its own scope when [`struct_literal_counts`]'s visitor reaches it
/// (issue #92, Codex's fifth round: a function-local alias is invisible to a scan built for
/// module-level declarations only) — and not in a nested `mod`, for [`own_aliases`]'s reason.
/// [`own_aliases`] already refuses to recurse into a `mod`; this shares it rather than going
/// through [`collect_item_aliases`], which does recurse and would leak a `mod` declared
/// inside this block's own statements into the block's scope, bypassing the module's opacity.
fn block_own_aliases(block: &syn::Block) -> Vec<UseAlias> {
    own_aliases(block.stmts.iter().filter_map(|stmt| match stmt {
        syn::Stmt::Item(item) => Some(item),
        _ => None,
    }))
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
    // A renamed re-export chains one alias to another, possibly through
    // another scope (issue #109), and a `type` alias chains the same way —
    // `type A = B; type B = CheckedDispatch;` is two aliases, and a literal
    // spelled `A { .. }` has to reach `CheckedDispatch` through both (issue
    // #92, Codex's fourth round). A plain relative path can also step into
    // a sibling module (issue #169), possibly more than once. Bounded by
    // the whole file's own alias count plus its item count: enough for any
    // real chain or descent, and it stops a crafted alias cycle
    // (`use a as b; use b as a;`) from looping forever. The item count
    // alone is not enough (Codex review, PR #176): one `use` item can pack
    // many chained hops into a single group, `use m::{a as b, b as c,
    // ...};`, so counting items undercounts how many hops a real, acyclic
    // chain may need. Module descent cannot cycle on its own, since each
    // step moves to a strictly smaller, physically nested subtree — the
    // item count alone bounds that half.
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

/// One scope of [`struct_literal_counts`]'s own stack: a module's item list (opaque,
/// steppable into by name — issue #169) or a block's own precomputed aliases (transparent —
/// issue #92's local `type` alias).
///
/// A block has no `&[syn::Item]` slice of its own — its statements interleave items with
/// everything else — so a block's aliases are collected once, by [`block_own_aliases`], and
/// carried here rather than recomputed from an item list the way a module's are. Module
/// descent (issue #169) is scoped to module frames only: nothing here asks for a `mod`
/// declared inside a block to be steppable into from outside that block, which is a case
/// nobody has built or needed.
enum LiteralScope<'ast> {
    /// A module's own item list.
    Module(&'ast [syn::Item]),
    /// A block's own precomputed aliases.
    Block(Vec<UseAlias>),
}

impl LiteralScope<'_> {
    /// This scope's own `use` and `type` aliases, computed on demand for a module the same
    /// way [`resolve_segments`] does.
    fn aliases(&self) -> Vec<UseAlias> {
        match self {
            Self::Module(items) => own_aliases(*items),
            Self::Block(aliases) => aliases.clone(),
        }
    }

    /// Whether a lookup that misses here falls through to the scope below without an
    /// explicit `super::` — true for a block, false for a module (issue #109).
    const fn transparent(&self) -> bool {
        matches!(self, Self::Block(_))
    }
}

/// What [`resolve_segments_through_blocks`]'s per-scope search found at a given frame: an
/// alias to substitute, or (module frames only) a sibling module to step into by name.
enum LiteralFound<'ast> {
    Alias(UseAlias),
    Module(&'ast [syn::Item]),
}

/// [`resolve_segments`], over a stack that mixes module scopes (opaque, and steppable into
/// by name — issue #169) and block scopes (transparent — issue #92's local `type` alias) —
/// [`struct_literal_counts`]'s own need.
///
/// A module does not inherit an outer scope's aliases just by lexical nesting (issue #109);
/// a block does — real Rust resolves a bare name against every block it is lexically inside
/// of, with no `super::` needed, before it falls back to the enclosing module. So a lookup
/// that misses at `scope` continues outward only while each scope it crosses is marked
/// transparent, stopping at — but still checking — the first opaque one. A plain relative
/// path may also name a sibling `mod` block declared at a module frame the search reaches
/// this way (issue #169): a construction reached from inside a block, through a sibling
/// module declared in an *enclosing* module, is exactly the shape a scanner that only
/// checked the block's own frame would miss — the dangerous direction for a pin that exists
/// to catch a forged construction.
///
/// A chained alias is resolved from where *it* was declared, not from the original use
/// site: once a name is found at `probe`, `scope` moves there before the next segment is
/// looked up. Real Rust resolves an alias's right-hand side lexically, at its own
/// declaration — `type Unchecked = CheckedDispatch;` at module scope means the
/// `CheckedDispatch` visible *there*, even when `Unchecked { .. }` is written inside a block
/// that shadows `CheckedDispatch` with its own, unrelated `type CheckedDispatch = Decoy;`.
/// Leaving `scope` at the use site let that inner shadow win regardless of where the alias
/// chasing it was actually declared, hiding a real construction behind an unrelated,
/// same-named local alias (Codex review, PR #183). Once resolution has stepped into a
/// sibling module by name, it leaves the lexical ancestor stack behind exactly as
/// [`resolve_segments`] does: `self::` still resolves inside that module, but `super::` and
/// further fallthrough do not, because a module reached by name has no ancestor this
/// per-file scan can identify past the point it was entered from.
/// One loop iteration of [`resolve_segments_through_blocks`] once resolution has stepped
/// into a module by name (`entered`) — mirrors [`resolve_segments`]'s own `entered` handling,
/// with no fallthrough, because a module reached by name has no ancestor this per-file scan
/// can identify.
///
/// `Ok(true)` substituted an alias or descended further, and the caller's loop should
/// continue; `Ok(false)` found neither, and the caller's loop should stop; `Err(segments)` is
/// an absolute alias's early return.
fn step_entered_scope<'ast>(
    items: &'ast [syn::Item],
    segments: &mut Vec<String>,
    descended_prefix: &mut Vec<String>,
    entered: &mut Option<&'ast [syn::Item]>,
) -> Result<bool, Vec<String>> {
    consume_self_prefix(segments);
    let Some(first) = segments.first().cloned() else {
        return Ok(false);
    };
    if let Some(alias) = own_aliases(items)
        .iter()
        .find(|candidate| candidate.local == first)
    {
        let mut resolved = alias.target.clone();
        resolved.extend(segments.drain(1..));
        *segments = resolved;
        if alias.absolute {
            return Err(segments.clone());
        }
        return Ok(true);
    }
    // A one-segment path names an item, not a module to step into (Codex review, PR #176):
    // `own_modules` is not even consulted once `segments` has nothing left past the head.
    if segments.len() > 1 {
        if let Some((_, module_items)) = own_modules(items)
            .into_iter()
            .find(|(name, _)| *name == first)
        {
            descended_prefix.push(segments.remove(0));
            *entered = Some(module_items);
            return Ok(true);
        }
    }
    Ok(false)
}

fn resolve_segments_through_blocks(path: &syn::Path, stack: &[LiteralScope<'_>]) -> Vec<String> {
    let mut segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| ident_name(&segment.ident))
        .collect();
    if path.leading_colon.is_some() {
        return segments;
    }
    let mut scope = stack.len().saturating_sub(1);
    let mut entered: Option<&[syn::Item]> = None;
    let mut descended_prefix: Vec<String> = Vec::new();
    // `stack[0]` is always the file's own module (`struct_literal_counts` never builds this
    // stack any other way), so `item_count`/`total_alias_count` over it already bounds any
    // alias chain or module descent reachable anywhere in the file, the same reasoning
    // `resolve_segments`'s own bound uses. A block frame is invisible to both — a `syn::Item`
    // walk never sees inside a `syn::Block` — so its own alias count is added separately.
    let module_bound = match stack.first() {
        Some(LiteralScope::Module(items)) => item_count(items) + total_alias_count(items),
        Some(LiteralScope::Block(_)) | None => 0,
    };
    let block_bound: usize = stack
        .iter()
        .map(|frame| match frame {
            LiteralScope::Block(aliases) => aliases.len(),
            LiteralScope::Module(_) => 0,
        })
        .sum();
    let bound = module_bound + block_bound + 1;
    for _ in 0..=bound {
        if let Some(items) = entered {
            match step_entered_scope(items, &mut segments, &mut descended_prefix, &mut entered) {
                Ok(true) => continue,
                Ok(false) => break,
                Err(resolved) => return resolved,
            }
        }

        consume_scope_prefix_through_blocks(&mut segments, &mut scope, stack);
        let Some(first) = segments.first().cloned() else {
            break;
        };
        let mut probe = scope;
        let found = loop {
            let Some(frame) = stack.get(probe) else {
                break None;
            };
            if let Some(alias) = frame
                .aliases()
                .into_iter()
                .find(|candidate| candidate.local == first)
            {
                break Some((LiteralFound::Alias(alias), probe));
            }
            if segments.len() > 1 {
                if let LiteralScope::Module(items) = frame {
                    if let Some((_, module_items)) = own_modules(items)
                        .into_iter()
                        .find(|(name, _)| *name == first)
                    {
                        break Some((LiteralFound::Module(module_items), probe));
                    }
                }
            }
            if frame.transparent() && probe > 0 {
                probe -= 1;
            } else {
                break None;
            }
        };
        let Some((found, at)) = found else {
            break;
        };
        match found {
            LiteralFound::Alias(alias) => {
                scope = at;
                let mut resolved = alias.target.clone();
                resolved.extend(segments.drain(1..));
                segments = resolved;
                if alias.absolute {
                    return segments;
                }
            }
            LiteralFound::Module(items) => {
                descended_prefix.push(segments.remove(0));
                entered = Some(items);
            }
        }
    }
    match entered {
        Some(_) => consume_self_prefix(&mut segments),
        None => consume_scope_prefix_through_blocks(&mut segments, &mut scope, stack),
    }
    // Nothing resolved past the last module entered by name: give the names descent
    // consumed back, so the answer is the path as written rather than a name silently
    // thrown away (Codex review, PR #176).
    descended_prefix.append(&mut segments);
    descended_prefix
}

/// The nearest module frame reachable from `level` by walking outward (toward index 0)
/// through consecutive transparent (block) frames — `level` itself, if it is already a
/// module.
///
/// `self`/`super` name a *module*; a block is not one, so neither can land on a block frame.
/// A `mod` may be declared directly inside a block (`fn f() { mod inner { .. } }`), so more
/// than one block can separate two module levels — this walks past all of them, not just one.
fn nearest_module_at_or_below(stack: &[LiteralScope<'_>], level: usize) -> usize {
    let mut level = level;
    while level > 0 && stack.get(level).is_some_and(LiteralScope::transparent) {
        level -= 1;
    }
    level
}

/// [`consume_scope_prefix`], aware that `stack` mixes block scopes with module ones.
///
/// `self` and `super` both name a module, never a block, so each first settles `scope` on
/// the nearest enclosing module — [`nearest_module_at_or_below`] — before doing anything
/// else: `self::X` looked up from three blocks deep in one function still means "`X` in this
/// function's own module", not "`X` in the innermost block". `super` then steps one module
/// further out, past whatever blocks separate the two module levels this time, the same way
/// [`consume_scope_prefix`] steps past exactly one module when every level is one. At the
/// file's own top level a `super` is left unconsumed, for the same reason it is there: this
/// scan sees one file, never the module that declared it, so there is nowhere left to step to
/// and pretending otherwise would answer a question it cannot see the answer to.
fn consume_scope_prefix_through_blocks(
    segments: &mut Vec<String>,
    scope: &mut usize,
    stack: &[LiteralScope<'_>],
) {
    loop {
        match segments.first().map(String::as_str) {
            Some("self") => {
                segments.remove(0);
                *scope = nearest_module_at_or_below(stack, *scope);
            }
            Some("super") => {
                let module = nearest_module_at_or_below(stack, *scope);
                if module == 0 {
                    break;
                }
                segments.remove(0);
                *scope = nearest_module_at_or_below(stack, module - 1);
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

/// Every name in `names` that `contents` writes to as a struct field, outside
/// `#[cfg(test)]` — in any of the three ways a field's value can be rewritten in place
/// rather than rebuilt.
///
/// A plain assignment, `x.field = value;`, is one route. A `&mut` reference taken to the
/// field is the second — `std::mem::swap(&mut x.field, &mut y.field)`,
/// `std::mem::replace(&mut x.field, value)`, and passing the reference to an arbitrary
/// function that takes `&mut T` are all routes to the same rewrite that spell no `=` at all,
/// and all three need a `&mut` to the field first, which is the shape this refuses. A
/// *method* call on the field is the third, and the one that needs neither: `x.field.
/// clone_from(&other)` autorefs `&mut x.field` implicitly, with no `&mut` token written
/// anywhere — so every method call on a guarded field is refused outright, since telling a
/// mutating method from a read-only one needs type inference `syn` does not have. A method
/// called on the whole *value* (`x.field()`, an accessor) is unaffected: its receiver is a
/// plain path, not a field access. A named binding in a struct pattern is the fourth:
/// `let Foo { field: ref mut slot, .. } = x;` borrows `field` mutably through the pattern
/// itself, with no assignment, no `&mut` expression and no method call anywhere for the
/// first three routes to see — and so, under match ergonomics, does a bare `field` or a
/// by-value `field: mut slot` once `x` is itself a `&mut` reference, with no `ref`, `mut` or
/// `&mut` written anywhere in the pattern to tell that apart from a harmless move or copy
/// (issue #171's finding 1). `syn` sees the pattern, never `x`'s type, so it cannot rule
/// the reference case out — every named binding on a guarded field is refused, not only an
/// explicit `ref mut`. Only a wildcard (`_`) binds no name and is left alone.
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
    let file = parse_rust(contents)?;
    let mut visitor = Mutations {
        names,
        found: Vec::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.found)
}

/// Whether `pat`, or any sub-pattern it contains, binds a name at all.
///
/// Issue #171's finding 1: `syn` sees a pattern's syntax, never the type of the value it
/// matches, and Rust's match ergonomics (RFC 2005) let that type decide what a *bare*
/// binding means. `let Foo { field, .. } = dispatch;` moves or copies `field` when
/// `dispatch` is owned — a read, safe to ignore — but binds it by mutable reference when
/// `dispatch: &mut Foo`, with no `ref`, `mut` or `&mut` written anywhere to tell the two
/// apart. An explicit `ref mut` is only the loudest of several spellings that reach a
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

/// Whether `op` is a compound-assignment operator (`+=`, `^=`, ...).
///
/// `syn` represents these as `BinOp` variants on an `Expr::Binary` rather than as
/// `Expr::Assign` with a binary right-hand side — Rust's own grammar treats `a += b` as one
/// operator token, not `a = a + b` spelled out. [`Mutations::visit_expr_binary`] is why this
/// matters: a scanner that only watched `Expr::Assign` for `=` would miss every compound
/// form.
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

/// [`mutated_field_names`]'s own visitor.
struct Mutations<'a> {
    names: &'a [&'a str],
    found: Vec<String>,
}

impl Mutations<'_> {
    fn note(&mut self, expr: &syn::Expr) {
        // Walks the whole chain of field accesses, not only the outermost one:
        // `dispatch.intent.request.kind = x;` assigns to `kind`, but `intent` and
        // `request` are guarded *ancestors* in the same chain, and rewriting through
        // either is the rewrite this whole family of checks exists to catch (issue #92,
        // Codex's tenth round). Stops at the first non-field expression, which is the
        // root the chain is built on.
        //
        // Parens or a macro's hygiene grouping may sit anywhere in the chain — around
        // the whole expression (`&mut (dispatch.bytes)`) or around an intermediate base
        // (`(dispatch).bytes`) — and `syn` keeps each as its own node rather than
        // discarding it, the same shape `type_alias_target` already unwraps. Left
        // unhandled, `core::mem::replace(&mut (dispatch.bytes), other)` rewrites a
        // guarded field with the gate green (Codex review, PR #183): `node.expr` is an
        // `Expr::Paren`, `note`'s own loop never matched anything but `Expr::Field`, and
        // the default traversal that reaches the field underneath does not know it is
        // being written to. Every step unwraps first, so a paren anywhere in the chain
        // is transparent to it.
        let mut current = Self::unwrap_parens(expr);
        while let syn::Expr::Field(field) = current {
            if let syn::Member::Named(ident) = &field.member {
                let name = ident_name(ident);
                if self.names.contains(&name.as_str()) {
                    self.found.push(name);
                }
            }
            current = Self::unwrap_parens(&field.base);
        }
    }

    /// `expr`, with any wrapping `Expr::Paren` or `Expr::Group` stripped, recursively.
    fn unwrap_parens(expr: &syn::Expr) -> &syn::Expr {
        match expr {
            syn::Expr::Paren(paren) => Self::unwrap_parens(&paren.expr),
            syn::Expr::Group(group) => Self::unwrap_parens(&group.expr),
            _ => expr,
        }
    }

    /// [`Self::note`], recursed through a destructuring assignment's own shape.
    ///
    /// `(dispatch.bytes, dispatch.intent) = (other, other_intent);` is valid Rust since
    /// destructuring assignment stabilized, and its left side parses as an ordinary
    /// `Expr::Tuple` of field-access expressions — not one `Expr::Field` chain, so
    /// `note`'s own `while let` never matches it, and the *assignment* meaning is
    /// something only this call site knows; the default traversal that would otherwise
    /// reach each field walks them as plain reads (Codex review, PR #183). A struct
    /// pattern (`Foo { bytes, .. } = dispatch;`) reuses struct-literal syntax as an
    /// assignment target the same way, with its fields as the assignable places; `..`
    /// carries no place of its own. An array pattern (`[a, b] = pair;`) is the third
    /// shape `syn` allows here. Anything else — a bare place, an index, a dereference —
    /// is a single target and goes straight to `note`.
    fn note_assignment_target(&mut self, expr: &syn::Expr) {
        match expr {
            syn::Expr::Tuple(tuple) => {
                for element in &tuple.elems {
                    self.note_assignment_target(element);
                }
            }
            syn::Expr::Array(array) => {
                for element in &array.elems {
                    self.note_assignment_target(element);
                }
            }
            syn::Expr::Struct(structure) => {
                for field in &structure.fields {
                    self.note_assignment_target(&field.expr);
                }
            }
            syn::Expr::Paren(paren) => self.note_assignment_target(&paren.expr),
            _ => self.note(expr),
        }
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
        self.note_assignment_target(&node.left);
        syn::visit::visit_expr_assign(self, node);
    }

    fn visit_expr_binary(&mut self, node: &'ast syn::ExprBinary) {
        // `dispatch.intent.request.input_crc ^= other;` rewrites `input_crc` exactly like
        // a plain `=` would, but `syn` represents every compound-assignment operator as a
        // `BinOp` on an `Expr::Binary` rather than as `Expr::Assign` — so `visit_expr_assign`
        // above never sees it, and the default traversal walks the left side as an ordinary
        // read (Codex review, PR #183). A compound-assignment target is always a single
        // place, never a tuple or struct destructuring, so this goes straight to `note`
        // rather than through `note_assignment_target`.
        if is_compound_assign(&node.op) {
            self.note(&node.left);
        }
        syn::visit::visit_expr_binary(self, node);
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        // `let ref mut slot = dispatch.intent.request;` borrows the initializer's own
        // place directly through the pattern — no `Expr::Assign`, no `Expr::Reference` and
        // no method call anywhere, so none of the routes above sees it, and there is no
        // struct pattern here for `visit_field_pat` to read either: the whole pattern is
        // one identifier (Codex review, PR #183). `ref` and `mut` are explicit keywords
        // here, not a scrutinee-dependent default binding mode the way a struct pattern's
        // field is, so the check is exact rather than over-broad: only `ref mut` creates a
        // mutable alias, and only when the whole pattern is one identifier binding directly
        // to the initializer expression. A nested correspondence — a tuple pattern's own
        // `ref mut` element matched against a tuple initializer's own element — is a
        // residual limit stated rather than closed, the same standing this file already
        // states for other syntactic scans.
        if let (syn::Pat::Ident(pat_ident), Some(init)) = (&node.pat, &node.init) {
            if pat_ident.by_ref.is_some() && pat_ident.mutability.is_some() {
                self.note(&init.expr);
            }
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
        // `let Foo { field: ref mut slot, .. } = x;` borrows `field` mutably through the
        // pattern itself — no `Expr::Assign`, no `Expr::Reference`, and no method call
        // anywhere, so none of the three routes above sees it. A bare `field` or a
        // by-value `field: mut slot` reaches the same alias under match ergonomics when
        // the scrutinee is `&mut`, a fact this scanner cannot see (issue #171's finding
        // 1) — so every named binding is reported, not only an explicit `ref mut`. It can
        // be arbitrarily nested (`field: Inner { deeper, .. }`), which is why this walks
        // the whole sub-pattern rather than checking only its outermost shape.
        if let syn::Member::Named(ident) = &node.member {
            let name = ident_name(ident);
            if self.names.contains(&name.as_str()) && pattern_binds_a_name(&node.pat) {
                self.found.push(name);
            }
        }
        syn::visit::visit_field_pat(self, node);
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
        // `stack[0]` is the file's top-level module; a later entry is one module or block
        // nested inside the last, pushed on entry and popped on exit by `visit_item_mod` and
        // `visit_block` — see `LiteralScope` and `resolve_segments_through_blocks`.
        stack: Vec<LiteralScope<'ast>>,
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
            // A nested module's own item list goes on top of the stack (issue #109
            // review): pushed while walking it, popped afterward, so `super::`/`crate::`
            // can still reach an ancestor scope while the module's own scope never
            // inherits it implicitly. An out-of-line declaration (`mod x;`) has no body
            // to push, but its own name and attributes must still be visited the default
            // way — an early return here had skipped them (Codex review, PR #160),
            // hiding a banned identifier spelled as a module name. A module is opaque:
            // unlike a block, a name that misses here does not fall through to the
            // enclosing scope without an explicit `super::`.
            let pushed = node.content.is_some();
            if let Some((_, items)) = node.content.as_ref() {
                self.stack.push(LiteralScope::Module(items));
            }
            syn::visit::visit_item_mod(self, node);
            if pushed {
                self.stack.pop();
            }
        }

        fn visit_block(&mut self, node: &'ast syn::Block) {
            // A function-local `use` or `type` alias is visible only inside the block that
            // declares it (issue #92, Codex's fifth round), so every block gets its own
            // scope rather than one flat, file-wide list — the same rule `visit_item_mod`
            // enforces for a module, on the same stack. A block is transparent (`true`):
            // real Rust resolves a bare name against every enclosing block, with no
            // `super::` needed, unlike a module.
            self.stack
                .push(LiteralScope::Block(block_own_aliases(node)));
            syn::visit::visit_block(self, node);
            self.stack.pop();
        }

        fn visit_expr_struct(&mut self, node: &'ast syn::ExprStruct) {
            let resolved = resolve_segments_through_blocks(&node.path, &self.stack);
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
        stack: vec![LiteralScope::Module(&file.items)],
        name: name.to_owned(),
        count: 0,
    };
    total.visit_file(&file);

    let mut inside_count = 0_usize;
    for target in inside_targets(&file, &inside) {
        let mut visitor = Literals {
            stack: target
                .stack()
                .iter()
                .copied()
                .map(LiteralScope::Module)
                .collect(),
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
    fn a_mod_nested_in_a_block_does_not_leak_its_alias_into_the_block() {
        // A `mod` may be declared directly inside a function body, and real Rust still keeps
        // its own aliases private to it: `inner::Decoy` (or a `use` reaching in) would be
        // needed to see `Decoy` from `outer`'s own body, not a bare name. `block_own_aliases`
        // used to delegate to `collect_item_aliases`, which *does* recurse into a `mod` — so a
        // decoy alias declared inside one leaked flat into the enclosing block's own scope,
        // shadowing whatever a genuine, correctly block-scoped alias of the same name would
        // otherwise have resolved to. This construction is the dangerous direction: the real
        // `Foo` below would have been masked and gone uncounted.
        let counts = struct_literal_counts(
            "fn outer() {\n\
             \x20   mod inner {\n\
             \x20       use Bar as Decoy;\n\
             \x20   }\n\
             \x20   use Foo as Decoy;\n\
             \x20   let _ = Decoy {};\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn super_steps_past_every_block_between_two_module_levels() {
        // Blocks are not modules, so the number of `super`s a path needs is a function of
        // module nesting alone — never of how many `{ }` blocks sit between the reference and
        // its enclosing module. One `super` from anywhere inside `f`'s body, however deep in
        // nested blocks, reaches `outer`'s parent (the file this scan read).
        let counts = struct_literal_counts(
            "use Foo as Alias;\n\
             mod outer {\n\
             \x20   fn f() {\n\
             \x20       {\n\
             \x20           {\n\
             \x20               let _ = super::Alias {};\n\
             \x20           }\n\
             \x20       }\n\
             \x20   }\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn self_names_the_enclosing_module_and_bypasses_a_blocks_own_shadow() {
        // `self::X` always names `X` in the enclosing *module*, not in whichever block the
        // reference happens to sit in — verified against real `rustc`: a block-local alias of
        // the same name is not what `self::` reaches, even from inside that very block.
        let counts = struct_literal_counts(
            "use Foo as X;\n\
             fn f() {\n\
             \x20   use Bar as X;\n\
             \x20   let _ = self::X {};\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_chained_alias_resolves_at_its_own_declaration_scope_not_the_use_site() {
        // Codex, PR #183's own review: real Rust resolves an alias's right-hand side
        // lexically, at *its own* declaration — `type Unchecked = Foo;` at module scope
        // means the `Foo` visible there, whatever a block using `Unchecked` later shadows
        // `Foo` with. The lookup that finds `Unchecked` falls through the block (transparent)
        // to the module (opaque) — but the alias it finds still has to be chased from where
        // *it* lives, not from the block the caller happened to be standing in, or the
        // block's own decoy `Foo` would resolve first and a real `Unchecked { .. }`
        // construction of the guarded type would go uncounted.
        let counts = struct_literal_counts(
            "type Unchecked = Foo;\n\
             fn f() {\n\
             \x20   type Foo = Decoy;\n\
             \x20   let _ = Unchecked {};\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_sibling_module_reached_from_inside_a_block_is_stepped_into() {
        // Issue #169's own capability, merged into this block-aware resolver rather than
        // inherited from `resolve_segments`: a plain relative path may name a sibling `mod`
        // declared at an *enclosing* scope, reached from inside a function body. A scanner
        // that only checked the block's own frame would miss this construction entirely —
        // the dangerous direction for a pin that exists to catch a forged one.
        let counts = struct_literal_counts(
            "mod traits {\n\
             \x20   pub use Foo as Pollable;\n\
             }\n\
             fn f() {\n\
             \x20   let _ = traits::Pollable {};\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 1, "{counts:?}");
    }

    #[test]
    fn a_mod_declared_inside_a_block_is_not_reached_from_outside_it() {
        // The mirror image of the test above: issue #169's descent is scoped to module
        // frames a lookup's fallthrough search actually reaches. A `mod` declared *inside*
        // one function's block is not in scope for a second, unrelated function — real Rust
        // agrees, and nothing here has ever asked for cross-block module visibility.
        let counts = struct_literal_counts(
            "fn f() {\n\
             \x20   mod traits {\n\
             \x20       pub use Foo as Pollable;\n\
             \x20   }\n\
             }\n\
             fn g() {\n\
             \x20   let _ = traits::Pollable {};\n\
             }",
            "Foo",
            FnScope::None,
        )
        .expect("the fixture parses");
        assert_eq!(counts.total, 0, "{counts:?}");
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
    fn a_destructuring_tuple_assignment_is_reported() {
        // Codex, PR #183's own review: `(dispatch.bytes, dispatch.intent) = (a, b);` is valid
        // Rust — destructuring assignment stabilized in 1.59 — and its left side parses as an
        // `Expr::Tuple` of field accesses, not the one `Expr::Field` chain `note` matches. The
        // default traversal `visit_expr_assign` falls back to would walk each field as a
        // plain read, with nothing to say either one is being written to.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, a: Bytes, b: Id) {\n\
             \x20   (dispatch.bytes, dispatch.intent) = (a, b);\n}",
            &["bytes", "intent"],
        )
        .expect("the fixture parses");
        let mut sorted = found;
        sorted.sort_unstable();
        assert_eq!(sorted, ["bytes", "intent"], "{sorted:?}");
    }

    #[test]
    fn a_destructuring_struct_assignment_is_reported() {
        // The struct-pattern shape of destructuring assignment: `Wrap { field: dispatch.bytes,
        // .. } = source;` reuses struct-literal syntax as an assignment target, with each
        // field's expression as the place actually being written to.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, source: Wrap) {\n\
             \x20   Wrap { field: dispatch.bytes, .. } = source;\n}",
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
    fn a_parenthesized_field_behind_a_mutable_reference_is_reported() {
        // Codex, PR #183's own review: `&mut (dispatch.bytes)` is valid Rust, and the parens
        // are their own `Expr::Paren` node — `note`'s own loop matched only `Expr::Field`
        // directly, so `core::mem::replace(&mut (dispatch.bytes), other)` rewrote the field
        // with the gate green.
        let found = mutated_field_names(
            "fn tamper(mut dispatch: Foo, other: Bytes) {\n\
             \x20   core::mem::replace(&mut (dispatch.bytes), other);\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["bytes"], "{found:?}");
    }

    #[test]
    fn a_parenthesized_base_in_a_field_chain_is_reported() {
        // The parens can sit around an intermediate base too, not only the whole expression:
        // `(dispatch).intent` is the same place as `dispatch.intent`.
        let found = mutated_field_names(
            "fn tamper(dispatch: Foo) {\n\
             \x20   let r = &mut (dispatch).intent;\n}",
            &["intent"],
        )
        .expect("the fixture parses");
        assert_eq!(found, ["intent"], "{found:?}");
    }

    #[test]
    fn a_compound_assignment_to_a_field_is_reported() {
        // Codex, PR #183's own review: `dispatch.intent.request.input_crc ^= other;` parses
        // as `Expr::Binary` with `BinOp::BitXorAssign`, not `Expr::Assign` — `visit_expr_assign`
        // never sees it, and the default traversal walks the field chain as a plain read.
        let found = mutated_field_names(
            "fn tamper(dispatch: &mut Foo, other: u32) {\n\
             \x20   dispatch.intent.request.input_crc ^= other;\n}",
            &["intent", "request"],
        )
        .expect("the fixture parses");
        let mut sorted = found;
        sorted.sort_unstable();
        assert_eq!(sorted, ["intent", "request"], "{sorted:?}");
    }

    #[test]
    fn every_compound_assignment_operator_is_reported() {
        for op in ["+=", "-=", "*=", "/=", "%=", "^=", "&=", "|=", "<<=", ">>="] {
            let found = mutated_field_names(
                &format!(
                    "fn tamper(dispatch: &mut Foo, other: u32) {{\n\
                     \x20   dispatch.bytes {op} other;\n}}"
                ),
                &["bytes"],
            )
            .unwrap_or_else(|error| panic!("{op} fixture parses: {error}"));
            assert_eq!(found, ["bytes"], "operator {op}: {found:?}");
        }
    }

    #[test]
    fn a_plain_binary_comparison_on_a_field_is_not_reported() {
        // `dispatch.bytes == other` is a read, and `BinOp::Eq` is not on the compound-
        // assignment list — this is the control that shows the new check is scoped to
        // assignment operators rather than to every `Expr::Binary`.
        let found = mutated_field_names(
            "fn read(dispatch: &Foo, other: u32) -> bool {\n\
             \x20   dispatch.bytes == other\n}",
            &["bytes"],
        )
        .expect("the fixture parses");
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_ref_mut_let_binding_of_a_field_chain_is_reported() {
        // Codex, PR #183's own review: `let ref mut slot = dispatch.intent.request;` borrows
        // the initializer's place directly through the pattern, with no struct pattern for
        // `visit_field_pat` to read and no `=`/`&mut`/method call for the other three routes.
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
