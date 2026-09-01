//! The resource budgets are a contract, so they are tested like one.
//!
//! These are integration tests rather than unit tests so that the numbers are exercised
//! through the crate's public surface, which is the surface `xtask` and the size probe
//! read them through.

use waymaker_core::budget;

#[test]
fn the_budgets_are_the_numbers_from_the_design_document() {
    assert_eq!(budget::RUNTIME_RAM_BYTES, 768);
    assert_eq!(budget::SCRATCH_PAGE_BYTES, 512);
    assert_eq!(budget::KERNEL_STATE_BYTES, 128);
    assert_eq!(budget::INCREMENTAL_CODE_FLASH_BYTES, 8 * 1024);
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
fn a_type_size_records_the_size_of_the_type_it_names() {
    let entry = budget::TypeSize::of::<u32>("u32");
    assert_eq!(entry.name, "u32");
    assert_eq!(entry.size, 4);
}

#[test]
fn the_kernel_state_registry_totals_its_entries() {
    let total: usize = budget::KERNEL_STATE_TYPES
        .iter()
        .map(|entry| entry.size)
        .sum();
    assert_eq!(total, budget::KERNEL_STATE_TOTAL_BYTES);
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
// error, which `xtask`'s `kernel-state-assertion` test proves by trying to build one.
waymaker_core::assert_kernel_state_size!([u8; 128]);
waymaker_core::assert_kernel_state_size!(u64, 8);
