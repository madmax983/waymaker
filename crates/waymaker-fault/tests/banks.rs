//! Bank selection, against every point a swap can be interrupted.
//!
//! Issue [#22](https://github.com/madmax983/waymaker/issues/22): "Bank selection is tested
//! against every partial-swap crash point from the injector", and "exactly one bank is
//! authoritative after any crash. Recovery never combines the footprints of two runs." §02
//! decision 7 is the invariant behind both: "a new run becomes authoritative only after its
//! payload and generation seal are durable."
//!
//! # This drives the real thing
//!
//! Every byte below is written and read by `waymaker_flash::bank` — the real layout, the
//! real header codec, the real generation seal and the real [`select`]. The writer, the
//! erase, the barriers and every crash point in between are the real harness. An earlier
//! version of this file *modelled* rung 0.2's protocol with a terminal record standing in
//! for a seal, because there was no bank module to drive; what it asserted then is what this
//! keeps satisfying now.
//!
//! # What the teeth are
//!
//! Three writers with one bug each, and one reader with one bug. A suite that only ever sees
//! a correct protocol is a suite that would pass with the selection deleted, so each mutant
//! names the guarantee it breaks:
//! [`a_swap_that_clears_both_banks_first_leaves_nothing_to_boot_from`],
//! [`a_swap_that_forgets_to_bump_the_generation_leaves_two_authorities`],
//! [`a_selection_that_ignores_the_seal_finds_two_authorities`], and
//! [`a_selection_that_ignores_the_header_boots_a_bank_that_was_never_written`] — the last
//! being the one that says what the seal's header digest is for.
//!
//! [`select`]: waymaker_flash::bank::select

use waymaker_core::RunId;
use waymaker_fault::{
    Breach, Durability, FaultError, Harness, Injection, Op, Progress, RecordId, Recovery, Run,
    Session, verify_oracle,
};
use waymaker_flash::bank::{self, Authority, BankHeader, BankId, BankLayout, Generation};
use waymaker_flash::frame::ProgramAlign;
use waymaker_flash::storage::{Geometry, StableStorage};

/// The run every bank below records.
const RUN: RunId = RunId(0x0000_0000_0000_00B7);

/// The one record the swap declares: a bank swap is one durable unit or it is nothing.
const SWAP: RecordId = RecordId(1);

/// The generation the device's stale bank carries when the sweep starts.
const STALE: Generation = Generation(0);

/// The generation the device is booting from when the swap starts.
const CURRENT: Generation = Generation(1);

/// The generation the swap installs.
///
/// Minted from the generation it replaces rather than written down, so the sweep drives the
/// one function that makes "generations do not wrap" true. A literal here would leave
/// `Generation::successor` with no caller on any swap path in the workspace.
const NEW: Generation = match CURRENT.successor() {
    Some(next) => next,
    None => unreachable!(),
};

/// The op index of the barrier that first makes any bank authoritative.
///
/// Everything before it is a device that has never committed, and a device that has never
/// committed has no authoritative bank to have lost. Asserted against the recorded sequence
/// in every test below, so a writer edited without this constant fails loudly rather than
/// silently moving what "after the first commit" means.
const FIRST_COMMIT: usize = 3;

/// A device of eight erase blocks: two banks of four blocks each.
///
/// Four blocks per bank rather than one on purpose. An erase is interrupted at erase blocks
/// and nowhere else, so a bank of *one* block has no interior tear point and "a partially
/// erased bank" would be a case the sweep could not reach. Four gives three — and because
/// the seal sits in the bank's last block and the header in its first, one of those three is
/// the stale-tail hazard exactly: a bank whose header is gone and whose seal is not.
fn geometry() -> Geometry {
    let Ok(geometry) = Geometry::new(256, 32, 4, 1) else {
        unreachable!("256 is eight whole 32-byte blocks of 4-byte units of single bytes")
    };
    geometry
}

fn layout() -> BankLayout {
    let Ok(layout) = BankLayout::new(geometry()) else {
        unreachable!("eight erase blocks is four per bank")
    };
    layout
}

fn align() -> ProgramAlign {
    let Some(align) = ProgramAlign::new(4) else {
        unreachable!("4 is a power of two within the program-size range")
    };
    align
}

/// The run input each generation's header carries, so the three banks differ in content as
/// well as in generation.
const fn input(generation: Generation) -> &'static [u8] {
    match generation.0 {
        0 => b"stale",
        1 => b"current",
        _ => b"next",
    }
}

fn header_of(generation: Generation) -> BankHeader<'static> {
    BankHeader {
        run: RUN,
        align: align(),
        workflow_kind: 7,
        workflow_version: 1,
        input_schema: 1,
        input: input(generation),
    }
}

// ---------------------------------------------------------------------------------------
// The writer's half: what a swap programs
// ---------------------------------------------------------------------------------------

/// Programs `id`'s bank header, padded to the program unit, and reports the bytes it wrote.
fn program_header(
    session: &mut Session,
    id: BankId,
    generation: Generation,
) -> Result<usize, FaultError> {
    let region = layout().bank(id);
    let mut page = [0_u8; 64];
    let Ok(written) = bank::encode_header(&header_of(generation), &mut page) else {
        unreachable!("a bank header of this shape fits 64 bytes")
    };
    let Some(bytes) = page.get(..written) else {
        unreachable!("`encode_header` reports what it wrote")
    };
    if written > region.payload_bytes() as usize {
        unreachable!(
            "a bank header of this shape fits a {}-byte bank",
            region.bytes()
        )
    }
    session.program(region.base(), bytes)?;
    Ok(written)
}

/// Programs `id`'s generation seal, naming the header already on media.
///
/// The digest comes from [`bank::seal_for`] over the bytes that *landed*, read back from the
/// device, rather than from the header this writer meant to write. A writer that sealed what
/// it intended would seal a header a failed program never put there — which is the bug
/// [`swap_that_seals_whatever_landed`] has and this one does not.
fn program_seal(
    session: &mut Session,
    id: BankId,
    generation: Generation,
) -> Result<(), FaultError> {
    let region = layout().bank(id);
    let mut page = [0_u8; 64];
    let Some(read_back) = page.get_mut(..region.payload_bytes().min(64) as usize) else {
        unreachable!("64 bytes is within a bank's payload")
    };
    session.read(region.base(), read_back)?;
    let Ok(seal) = bank::seal_for(read_back, generation) else {
        // The header on media does not decode, so there is no seal to write. A writer that
        // wrote one anyway is `swap_that_seals_whatever_landed`.
        return Ok(());
    };
    let mut sealed = [0_u8; 16];
    let Ok(written) = bank::encode_seal(&seal, align(), &mut sealed) else {
        unreachable!("a seal fits 16 bytes at a 4-byte program unit")
    };
    let Some(bytes) = sealed.get(..written) else {
        unreachable!("`encode_seal` reports what it wrote")
    };
    session.program(region.seal_offset(), bytes)
}

/// Installs a whole bank: the header, the payload barrier, the seal, the seal's barrier.
///
/// §10 steps 3 to 6. The two barriers are the protocol rather than caution: §02 decision 7
/// makes a bank authoritative only after its payload *and* its seal are durable, and a seal
/// ordered by the same barrier as its payload is a seal that can reach media first.
fn install(session: &mut Session, id: BankId, generation: Generation) -> Result<(), FaultError> {
    program_header(session, id, generation)?;
    session.barrier()?;
    program_seal(session, id, generation)?;
    session.barrier()
}

/// The device as a previous life left it: a stale bank, then the one in use.
///
/// Neither is declared as a record, because a record is what recovery must account for and
/// setup is not one.
fn previous_life(session: &mut Session) -> Result<(), FaultError> {
    install(session, BankId::B, STALE)?;
    install(session, BankId::A, CURRENT)
}

/// The honest swap: never erase the bank you are booting from.
fn swap(session: &mut Session) -> Result<(), FaultError> {
    previous_life(session)?;

    // Preparing the spare bank is not part of the durable unit, and declaring it as part of
    // one would be a silent weakening: a barrier issued after the erase and before the
    // payload would acknowledge the record while nothing of its content is on media, and the
    // oracle would then *require* recovery to produce a swap that had not happened. An
    // erased bank is not a swap. It is a bank with nothing in it.
    let spare = layout().bank(BankId::B);
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;

    // Steps 3 and 4: the new bank's payload, and the barrier that makes it durable. Not a
    // record, for the reason the erase is not one. §10: "a crash before step 5 recovers the
    // old run", so a header on media with no seal over it is not a swap that happened — and
    // a record declared here would be acknowledged by *this* barrier, obliging recovery to
    // produce a swap the device is right to have no trace of.
    program_header(session, BankId::B, NEW)?;
    session.barrier()?;

    // Steps 5 and 6. The seal is the only separately recoverable thing in a swap, so it is
    // the whole of the record: §10's "a crash after step 6 recovers the new run" is this
    // barrier, and nothing before it.
    session.begin_record(SWAP);
    program_seal(session, BankId::B, NEW)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

/// The swap with the bug a two-bank protocol exists to prevent: clear the device first.
fn swap_clearing_both(session: &mut Session) -> Result<(), FaultError> {
    previous_life(session)?;

    let a = layout().bank(BankId::A);
    let b = layout().bank(BankId::B);
    session.erase(a.base(), a.bytes())?;
    session.erase(b.base(), b.bytes())?;
    session.barrier()?;

    program_header(session, BankId::B, NEW)?;
    session.barrier()?;
    session.begin_record(SWAP);
    program_seal(session, BankId::B, NEW)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

/// The swap with a different bug: the new bank is sealed at the generation the bank being
/// replaced already carries.
///
/// Every frame verifies, every seal is present, and nothing on media says which of the two
/// banks is newer.
fn swap_without_bumping_the_generation(session: &mut Session) -> Result<(), FaultError> {
    previous_life(session)?;

    let spare = layout().bank(BankId::B);
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;

    program_header(session, BankId::B, CURRENT)?;
    session.barrier()?;
    session.begin_record(SWAP);
    program_seal(session, BankId::B, CURRENT)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

/// The swap that seals a header it never checked landed.
///
/// The one bug in it is that the seal's digest comes from the header this writer *meant* to
/// write rather than from the bytes on media, and that it carries on past a failed program.
/// A device left by it can have a torn header under a perfectly valid, highest-generation
/// seal — which is the state
/// [`a_selection_that_ignores_the_header_boots_a_bank_that_was_never_written`] shows a
/// seal-blind reader booting and the real selection refusing.
fn swap_that_seals_whatever_landed(session: &mut Session) -> Result<(), FaultError> {
    previous_life(session)?;

    let spare = layout().bank(BankId::B);
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;

    // The failed program is swallowed rather than propagated: §12 says `program` may fail,
    // and this writer does not look.
    let _ignored = program_header(session, BankId::B, NEW);
    session.barrier()?;
    session.begin_record(SWAP);

    // Sealed from what the writer intended, not from what is on media.
    let mut page = [0_u8; 64];
    let Ok(written) = bank::encode_header(&header_of(NEW), &mut page) else {
        unreachable!("a bank header of this shape fits 64 bytes")
    };
    let Some(intended) = page.get(..written) else {
        unreachable!("`encode_header` reports what it wrote")
    };
    let Ok(seal) = bank::seal_for(intended, NEW) else {
        unreachable!("the header this writer just encoded decodes")
    };
    let mut sealed = [0_u8; 16];
    let Ok(seal_len) = bank::encode_seal(&seal, align(), &mut sealed) else {
        unreachable!("a seal fits 16 bytes at a 4-byte program unit")
    };
    let Some(seal_bytes) = sealed.get(..seal_len) else {
        unreachable!("`encode_seal` reports what it wrote")
    };
    session.program(spare.seal_offset(), seal_bytes)?;
    session.barrier()?;
    session.end_record();
    Ok(())
}

// ---------------------------------------------------------------------------------------
// The reader's half: what makes a bank authoritative
// ---------------------------------------------------------------------------------------

/// One bank's two regions of `image`: everything but the seal, and the seal.
fn regions(image: &[u8], id: BankId) -> (&[u8], &[u8]) {
    let region = layout().bank(id);
    let payload = image
        .get(region.base() as usize..(region.base() + region.payload_bytes()) as usize)
        .unwrap_or_default();
    let seal = image
        .get(region.seal_offset() as usize..(region.seal_offset() + region.seal_bytes()) as usize)
        .unwrap_or_default();
    (payload, seal)
}

/// Which bank a reader boots from, decided by the real selection rule.
fn authority(image: &[u8]) -> Authority {
    bank::select(BankId::ALL.map(|id| {
        let (header, seal) = regions(image, id);
        bank::sealed_generation(header, seal)
    }))
}

/// How many banks a reader would boot from — §15's fourth oracle line counts this.
const fn authoritative_banks(authority: Authority) -> usize {
    match authority {
        Authority::Unsealed => 0,
        Authority::Bank { .. } => 1,
        Authority::Ambiguous { .. } => 2,
    }
}

/// What recovery produced: the swap's record if `installed` is the generation on media.
///
/// Deliberately not "…and exactly one bank is authoritative". Whether the *count* is legal is
/// the oracle's fourth line, and a recovery that pre-filtered on it would answer that line
/// before the oracle was asked — leaving `AmbiguousAuthority` reported as a lost record
/// instead, which is the wrong diagnosis for the right failure.
fn recovered(image: &[u8], installed: Generation) -> Vec<RecordId> {
    match authority(image) {
        Authority::Bank { generation, .. } | Authority::Ambiguous { generation }
            if generation == installed =>
        {
            vec![SWAP]
        }
        _ => Vec::new(),
    }
}

/// The swap that seals the new bank with the *old* bank's digest.
///
/// The one bug: `seal_for` is asked about bank A — the bank being replaced, whose header
/// decodes perfectly well — and the answer is programmed into bank B. Every structure on
/// media is intact, both headers decode, both seals decode, and the higher generation names
/// a header that is not beneath it.
///
/// This is the writer the sweep needed. Every other mutant here breaks a bank by *damaging*
/// it, and damage stops `decode_header` before the digest is ever compared — so deleting the
/// comparison from `sealed_generation` left all eight tests in this file green. Review of
/// this change measured that: 0 of 303 runs disagreed. With this writer the comparison is the
/// only thing standing between a reader and the wrong bank.
fn swap_sealed_with_the_wrong_header(session: &mut Session) -> Result<(), FaultError> {
    previous_life(session)?;

    let spare = layout().bank(BankId::B);
    session.erase(spare.base(), spare.bytes())?;
    session.barrier()?;

    program_header(session, BankId::B, NEW)?;
    session.barrier()?;

    // Sealed from bank A's header rather than bank B's.
    let other = layout().bank(BankId::A);
    let mut page = [0_u8; 64];
    let Some(read_back) = page.get_mut(..other.payload_bytes().min(64) as usize) else {
        unreachable!("64 bytes is within a bank's payload")
    };
    session.read(other.base(), read_back)?;
    let Ok(seal) = bank::seal_for(read_back, NEW) else {
        return Ok(());
    };
    let mut sealed = [0_u8; 16];
    let Ok(written) = bank::encode_seal(&seal, align(), &mut sealed) else {
        unreachable!("a seal fits 16 bytes at a 4-byte program unit")
    };
    let Some(bytes) = sealed.get(..written) else {
        unreachable!("`encode_seal` reports what it wrote")
    };
    session.begin_record(SWAP);
    session.program(spare.seal_offset(), bytes)?;
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

/// Whether `run` happened at or after the device's first durable commit.
fn after_first_commit(run: &Run) -> bool {
    match run.injection() {
        None => true,
        Some(Injection { op, progress, .. }) => {
            op > FIRST_COMMIT || (op == FIRST_COMMIT && progress == Progress::Whole)
        }
    }
}

/// Asserts that `FIRST_COMMIT` still names the barrier it was chosen for.
fn check_shape(runs: &[Run]) -> &Run {
    let Some(clean) = runs.first() else {
        unreachable!("the fault-free run is always first")
    };
    assert_eq!(
        clean.ops().get(FIRST_COMMIT),
        Some(&Op::Barrier),
        "the writer changed shape, so `FIRST_COMMIT` no longer names the first commit: {:?}",
        clean.ops()
    );
    clean
}

// ---------------------------------------------------------------------------------------
// The property
// ---------------------------------------------------------------------------------------

#[test]
fn exactly_one_bank_is_authoritative_at_every_point_a_swap_can_be_interrupted() {
    let runs = drive(swap);
    let clean = check_shape(&runs);
    assert!(runs.len() > 100, "only {} runs", runs.len());
    assert_eq!(
        authority(clean.image()),
        Authority::Bank {
            id: BankId::B,
            generation: NEW
        }
    );

    let mut installed = 0_usize;
    let mut still_the_old_one = 0_usize;
    let mut still_the_stale_one = 0_usize;
    let mut before_any_commit = 0_usize;
    for run in &runs {
        let authority = authority(run.image());
        let count = authoritative_banks(authority);
        assert_ne!(
            authority,
            Authority::Ambiguous { generation: NEW },
            "at {:?}: two banks are authoritative",
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

        match authority {
            Authority::Bank { id, generation } if generation == NEW => {
                assert_eq!(id, BankId::B, "the new run was installed in the wrong bank");
                installed += 1;
            }
            Authority::Bank { id, generation } if generation == CURRENT => {
                assert_eq!(id, BankId::A, "the old run moved bank");
                still_the_old_one += 1;
            }
            // The device as its previous life left it, with the run now current not yet
            // sealed. Reached by the crash points inside the setup's second install, and a
            // legal state: one bank, the highest valid seal on the device, and its own run
            // input under it.
            Authority::Bank { id, generation } if generation == STALE => {
                assert_eq!(id, BankId::B, "the stale run moved bank");
                still_the_stale_one += 1;
            }
            other => unreachable!("a device in state {other:?} was never written"),
        }

        let history = recovered(run.image(), NEW);
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
        installed > 0 && still_the_old_one > 0 && still_the_stale_one > 0,
        "{installed} runs installed the new generation, {still_the_old_one} kept the current \
         one and {still_the_stale_one} were still on the stale one"
    );
    assert!(
        before_any_commit > 0,
        "no crash point fell before the first commit, so the device-with-no-history case was \
         never reached"
    );
}

#[test]
fn recovery_never_combines_the_footprints_of_two_runs() {
    // Issue #22's second "must hold": whichever bank a reader boots, the run input it reads
    // is one bank's and never a blend. A bank that decodes is a bank whose header and whose
    // seal are the same run's, because the seal names the header's digest.
    let runs = drive(swap);
    let mut seen_old = 0_usize;
    let mut seen_new = 0_usize;
    for run in &runs {
        let Authority::Bank { id, generation } = authority(run.image()) else {
            continue;
        };
        let (header, _) = regions(run.image(), id);
        let Ok(decoded) = bank::decode_header(header) else {
            unreachable!(
                "at {:?}: a bank was authoritative whose header does not decode",
                run.injection()
            )
        };
        assert_eq!(decoded.run, RUN);
        assert_eq!(
            decoded.input,
            input(generation),
            "at {:?}: a bank at generation {} carries another run's input",
            run.injection(),
            generation.0
        );
        if generation == NEW {
            seen_new += 1;
        } else {
            seen_old += 1;
        }
    }
    assert!(
        seen_old > 0 && seen_new > 0,
        "{seen_old} old, {seen_new} new"
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
            authority(run.image()),
            Authority::Bank {
                id: BankId::A,
                generation: CURRENT
            },
            "at {:?}: a torn swap became authoritative",
            run.injection()
        );
        assert_eq!(run.ledger().state(SWAP), Some(Durability::PossiblyDurable));
    }
    assert!(torn > 0, "no crash point tore the swap");
}

#[test]
fn a_partially_erased_bank_is_never_mistaken_for_a_sealed_one() {
    // The stale-tail hazard, on the one operation that produces it. The seal is in the
    // bank's last erase block and the header is in its first, so an erase interrupted at a
    // block boundary leaves an erased header with a whole sealed generation still sitting
    // behind it.
    let runs = drive(swap);
    let mut half_erased = 0_usize;
    for run in &runs {
        let (header, seal) = regions(run.image(), BankId::B);
        let intact_seal = bank::decode_seal(seal).is_ok();
        let gone_header = bank::decode_header(header).is_err();
        if !(intact_seal && gone_header) {
            continue;
        }
        half_erased += 1;
        assert_eq!(
            bank::sealed_generation(header, seal),
            None,
            "at {:?}: a bank with no header reported a generation",
            run.injection()
        );
        assert_eq!(
            authority(run.image()),
            Authority::Bank {
                id: BankId::A,
                generation: CURRENT
            }
        );
    }
    assert!(
        half_erased > 0,
        "no crash point left a seal standing over an erased header, so the hazard this test \
         exists for was never reached"
    );
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
        let authority = authority(run.image());
        if authority != Authority::Unsealed || !after_first_commit(run) {
            continue;
        }
        let history = recovered(run.image(), NEW);
        assert_eq!(
            verify_oracle(
                run.ledger(),
                &Recovery::new(&history).authoritative_banks(authoritative_banks(authority))
            ),
            Err(Breach::NoAuthoritativeBank),
            "at {:?}: a device with no authoritative bank was accepted",
            run.injection()
        );
        caught += 1;
    }
    assert!(
        caught > 0,
        "clearing both banks never left the device with nothing to boot from, so the mutant \
         proves nothing"
    );
}

#[test]
fn a_swap_that_forgets_to_bump_the_generation_leaves_two_authorities() {
    // The other way a two-bank device loses its authority: not by having none, but by having
    // two that no longer disagree. §02 decision 7 makes a new run authoritative on its
    // *generation* seal, and a seal that repeats a generation seals nothing.
    //
    // The selection here is the real one. Nothing is substituted: `authority` is asked the
    // same question it is asked everywhere else in this file, and on this device it answers
    // two.
    let runs = drive(swap_without_bumping_the_generation);

    let mut caught = 0_usize;
    for run in &runs {
        let authority = authority(run.image());
        let count = authoritative_banks(authority);
        if count < 2 {
            continue;
        }
        let history = recovered(run.image(), CURRENT);
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
        "a swap that reused the old generation never produced two authorities, so the mutant \
         proves nothing"
    );
}

#[test]
fn a_selection_that_ignores_the_seal_finds_two_authorities() {
    // The rule somebody writes before the generation seal exists: "does the bank have a
    // header in it". After any swap has written its payload, both banks answer yes.
    let runs = drive(swap);

    let mut caught = 0_usize;
    for run in &runs {
        let count = BankId::ALL
            .into_iter()
            .filter(|id| bank::decode_header(regions(run.image(), *id).0).is_ok())
            .count();
        if count < 2 {
            continue;
        }
        let history = recovered(run.image(), NEW);
        assert_eq!(
            verify_oracle(
                run.ledger(),
                &Recovery::new(&history).authoritative_banks(count)
            ),
            Err(Breach::AmbiguousAuthority { count }),
            "at {:?}: two header-bearing banks were accepted as two authorities",
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
fn a_seal_that_names_another_banks_header_is_never_authoritative() {
    // The digest comparison, made load-bearing under crash injection.
    //
    // Bank B is whole: its header decodes, its seal decodes, and its generation is the
    // highest on the device. The only thing wrong with it is that the seal names bank A's
    // header. A selection that checked "does the header decode" and "is the seal valid" — the
    // two things every *other* mutant in this file breaks — boots it.
    let runs = drive(swap_sealed_with_the_wrong_header);

    let mut caught = 0_usize;
    for run in &runs {
        let (header, seal) = regions(run.image(), BankId::B);
        let (Ok(_), Ok(decoded)) = (bank::decode_header(header), bank::decode_seal(seal)) else {
            continue;
        };
        if decoded.generation != NEW {
            continue;
        }
        caught += 1;

        // Intact by every measure but the one that matters.
        assert_eq!(
            bank::sealed_generation(header, seal),
            None,
            "at {:?}: a seal naming another bank's header was accepted",
            run.injection()
        );
        assert_eq!(
            authority(run.image()),
            Authority::Bank {
                id: BankId::A,
                generation: CURRENT
            },
            "at {:?}: the device did not fall back to the bank it was booting",
            run.injection()
        );
    }
    assert!(
        caught > 0,
        "no crash point left an intact header under an intact seal naming another header, so \
         the mutant proves nothing"
    );
}

#[test]
fn a_selection_that_ignores_the_header_boots_a_bank_that_was_never_written() {
    // What the seal's header digest is *for*, shown rather than argued.
    //
    // The writer here has one bug: it seals the header it meant to write rather than the one
    // on media, and carries on past a failed program. A crash point inside the header's
    // program therefore leaves a torn header under a valid seal at the highest generation on
    // the device. A reader that asked only "is there a valid seal, and is it the highest"
    // boots that bank and reads a run input that was never written.
    //
    // The real selection refuses it — not by inspecting the header separately, but because
    // the seal names a digest the bytes on media do not compute to.
    let runs = drive(swap_that_seals_whatever_landed);

    let mut caught = 0_usize;
    for run in &runs {
        let (header, seal) = regions(run.image(), BankId::B);
        let Ok(decoded_seal) = bank::decode_seal(seal) else {
            continue;
        };
        if decoded_seal.generation != NEW || bank::decode_header(header).is_ok() {
            continue;
        }
        caught += 1;

        // The seal-blind reader: highest valid seal, header unread.
        let seal_blind = bank::select(
            BankId::ALL
                .map(|id| bank::decode_seal(regions(run.image(), id).1).ok())
                .map(|seal| seal.map(|seal| seal.generation)),
        );
        assert_eq!(
            seal_blind,
            Authority::Bank {
                id: BankId::B,
                generation: NEW
            },
            "at {:?}: the seal-blind reader did not boot the torn bank, so the mutant proves \
             nothing",
            run.injection()
        );

        // And the real one does not.
        assert_eq!(
            authority(run.image()),
            Authority::Bank {
                id: BankId::A,
                generation: CURRENT
            },
            "at {:?}: the real selection booted a bank whose header was never written",
            run.injection()
        );
    }
    assert!(
        caught > 0,
        "no crash point left a valid seal over a torn header, so the mutant proves nothing"
    );
}
