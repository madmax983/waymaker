# The durable effect protocol

One effect is seven steps. Design document §07 states them; this chapter is the reference.

## The seven steps

The identity comes first and is not one of them. A `(RunId, EffectSeq)` is minted once, by
the kernel, when the effect is new work; on every later delivery of that same effect the
kernel hands the identity back unchanged. The driver has no allocator of its own, so a retry
or a reboot cannot mint a second one.

| Step | What happens | Why it is where it is |
| --- | --- | --- |
| 1 | Program the schedule record's frame | The intent, not yet committed |
| 2 | Barrier | The frame is durable before anything seals it |
| 3 | Program the schedule record's commit seal, then barrier | After this the intent is durable |
| 4 | Perform the activity | The world changes here, and never before step 3 returned |
| 5 | Program the outcome record's frame | What the world answered |
| 6 | Barrier | The payload is durable before anything seals it |
| 7 | Program the outcome record's commit seal, then barrier | After this the outcome is durable and the workflow may observe it |

Step 4 is unreachable without step 3. That is a type, not a review note: `Effect::schedule`
takes steps 1 to 3 and returns a `Dispatchable`, `Dispatchable::intent` is the only accessor
for a `DurableIntent`, and `Activities::perform` accepts no other proof. `Dispatchable::resolve`
takes steps 5 to 7 and returns the only `Outcome` a caller can reach, so a workflow observes
a result after step 7's barrier and at no earlier point.

The one exception is a redelivery: `Effect::redelivering` mints the proof from a sequence
number, on the kernel's word that a schedule record for it is already on media. It is
crate-private, so the trust reaches one caller.

## What the media shows

```rust,ignore
{{#include ../../../crates/waymaker-drive/tests/book.rs:a_journal_records_the_intent_before_the_outcome}}
```

## The commit seal

A record is a frame plus a commit seal one program unit wide. The seal is the frame's own
check with bit 7 of each byte cleared, repeated to fill the unit. Two things follow:

- No byte of a seal is `0xFF`, so an erased program unit is never a seal.
- A seal that did not land whole is never a whole seal.

"Sealed but incomplete" is therefore a state the media cannot hold. A reader does not have
to detect it.

## Identity, and what it is for

Every attempt at one effect carries the identity the schedule record committed. A retry and
a reboot both redeliver that pair. It is the only value a downstream system can deduplicate
on. See [What is not promised](not-promised.md).

## Capacity

A run declares what its records may be worth before it starts. Waymaker prices the run's two
exits — a terminal record, and the header a `continue_as_new` writes into the other bank —
and holds that space back. Scheduling fails early, before any media is touched, rather than
late with an effect outstanding.

Waymaker never overwrites committed history to make room.

`continue_as_new` is `waymaker-flash`'s bank swap. Neither driver in this repository calls it
yet — both answer `ContinueUnsupported` — so a run that reaches capacity, or a bank with no
append point, stops there. Issue [#110](https://github.com/madmax983/waymaker/issues/110) is
where the two are joined.
