# Architecture

Five diagrams, drawn from the design document. Each carries an HTML comment label that
`cargo xtask check-layering` reads, and each is checked against the table that owns the
same facts — so a diagram cannot quietly fall behind the code it draws.

| Diagram | Design document | Checked against |
| --- | --- | --- |
| [Crate dependency flow](#crate-dependency-flow) | §05 Architecture | `xtask::policy::LAYERS` |
| [Durable effect protocol](#durable-effect-protocol) | §07 Durable effect protocol | `xtask::docs::EFFECT_PROTOCOL_STEPS` |
| [Record frame](#record-frame) | §09 Journal and wire format | `xtask::docs::RECORD_FRAME_FIELDS` |
| [Two-bank swap](#two-bank-swap) | §10 Two-bank lifecycle | `xtask::docs::TWO_BANK_SWAP_STEPS` |
| [The banks before and after](#two-bank-swap) | §10 Two-bank lifecycle | `xtask::docs::DIAGRAMS` |

The check is a text scan, not a Mermaid implementation. It proves that every crate, every
permitted edge and every protocol step the tables name appears in the right block, that a
step listed twice is drawn twice, and that no edge contradicts the layering. It reads only
what a reader sees: an indented fence, a fence quoted inside a longer one, and a Mermaid
`%%` comment are all text rather than picture, and satisfy nothing. It does not prove the
diagram renders, which is what the preview in a pull request is for.

## Crate dependency flow

Arrows point the way `Cargo.toml` does: a crate points at what it depends on. Every solid
arrow below is one `may_depend_on` entry in `xtask::policy::LAYERS`, and the gate compares
them one for one — add a fourth layer to that table and the build fails until its edge is
drawn here.

`waymaker-core` points at nothing at all: not at another layer, and not at a registry
crate. Nothing points down into `waymaker-embassy`.

`waymaker-size-probe` is in the picture because it is a real crate CI links on every pull
request and on every push to `main`, but it is not a layer: it declares all three as
*optional* dependencies, so that a variant linking none of them gives the baseline the
code-flash budget is a delta against, and nothing depends on it. Its edges are dashed for
that reason, and the gate ignores dashed edges — the contract is the solid ones.

<!-- diagram: crate-dependency-flow -->

```mermaid
graph TD
  waymaker-embassy["waymaker-embassy<br/>async façade · Ctx · dispatcher · wakeups · clock"]
  waymaker-flash["waymaker-flash<br/>two banks · wire encoding · CRC and seals · compaction"]
  waymaker-core["waymaker-core<br/>records · replay cursor · effect identity · transition rules"]
  waymaker-size-probe["waymaker-size-probe<br/>linked to be measured, never shipped"]

  waymaker-embassy --> waymaker-core
  waymaker-embassy --> waymaker-flash
  waymaker-flash --> waymaker-core

  waymaker-size-probe -.-> waymaker-embassy
  waymaker-size-probe -.-> waymaker-flash
  waymaker-size-probe -.-> waymaker-core

  classDef layer fill:#eef4ff,stroke:#3b6fd4,color:#12233f;
  classDef tool fill:#f5f5f5,stroke:#999999,color:#333333;
  class waymaker-embassy,waymaker-flash,waymaker-core layer;
  class waymaker-size-probe tool;
```

What each layer must not own is the other half of the contract, and it lives in
[CLAUDE.md](../CLAUDE.md#the-must-not-own-table).

## Durable effect protocol

Design document §07. The first execution of an activity is an ordered protocol with two
barriers per committed record: a **payload barrier** stops a seal becoming durable ahead of
the frame it seals, and a **commit barrier** proves the seal itself is durable before the
next irreversible action.

The tag beside each step is the state the system is in *after* it — what is true if power
fails there.

<!-- diagram: durable-effect-protocol -->

```mermaid
flowchart TD
  s1["1 · Write schedule frame<br/>effect sequence, activity kind, input length and digest,<br/>policy metadata — but not its commit seal"]
  s2["2 · Payload barrier<br/>the complete frame body must precede any durable seal"]
  s3["3 · Write schedule seal, then barrier<br/>the committed schedule must now survive reset"]
  s4["4 · Dispatch physical activity<br/>the dispatcher receives a stable (RunId, EffectSeq) for deduplication;<br/>replay reconstructs the input and verifies its recorded digest"]
  s5["5 · Write outcome frame<br/>EffectCompleted or EffectFailed with bounded result bytes —<br/>but not its commit seal"]
  s6["6 · Payload barrier<br/>the complete outcome frame must precede its durable seal"]
  s7["7 · Write outcome seal, then barrier<br/>the workflow may observe the result only after this barrier returns"]

  s1 --> s2 --> s3 --> s4 --> s5 --> s6 --> s7

  n1(["not dispatchable"])
  n2(["uncommitted intent"])
  n3(["durable intent"])
  n4(["at least once"])
  n5(["not observable"])
  n6(["uncommitted result"])
  n7(["replayable result"])

  s1 -.- n1
  s2 -.- n2
  s3 -.- n3
  s4 -.- n4
  s5 -.- n5
  s6 -.- n6
  s7 -.- n7

  classDef step fill:#eef4ff,stroke:#3b6fd4,color:#12233f;
  classDef state fill:#fff8e6,stroke:#c99a2e,color:#3f3212;
  class s1,s2,s3,s4,s5,s6,s7 step;
  class n1,n2,n3,n4,n5,n6,n7 state;
```

Step 4 is the one that cannot be made atomic. Power can fail after the activity has changed
the world and before its outcome is committed, so Waymaker redelivers the same stable
effect id. **There is no exactly-once physical promise**: exactly-once behavior needs an
idempotent activity, or a downstream system that deduplicates that id.

## Record frame

Design document §09. The unit a journal is made of: handwritten, fixed-endian,
self-delimiting, and independent of Rust layout. User payload bytes are opaque to the
kernel, which is why the whole of this picture lives in `waymaker-flash` and none of it in
`waymaker-core`.

Two checksums rather than one, and the order they are checked in is the point. `header_crc`
covers the twelve bytes before it, so `payload_len` is known to be the number the writer
wrote **before** it is used to find where the frame ends — which is what §09's "all lengths
are validated against caller buffers and bank bounds before reading" asks for. A single
checksum over the whole frame could not: finding it would mean trusting the length first.

<!-- diagram: record-frame -->

```mermaid
graph LR
  subgraph header["header · 12 bytes · covered by header_crc"]
    direction LR
    f0["+0 · 2B<br/>magic<br/>u16 LE"]
    f1["+2 · 1B<br/>format_version"]
    f2["+3 · 1B<br/>record_kind"]
    f3["+4 · 4B<br/>effect_seq<br/>u32 LE"]
    f4["+8 · 2B<br/>payload_len<br/>u16 LE"]
    f5["+10 · 2B<br/>header_crc<br/>u16 LE"]
    f0 --- f1 --- f2 --- f3 --- f4 --- f5
  end
  subgraph rest["body and trailer"]
    direction LR
    f6["+12 · N bytes<br/>payload [payload_len]<br/>opaque to the kernel"]
    f7["+12+N · 4B<br/>payload_crc<br/>u32 LE · frame_crc in code"]
    f8["+16+N<br/>padding to program<br/>granularity · 0xFF"]
    f6 --- f7 --- f8
  end
  header --> rest

  c1(["header_crc covers +0..+10"])
  c2(["payload_crc covers +0..+12+N — the header as well as the payload"])
  c3(["commit_seal · a storage-program unit, not a field · rung 0.2"])

  f5 -.- c1
  f7 -.- c2
  f8 -.- c3

  classDef field fill:#eef4ff,stroke:#3b6fd4,color:#12233f;
  classDef note fill:#fff8e6,stroke:#c99a2e,color:#3f3212;
  class f0,f1,f2,f3,f4,f5,f6,f7,f8 field;
  class c1,c2,c3 note;
```

`payload_crc` is §09's name for the field and `frame_crc` is what `waymaker-flash` calls it,
because it covers the header as well as the payload: that binds a payload to its header,
and it stops a record with an empty payload having a checksum of zero, which a zeroed page
would satisfy. `commit_seal` is a storage-program unit rather than a field, written after a
barrier, and it is what makes a frame *committed* rather than merely present — which is why
it hangs off the picture rather than sitting in it. It arrives with the barrier protocol at
rung 0.2. Until then the append scan treats a frame whose checksums hold as history, so a
torn write at the tail of a journal is not distinguishable from damage there. Both stop the
scan in the same place, which is what §14 requires either way.

Everything after the frame is padding, up to the device's program granularity from §12's
`Geometry`. It is written as `0xFF`, which an erased NOR cell already holds, and it is never
interpreted: the decoder reports the frame's own length, and the scan is what applies the
stride.

## Two-bank swap

Design document §10. Two fixed storage banks, usually one erase block each. The bank with
the highest valid generation seal is authoritative; its journal is then scanned to the last
valid committed record.

`continue_as_new` is the only way bounded history is reclaimed, and the workflow explicitly
supplies the bounded input for its next run — nothing serializes a suspended future.

<!-- diagram: two-bank-swap -->

```mermaid
flowchart TD
  begin(["continue_as_new"])
  k1["1 · Stop accepting new effects<br/>for the current run"]
  k2["2 · Erase the inactive bank"]
  k3["3 · Write the new bank header<br/>new RunId, workflow version, next-run input"]
  k4["4 · Barrier: the new bank payload becomes durable"]
  k5["5 · Write the higher-generation bank seal"]
  k6["6 · Barrier: the new bank becomes authoritative"]
  k7["7 · Return success to the adapter, then lazily erase the old bank"]

  begin --> k1 --> k2 --> k3 --> k4 --> k5 --> k6 --> k7

  older(["a crash before step 5<br/>recovers the old run"])
  newer(["a crash after step 6<br/>recovers the new run"])
  k4 -.- older
  k6 -.- newer

  classDef step fill:#eef4ff,stroke:#3b6fd4,color:#12233f;
  classDef recovery fill:#fff8e6,stroke:#c99a2e,color:#3f3212;
  class k1,k2,k3,k4,k5,k6,k7 step;
  class older,newer recovery;
```

Recovery never combines the two runs' footprints. The generation seal written at step 5 is
what makes the new bank authoritative, and it is written only once the payload it seals is
already durable.

The banks themselves, before and after:

<!-- diagram: two-bank-generations -->

```mermaid
graph LR
  subgraph before["before the swap"]
    direction TB
    a1["BANK A · generation 41 · active<br/>header + RunStarted input<br/>bank generation seal<br/>committed effect records, append →"]
    b1["BANK B · inactive<br/>erased, or a previous generation<br/>no newer valid seal"]
  end
  subgraph after["after the swap"]
    direction TB
    a2["BANK A · generation 41<br/>superseded, erased lazily"]
    b2["BANK B · generation 42 · active<br/>new header + next-run input<br/>higher-generation seal"]
  end
  before --> after
```

Capacity is explicit. Waymaker reserves enough tail space for a terminal record or a
`continue_as_new`, so ordinary effect scheduling fails early with `HistoryNearCapacity`; the
runtime never overwrites committed history to make room.

## See also

- [CLAUDE.md](../CLAUDE.md) — the invariants and layering rules a contributor works to.
- [The decision record](adr/README.md) — why each of these is the way it is.
- [The design document](design/waymaker-design-v0.2.html) — the source these diagrams draw.
