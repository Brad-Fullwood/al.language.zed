# Stage 4: editor features, two halves.
#   Zed half: keystrokes from zed-keys-editor.sh in the stage 3 boot (s34_boot), screenshots in $OUT/04.
#   LSP half: journey-lsp asks al-lsp directly for the same features.
# Sourced by journey.sh after stage03-zed.sh. Needs lib.sh variables.

# s4_content_check FILE: counts used by the content based Zed checks.
s4_count() { grep -c -- "$1" "$2" 2>/dev/null || true; }

s4_expectations() {
  cat > "$OUT/04/zed-expectations.txt" <<'EXP'
../03/highlight.png: Zed with the Journey Editor codeunit open. Keywords (codeunit, procedure, var, begin, exit), types (Integer) and the string/number literals are coloured, not plain text. The project file tree panel is visible.
zed-completion.png: a completion popup under the cursor at "Tot" in UseIt, listing Total (local variable) first, then snippet entries.
zed-hover.png: a hover card above or below the AddNumbers call showing the procedure signature "procedure AddNumbers(A: Integer; B: Integer): Integer".
zed-definition.png: the cursor is on the declaration line "procedure AddNumbers" (line 3) with the name selected, not on the call at line 12.
zed-diagnostics-error.png: a red squiggle at line 13 or 14 (the statement "Total := 5" has no semicolon, the error may sit on the next line), with an error marker in the gutter or status bar and a syntax error message.
zed-project-diagnostics.png: the Project Diagnostics tab listing the syntax error for JourneyEditor.Codeunit.al.
zed-codeaction.png: a code actions menu next to line 13 (quick fix entries for the error). An empty menu or no menu means no code action is offered.
zed-diagnostics-clear.png: the same code as the error shot with the "Total := 5" line removed, no squiggle, no error count in the status bar.
zed-rename-box.png: the inline rename editor (an extra row under line 3) holding SumNumbers.
zed-rename.png: the declaration on line 3 and the call on line 12 both read SumNumbers.
zed-format.png: the Messy procedure re-indented (Count: Integer under var, and the statements under begin, indented one level deeper than var/begin), saved with no dirty dot on the tab.
zed-snippets-menu.png: a completion menu on the blank line between UseIt and Messy with the "tprocedure" snippet entry.
zed-snippets.png: a new procedure skeleton (local procedure MyProcedure() with var, begin, end) inserted between UseIt and Messy.
zed-outline.png: the outline picker listing AddNumbers, UseIt, Messy (and SumNumbers after the rename).
EXP
}

stage_04() {
  mkdir -p "$OUT/04"
  s4_expectations
  s4_zed_half
  s4_lsp_half
}

s4_zed_half() {
  s34_boot; local rc=$?
  local run="$OUT/04/zedrun"
  if [[ $rc == 2 ]]; then
    local f
    for f in completion hover definition diagnostics diagnostics-clear codeaction rename format-on-save snippets outline project-diagnostics; do
      record 04 "zed.$f" SKIP "no project at $PROJ (stage 1 has not run)"
    done
    return 0
  fi
  if [[ ! -f "$run/shot-highlight.png" || ! -s "$run/shot-highlight.png" ]]; then
    local f
    for f in completion hover definition diagnostics diagnostics-clear codeaction rename format-on-save snippets outline project-diagnostics; do
      record 04 "zed.$f" FAIL "Zed did not render, no screenshots (drive.sh exit $rc, see $(step_log 03 zed-boot))"
    done
    return 0
  fi
  # screenshots: shot-<name>.png -> zed-<name>.png
  local p
  for p in "$run"/shot-*.png; do
    [[ -f "$p" ]] || continue
    cp "$p" "$OUT/04/zed-$(basename "$p" | sed 's/^shot-//')"
  done

  shotrec() { # ITEM PNG-NAME
    if [[ -f "$OUT/04/zed-$2.png" ]]; then record 04 "zed.$1" SHOT "$OUT/04/zed-$2.png"
    else record 04 "zed.$1" FAIL "no screenshot zed-$2.png (keystroke script did not reach it, see $(step_log 03 zed-boot))"; fi
  }
  shotrec completion completion
  shotrec hover hover
  shotrec definition definition
  shotrec diagnostics diagnostics-error
  shotrec diagnostics-clear diagnostics-clear
  shotrec codeaction codeaction
  shotrec outline outline
  shotrec project-diagnostics project-diagnostics

  local before="$OUT/04/JourneyEditor.original.al"
  local after="$run/artifact-after-format.al" snip="$run/artifact-after-snippet.al"
  # rename: content proof
  if [[ -f "$after" ]]; then
    cp "$after" "$OUT/04/zed-after-format.al"
    local new old
    new=$(s4_count SumNumbers "$after"); old=$(s4_count AddNumbers "$after")
    if [[ "$new" -ge 2 && "$old" == 0 ]]; then
      record 04 zed.rename PASS "SumNumbers on $new lines, AddNumbers on 0 (declaration and call renamed in the saved file)"
      record 04 zed.rename.shot SHOT "$OUT/04/zed-rename.png"
    else
      record 04 zed.rename FAIL "saved file has SumNumbers on $new lines and AddNumbers on $old (expected >=2 and 0)"
    fi
    # format on save: content proof (before = shipped file, after = saved file)
    if grep -q '^    Count := 1;' "$before" && ! grep -q '^    Count := 1;' "$after" && grep -q '^        Count := 1;' "$after"; then
      record 04 zed.format-on-save PASS "Messy body re-indented from 4 to 8 spaces after ctrl-s ($(diff "$before" "$after" | grep -c '^>') lines changed besides the rename)"
      record 04 zed.format-on-save.shot SHOT "$OUT/04/zed-format.png"
    else
      record 04 zed.format-on-save FAIL "saved file is not re-indented, see $OUT/04/zed-after-format.al vs $before"
    fi
    # the error line was deleted again: no trace of it in the saved file
    if grep -q "Total := 5" "$after"; then
      record 04 zed.diagnostics-fix FAIL "the broken line is still in the saved file"
    else
      record 04 zed.diagnostics-fix PASS "broken line removed in the buffer, saved file is clean"
    fi
  else
    record 04 zed.rename FAIL "no file copy after save (keystroke script stopped early)"
    record 04 zed.format-on-save FAIL "no file copy after save (keystroke script stopped early)"
  fi
  # snippets: content proof
  if [[ -f "$snip" ]]; then
    cp "$snip" "$OUT/04/zed-after-snippet.al"
    if grep -q 'procedure MyProcedure' "$snip"; then
      record 04 zed.snippets PASS "tprocedure expanded to a procedure MyProcedure skeleton in the saved file"
      record 04 zed.snippets.shot SHOT "$OUT/04/zed-snippets-menu.png"
    else
      record 04 zed.snippets FAIL "no 'procedure MyProcedure' in the saved file, see $OUT/04/zed-snippets-menu.png"
    fi
  else
    record 04 zed.snippets FAIL "no file copy after the snippet step"
  fi
}

s4_lsp_half() {
  local bin="$TARGET_DIR/debug/journey-lsp" lsp="$OUT/04/lsp"
  mkdir -p "$lsp"
  if [[ ! -f "$PROJ/app.json" ]]; then
    record 04 lsp.all SKIP "no project at $PROJ (stage 1 has not run)"; return 0
  fi
  if ! (cd "$ROOT" && CARGO_TARGET_DIR="$TARGET_DIR" run_step 04 journey-lsp-build 1200 cargo build -q -p al-test-harness --bin journey-lsp); then
    record 04 lsp.all FAIL "journey-lsp did not build, see $(step_log 04 journey-lsp-build)"; return 0
  fi
  # work on a copy: the server may write caches into the project
  rm -rf "$lsp/proj"; cp -a "$PROJ" "$lsp/proj"
  cp "$S34_DIR/JourneyEditor.Codeunit.al" "$lsp/proj/src/JourneyEditor.Codeunit.al"
  rm -f "$lsp/lines.txt"
  AL_LSP_BIN="$ALLSP" RUST_LOG=warn run_step 04 journey-lsp 300 "$bin" --project "$lsp/proj" --out "$lsp"
  local rc=$? line status item detail n=0
  while IFS= read -r line; do
    status="${line%% *}"; line="${line#* }"; item="${line%% *}"; detail="${line#* }"
    case "$status" in PASS|FAIL|SKIP) record 04 "$item" "$status" "$detail"; n=$((n+1)) ;; esac
  done < <(grep -E '^(PASS|FAIL|SKIP) ' "$lsp/lines.txt" 2>/dev/null)
  if [[ $n == 0 ]]; then
    record 04 lsp.all FAIL "journey-lsp produced no result lines (exit $rc), see $(step_log 04 journey-lsp)"
  elif [[ $rc != 0 ]]; then
    record 04 lsp.exit NIGGLE "journey-lsp exited $rc after $n result lines"
  fi
}
