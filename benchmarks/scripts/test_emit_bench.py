#!/usr/bin/env python3
"""Regressions for the release-grade emitter benchmark contract."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("emit_bench.py")
SPEC = importlib.util.spec_from_file_location("emit_bench", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
emit_bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(emit_bench)


class NavigationNormalizationTests(unittest.TestCase):
    def test_ignores_only_guid_and_action_discovery_order(self) -> None:
        left = b"""\
<NavigationDefinition xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <Actions xsi:type="ActionGroupDefinition" ControlGUID="{11111111-1111-1111-1111-111111111111}">
    <Actions xsi:type="ActionDefinition" TargetID="2" Name="Two" ControlGUID="{22222222-2222-2222-2222-222222222222}" />
    <Actions xsi:type="ActionDefinition" TargetID="1" Name="One" ControlGUID="{33333333-3333-3333-3333-333333333333}" />
  </Actions>
</NavigationDefinition>
"""
        right = b"""\
<NavigationDefinition xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <Actions xsi:type="ActionGroupDefinition" ControlGUID="{aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa}">
    <Actions Name="One" TargetID="1" xsi:type="ActionDefinition" ControlGUID="{bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb}" />
    <Actions xsi:type="ActionDefinition" Name="Two" TargetID="2" ControlGUID="{cccccccc-cccc-cccc-cccc-cccccccccccc}" />
  </Actions>
</NavigationDefinition>
"""
        self.assertEqual(
            emit_bench.normalize_navigation(left),
            emit_bench.normalize_navigation(right),
        )

    def test_keeps_action_content_significant(self) -> None:
        left = b"""\
<NavigationDefinition xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <Actions xsi:type="ActionDefinition" TargetID="1" />
</NavigationDefinition>
"""
        right = left.replace(b'TargetID="1"', b'TargetID="2"')
        self.assertNotEqual(
            emit_bench.normalize_navigation(left),
            emit_bench.normalize_navigation(right),
        )

    def test_keeps_non_action_order_significant(self) -> None:
        left = b"<NavigationDefinition><Group Name=\"A\"/><Group Name=\"B\"/></NavigationDefinition>"
        right = b"<NavigationDefinition><Group Name=\"B\"/><Group Name=\"A\"/></NavigationDefinition>"
        self.assertNotEqual(
            emit_bench.normalize_navigation(left),
            emit_bench.normalize_navigation(right),
        )


class PackageStagingTests(unittest.TestCase):
    def test_rejects_source_that_aliases_staging_destination(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            project = Path(root)
            destination = project / ".alpackages"
            destination.mkdir()
            sentinel = destination / "keep.txt"
            sentinel.write_text("must survive")
            original_source = emit_bench.PACKAGE_SOURCE
            emit_bench.PACKAGE_SOURCE = destination
            try:
                with self.assertRaisesRegex(RuntimeError, "aliases the staging destination"):
                    emit_bench.standardize_packages(project)
            finally:
                emit_bench.PACKAGE_SOURCE = original_source
            self.assertEqual(sentinel.read_text(), "must survive")


if __name__ == "__main__":
    unittest.main()
