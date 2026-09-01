//! The gate, run against the real workspace.
//!
//! The unit tests inside `xtask` prove each rule fires on a broken workspace. This test
//! proves the workspace we actually ship satisfies them.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the xtask crate lives inside the workspace")
        .to_path_buf()
}

#[test]
fn the_workspace_satisfies_its_own_layering_policy() {
    let violations =
        xtask::check_workspace(&workspace_root()).expect("the policy check should be runnable");

    assert!(
        violations.is_empty(),
        "workspace policy violations:\n{}",
        violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_three_firmware_crates_exist_and_are_layered() {
    let inputs =
        xtask::collect_inputs(&workspace_root()).expect("the workspace inputs should be readable");
    let graph = xtask::graph::PackageGraph::from_cargo_metadata(&inputs.metadata_json)
        .expect("cargo metadata should parse");

    for spec in xtask::policy::LAYERS {
        assert!(
            graph.find(spec.name).is_some(),
            "{} is missing from the workspace",
            spec.name
        );
    }

    assert!(
        graph.transitive_dependencies("waymaker-core").is_empty(),
        "the kernel must be dependency-free"
    );
    assert_eq!(
        graph.transitive_dependencies("waymaker-flash"),
        ["waymaker-core".to_owned()].into_iter().collect(),
        "waymaker-flash may only reach waymaker-core"
    );
    assert_eq!(
        graph.transitive_dependencies("waymaker-embassy"),
        ["waymaker-core".to_owned(), "waymaker-flash".to_owned()]
            .into_iter()
            .collect(),
        "waymaker-embassy may only reach the two crates below it"
    );
}
