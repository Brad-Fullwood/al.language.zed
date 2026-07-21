#!/usr/bin/env python3
"""Benchmark native .app emit (al-explorer pack-native) against Microsoft alc.

Two measurements per project:
  * full      — a cold compile with the standard BC symbol packages present.
                This is what a developer actually waits for.
  * emit-only — the same sources with an EMPTY .alpackages, isolating the
                parse+emit path from symbol loading. alc cannot resolve the
                application here, so this measures each tool's raw pipeline
                throughput on identical input rather than a valid build.

Fairness notes:
  * Every project's .alpackages is rebuilt to ONE coherent BC 28.1 package set.
    The customer folders ship a mix of 27.0 and 28.1 packages, which roughly
    doubles symbol-load cost for both tools and adds noise.
  * Runs are serialized; the first run of each pair is discarded as warmup.
  * Customer sources are treated as read-only: projects are copied into the
    benchmark tree, never modified in place.
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
SRC_PKGS = os.environ.get("AL_BENCH_PACKAGES", "")

# One coherent BC 28.1 symbol set (System 28.0 is the matching platform package).
PKG_SET = [
    "System.app",
    "Microsoft_System_28.0.51202.0.app",
    "Microsoft_System Application_28.1.49838.50794.app",
    "Microsoft_Business Foundation_28.1.49838.50065.app",
    "Microsoft_Base Application_28.1.49838.51422.app",
    "Microsoft_Application_28.1.49838.50065.app",
]

# Rounds per arm. Round 0 is warmup and is discarded from the statistics.
ROUNDS = 5


def standardize_pkgs(proj: str) -> int:
    dest = os.path.join(proj, ".alpackages")
    if os.path.isdir(dest):
        shutil.rmtree(dest)
    os.makedirs(dest)
    n = 0
    for name in PKG_SET:
        src = os.path.join(SRC_PKGS, name)
        if os.path.isfile(src):
            shutil.copy2(src, os.path.join(dest, name))
            n += 1
        else:
            print(f"  WARN missing package: {name}", file=sys.stderr)
    return n


def make_emitonly(proj: str, dest_root: str) -> str:
    dest = os.path.join(dest_root, os.path.basename(proj) + "-emitonly")
    if os.path.isdir(dest):
        shutil.rmtree(dest)
    os.makedirs(dest)
    shutil.copy2(os.path.join(proj, "app.json"), os.path.join(dest, "app.json"))
    if os.path.isdir(os.path.join(proj, "src")):
        shutil.copytree(os.path.join(proj, "src"), os.path.join(dest, "src"))
    else:
        for f in os.listdir(proj):
            if f.endswith(".al"):
                shutil.copy2(os.path.join(proj, f), os.path.join(dest, f))
    os.makedirs(os.path.join(dest, ".alpackages"))
    return dest


def loadavg():
    try:
        with open("/proc/loadavg") as fh:
            return float(fh.read().split()[0])
    except Exception:
        return None


def run_once(cmd, cwd, out_app):
    if os.path.exists(out_app):
        os.remove(out_app)
    load_before = loadavg()
    t0 = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=900)
    ms = (time.perf_counter() - t0) * 1000.0
    size = os.path.getsize(out_app) if os.path.exists(out_app) else 0
    return {
        "ms": ms, "rc": p.returncode, "size": size, "load": load_before,
        "stdout": p.stdout[-2000:], "stderr": p.stderr[-2000:],
    }


def summarize_runs(runs):
    """First run of each arm is warmup and is discarded."""
    timed = [r["ms"] for r in runs[1:]] or [runs[0]["ms"]]
    ok = [r for r in runs if r["rc"] == 0 and r["size"] > 0]
    loads = [r["load"] for r in runs if r["load"] is not None]
    return {
        "n": len(timed),
        "min_ms": round(min(timed), 2),
        "median_ms": round(statistics.median(timed), 2),
        "mean_ms": round(statistics.mean(timed), 2),
        "max_ms": round(max(timed), 2),
        "ok_runs": len(ok),
        "total_runs": len(runs),
        "out_size": runs[-1]["size"],
        "last_rc": runs[-1]["rc"],
        "mean_load": round(statistics.mean(loads), 2) if loads else None,
        "samples_ms": [round(r["ms"], 2) for r in runs],
        "last_stderr": runs[-1]["stderr"][-600:],
        "last_stdout": runs[-1]["stdout"][-600:],
    }


def bench_interleaved(arms, rounds):
    """Run competing tools round-robin.

    Running all of tool A then all of tool B lets background load drift between
    the two blocks and silently bias the ratio. Alternating them inside each
    round means any drift is shared, so the median comparison stays honest even
    on a machine that is not perfectly idle.
    """
    runs = {name: [] for name in arms}
    for i in range(rounds):
        tag = "warmup" if i == 0 else f"run{i}"
        for name, (cmd, cwd, out_app) in arms.items():
            r = run_once(cmd, cwd, out_app)
            runs[name].append(r)
            print(f"    {name:<12} {tag}: {r['ms']:>9.1f} ms  rc={r['rc']} "
                  f"size={r['size']:<8} load={r['load']}")
    return {name: summarize_runs(rs) for name, rs in runs.items()}


def main():
    projects_root = os.path.join(BENCH, "projects")
    results_root = os.path.join(BENCH, "results")
    work = os.path.join(BENCH, "work")
    os.makedirs(results_root, exist_ok=True)
    os.makedirs(work, exist_ok=True)

    targets = sys.argv[1:] or ["small", "medium", "large", "xl"]
    results = {"alc": ALC, "explorer": EXPLORER, "projects": {}}

    for size in targets:
        proj = os.path.join(projects_root, size)
        if not os.path.isdir(proj):
            print(f"skip {size}: not found")
            continue
        print(f"\n### {size}")
        npkg = standardize_pkgs(proj)
        n_al = len([f for f in os.listdir(os.path.join(proj, "src"))]) if os.path.isdir(os.path.join(proj, "src")) else 0
        print(f"  packages={npkg} al_files={n_al}")

        entry = {"packages": npkg, "al_files": n_al}

        print("  -- full build (symbols present) --")
        out_native = os.path.join(proj, "bench-native.app")
        out_alc = os.path.join(proj, "bench-alc.app")
        full = bench_interleaved({
            "native": ([EXPLORER, "pack-native", "--project", proj, "--out", out_native],
                       proj, out_native),
            "alc": (["dotnet", ALC, f"/project:{proj}", f"/out:{out_alc}",
                     f"/packagecachepath:{os.path.join(proj, '.alpackages')}"],
                    proj, out_alc),
        }, ROUNDS)
        entry["native_full"], entry["alc_full"] = full["native"], full["alc"]

        print("  -- emit-only (empty .alpackages) --")
        eo = make_emitonly(proj, work)
        out_native_eo = os.path.join(eo, "bench-native.app")
        out_alc_eo = os.path.join(eo, "bench-alc.app")
        eo_res = bench_interleaved({
            "native-eo": ([EXPLORER, "pack-native", "--project", eo, "--out", out_native_eo],
                          eo, out_native_eo),
            "alc-eo": (["dotnet", ALC, f"/project:{eo}", f"/out:{out_alc_eo}",
                        f"/packagecachepath:{os.path.join(eo, '.alpackages')}"],
                       eo, out_alc_eo),
        }, ROUNDS)
        entry["native_emitonly"], entry["alc_emitonly"] = eo_res["native-eo"], eo_res["alc-eo"]

        for key in ("full", "emitonly"):
            nat = entry[f"native_{key}"]["median_ms"]
            alc = entry[f"alc_{key}"]["median_ms"]
            entry[f"speedup_{key}"] = round(alc / nat, 2) if nat > 0 else None
            # An arm that never produced an .app did not do the work being timed.
            entry[f"valid_{key}"] = (entry[f"native_{key}"]["ok_runs"] > 0
                                     and entry[f"alc_{key}"]["ok_runs"] > 0)
            flag = "" if entry[f"valid_{key}"] else "   [NOT A VALID SPEEDUP: an arm produced no .app]"
            print(f"  => {key}: native {nat:.1f} ms vs alc {alc:.1f} ms "
                  f"= {entry[f'speedup_{key}']}x{flag}")

        results["projects"][size] = entry
        with open(os.path.join(results_root, "emit.json"), "w") as fh:
            json.dump(results, fh, indent=2)

    print("\nwrote", os.path.join(results_root, "emit.json"))


if __name__ == "__main__":
    main()
