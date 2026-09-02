# ADR 0010: The integrity check is catalogued and table-free

- Status: accepted
- Date: 2026-09-02

Settles deferred question: `integrity-check-algorithm`

## Context

Design document §16 leaves five questions open, and the first is "whether the default
integrity check is CRC32C or a smaller table-free CRC implementation". Issue
[#16](https://github.com/madmax983/waymaker/issues/16) says how to answer it: "decide with
measured code size and cycles per byte".

`waymaker-flash` has been computing two checksums since rung 0.1 — CRC-16/CCITT-FALSE over
the twelve-byte header, CRC-32/ISO-HDLC over the whole frame — both bitwise, and
[ADR 0007](0007-the-record-frame-is-checksummed-twice-and-the-kernel-owns-none-of-it.md)
records *why there are two*. It does not record why they are *these* two. That is the gap
this ADR closes, and it has to close before 1.0: a checksum is part of the wire format, and
the format freezes there.

§09 states the job: "CRC detects accidental corruption and torn writes; it is not
authentication." Nothing here needs a cryptographic digest, and nothing here needs the
error-detection properties of one polynomial over another at the record lengths involved —
both candidates are degree-32 CRCs with Hamming distance 4 well past a 64 KiB payload.

### The measurement

Four candidates, each compiled as a standalone function for `thumbv6m-none-eabi` under the
release profile design document §04 pins (`opt-level = "z"`, `lto = "fat"`,
`codegen-units = 1`), with `rustc 1.97.1` — the version `rust-toolchain.toml` pins.
Sizes are `llvm-nm --print-size` on the resulting archive; instruction counts are read off
`llvm-objdump -d`; cycle counts apply published Cortex-M0+ timings to those instructions
(ALU 1 cycle, `ldr` 2, taken branch 3, untaken branch 1).

| Candidate | `.text` | `.rodata` | Total | Instructions/byte | Cycles/byte (M0+) |
| --- | --- | --- | --- | --- | --- |
| CRC-32/ISO-HDLC, bitwise (**shipped**) | 52 B | 0 B | **52 B** | 78 | ~107 |
| CRC-32C (Castagnoli), bitwise | 52 B | 0 B | 52 B | 78 | ~107 |
| CRC-32C, 16-entry nibble table | 52 B | 64 B | 116 B | 17 | ~22 |
| CRC-32C, 256-entry byte table | 44 B | 1024 B | 1068 B | 12 | ~16 |
| CRC-16/CCITT-FALSE, bitwise (**shipped**) | 60 B | 0 B | 60 B | 78 | ~107 |

The cycle figures are a static model over the real instruction stream, not a run on
silicon. They are load-free in the bitwise cases, so the model has little room to be wrong;
the two table rows depend on flash wait states and are a floor.

Two things fall out, and the first is the one that makes the question as §16 asks it the
wrong question.

**The polynomial is free.** CRC-32C and CRC-32/ISO-HDLC assemble to the same 52 bytes and
the same 78 instructions per byte. Cortex-M0+ has no CRC instruction, so the polynomial is
an immediate either way. §16 offers "CRC32C **or** a smaller table-free CRC" as a trade;
on this target there is no trade, because the table — not the polynomial — is what costs.

**The table is where the decision is.** A nibble table buys 4.9× for 64 B of rodata; a byte
table buys 6.7× for 1024 B, which is 12.5% of §04's entire 8 KiB incremental code-flash
budget for the kernel and the flash adapter together.

### What a checksum actually costs at these speeds

Worth writing down, because the `crc` module previously asserted that a few hundred
shift-and-xor iterations "cost nothing anybody can measure", and the measurement does not
support that at the top of the range. At 48 MHz and ~107 cycles per byte:

- a 24-byte `EffectScheduled` record: ~2,600 cycles, ~54 µs;
- a full 512-byte scratch page: ~54,800 cycles, ~1.14 ms.

For comparison, a typical SPI NOR page program is quoted at 0.4–0.7 ms and up to 3 ms. So
the checksum is a small fraction of the barrier it precedes for a small record, and roughly
the same order as the program itself for a full page. Not free; not dominant.

## Decision

**The integrity check is CRC-32/ISO-HDLC over the frame and CRC-16/CCITT-FALSE over the
header, both computed bitwise with no lookup table.** CRC-32C is rejected.

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
   107-cycle-per-byte checksum currently misses. Spending 12.5% of the code-flash budget
   against no stated requirement is how a budget goes.

The header keeps its own CRC-16 for the reason ADR 0007 gives — `payload_len` has to be
trusted before the frame's end can be located — and it is 16 bits rather than 32 because it
is paid on every record and covers ten bytes.

`input_crc` on a scheduled effect is this same CRC-32/ISO-HDLC. There is one definition of
"the digest" in this repository and
[ADR 0011](0011-a-scheduled-effect-records-a-length-and-a-digest.md) depends on it.

### What would revisit this

Not a preference, and not a benchmark of the checksum on its own. A profile of a real
workload on real flash, against a latency requirement §04 does not currently state, showing
the checksum on the critical path. If that arrives, the answer is most likely the nibble
table — 4.9× for 64 B — and it is a **superseding ADR**, not an edit to this one and not a
quiet commit.

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
