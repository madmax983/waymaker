#!/usr/bin/env bash
# Deterministic instruction-count profile of `cargo xtask check-layering`.
#
# `check-layering` is the workspace's own realistic workload: it is the last stage of
# every CI run and the command every contributor is told to trust in CLAUDE.md's "Run
# this before you claim anything works" list. It reads every source file `collect_inputs`
# gathers from the real workspace and runs all 57 rules over them (`check_inputs`), so a
# profile of it is a profile of the gate doing its actual job, not a synthetic slice of a
# function chosen in advance.
#
# Wall-clock time on this hardware is not admissible evidence (shared vCPU, no isolation).
# Instruction count under `valgrind --tool=callgrind` is: it is deterministic to within a
# handful of instructions run to run, because it counts what the CPU executed rather than
# how long that took.
#
# Usage:
#   xtask/bench/check_layering_callgrind.sh
#
# Prints one line: `Ir <count>`. Requires `valgrind` and a `profiling`-profile build of
# `xtask` (built automatically if missing) so callgrind can attribute cost to real symbols
# instead of stripped addresses — `[profile.profiling]` in the workspace `Cargo.toml`
# turns off `strip` and fat LTO for exactly this reason (see its own doc comment).
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

cargo build --locked -p xtask --profile profiling >/dev/null

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

out_file="$work_dir/callgrind.out"
valgrind --tool=callgrind --callgrind-out-file="$out_file" \
    ./target/profiling/xtask check-layering >"$work_dir/stdout" 2>"$work_dir/stderr" || {
    cat "$work_dir/stdout" "$work_dir/stderr" >&2
    echo "check-layering did not exit 0 under callgrind" >&2
    exit 1
}

ir="$(grep -oP 'I\s+refs:\s+\K[0-9,]+' "$work_dir/stderr" | tr -d ',')"
if [ -z "$ir" ]; then
    echo "could not read an instruction count from valgrind's own output" >&2
    cat "$work_dir/stderr" >&2
    exit 1
fi

echo "Ir $ir"
