#!/usr/bin/env python3
"""Release symbol-index benchmark with isolated cache and daemon state.

Cold and warm ingest are separate daemon processes. The cold process starts
with an empty benchmark-only symbol cache; each warm process starts with the
persisted cache produced by the cold run. Search samples use one warm daemon
and discard the first request for each query.

Microsoft exposes no equivalent isolated package-index API, so these are
absolute native diagnostics rather than head-to-head performance claims.
"""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import time
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
BENCH = HERE.parent
REPO = BENCH.parent
EXPLORER = REPO / "target" / "release" / "al-explorer"
LSP = REPO / "target" / "release" / "al-lsp"
PROJECT = Path(os.environ.get("AL_BENCH_SYMBOL_PROJECT", BENCH / "projects" / "medium"))
RESULT_PATH = Path(
    os.environ.get("AL_BENCH_SYMBOL_RESULT", BENCH / "results" / "symbols.json")
)
CACHE_HOME = Path(
    os.environ.get("AL_BENCH_SYMBOL_CACHE", BENCH / "work" / "symbol-cache")
).resolve()
RUNTIME_HOME = CACHE_HOME / "runtime"
INDEX_CACHE = CACHE_HOME / "al-lsp" / "index"

QUERIES = ["Customer", "Sales Post", "Item Ledger", "Bench", "Gen. Journal"]
N_WARM = int(os.environ.get("AL_BENCH_SYMBOL_WARM_RUNS", "7"))
N_QUERY = int(os.environ.get("AL_BENCH_SYMBOL_QUERY_RUNS", "7"))


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def command_output(args: list[str], default: str = "unknown") -> str:
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=30)
        text = (result.stdout + result.stderr).strip()
        return text or default
    except (OSError, subprocess.TimeoutExpired):
        return default


def repository_metadata() -> dict[str, Any]:
    commit = command_output(["git", "-C", str(REPO), "rev-parse", "HEAD"]).splitlines()[0]
    status = command_output(["git", "-C", str(REPO), "status", "--porcelain"], "")
    diff = subprocess.run(
        ["git", "-C", str(REPO), "diff", "--binary", "HEAD"],
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


def machine_metadata() -> dict[str, Any]:
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


def project_metadata() -> dict[str, Any]:
    root = PROJECT.resolve()
    try:
        display = root.relative_to(REPO.resolve()).as_posix()
    except ValueError:
        display = "<external-project>"
    sources = sorted(path for path in root.rglob("*.al") if path.is_file())
    packages = sorted((root / ".alpackages").glob("*.app"))
    if not packages:
        raise RuntimeError(f"symbol benchmark project has no .app packages: {root}")
    digest = hashlib.sha256()
    lines = 0
    for source in sources:
        relative = source.relative_to(root).as_posix()
        content = source.read_bytes()
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update(content)
        lines += content.count(b"\n")
    return {
        "path": display,
        "sourceFiles": len(sources),
        "sourceLines": lines,
        "sourceSha256": digest.hexdigest(),
        "packages": [
            {
                "name": package.name,
                "bytes": package.stat().st_size,
                "sha256": sha256_file(package),
            }
            for package in packages
        ],
    }


def benchmark_env() -> dict[str, str]:
    environment = os.environ.copy()
    environment["XDG_CACHE_HOME"] = str(CACHE_HOME)
    environment["XDG_RUNTIME_DIR"] = str(RUNTIME_HOME)
    return environment


def run_checked(args: list[str], timeout: int = 600) -> tuple[float, subprocess.CompletedProcess[str]]:
    started = time.perf_counter()
    process = subprocess.run(
        [str(EXPLORER), *args],
        cwd=PROJECT,
        env=benchmark_env(),
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    if process.returncode != 0:
        raise RuntimeError(
            f"al-explorer {' '.join(args)} failed with {process.returncode}:\n"
            f"{process.stdout[-2000:]}\n{process.stderr[-4000:]}"
        )
    return elapsed_ms, process


def shutdown_daemon() -> None:
    run_checked(["daemon-shutdown", "--json"], timeout=30)


def clear_index_cache() -> None:
    # The only deletion target is the `index` child under this run's explicit
    # XDG cache root. Never delete CACHE_HOME itself or a normal user cache.
    target = INDEX_CACHE
    if (
        target.name != "index"
        or target.parent.name != "al-lsp"
        or CACHE_HOME.is_symlink()
        or target.parent.is_symlink()
    ):
        raise RuntimeError(f"refusing unsafe symbol cache target: {target}")
    if target.is_symlink() or target.is_file():
        target.unlink()
    elif target.is_dir():
        shutil.rmtree(target)


def parse_json(process: subprocess.CompletedProcess[str], operation: str) -> Any:
    try:
        return json.loads(process.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"{operation} returned invalid JSON: {error}\n{process.stdout[-2000:]}"
        ) from error


def parse_packages(process: subprocess.CompletedProcess[str]) -> dict[str, int]:
    data = parse_json(process, "packages")
    packages = data if isinstance(data, list) else data.get("packages")
    if not isinstance(packages, list):
        raise RuntimeError(f"packages returned an unexpected JSON shape: {data!r}")
    symbols = 0
    for package in packages:
        if not isinstance(package, dict):
            raise RuntimeError("packages response contains a non-object entry")
        count = next(
            (
                package[key]
                for key in (
                    "object_count",
                    "symbols",
                    "symbolCount",
                    "objects",
                    "objectCount",
                )
                if isinstance(package.get(key), int)
            ),
            None,
        )
        if count is None:
            raise RuntimeError(f"package response has no object count: {package!r}")
        symbols += count
    if not packages or symbols <= 0:
        raise RuntimeError(
            f"symbol benchmark requires loaded package objects; packages={len(packages)} symbols={symbols}"
        )
    return {"packages": len(packages), "symbols": symbols}


def sample(elapsed_ms: float) -> dict[str, Any]:
    try:
        load = os.getloadavg()[0]
    except OSError:
        load = None
    return {"elapsedMs": round(elapsed_ms, 3), "loadAverage1m": load}


def summarize(samples: list[dict[str, Any]]) -> dict[str, Any]:
    values = [entry["elapsedMs"] for entry in samples]
    return {
        "n": len(values),
        "minMs": round(min(values), 3),
        "medianMs": round(statistics.median(values), 3),
        "meanMs": round(statistics.mean(values), 3),
        "maxMs": round(max(values), 3),
        "stdevMs": round(statistics.stdev(values), 3) if len(values) > 1 else 0.0,
        "samples": samples,
    }


def main() -> int:
    if N_WARM < 2:
        raise RuntimeError("AL_BENCH_SYMBOL_WARM_RUNS must be at least 2")
    if N_QUERY < 2:
        raise RuntimeError("AL_BENCH_SYMBOL_QUERY_RUNS must be at least 2")
    for binary in (EXPLORER, LSP):
        if not binary.is_file():
            raise RuntimeError(f"required release binary is missing: {binary}")
    if not (PROJECT / "app.json").is_file():
        raise RuntimeError(f"symbol benchmark project has no app.json: {PROJECT}")

    CACHE_HOME.mkdir(parents=True, exist_ok=True)
    RUNTIME_HOME.mkdir(parents=True, exist_ok=True)
    RUNTIME_HOME.chmod(0o700)
    shutdown_daemon()
    clear_index_cache()

    result: dict[str, Any] = {
        "schemaVersion": 2,
        "generatedAtUtc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repository": repository_metadata(),
        "machine": machine_metadata(),
        "binaries": {
            "alExplorerSha256": sha256_file(EXPLORER),
            "alLspSha256": sha256_file(LSP),
            "rustc": command_output(["rustc", "-Vv"]),
        },
        "project": project_metadata(),
        "methodology": {
            "cache": "isolated XDG cache; only al-lsp/index is cleared",
            "daemon": "fresh daemon for cold and every warm-ingest sample",
            "warmRuns": N_WARM,
            "queryRuns": N_QUERY,
            "discardedQueryRuns": [0],
            "microsoftComparable": False,
        },
    }

    try:
        print("### cold ingest (fresh daemon, empty isolated symbol cache)")
        cold_ms, cold_process = run_checked(["packages", "--json"])
        cold_stats = parse_packages(cold_process)
        result["cold"] = {**sample(cold_ms), "stats": cold_stats}
        print(f"  cold: {cold_ms:.1f} ms stats={cold_stats}")
        shutdown_daemon()

        print("### warm persisted-cache ingest (fresh daemon each sample)")
        warm_samples = []
        for index in range(N_WARM):
            elapsed_ms, process = run_checked(["packages", "--json"])
            stats = parse_packages(process)
            if stats != cold_stats:
                raise RuntimeError(
                    f"package/object counts drifted: cold={cold_stats}, warm={stats}"
                )
            warm_samples.append(sample(elapsed_ms))
            shutdown_daemon()
            print(f"  warm {index}: {elapsed_ms:.1f} ms")
        result["warm"] = summarize(warm_samples)
        result["coldToWarmMedianRatio"] = round(
            cold_ms / result["warm"]["medianMs"], 3
        )

        print("### fuzzy symbol search (one warm daemon)")
        _, warm_process = run_checked(["packages", "--json"])
        if parse_packages(warm_process) != cold_stats:
            raise RuntimeError("warm search daemon loaded a different package set")
        result["search"] = {}
        for query in QUERIES:
            query_samples = []
            hits = None
            for index in range(N_QUERY):
                elapsed_ms, process = run_checked(
                    ["search", query, "--limit", "20", "--json"]
                )
                data = parse_json(process, f"search {query!r}")
                items = data if isinstance(data, list) else data.get("results")
                if not isinstance(items, list):
                    raise RuntimeError(
                        f"search {query!r} returned an unexpected JSON shape: {data!r}"
                    )
                if index == 0:
                    hits = len(items)
                else:
                    query_samples.append(sample(elapsed_ms))
            result["search"][query] = {
                **summarize(query_samples),
                "resultCount": hits,
            }
            print(
                f"  {query!r}: median "
                f"{result['search'][query]['medianMs']:.1f} ms hits={hits}"
            )
    finally:
        shutdown_daemon()

    RESULT_PATH.parent.mkdir(parents=True, exist_ok=True)
    temporary = RESULT_PATH.with_suffix(RESULT_PATH.suffix + ".tmp")
    temporary.write_text(json.dumps(result, indent=2) + "\n")
    temporary.replace(RESULT_PATH)
    print(f"\nwrote {RESULT_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
