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
    for f in scripts/check-release-hygiene.sh Cargo.toml Cargo.lock extension.toml \
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

main() {
    test_grammar_ancestry
    test_glibc_floor
    test_hygiene_fixtures
    echo "test-release-scripts: all passed"
}

main
