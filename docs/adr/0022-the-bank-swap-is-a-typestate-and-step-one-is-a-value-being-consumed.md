# 0022. The bank swap is a typestate, and step one is a value being consumed

- Status: Accepted
- Date: 2026-09-06

## Context

Design document §10's two-bank lifecycle is the last thing rung 0.2 owed. Issue
[#26](https://github.com/madmax983/waymaker/issues/26) states it as seven steps — stop
accepting new effects, erase the inactive bank, write the new bank header with the new
`RunId` and the next run's input, barrier, write the higher-generation seal, barrier, return
success and lazily erase the old bank — and then states two recovery rules and one
prohibition:

> A crash before step 5 recovers the **old** run. A crash after step 6 recovers the **new**
> run. Recovery never combines their footprints.

Every word of that is a statement about *where the barriers are*. §02 decision 6 is why the
protocol looks like this at all: a normal async workflow cannot serialise its hidden
suspension state, so Waymaker does not disguise storage maintenance as a snapshot — the
workflow supplies the bounded input for its next run, and the swap installs it.

The pieces were already here. `waymaker-flash`'s `bank` module has been §10's layout, header,
generation seal and selection rule since issue #22
([ADR 0017](0017-the-two-bank-layout-is-geometry-derived-and-the-seal-names-its-header.md));
`recovery` and `append` are the reader and writer of a bank's journal (ADRs
[0018](0018-recovery-is-a-position-and-only-erased-media-is-an-append-point.md) and
[0019](0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md)); and
`capacity` has priced the roll-over since issue #25
([ADR 0020](0020-the-capacity-reserve-is-an-outcome-and-a-terminal-record.md)). What was
missing was the thing that performs the seven steps, and `crates/waymaker-flash/src/lib.rs`
said so in one sentence: "what is still owed at 0.2 is the bank swap and `continue_as_new`".

The temptation was a `continue_as_new()` function that did all seven steps in one body. It
would be smaller, and it would make §10's ordering a convention the next patch is free to
re-order — with nothing visible until a power loss on somebody's device.

## Decision

§10's swap is `waymaker-flash`'s `swap` module: five types, each with one thing to do, and
the ordering enforced by which type a step returns.

**Step 1 is a value being consumed.** `Swap::beginning` takes a `Retired<C>` — the `Journal`
the run was appending with, or a `Recovery` of the bank it was reading — by value. Holding a
`Swap` means the retiring run has no writer left, so "stop accepting new effects for the
current run" is a fact about what exists rather than a discipline spread over call sites.
Both shapes are accepted because a bank whose scan ended `Damaged` or `Unsealed` has no
writer at all, and that is precisely the bank §10 says to recycle: requiring a `Journal`
would have made the swap unavailable in the case it is most needed.

**Everything decidable is decided before the erase.** `Swap::beginning` refuses a device with
no single authority, a generation at the ceiling, a next run that repeats the retired run's
id, a retired reader that is not over the bank the device booted, a reader from another
device, and a next-run input that would leave the installed bank no usable journal. The last
matters most in order: a swap that erased the spare bank and *then* discovered the header
would not fit has destroyed the only other copy of anything the device holds.

**The rest is `prepare` → `stage` → `payload_barrier` → `commit` → `reclaim`**, each
consuming the previous state. `Prepared` has no `commit`, `Staged` has no way to program, and
`Sealable` — the only value that can program a generation seal — is reachable only from
`Staged::payload_barrier`. A `compile_fail,E0599` doctest beside a compiling twin proves that
of the code as it stands, and the `swap-discipline` rule is what stops it being given back:
it pins the module's public surface, the one method each state may declare, the one body each
of `Sealable` and `Installed` may be constructed in, the spelling every barrier is taken
with, and — the sharpest of the five — the two erases, each held to the bank its row names,
before a barrier, without naming the other one.

**A seal is never programmed over a header that did not land** — and the reason is the `?` on
the program call, not where the digest came from. `waymaker-fault`'s
`swap_that_seals_whatever_landed` has two bugs, and this is worth being exact about because
an earlier draft of this ADR credited the wrong one: it seals the header it *intended* to
write, and it carries on past a failed program. `Prepared::stage` computes its digest from
the buffer the header was encoded into, before the program, so sealing the `BankHeader`
argument directly would be byte-for-byte the same. What the digest buys is that a seal names
one header, so a frame torn part-way through its program is not a candidate at any
generation.

**Which bank is erased is never a parameter.** The bank to install into is
`authority.bank.other()` and the bank to reclaim is the one the device booted, both fixed at
`beginning`. There is no argument to get wrong, which is why `reclaim` is crash-safe by
construction rather than by care: the new bank already carries a strictly higher generation,
so an interrupted erase of the old one can only remove a candidate, never promote one.

**Effect identity restarts, and stays distinguishable.** `Installed::allocator` is
`EffectIdAllocator::for_run` with the run id the swap installed. §07 identifies an effect by
the pair `(RunId, EffectSeq)` and the sequence restarts at zero, so the pair is the whole of
what keeps the two runs apart, and `SwapError::RunReused` is what refuses the one input under
which it cannot. That refusal is only as good as the `run` it is given, which is a caller's
argument this module cannot check — see Consequences.

Three things are held by three different mechanisms, and it is worth being explicit about
which:

| Claim | Held by |
| --- | --- |
| the seven steps are in §10's order, and no step is reachable without the one before it | the typestate, the `compile_fail` doctest, and `swap-discipline` |
| the crash windows behave — old run before step 5, new run after step 6, never combined | `crates/waymaker-fault/tests/swap.rs`, at every crash point of every step, with three mutant swaps as teeth |
| the barriers are real barriers | §12's contract and `waymaker-conformance`'s across-reset witness. Not this module's, and not a scanner's |

## Consequences

**Eight public types and eleven public functions for what a caller experiences as one
operation** — five states, the `Retired` enum, and two errors. Two of the five states cannot
be reached from the crate root: `waymaker_flash::Staged` and `Sealable` are already
`append`'s, so the swap's are `swap::Staged` and `swap::Sealable` and only the other three
are re-exported. That is a wart, and the alternative — renaming one pair — would make the two
protocols read as different things when they are deliberately the same shape.
That is the price of the ordering being a compile error, and it is the same price
[ADR 0019](0019-the-commit-seal-is-a-masked-repeat-and-the-writer-is-a-typestate.md) paid for
§07's two barriers. A caller writes a five-call chain; `and_then` makes it one expression.

**Two arguments this module cannot check, and what each costs.** `Swap::beginning` takes
`booted` and `run`, and reads no media, so neither is verified. A wrong `run` disables
`SwapError::RunReused` and leaves two runs whose effect identities collide for ever. A
*stale* `booted` — one naming a bank that has since lost a swap — is worse: `prepare` erases
the bank that is really authoritative, every check passes, and the live run is gone. The
refusal that would close the second is a read of the spare bank's seal, and it cannot be made
fail-closed here: the header it would have to decode is as long as the previous run's input,
bounded by the bank rather than by the caller's page, so a small page would turn it into a
guard that silently allows what it exists to refuse. Both are stated as preconditions and
recorded in CLAUDE.md's "What is not checked", and closing them by construction is the
dispatcher's — rung 0.4's — because a dispatcher that selects and swaps in one place cannot
hold a `booted` older than the swap it is planning. This is the alternative that was
considered and not taken: `beginning` could take the retiring bank's decoded `BankHeader`
instead of a bare `RunId`, which would make `run` *read* rather than asserted — it closes
half of it, and it does not close the half that matters.

**No budget raise, but only after the writer was made to stop copying itself.** The first
measurement was 18474 B against the 18432 B gate — 42 B over. ADR 0020 asked that issue #26
be "measured against a corrected figure rather than against a third raise", and a third raise
argued from 42 B would have been the worst version of the thing that ADR objected to. Two
changes closed it and both were real defects rather than gaming: the plan carried a
`Geometry` that the `JournalRegion` beside it already held, and each step took the whole
eighty-odd-byte plan *by value* to compare one field of it. Passing it by reference and
dropping the duplicate field, together with trimming the probe row's own arithmetic — a
three-armed `match` over `Authority` and four `black_box` calls that charged the engine for
this file — took the figure to **18098 B**, 334 B under the gate. That last part is issue
[#72](https://github.com/madmax983/waymaker/issues/72) in miniature, and is why that issue
should be fixed before the next raise is argued.

**`waymaker-spec` still cannot describe this.** The ghost model's banks hold no records and
no transition changes a bank and a record at once, so §14's "never recover the old run as
current" remains a statement about a model rather than about this code.
`single-authority`'s row in `obligation.rs` already says so; this change makes the gap
sharper, because there is now a real swap to abstract. It stays owed.

**The reserve is still not obliged on anybody.** `Reserve::rollover_bytes` prices the header
this module writes, and nothing makes a caller consult it before calling `Swap::beginning` —
`beginning` refuses an input that does not fit, which is the same answer arrived at one step
later. The dispatcher that would join the two is rung 0.4's.

**`Installed` hands back a `JournalRegion` and not a `Journal`.** The installed bank's journal
is known erased — this module erased it — so a writer at offset zero would be sound. It is
not offered: `Journal::after` taking a finished `Recovery` and nothing else is what makes ADR
0018's anti-bricking rule structural, and a second constructor for the writer, even a
provably correct one, is a second way to reach an append offset no scan vouched for. The cost
is a scan of an erased journal on the swap path, which is ADR 0018's erased-tail walk again
and is filed there rather than worked around here.

**The roll-over gate is the header plus the run's first record, and nothing further.**
`JournalRegion::of` refuses only a journal of zero bytes, and `BankRegion::max_run_input_bytes`
— whose own postcondition claimed to be the "can be used with" bound, and was not — reserves
one *empty* record. §09's `RunStarted` repeats the whole run input, so an input at that
ceiling installed a bank with a 24-byte journal and a 4064-byte mandatory first record,
durably, with the swap reporting success. Codex found it on the first review round.
`SwapError::InputTooLong` now prices the header and that record together. It deliberately
stops there: an effect scheduled, its outcome and a terminal record are `Reserve::for_layout`'s
floor, which is a policy about what a run may do rather than a fact about whether the run the
swap installs can start.

**The ordinary path is `Retired::Reserved`, and it was missing.** §10's roll-over is what a
run does when `Refusal::NearCapacity` refuses its next record, and the writer it holds at that
moment is a `Reserved` — the type that enforces the reserve. `Reserved` deliberately has no
way to hand its inner writer back, because an ungated writer escaping is what
`capacity-reserve` exists to make expensive, so the first version of `Retired` left the one
flow the reserve was written for reachable only by dropping the writer and scanning the bank
again. Every test in the swap suite sidestepped it by building a raw `Journal`, which is how
it went unnoticed; Codex found it on the third review round. A variant fixes it and costs no
public function: the `Reserved` goes in and the swap is what comes out, so nothing ungated
escapes.

**Two limits the second review round drew, both stated rather than closed.** A "device" here
is a `Geometry`, so two parts of the same model are one device and a caller with two chips
can prepare on one and commit on the other — that is the contract `append`, `recovery` and
`capacity` all keep, and closing it in this module alone would leave four identical names
meaning two different things, so it is issue
[#84](https://github.com/madmax983/waymaker/issues/84) rather than a line here. And
`SwapError::RunReused` compares the next run against the retiring one, which catches the
adjacent mistake and is not a uniqueness check: a run id from any earlier run passes it and
collides just as durably. Nothing on the device remembers the ids it has retired, so global
freshness is the caller's, and the documentation now says so where it used to imply
otherwise.

## Alternatives considered

**One `continue_as_new()` function.** Smaller, and the reason it was rejected is in the
Context: §10's recovery rules are statements about where the barriers are, and a single body
makes that a convention.

**A caller-pumped state machine** — `swap.step(storage)` returning a `Progress`. It has the
same public-surface cost, no ordering guarantee, and it puts the protocol's state in a value
a caller can hold across a decision. The typestate is the same idea with the compiler
checking it.

**Extending the `bank` module.** Rejected on that module's own terms: it says it must not own
media, and that is what keeps §10's selection rule a pure function a host can enumerate.

**Two constructors instead of the `Retired` enum** — `Swap::after_journal` and
`Swap::after_recovery`. Every public function of a layer must be reached by the size probe
and is a line in the `swap-discipline` pin, so a variant is cheaper than a constructor and
says the same thing.

**Reading the header back before sealing it.** `waymaker-fault`'s
`swap_that_seals_whatever_landed` shows what a writer that seals its *intention* can leave on
media, and a read-back is one answer to it. Carrying the digest computed from the bytes the
`program` call accepted is the other, and it is exact for the same reason and costs no read
and no second page.

**Refusing to swap while the retiring bank's journal is still appendable.** Tempting — it
would make "the run really has stopped" a fact about media rather than about a value — and
wrong: the ordinary reason to roll over is `Refusal::NearCapacity`, which is a journal that
is still perfectly appendable. Consuming the writer is what "stopped" can honestly mean here.
