#!/usr/bin/env bash
# Real-project validation gate (FB-16).
#
# "Tested and working" claims MUST be backed by this script passing against
# real AL projects — fixture projects and hermetic e2e suites do not count.
# Fixtures are small, ASCII, space-free, and have no real symbol packages;
# every defect in Feedback.md (2026-06-12) shipped behind green fixtures.
#
# Checks run the exact USER entry points (CLI commands as the Zed tasks
# invoke them, including shell substitution with paths containing spaces),
# not internal library functions.
#
# Usage:
#   scripts/validate-real-projects.sh
#   AL_VALIDATE_PROJECT_A=/path/a AL_VALIDATE_PROJECT_B="/path/with space/b" \
#     scripts/validate-real-projects.sh
#   AL_VALIDATE_NETWORK=1 ...   # also exercise symbol downloads (slow)
set -u

PROJECT_A="${AL_VALIDATE_PROJECT_A:-$HOME/Dev/AL/Debar-Git/brad-advania-journals}"
PROJECT_B="${AL_VALIDATE_PROJECT_B:-$HOME/Dev/AL/JIG UK/JIG UK}"

PASS=0
FAIL=0
declare -a FAILURES=()

ok()   { PASS=$((PASS+1)); printf '  [PASS] %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); FAILURES+=("$1"); printf '  [FAIL] %s\n' "$1"; }

require_binary() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "FATAL: '$1' not on PATH — run 'make install' first." >&2
        exit 2
    fi
}
require_binary al-explorer
require_binary al-lsp

check_project_exists() {
    local proj="$1"
    if [ ! -f "$proj/app.json" ]; then
        echo "FATAL: real project not found: $proj" >&2
        echo "Set AL_VALIDATE_PROJECT_A / AL_VALIDATE_PROJECT_B to real AL projects." >&2
        exit 2
    fi
}
check_project_exists "$PROJECT_A"
check_project_exists "$PROJECT_B"

# Fresh daemons so we validate cold-start behaviour too.
pkill -f "al-lsp[ ]daemon" 2>/dev/null && sleep 11 || true

validate_project() {
    local proj="$1"
    echo
    echo "=== Validating against: $proj ==="
    cd "$proj" || { bad "cd $proj"; return; }

    # 1. doctor
    if al-explorer doctor >/dev/null 2>&1; then
        ok "doctor"
    else
        bad "doctor exited non-zero"
    fi

    # 2. symbol search returns real results, no sentinel IDs in output
    local search_out
    search_out=$(al-explorer search "a" --limit 50 2>&1)
    if echo "$search_out" | grep -qE "results$"; then
        ok "search returns results"
    else
        bad "search produced no results: $(echo "$search_out" | head -2)"
    fi
    if echo "$search_out" | grep -qE "^\S+\s+-[0-9]"; then
        bad "search output shows negative object IDs (FB-2/3 regression)"
    else
        ok "no sentinel/negative IDs in search output"
    fi

    # 3. compile — success, or failure WITH an explanation (never bare)
    local compile_out compile_rc
    compile_out=$(al-explorer compile 2>&1); compile_rc=$?
    if [ $compile_rc -eq 0 ]; then
        ok "compile succeeds"
    elif [ "$(echo "$compile_out" | wc -l)" -gt 1 ]; then
        ok "compile failed but explained itself ($(echo "$compile_out" | wc -l) lines)"
    else
        bad "compile failed with no explanation (FB-14 regression): $compile_out"
    fi

    # 4. format through the SHELL exactly as Zed tasks run it, on a COPY in
    #    a directory with spaces (FB-13/FB-18). Must not split the path and
    #    must be idempotent.
    local tmpdir file copy
    tmpdir=$(mktemp -d "/tmp/al validate XXXX") || { bad "mktemp"; return; }
    file=$(find . -name "*.al" -path "*objects*" | head -1)
    [ -z "$file" ] && file=$(find . -name "*.al" | head -1)
    if [ -n "$file" ]; then
        copy="$tmpdir/copy with space.al"
        cp "$file" "$copy"
        if ZED_FILE="$copy" /bin/fish -c 'al-explorer format "$ZED_FILE"' >/dev/null 2>&1; then
            local first second
            first=$(cat "$copy")
            ZED_FILE="$copy" /bin/fish -c 'al-explorer format "$ZED_FILE"' >/dev/null 2>&1
            second=$(cat "$copy")
            if [ "$first" = "$second" ]; then
                ok "format via fish-substituted spaced path, idempotent"
            else
                bad "format is not idempotent on $file"
            fi
        else
            bad "format failed through fish on spaced path (FB-13 regression)"
        fi
    else
        bad "no .al file found to format"
    fi
    rm -rf "$tmpdir"

    # 5. dead-code runs and carries confidence grouping (FB-12)
    local dc_out
    dc_out=$(al-explorer dead-code 2>&1)
    if [ $? -eq 0 ]; then
        if echo "$dc_out" | grep -qiE "confidence|No dead code"; then
            ok "dead-code runs with confidence labelling"
        else
            bad "dead-code output lost confidence labelling (FB-12 regression)"
        fi
    else
        bad "dead-code exited non-zero"
    fi

    # 6. events/subscribers/trace pipeline (FB-6/7/8)
    if al-explorer events "OnAfter" >/dev/null 2>&1; then
        ok "events search"
    else
        bad "events search failed"
    fi
    # trace must EXPLAIN itself when there is no chain (FB-9)
    local trace_out
    trace_out=$(al-explorer trace "DefinitelyNotAnEventName123" 2>&1)
    if echo "$trace_out" | grep -q "may not be an event"; then
        ok "trace explains non-event input"
    else
        bad "trace silent on non-event input (FB-9 regression)"
    fi

    # 7. optional: symbol download over the network
    if [ "${AL_VALIDATE_NETWORK:-0}" = "1" ]; then
        local dl_out dl_rc
        dl_out=$(al-explorer download-symbols --source nuget 2>&1); dl_rc=$?
        if echo "$dl_out" | grep -q "os error 11"; then
            bad "download-symbols leaked EAGAIN (FB-15 regression)"
        elif echo "$dl_out" | grep -q "BCPublic"; then
            bad "download-symbols still references dead BCPublic feed"
        else
            ok "download-symbols completed without raw socket errors (rc=$dl_rc)"
        fi
    fi
}

validate_project "$PROJECT_A"
validate_project "$PROJECT_B"

# 8. TUI smoke over a real pty against project A: first frame fast, data
#    populates, no crash. Skipped when python3 is unavailable.
echo
echo "=== TUI smoke (pty) ==="
if command -v python3 >/dev/null 2>&1; then
    TUI_OUT=$(python3 - "$PROJECT_A" << 'PYEOF'
import os, pty, time, fcntl, termios, struct, sys, select
project = sys.argv[1]
pid, fd = pty.fork()
if pid == 0:
    os.chdir(project)
    os.execvp("al-explorer", ["al-explorer"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 180, 0, 0))
start = time.time(); buf = b""; data_at = None
while time.time() < start + 30:
    r, _, _ = select.select([fd], [], [], 0.2)
    if r:
        try: chunk = os.read(fd, 65536)
        except OSError: break
        if not chunk: break
        buf += chunk
        text = buf.decode("utf-8", "replace")
        if data_at is None and ("Table" in text or "Codeunit" in text):
            data_at = time.time() - start
            break
os.write(fd, b"q"); time.sleep(0.3)
try: os.kill(pid, 15)
except ProcessLookupError: pass
print(f"data_at={data_at}")
PYEOF
)
    DATA_AT=$(echo "$TUI_OUT" | grep -oP 'data_at=\K[0-9.]+' || true)
    if [ -n "$DATA_AT" ]; then
        if awk "BEGIN{exit !($DATA_AT < 10)}"; then
            ok "TUI shows data in ${DATA_AT}s"
        else
            bad "TUI took ${DATA_AT}s to show data (FB-1 regression threshold: 10s)"
        fi
    else
        bad "TUI never showed object data within 30s"
    fi
else
    echo "  [SKIP] python3 unavailable — TUI smoke skipped"
fi

echo
echo "=================================================="
echo "Real-project validation: $PASS passed, $FAIL failed"
if [ $FAIL -gt 0 ]; then
    echo "FAILURES:"
    for f in "${FAILURES[@]}"; do echo "  - $f"; done
    echo
    echo "DO NOT report features as 'tested and working' while this gate fails."
    exit 1
fi
echo "Gate PASSED — real-project entry points verified."
