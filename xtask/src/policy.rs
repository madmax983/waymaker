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

/// Crates that are built to be measured rather than shipped.
///
/// Not "fixtures": elsewhere in this workspace that word means test scaffolding — a
/// workspace that does not exist, an ELF image no linker would produce. This names a real,
/// committed, `#![no_std]` firmware crate that CI links on every push.
///
/// The size probe is firmware — `#![no_std]`, `#![no_main]`, built for
/// `thumbv6m-none-eabi` — but it is not a layer: it depends on all three of them at once,
/// on purpose, and nothing depends on it. Listing it here is what keeps
/// [`crate::graph::check_workspace_membership`] from reading it as a fourth layer that no
/// rule covers, while [`crate::size::check_size_probe`] applies the rules that do belong
/// to it.
pub const MEASUREMENT_CRATES: &[&str] = &["waymaker-size-probe"];

/// Crates that exist to test the layers, and are never linked into firmware.
///
/// Not host tooling — [`HOST_TOOLS`] is the gate itself — and not a measurement crate: a
/// test-support crate is a library that exists to test the layers, generic over the writer
/// under test rather than over the crate calling it, and the tests that drive it live with
/// it. `waymaker-fault` is
/// the in-memory storage model and crash injector of issue
/// [#18](https://github.com/madmax983/waymaker/issues/18): it depends on `waymaker-flash`
/// for design document §12's storage contract, models media in `Vec<u8>`, and is kept out
/// of `default-members` so that no firmware target ever builds it.
///
/// Why the category exists rather than the crate being a fourth layer: a layer's public
/// functions must all be reached by the size probe, so an exhaustive host-side crash
/// enumerator listed in [`LAYERS`] would be charged against an 8 KiB code-flash budget it
/// has nothing to do with. See
/// [ADR 0013](https://github.com/madmax983/waymaker/blob/main/docs/adr/0013-the-fault-harness-is-a-crate-above-the-layers.md).
///
/// What this category does *not* license is a layer depending on one of these, in any
/// dependency kind: [`check_dependency_direction`](crate::graph::check_dependency_direction)
/// reads [`LAYERS`] and nothing else, so `waymaker-flash` gaining a dev-dependency on
/// `waymaker-fault` is still a violation. The harness sits above the layers and the tests
/// that drive it live with it.
pub const TEST_SUPPORT_CRATES: &[&str] = &["waymaker-fault", "waymaker-spec"];

/// The crate that is allowed to know about Embassy.
pub const EMBASSY_FACADE: &str = "waymaker-embassy";

/// Package-name prefixes that identify the Embassy ecosystem.
///
/// `waymaker-embassy` matches `waymaker-embassy` exactly rather than by prefix, so the
/// façade crate is not mistaken for an Embassy crate by the prefix scan.
pub const EMBASSY_PREFIXES: &[&str] = &["embassy"];

/// Every workspace crate the manifest and crate-root rules apply to.
///
/// The three layers plus the test-support crates. Not the size probe, which has
/// [`crate::size::check_size_probe`] of its own, and not `xtask`, which is the gate.
///
/// A rule that iterates [`LAYERS`] alone leaves a test-support crate free to drop
/// `[lints] workspace = true`, grow a `build.rs`, or set `[lib] test = false` — the last of
/// which makes a crate report "no coverable lines" and pass the coverage gate. The
/// exemptions a test-support crate really has are narrow and specific: it is not
/// `#![no_std]`, and it may use `std`.
pub fn checked_members() -> impl Iterator<Item = &'static str> {
    LAYERS
        .iter()
        .map(|layer| layer.name)
        .chain(TEST_SUPPORT_CRATES.iter().copied())
}

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

    #[test]
    fn the_checked_members_are_the_layers_and_the_test_support_crates() {
        let checked: Vec<&str> = checked_members().collect();
        for spec in LAYERS {
            assert!(checked.contains(&spec.name), "{} is not checked", spec.name);
        }
        for name in TEST_SUPPORT_CRATES {
            assert!(checked.contains(name), "{name} is not checked");
        }
        for name in HOST_TOOLS.iter().chain(MEASUREMENT_CRATES) {
            assert!(
                !checked.contains(name),
                "{name} has rules of its own and is not checked here"
            );
        }
    }

    #[test]
    fn test_support_crates_are_none_of_the_other_three_categories() {
        for crate_name in TEST_SUPPORT_CRATES {
            assert!(
                layer(crate_name).is_none(),
                "{crate_name} must not also be a layer"
            );
            assert!(
                !HOST_TOOLS.contains(crate_name),
                "{crate_name} must not also be host tooling"
            );
            assert!(
                !MEASUREMENT_CRATES.contains(crate_name),
                "{crate_name} must not also be a measurement crate"
            );
        }
    }

    #[test]
    fn no_layer_may_reach_a_test_support_crate_in_any_dependency_kind() {
        // The harness depends on `waymaker-flash`. If a layer were also allowed to depend
        // on the harness the workspace would have a cycle, and the reason it does not is
        // that this allowance does not exist — asserted rather than remembered.
        for spec in LAYERS {
            for allowed in spec.allowed_dependencies() {
                assert!(
                    !TEST_SUPPORT_CRATES.contains(&allowed),
                    "{} may depend on the test-support crate {allowed}",
                    spec.name
                );
            }
        }
    }

    #[test]
    fn measurement_fixtures_are_neither_layers_nor_host_tools() {
        for fixture in MEASUREMENT_CRATES {
            assert!(
                layer(fixture).is_none(),
                "{fixture} must not also be a layer"
            );
            assert!(
                !HOST_TOOLS.contains(fixture),
                "{fixture} must not also be host tooling"
            );
        }
    }

    #[test]
    fn the_size_probe_is_the_crate_the_size_gate_links() {
        assert!(MEASUREMENT_CRATES.contains(&crate::size::PROBE_PACKAGE));
    }
}
