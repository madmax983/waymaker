# The Waymaker wire format, version 1

This is the byte-by-byte specification of everything a Waymaker device puts on media.
Design document §09 is what it implements; issue
[#41](https://github.com/madmax983/waymaker/issues/41) is what freezes it.

**The promise.** Records written by a shipped device stay readable by every later 1.x
firmware. Nothing in this document changes inside 1.x except by the rules in
[Changing the format](#changing-the-format), and the migration policy is
[ADR 0037](https://github.com/madmax983/waymaker/blob/main/docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md).

**What holds it.** The frozen bytes in
[`crates/waymaker-flash/tests/corpus/v1`](https://github.com/madmax983/waymaker/blob/main/crates/waymaker-flash/tests/corpus/v1/README.md),
run by `crates/waymaker-flash/tests/corpus.rs` as the `corpus` CI stage, and the
`wire-format` gate rule. A field width or a record number that moves fails a build.

## Conventions

| Rule | Value |
| --- | --- |
| Byte order | Little-endian, every multi-byte integer, everywhere. |
| Field alignment | None. Fields are packed at the offsets stated. A *record* is padded to the device's program unit, which is a property of the device rather than of the format. |
| Erased byte | `0xFF`. A constant, never learned from the device. |
| Padding | Written `0xFF`, never read. |
| Signed integers | None. Every field is unsigned. |

## The frozen numbers

Every number below fixes a byte on media and none of them may move inside 1.x. The
`wire-format` gate rule compares each row against the declaration that carries it, so this
table and the code cannot drift apart.

| Constant | Value | Fixes |
| --- | --- | --- |
| `MAGIC` | `0x4D57` | the two bytes a record frame begins with |
| `FORMAT_VERSION` | `1` | the version a v1 writer stamps on every frame |
| `OLDEST_READABLE_FORMAT_VERSION` | `1` | the oldest version a v1 reader accepts |
| `HEADER_BYTES` | `12` | the frame header |
| `TRAILER_BYTES` | `4` | the frame trailer |
| `HEADER_CRC_BYTES` | `2` | the header check on media |
| `FRAME_CRC_BYTES` | `4` | the frame check on media |
| `SEAL_PATTERN_BYTES` | `4` | the commit seal's repeating pattern |
| `MAX_PAYLOAD_BYTES` | `65535` | the widest payload `payload_len` can describe |
| `ERASED_BYTE` | `0xFF` | erased media, and every byte of padding |
| `SEAL_BYTE_MASK` | `0x7F` | the bit a commit seal clears, so no seal byte is erased |
| `RUN_STARTED_PREFIX_BYTES` | `4` | `RunStarted`'s fixed head, before the run input |
| `EFFECT_SCHEDULED_BODY_BYTES` | `8` | `EffectScheduled`'s whole body |
| `VERSION_MARKER_BODY_BYTES` | `4` | `VersionMarker`'s whole body |
| `TIMER_SCHEDULED_BODY_BYTES` | `17` | `TimerScheduled`'s whole body |
| `BANK_MAGIC` | `0x4B42` | the two bytes a bank header begins with |
| `SEAL_MAGIC` | `0x5347` | the two bytes a generation seal begins with |
| `HEADER_PREFIX_BYTES` | `22` | the bank header before the run input |
| `HEADER_TRAILER_BYTES` | `4` | the bank header trailer |
| `SEAL_BYTES` | `12` | the generation seal |

## Integrity checks

Two, and both are frozen by
[ADR 0012](https://github.com/madmax983/waymaker/blob/main/docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md).
The algorithm is swappable behind a trait; the widths are positions in the frame and are
not.

| Name | Algorithm | Width | Parameters |
| --- | --- | --- | --- |
| header check | CRC-16/CCITT-FALSE | 2 B | poly `0x1021`, init `0xFFFF`, no reflection, no final xor, check value `0x29B1` |
| frame check | CRC-32/ISO-HDLC | 4 B | reflected poly `0xEDB88320`, init `0xFFFFFFFF`, reflected in and out, final xor `0xFFFFFFFF`, check value `0xCBF43926` |

## The record frame

```text
offset  size  field           notes
     0     2  magic           u16, 0x4D57, reads as "WM" on media
     2     1  format_version  u8, 1
     3     1  record_kind     u8, a number from the record table below
     4     4  effect_seq      u32, zero on a run-scoped record
     8     2  payload_len     u16, N
    10     2  header_crc      u16, the header check over offsets 0..10
    12     N  payload         the record body
  12+N     4  frame_crc       u32, the frame check over offsets 0..12+N
  16+N     -  padding         to the device's program unit, written 0xFF
     B     A  commit_seal     one program unit, A; written after the payload barrier
```

`B` is `16+N` rounded up to the program unit `A`. A record occupies `B + A` bytes.

Three fixed numbers follow: a header is 12 bytes, frame overhead is 16 bytes, and the
longest frame is `16 + 65535 = 65551` bytes before padding.

### Why two checks

`payload_len` is read out of the bytes being validated. `header_crc` covers the ten bytes
before it, so a reader knows where the frame ends before it uses a length it found on
media. A single check over the whole frame could not do that.

`frame_crc` covers the header **and** the payload. §09 names the field `payload_crc`. The
check covers more than the name says, and that buys two things: a payload cannot be
transplanted onto another header, and a record with an empty payload still gets a check that
depends on which record it is.

### The commit seal

The seal is one program unit wide and carries no length of its own. Every byte of it is one
byte of the frame's own `frame_crc` with bit 7 cleared, repeated to fill the unit:

```text
seal[i] = frame_crc.to_le_bytes()[i % 4] & 0x7F
```

No byte of a seal is ever `0xFF`. Three properties follow, and they are the whole reason for
this shape:

- an erased program unit is never a seal, so an unsealed frame is refused rather than read;
- a seal that did not land whole is never a whole one, because a missing byte still reads
  erased;
- a seal is bound to the frame it seals, so a writer that sealed what it meant to write
  rather than what landed produces a seal the reader refuses.

It works at every program unit from 1 byte to 32 KiB. A structure with a magic and a check
of its own would need ten bytes and could not exist on a byte-programmable part.

## The record table

Eleven numbers. They are frozen: a number is never reused for another meaning, and a record
kind is never renumbered.

| # | Kind | Body | Since |
| --- | --- | --- | --- |
| 1 | `RunStarted` | `workflow_kind` u16, `workflow_version` u16, then the run input | v1 |
| 2 | `EffectScheduled` | `activity_kind` u16, `input_len` u16, `input_crc` u32 — 8 B exactly | v1 |
| 3 | `EffectCompleted` | the result bytes | v1 |
| 4 | `EffectFailed` | the failure payload | v1 |
| 5 | `TimerScheduled` | `clock_kind` u8, `deadline` u64, `armed_at` u64 — 17 B exactly | v1 |
| 6 | `TimerFired` | empty | v1 |
| 7 | `RunCompleted` | the terminal result | v1 |
| 8 | `RunFailed` | the terminal error | v1 |
| 9 | `VersionMarker` | `gate` u16, `version` u16 — 4 B exactly | v1 |
| 10 | `SignalReceived` | reserved; no v1 writer produces one | — |
| 11 | `ChildStarted` | reserved; no v1 writer produces one | — |

`effect_seq` is the record's own sequence for kinds 2 to 6 and 9. It **must** be zero on
kinds 1, 7 and 8, which are run-scoped: a reader refuses a non-zero one rather than ignoring
it.

`clock_kind` is `1` for a boot clock and `2` for a persistent one. Any other value is a
malformed record, not an unknown policy. A decoder that accepted one would let the next
reader guess which clock it meant, and §11 forbids that.

### What a reader must refuse

A body is not opaque because it is a payload. A conforming reader refuses each of these as a
malformed record, and a reader that accepted one would read journals this firmware does not.

| Refusal | Why |
| --- | --- |
| `effect_seq` is not zero on kind 1, 7 or 8 | those three are run-scoped and have no sequence of their own |
| kind 1's payload is shorter than 4 bytes | the workflow kind and version are its fixed head |
| kind 2's payload is not exactly 8 bytes | the body is four fields and no more |
| kind 5's payload is not exactly 17 bytes | a clock kind and two `u64`s |
| kind 5's `clock_kind` is not 1 or 2 | an unknown byte names no policy, and guessing is what §11 forbids |
| kind 6's payload is not empty | a firing carries the fact and nothing else |
| kind 9's payload is not exactly 4 bytes | a gate and a version |
| a bank header's `program_shift` is above 15 | a program unit is a `u16`, so a wider shift describes a device that cannot exist |

Kinds 3, 4, 7 and 8 carry opaque bytes and refuse nothing beyond the frame's own checks.

Which fields kinds 2, 5, 6 and 9 carry is settled rather than incidental — see
[ADR 0011](https://github.com/madmax983/waymaker/blob/main/docs/adr/0011-a-scheduled-effect-records-a-length-and-a-digest.md) and
[ADR 0030](https://github.com/madmax983/waymaker/blob/main/docs/adr/0030-a-timer-is-a-boundary-and-its-clock-kind-is-on-media.md).

## The bank header

One per bank, at the bank's base.

```text
offset  size  field             notes
     0     2  magic             u16, 0x4B42, reads as "BK" on media
     2     1  format_version    u8, 1
     3     1  program_shift     u8, log2 of the program unit the bank was written at
     4     8  run_id            u64
    12     2  workflow_kind     u16
    14     2  workflow_version  u16
    16     2  input_schema      u16
    18     2  input_len         u16, N
    20     2  header_crc        u16, the header check over offsets 0..20
    22     N  input             the bounded run input
  22+N     4  frame_crc         u32, the frame check over offsets 0..22+N
  26+N     -  padding           to the program unit, written 0xFF
```

The journal starts at `26+N` rounded up to the program unit. `program_shift` is on media
because nothing else records it, and a reader that guessed wrong would stride the journal
wrong.

## The generation seal

One per bank, at the end of the bank, so the header may grow with the run input without the
seal moving.

```text
offset  size  field         notes
     0     2  magic         u16, 0x5347, reads as "GS" on media
     2     4  generation    u32
     6     4  header_check  u32, the frame check over offsets 0..22+N of the header
    10     2  seal_check    u16, the header check over offsets 0..10
```

Twelve bytes, rounded up to the program unit and padded `0xFF`.

`header_check` covers the header's prefix and input — offsets `0..22+N` — and **not** the
header's own `frame_crc` trailer.

The seal names its header, so a seal that survived an erase which took its header names a
digest nothing on that bank computes to. The bank is then not a candidate at any
generation, with no assumption anywhere about which direction an erase ran in.

Generations do not wrap: a writer refuses to mint a successor at `u32::MAX`, which is what
makes the plain unsigned comparison in bank selection the order of the swaps.

## Forward compatibility

Two rules, and the second is the one that is usually left implicit.

**Which versions a reader reads.** A firmware writes one format version and reads a set of
them. `waymaker_flash::frame::reads_format_version` is that set, and both decoders — the
record frame and the bank header — take their answer from it. At v1 the set is `{1}`.

A version outside the set is `DecodeError::UnsupportedFormatVersion`, refused **after** the
header check and **before** anything the version could have changed the meaning of. That
order is only sound because the twelve-byte header is frozen across format versions: a
reader meeting a version it does not know can still say how far to skip.

**What a reader does with a record kind it does not know.** Skipping is a property of the
format version, and `waymaker_flash::frame::permits_unknown_record_skip` is the rule. **No
version permits it, v1 included**, so the answer is `false` for all 256 values a version
byte can hold.

When skipping is not permitted, a reader must:

1. **stop** at the unknown frame, and not advance past it;
2. expose the records **before** it as the whole of history, which is §14's "frame ignored;
   previous history prefix wins";
3. report **no append point**, so nothing is written after a record the reader could not
   read. `waymaker_flash::recovery` reports `Ending::Damaged` and the synchronous driver
   refuses the bank.

It must **not** skip the frame, must **not** truncate history at it, and must **not**
overwrite it. Each of those loses a record a writer committed.

The empty skip list is a decision rather than an oversight. To skip a record is to assert
that the rest of history means the same thing without it. That is false for every record in
the table above. If a reader skips a `TimerFired`, replay believes the timer never fired. If
it skips an `EffectCompleted`, replay performs the effect again.

A version that granted skipping needs three things in one change: the version accepted by
`reads_format_version`, the version listed by `permits_unknown_record_skip`, and the arm in
the scan that advances past an unknown frame. One of the three announces itself — a `const`
assertion in `frame.rs` refuses a non-empty skip list, and its message names the other two.
Nothing fails a build over the other two arriving alone.

## Changing the format

Inside 1.x, without a version bump:

- **May**: add a record kind, using the next unused number, with a body of its own.
- **May not**: renumber a kind, reuse a retired number, move or resize *any* field — in the
  frame header, a record body, the bank header or the generation seal — change the meaning
  of an existing body, or change either check's algorithm or width.

Adding a kind is safe in the direction the promise runs — a later firmware reads what an
earlier one wrote — and unsafe in the other. An earlier image meeting the new kind stops,
per the rule above. That is why the promise is one-directional and why **downgrade is not
supported**; the consequences are ADR 0037's.

A change the list forbids is a new format version, and that is a migration rather than a
release. ADR 0037 is the policy.

## Where each claim is held

| Claim | Held by |
| --- | --- |
| The frame layout is these bytes | the corpus, and `tests/frame.rs`'s golden frames |
| The bank header and seal layouts are these bytes | the corpus alone; there is no golden bank frame |
| The record numbers are these numbers | the corpus, and the `wire-format` gate rule |
| The frozen constants are these constants | the `wire-format` gate rule, and `const` assertions in `frame.rs` |
| The read set reaches both decoders | `tests/frame.rs` and `tests/bank.rs`, over all 256 version bytes |
| A scan stops at an unknown kind | `tests/frame.rs::a_scan_will_not_skip_an_unknown_kind` |
| Malformed bytes cause no out-of-bounds read | `waymaker-spec`'s `bounded-decoding` guarantee, over the domain it states |
