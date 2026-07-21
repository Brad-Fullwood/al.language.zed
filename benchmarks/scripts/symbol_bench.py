#!/usr/bin/env python3
"""Symbol index: cold ingest vs warm recall, and query latency at scale.

This measures the capability that has no Microsoft equivalent to compare
against — Microsoft's symbol handling is only reachable through the
EditorServices host inside VS Code, and its package load is not separately
observable or invocable. So this is recorded as a demonstration with absolute
numbers, not a head-to-head ratio.

  cold  — symbol cache cleared; packages parsed from .app containers on disk
  warm  — cache populated; the same work served from the persisted index
  query — fuzzy symbol search across the whole loaded package set
"""
import json
import os
import shutil
import statistics
import subprocess
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BENCH = os.path.dirname(HERE)
REPO = os.path.dirname(BENCH)
EXPLORER = os.path.join(REPO, "target", "release", "al-explorer")
PROJ = os.path.join(BENCH, "projects", "medium")
CACHE = os.path.expanduser("~/.cache/al-lsp")

QUERIES = ["Customer", "Sales Post", "Item Ledger", "Bench", "Gen. Journal"]
N_WARM = 7
N_QUERY = 7


def run(args, cwd=PROJ, timeout=600):
    t0 = time.perf_counter()
    p = subprocess.run([EXPLORER] + args, cwd=cwd, capture_output=True,
                       text=True, timeout=timeout)
    return (time.perf_counter() - t0) * 1000.0, p


def clear_cache():
    for sub in ("symbols", "index", "semantic"):
        d = os.path.join(CACHE, sub)
        if os.path.isdir(d):
            shutil.rmtree(d)


def parse_packages(stdout):
    """Pull package/symbol counts out of `packages --json`."""
    try:
        data = json.loads(stdout)
    except Exception:
        return None
    pkgs = data if isinstance(data, list) else data.get("packages", data)
    if not isinstance(pkgs, list):
        return {"raw": data}
    total = 0
    for p in pkgs:
        if isinstance(p, dict):
            for k in ("symbols", "symbol_count", "objects", "object_count"):
                if isinstance(p.get(k), int):
                    total += p[k]
                    break
    return {"packages": len(pkgs), "symbols": total}


def main():
    out = {"project": PROJ, "explorer": EXPLORER}

    print("### cold ingest (symbol cache cleared)")
    clear_cache()
    cold_ms, p_cold = run(["packages", "--json"])
    out["cold"] = {"ms": round(cold_ms, 2), "rc": p_cold.returncode}
    stats = parse_packages(p_cold.stdout)
    out["cold"]["stats"] = stats
    print(f"  cold: {cold_ms:.1f} ms rc={p_cold.returncode} stats={stats}")
    if p_cold.returncode != 0:
        print("  stderr:", p_cold.stderr[-500:])

    print("### warm recall (cache populated)")
    warm = []
    for i in range(N_WARM):
        ms, p = run(["packages", "--json"])
        if i > 0:
            warm.append(ms)
        print(f"  warm run{i}: {ms:.1f} ms rc={p.returncode}")
    out["warm"] = {
        "n": len(warm),
        "min_ms": round(min(warm), 2),
        "median_ms": round(statistics.median(warm), 2),
        "max_ms": round(max(warm), 2),
    }
    out["cold_to_warm_ratio"] = round(cold_ms / statistics.median(warm), 2) if warm else None
    print(f"  warm median: {out['warm']['median_ms']:.1f} ms "
          f"(cold/warm = {out['cold_to_warm_ratio']}x)")

    print("### fuzzy symbol search (warm)")
    out["search"] = {}
    for q in QUERIES:
        samples, size = [], None
        for i in range(N_QUERY):
            ms, p = run(["search", q, "--limit", "20", "--json"])
            if i == 0:
                try:
                    r = json.loads(p.stdout)
                    size = len(r if isinstance(r, list) else r.get("results", []))
                except Exception:
                    size = None
                continue
            samples.append(ms)
        out["search"][q] = {
            "n": len(samples),
            "median_ms": round(statistics.median(samples), 2),
            "min_ms": round(min(samples), 2),
            "results": size,
        }
        print(f"  search {q!r}: median {out['search'][q]['median_ms']:.1f} ms "
              f"(hits={size})")

    with open(os.path.join(BENCH, "results", "symbols.json"), "w") as fh:
        json.dump(out, fh, indent=2)
    print("\nwrote results/symbols.json")


if __name__ == "__main__":
    main()
