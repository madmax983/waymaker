# ADR 0053: a `crc32` nibble table still beats the branchless loop

- Status: accepted
- Date: 2026-09-15
- Issue: [#153](https://github.com/madmax983/waymaker/issues/153)
- Supersedes: [0046](0046-crc16-folds-its-nibble-round-to-a-multiply-crc32-stays-bitwise.md)'s
  `crc32` clause — "declines a table for it: a host instruction count is not the real-flash
  test ADR 0010 asked for" — and nothing else in 0046. `crc16`'s own clause, the multiply
  fold, is unchanged and unaffected: this ADR does not touch it, because a second,
  independent measurement (below) confirms it is still table-free.
- Related: [0010](0010-the-integrity-check-is-catalogued-and-table-free.md), whose bar ADR
  0046 held itself to and this ADR is explicit about still not clearing; and
  [0038](0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md), the
  profiling harness both ADRs' numbers come from.

## Context

This repository ran two independent lines of work against the same host instruction profile
at once, without either knowing about the other. One rewrote `crc16` and `crc32` as a
per-nibble lookup table and a closed-form multiply respectively, reasoning from the same
figures ADR 0010 named as the trigger for a table. The other, on `main`, reached `crc16`'s
identical multiply-fold independently and then measured `crc32`'s own table against ADR
0010's stricter bar — "a profile of a real workload **on real flash**, against a latency
requirement §04 does not currently state" — found neither exists, and declined the table on
that basis (ADR 0046), instead giving the bitwise loop a branchless, masked rewrite of its
own eight rounds.

Both are real, and merging them found the two `crc32` treatments could not both ship: one
crate cannot compute the same checksum two ways. What decides between them here is neither
ADR's own argument restated, but a measurement neither one had: **the table, remeasured
against `main`'s own branchless loop rather than the bitwise loop it replaced.** ADR 0046's
branchless rewrite is a real optimisation of the loop it kept, so the honest question is not
"does a table beat the original eight-round loop" — every candidate here already answers
that — but "does a table still beat the *best* loop this workspace has now shipped." Neither
prior ADR could ask that question, because neither had the other's code to measure against.

### The measurement

`cargo xtask profile`'s four workloads, engine-attributed instructions
(`callgrind_annotate`, the same figure ADR 0038 gates at zero heap blocks and publishes
uncounted for instructions), run twice on the same host and the same commit of everything
*except* `crates/waymaker-flash/src/crc.rs`: once with `main`'s own branchless-masked
bitwise loop for both checksums (ADR 0046's `crc16_nibble_round` reused for `crc16`, and the
eight-round masked loop for `crc32`), and once with the nibble table for `crc32` and the
closed-form multiply for `crc16` — the same two functions this ADR's companion branch had
already written, unmodified.

| workload | engine Ir, branchless loop | engine Ir, table | delta |
| --- | --- | --- | --- |
| `journal` | 263,523 | 202,448 | **-23.2%** |
| `driver` | 29,179 | 23,184 | **-20.5%** |
| `facade` | 35,186 | 30,081 | **-14.5%** |
| `conformance` | 41,113 | 41,113 | 0.0% (neither checksum runs here) |

The branchless loop is a real win over the original eight-round loop the table's own first
measurement had compared against (`journal` -31.7% against that older loop became this
loop's own smaller, un-tabulated improvement before the table is applied on top of it) —
and the table still cuts a further fifth to a quarter off of it. Both `crc16` results agree
with ADR 0046's own finding independently: `crc16`'s multiply fold measures identically
whichever
`crc32` treatment runs beside it, because the two functions share no state and the fold is
exhaustively checked against the four-round bitwise definition (`crc16_nibble_matches_the_
four_round_reduction`) rather than merely timed.

### What this does not change about ADR 0046's own reasoning

ADR 0046 was right that a host instruction count is not the real-flash, real-latency
evidence ADR 0010 asked a table to clear, and this ADR does not claim otherwise — §04 still
states no latency budget for either checksum to be on the critical path *of*, and the boards
still owe that number exactly as
[what the boards still owe](../../CLAUDE.md#what-the-boards-still-owe) already records for
everything else. What changed is not the evidentiary bar; it is that, once a maintainer
weighs a host-side, comparison-quality figure against the ~13 KiB budget anyway — the same
judgement the table's own author made before this merge, restated here on the corrected
baseline — the table wins by enough margin over ADR 0046's own optimised loop that the loop
is no longer
the better answer on the same terms ADR 0046 used to accept it. A decision revisited needs a
new ADR naming what it supersedes rather than an edit to the accepted one; this is that ADR.

### Why a `match` and not a `[u32; 16]`

An array is the obvious spelling for a lookup table, and it is not how `crc32_nibble_table`
is spelled in `crates/waymaker-flash/src/crc.rs`. Getting there took two rounds of finding
out why the obvious version does not work in this codebase specifically, both worth
recording so nobody rediscovers them the slow way.

**`indexing_slicing` is denied workspace-wide, and array indexing is not the only
casualty.** `TABLE[index]` is refused by `cargo clippy -D warnings` whether or not the index
is provably in range — clippy does not attempt the range analysis a masked `& 0xF` index
would need to pass, only literal indices are exempt. The alternative, `TABLE.get(index)`, is
not usable either: `<[T]>::get` is not yet stable as a `const fn` on the `1.97` toolchain
this workspace pins, and `crc32` has to stay `const fn` —
`both_are_usable_in_a_const_context` pins that property in `crc.rs`'s own tests, and
`frame::input_digest` is a `const fn` that calls `crc32` for the same reason
`waymaker-flash`'s doc comments already state: "so a golden frame in a test, or a table of
expected checksums in firmware, costs nothing at runtime."

**A dense `match` whose every arm is a distinct compile-time constant is a different way to
spell the same table, and LLVM already treats it as one.** `crc32_nibble_table`'s body is
`match nibble & 0xF { 0 => crc32_nibble(0), 1 => crc32_nibble(1), ..., _ => crc32_nibble(15) }`
— sixteen arms, each a call to the same `const fn` with the arm's own literal, covering the
full masked range with no gap and no repeat. Disassembling the built `waymaker-flash` object
(`objdump -d` on the `profiling`-profile `xtask` binary, x86-64 host) shows this compiles to
a single masked, indexed load from a table LLVM built in `.rodata` — the same instruction
shape a hand-written array index produces, with no clippy violation because no `Index`
operation is written in the source at all.

This did not work on the first attempt: `#[inline]` (the soft hint) left `crc32_nibble_table`
inlined into `crc32`'s loop but `crc32_nibble` itself as a standalone, uninlined function, so
each arm remained a genuine function call with a runtime argument — measured as **worse**
than the bitwise loop it was meant to replace (`journal` +5.1%, `driver` +21.2%, `facade`
+17.3%, all regressions, all measured and discarded before landing the version that works).
`#[inline(always)]` on both functions, with a documented `#[allow(clippy::inline_always)]`
on each, is what makes the table actually appear.

### Why this is checked structurally rather than as an array

`crates/waymaker-flash/src/crc.rs` never declares a `[u32; 16]`, so `integrity-check`'s
existing array ban — which reads `const`, `static`, `type` and `let` declarations — cannot
see this table at all, by construction. `xtask::source::INTEGRITY_CHECK_TABLES` is the
structural pin instead: it names the function whose body is the table
(`crc32_nibble_table`), the helper every arm must call (`crc32_nibble`), and the arm count
(sixteen) — and `check_integrity_check` verifies each of `crc32_nibble(0)` through
`crc32_nibble(15)` appears in that function's body exactly once, and that `crc32_nibble(`
appears exactly sixteen times in total, so a seventeenth arm calling something out of range
cannot hide behind the other sixteen being correct.

## Decision

**`crc32` is CRC-32/ISO-HDLC computed four bits at a time through a sixteen-entry table
expressed as a `match`, not an array; `crc16` is CRC-16/CCITT-FALSE computed through a
closed-form nibble multiply and stays table-free, exactly as ADR 0046 left it.** Both still
compute the exact catalogued check values ADR 0010 pinned, both are still `const fn`, and
the wire format is unchanged. `INTEGRITY_CHECK_PARAMETERS` retargets each polynomial's pin
to the `crc16_nibble`/`crc32_nibble` helpers that own the one occurrence of each;
`INTEGRITY_CHECK_TABLES` pins the one table this module is now allowed, as described above.

## Consequences

- `cargo xtask size`'s `default` and `facade` rows carry the cost of `crc32_nibble_table`'s
  sixteen `u32` entries — nominally 64 B of `.rodata`, subject to the same
  probe-versus-layers attribution ADR 0029 already accounts for.
- `waymaker-flash/src/crc.rs`'s claim of being "two loops a reader can check against a
  catalogue" is less exactly true for `crc32`: verifying it means reading a four-round loop
  and a sixteen-arm selection over it, checked against each other by
  `crc32_nibble_matches_the_four_round_reduction` and
  `crc32_nibble_table_is_every_nibble_with_no_gap_and_no_repeat` rather than read by eye
  alone.
- The "switch-to-lookup-table compiles this into a table load" claim is a disassembly this
  ADR states and reproduces steps for, not a thing CI re-derives on every run — the same
  standing ADR 0010's own cycle counts have.
- The instruction-count improvement is a host-side, comparison-quality figure, not a cycle
  count on `thumbv6m-none-eabi`; §04 still states no latency budget, and the boards still
  owe the real number.
- A later measurement against a *further*-optimised loop — one this ADR has not seen — could
  in principle close the gap again. Nothing here forecloses a third ADR if that happens; what
  it forecloses is deciding the question from either loop's own argument alone rather than a
  head-to-head number.

### Reproducing the measurement

```sh
cargo xtask profile   # needs valgrind; compare engine Ir per workload against the table above
```

## Alternatives considered

**Keeping `main`'s branchless-masked loop and dropping the table.** This is ADR 0046's own
decision, and it was the working assumption until this remeasurement — see
["Context"](#context) for why the table still wins against exactly that loop rather than
against the original one.

**A byte table (256 entries, ~1 KiB) for `crc32`.** Not remeasured against this specific
branchless baseline; the table's own earlier measurement against the original bitwise loop found a
larger win for roughly sixteen times the rodata, and nothing about the branchless
refinement changes the shape of that trade-off. Still not taken, for the reason ADR 0010's
own static model already gave: the nibble table captures most of the available win for a
small fraction of the cost.

**A byte table for `crc16`, or an `unsafe`-indexed array for either.** Not needed: `crc16`
reduces to a multiply with no table at all, and reaching for `unsafe` to keep an array
spelling would trade a lint for the one thing this workspace asks a contributor to justify
before reaching for. The `match`-compiles-to-a-table approach needs neither.
