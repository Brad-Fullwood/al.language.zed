#!/usr/bin/env python3
"""Builds checklist-runs.tsv from journey/CHECKLIST.md for stage 10.

CHECKLIST.md lives in the audit folder (default below). One output row per unique
item of kind cli, mcp-tool, lsp-command and daemon-method. Each row's invocation is the
documented text with placeholders replaced by shell variables that stage10-checklist.sh
sets ($PROJ, $F_CU, $F_TBL, $TBL, $CU, $LN, $COL, ...). Settings, editor actions and
LSP features are out of scope and only counted.

Usage: gen-checklist-runs.py [CHECKLIST.md] > checklist-runs.tsv
Counts and unmatched names go to stderr.

Columns: id, kind, name, doc_refs, invocation, expect, connects.
Invocation by kind:
  cli            arguments to al-explorer, run with cwd $PROJ
  mcp-tool       TOOL JSON-arguments
  daemon-method  METHOD JSON-params
  lsp-command    COMMAND JSON-arguments-array
  NONE           the doc gives no usable invocation (stage 10 records SKIP)
Expect: semicolon separated tokens, see stage10-checklist.sh.
"""
import re
import sys
from collections import OrderedDict

DEFAULT = "/home/bradf/Projects/Personal/al.language.zed-audit/journey/CHECKLIST.md"
path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT

CDX = "1b7a9471-d313-4aa4-99e8-7a6c762c8e7d"
F = "$F_CU"          # a codeunit file in $PROJ
POS = "$F_CU $LN $COL"  # 1-based line and column of a procedure name in that file

# Stage that already covers a CLI path. Edit here when journey.sh renumbers.
COVER = {
    "new": 1, "download-symbols": 2, "compile": 5, "package": 5, "pack-native": 5,
    "test-run": 6, "test-run-all": 6, "test-mutate": 6, "test-coverage": 6,
    "test-snapshot capture": 9, "test-snapshot replay": 9,
    "permissions": 7, "xlf generate": 7, "xlf refresh": 7, "xlf untranslated": 7, "xlf suggest": 7,
    "publish": 8, "debug start": 9, "debug breakpoint": 9, "debug state": 9, "debug eval": 9,
    "debug continue": 9, "debug step": 9, "debug history": 9, "debug stop": 9,
    "snapshot start": 9, "snapshot status": 9, "snapshot finish": 9, "snapshot list": 9,
    "snapshot show": 9, "profile start": 9, "profile stop": 9, "profile analyze": 9,
}

# cli: name -> (invocation, expect). Output of 'metrics' etc. may exit 1 with findings (doc line 24).
FIND = "rc=0,1;noerr;out"
CLI = {
    "version": ("version", "out"),
    "setup": ("setup", "rc=0,1;out"),
    "doctor": ("doctor", "rc=0,1,75;out"),
    "diag": ("diag", "out"),
    "clear-cache": ("clear-cache", "out"),
    "daemon-shutdown": ("daemon-shutdown --json", "json"),
    "init-debug": ("init-debug", "file=.zed/debug.json"),
    "trust": ("trust", "rc=0,1;out"),
    "trust --show": ("trust --show", "rc=0,1;out"),
    "trust --revoke": ("trust --revoke", "last;rc=0,1"),
    "trust (CI)": ("", "NONE:needs the digest of the values, which only a person reading trust --show can supply"),
    "pack-native --validate": ("", "covered by stage 5"),
    "search": ('search "$TBL"', 'hasre=.'),
    "object": ('object table "$TBL"', 'has=$TBL'),
    "by-id": ("by-id table $TBLID", "out"),
    "source": ('source "$CU"', "out"),
    "location": ('location "$CU"', "out"),
    "composed": ('composed table "$TBL"', "out"),
    "packages": ("packages", "out"),
    "deps": ("deps", "out"),
    "deps-graph": ("deps-graph --format json", "json"),
    "events": ("events OnAfterPostSalesDoc", "rc=0,1"),
    "subscribers": ("subscribers OnAfterPostSalesDoc", "rc=0,1"),
    "event-source": ("event-source --file $F_CU --line $LN", "rc=0,1"),
    "builtins": ("builtins", "hasre=[0-9]+ builtin types"),
    "rules": ("rules", "out"),
    "error-codes": ("error-codes", "has=AL0;hasre=[0-9]+ error codes"),
    "generate-completions": ("generate-completions bash", "has=al-explorer"),
    "hover": (f"hover {POS}", "out"),
    "definition": (f"definition {POS}", "out"),
    "references": (f"references {POS}", "out"),
    "signature": (f"signature {POS}", "rc=0,1"),
    "completions": (f"completions {POS}", "out"),
    "symbols": (f"symbols {F}", "has=$CU"),
    "folding": (f"folding {F}", "out"),
    "tokens": (f"tokens {F}", "out"),
    "parse": (f"parse {F}", "out"),
    "rename": (f"rename {POS} Journey10Renamed --dry-run", "out"),
    "hints": (f"hints {F} --start-line 1 --end-line 40", "rc=0,1"),
    "authenticate": ("authenticate", "NONE:the doc default (login) needs a browser sign-in, status and clear rows cover the subcommands"),
    "authenticate login": ("authenticate login --tenant $CDX_TENANT", "NONE:login opens a browser sign-in, the journey uses BC_ACCESS_TOKEN"),
    "authenticate status": ("authenticate status --tenant $CDX_TENANT", "rc=0,1;out"),
    "authenticate clear": ("authenticate clear --tenant $CDX_TENANT", "rc=0,1;out"),
    "format": (f"format {F} --check", "rc=0,1;noerr"),
    "lint": (f"lint {F}", "rc=0,1;noerr"),
    "fix": (f"fix {F} --dry-run", "rc=0,1;noerr"),
    "sort-members": (f"sort-members {F} --dry-run", "rc=0,1;noerr"),
    "organize-files": ("organize-files --dry-run", "rc=0,1;noerr"),
    "add-application-area": ("add-application-area --value All --dry-run", "rc=0,1;noerr"),
    "add-tooltips": ('add-tooltips --from-table "$TBL" --dry-run', "rc=0,1;noerr"),
    "add-data-classification": ("add-data-classification --value CustomerContent --dry-run", "rc=0,1;noerr"),
    "generate": ("generate test --name Journey10Gen --subject \"$CU\"", "has=Subtype = Test"),
    "trace": ("trace OnAfterPostSalesDoc --depth 10 --tree", "rc=0,1"),
    "intercept": ("intercept", "rc=0,1"),
    "entrypoints": ("entrypoints", "rc=0,1;out"),
    "graph": ("graph --format json", "json"),
    "insight-stats": ("insight-stats", "out"),
    "impact": ('impact "$TBL" --table', "rc=0,1;out"),
    "suggest-event": ('suggest-event --table "$TBL"', "rc=0,1;out"),
    "metrics": ("metrics --all", FIND),
    "dead-code": ("dead-code", FIND),
    "sql-scan": ("sql-scan", FIND),
    "duplicates": ("duplicates --min-tokens 20 --min-similarity 0.8", FIND),
    "arch-lint": ("arch-lint", FIND),
    "native-check": ("native-check", "rc=0,1;out"),
    "free-ids": ("free-ids --kind table --count 3", "out"),
    "breaking": ("breaking --baseline-app \"$APP\"", FIND),
    "upgrade": ("upgrade --baseline-app \"$APP\"", FIND),
    "obsolete": ("obsolete --used", FIND),
    "package-diff": ("package-diff $DEPAPP $DEPAPP --all", "rc=0,1;out"),
    "audit-data": ("audit-data", FIND),
    "permission-audit": ("permission-audit", FIND),
    "profiler-hints": ("profiler-hints", "rc=0,1"),
    "debug": ("debug", "NONE:bare 'debug' is a group, the subcommand rows run it"),
    "snapshot": ("snapshot", "NONE:bare 'snapshot' is a group, the subcommand rows run it"),
    "profile": ("profile", "NONE:bare 'profile' is a group, the subcommand rows run it"),
    "xlf": ("xlf", "NONE:bare 'xlf' is a group, the subcommand rows run it"),
    "test-snapshot": ("test-snapshot", "NONE:bare 'test-snapshot' is a group, the subcommand rows run it"),
    "tests": ("tests", "out"),
    "test-classify": ("test-classify", "rc=0,1;out"),
    "test-affected": (f"test-affected {F}", "rc=0,1"),
    "test-results": ("test-results", "rc=0,1"),
    "test-snapshot validate": ("test-snapshot validate $SNAP", "rc=0,1;out"),
    "test-snapshot diff": ("test-snapshot diff $SNAP $SNAP", "rc=0,1;out"),
    "snapshot show": ("snapshot show $SNAPZIP", "rc=0,1"),
    "profile start": ("profile start", "covered"),
    "al-lsp mcp": ("", "NONE:started by the mcp-tool rows through journey-rpc"),
    "parse ../elsewhere/Foo.al": ("parse $OUTSIDE", "out"),
    "--json": ('search "$TBL" --json', "json"),
    "--compact": ('search "$TBL" --compact', "json"),
    "--limit": ("search a --limit 2", "out"),
    "--offset": ("search a --limit 2 --offset 1", "out"),
    "--fields": ('search "$TBL" --fields kind,name', "json"),
    "--scope": ("entrypoints --scope workspace", "rc=0,1;out"),
    "--timeout-ms": ("version --timeout-ms 20000", "out"),
    "--idle-timeout-secs": ("", "NONE:flag of 'al-lsp daemon', the daemon rows start the daemon with the default"),
    "generate-completions": ("generate-completions bash", "has=al-explorer"),
    "al-lsp --version": ("@lsp --version", "has=al-lsp"),
    "al-lsp daemon": ("", "NONE:long running server in the foreground, the daemon rows start one"),
    "al-lsp stdio server": ("", "NONE:long running stdio server, the lsp-command rows drive it through journey-rpc"),
    "al-lsp Zed debug adapter": ("", "covered by stage 8"),
    "al-lsp legacy debug proxy": ("", "NONE:stdio proxy for the old adapter, stage 8 drives the current adapter"),
    "al-lsp mcp": ("", "NONE:started by the mcp-tool rows through journey-rpc"),
    "build": ("build", "rc=0"),
    "trust revoke": ("trust --revoke", "last"),
    "trust ci": ("", "NONE:needs the digest of the values, which only a person reading trust --show can supply"),
    "al-explorer query commands": ("", "NONE:group of rows already run one by one (hover, definition, references, signature, completions, symbols, folding, tokens, rename, hints)"),
    "make language": ("", "NONE:contributor make target that regenerates files in a checkout, not a developer journey command"),
    "make grammar": ("", "NONE:contributor make target that regenerates files in a checkout, not a developer journey command"),
    "make record-methods": ("", "NONE:contributor make target that regenerates files in a checkout, not a developer journey command"),
    "make microsoft-contracts": ("", "NONE:contributor make target that needs the Microsoft toolchain and a checkout"),
    "make live-bc-contracts": ("", "NONE:contributor make target that runs the live contract tests, needs a checkout"),
    "release hygiene check": ("", "NONE:contributor script in a checkout (scripts/check-release-hygiene.sh)"),
    "al-explorer": ("", "NONE:no subcommand opens the TUI, needs a terminal (tui_smoke covers it)"),
}
# cli names whose doc invocation has no row of its own in cli-commands.md but a plain form
ALIAS = {"al-explorer trust": "trust", "al-explorer format": "format"}

# mcp-tool: name -> (arguments JSON, expect)
MCP = {
    "al_call": ('{"method":"ping","params":{}}', "has=pong"),
    "al_debug": ('{"cmd":"state"}', "loose;nosession"),
    "al_debug start": ("", "covered"),
    "al_debug breakpoint": ('{"cmd":"breakpoint","file":"$F_CU","line":$LN}', "loose;nosession"),
    "al_debug state": ('{"cmd":"state"}', "loose;nosession"),
    "al_debug stack": ('{"cmd":"stack"}', "loose;nosession"),
    "al_debug variables": ('{"cmd":"variables"}', "loose;nosession"),
    "al_debug globals": ('{"cmd":"globals"}', "loose;nosession"),
    "al_debug expand": ('{"cmd":"expand","path":"Rec"}', "loose;nosession"),
    "al_debug eval": ('{"cmd":"eval","expr":"1+1"}', "loose;nosession"),
    "al_debug continue": ('{"cmd":"continue"}', "loose;nosession"),
    "al_debug step": ('{"cmd":"step","stepType":"over"}', "loose;nosession"),
    "al_debug history": ('{"cmd":"history"}', "loose;nosession"),
    "al_debug stop": ('{"cmd":"stop"}', "loose;nosession"),
    "al_snapshot": ('{"action":"list"}', "strict"),
    "al_build": ("{}", "has=appPath"),
    "al_downloadsymbols": ('{"source":"nuget"}', "loose"),
    "al_symbolsearch": ('{"query":"$TBL","limit":5}', "has=$TBL"),
    "al_getdiagnostics": ('{"file":"$F_CU"}', "strict"),
    "al_runtests": ("{}", "covered"),
    "al_deadcode": ("{}", "strict"),
    "al_sqlscan": ("{}", "strict"),
    "al_entrypoints": ('{"scope":"workspace"}', "strict"),
    "al_trace_event": ('{"event":"OnAfterPostSalesDoc","depth":10}', "strict"),
    "al_impact": ('{"symbol":"$TBL","scope":"workspace"}', "strict"),
    "al_suggestevent": ('{"query":{"source":{"type":"table","table":"$TBL"}}}', "loose"),
    "al_testclassify": ("{}", "strict"),
    "al_testcoverage": ("{}", "strict"),
    "al_testsnapshot": ("{}", "covered"),
    "al_testsnapshotreplay": ("{}", "covered"),
    "al_depgraph": ('{"format":"json"}', "strict"),
    "al_freeids": ('{"kind":"table","count":3}', "strict"),
}
MCP_COVER = {"al_debug start": 9, "al_runtests": 6, "al_testsnapshot": 9, "al_testsnapshotreplay": 9}

# lsp-command: name -> (arguments JSON array, expect)
URI = '"file://$F_CU"'
SUBURI = '"file://$F_SUB"'
PUBURI = '"file://$F_PUB"'  # a file with an [IntegrationEvent] or [BusinessEvent] publisher
POS = '{"line":$LN0,"character":$CH0}'
SUBPOS = '{"line":$SUBLN0,"character":0}'
LSP = {
    "al.downloadSymbols": ("[]", "loose"),
    "al.downloadSymbolsServer": ("[]", "covered"),
    "al.downloadSymbolsNuget": ("[]", "loose"),
    "al.clearSymbolCache": ("[]", "strict"),
    "al.formatFile": (f"[{URI}]", "loose"),
    "al.lintFile": (f"[{URI}]", "loose"),
    "al.getStatus": ("[]", "json"),
    "al.reindex": ("[]", "strict"),
    "al.compile": ("[]", "loose"),
    "al.applyRecommendedSettings": ("[]", "loose"),
    "al.findReferences": (f'[{{"uri":{URI},"position":{POS}}}]', "loose"),
    "al.showProfiler": (f"[{URI}]", "loose"),
    "al.runTest": (f"[{URI}]", "loose"),
    "al.showEventSource": (f'[{{"uri":{SUBURI},"position":{SUBPOS}}}]', "loose"),
    "al.showReport": (f'[{{"report":"metrics","object":"$CU","uri":{URI}}}]', "loose"),
    "al.showSubscribers": (f'[{{"uri":{PUBURI},"position":{{"line":$PUBLN0,"character":10}}}}]', "loose"),
}
LSP_COVER = {"al.downloadSymbolsServer": 2}

# daemon-method: name -> (params JSON, expect)
POSQ = '{"uri":"file://$F_CU","line":$LN0,"character":$CH0}'
D = {
    "ping": ("{}", "has=pong"),
    "status": ("{}", "strict;has=callGraph"),
    "shutdown": ("{}", "strict;last"),
    "handshake": ('{"nonce":"journey10"}', "strict;has=proof"),
    "diag/summary": ('{"cmd":"summary"}', "strict;has=process"),  # sent as method diag, see D_METHOD
    "diag": ("{}", "strict;has=sourceIndex"),
    "hover": (POSQ, "strict"),
    "definition": (POSQ, "strict"),
    "references": (POSQ, "strict"),
    "implementations": (POSQ, "strict"),
    "completions": ('{"uri":"file://$F_CU","line":$LN0,"character":$CH0,"limit":5}', "strict"),
    "signatureHelp": (POSQ, "strict"),
    "rename": ('{"uri":"file://$F_CU","line":$LN0,"character":$CH0,"newName":"Journey10Renamed"}', "strict"),
    "documentSymbols": ('{"uri":"file://$F_CU"}', "strict;has=$CU"),
    "foldingRanges": ('{"uri":"file://$F_CU"}', "strict"),
    "semanticTokens": ('{"uri":"file://$F_CU"}', "strict"),
    "inlayHints": ('{"uri":"file://$F_CU"}', "strict"),
    "codeActions": ('{"uri":"file://$F_CU","line":$LN0,"character":$CH0}', "loose"),
    "search": ('{"query":"$TBL","limit":5}', "strict;has=$TBL"),
    "object": ('{"kind":"table","name":"$TBL"}', "strict;has=$TBL"),
    "byId": ('{"kind":"table","id":$TBLID}', "strict"),
    "events": ('{"name":"OnAfterPostSalesDoc"}', "loose"),
    "subscribers": ('{"event":"OnAfterPostSalesDoc"}', "strict"),
    "composed": ('{"kind":"table","name":"$TBL"}', "strict"),
    "packages": ("{}", "strict"),
    "deps": ("{}", "strict"),
    "lint": ('{"uri":"file://$F_CU"}', "strict"),
    "format": ('{"uri":"file://$F_CU","check":true}', "loose"),
    "fix": ('{"uri":"file://$F_CU","dryRun":true}', "loose"),
    "fix.applicationArea": ('{"value":"All","dryRun":true}', "strict"),
    "fix.tooltips": ('{"fromTable":"$TBL","dryRun":true}', "strict"),
    "fix.dataClassification": ('{"value":"CustomerContent","dryRun":true}', "strict"),
    "rules": ("{}", "strict"),
    "parse": ('{"uri":"file://$F_CU"}', "strict"),
    "metrics": ('{"uri":"file://$F_CU"}', "strict"),
    "sqlPatterns": ("{}", "strict"),
    "sortMembers": ('{"uri":"file://$F_CU","dryRun":true}', "strict"),
    "organizeFiles": ('{"dryRun":true}', "strict"),
    "source": ('{"name":"$CU","listProcedures":true}', "strict"),
    "eventSource": ('{"file":"$F_SUB","line":$SUBLN}', "loose"),
    "location": ('{"name":"$CU"}', "strict"),
    "permissions": ('{"format":"al","name":"Journey10Perm","id":50190,"roleId":"JOURNEY10"}', "loose"),
    "compile": ("{}", "covered"),
    "package": ("{}", "covered"),
    "publish": ("{}", "covered"),
    "newProject": ("{}", "covered"),
    "errorCodes": ("{}", "strict"),
    "builtinTypes": ("{}", "strict"),
    "setup": ("{}", "loose"),
    "clearCache": ("{}", "strict"),
    "authenticate": ('{"cmd":"status"}', "loose"),
    "downloadSymbols": ("{}", "covered"),
    "snapshot": ('{"cmd":"list"}', "strict"),
    "profiling": ("{}", "covered"),
    "generate": ('{"kind":"test","name":"Journey10GenD","subject":"$CU"}', "loose"),
    "obsolete": ("{}", "strict"),
    "obsoleteUsages": ("{}", "strict"),
    "packageDiff": ('{"from":"$DEPAPP","to":"$DEPAPP","all":true}', "loose"),
    "audit.dataClassification": ("{}", "strict"),
    "permissions.audit": ("{}", "strict"),
    "deps.graph": ("{}", "strict"),
    "breaking": ('{"baselineSymbols":[{"kind":"Codeunit","id":50100,"name":"JourneyBaseline"}]}', "loose"),
    "arch.lint": ("{}", "strict"),
    "duplicates": ("{}", "strict"),
    "upgrade": ('{"baselineSymbols":[{"kind":"Codeunit","id":50100,"name":"JourneyBaseline"}]}', "loose"),
    "profiler.hints": ('{"hotspots":[]}', "loose"),
    "freeIds": ('{"kind":"table","count":3}', "strict"),
    "xlf.generate": ("{}", "covered"),
    "xlf.refresh": ("{}", "covered"),
    "xlf.untranslated": ("{}", "covered"),
    "xlf.suggest": ("{}", "covered"),
    "tests.discover": ("{}", "strict"),
    "tests.run": ("{}", "covered"),
    "tests.coverage": ("{}", "strict"),
    "tests.run_batch": ("{}", "covered"),
    "tests.run_auto": ("{}", "covered"),
    "tests.last_results": ("{}", "strict"),
    "tests.affected": ('{"changedFiles":["$F_CU"]}', "loose"),
    "tests.classify": ("{}", "strict"),
    "tests.snapshot_validate": ('{"snapshotPath":"$SNAP"}', "loose"),
    "tests.snapshot_capture": ("{}", "covered"),
    "tests.snapshot_replay": ("{}", "covered"),
    "tests.snapshot_diff": ('{"pathA":"$SNAP","pathB":"$SNAP"}', "loose"),
    "tests.mutate": ("{}", "covered"),
    "trace": ('{"event":"OnAfterPostSalesDoc","depth":10}', "strict"),
    "traceChain": ('{"event":"OnAfterPostSalesDoc"}', "loose"),
    "entrypoints": ('{"scope":"workspace"}', "strict"),
    "graphExport": ('{"format":"json","scope":"workspace"}', "strict"),
    "insightStats": ("{}", "strict"),
    "deadCode": ("{}", "strict"),
    "nativeCheck": ("{}", "strict"),
    "impact": ('{"symbol":"$TBL","scope":"workspace"}', "strict"),
    "tableImpact": ('{"table":"$TBL","scope":"workspace"}', "loose"),
    "suggestEvent": ('{"query":{"source":{"type":"table","table":"$TBL"}}}', "loose"),
    "eventMap": ('{"scope":"workspace"}', "strict"),
    "debug": ('{"cmd":"state"}', "loose;nosession"),
}
# Older doc spellings of a method name that the daemon answers under another name.
D_METHOD = {"diag/summary": "diag"}
D_COVER = {"compile": 5, "package": 5, "publish": 8, "newProject": 1, "downloadSymbols": 2,
           "profiling": 9, "xlf.generate": 7, "xlf.refresh": 7, "xlf.untranslated": 7,
           "xlf.suggest": 7, "tests.run": 6, "tests.run_batch": 6, "tests.run_auto": 6,
           "tests.snapshot_capture": 9, "tests.snapshot_replay": 9, "tests.mutate": 6}

CONNECTS_CLI = {"publish", "download-symbols", "authenticate", "authenticate login",
                "authenticate status", "authenticate clear", "snapshot", "snapshot start",
                "snapshot status", "snapshot finish", "profile", "profile start", "profile stop",
                "test-run", "test-run-all", "test-snapshot", "test-snapshot replay"}


def norm(name: str) -> str:
    n = name.strip().replace("`", "")
    suffix = re.search(r"\(CI\)", n)
    n = re.sub(r"\s*\(.*$", "", n).strip()
    if suffix:
        n += " (CI)"
    if n.startswith("al-explorer ") and n != "al-explorer query commands":
        n = n[len("al-explorer "):]
    return ALIAS.get(n, n)


def refs(cell: str):
    return [r.strip() for r in re.split(r"[;]", cell) if r.strip()]


rows = OrderedDict()   # (kind, name) -> dict(refs=[], connects=bool)
counts = {"setting": 0, "editor-action": 0, "lsp-feature": 0, "tui": 0}
for line in open(path, encoding="utf-8"):
    if not line.startswith("| "):
        continue
    cells = [c.strip() for c in line.strip().strip("|").split(" | ")]
    if len(cells) < 5 or cells[0] in ("Kind", "---"):
        continue
    kind, name, _inv, ref, conn = cells[0], cells[1], cells[2], cells[3], cells[4]
    if kind in counts:
        counts[kind] += 1
        continue
    if kind not in ("cli", "mcp-tool", "lsp-command", "daemon-method"):
        continue
    nm = norm(name)
    if kind == "cli" and (nm.startswith("TUI view") or nm.startswith("TUI Test")):
        counts["tui"] += 1
        continue
    if kind == "lsp-command" and nm.startswith("AL: "):
        counts["editor-action"] += 1
        continue
    if kind == "daemon-method" and nm == "fix.*":
        continue
    key = (kind, nm)
    entry = rows.setdefault(key, {"refs": [], "conn": False})
    for r in refs(ref):
        short = re.sub(r"^Docs/(reference|features)/", "", r)
        if short not in entry["refs"]:
            entry["refs"].append(short)
    if conn.strip().lower().startswith("y"):
        entry["conn"] = True

out = []
missing = []
for (kind, nm), e in rows.items():
    connects = "y" if (e["conn"] or (kind == "cli" and nm in CONNECTS_CLI)) else "n"
    inv, exp = "NONE", ""
    if kind == "cli":
        if nm in CLI:
            inv, exp = CLI[nm]
            if exp.startswith("NONE:"):
                exp, inv = "skip=" + exp[5:], "NONE"
            elif inv == "":
                exp, inv = "skip=no invocation in the doc", "NONE"
            elif exp == "covered":
                exp = "covered by stage 8"
            if nm in COVER:
                exp = f"covered by stage {COVER[nm]}"
            if inv != "NONE" and nm in COVER:
                pass
        elif nm in COVER or nm.startswith("TUI "):
            exp = f"covered by stage {COVER.get(nm, 0)}" if nm in COVER else "skip=interactive TUI key, tui_smoke covers it"
        else:
            missing.append((kind, nm))
            exp = "skip=doc gives no invocation"
    elif kind == "mcp-tool":
        key = nm
        if key in MCP:
            args, exp = MCP[key]
            inv = (nm.split(" ")[0] + " " + args) if args else "NONE"
            if key in MCP_COVER:
                exp = f"covered by stage {MCP_COVER[key]}"
        else:
            missing.append((kind, nm))
            exp = "skip=doc gives no invocation"
    elif kind == "lsp-command":
        if nm in LSP:
            args, exp = LSP[nm]
            inv = f"{nm} {args}"
            if nm in LSP_COVER:
                exp = f"covered by stage {LSP_COVER[nm]}"
        else:
            missing.append((kind, nm))
            exp = "skip=doc gives no invocation"
    else:
        if nm in D:
            args, exp = D[nm]
            inv = f"{D_METHOD.get(nm, nm)} {args}"
            if nm in D_COVER:
                exp = f"covered by stage {D_COVER[nm]}"
        else:
            missing.append((kind, nm))
            exp = "skip=doc gives no invocation"
    if exp.startswith("covered by stage"):
        inv = inv if inv != "NONE" else "NONE"
    out.append((f"{kind}.{nm}", kind, nm, "; ".join(e["refs"]), inv, exp, connects))

print("\t".join(["id", "kind", "name", "doc_refs", "invocation", "expect", "connects"]))
for r in out:
    print("\t".join(c.replace("\t", " ") for c in r))

by_kind = {}
for r in out:
    by_kind[r[1]] = by_kind.get(r[1], 0) + 1
print("rows by kind:", by_kind, "total", len(out), file=sys.stderr)
print("out of scope counts:", counts, file=sys.stderr)
print("no spec for:", missing, file=sys.stderr)
