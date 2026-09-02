# ADR 0005: Documentation is checked against the tables it describes

- Status: accepted
- Date: 2026-09-01
- Issue: [#11](https://github.com/madmax983/waymaker/issues/11)
- Supersedes: nothing
- Related: [ADR 0003](0003-the-eight-settled-design-decisions.md), [ADR 0004](0004-the-layering-contract-is-a-table-a-gate-reads.md)

## Context

Issue #11 asks for `CLAUDE.md`, an ADR record with a template, three Mermaid diagrams, and
`missing_docs` warned on in every crate.

Every one of those is prose about facts that live somewhere else. `CLAUDE.md`'s "must not
own" table is `xtask::policy::LAYERS`'s `must_not_own` column. Its command list is
`xtask::pipeline::STAGES`. The dependency diagram is `LAYERS`'s `may_depend_on`. The
seven-step protocol is design document §07. Each is true on the day it is written, and each
becomes untrue later without anything failing — which is the specific problem
[ADR 0001](0001-one-pipeline-table-and-a-per-crate-coverage-gate.md) and
[ADR 0004](0004-the-layering-contract-is-a-table-a-gate-reads.md) already solved twice, for
the pipeline and for the layering.

Documentation that has drifted is worse than none. Absent documentation sends a reader to
the code; wrong documentation sends them somewhere with confidence.

## Decision

**The documents are checked against the tables that own the facts.** `xtask::docs` adds
seven rules — `claude-md`, `adr-numbering`, `adr-structure`, `adr-index`,
`settled-decisions`, `diagrams`, `missing-docs` — and each compares a document against a
table rather than against a copy of one. `CLAUDE.md`'s must-not-own cells are compared to
`policy::LAYERS` byte for byte; its command list to `pipeline::STAGES`; the dependency
diagram's nodes and edges to `may_depend_on`; the §02 ADR to `docs::SETTLED_DECISIONS`.

**The anchors are machine-readable; the prose is not checked.** A rule matches an id, a
crate name, a `must_not_own` cell, a protocol step, an ADR number. It does not read the
sentences around them, so those stay free to be rewritten — and a row that says the
opposite of what it means still passes. `CLAUDE.md` has a "What is not checked" section
saying so, because the failure mode of a partial gate is a reader who believes it is total.

**What is checked is what a reader sees.** Every rule that matches text first removes what
the rendered page does not show: HTML comments in Markdown, `%%` comments in Mermaid, an
indented fence, a fence quoted inside a longer one. Otherwise a file reduced to "TODO:
rewrite" plus a block of ids in a comment satisfies every rule, and a diagram can be drawn
one way and labelled another.

**The dependency diagram is checked in both directions.** Every permitted edge must be
drawn, *and* every drawn edge between two layers must be permitted. Requiring only the first
would accept a picture with `waymaker-core --> waymaker-embassy` added — the layering
inverted, in the one place a reader looks to find out which way it goes.

**A lint level is matched, not an attribute string.** `#![warn(missing_docs)]`,
`#![expect(missing_docs)]`, `#![allow(warnings)]`, `#![cfg_attr(all(), allow(missing_docs))]`
and an attribute `rustfmt` split across three lines are five spellings of two facts. The
scanner in `xtask::source` joins multi-line attributes, ignores `//` and `/* */` comments,
and compares the arguments of each lint group, so each spelling is the same rule rather than
four holes.

**The size probe is linted on the firmware target.** Its binary is behind
`required-features`, so the workspace clippy stage never compiles it: the gate would report
its `#![no_std]`, `#![forbid(unsafe_code)]` and `#![warn(missing_docs)]` present while no
compiler had ever acted on them. The `probe-lint` stage closes that, and it lives in the
stage table like every other command.

## Consequences

- Changing the layering, the pipeline or the settled decisions now means changing
  `CLAUDE.md` or the diagrams in the same commit. That is the intended friction, and it is
  the only kind that keeps documentation true.
- Nine of the twenty-nine rules are about documents. That is a large share for a repository
  with three near-empty firmware crates, and it is deliberate: at rung 0.0 the documentation
  *is* most of what exists, and a gate built after the drift ratifies it.
- The gate cannot tell a correct explanation from a plausible one. It is a floor, and review
  is still the ceiling.
- The Mermaid check does not run Mermaid, so a block that no longer renders passes. The
  blocks were parsed against Mermaid 11 by hand when they were written, and the pull request
  preview is the ongoing check. Adding a Node toolchain to a `no_std` Rust repository to
  close this costs more than it is worth today.
- `docs/adr` is read for `*.md` only. An ADR may sit beside an image it refers to without a
  UTF-8 error taking down all twenty-nine rules — but a decision recorded in a file with
  another extension is a decision the record does not have.

## Alternatives considered

- **Generating `CLAUDE.md` from the tables.** It would make drift impossible rather than
  detectable. Rejected: the parts worth reading are the parts a generator cannot write, and
  a generated file is one nobody edits when the explanation is wrong.
- **Writing the documents and reviewing them.** The status quo everywhere, and the reason
  every long-lived repository has a `README` that describes a build system it no longer has.
- **A Markdown or Mermaid parser in `xtask`.** Correct, and a dependency in a crate that has
  four. The scanners here fail closed on what they do not understand — an unterminated
  fence, a `~~~` fence, a folded YAML scalar — which is the property that matters.
- **One ADR per document.** Rejected: this is one decision about how documentation is kept
  true, not four decisions about four files.
