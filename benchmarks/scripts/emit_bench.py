#!/usr/bin/env python3
"""Release-grade native-vs-alc build benchmark.

Every measured round starts a fresh Rust driver process. Inside that process,
both production compile backends build the same project in three states:

* processCold: each backend's first build in the fresh process (the OS page
  cache is not forcibly dropped; backend order alternates to balance it);
* warmUnchanged: same process and unchanged sources; and
* oneFileEdit: same process after one deterministic source edit.

Backend order alternates within and across rounds. Round zero is discarded as
machine/runtime warmup. A result is valid only when every measured build emits
an app without errors and every native/alc artifact pair is semantically
equivalent after normalizing only documented provenance and random GUIDs.
"""

from __future__ import annotations

import datetime as dt
import hashlib
import io
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import zipfile
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
BENCH = HERE.parent
REPO = BENCH.parent
DRIVER = REPO / "target" / "release" / "examples" / "build_bench"
RESULT_PATH = Path(os.environ.get("AL_BENCH_EMIT_RESULT", BENCH / "results" / "emit.json"))
ARTIFACT_ROOT = BENCH / "work" / "emit-artifacts"
PACKAGE_SOURCE_VALUE = os.environ.get("AL_BENCH_PACKAGES", "")
PACKAGE_SOURCE = Path(PACKAGE_SOURCE_VALUE) if PACKAGE_SOURCE_VALUE else None
CPUSET = os.environ.get("AL_BENCH_CPUSET", "")
ROUNDS = int(os.environ.get("AL_BENCH_ROUNDS", "6"))

PACKAGE_NAMES = [
    "System.app",
    "Microsoft_System_28.0.51202.0.app",
    "Microsoft_System Application_28.1.49838.50794.app",
    "Microsoft_Business Foundation_28.1.49838.50065.app",
    "Microsoft_Base Application_28.1.49838.51422.app",
    "Microsoft_Application_28.1.49838.50065.app",
]
SCENARIOS = ("processCold", "warmUnchanged", "oneFileEdit")
BACKENDS = ("native", "alc")
KNOWN_TARGETS = {"small", "medium", "large", "xl"}
PHASES = (
    "inputParseNs",
    "dependencyIndexNs",
    "semanticVerificationNs",
    "packageEmissionNs",
    "artifactVerificationNs",
    "outputWriteNs",
    "totalNs",
)


def command_output(args: list[str], default: str = "unknown") -> str:
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=30)
        text = (result.stdout + result.stderr).strip()
        return text or default
    except (OSError, subprocess.TimeoutExpired):
        return default


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def source_fingerprint(project: Path) -> dict[str, Any]:
    files = [project / "app.json"]
    files.extend(sorted(path for path in project.rglob("*.al") if path.is_file()))
    digest = hashlib.sha256()
    lines = 0
    for path in files:
        relative = path.relative_to(project).as_posix()
        content = path.read_bytes()
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update(content)
        lines += content.count(b"\n") + (1 if content and not content.endswith(b"\n") else 0)
    return {
        "files": len(files) - 1,
        "lines": lines,
        "sha256": digest.hexdigest(),
        "appJsonSha256": sha256_file(project / "app.json"),
    }


def standardize_packages(project: Path) -> list[dict[str, Any]]:
    if PACKAGE_SOURCE is None or not PACKAGE_SOURCE.is_dir():
        raise RuntimeError("AL_BENCH_PACKAGES must name a directory containing the pinned package set")
    destination = project / ".alpackages"
    if PACKAGE_SOURCE.resolve() == destination.resolve():
        raise RuntimeError(
            "AL_BENCH_PACKAGES must be an immutable source outside the generated "
            f"benchmark project; it aliases the staging destination {destination}"
        )
    missing = [name for name in PACKAGE_NAMES if not (PACKAGE_SOURCE / name).is_file()]
    if missing:
        raise RuntimeError("missing benchmark packages: " + ", ".join(missing))

    if destination.exists() or destination.is_symlink():
        if destination.is_symlink() or destination.is_file():
            destination.unlink()
        else:
            shutil.rmtree(destination)
    destination.mkdir(parents=True)
    metadata = []
    for name in PACKAGE_NAMES:
        source = PACKAGE_SOURCE / name
        target = destination / name
        shutil.copy2(source, target)
        metadata.append({
            "name": name,
            "bytes": target.stat().st_size,
            "sha256": sha256_file(target),
        })
    return metadata


def read_app(path: Path) -> dict[str, bytes]:
    data = path.read_bytes()
    offset = data.find(b"PK\x03\x04")
    if not data.startswith(b"NAVX") or offset < 0:
        raise ValueError(f"{path} is not a NAVX-wrapped zip package")
    with zipfile.ZipFile(io.BytesIO(data[offset:])) as archive:
        return {name: archive.read(name) for name in archive.namelist()}


def normalize_manifest(value: bytes) -> bytes:
    return re.sub(br"^\s*<Build .*?/>\s*$", b"", value, flags=re.MULTILINE)


def normalize_navigation(value: bytes) -> Any:
    """Return the stable semantic shape of generated navigation XML.

    Both compilers generate fresh ControlGUID values. Microsoft's compiler also
    emits sibling ActionDefinition elements in a different order across
    repeated builds of identical input, so that order cannot be an artifact
    equivalence contract. Other element ordering, element text, and every
    non-ControlGUID attribute remain significant.
    """

    try:
        root = ET.fromstring(value)
    except ET.ParseError as error:
        raise ValueError(f"navigation.xml is not valid XML: {error}") from error

    def local_name(name: str) -> str:
        return name.rsplit("}", 1)[-1]

    def is_action_definition(element: ET.Element) -> bool:
        return any(
            local_name(name) == "type" and attribute == "ActionDefinition"
            for name, attribute in element.attrib.items()
        )

    def canonical(element: ET.Element) -> Any:
        attributes = tuple(
            sorted(
                (name, attribute)
                for name, attribute in element.attrib.items()
                if local_name(name) != "ControlGUID"
            )
        )
        children = [canonical(child) for child in element]
        action_slots = [
            index for index, child in enumerate(element) if is_action_definition(child)
        ]
        if len(action_slots) > 1:
            ordered_actions = sorted(children[index] for index in action_slots)
            for index, action in zip(action_slots, ordered_actions):
                children[index] = action
        text = element.text.strip() if element.text and element.text.strip() else ""
        return element.tag, attributes, text, tuple(children)

    return canonical(root)


def normalize_symbol_reference(value: bytes) -> Any:
    parsed = json.loads(value.decode("utf-8-sig"))
    if isinstance(parsed, dict):
        for group in parsed.values():
            if isinstance(group, list) and all(isinstance(item, dict) for item in group):
                group.sort(key=lambda item: (item.get("Id"), item.get("Name")))
    return parsed


def compare_apps(native: Path, alc: Path) -> dict[str, Any]:
    left = read_app(native)
    right = read_app(alc)
    left_names = set(left)
    right_names = set(right)
    mismatches = []
    normalized_differences = []

    for name in sorted(left_names & right_names):
        if left[name] == right[name]:
            continue
        equivalent = False
        normalization = None
        if name == "NavxManifest.xml":
            equivalent = normalize_manifest(left[name]) == normalize_manifest(right[name])
            normalization = "Build timestamp and compiler provenance"
        elif name == "navigation.xml":
            equivalent = normalize_navigation(left[name]) == normalize_navigation(right[name])
            normalization = "random ControlGUID values"
        elif name == "SymbolReference.json":
            equivalent = (
                normalize_symbol_reference(left[name])
                == normalize_symbol_reference(right[name])
            )
            normalization = "top-level object discovery order"
        if equivalent:
            normalized_differences.append({"entry": name, "normalization": normalization})
            continue
        mismatches.append({
            "entry": name,
            "nativeBytes": len(left[name]),
            "alcBytes": len(right[name]),
            "nativeSha256": hashlib.sha256(left[name]).hexdigest(),
            "alcSha256": hashlib.sha256(right[name]).hexdigest(),
        })

    only_native = sorted(left_names - right_names)
    only_alc = sorted(right_names - left_names)
    return {
        "equivalent": not only_native and not only_alc and not mismatches,
        "entryCount": {"native": len(left), "alc": len(right)},
        "onlyNative": only_native,
        "onlyAlc": only_alc,
        "mismatches": mismatches,
        "normalizedDifferences": normalized_differences,
    }


def relative_artifact(path: str) -> str:
    return str(Path(path).resolve().relative_to(ARTIFACT_ROOT.resolve()))


def sanitize_round(raw: dict[str, Any]) -> dict[str, Any]:
    return {
        "editedFile": raw["editedFile"],
        "measurements": [
            {
                **measurement,
                "artifact": (
                    relative_artifact(measurement["artifact"])
                    if measurement.get("artifact")
                    else None
                ),
                "failure": (
                    {
                        "redacted": True,
                        "sha256": hashlib.sha256(
                            measurement["failure"].encode()
                        ).hexdigest(),
                    }
                    if measurement.get("failure")
                    else None
                ),
            }
            for measurement in raw["measurements"]
        ],
    }


def run_round(project: Path, size: str, round_index: int) -> tuple[dict[str, Any], dict[str, Any]]:
    artifact_dir = ARTIFACT_ROOT / size / f"round-{round_index}"
    artifact_dir.mkdir(parents=True, exist_ok=True)
    command = [str(DRIVER), str(project), str(artifact_dir)]
    if round_index % 2:
        command.append("--reverse")
    if CPUSET:
        command = ["taskset", "-c", CPUSET, *command]
    result = subprocess.run(command, capture_output=True, text=True, timeout=1800)
    if result.returncode != 0:
        raise RuntimeError(
            f"benchmark driver failed (round {round_index}, {size}):\n"
            f"{result.stdout[-2000:]}\n{result.stderr[-4000:]}"
        )
    try:
        raw = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"benchmark driver returned invalid JSON: {error}\n{result.stdout[-4000:]}")

    comparisons = {}
    by_key = {
        (measurement["backend"], measurement["scenario"]): measurement
        for measurement in raw["measurements"]
    }
    for scenario in SCENARIOS:
        native = Path(by_key[("native", scenario)]["artifact"])
        alc = Path(by_key[("alc", scenario)]["artifact"])
        comparisons[scenario] = compare_apps(native, alc)
    return sanitize_round(raw), comparisons


def summarize_samples(samples: list[float]) -> dict[str, Any]:
    return {
        "n": len(samples),
        "minMs": round(min(samples), 3),
        "medianMs": round(statistics.median(samples), 3),
        "meanMs": round(statistics.mean(samples), 3),
        "maxMs": round(max(samples), 3),
        "stdevMs": round(statistics.stdev(samples), 3) if len(samples) > 1 else 0.0,
        "samplesMs": [round(sample, 3) for sample in samples],
    }


def summarize_rounds(
    rounds: list[dict[str, Any]],
    comparisons: list[dict[str, Any]],
) -> tuple[dict[str, Any], bool]:
    measured = rounds[1:]
    summary: dict[str, Any] = {"backends": {}, "speedupAlcOverNative": {}}
    valid = True
    for backend in BACKENDS:
        summary["backends"][backend] = {}
        for scenario in SCENARIOS:
            measurements = [
                next(
                    item
                    for item in round_result["measurements"]
                    if item["backend"] == backend and item["scenario"] == scenario
                )
                for round_result in measured
            ]
            timing = summarize_samples([item["elapsedNs"] / 1_000_000 for item in measurements])
            timing["allSuccessful"] = all(
                item["success"] and item["errors"] == 0 and item["appSize"] > 0
                for item in measurements
            )
            timing["appSizes"] = [item["appSize"] for item in measurements]
            timing["loadAverages"] = [item["loadAverage"] for item in measurements]
            if backend == "native":
                timing["phases"] = {
                    phase: summarize_samples(
                        [item["timings"][phase] / 1_000_000 for item in measurements]
                    )
                    for phase in PHASES
                }
            summary["backends"][backend][scenario] = timing
            valid &= timing["allSuccessful"]

    for scenario in SCENARIOS:
        native_ms = summary["backends"]["native"][scenario]["medianMs"]
        alc_ms = summary["backends"]["alc"][scenario]["medianMs"]
        summary["speedupAlcOverNative"][scenario] = round(alc_ms / native_ms, 3)

    semantic_ok = all(
        comparison[scenario]["equivalent"]
        for comparison in comparisons[1:]
        for scenario in SCENARIOS
    )
    summary["semanticPackageComparison"] = {
        "allMeasuredPairsEquivalent": semantic_ok,
        "pairs": len(comparisons[1:]) * len(SCENARIOS),
            "normalizations": [
                "NavxManifest.xml Build timestamp/compiler provenance",
                "navigation.xml random ControlGUID values and Microsoft "
                "ActionDefinition discovery order",
                "SymbolReference.json top-level object discovery order",
            ],
    }
    return summary, valid and semantic_ok


def machine_metadata() -> dict[str, Any]:
    cpu_model = "unknown"
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu_model = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    memory_bytes = None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                memory_bytes = int(line.split()[1]) * 1024
                break
    except OSError:
        pass
    return {
        "os": platform.platform(),
        "kernel": platform.release(),
        "architecture": platform.machine(),
        "cpu": cpu_model,
        "logicalCpus": os.cpu_count(),
        "memoryBytes": memory_bytes,
        "cpuSet": CPUSET or "scheduler default",
    }


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


def compiler_metadata() -> dict[str, Any]:
    tool_dir = os.environ.get("AL_TOOL_PATH")
    if not tool_dir:
        extension = os.environ.get("AL_MS_EXT")
        if extension:
            tool_dir = str(Path(extension) / "bin" / "linux")
            os.environ["AL_TOOL_PATH"] = tool_dir
    if not tool_dir:
        raise RuntimeError("set AL_TOOL_PATH or AL_MS_EXT to the Microsoft AL extension tool directory")
    alc = Path(tool_dir) / "alc.dll"
    if not alc.is_file():
        raise RuntimeError(f"alc.dll not found at {alc}")
    version_output = command_output(["dotnet", str(alc), "/?"])
    first_line = next(
        (line.strip() for line in version_output.splitlines() if "AL Compiler version" in line),
        "unknown",
    )
    return {
        "version": first_line,
        "alcSha256": sha256_file(alc),
        "driverSha256": sha256_file(DRIVER),
        "rustc": command_output(["rustc", "-Vv"]),
    }


def main() -> int:
    if ROUNDS < 2:
        raise RuntimeError("AL_BENCH_ROUNDS must be at least 2 (one discarded + one measured)")
    if not DRIVER.is_file():
        raise RuntimeError(
            "release benchmark driver is missing; run "
            "`cargo build --release -p al-compile --example build_bench`"
        )

    targets = sys.argv[1:] or ["small", "medium", "large", "xl"]
    unknown_targets = sorted(set(targets) - KNOWN_TARGETS)
    if unknown_targets:
        raise RuntimeError(
            "unknown benchmark project target(s): " + ", ".join(unknown_targets)
        )
    if ARTIFACT_ROOT.exists():
        shutil.rmtree(ARTIFACT_ROOT)
    ARTIFACT_ROOT.mkdir(parents=True)

    result: dict[str, Any] = {
        "schemaVersion": 2,
        "generatedAtUtc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repository": repository_metadata(),
        "machine": machine_metadata(),
        "compiler": compiler_metadata(),
        "methodology": {
            "rounds": ROUNDS,
            "discardedRounds": [0],
            "measuredRounds": ROUNDS - 1,
            "scenarios": list(SCENARIOS),
            "backendOrderAlternates": True,
            "freshDriverPerRound": True,
            "nativePhaseUnits": "nanoseconds",
        },
        "packageSet": [],
        "projects": {},
    }
    all_valid = True
    package_set = None

    for size in targets:
        project = BENCH / "projects" / size
        if not project.is_dir():
            raise RuntimeError(f"generated project does not exist: {project}")
        current_packages = standardize_packages(project)
        if package_set is None:
            package_set = current_packages
            result["packageSet"] = current_packages
        elif current_packages != package_set:
            raise RuntimeError(f"package hashes drifted while staging {size}")

        before = source_fingerprint(project)
        print(f"\n### {size}: {before['files']} AL files, {before['lines']} lines")
        raw_rounds = []
        comparisons = []
        for round_index in range(ROUNDS):
            label = "discarded warmup" if round_index == 0 else f"measured {round_index}"
            print(f"  round {round_index} ({label})")
            round_result, comparison = run_round(project, size, round_index)
            raw_rounds.append(round_result)
            comparisons.append(comparison)
            for scenario in SCENARIOS:
                items = {
                    item["backend"]: item
                    for item in round_result["measurements"]
                    if item["scenario"] == scenario
                }
                print(
                    f"    {scenario:<15} native={items['native']['elapsedNs']/1e6:>9.2f} ms "
                    f"alc={items['alc']['elapsedNs']/1e6:>9.2f} ms "
                    f"semantic={'ok' if comparison[scenario]['equivalent'] else 'FAIL'}"
                )

        after = source_fingerprint(project)
        if after != before:
            raise RuntimeError(f"benchmark edit was not restored for {size}")
        summary, valid = summarize_rounds(raw_rounds, comparisons)
        all_valid &= valid
        result["projects"][size] = {
            "input": before,
            "summary": summary,
            "rounds": raw_rounds,
            "packageComparisons": comparisons,
            "valid": valid,
        }
        RESULT_PATH.parent.mkdir(parents=True, exist_ok=True)
        temporary = RESULT_PATH.with_suffix(RESULT_PATH.suffix + ".tmp")
        temporary.write_text(json.dumps(result, indent=2) + "\n")
        temporary.replace(RESULT_PATH)
        speedups = summary["speedupAlcOverNative"]
        print(
            "  medians alc/native: "
            + ", ".join(f"{scenario}={speedups[scenario]}x" for scenario in SCENARIOS)
        )
        print(f"  result: {'VALID' if valid else 'INVALID'}")

    result["valid"] = all_valid
    temporary = RESULT_PATH.with_suffix(RESULT_PATH.suffix + ".tmp")
    temporary.write_text(json.dumps(result, indent=2) + "\n")
    temporary.replace(RESULT_PATH)
    print(f"\nwrote {RESULT_PATH}")
    print(f"overall result: {'VALID' if all_valid else 'INVALID'}")
    return 0 if all_valid else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, zipfile.BadZipFile) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)
