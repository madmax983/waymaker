//! `cargo xtask` — the workspace's own policy gate.
//!
//! Running it locally and running it in CI are the same command, so CI is confirmation
//! rather than discovery.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask check-layering";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next();

    match command.as_deref() {
        Some("check-layering" | "check") | None => run_check(),
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
            println!("workspace policy: ok ({} rules)", rule_count());
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

const fn rule_count() -> usize {
    8
}

/// The workspace root, which is this crate's parent directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}
