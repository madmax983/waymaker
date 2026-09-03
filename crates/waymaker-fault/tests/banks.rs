//! The fourth line of §15's oracle: `recovered_banks.count_authoritative() == 1`.
//!
//! Issue [#19](https://github.com/madmax983/waymaker/issues/19) asks for "bank generation
//! selection under partial swaps", and §02 decision 7 is what it is protecting: "a new run
//! becomes authoritative only after its payload and generation seal are durable". A swap
//! interrupted anywhere must leave exactly one bank a reader would boot from — never two,
//! and never none.
//!
//! # This models rung 0.2's protocol; it does not implement it
//!
//! `waymaker-flash` has no bank geometry, no generation seal and no swap at rung 0.1, and
//! deliberately so — [`frame`]'s "Deferred" section says both belong to 0.2, and a firmware
//! crate that grew them early would be charged for them against an 8 KiB budget before
//! anything needed them. What is here instead is the smallest thing that gives the oracle's
//! fourth line something real to be checked against: a bank is a run's history written with
//! the *real* §09 codec, and its generation seal is the terminal record that closes it. The
//! writer, the erase, the barriers and every crash point in between are the real harness.
//!
//! What that buys, and what it does not: the *selection rule* — a bank counts only when its
//! frames verify and its seal is present, and the highest generation wins — is exercised
//! against every way a swap can be interrupted, so the shape of the answer is held. When
//! rung 0.2 writes the real seal as a storage-program unit, this file is what it has to keep
//! satisfying, and [`a_swap_that_clears_both_banks_first_leaves_nothing_to_boot_from`] and
//! [`a_selection_that_ignores_the_seal_finds_two_authorities`] are the proof that satisfying
//! it means something.

use std::collections::BTreeSet;

use waymaker_core::{ActivityKind, EffectSeq, RecordRef, ReplayCursor, RunId};
use waymaker_fault::{
    Breach, Durability, FaultError, Harness, Injection, Op, Progress, RecordId, Recovery, Run,
    Session, verify_oracle,
};
use waymaker_flash::frame::{self, ProgramAlign, Scan};
use waymaker_flash::storage::{Geometry, StableStorage};

/// The run every bank below records.
const RUN: RunId = RunId(0x0000_0000_0000_00B7);

/// Where each bank starts, and how long it is.
const BANK_A: u32 = 0;
const BANK_B: u32 = 128;
const BANK_BYTES: u32 = 128;

/// The one record the swap declares: a bank swap is one durable unit or it is nothing.
const SWAP: RecordId = RecordId(1);

/// The generation the swap installs.
const NEW_GENERATION: u32 = 2;

/// The op index of the barrier that first makes a bank authoritative.
///
/// Everything before it is the device as a previous life left it, and a device that has
/// never committed anything has no authoritative bank to count. Asserted against the
/// recorded sequence in every test below, so a writer edited without this constant fails
/// loudly rather than silently moving what "after the first commit" means.
const FIRST_COMMIT: usize = 9;

/// Two banks of four erase blocks each.
///
/// The block size is a quarter of a bank rather than a whole one on purpose: an erase is
/// interrupted at erase blocks and nowhere else, so a bank of *one* block has no interior
/// tear point at all and "a partially erased bank" would be a case the sweep could not
/// reach. Four blocks gives three.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 32, 4, 1) else {
        unreachable!("256 is eight whole 32-byte blocks of 4-byte units of single bytes")
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
// A bank, and what makes one authoritative
// ---------------------------------------------------------------------------------------

/// The four records a bank at `generation` holds: a run, one effect, and the seal.
///
/// The seal is the terminal record, and it carries the generation. That is the property
/// the selection below turns on: a bank is authoritative only once its *last* record is
/// durable, which is §02 decision 7 with the frame format standing in for rung 0.2's
/// storage-program unit.
const fn bank_records(generation: &[u8; 4]) -> [RecordRef<'_>; 4] {
    [
        RecordRef::RunStarted {
            workflow_kind: 7,
            workflow_version: 1,
            input: generation,
        },
        RecordRef::EffectScheduled {
            seq: EffectSeq::FIRST,
            kind: ActivityKind(1),
            input_len: 0,
            input_crc: 0,
        },
        RecordRef::EffectCompleted {
            seq: EffectSeq::FIRST,
            result: generation,
        },
        RecordRef::RunCompleted { result: generation },
    ]
}

/// Writes a whole bank at `base`, one program per record.
fn write_bank(session: &mut Session, base: u32, generation: u32) -> Result<(), FaultError> {
    let bytes = generation.to_le_bytes();
    let mut at = base;
    let mut buffer = [0_u8; 64];
    for record in bank_records(&bytes) {
        let Ok(written) = frame::encode(&record, align(), &mut buffer) else {
            unreachable!("64 bytes is more than any bank record")
        };
        let Some(frame_bytes) = buffer.get(..written) else {
            unreachable!("`encode` reports what it wrote")
        };
        let len = u32::try_from(written).unwrap_or(u32::MAX);
        if at.wrapping_add(len) > base.wrapping_add(BANK_BYTES) {
            unreachable!("a bank's four records fit in {BANK_BYTES} bytes")
        }
        session.program(at, frame_bytes)?;
        at = at.wrapping_add(len);
    }
    Ok(())
}

/// The generation `bank` is sealed with, or `None` if it is not sealed.
///
/// A bank is read the way recovery reads one: the real scan, then the real cursor, stopping
/// at the first frame either refuses. The seal is the terminal record, so a bank whose
/// frames verify but whose last record never landed has no generation — which is exactly
/// the state a swap is in for every crash point but the last.
fn sealed_generation(bank: &[u8]) -> Option<u32> {
    let mut cursor = ReplayCursor::new(RUN);
    let mut generation = None;
    for step in Scan::new(bank, align()) {
        let Ok(record) = step else { break };
        if cursor.advance(record).is_err() {
            break;
        }
        if let RecordRef::RunCompleted { result } = record {
            generation = <[u8; 4]>::try_from(result).ok().map(u32::from_le_bytes);
        }
    }
    generation
}

/// The two banks of `image`, as slices.
fn banks(image: &[u8]) -> [&[u8]; 2] {
    let a = image
        .get(BANK_A as usize..(BANK_A + BANK_BYTES) as usize)
        .unwrap_or_default();
    let b = image
        .get(BANK_B as usize..(BANK_B + BANK_BYTES) as usize)
        .unwrap_or_default();
    [a, b]
}

/// How many banks a reader would boot from, and which generation it would find.
///
/// Sealed banks with *different* generations are not two authorities: the higher one wins,
/// which is what a generation is for. Two banks sealed at the same generation are, and the
/// count says so rather than picking one — a tie is the state no protocol may produce, so a
/// selection that resolved it would be hiding the bug the count exists to find.
fn authority(image: &[u8]) -> (usize, Option<u32>) {
    let sealed: Vec<u32> = banks(image)
        .iter()
        .filter_map(|bank| sealed_generation(bank))
        .collect();
    match sealed.as_slice() {
        [] => (0, None),
        [only] => (1, Some(*only)),
        [left, right] if left == right => (2, Some(*left)),
        [left, right] => (1, Some(*left.max(right))),
        _ => unreachable!("a device of two banks has at most two sealed ones"),
    }
}

/// What recovery produced: the swap's record if `installed` is the generation on media.
///
/// Deliberately not "…and exactly one bank is authoritative". Whether the *count* is legal
/// is the oracle's fourth line, and a recovery that pre-filtered on it would answer that
/// line before the oracle was asked — leaving `AmbiguousAuthority` reported as a lost record
/// instead, which is the wrong diagnosis for the right failure.
fn recovered(image: &[u8], installed: u32) -> Vec<RecordId> {
    let (count, generation) = authority(image);
    if count >= 1 && generation == Some(installed) {
        vec![SWAP]
    } else {
        Vec::new()
    }
}

// ---------------------------------------------------------------------------------------
// The writers
// ---------------------------------------------------------------------------------------

/// The honest swap: never erase the bank you are booting from.
fn swap(session: &mut Session) -> Result<(), FaultError> {
    // The device as a previous life left it: a stale bank, then the one in use. Neither is
    // declared, because a record is what recovery must account for and setup is not one.
    write_bank(session, BANK_B, 0)?;
    session.barrier()?;
    write_bank(session, BANK_A, 1)?;
    session.barrier()?;

    // Preparing the spare bank is not part of the durable unit, and declaring it as part of
    // one would be a silent weakening: a barrier issued after the erase and before the
    // payload would acknowledge the record while nothing of its content is on media, and
    // the oracle would then *require* recovery to produce a swap that had not happened.
    // An erased bank is not a swap. It is a bank with nothing in it.
    session.erase(BANK_B, BANK_BYTES)?;
    session.barrier()?;

    // One durable unit: the payload and the seal, ordered by one barrier at the end.
    // Nothing in here is separately recoverable, so nothing in here is a record of its own.
    session.begin_record(SWAP);
    write_bank(session, BANK_B, NEW_GENERATION)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

/// The swap with the bug: clear the whole device before writing the new bank.
fn swap_clearing_both(session: &mut Session) -> Result<(), FaultError> {
    write_bank(session, BANK_B, 0)?;
    session.barrier()?;
    write_bank(session, BANK_A, 1)?;
    session.barrier()?;

    // The bug, and only the bug: the same shape as `swap`, with the bank being booted from
    // erased alongside the spare one.
    session.erase(BANK_A, BANK_BYTES)?;
    session.erase(BANK_B, BANK_BYTES)?;
    session.barrier()?;

    session.begin_record(SWAP);
    write_bank(session, BANK_B, NEW_GENERATION)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

/// The swap with a different bug: the new bank is sealed at the generation the bank being
/// replaced already carries.
///
/// Every frame verifies, every seal is present, and nothing on media says which of the two
/// banks is newer. This is the only writer here whose *real* selection rule can return two
/// authorities, and it is why `authority` resolves a tie by refusing to pick rather than by
/// taking the first: a tie is the state no protocol may reach, so a selection that resolved
/// it would hide the bug the count exists to find.
fn swap_without_bumping_the_generation(session: &mut Session) -> Result<(), FaultError> {
    write_bank(session, BANK_B, 0)?;
    session.barrier()?;
    write_bank(session, BANK_A, 1)?;
    session.barrier()?;

    session.erase(BANK_B, BANK_BYTES)?;
    session.barrier()?;

    session.begin_record(SWAP);
    write_bank(session, BANK_B, 1)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

fn drive(writer: fn(&mut Session) -> Result<(), FaultError>) -> Vec<Run> {
    match Harness::new(geometry()).run(writer) {
        Ok(runs) => runs,
        Err(error) => unreachable!("{error}"),
    }
}

/// Whether `run` happened at or after the swap's first durable commit.
fn after_first_commit(run: &Run) -> bool {
    match run.injection() {
        None => true,
        Some(Injection { op, progress, .. }) => {
            op > FIRST_COMMIT || (op == FIRST_COMMIT && progress == Progress::Whole)
        }
    }
}

// ---------------------------------------------------------------------------------------
// The property
// ---------------------------------------------------------------------------------------

#[test]
fn exactly_one_bank_is_authoritative_at_every_point_a_swap_can_be_interrupted() {
    let runs = drive(swap);
    let clean = runs
        .first()
        .unwrap_or_else(|| unreachable!("the fault-free run"));
    assert_eq!(
        clean.ops().get(FIRST_COMMIT),
        Some(&Op::Barrier),
        "the writer changed shape, so `FIRST_COMMIT` no longer names the first commit: {:?}",
        clean.ops()
    );
    assert!(runs.len() > 100, "only {} runs", runs.len());

    let (count, generation) = authority(clean.image());
    assert_eq!((count, generation), (1, Some(NEW_GENERATION)));

    let mut installed = 0_usize;
    let mut still_the_old_one = 0_usize;
    let mut before_any_commit = 0_usize;
    for run in &runs {
        let (count, generation) = authority(run.image());
        // Never two. For *this* writer that is a property of the protocol rather than of
        // the selection rule — three distinct generations across two banks cannot tie — so
        // the assertion cannot fail here, and it is not where the "never two" half is held.
        // `a_swap_that_forgets_to_bump_the_generation_leaves_two_authorities` is: it drives
        // a writer whose banks *can* tie and shows `authority` returning two.
        assert!(
            count <= 1,
            "at {:?}: {count} banks are authoritative, at generations {generation:?}",
            run.injection()
        );

        if !after_first_commit(run) {
            before_any_commit += 1;
            continue;
        }
        assert_eq!(
            count,
            1,
            "at {:?}: a committed device has {count} authoritative banks",
            run.injection()
        );

        match generation {
            Some(NEW_GENERATION) => installed += 1,
            Some(1) => still_the_old_one += 1,
            other => unreachable!("a bank at generation {other:?} was never written"),
        }

        let history = recovered(run.image(), NEW_GENERATION);
        assert_eq!(
            verify_oracle(
                run.ledger(),
                &Recovery::new(&history).authoritative_banks(count)
            ),
            Ok(()),
            "at {:?}: recovered {history:?} from a ledger of {:?}",
            run.injection(),
            run.ledger().records().collect::<Vec<_>>()
        );
    }

    // The sweep has to have seen both outcomes, or "exactly one" held because only one was
    // ever possible.
    assert!(
        installed > 0 && still_the_old_one > 0,
        "{installed} runs installed the new generation and {still_the_old_one} kept the old \
         one"
    );
    assert!(
        before_any_commit > 0,
        "no crash point fell before the first commit, so the device-with-no-history case \
         was never reached"
    );
}

#[test]
fn a_swap_torn_anywhere_is_never_half_installed() {
    // §02 decision 7: a new run becomes authoritative only after its payload *and* its
    // generation seal are durable. So a torn swap must never be the thing a reader boots
    // from, whatever landed.
    let runs = drive(swap);
    let mut torn = 0_usize;
    for run in &runs {
        if run.ledger().torn(SWAP) != Some(true) {
            continue;
        }
        torn += 1;
        assert_eq!(
            authority(run.image()).1,
            Some(1),
            "at {:?}: a torn swap became authoritative",
            run.injection()
        );
        assert_eq!(run.ledger().state(SWAP), Some(Durability::PossiblyDurable));
    }
    assert!(torn > 0, "no crash point tore the swap");
}

#[test]
fn a_partially_erased_bank_is_never_mistaken_for_a_sealed_one() {
    // The stale-tail hazard, on the one operation that can produce it: a bank of two erase
    // blocks, erased from the front, interrupted at the block boundary. The head reads
    // erased and a whole sealed generation is still sitting behind it.
    let runs = drive(swap);
    let interrupted: Vec<&Run> = runs
        .iter()
        .filter(|run| {
            matches!(
                run.injection(),
                Some(Injection {
                    progress: Progress::Bytes(_),
                    ..
                })
            ) && run.ops().iter().any(|op| matches!(op, Op::Erase { .. }))
        })
        .filter(|run| {
            let [_, b] = banks(run.image());
            b.first() == Some(&0xFF) && b.iter().any(|byte| *byte != 0xFF)
        })
        .collect();
    // More than one *world*, not more than one run. A `PowerLoss` and a `Failure` armed at
    // the same point of the same operation produce byte-identical media here, because no
    // writer in this file reacts to an error — so counting runs would report two where the
    // sweep has seen one. With a bank of one erase block, as this file had before, there is
    // no interior point at all and the whole test reduces to a single image.
    //
    // Two rather than three: the erase spans four blocks and stops at three interior
    // points, but a bank's four records reach only into the third, so stopping before the
    // fourth erases bytes that were already erased and leaves the bank entirely blank —
    // which is not a stale tail and is filtered out above.
    let worlds: BTreeSet<&[u8]> = interrupted.iter().map(|run| run.image()).collect();
    assert!(
        worlds.len() >= 2,
        "only {} distinct half-erased banks across the sweep",
        worlds.len()
    );
    for run in interrupted {
        let [_, b] = banks(run.image());
        assert_eq!(
            sealed_generation(b),
            None,
            "at {:?}: a half-erased bank reported a generation",
            run.injection()
        );
        assert_eq!(authority(run.image()), (1, Some(1)));
    }
}

// ---------------------------------------------------------------------------------------
// Teeth
// ---------------------------------------------------------------------------------------

#[test]
fn a_swap_that_clears_both_banks_first_leaves_nothing_to_boot_from() {
    // The bug a two-bank protocol exists to prevent: erasing the bank you are booting from.
    // At the crash point between the two erases and the new seal there is no authority at
    // all, and the oracle has to say so rather than accept an empty recovery as a legal
    // prefix.
    let runs = drive(swap_clearing_both);

    let mut caught = 0_usize;
    for run in &runs {
        let (count, _) = authority(run.image());
        if count != 0 || !after_first_commit(run) {
            continue;
        }
        let history = recovered(run.image(), NEW_GENERATION);
        assert_eq!(
            verify_oracle(
                run.ledger(),
                &Recovery::new(&history).authoritative_banks(count)
            ),
            Err(Breach::NoAuthoritativeBank),
            "at {:?}: a device with no authoritative bank was accepted",
            run.injection()
        );
        caught += 1;
    }
    assert!(
        caught > 0,
        "clearing both banks never left the device with nothing to boot from, so the \
         mutant proves nothing"
    );
}

#[test]
fn a_selection_that_ignores_the_seal_finds_two_authorities() {
    // The other half of the same line. This selection asks "does the bank have frames in
    // it", which is the rule somebody writes before the generation seal exists — and after
    // any swap has written its payload, both banks answer yes.
    let runs = drive(swap);

    let mut caught = 0_usize;
    for run in &runs {
        let count = banks(run.image())
            .iter()
            .filter(|bank| {
                Scan::new(bank, align())
                    .next()
                    .is_some_and(|step| step.is_ok())
            })
            .count();
        if count < 2 {
            continue;
        }
        let history = recovered(run.image(), NEW_GENERATION);
        assert_eq!(
            verify_oracle(
                run.ledger(),
                &Recovery::new(&history).authoritative_banks(count)
            ),
            Err(Breach::AmbiguousAuthority { count }),
            "at {:?}: two authoritative banks were accepted",
            run.injection()
        );
        caught += 1;
    }
    assert!(
        caught > 0,
        "a seal-blind selection never found two banks, so the mutant proves nothing"
    );
}

#[test]
fn a_swap_that_forgets_to_bump_the_generation_leaves_two_authorities() {
    // The other way a two-bank device loses its authority: not by having none, but by having
    // two that no longer disagree. §02 decision 7 makes a new run authoritative on its
    // *generation* seal, and a seal that repeats a generation seals nothing.
    //
    // Unlike `a_selection_that_ignores_the_seal_finds_two_authorities`, the selection here
    // is the real one. Nothing is substituted: `authority` is asked the same question it is
    // asked everywhere else in this file, and on this device it answers two.
    let runs = drive(swap_without_bumping_the_generation);

    let mut caught = 0_usize;
    for run in &runs {
        let (count, _) = authority(run.image());
        if count < 2 {
            continue;
        }
        let history = recovered(run.image(), 1);
        assert_eq!(
            verify_oracle(
                run.ledger(),
                &Recovery::new(&history).authoritative_banks(count)
            ),
            Err(Breach::AmbiguousAuthority { count }),
            "at {:?}: two banks sealed at the same generation were accepted",
            run.injection()
        );
        caught += 1;
    }
    assert!(
        caught > 0,
        "a swap that reused the old generation never produced two authorities, so the \
         mutant proves nothing"
    );
}
