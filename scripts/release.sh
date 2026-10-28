#!/usr/bin/env bash
# Release helper for edamame.  Each subcommand is one stage of the procedure in
# docs/dev/releasing.md, and each can be re-run after a failure: `push` and
# `release` skip whatever they find already done.
#
#   scripts/release.sh prepare 0.2.0   # stamp CHANGELOG.md, bump Cargo.toml / Cargo.lock
#   scripts/release.sh bench           # bench against the previous release tag
#   scripts/release.sh push            # checks, commit, push main, wait for CI
#   scripts/release.sh release         # tag, push the tag, wait for the release workflow
#   scripts/release.sh verify          # GitHub release, tap, crates.io, docs.rs
#
# Everything after `prepare` reads the version from Cargo.toml.  `-y` / `--yes`
# skips the confirmation before each push.  Runs on bash 3.2 (macOS's /bin/bash) and later.

set -euo pipefail

CRATE=edamame
RELEASE_FILES=(CHANGELOG.md Cargo.toml Cargo.lock)
BENCH_FILTER='^(full_pipeline|visual_cache)'
ASSUME_YES=0
CMD=''

cd "$(git rev-parse --show-toplevel)"

# ── Helpers ───────────────────────────────────────────────────────────────

die() { printf 'error: %s\n' "$*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }
skip() { printf '    (already done: %s)\n' "$*"; }

confirm() {
    ((ASSUME_YES)) && return 0
    local reply
    read -r -p "$1 [y/N] " reply </dev/tty
    [[ $reply == [yY] ]] || die "aborted"
}

usage() {
    # `p;}`, not `p}`: BSD sed requires the `;` before a closing brace.
    sed -n '2,/^$/{s/^# \{0,1\}//;p;}' "$0"
    exit "${1:-0}"
}

# `version` under `[package]`, not the first `version =` in the file.
current_version() {
    awk -F'"' '/^\[/ { s = ($0 == "[package]") } s && /^version *=/ { print $2; exit }' Cargo.toml
}

# The nearest `vX.Y.Z` tag behind HEAD: the release being compared against.  Read from
# ancestry, not Cargo.toml, so it is right whether or not `prepare` has bumped the version;
# `HEAD^` so that on a tagged release commit it is the release before.
previous_version() {
    local tag
    tag=$(git describe --tags --abbrev=0 --match 'v[0-9]*' HEAD^ 2>/dev/null) ||
        die "no release tag behind HEAD"
    printf '%s\n' "${tag#v}"
}

# True when $1 sorts strictly after $2.
version_gt() {
    [[ $1 != "$2" && $(printf '%s\n' "$1" "$2" | sort -V | tail -n1) == "$1" ]]
}

current_branch() { git symbolic-ref --short -q HEAD || true; }

require_branch() {
    [[ $(current_branch) == "$1" ]] || die "must be on '$1' (on '$(current_branch)')"
}

is_clean() {
    local status
    status=$(git status --porcelain) || die "git status failed"
    [[ -z $status ]]
}

require_clean() {
    is_clean || die "working tree is not clean"
}

# The `## [label]` section body: the lines up to the next `## ` heading.
changelog_section() {
    awk -v h="## [$1]" 'index($0, h) == 1 { on = 1; next } on && /^## / { exit } on' CHANGELOG.md
}

# True when the `## [label]` section has any non-blank line.  Captured rather than piped
# into `grep -q`, whose early exit can fail the writer with SIGPIPE under `pipefail`.
changelog_has_section() {
    local body
    body=$(changelog_section "$1") || return 1
    [[ $body == *[^[:space:]]* ]]
}

# The archive names (one per target) that `dist plan`'s JSON manifest lists for tag $1, one
# per line.  Dies unless the plan announces $1 and lists at least one archive, so a check
# that loops over them cannot pass vacuously.
plan_archives() {
    local plan archives
    plan=$(dist plan --tag "$1" --output-format=json) || die "dist plan failed"
    [[ $(jq -r .announcement_tag <<<"$plan") == "$1" ]] || die "dist plan does not announce $1"
    archives=$(jq -r '.artifacts[] | select(.kind == "executable-zip") | .name' <<<"$plan")
    [[ -n $archives ]] || die "dist plan lists no archives for $1"
    printf '%s\n' "$archives"
}

# Prints the id of the push-triggered run of workflow $1 on commit $2, waiting for it to appear.
# The `|| die` matters: bash clears `set -e` inside `$(...)`, where this always runs, so
# without it a `gh` failure (auth, network) would read as a run that has not appeared yet.
find_run() {
    local workflow=$1 sha=$2 id=''
    for _ in {1..24}; do
        id=$(gh run list --workflow "$workflow" --commit "$sha" --event push --limit 1 \
            --json databaseId --jq '.[0].databaseId // empty') ||
            die "gh run list failed while looking for the $workflow run"
        [[ -n $id ]] && break
        printf '    waiting for the %s run to appear...\n' "$workflow" >&2
        sleep 5
    done
    [[ -n $id ]] || die "no $workflow run found for ${sha:0:12} after 2 minutes"
    printf '%s\n' "$id"
}

# Called when `gh run watch` on run $2 of workflow $1 exits non-zero, which it also does when
# the watch itself stops (Ctrl-C, a network drop) with the run still going.
run_failed() {
    local status
    status=$(gh run view "$2" --json status --jq .status 2>/dev/null) || status=unknown
    [[ $status == completed ]] ||
        die "stopped watching $1 run $2 before it finished (status: $status); re-run '$0 $CMD' to resume"
    die "$1 run $2 failed.
    Inspect:  gh run view $2 --log-failed
    Rerun:    gh run rerun $2 --failed   (then re-run '$0 $CMD')"
}

# Waits for the push-triggered run of workflow $1 on commit $2 and dies unless it succeeds.
wait_for_run() {
    local id
    id=$(find_run "$1" "$2")
    gh run watch "$id" --exit-status || run_failed "$1" "$id"
}

# True when crates.io lists version $1.
crates_io_has() {
    local body
    body=$(curl -fsS -A "$CRATE-release-script" "https://crates.io/api/v1/crates/$CRATE/$1") ||
        return 1
    [[ $body == *"\"num\":\"$1\""* ]]
}

# True when the only failed jobs of run $1 are the crates.io publish job, i.e. everything
# else shipped and the crate can be published by hand.
only_crates_publish_failed() {
    local failed
    failed=$(gh run view "$1" --json jobs \
        --jq '.jobs[] | select(.conclusion == "failure") | .name') || return 1
    [[ -n $failed ]] && ! grep -qv '^custom-publish-crates' <<<"$failed"
}

# True when release run $1 has finished and succeeded, or failed only in the crates.io job
# and crates.io has version $2 anyway (published by hand).  A run still going never passes:
# the crates.io job can fail while the Homebrew job, which runs alongside it, is unfinished.
release_run_ok() {
    local state
    state=$(gh run view "$1" --json status,conclusion --jq '.status + " " + .conclusion') ||
        return 1
    [[ $state == "completed success" ]] && return 0
    [[ $state == completed\ * ]] && only_crates_publish_failed "$1" && crates_io_has "$2"
}

# What defines the bench harness outside `benches/`: the `[[bench]]` tables and the
# `criterion` dependency line in Cargo.toml, and the criterion version Cargo.lock resolves.
# Reads both files at revision $1, or from the working tree when $1 is empty.
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

# ── prepare ───────────────────────────────────────────────────────────────

cmd_prepare() {
    local version=${1:-}
    [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "usage: $0 prepare X.Y.Z"
    local current
    current=$(current_version)
    [[ -n $current ]] || die "Cargo.toml has no version under [package]"

    step "Checking preconditions"
    require_branch develop
    require_clean
    git fetch --quiet --tags origin
    git merge-base --is-ancestor origin/develop HEAD ||
        die "origin/develop has commits this branch lacks; pull first"
    git merge-base --is-ancestor origin/main HEAD ||
        die "origin/main has commits develop lacks; rebase develop onto main first"
    version_gt "$version" "$current" || die "$version is not newer than $current"
    git rev-parse -q --verify "refs/tags/v$version" >/dev/null && die "tag v$version already exists"
    changelog_has_section Unreleased ||
        die "the [Unreleased] section of CHANGELOG.md is empty; write the release notes first"

    step "Stamping CHANGELOG.md as $version"
    # Renames the [Unreleased] section and rewrites the footer link block the way
    # tests/changelog.rs expects: [Unreleased] compares the new tag with HEAD, and the new
    # version compares the previous [Unreleased] base with the new tag.
    local stamped
    stamped=$(awk -v v="$version" -v d="$(date +%F)" '
        $0 == "## [Unreleased]" { print; print ""; print "## [" v "] - " d; h = 1; next }
        /^\[Unreleased\]: .*\/compare\/v[^ ]*\.\.\.HEAD$/ {
            url = substr($0, length("[Unreleased]: ") + 1)
            i = index(url, "/compare/")
            base = substr(url, 1, i - 1)
            prev = substr(url, i + length("/compare/"))
            sub(/\.\.\.HEAD$/, "", prev)
            print "[Unreleased]: " base "/compare/v" v "...HEAD"
            print "[" v "]: " base "/compare/" prev "...v" v
            f = 1
            next
        }
        { print }
        END { if (!h || !f) exit 1 }
    ' CHANGELOG.md) || die "CHANGELOG.md has no [Unreleased] heading or footer link"
    printf '%s\n' "$stamped" >CHANGELOG.md

    step "Bumping Cargo.toml and Cargo.lock to $version"
    local bumped
    bumped=$(awk -v v="$version" '
        /^\[/ { s = ($0 == "[package]") }
        s && !done && /^version *=/ { print "version = \"" v "\""; done = 1; next }
        { print }
        END { exit !done }
    ' Cargo.toml) || die "found no version under [package] in Cargo.toml to bump"
    printf '%s\n' "$bumped" >Cargo.toml
    cargo update -p "$CRATE"

    step "Prepared $version"
    git diff --stat
    echo
    echo "Review with 'git diff', then run: $0 bench (optional), then $0 push"
}

# ── Checks (run by `push`) ────────────────────────────────────────────────

# Everything that must pass before the release commit reaches main.  `dist plan` and the
# publish dry run are here because CI on main runs neither: skipping them would let a
# packaging error surface only after the tag is public.
run_checks() {
    local version=$1

    step "cargo fmt"
    cargo fmt -- --check
    step "cargo clippy"
    cargo clippy --all-targets -- -D warnings
    step "Tests"
    if cargo nextest --version >/dev/null 2>&1; then
        # nextest cannot run doctests, so they get their own pass.
        cargo nextest run --no-fail-fast
        cargo test --doc
    else
        cargo test --no-fail-fast
    fi

    step "dist plan"
    local archives
    archives=$(plan_archives "v$version")
    printf '%s\n' "$archives"

    step "cargo publish --dry-run"
    local dirty=()
    is_clean || dirty=(--allow-dirty)
    cargo publish --dry-run --locked ${dirty[@]+"${dirty[@]}"}
}

# ── bench ─────────────────────────────────────────────────────────────────

cmd_bench() {
    # `worktree` is deliberately not local: the EXIT trap reads it after this function returns.
    local version prev core target_dir
    version=$(current_version)
    prev=${PREV:-$(previous_version)}
    core=${CORE:-3}
    target_dir="$PWD/target"

    git diff --quiet "v$prev" -- benches/ ||
        die "benches/ changed since v$prev; the comparison would be meaningless"
    local old_harness
    old_harness=$(bench_harness "v$prev") || die "cannot read Cargo.toml / Cargo.lock at v$prev"
    [[ $old_harness == "$(bench_harness '')" ]] ||
        die "the criterion dependency or a [[bench]] entry changed since v$prev; the comparison would be meaningless"

    local pin=()
    if command -v taskset >/dev/null; then
        pin=(taskset -c "$core")
    else
        echo "warning: taskset not found; benches run unpinned" >&2
    fi

    worktree=$(mktemp -d)
    trap 'git worktree remove --force "$worktree" 2>/dev/null || true; rm -rf "$worktree"' EXIT
    step "Checking out v$prev into a temporary worktree"
    git worktree add --detach "$worktree" "v$prev"

    step "Building both bench binaries"
    (cd "$worktree" && CARGO_TARGET_DIR="$target_dir" cargo bench --bench pipeline --no-run)
    cargo bench --bench pipeline --no-run

    step "Benching v$prev (baseline) on core $core"
    # `${pin[@]+...}`: bash before 4.4 (macOS's /bin/bash) treats an empty array as unset
    # under `set -u`, and `pin` is empty exactly where taskset is missing.
    (cd "$worktree" && CARGO_TARGET_DIR="$target_dir" ${pin[@]+"${pin[@]}"} \
        cargo bench --bench pipeline -- --noplot "$BENCH_FILTER" --save-baseline "v$prev")
    step "Benching $version against v$prev on core $core"
    ${pin[@]+"${pin[@]}"} cargo bench --bench pipeline -- --noplot "$BENCH_FILTER" --baseline "v$prev"

    echo
    echo "A \"regressed\" line is something to confirm, not a release blocker by itself."
    echo "Next: $0 push"
}

# ── push / release ────────────────────────────────────────────────────────

# The preconditions `push` and `release` share.  Sets the caller's `version`, `tag` and `subject`.
release_preamble() {
    version=$(current_version)
    tag="v$version"
    subject="chore(release): $tag"
    require_branch develop
    git fetch --quiet --tags origin
    git merge-base --is-ancestor origin/develop HEAD ||
        die "origin/develop has commits this branch lacks; pull first"
}

cmd_push() {
    local version tag subject
    release_preamble

    # Everything that can refuse cheaply is checked before the slow checks run.  An existing
    # tag anywhere but the release commit means `prepare` was skipped.
    local committed=0 pushed=0 tagged=0
    git rev-parse -q --verify "refs/tags/$tag" >/dev/null && tagged=1
    if [[ $(git log -1 --format=%s) == "$subject" ]]; then
        committed=1
        if ((tagged)); then
            [[ $(git rev-parse "$tag^{commit}") == $(git rev-parse HEAD) ]] ||
                die "$tag already exists and does not point at HEAD; run '$0 prepare' first"
        fi
        require_clean
        [[ $(git rev-parse origin/main) == "$(git rev-parse HEAD)" ]] && pushed=1
    else
        # No release commit yet, so the tag cannot be on it.
        ((!tagged)) || die "$tag already exists; run '$0 prepare' with a new version first"
        local changed path
        changed=$(git status --porcelain | cut -c4-)
        [[ -n $changed ]] || die "nothing to commit, and HEAD is not '$subject'"
        while IFS= read -r path; do
            [[ " ${RELEASE_FILES[*]} " == *" $path "* ]] ||
                die "uncommitted change outside the release files: $path"
        done <<<"$changed"
        changelog_has_section "$version" ||
            die "CHANGELOG.md has no [$version] section; run '$0 prepare $version' first"
    fi
    if ((!pushed)); then
        git merge-base --is-ancestor origin/main HEAD ||
            die "origin/main has commits develop lacks; it cannot fast-forward"
    fi

    # Guards the push, not the commit: a release commit made but not yet pushed (by an
    # earlier run that stopped at the prompt, or by hand) is checked again.
    if ((pushed)); then
        step "Checks"
        skip "origin/main is at the release commit"
    else
        run_checks "$version"
    fi

    step "Release commit"
    if ((committed)); then
        skip "HEAD is '$subject'"
    else
        git commit -m "$subject" -- "${RELEASE_FILES[@]}"
    fi
    local sha
    sha=$(git rev-parse HEAD)

    step "Push to main"
    if ((pushed)); then
        skip "origin/main is at ${sha:0:12}"
    else
        git log --oneline origin/main..HEAD
        confirm "Fast-forward origin/main to ${sha:0:12} (the commits above)?"
        git push origin HEAD:main
    fi
    # Keep the local main branch in step; it is never checked out here.
    git fetch --quiet origin main:main ||
        echo "warning: could not fast-forward the local main branch; update it by hand" >&2

    step "Waiting for CI on main"
    wait_for_run ci.yml "$sha"

    step "Pushed $tag's commit to main; CI is green"
    echo "Review main on GitHub, then run: $0 release"
}

cmd_release() {
    local version tag subject
    release_preamble
    [[ $(git log -1 --format=%s) == "$subject" ]] ||
        die "HEAD is not '$subject'; run '$0 push' first"
    require_clean
    local sha
    sha=$(git rev-parse HEAD)
    [[ $(git rev-parse origin/main) == "$sha" ]] ||
        die "origin/main is not at ${sha:0:12}; run '$0 push' first"

    # Re-checked rather than trusted from `push`: the tag push publishes, so it must not
    # follow a CI run that failed or was rerun red since.
    step "Checking CI on main"
    wait_for_run ci.yml "$sha"

    step "Tag $tag"
    if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        [[ $(git rev-parse "$tag^{commit}") == "$sha" ]] || die "$tag exists but does not point at HEAD"
        skip "$tag exists"
    else
        git tag -a "$tag" -m "$CRATE $tag"
    fi

    step "Push $tag (triggers the release workflow)"
    if [[ -n $(git ls-remote --tags origin "refs/tags/$tag") ]]; then
        skip "$tag is on origin"
    else
        confirm "Push $tag to origin, publishing the release?"
        git push origin "$tag"
    fi

    step "Waiting for the release workflow"
    local run
    run=$(find_run release.yml "$sha")
    if ! gh run watch "$run" --exit-status; then
        # A crate published by hand after the publish job failed counts as done.
        release_run_ok "$run" "$version" || run_failed release.yml "$run"
        skip "the crates.io job failed, but crates.io has $version"
    fi

    step "Push develop"
    if [[ $(git rev-parse origin/develop) == "$sha" ]]; then
        skip "origin/develop is at ${sha:0:12}"
    else
        confirm "Push develop to origin?"
        git push origin develop
    fi

    step "Released $tag"
    echo "Next: $0 verify (docs.rs can take a while to build)"
}

# ── verify ────────────────────────────────────────────────────────────────

cmd_verify() {
    local version tag failed=0
    version=$(current_version)
    tag="v$version"

    check() {
        if "${@:2}" >/dev/null 2>&1; then
            printf '  ok    %s\n' "$1"
        else
            printf '  FAIL  %s\n' "$1"
            failed=1
        fi
    }

    release_has_archives() {
        local assets archives archive
        archives=$(plan_archives "$tag") || return 1
        assets=$(gh release view "$tag" --json assets --jq '.assets[].name') || return 1
        for archive in $archives; do
            grep -qxF "$archive" <<<"$assets" || return 1
        done
    }
    release_run_succeeded() {
        local sha run
        sha=$(git rev-parse "$tag^{commit}") || return 1
        run=$(gh run list --workflow release.yml --commit "$sha" --event push --limit 1 \
            --json databaseId --jq '.[0].databaseId // empty') || return 1
        [[ -n $run ]] && release_run_ok "$run" "$version"
    }
    # `base64 --decode` rather than `-d`: older macOS spells the short flag `-D`.
    tap_formula_updated() {
        local tap formula
        tap=$(sed -n 's/^tap *= *"\(.*\)"/\1/p' dist-workspace.toml)
        formula=$(gh api "repos/$tap/contents/Formula/$CRATE.rb" --jq .content |
            base64 --decode) || return 1
        grep -qxF "  version \"$version\"" <<<"$formula"
    }
    docs_rs_built() {
        local status
        status=$(curl -fsS "https://docs.rs/crate/$CRATE/$version/status.json") || return 1
        [[ $status == *'"doc_status":true'* ]]
    }

    step "Verifying $tag"
    check "GitHub release $tag has every archive dist plans" release_has_archives
    check "release workflow succeeded (a crates.io job failure counts if crates.io has $version)" \
        release_run_succeeded
    check "Homebrew formula is at $version" tap_formula_updated
    check "crates.io has $version" crates_io_has "$version"
    check "docs.rs built $version" docs_rs_built

    ((failed == 0)) || die "some checks failed (docs.rs may still be building; re-run later)"
    echo
    echo "All published.  Also skim the user docs for anything this release changed."
}

# ── Dispatch ──────────────────────────────────────────────────────────────

args=()
for arg in "$@"; do
    case $arg in
        -y | --yes) ASSUME_YES=1 ;;
        -h | --help) usage ;;
        *) args+=("$arg") ;;
    esac
done
set -- ${args[@]+"${args[@]}"}
CMD=${1:-}

case ${1:-} in
    prepare) cmd_prepare "${@:2}" ;;
    bench) cmd_bench ;;
    push) cmd_push ;;
    release) cmd_release ;;
    verify) cmd_verify ;;
    *) usage 1 ;;
esac
