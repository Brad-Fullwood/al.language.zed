#!/usr/bin/env bash
# test-release-scripts.sh: tests for the release scripts that need no grammar submodule.
#
# Usage: bash scripts/test-release-scripts.sh
# Every fixture is written under one mktemp directory, which is removed on exit.

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
t="$(mktemp -d)"
trap 'rm -rf "$t"' EXIT

# Keep the user's git config out of the fixtures.
export GIT_CONFIG_GLOBAL="$t/gitconfig" GIT_CONFIG_NOSYSTEM=1
: > "$t/gitconfig"

RC=0
OUT=""
ERR=""

pass() {
    echo "PASS: $*"
}

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

# Runs a command and keeps its exit code, stdout and stderr without stopping on a failure.
run_cmd() {
    RC=0
    "$@" >"$t/stdout" 2>"$t/stderr" || RC=$?
    OUT="$(cat "$t/stdout")"
    ERR="$(cat "$t/stderr")"
}

expect_exit() {
    local want="$1" name="$2"
    [ "${RC}" -eq "${want}" ] || fail "${name}: exit ${RC}, want ${want}. stdout: ${OUT} stderr: ${ERR}"
    pass "${name} (exit ${RC})"
}

expect_nonzero() {
    local name="$1"
    [ "${RC}" -ne 0 ] || fail "${name}: exit 0, want non-zero. stdout: ${OUT}"
    pass "${name} (exit ${RC})"
}

# expect_in <stdout|stderr> <text> <name>
expect_in() {
    local stream="$1" needle="$2" name="$3" text
    case "${stream}" in
        stdout) text="${OUT}" ;;
        stderr) text="${ERR}" ;;
        *) fail "expect_in: unknown stream ${stream}" ;;
    esac
    grep -qF -- "${needle}" <<< "${text}" \
        || fail "${name}: ${stream} does not contain '${needle}'. ${stream}: ${text}"
    pass "${name}"
}

assert_in_t() {
    case "$1" in
        "$t"/*) ;;
        *) fail "refusing to use a path outside $t: $1" ;;
    esac
}

git_fx() {
    local dir="$1"
    shift
    assert_in_t "${dir}"
    git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false -C "${dir}" "$@"
}

test_grammar_ancestry() {
    local helper="${REPO}/scripts/check-grammar-rev-merged.sh"
    local g="$t/grammar" a b
    mkdir -p "${g}"
    git_fx "${g}" init -q -b dev
    git_fx "${g}" remote add origin https://example.invalid/grammar.git
    git_fx "${g}" commit -q --allow-empty -m "A on dev"
    a="$(git_fx "${g}" rev-parse HEAD)"
    git_fx "${g}" checkout -q -b side
    git_fx "${g}" commit -q --allow-empty -m "B on a side branch"
    b="$(git_fx "${g}" rev-parse HEAD)"
    git_fx "${g}" update-ref refs/remotes/origin/dev "${a}"

    run_cmd bash "${helper}" "${g}" "${a}" dev
    expect_exit 0 "grammar ancestry: rev on dev exits 0"

    run_cmd bash "${helper}" "${g}" "${b}" dev
    expect_exit 1 "grammar ancestry: rev on a side branch exits 1"
    expect_in stderr "is not on dev" "grammar ancestry: unmerged rev message names the branch"

    run_cmd bash "${helper}" "${g}" "${a}" main
    expect_exit 2 "grammar ancestry: missing origin/main exits 2"

    run_cmd bash "${helper}" "${g}" 0000000000000000000000000000000000000001 dev
    expect_exit 1 "grammar ancestry: rev the clone does not have exits 1"

    run_cmd bash "${helper}" "${g}" "${a}"
    expect_exit 2 "grammar ancestry: missing branch argument exits 2"
}

test_glibc_floor() {
    local floor="${REPO}/scripts/check-glibc-floor.sh"
    local text="$t/not-an-elf.txt"
    if [ "$(uname -s)" != Linux ] || ! command -v readelf >/dev/null 2>&1; then
        echo "NOTE: glibc floor tests skipped. They need Linux and readelf. uname -s is $(uname -s)."
        return 0
    fi

    run_cmd bash "${floor}" 99.0 /bin/true
    expect_exit 0 "glibc floor: floor 99.0 accepts /bin/true"
    expect_in stdout "highest GLIBC_" "glibc floor: prints the highest GLIBC_ version"

    run_cmd bash "${floor}" 2.0 /bin/true
    expect_exit 1 "glibc floor: floor 2.0 rejects /bin/true"

    run_cmd bash "${floor}" 2.x /bin/true
    expect_exit 2 "glibc floor: malformed floor 2.x exits 2"

    printf 'not an ELF file\n' > "${text}"
    run_cmd bash "${floor}" 99.0 "${text}"
    expect_exit 2 "glibc floor: text file exits 2"
}

# Copies the files check_versions and check_example_schema_tag read, then commits them.
# The fixture has no tree-sitter-al submodule, so the script stops at the submodule check.
make_hygiene_fixture() {
    local dir="$1" f
    assert_in_t "${dir}"
    mkdir -p "${dir}/scripts" "${dir}/examples" "${dir}/crates/al-lsp" "${dir}/crates/al-explorer"
    for f in scripts/check-release-hygiene.sh scripts/require-ci-success.sh Cargo.toml Cargo.lock extension.toml \
        examples/zed-settings.jsonc crates/al-lsp/Cargo.toml crates/al-explorer/Cargo.toml; do
        cp -p "${REPO}/${f}" "${dir}/${f}"
    done
    git_fx "${dir}" init -q -b main
    git_fx "${dir}" add -A
    git_fx "${dir}" commit -q -m "fixture"
}

# Commits the change a negative case makes, so the case runs on a clean tree.
commit_fixture_change() {
    local dir="$1" msg="$2"
    git_fx "${dir}" add -A
    git_fx "${dir}" commit -q -m "${msg}"
}

test_hygiene_fixtures() {
    local fx="$t/hyg-base" n
    make_hygiene_fixture "${fx}"

    run_cmd bash "${fx}/scripts/check-release-hygiene.sh"
    expect_nonzero "hygiene fixture: unmodified copy stops at the submodule check"
    expect_in stdout "example schema URLs use" "hygiene fixture: unmodified copy passes the schema check"
    expect_in stderr "tree-sitter-al submodule directory is missing" "hygiene fixture: unmodified copy reaches the submodule check"

    # Schema URL: rewrite the first versioned schema URL to v0.3.0.
    fx="$t/hyg-schema"
    make_hygiene_fixture "${fx}"
    awk 'done_once == 0 && /\/v[0-9]+\.[0-9]+\.[0-9]+\/schemas\// { sub(/\/v[0-9]+\.[0-9]+\.[0-9]+\/schemas\//, "/v0.3.0/schemas/"); done_once = 1 } { print }' \
        "${fx}/examples/zed-settings.jsonc" > "$t/zed-settings.tmp"
    mv "$t/zed-settings.tmp" "${fx}/examples/zed-settings.jsonc"
    n="$(grep -c '/v0\.3\.0/schemas/' "${fx}/examples/zed-settings.jsonc" || true)"
    [ "${n}" -eq 1 ] || fail "hygiene schema fixture: expected 1 rewritten URL, found ${n}"
    commit_fixture_change "${fx}" "schema URL set to v0.3.0"
    run_cmd bash "${fx}/scripts/check-release-hygiene.sh"
    expect_nonzero "hygiene fixture: one schema URL at v0.3.0 is refused"
    expect_in stderr "example schema URLs must use" "hygiene fixture: refusal names the schema check"

    # Cargo.lock: set the zed-al version (the line after its name) to 0.0.1.
    fx="$t/hyg-lock"
    make_hygiene_fixture "${fx}"
    awk 'prev == "name = \"zed-al\"" && /^version = / { sub(/"[^"]*"$/, "\"0.0.1\"") } { prev = $0; print }' \
        "${fx}/Cargo.lock" > "$t/Cargo.lock.tmp"
    mv "$t/Cargo.lock.tmp" "${fx}/Cargo.lock"
    n="$(grep -c '^version = "0\.0\.1"$' "${fx}/Cargo.lock" || true)"
    [ "${n}" -eq 1 ] || fail "hygiene lock fixture: expected 1 version line set to 0.0.1, found ${n}"
    commit_fixture_change "${fx}" "Cargo.lock zed-al set to 0.0.1"
    run_cmd bash "${fx}/scripts/check-release-hygiene.sh"
    expect_nonzero "hygiene fixture: zed-al at 0.0.1 in Cargo.lock is refused"
    expect_in stderr "Cargo.lock zed-al version" "hygiene fixture: refusal names the Cargo.lock check"
}

# Writes one fake workflow run to a JSON file in the shape gh run list --json prints.
# rows_json <file> <head sha> <event> <head branch> <status> <conclusion>
rows_json() {
    printf '[{"databaseId":101,"status":"%s","conclusion":"%s","url":"https://example.invalid/runs/101","headSha":"%s","headBranch":"%s","event":"%s"}]\n' \
        "$5" "$6" "$2" "$4" "$3" > "$1"
}

# Runs require-ci-success.sh in the fixture repo with the stub gh. The rows come from the JSON file given first.
run_require_ci() {
    # shellcheck disable=SC2016 # $1 and $2 belong to the inner bash
    run_cmd env PATH="$t/bin:${PATH}" GH_STUB_JSON="$1" REQUIRE_CI=1 GH_TOKEN=x \
        GITHUB_REPOSITORY=owner/repo CI_WORKFLOW=CI CI_POLL_SECONDS=0 \
        CI_DISCOVERY_TIMEOUT_SECONDS=0 CI_TIMEOUT_SECONDS=0 \
        bash -c 'cd "$1" && exec "$2"' _ "$t/ci-repo" "${REPO}/scripts/require-ci-success.sh"
}

test_require_ci() {
    local g="$t/ci-repo" head_dev head_new
    command -v jq >/dev/null || fail "require ci tests need jq"
    mkdir -p "$t/bin" "${g}"

    # The stub answers "gh run list ... --jq FILTER" by applying FILTER to GH_STUB_JSON.
    # It ignores --event, --commit and the other flags, so the jq filter in the script decides.
    cat > "$t/bin/gh" <<'STUB'
#!/usr/bin/env bash
[ "$1 $2" = "run list" ] || { echo "stub gh: unexpected arguments: $*" >&2; exit 2; }
shift 2
filter=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --jq) filter="$2"; shift 2 ;;
        *) shift ;;
    esac
done
[ -n "${filter}" ] || { echo "stub gh: no --jq filter" >&2; exit 2; }
jq -r "${filter}" "${GH_STUB_JSON}"
STUB
    chmod 755 "$t/bin/gh"

    git_fx "${g}" init -q -b dev
    git_fx "${g}" commit -q --allow-empty -m "dev head"
    head_dev="$(git_fx "${g}" rev-parse HEAD)"
    git_fx "${g}" update-ref refs/remotes/origin/dev "${head_dev}"

    rows_json "$t/rows-pr.json" "${head_dev}" pull_request dev completed success
    run_require_ci "$t/rows-pr.json"
    expect_nonzero "require ci: completed success pull_request run is refused"
    expect_in stderr "no CI run was found" "require ci: pull_request run is not counted"

    rows_json "$t/rows-push-dev.json" "${head_dev}" push dev completed success
    run_require_ci "$t/rows-push-dev.json"
    expect_exit 0 "require ci: completed success push run on dev passes"
    expect_in stdout "CI succeeded for" "require ci: success line names the run"

    rows_json "$t/rows-push-feature.json" "${head_dev}" push feature/x completed success
    run_require_ci "$t/rows-push-feature.json"
    expect_nonzero "require ci: push run on a feature branch is refused"
    expect_in stderr "no CI run was found" "require ci: feature branch run is not counted"

    # Last case: a new commit on dev that origin/dev does not have yet. Its run passes the
    # filter, so only the ancestry check can refuse it.
    git_fx "${g}" commit -q --allow-empty -m "unmerged commit"
    head_new="$(git_fx "${g}" rev-parse HEAD)"
    rows_json "$t/rows-push-new.json" "${head_new}" push dev completed success
    run_require_ci "$t/rows-push-new.json"
    expect_nonzero "require ci: passing run on a commit not on dev or main is refused"
    expect_in stderr "is not on dev or main" "require ci: refusal names the ancestry check"
}

# A tag needs a non-empty Docs/release-notes/<tag>.md. With the file present the
# run stops at the next missing piece of the fixture, the grammar submodule.
test_release_notes() {
    local fx="$t/hyg-notes" tag
    make_hygiene_fixture "${fx}"
    tag="v$(awk -F'"' '/^version = / { print $2; exit }' "${fx}/extension.toml")"
    git_fx "${fx}" tag "${tag}"

    run_cmd bash "${fx}/scripts/check-release-hygiene.sh" --tag "${tag}"
    expect_nonzero "release notes: a tag without notes is refused"
    expect_in stderr "missing Docs/release-notes/${tag}.md" "release notes: refusal names the notes file"

    mkdir -p "${fx}/Docs/release-notes"
    : > "${fx}/Docs/release-notes/${tag}.md"
    commit_fixture_change "${fx}" "empty release notes"
    git_fx "${fx}" tag -f "${tag}" >/dev/null
    run_cmd bash "${fx}/scripts/check-release-hygiene.sh" --tag "${tag}"
    expect_in stderr "missing Docs/release-notes/${tag}.md" "release notes: an empty notes file is refused"

    printf '# Notes\n' > "${fx}/Docs/release-notes/${tag}.md"
    commit_fixture_change "${fx}" "release notes"
    git_fx "${fx}" tag -f "${tag}" >/dev/null
    run_cmd bash "${fx}/scripts/check-release-hygiene.sh" --tag "${tag}"
    expect_nonzero "release notes: with notes the fixture stops at a later check"
    expect_in stdout "release notes Docs/release-notes/${tag}.md present" "release notes: the notes check passes"
    expect_in stderr "tree-sitter-al submodule directory is missing" "release notes: the run reaches the submodule check"
}

main() {
    test_grammar_ancestry
    test_glibc_floor
    test_hygiene_fixtures
    test_release_notes
    test_require_ci
    echo "test-release-scripts: all passed"
}

main
