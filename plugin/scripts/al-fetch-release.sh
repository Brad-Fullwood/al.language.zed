#!/usr/bin/env bash
#
# Called from the SessionStart hook (al-session-context.sh) when al-bin.sh
# would otherwise fail: neither al-lsp nor al-explorer is on PATH or in the
# plugin's own cache directory. Downloads the platform release archive,
# verifies every file it contains against the release's published
# binary-checksums.txt, and only then installs it. Nothing is made
# executable, and nothing is added to the plugin's cache directory, before
# its digest matches.
#
# A message always goes to stderr. When there is something the calling
# session should know (installed, or refused for an actionable reason), the
# same message also goes to stdout, which al-session-context.sh folds into
# its SessionStart context; "already have both binaries" stays on stderr
# only, so a normal session with no work to do adds nothing to the model's
# context.
#
# This script always exits 0. A session starting must not fail because a
# download did or did not happen; scripts/al-bin.sh still refuses clearly,
# with installation instructions, when a skill or the MCP server actually
# tries to run a binary that was never installed.
#
# Testing hooks (not for normal use):
#   AL_RELEASE_BASE_URL          override the base URL archives are fetched
#                                 from, for pointing at a local test server.
#   AL_ALLOW_INSECURE_RELEASE_URL=1
#                                 required in addition to the above to allow
#                                 a non-https base URL. Without it a non-https
#                                 URL is refused before any request is made.
#   AL_SKIP_FETCH_RELEASE=1      skip this script entirely, no network call.

set -uo pipefail

REPO="Brad-Fullwood/al.language.zed"
BINARY_CHECKSUMS_ASSET="binary-checksums.txt"

# The release this script trusts, not whatever GitHub calls "latest" today.
# Bump this only after cutting a release built from a release.yml that
# publishes binary-checksums.txt (see BINARY_CHECKSUMS_ASSET in src/lib.rs and
# the "Collect per-binary checksums" step in .github/workflows/release.yml).
# v0.2.2 predates that asset, so a download against it refuses below at the
# "release does not publish" step rather than skipping verification.
AL_PIN_RELEASE_TAG="${AL_PIN_RELEASE_TAG:-v0.2.2}"

log() {
	printf 'al-fetch-release: %s\n' "$1" >&2
}

# Like log, but also on stdout: the caller reads only stdout, so only a
# report() line reaches the session's context.
report() {
	log "$1"
	printf '%s\n' "$1"
}

if [ "${AL_SKIP_FETCH_RELEASE:-}" = "1" ]; then
	log "skipped (AL_SKIP_FETCH_RELEASE=1)"
	exit 0
fi

data_dir="${CLAUDE_PLUGIN_DATA:-$HOME/.claude/plugins/data/al-bc}"
bin_dir="$data_dir/bin"

have_pair() {
	[ -x "$1/al-explorer" ] && [ -x "$1/al-lsp" ]
}

# 1. Already available: PATH, or the plugin's own cache from an earlier
# session. Nothing to do, and nothing worth telling the agent. This does not
# check that both names resolve to the same directory the way al-bin.sh's
# full resolution does; two unrelated installs happening to both be on PATH
# is unlikely enough not to duplicate that logic here, and al-bin.sh still
# gives its own clear error if that ever happens.
if command -v al-explorer >/dev/null 2>&1 && command -v al-lsp >/dev/null 2>&1; then
	log "al-explorer and al-lsp are already on PATH; nothing to download"
	exit 0
fi
if have_pair "$bin_dir"; then
	log "al-explorer and al-lsp are already in $bin_dir; nothing to download"
	exit 0
fi

# 2. Platform triple. Matches the artifact_name values in
# .github/workflows/release.yml and the asset names al-bin.sh already tells
# people to download by hand.
plat_os=""
case "$(uname -s)" in
Linux) plat_os="linux" ;;
Darwin) plat_os="macos" ;;
*)
	report "no automatic download for this platform ($(uname -s)); install al-lsp and al-explorer by hand, or set AL_BIN_DIR (see al-bin.sh's message for how)"
	exit 0
	;;
esac

plat_arch=""
case "$(uname -m)" in
x86_64 | amd64) plat_arch="x86_64" ;;
arm64 | aarch64) plat_arch="aarch64" ;;
*)
	report "no automatic download for this architecture ($(uname -m)); install al-lsp and al-explorer by hand, or set AL_BIN_DIR"
	exit 0
	;;
esac

asset_name="al-${plat_os}-${plat_arch}.tar.gz"

# 3. https only, unless a test explicitly opts into a local http server.
base_url="${AL_RELEASE_BASE_URL:-https://github.com/$REPO/releases/download/$AL_PIN_RELEASE_TAG}"
proto="https"
case "$base_url" in
https://*) ;;
*)
	if [ "${AL_ALLOW_INSECURE_RELEASE_URL:-}" != "1" ]; then
		report "refusing to fetch a release from a non-https URL ($base_url); nothing was downloaded"
		exit 0
	fi
	log "AL_ALLOW_INSECURE_RELEASE_URL=1 is set; allowing a non-https URL for testing ($base_url)"
	proto="https,http"
	;;
esac

if ! command -v curl >/dev/null 2>&1; then
	report "no curl on PATH; cannot download al-lsp and al-explorer automatically. Install them by hand (see al-bin.sh's message)"
	exit 0
fi

sha256_of() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" | cut -d' ' -f1
	else
		return 1
	fi
}

# Read a sha256sum-style listing (line = "<64-hex-digest>  <name>", an
# optional leading "*" on the name for binary mode) and print the digest for
# the exact name given, mirroring expected_sha256() in src/lib.rs.
expected_digest() {
	local listing="$1" key="$2" digest_line line_digest line_rest
	while IFS= read -r digest_line || [ -n "$digest_line" ]; do
		line_digest="${digest_line%%[[:space:]]*}"
		line_rest="${digest_line#"$line_digest"}"
		while :; do
			case "$line_rest" in
			[[:blank:]]*) line_rest="${line_rest#?}" ;;
			"*"*) line_rest="${line_rest#\*}" ;;
			*) break ;;
			esac
		done
		case "$line_digest" in
		*[!0-9a-f]*) continue ;;
		esac
		if [ "${#line_digest}" -eq 64 ] && [ "$line_rest" = "$key" ]; then
			printf '%s\n' "$line_digest"
			return 0
		fi
	done <"$listing"
	return 1
}

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/al-fetch-release.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

fetch() {
	# $1 = url, $2 = destination file.
	curl --fail --silent --show-error --location \
		--proto "=$proto" --proto-redir "=$proto" \
		--connect-timeout 10 --max-time 120 --max-filesize 209715200 \
		-o "$2" "$1"
}

checksums_path="$work_dir/$BINARY_CHECKSUMS_ASSET"
checksums_url="$base_url/$BINARY_CHECKSUMS_ASSET"
if ! fetch "$checksums_url" "$checksums_path" || [ ! -s "$checksums_path" ]; then
	report "release $AL_PIN_RELEASE_TAG does not publish $BINARY_CHECKSUMS_ASSET ($checksums_url); refusing to install an archive with nothing to verify it against"
	exit 0
fi

archive_path="$work_dir/$asset_name"
archive_url="$base_url/$asset_name"
if ! fetch "$archive_url" "$archive_path"; then
	report "could not download $asset_name from release $AL_PIN_RELEASE_TAG ($archive_url); nothing was installed"
	exit 0
fi

if ! command -v tar >/dev/null 2>&1; then
	report "no tar on PATH; cannot extract $asset_name. Install al-lsp and al-explorer by hand"
	exit 0
fi

stage_dir="$work_dir/stage"
mkdir -p "$stage_dir"
if ! tar -xzf "$archive_path" -C "$stage_dir"; then
	report "could not extract $asset_name from release $AL_PIN_RELEASE_TAG; nothing was installed"
	exit 0
fi

file_list="$work_dir/extracted-files.txt"
(cd "$stage_dir" && find . -type f | sed 's|^\./||') | LC_ALL=C sort >"$file_list"
if [ ! -s "$file_list" ]; then
	report "$asset_name from release $AL_PIN_RELEASE_TAG extracted no files; nothing was installed"
	exit 0
fi

# Verify every extracted file before anything is made executable or moved
# into the plugin's cache directory. A file the listing does not name, or
# whose digest does not match, stops the install: nothing partial is left
# where al-bin.sh or PATH would find it.
while IFS= read -r rel; do
	key="$asset_name/$rel"
	expected="$(expected_digest "$checksums_path" "$key")"
	if [ -z "$expected" ]; then
		report "$BINARY_CHECKSUMS_ASSET from release $AL_PIN_RELEASE_TAG has no digest for $key; refusing to install anything from $asset_name"
		exit 0
	fi
	actual="$(sha256_of "$stage_dir/$rel")"
	if [ -z "$actual" ]; then
		report "no sha256sum or shasum on PATH; cannot verify $asset_name. Nothing was installed"
		exit 0
	fi
	if [ "$actual" != "$expected" ]; then
		report "checksum mismatch for $rel from $asset_name: expected $expected, got $actual. Release $AL_PIN_RELEASE_TAG's download does not match its published digest, so nothing was installed"
		exit 0
	fi
done <"$file_list"

if ! have_pair "$stage_dir"; then
	report "$asset_name from release $AL_PIN_RELEASE_TAG has verified digests but is missing al-explorer or al-lsp at its root; nothing was installed"
	exit 0
fi

chmod +x "$stage_dir/al-explorer" "$stage_dir/al-lsp"

mkdir -p "$data_dir"
rm -rf "${bin_dir:?}.new"
mv "$stage_dir" "$bin_dir.new"
rm -rf "${bin_dir:?}"
mv "$bin_dir.new" "$bin_dir"

report "installed al-lsp and al-explorer $AL_PIN_RELEASE_TAG into $bin_dir, verified against $BINARY_CHECKSUMS_ASSET. Add it to PATH this session to call them directly: export PATH=\"$bin_dir:\$PATH\" (every al-bc skill and the MCP server already find it there through al-bin.sh)"
