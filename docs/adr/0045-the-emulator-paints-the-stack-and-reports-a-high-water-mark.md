# ADR 0045: the emulator paints the stack, and reports a high-water mark

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

**Holding `depth_from` inside the stack's memory map is not the same as holding it below the
live stack pointer, and review of the fix above found exactly that gap.** A stale `depth_from`
that is still a legal address somewhere in `[_stack_end, _stack_start]` — or `usize::MAX`,
which the region clamp alone converts to `_stack_start` — passes the first clamp untouched,
and `paint` would fill up to it even where the real stack pointer sits far below that address,
overwriting frames the caller and its own callees are still using. `clamp_to_stack_region` now
also takes the lower of the region-clamped value and a *fresh* [`current_stack_pointer`]
reading, taken at the moment either function is called rather than trusted from the argument:
a caller's `depth_from` can only ever narrow what gets touched, never widen it past where the
stack genuinely is. The two functions' own live readings can differ by the few bytes each
one's own call frame costs — `GUARD_BYTES` is what already exists to absorb exactly that kind
of variance, so no new margin was needed.

**Reading the live stack pointer independently at three different points in the boot is itself
the next gap, and it is sharper than a rounding error.** `available_bytes` runs once before
`paint`, and `high_water_mark` runs once after the measured boot returns; each took its own
fresh live reading, so the two could disagree by the few bytes their own call frames cost —
and, because `available_bytes` runs earliest, with the least stack consumed since `main`'s own
reading, its bound tends to be the *largest* of the three, which is exactly the wrong direction
for a safety net: a run that genuinely disturbed every byte `paint` painted could still report
`used` a few bytes short of `available`, and `StackUsage::shortfall`'s `used >= available` check
— the one line meant to catch that exact case — would not fire. `paint` now returns the bound
it resolved, and `main` passes that same value on to `high_water_mark` and to a *second*,
later call to `available_bytes`, rather than letting either re-derive an independent reading
of its own. Each still clamps defensively, so neither depends on the other for its own
soundness — but in the ordinary case a call given an already-resolved bound finds its own live
reading no smaller, so the clamp is a no-op, and the two figures are measured against the one
bound `paint` actually used.

**Reusing one bound closes the gap between two independent readings, but not the gap
`paint`'s own guard margin leaves on purpose, and review found that one too.** `paint` writes
nothing at all once the region between `_stack_end` and its resolved bound is no wider than
`GUARD_BYTES` — `high_water_mark` then reports `used = 0` over the same region, honestly, but
`available_bytes` does not know about `GUARD_BYTES` and reports the region's full width
regardless. A resolved bound 50 bytes wide is therefore a report of `used=0 available=50`,
which the shortfall check's original `available == 0` line did not catch, even though nothing
was painted and nothing was measured. `xtask::emulate::StackUsage::shortfall` now refuses any
`available` no wider than `STACK_GUARD_BYTES` — a duplicate of `stack::GUARD_BYTES`, since
`xtask` has no dependency on `waymaker-emu` to import it through, held to the real constant by
`the_duplicated_guard_bytes_matches_the_real_stack_module`, which reads the literal back out of
the shipped file rather than trusting the two copies to stay in step on their own.
`a_stack_region_no_wider_than_the_guard_is_refused_even_when_not_empty` is the test watched
failing against the `== 0` check before this closed it.

**Passing the same argument to two functions is not the same as computing one number, and
review found that `high_water_mark` and `available_bytes` still each clamped it on their
own.** Both took `resolved` — the bound `paint` returned — but each still called
`clamp_to_stack_region` itself, at its own call site, taking the lower of `resolved` and *its
own* fresh stack-pointer reading. Two calls one statement apart can still read that fresh
value differently if their own frames differ enough, so `used` and a later `available` could
still disagree by those few bytes — the same shape of gap the bound-reuse fix above closed
between `paint` and its callers, reopened one level down between `high_water_mark` and
`available_bytes`. `high_water_mark` now returns *both* numbers, computed from the one
`depth_from` it resolves for itself in that single call, through `region_bytes` — a shared
arithmetic function neither figure can be computed from a different bound at, because there
is now only the one place either is computed. What `available_bytes` alone is still for is
the pre-`paint` gate check, where `paint` has not run yet and there is no shared bound yet to
reuse.

**A safe `pub fn` scanning raw stack bytes needs more than a bounded address, and a sixth
finding said so.** `clamp_to_stack_region` keeps every read `high_water_mark` performs inside
RAM this image owns, but "inside RAM" is not "initialized": a byte `paint` never wrote is not
`POISON` and is not a byte the run touched either — it is memory nobody has told the compiler
anything about, and `read_volatile`ing it is undefined behavior regardless of how carefully
the address is bounded. Before this, that precondition — call `paint` first — lived only in
`high_water_mark`'s own doc comment, and nothing stopped a safe caller from reaching the
function directly with an arbitrary `usize` and no prior `paint` call at all. `paint` now
returns `Painted`, a type with no public constructor of its own, and `high_water_mark` takes
one instead of a bare `usize`: the one call in this crate that produces a `Painted` is `paint`
itself, so a caller who has not painted cannot name a value of the type this scan requires.
The fix costs nothing at the one real call site in `main`, which already passed `paint`'s own
return value straight through.

**"The stack pointer" is two registers in Thread mode, and a seventh finding is that reading
one of them unconditionally was never argued, only true by accident.** `CONTROL.SPSEL` says
which of MSP and PSP is live, and `current_stack_pointer` read MSP regardless. Nothing in this
image ever sets `SPSEL` — there is no RTOS and no second stack here — so MSP has in fact always
been the active pointer, and every figure this ADR reports is correct for that reason. That
reason lives in how this crate happens to be used today, not in `current_stack_pointer`'s own
signature: it is `pub`, reachable from any future caller in any future execution context, and
nothing pins it to the one file `paint` and `high_water_mark` are pinned to. `current_stack_pointer`
now reads `CONTROL` first and follows `SPSEL` to whichever register is actually live, so the
function is sound on its own terms rather than sound because nothing has asked it the hard
question yet.

**The extern-block exemption was a range, and an eighth finding is the same shape of gap
`sole_depth_zero_unsafe` had already closed for the two permitted functions.** `covers`
treated *every* `unsafe` occurrence between the block's opening brace and its closing one as
part of the one exemption the 2024 edition's linker-symbol block needs — so a second,
unrelated `unsafe` occurrence sitting anywhere inside those braces, beside the two permitted
statics rather than replacing either, passed unnoticed. The fix mirrors the one already used
for `paint` and `high_water_mark`: only the offset of the block's own opening `unsafe` keyword
is stored and matched exactly, so a sibling `unsafe` elsewhere in the block is refused rather
than inherited. `a_second_unsafe_hidden_inside_the_extern_block_is_reported` is the test
watched failing against the old range check before this closed it.

**`SPSEL` is a Thread-mode question, and a ninth finding is that the seventh's own fix
answered it unconditionally.** Handler mode — running an exception — always executes on MSP,
whatever `CONTROL.SPSEL` says; `SPSEL` only governs which register Thread mode uses. Nothing
in this image installs a handler that calls `current_stack_pointer`, so the gap was again one
of signature rather than of the one real call site — a future caller reached from an
exception, after Thread mode had selected PSP, would have read `SPSEL` and returned the
*inactive* register. `SCB::vect_active()` is checked first now — a safe function, reading a
read-only status register with no side effects, the same shape of safety `msp::read()` and
`psp::read()` already have — and Handler mode answers MSP without consulting `SPSEL` at all;
only `VectActive::ThreadMode` reaches the `SPSEL` check the seventh finding added.

**Answering "which register is active" correctly is not the same as answering "is it safe to
paint below it", and a tenth finding is that the ninth's own fix conflated the two.** MSP
genuinely is the active register in Handler mode — that part of the ninth finding's fix is
right — but `paint` does not merely need to know which register is active; it needs "every
byte between `_stack_end` and that register is unused", and that second claim can be false in
Handler mode in a way no register read can rescue: an exception can interrupt Thread mode
while Thread mode was using PSP for a *second* stack, and this module's single `_stack_end` /
`_stack_start` pair has no way to represent two regions or to know where that interrupted PSP
frame sits. Reading MSP correctly identified the active register and then let
`clamp_to_stack_region` treat it as if it were the only stack this image has, which is exactly
the assumption a second stack breaks. `clamp_to_stack_region` now checks `SCB::vect_active()`
itself, before it does anything else, and collapses to the empty region at `stack_floor()`
outside Thread mode — the same degenerate shape a region no wider than `GUARD_BYTES` already
produces, which `paint` already declines to write into and `StackUsage::shortfall` already
refuses as a measurement that did not happen. No new mechanism was needed to close it, because
the failure mode this closes is already one this module knows how to refuse. This image
installs no handler that reaches `crate::stack` at all, so the branch is dead code on every
boot this ADR measures — it exists for the caller this crate does not have yet, the same
standing `CLAUDE.md`'s "What is not checked" section already states for gaps a scanner or a
runtime check cannot see past.

**Handler mode was one half of "MSP is not necessarily the stack in use", and an eleventh
finding is that Thread mode is the other half of the same question.** The tenth's fix checked
processor *mode* — Handler or Thread — but `SPSEL` can name PSP while Thread mode is still the
mode in force, and nothing about being in Thread mode says PSP is inside
`[_stack_end, _stack_start]`: a PSP reading from a stack this module knows nothing about can
sit *above* `_stack_start` outright, at which point `clamp_to_stack_region`'s own
`.min(current_stack_pointer())` stops narrowing anything — the region clamp alone decides the
bound, which is exactly the protection the live-pointer check exists to add, silently absent.

**The eleventh's own fix reframed the question in a way that quietly undid the tenth's, and a
twelfth finding is that regression.** It restated the check as "is MSP the register in use at
all" and answered `true` unconditionally in Handler mode — which is a true fact about which
register is active, but not the fact `clamp_to_stack_region` needs: the tenth's whole point was
that MSP being active in Handler mode does *not* make the memory below it safe, because an
exception can land there having interrupted a Thread-mode context that was using PSP for a
second, still-live stack. Answering the "which register" question and calling that "safe"
collapsed the tenth's unconditional Handler-mode refusal back into the ordinary clamp-and-min
path — precisely the state the tenth finding closed. The two conditions are conjunctive, not a
choice of which one to ask: `clamp_to_stack_region` now trusts the live reading in exactly one
state, Thread mode with `SPSEL` itself naming MSP, and collapses to the empty region in every
other one — Handler mode included, unconditionally, regardless of what register answers there.
This image runs in Thread mode with MSP selected for the whole of every boot this ADR
measures, so the collapse is not one this boot's own measurement ever takes; it exists for the
caller this crate does not have yet.

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

**None of the twelve hardenings changed what the figure means, only what a wrong caller could do
to it, how the one real caller is sequenced, and how strictly the gate reads a degenerate or
an internally inconsistent report.** `clamp_to_stack_region` is a floor-and-ceiling clamp plus
a live-stack-pointer clamp, not a new measurement path, and what changed is the *worst case*
for an argument this ADR's own text had already named as an obligation on the caller rather
than a check: it is now a check too. `main` calls `available_bytes` once, on its own original
reading, to decide whether to attempt a measurement at all; the reported `used` and
`available` both then come from the single later call to `high_water_mark`, against the bound
`paint` returned. That one early reading sits within the handful of bytes `available_bytes`'s
own call frame costs of `main`'s original reading, which is inside the noise `GUARD_BYTES`
already exists to absorb and smaller than what the ADR's own lower-bound honesty already asks
a reader to expect.

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
