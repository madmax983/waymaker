//! Activity kinds: a number on the dispatch path, a name only in diagnostics.
//!
//! Design document §13: "Activity names are compile-time metadata for logs and
//! diagnostics. The runtime dispatch path uses numeric kinds and does not store names in
//! every record." That is
//! [`numeric-kinds-and-borrowed-bytes`](https://github.com/madmax983/waymaker/blob/main/docs/adr/0003-the-eight-settled-design-decisions.md#numeric-kinds-and-borrowed-bytes)
//! applied to activities: a name in every record is flash spent per effect on something
//! only a developer reads, in a journal whose capacity is the resource the whole design is
//! careful with.
//!
//! # What this module owns
//!
//! [`ActivityKind`], the numeric kind that travels in a record; [`ActivityName`], the
//! rodata pairing of a kind with the identifier it was declared as; and
//! [`activity_kinds!`](crate::activity_kinds), which builds both from one list so that a
//! kind's number and its name cannot drift apart.
//!
//! # What this module must not own
//!
//! Dispatch. Nothing here calls an activity, knows what one does, or holds a table of
//! handlers — that is the façade's job, above two layers. It owns no registry either: a
//! table is a `const` the firmware declares and passes in, so the kernel holds no hidden
//! global state.

/// What kind of activity an effect requests, as the dispatch path sees it.
///
/// `u16` rather than `u8` because §09 and §13 already use `u16` for `workflow_kind` and
/// `workflow_version`, so it is the width this record format speaks in — and because 256
/// activity kinds is a ceiling that could only be raised later by changing a wire format
/// meant to be frozen.
///
/// The field is public so that a wire encoder reaches the integer directly; the kernel
/// grows no accessor for it.
///
/// Not [`Ord`]: the numbers are labels a firmware author picks, so one kind is not less
/// than another, and a derived ordering would let a call site read meaning into an
/// arbitrary numbering. [`Hash`] stays, because a dispatcher keying a table on a kind is
/// asking whether two kinds are the same, which is what [`Eq`] already says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ActivityKind(pub u16);

/// A kind and the name it was declared with, for logs and diagnostics.
///
/// # Invariants
///
/// This is compile-time metadata. Tables of it live in rodata and are never written to a
/// record, so nothing here is part of the wire format and none of it is charged per
/// effect. It holds a fat pointer, so its size is target-dependent — which is why it is
/// deliberately *not* pinned and *not* registered as kernel state.
///
/// A table is expected to have distinct kinds; [`kinds_are_distinct`](Self::kinds_are_distinct)
/// is `const` so that a table can prove it at compile time, and
/// [`activity_kinds!`](crate::activity_kinds) emits exactly that assertion.
///
/// The derives stop at `Clone`, `Copy`, `Debug`, `PartialEq` and `Eq`: a table entry is
/// compared and printed, never hashed or sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActivityName {
    /// The number the dispatch path and the journal use.
    pub kind: ActivityKind,
    /// The identifier the kind was declared as, for a log line to quote.
    pub name: &'static str,
}

impl ActivityName {
    /// The name `kind` was declared with in `table`, or [`None`] when it is not there.
    ///
    /// # Postconditions
    ///
    /// Returns the name of the *first* entry whose kind matches, and [`None`] when none
    /// does — including for an empty table, which firmware that declares no activities
    /// still links. An undeclared kind has no name rather than a wrong one: a lookup that
    /// guessed would put a plausible lie in a log, which is worse than a missing name
    /// because nothing downstream can tell the two apart.
    ///
    /// "First match" is only observable in a table with a duplicated kind, which
    /// [`activity_kinds!`](crate::activity_kinds) makes a compile error; a hand-written
    /// table can still contain one, so the behaviour is stated rather than left to chance.
    ///
    /// A linear scan over a table with a handful of entries, walked with
    /// [`slice::split_first`] rather than by index because `indexing_slicing` is denied in
    /// this workspace and a `const fn` has no iterator to reach for.
    #[must_use]
    pub const fn lookup(table: &[Self], kind: ActivityKind) -> Option<&'static str> {
        let mut rest = table;
        while let Some((entry, tail)) = rest.split_first() {
            // The `u16`s rather than the newtypes: derived `PartialEq` is not `const`, so
            // tidying this to `entry.kind == kind` would not compile in a `const fn`.
            if entry.kind.0 == kind.0 {
                return Some(entry.name);
            }
            rest = tail;
        }
        None
    }

    /// Whether no two entries of `table` share a kind.
    ///
    /// # Postconditions
    ///
    /// `true` when every kind in `table` appears once, and vacuously `true` for an empty
    /// table — an empty table has no clash to find, and a `const` assertion over one must
    /// not fail a build for having nothing in it. Compares every pair rather than adjacent
    /// entries, so a duplicate that is not next to its twin is still found.
    ///
    /// `const` so that a table can be asserted at compile time, which is what
    /// [`activity_kinds!`](crate::activity_kinds) does: two names on one number would make
    /// the dispatch path take the first while the second sat in the binary describing
    /// effects that never carry it.
    #[must_use]
    pub const fn kinds_are_distinct(table: &[Self]) -> bool {
        let mut rest = table;
        while let Some((entry, tail)) = rest.split_first() {
            let mut others = tail;
            while let Some((other, next)) = others.split_first() {
                // Compared as `u16`s for the same reason as in `lookup`: derived
                // `PartialEq` is not `const`.
                if entry.kind.0 == other.kind.0 {
                    return false;
                }
                others = next;
            }
            rest = tail;
        }
        true
    }
}

/// Declares activity kinds and their diagnostic table from one list.
///
/// One list produces both the [`ActivityKind`](crate::ActivityKind) constants the dispatch
/// path uses and the [`ActivityName`](crate::ActivityName) table a log line reads, so a
/// renumbered constant carries its name with it. Two hand-maintained lists is the failure
/// §13 invites: a log line that names the wrong activity.
///
/// The expansion also emits a `const` assertion that the table's kinds are distinct, so two
/// kinds sharing a number is a compile error rather than a lookup that silently returns the
/// first match.
///
/// # Grammar
///
/// ```text
/// $vis:vis $table:ident { $( $(#[$meta:meta])* $name:ident = $value:expr ),* $(,)? }
/// ```
///
/// The visibility applies to the constants and to the table alike; each kind may carry its
/// own attributes, which is where its doc comment goes. Those attributes are for
/// documentation and lints: `#[cfg]` on an individual kind is not supported, because the
/// table below still names the constant the `cfg` removed and the expansion stops
/// compiling.
///
/// # Examples
///
/// ```
/// waymaker_core::activity_kinds! {
///     pub OTA_ACTIVITIES {
///         /// Fetch the image over the network.
///         DOWNLOAD = 1,
///         /// Check the image signature before anything is written.
///         VERIFY_SIGNATURE = 2,
///     }
/// }
///
/// use waymaker_core::{ActivityKind, ActivityName};
///
/// assert_eq!(DOWNLOAD, ActivityKind(1));
/// assert_eq!(
///     ActivityName::lookup(OTA_ACTIVITIES, VERIFY_SIGNATURE),
///     Some("VERIFY_SIGNATURE"),
/// );
/// ```
///
/// Every path in the expansion is written `$crate::…`, and every macro it invokes is
/// written `::core::…`, so this works from a crate that has imported nothing from
/// `waymaker-core` — which is how firmware invokes it, and how the tests do. The leading
/// `::` is the same precaution [`assert_kernel_state_size!`](crate::assert_kernel_state_size)
/// takes for `::core::mem::size_of`: an unqualified `assert!`, `concat!` or `stringify!`
/// resolves at the *call site*, so a firmware crate that shadowed one of them would be
/// told its table is broken by a macro it did not write.
#[macro_export]
macro_rules! activity_kinds {
    (
        $vis:vis $table:ident {
            $( $(#[$meta:meta])* $name:ident = $value:expr ),* $(,)?
        }
    ) => {
        $(
            $(#[$meta])*
            $vis const $name: $crate::ActivityKind = $crate::ActivityKind($value);
        )*

        #[doc = ::core::concat!(
            "Every activity kind declared alongside `",
            ::core::stringify!($table),
            "`, paired with the name it was declared as.",
        )]
        ///
        /// Compile-time metadata for logs and diagnostics: this table lives in rodata and
        /// is never written to a record. Declaration order is preserved.
        $vis const $table: &[$crate::ActivityName] = &[
            $(
                $crate::ActivityName {
                    kind: $name,
                    name: ::core::stringify!($name),
                }
            ),*
        ];

        const _: () = ::core::assert!(
            $crate::ActivityName::kinds_are_distinct($table),
            ::core::concat!(
                "two activity kinds in `",
                ::core::stringify!($table),
                "` share a number, so a log line would name the wrong activity",
            ),
        );
    };
}

// `ActivityKind` travels in records, so its width is pinned. `ActivityName` deliberately is
// not: it holds a fat pointer, whose size differs between the host and `thumbv6m-none-eabi`.
const _: () = assert!(core::mem::size_of::<ActivityKind>() == 2);
const _: () = assert!(core::mem::align_of::<ActivityKind>() == 2);

#[cfg(test)]
// The macro takes its visibility from the caller and emits it verbatim, which is the whole
// point: a firmware crate writes `pub(crate)` and gets `pub(crate)`. Inside a private test
// module that is redundant, and `pub` would be `unreachable_pub` instead — the table is
// test-only either way, so the lint is allowed here rather than the macro weakened to suit
// a fixture.
#[allow(
    clippy::redundant_pub_crate,
    reason = "the macro emits the caller's visibility, and a test module is private"
)]
mod tests {
    use super::*;

    // Declared through the exported macro so that what the tests below read is what a
    // firmware crate would get, expansion and all — not a hand-written table that happens
    // to look like one.
    crate::activity_kinds! {
        pub(crate) TEST_ACTIVITIES {
            /// Fetch the image over the network.
            DOWNLOAD = 1,
            /// Check the image signature before anything is written.
            VERIFY_SIGNATURE = 2,
            /// Write the verified image into the inactive bank.
            FLASH_IMAGE = 3,
        }
    }

    #[test]
    fn a_kind_resolves_to_its_declared_name() {
        assert_eq!(
            ActivityName::lookup(TEST_ACTIVITIES, DOWNLOAD),
            Some("DOWNLOAD")
        );
        assert_eq!(
            ActivityName::lookup(TEST_ACTIVITIES, VERIFY_SIGNATURE),
            Some("VERIFY_SIGNATURE")
        );
        assert_eq!(
            ActivityName::lookup(TEST_ACTIVITIES, FLASH_IMAGE),
            Some("FLASH_IMAGE")
        );
    }

    #[test]
    fn an_unknown_kind_has_no_name() {
        // A lookup that guessed would put a plausible lie in a log, which is worse than a
        // missing name: nothing downstream can tell the two apart.
        assert_eq!(ActivityName::lookup(TEST_ACTIVITIES, ActivityKind(0)), None);
        assert_eq!(ActivityName::lookup(TEST_ACTIVITIES, ActivityKind(4)), None);
        assert_eq!(
            ActivityName::lookup(TEST_ACTIVITIES, ActivityKind(u16::MAX)),
            None
        );
    }

    #[test]
    fn an_empty_table_has_no_names() {
        // Firmware that declares no activities still links the lookup, so the empty table
        // has to be an answer rather than an edge case that reads past the end.
        const EMPTY: &[ActivityName] = &[];

        assert_eq!(ActivityName::lookup(EMPTY, ActivityKind(0)), None);
        assert_eq!(ActivityName::lookup(EMPTY, ActivityKind(1)), None);
    }

    #[test]
    fn distinct_kinds_are_distinct() {
        const SINGLETON: &[ActivityName] = &[ActivityName {
            kind: ActivityKind(9),
            name: "NINE",
        }];
        const EMPTY: &[ActivityName] = &[];

        assert!(ActivityName::kinds_are_distinct(TEST_ACTIVITIES));
        assert!(ActivityName::kinds_are_distinct(SINGLETON));
        // Vacuously true, and deliberately so: an empty table has no clash to find, and a
        // `const` assertion over one must not fail the build for having nothing in it.
        assert!(ActivityName::kinds_are_distinct(EMPTY));
    }

    #[test]
    fn a_duplicate_kind_is_detected() {
        // Two names on one number is the drift the macro's `const` assertion exists to
        // reject: the dispatch path would take the first, so the second name would sit in
        // the binary describing effects that never carry it.
        const CLASHING: &[ActivityName] = &[
            ActivityName {
                kind: ActivityKind(7),
                name: "SEVEN",
            },
            ActivityName {
                kind: ActivityKind(8),
                name: "EIGHT",
            },
            ActivityName {
                kind: ActivityKind(7),
                name: "SEVEN_AGAIN",
            },
        ];

        // The duplicated entries are not adjacent, so a check that only compared
        // neighbours would call this table distinct.
        assert!(!ActivityName::kinds_are_distinct(CLASHING));
        assert_eq!(
            ActivityName::lookup(CLASHING, ActivityKind(8)),
            Some("EIGHT")
        );
        // First match is the documented postcondition, and the twins are not adjacent, so
        // this also pins the scan direction: a lookup that kept going and returned the
        // last match would answer `SEVEN_AGAIN`.
        assert_eq!(
            ActivityName::lookup(CLASHING, ActivityKind(7)),
            Some("SEVEN")
        );
    }

    #[test]
    fn the_macro_declares_constants_and_a_table() {
        // One list produces both, so a renumbered constant cannot leave its name behind.
        assert_eq!(DOWNLOAD, ActivityKind(1));
        assert_eq!(VERIFY_SIGNATURE, ActivityKind(2));
        assert_eq!(FLASH_IMAGE, ActivityKind(3));

        assert_eq!(TEST_ACTIVITIES.len(), 3);
        assert!(
            TEST_ACTIVITIES
                .iter()
                .map(|entry| (entry.kind, entry.name))
                .eq([
                    (DOWNLOAD, "DOWNLOAD"),
                    (VERIFY_SIGNATURE, "VERIFY_SIGNATURE"),
                    (FLASH_IMAGE, "FLASH_IMAGE"),
                ])
        );
    }
}
