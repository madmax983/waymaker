//! A live writer answers `Interruption::Failure` by swapping banks, with no power loss at all.
//!
//! Issue [#67](https://github.com/madmax983/waymaker/issues/67)'s third gap: "an append-only
//! journal with a half-written record in it cannot advance", and the obvious firmware response
//! — retrying elsewhere — had no place in the model. It needed no new transition to describe,
//! once records carried a bank: `Journal::begin_seal` and `Journal::begin_erase` never
//! depended on the *other* bank's record state, so a device that hit a torn record could
//! already seal the *other*, blank bank and carry on declaring fresh records there — abandoning
//! the torn tail rather than being stuck behind it. This file is the proof that the reachable
//! state the issue asked for is really reachable, and that recovery answers it the way §10's
//! swap says it should.
//!
//! This is compaction in exactly the sense design document §10 and the swap discipline
//! (`waymaker_flash::bank`, issue #22) already use the word: the run that owned the torn
//! record is not repaired, it is replaced. What issue #95 already records — that the replaced
//! run's own identity is not carried over — is unaffected; nothing here claims otherwise.

use waymaker_spec::explore::explore;
use waymaker_spec::model::{Bound, Guards, OnMedia};
use waymaker_spec::reader::{Reader, Specified};

const CEILING: usize = 200_000;

fn proof_space() -> waymaker_spec::explore::Explored {
    match explore(Bound::PROOF, Guards::ENFORCED, CEILING) {
        Ok(explored) => explored,
        Err(error) => unreachable!("{error}"),
    }
}

#[test]
fn a_live_device_can_swap_banks_behind_a_torn_record_with_no_power_loss() {
    // `state.powered()` is the tell: `Transition::Tear` always leaves the device unpowered,
    // so a torn record on a device that is still powered can only have arrived through
    // `Transition::FailedProgram` — design document §12's "a call may fail" while the
    // controller is still running. Finding such a state, sealed at all, is the reachability
    // half of issue #67's third gap.
    let explored = proof_space();
    let compacted = explored.states().iter().find_map(|state| {
        if !(state.powered() && state.authoritative().len() == 1) {
            return None;
        }
        let authoritative = state.authoritative()[0];
        let torn = state
            .records()
            .iter()
            .find(|record| record.media == OnMedia::Partial && record.bank != authoritative)?;
        Some((state, authoritative, torn))
    });
    let (state, authoritative, torn) = compacted.expect(
        "no reachable state has a torn record abandoned in a retired bank on a still-powered \
         device — the live compaction this file exists to prove has no witness",
    );
    assert_ne!(
        torn.bank, authoritative,
        "the torn record is in the bank recovery would boot from, which is not compaction — \
         it is a live device about to strand itself"
    );

    // Recovery agrees: nothing from the torn record's bank appears, whole or not.
    let recovered = Specified.recover(state);
    assert!(
        !recovered.contains(&torn.id),
        "recovery produced the torn record {torn:?}"
    );
    for record in state
        .records()
        .iter()
        .filter(|record| record.bank == torn.bank)
    {
        assert!(
            !recovered.contains(&record.id),
            "recovery produced record {record:?} from the bank compaction abandoned"
        );
    }
}

#[test]
fn a_seal_on_the_surviving_bank_is_legal_with_a_torn_record_still_on_the_other_one() {
    // Compaction needed no `Transition::Compact` of its own because `Journal::begin_seal`
    // never looked at the *other* bank's records — this is that fact, demonstrated rather
    // than argued: a `BeginSeal` on the blank bank stays legal even from a state where a live,
    // still-powered device has a torn record sitting in the bank it is about to abandon.
    let explored = proof_space();
    let witness = explored.states().iter().find_map(|state| {
        if !(state.powered() && state.has_torn_record()) {
            return None;
        }
        let torn_bank = state
            .records()
            .iter()
            .find(|record| record.media == OnMedia::Partial)
            .expect("has_torn_record()")
            .bank;
        let blank = torn_bank.other();
        if state.bank(blank) != waymaker_spec::model::Bank::Erased {
            return None;
        }
        let sealed = state
            .step(
                waymaker_spec::model::Transition::BeginSeal(blank),
                Guards::ENFORCED,
                Bound::PROOF,
            )
            .expect("sealing the blank bank does not consult the torn record elsewhere");
        Some((state.bank(torn_bank), sealed.bank(torn_bank)))
    });
    let (before, after) = witness.expect(
        "no reachable, still-powered, torn-record state had a blank bank to seal — this test \
         is about nothing",
    );
    assert_eq!(before, after, "sealing changed the other bank");
}
