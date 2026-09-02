# ADR 0012: The integrity check is swappable behind a trait, and the seal widths are not

- Status: accepted
- Date: 2026-09-02
- Issue: [#17](https://github.com/madmax983/waymaker/issues/17)
- Supersedes: nothing
- Related: [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md), [0010](0010-the-integrity-check-is-catalogued-and-table-free.md), [0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md)

## Context

[ADR 0010](0010-the-integrity-check-is-catalogued-and-table-free.md) settles §16's first
deferred question — CRC-32/ISO-HDLC over the header and payload, CRC-16/CCITT-FALSE over
the header, both table-free — with measurements taken on `thumbv6m-none-eabi`. It settles
*which algorithm*. Issue [#17](https://github.com/madmax983/waymaker/issues/17) asks for
three things that decision does not supply on its own, and this ADR is the design that
answers them.

**The choice has to stay swappable.** #17: "Keep the implementation **out** of
`waymaker-core` — the kernel does not own a CRC. It belongs behind a `waymaker-flash` trait
or feature so the choice stays swappable." The kernel half was already true; the swap point
was not. `frame.rs` called `crc16` and `crc32` by name, so ADR 0010's own
["What would revisit this"](0010-the-integrity-check-is-catalogued-and-table-free.md#what-would-revisit-this)
— a nibble table against a latency requirement, or CRC-32C against a record past 11.2 KiB —
would have arrived as edits to the codec. A codec edited to change a checksum is a codec
that can stop sealing what it says it seals, and the diff that does it looks like a
one-line substitution.

**The widths have to be settled.** #17's second "done when" is that "the frame's
`header_crc` / `payload_crc` widths are settled as a result". ADR 0010 gives the reasoning
for the header's sixteen bits in one sentence and nothing states the frame's thirty-two at
all: they were facts about a struct literal in `encode`.

**The rejected candidate has to exist.** ADR 0010 rejects CRC-32C on a measurement, and no
CRC-32C was in the repository to have been measured against. A rejection with no runnable
candidate behind it is an assertion, and #17 asks to "implement or evaluate both
candidates".

## Decision

**The two seals go through a `waymaker-flash` trait, `IntegrityCheck`; the shipped
implementation is `Catalogued`; and the seal widths are the trait's return types rather
than anything an implementation may choose.**

```rust
pub trait IntegrityCheck {
    fn header_check(bytes: &[u8]) -> u16;
    fn frame_check(bytes: &[u8]) -> u32;
}
```

Four things follow from that shape, each of them deliberate.

1. **A type parameter, not a cargo feature.** #17 offers either. Cargo features are
   additive and unify across a dependency graph, so two features naming two algorithms are
   two features that can both be enabled — which for a wire format means a firmware that
   seals with one check and verifies with another, produced by a dependency nobody read. A
   type parameter cannot be in two states at once.
2. **Static methods, so an implementation is a marker.** Neither method takes `self`. There
   is nothing to construct, nothing to pass down a call chain, and nothing to keep in sync
   between a writer and a reader. `Scan<'a, C = Catalogued>` carries its choice as a
   zero-sized `PhantomData`.
3. **The default is the shipped answer, and the trait is free.** `encode`, `decode` and
   `Scan::new` are the `Catalogued` instantiations of `encode_with`, `decode_with` and
   `Scan::with_integrity`, as `#[inline]` wrappers, so existing code did not change and
   there is one monomorphisation in the image. `input_digest` is the exception and stays a
   `const fn` calling `crc32` directly, for the reason in
   [Consequences](#consequences): a trait method cannot be `const`.

   The *trait* costs nothing — holding the size probe unchanged, `cargo xtask size`
   measures **7288 B** against **7296 B** before, and the eight bytes are codegen noise. The
   *row* the gate prints is **7560 B**, because `size-probe-reach` makes the probe name both
   entry points and it then runs one codec body twice. That number, its cause and what it
   costs the budget are in [Consequences](#consequences), which is where a figure a reader
   can check against `cargo xtask size` belongs.
4. **The widths are not swappable.** §09's frame spends two bytes on `header_crc` and four
   on `payload_crc`, and [ADR 0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md)
   freezes the header layout across format versions so that a reader can find the end of a
   frame it cannot interpret. A `header_check` returning a `u32` is therefore not an
   implementation detail — it is two more bytes per record, on media, for the life of the
   format. The signature is where that is stated, `frame::HEADER_CRC_BYTES` and
   `frame::FRAME_CRC_BYTES` are what say it in bytes, and `const` assertions in `frame.rs`
   fail the build if the two stop agreeing.

**Swapping to an implementation that computes different seals is a wire-format change, and
it is loud.** A frame sealed with one such implementation fails `decode` with
`IntegrityFailed` under another — at the header when the header check differs, at the
trailer otherwise. That is the §14 answer, "frame ignored; previous history prefix wins",
rather than a journal walked wrong, and
`a_frame_sealed_by_one_check_is_refused_by_the_other` is the test that pins it.

Swapping to a different implementation of the *same* algorithm is not a format change, and
that distinction is the reason this is a trait rather than a hard-wired call. ADR 0010's
"what would revisit this" predicts exactly one such implementation — a nibble table, bought
against a latency requirement §04 does not yet state — and says that "a table is an
implementation of an algorithm and not a different one". `every_candidate_computes_the_same_crc32c`
is what stops that being a claim: the bitwise, nibble-table and byte-table forms of CRC-32C
are all in `tests/integrity.rs` and all three agree over every input length up to 137.

**The rejected candidate ships as a test, not as firmware.** CRC-32C is implemented in
`crates/waymaker-flash/tests/integrity.rs` in all three forms ADR 0010 measured — bitwise,
nibble table, byte table — and the bitwise one is anchored to its published check value
`0xE306_9283`. That is §16's other candidate, made runnable: the `.rodata` figures issue #17
asks to be measured (64 B and 1024 B) are properties of the two table forms and of nothing
else, so a repository that held neither had described its own measurement rather than kept
it. CRC-16/ARC is there too, anchored to `0xBB3D`; it is not a §16 candidate but a second
*differing* header hook, without which no test could tell whether the codec consults
`header_check` at all.

Together they are the second implementation of `IntegrityCheck`, which proves the swap point
is real rather than a trait only its default has ever satisfied. They are not in `src/`,
because ADR 0010 rejected CRC-32C and a rejected algorithm compiled into firmware is an
invitation to a wire-format fork.

### The failure modes, confirmed

#17 asks for the choice to be confirmed against "torn writes at program-unit boundaries and
stale erased-tail bytes". Both are swept in `tests/integrity.rs` rather than argued:

| Failure mode | What is swept | Test |
| --- | --- | --- |
| Write torn at a program-unit boundary | every boundary of every frame, at alignments 1, 2, 4, 8, 16 and 256, for all six record variants, tail erased | `a_write_torn_at_a_program_unit_boundary_is_never_read_as_a_record` |
| Write torn inside a program unit | every byte offset of every frame | `a_write_torn_inside_a_program_unit_is_never_read_as_a_record` |
| Torn tail behind committed history | the prefix survives and the scan stops *at* the tear | `a_torn_write_leaves_the_committed_prefix_of_earlier_records_intact` |
| Stale erased tail | an erased tail ends history; anything programmed after an erased run does not | `a_stale_erased_tail_is_the_end_of_history_and_a_programmed_one_is_not` |
| Partial program (NOR clears bits, never sets them) | every `1` bit of every frame byte, cleared | `every_bit_a_partial_program_could_clear_is_caught` |
| Burst error | every pattern of every burst up to nine bits, at every bit position | `every_burst_error_up_to_the_check_width_is_caught` |
| Erased or zeroed page | both, through `decode` and through `Scan` | `an_erased_page_and_a_zeroed_page_are_never_records` |

### What is still not authentication

#17 puts authentication out of scope in those words, and §09 agrees: "CRC detects
accidental corruption and torn writes; it is not authentication." Anyone who can write the
media can rewrite a record and reseal it. Rather than state that in prose and hope no
documentation drifts, it is a passing test —
`a_resealed_forgery_is_accepted_because_a_crc_is_not_authentication` — which encodes a
forged record, reseals it with the shipped check, and asserts that the codec hands it back
as history. No implementation of `IntegrityCheck` changes that, and none may be documented
as if it did.

## Consequences

- The `integrity-check` gate rule grew two more halves, both pinned by
  `xtask::source::SEAL_BINDINGS`, and it stays one rule id because it is one decision: ADR
  0010 says which loops, and this says the codec still reaches them, at the same widths.
  - **The binding.** A `waymaker-flash/src/integrity.rs` that is gone, a trait or a shipped
    `impl` that is renamed, *declared twice* — a decoy above the real one defeated the first
    version of this rule, so ambiguity is now a violation rather than a tie-break — a seal
    whose signature stops returning `u16` or `u32`, or a `Catalogued` whose method body is
    anything but one unqualified call to `crc16` or `crc32`. A token count was not enough:
    `fast::crc32(bytes)` calling a Castagnoli loop in a sibling module satisfied it, with
    `crc.rs` untouched so the other half passed too.
  - **The routing.** `encode_with` and `decode_with` must compute each seal through
    `C::header_check` and `C::frame_check` exactly once and must not name a checksum function
    at all; `input_digest` must call `crc32` once; and the scan's `next` must walk with
    `decode_with`. Without this the trait was pinned and nothing was obliged to call it — a
    codec hard-wired straight back to `crc16` and `crc32`, with `integrity.rs` perfect,
    passed all 34 rules.
- `waymaker-flash` grew two public types (`IntegrityCheck`, `Catalogued`), four public
  functions (`encode_with`, `decode_with`, `input_digest_with`, `Scan::with_integrity`) and
  two public constants (`frame::HEADER_CRC_BYTES`, `frame::FRAME_CRC_BYTES`). Every one of
  the functions, and both trait methods, is called by the size probe, because
  `size-probe-reach` requires it; constants are outside that rule.
- The `default` size row moved from 7296 B to **7560 B**, and about 270 B of that is the
  probe rather than the library: `size-probe-reach` wants both `encode` and `encode_with`
  named in call position, and the probe now runs the same codec body twice with its own
  argument setup and fold arithmetic each time. Measured: 7288 B with a single entry point
  per body, 7560 B with both. The delta over-charges rather than under-charges, which is
  the direction a budget can survive, and the second call site can be dropped when a real
  caller of `encode_with` exists to reach it.
- `input_digest` stays a `const fn` and `input_digest_with` cannot be one, because a trait
  method cannot be `const`. The two are pinned equal by
  `an_input_digest_is_the_frame_check_of_whatever_seals_the_frame`: a build that sealed
  frames with one check and digested activity inputs with another would record digests no
  replay could reproduce, and §08's divergence comparison would fail on every effect.
- `Scan` gained a type parameter with a default, so `Scan<'_>` still means what it meant and
  no existing signature changed. It derives `Clone`, `Debug`, `PartialEq` and `Eq`, so those
  bounds now fall on `C` — a marker meant to be used with the scan derives them too.
- This ADR does **not** settle a deferred question and does not carry the marker: §16's
  first question is ADR 0010's, and two ADRs claiming one question fail the
  `deferred-questions` rule.

## Alternatives considered

**A cargo feature per algorithm.** Rejected for the additivity reason above, and for a
second: `empty-default-features` requires a firmware crate's `default` feature to enable
nothing, so the shipped check would have had to be the *absence* of a feature — which makes
"what does this image seal with" a question about the whole dependency graph rather than
about a type.

**A `&dyn IntegrityCheck` passed to the codec.** Rejected on cost and on `const`: a vtable
and an indirect call per seal on a core with no branch predictor, and `input_digest` could
not stay a `const fn`.

**Const generics over the polynomial and initial value.** Rejected because it parameterises
the wrong thing. A nibble table is the same polynomial and a different implementation; a
reflected loop and a table-driven one do not differ by a constant. Naming the parameters
would have made the one change ADR 0010 predicts — the table — the one change the design
could not express.

**Leaving it hard-wired and closing #17 on ADR 0010.** Rejected because three of the
issue's four "Work" bullets are about exactly what the ADR does not do: implement the
rejected candidate, confirm the failure modes, and put the choice behind a swap point.
