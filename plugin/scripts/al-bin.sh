#!/usr/bin/env bash
#
# Resolve al-explorer / al-lsp on this machine and exec the requested one.
#
#   al-bin.sh al-explorer --json search "Sales-Post"
#   al-bin.sh al-lsp mcp --project /path/to/project
#
# Every message this script writes goes to stderr. Stdout belongs to the binary,
# because .mcp.json runs `al-bin.sh al-lsp mcp` and Claude Code reads JSON-RPC
# frames from that stream.

set -euo pipefail

REPO_URL="https://github.com/Brad-Fullwood/al.language.zed"

log() {
	printf '%s\n' "$*" >&2
}

usage_error() {
	log "al-bin.sh: $1"
	log "usage: al-bin.sh <al-explorer|al-lsp> [args...]"
	exit 64
}

requested="${1-}"
case "$requested" in
al-explorer | al-lsp) shift ;;
'') usage_error "no binary named" ;;
*) usage_error "unknown binary '$requested' (expected al-explorer or al-lsp)" ;;
esac

# A candidate directory has to hold both binaries. al-explorer spawns the daemon
# by looking next to its own executable before it walks PATH
# (crates/al-protocol/src/client.rs), so a directory holding only one of them
# gives the CLI and the daemon different builds.
has_pair() {
	local dir="$1" name
	[ -n "$dir" ] && [ -d "$dir" ] || return 1
	for name in al-explorer al-lsp; do
		if [ ! -x "$dir/$name" ] && [ ! -x "$dir/$name.exe" ]; then
			return 1
		fi
	done
	return 0
}

bin_dir=""

# 1. Explicit override.
if [ -n "${AL_BIN_DIR-}" ] && has_pair "$AL_BIN_DIR"; then
	bin_dir="$AL_BIN_DIR"
fi

# 2. A target/ directory in an enclosing checkout of this repository, for
#    someone developing the toolchain and the plugin together.
if [ -z "$bin_dir" ]; then
	search="${CLAUDE_PLUGIN_ROOT-}"
	if [ -z "$search" ]; then
		search="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
	fi
	while [ -n "$search" ] && [ "$search" != "/" ]; do
		for profile in release debug; do
			if has_pair "$search/target/$profile"; then
				bin_dir="$search/target/$profile"
				break
			fi
		done
		[ -z "$bin_dir" ] || break
		search="$(dirname -- "$search")"
	done
fi

# 3. PATH.
if [ -z "$bin_dir" ]; then
	on_path="$(command -v al-explorer 2>/dev/null || true)"
	if [ -n "$on_path" ]; then
		candidate="$(CDPATH='' cd -- "$(dirname -- "$on_path")" && pwd)"
		if has_pair "$candidate"; then
			bin_dir="$candidate"
		fi
	fi
fi

# 4. A release archive unpacked into the plugin's persistent data directory,
#    which survives plugin updates.
if [ -z "$bin_dir" ] && [ -n "${CLAUDE_PLUGIN_DATA-}" ] && has_pair "${CLAUDE_PLUGIN_DATA}/bin"; then
	bin_dir="${CLAUDE_PLUGIN_DATA}/bin"
fi

if [ -z "$bin_dir" ]; then
	data_bin="${CLAUDE_PLUGIN_DATA:-$HOME/.claude/plugins/data/al-bc}/bin"
	log "al-bin.sh: al-explorer and al-lsp were not found."
	log ""
	log "The al-bc plugin needs both binaries in one directory. Install them one of these ways."
	log ""
	log "1. cargo install from a checkout (needs Rust and the .NET 8 SDK):"
	log "     git clone $REPO_URL"
	log "     cd al.language.zed"
	log "     cargo install --path crates/al-explorer"
	log "     cargo install --path crates/al-lsp --bin al-lsp --features semantic"
	log "   Both land in ~/.cargo/bin, which this script finds on PATH."
	log ""
	log "2. Download a release archive. It carries al-lsp, al-explorer and the"
	log "   semantic bridge together:"
	log "     $REPO_URL/releases/latest/download/al-linux-x86_64.tar.gz"
	log "   Other assets: al-linux-aarch64.tar.gz, al-macos-x86_64.tar.gz,"
	log "   al-macos-aarch64.tar.gz, al-windows-x86_64.zip."
	log "     mkdir -p \"$data_bin\""
	log "     tar -xzf al-linux-x86_64.tar.gz -C \"$data_bin\""
	log ""
	log "3. Point AL_BIN_DIR at a directory that already holds both:"
	log "     export AL_BIN_DIR=/path/to/dir"
	log ""
	log "Searched: \$AL_BIN_DIR, target/release and target/debug above the plugin"
	log "directory, PATH, and \"$data_bin\"."
	exit 127
fi

exe="$bin_dir/$requested"
[ -x "$exe" ] || exe="$exe.exe"

exec "$exe" "$@"
