#!/usr/bin/env bash
# Bench a base revision and the current tree back to back, and report each case's change.
#
#   scripts/bench-compare.sh            # compare against develop (for a pull request)
#   scripts/bench-compare.sh v0.1.6     # compare against any commit, branch or tag
#
# The base is checked out into a temporary worktree and built into this checkout's
# target/, so both builds share a toolchain, the dependency builds, and the machine's
# state during the run.  The current tree includes uncommitted changes.  A closing summary
# lists the cases criterion judged improved or regressed, both for the base against its own
# previous saved run (a noise check) and for the current tree against the base.
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
# Criterion stores a baseline as a directory, so the name can't contain a slash.  The commit
# is part of it so a re-run's noise check compares the same code even after the ref moves.
baseline=$(printf '%s-%s' "$ref" "$short" | tr -c 'A-Za-z0-9._-' '_')

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

# Print the cases criterion judged "improved" or "regressed" in log $2, under heading $1.
# Each comes with its change interval and the run's time interval; with a third log, also
# that case's time interval there, since a noisy run on either side can produce the verdict.
# Reads criterion's console report, which is a pipe here, so it carries no progress redraws.
summarize() {
    awk -v heading="$1" -v base="${3:-}" '
        function inside(s, parts) { sub(/.*\[/, "", s); sub(/\].*/, "", s); return split(s, parts, " ") }
        function span(s, p, n) { n = inside(s, p); return n == 6 ? p[1] " " p[2] " … " p[5] " " p[6] : "?" }
        function change(s, p, n) { n = inside(s, p); return n == 3 ? p[2] " [" p[1] " … " p[3] "]" : "?" }
        FNR == 1 { name = "" }
        /^(Benchmarking |Found |Warning|Gnuplot )/ { next }
        /^[^ \t]/ { name = $1 }
        / time: +\[/ {
            if (FILENAME == base) { base_time[name] = span($0); next }
            time[name] = span($0); cases++
        }
        FILENAME == base { next }
        / change: +\[/ { delta[name] = change($0); compared++ }
        # Every comparison ends in one of these four; counting them catches a reworded one.
        /^[ \t]+(Performance has (regressed|improved)|Change within noise threshold|No change in performance detected)\.$/ {
            verdicts++
        }
        /Performance has (regressed|improved)\./ {
            verdict = /regressed/ ? "regressed" : "improved"
            out[++hits] = sprintf("  %-9s  %s  %s\n             %s[%s]", verdict, name, delta[name],
                base == "" ? "time " : "base [" base_time[name] "]  →  this tree ", time[name])
        }
        END {
            printf "\n%s\n", heading
            # Without these checks, a change to the report format of criterion would read
            # as "nothing changed".
            if (!cases) {
                print "  warning: found no results in criterion'\''s output; its report format may have"
                print "  changed (after a criterion upgrade?), and this summary needs updating to match"
                exit
            }
            if (!compared) { print "  no earlier baseline to compare with"; exit }
            if (verdicts != compared) {
                printf "  warning: %d of %d comparisons ended in no verdict this summary recognizes; criterion'\''s\n", compared - verdicts, compared
                print "  report format may have changed, so the list below may be incomplete"
            }
            printf "  %d of %d cases changed beyond noise\n", hits, cases
            for (i = 1; i <= hits; i++) print out[i]
        }
    ' ${3:+"$3"} "$2"
}

target_dir="$PWD/target"
worktree=$(mktemp -d)
logs=$(mktemp -d)
trap 'git worktree remove --force "$worktree" 2>/dev/null || true; rm -rf "$worktree" "$logs"' EXIT
step "Checking out $ref ($short) into a temporary worktree"
git worktree add --detach "$worktree" "$commit"

# Build both first, so `taskset` confines only the measured runs, not the compile.
step "Building both bench binaries"
(cd "$worktree" && CARGO_TARGET_DIR="$target_dir" cargo bench --bench pipeline --no-run)
cargo bench --bench pipeline --no-run

step "Benching $ref ($short), $where"
# `${pin[@]+...}`: bash before 4.4 treats an empty array as unset under `set -u`.
(cd "$worktree" && CARGO_TARGET_DIR="$target_dir" ${pin[@]+"${pin[@]}"} \
    cargo bench --bench pipeline -- --noplot "$filter" --save-baseline "$baseline") |
    tee "$logs/base"
step "Benching the current tree against $ref, $where"
${pin[@]+"${pin[@]}"} cargo bench --bench pipeline -- --noplot "$filter" --baseline "$baseline" |
    tee "$logs/tree"

step "Summary"
summarize "$ref against its previous saved run (same code: anything listed is noise in one of the two):" \
    "$logs/base"
summarize "The current tree against $ref:" "$logs/tree" "$logs/base"

echo
echo "A \"regressed\" line could be anomalous. Re-run the case to confirm it. A wide time"
echo "interval on either side, or the same case listed in both sections, points to noise."
