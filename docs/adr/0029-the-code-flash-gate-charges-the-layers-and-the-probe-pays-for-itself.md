# ADR 0029: The code-flash gate charges the layers, and the probe pays for itself

- Status: accepted
- Date: 2026-09-07
- Issue: [#72](https://github.com/madmax983/waymaker/issues/72)
- Supersedes: nothing
- Amends: [ADR 0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md), which
  decided that sections are read and symbols are not
- Related: [ADR 0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md),
  [ADR 0019](0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md),
  [ADR 0020](0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md)

## Context

Design document §04 states the incremental code-flash budget for "core + flash adapter".
`cargo xtask size` measured something wider. The gated number was the difference between two
linked images, and the larger image holds the size probe's own `match` arms, folds and calls
as well as the layers'.

The probe's code is not incidental. `size-probe-reach` requires the probe to call every
public function each layer declares, so the probe's share grows with the library's. ADR 0002
called it "the probe's own arithmetic plus the cost of linking the crates" and said it was
"an honest zero for code that does not exist". It stopped being a zero at rung 0.1.

At rung 0.5 the gate read 18386 B. The symbol table attributes 7534 B of that to the probe.
More than a third of a budget stated for two crates was being spent by a third crate that
ships nothing.

Two raises rest on that number. ADR 0017 took the gate from 8 KiB to 16 KiB and ADR 0020
from 16 KiB to 18 KiB. ADR 0020 said plainly that the next change should be this correction
rather than a third raise.

## Decision

### The gate charges the layers, and the report states the split

The gated figure is now the image delta **less what the symbol table attributes to the
probe**:

```
layers = (image.flash - baseline.flash) - (image.probe_flash - baseline.probe_flash)
```

Both terms are deltas. The baseline image holds the probe's entry point too, and the image
delta has already subtracted it.

`SizeReport::render` prints `Δflash`, `probe` and `layers` on every row of every run, and
the JSON report carries `probe_flash` and `layers_flash`. The base-branch diff prints the
layers' figure beside the image cost for the row the budget gates. A reader can therefore
see the split rather than take the gate's word for it, which is issue #72's second "done
when" as well as its first.

### A symbol belongs to the crate that declares it

`size::defining_crate` reads the first crate-root component of a mangled name. It reads the
`v0` form (`_RNvCs<hash>_14waymaker_flash5frame…`) and the `legacy` form
(`_ZN14waymaker_flash…`).

The defining crate, not the instantiating one. Fat LTO monomorphises
`waymaker_flash::frame::encode_with` for the probe, and the symbol then names both crates.
Crediting the byte count to the instantiation would hand every generic in the engine back to
the row this change corrects.

### Everything unattributable stays with the layers

The subtraction removes only bytes a symbol names as the probe's. `.rodata` string data,
`compiler_builtins`, the `__aeabi_*` helpers and the alignment padding between functions are
all charged to the layers. A byte nobody can attribute is a byte the layers brought, and a
budget must err toward failing.

### Symbols are read, not shelled out for

`xtask::elf::symbols` parses `.symtab` and its string table, as `xtask::elf::sections`
parses the section header table. ADR 0002's reason holds: a gate whose measurement needs a
binary that may not be installed reports "tool missing" on the day it matters.

`xtask/tests/size_budgets.rs` takes a second opinion from `llvm-nm` in the pinned
toolchain's sysroot, as it already does from `llvm-size` for the sections. An offset wrong in
both the parser and the synthetic-image builder would otherwise leave every test green.

### One image, built without `strip`

The matrix passes `--config profile.release.strip="none"`. It reads the section sizes and
the symbols from that one image.

ADR 0002 chose section headers over symbols so that the measurement survives the release
profile's `strip = "symbols"`. That reason is now inverted, so the setting is turned off for
the measurement — and the claim it rests on is checked rather than assumed.
`size::check_symbols_are_not_measured` fails a run in which the image carries no symbol
table, or in which a symbol, string or debug section is allocated. Stripping removes only
unallocated sections, so while that holds, the attributed image and the shipped image have
the same section sizes.

### Everything that reads zero still fails closed

ADR 0002's rule extends to the new number:

- a row that attributes **nothing** to the probe is `Unmeasurable`, not a probe that cost
  nothing. Every image the matrix links is the probe;
- a probe share **larger than the image** it was read from is `Unmeasurable`;
- a report row with no `probe_flash` field is rejected, so a truncated artifact cannot gate
  clean. The report schema goes from 1 to 2, and a schema-1 base branch reads as "not
  compared".

### The budget comes down to 12 KiB

The corrected figure is **10852 B**. `INCREMENTAL_CODE_FLASH_BYTES` goes from 18 KiB to
**12 KiB**, which is a cut of 6 KiB rather than a raise, and leaves 1436 B.

12 KiB rather than 11 KiB because ADR 0028 records that §11's timer vocabulary left 46 B and
that issue #33's record bodies do not fit under it. A ceiling with no room for the next rung
is a ceiling the next pull request raises, which is the pattern this change exists to end.
Not 8 KiB, because §04's v0.1 number does not scope rung 0.2's two-bank lifecycle or §10's
capacity reserve, and 10852 B is over it.

## Consequences

- The number the gate prints is the number §04 states. `Δflash` is still printed, so nothing
  is hidden — the report says which of the two is gated.
- The budget is 6 KiB tighter than it was this morning, and the engine has 1436 B of room.
- The matrix links with symbols in the image, so the linked artifacts are larger on disk.
  The measured sections are unchanged, and `check_symbols_are_not_measured` is what says so.
- **Fat LTO can inline a layer body into a probe symbol**, and the subtraction then charges
  that body to the probe. This is the one direction in which the correction understates the
  layers, and it is stated in
  [what is not checked](../../CLAUDE.md#what-is-not-checked) rather than left implied. The
  `facade` row shows the shape of it: `waymaker-embassy` carries no symbol of its own,
  because its `const fn` façade is inlined into the probe's call sites.
- `.rodata` holds one sized symbol in the current image, so the attribution is a `.text`
  fact and the other 1804 B are charged to the layers. That is the conservative direction,
  and it means the layers' figure is not a per-crate accounting.
- Attribution reads Rust's mangling, which is not a stable ABI. A third mangling scheme
  would attribute nothing, every row would read `probe = 0`, and the gate would fail closed
  rather than pass.

## Alternatives considered

**A third measured variant with opaque stubs.** Link a probe whose calls are replaced by
stubs of the same shape and subtract. Rejected: "the same shape" is not a thing the compiler
guarantees, and the number would move with the optimiser rather than with this repository.

**Report the split and keep gating the image delta.** Issue #72 offers this. Rejected: it
leaves the budget stated in §04's words and enforced against a different number, and the
whole cost of the correction is already paid once the split is measured.

**Gate the sum of symbols attributed to `waymaker-core` and `waymaker-flash`.** Rejected: it
drops `.rodata`, `compiler_builtins` and padding — 2.6 KiB in the current image — out of the
budget for nothing, which is a loosening dressed as a correction.

**Build twice, stripped and unstripped, and compare.** Rejected: it doubles the link work
per row to check a property the unstripped image can be asked about directly, and it adds a
class of bug — two images measured as one — that one build does not have.

**Shrink the probe instead.** Rejected as the answer, though it remains worth doing: the
probe must reach every public function the layers declare, so its share is bounded below by
the library's surface. Shrinking it moves the number without fixing what the number means.
