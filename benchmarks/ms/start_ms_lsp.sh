#!/usr/bin/env bash
# Launch Microsoft's AL language server (EditorServices host) on stdio.
# Prefers the self-contained native binary; falls back to `dotnet <dll>`.
set -euo pipefail

EXT="${AL_MS_EXT:?Set AL_MS_EXT to the installed Microsoft AL extension directory}"
BIN="$EXT/bin/linux/Microsoft.Dynamics.Nav.EditorServices.Host"
DLL="$EXT/bin/linux/Microsoft.Dynamics.Nav.EditorServices.Host.dll"

SESSION_ID="$(cat /proc/sys/kernel/random/uuid 2>/dev/null || echo 00000000-0000-0000-0000-000000000000)"
ARGS=(
  /disableTelemetry
  /browser:SystemDefault
  /logLevel:Normal
  /semanticFolding:true
  /extendGoToSymbolInWorkspace:true
  /extendGoToSymbolInWorkspaceResultLimit:100
  /extendGoToSymbolInWorkspaceIncludeSymbolFiles:true
  /testCoverageCachePath:./.coverage
  "/sessionId:$SESSION_ID"
)

if [[ -x "$BIN" ]]; then
  exec "$BIN" "${ARGS[@]}"
elif [[ -f "$DLL" ]]; then
  exec dotnet "$DLL" "${ARGS[@]}"
else
  echo "Microsoft AL language server not found under $EXT/bin/linux" >&2
  exit 127
fi
