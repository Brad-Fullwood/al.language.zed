#!/usr/bin/env python3
"""Server-agnostic LSP latency benchmark client.

Speaks LSP over stdio to any server binary and measures wall-clock latency for
initialize, first diagnostic publish, and a set of standard requests. Both
al-lsp and Microsoft's EditorServices host are driven through this same client
so the comparison is apples-to-apples.

Server-specific lifecycle steps model the real editor client around the shared
LSP core. For example, Microsoft requires workspace activation, a project-ready
event, and an active-document request. Probe profiles likewise map a logical
operation to the real provider protocol when the editor extension uses a custom
endpoint (Microsoft definition uses `al/gotodefinition`). The result records the
actual method for every probe so those adaptations remain visible.
"""
import argparse
import copy
import datetime as dt
import hashlib
import json
import os
import platform
import shlex
import subprocess
import statistics
import sys
import threading
import time
from pathlib import Path


HERE = Path(__file__).resolve().parent
BENCH = HERE.parent
REPO = BENCH.parent


def path_to_uri(p: str) -> str:
    p = os.path.abspath(p)
    return "file://" + p.replace(os.sep, "/")


def loadavg():
    """1-minute load average, recorded alongside every result.

    The two servers run in sequence rather than interleaved (LSP sessions are
    long-lived and stateful), so a latency comparison is only trustworthy when
    both legs saw comparable load. Recording it lets the report say so instead
    of assuming it.
    """
    try:
        with open("/proc/loadavg") as fh:
            return float(fh.read().split()[0])
    except Exception:
        return None


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def command_output(args, default="unknown"):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=30)
        text = (result.stdout + result.stderr).strip()
        return text or default
    except (OSError, subprocess.TimeoutExpired):
        return default


def repository_metadata():
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


def machine_metadata():
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


def server_artifacts(command, extra_paths=()):
    artifacts = []
    seen = set()
    for candidate in (command[0], *extra_paths):
        executable = Path(candidate)
        if not executable.is_file():
            continue
        resolved = executable.resolve()
        if resolved in seen:
            continue
        seen.add(resolved)
        artifacts.append(
            {
                "name": executable.name,
                "bytes": executable.stat().st_size,
                "sha256": sha256_file(executable),
            }
        )
    return artifacts


def display_project_path(path):
    resolved = Path(path).resolve()
    try:
        return resolved.relative_to(REPO.resolve()).as_posix()
    except ValueError:
        return "<external-project>"


def write_result(path, result):
    output = Path(path)
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_suffix(output.suffix + ".tmp")
    temporary.write_text(json.dumps(result, indent=2) + "\n")
    temporary.replace(output)


def prepare_output_paths(result_path, stderr_path):
    """Create output parents before spawning a server that writes either file."""
    for path in (Path(result_path), Path(stderr_path)):
        path.parent.mkdir(parents=True, exist_ok=True)


def check_stderr_contract(text, required=(), forbidden=(), terminal=None):
    """Evaluate literal stderr evidence after the server has fully exited."""
    checks = {
        "required": [
            {"pattern": pattern, "found": pattern in text}
            for pattern in required
        ],
        "forbidden": [
            {"pattern": pattern, "found": pattern in text}
            for pattern in forbidden
        ],
        "terminal": None,
    }
    reasons = []
    for check in checks["required"]:
        if not check["found"]:
            reasons.append(
                f"server stderr did not contain required evidence: {check['pattern']}"
            )
    for check in checks["forbidden"]:
        if check["found"]:
            reasons.append(
                f"server stderr contained forbidden evidence: {check['pattern']}"
            )
    if terminal:
        lines = [line for line in text.splitlines() if line.strip()]
        last_line = lines[-1] if lines else ""
        matched = terminal in last_line
        checks["terminal"] = {
            "pattern": terminal,
            "matched": matched,
            "lastLine": last_line,
        }
        if not matched:
            reasons.append(
                "server emitted work after its expected terminal stderr marker "
                f"or never emitted it: {terminal}"
            )
    return checks, reasons


def shutdown_contract_reasons(result, allow_forced_kill=False):
    reasons = []
    if not result["requestOk"]:
        reasons.append("server did not complete the LSP shutdown request")
    if not result["exitSent"]:
        reasons.append("benchmark client did not send the LSP exit notification")
    if not result["stdinClosed"]:
        reasons.append("benchmark client did not close the stdio LSP transport")
    if result["forcedKill"] and not allow_forced_kill:
        reasons.append("server required a forced kill after LSP shutdown")
    if result["processExitCode"] != 0 and not (
        allow_forced_kill and result["forcedKill"]
    ):
        reasons.append(f"server exited with code {result['processExitCode']}")
    return reasons


def failed_required_steps(entries):
    return [
        entry
        for entry in entries
        if entry.get("kind") in {"request", "wait_notification"}
        and not entry.get("ok", False)
    ]


def load_lifecycle_steps(path, substitutions):
    """Load lifecycle steps after replacing their declared placeholders."""
    raw = Path(path).read_text()
    for key, value in substitutions.items():
        raw = raw.replace(key, json.dumps(value)[1:-1])
    return json.loads(raw)


def mapping_contains(actual, expected):
    """Return whether actual recursively contains every expected mapping item."""
    if not isinstance(actual, dict) or not isinstance(expected, dict):
        return actual == expected
    return all(
        key in actual and mapping_contains(actual[key], value)
        for key, value in expected.items()
    )


def execute_lifecycle_steps(client, items, default_timeout):
    """Execute declared request, notification, and readiness-wait steps."""
    entries = []
    total_ms = 0.0
    for item in items:
        method = item["method"]
        params = item.get("params")
        kind = item.get("kind", "request")
        if kind == "notification":
            client.notify(method, params)
            entries.append({"method": method, "kind": kind, "ms": 0.0})
            continue
        if kind == "wait_notification":
            started = time.perf_counter()
            expected = item.get("match", {})
            message, _ = client.wait_notification(
                method,
                timeout=item.get("timeout", default_timeout),
                predicate=lambda candidate: mapping_contains(
                    candidate.get("params", {}),
                    expected,
                ),
            )
            elapsed = (time.perf_counter() - started) * 1000.0
            total_ms += elapsed
            entries.append({
                "method": method,
                "kind": kind,
                "ms": round(elapsed, 4),
                "ok": message is not None,
                "match": expected,
            })
            continue
        if kind != "request":
            raise RuntimeError(f"unsupported lifecycle step kind: {kind}")
        response, elapsed = client.request(
            method,
            params,
            timeout=item.get("timeout", default_timeout),
        )
        total_ms += elapsed
        entries.append({
            "method": method,
            "kind": kind,
            "ms": round(elapsed, 4),
            "ok": response is not None and "error" not in (response or {}),
            "error": (response or {}).get("error"),
        })
    return entries, total_ms


def deep_merge(base: dict, extra: dict) -> dict:
    for k, v in extra.items():
        if isinstance(v, dict) and isinstance(base.get(k), dict):
            deep_merge(base[k], v)
        else:
            base[k] = v
    return base


def apply_probe_profile(probes, profile):
    """Adapt a logical probe to a server's real client-facing protocol."""
    adapted = []
    for name, method, params in probes:
        override = profile.get(name)
        if not override:
            adapted.append((name, method, params))
            continue
        envelope = override.get("wrapParamsAs")
        if envelope:
            adapted_params = copy.deepcopy(override.get("params", {}))
            adapted_params[envelope] = params
        else:
            adapted_params = copy.deepcopy(params)
            deep_merge(adapted_params, override.get("params", {}))
        adapted.append((name, override.get("method", method), adapted_params))
    return adapted


class LspClient:
    def __init__(self, cmd, cwd, stderr_path, workspace_folders=None):
        self.stderr_file = open(stderr_path, "wb")
        self.proc = subprocess.Popen(
            cmd, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.stderr_file, bufsize=0,
        )
        self.workspace_folders = workspace_folders
        self._id = 0
        self._lock = threading.Lock()
        self._responses = {}
        self._notifications = []
        self._event = threading.Condition()
        self._alive = True
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()

    def _read_loop(self):
        f = self.proc.stdout
        while self._alive:
            headers = {}
            while True:
                line = f.readline()
                if not line:
                    self._alive = False
                    with self._event:
                        self._event.notify_all()
                    return
                line = line.decode("utf-8", "replace").strip()
                if line == "":
                    break
                if ":" in line:
                    k, v = line.split(":", 1)
                    headers[k.strip().lower()] = v.strip()
            n = int(headers.get("content-length", 0))
            if n <= 0:
                continue
            body = b""
            while len(body) < n:
                chunk = f.read(n - len(body))
                if not chunk:
                    self._alive = False
                    return
                body += chunk
            try:
                msg = json.loads(body.decode("utf-8", "replace"))
            except Exception:
                continue
            now = time.perf_counter()
            with self._event:
                if "id" in msg and ("result" in msg or "error" in msg):
                    self._responses[msg["id"]] = (msg, now)
                elif "method" in msg:
                    self._notifications.append((msg, now))
                    # Server-to-client requests must be answered or the server
                    # may block waiting (MS pulls `workspace/configuration`).
                    if "id" in msg:
                        self._answer_server_request(msg)
                self._event.notify_all()

    def _answer_server_request(self, msg):
        method = msg.get("method", "")
        if method == "workspace/configuration":
            items = msg.get("params", {}).get("items", [])
            result = [{} for _ in items]
        elif method in ("window/workDoneProgress/create", "client/registerCapability",
                        "client/unregisterCapability"):
            result = None
        elif method == "workspace/workspaceFolders":
            result = self.workspace_folders
        else:
            result = None
        self._send({"jsonrpc": "2.0", "id": msg["id"], "result": result})

    def _send(self, obj):
        data = json.dumps(obj).encode("utf-8")
        header = f"Content-Length: {len(data)}\r\n\r\n".encode("ascii")
        with self._lock:
            try:
                self.proc.stdin.write(header + data)
                self.proc.stdin.flush()
            except (BrokenPipeError, ValueError):
                pass

    def notify(self, method, params):
        message = {"jsonrpc": "2.0", "method": method}
        # JSON-RPC permits an omitted params member, while tower-lsp rejects
        # `params: null` for parameterless LSP methods such as shutdown/exit.
        if params is not None:
            message["params"] = params
        self._send(message)

    def request(self, method, params, timeout=30.0):
        with self._event:
            self._id += 1
            rid = self._id
        t0 = time.perf_counter()
        message = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            message["params"] = params
        self._send(message)
        deadline = time.time() + timeout
        with self._event:
            while rid not in self._responses:
                if not self._alive:
                    return None, (time.perf_counter() - t0) * 1000.0
                remaining = deadline - time.time()
                if remaining <= 0:
                    self.notify("$/cancelRequest", {"id": rid})
                    return None, (time.perf_counter() - t0) * 1000.0
                self._event.wait(min(remaining, 0.05))
            msg, _ = self._responses.pop(rid)
        return msg, (time.perf_counter() - t0) * 1000.0

    def wait_notification(self, method, timeout=30.0, predicate=None, after=None):
        deadline = time.time() + timeout
        seen = 0
        with self._event:
            while True:
                while seen < len(self._notifications):
                    msg, ts = self._notifications[seen]
                    seen += 1
                    if (
                        msg.get("method") == method
                        and (after is None or ts >= after)
                        and (predicate is None or predicate(msg))
                    ):
                        return msg, ts
                if not self._alive:
                    return None, None
                remaining = deadline - time.time()
                if remaining <= 0:
                    return None, None
                self._event.wait(min(remaining, 0.05))

    def close(self, grace=3.0):
        started = time.perf_counter()
        result = {
            "requestOk": False,
            "requestError": None,
            "exitSent": False,
            "stdinClosed": False,
            "forcedKill": False,
            "processExitCode": None,
            "elapsedMs": None,
        }
        try:
            response, _ = self.request("shutdown", None, timeout=grace)
            result["requestOk"] = (
                response is not None and "error" not in response
            )
            result["requestError"] = (response or {}).get("error")
            self.notify("exit", None)
            result["exitSent"] = True
            # stdio is the LSP transport. tower-lsp records `exit` in service
            # state, then finishes Server::serve when its framed stdin reaches
            # EOF. Keeping the parent pipe open while waiting for the child is
            # therefore a client/server deadlock, not a slow server shutdown.
            self.proc.stdin.close()
            result["stdinClosed"] = True
        except Exception as error:
            result["requestError"] = str(error)
        try:
            self.proc.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            result["forcedKill"] = True
            self.proc.kill()
            self.proc.wait(timeout=grace)
        result["processExitCode"] = self.proc.returncode
        self._alive = False
        try:
            self.stderr_file.close()
        except Exception:
            pass
        result["elapsedMs"] = round(
            (time.perf_counter() - started) * 1000.0,
            4,
        )
        return result


def summarize(samples):
    if not samples:
        return None
    return {
        "n": len(samples),
        "min_ms": round(min(samples), 4),
        "median_ms": round(statistics.median(samples), 4),
        "mean_ms": round(statistics.mean(samples), 4),
        "max_ms": round(max(samples), 4),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server", required=True, help="server command (may include args, space separated)")
    ap.add_argument("--label", required=True)
    ap.add_argument("--root", required=True)
    ap.add_argument("--open-file", required=True)
    ap.add_argument("--completion-pos", default="45:12", help="line:character (0-based)")
    ap.add_argument("--hover-pos", default="4:20")
    ap.add_argument("--definition-pos", default="4:20")
    ap.add_argument("--iterations", type=int, default=11)
    ap.add_argument("--diag-timeout", type=float, default=60.0)
    ap.add_argument("--req-timeout", type=float, default=30.0)
    ap.add_argument("--pre-timeout", type=float, default=120.0)
    ap.add_argument("--pre-requests", default=None,
                    help="JSON lifecycle steps run after initialized and before didOpen")
    ap.add_argument("--post-open-requests", default=None,
                    help="JSON lifecycle steps run immediately after didOpen")
    ap.add_argument("--probe-profile", default=None,
                    help="JSON adaptations from logical probes to a server's real protocol")
    ap.add_argument("--server-artifact", action="append", default=[],
                    help="additional server artifact to hash (repeatable)")
    ap.add_argument("--require-stderr-pattern", action="append", default=[],
                    help="literal evidence required in the complete server stderr log")
    ap.add_argument("--forbid-stderr-pattern", action="append", default=[],
                    help="literal evidence that invalidates the complete server stderr log")
    ap.add_argument("--terminal-stderr-pattern", default=None,
                    help="literal text required in the last non-empty server stderr line")
    ap.add_argument("--allow-forced-kill", action="store_true",
                    help="record, but do not invalidate, a server known to require client termination after clean LSP shutdown")
    ap.add_argument("--init-params", default=None, help="JSON file merged into initialize params")
    ap.add_argument("--out", required=True)
    ap.add_argument("--stderr-log", required=True)
    args = ap.parse_args()
    if args.iterations < 2:
        raise RuntimeError("--iterations must be at least 2 (one discarded + one measured)")
    prepare_output_paths(args.out, args.stderr_log)

    root = os.path.abspath(args.root)
    root_uri = path_to_uri(root)
    open_path = os.path.abspath(args.open_file)
    open_uri = path_to_uri(open_path)
    with open(open_path, "r", encoding="utf-8") as fh:
        open_text = fh.read()

    cmd = shlex.split(args.server)
    if not cmd:
        raise RuntimeError("--server resolved to an empty command")
    out = {
        "schemaVersion": 3,
        "generatedAtUtc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repository": repository_metadata(),
        "machine": machine_metadata(),
        "label": args.label,
        "serverCommand": [Path(cmd[0]).name, *cmd[1:]],
        "serverArtifacts": server_artifacts(cmd, args.server_artifact),
        "root": display_project_path(root),
        "openFile": Path(open_path).resolve().relative_to(Path(root).resolve()).as_posix(),
        "iterations": args.iterations,
        "discardedIterations": [0],
        "pre_requests": [],
        "post_open_requests": [],
        "requests": {},
        "notes": [],
        "load_start": loadavg(),
    }

    t_spawn = time.perf_counter()
    workspace_folders = [
        {"uri": root_uri, "name": os.path.basename(root.rstrip("/"))}
    ]
    cli = LspClient(
        cmd,
        cwd=root,
        stderr_path=args.stderr_log,
        workspace_folders=workspace_folders,
    )

    init_params = {
        "processId": os.getpid(),
        "clientInfo": {"name": "al-bench", "version": "1.0"},
        "locale": "en-us",
        "rootPath": root,
        "rootUri": root_uri,
        "workspaceFolders": workspace_folders,
        "capabilities": {
            "workspace": {
                "workspaceFolders": True,
                "configuration": True,
                "didChangeConfiguration": {"dynamicRegistration": True},
                "symbol": {"dynamicRegistration": True},
                "applyEdit": True,
            },
            "textDocument": {
                "synchronization": {"dynamicRegistration": True, "didSave": True},
                "completion": {
                    "dynamicRegistration": True,
                    "completionItem": {"snippetSupport": True, "documentationFormat": ["markdown", "plaintext"]},
                    "contextSupport": True,
                },
                "hover": {"dynamicRegistration": True, "contentFormat": ["markdown", "plaintext"]},
                "signatureHelp": {"dynamicRegistration": True},
                "definition": {"dynamicRegistration": True, "linkSupport": True},
                "references": {"dynamicRegistration": True},
                "documentSymbol": {"dynamicRegistration": True, "hierarchicalDocumentSymbolSupport": True},
                "publishDiagnostics": {"relatedInformation": True},
                "formatting": {"dynamicRegistration": True},
                "codeAction": {"dynamicRegistration": True},
                "rename": {"dynamicRegistration": True},
            },
            "window": {"workDoneProgress": True},
        },
        "initializationOptions": {},
    }
    if args.init_params:
        with open(args.init_params) as fh:
            deep_merge(init_params, json.load(fh))

    resp, init_ms = cli.request("initialize", init_params, timeout=args.req_timeout)
    out["spawn_to_init_ms"] = round((time.perf_counter() - t_spawn) * 1000.0, 4)
    out["initialize_ms"] = round(init_ms, 4)
    if resp is None:
        out["notes"].append("initialize TIMED OUT or server died")
        out["valid"] = False
        out["invalidReasons"] = ["initialize timed out or server exited"]
        write_result(args.out, out)
        cli.close()
        print(f"[{args.label}] initialize FAILED", file=sys.stderr)
        return 1
    caps = (resp.get("result") or {}).get("capabilities", {})
    out["server_capabilities"] = sorted(caps.keys())

    cli.notify("initialized", {})

    lifecycle_start = time.perf_counter()
    substitutions = {
        "__ROOT_URI__": root_uri,
        "__ROOT_PATH__": root,
        "__ROOT_NAME__": os.path.basename(root.rstrip("/")),
        "__OPEN_URI__": open_uri,
        "__OPEN_PATH__": open_path,
        "__ALPACKAGES__": os.path.join(root, ".alpackages"),
    }
    pre_total_ms = 0.0
    if args.pre_requests:
        out["pre_requests"], pre_total_ms = execute_lifecycle_steps(
            cli,
            load_lifecycle_steps(args.pre_requests, substitutions),
            args.pre_timeout,
        )
    out["pre_requests_total_ms"] = round(pre_total_ms, 4)
    failed_pre_requests = failed_required_steps(out["pre_requests"])
    if failed_pre_requests:
        failed_methods = ", ".join(entry["method"] for entry in failed_pre_requests)
        out["cold_ready_ms"] = None
        out["load_end"] = loadavg()
        out["valid"] = False
        out["invalidReasons"] = [
            f"required server-specific pre-request failed: {failed_methods}"
        ]
        write_result(args.out, out)
        cli.close()
        print(
            f"[{args.label}] required pre-request FAILED: {failed_methods}",
            file=sys.stderr,
        )
        return 1

    def project_diagnostic(message):
        uri = message.get("params", {}).get("uri", "")
        return uri == root_uri or uri.startswith(root_uri.rstrip("/") + "/")

    # Microsoft publishes clean project diagnostics during activation, before
    # didOpen. Preserve that real readiness event rather than requiring a
    # duplicate notification the server does not send.
    diag, diag_ts = cli.wait_notification(
        "textDocument/publishDiagnostics",
        timeout=0,
        predicate=project_diagnostic,
        after=lifecycle_start,
    )
    diagnostic_phase = "pre_open" if diag is not None else None

    t_open = time.perf_counter()
    cli.notify("textDocument/didOpen", {
        "textDocument": {"uri": open_uri, "languageId": "al", "version": 1, "text": open_text}
    })

    post_open_total_ms = 0.0
    if args.post_open_requests:
        out["post_open_requests"], post_open_total_ms = execute_lifecycle_steps(
            cli,
            load_lifecycle_steps(args.post_open_requests, substitutions),
            args.pre_timeout,
        )
    out["post_open_requests_total_ms"] = round(post_open_total_ms, 4)
    failed_post_open_requests = failed_required_steps(out["post_open_requests"])
    if failed_post_open_requests:
        failed_methods = ", ".join(
            entry["method"] for entry in failed_post_open_requests
        )
        out["cold_ready_ms"] = None
        out["load_end"] = loadavg()
        out["valid"] = False
        out["invalidReasons"] = [
            f"required post-open request failed: {failed_methods}"
        ]
        write_result(args.out, out)
        cli.close()
        print(
            f"[{args.label}] required post-open request FAILED: {failed_methods}",
            file=sys.stderr,
        )
        return 1
    lifecycle_ready = time.perf_counter()

    if diag is None:
        diag, diag_ts = cli.wait_notification(
            "textDocument/publishDiagnostics",
            timeout=args.diag_timeout,
            predicate=lambda message: message.get("params", {}).get("uri") == open_uri,
            after=t_open,
        )
        if diag is not None:
            diagnostic_phase = "post_open"
    if diag is None:
        out["first_diagnostic_ms"] = None
        out["notes"].append(f"no publishDiagnostics within {args.diag_timeout}s — GAP")
    else:
        out["first_diagnostic_ms"] = round(
            (diag_ts - lifecycle_start) * 1000.0,
            4,
        )
        out["first_diagnostic_count"] = len(diag.get("params", {}).get("diagnostics", []))
        out["first_diagnostic_file"] = os.path.basename(
            diag.get("params", {}).get("uri", "")
        )
        out["first_diagnostic_phase"] = diagnostic_phase

    if out.get("first_diagnostic_ms") is not None:
        lifecycle_ready_ms = (lifecycle_ready - lifecycle_start) * 1000.0
        out["cold_ready_ms"] = round(
            out["initialize_ms"]
            + max(lifecycle_ready_ms, out["first_diagnostic_ms"]),
            4,
        )
    else:
        out["cold_ready_ms"] = None

    def pos(spec):
        line, char = spec.split(":")
        return {"line": int(line), "character": int(char)}

    probes = [
        ("completion", "textDocument/completion",
         {"textDocument": {"uri": open_uri}, "position": pos(args.completion_pos),
          "context": {"triggerKind": 1}}),
        ("hover", "textDocument/hover",
         {"textDocument": {"uri": open_uri}, "position": pos(args.hover_pos)}),
        ("definition", "textDocument/definition",
         {"textDocument": {"uri": open_uri}, "position": pos(args.definition_pos)}),
        ("documentSymbol", "textDocument/documentSymbol",
         {"textDocument": {"uri": open_uri}}),
        ("workspaceSymbol", "workspace/symbol", {"query": "Bench"}),
    ]
    if args.probe_profile:
        with open(args.probe_profile) as profile_file:
            probes = apply_probe_profile(probes, json.load(profile_file))

    loads = []
    for name, method, params in probes:
        samples = []
        result_size = None
        errors = 0
        loads.append(loadavg())
        for i in range(args.iterations):
            r, ms = cli.request(method, params, timeout=args.req_timeout)
            if r is None:
                errors += 1
                continue
            if "error" in r:
                errors += 1
                continue
            if i == 0:
                res = r.get("result")
                if isinstance(res, list):
                    result_size = len(res)
                elif isinstance(res, dict) and isinstance(res.get("items"), list):
                    result_size = len(res["items"])
                elif res is None:
                    result_size = 0
                else:
                    result_size = 1
            if i > 0:  # discard warmup
                samples.append(ms)
        entry = summarize(samples) or {"n": 0}
        entry["method"] = method
        entry["result_size"] = result_size
        entry["errors"] = errors
        out["requests"][name] = entry

    out["shutdown"] = cli.close()
    out["shutdown"]["forcedKillAllowed"] = args.allow_forced_kill
    stderr_text = Path(args.stderr_log).read_text(
        encoding="utf-8",
        errors="replace",
    )
    out["stderrContract"], stderr_reasons = check_stderr_contract(
        stderr_text,
        args.require_stderr_pattern,
        args.forbid_stderr_pattern,
        args.terminal_stderr_pattern,
    )
    seen = [l for l in loads if l is not None]
    out["load_end"] = loadavg()
    out["load_mean_during_probes"] = round(statistics.mean(seen), 2) if seen else None

    invalid_reasons = []
    invalid_reasons.extend(
        shutdown_contract_reasons(out["shutdown"], args.allow_forced_kill)
    )
    if args.allow_forced_kill and out["shutdown"]["forcedKill"]:
        out["notes"].append(
            "server completed standard LSP shutdown/exit but did not terminate; "
            "the client applied the explicitly allowed forced-termination fallback"
        )
    invalid_reasons.extend(stderr_reasons)
    if not out["serverArtifacts"]:
        invalid_reasons.append("no benchmark server binary artifact was identified")
    for phase, entries in (
        ("pre-request", out["pre_requests"]),
        ("post-open request", out["post_open_requests"]),
    ):
        if failed_required_steps(entries):
            invalid_reasons.append(f"one or more server-specific {phase}s failed")
    if out.get("first_diagnostic_ms") is None:
        invalid_reasons.append("no publishDiagnostics notification was observed")
    expected_samples = args.iterations - 1
    for name, entry in out["requests"].items():
        if entry.get("errors") != 0:
            invalid_reasons.append(f"{name} returned {entry.get('errors')} errors/timeouts")
        if entry.get("n") != expected_samples:
            invalid_reasons.append(
                f"{name} recorded {entry.get('n', 0)} measured samples; expected {expected_samples}"
            )
        if not isinstance(entry.get("result_size"), int) or entry["result_size"] <= 0:
            invalid_reasons.append(f"{name} warmup returned an empty result")
    out["valid"] = not invalid_reasons
    out["invalidReasons"] = invalid_reasons
    write_result(args.out, out)

    print(f"\n=== {args.label} ===")
    print(f"  initialize          {out['initialize_ms']:>10.3f} ms")
    if pre_total_ms:
        print(f"  pre-requests        {pre_total_ms:>10.3f} ms")
    if post_open_total_ms:
        print(f"  post-open requests  {post_open_total_ms:>10.3f} ms")
    fd = out.get("first_diagnostic_ms")
    print(f"  first diagnostic    {fd if fd is None else f'{fd:10.3f} ms'}"
          f"  (count={out.get('first_diagnostic_count')})")
    cr = out.get("cold_ready_ms")
    print(f"  COLD READY          {cr if cr is None else f'{cr:10.3f} ms'}")
    for name, e in out["requests"].items():
        if e.get("n"):
            print(f"  {name:<18}{e['median_ms']:>10.3f} ms  (n={e['n']} size={e['result_size']} err={e['errors']})")
        else:
            print(f"  {name:<18}{'FAILED':>10}  (err={e['errors']})")
    print(f"  load: start={out['load_start']} during={out['load_mean_during_probes']} "
          f"end={out['load_end']}")
    for n in out["notes"]:
        print(f"  NOTE: {n}")
    if invalid_reasons:
        for reason in invalid_reasons:
            print(f"  INVALID: {reason}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
