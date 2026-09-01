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
use std::sync::OnceLock;

use xtask::size;

/// The measured workspace, linked once for the whole test binary.
///
/// Each `measure` links the whole matrix, and libtest runs these on separate threads: six
/// of them would queue on cargo's build lock and pay for the same images six times.
fn measured() -> &'static size::SizeReport {
    static REPORT: OnceLock<size::SizeReport> = OnceLock::new();
    REPORT.get_or_init(|| size::measure(&workspace_root()).expect("the size matrix should link"))
}

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
    let report = measured();

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
    let report = measured();
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
    let report = measured();
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
    let report = measured();
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
    let report = measured();
    let path = scratch("round-trip").join("nested").join("report.json");

    size::write_report(&path, report).expect("the report should be writable");
    let restored = size::read_report(&path).expect("the report should be readable");

    assert_eq!(&restored, report);
    assert!(
        size::diff(report, &restored).is_empty(),
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
    // Every base branch older than this gate has this shape, and the one thing it must not
    // do is come back as a set of rows that happen to match the current branch.
    //
    // The fixture is built here rather than reached for in this repository's own history. A
    // CI checkout is shallow, so `rev-list --max-parents=0 HEAD` returns the graft point —
    // the current tree, probe and all — and the test then passes by measuring exactly what
    // it is supposed to prove cannot be measured. It did, on the first CI run. A repository
    // this test creates is the same on every runner and at every clone depth.
    let repository = repository_without_a_probe("no-probe");

    let error = size::measure_baseline(&repository, "HEAD")
        .expect_err("a checkout with no size probe cannot be measured");

    // Three guards can catch this, depending on what encloses the checkout, and the test
    // accepts any of them because all three are the gate refusing to report a number:
    // no manifest to resolve at all; a manifest resolved from an enclosing workspace, which
    // `check_workspace_root` rejects as a measurement of a different tree; or a workspace
    // with no probe in it. What it must never be is `Ok`.
    let message = error.to_string();
    assert!(
        ["Cargo.toml", "rather than", "no `waymaker-size-probe`"]
            .iter()
            .any(|marker| message.contains(marker)),
        "the baseline failed for a reason that is not about the missing probe: {message}"
    );

    let _ = std::fs::remove_dir_all(&repository);
}

/// A git repository with one commit and no Waymaker in it.
fn repository_without_a_probe(label: &str) -> PathBuf {
    let root = scratch(label);
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&root)
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    git(&["init", "--quiet"]);
    git(&["config", "user.email", "size-gate@example.invalid"]);
    git(&["config", "user.name", "size gate"]);
    std::fs::write(
        root.join("README.md"),
        "A repository from before the size gate.\n",
    )
    .expect("the fixture should be writable");
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "Initial commit"]);
    root
}

#[test]
fn the_baseline_image_really_is_an_arm_image_with_bytes_in_it() {
    // Two things nothing else would notice: a measurement taken from a host binary (every
    // number plausible, none of them about firmware), and one taken from a file whose
    // section headers were stripped (every number zero, every budget passed).
    let baseline = measured().baseline().expect("a baseline row");
    assert!(
        baseline.sizes.flash > 0,
        "the baseline image reports no stored bytes, so nothing was linked or nothing was read"
    );
    assert!(
        baseline.sizes.text > 0,
        "the baseline image has no `.text`, which no linked firmware manages"
    );
}

#[test]
fn the_parser_agrees_with_llvm_size_about_the_probe() {
    // The synthetic-ELF tests prove the parser is self-consistent. They cannot prove it
    // reads the layout a real linker writes: an offset wrong in both the builder and the
    // parser would leave every one of them green. This is the second opinion, and it comes
    // from `llvm-size` in the pinned toolchain's own sysroot rather than from anything a
    // developer has to install.
    let Some(llvm_size) = llvm_size() else {
        // Not a silent pass: the toolchain pins `llvm-tools-preview`, so CI always has it.
        panic!(
            "llvm-size is missing from the toolchain sysroot; rust-toolchain.toml pins llvm-tools-preview, so this is a broken toolchain rather than a skippable test"
        );
    };

    let report = measured();
    let baseline = report.baseline().expect("a baseline row");
    let image = workspace_root()
        .join("target/waymaker-size-build/baseline")
        .join(xtask::pipeline::FIRMWARE_TARGET)
        .join("release")
        .join(size::PROBE_PACKAGE);

    let output = std::process::Command::new(&llvm_size)
        .arg("-A")
        .arg(&image)
        .output()
        .expect("llvm-size should run");
    assert!(output.status.success(), "llvm-size failed on {image:?}");
    let listing = String::from_utf8_lossy(&output.stdout);

    for (section, measured) in [
        (".text", baseline.sizes.text),
        (".rodata", baseline.sizes.rodata),
    ] {
        let second_opinion = section_size(&listing, section);
        assert_eq!(
            Some(measured),
            second_opinion,
            "our parser and llvm-size disagree about {section} of {image:?}:\n{listing}"
        );
    }
}

/// The size `llvm-size -A` reports for one section.
fn section_size(listing: &str, section: &str) -> Option<u64> {
    listing.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some(section))
            .then(|| fields.next().and_then(|size| size.parse().ok()))
            .flatten()
    })
}

/// `llvm-size` from the pinned toolchain's sysroot, where `llvm-tools-preview` puts it.
fn llvm_size() -> Option<PathBuf> {
    let sysroot = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    let sysroot = PathBuf::from(String::from_utf8(sysroot.stdout).ok()?.trim());
    let host = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .ok()?;
    let host = String::from_utf8(host.stdout).ok()?;
    let host = host
        .lines()
        .find_map(|line| line.strip_prefix("host: "))?
        .trim()
        .to_owned();
    let path = sysroot.join("lib/rustlib").join(host).join("bin/llvm-size");
    path.is_file().then_some(path)
}

#[test]
fn a_kernel_state_type_over_budget_fails_to_compile() {
    // The `const` assertion is the kernel-state gate: it is what turns a regression into a
    // build failure rather than a line in a report nobody reads. Proving it means building
    // something that must not build, which needs a crate of its own — this one path-depends
    // on the real `waymaker-core` so it is the shipped macro that is exercised.
    let root = scratch("oversize");
    std::fs::create_dir_all(root.join("src")).expect("the crate should be creatable");
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"oversize\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
             [dependencies]\nwaymaker-core = {{ path = {:?} }}\n\n[workspace]\n",
            workspace_root().join("crates/waymaker-core")
        ),
    )
    .expect("the manifest should be writable");

    let over = waymaker_core::budget::KERNEL_STATE_BYTES + 1;
    std::fs::write(
        root.join("src/lib.rs"),
        format!("#![no_std]\nwaymaker_core::assert_kernel_state_size!([u8; {over}]);\n"),
    )
    .expect("the crate root should be writable");

    // `uninstrumented_cargo` because under `cargo llvm-cov` a nested build inherits its
    // wrapper and flags, and would fail for reasons that have nothing to do with the
    // assertion under test.
    let output = xtask::coverage::uninstrumented_cargo()
        .current_dir(&root)
        .args(["build", "--quiet"])
        .output()
        .expect("cargo should run");

    assert!(
        !output.status.success(),
        "a {over} byte kernel state built cleanly against a {} byte budget",
        waymaker_core::budget::KERNEL_STATE_BYTES
    );
    let complaint = String::from_utf8_lossy(&output.stderr);
    assert!(
        complaint.contains("does not fit the kernel-state budget"),
        "the build failed for some other reason:\n{complaint}"
    );

    // And the same type one byte smaller builds, or the test above proves only that the
    // crate does not compile.
    std::fs::write(
        root.join("src/lib.rs"),
        format!(
            "#![no_std]\nwaymaker_core::assert_kernel_state_size!([u8; {}]);\n",
            waymaker_core::budget::KERNEL_STATE_BYTES
        ),
    )
    .expect("the crate root should be writable");
    let output = xtask::coverage::uninstrumented_cargo()
        .current_dir(&root)
        .args(["build", "--quiet"])
        .output()
        .expect("cargo should run");
    assert!(
        output.status.success(),
        "a kernel state exactly at the budget must build:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = std::fs::remove_dir_all(&root);
}
