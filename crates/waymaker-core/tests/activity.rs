//! Activity kinds are numbers on the dispatch path and names only in diagnostics.
//!
//! Design document §13: "Activity names are compile-time metadata for logs and
//! diagnostics. The runtime dispatch path uses numeric kinds and does not store names in
//! every record." The risk that creates is drift — a constant renumbered without its
//! table, so a log says `DOWNLOAD` about a `FLASH_IMAGE`. `activity_kinds!` exists to make
//! that unrepresentable by declaring both from one list, and this file tests that it does.

use waymaker_core::{ActivityKind, ActivityName};

// Declared at the top level of the test crate because that is where firmware declares it:
// the macro has to work from outside `waymaker-core`, through `$crate` paths, with no
// imports in scope beyond the ones above.
waymaker_core::activity_kinds! {
    pub(crate) OTA_ACTIVITIES {
        /// Fetch the image over the network.
        DOWNLOAD = 1,
        /// Check the image signature before anything is written.
        VERIFY_SIGNATURE = 2,
        /// Write the verified image into the inactive bank.
        FLASH_IMAGE = 3,
    }
}

#[test]
fn activity_kinds_macro_builds_a_distinct_named_table() {
    assert_eq!(DOWNLOAD, ActivityKind(1));
    assert_eq!(VERIFY_SIGNATURE, ActivityKind(2));
    assert_eq!(FLASH_IMAGE, ActivityKind(3));

    // The table is in declaration order and carries the constants' own spellings, so a
    // renumbered constant moves its name with it.
    assert!(
        OTA_ACTIVITIES
            .iter()
            .map(|entry| (entry.kind, entry.name))
            .eq([
                (DOWNLOAD, "DOWNLOAD"),
                (VERIFY_SIGNATURE, "VERIFY_SIGNATURE"),
                (FLASH_IMAGE, "FLASH_IMAGE"),
            ])
    );

    assert!(ActivityName::kinds_are_distinct(OTA_ACTIVITIES));
}

#[test]
fn every_declared_kind_looks_up_the_name_it_was_declared_with() {
    for entry in OTA_ACTIVITIES {
        assert_eq!(
            ActivityName::lookup(OTA_ACTIVITIES, entry.kind),
            Some(entry.name),
            "{:?} did not round trip",
            entry.kind
        );
    }

    // Zero is not declared above, and `u16::MAX` is not either: an undeclared kind has no
    // name rather than a wrong one, because a lookup that guessed would put a plausible
    // lie in a log.
    assert_eq!(ActivityName::lookup(OTA_ACTIVITIES, ActivityKind(0)), None);
    assert_eq!(
        ActivityName::lookup(OTA_ACTIVITIES, ActivityKind(u16::MAX)),
        None
    );
}
