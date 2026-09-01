//! The coverage gate.
//!
//! Issue #9 asks for "coverage gate at 85% minimum, reported per crate so kernel coverage
//! cannot hide behind adapter code". A workspace total cannot express that: a kernel with
//! no tests and a well-tested adapter average out to a number that passes. So this module
//! buckets `cargo llvm-cov`'s JSON export by the crate each file belongs to and gates every
//! crate on its own.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::graph::PackageGraph;

/// The gate, in basis points of covered lines: 8500 is 85.00%.
///
/// Basis points rather than a float so the comparison is exact integer arithmetic. A
/// crate at 84.999% is below the gate, and no rounding mode gets a say in it.
pub const MINIMUM_LINE_COVERAGE_BASIS_POINTS: u64 = 8_500;

/// One basis point per hundredth of a percent.
const BASIS_POINTS_PER_UNIT: u64 = 10_000;

/// A workspace crate and the directory its sources live under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateRoot {
    /// The package name.
    pub name: String,
    /// The directory containing the package manifest.
    pub directory: PathBuf,
}

/// Line coverage for one crate, or for the workspace as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateCoverage {
    /// The package name.
    pub name: String,
    /// Coverable lines the instrumented build reported.
    pub lines: u64,
    /// How many of them were executed.
    pub covered: u64,
}

impl CrateCoverage {
    /// Covered lines in basis points, or `None` when the crate has nothing to cover.
    ///
    /// The distinction matters: a crate with no coverable lines is not a crate at 0%.
    /// At rung 0.0 the three firmware crates are documentation and attributes, and a gate
    /// that failed them would be measuring the absence of code rather than the absence of
    /// tests. The moment one of them has a function, it has lines, and the gate applies.
    #[must_use]
    pub const fn percent_basis_points(&self) -> Option<u64> {
        // `checked_div` is what makes "no coverable lines" a `None` rather than a
        // division by zero. An overflowing multiply is reported as 0% rather than as
        // `None`, so a line count no real crate has fails the gate instead of being
        // mistaken for a crate with nothing to cover.
        match self.covered.checked_mul(BASIS_POINTS_PER_UNIT) {
            Some(scaled) => scaled.checked_div(self.lines),
            None => Some(0),
        }
    }

    /// The percentage as `85.00%`, or `n/a` where there is nothing to cover.
    #[must_use]
    pub fn render_percent(&self) -> String {
        self.percent_basis_points()
            .map_or_else(|| "n/a".to_owned(), render_basis_points)
    }
}

/// Renders basis points as a percentage with two decimal places.
fn render_basis_points(basis_points: u64) -> String {
    format!("{}.{:02}%", basis_points / 100, basis_points % 100)
}

/// Per-crate line coverage for the whole workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageReport {
    crates: Vec<CrateCoverage>,
}

impl CoverageReport {
    /// Every workspace crate, ordered by name, including those with nothing to cover.
    ///
    /// A crate that vanishes from the report is the failure mode this gate exists to stop,
    /// so a crate with no measured lines gets a row saying so rather than no row at all.
    #[must_use]
    pub fn crates(&self) -> &[CrateCoverage] {
        &self.crates
    }

    /// The row for `name`, if the workspace has that crate.
    #[must_use]
    pub fn crate_named(&self, name: &str) -> Option<&CrateCoverage> {
        self.crates.iter().find(|entry| entry.name == name)
    }

    /// The workspace total, which the gate deliberately does not use.
    ///
    /// It is reported because it is the number people ask for, and it is not gated because
    /// a total is exactly how an untested kernel hides behind a tested adapter.
    #[must_use]
    pub fn total(&self) -> CrateCoverage {
        CrateCoverage {
            name: "TOTAL".to_owned(),
            lines: self.crates.iter().map(|entry| entry.lines).sum(),
            covered: self.crates.iter().map(|entry| entry.covered).sum(),
        }
    }

    /// Every crate below `minimum`, in report order.
    #[must_use]
    pub fn shortfalls(&self, minimum: u64) -> Vec<&CrateCoverage> {
        self.crates
            .iter()
            .filter(|entry| {
                entry
                    .percent_basis_points()
                    .is_some_and(|got| got < minimum)
            })
            .collect()
    }

    /// A table with one row per crate, plus the workspace total.
    #[must_use]
    pub fn render(&self, minimum: u64) -> String {
        let width = self
            .crates
            .iter()
            .map(|entry| entry.name.len())
            .max()
            .unwrap_or(0)
            .max("TOTAL".len());

        let total = self.total();
        let mut table = vec![format!(
            "line coverage, gate {} per crate\n",
            render_basis_points(minimum)
        )];
        for entry in self.crates.iter().chain(core::iter::once(&total)) {
            let verdict = match entry.percent_basis_points() {
                None => "no coverable lines",
                Some(got) if got < minimum => "BELOW GATE",
                Some(_) => "ok",
            };
            table.push(format!(
                "  {:<width$}  {:>8}  {}/{}  {verdict}\n",
                entry.name,
                entry.render_percent(),
                entry.covered,
                entry.lines,
            ));
        }
        table.concat()
    }

    /// Why the gate failed, or `None` if it did not.
    #[must_use]
    pub fn shortfall_report(&self, minimum: u64) -> Option<String> {
        let shortfalls = self.shortfalls(minimum);
        if shortfalls.is_empty() {
            return None;
        }
        let mut message = vec![format!(
            "{} crate(s) below the {} line-coverage gate:",
            shortfalls.len(),
            render_basis_points(minimum)
        )];
        for entry in shortfalls {
            message.push(format!(
                "\n  {} at {} ({}/{} lines)",
                entry.name,
                entry.render_percent(),
                entry.covered,
                entry.lines
            ));
        }
        message.push(
            "\n\nThe gate is per crate on purpose: a workspace total lets an untested crate hide behind a tested one."
                .to_owned(),
        );
        Some(message.concat())
    }
}

/// The coverage report could not be read, so the gate does not know whether it passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageError {
    message: String,
}

impl CoverageError {
    /// Records why the report could not be used.
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CoverageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CoverageError {}

/// Every workspace member, with the directory its manifest sits in.
///
/// Registry dependencies are excluded: their coverage is not this workspace's to gate,
/// and including them would drown the report.
#[must_use]
pub fn crate_roots(graph: &PackageGraph) -> Vec<CrateRoot> {
    let mut roots: Vec<CrateRoot> = graph
        .workspace_members()
        .iter()
        .filter_map(|id| graph.by_id(id))
        .filter_map(|package| {
            let directory = package.manifest_path.as_ref()?.parent()?.to_path_buf();
            Some(CrateRoot {
                name: package.name.clone(),
                directory,
            })
        })
        .collect();
    roots.sort_by(|left, right| left.name.cmp(&right.name));
    roots
}

/// Buckets `cargo llvm-cov`'s JSON export by the crate each file belongs to.
///
/// # Errors
///
/// Returns [`CoverageError`] if the export is not the JSON shape llvm-cov produces, if an
/// entry reports more covered lines than it has, or if it attributes no file to any crate
/// in this workspace. All three fail the gate rather than silently contributing nothing.
///
/// That last one is the gate's own fail-open: bucketing is by path prefix, so a report
/// produced in a different checkout — a downloaded CI artifact, a container with a
/// different working directory — matches no crate, every row reads "no coverable lines",
/// and the gate would otherwise pass with nothing measured.
pub fn summarize(json: &str, roots: &[CrateRoot]) -> Result<CoverageReport, CoverageError> {
    let document: Value = serde_json::from_str(json)
        .map_err(|err| CoverageError::new(format!("could not parse the coverage report: {err}")))?;

    let exports = document
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| CoverageError::new("the coverage report has no `data` array"))?;

    let mut crates: Vec<CrateCoverage> = roots
        .iter()
        .map(|root| CrateCoverage {
            name: root.name.clone(),
            lines: 0,
            covered: 0,
        })
        .collect();
    crates.sort_by(|left, right| left.name.cmp(&right.name));

    let mut seen: Vec<&str> = Vec::new();
    let mut attributed = 0_usize;

    for export in exports {
        let files = export
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| CoverageError::new("a coverage export has no `files` array"))?;

        for file in files {
            let filename = file
                .get("filename")
                .and_then(Value::as_str)
                .ok_or_else(|| CoverageError::new("a coverage entry has no `filename`"))?;
            let lines = file.get("summary").and_then(|summary| summary.get("lines"));
            let count = lines
                .and_then(|lines| lines.get("count"))
                .and_then(Value::as_u64);
            let covered = lines
                .and_then(|lines| lines.get("covered"))
                .and_then(Value::as_u64);
            let (Some(count), Some(covered)) = (count, covered) else {
                return Err(CoverageError::new(format!(
                    "the coverage entry for {filename} has no line summary"
                )));
            };
            if covered > count {
                return Err(CoverageError::new(format!(
                    "the coverage entry for {filename} reports {covered} covered lines out of {count}"
                )));
            }

            // llvm-cov emits one entry per file, but a report assembled from several runs
            // can list the same file more than once, and summing it twice would inflate a
            // crate past the gate.
            if seen.contains(&filename) {
                continue;
            }
            seen.push(filename);

            let Some(owner) = owning_crate(Path::new(filename), roots) else {
                continue;
            };
            if let Some(entry) = crates.iter_mut().find(|entry| entry.name == owner) {
                entry.lines = entry.lines.saturating_add(count);
                entry.covered = entry.covered.saturating_add(covered);
                attributed = attributed.saturating_add(1);
            }
        }
    }

    if attributed == 0 {
        return Err(CoverageError::new(format!(
            "the coverage report attributes none of its {} file(s) to a crate in this workspace, so nothing was measured; it was probably produced somewhere else",
            seen.len()
        )));
    }

    Ok(CoverageReport { crates })
}

/// Where `cargo xtask coverage` writes the export it then gates.
pub const REPORT_PATH: &str = "target/waymaker-coverage.json";

/// Environment variables `cargo llvm-cov` sets, which a child cargo must not inherit.
///
/// `cargo llvm-cov` installs itself as `RUSTC_WRAPPER` and passes its instructions through
/// these variables. A cargo process spawned underneath it — a test that shells out to
/// `cargo build --target thumbv6m-none-eabi`, say — inherits them and compiles the firmware
/// instrumented, which fails: there is no `profiler_builtins` for that target. The prefixes
/// cover the several names cargo-llvm-cov uses without pinning the exact set, which is an
/// implementation detail of a tool this repository does not own.
pub const INSTRUMENTATION_ENV_PREFIXES: &[&str] = &[
    "CARGO_LLVM_COV",
    "__CARGO_LLVM_COV",
    "LLVM_PROFILE",
    "__LLVM_PROFILE",
    "RUSTC_WRAPPER",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
];

/// Variables that steer `llvm-cov` itself, which the coverage run must not inherit either.
///
/// `LLVM_COV_FLAGS` is forwarded to `llvm-cov`, so `--ignore-filename-regex` set in a
/// workflow's `env:` block would quietly remove files from the report the gate then passes.
/// The gate decides what it measures.
const REPORT_STEERING_ENV: &[&str] = &["LLVM_COV_FLAGS", "LLVM_PROFDATA_FLAGS"];

/// The names in `environment` that a child cargo must not inherit from a coverage run.
///
/// Pure so that the prefix matching is testable without a coverage run to inherit from.
#[must_use]
pub fn instrumentation_variables<'a, I>(environment: I) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    environment
        .into_iter()
        .filter(|name| {
            INSTRUMENTATION_ENV_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
        .collect()
}

/// A `cargo` command with any coverage instrumentation stripped from its environment.
///
/// Use this for every cargo process spawned from a test: under `cargo llvm-cov` the
/// inherited environment makes a firmware build fail, and — worse — makes it succeed as a
/// stale cache hit, so the test verifies nothing and nobody notices.
#[must_use]
pub fn uninstrumented_cargo() -> std::process::Command {
    let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    let mut command = std::process::Command::new(cargo);
    let names: Vec<String> = std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| {
            INSTRUMENTATION_ENV_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
        .collect();
    for name in names {
        command.env_remove(name);
    }
    // A developer's own wrapper, which cargo-llvm-cov saves before installing its own.
    if let Some(wrapper) = std::env::var_os("__CARGO_LLVM_COV_RUSTC_WRAPPER_PRE_EXISTING") {
        command.env("RUSTC_WRAPPER", wrapper);
    }
    command
}

/// What to install when the coverage tool is missing.
const INSTALL_HINT: &str = "install it with `cargo install cargo-llvm-cov`, or with taiki-e/install-action@cargo-llvm-cov in CI";

/// Measures the workspace at `root` and buckets the result per crate.
///
/// With `report`, an export produced earlier is gated instead of running the tool again;
/// without it, `cargo llvm-cov` is run over the same workspace and feature selection the
/// rest of the pipeline uses.
///
/// # Errors
///
/// Returns [`CoverageError`] if the workspace cannot be resolved, if `cargo llvm-cov`
/// cannot be run or fails, or if the export cannot be read. The gate fails closed: a
/// coverage run that did not happen is not a coverage run that passed.
pub fn measure(root: &Path, report: Option<&Path>) -> Result<CoverageReport, CoverageError> {
    let metadata = crate::run_cargo_metadata(root)
        .map_err(|err| CoverageError::new(format!("could not resolve the workspace: {err}")))?;
    let graph = PackageGraph::from_cargo_metadata(&metadata)
        .map_err(|err| CoverageError::new(format!("could not parse cargo metadata: {err}")))?;
    let roots = crate_roots(&graph);

    let json = if let Some(path) = report {
        read_report(path)?
    } else {
        let path = root.join(REPORT_PATH);
        run_llvm_cov(root, &path)?;
        read_report(&path)?
    };

    let summary = summarize(&json, &roots)?;
    check_nothing_vanished(&graph, &summary)?;
    Ok(summary)
}

/// Fails when a crate that declares code contributed no coverable lines.
///
/// "No coverable lines" is the gate's one passing state that is not a measurement, so it is
/// also the state anything hidden from the measurement lands in. This is what stops it being
/// a place to hide.
fn check_nothing_vanished(
    graph: &PackageGraph,
    report: &CoverageReport,
) -> Result<(), CoverageError> {
    for entry in report.crates() {
        if entry.lines != 0 {
            continue;
        }
        let Some(source) = graph
            .find(&entry.name)
            .and_then(|package| package.lib_source_path.as_ref())
        else {
            continue;
        };
        let Ok(contents) = std::fs::read_to_string(source) else {
            continue;
        };
        if declares_executable_code(&contents) {
            return Err(CoverageError::new(format!(
                "{} declares code in {} but contributed no coverable lines; it was not measured, which is not the same as being covered",
                entry.name,
                source.display()
            )));
        }
    }
    Ok(())
}

fn read_report(path: &Path) -> Result<String, CoverageError> {
    std::fs::read_to_string(path).map_err(|err| {
        CoverageError::new(format!(
            "could not read the coverage report at {}: {err}",
            path.display()
        ))
    })
}

/// Runs `cargo llvm-cov` over the workspace, writing its JSON export to `output`.
fn run_llvm_cov(root: &Path, output: &Path) -> Result<(), CoverageError> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            CoverageError::new(format!(
                "could not create {} for the coverage report: {err}",
                parent.display()
            ))
        })?;
    }

    let mut command = std::process::Command::new(
        std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from),
    );
    for name in REPORT_STEERING_ENV {
        command.env_remove(name);
    }
    let status = command
        .current_dir(root)
        // The same workspace and feature selection as the test stage, so the gate measures
        // what the pipeline runs rather than a differently configured build.
        //
        // `--summary-only` because the gate reads per-file line totals and nothing else;
        // the full export carries every coverage segment and is two orders of magnitude
        // larger for no gain.
        .args([
            "llvm-cov",
            "--locked",
            "--workspace",
            "--no-default-features",
            "--json",
            "--summary-only",
            "--output-path",
        ])
        .arg(output)
        .status()
        .map_err(|err| {
            CoverageError::new(format!(
                "could not run cargo llvm-cov: {err}; {INSTALL_HINT}"
            ))
        })?;

    if status.success() {
        Ok(())
    } else {
        Err(CoverageError::new(format!(
            "cargo llvm-cov failed ({status}); if the subcommand is missing, {INSTALL_HINT}"
        )))
    }
}

/// Whether a crate root looks like it contains code the coverage run should have measured.
///
/// A crate reporting no coverable lines is legitimate at rung 0.0 — the firmware crates are
/// documentation and attributes. It is also what a crate looks like when it has been hidden
/// from the measurement: `[lib] test = false`, an exclusion regex, code behind a feature the
/// coverage run does not enable. This distinguishes the two by the only evidence available
/// without compiling anything: whether the crate root declares a function or an impl.
///
/// Deliberately shallow. It is a tripwire on "this crate vanished", not a parser; a crate
/// whose only code lives in a module file will not trip it, and that is a smaller gap than
/// the one it closes.
#[must_use]
pub fn declares_executable_code(source: &str) -> bool {
    source
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .any(|line| {
            line.starts_with("fn ")
                || line.starts_with("impl ")
                || line.contains(" fn ")
                || line.starts_with("impl<")
        })
}

/// The crate whose root is the longest prefix of `file`.
///
/// Longest wins so that a crate nested inside another crate's directory is attributed to
/// itself. The comparison is by path component, so `/w/xtask-helpers` is not swallowed by
/// the `/w/xtask` root the way a string prefix would swallow it.
fn owning_crate<'a>(file: &Path, roots: &'a [CrateRoot]) -> Option<&'a str> {
    roots
        .iter()
        .filter(|root| file.starts_with(&root.directory))
        .max_by_key(|root| root.directory.components().count())
        .map(|root| root.name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn roots() -> Vec<CrateRoot> {
        ["waymaker-core", "waymaker-flash", "xtask"]
            .into_iter()
            .map(|name| CrateRoot {
                name: name.to_owned(),
                directory: PathBuf::from(format!("/w/{name}")),
            })
            .collect()
    }

    /// A minimal llvm-cov export: `data[0].files[*].summary.lines.{count,covered}`.
    fn export(files: &[(&str, u64, u64)]) -> String {
        let entries: Vec<String> = files
            .iter()
            .map(|(filename, count, covered)| {
                format!(
                    r#"{{"filename":"{filename}","summary":{{"lines":{{"count":{count},"covered":{covered},"percent":0.0}}}}}}"#
                )
            })
            .collect();
        format!(
            r#"{{"version":"3.1.0","type":"llvm.coverage.json.export","data":[{{"files":[{}],"totals":{{}}}}]}}"#,
            entries.join(",")
        )
    }

    fn summarize_files(files: &[(&str, u64, u64)]) -> CoverageReport {
        summarize(&export(files), &roots()).expect("the export should be summarizable")
    }

    #[test]
    fn every_file_is_bucketed_into_the_crate_that_owns_it() {
        let report = summarize_files(&[
            ("/w/waymaker-core/src/lib.rs", 100, 90),
            ("/w/waymaker-core/src/cursor.rs", 100, 80),
            ("/w/xtask/src/lib.rs", 200, 200),
        ]);

        let core = report
            .crate_named("waymaker-core")
            .expect("core is a crate");
        assert_eq!((core.lines, core.covered), (200, 170));
        let xtask = report.crate_named("xtask").expect("xtask is a crate");
        assert_eq!((xtask.lines, xtask.covered), (200, 200));
    }

    #[test]
    fn a_file_outside_every_crate_root_is_ignored() {
        let report = summarize_files(&[
            ("/home/me/.cargo/registry/src/serde/lib.rs", 1000, 0),
            ("/w/xtask/src/lib.rs", 100, 100),
        ]);
        assert_eq!(report.crates().len(), 3, "one row per workspace crate");
        let xtask = report.crate_named("xtask").expect("xtask is a crate");
        assert_eq!(xtask.lines, 100);
    }

    #[test]
    fn the_longest_matching_crate_root_wins() {
        // A crate nested inside another crate's directory belongs to the nested crate.
        let roots = vec![
            CrateRoot {
                name: "outer".to_owned(),
                directory: PathBuf::from("/w"),
            },
            CrateRoot {
                name: "inner".to_owned(),
                directory: PathBuf::from("/w/crates/inner"),
            },
        ];
        let report = summarize(&export(&[("/w/crates/inner/src/lib.rs", 10, 10)]), &roots)
            .expect("summarizable");
        assert_eq!(
            report.crate_named("inner").expect("inner is a crate").lines,
            10
        );
        assert_eq!(
            report.crate_named("outer").expect("outer is a crate").lines,
            0
        );
    }

    #[test]
    fn a_sibling_directory_with_a_shared_prefix_is_not_captured() {
        // `/w/xtask-helpers` must not be swallowed by the `/w/xtask` root.
        let report = summarize_files(&[
            ("/w/xtask-helpers/src/lib.rs", 50, 0),
            ("/w/waymaker-core/src/lib.rs", 10, 10),
        ]);
        assert_eq!(
            report.crate_named("xtask").expect("xtask is a crate").lines,
            0
        );
    }

    #[test]
    fn a_report_that_matches_no_crate_in_this_workspace_is_an_error() {
        // The gate's own fail-open. Bucketing is by path prefix, so an export produced in
        // another checkout matches nothing, every row reads "no coverable lines", and the
        // gate would otherwise pass having measured not one line.
        let error = summarize(
            &export(&[("/some/other/checkout/src/lib.rs", 100, 0)]),
            &roots(),
        )
        .expect_err("a report about somewhere else must fail closed");
        assert!(
            error.to_string().contains("nothing was measured"),
            "{error}"
        );
    }

    #[test]
    fn an_empty_report_is_an_error() {
        let error =
            summarize(&export(&[]), &roots()).expect_err("an empty report must fail closed");
        assert!(
            error.to_string().contains("nothing was measured"),
            "{error}"
        );
    }

    #[test]
    fn a_file_listed_twice_is_counted_once() {
        // A report stitched together from several runs can repeat a file; summing it twice
        // would push a crate over the gate on the strength of one well-covered file.
        let report = summarize_files(&[
            ("/w/waymaker-core/src/lib.rs", 100, 100),
            ("/w/waymaker-core/src/lib.rs", 100, 100),
            ("/w/waymaker-core/src/cursor.rs", 100, 0),
        ]);
        let core = report
            .crate_named("waymaker-core")
            .expect("core is a crate");
        assert_eq!((core.lines, core.covered), (200, 100));
        assert_eq!(
            report.shortfalls(MINIMUM_LINE_COVERAGE_BASIS_POINTS).len(),
            1
        );
    }

    #[test]
    fn a_crate_with_no_coverable_lines_passes_and_says_so() {
        // Rung 0.0: the firmware crates are documentation and attributes. A crate with
        // nothing to cover is not a crate at 0%.
        let report = summarize_files(&[("/w/xtask/src/lib.rs", 100, 100)]);
        let core = report
            .crate_named("waymaker-core")
            .expect("core is a crate");
        assert_eq!(core.lines, 0);
        assert!(core.percent_basis_points().is_none());
        assert!(
            report
                .shortfalls(MINIMUM_LINE_COVERAGE_BASIS_POINTS)
                .is_empty()
        );
        assert!(
            report
                .render(MINIMUM_LINE_COVERAGE_BASIS_POINTS)
                .contains("n/a"),
            "{}",
            report.render(MINIMUM_LINE_COVERAGE_BASIS_POINTS)
        );
    }

    #[test]
    fn a_crate_below_the_minimum_fails_the_gate() {
        let report = summarize_files(&[
            ("/w/waymaker-core/src/lib.rs", 100, 84),
            ("/w/xtask/src/lib.rs", 100, 100),
        ]);
        let shortfalls = report.shortfalls(MINIMUM_LINE_COVERAGE_BASIS_POINTS);
        assert_eq!(shortfalls.len(), 1);
        assert_eq!(shortfalls[0].name, "waymaker-core");
    }

    #[test]
    fn each_row_carries_the_verdict_the_gate_reached_for_it() {
        let report = summarize_files(&[
            ("/w/waymaker-core/src/lib.rs", 100, 84),
            ("/w/xtask/src/lib.rs", 100, 100),
        ]);
        let rendered = report.render(MINIMUM_LINE_COVERAGE_BASIS_POINTS);
        let row = |name: &str| {
            rendered
                .lines()
                .find(|line| line.trim_start().starts_with(name))
                .unwrap_or_default()
                .to_owned()
        };
        assert!(row("waymaker-core").contains("BELOW GATE"), "{rendered}");
        assert!(row("xtask").ends_with("ok"), "{rendered}");
        assert!(
            row("waymaker-flash").contains("no coverable lines"),
            "{rendered}"
        );
        // The total is 92% and reads `ok` while a crate is below the gate. That is the
        // whole argument for gating per crate, printed on one screen.
        assert!(row("TOTAL").ends_with("ok"), "{rendered}");
    }

    #[test]
    fn a_crate_exactly_at_the_minimum_passes() {
        let report = summarize_files(&[("/w/waymaker-core/src/lib.rs", 100, 85)]);
        assert!(
            report
                .shortfalls(MINIMUM_LINE_COVERAGE_BASIS_POINTS)
                .is_empty()
        );
    }

    #[test]
    fn a_crate_a_fraction_below_the_minimum_fails() {
        // 8499/10000 of a line is still below the gate; truncation must not round up.
        let report = summarize_files(&[("/w/waymaker-core/src/lib.rs", 10_000, 8_499)]);
        assert_eq!(
            report
                .crate_named("waymaker-core")
                .expect("core is a crate")
                .percent_basis_points(),
            Some(8_499)
        );
        assert_eq!(
            report.shortfalls(MINIMUM_LINE_COVERAGE_BASIS_POINTS).len(),
            1
        );
    }

    #[test]
    fn the_gate_is_eighty_five_percent() {
        assert_eq!(MINIMUM_LINE_COVERAGE_BASIS_POINTS, 8_500);
    }

    #[test]
    fn a_well_covered_crate_cannot_hide_a_bare_one() {
        // The acceptance criterion: the workspace total here is 92%, comfortably over the
        // gate, and the gate still fails.
        let report = summarize_files(&[
            ("/w/xtask/src/lib.rs", 900, 900),
            ("/w/waymaker-core/src/lib.rs", 100, 20),
        ]);
        assert_eq!(report.total().percent_basis_points(), Some(9_200));
        let shortfalls = report.shortfalls(MINIMUM_LINE_COVERAGE_BASIS_POINTS);
        assert_eq!(shortfalls.len(), 1);
        assert_eq!(shortfalls[0].name, "waymaker-core");
    }

    #[test]
    fn the_report_renders_one_row_per_crate_with_its_percentage() {
        let report = summarize_files(&[("/w/waymaker-core/src/lib.rs", 200, 101)]);
        let rendered = report.render(MINIMUM_LINE_COVERAGE_BASIS_POINTS);
        assert!(rendered.contains("waymaker-core"), "{rendered}");
        assert!(rendered.contains("50.50%"), "{rendered}");
        assert!(rendered.contains("101/200"), "{rendered}");
        for root in roots() {
            assert!(rendered.contains(&root.name), "{rendered}");
        }
    }

    #[test]
    fn the_rows_are_ordered_by_crate_name() {
        // The roots deliberately arrive out of order: a report whose rows follow the
        // workspace's declaration order is a report whose diff churns for no reason.
        let unsorted = ["zulu", "alpha", "mike"]
            .into_iter()
            .map(|name| CrateRoot {
                name: name.to_owned(),
                directory: PathBuf::from(format!("/w/{name}")),
            })
            .collect::<Vec<_>>();
        let report = summarize(&export(&[("/w/alpha/src/lib.rs", 10, 10)]), &unsorted)
            .expect("the export should be summarizable");

        let names: Vec<&str> = report.crates().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["alpha", "mike", "zulu"]);
    }

    #[test]
    fn malformed_json_is_an_error_rather_than_a_pass() {
        let error = summarize("not json", &roots()).expect_err("a broken report must fail closed");
        assert!(error.to_string().contains("coverage report"), "{error}");
    }

    #[test]
    fn a_report_without_a_data_section_is_an_error() {
        let error = summarize(r#"{"version":"3.1.0"}"#, &roots())
            .expect_err("a report with no data must fail closed");
        assert!(error.to_string().contains("data"), "{error}");
    }

    #[test]
    fn a_file_entry_without_a_line_summary_is_an_error() {
        let json = r#"{"data":[{"files":[{"filename":"/w/xtask/src/lib.rs"}]}]}"#;
        let error = summarize(json, &roots()).expect_err("an unreadable entry must fail closed");
        assert!(error.to_string().contains("lib.rs"), "{error}");
    }

    #[test]
    fn a_covered_count_above_the_line_count_is_an_error() {
        let error = summarize(&export(&[("/w/xtask/src/lib.rs", 10, 11)]), &roots())
            .expect_err("an impossible summary must fail closed");
        assert!(error.to_string().contains("covered"), "{error}");
    }

    #[test]
    fn an_export_without_a_files_array_is_an_error() {
        let error = summarize(r#"{"data":[{"totals":{}}]}"#, &roots())
            .expect_err("an export with no files must fail closed");
        assert!(error.to_string().contains("files"), "{error}");
    }

    #[test]
    fn a_file_entry_without_a_filename_is_an_error() {
        let json = r#"{"data":[{"files":[{"summary":{"lines":{"count":1,"covered":1}}}]}]}"#;
        let error = summarize(json, &roots()).expect_err("a nameless entry must fail closed");
        assert!(error.to_string().contains("filename"), "{error}");
    }

    #[test]
    fn a_line_count_too_large_to_scale_is_reported_as_zero_rather_than_as_nothing() {
        // Not reachable from any real crate. It is pinned because the alternative — an
        // overflow that returns `None` — would read as "no coverable lines" and pass.
        let absurd = CrateCoverage {
            name: "absurd".to_owned(),
            lines: u64::MAX,
            covered: u64::MAX,
        };
        assert_eq!(absurd.percent_basis_points(), Some(0));
        assert_eq!(absurd.render_percent(), "0.00%");
    }

    #[test]
    fn a_package_with_no_manifest_path_contributes_no_crate_root() {
        const METADATA: &str = r#"{
          "packages": [
            { "id": "ghost", "name": "ghost", "source": null,
              "dependencies": [], "features": {}, "targets": [] }
          ],
          "workspace_members": ["ghost"],
          "resolve": { "nodes": [ { "id": "ghost", "deps": [] } ] }
        }"#;
        let graph =
            crate::graph::PackageGraph::from_cargo_metadata(METADATA).expect("metadata parses");
        assert!(crate_roots(&graph).is_empty());
    }

    #[test]
    fn a_crate_root_with_a_function_declares_executable_code() {
        assert!(declares_executable_code("pub fn counter() -> u8 { 0 }\n"));
        assert!(declares_executable_code("fn private() {}\n"));
        assert!(declares_executable_code(
            "impl Cursor {\n    fn step(&self) {}\n}\n"
        ));
        assert!(declares_executable_code(
            "    pub const fn zero() -> u8 { 0 }"
        ));
    }

    #[test]
    fn a_crate_root_of_documentation_and_attributes_declares_none() {
        // The rung 0.0 firmware crates, which legitimately report no coverable lines.
        let scaffolding = "//! Docs about a fn that does not exist yet.\n\
                           #![no_std]\n#![forbid(unsafe_code)]\n";
        assert!(!declares_executable_code(scaffolding));
    }

    #[test]
    fn crate_roots_are_taken_from_the_workspace_members() {
        const METADATA: &str = r#"{
          "packages": [
            { "id": "core", "name": "waymaker-core", "source": null,
              "manifest_path": "/w/crates/waymaker-core/Cargo.toml",
              "dependencies": [], "features": {}, "targets": [] },
            { "id": "dep", "name": "serde", "source": "registry+https://crates.io",
              "manifest_path": "/reg/serde/Cargo.toml",
              "dependencies": [], "features": {}, "targets": [] }
          ],
          "workspace_members": ["core"],
          "resolve": { "nodes": [ { "id": "core", "deps": [] }, { "id": "dep", "deps": [] } ] }
        }"#;
        let graph =
            crate::graph::PackageGraph::from_cargo_metadata(METADATA).expect("metadata parses");
        let roots = crate_roots(&graph);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "waymaker-core");
        assert_eq!(roots[0].directory, PathBuf::from("/w/crates/waymaker-core"));
    }

    #[test]
    fn a_shortfall_names_the_crate_the_gate_and_what_was_measured() {
        let report = summarize_files(&[("/w/waymaker-core/src/lib.rs", 100, 20)]);
        let message = report.shortfall_report(MINIMUM_LINE_COVERAGE_BASIS_POINTS);
        let message = message.expect("a shortfall should produce a message");
        assert!(message.contains("waymaker-core"), "{message}");
        assert!(message.contains("20.00%"), "{message}");
        assert!(message.contains("85.00%"), "{message}");
    }

    #[test]
    fn a_passing_report_produces_no_shortfall_message() {
        let report = summarize_files(&[("/w/waymaker-core/src/lib.rs", 100, 100)]);
        assert!(
            report
                .shortfall_report(MINIMUM_LINE_COVERAGE_BASIS_POINTS)
                .is_none()
        );
    }
}
