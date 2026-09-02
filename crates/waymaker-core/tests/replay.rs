//! The streaming replay cursor, tested through the surface a driver sees.
//!
//! Design document §02 decision 2, §06 Cold-start replay, §08 Replay and determinism. A
//! cursor advances through history in workflow order; there is no `Journal::get(id)` and no
//! in-memory event index, which is what makes replay constant-memory and free of random
//! reads.
//!
//! These tests are written against the two properties that are cheap to claim and easy to
//! lose:
//!
//! * the cursor keeps nothing from the caller's scratch page, so one page is enough however
//!   long history is — proved by overwriting the page between every record;
//! * the cursor's state does not grow with history — proved by replaying five history
//!   lengths spanning five orders of magnitude and comparing the live bytes.
//!
//! What is deliberately *not* here is the transition table of §08: whether a workflow's
//! request matches what history recorded is issue #15's, and this cursor knows nothing
//! about a workflow. It validates history against itself.

use waymaker_core::budget::SCRATCH_PAGE_BYTES;
use waymaker_core::replay::{PendingEffect, Position, ReplayCursor, Step};
use waymaker_core::{ActivityKind, EffectId, EffectSeq, KernelError, RecordRef, RunId};

/// The run every test below replays, unless it says otherwise.
const RUN: RunId = RunId(0x0102_0304_0506_0708);

/// The activity kind the synthetic histories schedule.
const KIND: ActivityKind = ActivityKind(7);

/// A cursor that has consumed `RunStarted` and nothing else.
fn started() -> ReplayCursor {
    let mut cursor = ReplayCursor::new(RUN);
    let step = cursor.advance(RecordRef::RunStarted {
        workflow_kind: 1,
        workflow_version: 2,
        input: b"input",
    });
    assert!(step.is_ok(), "a fresh cursor must accept `RunStarted`");
    cursor
}

/// `RecordRef::EffectScheduled` for `seq`, with a digest a test can recognise.
const fn schedule(seq: u32) -> RecordRef<'static> {
    RecordRef::EffectScheduled {
        seq: EffectSeq(seq),
        kind: KIND,
        input_len: 4,
        input_crc: 0xDEAD_BEEF,
    }
}

/// The effect `schedule(seq)` commits, as the cursor reports it.
const fn pending(seq: u32) -> PendingEffect {
    PendingEffect {
        id: EffectId {
            run: RUN,
            seq: EffectSeq(seq),
        },
        kind: KIND,
        input_len: 4,
        input_crc: 0xDEAD_BEEF,
    }
}

// ---------------------------------------------------------------------------
// Position, and the order history has to arrive in.
// ---------------------------------------------------------------------------

#[test]
fn a_fresh_cursor_stands_before_the_run() {
    let cursor = ReplayCursor::new(RUN);
    assert_eq!(cursor.position(), Position::BeforeRun);
    assert_eq!(cursor.run(), RUN);
    assert_eq!(cursor.pending(), None);
    assert_eq!(cursor.next_seq(), Some(EffectSeq::FIRST));
    assert!(!cursor.position().is_terminal());
}

#[test]
fn the_first_record_must_be_the_one_that_starts_the_run() {
    let mut cursor = ReplayCursor::new(RUN);
    assert_eq!(
        cursor.advance(schedule(0)),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        cursor.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
}

#[test]
fn run_started_hands_back_the_run_input_and_begins_replay() {
    let mut cursor = ReplayCursor::new(RUN);
    let step = cursor.advance(RecordRef::RunStarted {
        workflow_kind: 0x1234,
        workflow_version: 9,
        input: b"input",
    });
    assert_eq!(
        step,
        Ok(Step::RunStarted {
            workflow_kind: 0x1234,
            workflow_version: 9,
            input: b"input",
        })
    );
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.next_seq(), Some(EffectSeq::FIRST));
}

#[test]
fn a_run_can_only_start_once() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 2,
            input: b"again",
        }),
        Err(KernelError::MalformedHistory)
    );
}

// ---------------------------------------------------------------------------
// Effects: a schedule, then exactly one outcome for it.
// ---------------------------------------------------------------------------

#[test]
fn a_schedule_becomes_the_first_unresolved_effect() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(schedule(0)),
        Ok(Step::EffectScheduled(pending(0)))
    );
    assert_eq!(cursor.position(), Position::AwaitingOutcome);
    assert_eq!(cursor.pending(), Some(pending(0)));
    // The identity is spent: a redelivery reuses it rather than minting a fresh one.
    assert_eq!(cursor.next_seq(), Some(EffectSeq(1)));
}

#[test]
fn a_completion_resolves_the_effect_it_names() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"result",
        }),
        Ok(Step::EffectCompleted {
            id: EffectId {
                run: RUN,
                seq: EffectSeq(0)
            },
            result: b"result",
        })
    );
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.pending(), None);
}

#[test]
fn a_failure_resolves_the_effect_it_names() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(RecordRef::EffectFailed {
            seq: EffectSeq(0),
            error: b"nope",
        }),
        Ok(Step::EffectFailed {
            id: EffectId {
                run: RUN,
                seq: EffectSeq(0)
            },
            error: b"nope",
        })
    );
    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.pending(), None);
}

#[test]
fn an_outcome_with_no_schedule_is_refused() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"",
        }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn an_outcome_for_another_effect_is_refused() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(1),
            result: b"",
        }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_second_outcome_for_a_resolved_effect_is_refused() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    let _ = cursor.advance(RecordRef::EffectCompleted {
        seq: EffectSeq(0),
        result: b"",
    });
    assert_eq!(
        cursor.advance(RecordRef::EffectFailed {
            seq: EffectSeq(0),
            error: b"",
        }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_schedule_while_an_effect_is_unresolved_is_refused() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(schedule(1)),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_schedule_that_skips_a_sequence_is_refused() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(schedule(1)),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_schedule_that_reuses_a_sequence_is_refused() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    let _ = cursor.advance(RecordRef::EffectCompleted {
        seq: EffectSeq(0),
        result: b"",
    });
    assert_eq!(
        cursor.advance(schedule(0)),
        Err(KernelError::MalformedHistory)
    );
}

// ---------------------------------------------------------------------------
// Terminal records.
// ---------------------------------------------------------------------------

#[test]
fn a_run_completed_record_ends_the_run() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(RecordRef::RunCompleted { result: b"done" }),
        Ok(Step::RunCompleted { result: b"done" })
    );
    assert_eq!(cursor.position(), Position::RunCompleted);
    assert!(cursor.position().is_terminal());
}

#[test]
fn a_run_failed_record_ends_the_run() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(RecordRef::RunFailed { error: b"bad" }),
        Ok(Step::RunFailed { error: b"bad" })
    );
    assert_eq!(cursor.position(), Position::RunFailed);
    assert!(cursor.position().is_terminal());
}

#[test]
fn nothing_may_follow_a_terminal_record() {
    let mut cursor = started();
    let _ = cursor.advance(RecordRef::RunCompleted { result: b"" });
    assert_eq!(
        cursor.advance(schedule(0)),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_run_cannot_end_with_an_effect_unresolved() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.advance(RecordRef::RunCompleted { result: b"" }),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_run_cannot_end_before_it_starts() {
    let mut cursor = ReplayCursor::new(RUN);
    assert_eq!(
        cursor.advance(RecordRef::RunFailed { error: b"" }),
        Err(KernelError::MalformedHistory)
    );
}

// ---------------------------------------------------------------------------
// Recovery stops at the first bad record, and stays stopped.
// ---------------------------------------------------------------------------

#[test]
fn the_cursor_stays_halted_at_the_record_that_stopped_it() {
    let mut cursor = started();
    assert_eq!(
        cursor.advance(schedule(4)),
        Err(KernelError::MalformedHistory)
    );
    // A record that would have been perfectly legal a moment ago is refused too: §14's
    // "recovery exposes only a legal prefix of committed records" is not a best-effort.
    assert_eq!(
        cursor.advance(schedule(0)),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(
        cursor.position(),
        Position::Halted(KernelError::MalformedHistory)
    );
    assert_eq!(cursor.pending(), None);
}

// ---------------------------------------------------------------------------
// Minting the identity for the work that comes after the committed prefix.
// ---------------------------------------------------------------------------

#[test]
fn the_next_effect_continues_after_the_committed_prefix() {
    let mut cursor = started();
    for seq in 0..3 {
        let _ = cursor.advance(schedule(seq));
        let _ = cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(seq),
            result: b"",
        });
    }
    assert_eq!(cursor.next_seq(), Some(EffectSeq(3)));
    assert_eq!(
        cursor.next_effect_id(),
        Ok(EffectId {
            run: RUN,
            seq: EffectSeq(3)
        })
    );
    // Asking did not spend it: only a committed schedule record does that.
    assert_eq!(cursor.next_seq(), Some(EffectSeq(3)));
    let _ = cursor.advance(schedule(3));
    assert_eq!(cursor.next_seq(), Some(EffectSeq(4)));
}

#[test]
fn a_new_effect_cannot_replace_an_unresolved_one() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(
        cursor.next_effect_id(),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn no_effect_follows_a_terminal_record() {
    let mut cursor = started();
    let _ = cursor.advance(RecordRef::RunCompleted { result: b"" });
    assert_eq!(
        cursor.next_effect_id(),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn no_effect_is_minted_before_the_run_starts() {
    let cursor = ReplayCursor::new(RUN);
    assert_eq!(
        cursor.next_effect_id(),
        Err(KernelError::NondeterministicWorkflow)
    );
}

#[test]
fn a_halted_cursor_mints_nothing() {
    let mut cursor = started();
    let _ = cursor.advance(schedule(9));
    assert_eq!(cursor.next_effect_id(), Err(KernelError::MalformedHistory));
    // And the refused record moved nothing: `next_seq` still reports what history
    // committed, which is nothing.
    assert_eq!(cursor.next_seq(), Some(EffectSeq::FIRST));
}

// ---------------------------------------------------------------------------
// One caller-owned scratch page, and constant memory.
// ---------------------------------------------------------------------------

/// The cursor's exact size, pinned rather than bounded.
///
/// Sixteen bytes of [`EffectIdAllocator`](waymaker_core::EffectIdAllocator) — a `u64` run
/// id and an `Option<EffectSeq>` — and sixteen of state: the twelve-byte schedule the
/// cursor holds an outcome open for, plus a discriminant.
///
/// Pinned because a bound is what a page buffer hides behind. `size_of::<ReplayCursor>() <
/// SCRATCH_PAGE_BYTES` leaves 480 bytes of headroom, and `<= KERNEL_STATE_BYTES` leaves
/// 96 — so a cursor that grew a 64-byte scratch buffer, which is precisely the thing issue
/// #14 forbids, would satisfy both and every other assertion in this file. An equality
/// notices it. The number is the same on `thumbv6m-none-eabi`, where a `u64` aligns to four
/// rather than eight: the allocator is 16 either way, and `Scheduled` is 12 bytes of
/// four-aligned scalars.
const CURSOR_BYTES: usize = 32;

#[test]
fn the_cursor_is_exactly_the_state_it_declares() {
    assert_eq!(size_of::<ReplayCursor>(), CURSOR_BYTES);
    // Whether it holds a *borrow* is not something a size can see: a type with no lifetime
    // parameter has nowhere to put a non-`'static` one, and that is checked by this file
    // compiling at all — `ReplayCursor` is written without `<'_>` in every line below.
    const {
        assert!(CURSOR_BYTES < SCRATCH_PAGE_BYTES);
    }
}

#[test]
fn the_cursor_is_charged_for_through_the_machine_that_contains_it() {
    // The cursor is live kernel state and has to be budgeted, but not on its own: the
    // replay machine of design document §08 contains it, and `kernel_state_types!` sums
    // types that are *independently* live. Registering both would spend the same 32 B twice
    // against a 128 B budget, so the machine replaced the cursor's row rather than joining
    // it — which is what this checks, in both directions.
    assert!(
        !waymaker_core::budget::KERNEL_STATE_TYPES
            .iter()
            .any(|entry| entry.name.contains("ReplayCursor")),
        "the cursor is registered beside the machine that contains it"
    );
    let registered = waymaker_core::budget::KERNEL_STATE_TYPES
        .iter()
        .find(|entry| entry.name.contains("ReplayMachine"));
    let registered = registered.expect("the machine is live kernel state and must be budgeted");
    // Compared against the pinned constant rather than against `size_of` again: the registry
    // records `size_of` itself, so comparing the two would be comparing an expression with
    // itself.
    assert!(
        registered.size >= CURSOR_BYTES,
        "the machine is {} bytes and contains a {CURSOR_BYTES} byte cursor",
        registered.size
    );
}

/// The bytes a driver holds live while replaying: the cursor, and one scratch page.
///
/// Both terms are compile-time constants, and saying so plainly is the honest form of the
/// claim: `ReplayCursor` has no lifetime parameter and no collection, and `waymaker-core`
/// is `no_std` with an empty dependency table, so there is no allocator a term could hide
/// in. A comparison of this figure across history lengths therefore cannot fail, and is not
/// offered as though it could — [`the_cursor_is_exactly_the_state_it_declares`] is what
/// catches a cursor that grew, and
/// [`replaying_a_million_effects_fits_a_stack_a_microcontroller_would_have`] is what
/// catches one that grows on the stack instead.
const fn live_bytes(cursor: &ReplayCursor, page: &[u8]) -> usize {
    size_of_val(cursor) + size_of_val(page)
}

/// Replays `effects` schedule/completion pairs through one page, poisoning it between
/// records, and returns the live bytes at the end.
///
/// The page is overwritten with `0xA5` before every record is written into it. That bites
/// on the payloads that actually borrow the page — the run input, and every completion
/// result, both of which are compared against what was written. A schedule record has no
/// borrowed field at all (its kind, length and digest are scalars), so the poison before it
/// proves nothing and is there only so that no step of the loop leaves the page intact by
/// accident.
///
/// The history is generated as it is consumed: nothing here holds more than one record and
/// nothing collects, which is what lets the caller run this at a million effects inside a
/// stack a microcontroller would have.
fn replay_generated_history(effects: u32) -> usize {
    let mut page = [0_u8; SCRATCH_PAGE_BYTES];
    let mut cursor = ReplayCursor::new(RUN);

    page.fill(0xA5);
    page[..5].copy_from_slice(b"input");
    let started = cursor.advance(RecordRef::RunStarted {
        workflow_kind: 1,
        workflow_version: 1,
        input: &page[..5],
    });
    assert_eq!(
        started,
        Ok(Step::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"input",
        })
    );

    for seq in 0..effects {
        page.fill(0xA5);
        assert_eq!(
            cursor.advance(schedule(seq)),
            Ok(Step::EffectScheduled(pending(seq)))
        );

        page.fill(0xA5);
        page[..4].copy_from_slice(&seq.to_le_bytes());
        let outcome = cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(seq),
            result: &page[..4],
        });
        assert_eq!(
            outcome,
            Ok(Step::EffectCompleted {
                id: EffectId {
                    run: RUN,
                    seq: EffectSeq(seq)
                },
                result: &seq.to_le_bytes(),
            })
        );
    }

    assert_eq!(cursor.position(), Position::Replaying);
    assert_eq!(cursor.next_seq(), Some(EffectSeq(effects)));
    live_bytes(&cursor, &page)
}

#[test]
fn a_long_history_replays_step_for_step_through_one_page() {
    // Five orders of magnitude. Every step is asserted, so what this catches is a cursor
    // that stops tracking correctly once history gets long — an off-by-one in the sequence
    // check, say. It is deliberately *not* named as a memory measurement: `live_bytes` is
    // two compile-time constants, and the two tests around it are the ones that can fail
    // over memory.
    let baseline = replay_generated_history(0);
    for effects in [1, 2, 1_000, 200_000] {
        assert_eq!(replay_generated_history(effects), baseline);
    }
}

#[test]
fn replaying_a_million_effects_fits_a_stack_a_microcontroller_would_have() {
    // The half of "constant memory regardless of history length" that can actually fail. A
    // cursor that recursed per record, or that grew anything on the stack, overflows a stack
    // this size long before a million effects — and takes the test binary down with it,
    // which is the loud failure a silently-growing replay deserves.
    //
    // 64 KiB is chosen to be a plausible figure for the whole of a Cortex-M0+ part's RAM, so
    // what passes here is a replay that would fit the device design document §04's budgets
    // are stated for. It is still a host thread and not that device: it establishes that the
    // algorithm does not grow, not that a linked firmware fits.
    const STACK_BYTES: usize = 64 * 1024;
    const EFFECTS: u32 = 1_000_000;

    let replay = std::thread::Builder::new()
        .stack_size(STACK_BYTES)
        .spawn(|| replay_generated_history(EFFECTS))
        .expect("the host can spawn a thread");
    let live = replay
        .join()
        .expect("replaying a million effects overflowed a 64 KiB stack");

    assert_eq!(live, replay_generated_history(0));
}

#[test]
fn a_cursor_cannot_be_started_part_way_through_history() {
    // The observable consequence of having no seek. A cursor is a position that begins
    // before the first record and can only move forwards, so handing it the *suffix* of a
    // legal history — which is what any by-id lookup would amount to — is refused rather
    // than resolved. A cursor that could be positioned by effect id would accept this.
    let mut cursor = ReplayCursor::new(RUN);
    assert_eq!(
        cursor.advance(RecordRef::EffectCompleted {
            seq: EffectSeq(4),
            result: b"out",
        }),
        Err(KernelError::MalformedHistory)
    );

    // Nor can a legal history be replayed from its second record.
    let mut second = ReplayCursor::new(RUN);
    assert_eq!(
        second.advance(schedule(0)),
        Err(KernelError::MalformedHistory)
    );
}

#[test]
fn a_forward_only_single_pass_source_is_enough_to_replay() {
    // The whole of §06's step 5 over an iterator that is consumed as it goes: no index, no
    // second pass, nothing to look back at. What this asserts is the resulting position and
    // the identity of the effect left unresolved, both of which change if the cursor
    // mistracks a record.
    let mut cursor = ReplayCursor::new(RUN);
    let history: [RecordRef<'_>; 4] = [
        RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"in",
        },
        schedule(0),
        RecordRef::EffectCompleted {
            seq: EffectSeq(0),
            result: b"out",
        },
        schedule(1),
    ];
    for record in history {
        assert!(cursor.advance(record).is_ok());
    }
    assert_eq!(cursor.pending(), Some(pending(1)));
    assert_eq!(cursor.position(), Position::AwaitingOutcome);
    assert_eq!(cursor.next_seq(), Some(EffectSeq(2)));
}

// ---------------------------------------------------------------------------
// Every state against every record.
// ---------------------------------------------------------------------------

/// The six positions a cursor can be advanced *from*, each with a cursor standing in it.
///
/// `Halted` is reached the way a device reaches it — by being handed a record that could not
/// legally follow — rather than by construction, so the row below is the real thing.
fn every_source_position() -> [(Position, ReplayCursor); 6] {
    let before_run = ReplayCursor::new(RUN);

    let replaying = started();

    let mut awaiting = started();
    let _ = awaiting.advance(schedule(0));

    let mut completed = started();
    let _ = completed.advance(RecordRef::RunCompleted { result: b"" });

    let mut failed = started();
    let _ = failed.advance(RecordRef::RunFailed { error: b"" });

    let mut halted = started();
    let _ = halted.advance(schedule(9));

    [
        (Position::BeforeRun, before_run),
        (Position::Replaying, replaying),
        (Position::AwaitingOutcome, awaiting),
        (Position::RunCompleted, completed),
        (Position::RunFailed, failed),
        (Position::Halted(KernelError::MalformedHistory), halted),
    ]
}

/// One of each record variant, with the sequence that would be legal if any is.
///
/// The sequence depends on where the cursor stands — a schedule is legal at the next
/// unissued sequence, and an outcome at the pending one — so the caller passes the sequence
/// each of those should carry from that position. Anything the position refuses is refused
/// for the *kind*, not because the test picked an unlucky number.
const fn every_record(schedule_seq: u32, outcome_seq: u32) -> [RecordRef<'static>; 6] {
    [
        RecordRef::RunStarted {
            workflow_kind: 1,
            workflow_version: 1,
            input: b"in",
        },
        schedule(schedule_seq),
        RecordRef::EffectCompleted {
            seq: EffectSeq(outcome_seq),
            result: b"out",
        },
        RecordRef::EffectFailed {
            seq: EffectSeq(outcome_seq),
            error: b"bad",
        },
        RecordRef::RunCompleted { result: b"done" },
        RecordRef::RunFailed { error: b"gone" },
    ]
}

#[test]
fn every_position_accepts_exactly_the_records_a_run_could_have_written_next() {
    // All thirty-six cells, stated as a table rather than reached by whichever tests
    // happened to be written. Six of them are legal — the six edges of the transition
    // diagram on `Position` — and the other thirty are histories no execution could have
    // produced. Before this table existed, twenty-one cells were never exercised, and a
    // mutation that let a *failed* run carry on running passed the whole suite.
    //
    // Columns are the record variants in §09's order: RunStarted, EffectScheduled,
    // EffectCompleted, EffectFailed, RunCompleted, RunFailed.
    const LEGAL: [[bool; 6]; 6] = [
        // BeforeRun: only the record that starts the run.
        [true, false, false, false, false, false],
        // Replaying: the next effect, or either terminal record.
        [false, true, false, false, true, true],
        // AwaitingOutcome: only this effect's outcome. A run cannot end mid-effect,
        // because §07 commits an outcome frame before the workflow can observe anything.
        [false, false, true, true, false, false],
        // RunCompleted, RunFailed: terminal. Nothing may follow either of them.
        [false, false, false, false, false, false],
        [false, false, false, false, false, false],
        // Halted: recovery stopped, and stays stopped.
        [false, false, false, false, false, false],
    ];

    for (row, (position, _)) in every_source_position().iter().enumerate() {
        // A schedule is legal only from `Replaying`, at sequence 0 for these fixtures; an
        // outcome only from `AwaitingOutcome`, at the pending sequence, which is also 0.
        let records = every_record(0, 0);
        for (column, record) in records.into_iter().enumerate() {
            // A fresh cursor per cell: a refusal halts, and a halted cursor would answer
            // for every column after it.
            let mut cursor = every_source_position()
                .into_iter()
                .nth(row)
                .map(|(_, cursor)| cursor)
                .expect("the row index came from the same array");
            let outcome = cursor.advance(record);
            assert_eq!(
                outcome.is_ok(),
                LEGAL[row][column],
                "position {position:?} against record {record:?}"
            );
            if !LEGAL[row][column] {
                assert_eq!(outcome, Err(KernelError::MalformedHistory));
                assert_eq!(
                    cursor.position(),
                    Position::Halted(KernelError::MalformedHistory)
                );
            }
        }
    }
}

#[test]
fn an_outcome_naming_another_effect_is_refused_whichever_outcome_it_is() {
    // The guard on the failure arm, which the completion arm's test does not cover: an
    // unguarded arm would resolve the pending effect and hand back a `Step::EffectFailed`
    // carrying an `EffectId` that belongs to no effect at all.
    for record in [
        RecordRef::EffectCompleted {
            seq: EffectSeq(1),
            result: b"out",
        },
        RecordRef::EffectFailed {
            seq: EffectSeq(1),
            error: b"bad",
        },
    ] {
        let mut cursor = started();
        let _ = cursor.advance(schedule(0));
        assert_eq!(cursor.advance(record), Err(KernelError::MalformedHistory));
    }
}

#[test]
fn a_cursor_halted_mid_effect_offers_nothing_to_redeliver() {
    // The documented behaviour that a halted cursor reports no pending effect, tested from
    // the one position where there is something to forget. A run whose history stopped
    // being legal has no effect anybody should redeliver: the identity was minted against a
    // prefix that turned out not to be one.
    let mut cursor = started();
    let _ = cursor.advance(schedule(0));
    assert_eq!(cursor.pending(), Some(pending(0)));

    assert_eq!(
        cursor.advance(RecordRef::RunCompleted { result: b"" }),
        Err(KernelError::MalformedHistory)
    );
    assert_eq!(cursor.pending(), None);
    assert_eq!(cursor.next_effect_id(), Err(KernelError::MalformedHistory));
}
