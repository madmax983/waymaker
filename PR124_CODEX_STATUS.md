# Waymaker PR #124 — Codex Feedback Status

**PR**: https://github.com/madmax983/waymaker/pull/124
**Branch**: `fix/review-batch-integrated` (review branch; the remote ref is
force-moved by the PR watch via `gh_branch.py`, so the remote is a single
squashed commit — the full history lives in
`~/workspace/waymaker/batch-integrated`)
**Base**: `main`

## New Codex findings (codex-watch run, 2026-09-12, review 5187982830) — all handled

### Automated-review header card (review 5187982830) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — treat externally defined macros as unresolved (review_comment 3997510667) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: the definition scan examines only `macro_rules!` bodies in
this crate, so a macro exported by a dependency is invisible to it — and an
argument-free invocation carries no `mod`/`include!` tokens for the
invocation-token scan either. `use dependency::load_startup;` +
`load_startup!();`, expanding in the dependency to `#[path = "xtensa.rs"] mod
arm_startup;`, compiles the gated file's hand-written `unsafe` into an ARM
image the separately gated `mod xtensa;` still excuses. RED first: the three
new refusal tests failed with 0 violations before the fix, exactly the miss.

**Fix — and why not blanket fail-closed**: failing closed on *every*
unresolved invocation would over-block legitimate cases, which the finding
itself invites checking. The real firmware invokes compiler builtins
(`format_args!`, `asm!`, `naked_asm!`, `core::ptr::addr_of_mut!`) whose
expansions are fixed by rustc and contain no items at all — refusing the
exemption on those buys zero soundness at pure false-positive cost — plus one
genuinely external macro, `hprintln!` from `cortex-m-semihosting`. So the walk
now accounts for every non-`include!` invocation instead of merely token
scanning it (`macro_is_accounted_for`):
- **compiler builtins** (`BUILTIN_MACROS`): fixed expansions, no `mod`/`include!`
  possible — ignored, but only when unshadowed (no local `macro_rules!` with the
  name, no `use` importing it from another crate, no `#[macro_use] extern
  crate` whose textual-scope macros could shadow a builtin);
- **crate-local `macro_rules!`** (collected crate-wide by `local_macro_names`):
  body already scanned by the definition walk, invocation tokens still scanned
  as before (the forwarding-wrapper case);
- **pinned external exemptions** (`EXTERNAL_MACRO_EXEMPTIONS`): `(crate, name)`
  pairs verified against the Cargo.lock-pinned dependency source —
  `cortex-m-semihosting 0.5.0`'s `hprintln!` expands to
  `$crate::export::hstdout_str` / `hstdout_fmt` (read in the registry source:
  no `mod`, no `include!`). Resolved through the file's `use` imports
  (`external_macro_imports`, renames honored) or a path rooted at the exempted
  crate; a bare name is never trusted. Re-verify on a version bump.
- **anything else** → `unresolvable`, exemption refused (fail closed).

Also: `visit_item_macro` now returns right after scanning a `macro_rules!`
definition's body instead of feeding the definition itself to `record` (the
`macro_rules` path would otherwise fail closed as an external-looking name).

**Tests** (RED first — the 3 refusal tests failed with 0 violations before the
fix; the 4 keep-green tests passed before and after):
`an_external_macro_invocation_with_no_visible_tokens_refuses_the_exemption`
(the exact Codex case), `an_unknown_bare_macro_invocation_refuses_the_exemption`,
`an_external_crate_shadowing_a_builtin_name_refuses_the_exemption` (a `use`
importing a builtin name from another crate is not the builtin),
`a_builtin_macro_invocation_keeps_the_exemption` (no false positive on
`format_args!`/`concat!`/`vec!`/`core::ptr::addr_of_mut!`),
`the_pinned_external_macro_exemption_keeps_the_exemption` (the real crate's
`hprintln!` case), `a_renamed_import_of_the_exempted_external_macro_keeps_the_exemption`,
`a_local_macro_invocation_with_clean_tokens_keeps_the_exemption`.

**Real-crate check**: ran `check_no_handwritten_unsafe` over the actual
`crates/waymaker-emu/src/*.rs` — 0 violations; the gate stays green on the
real firmware (temporary verification test, removed after passing).

**Verification (this run)**: `cargo fmt --all --check` clean;
`cargo clippy --locked --workspace --all-targets -- -D warnings` clean;
`cargo test --locked -p xtask` all green — 1060 lib tests (incl. the 7 new
ones) + 19 + 35 integration tests; `cargo xtask check-layering` ok (56 rules).

## New Codex findings (codex-watch run, 2026-09-12, review 5187834130) — all handled

### Automated-review header card (review 5187834130) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — inspect macro-generated modules before exempting unsafe (review_comment 3997403828) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: the opaque-token scan (`tokens_could_include`) rejected only
`include!`-shaped invocations, but a `macro_rules!` body expanding to
`#[path = "xtensa.rs"] mod arm_startup;` compiles the gated file on ARM while
`syn` leaves the body opaque — the definition is an `ItemMacro`, not an
`ItemMod`, and no invocation is a `mod` item either, so the
module-declaration walk never sees the expansion and the separately gated
`mod xtensa;` still excuses the file's hand-written `unsafe`. Verified
empirically against rustc (system toolchain): a scratch program with that
macro shape compiles `xtensa.rs` into the binary on invocation, and so does
the forwarding-wrapper form `pass!(#[path = "xtensa.rs"] mod arm_startup;)`
with `macro_rules! pass { ($($t:tt)*) => { $($t)* } }` — the same hole at the
invocation-token call site added for review_comment 3996149092. RED first:
the three new refusal tests failed with 0 violations before the fix, exactly
the miss.

**Fix**: per the comment's "fail closed, just as the checker does for opaque
inclusions" option — new `tokens_could_declare_module` scans opaque token
streams for the `mod` keyword at any group nesting depth, and any `mod` shape
in a `macro_rules!` body or a non-`include` invocation's tokens marks the
file's inclusions unresolvable, refusing the exemption. Both call sites
(`visit_item_macro`'s definition scan and `record`'s invocation-token scan)
check it alongside `tokens_could_include`. The scan deliberately does not
distinguish a file declaration (`mod arm_startup;`) from an inline module
(`mod wrapper { ... }`): an inline module declares no file and cannot reach
the gated file, but telling them apart in opaque tokens is exactly the
resolution the checker cannot do — and the error direction stays fail-closed,
matching the established posture (any-file `include!` shapes, unevaluated
`cfg_attr` predicates, file-wide alias collection). `mod` is a strict
keyword, so the identifier can only ever be a module item. Doc comments on
`xtensa_gated_module`, `include_declarations`, `record`, `visit_item_macro`,
and `tokens_could_include` updated; the review_comment id is cited at each
touched spot. The real firmware uses no `macro_rules!` at all, so nothing
in-tree trips the widened scan.

**Tests** (RED first — all three refusal tests failed with 0 violations before
the fix; the keep-the-exemption tests passed before and after):
`a_macro_rules_body_hiding_a_module_declaration_of_the_gated_file_refuses_the_exemption`
(the exact Codex case),
`a_macro_rules_body_hiding_a_module_declaration_of_another_file_refuses_the_exemption`
(any `mod` shape fails closed — documents the conservative trade-off),
`a_macro_invocation_hiding_a_module_declaration_refuses_the_exemption`
(the forwarding-wrapper form at the invocation-token call site).

**Verification (this run)**: `cargo fmt --all --check` clean;
`cargo clippy --locked --workspace --all-targets -- -D warnings` clean;
`cargo test --locked -p xtask` all green — 1053 lib tests (incl. the 3 new
ones) + 19 + 35 integration tests; `cargo xtask check-layering` ok (56 rules).

## New Codex findings (codex-watch run, 2026-09-12, review 5187652473) — all handled

### Automated-review header card (review 5187652473) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — recognize qualified aliases of the include macro (review_comment 3997221943) — FIXED

**File**: `xtask/src/emulate.rs` (was an uncommitted working-tree change; this
run verified, gated, and pushed it)

**Verified real**: the parsed-invocation check in `Inclusions::record` matched a
collected alias (`use core::include as paste;`) only on a single-segment path,
on the assumption that a rename is a name binding and qualified paths cannot
resolve through it. Verified empirically against rustc: `mod wrapper {
use core::include as paste; self::paste!("xtensa.rs"); }` compiles and pastes
the file, as does `crate::paste!` through a `pub use` re-export. So an
ARM-reachable qualified alias could paste the gated file's hand-written
`unsafe` into an ARM image while the Xtensa exemption stood on the gated
`mod xtensa;` alone.

**Fix**: the alias now matches on the macro path's last segment, exactly like
the literal `include` check already did — any path ending in a collected alias
counts as an inclusion. The error direction stays fail-closed (the checker
cannot prove which binding a qualified path names; an unrelated macro sharing
the name only costs an extra inclusion record, never a lost one). Doc comments
on `record` and `include_aliases` updated; the review_comment id is cited at
each touched spot.

**Tests** (RED first — both refusal tests failed with 0 violations against the
old single-segment logic, confirming the miss; the keep-the-exemption test
passed before and after): `a_self_qualified_alias_include_macro_refuses_the_exemption`
(the exact Codex case), `a_crate_qualified_alias_include_macro_refuses_the_exemption`
(the re-export case), `a_self_qualified_alias_include_macro_naming_another_file_keeps_the_exemption`
(no false positive when the qualified alias names a different file).

**Verification (this run)**: `cargo fmt --all --check` clean;
`cargo clippy --locked --workspace --all-targets -- -D warnings` clean;
`cargo test --locked -p xtask` 1050 lib tests (incl. the 3 new ones) + 19 + 35
integration tests, all green; `cargo xtask check-layering` ok (56 rules).
Pushed `fix/review-batch-integrated` via `gh_branch.py` (remote commit
f0b8cac58bac3563583dc71b3fbab544f054062d, parented on main; main untouched),
posted PR comment issuecomment-5648348310, and marked review 5187652473 /
review_comment 3997221943 handled in `pr-watch/state.json`.

## New Codex findings (codex-watch run, 2026-09-12, review 5187408849) — all handled

### Automated-review header card (review 5187408849) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — track include aliases inside opaque macro bodies (review_comment 3997052796) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: the opaque-token scan `tokens_could_include` recognized only
the literal identifier `include`, while the parsed-invocation alias handling
(`use core::include as paste;` making `paste!(...)` an inclusion, added for
review_comment 3996723105) never sees invocations hidden in opaque macro
bodies. Verified empirically with the system toolchain: a scratch program with
`use core::include as paste;` plus a `macro_rules!` body containing
`paste!("gated.rs")` compiles and the invocation pastes the file — rustc
resolves the alias textually at the definition site. So an ARM-reachable file
with that shape pasted the gated file's hand-written `unsafe` into an ARM
image while the exemption stood on the gated `mod xtensa;` alone. The same
hole existed at the scan's other call site: the invocation-token scan in
`record` (added for review_comment 3996149092) also saw only the literal
`include`, so `pass!(paste!("xtensa.rs"))` slipped past too. A grep confirmed
these were the only hard-coded `include`-identifier checks: `record`'s path
check was already alias-aware and the alias collector must match the builtin's
own name.

**Fix**: per the comment's "pass the collected aliases into this scan" option —
`tokens_could_include` now takes the file's `include_aliases` and matches the
identifier `include` or any alias immediately followed by `!`, at any group
nesting depth. Both call sites pass the aliases (`visit_item_macro`'s
`macro_rules!`-body scan and `record`'s invocation-token scan, plus the
recursion). Like the literal shape, any `<alias>!` shape fails closed — token
text cannot see paths or hygiene, so the scan does not try to tell a real
renamed inclusion from another macro sharing the name; the error direction
stays fail-closed, matching the established posture for unmeasurable
inclusions. A bare alias identifier not followed by `!` is not an inclusion.
Doc comments on `include_declarations` and `tokens_could_include` updated; the
new review_comment id is cited at each touched spot. The real firmware uses no
renamed `include!` imports, so nothing in-tree trips the widened scan.

**Tests** (RED first — both refusal tests failed with 0 violations before the
fix, exactly the miss; the keep-the-exemption test passed before and after):
`an_aliased_include_macro_in_a_macro_rules_body_refuses_the_exemption` (the
exact Codex case),
`an_aliased_include_macro_in_a_wrapper_invocation_refuses_the_exemption` (the
same hole at the invocation-token call site),
`a_macro_rules_body_with_an_unused_alias_keeps_the_exemption` (alias imported
but only ever a bare identifier, never invoked — no false positive).

**Verification (this run)**: `cargo fmt --all --check` clean;
`cargo clippy --locked --workspace --all-targets -- -D warnings` clean;
`cargo test --locked -p xtask` 1047 lib tests (incl. the 4 new ones) + 19 + 35
integration tests, all green; `cargo xtask check-layering` ok (56 rules).
This run also committed the companion staged fix for review_comment 3996992622
(nested-module alias collection in `include_aliases`, test
`a_nested_module_aliasing_include_macro_refuses_the_exemption`), whose
state.json entry was already marked handled but whose fix had never landed;
it is covered by the same gate run.

## New Codex findings (review 5186362411) — all handled

### Automated-review header card (review 5186362411) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — `include!` nested in macro invocation arguments (review_comment 3996149092) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: the inclusion walk's `record` returned early for any macro
invocation whose path was not `include`, never inspecting its token stream — so
`macro_rules! pass { ($($t:tt)*) => { $($t)* } }` followed by
`pass!(include!("xtensa.rs"))` slipped past both the invocation walk (the
invocation's path is `pass`) and the `macro_rules!`-body scan (the definition's
body is just `$($t)*`, no `include!` shape). rustc expands the forwarding wrapper
into the inclusion, pasting the gated file's hand-written `unsafe` into an ARM
image while the exemption stood on the gated `mod xtensa;` alone.

**Fix**: per the comment's "scan non-`include` macro invocation tokens as well"
option — the established posture for unmeasurable inclusions (computed
`include!` paths, `include!`-shaped `macro_rules!` bodies). `record` now scans a
non-`include` invocation's token stream with `tokens_could_include` and fails the
exemption closed when an `include!` shape is present: the wrapper's expansion is
opaque (it may drop, reorder, or paste the tokens), so such an invocation is an
inclusion the checker cannot enumerate. Doc comments on `include_declarations`
and `tokens_could_include` updated to cover invocation tokens. The real firmware
uses no such pattern (no `include!` or `macro_rules!` in `waymaker-emu`), so
nothing in-tree trips the new check.

**Tests** (RED first — the two refusal tests failed with 0 violations before the
fix, exactly the miss; the keep-the-exemption test passed before and after):
`a_macro_invocation_hiding_an_include_of_the_gated_file_refuses_the_exemption`
(the exact Codex case),
`a_macro_invocation_hiding_an_include_of_another_file_refuses_the_exemption`
(any `include!` shape fails closed — documents the conservative trade-off),
`a_macro_invocation_without_an_include_keeps_the_exemption` (a wrapper
invocation with no `include!` in its tokens is not an inclusion — no false
positive).

**Verification (this run)**: `cargo fmt --all --check` clean;
`cargo clippy --locked --workspace --all-targets -- -D warnings` clean;
`cargo test --locked -p xtask` 1034 lib tests (incl. the 3 new ones) + 19 + 35
integration tests, all green; `cargo --locked xtask check-layering` ok (56
rules).

### Automated-review header card (review 5186071804) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — align the BSS end before clearing words (review_comment 3995859254) — FIXED

**File**: `crates/waymaker-emu/memory-xtensa.x`

**Verified real**: `.bss (NOLOAD) : ALIGN(4)` aligned only the output section's
start, so a final input with byte or halfword alignment could leave `_ebss`
non-word-aligned — and `zero_bss()` clears whole `u32` words, so its last
volatile write would clear up to three bytes past `.bss`.

**Fix**: `. = ALIGN(4);` before `_ebss = .;` (rounds the end up to a word
boundary; the rounded-up bytes sit in unlinked DRAM, which nothing else uses).
`zero_bss()` itself needed no change: with both ends word-aligned, its word
loop is exact. This is the only linker script with the pattern (no other
`.x`/`.ld` in the repo defines `_ebss`).

**Tests** (RED first — failed before the fix): `the_xtensa_linker_script_word_aligns_the_bss_end`
(asserts the `.bss` body rounds its end up to a word before defining `_ebss`).

**Verification (this run)**: `cargo fmt --all --check` clean;
`cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings` clean;
`cargo test --locked -p xtask` 1028 passed, 0 failed;
`cargo --locked xtask check-layering` ok (56 rules).

### P2 — account for include! before granting the unsafe exemption (review_comment 3995859257) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: the `emulation-boot` unsafe gate's exemption walk
(`module_declarations`) visited only `mod` items (`ItemMod`); an ARM-reachable
`include!("xtensa.rs")` is an `ItemMacro` the walk never saw, so it pasted the
gated file's hand-written `unsafe` into an ARM image with no declaration the
gate check could see — the exemption stood on the gated `mod xtensa;` alone.

**Fix**: new `include_declarations` visitor enumerates `include!` invocations in
item, statement, and expression position, resolves each against the including
file's directory (rustc resolves `include!` against the containing file, not
any module directory), and `xtensa_gated_module` now requires the
`#[cfg(target_arch = "xtensa")]` gate on every inclusion reaching the file — a
gated inclusion alone can also carry the exemption. A non-literal argument
(e.g. `include!(concat!(...))`) names a file the checker cannot enumerate and
fails closed, like an unparsable file. `include_str!`/`include_bytes!` produce
data, not compiled code, so they are not declarations. The real firmware uses
no `include!`, so nothing in-tree trips the new check.

**Tests** (RED first — the refusal tests failed before the fix):
`an_ungated_include_macro_reaching_the_gated_file_refuses_the_exemption`,
`a_gated_include_macro_keeps_the_exemption` (gated inclusion alone carries the
exemption), `an_include_macro_naming_another_file_keeps_the_exemption` (no
false positive), `an_include_macro_in_a_nested_module_file_resolves_against_the_file`
(resolves against the file's directory, not the module directory),
`an_include_macro_with_a_computed_path_refuses_the_exemption` (fail-closed).

**Verification (this run)**: `cargo fmt --all --check` clean;
`cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings` clean;
`cargo test --locked -p xtask` 1028 passed, 0 failed;
`cargo --locked xtask check-layering` ok (56 rules).

## New Codex findings (review 5185952365) — all handled

### Automated-review header card (review 5185952365) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — collect every nested cfg_attr path, not just the first (review_comment 3995728343) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: `cfg_attr_applied_path` used `find_map` over the attributes a
`#[cfg_attr]` applies, so one outer `cfg_attr` holding several nested `cfg_attr`
alternatives returned only the first alternative's `path`. Concretely,
`#[cfg_attr(target_arch = "arm", cfg_attr(any(), path = "decoy.rs"), cfg_attr(all(), path = "xtensa.rs"))] mod arm_startup;`
compiles `xtensa.rs` on ARM (rustc expands each nested `cfg_attr` in place once
the outer predicate holds), but the checker saw only `decoy.rs` and granted the
unsafe exemption. RED first: the new refusal test failed with 0 violations
before the fix, exactly the miss.

**Fix**: `cfg_attr_applied_path` → `cfg_attr_applied_paths`, a collector that
appends every `path = "..."` at every nesting depth instead of returning the
first; `cfg_attr_path_value` → `cfg_attr_path_values` returning `Vec<String>`.
Both call sites (`module_resolves_to` for declarations, `module_declarations`
for inline-module directory candidates) switched from `filter_map` to
`flat_map`, so they see every alternative automatically. A nested list that
fails to parse is now skipped rather than aborting the whole search — an
unreadable branch must not hide the readable alternatives. The recursion only
adds reach the module could have, never removes it, so the exemption stays
fail-closed.

**Tests** (RED first — the refusal test failed with 0 violations before the
fix): `a_later_nested_cfg_attr_alternative_naming_the_gated_file_refuses_the_exemption`
(the exact Codex case: inactive first branch naming `decoy.rs`, active later
branch naming `xtensa.rs`), `every_nested_cfg_attr_alternative_path_is_collected`
(AST-level: parses the attribute and asserts both paths are collected in
order).

**Verification (this run)**: `cargo fmt --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test -p xtask` all green; `cargo xtask check-layering` ok.

## New Codex findings (review 5185904794) — all handled

### Automated-review header card (review 5185904794) — nothing actionable

The review body is Codex's automated-review header for commit `f3f1f5833b`; it
contains no findings or suggestions, so there was nothing to fix.

### P2 — `#[cfg_attr(.., cfg_attr(.., path = ..))]` nested inside `cfg_attr` (review_comment 3995679544) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: `cfg_attr_path_value` parsed only the top-level `Meta::NameValue`
items of a `#[cfg_attr]`'s applied attributes, so
`#[cfg_attr(target_arch = "arm", cfg_attr(any(), path = "xtensa.rs"))] mod
arm_startup;` fell back to resolving `arm_startup.rs` — while rustc expands the
inner `cfg_attr` once the outer predicate holds and loads `src/xtensa.rs` on ARM.
The checker then saw only the separately gated `mod xtensa;` and granted the unsafe
exemption. RED first: both new refusal tests failed with 0 violations before the
fix, exactly the miss.

**Fix**: new `cfg_attr_applied_path` helper recurses into nested `cfg_attr` meta
lists, skipping each level's predicate (the first comma-separated item, which the
checker does not evaluate) and returning the first `path = "..."` at any depth —
depth-first, matching rustc's in-place expansion order. The recursion only adds
reach the module could have, never removes it, so the exemption stays fail-closed;
both call sites (`module_resolves_to` for declarations, `module_declarations` for
inline-module directory candidates) get the recursion automatically.

**Tests** (RED first — both refusal tests failed with 0 violations before the fix):
`a_nested_cfg_attr_path_reaching_the_gated_file_refuses_the_exemption` (the exact
Codex case), `a_doubly_nested_cfg_attr_path_reaching_the_gated_file_refuses_the_exemption`
(three levels deep), `a_nested_cfg_attr_path_naming_another_file_keeps_the_exemption`
(nested path naming a different file keeps the exemption — no false positive).

**Verification (this run)**: `cargo fmt --check` clean;
`cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test -p xtask` all green — 1020 lib tests (incl. the 3 new ones), 19 + 35
integration tests; `cargo xtask check-layering` ok (56 rules).

## New Codex findings (review 5185764758) — all handled

### Automated-review header card (review 5185764758) — nothing actionable

The review body is Codex's automated-review header for commit `62fbee6a6a`; it
contains no findings or suggestions, so there was nothing to fix.

### P2 — `#[path]` inside inline modules resolved against the wrong directory (review_comment 3995553789) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: `module_declarations` already computed the inline module's
directory, but `module_resolves_to` joined every `#[path]` to the declaring
file's directory. Verified against rustc (nightly): `mod wrapper {
#[path = "../xtensa.rs"] mod arm_startup; }` in `src/main.rs` resolves the path
against `src/wrapper/` (rustc reports `couldn't find file
'src/wrapper/../xtensa.rs'` when it is absent), while the checker normalized
`src/../xtensa.rs` to `xtensa.rs` — missing that `src/xtensa.rs` is
ARM-reachable and granting the unsafe exemption on the gated root declaration
alone. Also verified: `#[path]` on the inline module itself redirects its
directory for nested declarations (`#[path = "custom/"] mod wrapper` makes a
nested `#[path = "nested.rs"]` load `src/custom/nested.rs`), including for
nested plain `mod` items.

**Fix**: `module_declarations` now yields a third element per declaration — the
directory `#[path]` spellings resolve against: the innermost enclosing inline
module's directory (a `#[path]` on the inline module redirects it, matching
rustc), or the declaring file's directory at the file's top level. The visitor
tracks inline nesting via `dir_stack` depth; `module_resolves_to` joins
`#[path]`/`#[cfg_attr(.., path)]` values to that directory instead of the file
directory. Known limitation, documented in the function docs: a
`#[cfg_attr(.., path = "...")]` on an inline module itself is now modeled (see the
new P2 section below, review_comment 3995614205): each conditional path adds a
candidate directory for the nested declarations.

**Tests** (RED first — the three refusal tests failed with 0 violations before
the fix): `an_ungated_path_attribute_in_an_inline_module_refuses_the_exemption`
(the exact Codex case), `a_gated_path_attribute_in_an_inline_module_keeps_the_exemption`,
`an_ungated_path_attribute_in_a_doubly_nested_inline_module_refuses_the_exemption`
(`../../xtensa.rs` through `mod outer { mod inner { ... } }`),
`a_path_attribute_on_an_inline_module_redirects_nested_resolution`.

## New Codex findings (review 5185221629) — all handled

### P2 — `.rodata` at instruction-bus addresses (review_comment 3995126210) — FIXED

**Files**: `crates/waymaker-emu/memory-xtensa.x`, `xtask/src/emulate.rs`

**Verified real**: the project's own model says the S3's instruction SRAM is
fetch-only for ordinary loads/stores (only `l32r` literal loads reach it), and
the script's own header notes blocks 0-1 have no data-bus address at all. The
guest's byte-addressed constants — the format strings `report()`/`panic()`
print the census with, read by `Writer::write_str` via ordinary byte loads —
sat in `.rodata` inside the IRAM output section, so the first census byte
would fault (LoadProhibited) on silicon before the census was complete. This
also matches esp-hal practice (`.rodata` in DRAM).

**Fix**: `.rodata` gets its own output section mapped `> DRAM`; the IRAM
`.text` section keeps only `.literal` pools and `.text`. Header comment and
the `XTENSA_LINK_ARGS` doc comment updated to match.

**Test**: new `the_xtensa_linker_script_keeps_rodata_in_the_data_bus_view`
parses the script's `SECTIONS` block (`xtensa_output_sections` helper) and
asserts `.rodata` is collected by exactly one output section, that it is
DRAM-mapped, and that no IRAM-mapped section collects it.

### P2 — `#[cfg_attr(.., path = ..)]` hole in the unsafe exemption (review_comment 3995126216) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: `module_resolves_to` read only plain `#[path]`, so
`#[cfg_attr(target_arch = "arm", path = "xtensa.rs")] mod arm_startup;` in a
root resolved to `arm_startup.rs` by default, the exemption loop skipped it,
saw only the gated `mod xtensa;`, and granted the exemption — while the ARM
image compiles the hand-written `unsafe` file. Same class of hole as the
plain-`#[path]` fix it follows.

**Fix**: new `cfg_attr_path_value` reads the `path = "..."` off the
attributes a `#[cfg_attr]` applies (the first item is the predicate, which the
checker does not evaluate); `path_value` now shares the string extraction via
`meta_str_value`. `module_resolves_to` reads conditional paths FIRST — a
conditional path naming the file counts as reaching it no matter what a plain
`#[path]` beside it says (rustc applies both in order and the unevaluated
predicate may be active) — then the plain `#[path]` override, then the default
name. Fail-closed direction preserved: the gate required is still exactly
`#[cfg(target_arch = "xtensa")]`.

**Tests**: `a_cfg_attr_path_reaching_the_gated_file_refuses_the_exemption`,
`a_plain_path_beside_a_cfg_attr_path_naming_the_gated_file_refuses_the_exemption`
(locks the conditional-first ordering),
`a_cfg_attr_path_naming_another_file_keeps_the_exemption`,
`a_gated_cfg_attr_path_keeps_the_exemption`.

## Earlier findings (reviews 5184497214, 5184588307) — all handled

### P2 — Stack-pointer switch in ordinary `_start` (review_comment 3994472340) — DECLINED, documented

**Files**: `crates/waymaker-emu/src/xtensa.rs`, `crates/waymaker-emu/src/main.rs`

Codex suggested a naked-assembly trampoline instead of switching Xtensa `a1`
inside the ordinary Rust `_start()` via inline `asm!`.

**Why declined**: on Xtensa's windowed ABI the `call8` into `firmware_main`
requires the caller to have executed `entry` first, and a naked function cannot
emit that prologue without hand-writing the very thing it was meant to avoid. A
naked `l32r`/`call8` without `entry` was actually tried: the image built and the
entry disassembly looked right, but the guest faulted in QEMU — the windowed ABI
needs the `entry` the naked form omits. The inline form is sound here: `_start`
has no locals and nothing is live across the `asm!` (which carries
`options(nostack, nomem)`), so the compiler cannot emit spills or stack-relative
accesses around it; the only subsequent effect is the diverging call into
`firmware_main`, which correctly uses the new stack. The stack address itself is
materialized by the compiler (`top = in(reg) STACK_TOP`), not by a hand-written
`l32r` whose relocations the Xtensa linker mangles.

**Docs**: the rationale is written into the `_start` doc comment and the
`main.rs` `cfg_attr` comment, so a future reader doesn't "simplify" it back.

**Test**: `the_xtensa_start_switches_stacks_without_a_naked_trampoline` locks the
design in (no `#[unsafe(naked)]` on `_start`, rationale documented, `mov a1`
with compiler-materialized input, no hand-written `l32r`).

### P2 — `#[path]` hole in the unsafe exemption (review_comment 3994472345) — FIXED

**File**: `xtask/src/emulate.rs` (committed as `289e14c`)

`xtensa_gated_module` only resolved plain `mod <ident>;` declarations, so an
ungated `#[path = "xtensa.rs"] mod arm_startup;` would include the same file in
an ARM image where the hand-written `unsafe` has no exception to stand under.

**Fix**: `module_resolves_to` honors `#[path]` (read off the parsed
`Meta::NameValue`, string literal only; `#[path]` on the gated declaration
itself names the gated file), `normalize_path` folds `.`/`..` segments so
non-canonical spellings resolve identically to rustc, inline modules resolve to
no file, and the exemption now requires *every* declaration resolving to the
file to carry `#[cfg(target_arch = "xtensa")]`. A root that doesn't parse still
fails closed.

**Tests**: `an_ungated_path_attribute_reaching_the_gated_file_refuses_the_exemption`,
`a_noncanonical_path_spelling_reaching_the_gated_file_refuses_the_exemption`,
`a_gated_path_attribute_keeps_the_exemption`.

### P2 — IRAM/DRAM alias the same physical SRAM (review_comment 3994548771) — FIXED

**Files**: `crates/waymaker-emu/memory-xtensa.x`, `crates/waymaker-emu/src/xtensa.rs`,
`xtask/src/emulate.rs`

The linker script granted the linker both bus views at their full mapped ranges
(IRAM `0x40370000+0x70000`, DRAM `0x3FC88000+0x78000`). The two views alias the
same physical HP SRAM, so the linker was free to place `.text` at the same
physical address as `.data`/`.bss` — loading data or `zero_bss()` would then
overwrite instructions.

**Fix**: the script now grants a non-overlapping split of the physical SRAM:

- IRAM (instruction-bus view) `ORIGIN=0x40370000, LENGTH=0x20000` (128 KiB)
  → physical `0x3FC80000..0x3FCA0000`: `.literal`/`.text`/`.rodata`
- DRAM (data-bus view) `ORIGIN=0x3FCA0000, LENGTH=0x50000` (320 KiB)
  → physical `0x3FCA0000..0x3FCF0000`: `.data`/`.bss`

Adjacent and disjoint in physical SRAM, so the bound is structural — the linker
cannot alias them. Data stays in the data-bus view on purpose: the S3's
instruction SRAM is fetch-only for ordinary loads/stores, so `.data`/`.bss` in
IRAM would fault on silicon the first time a static is touched.

`STACK_TOP` (`0x3FCFFFE0`) is deliberately *not* a linker region: it sits in the
data-bus window's top 64 KiB (physical `0x3FCF0000..0x3FD00000`), above the DRAM
region's physical end (`0x3FCF0000`) and with no instruction-bus address, so the
downward-growing stack can never physically overlap the linked image.

**Test**: `the_xtensa_linker_script_stays_inside_mapped_sram` now parses both
regions, asserts each stays inside its TRM window (TRM Table 13-3), asserts the
two regions are disjoint in the shared physical frame, and asserts `STACK_TOP`
is 32 B below the data-bus window top and at/above the DRAM region end.

## New Codex findings (review 5185823146) — all handled

### Automated-review header card (review 5185823146) — nothing actionable

The review body is Codex's automated-review header for commit `16bcbaad41`; it
contains no findings or suggestions, so there was nothing to fix.

### P2 — `#[cfg_attr(.., path = ..)]` on an enclosing inline module (review_comment 3995614205) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: `module_declarations` read only a direct `#[path]` on an
inline module, so `#[cfg_attr(target_arch = "arm", path = "custom/deep")] mod
wrapper { #[path = "../../xtensa.rs"] mod arm_startup; }` resolved the nested
path against the plain `src/wrapper/` (normalizing to `xtensa.rs`, no match)
while rustc loads `src/xtensa.rs` on ARM — the checker saw only the gated root
declaration and wrongly granted the unsafe exemption. RED first: the new
refusal test failed with 0 violations before the fix, exactly the miss.

**Fix**: `module_declarations` now yields candidate directory lists per
declaration. An inline module contributes its plain directory (its own
`#[path]`, if any, redirecting it) plus one candidate per
`#[cfg_attr(.., path = "...")]` on it — predicates are still not evaluated, so
each conditional path counts as a directory the module could live in.
`module_resolves_to` tries every candidate path directory (and module
directory). Extra candidates can only add declarations that must carry the
gate, never remove them, so the exemption stays fail-closed; the previous
"not modeled" limitation documented above is gone.

**Tests** (RED first — the refusal test failed with 0 violations before the
fix): `a_cfg_attr_path_on_an_inline_module_reaching_the_gated_file_refuses_the_exemption`
(the exact Codex case), `a_gated_declaration_under_a_cfg_attr_redirected_inline_module_keeps_the_exemption`
(gated nested declaration under the same redirect keeps the exemption — no
false refusal), `a_cfg_attr_path_on_an_inline_module_not_reaching_the_file_keeps_the_exemption`
(conditional redirect naming another file keeps the exemption — no false
positive).

## New Codex findings (review 5186169273) — all handled

### Automated-review header card (review 5186169273) — nothing actionable

The review body is Codex's automated-review header for commit `85894a65df`; it
contains no findings or suggestions, so there was nothing to fix.

### P2 — `include!` hidden in a `macro_rules!` body (review_comment 3995964718) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: `syn` leaves a `macro_rules!` body as opaque tokens, so the
`include_declarations` visitor (added for review_comment 3995859257) never saw
an `include!("xtensa.rs")` nested inside one — neither the definition (an
`ItemMacro` whose path is `macro_rules`, not `include`) nor any invocation
(whose path is the macro's name) is recorded. An ARM-reachable invocation would
paste the gated file's hand-written `unsafe` into an ARM image no recorded
declaration excuses, while the exemption stood on the gated `mod xtensa;`
alone. RED first: the new refusal tests failed with 0 violations before the
fix, exactly the miss.

**Fix**: per the comment's "fail closed when they could generate an inclusion"
option — the established posture for unmeasurable inclusions (computed
`include!` paths, unparsable files). `visit_item_macro` now detects a
`macro_rules!` definition whose token tree holds an `include!`-shaped invocation
(identifier `include` immediately followed by a lone `!`, at any group nesting
depth, via the new `tokens_could_include`) and marks the file's inclusions
unresolvable, refusing the exemption. The scan deliberately does not resolve
which file the nested inclusion names — token text cannot see through nested
macro layers the way the parsed-argument walk does — so any `include!` shape
fails closed. The real firmware uses no `macro_rules!` with `include!`, so
nothing in-tree trips the new check.

**Tests** (RED first — the refusal tests failed with 0 violations before the
fix):
`a_macro_rules_body_hiding_an_include_of_the_gated_file_refuses_the_exemption`
(the exact Codex case),
`a_macro_rules_body_hiding_an_include_of_another_file_refuses_the_exemption`
(any `include!` shape fails closed — documents the conservative trade-off),
`a_macro_rules_body_without_an_include_keeps_the_exemption` (a `macro_rules!`
with no `include!` in its body is not an inclusion — no false positive).

**Dependency note**: `tokens_could_include` walks `syn::Macro`'s token stream,
which is `proc-macro2`'s type, so `proc-macro2 = "1"` is now a direct
dependency of `xtask` (already in the lock at 1.0.107; one-line `Cargo.lock`
addition, no version change).

## Earlier findings (already handled, for context)

- P1 Xtensa SRAM bounds (3994343698): DRAM/IRAM/`STACK_TOP` moved inside the TRM
  windows — superseded by the IRAM/DRAM split above.
- P2 Cargo cache path (3994299510): `esp_cargo_home` resolved from workspace root.
- P2 Unsafe exception scope (3994299513): exact-path resolution for the gated
  module — superseded by the `#[path]` hardening above.
- P2 Signed whole-image delta (3994343701): gated rows linking less than baseline
  are refused as `BudgetShortfall::Unmeasurable` (committed `289e14c`).

## Verification (this run, 2026-09-12)

- `cargo fmt --check`: clean
- `cargo clippy -p xtask --all-targets -- -D warnings`: clean
- `cargo test -p xtask`: all green — 1007 lib tests (incl. the 5 new ones),
  19 + 35 integration tests, doc-tests, exit 0
- `cargo xtask check-layering`: ok (56 rules)
- Physical ESP32-S3: not verified in this environment (QEMU only); the Espressif
  QEMU fork is unavailable in the sandbox, so guest boot remains CI's job.

## Verification (codex-watch run, 2026-09-12, review 5185823146)

- `cargo fmt --check`: clean
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- `cargo test -p xtask --lib`: all green — 1017 lib tests (incl. the 3 new ones)
- `cargo xtask check-layering`: ok (56 rules)
- Physical ESP32-S3: not verified in this environment; guest boot remains CI's job.

## New Codex findings (codex-watch run, 2026-09-12, review 5186430508) — all handled

### Automated-review header card (review 5186430508) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — composite cfg before the unsafe exemption (review_comment 3996215973) — NOT A BUG, declined with reasons

**Claim**: when a declaration uses `#[cfg(any(target_arch = "arm",
target_arch = "xtensa"))]`, `parse_nested_meta` visits both nested
`target_arch` entries and leaves `gated` true after the Xtensa entry, so the
module compiles on ARM while `xtensa_gated_module` still exempts its
hand-written `unsafe` — and the fix would be to compare the parsed meta
structurally and accept only the single predicate.

**Verified not real**: `syn`'s `parse_nested_meta` does not descend into nested
predicate groups — the callback receives one `ParseNestedMeta` per
comma-separated item at the attribute's top level, so for the `any(...)` case it
sees a single meta whose path is `any`, which is not `target_arch`, and `gated`
stays false. The exemption is therefore already refused for composite
predicates. Confirmed empirically: a regression test declaring the module with
the exact composite cfg from the comment reports the `unsafe` violation
(`hand_written_unsafe_in_a_composite_cfg_module_is_reported`), i.e. the gate
refuses the exemption exactly as the comment asks for. The suggested structural
rewrite would change no accepted case and is out of scope, so no production code
was touched.

**Tests**: `hand_written_unsafe_in_a_composite_cfg_module_is_reported` (pins the
invariant the comment questioned: a composite cfg that compiles on ARM never
carries the Xtensa exemption). It passes against the current code — it is a
characterization test, not a RED/GREEN pair, because there was no bug to fix.

## Verification (codex-watch run, 2026-09-12, review 5186430508)

- `cargo fmt --all --check`: clean
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: clean
- `cargo test --locked -p xtask`: all green — 1035 lib tests (incl. the 1 new
  one), 19 + 35 integration tests
- `cargo xtask check-layering`: ok

## New Codex findings (codex-watch run, 2026-09-12, review 5186502367) — all handled

### Automated-review header card (review 5186502367) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — wait for the complete failure line before killing QEMU (review_comment 3996291622) — FIXED

**File**: `xtask/src/emulate.rs` (`xtensa_failed`)

**Verified real**: the guest's UART writer pushes the serial log byte by byte
(`Writer::putc` per byte in `crates/waymaker-emu/src/xtensa.rs`) while
`start_xtensa` polls it every 100 ms, and `str::lines` yields the unterminated
trailing fragment as a line — so a poll landing on `waymaker-emu: failed `
mid-write matched the detector and killed QEMU before the diagnostic and its
newline arrived, truncating the very output the detector exists to preserve.
(The census path is safe: `parse` requires the full cases+rig+ok set, so the
finding is correctly scoped to the failure detector.)

**Fix**: `xtensa_failed` now matches only terminated lines —
`split_inclusive('\n')` filtered to chunks ending in `'\n'` — so a bare
`failed `/`panicked:` prefix never fires early. The guest drains the UART
(`uart_drain`) before parking, so the newline is at most one poll away: the
kill is delayed, not risked.

**Tests** (RED first — both failed before the fix, exactly the truncation race;
both pass after, and all pre-existing failure/panic detector tests still pass):
`a_half_written_failure_line_does_not_end_the_run`,
`a_half_written_panic_line_does_not_end_the_run`.

## Verification (codex-watch run, 2026-09-12, review 5186502367)

- `cargo fmt --all --check`: clean
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: clean
- `cargo test --locked -p xtask`: all green — 1037 lib tests (incl. the 2 new
  ones), 19 + 35 integration tests
- `cargo xtask check-layering`: ok (56 rules)

## New Codex findings (codex-watch run, 2026-09-12, review 5186562619) — all handled

### Automated-review header card (review 5186562619) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — stack switch in inline `asm!` contradicts `options(nostack)` (review_comment 3996354964) — FIXED

**File**: `crates/waymaker-emu/src/xtensa.rs` (`_start`)

**Verified real**: the finding is correct and supersedes the earlier defense of
the ordinary-function form (review_comment 3994472340, which argued "no locals,
nothing live across the block" made it sound). `options(nostack)` is a promise
to the compiler that the assembly does not modify the stack pointer — replacing
`a1` contradicts that contract outright, regardless of what today's codegen
happens to emit around the block. The promise is the contract, not the codegen.

**Fix**: `_start` is now `#[unsafe(naked)]` with a `naked_asm!` template that
hand-writes the `entry a1, 32` the windowed ABI requires of a caller before a
windowed call (the earlier entry-less naked attempt built and disassembled
plausibly but faulted in QEMU), builds `STACK_TOP` from `const` immediates
(`addi`'s 12-bit signed immediate cannot add the low half `0xFFE0`, so the
template builds `STACK_TOP + 32` and subtracts 32 — no hand-written `l32r`, whose
relocations the Xtensa linker mangles), and enters Rust with `call8
{main}` (`main = sym firmware_main`; `a1` needs no handoff — the window rotation
keeps the caller's `a1` as the callee's). A naked function cannot be `-> !`, so
the signature says `()` and the never-returns is by construction: the template
ends in the diverging windowed call. `crates/waymaker-emu/src/main.rs`'s module
comment updated to match (hand-written assembly, not inline asm).

**Tests**: replaced
`the_xtensa_start_switches_stacks_without_a_naked_trampoline` with
`the_xtensa_start_installs_the_stack_in_a_naked_entry`, which asserts the naked
attribute, the hand-written `entry`, the `STACK_TOP`-derived immediates (and no
re-typed address literal), the windowed `call8` against the `firmware_main`
symbol, no `l32r`, and no `nostack`. The test fails against the old inline-asm
form by construction — it asserts `#[unsafe(naked)]`, which that form lacked —
and passes after.

**Real-hardware-path verification (this run)**: the guest was built with the
`esp` toolchain (`xtensa-esp32s3-none-elf`, `-Z build-std=core`); the entry
disassembly shows `entry a1, 32` followed by the immediate sequence computing
`a1 = 0x3FCF_FFE0 = STACK_TOP`; `esptool elf2image` packed it into the 4 MiB
flash image and the Espressif QEMU fork booted it to a full census —
`waymaker-emu: cases passed=21 exempt=2`, rig line, `waymaker-emu: ok`. The
naked entry with the hand-written `entry` runs; the contract violation is gone.

### P2 — wait for the breach detail before killing QEMU (review_comment 3996354967) — FIXED

**Files**: `crates/waymaker-emu/src/xtensa.rs` (guest),
`xtask/src/emulate.rs` (harness detector + tests)

**Verified real**: on `Trouble::Breach` the guest printed the terminated
`failed` line and then a separate `breach code=...` line, but `start_xtensa`
kills QEMU as soon as the first terminated `failed` line reaches the serial log
— a poll landing between the two writes loses the code identifying the violated
outcome. The earlier unterminated-line fix (only terminated lines count) does
not cover this: both lines are terminated; the race is between them.

**Fix**: instead of teaching the harness to wait for a second line (which would
reintroduce a wait-for-output-that-might-never-come shape), the breach code now
rides on the `failed` line itself —
`"{PREFIX} failed {message} (breach code={code})"` — so there is no second line
left to race the kill. The harness detector (`xtensa_failed`) is unchanged: it
fires on the first terminated `failed`-prefixed line, which now already carries
the code.

**Tests**: `the_xtensa_breach_code_rides_the_failed_line` (the guest prints the
code on the failed line — fails against the old guest, which had no such
format — and no standalone `breach code=` line remains),
`a_breach_failure_line_is_terminal_with_its_code` (lock-in: the detector still
recognizes the new format, and the code is inside the one terminated line the
kill waits for). Both pass.

## Verification (codex-watch run, 2026-09-12, review 5186562619)

- `cargo fmt --all --check`: clean
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: clean
- `cargo test --locked -p xtask`: all green — 1039 lib tests (incl. the 3 new
  ones), 19 + 35 integration tests
- `cargo test --locked --workspace`: all green, no failures
- `cargo xtask check-layering`: ok (56 rules)
- Xtensa guest: builds with the `esp` toolchain; entry disassembly verified;
  boots in the Espressif QEMU fork to a full passing census

## New Codex findings (codex-watch run, 2026-09-12, review 5188125246) — all handled

### Automated-review header card (review 5188125246) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — inspect procedural macro attributes before exempting unsafe (review_comment 3997629593) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: the inclusion walk's visitor overrode only `visit_item_macro`,
`visit_stmt_macro`, and `visit_expr_macro` — never `visit_attribute`. An external
procedural attribute (e.g. `use evil_startup::startup;` plus `#[startup]` on an
ARM-reachable item) expanding in its defining crate to
`#[path = "xtensa.rs"] mod arm_startup;` was therefore invisible: no `record`
call, no `unresolvable`, exemption granted. RED first: the five new refusal
tests failed with 0 violations before the fix, exactly the miss.

**Fix**: the visitor now overrides `visit_attribute` and classifies every
attribute through a new module-level `AttributeGate` (extracted, and the
`Inclusions` visitor moved to module level with it, so `include_declarations`
stays under clippy's `too_many_lines`):
- **builtin attributes** (`BUILTIN_ATTRIBUTES`): inert compiler directives —
  ignored, but only when unshadowed (a `use` importing the name from another
  crate, or an external glob import, fails closed; `#[macro_use] extern crate`
  cannot shadow an attribute, since `macro_rules!` macros are never usable as
  attributes);
- **`derive`**: each derived name must be a std derive (`STD_DERIVES`: `Clone`,
  `Copy`, `Debug`, `Default`, `Eq`, `Hash`, `Ord`, `PartialEq`, `PartialOrd`),
  whose expansions are trait impls only, unshadowed by an external import; any
  other derive (custom, crate-qualified, or an unparsable list) fails closed;
- **pinned external-attribute exemptions** (`EXTERNAL_ATTRIBUTE_EXEMPTIONS`):
  `(cortex_m_rt, entry)` — verified in the Cargo.lock-pinned
  `cortex-m-rt-macros 0.7.6` source (renames the input function, emits the
  exported trampoline, hoists `static mut` locals into explicit arguments: no
  `mod`, no `include!`), resolved through the file's `use` imports (renames
  honored) or a path rooted at `cortex_m_rt`; a bare `#[entry]` with no import
  fails closed. Re-verify against the pinned source on a version bump.
- **anything else** → `unresolvable`, exemption refused (fail closed).

**Tests** (RED first — the 5 refusal tests failed with 0 violations before the
fix; the 4 keep-green tests passed before and after):
`an_unknown_external_attribute_macro_refuses_the_exemption` (the exact Codex
case), `an_unknown_bare_attribute_macro_refuses_the_exemption`,
`an_entry_attribute_from_another_crate_refuses_the_exemption` (the (crate, name)
key is doing the work), `a_bare_entry_attribute_with_no_import_refuses_the_exemption`,
`a_shadowed_builtin_attribute_refuses_the_exemption` (a `use` importing a builtin
attribute name from another crate is not the builtin),
`an_external_derive_refuses_the_exemption` (custom derives are unexamined
expansions), `the_pinned_entry_attribute_keeps_the_exemption` (the real crate's
`#[entry]` case), `a_renamed_entry_import_keeps_the_exemption`,
`std_derives_keep_the_exemption` (the real crate's `boot.rs` derives).

**Real-crate check**: ran `check_emulation_boot` over the actual
`crates/waymaker-emu/src/*.rs` — 0 violations; the gate stays green on the real
firmware (temporary verification test, removed after passing).

### P2 — wait for the full multiline panic report (review_comment 3997629597) — FIXED

**Files**: `crates/waymaker-emu/src/xtensa.rs`; `xtask/src/emulate.rs` (detector
doc comment + contract test)

**Verified real**: this fork's `PanicInfo` `Display` writes
`panicked at <location>:` followed by a newline and then the message —
unconditionally (read in the esp toolchain's
`library/core/src/panic/panic_info.rs`, where `message()` returns
`PanicMessage` by value). The old handler's
`uprintln(format_args!("{PREFIX} panicked: {info}"))` was therefore always two
terminated lines, and `start_xtensa` kills QEMU on the first terminated
`panicked:` line — before the message bytes are written and before
`uart_drain()` runs, losing the diagnostic the detector exists to preserve.
The ARM handler is unaffected (reviewed, not changed): `debug::exit` is
synchronous semihosting, so no poll loop kills the machine mid-write.

**Fix** (Codex's first suggested option — emit the diagnostic on one line): the
panic handler now prints `{PREFIX} panicked: <message> at <location>` as
separate `uprint` pieces plus one trailing newline (new `uprint` helper;
`uprintln` refactored onto it), so no formatting step can reintroduce the
structural newline. The `panicked:` prefix stays first, so `xtensa_failed`
still recognizes the line; its doc comment now documents the one-line shape.

**Tests**: `a_guest_panic_report_is_one_line` pins the harness half of the
contract — the whole report (message and location) inside the first terminated
line, which `xtensa_failed` treats as terminal. The guest half is verified by
compiling the guest for both targets: `cargo check -p waymaker-emu --features
emu --target xtensa-esp32s3-none-elf -Z build-std=core` with the `esp`
toolchain, and `--target thumbv7em-none-eabi` with the workspace toolchain —
both pass.

## Verification (codex-watch run, 2026-09-12, review 5188125246)

- `cargo fmt --all --check`: clean
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: clean
- `cargo test --locked -p xtask`: all green — 1070 lib tests (incl. the 10 new
  ones), 19 + 35 integration tests
- `cargo xtask check-layering`: ok (56 rules)
- Xtensa guest: `cargo check` for `xtensa-esp32s3-none-elf` (esp toolchain,
  `-Z build-std=core`) and `thumbv7em-none-eabi` both pass

## New Codex findings (codex-watch run, 2026-09-12, review 5188236094) — all handled

### Automated-review header card (review 5188236094) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — recursively classify attributes emitted by cfg_attr (review_comment 3997773904) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: `AttributeGate::is_accounted_for` whitelisted `cfg_attr` as a
builtin without inspecting the attributes it emits. `#[cfg_attr(target_arch =
"arm", evil::startup)]` on an ARM-reachable item would therefore apply the
procedural macro under the predicate while the walk saw only the inert outer
name — and the expansion, invisible in the defining crate, could declare the
gated file. RED first: the new refusal test failed with 0 violations before
the fix, exactly the miss.

**Fix**: a `cfg_attr` is now accounted for only when every attribute it emits
is accounted for. `cfg_attr_is_accounted_for` parses the argument list, skips
the predicate (the first meta), re-wraps each remaining meta as an outer
attribute, and runs the whole classifier on it — shadowing checks, `derive`
handling, and nested `cfg_attr` all apply unchanged, so `#[cfg_attr(a,
cfg_attr(b, evil::startup))]` fails closed too. The checker does not evaluate
predicates, so an emitted attribute counts as applied; an unparsable argument
list fails closed. The branch sits after the import/glob shadow checks, so a
shadowed `cfg_attr` still fails closed as before.

**Tests** (RED first — the refusal test failed with 0 violations before the
fix; the keep-green test passed before and after):
`a_procedural_attribute_emitted_by_cfg_attr_refuses_the_exemption` (the exact
Codex case), `a_builtin_attribute_emitted_by_cfg_attr_keeps_the_exemption`
(the recursion is not a blanket refusal: `allow(dead_code)` inside `cfg_attr`
stays green).

### P2 — resolve external macro paths before trusting local name matches (review_comment 3997773907) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: in `macro_is_accounted_for`, a multi-segment path whose root
was neither `core`/`std` nor a pinned exemption fell through to the crate-wide
local-name check — so `dependency::load_startup!()` alongside an unrelated
local `macro_rules! load_startup` was treated as accounted for, while the
dependency's actual expansion stayed invisible and could declare the gated
file. RED first: the new refusal test failed with 0 violations before the
fix, exactly the miss.

**Fix**: the local-name shortcut now applies only to `crate::` / `self::` /
`super::` roots, which resolve inside the crate — and imports resolve first,
since `use dependency::name;` plus `self::name!()` invokes the dependency's
macro (only the pinned exemption then passes). Any other root fails closed:
an explicit external path is never excused by a same-named local macro, and a
bare leading identifier may name a local module path the checker cannot
resolve. The `core::` / `std::` and pinned-exemption arms are unchanged.

**Tests** (RED first — the refusal test failed with 0 violations before the
fix; the keep-green test passed before and after):
`an_external_macro_path_sharing_a_local_name_refuses_the_exemption` (the exact
Codex case), `a_crate_rooted_path_to_a_local_macro_keeps_the_exemption` (the
kept carve-out: `crate::load_startup!()` to a local `macro_rules!` stays
green).

### P2 — escape newlines in panic messages before printing the prefix (review_comment 3997773910) — FIXED

**File**: `crates/waymaker-emu/src/xtensa.rs`

**Verified real**: the panic handler printed `info.message()` verbatim after the
`{PREFIX} panicked:` prefix. `PanicInfo`'s `Display` was already split into
separate pieces (review 3997629597), but the message payload itself commonly
contains newlines — assertion diagnostics span lines — and one of those
terminates the prefixed line early. `xtensa_failed` kills QEMU on the first
terminated `panicked:` line, truncating the diagnostic before `uart_drain()`
runs, losing the location the one-line shape exists to preserve.

**Fix** (Codex's suggested option): the message now goes through a new
`EscapeNewlines` `fmt::Write` adapter — `\n` → `\\n`, `\r` → `\\r`, everything
else passthrough — so the report stays one terminated line with no allocator
(the guest has none). The prefix, the escaped message, ` at {location}`, and a
single trailing newline are still separate writes; the panic-handler doc
comment documents the escaping.

**Tests**: two host-verified unit tests for the adapter (the guest module is
`target_arch = "xtensa"`-gated, so they cannot run under `cargo test` here;
the exact adapter + test text was extracted mechanically and run as a host
test binary — both pass):
`embedded_newlines_in_the_panic_message_are_escaped`,
`plain_text_passes_through_unescaped`. Guest compile verified for both
targets (below); a full QEMU boot was not run in this environment.

## Verification (codex-watch run, 2026-09-12, review 5188236094)

- `cargo fmt --all --check`: clean
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: clean
- `cargo test --locked -p xtask`: all green — 1074 lib tests (incl. the 4 new
  ones), 19 + 35 integration tests
- `cargo xtask check-layering`: ok (56 rules) — includes `check_emulation_boot`
  over the real `crates/waymaker-emu/src/*.rs`: 0 violations, so the tightened
  classifier introduces no false positives on the real firmware
- Xtensa guest: `cargo check -p waymaker-emu --features emu
  --target xtensa-esp32s3-none-elf -Z build-std=core` with the `esp` toolchain,
  and `--target thumbv7em-none-eabi` with the workspace toolchain — both pass
- Escape-adapter unit tests: extracted text run as a host test binary — 2 pass
- QEMU boot of the ESP32-S3 image: not run in this environment (deferred to
  the emulate harness)

## New Codex findings (codex-watch run, 2026-09-12, review 5188402021) — all handled

### Automated-review header card (review 5188402021) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — bare external imports resolve before local macro names (review_comment 3997890781) — FIXED

**File**: `xtask/src/emulate.rs` (`macro_is_accounted_for`, single-segment arm)

**Verified real**: the bare-invocation arm consulted the crate-wide
over-approximated `local_macros` set before the file's `macro_imports`. With
an unrelated `macro_rules! load_startup` in another module and `use
dependency::load_startup;` in the ARM-reachable file, the bare
`load_startup!()` resolves to the dependency's macro — textual scope covers
only the defining module — whose expansion is invisible here and could declare
the gated file. The old order granted the exemption on the strength of the
unrelated definition. RED first: the new refusal test failed with 0 violations
before the fix, exactly the miss.

**Fix**: the imports check now runs before the local-name check (as the
`crate::`-rooted path arm already did). Failing closed is preserved for the
ambiguous case — a name that is both locally defined and imported classifies
by the import, never the local shortcut.

**Tests**: `an_imported_external_macro_beats_an_unrelated_local_definition_refuses_the_exemption`
(the exact Codex case), plus the existing keep-greens
(`a_local_macro_invocation_with_clean_tokens_keeps_the_exemption`,
`a_builtin_macro_invocation_keeps_the_exemption`,
`an_external_crate_shadowing_a_builtin_name_refuses_the_exemption`).

### P2 — include aliases restricted to the builtin macro (review_comment 3997890782) — FIXED

**File**: `xtask/src/emulate.rs` (`include_aliases`)

**Verified real**: the alias collector recorded a rename as an alias for the
builtin `include!` whenever the imported leaf was named `include`, without
checking the root. `use evil::include as paste;` plus `paste!("other.rs");`
was recorded as a resolved inclusion of `other.rs` and returned before the
external-macro classification — which would have failed closed (imports
resolve `paste` to `(evil, include)`, not the pinned exemption). The hostile
macro's expansion could include the gated file. RED first: the new refusal
test failed with 0 violations before the fix.

**Fix**: only renames rooted at `core`/`std` are collected as aliases; any
other root falls through to `macro_is_accounted_for` and fails closed like
any unexamined external macro.

**Tests**: `an_include_alias_imported_from_another_crate_is_not_an_alias`
(the exact Codex case), plus the existing keep-greens
(`an_aliased_include_macro_naming_another_file_keeps_the_exemption`,
`a_self_qualified_alias_include_macro_refuses_the_exemption`,
`a_crate_qualified_alias_include_macro_refuses_the_exemption`, …).

### P2 — locally shadowed pinned attribute paths rejected (review_comment 3997890784) — FIXED

**File**: `xtask/src/emulate.rs` (`AttributeGate::is_accounted_for`, new `shadowed_path_roots`)

**Verified real**: the multi-segment attribute arm trusted `(root, name) ==
("cortex_m_rt", "entry")` outright. A local item shadows the extern prelude
in path resolution, so `mod cortex_m_rt { pub use evil::entry; }` plus
`#[cortex_m_rt::entry]` applies evil's procedural macro — an unexamined
expansion that could declare the gated file — not the pinned one. RED first:
the new refusal test failed with 0 violations before the fix.

**Fix**: new `shadowed_path_roots` collects every name a file's items and
imports bind (file-wide over-approximation, like `local_macro_names`); a
pinned `(crate, name)` exemption fails closed when its root is shadowed.

**Same-class sibling fixed in the same pass**: the multi-segment arm of
`macro_is_accounted_for` had the identical hole for the pinned
`cortex_m_semihosting::hprintln` exemption — a local `mod
cortex_m_semihosting` re-exporting evil's macro would resolve there. It now
fails closed on a shadowed root too.

**Tests**: `a_local_module_shadowing_the_pinned_attribute_crate_refuses_the_exemption`
(the exact Codex case),
`a_local_module_shadowing_the_pinned_macro_crate_refuses_the_exemption` (the
sibling), keep-greens `a_pinned_attribute_path_with_no_shadowing_keeps_the_exemption`
and `a_pinned_macro_path_with_no_shadowing_keeps_the_exemption`.

## Verification (codex-watch run, 2026-09-12, review 5188402021)

- `cargo fmt --all --check`: clean
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: clean
- `cargo test --locked -p xtask`: all green — 1080 lib tests (incl. the 6 new
  ones), 19 + 35 integration tests
- `cargo xtask check-layering`: ok (56 rules) — includes `check_emulation_boot`
  over the real `crates/waymaker-emu/src/*.rs`: 0 violations, so the tightened
  classifier introduces no false positives on the real firmware

## New Codex findings (codex-watch run, 2026-09-12, review 5188488869) — all handled

### Automated-review header card (review 5188488869) — nothing actionable

The review body is Codex's automated-review header; it contains no findings or
suggestions, so there was nothing to fix.

### P2 — local-path macro re-exports can disguise an external macro as a builtin (review_comment 3997946866) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: the import walk recorded only `use` paths rooted at an
external crate name, so `use crate::shim::assert;` bound nothing the checker
knew about — and the bare-name arm then trusted `assert!()` as the builtin
(or as a same-named local `macro_rules!` elsewhere in the crate). But
`mod shim { pub use evil::assert; }` in another file makes that import name
the dependency's macro, whose expansion is invisible to every scan here.
RED first: the two new refusal tests failed with 0 violations before the
fix, exactly the miss.

**Fix**: `MacroImports` gains `local_imports: HashSet<String>` — names a
`use` binds through a path rooted at `crate::` / `self::` / `super::`, which
the checker cannot resolve through the module tree. Both the bare-name and
the `crate::` / `self::` / `super::` qualified-path arms fail closed when the
name is in that set, before the local-name set or the builtin whitelist is
consulted; the attribute gate (`is_accounted_for`) and the derive check fail
closed on it too, since the same re-export can smuggle an attribute or derive
macro. `core::` / `std::` roots are deliberately *not* recorded: they name
the builtin namespace, and recording them regressed the real behavior —
`use core::fmt::Debug;` plus `#[derive(Debug)]`, `use core::cfg;` plus
`#[cfg(..)]`, and `use core::assert;` plus `assert!(..)` all compile and must
keep their exemptions (verified against rustc; RED-confirmed the regression
with a mutation check, then fixed).

**Tests** (RED first — the 2 refusal tests failed with 0 violations before the
fix; the keep-greens passed before and after):
`a_builtin_name_imported_through_a_local_reexport_refuses_the_exemption`
(the exact Codex case, cross-file),
`a_builtin_name_reexported_through_a_local_module_in_the_same_file_refuses_the_exemption`
(same-file variant),
`a_local_macro_invoked_bare_keeps_the_exemption` (a bare vetted local macro
with no local-path `use` stays exempt),
`a_builtin_name_imported_from_core_keeps_the_exemption`,
`a_builtin_attribute_imported_from_core_keeps_the_exemption`,
`a_std_derive_named_through_a_core_import_keeps_the_exemption` (the builtin
namespace keeps its behavior).

### P2 — external macro nested inside an opaque `macro_rules!` body is not classified (review_comment 3997946868) — FIXED

**File**: `xtask/src/emulate.rs`

**Verified real**: `syn` leaves a `macro_rules!` body as opaque tokens, so a
nested invocation's path never reached `Inclusions::record` — and the shape
scans (`tokens_could_include`, `tokens_could_declare_module`) only see
`include!` / `mod` tokens, not the expansion hiding behind a macro name.
`macro_rules! wrapper { () => { evil::load_startup!(); } }` smuggled the
dependency's invisible expansion past both scans: the body holds neither
shape, and the later `wrapper!()` is trusted as a local macro — while the
expansion could declare `#[path = "xtensa.rs"] mod arm_startup;` on ARM. RED
first: the new refusal test failed with 0 violations before the fix, exactly
the miss.

**Fix**: new `nested_invocation_paths` walks opaque token trees and collects
every macro-invocation path (a lone `!` followed by a
parenthesized/bracketed/braced group; `!=` is not mistaken for one; groups
are descended into; `$crate` is normalized to the local crate), and the
definition scan classifies each one with `macro_path_is_accounted_for` — the
same rule the top-level walk applies, factored out of
`macro_is_accounted_for`. A nested builtin stays fine (fixed expansion), a
nested local macro stays fine (its own definition is vetted by the same
scan — every definition is visited independently, so no transitive walk is
needed), and anything else fails the exemption closed.

**Tests** (RED first — the refusal test failed with 0 violations before the
fix; the keep-greens passed before and after):
`an_external_macro_nested_in_a_macro_rules_body_refuses_the_exemption` (the
exact Codex case), `a_builtin_macro_nested_in_a_macro_rules_body_keeps_the_exemption`,
`a_local_macro_nested_in_a_macro_rules_body_keeps_the_exemption`.

## Verification (codex-watch run, 2026-09-12, review 5188488869)

- `cargo fmt --all --check`: clean
- `cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings`: clean
- `cargo test --locked -p xtask`: all green — 1089 lib tests (incl. the 9 new
  ones), 19 + 35 integration tests
- `cargo xtask check-layering`: ok (56 rules)
- Real-crate check: `check_no_handwritten_unsafe` over the actual
  `crates/waymaker-emu/src/*.rs` — 0 violations (the firmware has no
  `macro_rules!` and no local-path imports of builtin names, so the widened
  scans introduce no false positives); temporary verification test, removed
  after passing

## Verification (codex-watch run, 2026-09-12, review 5188655688 + review_comment 3998060865)

- Review 5188655688: header card only, no actionable findings — declined with reason.
- Review comment 3998060865 (P2, "Select the Xtensa GCC version deterministically"): verified real via RED-first test — with `esp-9.0.0_20210101` and `esp-15.2.0_20250920` present, the old lexicographic `dirs.sort()` + "last wins" picked the **older** `esp-9` driver.

**Fix**: `esp_gcc_dir_in` now sorts by a numeric version key (`gcc_dir_version` parses `esp-<major>.<minor>.<patch>_<date>` into components compared numerically); unparseable names rank below every parsed one, and the name breaks ties so the order stays deterministic.

**Tests** (RED first — `the_newest_gcc_wins_numerically_not_lexicographically` failed with the old code picking `esp-9`):
`the_newest_gcc_wins_numerically_not_lexicographically`, `the_newer_build_date_wins_at_equal_versions`, `unparseable_gcc_names_stay_deterministic_and_lose_to_parsed_ones`.

**Main sync**: rebased onto `origin/main` (354d856, post-#122/#123/#125). One conflict in `crates/waymaker-fault/tests/harness.rs` — the same `empty_writer` area #122 rewrote — resolved in favor of main's merged version (identical tests, `const fn` + `Infallible` idiom). Verified via tree diff that every other #124 file (scanner hardening, ESP32-S3, linker scripts) is byte-identical to the pre-rebase tree.

- `cargo fmt --all --check`: clean
- `cargo clippy --workspace --all-targets -- -D warnings`: clean
- `cargo test --locked -p xtask`: all green — 1092 lib tests (incl. the 3 new ones), 19 + 35 integration tests
- `cargo xtask check-layering`: ok (56 rules)
