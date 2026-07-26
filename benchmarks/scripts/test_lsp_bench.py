#!/usr/bin/env python3
"""Regressions for the shared LSP benchmark client."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import threading
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("lsp_bench.py")
MICROSOFT_ACTIVE_WORKSPACE = SCRIPT.parent.parent / "ms" / "ms_active_ws.json"
MICROSOFT_ACTIVE_DOCUMENT = SCRIPT.parent.parent / "ms" / "ms_active_document.json"
MICROSOFT_PROBE_PROFILE = SCRIPT.parent.parent / "ms" / "ms_probe_profile.json"
SPEC = importlib.util.spec_from_file_location("lsp_bench", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
lsp_bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(lsp_bench)


class ClientContractTests(unittest.TestCase):
    def test_notification_is_sent_exactly_once(self) -> None:
        sent = []
        client = object.__new__(lsp_bench.LspClient)
        client._send = sent.append
        client.notify("initialized", {})
        self.assertEqual(
            sent,
            [{"jsonrpc": "2.0", "method": "initialized", "params": {}}],
        )

    def test_timed_out_request_is_cancelled(self) -> None:
        sent = []
        client = object.__new__(lsp_bench.LspClient)
        client._id = 0
        client._event = threading.Condition()
        client._responses = {}
        client._alive = True
        client._send = sent.append
        response, _ = client.request("slow/request", {}, timeout=0)
        self.assertIsNone(response)
        self.assertEqual(
            sent,
            [
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "slow/request",
                    "params": {},
                },
                {
                    "jsonrpc": "2.0",
                    "method": "$/cancelRequest",
                    "params": {"id": 1},
                },
            ],
        )

    def test_notification_wait_ignores_messages_before_start(self) -> None:
        client = object.__new__(lsp_bench.LspClient)
        client._event = threading.Condition()
        client._alive = False
        now = time.perf_counter()
        stale = {"method": "textDocument/publishDiagnostics", "params": {}}
        current = {"method": "textDocument/publishDiagnostics", "params": {}}
        client._notifications = [(stale, now - 1), (current, now + 1)]
        message, timestamp = client.wait_notification(
            "textDocument/publishDiagnostics",
            after=now,
        )
        self.assertIs(message, current)
        self.assertEqual(timestamp, now + 1)

    def test_failed_required_lifecycle_step_is_detected(self) -> None:
        entries = [
            {"method": "one", "kind": "notification", "ms": 0.0},
            {"method": "two", "kind": "request", "ok": True},
            {"method": "three", "kind": "request", "ok": False},
            {"method": "four", "kind": "wait_notification", "ok": False},
        ]
        self.assertEqual(
            lsp_bench.failed_required_steps(entries),
            [entries[2], entries[3]],
        )

    def test_mapping_contains_nested_expected_values(self) -> None:
        actual = {
            "percent": 100,
            "owner": 0,
            "details": {"phase": "compile", "extra": True},
        }
        self.assertTrue(
            lsp_bench.mapping_contains(
                actual,
                {"percent": 100, "details": {"phase": "compile"}},
            )
        )
        self.assertFalse(lsp_bench.mapping_contains(actual, {"percent": 99}))

    def test_microsoft_definition_uses_the_extension_protocol(self) -> None:
        standard_params = {
            "textDocument": {"uri": "file:///tmp/Page.al"},
            "position": {"line": 5, "character": 20},
        }
        profile = json.loads(MICROSOFT_PROBE_PROFILE.read_text())
        adapted = lsp_bench.apply_probe_profile(
            [("definition", "textDocument/definition", standard_params)],
            profile,
        )
        self.assertEqual(len(adapted), 1)
        name, method, params = adapted[0]
        self.assertEqual(name, "definition")
        self.assertEqual(method, "al/gotodefinition")
        self.assertEqual(params["textDocumentPositionParams"], standard_params)
        self.assertIsNone(params["configuration"])
        self.assertEqual(params["browserInfo"]["browser"], "SystemDefault")

    def test_output_parents_exist_before_server_spawn(self) -> None:
        with tempfile.TemporaryDirectory() as root:
            result = Path(root) / "results" / "nested" / "lsp.json"
            stderr = Path(root) / "logs" / "nested" / "lsp.stderr.log"
            lsp_bench.prepare_output_paths(result, stderr)
            self.assertTrue(result.parent.is_dir())
            self.assertTrue(stderr.parent.is_dir())

    def test_microsoft_activation_fixture_matches_extension_contract(self) -> None:
        root = "/tmp/AL Project"
        requests = lsp_bench.load_lifecycle_steps(
            MICROSOFT_ACTIVE_WORKSPACE,
            {
                "__ROOT_URI__": "file:///tmp/AL Project",
                "__ROOT_PATH__": root,
                "__ROOT_NAME__": "AL Project",
                "__ALPACKAGES__": f"{root}/.alpackages",
            },
        )
        self.assertEqual(len(requests), 2)
        request, ready = requests
        self.assertEqual(request["method"], "al/setActiveWorkspace")
        self.assertEqual(request["kind"], "request")

        params = request["params"]
        self.assertEqual(
            params["currentWorkspaceFolderPath"],
            {
                "uri": {
                    "$mid": 1,
                    "fsPath": root,
                    "path": root,
                    "scheme": "file",
                },
                "name": "AL Project",
                "index": 0,
            },
        )
        settings = params["settings"]
        self.assertIs(settings["setActiveWorkspace"], True)
        self.assertEqual(settings["activeWorkspaceClosure"], [root])
        self.assertEqual(settings["expectedProjectReferenceDefinitions"], [])
        self.assertNotIn("setActiveWorkspaceOptions", settings)
        resources = settings["alResourceConfigurationSettings"]
        self.assertEqual(resources["backgroundCodeAnalysis"], "File")
        self.assertEqual(resources["packageCachePaths"], [f"{root}/.alpackages"])
        self.assertEqual(
            ready,
            {
                "method": "al/progressNotification",
                "kind": "wait_notification",
                "timeout": 300,
                "match": {"percent": 100},
            },
        )

    def test_microsoft_active_document_fixture_uses_open_uri(self) -> None:
        requests = lsp_bench.load_lifecycle_steps(
            MICROSOFT_ACTIVE_DOCUMENT,
            {"__OPEN_URI__": "file:///tmp/Page.al"},
        )
        self.assertEqual(
            requests,
            [
                {
                    "method": "al/didChangeActiveDocument",
                    "kind": "request",
                    "params": {
                        "textDocument": {"uri": "file:///tmp/Page.al"},
                        "sequenceNumber": 0,
                    },
                }
            ],
        )


if __name__ == "__main__":
    unittest.main()
