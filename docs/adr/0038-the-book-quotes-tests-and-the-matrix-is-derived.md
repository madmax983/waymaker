# ADR 0038: The book quotes tests, and the matrix is derived

- Status: accepted
- Date: 2026-09-10

## Context

Issue [#42](https://github.com/madmax983/waymaker/issues/42) asks for an mdBook and a
hardware compatibility matrix. Its two "done when"s are that the book builds in CI and that
"its code samples are tested, not merely quoted".

A book is the highest-rot artifact this repository has. Every other document here is held to
a table — `CLAUDE.md` to `xtask::policy::LAYERS`, the ADR record to
`xtask::docs::SETTLED_DECISIONS`, the frozen format to a corpus. A book is prose that a
reader believes, and nothing in a normal build fails when it stops being true.

Three measurements decided this ADR, and all three were taken rather than assumed.

**mdBook exits zero on a broken book.** An `{{#include}}` whose file is missing renders the
directive into the page, logs `ERROR`, and exits `0`. An anchor the file does not declare is
worse: it renders as *nothing at all*, logs nothing, and exits `0`. So "the book builds in
CI" cannot be `mdbook build` alone. Both cases were reproduced against mdBook 0.5.4 before
this was written.

**mdBook 0.5 has no library target.** The crate is a binary; `mdbook-driver` is the split-out
library. Taking it as an `xtask` dependency would add roughly thirty seconds of cold build to
the six workspace stages and would need a lint stage of its own, in exchange for pinning a
version in `Cargo.lock`.

**The write-amplification figure is cheap to measure.** `xtask::wear::measure` runs the real
journal writer over three modelled parts in under a millisecond, so the matrix's most
perishable column can be recomputed on every run of the gate instead of transcribed.

## Decision

**The book is `docs/book`, and two gate rules own it.**

`book` holds the shape and the samples. `hardware-matrix` holds the matrix. Both read the
tables that already own the facts — `docs::FAILURE_ROWS`, `docs::HARDWARE_TARGETS`,
`wear::PARTS` — plus two new ones, `book::BOOK_CHAPTERS` and `book::NON_GOALS`.

**A chapter may not carry a Rust sample of its own.** A Rust fence may hold `{{#include}}`
directives and nothing else. That is the whole of issue #42's second "done when": the bytes
the book shows are the bytes of `crates/waymaker-drive/tests/book.rs`, which the `test` stage
compiles and runs against `waymaker-fault`'s model of NOR, the real driver and the real
codec.

The tie is the anchor's **name**. An anchor with no `#[test]` of that name in the file it
comes from fails the build, and so does an anchor no chapter shows. A line-ranged include is
refused outright: a line range stops being the right lines the first time somebody inserts
one above it.

**The wire-format chapter includes the frozen specification rather than restating it.** A
second copy would pass every rule that reads the first and say something else. The five
relative links inside `docs/format/wire-format-v1.md` become absolute GitHub URLs in the same
change, because that document now has to resolve from two places.

**Every matrix cell but one is derived.** The geometry comes from `wear::PARTS`, the
power-cut standing from `docs::HARDWARE_TARGETS`, and the write amplification from the
measurement this run took. Only the clock column is declared, because no table here holds it.
So the book cannot say `Passed` where the attestation record says `Not run`, and a figure
that drifted fails a build rather than going on being published.

**`cargo xtask book` renders, and fails closed on everything mdBook does not.** A missing
renderer is an error, not a skip. An `ERROR` line beside a zero exit is a failure. A chapter
with no HTML is a failure. A page that shows none of the sample it includes is a failure.
CI installs a pinned mdBook with `taiki-e/install-action`, which is the same mechanism the
coverage stage already uses for `cargo-llvm-cov`.

## What review defeated, and what closed it

The first version of both rules was written, then attacked. Every row below is a mutation
that was *run* against `cargo xtask check-layering` and watched printing `ok`, and each one
is now a test.

| The mutation | Why it worked | What closed it |
| --- | --- | --- |
| A fabricated table of two boards that do not exist, both `Passed`, with the honest rows hidden in HTML comments | each cell was searched for *somewhere* in the chapter, and a comment is somewhere | comments stripped, then whole rows compared cell for cell against `MATRIX_TABLE_HEADER` plus the derived rows in order |
| `63.37` → `163.37`; `Not run` → `Not run in the lab, Passed on the bench` | `contains` cannot see a superstring | the same positional equality |
| A duplicate row for a declared id, and rows whose first cell was bold or bare | the id scan read only a backticked first cell | the row sequence must match exactly, so an extra row is an extra row |
| Prose above the table claiming every board had passed | prose is not checked | the chapter may not say `Passed` while no row renders it |
| `#[ignore]` written *above* the `ANCHOR` marker | any `#[` line kept the attribute run alive, and a reader of the book never sees it | the whole attribute run is read, and `#[ignore]` or `#[cfg(` refuses the pairing — `failure-matrix`'s standard, met here |
| An anchor shrunk to two comment lines advertising an API that does not exist, with its test left below | the tie was the anchor's *name* only | an anchor must **contain** its own `#[test] fn`, unless it is named in `BOOK_FIXTURE_ANCHORS` — and a fixture must still declare an item |
| A bare ` ``` ` fence carrying Rust | the ban asked whether the info string said "rust" | an allowlist of quotable languages; everything else, the empty info string included, must be include-only |
| `{{#include a}} let x = 1; {{#include b}}` | `starts_with` and `ends_with` both hold, and the middle renders | a line must be exactly one directive |
| `{{#playground …}}` and `{{#rustdoc_include …:1:40}}` | `includes()` matched only `{{#include` | any directive that is not `{{#include}}` is refused |
| A four-space indented block | Markdown renders it as code with no fence at all | no chapter may indent a line by four spaces |
| A chapter named `rogue.MD`, linked from the summary | the collector filtered `extension == "md"` | every *file* under the book's source must be a declared chapter |
| An anchor renamed so the page renders nothing | `verify_render` took its witness *from the anchor*, so a missing anchor gave it nothing to look for | a missing witness is a problem rather than a skip |
| `docs/book (deleted; see the archive)` in `CLAUDE.md` | the link check was `contains("docs/book")` | the check is the summary's path |

Two findings were about honesty rather than evasion, and both changed a rendered cell. Every
crash sweep in this workspace lays the part out at a four-byte program unit, so
`byte-programmable` and `page-programmable` carry a measured wear figure and no sweep of
their own; `render_power_cut` derives that from `SWEPT_PROGRAM_BYTES` instead of printing
"swept" against a row nothing has ever swept. And a modelled part whose measurement produced
no figure used to render the same `Not measured` a board gets; it is now a violation.

## Consequences

The book cannot quietly stop being true. A chapter that loses its link, a sample that stops
compiling, a failure row that disappears, a non-goal that softens, a matrix figure that
drifts, and a board row that claims a pass are each a red build.

**A contributor needs mdBook installed** to run `cargo xtask book`. It is not in the
pre-commit hook, and `check-layering` needs none of it, so the cost falls only on someone
building the book locally.

**The gate now takes a measurement.** `collect_inputs` runs `xtask::wear::measure`, so
`check-layering` does work it did not do before. It is sub-millisecond, and it keeps every
rule a pure function over already-read input.

**Two things this deliberately did not do.** The porting sample is not run through
`waymaker-conformance`: that would make `waymaker-drive` depend on a crate `CLAUDE.md`
records as one that nothing depends on, and the sample already drives the real writer and
the real recovery over the adapter. And `mdbook test` is not run: it compiles a snippet
standalone, which proves less than running the sample against the engine, and would need
every anchor to be a whole program.

**What is still not checked.** Prose, except for the one word the matrix chapter may not
say. Both rules match ids, file names and rendered cells.
A chapter whose sentences describe another engine passes as long as its includes and its
tables are right, and a `Passed` row is only ever as true as the bench log behind the ADR
that flipped it.
