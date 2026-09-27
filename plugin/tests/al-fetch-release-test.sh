#!/usr/bin/env bash
#
# Regression test for R9-PLUGIN-1 (Docs/campaign/findings/r9-session-review.md):
# al-fetch-release.sh installed a symlink or a directory named al-lsp or
# al-explorer as if it were the verified file.
#
# Serves a small archive over a local python3 -m http.server: curl in
# al-fetch-release.sh restricts --proto to https/http, so a file:// URL
# would be refused before any of this ran.
#
# Run directly, or via `make plugin-validate`.

set -eu

root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
fetch_script="$root/plugin/scripts/al-fetch-release.sh"

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
# would otherwise short-circuit the script under test ("already on PATH;
# nothing to download"). Strip any PATH entry that holds either binary;
# curl/tar/sha256sum/python3 live elsewhere and are unaffected.
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

scenario_symlink_member
scenario_directory_member

printf 'al-fetch-release-test: all scenarios passed\n'
