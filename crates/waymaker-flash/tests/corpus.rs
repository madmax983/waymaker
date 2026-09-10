//! The v1 format conformance corpus: frozen bytes every later 1.x firmware must decode.
//!
//! Issue [#41](https://github.com/madmax983/waymaker/issues/41) asks for "recorded byte
//! sequences committed to the repository that every future version must still decode
//! identically". [`corpus/v1`](../tests/corpus/v1/README.md) is that corpus, and this file
//! is what runs it.
//!
//! # What a corpus catches that nothing else here does
//!
//! Every other test in this crate drives the encoder and the decoder together. A record
//! kind renumbered, a field reordered, a checksum taken over the wrong range: each changes
//! both sides at once, so every round trip still passes and every property still holds. The
//! change is invisible until a device that shipped last year is asked to read a journal
//! this firmware wrote. Bytes committed to the repository are the only thing that can
//! notice, because they were written before the change.
//!
//! `tests/frame.rs`'s golden frames make the same argument for nine records at one
//! alignment each. This corpus is the wider claim: every record kind, four program
//! granularities, a multi-record journal, the bank header and the generation seal — the
//! whole of what a v1 device puts on media.
//!
//! # How the bytes were derived
//!
//! By an encoder written from the field list in
//! [`docs/format/wire-format-v1.md`](../../../docs/format/wire-format-v1.md), separately
//! from this crate, and cross-checked against `tests/frame.rs`'s golden frames — which the
//! same encoder reproduces byte for byte. They are **frozen**: a case is added, never
//! regenerated. See the corpus's own README.
//!
//! # Every claim here is falsifiable
//!
//! [`every_byte_a_reader_reads_is_covered_by_a_check`] mutates each corpus file one byte at
//! a time and requires the decoder to refuse each mutation of a byte it reads — and to
//! ignore each mutation of a pad byte, which is §09's "stale tail bytes never interpreted".
//! A corpus that passed because nothing read it would fail that test.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use waymaker_core::timer::ClockKind;
use waymaker_core::version::GateId;
use waymaker_core::{ActivityKind, EffectSeq, RecordRef, RunId};
use waymaker_flash::bank::{self, BankHeader, Generation};
use waymaker_flash::frame::{self, ProgramAlign, Scan};

/// What one corpus file holds.
enum Expected {
    /// One record frame, at the granularity it was encoded for.
    Record {
        record: RecordRef<'static>,
        align: u8,
    },
    /// Several record frames back to back, as [`Scan`] walks them.
    Journal {
        records: &'static [RecordRef<'static>],
        align: u8,
    },
    /// A bank header frame. Its granularity is a field of the header itself.
    BankHeader { header: BankHeader<'static> },
    /// A generation seal, and the header file whose frame it names.
    GenerationSeal {
        generation: Generation,
        header_file: &'static str,
        align: u8,
    },
}

/// One corpus case: a file name, and what that file must decode to for ever.
struct Case {
    file: &'static str,
    expected: Expected,
}

/// The run input every `RunStarted` and bank-header case in the corpus carries.
const RUN_INPUT: &[u8] = b"input";

/// The whole corpus, as a table.
///
/// The file names are the *only* place a corpus file may be named:
/// [`the_corpus_directory_holds_exactly_the_cases_the_table_names`] reads the directory and
/// requires the two to agree in both directions, so a file added without a case is a
/// failure rather than a file nothing reads.
#[expect(
    clippy::too_many_lines,
    reason = "one literal per corpus case; splitting the table hides what it covers"
)]
fn cases() -> Vec<Case> {
    vec![
        Case {
            file: "record-01-run-started.bin",
            expected: Expected::Record {
                record: RecordRef::RunStarted {
                    workflow_kind: 7,
                    workflow_version: 3,
                    input: RUN_INPUT,
                },
                align: 1,
            },
        },
        Case {
            file: "record-02-effect-scheduled.bin",
            expected: Expected::Record {
                record: RecordRef::EffectScheduled {
                    seq: EffectSeq(1),
                    kind: ActivityKind(9),
                    input_len: 5,
                    input_crc: 0xDEAD_BEEF,
                },
                align: 1,
            },
        },
        Case {
            file: "record-03-effect-completed.bin",
            expected: Expected::Record {
                record: RecordRef::EffectCompleted {
                    seq: EffectSeq(2),
                    result: &[1, 2, 3],
                },
                align: 1,
            },
        },
        Case {
            file: "record-04-effect-failed.bin",
            expected: Expected::Record {
                record: RecordRef::EffectFailed {
                    seq: EffectSeq(3),
                    error: b"no",
                },
                align: 1,
            },
        },
        Case {
            file: "record-05-timer-scheduled.bin",
            expected: Expected::Record {
                record: RecordRef::TimerScheduled {
                    seq: EffectSeq(4),
                    clock_kind: ClockKind::AT_PERSISTENT_TIME,
                    deadline: 0x0102_0304_0506_0708,
                    armed_at: 0x1122_3344_5566_7788,
                },
                align: 1,
            },
        },
        Case {
            file: "record-06-timer-fired.bin",
            expected: Expected::Record {
                record: RecordRef::TimerFired { seq: EffectSeq(4) },
                align: 1,
            },
        },
        Case {
            file: "record-07-run-completed.bin",
            expected: Expected::Record {
                record: RecordRef::RunCompleted { result: b"ok" },
                align: 1,
            },
        },
        Case {
            file: "record-08-run-failed.bin",
            expected: Expected::Record {
                record: RecordRef::RunFailed { error: b"why" },
                align: 1,
            },
        },
        Case {
            file: "record-09-version-marker.bin",
            expected: Expected::Record {
                record: RecordRef::VersionMarker {
                    seq: EffectSeq(5),
                    gate: GateId(0x1234),
                    version: 2,
                },
                align: 1,
            },
        },
        Case {
            file: "record-03-effect-completed-align-4.bin",
            expected: Expected::Record {
                record: RecordRef::EffectCompleted {
                    seq: EffectSeq(2),
                    result: &[1, 2, 3],
                },
                align: 4,
            },
        },
        Case {
            file: "record-06-timer-fired-align-8.bin",
            expected: Expected::Record {
                record: RecordRef::TimerFired { seq: EffectSeq(4) },
                align: 8,
            },
        },
        Case {
            file: "record-07-run-completed-empty-align-16.bin",
            expected: Expected::Record {
                record: RecordRef::RunCompleted { result: &[] },
                align: 16,
            },
        },
        // Six cases whose every multi-byte field carries a distinct non-zero byte in every
        // position. Without them the corpus cannot say a width narrowed: a `version` read as
        // a `u8` decodes every other case here to exactly the record it expects, and
        // re-encodes byte-identically, because none of them has a high byte to lose.
        Case {
            file: "record-01-run-started-wide.bin",
            expected: Expected::Record {
                record: RecordRef::RunStarted {
                    workflow_kind: 0xA1B2,
                    workflow_version: 0xC3D4,
                    input: b"wide",
                },
                align: 1,
            },
        },
        Case {
            file: "record-02-effect-scheduled-wide.bin",
            expected: Expected::Record {
                record: RecordRef::EffectScheduled {
                    seq: EffectSeq(0xFEDC_BA98),
                    kind: ActivityKind(0x1A2B),
                    input_len: 0x3C4D,
                    input_crc: 0x5E6F_7081,
                },
                align: 1,
            },
        },
        Case {
            file: "record-09-version-marker-wide.bin",
            expected: Expected::Record {
                record: RecordRef::VersionMarker {
                    seq: EffectSeq(0x0102_0304),
                    gate: GateId(0xF1E2),
                    version: 0xD3C4,
                },
                align: 1,
            },
        },
        // And one record whose unpadded length is not already a multiple of its program
        // unit, so the padding half of the tooth has more than a single byte of a single
        // file to work with.
        Case {
            file: "record-03-effect-completed-align-8.bin",
            expected: Expected::Record {
                record: RecordRef::EffectCompleted {
                    seq: EffectSeq(0x1122_3344),
                    result: &[1, 2, 3],
                },
                align: 8,
            },
        },
        Case {
            file: "bank-header-wide-align-8.bin",
            expected: Expected::BankHeader {
                header: BankHeader {
                    run: RunId(0x8899_AABB_CCDD_EEFF),
                    align: align_of(8),
                    workflow_kind: 0xA1B2,
                    workflow_version: 0xC3D4,
                    input_schema: 0xE5F6,
                    input: b"wide",
                },
            },
        },
        Case {
            file: "generation-seal-wide-align-8.bin",
            expected: Expected::GenerationSeal {
                generation: Generation(0x91A2_B3C4),
                header_file: "bank-header-wide-align-8.bin",
                align: 8,
            },
        },
        Case {
            file: "journal-align-4.bin",
            expected: Expected::Journal {
                records: &[
                    RecordRef::RunStarted {
                        workflow_kind: 7,
                        workflow_version: 3,
                        input: RUN_INPUT,
                    },
                    RecordRef::EffectScheduled {
                        seq: EffectSeq(0),
                        kind: ActivityKind(9),
                        input_len: 5,
                        input_crc: 0xDEAD_BEEF,
                    },
                    RecordRef::EffectCompleted {
                        seq: EffectSeq(0),
                        result: &[1, 2, 3],
                    },
                ],
                align: 4,
            },
        },
        Case {
            file: "bank-header-align-4.bin",
            expected: Expected::BankHeader {
                header: BankHeader {
                    run: RunId(0x0011_2233_4455_6677),
                    align: align_of(4),
                    workflow_kind: 7,
                    workflow_version: 3,
                    input_schema: 0x00AB,
                    input: RUN_INPUT,
                },
            },
        },
        Case {
            file: "generation-seal-align-4.bin",
            expected: Expected::GenerationSeal {
                generation: Generation(5),
                header_file: "bank-header-align-4.bin",
                align: 4,
            },
        },
    ]
}

/// Where the corpus lives, relative to this crate.
fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("corpus")
        .join("v1")
}

/// The bytes of one corpus file, or none at all.
///
/// Total rather than panicking: the workspace denies `panic!` outside a test body, and a
/// helper in an integration test is not one. A file that is not there reads as empty, which
/// fails every assertion below — and
/// [`the_corpus_directory_holds_exactly_the_cases_the_table_names`] is what names it.
fn read(file: &str) -> Vec<u8> {
    fs::read(corpus_dir().join(file)).unwrap_or_default()
}

/// The granularity a case was encoded at, as the type the encoder takes.
///
/// Total for [`read`]'s reason, and it fails closed the same way: a granularity the table
/// got wrong falls back to one byte, and the case then re-encodes to other bytes than its
/// file holds.
fn align_of(align: u8) -> ProgramAlign {
    ProgramAlign::new(u16::from(align)).unwrap_or(ProgramAlign::BYTE)
}

#[test]
fn every_corpus_file_decodes_to_the_record_the_table_names() {
    let cases = cases();
    assert!(!cases.is_empty(), "the corpus table is empty");

    for case in &cases {
        let bytes = read(case.file);
        match &case.expected {
            Expected::Record { record, .. } => {
                let frame = frame::decode(&bytes)
                    .unwrap_or_else(|error| panic!("`{}` did not decode: {error:?}", case.file));
                assert_eq!(
                    frame.decoded,
                    frame::Decoded::Record(*record),
                    "`{}` decoded to another record",
                    case.file
                );
                assert_eq!(
                    frame.format_version,
                    frame::FORMAT_VERSION,
                    "`{}` declares another format version",
                    case.file
                );
            }
            Expected::Journal { records, align } => {
                let walked: Vec<RecordRef<'_>> = Scan::new(&bytes, align_of(*align))
                    .map(|step| {
                        step.unwrap_or_else(|error| {
                            panic!("`{}` did not walk: {error:?}", case.file)
                        })
                    })
                    .collect();
                assert_eq!(
                    walked.as_slice(),
                    *records,
                    "`{}` walked to another history",
                    case.file
                );
            }
            Expected::BankHeader { header } => {
                let decoded = bank::decode_header(&bytes)
                    .unwrap_or_else(|error| panic!("`{}` did not decode: {error:?}", case.file));
                assert_eq!(
                    decoded, *header,
                    "`{}` decoded to another header",
                    case.file
                );
            }
            Expected::GenerationSeal {
                generation,
                header_file,
                ..
            } => {
                let seal = bank::decode_seal(&bytes)
                    .unwrap_or_else(|error| panic!("`{}` did not decode: {error:?}", case.file));
                assert_eq!(
                    seal.generation, *generation,
                    "`{}` names another generation",
                    case.file
                );
                let header = read(header_file);
                assert_eq!(
                    bank::sealed_generation(&header, &bytes),
                    Some(*generation),
                    "`{}` does not seal `{header_file}`",
                    case.file
                );
            }
        }
    }
}

#[test]
fn re_encoding_every_corpus_case_reproduces_its_bytes() {
    // The other direction, and the one that catches a writer that drifted: a decoder made
    // permissive enough to read an old journal would pass the test above while writing
    // something else.
    let mut page = [0_u8; 512];
    for case in &cases() {
        let bytes = read(case.file);
        let written = match &case.expected {
            Expected::Record { record, align } => {
                frame::encode(record, align_of(*align), &mut page)
                    .unwrap_or_else(|error| panic!("`{}` did not re-encode: {error:?}", case.file))
            }
            Expected::Journal { records, align } => {
                let mut at = 0;
                for record in *records {
                    let slot = page
                        .get_mut(at..)
                        .unwrap_or_else(|| panic!("`{}` does not fit the page", case.file));
                    at += frame::encode(record, align_of(*align), slot).unwrap_or_else(|error| {
                        panic!("`{}` did not re-encode: {error:?}", case.file)
                    });
                }
                at
            }
            Expected::BankHeader { header } => bank::encode_header(header, &mut page)
                .unwrap_or_else(|error| panic!("`{}` did not re-encode: {error:?}", case.file)),
            Expected::GenerationSeal {
                generation,
                header_file,
                align,
            } => {
                let header = read(header_file);
                let seal = bank::seal_for(&header, *generation).unwrap_or_else(|error| {
                    panic!("`{}` has no seal for its header: {error:?}", case.file)
                });
                bank::encode_seal(&seal, align_of(*align), &mut page)
                    .unwrap_or_else(|error| panic!("`{}` did not re-encode: {error:?}", case.file))
            }
        };
        assert_eq!(
            page.get(..written),
            Some(bytes.as_slice()),
            "`{}` re-encoded to other bytes",
            case.file
        );
    }
}

#[test]
fn the_corpus_directory_holds_exactly_the_cases_the_table_names() {
    // Both directions, because both rot. A case naming a file that is not there is a check
    // that cannot run; a file no case names is a file nothing reads, which is how a corpus
    // becomes decoration.
    let named: BTreeSet<String> = cases().iter().map(|case| (*case.file).to_owned()).collect();
    let mut present = BTreeSet::new();
    for entry in fs::read_dir(corpus_dir()).expect("the corpus directory should be readable") {
        let entry = entry.expect("the corpus directory should be listable");
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "bin") {
            present.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }
    assert_eq!(
        named, present,
        "the corpus table and the directory disagree"
    );
}

#[test]
fn every_record_kind_this_firmware_writes_has_a_corpus_case() {
    // The census. §09 numbers eleven kinds; the two with no body yet — `SIGNAL_RECEIVED` and
    // `CHILD_STARTED` — are reserved rather than written, and they are not `RecordRef`
    // variants at all, so a corpus case for either would be bytes no writer produces.
    //
    // `written` is derived from `SAMPLES` rather than listed, and `SAMPLES` is held to
    // `variant_index`'s exhaustive `match` by the test below. So giving a reserved kind a
    // body — which makes it a `RecordRef` variant — is a compile error in `variant_index`,
    // then a failure here until it has a corpus case. A hardcoded list would have been a
    // list that stops growing.
    let written: BTreeSet<u8> = samples().iter().map(|record| record.kind().0).collect();
    let covered: BTreeSet<u8> = cases()
        .iter()
        .flat_map(|case| match &case.expected {
            Expected::Record { record, .. } => vec![record.kind().0],
            Expected::Journal { records, .. } => {
                records.iter().map(|record| record.kind().0).collect()
            }
            Expected::BankHeader { .. } | Expected::GenerationSeal { .. } => Vec::new(),
        })
        .collect();
    for kind in &written {
        assert!(
            covered.contains(kind),
            "record kind {kind} is written to media and has no corpus case"
        );
    }
    assert_eq!(
        covered, written,
        "the corpus covers a kind no writer produces"
    );
}

#[test]
fn the_sample_list_holds_one_record_of_every_variant() {
    // What makes the census above grow. `variant_index` has no wildcard arm, so a variant
    // added to `RecordRef` does not compile until it is answered here; this then fails until
    // `samples` carries one.
    let mut seen = BTreeSet::new();
    for record in samples() {
        seen.insert(variant_index(&record));
    }
    assert_eq!(
        seen,
        (0..VARIANTS).collect::<BTreeSet<usize>>(),
        "the sample list has gained or lost a variant"
    );
}

/// How many variants `RecordRef` has.
const VARIANTS: usize = 9;

/// Which variant a record is, as an exhaustive match with no wildcard.
const fn variant_index(record: &RecordRef<'_>) -> usize {
    match record {
        RecordRef::RunStarted { .. } => 0,
        RecordRef::EffectScheduled { .. } => 1,
        RecordRef::EffectCompleted { .. } => 2,
        RecordRef::EffectFailed { .. } => 3,
        RecordRef::TimerScheduled { .. } => 4,
        RecordRef::TimerFired { .. } => 5,
        RecordRef::RunCompleted { .. } => 6,
        RecordRef::RunFailed { .. } => 7,
        RecordRef::VersionMarker { .. } => 8,
    }
}

/// One record of every variant a v1 writer produces.
fn samples() -> Vec<RecordRef<'static>> {
    vec![
        RecordRef::RunStarted {
            workflow_kind: 0,
            workflow_version: 0,
            input: &[],
        },
        RecordRef::EffectScheduled {
            seq: EffectSeq(0),
            kind: ActivityKind(0),
            input_len: 0,
            input_crc: 0,
        },
        RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: &[],
        },
        RecordRef::EffectFailed {
            seq: EffectSeq(0),
            error: &[],
        },
        RecordRef::TimerScheduled {
            seq: EffectSeq(0),
            clock_kind: ClockKind::AFTER_BOOT,
            deadline: 0,
            armed_at: 0,
        },
        RecordRef::TimerFired { seq: EffectSeq(0) },
        RecordRef::RunCompleted { result: &[] },
        RecordRef::RunFailed { error: &[] },
        RecordRef::VersionMarker {
            seq: EffectSeq(0),
            gate: GateId(0),
            version: 0,
        },
    ]
}

#[test]
fn every_byte_a_reader_reads_is_covered_by_a_check() {
    // The tooth. A corpus is an instrument, and an instrument that cannot fail is
    // decoration: this drives every single-byte mutation of every case and requires the
    // reader to refuse each byte it reads — and to ignore each pad byte, which is §09's
    // "padding to the device's program alignment with stale tail bytes never interpreted".
    //
    // Every case, not the record ones alone. An earlier version skipped the journal, the
    // bank header and the generation seal, which left the whole bank codec — its own magic,
    // its own prefix width, its own two checks — with no tooth at all, under a test whose
    // name said otherwise.
    // Counted per shape rather than in total, because a total is exactly how three of the
    // four shapes go untested behind a number the record cases alone make large.
    let mut checked = [0_usize; 4];
    let mut ignored = 0_usize;
    for case in &cases() {
        let bytes = read(case.file);
        let pads = pad_ranges(&case.expected);
        let shape = match case.expected {
            Expected::Record { .. } => 0,
            Expected::Journal { .. } => 1,
            Expected::BankHeader { .. } => 2,
            Expected::GenerationSeal { .. } => 3,
        };

        for index in 0..bytes.len() {
            let mut mutated = bytes.clone();
            let slot = mutated.get_mut(index).expect("in range");
            *slot = slot.wrapping_add(1);

            if pads.iter().any(|pad| pad.contains(&index)) {
                assert!(
                    reads_back(&mutated, case),
                    "`{}` byte {index} is padding and changed what the bytes say",
                    case.file
                );
                ignored += 1;
            } else {
                assert!(
                    !reads_back(&mutated, case),
                    "`{}` byte {index} is read and the mutation was not caught",
                    case.file
                );
                if let Some(count) = checked.get_mut(shape) {
                    *count += 1;
                }
            }
        }
    }
    for (shape, count) in checked.iter().enumerate() {
        assert!(
            *count > 0,
            "shape {shape} had no read byte mutated, so the tooth is blunt for it"
        );
    }
    assert!(
        ignored > 0,
        "no pad byte was mutated, so the padding half of the claim is untested"
    );
}

/// Whether `bytes` still reads back as the case expects.
///
/// One predicate for all four shapes, so the tooth above says the same thing about each.
fn reads_back(bytes: &[u8], case: &Case) -> bool {
    match &case.expected {
        Expected::Record { record, align } => {
            let mut scan = Scan::new(bytes, align_of(*align));
            scan.next() == Some(Ok(*record))
        }
        Expected::Journal { records, align } => {
            let walked: Vec<RecordRef<'_>> = Scan::new(bytes, align_of(*align))
                .take_while(Result::is_ok)
                .flatten()
                .collect();
            walked.as_slice() == *records
        }
        Expected::BankHeader { header } => bank::decode_header(bytes) == Ok(*header),
        Expected::GenerationSeal {
            generation,
            header_file,
            ..
        } => bank::sealed_generation(&read(header_file), bytes) == Some(*generation),
    }
}

/// The byte ranges of `expected` that no reader reads: the pad between a frame's last
/// checksum byte and whatever follows it.
///
/// Derived from the record rather than from the file, because a range taken from the file's
/// own length would be a range the file agrees with however wrong it is.
fn pad_ranges(expected: &Expected) -> Vec<core::ops::Range<usize>> {
    match expected {
        Expected::Record { record, align } => pad_ranges_of(&[*record], align_of(*align), 0),
        Expected::Journal { records, align } => pad_ranges_of(records, align_of(*align), 0),
        Expected::BankHeader { header } => {
            // A bank header's frame is its prefix, its input and its trailer; everything
            // after that, up to the program unit, is pad.
            let unpadded =
                bank::HEADER_PREFIX_BYTES + header.input.len() + bank::HEADER_TRAILER_BYTES;
            let padded = header.align.round_up(unpadded).unwrap_or(unpadded);
            core::iter::once(unpadded..padded).collect()
        }
        Expected::GenerationSeal { align, .. } => {
            // Twelve bytes, rounded up. At a granularity that divides twelve there is no
            // pad at all, and the empty range is the honest answer rather than a skip.
            let padded = align_of(*align)
                .round_up(bank::SEAL_BYTES)
                .unwrap_or(bank::SEAL_BYTES);
            core::iter::once(bank::SEAL_BYTES..padded).collect()
        }
    }
}

/// The pad ranges of a run of records laid out back to back from `at`.
fn pad_ranges_of(
    records: &[RecordRef<'_>],
    align: ProgramAlign,
    mut at: usize,
) -> Vec<core::ops::Range<usize>> {
    let mut ranges = Vec::new();
    for record in records {
        let unpadded = frame::FRAME_OVERHEAD_BYTES + payload_len_of(record);
        let (Ok(body), Ok(total)) = (
            frame::body_len(record, align),
            frame::encoded_len(record, align),
        ) else {
            unreachable!("every case in the table encodes")
        };
        ranges.push(at + unpadded..at + body);
        at += total;
    }
    ranges
}

/// How many payload bytes a record spends, for the padding arithmetic above.
///
/// Written out here rather than taken from `frame::encoded_len`, because the padding claim
/// is about where the *frame* stops and a figure the encoder computed would move with it.
/// The four fixed widths are §09's, and the `const` assertions in `frame.rs` are what hold
/// them.
const fn payload_len_of(record: &RecordRef<'_>) -> usize {
    match record {
        RecordRef::RunStarted { input, .. } => 4 + input.len(),
        RecordRef::EffectScheduled { .. } => 8,
        RecordRef::TimerScheduled { .. } => 17,
        RecordRef::TimerFired { .. } => 0,
        RecordRef::VersionMarker { .. } => 4,
        RecordRef::EffectCompleted { result: bytes, .. }
        | RecordRef::EffectFailed { error: bytes, .. }
        | RecordRef::RunCompleted { result: bytes }
        | RecordRef::RunFailed { error: bytes } => bytes.len(),
    }
}

#[test]
fn the_corpus_is_read_at_the_format_version_it_was_written_at() {
    // The freeze, stated as the promise issue #41 makes: a later 1.x firmware reads what an
    // earlier one wrote. `reads_format_version` is what a firmware answers with, and every
    // corpus file declares a version it answers `true` for.
    let mut checked = 0_usize;
    for case in &cases() {
        // A generation seal carries no version byte: it is a fixed twelve bytes that the
        // header it names is read at. Every other case is a frame, and a frame's third byte
        // is its version.
        if matches!(case.expected, Expected::GenerationSeal { .. }) {
            continue;
        }
        let bytes = read(case.file);
        let version = *bytes.get(2).expect("every frame has a version byte");
        checked += 1;
        assert!(
            frame::reads_format_version(version),
            "`{}` declares format version {version}, which this firmware does not read",
            case.file
        );
    }
    assert!(checked > 0, "no corpus case carried a version byte");
}
