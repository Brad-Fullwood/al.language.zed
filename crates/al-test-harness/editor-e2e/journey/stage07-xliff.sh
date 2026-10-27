# Stage 7: XLIFF and permission set generation compared with Microsoft alc.
# shellcheck shell=bash

# journey_xlf_ids FILE: trans-unit ids, sorted, one per line.
journey_xlf_ids() {
  python3 -I -c '
import sys, xml.etree.ElementTree as ET
t = ET.parse(sys.argv[1])
for e in t.iter():
    if e.tag.endswith("}trans-unit") or e.tag == "trans-unit":
        print(e.get("id"))
' "$1" | sort
}

stage_07() {
  local s=07
  mkdir -p "$OUT/$s"
  if [[ ! -f "$PROJ/app.json" ]]; then record "$s" project SKIP "no project from stage 1"; return 0; fi
  type journey_write_sources > /dev/null 2>&1 && journey_write_sources

  local tmp="$PROJ/app.json.tmp"
  jq '.features = ["TranslationFile"]' "$PROJ/app.json" > "$tmp" && mv "$tmp" "$PROJ/app.json"
  [[ "$(jq -c .features "$PROJ/app.json")" == '["TranslationFile"]' ]] && record "$s" feature PASS "TranslationFile enabled in app.json" || record "$s" feature FAIL "could not set features"
  rm -rf "$PROJ/Translations"

  # Native
  local gfile='' nat_x="$OUT/$s/native.g.xlf" alc_x="$OUT/$s/alc.g.xlf"
  if journey_alx "$s" xlf-generate 60 xlf generate --json; then
    gfile=$(journey_body "$s" xlf-generate | jq -r .path 2>/dev/null)
    if [[ -s "$gfile" ]]; then
      cp "$gfile" "$nat_x"; record "$s" xlf-generate PASS "$(basename "$gfile"), $(journey_body "$s" xlf-generate | jq -r .units) units"
    else record "$s" xlf-generate FAIL "exit 0 but no file at '$gfile'"; fi
  else record "$s" xlf-generate FAIL "xlf generate exit $(cat "$OUT/$s/xlf-generate.rc")"; fi
  rm -rf "$PROJ/Translations"

  # alc writes Translations/<Name>.g.xlf when TranslationFile is on.
  local arc=0
  journey_alc_build "$OUT/$s/alc.app" alc 07 || arc=$?
  local af; af=$(find "$PROJ/Translations" -name '*.g.xlf' 2>/dev/null | head -n 1)
  if [[ "$arc" == 0 && -s "$af" ]]; then cp "$af" "$alc_x"; record "$s" alc-xlf PASS "alc wrote $(basename "$af")"; else record "$s" alc-xlf FAIL "alc exit $arc, g.xlf present: $([[ -s $af ]] && echo yes || echo no)"; fi

  if [[ -s "$nat_x" && -s "$alc_x" ]]; then
    if ! xmllint --noout "$nat_x" 2> /dev/null; then record "$s" native-xlf-wellformed FAIL "native g.xlf is not well formed"; fi
    journey_xlf_ids "$nat_x" > "$OUT/$s/native.ids"
    journey_xlf_ids "$alc_x" > "$OUT/$s/alc.ids"
    comm -12 "$OUT/$s/native.ids" "$OUT/$s/alc.ids" > "$OUT/$s/both.ids"
    comm -23 "$OUT/$s/native.ids" "$OUT/$s/alc.ids" > "$OUT/$s/native-only.ids"
    comm -13 "$OUT/$s/native.ids" "$OUT/$s/alc.ids" > "$OUT/$s/alc-only.ids"
    local nb nn na nt at
    nb=$(wc -l < "$OUT/$s/both.ids"); nn=$(wc -l < "$OUT/$s/native-only.ids"); na=$(wc -l < "$OUT/$s/alc-only.ids")
    nt=$(wc -l < "$OUT/$s/native.ids"); at=$(wc -l < "$OUT/$s/alc.ids")
    if [[ "$nn" == 0 && "$na" == 0 && "$nt" -gt 0 ]]; then record "$s" xlf-ids PASS "$nb ids match"; else record "$s" xlf-ids FAIL "$nb match, $nn only native, $na only alc (native $nt, alc $at), lists in $s/native-only.ids and $s/alc-only.ids"; fi
    # Source texts for the shared ids
    python3 -I - "$nat_x" "$alc_x" > "$OUT/$s/source-diff.txt" <<'PY'
import sys, xml.etree.ElementTree as ET
def load(p):
    d = {}
    for e in ET.parse(p).iter():
        if e.tag.endswith("trans-unit"):
            src = [c for c in e if c.tag.endswith("source")]
            d[e.get("id")] = (src[0].text if src else None)
    return d
a, b = load(sys.argv[1]), load(sys.argv[2])
for k in sorted(set(a) & set(b)):
    if a[k] != b[k]:
        print(f"{k}\tnative={a[k]!r}\talc={b[k]!r}")
PY
    local sd; sd=$(wc -l < "$OUT/$s/source-diff.txt")
    [[ "$sd" == 0 ]] && record "$s" xlf-sources PASS "source text equal for all shared ids" || record "$s" xlf-sources FAIL "$sd shared ids with different source text, see $s/source-diff.txt"
  fi

  # Permission set: generate, compile under alc.
  if journey_alx "$s" permissions 60 permissions --format al --name "AZJ Journey" --id 50145 --json; then
    journey_body "$s" permissions | jq -r .content > "$PROJ/src/JourneyPermissions.PermissionSet.al"
    if grep -q '^permissionset 50145' "$PROJ/src/JourneyPermissions.PermissionSet.al"; then
      record "$s" permissions PASS "$(journey_body "$s" permissions | jq -r .objectCount) objects"
    else record "$s" permissions FAIL "no permissionset 50145 in output"; fi
    local prc=0; journey_alc_build "$OUT/$s/alc-perm.app" alc-perm 07 || prc=$?
    if [[ "$prc" == 0 ]]; then record "$s" permissions-compile PASS "alc builds the generated permission set"; else record "$s" permissions-compile FAIL "alc exit $prc: $(grep -m1 -E 'error' "$(step_log $s alc-perm)" | cut -c1-200)"; fi
  else record "$s" permissions FAIL "permissions exit $(cat "$OUT/$s/permissions.rc")"; fi
  # stdout alone must be the XML document, so stderr goes to its own file.
  if env -C "$PROJ" "$ALX" permissions --format xml --name AZJ > "$OUT/$s/permissions.xml" 2> "$OUT/$s/permissions-xml.stderr"; then
    if xmllint --noout "$OUT/$s/permissions.xml" 2> /dev/null; then record "$s" permissions-xml PASS "stdout is well formed XML"; else record "$s" permissions-xml FAIL "stdout is not well formed XML: $(xmllint --noout "$OUT/$s/permissions.xml" 2>&1 | head -n 1)"; fi
  else record "$s" permissions-xml FAIL "permissions --format xml exit $?"; fi
  if [[ -n "$gfile" ]]; then
    [[ "$(basename "$gfile")" == "$(basename "${af:-none}")" ]] || record "$s" xlf-filename NIGGLE "native writes $(basename "$gfile"), alc writes '$(basename "${af:-none}")'"
  fi
  sync
}
