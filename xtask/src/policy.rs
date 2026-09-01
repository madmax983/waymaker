//! The layering contract, transcribed from §05 of the design document.
//!
//! Adding a crate to the workspace means adding a row here. The rules in
//! [`crate::graph`] read this table and nothing else, so the table and the gate cannot
//! drift apart.

/// One crate in the layering, and everything it is allowed to reach for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerSpec {
    /// Package name as it appears in `Cargo.toml`.
    pub name: &'static str,
    /// Every package this crate may depend on, directly or transitively, in any
    /// dependency kind. An empty list means the crate must have no dependencies at all.
    pub may_depend_on: &'static [&'static str],
    /// The design document's "must not own" column, quoted so that a violation message
    /// can say why the rule exists.
    pub must_not_own: &'static str,
}

/// The three firmware crates, ordered from the kernel outwards.
pub const LAYERS: &[LayerSpec] = &[
    LayerSpec {
        name: "waymaker-core",
        may_depend_on: &[],
        must_not_own: "allocation, serialization framework, CRC, clock, storage driver, executor, logging",
    },
    LayerSpec {
        name: "waymaker-flash",
        may_depend_on: &["waymaker-core"],
        must_not_own: "activities, workflow types, timers, Embassy",
    },
    LayerSpec {
        name: "waymaker-embassy",
        may_depend_on: &["waymaker-core", "waymaker-flash"],
        must_not_own: "on-media authority or hidden global state",
    },
];

/// The crate that is allowed to know about Embassy.
pub const EMBASSY_FACADE: &str = "waymaker-embassy";

/// Package-name prefixes that identify the Embassy ecosystem.
///
/// `waymaker-embassy` matches `waymaker-embassy` exactly rather than by prefix, so the
/// façade crate is not mistaken for an Embassy crate by the prefix scan.
pub const EMBASSY_PREFIXES: &[&str] = &["embassy"];

/// Returns the layer specification for `name`, if `name` is one of the firmware crates.
#[must_use]
pub fn layer(name: &str) -> Option<&'static LayerSpec> {
    LAYERS.iter().find(|layer| layer.name == name)
}

/// Returns true if `name` is an Embassy crate or the Waymaker Embassy façade.
#[must_use]
pub fn is_embassy_package(name: &str) -> bool {
    name == EMBASSY_FACADE
        || EMBASSY_PREFIXES
            .iter()
            .any(|prefix| name == *prefix || name.starts_with(&format!("{prefix}-")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_are_ordered_from_the_kernel_outwards() {
        assert_eq!(LAYERS[0].name, "waymaker-core");
        assert!(LAYERS[0].may_depend_on.is_empty());
        assert_eq!(LAYERS[1].may_depend_on, ["waymaker-core"]);
        assert_eq!(LAYERS[2].may_depend_on, ["waymaker-core", "waymaker-flash"]);
    }

    #[test]
    fn embassy_ecosystem_crates_are_recognised() {
        assert!(is_embassy_package("embassy"));
        assert!(is_embassy_package("embassy-time"));
        assert!(is_embassy_package("embassy-executor"));
        assert!(is_embassy_package("waymaker-embassy"));
    }

    #[test]
    fn unrelated_crates_are_not_mistaken_for_embassy() {
        assert!(!is_embassy_package("embassytown"));
        assert!(!is_embassy_package("waymaker-core"));
        assert!(!is_embassy_package("serde"));
    }

    #[test]
    fn every_layer_may_only_depend_on_other_layers() {
        for spec in LAYERS {
            for allowed in spec.may_depend_on {
                assert!(
                    layer(allowed).is_some(),
                    "{} may depend on {allowed}, which is not a layer",
                    spec.name
                );
            }
        }
    }
}
