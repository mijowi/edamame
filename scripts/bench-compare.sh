#!/usr/bin/env bash
# Bench a base revision and the current tree back to back, and report each case's change.
#
#   scripts/bench-compare.sh            # compare against develop (for a pull request)
#   scripts/bench-compare.sh v0.1.6     # compare against any commit, branch or tag
#
# The base is checked out into a temporary worktree and built into this checkout's
# target/, so both builds share a toolchain, the dependency builds, and the machine's
# state during the run.  The current tree includes uncommitted changes.
#
# Environment:
#   EDAMAME_BENCH_CORE=N     pin both runs to CPU N with taskset (Linux; recommended
#                            on hybrid CPUs)
#   EDAMAME_BENCH_FILTER=RE  criterion filter (default: the end-to-end cases)
#
# Refuses when the bench harness differs between the two, since the runs would time
# different things.  See docs/dev/performance.md for how to read the results.
# Runs on bash 3.2 (macOS's /bin/bash) and later.

set -euo pipefail

filter=${EDAMAME_BENCH_FILTER:-'^(full_pipeline|visual_cache)'}
core=${EDAMAME_BENCH_CORE:-}

# Before the `cd` below, which would break a relative "$0".
case ${1:-} in
    -h | --help) sed -n '2,/^$/{s/^# \{0,1\}//;p;}' "$0"; exit 0 ;;
esac

cd "$(git rev-parse --show-toplevel)"

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }
[[ $# -le 1 ]] || die "usage: $0 [REF]"

ref=${1:-develop}
commit=$(git rev-parse --verify --quiet "$ref^{commit}") || die "unknown revision: $ref"
short=$(git rev-parse --short "$commit")
# Criterion stores a baseline as a directory, so the name can't contain a slash.
baseline=$(printf '%s' "$ref" | tr -c 'A-Za-z0-9._-' '_')

git merge-base --is-ancestor "$commit" HEAD ||
    echo "warning: HEAD is not based on $ref ($short); the comparison includes every difference between them, not just this branch's. Rebase first." >&2
# A local branch that is behind its upstream passes the check above, yet would bench
# someone else's changes as this branch's.
if up=$(git rev-parse --verify --quiet "$ref@{upstream}^{commit}") && [[ $up != "$commit" ]] &&
    git merge-base --is-ancestor "$commit" "$up"; then
    echo "warning: $ref is behind its upstream; update it (git fetch, then fast-forward), or pass the upstream ref instead." >&2
fi

# What defines the bench harness: everything under benches/, the `[[bench]]` tables and
# the `criterion` dependency line in Cargo.toml, and the criterion version Cargo.lock
# resolves.  Reads revision $1, or the working tree when $1 is empty.
bench_harness() {
    local toml lock
    if [[ -n $1 ]]; then
        toml=$(git show "$1:Cargo.toml") && lock=$(git show "$1:Cargo.lock") || return 1
    else
        toml=$(<Cargo.toml) && lock=$(<Cargo.lock) || return 1
    fi
    awk '/^\[/ { on = ($0 == "[[bench]]") } on || /^criterion *=/' <<<"$toml"
    awk '$0 == "name = \"criterion\"" { getline; print }' <<<"$lock"
}

# `git diff` skips untracked files, so a new one under benches/ is checked separately.
if ! git diff --quiet "$commit" -- benches/ ||
    [[ -n $(git ls-files --others --exclude-standard -- benches/) ]]; then
    die "benches/ differs from $ref; the comparison would be meaningless"
fi
old_harness=$(bench_harness "$commit") || die "cannot read Cargo.toml / Cargo.lock at $ref"
[[ $old_harness == "$(bench_harness '')" ]] ||
    die "the criterion dependency or a [[bench]] entry differs from $ref; the comparison would be meaningless"

pin=()
where=unpinned
if [[ -n $core ]]; then
    if command -v taskset >/dev/null; then
        pin=(taskset -c "$core")
        where="on core $core"
    else
        echo "warning: taskset not found; benches run unpinned" >&2
    fi
fi

target_dir="$PWD/target"
worktree=$(mktemp -d)
trap 'git worktree remove --force "$worktree" 2>/dev/null || true; rm -rf "$worktree"' EXIT
step "Checking out $ref ($short) into a temporary worktree"
git worktree add --detach "$worktree" "$commit"

# Build both first, so `taskset` confines only the measured runs, not the compile.
step "Building both bench binaries"
(cd "$worktree" && CARGO_TARGET_DIR="$target_dir" cargo bench --bench pipeline --no-run)
cargo bench --bench pipeline --no-run

step "Benching $ref ($short), $where"
# `${pin[@]+...}`: bash before 4.4 treats an empty array as unset under `set -u`.
(cd "$worktree" && CARGO_TARGET_DIR="$target_dir" ${pin[@]+"${pin[@]}"} \
    cargo bench --bench pipeline -- --noplot "$filter" --save-baseline "$baseline")
step "Benching the current tree against $ref, $where"
${pin[@]+"${pin[@]}"} cargo bench --bench pipeline -- --noplot "$filter" --baseline "$baseline"

echo
echo "A \"regressed\" line is a lead, not a verdict: re-run the case to confirm it."
