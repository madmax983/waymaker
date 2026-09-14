# ADR 0045: a nibble table is a superseding ADR, and CRC-16 needed none

- Status: accepted
- Date: 2026-09-13
- Issue: [#153](https://github.com/madmax983/waymaker/issues/153)
- Supersedes: [0010](0010-the-integrity-check-is-catalogued-and-table-free.md)'s decision
  that both checksums stay bitwise with no lookup table. Nothing else in 0010 is changed:
  the polynomials, the initial values, the choice of CRC-32/ISO-HDLC over CRC-32C, and the
  wire format are all exactly as 0010 left them.
- Related: [0038](0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md),
  which is the profiling harness this ADR's measurements come from.

## Context

ADR 0010 kept both checksums bitwise, and named what would revisit it: "a profile of a real
workload on real flash, against a latency requirement §04 does not currently state, showing
the checksum on the critical path. If that arrives the answer is most likely the nibble
table — 4.4× for 64 B — and it stays CRC-32/ISO-HDLC, because a table is an implementation
of an algorithm and not a different one."

`cargo xtask profile` is a profile of a real workload — issue #38's own host-side
instruction-count harness, driving `waymaker-rig`'s power-cut rig, the synchronous driver,
and the async façade over `waymaker-fault`'s model of NOR under valgrind's callgrind. It is
not the profile ADR 0010 asked for in one specific way, and that gap is not papered over
here: it measures host instructions under a host profile, not cycles on real flash, and
§04 still states no latency requirement for the checksum to be on the critical path *of*.
CLAUDE.md is explicit about this figure everywhere else it appears — "convertible into no
cycle count on any part" — and that is exactly as true of the numbers below. What changed is
not that a latency requirement appeared; it is that a maintainer, shown the host-side share
`crc16`/`crc32` hold of this workspace's own instrumented workloads, judged 64 B of rodata
against §04's ~13 KiB budget worth the trade. That judgement is this ADR's decision, not a
claim that ADR 0010's stricter bar was met.

### The measurement

`callgrind_annotate` over the four `cargo xtask profile` workloads, engine-attributed
instructions (the same figure `cargo xtask profile` gates at zero heap blocks and publishes
uncounted for instructions, per ADR 0038), on `main` at the commit before this change:

| workload | engine Ir | `crc16` | `crc32` | combined share |
| --- | --- | --- | --- | --- |
| `journal` | 297,712 | 83,814 (28.2%) | 61,778 (20.8%) | 48.9% |
| `driver` | 30,623 | 3,204 (10.5%) | 7,796 (25.5%) | 35.9% |
| `facade` | 38,732 | 4,272 (11.0%) | 8,594 (22.2%) | 33.2% |
| `conformance` | 41,113 | 0 | 0 | 0% (neither checksum runs here) |

Nothing else in any workload clears 5% of that workload's engine instructions. This is not
a new finding about the shape of the cost: ADR 0010's own static model already put a
bitwise byte at 74–91 instructions against a table's 12–17, and the table above is that
same ratio showing up once this workspace had a workload to point a profiler at.

### Why CRC-16 is not in the decision this ADR makes

The obvious next step — a 16-entry `[u16; 16]` table for `crc16`, mirroring `crc32`'s — was
not needed. `crc16`'s nibble-at-a-time table, worked out by hand and checked against the
four-round bitwise definition for every one of its sixteen inputs, is `nibble * 0x1021` with
no remainder: for this specific polynomial, folding a single nibble into an otherwise-empty
16-bit register never produces two bit-contributions that land on the same output bit, so
the GF(2) XOR-reduction and ordinary integer multiplication compute the same thing. That is
a property of `0x1021`, not of CRC in general — the equivalent check for `crc32`'s reflected
polynomial fails at the third table entry (`crc32_nibble(3)`), which is why `crc32` still
needs a real table and `crc16` does not.

So `crc16` is unaffected by this ADR: it is still bitwise, still declares no array, still
costs 0 B of rodata, and the `integrity-check` gate's parameter pins still hold it to the
same polynomial and initial value as before — retargeted to the new `crc16_nibble` helper
function they now live in, not loosened. The win came from a cheaper formula for the same
answer, which is the kind of change this repository has never needed a superseding ADR for.

### What a table for CRC-32 costs and buys, measured on the code this ADR ships

Same four workloads, same commit, with `crc16` reformulated and `crc32` given the table
this ADR permits:

| workload | engine Ir before | engine Ir after | delta |
| --- | --- | --- | --- |
| `journal` | 297,712 | 203,235 | **-31.7%** |
| `driver` | 30,623 | 23,273 | **-24.0%** |
| `facade` | 38,732 | 30,180 | **-22.1%** |
| `conformance` | 41,113 | 41,113 | 0.00% (unaffected, as expected) |

`cargo xtask size`'s `default` and `facade` rows carry the rodata cost — see
["Consequences"](#consequences) for the figure. DHAT's heap gate is unaffected: 0 engine
blocks on every workload, before and after this change — a `match` over `const fn` calls
allocates nothing, same as the loop it replaced.

### Why a `match` and not a `[u32; 16]`

An array is the obvious spelling for a lookup table, and it is not how `crc32_nibble_table`
is spelled in `crates/waymaker-flash/src/crc.rs`. Getting there took two rounds of finding
out why the obvious version does not work in this codebase specifically, both worth
recording so nobody rediscovers them the slow way.

**`indexing_slicing` is denied workspace-wide, and array indexing is not the only casualty.**
`TABLE[index]` is refused by `cargo clippy -D warnings` whether or not the index is
provably in range — clippy does not attempt the range analysis a masked `& 0xF` index would
need to pass, only literal indices are exempt. The alternative, `TABLE.get(index)`, is not
usable either: `<[T]>::get` is not yet stable as a `const fn` on the `1.97` toolchain this
workspace pins (confirmed against `rustc 1.97.1` directly), and `crc32` has to stay
`const fn` — `both_are_usable_in_a_const_context` pins that property in `crc.rs`'s own
tests, and `frame::input_digest` is a `const fn` that calls `crc32` for the same reason
`waymaker-flash`'s doc comments already state: "so a golden frame in a test, or a table of
expected checksums in firmware, costs nothing at runtime."

**A dense `match` whose every arm is a distinct compile-time constant is a different way to
spell the same table, and LLVM already treats it as one.** `crc32_nibble_table`'s body is
`match nibble & 0xF { 0 => crc32_nibble(0), 1 => crc32_nibble(1), ..., _ => crc32_nibble(15) }`
— sixteen arms, each a call to the same `const fn` with the arm's own literal, covering the
full masked range with no gap and no repeat. Disassembling the built `waymaker-flash` object
(`objdump -d` on the `profiling`-profile `xtask` binary, x86-64 host) shows this compiles to
`and $0xf,%eax; movzbl; xor (%rdx,%rax,4),%r9d` — a single masked, indexed load from a table
LLVM built in `.rodata` — the same instruction shape a hand-written array index produces,
with no clippy violation because no `Index` operation is written in the source at all.

This did not work on the first attempt. `#[inline]` (the soft hint) left `crc32_nibble_table`
inlined into `crc32`'s loop but `crc32_nibble` itself as a standalone, uninlined function —
so each arm remained a genuine function call with a runtime argument, which LLVM correctly
recognised as "call this function with the masked index" and simplified to exactly that: a
`call` instruction per nibble, measured as **worse** than the original bitwise loop (`journal`
+5.1%, `driver` +21.2%, `facade` +17.3%, all regressions, all measured before being
discarded). The switch-to-lookup-table transform needs each arm's *value* visible as a
literal before it can spot "sixteen distinct constants, one per case" — which needs
`crc32_nibble`'s own four-round body inlined into each arm first. `#[inline(always)]` on
both functions, with a documented `#[allow(clippy::inline_always)]` on each (clippy's own
`inline_always` lint objects to the attribute on principle, and the reason given here is the
same disassembly and measurement this section describes), is what makes the table actually
appear. This workspace's rule for `#[inline]` — "only with a before/after instruction count,
and never on a large function" — is satisfied on both counts: the counts are above, and each
function's whole body is one four-round loop or one sixteen-arm match.

### Why this is checked structurally rather than as an array

`crates/waymaker-flash/src/crc.rs` never declares a `[u32; 16]`, so `integrity-check`'s
existing array ban — which reads `const`, `static`, `type` and `let` declarations — cannot
see this table at all, by construction. Leaving the ban as the whole of the gate would make
this table invisible to the one rule that exists to make a table decision loud rather than
quiet, which is the opposite of what ADR 0010 asked a superseding decision to do.
`xtask::source::INTEGRITY_CHECK_TABLES` is the structural pin instead: it names the function
whose body is the table (`crc32_nibble_table`), the helper every arm must call
(`crc32_nibble`), and the arm count (sixteen) — and `check_integrity_check` verifies each of
`crc32_nibble(0)` through `crc32_nibble(15)` appears in that function's body exactly once,
and that `crc32_nibble(` appears exactly sixteen times in total, so a seventeenth arm
calling something out of range cannot hide behind the other sixteen being correct. A second
table, an array, a byte-wide table, or a table for a third checksum this workspace does not
have yet are all still caught by the pre-existing rules: the array ban for the first three,
and the fact that `INTEGRITY_CHECK_TABLES` names exactly one entry for the fourth.

## Decision

**`crc32` is CRC-32/ISO-HDLC computed four bits at a time through a sixteen-entry table
expressed as a `match`, not an array; `crc16` is CRC-16/CCITT-FALSE computed through a
closed-form nibble multiply and remains table-free.** Both still compute the exact
catalogued check values ADR 0010 pinned, both are still `const fn`, and the wire format is
unchanged — this is an implementation change to two functions, not to what they compute or
to anything a device writes.

The `integrity-check` gate's `INTEGRITY_CHECK_PARAMETERS` retargets each polynomial's pin
from `crc16`/`crc32` to the new `crc16_nibble`/`crc32_nibble` helpers that now own the one
occurrence of each; the initial-value and final-xor rows are unmoved. `INTEGRITY_CHECK_TABLES`
is new, and is the structural pin ["Why this is checked structurally"](#why-this-is-checked-structurally-rather-than-as-an-array)
describes.

## Consequences

- `cargo xtask size`'s `default` and `facade` rows carry the cost of `crc32_nibble_table`'s
  sixteen `u32` entries — nominally 64 B of `.rodata`, subject to the same
  probe-versus-layers attribution ADR 0029 already accounts for. Whichever the measured
  figure, it is a decision with a number now rather than an optimisation nobody weighed
  against the budget.
- `crc16`'s change costs nothing: no rodata, no new gate surface beyond the retargeted
  parameter pins, and the same instruction count reduction ADR 0010's own static model
  predicted a table would need to buy.
- `waymaker-flash/src/crc.rs`'s claim of being "two loops a reader can check against a
  catalogue" is now less exactly true for `crc32`: verifying it means reading a four-round
  loop and a sixteen-arm selection over it, checked against each other by
  `crc32_nibble_matches_the_four_round_reduction` and
  `crc32_nibble_table_is_every_nibble_with_no_gap_and_no_repeat` rather than read by eye
  alone.
- The "switch-to-lookup-table compiles this into a table load" claim is a disassembly this
  ADR states and reproduces steps for, not a thing CI re-derives on every run — the same
  standing ADR 0010's own cycle counts have, and for the same reason given there: there is
  no gate that could tell "a compiler stopped applying this optimisation" apart from "a
  compiler applied a different one that costs the same" without becoming a second
  implementation of the optimiser.
- The instruction-count improvement is a host-side, comparison-quality figure. It is not a
  cycle count on `thumbv6m-none-eabi`, §04 still states no latency budget, and the boards
  still owe the real number exactly as they owe every other figure in
  [what the boards still owe](../../CLAUDE.md#what-the-boards-still-owe).

### Reproducing the disassembly

```sh
cargo build --locked -p xtask --bin xtask --profile profiling
nm target/profiling/xtask | grep waymaker_flash3crc   # crc16/crc32 should be the only two symbols
objdump -d --no-show-raw-insn target/profiling/xtask   # find crc32's symbol; its loop body
                                                        # should show `xor (%reg,%reg,4),%reg`
                                                        # against a `lea`-loaded table address,
                                                        # never a `call`
```

### Reproducing the measurement

```sh
cargo xtask profile   # needs valgrind; compare engine Ir per workload against the tables above
```

## Alternatives considered

**A byte table (256 entries, ~1 KiB) for `crc32`.** Measured at the same time as the nibble
table: `journal` -40.7%, `driver` -30.0%, `facade` -27.7% — a larger win for roughly sixteen
times the rodata. Not taken, for the same reason ADR 0010 preferred the nibble table over
the byte table in its own static model: the nibble table already captures most of the
available win (comparing the two, roughly 80% of the byte table's instruction-count
reduction) for a small fraction of its cost, and ADR 0010's own words — "the answer is most
likely the nibble table" — anticipated exactly this trade.

**A byte table for `crc16` too, or a nibble table spelled as an array with `unsafe`
`get_unchecked` to sidestep `indexing_slicing`.** Not needed: `crc16` reduces to a multiply
with no table at all, and reaching for `unsafe` to keep an array spelling would have traded
a lint for the one thing this workspace asks contributors to ask before reaching for. The
`match`-compiles-to-a-table approach costs neither a lint suppression this workspace does
not already carry a precedent for (`redundant_pub_crate`'s documented `#[allow]` is the
precedent `inline_always`'s follows) nor any `unsafe`.

**Leaving `crc16`'s eight-round bitwise loop in place, using the table only for `crc32`.**
Rejected once the nibble-multiply reduction was found: it is free in every sense ADR 0010's
own criteria care about — no rodata, no gate change beyond a pin retarget, no new lint
surface — so declining it would be leaving a genuine zero-cost improvement on the table for
no stated reason.
