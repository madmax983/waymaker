# The v1 format conformance corpus

Frozen bytes. Every later 1.x firmware must decode each file here to the record
`crates/waymaker-flash/tests/corpus.rs` names for it, and must re-encode that record to the
same bytes.

## What these files are for

Every other test in this crate drives the encoder and the decoder together, so a record kind
renumbered or a field reordered changes both sides at once and every round trip still
passes. The change is invisible until a device that shipped last year is asked to read a
journal this firmware wrote. These bytes were written before the change, which is the only
thing that can notice.

## How they were derived

By an encoder written from the field list in
[`docs/format/wire-format-v1.md`](../../../../../docs/format/wire-format-v1.md), separately
from `waymaker-flash`, and cross-checked against `tests/frame.rs`'s golden frames — which
that encoder reproduces byte for byte. A corpus produced by the code under test would prove
only that the code agrees with itself.

## The rule

**A case is added. A case is never regenerated.**

A file here that no longer decodes is a wire-format break, not a stale fixture. Fix the
code, or bump the format version and write the migration — see
[ADR 0037](../../../../../docs/adr/0037-the-wire-format-is-frozen-at-v1-and-migration-is-a-new-bank.md).

Adding a record kind means adding a case: `every_record_kind_this_firmware_writes_has_a_corpus_case`
fails a build without one, and `the_corpus_directory_holds_exactly_the_cases_the_table_names`
fails a build over a file no case names.

## What is covered

| File | Holds |
| --- | --- |
| `record-01-run-started.bin` … `record-09-version-marker.bin` | one frame per record kind a v1 writer produces, at a byte-programmable granularity |
| `record-03-effect-completed-align-4.bin` | padding, and a four-byte commit seal |
| `record-06-timer-fired-align-8.bin` | an empty body at an eight-byte granularity |
| `record-07-run-completed-empty-align-16.bin` | the shortest record there is, at the widest granularity here |
| `journal-align-4.bin` | three frames back to back, as a scan walks them |
| `bank-header-align-4.bin` | a bank header frame |
| `generation-seal-align-4.bin` | the generation seal that names that header |

Kinds 10 and 11 are reserved and no v1 writer produces one, so neither has a case. A case
for a kind nothing writes would be bytes the format does not have.
