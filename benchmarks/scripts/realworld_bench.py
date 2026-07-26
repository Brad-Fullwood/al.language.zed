#!/usr/bin/env python3
"""Private-corpus release benchmark with publish-safe aggregate output.

Each selected project is copied into `benchmarks/work/`; source input is never
modified. The same release driver and semantic package comparison used by the
public synthetic tier measure process-cold, warm-unchanged, and one-file-edit
states. Project names, source paths, source text, package names, diagnostic
text, and artifact paths are excluded from the result.
"""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path
from typing import Any

from emit_bench import (
    DRIVER,
    SCENARIOS,
    compiler_metadata,
    machine_metadata,
    repository_metadata,
    run_round,
    sha256_file,
    source_fingerprint,
    summarize_rounds,
)

HERE = Path(__file__).resolve().parent
BENCH = HERE.parent
CORPUS_ROOT_VALUE = os.environ.get("AL_BENCH_REAL_ROOT", "")
CORPUS_ROOT = Path(CORPUS_ROOT_VALUE).resolve() if CORPUS_ROOT_VALUE else None
CORPUS_NAME = os.environ.get("AL_BENCH_CORPUS_NAME", "private production corpus")
PROJECTS = [
    name.strip()
    for name in os.environ.get("AL_BENCH_PROJECTS", "").split(",")
    if name.strip()
]
ROUNDS = int(os.environ.get("AL_BENCH_REAL_ROUNDS", "4"))
RESULT_PATH = Path(
    os.environ.get("AL_BENCH_REAL_RESULT", BENCH / "results" / "realworld.json")
)


def resolve_source(name: str) -> Path:
    if CORPUS_ROOT is None or not CORPUS_ROOT.is_dir():
        raise RuntimeError("AL_BENCH_REAL_ROOT must name the private corpus directory")
    source = (CORPUS_ROOT / name).resolve()
    try:
        source.relative_to(CORPUS_ROOT)
    except ValueError as error:
        raise RuntimeError(f"project target escapes AL_BENCH_REAL_ROOT: {name}") from error
    if not source.is_dir() or not (source / "app.json").is_file():
        raise RuntimeError(f"private benchmark project is missing or has no app.json: {name}")
    return source


def stage_project(source: Path, label: str) -> Path:
    work_root = BENCH / "work" / "realworld"
    if work_root.is_symlink():
        raise RuntimeError(f"refusing symlinked real-world benchmark work root: {work_root}")
    destination = work_root / label
    if destination.is_symlink() or destination.is_file():
        destination.unlink()
    elif destination.is_dir():
        shutil.rmtree(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(
        source,
        destination,
        ignore=shutil.ignore_patterns(
            ".git", "output", "*.app", ".vscode", ".snapshots", "target"
        ),
    )
    source_packages = source / ".alpackages"
    destination_packages = destination / ".alpackages"
    destination_packages.mkdir(parents=True, exist_ok=True)
    if source_packages.is_dir():
        for package in sorted(source_packages.glob("*.app")):
            shutil.copy2(package, destination_packages / package.name)
    return destination


def package_fingerprint(project: Path) -> dict[str, Any]:
    packages = sorted((project / ".alpackages").glob("*.app"))
    if not packages:
        raise RuntimeError("staged private project has no .app package dependencies")
    digest = hashlib.sha256()
    total_bytes = 0
    for package in packages:
        file_hash = sha256_file(package)
        digest.update(file_hash.encode())
        digest.update(b"\0")
        total_bytes += package.stat().st_size
    return {
        "count": len(packages),
        "bytes": total_bytes,
        "aggregateContentSha256": digest.hexdigest(),
    }


def publish_round(round_result: dict[str, Any]) -> dict[str, Any]:
    measurements = []
    for measurement in round_result["measurements"]:
        measurements.append(
            {
                "backend": measurement["backend"],
                "scenario": measurement["scenario"],
                "elapsedNs": measurement["elapsedNs"],
                "loadAverage": measurement["loadAverage"],
                "success": measurement["success"],
                "appSize": measurement["appSize"],
                "diagnostics": measurement["diagnostics"],
                "errors": measurement["errors"],
                "timings": measurement["timings"],
            }
        )
    return {"editedFile": "<redacted>", "measurements": measurements}


def publish_comparison(comparison: dict[str, Any]) -> dict[str, Any]:
    return {
        scenario: {
            "equivalent": value["equivalent"],
            "entryCount": value["entryCount"],
            "onlyNativeCount": len(value["onlyNative"]),
            "onlyAlcCount": len(value["onlyAlc"]),
            "mismatchCount": len(value["mismatches"]),
            "normalizedDifferences": [
                item["normalization"] for item in value["normalizedDifferences"]
            ],
        }
        for scenario, value in comparison.items()
    }


def write_result(result: dict[str, Any]) -> None:
    RESULT_PATH.parent.mkdir(parents=True, exist_ok=True)
    temporary = RESULT_PATH.with_suffix(RESULT_PATH.suffix + ".tmp")
    temporary.write_text(json.dumps(result, indent=2) + "\n")
    temporary.replace(RESULT_PATH)


def main() -> int:
    if ROUNDS < 2:
        raise RuntimeError(
            "AL_BENCH_REAL_ROUNDS must be at least 2 (one discarded + one measured)"
        )
    if not DRIVER.is_file():
        raise RuntimeError(
            "release benchmark driver is missing; run "
            "`cargo build --release -p al-compile --example build_bench`"
        )
    targets = sys.argv[1:] or PROJECTS
    if not targets:
        raise RuntimeError("set AL_BENCH_PROJECTS or pass one or more project names")

    result: dict[str, Any] = {
        "schemaVersion": 2,
        "generatedAtUtc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repository": repository_metadata(),
        "machine": machine_metadata(),
        "compiler": compiler_metadata(),
        "corpus": CORPUS_NAME,
        "privacy": {
            "sourceCopiedReadOnly": True,
            "projectNamesPublished": False,
            "sourceOrDiagnosticTextPublished": False,
            "packageNamesPublished": False,
        },
        "methodology": {
            "rounds": ROUNDS,
            "discardedRounds": [0],
            "measuredRounds": ROUNDS - 1,
            "scenarios": list(SCENARIOS),
            "backendOrderAlternates": True,
            "freshDriverPerRound": True,
            "semanticPackageComparisonRequired": True,
        },
        "projects": {},
    }
    all_valid = True

    for index, name in enumerate(targets, start=1):
        label = f"project-{index:02d}"
        source = resolve_source(name)
        source_before = source_fingerprint(source)
        staged = stage_project(source, label)
        staged_before = source_fingerprint(staged)
        if staged_before != source_before:
            raise RuntimeError(f"{label}: staged source fingerprint differs from read-only input")
        packages = package_fingerprint(staged)
        print(
            f"\n### {label}: {staged_before['files']} AL files, "
            f"{staged_before['lines']} lines, {packages['count']} packages"
        )

        rounds = []
        comparisons = []
        for round_index in range(ROUNDS):
            round_result, comparison = run_round(
                staged, f"realworld/{label}", round_index
            )
            rounds.append(round_result)
            comparisons.append(comparison)
            state = "discarded warmup" if round_index == 0 else "measured"
            equivalence = all(
                comparison[scenario]["equivalent"] for scenario in SCENARIOS
            )
            print(
                f"  round {round_index} ({state}): "
                f"semantic={'ok' if equivalence else 'FAIL'}"
            )

        if source_fingerprint(staged) != staged_before:
            raise RuntimeError(f"{label}: benchmark edit was not restored")
        if source_fingerprint(source) != source_before:
            raise RuntimeError(f"{label}: read-only source changed during benchmark")

        summary, valid = summarize_rounds(rounds, comparisons)
        all_valid &= valid
        result["projects"][label] = {
            "input": staged_before,
            "packageSet": packages,
            "summary": summary,
            "rounds": [publish_round(round_result) for round_result in rounds],
            "packageComparisons": [
                publish_comparison(comparison) for comparison in comparisons
            ],
            "valid": valid,
        }
        write_result(result)
        print(f"  result: {'VALID' if valid else 'INVALID'}")

    result["valid"] = all_valid
    write_result(result)
    print(f"\nwrote {RESULT_PATH}")
    return 0 if all_valid else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
