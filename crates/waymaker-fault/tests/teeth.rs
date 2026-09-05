//! Proof that the sweep can fail: a broken cursor or a broken codec has to be caught.
//!
//! Issue [#19](https://github.com/madmax983/waymaker/issues/19)'s second exit criterion —
//! "a deliberately introduced bug in the cursor or codec makes it fail (the test is proven
//! to have teeth)". A property suite that only ever passes is indistinguishable from one
//! that checks nothing, and the difference cannot be established by reading it.
//!
//! # What is really mutated, and what is modelled
//!
//! The codec mutants are the real thing. [ADR 0012](https://github.com/madmax983/waymaker/blob/main/docs/adr/0012-the-integrity-check-is-swappable-behind-a-trait-and-the-seal-widths-are-not.md)
//! put the frame's two seals behind `waymaker-flash`'s [`IntegrityCheck`] trait, so a codec
//! that stops sealing what it says it seals is an implementation of that trait — no fork of
//! `frame.rs`, no `#[cfg]`, and the journal is written *and* read by the same broken
//! firmware, which is the only version of the bug worth testing. The seal each mutant
//! returns is the value an unprogrammed field reads back as, because that is what makes it
//! a plausible bug rather than an obviously fatal one: a check that accepts erased bytes
//! accepts a torn write.
//!
//! Most of the cursor mutants are models. `waymaker-core`'s [`ReplayCursor`] is a `const fn`
//! state machine with no seam to inject through, so what is mutated in those is the *step* a
//! caller takes with the record it hands back: history read one short, two records swapped,
//! one skipped, one invented past the end. Those are the observable behaviours of the
//! off-by-one and ordering bugs a cursor can have, and what they establish is that the oracle
//! names each of them at some crash point rather than shrugging.
//!
//! One is not a model.
//! [`without_the_cursor_a_reordered_journal_would_be_replayed_as_history`] runs the real
//! cursor against a journal whose frames all verify and whose *order* is illegal, and shows
//! the scan alone accepting exactly what the cursor refuses. That case cannot arise from a
//! crash point — every history the sweeps draw is legal, and a prefix of a legal history is
//! legal too, so across every run in this workspace the cursor never refuses a record the
//! scan accepted — which is the code being right rather than the suite being weak, but it
//! does mean the sweeps alone would not notice a cursor that stopped checking. That test is
//! what notices.
//!
//! # Every mutant is checked against a control
//!
//! Each test asserts the unmutated pipeline is clean over the same runs first. Otherwise a
//! mutant "caught" by a fixture that was already broken would prove the opposite of what it
//! claims.

use std::cell::RefCell;
use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, EffectSeq, RecordRef, ReplayCursor, RunId};
use waymaker_fault::{
    Breach, FaultError, Harness, RecordId, Recovery, Run, Session, verify_oracle,
};
use waymaker_flash::frame::{self, ProgramAlign, Scan};
use waymaker_flash::integrity::{Catalogued, IntegrityCheck};
use waymaker_flash::storage::{Geometry, StableStorage};

/// The run the fixture below belongs to.
const RUN: RunId = RunId(0x0000_0000_DEAD_BEEF);

/// How many effects the fixture journal records.
const EFFECTS: u32 = 3;

fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 256, 4, 1) else {
        unreachable!("256 is one whole 256-byte block of 4-byte units of single bytes")
    };
    geometry
}

fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    align
}

// ---------------------------------------------------------------------------------------
// The mutants
// ---------------------------------------------------------------------------------------

/// A codec that computes the header's seal and takes the payload's on trust.
///
/// `frame_check` returns the value an unprogrammed four-byte field reads back as, so a
/// frame whose header landed and whose payload was torn verifies: the trailer was never
/// written, and the check agrees with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TrustsThePayload;

impl IntegrityCheck for TrustsThePayload {
    fn header_check(bytes: &[u8]) -> u16 {
        Catalogued::header_check(bytes)
    }

    fn frame_check(_: &[u8]) -> u32 {
        u32::MAX
    }
}

/// A codec that seals nothing at all, and reads back whatever an erased cell says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TrustsEverything;

impl IntegrityCheck for TrustsEverything {
    fn header_check(_: &[u8]) -> u16 {
        u16::MAX
    }

    fn frame_check(_: &[u8]) -> u32 {
        u32::MAX
    }
}

// ---------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------

/// The record id the fixture gives record `index`.
const fn id(index: u32) -> RecordId {
    RecordId(index)
}

/// The `index`-th record of the fixture history, borrowing `payload`.
fn record(index: u32, payload: &[u8]) -> RecordRef<'_> {
    if index == 0 {
        return RecordRef::RunStarted {
            workflow_kind: 7,
            workflow_version: 1,
            input: payload,
        };
    }
    let seq = EffectSeq((index - 1) / 2);
    if index % 2 == 1 {
        RecordRef::EffectScheduled {
            seq,
            kind: ActivityKind(1),
            input_len: u16::try_from(payload.len()).unwrap_or(0),
            input_crc: frame::input_digest(payload),
        }
    } else {
        RecordRef::EffectCompleted {
            seq,
            result: payload,
        }
    }
}

/// Which fixture record `record` is, by what is in it.
const fn identify(record: &RecordRef<'_>) -> RecordId {
    match record {
        RecordRef::RunStarted { .. } => id(0),
        RecordRef::EffectScheduled { seq, .. } => id(seq.0.wrapping_mul(2).wrapping_add(1)),
        RecordRef::EffectCompleted { seq, .. } => id(seq.0.wrapping_mul(2).wrapping_add(2)),
        RecordRef::EffectFailed { .. }
        | RecordRef::RunCompleted { .. }
        | RecordRef::RunFailed { .. } => RecordId(u32::MAX),
    }
}

/// How many records the fixture writes: a run start, and a schedule and a completion each.
const RECORDS: u32 = 1 + EFFECTS * 2;

/// The payload record `index` carries.
fn payload(index: u32) -> Vec<u8> {
    (0..4)
        .map(|at| u8::try_from((index + at) & 0xFF).unwrap_or(0))
        .collect()
}

/// Appends the fixture history, sealing every frame with `C`.
fn write<C: IntegrityCheck>(session: &mut Session) -> Result<(), FaultError> {
    let mut buffer = [0_u8; 64];
    let mut at = 0_u32;
    for index in 0..RECORDS {
        let bytes = payload(index);
        let Ok(written) = frame::encode_with::<C>(&record(index, &bytes), align(), &mut buffer)
        else {
            unreachable!("64 bytes is more than any fixture record")
        };
        let Some(frame_bytes) = buffer.get(..written) else {
            unreachable!("`encode_with` reports what it wrote")
        };
        session.begin_record(id(index));
        session.program(at, frame_bytes)?;
        at = at.wrapping_add(u32::try_from(written).unwrap_or(u32::MAX));
        session.barrier()?;
    }
    Ok(())
}

/// Appends the fixture history with each record's commit seal programmed *before* the frame
/// it seals, sealing every frame with `C`.
///
/// The inverse of §07's order, and the reason issue #24's writer is a typestate rather than
/// a convention: this function cannot be written against
/// [`waymaker_flash::append`](waymaker_flash::append), because the value that can program a
/// seal is one only the payload barrier produces. It reaches around the writer API to the
/// session, which is what a tooth is for.
///
/// What it buys the two codec mutants below is a torn frame under a seal that landed. With
/// the honest order a torn frame has no seal over it whatever the codec does, so a codec
/// that stopped sealing its payload would be caught by the seal rather than by the oracle —
/// and the sweep would have nothing to say.
fn write_sealing_early<C: IntegrityCheck>(session: &mut Session) -> Result<(), FaultError> {
    let mut buffer = [0_u8; 64];
    let mut at = 0_u32;
    for index in 0..RECORDS {
        let bytes = payload(index);
        let staged = record(index, &bytes);
        let (Ok(written), Ok(body)) = (
            frame::encode_with::<C>(&staged, align(), &mut buffer),
            frame::body_len(&staged, align()),
        ) else {
            unreachable!("64 bytes is more than any fixture record")
        };
        let (Some(seal_bytes), Some(frame_bytes)) = (buffer.get(body..written), buffer.get(..body))
        else {
            unreachable!("`encode_with` reports what it wrote")
        };
        session.begin_record(id(index));
        session.program(
            at.wrapping_add(u32::try_from(body).unwrap_or(0)),
            seal_bytes,
        )?;
        session.program(at, frame_bytes)?;
        at = at.wrapping_add(u32::try_from(written).unwrap_or(u32::MAX));
        session.barrier()?;
    }
    Ok(())
}

/// Every run of the fixture, sealed and read with `C`.
fn runs<C: IntegrityCheck>() -> Vec<Run> {
    match Harness::new(geometry()).run(write::<C>) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

/// Every run of the fixture written by [`write_sealing_early`], sealed and read with `C`.
fn runs_sealing_early<C: IntegrityCheck>() -> Vec<Run> {
    match Harness::new(geometry()).run(write_sealing_early::<C>) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

/// The breaches a codec provokes over runs whose seals were programmed too early.
///
/// The control comes first, for the reason [`breaches`] takes one: a writer that seals ahead
/// of its frame is not on its own enough to recover a torn record — the shipped codec still
/// refuses one, because its trailer is a checksum an erased field does not satisfy. So a
/// mutant "caught" here is evidence about the codec rather than about the writer.
fn breaches_sealing_early<C: IntegrityCheck>() -> BTreeSet<&'static str> {
    for run in &runs_sealing_early::<Catalogued>() {
        let recovered = recover::<Catalogued>(run.image());
        if let Err(breach) = verify_oracle(run.ledger(), &Recovery::new(&recovered)) {
            unreachable!("the control is not clean: {breach}");
        }
    }

    let mut caught = BTreeSet::new();
    for run in &runs_sealing_early::<C>() {
        let recovered = recover::<C>(run.image());
        if let Err(breach) = verify_oracle(run.ledger(), &Recovery::new(&recovered)) {
            caught.insert(discriminant(&breach));
        }
    }
    caught
}

/// Recovery, through the real scan and the real cursor, verifying with `C`.
fn recover<C: IntegrityCheck>(image: &[u8]) -> Vec<RecordId> {
    let mut cursor = ReplayCursor::new(RUN);
    let mut recovered = Vec::new();
    for step in Scan::<'_, C>::with_integrity(image, align()) {
        let Ok(found) = step else { break };
        if cursor.advance(found).is_err() {
            break;
        }
        recovered.push(identify(&found));
    }
    recovered
}

/// The breaches `mutate` provokes across the whole enumerated crash set.
///
/// The control comes first: an unmutated recovery of the same runs must be clean, or a
/// "caught" mutant would only be evidence that the fixture was already wrong.
fn breaches<C, M>(runs: &[Run], mutate: M) -> BTreeSet<Breach>
where
    C: IntegrityCheck,
    M: Fn(Vec<RecordId>) -> Vec<RecordId>,
{
    let mut caught = BTreeSet::new();
    for run in runs {
        let honest = recover::<C>(run.image());
        assert_eq!(
            verify_oracle(run.ledger(), &Recovery::new(&honest)),
            Ok(()),
            "the control failed at {:?}: recovered {honest:?}",
            run.injection()
        );
        if let Err(breach) = verify_oracle(run.ledger(), &Recovery::new(&mutate(honest))) {
            caught.insert(breach);
        }
    }
    caught
}

// ---------------------------------------------------------------------------------------
// The codec has teeth
// ---------------------------------------------------------------------------------------

#[test]
fn the_unmutated_pipeline_recovers_the_whole_fixture_and_satisfies_the_oracle() {
    let runs = runs::<Catalogued>();
    assert!(runs.len() > 100, "only {} runs", runs.len());

    let clean = runs
        .first()
        .unwrap_or_else(|| unreachable!("the fault-free run"));
    assert_eq!(
        recover::<Catalogued>(clean.image()),
        (0..RECORDS).map(id).collect::<Vec<_>>()
    );

    let mut lengths = BTreeSet::new();
    for run in &runs {
        let recovered = recover::<Catalogued>(run.image());
        assert_eq!(
            verify_oracle(run.ledger(), &Recovery::new(&recovered)),
            Ok(()),
            "at {:?}: recovered {recovered:?}",
            run.injection()
        );
        lengths.insert(recovered.len());
    }
    assert_eq!(
        lengths,
        (0..=RECORDS as usize).collect::<BTreeSet<usize>>(),
        "not every prefix length occurred, so the fixture does not exercise the range"
    );
}

#[test]
fn a_codec_that_stops_sealing_its_payload_recovers_a_record_that_is_half_there() {
    // The bug: `frame_check` returns what an unwritten trailer reads back as. Every frame
    // still round-trips, every test that only checks `decode(encode(r)) == r` still passes,
    // and a write torn inside its payload is now accepted as history.
    //
    // It takes a second bug to get there since issue #24, and that is the finding rather
    // than an inconvenience: with the commit seal written after a payload barrier, a torn
    // frame has no seal over it and is refused whatever the codec believes. So the writer
    // here programs the seal *first* — §07's order inverted — and the two together are what
    // recovers half a record.
    let caught = breaches_sealing_early::<TrustsThePayload>();
    assert!(
        caught.contains("RecoveredATornRecord"),
        "a codec that takes its payload on trust was not caught; breaches seen: {caught:?}"
    );
}

#[test]
fn a_codec_that_seals_nothing_recovers_a_record_that_is_half_there() {
    // As above, and for the same reason it needs a writer that seals ahead of its frame.
    let caught = breaches_sealing_early::<TrustsEverything>();
    assert!(
        caught.contains("RecoveredATornRecord"),
        "a codec that seals nothing at all was not caught; breaches seen: {caught:?}"
    );
}

#[test]
fn the_honest_order_refuses_a_torn_record_whatever_the_codec_believes() {
    // The other half of the two tests above, and issue #24's guarantee stated as a tooth of
    // its own: with the seal programmed after the payload barrier, neither codec mutant can
    // recover a torn record. Both are caught by the seal instead of by the checksum they
    // stopped computing, at every crash point.
    //
    // Without this the two tests above would read as "the codec mutants are still caught",
    // which they are not: what catches them is a writer bug those tests have to add.
    for run in &runs::<TrustsThePayload>() {
        let recovered = recover::<TrustsThePayload>(run.image());
        if let Err(breach) = verify_oracle(run.ledger(), &Recovery::new(&recovered)) {
            unreachable!("a torn record survived the commit seal: {breach}");
        }
    }
    for run in &runs::<TrustsEverything>() {
        let recovered = recover::<TrustsEverything>(run.image());
        if let Err(breach) = verify_oracle(run.ledger(), &Recovery::new(&recovered)) {
            unreachable!("a torn record survived the commit seal: {breach}");
        }
    }
}

/// The name of a breach's variant, for asserting *which* diagnosis was reached.
const fn discriminant(breach: &Breach) -> &'static str {
    match breach {
        Breach::DuplicateRecordId { .. } => "DuplicateRecordId",
        Breach::NotAPrefix { .. } => "NotAPrefix",
        Breach::RecoveredWhatWasNeverAttempted { .. } => "RecoveredWhatWasNeverAttempted",
        Breach::RecoveredATornRecord { .. } => "RecoveredATornRecord",
        Breach::LostAnAcknowledgedRecord { .. } => "LostAnAcknowledgedRecord",
        Breach::DispatchedWithoutADurableIntent { .. } => "DispatchedWithoutADurableIntent",
        Breach::NoAuthoritativeBank => "NoAuthoritativeBank",
        Breach::AmbiguousAuthority { .. } => "AmbiguousAuthority",
    }
}

// ---------------------------------------------------------------------------------------
// The cursor has teeth
// ---------------------------------------------------------------------------------------

#[test]
fn a_cursor_that_stops_one_record_early_loses_an_acknowledgment() {
    let runs = runs::<Catalogued>();
    let caught = breaches::<Catalogued, _>(&runs, |mut history| {
        history.pop();
        history
    });
    assert!(
        caught
            .iter()
            .any(|breach| matches!(breach, Breach::LostAnAcknowledgedRecord { .. })),
        "a recovery short by one record was accepted at every crash point: {caught:?}"
    );
}

#[test]
fn a_cursor_that_swaps_two_records_is_not_a_prefix_of_anything() {
    let runs = runs::<Catalogued>();
    let caught = breaches::<Catalogued, _>(&runs, |mut history| {
        let len = history.len();
        if len >= 2 {
            history.swap(len - 2, len - 1);
        }
        history
    });
    assert!(
        caught
            .iter()
            .any(|breach| matches!(breach, Breach::NotAPrefix { .. })),
        "history read out of order was accepted at every crash point: {caught:?}"
    );
}

#[test]
fn a_cursor_that_skips_a_record_is_not_a_prefix_of_anything() {
    let runs = runs::<Catalogued>();
    let caught = breaches::<Catalogued, _>(&runs, |mut history| {
        if history.len() >= 2 {
            history.remove(0);
        }
        history
    });
    assert!(
        caught
            .iter()
            .any(|breach| matches!(breach, Breach::NotAPrefix { .. })),
        "history with a record skipped was accepted at every crash point: {caught:?}"
    );
}

#[test]
fn a_cursor_that_reads_one_record_past_history_invents_what_never_reached_media() {
    let runs = runs::<Catalogued>();
    let caught = breaches::<Catalogued, _>(&runs, |mut history| {
        let next = u32::try_from(history.len()).unwrap_or(u32::MAX);
        history.push(id(next));
        history
    });
    assert!(
        caught
            .iter()
            .any(|breach| matches!(breach, Breach::RecoveredWhatWasNeverAttempted { .. })),
        "a recovery that ran past the end of history was accepted at every crash point: \
         {caught:?}"
    );
}

#[test]
fn an_effect_dispatched_before_its_intent_is_durable_is_caught() {
    // The oracle's third line, reached by a writer that really commits the bug rather than
    // by a caller that mis-declares a dispatch.
    //
    // This matters, and it is subtle. For a writer that dispatches *after* its barrier — the
    // shape §02 decision 3 requires, and the shape every other writer in this workspace has
    // — the third line can never be the check that fires: a record whose barrier returned is
    // `Acknowledged`, so `ledger.acknowledged()` already demands it and
    // `LostAnAcknowledgedRecord` is reported first. The third line is only load-bearing for
    // an intent recovery is *permitted* to drop, which is exactly what a dispatch before the
    // barrier produces.
    //
    // So the writer below inverts the order: the effect happens, and then its intent is
    // recorded. At the crash points that tear that record, the effect has been dispatched
    // into a world that cannot be rolled back and no committed history accounts for it.
    let control = dispatch_log(false);
    let inverted = dispatch_log(true);

    // The control first. The correct order is caught by nothing, at any crash point, or a
    // "caught" verdict below would be evidence of the harness rather than of the bug.
    let mut dispatched_at_all = 0_usize;
    for (run, dispatched) in &control {
        dispatched_at_all += dispatched.len();
        let recovered = recover::<Catalogued>(run.image());
        assert_eq!(
            verify_oracle(
                run.ledger(),
                &Recovery::new(&recovered).dispatched(dispatched)
            ),
            Ok(()),
            "the control failed at {:?}: dispatched {dispatched:?}, recovered {recovered:?}",
            run.injection()
        );
    }
    assert!(
        dispatched_at_all > 0,
        "the control never dispatched anything, so it is not a control"
    );

    let mut caught = 0_usize;
    let mut other = Vec::new();
    for (run, dispatched) in &inverted {
        let recovered = recover::<Catalogued>(run.image());
        match verify_oracle(
            run.ledger(),
            &Recovery::new(&recovered).dispatched(dispatched),
        ) {
            Ok(()) => {}
            Err(Breach::DispatchedWithoutADurableIntent { intent }) => {
                assert!(
                    dispatched.contains(&intent),
                    "the oracle named {intent:?}, which was never dispatched"
                );
                caught += 1;
            }
            Err(breach) => other.push(breach),
        }
    }
    assert!(
        caught > 0,
        "an effect dispatched before its intent was durable was accepted at every crash          point; other breaches seen: {other:?}"
    );
    assert!(
        other.is_empty(),
        "the inverted writer breached the oracle in some other way as well: {other:?}"
    );
}

/// Every run of the fixture, with the dispatch each run performed.
///
/// `before_the_barrier` inverts §02 decision 3: the effect is handed to the world and the
/// schedule record is written afterwards.
fn dispatch_log(before_the_barrier: bool) -> Vec<(Run, Vec<RecordId>)> {
    let log: RefCell<Vec<Vec<RecordId>>> = RefCell::new(Vec::new());
    let runs = Harness::new(geometry()).run(|session| {
        log.borrow_mut().push(Vec::new());
        let mut buffer = [0_u8; 64];
        let mut at = 0_u32;
        for index in 0..RECORDS {
            let bytes = payload(index);
            let scheduled = matches!(record(index, &bytes), RecordRef::EffectScheduled { .. });
            let Ok(written) =
                frame::encode_with::<Catalogued>(&record(index, &bytes), align(), &mut buffer)
            else {
                unreachable!("64 bytes is more than any fixture record")
            };
            let Some(frame_bytes) = buffer.get(..written) else {
                unreachable!("`encode_with` reports what it wrote")
            };

            if before_the_barrier && scheduled {
                // The bug: the effect happens here, before anything of its intent is on
                // media.
                if let Some(run) = log.borrow_mut().last_mut() {
                    run.push(id(index));
                }
            }

            session.begin_record(id(index));
            session.program(at, frame_bytes)?;
            at = at.wrapping_add(u32::try_from(written).unwrap_or(u32::MAX));
            session.barrier()?;

            if !before_the_barrier && scheduled {
                // The barrier returned, so the intent is durable and the effect may happen.
                if let Some(run) = log.borrow_mut().last_mut() {
                    run.push(id(index));
                }
            }
        }
        Ok::<(), FaultError>(())
    });
    let runs = match runs {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    };
    let log = log.into_inner();
    assert_eq!(log.len(), runs.len(), "one dispatch log per run");
    runs.into_iter().zip(log).collect()
}

// ---------------------------------------------------------------------------------------
// The cursor is load-bearing
// ---------------------------------------------------------------------------------------

#[test]
fn without_the_cursor_a_reordered_journal_would_be_replayed_as_history() {
    // The cursor cannot be caught by the sweeps, and it is worth being exact about why.
    // Every history the generator draws is legal, and every crash point leaves a *prefix*
    // of a legal history, which is legal too — so across every run in this workspace the
    // cursor never refuses a record the scan accepted. That is the code being right, not the
    // suite being weak, but it does mean nothing in the sweeps would notice if
    // `ReplayCursor::advance` stopped checking anything at all.
    //
    // A journal that is illegal *in its ordering* while every frame in it verifies is the
    // one shape that separates the two readers, and it cannot arise from a crash point: it
    // takes a hand assembled — or damaged — journal. So this builds one, by transplanting
    // two whole frames on media, and shows that the scan alone accepts what the cursor
    // refuses.
    let runs = runs::<Catalogued>();
    let clean = runs
        .first()
        .unwrap_or_else(|| unreachable!("the fault-free run"));
    let image = clean.image();

    // Re-lay the journal with a schedule and its own completion transposed, so history says
    // an effect finished before it was ever scheduled. Rebuilt rather than swapped in place,
    // because the two frames are different widths — a completion carries no kind, length or
    // digest — and an in-place swap would be a transplant this format does not permit.
    let order = [0, 2, 1, 3, 4, 5, 6];
    assert_eq!(
        order.len(),
        RECORDS as usize,
        "the transposition must name every record the fixture writes"
    );
    let mut reordered = Vec::new();
    let mut buffer = [0_u8; 64];
    for index in order {
        let bytes = payload(index);
        let Ok(written) = frame::encode(&record(index, &bytes), align(), &mut buffer) else {
            unreachable!("64 bytes is more than any fixture record")
        };
        reordered.extend_from_slice(buffer.get(..written).unwrap_or_default());
    }
    reordered.resize(image.len(), 0xFF);

    // Every frame still checks out, so the codec has nothing to say about this journal.
    assert!(
        Scan::new(&reordered, align()).all(|step| step.is_ok()),
        "the transplant damaged a frame, so this tests the codec rather than the cursor"
    );

    // The scan alone: it hands back the completion before the schedule, and the oracle
    // reports history that is not a prefix of what was committed. This is the recovery a
    // cursor that checked nothing would produce.
    let scan_only: Vec<RecordId> = Scan::new(&reordered, align())
        .take_while(Result::is_ok)
        .flatten()
        .map(|record| identify(&record))
        .collect();
    assert_eq!(
        verify_oracle(clean.ledger(), &Recovery::new(&scan_only)),
        Err(Breach::NotAPrefix {
            position: 1,
            expected: Some(id(1)),
            found: id(2),
        }),
        "a reordered journal was accepted without the cursor, so the oracle would not \
         notice a cursor that stopped ordering history"
    );

    // The real reader: the cursor refuses the completion that has no schedule, recovery
    // stops there, and the only thing the oracle can hold against it is the acknowledged
    // records the transplant destroyed — never a reordering.
    let recovered = recover::<Catalogued>(&reordered);
    assert_eq!(
        recovered,
        vec![id(0)],
        "the cursor replayed a reordered journal"
    );
    assert_eq!(
        verify_oracle(clean.ledger(), &Recovery::new(&recovered)),
        Err(Breach::LostAnAcknowledgedRecord { record: id(1) }),
        "damaging a finished journal must lose records, and lose only those"
    );
}
