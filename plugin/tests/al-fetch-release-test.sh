#!/usr/bin/env bash
#
# Regression tests for al-fetch-release.sh and plugin/evals/run.sh:
#
#   A symlink or a directory named al-lsp or al-explorer in the release
#   archive is refused before install.
#   A failed install step (mkdir, mv or rm) is named in the refusal. The
#   report does not claim anything was installed.
#   An archive that lacks a file the listing names under the archive's name
#   is refused.
#   A missing or mismatched al-lsp skips an eval case by name, with the
#   reason al-explorer gave.
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
# any PATH entry that holds either binary. curl, tar, sha256sum, python3 and
# jq live elsewhere and are unaffected.
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

# ── a symlinked al-lsp is refused ───────────────────────────────────
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

# ── a directory named al-lsp is refused ─────────────────────────────
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

# ── a failed install step is named and nothing is reported installed ─
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

# ── an archive that lacks a listed bridge file is refused ───────────
# Every extracted file is listed and matches, and al-explorer and al-lsp are
# at the root, so only the check that every listed file was extracted stops
# this install. check_bridge_files in src/lib.rs refuses the same archive.
scenario_missing_listed_file() {
	local stage="$work/stage-missing" data="$work/data-missing" out rc
	rm -rf "$stage" "$data"
	mkdir -p "$stage/bridge"
	printf 'explorer-bytes\n' >"$stage/al-explorer"
	printf 'lsp-bytes\n' >"$stage/al-lsp"
	chmod +x "$stage/al-explorer" "$stage/al-lsp"
	printf 'dep-bytes\n' >"$stage/bridge/Dep.dll"
	(cd "$stage" && tar -czf "$serve_dir/$asset_name" al-explorer al-lsp bridge/Dep.dll)
	{
		printf '%s  %s/al-explorer\n' "$(sha256_of "$stage/al-explorer")" "$asset_name"
		printf '%s  %s/al-lsp\n' "$(sha256_of "$stage/al-lsp")" "$asset_name"
		printf '%s  %s/bridge/AlBridge.dll\n' \
			"0000000000000000000000000000000000000000000000000000000000000000" "$asset_name"
		printf '%s  %s/bridge/Dep.dll\n' "$(sha256_of "$stage/bridge/Dep.dll")" "$asset_name"
		printf '%s  %s/al-lsp\n' \
			"1111111111111111111111111111111111111111111111111111111111111111" "al-other-platform.tar.gz"
	} >"$serve_dir/binary-checksums.txt"

	out="$(run_fetch "$data" 2>&1)" && rc=0 || rc=$?
	[ "$rc" -eq 0 ] || fail "missing listed file: al-fetch-release.sh exited $rc, its contract is always 0. Output: $out"
	printf '%s\n' "$out" | grep -qF "$asset_name/bridge/AlBridge.dll is listed in binary-checksums.txt but was not extracted" ||
		fail "missing listed file: expected a refusal naming $asset_name/bridge/AlBridge.dll. Output: $out"
	if printf '%s\n' "$out" | grep -qF "al-other-platform.tar.gz"; then
		fail "missing listed file: a name listed under another asset was reported. Output: $out"
	fi
	[ ! -d "$data/bin" ] ||
		fail "missing listed file: something was installed into $data/bin"
	pass "an archive that lacks a listed bridge file is refused, not installed"
}

# run.sh now asks al-explorer itself whether al-lsp is usable (a preflight
# call before any case runs) instead of guessing from the filesystem, so
# these two scenarios need an al-explorer that behaves like the real one for
# that one question. This fake implements only that: it looks for al-lsp on
# PATH, the way find_al_lsp_binary does (crates/al-protocol/src/client/mod.rs)
# once no sibling binary exists, and reports the same refusal shape al-explorer
# reports on stdout when none is found or the one found answers the wrong
# version to --version. $1 is the path to write the fake binary to.
write_fake_al_explorer() {
	cat >"$1" <<'SH'
#!/bin/sh
found=""
old_ifs="$IFS"
IFS=:
for dir in $PATH; do
	if [ -x "$dir/al-lsp" ]; then
		found="$dir/al-lsp"
		break
	fi
done
IFS="$old_ifs"

if [ -z "$found" ]; then
	printf '{"error":"Cannot find al-lsp. It is normally installed beside al-explorer; this executable has no al-lsp next to it and there is none on PATH."}\n'
	exit 1
fi

version="$("$found" --version 2>/dev/null)"
printf '{"error":"The only al-lsp available is %s (%s), and this client is version 0.4.0. It is on PATH rather than beside this executable."}\n' "$found" "$version"
exit 1
SH
	chmod +x "$1"
}

# ── a missing al-lsp skips the eval case by name ────────────────────
scenario_missing_al_lsp_eval() {
	local fake_bin="$work/fake-bin" case_file="$work/scratch-case.json" out rc
	rm -rf "$fake_bin"
	mkdir -p "$fake_bin"
	write_fake_al_explorer "$fake_bin/al-explorer"

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
	printf '%s\n' "$out" | grep -qE '^SKIP  scratch-missing-al-lsp -- Cannot find al-lsp' ||
		fail "missing al-lsp: expected an upfront SKIP naming al-lsp (exit $rc). Output: $out"
	if printf '%s\n' "$out" | grep -q '^FAIL'; then
		fail "missing al-lsp: a case reported FAIL instead of SKIP. Output: $out"
	fi
	pass "a missing al-lsp skips the eval case instead of reporting a wrong answer"
}

# ── a mismatched al-lsp on PATH skips the eval case by name ─────────
scenario_mismatched_al_lsp_eval() {
	local fake_bin="$work/fake-bin-mismatch" fake_lsp_dir="$work/fake-lsp-path" \
		case_file="$work/scratch-mismatch-case.json" out rc
	rm -rf "$fake_bin" "$fake_lsp_dir"
	mkdir -p "$fake_bin" "$fake_lsp_dir"
	write_fake_al_explorer "$fake_bin/al-explorer"

	cat >"$fake_lsp_dir/al-lsp" <<'SH'
#!/bin/sh
if [ "${1-}" = "--version" ]; then
	printf 'al-lsp 0.0.1\n'
	exit 0
fi
exit 1
SH
	chmod +x "$fake_lsp_dir/al-lsp"

	cat >"$case_file" <<'JSON'
{
  "id": "scratch-mismatched-al-lsp",
  "question": "scratch case for the mismatched al-lsp regression test",
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

	out="$(PATH="$fake_lsp_dir:$test_path" AL_EXPLORER_BIN="$fake_bin/al-explorer" \
		bash "$eval_runner" "$case_file" 2>&1)" && rc=0 || rc=$?
	printf '%s\n' "$out" | grep -qE '^SKIP  scratch-mismatched-al-lsp -- The only al-lsp available is .*0\.0\.1' ||
		fail "mismatched al-lsp: expected an upfront SKIP naming the version mismatch (exit $rc). Output: $out"
	if printf '%s\n' "$out" | grep -q '^FAIL'; then
		fail "mismatched al-lsp: a case reported FAIL instead of SKIP. Output: $out"
	fi
	pass "a mismatched al-lsp on PATH skips the eval case instead of reporting a wrong answer"
}

scenario_symlink_member
scenario_directory_member
scenario_failed_install_step
scenario_missing_listed_file
scenario_missing_al_lsp_eval
scenario_mismatched_al_lsp_eval

printf 'al-fetch-release-test: all scenarios passed\n'
