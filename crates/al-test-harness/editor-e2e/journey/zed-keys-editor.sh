#!/usr/bin/env bash
# Runs INSIDE the container after Zed trusted the project (drive.sh --keys-file).
# Opens on src/JourneyEditor.Codeunit.al (line numbers below are 1-based and match
# journey/JourneyEditor.Codeunit.al), presses the Zed key chords for each editor
# feature, and writes a screenshot after each one to /out/shot-<name>.png.
# Files worth checking on the host are copied to /out/artifact-<name>.al.
# Zed default Linux keys: ctrl-g go to line, ctrl-space completion, ctrl-k ctrl-i hover,
# F12 definition, ctrl-. code actions, F2 rename, ctrl-s save, ctrl-shift-o outline,
# ctrl-shift-m project diagnostics, ctrl-shift-k delete line.
OUTD=${OUTD:-/out}
F=/tmp/proj/src/JourneyEditor.Codeunit.al

shot()  { sleep "${2:-2}"; grim "$OUTD/shot-$1.png"; echo "KEYS shot $1"; }
snap()  { cp "$F" "$OUTD/artifact-$1.al"; echo "KEYS snap $1"; }
press() { wtype "$@"; sleep 0.5; }
chord() { wtype -M ctrl -k "$1" -m ctrl; sleep 0.5; }
cchord() { wtype -M ctrl -M shift -k "$1" -m shift -m ctrl; sleep 0.5; }
goto()  { chord g; sleep 1; wtype "$1"; sleep 0.5; wtype -k Return; sleep 1; }
esc()   { wtype -k Escape; sleep 0.5; }

snap before
shot highlight 0

# Wait for al-lsp to finish its project scan before any editor request. With the Base Application
# symbols in .alpackages the container builds the dependency source index from nothing (about 60 s,
# the host keeps a cache), and requests sent meanwhile are cancelled: F2 opened no rename box and
# the ctrl-a plus typed text then replaced the whole buffer. Capped at 240 s.
ready=0
while (( ready < 240 )); do
  grep -q "Published project-scoped diagnostics generation" "$HOME/.local/share/al-lsp/logs/al-lsp.log" 2>/dev/null && break
  sleep 1; ready=$((ready + 1))
done
echo "$ready" > "$OUTD/artifact-ready-wait.txt"
echo "KEYS al-lsp ready after waiting ${ready}s"
sleep 2

# completion: a blank line 13 inside UseIt, a partial identifier, ctrl-space
goto 13:1
press -k Return
goto 13:1
wtype '        Tot'; sleep 1
chord space
shot completion 3
esc
cchord k          # delete the typed line (the blank line pushed exit(Total) down, so it stays)
sleep 1

# hover on the AddNumbers call
goto 12:21
sleep 2           # hover shows nothing when asked right after a jump, the server needs a moment
wtype -M ctrl -k k -m ctrl; sleep 0.3; wtype -M ctrl -k i -m ctrl
shot hover 2
esc

# go to definition from the call: cursor should land on line 3
press -k F12
shot definition 2

# deliberate syntax error: a statement without its semicolon on its own line
goto 13:1
wtype 'Total := 5'; press -k Return
sleep 5
shot diagnostics-error 1
# project diagnostics while the error exists
cchord m
shot project-diagnostics 3
chord w           # close the diagnostics tab
sleep 1
# code action menu on the error
goto 13:6
chord period
shot codeaction 3
esc
# fix: delete the error line, wait for the diagnostic to clear
goto 13:1
cchord k
sleep 5
shot diagnostics-clear 1

# rename the procedure from its declaration
goto 3:20
sleep 3           # a cold or busy al-lsp answers prepareRename late, and F2 before that does nothing
press -k F2
sleep 3
chord a
wtype 'SumNumbers'; sleep 0.5
shot rename-box 1
press -k Return
sleep 3
shot rename 1

# save: format on save re-indents the Messy procedure
chord s
sleep 10          # Zed waits 5 s for the formatter, then saves unformatted (Zed.log: timed out waiting for formatting)
esc               # nudge a repaint, the software renderer can show the frame from before the save
shot format 3
snap after-format

# snippet: type a prefix on the blank line 15 and accept the completion
goto 15:1
wtype 'tprocedure'; sleep 2
shot snippets-menu 1
press -k Return
sleep 2
shot snippets 1
chord s
sleep 3
snap after-snippet

# document outline
esc
cchord o
shot outline 3
esc

# Zed's own log (drive.sh's zed-app.log picks telemetry.log, so copy the right one)
cp "$HOME/.local/share/zed/logs/Zed.log" "$OUTD/artifact-Zed.log" 2>/dev/null || true
# al-lsp's own log (start time, index, semantic bridge, slow requests) for the same run.
cp "$HOME/.local/share/al-lsp/logs/al-lsp.log" "$OUTD/artifact-al-lsp.log" 2>/dev/null || true
