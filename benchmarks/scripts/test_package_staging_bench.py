#!/usr/bin/env python3
"""Regressions for shared benchmark package staging."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from package_staging import PACKAGE_NAMES, stage_package_set


class PackageStagingTests(unittest.TestCase):
    def test_rejects_source_inside_generated_project_without_deleting_it(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            project = Path(root)
            source = project / "source"
            source.mkdir()
            sentinel = source / "keep.txt"
            sentinel.write_text("must survive")
            with self.assertRaisesRegex(RuntimeError, "outside the generated benchmark project"):
                stage_package_set(project, source)
            self.assertEqual(sentinel.read_text(), "must survive")

    def test_rejects_incomplete_package_set_before_replacing_destination(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            base = Path(root)
            project = base / "project"
            source = base / "source"
            destination = project / ".alpackages"
            source.mkdir()
            destination.mkdir(parents=True)
            sentinel = destination / "keep.txt"
            sentinel.write_text("must survive")
            with self.assertRaisesRegex(RuntimeError, "missing benchmark packages"):
                stage_package_set(project, source)
            self.assertEqual(sentinel.read_text(), "must survive")

    def test_stages_exact_package_set_with_hashes(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            base = Path(root)
            project = base / "project"
            source = base / "source"
            project.mkdir()
            source.mkdir()
            for index, name in enumerate(PACKAGE_NAMES):
                (source / name).write_bytes(f"package-{index}".encode())
            metadata = stage_package_set(project, source)
            self.assertEqual([entry["name"] for entry in metadata], list(PACKAGE_NAMES))
            self.assertTrue(all(entry["sha256"] for entry in metadata))
            self.assertEqual(
                sorted(path.name for path in (project / ".alpackages").iterdir()),
                sorted(PACKAGE_NAMES),
            )


if __name__ == "__main__":
    unittest.main()
