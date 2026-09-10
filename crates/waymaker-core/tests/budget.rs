//! The resource budgets are a contract, so they are tested like one.
//!
//! These are integration tests rather than unit tests so that the numbers are exercised
//! through the crate's public surface, which is the surface `xtask` and the size probe
//! read them through. What the macro *builds* is tested inside `budget.rs` instead, over a
//! fixture registry of known contents: the real list is free to grow as each rung adds live
//! state, so a test that pinned its exact total would be a test about the roadmap.

use waymaker_core::budget;

#[test]
fn the_budgets_are_the_numbers_from_the_design_document() {
    assert_eq!(budget::RUNTIME_RAM_BYTES, 768);
    assert_eq!(budget::SCRATCH_PAGE_BYTES, 512);
    assert_eq!(budget::KERNEL_STATE_BYTES, 128);
    // The one budget that is *not* the design document's number, and the assertion says so
    // rather than being quietly relaxed. §04 states 8 KiB and labels its column "v0.1
    // target"; ADR 0017 raised the gate to 16 KiB for rung 0.2's two-bank lifecycle and ADR
    // 0020 to 18 KiB for §10's capacity reserve, neither of which §04's row scopes. ADR 0020
    // argued that the next change should be issue #72 correcting what the figure measures
    // rather than a third raise, and ADR 0029 is that correction: the gate now charges the
    // layers rather than the linked image, and the ceiling came down to 12 KiB against a
    // measured 10852 B. ADR 0036 then takes it to 13 KiB for issue #40's versioning, which
    // costs 672 B of layers — the first raise argued from a corrected figure. Changing it
    // again is changing this line, which is the point.
    assert_eq!(budget::INCREMENTAL_CODE_FLASH_BYTES, 13 * 1024);
    // A relation between two constants is a compile-time fact, so it is asserted at compile
    // time — see the note below the next test.
    const {
        assert!(
            budget::INCREMENTAL_CODE_FLASH_BYTES > 8 * 1024,
            "a raise that went the other way is a budget nobody meant to tighten silently"
        );
    }
}

// A relation between two constants is a compile-time fact, so it is asserted at compile
// time. Written as `const` blocks rather than as runtime assertions because a runtime
// assertion over two constants is a tautology the optimiser deletes, and because a budget
// table that contradicts itself should fail to build rather than fail a test run.
#[test]
fn the_scratch_page_fits_inside_the_runtime_ram_budget() {
    const {
        assert!(budget::SCRATCH_PAGE_BYTES < budget::RUNTIME_RAM_BYTES);
        assert!(budget::ENGINE_RAM_BYTES == budget::RUNTIME_RAM_BYTES - budget::SCRATCH_PAGE_BYTES);
    }
}

#[test]
fn kernel_state_fits_the_runtime_ram_budget_it_is_part_of() {
    const {
        assert!(budget::KERNEL_STATE_BYTES <= budget::ENGINE_RAM_BYTES);
    }
}

#[test]
fn the_kernel_state_registry_is_within_budget() {
    // Summed from the registry rather than read from the constant: this checks that what
    // the report will print fits, which is the thing that can drift.
    let total: usize = budget::KERNEL_STATE_TYPES
        .iter()
        .map(|entry| entry.size)
        .sum();
    assert!(
        total <= budget::KERNEL_STATE_BYTES,
        "the registry totals {total} B"
    );
    for entry in budget::KERNEL_STATE_TYPES {
        assert!(
            entry.size <= budget::KERNEL_STATE_BYTES,
            "{} is {} bytes",
            entry.name,
            entry.size
        );
    }
}

#[test]
fn the_registry_names_each_type_once() {
    let mut names: Vec<&str> = budget::KERNEL_STATE_TYPES
        .iter()
        .map(|entry| entry.name)
        .collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), before, "a type is registered twice");
}

// A type that fits passes the public assertion macro. A type that does not is a compile
// error, which `a_kernel_state_type_over_budget_fails_to_compile` in
// `xtask/tests/size_budgets.rs` proves by building a crate that must not build.
waymaker_core::assert_kernel_state_size!([u8; 128]);
waymaker_core::assert_kernel_state_size!(u64, 8);

#[test]
fn the_context_gets_what_the_kernel_state_leaves_of_engine_ram() {
    // Design document §04 lists runtime RAM as "cursor, context, record header, and storage
    // scratch". The cursor and the record header are in the kernel-state registry and the
    // scratch page is the caller's; the context is the remaining term, so it gets the
    // remaining share. A partition rather than two independent numbers, because two
    // independent numbers can sum to more than the budget they are drawn from.
    const {
        assert!(budget::KERNEL_STATE_BYTES + budget::CONTEXT_RAM_BYTES == budget::ENGINE_RAM_BYTES);
    }
}

#[test]
fn the_facade_ceiling_is_the_engine_ceiling_and_room_for_the_facade() {
    assert_eq!(budget::FACADE_CODE_FLASH_BYTES, 14 * 1024);
    // The façade image strictly contains the engine one, so a ceiling below the engine's
    // would be a budget no build could satisfy and every build would blame on the façade.
    const {
        assert!(budget::FACADE_CODE_FLASH_BYTES >= budget::INCREMENTAL_CODE_FLASH_BYTES);
    }
}
