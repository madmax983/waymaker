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
        // A git hook exports `GIT_DIR`, `GIT_INDEX_FILE` and friends, pointing at the
        // repository being committed to. Inherited here, `git init` and `git commit` act on
        // *that* repository instead of on the fixture, and this test fails inside
        // `.githooks/pre-commit` while passing everywhere else — which is exactly the
        // configuration `cargo xtask install-hooks` asks a contributor to run in.
        let output = std::process::Command::new("git")
            .current_dir(&root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_PREFIX")
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

#[test]
fn the_gated_figure_is_the_layers_share_rather_than_the_whole_image() {
    // Issue #72: the probe's own `match` arms, folds and calls exist only to keep the
    // layers' code alive past `--gc-sections`, and design document §04's budget is stated
    // for "core + flash adapter". This is the correction, measured on the real image.
    let report = measured();
    let delta = report.delta_of("default").expect("a default row");
    let probe = report
        .probe_delta_of("default")
        .expect("the symbol table names the probe's own code");
    let layers = report
        .layers_flash_of("default")
        .expect("the layers' share is measurable");

    assert!(
        probe > 0,
        "no byte of the engine image is attributed to the probe, but the probe is what was linked"
    );
    assert!(
        layers < delta.flash,
        "the layers' share ({layers} B) is the image delta ({} B) less the probe's own \
         growth, so it cannot be the whole delta",
        delta.flash
    );
    assert_eq!(
        layers,
        delta.flash - probe,
        "the layers' share is the delta less the probe's own growth and nothing else"
    );
}

#[test]
fn the_probes_own_code_is_a_real_share_of_the_image_it_is_subtracted_from() {
    // A sanity bound in both directions. Zero would mean the symbol table was not read;
    // everything would mean the layers were dead-stripped and the gate measures nothing.
    let report = measured();
    for name in ["default", "facade"] {
        let row = report.row(name).expect("a measured row");
        assert!(
            row.probe_flash > 0 && row.probe_flash < row.sizes.flash,
            "`{name}` attributes {} B to the probe out of an image of {} B",
            row.probe_flash,
            row.sizes.flash
        );
    }
}

#[test]
fn the_symbol_reader_agrees_with_llvm_nm_about_what_the_probe_costs() {
    // The second opinion for the symbol *reader*, as the `llvm-size` test above is for the
    // sections: a symbol table read at the wrong offsets answers with well-formed nonsense,
    // and the gate would then subtract it. It says nothing about the *attribution*, because
    // `defining_crate` decides both sides of the comparison — the test below this one holds
    // that, and the per-symbol section index is held in `xtask::elf`'s own tests, against
    // `llvm-readobj`.
    let Some(llvm_nm) = llvm_tool("llvm-nm") else {
        panic!(
            "llvm-nm is missing from the toolchain sysroot; rust-toolchain.toml pins llvm-tools-preview, so this is a broken toolchain rather than a skippable test"
        );
    };

    let report = measured();
    let row = report.row("default").expect("a default row");
    let image = default_image();

    let output = std::process::Command::new(&llvm_nm)
        .args(["--print-size", "--defined-only"])
        .arg(&image)
        .output()
        .expect("llvm-nm should run");
    assert!(output.status.success(), "llvm-nm failed on {image:?}");

    let probe = size::probe_crate_name();
    let listing = String::from_utf8_lossy(&output.stdout);
    let mut second_opinion: Vec<(u64, u64)> = listing
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // `<address> <size> <type> <name>`; a symbol with no size prints three fields.
            let (Some(address), Some(bytes), Some(name)) =
                (fields.first(), fields.get(1), fields.get(3))
            else {
                return None;
            };
            // `n`/`N` is debug information, which costs no flash. Everything else
            // `--defined-only` prints is in an allocated section of this image.
            if matches!(fields.get(2), Some(&"n" | &"N")) {
                return None;
            }
            if size::defining_crate(name) != Some(probe.as_str()) {
                return None;
            }
            Some((
                u64::from_str_radix(address, 16).ok()?,
                u64::from_str_radix(bytes, 16).ok()?,
            ))
        })
        .collect();
    second_opinion.sort_unstable();

    // Each symbol's placement and width, not their total. A total would be a reading of
    // `attributed_flash`'s rule as well as of this reader — that rule measures the union of
    // address ranges, so the two agree only while nothing is folded — and this test is
    // about the offsets the table is read at. `llvm-nm` clears the ARM interworking bit
    // that `st_value` carries on a Thumb function, so it is masked here.
    let mut ours: Vec<(u64, u64)> = xtask::elf::symbols(&std::fs::read(&image).expect("readable"))
        .expect("the image should parse")
        .into_iter()
        .filter(|symbol| symbol.size > 0)
        .filter(|symbol| size::defining_crate(&symbol.name) == Some(probe.as_str()))
        .map(|symbol| (symbol.address & !1, symbol.size))
        .collect();
    ours.sort_unstable();

    assert!(!ours.is_empty(), "no symbol is credited to `{probe}`");
    assert_eq!(
        ours, second_opinion,
        "our symbol reader and llvm-nm disagree about `{probe}`'s symbols in {image:?}"
    );
    assert!(
        row.probe_flash > 0 && row.probe_flash <= ours.iter().map(|(_, size)| size).sum::<u64>(),
        "the attributed figure ({} B) is not within the bytes those symbols name",
        row.probe_flash
    );
}

#[test]
fn stripping_the_symbol_table_moves_no_byte_the_gate_measures() {
    // ADR 0029's central claim, driven rather than argued. The matrix links with
    // `--config profile.release.strip="none"` so that there are symbols to attribute, and
    // gates the section sizes of that same image. That is one measurement only while
    // stripping touches nothing allocated. `size::check_symbols_are_not_measured` asks each
    // image whether that holds; this links the workspace's own release profile beside it
    // and compares the answer.
    let stripped = scratch("stripped");
    let output = xtask::coverage::uninstrumented_cargo()
        .current_dir(workspace_root())
        .args([
            "build",
            "--locked",
            "--release",
            "--message-format",
            "json-render-diagnostics",
            "--target",
            xtask::pipeline::FIRMWARE_TARGET,
            "--target-dir",
        ])
        .arg(&stripped)
        .args([
            "--package",
            size::PROBE_PACKAGE,
            "--no-default-features",
            "--features",
            "probe,engine",
        ])
        .output()
        .expect("cargo build should run");
    assert!(
        output.status.success(),
        "linking the stripped image failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let image = size::executable_path(
        &String::from_utf8_lossy(&output.stdout),
        size::PROBE_PACKAGE,
    )
    .expect("the build produced an image");
    let bytes = std::fs::read(&image).expect("the image should be readable");
    let sections = xtask::elf::sections(&bytes).expect("the image should parse");
    let stripped_sizes = size::SectionSizes::of(&sections);

    // The stripped image has no symbol table, which is exactly why the gate does not use it.
    assert!(
        size::check_symbols_are_not_measured(&sections).is_err(),
        "the release profile strips symbols, so the gated image cannot be the attributed one"
    );

    let measured = measured().row("default").expect("a default row").sizes;
    assert_eq!(
        stripped_sizes, measured,
        "stripping changed a section the budget is measured on, so the attribution and the \
         gated sizes are readings of two different images"
    );
}

#[test]
fn no_symbol_the_gate_credits_to_the_probe_is_a_traits_own_provided_body() {
    // `defining_crate` reads the first crate root of a mangled path, and one v0 production
    // puts them the other way round: `Y` is `<Self as Trait>::method` for a method the
    // *trait* provides, and it names the self type first. A layer trait with a default
    // body, implemented for a probe type, would then be a layer's bytes under the probe's
    // name — and this gate subtracts what it reads as the probe's.
    //
    // `X`, the impl's own method, demangles to the same `<A as B>::m` shape and is
    // genuinely the probe's, so a demangler cannot tell the two apart. The mangled form
    // can, and this scans for it the other way round from `defining_crate`: find the
    // length-prefixed crate name as a substring and look at what came before it, rather
    // than parse crate roots left to right. `llvm-nm --demangle` supplies the name a
    // failure has to be readable as.
    let Some(llvm_nm) = llvm_tool("llvm-nm") else {
        panic!(
            "llvm-nm is missing from the toolchain sysroot; rust-toolchain.toml pins llvm-tools-preview, so this is a broken toolchain rather than a skippable test"
        );
    };

    let image = default_image();
    let output = std::process::Command::new(&llvm_nm)
        .args(["--print-size", "--defined-only", "--demangle"])
        .arg(&image)
        .output()
        .expect("llvm-nm should run");
    assert!(output.status.success(), "llvm-nm failed on {image:?}");
    let demangled = String::from_utf8_lossy(&output.stdout);

    let bytes = std::fs::read(&image).expect("the image should be readable");
    let symbols = xtask::elf::symbols(&bytes).expect("the image should parse");
    let probe = size::probe_crate_name();

    let credited: Vec<&xtask::elf::Symbol> = symbols
        .iter()
        .filter(|symbol| size::defining_crate(&symbol.name) == Some(probe.as_str()))
        .collect();
    assert!(
        !credited.is_empty(),
        "no symbol is credited to the probe, so this test checks nothing"
    );

    // Falsifiable on this image rather than only on one a future layer produces: it
    // already carries `<waymaker_flash::storage::Geometry as core::cmp::PartialEq>::ne`,
    // whose body is `core`'s. A `defining_crate` that read the first crate root of a `Y`
    // name would answer `waymaker_flash` for it.
    let qualified: Vec<&xtask::elf::Symbol> = symbols
        .iter()
        .filter(|symbol| head_of(&symbol.name).is_some_and(|head| head.contains('Y')))
        .collect();
    assert!(
        !qualified.is_empty(),
        "the image carries no `Y` symbol, so the half of this test that can fail checks nothing"
    );
    for symbol in qualified {
        assert_eq!(
            size::defining_crate(&symbol.name),
            None,
            "`{}` is a trait's own provided body, whose crate the mangled path names second",
            symbol.name
        );
    }

    for symbol in credited {
        // `llvm-nm --print-size` prints `<address> <size> <type> <name>`, and the address
        // is what identifies a defined symbol: any number of functions share a size, so
        // pairing on one picks whichever came first. Bit 0 of `st_value` is the ARM
        // interworking flag — a Thumb function is odd — and `llvm-nm` clears it while the
        // ELF field carries it, so it is masked here; the size is matched as well, which
        // is what tells a Thumb function from the zero-sized `$t` symbol at the same place.
        let spelled = demangled
            .lines()
            .find_map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                let address = u64::from_str_radix(fields.first()?, 16).ok()?;
                let size = u64::from_str_radix(fields.get(1)?, 16).ok()?;
                if address != symbol.address & !1 || size != symbol.size {
                    return None;
                }
                Some(fields.get(3..)?.join(" "))
            })
            .unwrap_or_else(|| {
                panic!(
                    "llvm-nm lists no {} B symbol at {:#x}, which our reader credits to `{probe}`",
                    symbol.size,
                    symbol.address & !1
                )
            });

        let head = head_of(&symbol.name).unwrap_or_else(|| {
            panic!("`{spelled}` is credited to `{probe}` and names no crate of this workspace")
        });
        assert!(
            !head.contains('Y'),
            "the gate credits {} B to `{probe}` for `{spelled}`, whose mangled path opens with the `Y` production — the body of a provided method belongs to the trait, so this would subtract a layer's bytes from the budget",
            symbol.size,
        );
    }
}

/// The part of a `v0` mangled name before the first crate of this workspace it names.
///
/// Found by searching for the length-prefixed crate name as a substring, which is the
/// opposite way round from `size::defining_crate` — that parses crate roots left to right.
/// Two readings of one name agreeing is worth more than one reading checked against itself.
fn head_of(mangled: &str) -> Option<&str> {
    [
        "waymaker-core",
        "waymaker-flash",
        "waymaker-embassy",
        size::PROBE_PACKAGE,
    ]
    .iter()
    .map(|package| package.replace('-', "_"))
    .filter_map(|crate_name| mangled.find(&format!("{}{crate_name}", crate_name.len())))
    .min()
    .and_then(|at| mangled.get(..at))
}

/// The `default` row's linked image, once [`measured`] has linked it.
///
/// Through `measured` rather than by joining the path, because the matrix is what links
/// these images: a test that only names the file passes on a developer's machine, where a
/// previous run left one behind, and fails on a clean checkout — or, worse, measures the
/// image a previous run left.
fn default_image() -> PathBuf {
    assert!(
        measured().row("default").is_some(),
        "the matrix linked no `default` image"
    );
    workspace_root()
        .join("target/waymaker-size-build/default")
        .join(xtask::pipeline::FIRMWARE_TARGET)
        .join("release")
        .join(size::PROBE_PACKAGE)
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
    llvm_tool("llvm-size")
}

/// One tool from the pinned toolchain's sysroot, where `llvm-tools-preview` puts it.
fn llvm_tool(tool: &str) -> Option<PathBuf> {
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
    let path = sysroot
        .join("lib/rustlib")
        .join(host)
        .join("bin")
        .join(tool);
    path.is_file().then_some(path)
}

/// Builds `source` as a crate that path-depends on the real `waymaker-core`.
///
/// Returns whether it built and what it complained about. The shipped macro is what is
/// exercised, so a crate of its own is the only way to watch it refuse.
fn build_against_the_kernel(label: &str, source: &str) -> (bool, String) {
    let root = scratch(label);
    std::fs::create_dir_all(root.join("src")).expect("the crate should be creatable");
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{label}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
             [dependencies]\nwaymaker-core = {{ path = \"{}\" }}\n\n[workspace]\n",
            workspace_root().join("crates/waymaker-core").display()
        ),
    )
    .expect("the manifest should be writable");
    std::fs::write(root.join("src/lib.rs"), source).expect("the crate root should be writable");

    // `uninstrumented_cargo` because under `cargo llvm-cov` a nested build inherits its
    // wrapper and flags, and would fail for reasons that have nothing to do with the
    // assertion under test.
    let output = xtask::coverage::uninstrumented_cargo()
        .current_dir(&root)
        .args(["build", "--quiet"])
        .output()
        .expect("cargo should run");
    let complaint = String::from_utf8_lossy(&output.stderr).into_owned();
    let built = output.status.success();
    let _ = std::fs::remove_dir_all(&root);
    (built, complaint)
}

#[test]
fn a_context_over_its_share_fails_to_compile() {
    // The falsifier for `assert_context_size!`. It is the exact check on the target the
    // budget is stated for — `cargo xtask size` reports a host figure, which is only an
    // upper bound — so a version of it that could not refuse would leave the target figure
    // measured by nothing.
    let over = waymaker_core::budget::CONTEXT_RAM_BYTES + 1;
    let (built, complaint) = build_against_the_kernel(
        "wide-context",
        &format!("#![no_std]\nwaymaker_core::assert_context_size!([u8; {over}]);\n"),
    );
    assert!(
        !built,
        "a {over} byte context built cleanly against a {} byte share",
        waymaker_core::budget::CONTEXT_RAM_BYTES
    );
    assert!(
        complaint.contains("does not fit the context's share of runtime RAM"),
        "the build failed for some other reason:\n{complaint}"
    );

    // And the same type exactly at the share builds, or the assertion above proves only
    // that the crate does not compile.
    let (built, complaint) = build_against_the_kernel(
        "exact-context",
        &format!(
            "#![no_std]\nwaymaker_core::assert_context_size!([u8; {}]);\n",
            waymaker_core::budget::CONTEXT_RAM_BYTES
        ),
    );
    assert!(
        built,
        "a context exactly at the share must build:\n{complaint}"
    );
}

#[test]
fn a_caller_that_shadows_assert_cannot_turn_either_budget_off() {
    // `macro_rules!` resolves macro names at the *call site*, so an unqualified `assert!`
    // inside either expansion is one the calling crate can replace with a no-op. Both
    // macros are the one part of this crate's surface downstream firmware touches, and a
    // budget a caller can switch off is not a budget.
    for (macro_name, share) in [
        (
            "assert_context_size",
            waymaker_core::budget::CONTEXT_RAM_BYTES,
        ),
        (
            "assert_kernel_state_size",
            waymaker_core::budget::KERNEL_STATE_BYTES,
        ),
    ] {
        let (built, complaint) = build_against_the_kernel(
            "shadowed-assert",
            &format!(
                "#![no_std]\n\
                 macro_rules! assert {{ ($($t:tt)*) => {{ () }} }}\n\
                 macro_rules! concat {{ ($($t:tt)*) => {{ \"\" }} }}\n\
                 macro_rules! stringify {{ ($($t:tt)*) => {{ \"\" }} }}\n\
                 waymaker_core::{macro_name}!([u8; {}]);\n",
                share + 1
            ),
        );
        assert!(
            !built,
            "`{macro_name}` was switched off by a caller that shadowed `assert!`"
        );
        assert!(
            complaint.contains("does not fit"),
            "`{macro_name}` failed for some other reason:\n{complaint}"
        );
    }
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
