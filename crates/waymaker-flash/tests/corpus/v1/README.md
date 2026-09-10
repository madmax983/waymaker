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
from `waymaker-flash`, and cross-checked against `tests/frame.rs`'s golden frames. One case
is the artifact of that cross-check: `record-08-run-failed.bin` is byte-identical to
`golden::RUN_FAILED`, and `the_corpus_agrees_with_the_golden_frame_it_overlaps` is what says
so. The rest of the cross-check happened off-repo and left nothing behind, which is stated
rather than implied. A corpus produced by the code under test would prove only that the code
agrees with itself.

## The rule

**A case is added. A case is never regenerated.**

`docs::WIRE_FORMAT_CORPUS_FILES` carries every file's length and CRC-32, and the
`wire-format` gate rule compares them — so regenerating a case means editing a digest a
reviewer can see, rather than making a same-length binary change that renders in a diff as
`Bin` with no content. `.gitattributes` marks these files `binary`, so no checkout rewrites a
byte of them either. An
end-of-line conversion here would turn a fixture into a different journal, and the test that
noticed would look like a format break rather than a checkout.

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
| `record-07-run-completed-empty-align-16.bin` | an empty payload, padded to a sixteen-byte program unit |
| `journal-align-4.bin` | three frames back to back, as a scan walks them |
| `record-03-effect-completed-align-8.bin` | a record whose unpadded length is not already a multiple of its program unit, so the padding really pads |
| `bank-header-align-4.bin` | a bank header frame |
| `generation-seal-align-4.bin` | the generation seal that names that header |
| `*-wide-*.bin` | the same shapes with a distinct non-zero byte in every position of every multi-byte field |

The wide cases exist because the others cannot say a width narrowed. A `version` read as a
`u8` decodes every narrow case to exactly the record it expects and re-encodes it
byte-identically, because none of them has a high byte to lose.

Kinds 10 and 11 are reserved and no v1 writer produces one, so neither has a case. A case
for a kind nothing writes would be bytes the format does not have.
