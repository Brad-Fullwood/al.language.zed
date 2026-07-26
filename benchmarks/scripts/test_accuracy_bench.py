#!/usr/bin/env python3
"""Regressions for the planted-defect accuracy benchmark contract."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("accuracy_bench.py")
SPEC = importlib.util.spec_from_file_location("accuracy_bench", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
accuracy_bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(accuracy_bench)


class ResultInitializationTests(unittest.TestCase):
    def test_prepares_nested_output_directory_before_lsp_logging(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            original_result = accuracy_bench.RESULT_PATH
            result = Path(root) / "fresh" / "nested" / "accuracy.json"
            accuracy_bench.RESULT_PATH = result
            try:
                accuracy_bench.prepare_result_directory()
            finally:
                accuracy_bench.RESULT_PATH = original_result
            self.assertTrue(result.parent.is_dir())


if __name__ == "__main__":
    unittest.main()
