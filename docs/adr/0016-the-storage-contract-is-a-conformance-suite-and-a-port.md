# ADR 0016: The storage contract is a conformance suite and a port, both above the layers

- Status: accepted
- Date: 2026-09-03

## Context

Design document §12 states a storage contract in five sentences, and issue
[#21](https://github.com/madmax983/waymaker/issues/21) asks for them to be "documented and
tested". Two of those sentences were already real code: `waymaker-flash`'s `Geometry` is the
only legal description of a device and the only thing that decides whether an offset and a
length are allowed, and `StableStorage` is the four operations and one barrier every port
implements, with its public surface pinned by the `storage-contract` rule so that §05's "must
not expand the firmware traits to accommodate host conveniences" is a build failure. Issue
#18 built that much.

What #21 also asks for is two things that did not exist:

- **a conformance test suite that any adapter can be run against**, and
- **an `embedded-storage` implementation adapted without `embedded-storage` becoming a kernel
  dependency**.

Neither can live in `waymaker-flash`. A layer's public functions must all be reached by the
size probe, so a suite inside it would be charged against an 8 KiB code-flash budget with
twelve bytes of headroom; and every layer's `may_depend_on_external` list in
`xtask::policy::LAYERS` is empty, which is the mechanism that makes the second bullet's
"without becoming a kernel dependency" a fact rather than an intention.

The harder question is what a conformance suite can honestly claim. Three of §12's five
sentences are not observable from inside one process:

- "`program` and `erase` may fail or be interrupted at any supported unit" is satisfied
  vacuously by a driver that never fails, so no suite can ask for it;
- "after `barrier` returns, all earlier successful mutations survive reset" and "no later
  mutation may become durable before mutations ordered by a completed barrier" are statements
  about what is on media *after the power came back*.

A suite that reported "all five clauses covered" would be reporting on two of them.

## Decision

**`waymaker-conformance` is a fourth crate above the layers**, alongside `waymaker-fault` and
`waymaker-spec` in `policy::TEST_SUPPORT_CRATES`, outside `default-members`. It depends on
`waymaker-flash` for the contract and on `embedded-storage` for the port, and nothing depends
on it.

**It is `#![no_std]` and allocation-free**, unlike the other two. `waymaker-fault` models
media and `waymaker-spec` enumerates a state space; both belong on a host. A conformance
suite is something a driver author runs *against the part*, over a debug probe, on a device
with no allocator. The one buffer the suite needs is the caller's, for the reason
[ADR 0008](0008-the-replay-cursor-is-pumped-by-its-caller.md) gives for the replay cursor: a
suite with an internal `[u8; N]` either refuses a 256-byte-page SPI part or charges every
4-byte-page internal flash for one.

**The contract is a table of six clauses, and every row says what discharges it.** The five
are §12's; the sixth, `operations-act-on-what-they-name`, is `StableStorage`'s own
documentation, and without it the suite would be a suite of refusals that never checked that
a legal operation works. Each row carries one of four discharges, and three of the four are
ways of saying "not the in-process suite":

| Clause | Discharged by |
| --- | --- |
| `validated-before-media` | the in-process suite |
| `operations-act-on-what-they-name` | the in-process suite |
| `barrier-is-durable` | the across-reset witness |
| `barrier-orders-what-follows` | the across-reset witness |
| `interruptible-mutations` | a crash injector, not a suite — `waymaker-fault` |
| `one-way-bits-are-the-drivers` | the driver, not the protocol |

**The two barrier clauses are a two-phase witness rather than a claim.** `durability::arm`
writes into three erase blocks — an acknowledged witness, then a *seal*, each crossing a
barrier, then an unacknowledged witness with no barrier after it — and the caller cuts the
power wherever it likes. `durability::verify` then asks two questions, and they are the two
clauses: a seal on media without its witness is a barrier that did not mean what it said, and
an unacknowledged witness on media without the seal is a write that overtook a completed
barrier. Anything else, a device that lost everything included, is not a breach. The erases
run backwards — unacknowledged block first — so that a stale witness from a previous run can
never be read beside a freshly erased seal.

**A skipped case is a fact about the geometry, never a silence.** An outcome starts at
`NotRun` and `Report::verdict` refuses a report that still holds one, so a case added to the
table and forgotten in the runner fails a run rather than shrinking it. The only other way a
case does not pass is `NotApplicable`, and every reason is a property of the device: a
one-byte unit, an erase block that is a single program unit, an erased state with no bits to
clear.

**The port validates.** `embedded-storage`'s `check_read`, `check_write` and `check_erase`
are helpers a driver *may* call; §12 puts the obligation on the adapter, so
`NorFlashStorage` validates against its own `Geometry` before the driver is reached. It also
derives that geometry from the driver's own constants and refuses a part that cannot be
described — units that do not nest, are not powers of two, or do not fit in 32 bits — at
construction rather than at the first operation that would have gone somewhere unexpected.
Its `barrier` is a no-op, and that is a claim about `NorFlash` rather than an omission: its
`write` and `erase` are blocking and complete when they return, so a driver with a cache in
front of it needs a port of its own.

**The `storage-conformance` rule holds the four places the contract lives together**:
`xtask::docs::STORAGE_CONTRACT_CLAUSES`, `CLAUDE.md`, this ADR, and the crate's own table in
`crates/waymaker-conformance/src/clause.rs`. It compares ids *and* discharges in both
directions, because two tables that agree on the names of six things and disagree about what
any of them costs is the failure worth catching.

## Consequences

Issue #21's two "done when" bullets are one test each, and both are run by the ordinary
workspace test stage rather than by a feature nobody enables. `embedded-storage` is a
non-optional dependency for exactly that reason: a feature CI never turns on is a measurement
that did not happen.

The suite has been observed failing. Thirteen adapters wrong in one way each — a validator
that validates after the write, an erase that takes the whole chip, a barrier that is a
no-op with a scribble in it, a read that always returns the erased byte — are each required
to be caught by the case that names them, and a control adapter with no flaw is required to
pass. `waymaker_fault::Device`, written for issue #18 and knowing nothing about this crate,
is a second adapter the whole suite runs against.

`embedded-storage` is the first third-party dependency in this workspace outside `xtask`. It
is a `#![no_std]`, zero-dependency crate, and it reaches no layer: `waymaker-core` growing it
fails `kernel-zero-dependencies` and `waymaker-flash` growing it fails
`dependency-direction`, because every layer's `may_depend_on_external` list is empty.

What this leaves owed:

- **The across-reset witness proves nothing until somebody resets a device.** On the host it
  is driven through `waymaker-fault`'s injector at every crash point the write sequence has,
  which is a strong test of the *oracle* and no test at all of a real barrier. §15's hardware
  half — power-cut loops against real NOR — is still owed at rung 0.2, where the boards are.
- **`verify` judges one arm.** A region armed twice with no `verify` between asks a stateless
  reader to tell two histories apart, and it cannot. The backwards erase order removes the
  realistic case; the general one is a precondition, written down rather than checked.
- **A device that reads a constant zero exempts three cases rather than failing them.** The
  erased state would have no programmable bits, which is a legitimate thing for media to be
  and indistinguishable from a broken read. It is in the teeth table as an exemption rather
  than a catch, so the hole is a test rather than a footnote.
- **A one-byte program unit exempts `RefusedProgramTouchesNoMedia`.** With that geometry every
  in-bounds program is legal, so no illegal operation names bytes inside the region; issuing
  one that named bytes outside it would ask a broken driver to damage the media the caller
  said not to touch.
- **`Report` does not carry the driver's own error.** It is `S::Error` on a generic
  parameter, and a report that named it could not be a plain array of `Copy` values on a
  target with no allocator. The case id names the operation.
