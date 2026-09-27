#!/usr/bin/env bash
#
# Regression tests for the plugin findings in round 9 of the improvement
# campaign (Docs/campaign/findings/r9-session-review.md):
#
#   R9-PLUGIN-1: al-fetch-release.sh installed a symlink or a directory
#                named al-lsp or al-explorer as if it were the verified file.
#   R9-PLUGIN-2: al-fetch-release.sh reported "installed" when a step of the
#                install (mkdir/mv/rm) failed, and nothing was installed.
#   R9-PLUGIN-3: plugin/evals/run.sh turned a missing or unrelated al-lsp
#                into twelve reports of a wrong answer, with no reason shown.
#
# Serves small archives over a local python3 -m http.server: curl in
# al-fetch-release.sh restricts --proto to https/http, so a file:// URL
# would be refused before any of this ran.
#
# Run directly, or via `make plugin-validate`.

set -eu

root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
fetch_script="$root/plugin/scripts/al-fetch-release.sh"
eval_runner="$root/plugin/evals/run.sh"

fail() {
	printf 'al-fetch-release-test: FAIL: %s\n' "$1" >&2
	exit 1
}

pass() {
	printf 'al-fetch-release-test: ok: %s\n' "$1"
}

work="$(mktemp -d "${TMPDIR:-/tmp}/al-fetch-release-test.XXXXXX")"
server_pid=""
cleanup() {
	if [ -n "$server_pid" ]; then
		kill "$server_pid" >/dev/null 2>&1 || true
		wait "$server_pid" 2>/dev/null || true
	fi
	rm -rf "$work"
}
trap cleanup EXIT

sha256_of() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	else
		shasum -a 256 "$1" | cut -d' ' -f1
	fi
}

# This machine has a real al-explorer/al-lsp installed for development, which
# would otherwise short-circuit both scripts under test ("already on PATH;
# nothing to download", or a real daemon answering every eval check). Strip
# any PATH entry that holds either binary; curl/tar/sha256sum/python3/jq live
# elsewhere and are unaffected.
path_without_binaries() {
	local dir filtered="" dirs
	IFS=':' read -ra dirs <<<"$PATH"
	for dir in "${dirs[@]}"; do
		if [ -x "$dir/al-explorer" ] || [ -x "$dir/al-lsp" ]; then
			continue
		fi
		filtered="${filtered:+$filtered:}$dir"
	done
	printf '%s\n' "$filtered"
}
test_path="$(path_without_binaries)"

plat_os=""
case "$(uname -s)" in
Linux) plat_os="linux" ;;
Darwin) plat_os="macos" ;;
*) fail "unsupported test platform: $(uname -s)" ;;
esac
plat_arch=""
case "$(uname -m)" in
x86_64 | amd64) plat_arch="x86_64" ;;
arm64 | aarch64) plat_arch="aarch64" ;;
*) fail "unsupported test platform: $(uname -m)" ;;
esac
asset_name="al-${plat_os}-${plat_arch}.tar.gz"

serve_dir="$work/serve"
mkdir -p "$serve_dir"

command -v python3 >/dev/null 2>&1 || fail "python3 is required to serve the test archives"

port=$((20000 + (RANDOM % 20000)))
base_url="http://127.0.0.1:$port"
python3 -m http.server "$port" --directory "$serve_dir" --bind 127.0.0.1 \
	>"$work/server.log" 2>&1 &
server_pid=$!

ready=0
i=0
while [ "$i" -lt 50 ]; do
	if curl -s -o /dev/null "$base_url/"; then
		ready=1
		break
	fi
	sleep 0.1
	i=$((i + 1))
done
[ "$ready" -eq 1 ] || fail "local http server on $base_url did not come up"

# $1 = CLAUDE_PLUGIN_DATA for this attempt.
run_fetch() {
	PATH="$test_path" \
		CLAUDE_PLUGIN_DATA="$1" \
		AL_RELEASE_BASE_URL="$base_url" \
		AL_ALLOW_INSECURE_RELEASE_URL=1 \
		bash "$fetch_script"
}

# ── R9-PLUGIN-1a: a symlinked al-lsp is refused ─────────────────────
scenario_symlink_member() {
	local stage="$work/stage-symlink" data="$work/data-symlink" out rc
	rm -rf "$stage" "$data"
	mkdir -p "$stage"
	printf 'explorer-bytes\n' >"$stage/al-explorer"
	chmod +x "$stage/al-explorer"
	ln -s /bin/sh "$stage/al-lsp"
	(cd "$stage" && tar -czf "$serve_dir/$asset_name" al-explorer al-lsp)
	printf '%s  %s/al-explorer\n' "$(sha256_of "$stage/al-explorer")" "$asset_name" \
		>"$serve_dir/binary-checksums.txt"

	out="$(run_fetch "$data" 2>&1)" && rc=0 || rc=$?
	[ "$rc" -eq 0 ] || fail "symlink member: al-fetch-release.sh exited $rc, its contract is always 0. Output: $out"
	printf '%s\n' "$out" | grep -qF "not a regular file or a directory" ||
		fail "symlink member: expected a 'not a regular file or a directory' refusal. Output: $out"
	[ ! -e "$data/bin/al-lsp" ] ||
		fail "symlink member: al-lsp was installed although it was a symlink"
	pass "a symlinked al-lsp is refused, not installed"
}

# ── R9-PLUGIN-1b: a directory named al-lsp is refused ───────────────
scenario_directory_member() {
	local stage="$work/stage-dir" data="$work/data-dir" out rc
	rm -rf "$stage" "$data"
	mkdir -p "$stage/al-lsp"
	printf 'explorer-bytes\n' >"$stage/al-explorer"
	chmod +x "$stage/al-explorer"
	printf 'inner\n' >"$stage/al-lsp/inner"
	(cd "$stage" && tar -czf "$serve_dir/$asset_name" al-explorer al-lsp)
	{
		printf '%s  %s/al-explorer\n' "$(sha256_of "$stage/al-explorer")" "$asset_name"
		printf '%s  %s/al-lsp/inner\n' "$(sha256_of "$stage/al-lsp/inner")" "$asset_name"
	} >"$serve_dir/binary-checksums.txt"

	out="$(run_fetch "$data" 2>&1)" && rc=0 || rc=$?
	[ "$rc" -eq 0 ] || fail "directory member: al-fetch-release.sh exited $rc, its contract is always 0. Output: $out"
	printf '%s\n' "$out" | grep -qF "al-lsp as something other than a regular file" ||
		fail "directory member: expected an 'al-lsp as something other than a regular file' refusal. Output: $out"
	[ ! -d "$data/bin" ] ||
		fail "directory member: something was installed into $data/bin"
	pass "a directory named al-lsp is refused, not installed"
}

# ── R9-PLUGIN-2: a failed install step is named, not reported installed ──
scenario_failed_install_step() {
	local stage="$work/stage-good" blocker="$work/blocker-file" data out rc
	rm -rf "$stage"
	mkdir -p "$stage"
	printf 'explorer-bytes\n' >"$stage/al-explorer"
	printf 'lsp-bytes\n' >"$stage/al-lsp"
	chmod +x "$stage/al-explorer" "$stage/al-lsp"
	(cd "$stage" && tar -czf "$serve_dir/$asset_name" al-explorer al-lsp)
	{
		printf '%s  %s/al-explorer\n' "$(sha256_of "$stage/al-explorer")" "$asset_name"
		printf '%s  %s/al-lsp\n' "$(sha256_of "$stage/al-lsp")" "$asset_name"
	} >"$serve_dir/binary-checksums.txt"

	# A regular file where a directory needs to go: mkdir -p fails on any
	# platform, without relying on permission bits (which root ignores).
	rm -f "$blocker"
	printf 'not a directory\n' >"$blocker"
	data="$blocker/data"

	out="$(run_fetch "$data" 2>&1)" && rc=0 || rc=$?
	[ "$rc" -eq 0 ] || fail "failed install step: al-fetch-release.sh exited $rc, its contract is always 0. Output: $out"
	if printf '%s\n' "$out" | grep -qF "installed al-lsp and al-explorer"; then
		fail "failed install step: reported installed although $data could not be created. Output: $out"
	fi
	printf '%s\n' "$out" | grep -qF "could not create $data" ||
		fail "failed install step: expected a refusal naming the 'create $data' step. Output: $out"
	pass "a failed install step is named and nothing is reported installed"
}

# ── R9-PLUGIN-3: a missing al-lsp skips the eval case by name ───────────
scenario_missing_al_lsp_eval() {
	local fake_bin="$work/fake-bin" case_file="$work/scratch-case.json" out rc
	rm -rf "$fake_bin"
	mkdir -p "$fake_bin"
	printf '#!/bin/sh\nexit 1\n' >"$fake_bin/al-explorer"
	chmod +x "$fake_bin/al-explorer"

	cat >"$case_file" <<'JSON'
{
  "id": "scratch-missing-al-lsp",
  "question": "scratch case for the missing al-lsp regression test",
  "skill": "al-bc:bc-object-id-allocator",
  "fixture": "crates/al-test-harness/data/test_al_project",
  "overlay": null,
  "checks": [
    {
      "args": ["--compact", "free-ids", "--kind", "table"],
      "jq": ".nextFree",
      "expect_exact": "50101"
    }
  ]
}
JSON

	out="$(PATH="$test_path" AL_EXPLORER_BIN="$fake_bin/al-explorer" \
		bash "$eval_runner" "$case_file" 2>&1)" && rc=0 || rc=$?
	printf '%s\n' "$out" | grep -qE '^SKIP  scratch-missing-al-lsp -- no al-lsp binary' ||
		fail "missing al-lsp: expected an upfront SKIP naming al-lsp (exit $rc). Output: $out"
	if printf '%s\n' "$out" | grep -q '^FAIL'; then
		fail "missing al-lsp: a case reported FAIL instead of SKIP. Output: $out"
	fi
	pass "a missing al-lsp skips the eval case instead of reporting a wrong answer"
}

scenario_symlink_member
scenario_directory_member
scenario_failed_install_step
scenario_missing_al_lsp_eval

printf 'al-fetch-release-test: all scenarios passed\n'
