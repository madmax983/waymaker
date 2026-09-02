# ADR 0006: Effect identity is newtypes, and exhaustion is terminal

- Status: accepted
- Date: 2026-09-02
- Issue: [#12](https://github.com/madmax983/waymaker/issues/12)
- Supersedes: nothing
- Related: [ADR 0003](0003-the-eight-settled-design-decisions.md), [ADR 0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md)

## Context

Design document §07 gives the kernel its identity types: "Unique on a device when combined;
only `EffectSeq` is repeated in every record because `RunId` lives in the bank header. Effect
sequences never reset within a run. Wraparound is a terminal `IdExhausted` condition, not
silent reuse."

Everything the engine does downstream of that sentence assumes it. Replay is a cursor walking
history in workflow order and matching what it finds against what the workflow asks for
next — [`replay-is-sequential`](0003-the-eight-settled-design-decisions.md#replay-is-sequential) —
so a sequence number is not a label on an effect, it is the thing that says *which* effect
this is. §07 also hands the dispatcher a stable `(RunId, EffectSeq)` pair for deduplication:
a device that reuses a sequence number tells the dispatcher that a fresh effect is a retry of
an old one, and the dispatcher believes it. §08 turns a mismatch into
`NondeterministicWorkflow` and stops — "never guess" — which is a defence that only works
when a repeat is impossible rather than merely unlikely.

A 32-bit sequence space is large. It is not infinite, and the failure at the end of it is
silent: `u32::MAX + 1` is `0` in release, the next effect claims the identity of the run's
first effect, and replay resolves it against a recorded result belonging to something else.
That is a corrupted run that reports success. So the boundary needs a representation that
cannot be arithmetic'd past, not a comment saying it will not happen.

The kernel that holds this has no dependencies —
[`kernel-is-dependency-free`](0003-the-eight-settled-design-decisions.md#kernel-is-dependency-free) —
no allocator, no `panic!`, no `unwrap`, no `unsafe`, an 8 KiB incremental code-flash budget
and 128 B of live kernel state. The design available is therefore types and const assertions,
which is most of what this decision is.

## Decision

### The identity types are the issue's literal shape, and their layout is pinned

```rust
#[repr(transparent)] pub struct RunId(pub u64);
#[repr(transparent)] pub struct EffectSeq(pub u32);
pub struct EffectId { pub run: RunId, pub seq: EffectSeq }
```

Newtypes rather than `u64` and `u32`, so that a run cannot be passed where a sequence is
expected; public fields and `#[repr(transparent)]`, so that a wire encoder in
`waymaker-flash` can reach the integer without the kernel growing an accessor for it. The
fields are public because the alternative is a `get()` on each type, and a `pub fn` in a
layer is a `pub fn` the size probe must reach (`size-probe-reach`) — a public surface has an
enforced cost here, which is a reason to keep it deliberate.

`EffectId` orders `run` before `seq` so the derived `Ord` is lexicographic in that order, and
comparing two ids from the same run compares their sequences.

**Both size *and* alignment are pinned by `const _: () = assert!(...)`**: `RunId` 8/8,
`EffectSeq` 4/4, `EffectId` 16/8. Size alone would be a weaker statement than it looks —
`size_of` is what the const assertion in `kernel_state_types!` charges the budget for, but
alignment is what decides whether that size is what a containing struct actually costs.

`EffectId` is 16 bytes, not 12: `RunId`'s alignment of 8 gives the struct a tail of four
padding bytes. That is accepted rather than fought. In-memory layout is free here because the
wire format is not this struct — §09's `RecordFrame` writes `effect_seq` as four little-endian
bytes and the run id lives once in the bank header, and `waymaker-flash` writes those fields
one at a time — so nothing on media pays for the padding.
`#[repr(packed)]` would remove it and is off the table regardless: reading a field out of a
packed struct requires `unsafe`, and every firmware crate root is `#![forbid(unsafe_code)]`.

### Sequences are minted by one allocator, and nothing else

`EffectIdAllocator { run: RunId, next: Option<EffectSeq> }` is the only thing in the kernel
that produces an `EffectId`. It is 16 B of live kernel state and it is registered in
`kernel_state_types!` — the first real entry in a registry that has been empty and honest
about it since rung 0.0 (see [ADR 0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md)),
so it is const-asserted against the 128 B budget and appears in `cargo xtask size`.

- **The first sequence of a run is 0**, and `EffectSeq::MAX` **is allocatable**. A boundary
  where the last value is reserved is a boundary that gets miscounted; the run may issue
  every value in the space and then stop.
- **Exhaustion is `next: None`.** There is no `exhausted: bool` beside a `next: u32`, because
  that pair can represent a state that must not exist — exhausted with a live next, or a
  wrapped next with the flag clear. Once `next` is `None` there is no code path back to
  `Some`, so stickiness is a property of the representation rather than a discipline the
  call sites keep. Every call after the first failure returns `Err(KernelError::IdExhausted)`
  and `peek()` stays `None` — never `Some(FIRST)`, which is what wraparound looks like from
  the outside.
- **Successor arithmetic is `checked_add`, and only `checked_add`.** Not `+` (panics in debug,
  wraps in release, and the workspace has no panic to spare), not `wrapping_add` (the exact
  bug §07 forbids), and not `saturating_add` — saturation is the worst of the three here,
  because it would reissue `MAX` for ever and hand the dispatcher a duplicate identity while
  looking, at every individual call site, entirely fine.
- **The allocator is not `Clone`, not `Copy`, and not `Default`.** A copied allocator is two
  allocators minting the same sequence twice; a defaulted one is a run silently restarting at
  0. Both are exactly the failure this ADR exists to prevent, so neither trait is derived.
  `Debug`, `PartialEq` and `Eq` are, for tests.
- **`resume(run, last_committed)` is the replay path.** Recovery reads the highest committed
  sequence out of history and continues after it: `resume(run, None)` is a fresh run, and
  `resume(run, Some(EffectSeq::MAX))` is already exhausted. It is also how a test reaches the
  boundary — the issue's acceptance criterion is a run that spends its sequence space, and
  2^32 allocations is not a test, it is a build timeout. Resuming near `MAX` tests the same
  states the long way round would reach, and it is a function replay needs regardless, not a
  hook cut for the test suite.

### Activity kinds are `u16`, and names never reach a record

`ActivityKind(pub u16)`, pinned at 2/2. Not `u8`: §09 and §13 already use `u16` for
`workflow_kind` and `workflow_version`, so `u16` is the width this record format speaks in,
and 256 activity kinds is a ceiling that cannot be raised later without changing a wire format
that is meant to be frozen. Two bytes now is cheaper than a format version bump then.

Names are compile-time metadata. `ActivityName { kind, name: &'static str }` tables live in
rodata for logs and diagnostics, and the dispatch path uses the number —
[`numeric-kinds-and-borrowed-bytes`](0003-the-eight-settled-design-decisions.md#numeric-kinds-and-borrowed-bytes),
and §13: "Activity names are compile-time metadata for logs and diagnostics. The runtime
dispatch path uses numeric kinds and does not store names in every record."

The `activity_kinds!` macro takes **one** list and produces both the constants and the table,
so a kind's number and its name cannot drift apart — the failure mode of two hand-maintained
lists is a log line that names the wrong activity, which is worse than no log line. The macro
also emits `const _: () = assert!(ActivityName::kinds_are_distinct(TABLE))`, so two kinds
sharing a number is a compile error rather than a lookup that silently returns the first
match.

### The error vocabulary is the kernel's, the decoding is not

`KernelError` is `IdExhausted`, `HistoryNearCapacity`, `NondeterministicWorkflow`,
`IncompatibleWorkflow`, and `Decode(DecodeError)`; `DecodeError` is `Truncated`,
`LengthOutOfBounds`, `UnknownRecordKind`, `UnsupportedFormatVersion`, `IntegrityFailed`.
The first four are §07's terminal sequence condition, §10's "ordinary effect scheduling fails
early with `HistoryNearCapacity`; the runtime never overwrites committed history to make room",
and §08's two replay refusals.

`DecodeError` is a vocabulary, not a decoder. §14 requires bounded decoding — "malformed
storage cannot cause out-of-bounds reads or allocation" — and the code that checks a CRC and
walks a frame lives in `waymaker-flash`, whose row in `policy::LAYERS` owns the wire encoding
and whose neighbour's must-not-own cell names CRC explicitly. Core names the outcomes so that
both layers and the façade above them speak one set of errors; core computes none of them.

`Display` writes a short static literal, obtained from a `const fn message()`, through
`Formatter::write_str`. No interpolation, no `{}` — a single `write!` with an argument pulls
`core::fmt::write` and its formatting machinery into an image with an 8 KiB incremental
budget, and it would be paid for a string nobody reads on a device with no console. The same
`message()` is what a caller uses when it wants the text without a formatter.

Neither enum is `#[non_exhaustive]` at 0.x. Every adapter that matches these lives in this
workspace, and an exhaustive match is how the compiler tells whoever adds a variant which
call sites now have a case to think about; `#[non_exhaustive]` would replace that list with a
wildcard arm that silently absorbs it.

Both hand-written `Display` impls and the `From<DecodeError> for KernelError` impl are trait
methods in a layer crate, which `size-probe-reach` counts (see
[ADR 0002](0002-size-budgets-are-measured-as-deltas-against-a-probe-firmware.md)): the probe
therefore takes a function pointer to `<KernelError as Display>::fmt` and pushes it through
`black_box`, which retains the impl body for measurement without linking the formatting
machinery the impl was written to avoid.

## Consequences

- **16 B of the 128 B kernel-state budget is spent**, on the first type to be registered. It
  is an eighth of the kernel's live state for a run id and a sequence, and the cursor and
  transition state that rung 0.1 still owes have to fit in what is left. That is the number
  being visible working as intended, but it is a real narrowing of the room remaining.
- **A run that needs more than 2^32 effects must `continue_as_new`.** There is no other way
  out: the sequence space does not reset within a run, so exhaustion is terminal by
  construction. `continue_as_new` is also the only point at which history is reclaimed
  ([`no-snapshotted-futures`](0003-the-eight-settled-design-decisions.md#no-snapshotted-futures)),
  so the two boundaries coincide, which is the design working rather than a coincidence to
  rely on quietly.
- **A later rung that folds the allocator into the cursor must *replace* the registry entry,
  not add beside it.** `kernel_state_types!` sums independently live types; registering both
  a cursor that contains an allocator and the allocator itself double-counts 16 B against a
  128 B budget, and the failure direction is a build that fails for state that does not exist
  — recoverable, but confusing enough to be worth writing down here.
- **Adding a variant to either error enum is a breaking change for anything outside this
  workspace** that matches it exhaustively. That is accepted deliberately: at 0.x there is
  nothing outside this workspace, and the in-workspace exhaustiveness is worth more than the
  forward compatibility. When there is an adapter with its own release cycle, this decision
  gets revisited by a new ADR.
- **The public surface grew, and every item of it has to be reachable from the size probe.**
  `successor`, `for_run`, `resume`, `run`, `peek`, `allocate`, `lookup`,
  `kinds_are_distinct`, `message`, `fmt` and `from` all appear in the probe's `engine()`. The
  probe is now a small program rather than a stub, which is what makes the 8 KiB delta a
  measurement of the kernel — and it is also a second place to update whenever the kernel
  gains a function.
- **`Display` is deliberately worse than it could be.** The messages are short static
  literals with no context in them: no sequence number in `IdExhausted`, no offset in
  `Truncated`. A caller that wants those has the values in hand at the point it built the
  error. This is the 8 KiB budget being charged to diagnostics, and it will be noticed by
  whoever debugs a decode failure from a log line.

## Alternatives considered

- **`EffectSeq(NonZeroU32)`.** Genuinely attractive: the niche makes `Option<EffectSeq>` free,
  so the allocator's exhaustion representation would cost nothing at all, and `0` becomes
  available as a "no effect" sentinel on the wire. Rejected for now on two grounds — it
  deviates from the shape the issue and §07 state literally (`pub u32`), and choosing a wire
  sentinel is `waymaker-flash`'s decision to make, not the kernel's. The `Option<EffectSeq>`
  in the allocator costs 4 bytes of padding today, and buying a niche with a design constraint
  on a layer above is the wrong trade at rung 0.1.
- **`{ next: u32, exhausted: bool }`.** Rejected: it can represent a state that must not
  exist. Two fields that must agree are two fields that eventually will not.
- **A `next()` or `successor_mut()` method on `EffectSeq` that mints the following id.**
  Rejected: it makes every holder of a sequence a source of identity. The point of routing
  minting through one non-`Copy` allocator is that there is exactly one place that can issue,
  and `successor()` — which is pure, returns `Option`, and mints nothing — is what the
  allocator itself is built from.
- **`ActivityKind(u8)`.** Rejected: 256 kinds is a limit that can only be raised by changing a
  frozen wire format, and the record widths around it are already `u16`.
- **String activity kinds, or names stored per record.** Rejected by
  [`numeric-kinds-and-borrowed-bytes`](0003-the-eight-settled-design-decisions.md#numeric-kinds-and-borrowed-bytes)
  and §13 directly: a name in every record is flash spent per effect on something only a
  developer reads, in a journal whose capacity is the resource the whole design is careful
  with.
- **`#[non_exhaustive]` on the error enums.** Rejected at 0.x for the reason above; revisited
  when an out-of-workspace adapter exists.
- **`#[repr(packed)]` on `EffectId` to reclaim the four padding bytes.** Rejected: reading a
  field of a packed struct needs `unsafe`, which every firmware crate root forbids, and the
  bytes are not on the wire in the first place.
