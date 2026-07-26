#!/usr/bin/env python3
"""Fail-closed contracts for committed benchmark evidence."""

from __future__ import annotations

import hashlib
import json
import re
import unittest
from pathlib import Path


PUBLISHED = (
    Path(__file__).resolve().parent.parent
    / "results"
    / "published"
    / "2026-07-26"
)
SCENARIOS = ("processCold", "warmUnchanged", "oneFileEdit")
BACKENDS = ("native", "alc")
PROBES = (
    "completion",
    "hover",
    "definition",
    "documentSymbol",
    "workspaceSymbol",
)


def read_json(name: str) -> dict:
    return json.loads((PUBLISHED / name).read_text(encoding="utf-8"))


def assert_clean_repository(test: unittest.TestCase, result: dict) -> None:
    repository = result["repository"]
    test.assertRegex(repository["commit"], r"^[0-9a-f]{40}$")
    test.assertFalse(repository["dirty"])
    test.assertIsNone(repository["trackedDiffSha256"])


class PublishedEvidenceTests(unittest.TestCase):
    def test_manifest_hashes_match_exact_files(self) -> None:
        manifest = (PUBLISHED / "README.md").read_text(encoding="utf-8")
        declared = dict(
            re.findall(
                r"^\| `([^`]+)` \| `([0-9a-f]{64})` \|$",
                manifest,
                flags=re.MULTILINE,
            )
        )
        files = sorted(
            path.name
            for path in PUBLISHED.iterdir()
            if path.is_file() and path.name != "README.md"
        )
        self.assertEqual(sorted(declared), files)
        for name, expected in declared.items():
            actual = hashlib.sha256((PUBLISHED / name).read_bytes()).hexdigest()
            self.assertEqual(actual, expected, name)

    def test_accuracy_result_is_complete_and_clean(self) -> None:
        result = read_json("accuracy.json")
        assert_clean_repository(self, result)
        self.assertTrue(result["valid"])
        self.assertEqual(result["executionErrors"], [])
        self.assertEqual(result["totals"]["native_build"]["caught"], 14)
        self.assertEqual(result["totals"]["native_build"]["total"], 14)
        self.assertFalse(result["totals"]["native_build"]["false_positive"])
        self.assertEqual(result["totals"]["alc"]["caught"], 13)
        self.assertFalse(result["totals"]["alc"]["false_positive"])
        self.assertFalse(result["totals"]["al_lsp"]["false_positive"])
        self.assertFalse(result["totals"]["native_check"]["false_positive"])

    def test_emitter_result_is_successful_and_semantically_equivalent(self) -> None:
        result = read_json("emit.json")
        assert_clean_repository(self, result)
        self.assertTrue(result["valid"])
        self.assertEqual(set(result["projects"]), {"small", "medium", "large", "xl"})
        for name, project in result["projects"].items():
            self.assertTrue(project["valid"], name)
            comparison = project["summary"]["semanticPackageComparison"]
            self.assertTrue(comparison["allMeasuredPairsEquivalent"], name)
            self.assertEqual(comparison["pairs"], 15, name)
            for backend in BACKENDS:
                for scenario in SCENARIOS:
                    summary = project["summary"]["backends"][backend][scenario]
                    self.assertEqual(summary["n"], 5, f"{name}/{backend}/{scenario}")
                    self.assertTrue(
                        summary["allSuccessful"],
                        f"{name}/{backend}/{scenario}",
                    )
            for round_comparison in project["packageComparisons"]:
                for scenario in SCENARIOS:
                    self.assertTrue(
                        round_comparison[scenario]["equivalent"],
                        f"{name}/{scenario}",
                    )

    def test_symbol_result_has_nonempty_index_and_queries(self) -> None:
        result = read_json("symbols.json")
        assert_clean_repository(self, result)
        self.assertEqual(result["cold"]["stats"], {"packages": 6, "symbols": 11799})
        self.assertEqual(result["warm"]["n"], 7)
        for query, summary in result["search"].items():
            self.assertEqual(summary["n"], 6, query)
            self.assertGreater(summary["resultCount"], 0, query)

    def test_lsp_results_are_nonempty_and_lifecycle_honest(self) -> None:
        native = read_json("lsp_al_medium.json")
        microsoft = read_json("lsp_ms_medium.json")
        for result in (native, microsoft):
            assert_clean_repository(self, result)
            self.assertTrue(result["valid"])
            self.assertEqual(result["invalidReasons"], [])
            self.assertTrue(result["shutdown"]["requestOk"])
            self.assertTrue(result["shutdown"]["exitSent"])
            self.assertTrue(result["shutdown"]["stdinClosed"])
            for probe in PROBES:
                summary = result["requests"][probe]
                self.assertEqual(summary["n"], 10, probe)
                self.assertEqual(summary["errors"], 0, probe)
                self.assertGreater(summary["result_size"], 0, probe)

        self.assertEqual(native["repository"]["commit"], microsoft["repository"]["commit"])
        self.assertTrue(native["readinessEvidence"]["found"])
        self.assertEqual(
            native["readinessEvidence"]["pattern"],
            "semantic analysis complete",
        )
        self.assertGreaterEqual(
            native["cold_ready_ms"],
            native["readinessEvidence"]["ms"],
        )
        self.assertFalse(native["shutdown"]["forcedKill"])
        self.assertEqual(native["shutdown"]["processExitCode"], 0)
        self.assertFalse(native["shutdown"]["forcedKillAllowed"])
        required = {
            entry["pattern"]: entry["found"]
            for entry in native["stderrContract"]["required"]
        }
        self.assertTrue(required["Semantic bridge initialized"])
        self.assertTrue(required["semantic analysis complete"])
        self.assertTrue(native["stderrContract"]["terminal"]["matched"])

        self.assertTrue(microsoft["shutdown"]["forcedKill"])
        self.assertTrue(microsoft["shutdown"]["forcedKillAllowed"])
        self.assertEqual(microsoft["shutdown"]["processExitCode"], -9)
        self.assertTrue(microsoft["notes"])


if __name__ == "__main__":
    unittest.main()
