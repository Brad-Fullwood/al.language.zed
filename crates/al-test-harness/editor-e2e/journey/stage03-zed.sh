# Stage 3: open the project in headless Zed (podman, via drive.sh) and check the extension loads.
# The same Zed boot also runs stage 4's keystrokes (zed-keys-editor.sh), so s34_boot is
# shared: whichever stage runs first boots Zed, the other reuses the result.
# Sourced by journey.sh. Needs lib.sh variables (ROOT OUT PROJ TARGET_DIR).

S34_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
S34_EDITOR_FILE="src/JourneyEditor.Codeunit.al"

# s34_boot: one Zed boot for stages 3 and 4. Results land in $OUT/04/zedrun.
# Returns 0 when drive.sh passed, 1 when it failed, 2 when there is no project.
s34_boot() {
  local run="$OUT/04/zedrun" marker="$OUT/04/.boot-rc"
  [[ -f "$marker" ]] && return "$(cat "$marker")"
  mkdir -p "$OUT/03" "$OUT/04" "$run"
  if [[ ! -f "$PROJ/app.json" ]]; then echo 2 > "$marker"; return 2; fi
  mkdir -p "$PROJ/src"
  cp "$S34_DIR/JourneyEditor.Codeunit.al" "$PROJ/$S34_EDITOR_FILE"
  cp "$S34_DIR/JourneyEditor.Codeunit.al" "$OUT/04/JourneyEditor.original.al"
  # Zed settings: format on save through the language server, no telemetry or onboarding noise.
  cat > "$OUT/04/zed-settings.json" <<'JSON'
{
  "telemetry": { "diagnostics": false, "metrics": false },
  "autosave": "off",
  "languages": {
    "AL": { "format_on_save": "on", "formatter": "language_server" }
  }
}
JSON
  # drive.sh finds al-lsp at $ROOT/target/debug/al-lsp.
  [[ -e "$ROOT/target" ]] || ln -s "$TARGET_DIR" "$ROOT/target"
  # cage (the headless compositor) sometimes segfaults on a loaded machine: retry the boot.
  local attempt rc=1
  for attempt in 1 2 3; do
    rm -rf "$run"; mkdir -p "$run"
    SLOW_SECONDS=900 run_step 03 zed-boot 1500 \
      bash "$ROOT/crates/al-test-harness/editor-e2e/drive.sh" \
        --proj-dir "$PROJ" --file "$S34_EDITOR_FILE" \
        --zed-settings "$OUT/04/zed-settings.json" \
        --keys-file "$S34_DIR/zed-keys-editor.sh" \
        --out "$run/zed-final.png"
    rc=$?
    [[ $rc == 0 ]] && break
    grep -q "Segmentation fault" "$(step_log 03 zed-boot)" || break
    cp "$(step_log 03 zed-boot)" "$OUT/03/zed-boot.attempt$attempt.log"
  done
  echo "$rc" > "$marker"
  return "$rc"
}

stage_03() {
  mkdir -p "$OUT/03" "$OUT/04"
  s34_boot; local rc=$?
  local run="$OUT/04/zedrun" log; log="$(step_log 03 zed-boot)"
  if [[ $rc == 2 ]]; then
    record 03 zed.boot SKIP "no project at $PROJ (stage 1 has not run)"
    return 0
  fi
  if [[ $rc == 0 ]]; then
    record 03 zed.boot PASS "drive.sh exit 0, took $(cat "$OUT/03/zed-boot.secs")s"
  else
    record 03 zed.boot FAIL "drive.sh exit $rc, see $log"
  fi

  # al-lsp spawned by Zed (the extension started it)
  local procs; procs=$(grep -o 'AL_LSP_PROCS: [0-9]*' "$log" | tail -1 | grep -o '[0-9]*$')
  if [[ -n "$procs" && "$procs" -ge 1 ]]; then
    record 03 zed.al-lsp-spawn PASS "AL_LSP_PROCS: $procs stdio server(s)"
  else
    record 03 zed.al-lsp-spawn FAIL "AL_LSP_PROCS: ${procs:-missing}, see $log"
  fi

  # extension loaded: Zed's application log or the install step shows it
  local applog="$run/artifact-Zed.log"
  [[ -f "$applog" ]] || applog="$run/zed-app.log"
  if [[ -f "$applog" ]]; then
    cp "$applog" "$OUT/03/Zed.log"
    local ext; ext=$(grep -iE "extensions updated\. loading [1-9]|starting language server al-lsp" "$applog" | head -5)
    printf '%s\n' "$ext" > "$OUT/03/extension-lines.txt"
    if [[ -n "$ext" ]]; then
      record 03 zed.extension-loads PASS "$(echo "$ext" | head -1 | cut -c1-160)"
    elif [[ -n "$procs" && "$procs" -ge 1 ]]; then
      record 03 zed.extension-loads PASS "no log line names it, but Zed started al-lsp so the extension ran"
    else
      record 03 zed.extension-loads FAIL "no extension line in Zed.log and no al-lsp process"
    fi
    # error lines that mention the extension or the language server
    grep -iE "(error|warn|panic|fail)" "$applog" \
      | grep -iE "extension|language server|al-lsp|\bal\b|wasm|grammar|lsp" > "$OUT/03/log-errors.txt" || true
    local n; n=$(wc -l < "$OUT/03/log-errors.txt")
    if [[ "$n" == 0 ]]; then
      record 03 zed.log-errors PASS "no extension or LSP error lines in Zed.log"
    else
      record 03 zed.log-errors FAIL "$n error lines, first: $(head -1 "$OUT/03/log-errors.txt" | cut -c1-200) (all in $OUT/03/log-errors.txt)"
    fi
  else
    record 03 zed.extension-loads FAIL "no Zed log was collected"
    record 03 zed.log-errors SKIP "no Zed log was collected"
  fi

  # highlighting screenshot (taken by the keys script before any keystroke)
  if [[ -f "$run/shot-highlight.png" ]]; then
    cp "$run/shot-highlight.png" "$OUT/03/highlight.png"
    record 03 zed.highlight SHOT "$OUT/03/highlight.png"
  else
    record 03 zed.highlight FAIL "no screenshot was taken"
  fi
}
