#!/usr/bin/env python3
"""Accuracy comparison: which planted defects does each toolchain actually report?

Every case is its own project (see gen_accuracy_corpus.py), so a parse error in
one case cannot suppress semantic analysis in another.

Scoring is deliberately strict. A tool "catches" a case only when it emits an
error- or warning-severity diagnostic in the defect file within
+/-LINE_TOLERANCE lines of the planted defect. Merely flagging the file
somewhere is not enough — an earlier revision of this script credited a
duplicate-ID finding as a catch for an undeclared-variable case, which
flattered the wrong tool. Diagnostics in the right file but the wrong place
are counted separately as `off_target`; severity remains visible in the raw
result.

The control case inverts the test: any error there is a false positive.

Sides:
  alc          — Microsoft's compiler; the engine behind VS Code / Cursor.
  al-lsp       — this repo's server, via textDocument/publishDiagnostics.
  native-build — this repo's production `pack-native` verification gate.
  native-check — this repo's .NET-free workspace checks.
"""
import datetime as dt
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import time
from pathlib import Path

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
RESULT_PATH = Path(os.environ.get(
    "AL_BENCH_ACCURACY_RESULT",
    os.path.join(BENCH, "results", "accuracy.json"),
))

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
        rc, blob, timed_out = p.returncode, p.stdout + "\n" + p.stderr, False
    except subprocess.TimeoutExpired:
        rc, blob, timed_out = -9, "TIMEOUT", True
    ms = (time.perf_counter() - t0) * 1000.0
    diags = []
    for line in blob.splitlines():
        m = ALC_DIAG.match(line.strip())
        if m:
            diags.append({"file": os.path.basename(m.group("file")),
                          "line": int(m.group("line")),
                          "severity": m.group("sev"), "code": m.group("code"),
                          "message": m.group("msg")})
    return {
        "ms": round(ms, 1),
        "rc": rc,
        "diagnostics": diags,
        "raw_tail": blob[-1200:],
        "timed_out": timed_out,
    }


def run_native_check(proj):
    t0 = time.perf_counter()
    try:
        p = subprocess.run([EXPLORER, "native-check", "--json"], cwd=proj,
                           capture_output=True, text=True, timeout=300)
    except subprocess.TimeoutExpired:
        return {
            "ms": 0,
            "rc": -9,
            "diagnostics": [],
            "raw_tail": "TIMEOUT",
            "timed_out": True,
            "parse_error": None,
        }
    ms = (time.perf_counter() - t0) * 1000.0
    diags = []
    parse_error = None
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
    except (json.JSONDecodeError, TypeError, ValueError) as error:
        parse_error = str(error)
    return {"ms": round(ms, 1), "rc": p.returncode, "diagnostics": diags,
            "raw_tail": (p.stdout + p.stderr)[-1200:], "timed_out": False,
            "parse_error": parse_error}


def run_native_build(proj):
    """Run the exact native verifier used by pack-native/normal native builds."""
    out_app = os.path.join(proj, "native-bench.app")
    if os.path.exists(out_app):
        os.remove(out_app)
    t0 = time.perf_counter()
    try:
        p = subprocess.run(
            [EXPLORER, "pack-native", "--project", proj, "--out", out_app, "--json"],
            capture_output=True, text=True, timeout=300)
    except subprocess.TimeoutExpired:
        return {
            "ms": 0,
            "rc": -9,
            "diagnostics": [],
            "raw_tail": "TIMEOUT",
            "timed_out": True,
            "parse_error": None,
        }
    ms = (time.perf_counter() - t0) * 1000.0
    diags = []
    parse_error = None
    try:
        data = json.loads(p.stdout)
        for d in data.get("diagnostics", []) if isinstance(data, dict) else []:
            if not isinstance(d, dict):
                continue
            diags.append({"file": os.path.basename(d.get("file") or ""),
                          "line": int(d.get("line") or 0),
                          "severity": d.get("severity", "error"),
                          "code": d.get("code", ""), "message": d.get("message", "")})
    except (json.JSONDecodeError, TypeError, ValueError) as error:
        parse_error = str(error)
    return {"ms": round(ms, 1), "rc": p.returncode, "diagnostics": diags,
            "raw_tail": (p.stdout + p.stderr)[-1200:], "timed_out": False,
            "parse_error": parse_error}


def run_al_lsp(proj, files, timeout=45.0):
    from lsp_bench import LspClient, path_to_uri
    log = str(RESULT_PATH.parent / "accuracy_al_lsp.stderr.log")
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
    if r is None or "error" in r:
        cli.close()
        return {"diagnostics": [], "error": "initialize failed", "missing_files": files}
    cli.notify("initialized", {})
    want = set()
    for fn in files:
        p = os.path.join(proj, "src", fn)
        want.add(fn)
        cli.notify("textDocument/didOpen", {"textDocument": {
            "uri": path_to_uri(p), "languageId": "al", "version": 1,
            "text": open(p, encoding="utf-8").read()}})

    deadline = time.time() + timeout
    seen = set()
    while time.time() < deadline:
        with cli._event:
            cli._event.wait(0.2)
            seen = {
                os.path.basename(m.get("params", {}).get("uri", ""))
                for m, _ in cli._notifications
                if m.get("method") == "textDocument/publishDiagnostics"
            }
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
    missing = sorted(want - seen)
    return {
        "diagnostics": diags,
        "error": (
            f"publishDiagnostics missing for: {', '.join(missing)}"
            if missing
            else None
        ),
        "missing_files": missing,
    }


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


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def command_output(args, default="unknown"):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=30)
        text = (result.stdout + result.stderr).strip()
        return text or default
    except (OSError, subprocess.TimeoutExpired):
        return default


def repository_metadata():
    commit = command_output(["git", "-C", REPO, "rev-parse", "HEAD"]).splitlines()[0]
    status = command_output(["git", "-C", REPO, "status", "--porcelain"], "")
    diff = subprocess.run(
        ["git", "-C", REPO, "diff", "--binary", "HEAD"],
        capture_output=True,
        check=True,
    ).stdout
    dirty = bool(status)
    if dirty and os.environ.get("AL_BENCH_ALLOW_DIRTY") != "1":
        raise RuntimeError(
            "publishable benchmarks require a clean repository; "
            "set AL_BENCH_ALLOW_DIRTY=1 only for an explicitly provisional run"
        )
    return {
        "commit": commit,
        "dirty": dirty,
        "trackedDiffSha256": hashlib.sha256(diff).hexdigest() if dirty else None,
    }


def machine_metadata():
    cpu = "unknown"
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    return {
        "os": platform.platform(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "cpu": cpu,
        "logicalCpus": os.cpu_count(),
    }


def compiler_metadata():
    if not os.path.isfile(ALC):
        raise RuntimeError(f"alc.dll not found at {ALC}")
    version_output = command_output(["dotnet", ALC, "/?"])
    version = next(
        (line.strip() for line in version_output.splitlines() if "AL Compiler version" in line),
        "unknown",
    )
    binaries = {}
    for name, path in (("alc", ALC), ("alExplorer", EXPLORER), ("alLsp", AL_LSP)):
        if not os.path.isfile(path):
            raise RuntimeError(f"required release binary is missing: {path}")
        binaries[name] = {"sha256": sha256_file(path)}
    return {"version": version, "binaries": binaries}


def package_metadata():
    return [
        {
            "name": path.name,
            "bytes": path.stat().st_size,
            "sha256": sha256_file(path),
        }
        for path in sorted(Path(SHARED_PKGS).glob("*.app"))
    ]


def published_manifest(manifest):
    sanitized = []
    for case in manifest:
        item = dict(case)
        item["project"] = str(Path(case["project"]).resolve().relative_to(Path(REPO).resolve()))
        sanitized.append(item)
    return sanitized


def write_result(manifest, out):
    RESULT_PATH.parent.mkdir(parents=True, exist_ok=True)
    RESULT_PATH.write_text(json.dumps({"manifest": published_manifest(manifest), **out}, indent=2) + "\n")


def main():
    with open(os.path.join(ROOT, "manifest.json"), encoding="utf-8") as handle:
        manifest = json.load(handle)
    out = {
        "schemaVersion": 2,
        "generatedAtUtc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repository": repository_metadata(),
        "machine": machine_metadata(),
        "compiler": compiler_metadata(),
        "packageSet": package_metadata(),
        "line_tolerance": LINE_TOLERANCE,
        "cases": {},
    }
    tools = ("alc", "al_lsp", "native_build", "native_check")
    execution_errors = []
    if not manifest:
        raise RuntimeError("accuracy manifest is empty")
    if not any(case.get("class") == "control" for case in manifest):
        raise RuntimeError("accuracy manifest has no clean control case")
    if not out["packageSet"]:
        raise RuntimeError("accuracy corpus has no shared .app package set")

    for m in manifest:
        proj = m["project"]
        print(f"\n### {m['id']}  ({m['class']})")
        res = {}
        a = run_alc(proj)
        if a["timed_out"] or (a["rc"] != 0 and not a["diagnostics"]):
            execution_errors.append(
                f"{m['id']}: alc execution was not scoreable (rc={a['rc']}, timeout={a['timed_out']})"
            )
        res["alc"] = {"raw": a, "score": judge(m, a["diagnostics"])}
        print(f"  alc          rc={a['rc']:<4} diags={len(a['diagnostics']):<3} "
              f"{a['ms']:>7.0f} ms  caught={res['alc']['score']['caught']}")
        l = run_al_lsp(proj, m["files"])
        if l.get("error"):
            execution_errors.append(f"{m['id']}: al-lsp {l['error']}")
        res["al_lsp"] = {"raw": l, "score": judge(m, l["diagnostics"])}
        print(f"  al-lsp       diags={len(l['diagnostics']):<3} "
              f"caught={res['al_lsp']['score']['caught']}")
        b = run_native_build(proj)
        if b["timed_out"] or b["parse_error"] or (b["rc"] != 0 and not b["diagnostics"]):
            execution_errors.append(
                f"{m['id']}: native build execution was not scoreable "
                f"(rc={b['rc']}, timeout={b['timed_out']}, parse={b['parse_error']})"
            )
        res["native_build"] = {"raw": b, "score": judge(m, b["diagnostics"])}
        print(f"  native-build rc={b['rc']:<4} diags={len(b['diagnostics']):<3} "
              f"{b['ms']:>7.0f} ms  caught={res['native_build']['score']['caught']}")
        n = run_native_check(proj)
        # `native-check` deliberately exits non-zero when it reports
        # build-blocking findings. A non-zero status is scoreable when the
        # JSON contract parsed and contains diagnostics, just like
        # `pack-native`; only a silent non-zero exit is an execution failure.
        if n["timed_out"] or n["parse_error"] or (
            n["rc"] != 0 and not n["diagnostics"]
        ):
            execution_errors.append(
                f"{m['id']}: native-check execution was not scoreable "
                f"(rc={n['rc']}, timeout={n['timed_out']}, parse={n['parse_error']})"
            )
        res["native_check"] = {"raw": n, "score": judge(m, n["diagnostics"])}
        print(f"  native-check rc={n['rc']:<4} diags={len(n['diagnostics']):<3} "
              f"{n['ms']:>7.0f} ms  caught={res['native_check']['score']['caught']}")
        out["cases"][m["id"]] = {"class": m["class"], "expect": m["expect"], "tools": res}
        write_result(manifest, out)

    print("\n=== per-case ===")
    print(f"{'case':<26}{'class':<11}{'alc':<7}{'al-lsp':<9}{'nbld':<7}{'nchk':<7}{'ours':<7}")
    for m in manifest:
        c = out["cases"][m["id"]]["tools"]
        def mk(t):
            return "OK" if c[t]["score"]["caught"] else "--"
        ours = "OK" if (c["al_lsp"]["score"]["caught"] or
                        c["native_build"]["score"]["caught"] or
                        c["native_check"]["score"]["caught"]) else "--"
        if m["class"] == "control":
            ours = "OK" if (c["al_lsp"]["score"]["caught"] and
                            c["native_build"]["score"]["caught"] and
                            c["native_check"]["score"]["caught"]) else "--"
        print(f"{m['id']:<26}{m['class']:<11}{mk('alc'):<7}{mk('al_lsp'):<9}"
              f"{mk('native_build'):<7}{mk('native_check'):<7}{ours:<7}")

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
                 or out["cases"][m["id"]]["tools"]["native_build"]["score"]["caught"]
                 or out["cases"][m["id"]]["tools"]["native_check"]["score"]["caught"])
    totals["ours_combined"] = {"caught": ours_n, "total": len(real)}
    print(f"  {'ours (any)':<14} {ours_n}/{len(real)}")

    by_class = {}
    for m in real:
        cl = m["class"]
        d = by_class.setdefault(cl, {t: 0 for t in tools} | {"n": 0, "ours": 0})
        d["n"] += 1
        for t in tools:
            if out["cases"][m["id"]]["tools"][t]["score"]["caught"]:
                d[t] += 1
        if (out["cases"][m["id"]]["tools"]["al_lsp"]["score"]["caught"]
                or out["cases"][m["id"]]["tools"]["native_build"]["score"]["caught"]
                or out["cases"][m["id"]]["tools"]["native_check"]["score"]["caught"]):
            d["ours"] += 1
    print("\n=== by class ===")
    print(f"{'class':<12}{'n':<4}{'alc':<7}{'ours':<7}")
    for cl, d in by_class.items():
        print(f"{cl:<12}{d['n']:<4}{d['alc']:<7}{d['ours']:<7}")

    out["totals"] = totals
    out["by_class"] = by_class
    out["valid"] = not execution_errors
    out["executionErrors"] = execution_errors
    write_result(manifest, out)
    print(f"\nwrote {RESULT_PATH}")
    if execution_errors:
        for error in execution_errors:
            print(f"INVALID: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
