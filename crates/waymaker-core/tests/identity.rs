//! Effect identity is a contract, so it is tested through the surface callers see.
//!
//! Design document §07: a run's identity is `(RunId, EffectSeq)`, effect sequences never
//! reset within a run, and wraparound is a terminal `IdExhausted` condition rather than
//! silent reuse. Silent reuse is the failure worth a test of its own: a reused sequence
//! makes two different effects indistinguishable in history, which is exactly the
//! confusion replay cannot detect and cannot recover from.
//!
//! These are integration tests rather than unit tests because they exercise the root
//! re-exports — the paths `waymaker-flash` and `waymaker-embassy` will use — and because
//! `std` is available here, so a run's issued sequences can be collected and inspected.

use waymaker_core::budget;
use waymaker_core::{EffectId, EffectIdAllocator, EffectSeq, KernelError, RunId};

#[test]
fn size_and_alignment_of_every_identity_type_is_pinned() {
    // Pinned in a `const` block rather than at runtime: the size of a type is a
    // compile-time fact, and a firmware budget that only fails after a test run is a
    // budget somebody has to remember to look at.
    //
    // `EffectId` is 16 bytes for 12 bytes of data — the `u64` alignment pulls four bytes
    // of tail padding in. That is deliberate rather than tolerated: the wire format is
    // written field by field in `waymaker-flash`, so in-memory layout is free to be
    // whatever the target prefers, and only `EffectSeq` is repeated per record anyway.
    const {
        assert!(core::mem::size_of::<RunId>() == 8);
        assert!(core::mem::align_of::<RunId>() == 8);

        assert!(core::mem::size_of::<EffectSeq>() == 4);
        assert!(core::mem::align_of::<EffectSeq>() == 4);

        assert!(core::mem::size_of::<EffectId>() == 16);
        assert!(core::mem::align_of::<EffectId>() == 8);

        assert!(core::mem::size_of::<EffectIdAllocator>() == 16);
        assert!(core::mem::align_of::<EffectIdAllocator>() == 8);

        assert!(core::mem::size_of::<waymaker_core::ActivityKind>() == 2);
        assert!(core::mem::align_of::<waymaker_core::ActivityKind>() == 2);
    }
}

#[test]
fn the_allocator_is_charged_for_through_the_cursor_that_contains_it() {
    // The allocator is live for the whole of a run, so it is kernel state and has to be
    // charged for — but it is no longer charged for on its own. The replay cursor contains
    // it, and `kernel_state_types!` sums types that are *independently* live, so
    // registering both would spend 16 B of a 128 B budget twice. The fold was anticipated
    // in `budget.rs`; this is the test that it was done as that comment requires, rather
    // than by adding a row beside the old one.
    let cursor = budget::KERNEL_STATE_TYPES
        .iter()
        .find(|entry| entry.name.ends_with("ReplayCursor"))
        .expect("the replay cursor is live kernel state and must be registered");

    assert!(
        !budget::KERNEL_STATE_TYPES
            .iter()
            .any(|entry| entry.name.ends_with("EffectIdAllocator")),
        "the allocator is registered beside the cursor that contains it, so its bytes are \
         counted twice"
    );
    assert!(
        cursor.size >= core::mem::size_of::<EffectIdAllocator>(),
        "the cursor is {} bytes and contains a {} byte allocator",
        cursor.size,
        core::mem::size_of::<EffectIdAllocator>()
    );

    let total: usize = budget::KERNEL_STATE_TYPES
        .iter()
        .map(|entry| entry.size)
        .sum();
    assert!(
        total <= budget::KERNEL_STATE_BYTES,
        "the registry totals {total} B against a {} B budget",
        budget::KERNEL_STATE_BYTES
    );
}

#[test]
fn a_run_that_spends_its_sequence_space_terminates_with_id_exhausted() {
    // The acceptance criterion of issue #12. A 32-bit sequence space cannot be walked in
    // a test, so the run is resumed near its ceiling instead — which is not a shortcut:
    // replay restarts a run from its committed prefix rather than from zero, so
    // `resume` is the path every recovered run already takes.
    const RESUME_FROM: EffectSeq = EffectSeq(u32::MAX - 3);
    const REMAINING: usize = 3;
    const FURTHER_ATTEMPTS: u32 = 4;

    let run = RunId(0x0BAD_F00D_DEAD_BEEF);
    let mut allocator = EffectIdAllocator::resume(run, Some(RESUME_FROM));

    // Allocate until the allocator refuses, bounded so that a wrapping implementation
    // fails this test rather than hanging it for four billion iterations.
    let mut issued: Vec<EffectSeq> = Vec::new();
    let terminal = loop {
        match allocator.allocate() {
            Ok(id) => {
                assert_eq!(id.run, run, "an issued id changed the run it belongs to");
                issued.push(id.seq);
                assert!(
                    issued.len() <= REMAINING,
                    "the allocator issued {} ids from {RESUME_FROM:?}, so it is wrapping \
                     rather than terminating",
                    issued.len()
                );
            }
            Err(error) => break error,
        }
    };

    assert_eq!(terminal, KernelError::IdExhausted);
    assert_eq!(issued.len(), REMAINING);

    // Strictly increasing is the whole property: it rules out a repeat and a reset in one
    // statement, and a reset to `FIRST` is what wraparound looks like from the outside.
    assert!(
        issued
            .iter()
            .zip(issued.iter().skip(1))
            .all(|(before, after)| before < after),
        "issued sequences are not strictly increasing: {issued:?}"
    );
    let mut unique = issued.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        issued.len(),
        "a sequence was issued more than once: {issued:?}"
    );

    assert_eq!(issued.first(), Some(&EffectSeq(u32::MAX - 2)));
    assert_eq!(
        issued.last(),
        Some(&EffectSeq::MAX),
        "the last sequence a run can issue is `EffectSeq::MAX`, and it is allocatable"
    );

    // Terminal means terminal: exhaustion is sticky, not a one-off refusal that the next
    // call forgets. Each further attempt must fail the same way and leave nothing to peek.
    for attempt in 0..FURTHER_ATTEMPTS {
        assert_eq!(
            allocator.allocate(),
            Err(KernelError::IdExhausted),
            "attempt {attempt} after exhaustion did not refuse"
        );
        assert_eq!(
            allocator.peek(),
            None,
            "attempt {attempt} left a sequence to issue, so the space was reused"
        );
        assert_eq!(
            allocator.run(),
            run,
            "attempt {attempt} changed the run the allocator mints for"
        );
    }
}

#[test]
fn debug_of_an_effect_id_names_run_and_seq() {
    // Diagnostics are the only reason these are newtypes rather than a `(u64, u32)`: a
    // log line has to say which half is which without the reader counting fields.
    let rendered = format!(
        "{:?}",
        EffectId {
            run: RunId(7),
            seq: EffectSeq(9),
        }
    );

    assert!(rendered.contains("EffectId"), "{rendered}");
    assert!(rendered.contains("RunId"), "{rendered}");
    assert!(rendered.contains("EffectSeq"), "{rendered}");
    assert!(rendered.contains('7'), "{rendered}");
    assert!(rendered.contains('9'), "{rendered}");
}
