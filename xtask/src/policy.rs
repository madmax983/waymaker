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
    /// Every workspace crate this crate may depend on, directly or transitively, in any
    /// dependency kind.
    pub may_depend_on: &'static [&'static str],
    /// Third-party crates this layer is allowed to reach.
    ///
    /// This is the designed escape hatch. `waymaker-embassy` will need Embassy itself at
    /// rung 0.4; naming those crates here is how that arrives without anyone reaching for
    /// a broader exemption. A layer with both lists empty must have no dependencies at
    /// all.
    pub may_depend_on_external: &'static [&'static str],
    /// The design document's "must not own" column, quoted so that a violation message
    /// can say why the rule exists.
    pub must_not_own: &'static str,
}

/// The three firmware crates, ordered from the kernel outwards.
pub const LAYERS: &[LayerSpec] = &[
    LayerSpec {
        name: "waymaker-core",
        may_depend_on: &[],
        may_depend_on_external: &[],
        must_not_own: "allocation, serialization framework, CRC, clock, storage driver, executor, logging",
    },
    LayerSpec {
        name: "waymaker-flash",
        may_depend_on: &["waymaker-core"],
        may_depend_on_external: &[],
        must_not_own: "activities, workflow types, timers, Embassy",
    },
    LayerSpec {
        name: "waymaker-embassy",
        may_depend_on: &["waymaker-core", "waymaker-flash"],
        // Rung 0.4 adds the Embassy crates the facade actually needs. Until then the
        // facade is as dependency-free as the layers below it.
        may_depend_on_external: &[],
        must_not_own: "on-media authority or hidden global state",
    },
];

/// Host-only tooling that lives in the workspace but is not part of the firmware
/// layering. These crates are excluded from firmware-target builds by `default-members`.
pub const HOST_TOOLS: &[&str] = &["xtask"];

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

impl LayerSpec {
    /// Every crate this layer may reach, workspace and third-party alike.
    pub fn allowed_dependencies(&self) -> impl Iterator<Item = &'static str> + use<'_> {
        self.may_depend_on
            .iter()
            .chain(self.may_depend_on_external)
            .copied()
    }

    /// Renders the allowlist for a violation message.
    #[must_use]
    pub fn render_allowed(&self) -> String {
        let allowed: Vec<&str> = self.allowed_dependencies().collect();
        if allowed.is_empty() {
            "nothing".to_owned()
        } else {
            allowed.join(", ")
        }
    }
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
    fn every_workspace_allowance_names_a_layer() {
        // Third-party allowances belong in `may_depend_on_external`, which this
        // assertion deliberately does not constrain.
        for spec in LAYERS {
            for allowed in spec.may_depend_on {
                assert!(
                    layer(allowed).is_some(),
                    "{} may depend on {allowed}, which is not a layer; third-party crates belong in may_depend_on_external",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn the_allowlist_covers_both_workspace_and_external_crates() {
        let spec = LayerSpec {
            name: "example",
            may_depend_on: &["waymaker-core"],
            may_depend_on_external: &["embassy-time"],
            must_not_own: "nothing in particular",
        };
        let allowed: Vec<&str> = spec.allowed_dependencies().collect();
        assert_eq!(allowed, ["waymaker-core", "embassy-time"]);
        assert_eq!(spec.render_allowed(), "waymaker-core, embassy-time");
    }

    #[test]
    fn a_layer_with_no_allowances_renders_as_nothing() {
        let kernel = layer("waymaker-core").expect("the kernel is a layer");
        assert_eq!(kernel.render_allowed(), "nothing");
    }

    #[test]
    fn host_tools_are_not_layers() {
        for tool in HOST_TOOLS {
            assert!(layer(tool).is_none(), "{tool} must not also be a layer");
        }
    }
}
