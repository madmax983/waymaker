# ADR 0010: The integrity check is catalogued and table-free

- Status: accepted
- Date: 2026-09-02
- Issue: [#16](https://github.com/madmax983/waymaker/issues/16)
- Supersedes: none
- Related: [0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md), [0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md), [0012](0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md)

Settles deferred question: `integrity-check-algorithm`

## Context

Design document §16 leaves five questions open, and the first is "whether the default
integrity check is CRC32C or a smaller table-free CRC implementation". Issue
[#16](https://github.com/madmax983/waymaker/issues/16) says how to answer it: "decide with
measured code size and cycles per byte".

`waymaker-flash` has been computing two checksums since rung 0.1 — CRC-16/CCITT-FALSE over
the first ten bytes of the twelve-byte header, CRC-32/ISO-HDLC over the header and payload
— both bitwise, and
[ADR 0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md)
records *why there are two*. It does not record why they are *these* two. That is the gap
this ADR closes, and it has to close before 1.0: a checksum is part of the wire format, and
the format freezes there.

§09 states the job: "CRC detects accidental corruption and torn writes; it is not
authentication." Nothing here needs a cryptographic digest. The two candidates' error detection is *not*
identical, though, and the difference runs the other way from what one might assume:
CRC-32C has `(x+1)` as a factor and so detects every odd-weight error out to ~2³¹ bits,
while CRC-32/ISO-HDLC is primitive and does not — its first undetected 3-bit error is at a
dataword of 91,607 bits, about **11.2 KiB**, beyond which it drops from Hamming distance 4
to 3. (Both figures reproduce Koopman's published bounds.) That matters against
`MAX_FRAME_BYTES`, which is 65,551, and does not matter at the extents this format actually
checksums: 20 bytes for an `EffectScheduled` record and at most 508 for a scratch page,
where ISO-HDLC gives HD ≥ 5. The decision below is taken knowing this, and
["What would revisit this"](#what-would-revisit-this) names it as the one thing that would
reopen it.

### The measurement

Four candidates, each compiled as a standalone function for `thumbv6m-none-eabi` under the
release profile design document §04 pins (`opt-level = "z"`, `lto = "fat"`,
`codegen-units = 1`), with `rustc 1.97.1` — the version `rust-toolchain.toml` pins.
Sizes are `llvm-nm --print-size` on the resulting archive; instruction counts are read off
`llvm-objdump -d`; cycle counts apply the Cortex-M0+ instruction timings in
[the RP2040 datasheet's Table 81](https://datasheets.raspberrypi.com/rp2040/rp2040-datasheet.pdf)
to those instructions — ALU 1 cycle, `ldr`/`ldrb` 2, **taken branch 2**, untaken branch 1.
The taken-branch figure is worth stating explicitly: 3 is the Cortex-M0/M3 number, and the
M0+'s two-stage pipeline makes it 2. An earlier draft of this ADR used 3 and inflated every
cycle figure below by about 13%.

| Candidate | `.text` | `.rodata` | Total | Instructions/byte | Cycles/byte (M0+) |
| --- | --- | --- | --- | --- | --- |
| CRC-32/ISO-HDLC, bitwise (**shipped**) | 52 B | 0 B | **52 B** | 74–82, mean 78 | **93** |
| CRC-32C (Castagnoli), bitwise | 52 B | 0 B | 52 B | 74–82, mean 78 | 93 |
| CRC-32C, 16-entry nibble table | 52 B | 64 B | 116 B | 17 | 21 |
| CRC-32C, 256-entry byte table | 44 B | 1024 B | 1068 B | 12 | 15 |
| CRC-16/CCITT-FALSE, bitwise (**shipped**) | 60 B | 0 B | 60 B | 83–91, mean 87 | **102** |

Three things about those numbers. The instruction count for a bitwise loop is
data-dependent — the polynomial xor is branched over when the bit is clear — so it is a
range with a mean, not a figure. The **cycle** count is not: at 2 cycles for a taken branch
the two paths cost the same, so 93 and 102 are exact rather than averaged. And CRC-16 is
*not* the same loop as CRC-32, which an earlier draft assumed: LLVM tests the top bit with
`sxth`/`cmp`/`bpl` rather than a shift, one instruction more per bit, which makes the
16-bit checksum the more expensive of the two per byte.

The figures are a static model over the real instruction stream, not a run on silicon. The
bitwise rows are load-free, so the model has little room to be wrong; the two table rows
depend on flash wait states and are a floor.

Two things fall out, and the first is the one that makes the question as §16 asks it the
wrong question.

**The polynomial is free.** CRC-32C and CRC-32/ISO-HDLC assemble to the same 52 bytes and
the same 78 instructions per byte. Cortex-M0+ has no CRC instruction, so the polynomial is
an immediate either way. §16 offers "CRC32C **or** a smaller table-free CRC" as a trade;
on this target there is no trade, because the table — not the polynomial — is what costs.

**The table is where the decision is.** A nibble table buys 4.4× for 64 B of rodata; a byte
table buys 6.2× for 1024 B, which is 12.5% of §04's entire 8 KiB incremental code-flash
budget for the kernel and the flash adapter together.

### What a checksum actually costs at these speeds

Worth writing down, because the `crc` module previously asserted that a few hundred
shift-and-xor iterations "cost nothing anybody can measure", and the measurement does not
support that at the top of the range.

The bytes checksummed are not the bytes written, which an earlier draft conflated. A record
is sealed twice and neither seal covers itself: `frame_crc` covers `HEADER_BYTES +
payload_len`, and `header_crc` covers `HEADER_BYTES - 2`. So for an `EffectScheduled`
record that is 20 bytes of CRC-32 and 10 of CRC-16, not the 24 bytes the record occupies.
At 48 MHz — an ordinary M0+ clock for an RP2040 or SAMD21 class part, and a figure §04
does not state, so it is an assumption of this ADR rather than a requirement:

- an `EffectScheduled` record — 20 B × 93 + 10 B × 102 — is **2,880 cycles, ~60 µs**;
- a full 512-byte page — at most 508 B covered, plus the header — is **~48,300 cycles,
  ~1.0 ms**.

For comparison, a typical SPI NOR page program is quoted at 0.4–0.7 ms and up to 3 ms. So
the checksum is a small fraction of the barrier it precedes for a small record, and roughly
the same order as the program itself for a full page. Not free; not dominant.

### Reproducing it

The candidates are five standalone functions, each the loop named in the table over a
`&[u8]`, compiled as a `staticlib` for the firmware target under the workspace's release
profile:

```sh
cargo build --release --target thumbv6m-none-eabi     # with [profile.release] as §04 pins
llvm-nm --print-size --size-sort --defined-only target/thumbv6m-none-eabi/release/lib*.a
llvm-objdump -d --triple=thumbv6m-none-eabi target/thumbv6m-none-eabi/release/lib*.a
```

The bodies are the two in `crates/waymaker-flash/src/crc.rs` plus three variants of them:
the same reflected loop with `0x82F6_3B78` for CRC-32C, and CRC-32C folded four bits and
eight bits at a time against a `[u32; 16]` and a `[u32; 256]` generated by the same loop.
All three CRC-32C variants are now in the repository — `crc32c`, `crc32c_nibble` and
`crc32c_byte` in `crates/waymaker-flash/tests/integrity.rs`, added for issue
[#17](https://github.com/madmax983/waymaker/issues/17), with their fold tables as `static`
arrays generated at compile time from the bitwise loop. `every_candidate_computes_the_same_crc32c`
holds all three to one answer on every run of `cargo test`, which is what turns this ADR's
load-bearing claim — that "a table is an implementation of an algorithm and not a different
one" — from a sentence into something CI can fail over.

**Be precise about what that does and does not give you**, because Codex was right to push
on the first version of this paragraph. Those three are the *algorithms*, checked. They are
not the compilation units the size figures came from: they live in an integration-test
target, and the command above builds a `staticlib` and reads `lib*.a`, which no test target
contributes to. Rebuilding the table above still means what it always meant — lifting the
five bodies into a `staticlib` crate for `thumbv6m-none-eabi` under §04's release profile,
by hand. What the repository now saves you is writing the bodies and trusting that they are
the algorithms they claim to be.

CI does not re-run the measurement — there is no cycles budget for it to gate, which is
[the point](#what-would-revisit-this) — so the numbers above remain a dated measurement
rather than a checked one, stated precisely enough to be checked by hand. Making them a
gated row of `cargo xtask size` is
[issue #61](https://github.com/madmax983/waymaker/issues/61); it is a change to the size
harness rather than to this decision, which is why it is not in this ADR.

## Decision

**The integrity check is CRC-32/ISO-HDLC over the header and payload, and
CRC-16/CCITT-FALSE over the header's first ten bytes, both computed bitwise with no lookup
table.** CRC-32C is rejected.

Three reasons, in the order the evidence supports them.

1. **Cost does not choose between the polynomials**, so something else has to. The
   measurement above is what rules the cost argument out rather than assuming it away.
2. **CRC-32/ISO-HDLC is the one a host can check without reimplementing anything.** It is
   zlib's, gzip's and PNG's; every mainstream language has it in its standard library or
   one import away. A journal pulled off a device is verifiable by a tool nobody had to
   write, and a checksum verified only against the implementation that produced it is a
   checksum that agrees with its own bugs. CRC-32C's real advantage is hardware
   acceleration on SSE4.2 and ARMv8 — neither of which is Cortex-M0+, and neither of which
   this decision can spend.
3. **Table-free, because the table is not free and nothing has asked for the speed.** §04
   budgets code flash and RAM; it budgets no latency at all, so there is no number a
   93-cycle-per-byte checksum currently misses. Spending 12.5% of the code-flash budget
   against no stated requirement is how a budget goes.

The header keeps its own CRC-16 for the reason ADR 0007 gives — `payload_len` has to be
trusted before the frame's end can be located — and it is 16 bits rather than 32 because it
is paid on every record and covers ten bytes.

`input_crc` on a scheduled effect is this same CRC-32/ISO-HDLC. There is one definition of
"the digest" in this repository and
[ADR 0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md) depends on it.

### What would revisit this

Two things, and neither is a preference.

A profile of a real workload on real flash, against a latency requirement §04 does not
currently state, showing the checksum on the critical path. If that arrives the answer is
most likely the nibble table — 4.4× for 64 B — and it stays CRC-32/ISO-HDLC, because a
table is an implementation of an algorithm and not a different one.

Or a record whose checksummed extent goes past **11.2 KiB**, where ISO-HDLC's Hamming
distance falls from 4 to 3 and CRC-32C's does not. Today the largest thing sealed in one go
is a 512-byte scratch page, two orders of magnitude short of it; `MAX_FRAME_BYTES` is
65,551 and nothing writes a record near it. A rung that starts to would be choosing between
CRC-32C and a smaller maximum record, and that is a different decision from this one.

Either is a **superseding ADR**, not an edit to this one and not a quiet commit.

## Consequences

- The `integrity-check` gate rule fails a pull request that changes either polynomial or
  initial value, or that adds a `const`/`static` array to `crates/waymaker-flash/src/crc.rs`.
  `#[cfg(test)]` modules are exempt: the bit-flip sweep's `const MESSAGE: [u8; 12]` is a
  fixture, not a lookup table.
- The parameters are pinned as the literals they appear as, in
  `xtask::source::INTEGRITY_CHECK_PARAMETERS`. A reflected polynomial changed from
  `0xEDB8_8320` to `0x82F6_3B78` passes every round-trip test in this repository and fails
  against every zlib in the world, which is exactly the class of change a round-trip test
  cannot see.
- Adding a table is now a visible decision with a number attached rather than an
  optimisation somebody notices later in a size report.
- The `crc` module's own claim that the bitwise loop "costs nothing anybody can measure" was
  wrong for a full page and has been replaced with the figures above.
- This does not gate the *check values*. `crc16(b"123456789") == 0x29B1` and
  `crc32(b"123456789") == 0xCBF4_3926` are asserted by the module's tests, which is where an
  external anchor belongs; the gate pins the parameters so that a change to them is
  deliberate, and the tests catch an implementation that no longer computes them.
