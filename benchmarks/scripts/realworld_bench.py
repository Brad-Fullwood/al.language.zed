#!/usr/bin/env python3
"""Real-world tier: a production Business Central extension, not synthetic code.

Synthetic projects are uniform by construction; real AL
has interfaces, enums, report layouts, event subscribers, permission sets and
inconsistent formatting, which is where parsers actually diverge.

The customer working copy is treated as READ-ONLY: each project is copied into
benchmarks/work/ before anything runs, and only aggregate timings and counts
are ever reported — never customer source.

Unlike the synthetic tier this uses each project's OWN .alpackages unmodified,
because real projects commonly depend on third-party symbols that the
standardised Microsoft-only set does not contain.
"""
import json
import os
import shutil
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BENCH = os.path.dirname(HERE)
REPO = os.path.dirname(BENCH)

ALC = os.environ.get("ALC_PATH") or os.path.join(
    os.environ.get("AL_MS_EXT", ""), "bin", "linux", "alc.dll")
EXPLORER = os.path.join(REPO, "target", "release", "al-explorer")
CORPUS_ROOT = os.environ.get("AL_BENCH_REAL_ROOT", "")
CORPUS_NAME = os.environ.get("AL_BENCH_CORPUS_NAME", "private production corpus")

PROJECTS = [
    name.strip()
    for name in os.environ.get("AL_BENCH_PROJECTS", "").split(",")
    if name.strip()
]
ROUNDS = 4


def loadavg():
    try:
        with open("/proc/loadavg") as fh:
            return float(fh.read().split()[0])
    except Exception:
        return None


def stage(name):
    """Copy a customer project into the benchmark work tree (read-only source)."""
    src = os.path.join(CORPUS_ROOT, name)
    dest = os.path.join(BENCH, "work", "real-" + name.replace(" ", "-"))
    if os.path.isdir(dest):
        shutil.rmtree(dest)
    shutil.copytree(src, dest, ignore=shutil.ignore_patterns(
        ".git", "output", "*.app", ".vscode", ".snapshots"))
    pkgs = os.path.join(src, ".alpackages")
    dpkgs = os.path.join(dest, ".alpackages")
    os.makedirs(dpkgs, exist_ok=True)
    if os.path.isdir(pkgs):
        for f in os.listdir(pkgs):
            s = os.path.join(pkgs, f)
            if os.path.isfile(s) and not os.path.isfile(os.path.join(dpkgs, f)):
                shutil.copy2(s, os.path.join(dpkgs, f))
    return dest


def count_al(root):
    n = lines = 0
    for dirpath, _, files in os.walk(root):
        if ".alpackages" in dirpath:
            continue
        for f in files:
            if f.endswith(".al"):
                n += 1
                try:
                    with open(os.path.join(dirpath, f), "r", encoding="utf-8",
                              errors="replace") as fh:
                        lines += sum(1 for _ in fh)
                except Exception:
                    pass
    return n, lines


def run_once(cmd, cwd, out_app):
    if os.path.exists(out_app):
        os.remove(out_app)
    lb = loadavg()
    t0 = time.perf_counter()
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=1800)
        rc, so, se = p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        rc, so, se = -9, "", "TIMEOUT after 1800s"
    ms = (time.perf_counter() - t0) * 1000.0
    size = os.path.getsize(out_app) if os.path.exists(out_app) else 0
    return {"ms": ms, "rc": rc, "size": size, "load": lb,
            "stdout": so[-1500:], "stderr": se[-1500:]}


def summarize(runs):
    timed = [r["ms"] for r in runs[1:]] or [runs[0]["ms"]]
    return {
        "n": len(timed),
        "min_ms": round(min(timed), 2),
        "median_ms": round(statistics.median(timed), 2),
        "max_ms": round(max(timed), 2),
        "ok_runs": sum(1 for r in runs if r["rc"] == 0 and r["size"] > 0),
        "total_runs": len(runs),
        "out_size": runs[-1]["size"],
        "last_rc": runs[-1]["rc"],
        "samples_ms": [round(r["ms"], 2) for r in runs],
        "last_stderr": runs[-1]["stderr"][-800:],
        "last_stdout": runs[-1]["stdout"][-800:],
    }


def main():
    results = {"corpus": CORPUS_NAME,
               "note": "customer sources copied read-only; only aggregates reported",
               "projects": {}}
    targets = sys.argv[1:] or PROJECTS

    for name in targets:
        src = os.path.join(CORPUS_ROOT, name)
        if not os.path.isdir(src):
            print(f"skip {name}: not present")
            continue
        print(f"\n### {name}")
        proj = stage(name)
        n_files, n_lines = count_al(proj)
        npkg = len(os.listdir(os.path.join(proj, ".alpackages")))
        print(f"  al_files={n_files} lines={n_lines} packages={npkg}")
        entry = {"al_files": n_files, "al_lines": n_lines, "packages": npkg}

        out_native = os.path.join(proj, "bench-native.app")
        out_alc = os.path.join(proj, "bench-alc.app")
        arms = {
            "native": ([EXPLORER, "pack-native", "--project", proj, "--out", out_native],
                       proj, out_native),
            "alc": (["dotnet", ALC, f"/project:{proj}", f"/out:{out_alc}",
                     f"/packagecachepath:{os.path.join(proj, '.alpackages')}"],
                    proj, out_alc),
        }
        runs = {k: [] for k in arms}
        for i in range(ROUNDS):
            tag = "warmup" if i == 0 else f"run{i}"
            for k, (cmd, cwd, out) in arms.items():
                r = run_once(cmd, cwd, out)
                runs[k].append(r)
                print(f"    {k:<8} {tag}: {r['ms']:>10.1f} ms rc={r['rc']} "
                      f"size={r['size']:<9} load={r['load']}")
        for k in arms:
            entry[k] = summarize(runs[k])

        nat, alc = entry["native"]["median_ms"], entry["alc"]["median_ms"]
        entry["speedup"] = round(alc / nat, 2) if nat else None
        entry["valid"] = entry["native"]["ok_runs"] > 0 and entry["alc"]["ok_runs"] > 0
        flag = "" if entry["valid"] else "   [NOT VALID: an arm produced no .app]"
        print(f"  => native {nat:.1f} ms vs alc {alc:.1f} ms = {entry['speedup']}x{flag}")
        if not entry["valid"]:
            print(f"     alc rc={entry['alc']['last_rc']} tail: {entry['alc']['last_stdout'][-300:]}")

        results["projects"][name] = entry
        with open(os.path.join(BENCH, "results", "realworld.json"), "w") as fh:
            json.dump(results, fh, indent=2)

    print("\nwrote results/realworld.json")


if __name__ == "__main__":
    main()
