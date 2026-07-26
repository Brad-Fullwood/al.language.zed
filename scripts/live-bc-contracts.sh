#!/usr/bin/env bash
set -euo pipefail

unavailable() {
    echo "UNAVAILABLE: $*" >&2
    exit 2
}

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
    [[ -n "${!name:-}" ]] || unavailable "$name must be set and non-blank"
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
