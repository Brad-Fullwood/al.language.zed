#!/usr/bin/env bash
#
# SessionEnd hook. In an AL project, stop the al-lsp daemon this session
# started. Anywhere else, do nothing.
#
# The daemon holds the dependency source index, which reaches gigabytes on a
# project with Base Application, and nothing releases it while the daemon
# lives. It exits on its own after 30 idle minutes; this reclaims it when the
# session that was using it ends.
#
# `al-explorer daemon-shutdown` returns only once the endpoint has stopped
# accepting, so the next session cannot connect to a dying daemon.
#
# Everything this script writes goes to stderr, and it always exits 0: a
# session ending must not fail because a daemon could not be stopped.

set -uo pipefail

payload="$(cat)"

cwd=""
if command -v jq >/dev/null 2>&1; then
	cwd="$(printf '%s' "$payload" | jq -r '.cwd // empty' 2>/dev/null || true)"
fi
[ -n "$cwd" ] || cwd="${CLAUDE_PROJECT_DIR:-$PWD}"

# The same AL-project test the SessionStart hook uses: an app.json that
# declares an id and a publisher.
app_json="$cwd/app.json"
[ -f "$app_json" ] || exit 0
if command -v jq >/dev/null 2>&1; then
	jq -e 'has("id") and has("publisher")' "$app_json" >/dev/null 2>&1 || exit 0
else
	grep -q '"publisher"' "$app_json" 2>/dev/null || exit 0
fi

cd "$cwd" || exit 0

al_bin="${CLAUDE_PLUGIN_ROOT:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}/scripts/al-bin.sh"
[ -x "$al_bin" ] || exit 0

# al-bin.sh exits 127 with installation instructions when it finds no
# binaries. At session end there is nothing to install for, so stay quiet.
if ! output="$("$al_bin" al-explorer daemon-shutdown 2>&1)"; then
	printf 'al-session-end: could not stop the al-lsp daemon for %s\n' "$cwd" >&2
	printf '%s\n' "$output" >&2
fi

exit 0
