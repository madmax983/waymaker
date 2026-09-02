//! The resource budgets from design document §04, as numbers the compiler can check.
//!
//! "Small" is four independent budgets, and the design document is explicit that they are
//! gates rather than claims. Two things enforce them, and they read the same constants:
//!
//! * the `const` assertions in this module, which fail the build for
//!   `thumbv6m-none-eabi` — the target the budgets are stated for — the moment kernel
//!   state outgrows [`KERNEL_STATE_BYTES`];
//! * `cargo xtask size`, which links the size probe once per feature and measures the
//!   section deltas against a firmware that links nothing from Waymaker.
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
/// That doctest proves the path resolves; it cannot prove the expansion is hygienic,
/// because a doctest compiles as an ordinary crate with nothing shadowed. The leading `::`
/// on `::core::mem::size_of` below is what makes it hygienic: an unqualified `core`
/// resolves at the *call site*, so a firmware crate with a module of its own by that name —
/// or one that renamed a dependency to it — would be told that `mem` could not be found,
/// by a macro it did not write. This is the one item of this crate's surface that
/// downstream firmware touches, and the leading `::` is not optional in it.
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
            ::core::mem::size_of::<$ty>() <= $limit,
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
        pub const KERNEL_STATE_TYPES: &[$crate::budget::TypeSize] = &[
            $($crate::budget::TypeSize::of::<$ty>(stringify!($ty))),*
        ];

        /// The kernel's live state in bytes: the sum of [`KERNEL_STATE_TYPES`].
        ///
        /// The budget applies to the sum rather than to any one type, so the registry
        /// names types that are independently live rather than types that contain one
        /// another.
        pub const KERNEL_STATE_TOTAL_BYTES: usize = 0 $(+ ::core::mem::size_of::<$ty>())*;

        $($crate::assert_kernel_state_size!($ty);)*

        const _: () = assert!(
            KERNEL_STATE_TOTAL_BYTES <= $crate::budget::KERNEL_STATE_BYTES,
            "the kernel's live state exceeds the budget in design document \u{a7}04; \
             run `cargo xtask size` for the measured figure, which names it",
        );
    };
}

// The replay machine is live for the whole of a run, so it is the kernel's state and is
// charged for here. It *contains* the replay cursor, which contains the effect id
// allocator, which is why neither has a row of its own: this total sums types that are
// independently live, so registering a container beside its contents would spend the same
// bytes twice against a 128 B budget. That fold is the instruction the previous version of
// this comment left for whoever added the cursor, and then the machine; it is recorded in
// ADR 0008 and ADR 0009, and the same rule applies to whatever contains the machine next —
// replace the entry, never add beside it.
//
// The record view is live while the machine resolves one record against what the workflow
// asks for next, so it is charged here too. It holds a fat pointer into the caller's page
// rather than the bytes themselves, which is why its size is target-dependent and is
// budgeted through this registry rather than pinned to a literal beside its declaration.
// The context joins them at rung 0.4.
kernel_state_types! {
    crate::transition::ReplayMachine,
    crate::record::RecordRef<'static>,
}

/// The assertion macro, documented beside the constants it is stated against.
///
/// `#[macro_export]` puts it at the crate root, which is where a caller writes it. This
/// re-export is so that a reader following the module documentation, the README or ADR
/// 0002 to `waymaker_core::budget` finds it there too.
#[doc(inline)]
pub use crate::assert_kernel_state_size;

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

    /// A registry of known contents, held still so the macro's arithmetic can be checked.
    ///
    /// The macro emits `pub` items, which are unreachable from outside a test module; that
    /// is what a fixture is, so the lint is allowed here rather than the macro weakened.
    ///
    /// This is what exercises what `kernel_state_types!` actually builds. Two entries whose
    /// names and sizes this file chose are what let the tests below state an exact total,
    /// so what is proved is the macro's arithmetic rather than whatever happens to be
    /// registered in the real list on the day — which is free to grow without rewriting a
    /// test that was never about it.
    #[allow(
        unreachable_pub,
        reason = "a test-only registry is reachable from its tests"
    )]
    mod populated {
        kernel_state_types! {
            [u8; 8],
            u32,
        }
    }

    #[test]
    fn the_registry_names_each_type_and_its_size() {
        // Compared by iterator rather than collected: this crate is `no_std`, and putting
        // the standard library back to hold a two-element list is what the gate's
        // `extern crate std` rule exists to stop.
        assert!(
            populated::KERNEL_STATE_TYPES
                .iter()
                .map(|entry| (entry.name, entry.size))
                .eq([("[u8; 8]", 8), ("u32", 4)])
        );
    }

    #[test]
    fn the_registry_total_is_the_sum_of_its_entries() {
        assert_eq!(populated::KERNEL_STATE_TOTAL_BYTES, 12);
        let summed: usize = populated::KERNEL_STATE_TYPES
            .iter()
            .map(|entry| entry.size)
            .sum();
        assert_eq!(summed, populated::KERNEL_STATE_TOTAL_BYTES);
    }

    #[test]
    fn an_empty_registry_totals_nothing() {
        #[allow(
            unreachable_pub,
            reason = "a test-only registry is reachable from its tests"
        )]
        mod empty {
            kernel_state_types! {}
        }
        assert!(empty::KERNEL_STATE_TYPES.is_empty());
        assert_eq!(empty::KERNEL_STATE_TOTAL_BYTES, 0);
    }
}
