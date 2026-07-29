#!/usr/bin/env bash
set -euo pipefail

unavailable() {
    echo "UNAVAILABLE: $*" >&2
    exit 2
}

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
generated_project=""

cleanup() {
    if [[ -n "$generated_project" && -d "$generated_project" ]]; then
        rm -rf -- "$generated_project"
    fi
}
trap cleanup EXIT

require_non_blank() {
    local name="$1"
    [[ -n "${!name:-}" ]] || unavailable "$name must be set and non-blank"
}

prepare_repository_fixture() {
    require_non_blank AL_LIVE_BC_TENANT
    require_non_blank AL_LIVE_BC_ENVIRONMENT
    require_non_blank AL_LIVE_BC_VERSION

    local tenant="$AL_LIVE_BC_TENANT"
    local environment="$AL_LIVE_BC_ENVIRONMENT"
    local tenant_pattern='^[[:alnum:]][[:alnum:].-]*[[:alnum:]]$'
    local environment_pattern='^[[:alnum:]_. -]+$'
    if [[ ! "$tenant" =~ $tenant_pattern ]] || [[ "$tenant" == *..* ]]; then
        unavailable "AL_LIVE_BC_TENANT must be a tenant GUID or domain"
    fi
    [[ "$environment" =~ $environment_pattern ]] ||
        unavailable "AL_LIVE_BC_ENVIRONMENT contains unsupported characters"

    local fixture_source="$repo_root/crates/al-test-harness/data/live_bc_contract_project"
    [[ -f "$fixture_source/app.json" ]] ||
        unavailable "repository live-BC fixture is missing app.json"
    [[ -f "$fixture_source/.vscode/launch.json.in" ]] ||
        unavailable "repository live-BC fixture is missing launch.json.in"

    generated_project="$(mktemp -d "${TMPDIR:-/tmp}/al-live-bc-contract.XXXXXX")"
    cp -R "$fixture_source/." "$generated_project/"

    local epoch day half build revision fixture_version
    epoch="$(date -u +%s)"
    day="$((epoch / 86400))"
    half="$(((epoch % 86400) / 43200))"
    build="$((day * 2 + half))"
    revision="$((epoch % 43200))"
    ((build <= 65535)) ||
        unavailable "generated fixture version no longer fits an AL version component"
    fixture_version="1.0.${build}.${revision}"
    sed -i \
        "s/\"version\": \"1.0.0.0\"/\"version\": \"$fixture_version\"/" \
        "$generated_project/app.json"

    local launch_template
    launch_template="$(<"$generated_project/.vscode/launch.json.in")"
    launch_template="${launch_template//@TENANT@/$tenant}"
    launch_template="${launch_template//@ENVIRONMENT@/$environment}"
    printf '%s\n' "$launch_template" >"$generated_project/.vscode/launch.json"
    rm -- "$generated_project/.vscode/launch.json.in"

    local breakpoint_file="src/LiveContractTests.Codeunit.al"
    local marker_count breakpoint_line
    marker_count="$(
        grep -c 'LIVE_BC_BREAKPOINT' "$generated_project/$breakpoint_file" || true
    )"
    [[ "$marker_count" == "1" ]] ||
        unavailable "repository live-BC fixture must contain exactly one breakpoint marker"
    breakpoint_line="$(
        awk '/LIVE_BC_BREAKPOINT/ { print NR; exit }' \
            "$generated_project/$breakpoint_file"
    )"

    export AL_LIVE_BC_PROJECT="$generated_project"
    export AL_LIVE_BC_CONFIG="Live BC Contract"
    export AL_LIVE_BC_TEST_CODEUNIT_ID="50100"
    export AL_LIVE_BC_TEST_CODEUNIT_NAME="Live Contract Tests"
    export AL_LIVE_BC_TEST_METHOD="PublishDebugAndSnapshot"
    export AL_LIVE_BC_BREAKPOINT_FILE="$breakpoint_file"
    export AL_LIVE_BC_BREAKPOINT_LINE="$breakpoint_line"
    export AL_LIVE_BC_EVAL="ObservedValue"
    export AL_LIVE_BC_EXPECT_EVAL="42"
}

if [[ -z "${AL_LIVE_BC_PROJECT:-}" ]]; then
    prepare_repository_fixture
fi

required=(
    AL_LIVE_BC_PROJECT
    AL_LIVE_BC_CONFIG
    AL_LIVE_BC_TEST_CODEUNIT_ID
    AL_LIVE_BC_TEST_CODEUNIT_NAME
    AL_LIVE_BC_TEST_METHOD
    AL_LIVE_BC_BREAKPOINT_FILE
    AL_LIVE_BC_BREAKPOINT_LINE
    AL_LIVE_BC_EVAL
    AL_LIVE_BC_VERSION
)
for name in "${required[@]}"; do
    require_non_blank "$name"
done

[[ -d "$AL_LIVE_BC_PROJECT" ]] ||
    unavailable "AL_LIVE_BC_PROJECT is not a directory"
live_project="$(cd "$AL_LIVE_BC_PROJECT" && pwd -P)"
[[ -f "$live_project/app.json" ]] ||
    unavailable "AL_LIVE_BC_PROJECT has no app.json"

[[ "$AL_LIVE_BC_TEST_CODEUNIT_ID" =~ ^[1-9][0-9]*$ ]] ||
    unavailable "AL_LIVE_BC_TEST_CODEUNIT_ID must be a positive integer"
[[ "$AL_LIVE_BC_BREAKPOINT_LINE" =~ ^[1-9][0-9]*$ ]] ||
    unavailable "AL_LIVE_BC_BREAKPOINT_LINE must be a positive 1-based integer"
[[ "${AL_LIVE_BC_TIMEOUT_SECS:-300}" =~ ^[1-9][0-9]*$ ]] ||
    unavailable "AL_LIVE_BC_TIMEOUT_SECS must be a positive integer"

if [[ "$AL_LIVE_BC_BREAKPOINT_FILE" = /* ]]; then
    breakpoint_file="$AL_LIVE_BC_BREAKPOINT_FILE"
else
    breakpoint_file="$live_project/$AL_LIVE_BC_BREAKPOINT_FILE"
fi
[[ -f "$breakpoint_file" ]] ||
    unavailable "AL_LIVE_BC_BREAKPOINT_FILE does not identify an existing file"

access_token="${BC_ACCESS_TOKEN:-}"
legacy_token="${BC_TOKEN:-}"
[[ -n "$access_token" || -n "$legacy_token" ]] ||
    unavailable "BC_ACCESS_TOKEN or BC_TOKEN must provide a headless AAD bearer token"
if [[ -n "$access_token" && -n "$legacy_token" && "$access_token" != "$legacy_token" ]]; then
    unavailable "BC_ACCESS_TOKEN and BC_TOKEN are both set to different values"
fi

export AL_LIVE_BC_PROJECT="$live_project"

if [[ "${1:-}" == "--preflight-only" ]]; then
    echo "Live BC contract preflight passed; live operations were not run."
    exit 0
fi
[[ $# -eq 0 ]] || unavailable "unknown argument: $1"

echo "=== Live Business Central contract profile ==="
echo "Project: $AL_LIVE_BC_PROJECT"
echo "Config:  $AL_LIVE_BC_CONFIG"
echo "Test:    $AL_LIVE_BC_TEST_CODEUNIT_ID $AL_LIVE_BC_TEST_CODEUNIT_NAME::$AL_LIVE_BC_TEST_METHOD"
echo "The bearer token is present and will not be printed."

cargo build -p al-lsp --bin al-lsp -p al-explorer --bin al-explorer
cargo test -p al-test-harness --test live_bc_contract \
    live_bc_publish_dap_test_and_snapshot_contract \
    -- --ignored --nocapture --test-threads=1

echo "Live Business Central contract profile passed."
