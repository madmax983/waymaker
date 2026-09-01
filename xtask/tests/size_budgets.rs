//! The size gate, run against the real workspace.
//!
//! The unit tests inside `xtask` prove the accounting on synthetic ELF images: what counts
//! as flash, what a delta is, which budget a number breaches. These prove the other half —
//! that the matrix really links, that the images it produces can be read, and that the
//! failure modes fail closed rather than reporting zero.
//!
//! They link firmware, so they are slower than the unit tests. They are here rather than
//! mocked because "the probe links" is the one property no synthetic image can establish,
//! and it is the property the whole gate rests on.

// `clippy.toml` exempts `#[test]` bodies from `expect_used`, but not the free helper
// functions an integration-test crate needs. Every function in this file is test code.
#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

use xtask::size;

/// The workspace root, which is this crate's parent directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn scratch(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "waymaker-size-{label}-{}-{}",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("the scratch directory should be creatable");
    path
}

#[test]
fn the_workspace_we_ship_is_within_every_size_budget() {
    let report = size::measure(&workspace_root()).expect("the size matrix should link");

    assert!(
        report.baseline().is_some(),
        "every report needs the image the deltas are measured against"
    );
    let shortfalls = report.shortfalls();
    assert!(
        shortfalls.is_empty(),
        "the workspace exceeds a budget from design document \u{a7}04:\n{}",
        shortfalls
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_matrix_links_a_baseline_a_default_and_a_facade_image() {
    let report = size::measure(&workspace_root()).expect("the size matrix should link");
    for expected in ["baseline", "default", "facade"] {
        assert!(
            report.row(expected).is_some(),
            "the matrix has no `{expected}` row: {:?}",
            report
                .rows()
                .iter()
                .map(|row| &row.name)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn the_engine_costs_more_flash_than_the_baseline() {
    // The whole gate rests on the probe actually linking the layers rather than the
    // linker discarding them: a probe whose engine is dead-stripped reports a delta of
    // zero and passes every budget for ever.
    let report = size::measure(&workspace_root()).expect("the size matrix should link");
    let delta = report
        .delta_of("default")
        .expect("the default row is measured against the baseline");
    assert!(
        delta.flash > 0,
        "linking the kernel and the flash adapter cost nothing, which means the linker \
         discarded them and the gate is measuring an empty image"
    );
    assert!(
        delta.text > 0,
        "the engine contributed no `.text`, so nothing of it survived to be measured"
    );
}

#[test]
fn the_facade_costs_at_least_as_much_as_the_engine_it_sits_on() {
    let report = size::measure(&workspace_root()).expect("the size matrix should link");
    let engine = report.delta_of("default").expect("a default row");
    let facade = report.delta_of("facade").expect("a facade row");
    assert!(
        facade.flash >= engine.flash,
        "the facade ({} B) links the engine ({} B) and cannot be smaller than it",
        facade.flash,
        engine.flash
    );
}

#[test]
fn a_report_written_to_disk_reads_back_unchanged() {
    let report = size::measure(&workspace_root()).expect("the size matrix should link");
    let path = scratch("round-trip").join("nested").join("report.json");

    size::write_report(&path, &report).expect("the report should be writable");
    let restored = size::read_report(&path).expect("the report should be readable");

    assert_eq!(restored, report);
    assert!(
        size::diff(&report, &restored).is_empty(),
        "a report diffed against its own round trip must show no change"
    );
    let _ = std::fs::remove_dir_all(path.parent().and_then(Path::parent).unwrap_or(&path));
}

#[test]
fn a_missing_report_is_an_error_rather_than_an_empty_one() {
    let error = size::read_report(&scratch("missing").join("absent.json"))
        .expect_err("a report that is not there has not passed");
    assert!(error.to_string().contains("could not read"), "{error}");
}

#[test]
fn a_directory_that_is_not_this_workspace_is_not_measured_as_this_workspace() {
    // The failure this guards is silent: `cargo metadata` resolves the nearest manifest at
    // or above its working directory, so a base-branch checkout with no manifest of its own
    // would otherwise be measured as whatever workspace encloses it.
    let empty = scratch("not-a-workspace");
    let error = size::measure(&empty).expect_err("an empty directory has nothing to measure");
    let message = error.to_string();
    assert!(
        message.contains("workspace") || message.contains("cargo metadata"),
        "{message}"
    );
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn a_base_reference_that_does_not_exist_is_reported_rather_than_measured() {
    let error = size::measure_baseline(&workspace_root(), "no-such-branch-cf3a1d")
        .expect_err("an unknown reference cannot be measured");
    assert!(
        error.to_string().contains("does not name a commit"),
        "{error}"
    );
}

#[test]
fn a_base_commit_from_before_the_probe_existed_is_reported_rather_than_measured() {
    // The very first commit of this repository has no workspace manifest, which is the
    // shape of every base branch older than this gate. It must come back as "cannot
    // measure", never as a set of rows that happen to match the current branch.
    let root = workspace_root();
    let Some(first) = first_commit(&root) else {
        return;
    };
    let error = size::measure_baseline(&root, &first)
        .expect_err("a commit with no size probe cannot be measured");
    let message = error.to_string();
    assert!(
        message.contains("rather than") || message.contains("no `waymaker-size-probe`"),
        "{message}"
    );
}

/// The repository's first commit, or `None` where there is no git history to read.
fn first_commit(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .next()
        .map(str::to_owned)
        .filter(|commit| !commit.is_empty())
}
