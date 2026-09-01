//! `cargo xtask` — the workspace's own policy gate.
//!
//! Running it locally and running it in CI are the same command, so CI is confirmation
//! rather than discovery.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "usage: cargo xtask <check-layering \
    | coverage [--report FILE] \
    | size [--report FILE] [--json FILE] [--baseline-ref REF] [--no-baseline] \
    | install-hooks>";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    let rest: Vec<String> = args.collect();

    match command.as_deref() {
        Some("check-layering" | "check") | None => run_check(),
        Some("coverage") => run_coverage(&rest),
        Some("size") => run_size(&rest),
        Some("install-hooks") => run_install_hooks(),
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("xtask: unknown command `{other}`\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn run_check() -> ExitCode {
    let root = workspace_root();
    match xtask::check_workspace(&root) {
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
        Ok(violations) if violations.is_empty() => {
            println!("workspace policy: ok ({} rules)", xtask::RULES.len());
            ExitCode::SUCCESS
        }
        Ok(violations) => {
            eprintln!("workspace policy: {} violation(s)", violations.len());
            for violation in &violations {
                eprintln!("  {violation}");
            }
            eprintln!(
                "\nThe layering contract is design document §05: waymaker-embassy -> waymaker-flash -> waymaker-core."
            );
            ExitCode::FAILURE
        }
    }
}

/// Measures line coverage and gates every crate on its own.
///
/// `--report FILE` gates an export produced earlier instead of running the tool again,
/// which is what makes this command testable without a coverage run inside a test.
fn run_coverage(args: &[String]) -> ExitCode {
    let report = match parse_report_flag(args) {
        Ok(report) => report,
        Err(message) => {
            eprintln!("xtask: {message}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let root = workspace_root();
    let measured = xtask::coverage::measure(&root, report.as_deref());
    let Ok(report) = measured.inspect_err(|error| eprintln!("xtask: {error}")) else {
        return ExitCode::FAILURE;
    };

    let minimum = xtask::coverage::MINIMUM_LINE_COVERAGE_BASIS_POINTS;
    print!("{}", report.render(minimum));
    report.shortfall_report(minimum).map_or_else(
        || {
            println!("coverage: ok");
            ExitCode::SUCCESS
        },
        |shortfall| {
            eprintln!("{shortfall}");
            ExitCode::FAILURE
        },
    )
}

/// Reads the optional `--report FILE` argument.
fn parse_report_flag(args: &[String]) -> Result<Option<PathBuf>, String> {
    let mut args = args.iter();
    let mut report = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--report" => {
                let path = args
                    .next()
                    .ok_or_else(|| "`--report` needs a path".to_owned())?;
                report = Some(PathBuf::from(path));
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(report)
}

/// Options for `cargo xtask size`.
#[derive(Debug, Default)]
struct SizeOptions {
    /// Gate a report produced earlier instead of linking the matrix again.
    report: Option<PathBuf>,
    /// Where to write the report this run produces.
    json: Option<PathBuf>,
    /// The base branch to diff against; taken from the CI environment when absent.
    baseline_ref: Option<String>,
    /// Skip the base-branch diff entirely.
    no_baseline: bool,
}

/// Links the size matrix, gates it against the budgets, and diffs it against the base.
///
/// `--report FILE` gates a report produced earlier rather than linking the matrix again,
/// which is what lets the command be exercised without a firmware build inside a test.
fn run_size(args: &[String]) -> ExitCode {
    let options = match parse_size_flags(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("xtask: {message}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    let root = workspace_root();
    let measured = options
        .report
        .as_deref()
        .map_or_else(|| xtask::size::measure(&root), xtask::size::read_report);
    let Ok(report) = measured.inspect_err(|error| eprintln!("xtask: {error}")) else {
        return ExitCode::FAILURE;
    };

    print!("{}", report.render());

    // Written before the gate is applied, not after: the run that fails a budget is the
    // run whose numbers somebody wants to look at.
    let json = options
        .json
        .clone()
        .unwrap_or_else(|| root.join(xtask::size::REPORT_PATH));
    if let Err(error) = xtask::size::write_report(&json, &report) {
        eprintln!("xtask: {error}");
        return ExitCode::FAILURE;
    }
    println!("wrote {}", json.display());

    print!("{}", baseline_diff(&root, &options, &report));

    report.shortfall_report().map_or_else(
        || {
            println!("size: ok");
            ExitCode::SUCCESS
        },
        |shortfall| {
            eprintln!("{shortfall}");
            ExitCode::FAILURE
        },
    )
}

/// The diff against the base branch, or a line saying why there is not one.
///
/// Never a failure. A base branch that cannot be measured — a shallow clone, a push
/// rather than a pull request, a base from before this gate existed — is a missing
/// comparison, and a missing comparison is not a budget breach. The budgets themselves
/// are gated whether or not there is anything to compare against.
fn baseline_diff(root: &Path, options: &SizeOptions, report: &xtask::size::SizeReport) -> String {
    if options.no_baseline {
        return String::new();
    }
    let Some(reference) = options
        .baseline_ref
        .clone()
        .or_else(xtask::size::base_ref_from_environment)
    else {
        return "size against the base branch: not compared; no base branch was named and GITHUB_BASE_REF is unset\n".to_owned();
    };

    match xtask::size::measure_baseline(root, &reference) {
        Ok(base) => {
            let rows = xtask::size::render_diff(&xtask::size::diff(&base, report));
            // Appended rather than folded into the row diff: kernel state is the one budget
            // the section sizes cannot see, so a pull request that changes only the
            // registry has no row to move and would otherwise read "no change".
            let kernel_state = xtask::size::kernel_state_change(&base, report)
                .map_or_else(String::new, |change| format!("  {change}\n"));
            format!("{rows}{kernel_state}")
        }
        Err(error) => format!("size against the base branch: not compared; {error}\n"),
    }
}

/// Reads the arguments of `cargo xtask size`.
fn parse_size_flags(args: &[String]) -> Result<SizeOptions, String> {
    let mut args = args.iter();
    let mut options = SizeOptions::default();
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--report" => {
                options.report = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "`--report` needs a path".to_owned())?,
                ));
            }
            "--json" => {
                options.json = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "`--json` needs a path".to_owned())?,
                ));
            }
            "--baseline-ref" => {
                options.baseline_ref = Some(
                    args.next()
                        .ok_or_else(|| "`--baseline-ref` needs a git reference".to_owned())?
                        .clone(),
                );
            }
            "--no-baseline" => options.no_baseline = true,
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(options)
}

/// Generates the pre-commit hook and points git at the directory holding it.
fn run_install_hooks() -> ExitCode {
    let root = workspace_root();
    let path = match xtask::pipeline::install_pre_commit_hook(&root) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("xtask: could not write the pre-commit hook: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!("wrote {}", path.display());

    let configured = Command::new("git")
        .current_dir(&root)
        .args(["config", "core.hooksPath", xtask::pipeline::HOOKS_PATH])
        .status();
    match configured {
        Ok(status) if status.success() => {
            println!("git config core.hooksPath {}", xtask::pipeline::HOOKS_PATH);
        }
        // Not a failure: the hook is written and reviewable either way, and a checkout
        // without git on the path is a checkout that is not about to commit anything.
        _ => eprintln!(
            "xtask: could not set core.hooksPath; run `git config core.hooksPath {}` yourself",
            xtask::pipeline::HOOKS_PATH
        ),
    }
    ExitCode::SUCCESS
}

/// The root of the workspace the binary was invoked in.
///
/// Asked of cargo rather than baked in with `env!("CARGO_MANIFEST_DIR")`: a compile-time
/// path makes the binary check whichever workspace it was built in, no matter where it is
/// run, which is wrong for a cached or copied binary and makes the gate untestable
/// against any workspace but its own. The compile-time path remains the fallback for the
/// case where cargo is not on the path.
fn workspace_root() -> PathBuf {
    locate_workspace_root().unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    })
}

fn locate_workspace_root() -> Option<PathBuf> {
    let cargo = std::env::var_os("CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from);
    let output = Command::new(cargo)
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let manifest = String::from_utf8(output.stdout).ok()?;
    Path::new(manifest.trim()).parent().map(Path::to_path_buf)
}
