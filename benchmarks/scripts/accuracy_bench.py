#!/usr/bin/env python3
"""Accuracy comparison: which planted defects does each toolchain actually report?

Every case is its own project (see gen_accuracy_corpus.py), so a parse error in
one case cannot suppress semantic analysis in another.

Scoring is deliberately strict. A tool "catches" a case only when it emits an
error-severity diagnostic in the defect file within +/-LINE_TOLERANCE lines of
the planted defect. Merely flagging the file somewhere is not enough — an
earlier revision of this script credited a duplicate-ID finding as a catch for
an undeclared-variable case, which flattered the wrong tool. Diagnostics in the
right file but the wrong place are counted separately as `off_target`.

The control case inverts the test: any error there is a false positive.

Sides:
  alc          — Microsoft's compiler; the engine behind VS Code / Cursor.
  al-lsp       — this repo's server, via textDocument/publishDiagnostics.
  native-check — this repo's .NET-free workspace checks.
"""
import json
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BENCH = os.path.dirname(HERE)
REPO = os.path.dirname(BENCH)
sys.path.insert(0, HERE)

ALC = os.environ.get("ALC_PATH") or os.path.join(
    os.environ.get("AL_MS_EXT", ""), "bin", "linux", "alc.dll")
EXPLORER = os.path.join(REPO, "target", "release", "al-explorer")
AL_LSP = os.path.join(REPO, "target", "release", "al-lsp")
ROOT = os.path.join(BENCH, "projects", "accuracy")
SHARED_PKGS = os.path.join(ROOT, "_packages")

LINE_TOLERANCE = 2

ALC_DIAG = re.compile(r"^(?P<file>.+?)\((?P<line>\d+),(?P<col>\d+)\):\s+"
                      r"(?P<sev>error|warning|info)\s+(?P<code>[A-Za-z0-9]+):\s+(?P<msg>.*)$")


def run_alc(proj):
    out_app = os.path.join(proj, "out.app")
    if os.path.exists(out_app):
        os.remove(out_app)
    t0 = time.perf_counter()
    try:
        p = subprocess.run(
            ["dotnet", ALC, f"/project:{proj}", f"/out:{out_app}",
             f"/packagecachepath:{SHARED_PKGS}"],
            capture_output=True, text=True, timeout=600)
        rc, blob = p.returncode, p.stdout + "\n" + p.stderr
    except subprocess.TimeoutExpired:
        rc, blob = -9, "TIMEOUT"
    ms = (time.perf_counter() - t0) * 1000.0
    diags = []
    for line in blob.splitlines():
        m = ALC_DIAG.match(line.strip())
        if m:
            diags.append({"file": os.path.basename(m.group("file")),
                          "line": int(m.group("line")),
                          "severity": m.group("sev"), "code": m.group("code"),
                          "message": m.group("msg")})
    return {"ms": round(ms, 1), "rc": rc, "diagnostics": diags, "raw_tail": blob[-1200:]}


def run_native_check(proj):
    t0 = time.perf_counter()
    try:
        p = subprocess.run([EXPLORER, "native-check", "--json"], cwd=proj,
                           capture_output=True, text=True, timeout=300)
    except subprocess.TimeoutExpired:
        return {"ms": 0, "rc": -9, "diagnostics": [], "raw_tail": "TIMEOUT"}
    ms = (time.perf_counter() - t0) * 1000.0
    diags = []
    try:
        data = json.loads(p.stdout)
        items = data if isinstance(data, list) else (
            data.get("findings") or data.get("diagnostics") or data.get("issues") or [])
        for d in items or []:
            if not isinstance(d, dict):
                continue
            diags.append({"file": os.path.basename(d.get("file") or d.get("path") or ""),
                          "line": int(d.get("line") or 0),
                          "severity": d.get("severity", "error"),
                          "code": d.get("code", ""), "message": d.get("message", "")})
    except Exception:
        pass
    return {"ms": round(ms, 1), "rc": p.returncode, "diagnostics": diags,
            "raw_tail": (p.stdout + p.stderr)[-1200:]}


def run_al_lsp(proj, files, timeout=45.0):
    from lsp_bench import LspClient, path_to_uri
    log = os.path.join(BENCH, "results", "accuracy_al_lsp.stderr.log")
    cli = LspClient([AL_LSP], cwd=proj, stderr_path=log)
    root_uri = path_to_uri(proj)
    init = {"processId": os.getpid(), "rootPath": proj, "rootUri": root_uri,
            "workspaceFolders": [{"uri": root_uri, "name": os.path.basename(proj)}],
            "capabilities": {"workspace": {"workspaceFolders": True, "configuration": True},
                             "textDocument": {"synchronization": {"didSave": True},
                                              "publishDiagnostics": {"relatedInformation": True}},
                             "window": {"workDoneProgress": True}},
            "initializationOptions": {}}
    r, _ = cli.request("initialize", init, timeout=60)
    if r is None:
        cli.close()
        return {"diagnostics": [], "error": "initialize failed"}
    cli.notify("initialized", {})
    want = set()
    for fn in files:
        p = os.path.join(proj, "src", fn)
        want.add(fn)
        cli.notify("textDocument/didOpen", {"textDocument": {
            "uri": path_to_uri(p), "languageId": "al", "version": 1,
            "text": open(p, encoding="utf-8").read()}})

    deadline = time.time() + timeout
    while time.time() < deadline:
        with cli._event:
            cli._event.wait(0.2)
            seen = {os.path.basename(m.get("params", {}).get("uri", ""))
                    for m, _ in cli._notifications
                    if m.get("method") == "textDocument/publishDiagnostics"}
        if want.issubset(seen):
            break

    diags = []
    with cli._event:
        for msg, _ in cli._notifications:
            if msg.get("method") != "textDocument/publishDiagnostics":
                continue
            params = msg.get("params", {})
            fn = os.path.basename(params.get("uri", ""))
            for d in params.get("diagnostics", []):
                sev = {1: "error", 2: "warning", 3: "info", 4: "hint"}.get(
                    d.get("severity", 1), "error")
                diags.append({"file": fn,
                              "line": d.get("range", {}).get("start", {}).get("line", 0) + 1,
                              "severity": sev, "code": str(d.get("code", "")),
                              "message": d.get("message", "")})
    cli.close()
    return {"diagnostics": diags}


# A defect the developer is told about counts as caught regardless of whether
# the tool calls it an error or a warning — native-check reports its findings
# as warnings (AL-NC*) where alc reports errors, and scoring only `error` would
# credit alc for work we also do. Severity is recorded so the report can say
# which side treats a given defect as build-breaking.
REPORTED = ("error", "warning")


def judge(case, diags):
    """Did this tool find THIS defect, at roughly the right place?"""
    rep = [d for d in diags if d["severity"] in REPORTED]
    in_file = [d for d in rep if d["file"] == case["defect_file"]]
    if case["class"] == "control":
        return {"caught": len(rep) == 0, "on_target": 0,
                "off_target": len(rep), "severities": sorted({d["severity"] for d in rep}),
                "codes": sorted({d["code"] for d in rep}),
                "messages": [d["message"][:120] for d in rep[:3]]}
    on = [d for d in in_file
          if case["defect_line"] is not None
          and abs(d["line"] - case["defect_line"]) <= LINE_TOLERANCE]
    return {"caught": len(on) > 0, "on_target": len(on),
            "off_target": len(in_file) - len(on),
            "severities": sorted({d["severity"] for d in on}),
            "codes": sorted({d["code"] for d in on}),
            "messages": [f"L{d['line']} {d['severity']} {d['code']}: {d['message'][:90]}"
                         for d in on[:2]]}


def main():
    manifest = json.load(open(os.path.join(ROOT, "manifest.json")))
    os.makedirs(os.path.join(BENCH, "results"), exist_ok=True)
    out = {"line_tolerance": LINE_TOLERANCE, "cases": {}}
    tools = ("alc", "al_lsp", "native_check")

    for m in manifest:
        proj = m["project"]
        print(f"\n### {m['id']}  ({m['class']})")
        res = {}
        a = run_alc(proj)
        res["alc"] = {"raw": a, "score": judge(m, a["diagnostics"])}
        print(f"  alc          rc={a['rc']:<4} diags={len(a['diagnostics']):<3} "
              f"{a['ms']:>7.0f} ms  caught={res['alc']['score']['caught']}")
        l = run_al_lsp(proj, m["files"])
        res["al_lsp"] = {"raw": l, "score": judge(m, l["diagnostics"])}
        print(f"  al-lsp       diags={len(l['diagnostics']):<3} "
              f"caught={res['al_lsp']['score']['caught']}")
        n = run_native_check(proj)
        res["native_check"] = {"raw": n, "score": judge(m, n["diagnostics"])}
        print(f"  native-check rc={n['rc']:<4} diags={len(n['diagnostics']):<3} "
              f"{n['ms']:>7.0f} ms  caught={res['native_check']['score']['caught']}")
        out["cases"][m["id"]] = {"class": m["class"], "expect": m["expect"], "tools": res}
        with open(os.path.join(BENCH, "results", "accuracy.json"), "w") as fh:
            json.dump({"manifest": manifest, **out}, fh, indent=2)

    print("\n=== per-case ===")
    print(f"{'case':<26}{'class':<11}{'alc':<7}{'al-lsp':<9}{'nchk':<7}{'ours':<7}")
    for m in manifest:
        c = out["cases"][m["id"]]["tools"]
        def mk(t):
            return "OK" if c[t]["score"]["caught"] else "--"
        ours = "OK" if (c["al_lsp"]["score"]["caught"] or
                        c["native_check"]["score"]["caught"]) else "--"
        if m["class"] == "control":
            ours = "OK" if (c["al_lsp"]["score"]["caught"] and
                            c["native_check"]["score"]["caught"]) else "--"
        print(f"{m['id']:<26}{m['class']:<11}{mk('alc'):<7}{mk('al_lsp'):<9}"
              f"{mk('native_check'):<7}{ours:<7}")

    real = [m for m in manifest if m["class"] != "control"]
    ctrl = [m for m in manifest if m["class"] == "control"]
    print(f"\n=== totals (of {len(real)} real defects) ===")
    totals = {}
    for t in tools:
        n = sum(1 for m in real if out["cases"][m["id"]]["tools"][t]["score"]["caught"])
        fp = any(not out["cases"][m["id"]]["tools"][t]["score"]["caught"] for m in ctrl)
        totals[t] = {"caught": n, "total": len(real), "false_positive": fp}
        print(f"  {t:<14} {n}/{len(real)}" + ("   FALSE POSITIVE ON CONTROL" if fp else ""))
    ours_n = sum(1 for m in real
                 if out["cases"][m["id"]]["tools"]["al_lsp"]["score"]["caught"]
                 or out["cases"][m["id"]]["tools"]["native_check"]["score"]["caught"])
    totals["ours_combined"] = {"caught": ours_n, "total": len(real)}
    print(f"  {'ours (both)':<14} {ours_n}/{len(real)}")

    by_class = {}
    for m in real:
        cl = m["class"]
        d = by_class.setdefault(cl, {t: 0 for t in tools} | {"n": 0, "ours": 0})
        d["n"] += 1
        for t in tools:
            if out["cases"][m["id"]]["tools"][t]["score"]["caught"]:
                d[t] += 1
        if (out["cases"][m["id"]]["tools"]["al_lsp"]["score"]["caught"]
                or out["cases"][m["id"]]["tools"]["native_check"]["score"]["caught"]):
            d["ours"] += 1
    print("\n=== by class ===")
    print(f"{'class':<12}{'n':<4}{'alc':<7}{'ours':<7}")
    for cl, d in by_class.items():
        print(f"{cl:<12}{d['n']:<4}{d['alc']:<7}{d['ours']:<7}")

    out["totals"] = totals
    out["by_class"] = by_class
    with open(os.path.join(BENCH, "results", "accuracy.json"), "w") as fh:
        json.dump({"manifest": manifest, **out}, fh, indent=2)
    print("\nwrote results/accuracy.json")


if __name__ == "__main__":
    main()
