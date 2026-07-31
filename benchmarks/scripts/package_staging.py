"""Shared, fail-closed staging for the synthetic benchmark package set."""

from __future__ import annotations

import hashlib
import shutil
from pathlib import Path
from typing import Any

PACKAGE_NAMES = (
    "System.app",
    "Microsoft_System_28.0.51202.0.app",
    "Microsoft_System Application_28.1.49838.50794.app",
    "Microsoft_Business Foundation_28.1.49838.50065.app",
    "Microsoft_Base Application_28.1.49838.51422.app",
    "Microsoft_Application_28.1.49838.50065.app",
)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _is_within(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
    except ValueError:
        return False
    return True


def stage_package_set(project: Path, package_source: Path | None) -> list[dict[str, Any]]:
    """Replace ``project/.alpackages`` from an explicit immutable source."""
    project = project.resolve()
    if package_source is None or not package_source.is_dir():
        raise RuntimeError(
            "AL_BENCH_PACKAGES must name a directory containing the pinned package set"
        )
    source = package_source.resolve()
    destination = project / ".alpackages"

    if (
        source == destination
        or _is_within(source, destination)
        or _is_within(destination, source)
    ):
        raise RuntimeError(
            "AL_BENCH_PACKAGES must be an immutable source outside the generated "
            f"benchmark project; it aliases the staging destination {destination}"
        )
    if _is_within(source, project):
        raise RuntimeError(
            "AL_BENCH_PACKAGES must be an immutable source outside the generated "
            f"benchmark project {project}"
        )

    missing = [name for name in PACKAGE_NAMES if not (source / name).is_file()]
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
        source_file = source / name
        target = destination / name
        shutil.copy2(source_file, target)
        metadata.append(
            {
                "name": name,
                "bytes": target.stat().st_size,
                "sha256": sha256_file(target),
            }
        )
    return metadata
