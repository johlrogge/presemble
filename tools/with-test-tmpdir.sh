#!/usr/bin/env bash
# with-test-tmpdir.sh — run a command (tests, the smoketest, a bench) with
# TMPDIR set to a fresh directory of its own, target/tmp/run-<pid>, and
# remove that directory afterwards: on success, on failure and on Ctrl-C.
#
# Rust's `tempfile` and `mktemp` both honour TMPDIR, so whatever a run writes
# next to its temp dirs lands here, on disk under target/, and goes away with
# it. git does not look for a repository above target/tmp, so a temp dir is
# not inside this one. A site built in a temp dir writes its output to <parent>/output/<name>
# and <parent>/output-editorial/<name>; those are counted before the
# directory is removed, and a run that left any fails: tests keep their
# output inside their own temp dirs (a test site lives at <tmp>/site).
#
# Usage: bash tools/with-test-tmpdir.sh <command> [args…]
# Exit status: the command's when it failed; otherwise 1 when the run left
# output outside its temp dirs, else 0.

set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
run_dir="$root/target/tmp/run-$$"
mkdir -p "$run_dir"
export TMPDIR="$run_dir"
# target/ is inside this repository's work tree: stop git's search for a
# repository at target/tmp, so a temp dir is "not in a repository" as it is
# under /tmp (a test may still `git init` inside its own temp dir).
export GIT_CEILING_DIRECTORIES="$root/target/tmp${GIT_CEILING_DIRECTORIES:+:$GIT_CEILING_DIRECTORIES}"

finish() {
    local status=$?
    local leaked
    leaked=$(find "$run_dir" -mindepth 2 -maxdepth 2 -path "$run_dir/output*/*" 2>/dev/null | wc -l)
    rm -rf "$run_dir"
    if [ "$leaked" -gt 0 ]; then
        echo "error: tests left $leaked output directories outside their temp dirs" >&2
        # The command's own failure comes first; a passing run fails here.
        if [ "$status" -eq 0 ]; then
            status=1
        fi
    fi
    exit "$status"
}
trap finish EXIT

"$@"
