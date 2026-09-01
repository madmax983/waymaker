# ADR 0004: The layering contract is a table a gate reads

- Status: accepted
- Date: 2026-09-01
- Issue: [#8](https://github.com/madmax983/waymaker/issues/8)
- Supersedes: nothing
- Related: [ADR 0003](0003-the-eight-settled-design-decisions.md), [ADR 0001](0001-one-pipeline-table-and-a-per-crate-coverage-gate.md)

## Context

Written after the fact. Issue #8 created the three-crate workspace and the gate that
enforces its dependency direction, and it did so before this repository had a decision
record. The decisions it settled were recorded in the only places available at the time:
seven lines of comment in `Cargo.toml`, a doc comment in `xtask/src/policy.rs`, and a
section of the README.

That left the record with a hole in an awkward place. [ADR 0001](0001-one-pipeline-table-and-a-per-crate-coverage-gate.md)
opens by citing this decision as settled precedent — "the layering contract from design
document §05 is not a convention that reviewers remember; it is a table in
`xtask/src/policy.rs` that a gate reads and fails a pull request over" — and the ADR it
cites did not exist. `docs/adr/README.md` says every settled decision in this repository has
a file here, and issue #11's second "done when" says each subsequent settled decision has
its own ADR. This is that file.

Nothing here is new. It records what was decided, with the reasoning that was already
written down, so that the decision can be cited and revisited like any other.

## Decision

**The layering contract is a table, and a gate reads it.** `xtask::policy::LAYERS` holds one
row per firmware crate: its name, the workspace crates it may depend on, the third-party
crates it may reach, and the design document's "must not own" cell quoted verbatim. Adding a
crate to the workspace means adding a row; a workspace member no row covers fails
`workspace-membership` rather than being subject to no rule at all.

The gate reads `cargo metadata` rather than the manifests, because cargo has already
flattened `[target.'cfg(...)'.dependencies]` and optional dependencies by then. A dependency
that only exists under a `cfg` is still a dependency.

**`unsafe_code` is denied at the workspace level and forbidden in each crate root.** The
workspace could have forbidden it outright, which is stronger. It does not, because
`forbid` at the workspace level cannot be relaxed by any in-crate attribute: the only way
out would be to drop `[lints] workspace = true` from a member manifest entirely, and that
silently loses pedantic, nursery and `unwrap_used` along with it. `deny` keeps a documented
exception to a reviewable one-line `#![allow(unsafe_code)]` plus an ADR — and the
`crate-attributes` rule reports that allow, so the exception cannot be a quiet one.

Each firmware crate root then raises it to `#![forbid(unsafe_code)]`, which the same rule
requires. The strength is where it can be seen; the flexibility is where it can be reviewed.

**`missing_docs` is set per crate root, not as a workspace lint.** A workspace lint applies
to every target, `tests/*.rs` included, where a missing crate-level doc comment is noise
rather than a finding. Per-root costs one line per crate and is checked by the
`missing-docs` rule, which came later — see [ADR 0005](0005-documentation-is-checked-against-the-tables-it-describes.md).

**A gate is a pure function over already-read input.** Every rule takes parsed input and
returns violations, so it can be tested against a workspace that does not exist — a
`cargo metadata` document with a kernel that has dependencies, a manifest with no lint
table, an ELF no linker would produce. Reading the disk happens in one place, and it fails
closed.

## Consequences

- Every convenience a dependency would have given the kernel is either written in it or
  pushed a layer up. CRC is the clearest example: it is a natural thing for a journal to own
  and it lives in `waymaker-flash`, because of `kernel-is-dependency-free`.
- `xtask` is host tooling inside a workspace whose other members are firmware. It is kept
  out of firmware-target builds by `default-members`, which is a mechanism a contributor has
  to know about before adding a fourth non-firmware crate.
- The `deny`-not-`forbid` choice means the workspace is one line away from allowing unsafe
  code in a crate. That line is a violation the gate reports, so the cost is a reviewable
  exception rather than a silent one — but it is a cost, and `forbid` would not have it.
- Writing this ADR after the fact is itself a consequence worth naming: the reasoning
  survived only because it happened to be written in `Cargo.toml`. A decision recorded in a
  comment is one refactor away from being recorded nowhere.

## Alternatives considered

- **`forbid(unsafe_code)` at the workspace level.** Rejected for the reason above: the
  escape hatch is dropping the whole lint table, which loses more than it gains and is
  harder to notice in review than an `#![allow]` the gate names.
- **A `CONTRIBUTING.md` describing the layering.** Rejected. The failure this decision
  prevents is a dependency added in good faith by someone who has not read that file, which
  is every failure of this kind.
- **`missing_docs` as a workspace lint.** Rejected: it fires on every integration test
  target, and a rule whose output is mostly noise is a rule people learn to scroll past.
