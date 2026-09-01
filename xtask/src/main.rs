//! `cargo xtask` — the workspace's own policy gate.
//!
//! Running it locally and running it in CI are the same command, so CI is confirmation
//! rather than discovery.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

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
