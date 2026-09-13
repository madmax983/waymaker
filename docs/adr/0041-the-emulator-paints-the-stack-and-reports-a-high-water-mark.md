# ADR 0041: the emulator paints the stack, and reports a high-water mark

- Status: accepted
- Date: 2026-09-13
- Issue: [#47](https://github.com/madmax983/waymaker/issues/47)
- Supersedes: nothing
- Related: [0040](0040-the-emulator-runs-the-rig-and-attests-to-no-board.md),
  [0038](0038-no-alloc-is-a-measurement-and-the-instruction-figure-is-a-comparison.md),
  [0035](0035-the-facade-row-is-gated-and-runtime-ram-is-composed.md),
  [0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md)

## Context

Issue #47 is a question ADR 0002 asked itself and then deferred: `cargo xtask size` gates
runtime RAM as an ELF section delta, and named that "engine statics" rather than "runtime
RAM" for a stated reason — design document §04's own accounting is "cursor, context, record
header, and storage scratch", and most of that lives on the stack, not in `.data` or `.bss`.
A deeper call chain moves no writable section and no type size, so the gate stays green while
the real number goes wherever it likes. ADR 0035 composed the figure further — a scratch
page, a kernel-state registry, a context — and said the same thing about the fourth term:

> What is still not measured is the depth of the call chain... The report says so where it
> prints the total, rather than printing "runtime RAM: ok".

ADR 0040 then built the one thing in this workspace that runs Waymaker's code on the part
§04's budgets are stated for, and named the obvious next use of it without taking it:

> An emulated boot is the first thing in this workspace that could measure it — paint the
> region and read the high-water mark after the run — and it does not. Doing it needs a
> second `unsafe` expansion and a memory-map symbol, and this change would rather ship a boot
> that is honestly a boot than a boot with a half-argued number attached.

Issue #47's own "done when" accepts a report that says precisely what is measured and what is
not — and that half was already true before this change. What was still missing was a
measured number, and two of the three routes issue #47 names are closed by this workspace's
own toolchain: `-Z emit-stack-sizes` and `cargo-call-stack` both need nightly, and
`rust-toolchain.toml` pins stable deliberately, for `workspace-lints`' reason — a floating or
dual toolchain turns an unrelated change red for a lint or a feature nobody in it saw. The
third route, a compile-time bounded-depth invariant, proves the engine recurses nowhere; it
does not produce a byte figure, because depth and frame size are two different facts and a
proof of the first says nothing about the second.

## Decision

**Each emulated boot paints its own unused stack before the rig runs, and reports how far the
paint was disturbed after.**

`waymaker_emu::stack` is the new module, and it is the third and last reason this crate writes
the `unsafe` keyword at all — after the two macro expansions, `#[cortex_m_rt::entry]` and
`debug::exit`. `main::measured_run` — `#[inline(never)]`, so its frame cannot be folded back
into `main`'s — holds every local the rig and the conformance suite use. `main` reads the
stack pointer first, before calling anything else, through `cortex_m::register::msp::read()` —
a plain register read the `cortex-m` crate exposes as safe, so this needs no `unsafe` of its
own and depends on nothing a compiler chose. `stack::paint` fills every byte from the linker's
`_stack_end` up to that reading, less a guard margin near it that is never painted, with a
repeating, non-trivial byte; `measured_run` is called; and `stack::high_water_mark` scans from
`_stack_end` up to that same bound for the first byte the run disturbed.

The figure this produces is a *lower bound*, not an exact reading, and the module documentation
says so rather than the sharper claim an earlier draft of this change made: a frame can reserve
bytes it never writes — alignment padding, a buffer only partly filled — and a byte like that
still reads as the paint pattern, understating how deep the stack pointer actually went. The
guard margin does still guarantee a *floor*: never painted and never scanned, so the reported
figure can never read below it. Painting the stack and reading back a high-water mark is a
known technique with a known limit, and stating the limit is more honest than a rule this
workspace has never asked any other measurement here to meet either.

**The number is reported and gated, not folded into §04's own figure.** `cargo xtask size` and
`cargo xtask emulate` measure two different things and always will: the size gate's runtime
RAM figure is the *engine's* four terms, on a host build that never links `waymaker-rig` or
`waymaker-conformance` at all; this image links both alongside the three layers, boots the
whole thing on real ARM cores, and reports the *whole call chain's* depth on *this* run. A
number that conflated the two would be exactly the "half-argued number" ADR 0040 declined to
attach. So `xtask::emulate::StackUsage` is read from a third census line — `used` and
`available` — and fails closed on its own account: a region reported empty, or one disturbed
all the way to its own ceiling, is not a measurement that happened, by the same rule every
other emulate failure obeys — not a byte ceiling against §04, which does not exist for this
figure and is not invented here. The two machines' `StackUsage` values are **not** required to
agree, unlike their `Census`: a Cortex-M0 and a Cortex-M4 compile the same source into
different instructions, so a different byte count here is expected, where a different census
would mean the rig behaved differently on the two.

**`emulation-boot`'s `unsafe`-keyword rule grows a third, narrowly named exception, and it is
more than a name check.** Before this, the rule read: no file of `waymaker-emu` writes
`unsafe`, full stop, with the two macro expansions never spelling the keyword in this crate's
own source at all. `paint` and `high_water_mark` are hand-written, so the rule now also
permits the keyword inside the braced body of a function named in
`xtask::emulate::PERMITTED_UNSAFE_FUNCTIONS`, and inside the one `unsafe extern "C" { .. }`
block the 2024 edition requires to name the linker symbols — both confined to the one file
`xtask::emulate::STACK_MODULE` now names by its crate-relative path rather than a bare file
name. A name alone proved not to be enough: a bodiless signature — a trait method — has a
`declaration_count` of exactly one too and would still resolve onto whatever block happens to
follow it, so the rule also requires the header to reach a `{` before a `;`
(`crate::emulate::has_a_body`), the found body to be scanned at its own nesting depth so a
nested item or a closure cannot hide a second `unsafe` inside it
(`crate::source::nesting_depth_at`), and the one permitted extern block to declare
`_stack_end` and `_stack_start` and nothing else — which is what tells it apart from
`unsafe extern "C" fn trap_handler() { .. }`, a foreign function whose `unsafe` is also
immediately followed by the word `extern`.

**Depth zero is not uniqueness, and a fourth round of review found the gap.** A second,
unrelated `unsafe` statement placed *beside* the legitimate fill — a sibling rather than a
nested decoy — sits at the same nesting depth as the real one, so a check that only asked
"is this `unsafe` contained in the permitted function, at depth zero" would have permitted
both. `crate::emulate::sole_depth_zero_unsafe` closes it: it collects every depth-zero
`unsafe` in a permitted function's body and returns the one offset only when there is
exactly one. Two or more refuses the whole function, because which one is the legitimate
fill would be a guess, and this rule does not guess. `a_sibling_unsafe_beside_the_legitimate_one_is_reported`
is the test watched failing against the old containment-only check before this closed it,
and `a_clean_stack_module_with_two_linker_symbols_passes` is its positive twin.

**A safe `pub fn` must be sound for every input, and a fourth finding said this one was not.**
`paint`, `high_water_mark` and `available_bytes` took `depth_from` from their caller with no
validation, so a stale or otherwise wrong reading — the one path this ADR's own module
documentation says a caller must not take, but nothing stopped a future caller from taking it
anyway — could carry every pointer either function computes outside the stack region this
image owns. A safe function has to stay sound for any argument, not only the one a
well-behaved caller passes today. `_stack_start`, the other linker-provided bound, is now
named beside `_stack_end`, and `crate::stack::clamp_to_stack_region` holds `depth_from` to
`[_stack_end, _stack_start]` before either function computes a single pointer from it. The
cost of a wrong reading is now a wrong *measurement* — the same lower-bound honesty this ADR
already states — never an out-of-bounds access.

## Consequences

**A real, measured stack figure exists where before there was none**, on both architectures
this workspace is built for. It is reported per machine, fails closed on a degenerate
measurement, and is never smoothed into a single cross-machine number the way the census is —
because the two machines' code is not the same code, only the same source.

**It is not §04's figure, and does not become one.** `cargo xtask size`'s runtime RAM total —
the scratch page, the kernel-state registry, the context, and the statics delta — is
untouched: it is still stack-blind for call-chain depth, and its own report still says so.
This ADR closes issue #47 by the "or" in its own "done when": a real number now exists and
fails closed the way this workspace's other unbudgeted figures do — write amplification, the
`no_alloc` instruction counts — and the two figures' difference in scope is stated rather than
implied, which is a sharper answer than folding one into the other would have been.

**The exception surface grows by two named functions and one linker-symbol block, not by a
crate**, and closing the surface took more than pinning a name: `has_a_body`,
`nesting_depth_at`, `sole_depth_zero_unsafe` and the extern block's own content check are
each answers to a concrete adversarial file constructed against an earlier version of this
rule and found to pass it —
`a_bodiless_signature_above_the_real_function_does_not_borrow_its_exemption`,
`a_nested_function_inside_paint_does_not_inherit_its_exemption`,
`a_sibling_unsafe_beside_the_legitimate_one_is_reported` and
`a_foreign_function_item_is_not_the_permitted_extern_block` are the four that were watched
failing before the checks that close them existed. `hand_written_unsafe_is_reported` and
`hand_written_unsafe_in_a_sibling_module_is_reported` still pass unmodified, and
`unsafe_in_stack_rs_outside_the_two_named_functions_is_reported` is the sibling test showing
the same file does not get a blanket pass.

**Neither hardening changed what the figure means, only what a wrong caller could do to it.**
`clamp_to_stack_region` is a floor-and-ceiling clamp, not a new measurement path: a
`depth_from` inside the real stack region is unaffected, and the reported figure for the one
caller this crate has — `main`, reading `current_stack_pointer()` first — is identical before
and after. What changed is the *worst case* for an argument this ADR's own text had already
named as an obligation on the caller rather than a check: it is now a check too.

**What is still owed.** A decoy `stack.rs` reproducing the whole crate-relative suffix in a
different, deeper directory would still be read as the permitted module — narrower than the
bare-file-name version this change replaced, but not eliminated, and named again in
`CLAUDE.md`'s "what is not checked" rather than left implied. A closure or nested item defined
but never invoked, at the permitted function's own nesting depth, is not caught by depth alone
— the same shape of gap `effect-protocol` accepts for the same reason, and this rule inherits
rather than closes further. The figure remains a whole-image number: it cannot be split into
the engine's share and the rig-and-conformance harness's without a call graph, which is the
same tool this ADR declined to add nightly for. And the guard margin's size — 128 bytes — is
chosen rather than measured; it is conservative against what `paint` and `high_water_mark`
could plausibly need if a future rebuild stopped inlining them, not a proven bound.

## Alternatives considered

**Deriving `depth_from` from the address of a local `main` declares**, rather than reading the
stack pointer register. This is what the first version of this change did, and review found
the reasoning it rested on: Rust does not promise a compiler places one local at a shallower
address than locals declared after it within the same frame, so nothing but incidental
compiler behaviour kept a future local added to `main` from landing deeper than the one this
measurement anchored to. `cortex_m::register::msp::read()` is ground truth instead — the
hardware's own answer to "how deep is the stack right now" — and it costs nothing new: it is a
plain safe function, already reachable through `cortex-m-rt`'s own dependency graph, named
directly rather than reached through a crate that does not re-export it.

**`__ebss` as the stack's lower bound.** Also the first draft's choice, and also wrong for a
reason review found rather than one anyone had argued for: `cortex-m-rt`'s linker script
places an `.uninit` section, and only after it the `_stack_end` symbol this crate now names.
The two coincide today because nothing in this workspace declares `#[link_section = ".uninit"]`
— but a future use of that section, which is exactly how retained RAM would be modelled, would
have `paint` overwrite it on every boot with no rule and no test noticing. Naming `_stack_end`
costs nothing today and stays correct the day that changes.

**`#[cortex_m_rt::pre_init]`**, which runs before `.data`/`.bss` are initialised and so would
avoid any question of frame placement entirely. Rejected: the attribute's own documentation
warns that even `&1` inside a `#[pre_init]` function or anything it calls is immediate
undefined behaviour, through rvalue static promotion, before any static is valid to touch — a
hazard this workspace cannot verify by running the result, since no board and no CI job here
can catch a bug that only sometimes reproduces. Reading the stack pointer from ordinary,
fully-initialised `main`-time code removes the reason `pre_init` would have been tempting —
frame placement — without taking on a hazard this change could not inspect its way out of.

**A generic "any `unsafe` block carrying a `// SAFETY:` comment" exception**, rather than named
functions in one named file. Rejected for matching this workspace's own convention worse:
every other pin in this gate — `EFFECT_STEP_BODIES`, `SEALING_FUNCTIONS`, and
`emulation-boot`'s own file-and-attribute checks — names a program element and requires it
found, not a floating comment convention a scanner cannot verify is even true of the code
beneath it. A comment can be copied anywhere; a function name checked against a fixed list,
in one file, at its own nesting depth, is what the rest of this gate already does.

**Threading `Trouble::Stack` through `boot::Trouble` for a degenerate paint region.** Rejected
because the two questions are independent: `boot::Trouble` is about whether the rig and the
conformance suite behaved, and a degenerate stack region is a fact about the image's own
memory map, checked in `main` before `measured_run` is ever called. Folding it in would have
made `boot::run`'s signature answer for something it does not do.
