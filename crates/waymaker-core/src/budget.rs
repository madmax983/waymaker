//! The resource budgets from design document §04, as numbers the compiler can check.
//!
//! "Small" is four independent budgets, and the design document is explicit that they are
//! gates rather than claims. Two things enforce them, and they read the same constants:
//!
//! * the `const` assertions in this module, which fail the build for
//!   `thumbv6m-none-eabi` — the target the budgets are stated for — the moment kernel
//!   state outgrows [`KERNEL_STATE_BYTES`];
//! * `cargo xtask size`, which links the size probe once per feature combination and
//!   measures the section deltas against a firmware that links nothing from Waymaker.
//!
//! Neither transcribes the numbers: the gate depends on this crate, so a budget changed
//! here is a budget changed everywhere.
//!
//! # Registering a kernel state type
//!
//! Add it to the `kernel_state_types!` list below. That single list produces the
//! registry [`KERNEL_STATE_TYPES`], the total [`KERNEL_STATE_TOTAL_BYTES`], and a `const`
//! assertion for the type and for the new total — so a type cannot be added to the
//! kernel's live state without being budgeted, and cannot be budgeted without appearing
//! in the report.

/// Runtime RAM budget in bytes, stated with a [`SCRATCH_PAGE_BYTES`] scratch page.
///
/// Design document §04: "Cursor, context, record header, and storage scratch. Excludes
/// user workflow future and hardware-driver buffers."
pub const RUNTIME_RAM_BYTES: usize = 768;

/// The storage scratch page [`RUNTIME_RAM_BYTES`] is stated with.
///
/// The page is caller-owned: the engine borrows it rather than holding it, so it does not
/// appear in the engine's own `.bss`. [`ENGINE_RAM_BYTES`] is what is left for the parts
/// that do.
pub const SCRATCH_PAGE_BYTES: usize = 512;

/// Runtime RAM the engine may own itself, once the caller's scratch page is accounted for.
///
/// This is the number `cargo xtask size` gates the measured `.data + .bss` delta against,
/// because the probe firmware deliberately does not allocate a scratch page: a budget that
/// counted the caller's buffer would be measuring the caller.
pub const ENGINE_RAM_BYTES: usize = RUNTIME_RAM_BYTES - SCRATCH_PAGE_BYTES;

/// Kernel state budget in bytes: `waymaker-core` state only, no page buffer.
pub const KERNEL_STATE_BYTES: usize = 128;

/// Incremental code-flash budget in bytes for the kernel plus the flash adapter.
///
/// Design document §04: "Measured on `thumbv6m-none-eabi` with release-size settings.
/// This is a gate, not an unverified claim."
pub const INCREMENTAL_CODE_FLASH_BYTES: usize = 8 * 1024;

/// One type that is part of the kernel's live state, and the space it occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeSize {
    /// The type, as written in the `kernel_state_types!` list.
    pub name: &'static str,
    /// `size_of` for whichever target this crate was compiled for.
    ///
    /// The budget is stated for `thumbv6m-none-eabi`, so the authoritative evaluation is
    /// the `const` assertion in a firmware build. A host build of the same constant can
    /// differ wherever a type contains a pointer, which is why `cargo xtask size` labels
    /// the figure it prints with the target it was compiled for.
    pub size: usize,
}

impl TypeSize {
    /// Records `size_of::<T>()` under `name`.
    #[must_use]
    pub const fn of<T>(name: &'static str) -> Self {
        Self {
            name,
            size: core::mem::size_of::<T>(),
        }
    }
}

/// Fails the build when `$ty` does not fit the kernel-state budget.
///
/// The one-argument form uses [`KERNEL_STATE_BYTES`]; the two-argument form takes a
/// tighter limit, for a type that is only part of the state.
///
/// ```
/// waymaker_core::assert_kernel_state_size!(u64);
/// waymaker_core::assert_kernel_state_size!(u64, 8);
/// ```
///
/// The failure is a compile error rather than a report, which is the point: a regression
/// that only shows up in a report is a regression somebody has to be looking for.
#[macro_export]
macro_rules! assert_kernel_state_size {
    ($ty:ty) => {
        $crate::assert_kernel_state_size!($ty, $crate::budget::KERNEL_STATE_BYTES);
    };
    ($ty:ty, $limit:expr) => {
        const _: () = assert!(
            core::mem::size_of::<$ty>() <= $limit,
            concat!(
                "`",
                stringify!($ty),
                "` does not fit the kernel-state budget; see design document \u{a7}04 and \
                 run `cargo xtask size` for the measured figure"
            ),
        );
    };
}

/// Declares the kernel's live state: the registry, the total, and the assertions on both.
///
/// Written as a macro so that the list is written once. A hand-maintained registry beside
/// hand-written assertions is two lists that drift, and the drift is silent in the
/// direction that matters — a type asserted but not registered is a type missing from the
/// size report.
macro_rules! kernel_state_types {
    ($($ty:ty),* $(,)?) => {
        /// Every type that is part of the kernel's live state, with its size.
        ///
        /// Empty at rung 0.0: the record codec, cursor, and transition rules arrive with
        /// rung 0.1, and an empty registry that totals zero is the honest report until
        /// they do.
        pub const KERNEL_STATE_TYPES: &[TypeSize] = &[
            $(TypeSize::of::<$ty>(stringify!($ty))),*
        ];

        /// The kernel's live state in bytes: the sum of [`KERNEL_STATE_TYPES`].
        ///
        /// The budget applies to the sum rather than to any one type, so the registry
        /// names types that are independently live rather than types that contain one
        /// another.
        pub const KERNEL_STATE_TOTAL_BYTES: usize = 0 $(+ core::mem::size_of::<$ty>())*;

        $($crate::assert_kernel_state_size!($ty);)*

        const _: () = assert!(
            KERNEL_STATE_TOTAL_BYTES <= KERNEL_STATE_BYTES,
            "the kernel's live state exceeds the budget in design document \u{a7}04; \
             run `cargo xtask size` for the measured figure",
        );
    };
}

kernel_state_types! {
    // Rung 0.1 adds the replay cursor, the record header view, and the context.
}

const _: () = assert!(
    SCRATCH_PAGE_BYTES < RUNTIME_RAM_BYTES,
    "the scratch page must leave the engine some runtime RAM",
);
const _: () = assert!(
    KERNEL_STATE_BYTES <= ENGINE_RAM_BYTES,
    "kernel state is part of runtime RAM, so its budget cannot exceed what is left after \
     the scratch page",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_size_records_the_name_it_was_given() {
        let entry = TypeSize::of::<[u8; 16]>("[u8; 16]");
        assert_eq!(entry.name, "[u8; 16]");
        assert_eq!(entry.size, 16);
    }

    #[test]
    fn a_zero_sized_type_costs_nothing() {
        assert_eq!(TypeSize::of::<()>("()").size, 0);
    }

    #[test]
    fn the_registry_and_the_total_agree() {
        let summed: usize = KERNEL_STATE_TYPES.iter().map(|entry| entry.size).sum();
        assert_eq!(summed, KERNEL_STATE_TOTAL_BYTES);
    }
}
