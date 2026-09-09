# ADR 0034: a codec is a bridge behind a feature, and the probe mirrors it

- Status: accepted
- Date: 2026-09-09
- Issue: [#37](https://github.com/madmax983/waymaker/issues/37)
- Supersedes: nothing
- Related: [0029](0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md),
  [0032](0032-the-facade-is-four-futures-over-a-durable-half-it-does-not-own.md),
  [0033](0033-the-dispatcher-answers-in-a-bound-the-journal-states.md)

## Context

Design document §02 decision 4 says records are numeric kinds and borrowed bytes, and that
Serde and Postcard are "optional conveniences, never wire-format requirements". §04 asks
that "adding Serde, Postcard, `defmt`, Embassy, or a CRC implementation must show its own
incremental cost".

Issue [#35](https://github.com/madmax983/waymaker/issues/35) left the `Decode` trait: one
method over borrowed bytes, with an implementation for `()` and nothing else. Every
workflow so far writes its own byte parsing — `waymaker-drive`'s OTA example reads an
eight-byte handle with `try_into`. Issue #37 asks for the optional half, and for the cost of
enabling it to be a number.

Three things had to be decided.

**What `serde` alone can be.** Serde is a data model, not a format. A feature that enabled
it and added no code would be a decorative row of the size report — and a row of zero is
what a broken gate also prints.

**Whether the probe can reach a feature it cannot name.** The size matrix is derived: a
feature a layer declares becomes a row, selected as `--features waymaker-embassy/postcard`.
That enables the layer's feature and defines no `cfg` in `waymaker-size-probe`, so the probe
cannot write a call the row turns on. The row links the codec, reaches none of it, and
reports the delta of an image nobody exercised. Nothing noticed: the row is not identical to
its base, so the existing "unexercised measurement" notice stays quiet.

**What stops a codec becoming a requirement.** Not the manifest. A `Ctx::activity` bounded
on `DeserializeOwned`, or a `Handoff` naming a codec type, makes every workflow carry the
codec whatever the features say — and breaks no layering rule, needs no new dependency, and
passes every test, because the run still completes.

## Decision

**The `serde` feature is a bridge that names no format.** `decode::Format` is one method:
read `bytes` as a `T`, where `T: DeserializeOwned`. `decode::Coded<F, T>` is the `Decode` a
workflow names, and its only inherent method hands the value back. A firmware with its own
codec implements `Format` and gets `Decode` for every type that codec reads, without
enabling postcard and without this crate naming a format. That is what makes the feature
earn its row rather than decorate one.

`DeserializeOwned` rather than `Deserialize<'de>`: `bytes` is the caller's buffer and the
next boundary overwrites it, so a value that borrowed it would be a dangling read one
`.await` later. Widening `Decode` with a lifetime would offer that borrow to every
implementor.

**The `postcard` feature is one `Format`.** `decode::Postcard` and the alias
`decode::FromPostcard<T>`. It enables `serde` rather than `dep:serde` on its own, because a
format with no bridge is a format for nothing.

**Neither is re-exported at the crate root.** `waymaker_embassy::decode::FromPostcard` and
no shorter path.

**Postcard reads one complete value, not a prefix.** `postcard::from_bytes` stops at the end
of a value and never asks what follows, so a `(u8, u16)` decoder reads a record written for a
wider type and answers a plausible wrong value. `Format::read` uses `take_from_bytes` and
refuses a remainder. That is the case a firmware meets after narrowing a result type: the old
record still starts with a legal value, and replay would hand the workflow one on every boot
with no checksum failing.

**`Coded` has no `Debug`.** It would forward to `T`'s, which pulls `core::fmt` into a
firmware that only wanted to decode — measured, the `postcard` row goes from 208 B to
3028 B. `Clone` and `Copy` are there, bounded on `T` alone, because a derive would ask them
of a format that is only a type-level tag. A caller that wants to print calls `into_inner`. A codec type on the crate root is a codec every workflow reads about, and
the rule below is what keeps it there — it found this during implementation, on a crate root
that had re-exported all four.

**The probe mirrors a layer feature under a feature of its own.** `mirror_feature` derives
the name from both — `waymaker-embassy` plus `postcard` is `embassy-postcard` — so a feature
added to a layer has one place to be mirrored and no table to remember, and two layers
declaring one feature name mirror it under two probe features. `check_probe_mirrors` then
makes the mirror **compulsory** in three ways: a layer feature the probe does not mirror, a
mirror that does not enable the layer feature it names, and a mirror the probe declares and
never `#[cfg]`s on, each fail the `size-probe` rule. The third is review's: declaring a
mirror is not reaching it, and a feature that adds no public function adds no
`size-probe-reach` obligation either, so a mirror with no `#[cfg]` behind it produced exactly
the row of zero this exists to prevent.

`matrix` selects the mirror and never falls back to `<layer>/<feature>`. `xtask size` runs
*before* `check-layering` in the stage table, so a fallback would let the size stage print
`ok` over an unmeasured row and leave the last stage to notice. Selecting a feature the probe
does not declare fails the link instead, which is the direction a measurement should fail in.

The bridge's own row is driven over a format the probe supplies — one byte through serde's
own value deserializer — because a format that always refused would let the optimiser fold
the success path away, and a folded arm is not a measurement. It is the same mistake ADR
0031 records the clock row making with a `Result` discriminant.

**`codec-is-optional` is what keeps the codec out of the boundary.** No `waymaker-embassy`
module but `decode.rs` may name a codec; every codec item in `decode.rs` is behind a
`#[cfg(feature = ..)]` and `pub trait Decode` is behind none; both dependencies are
`optional`; and each feature enables what its row names.

Three of its halves are things review demonstrated rather than things anybody predicted, and
each was watched passing on a mutation before it was closed. A `pub struct Bridge {` whose
declaration line named no codec and whose field below it did. A `#[cfg(all(feature =
"serde"))]` on the *trait*, which a compound `cfg` is not read as gating — so an item behind
one is reported while the trait behind one was silent, and `Decode` left the default build
with the gate green; the two halves now fail in the same direction. And `pub trait Decoder`,
which satisfied the "declares no `pub trait Decode`" branch as a prefix while the pinned
trait was gone.

The module half reads *items* rather than lines. Review of this change wrote a
`pub struct Bridge {` whose declaration line named no codec and whose field below it did,
and watched a line-based version stay green — the compiler refuses that in a default build,
because what it names is behind the same feature, but a rule that leans on the compiler for
its own claim stops holding the day the codec becomes reachable another way.

**A proc macro is not a dependency this layering is about.**
`dependency-direction-transitive` reads a resolved graph, and `postcard` reaches `cobs`
reaches `thiserror` reaches `thiserror-impl` reaches `syn`, `quote` and `proc-macro2`. None
of those last four is in a firmware image: a proc macro is compiled for the build host and
runs there. `illegal_reach_paths` now stops at a package whose library target is a proc
macro and does not walk through it, so the façade's allowlist is the five crates it really
reaches — `serde`, `serde_core`, `postcard`, `cobs`, `thiserror` — rather than a list of
build tooling. A *direct* edge to a proc macro is still `dependency-direction`'s, which
reads the manifest.

## Consequences

**Enabling postcard costs 208 B of code flash, and enabling the bridge alone costs 32 B.**
Measured by `cargo xtask size` on `thumbv6m-none-eabi` with the release-size profile, both
against the `facade` row. The two are *not* additive: `postcard` enables `serde`, so the
208 B already contains the bridge's 32 B and the format above it is 176 B. The gated row — §04's "core + flash adapter" — reads 12220 B of
12288, two below where [ADR 0033](0033-the-dispatcher-answers-in-a-bound-the-journal-states.md)
left it. That is not a codec making the engine smaller: it is a larger probe making the
optimiser choose slightly differently — the `serde` row shows the same effect as −4 B in the
`layers` column. The codec is above the gated row and costs it nothing.

Where those bytes land is worth reading carefully, because the split does not fall where a
reader would expect. Of postcard's 208 B, the symbol table attributes **198 B to the probe
and 10 B to the layers**. That is not the gate failing; it is
[ADR 0029](0029-the-code-flash-gate-charges-the-layers-and-the-probe-pays-for-itself.md)'s
stated limit met for the first time by code that is *entirely* generic: `Coded::decode`,
`Format::read` and `postcard::from_bytes` are all monomorphised into the probe's call site,
and fat LTO inlines them there. A real firmware pays the same way, at its own call sites, so
the whole-image delta is the honest figure for a generic codec and the `layers` column is
not. The report prints `Δflash`, `probe` and `layers` on every row, so the reading is
available rather than taken on trust.

**The 208 B is per instantiation, not per feature.** `Format::read<T>` is generic, so each
distinct workflow result type instantiates its own postcard deserializer. The row drives one
`T`, `(u8, u16)`. Adding a second — `(u32, i64, bool)` — takes the row to 542 B, so the
second type costs **334 B**, more than the first one reports. A firmware decoding three answer types
pays three times, and ADR 0030 left 66 B under the gated budget. The row is a floor and is
described as one; the alternative, driving two types in the probe, would publish a number
that overstates the single-type case instead.

**A codec feature is compiled by four stages of its own.** Every other stage passes
`--no-default-features`, so `codec-lint`, `codec-test`, `codec-docs` and `codec-firmware`
are what lint it, test it, build its documentation, and build it for the part. Without them
the helpers would ship unlinted and untested with the build green, which is the failure the
per-crate coverage gate cannot see either: `cargo xtask coverage` runs
`--no-default-features` too, so this code is in no coverage denominator. `codec-test` runs
every target rather than one file, because the `compile_fail` doctest on `FromPostcard` is
what says a borrowed `T` cannot reach a workflow.

The `serde` feature *alone* is compiled by none of the four — all four select `postcard`,
which implies it. What compiles it is the size job: the `waymaker-embassy/serde` row links
the probe against that selection, and a bridge that stopped compiling under it fails there.

**`thiserror` is now reachable from a firmware layer**, through `cobs`, when the `postcard`
feature is on. CLAUDE.md's line that neither `thiserror` nor `anyhow` is reachable in the
firmware crates was true and no longer is, and is corrected rather than left standing.

**What is not decided.** There is no `Encode`. §07's inputs are bytes a workflow already
holds, and nothing in this workspace asks a codec to produce them; adding one would be a
second surface with no caller. There is no `defmt` feature either — §04 names it, and it is
a logging decision rather than a codec one.

**What is not checked.** That a `Format` implementor reads the bytes it was handed: an
implementation that ignored them and answered a constant would satisfy every rule here, and
the workflow would replay a value the journal never held. That is the same standing as
`Activities::perform`'s contract not to truncate its own answer. And `codec-is-optional`
compares *names*, so a type alias for `DeserializeOwned` declared in `decode.rs` and used in
`ctx.rs` names nothing forbidden. Nor does it read *which* feature gates an item: `code_only`
removes string literals along with comments, so the attribute's argument is gone by the time
the scan sees it. That loses nothing the rule claims — any single positive feature gate keeps
the item out of a default build — and gating a codec on the wrong feature is a compile error
the moment that feature is enabled. A compound `cfg` is not read as gating at all, so an item
behind `any(feature = "postcard", unix)` is reported rather than trusted, which is the
conservative direction.
