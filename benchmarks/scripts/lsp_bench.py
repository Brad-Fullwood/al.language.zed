#!/usr/bin/env python3
"""Server-agnostic LSP latency benchmark client.

Speaks LSP over stdio to any server binary and measures wall-clock latency for
initialize, first diagnostic publish, and a set of standard requests. Both
al-lsp and Microsoft's EditorServices host are driven through this same client
so the comparison is apples-to-apples.

Servers that need extra handshake steps before they will load a project (the
Microsoft host requires `al/setActiveWorkspace`) are handled via --pre-requests,
which are executed after `initialized` and before the first didOpen, and are
counted toward the reported cold-ready time.
"""
import argparse
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


def server_artifacts(command):
    artifacts = []
    executable = Path(command[0])
    if executable.is_file():
        artifacts.append(
            {
                "name": executable.name,
                "bytes": executable.stat().st_size,
                "sha256": sha256_file(executable),
            }
        )
    extension = os.environ.get("AL_MS_EXT")
    if extension:
        tool_dir = Path(extension) / "bin" / "linux"
        for name in (
            "Microsoft.Dynamics.Nav.EditorServices.Host",
            "Microsoft.Dynamics.Nav.EditorServices.Host.dll",
            "alc.dll",
        ):
            path = tool_dir / name
            if path.is_file():
                artifacts.append(
                    {
                        "name": name,
                        "bytes": path.stat().st_size,
                        "sha256": sha256_file(path),
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


def deep_merge(base: dict, extra: dict) -> dict:
    for k, v in extra.items():
        if isinstance(v, dict) and isinstance(base.get(k), dict):
            deep_merge(base[k], v)
        else:
            base[k] = v
    return base


class LspClient:
    def __init__(self, cmd, cwd, stderr_path):
        self.stderr_file = open(stderr_path, "wb")
        self.proc = subprocess.Popen(
            cmd, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=self.stderr_file, bufsize=0,
        )
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
            result = None
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
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def request(self, method, params, timeout=30.0):
        with self._event:
            self._id += 1
            rid = self._id
        t0 = time.perf_counter()
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deadline = time.time() + timeout
        with self._event:
            while rid not in self._responses:
                if not self._alive:
                    return None, (time.perf_counter() - t0) * 1000.0
                remaining = deadline - time.time()
                if remaining <= 0:
                    return None, (time.perf_counter() - t0) * 1000.0
                self._event.wait(min(remaining, 0.05))
            msg, _ = self._responses.pop(rid)
        return msg, (time.perf_counter() - t0) * 1000.0

    def wait_notification(self, method, timeout=30.0, predicate=None):
        deadline = time.time() + timeout
        seen = 0
        with self._event:
            while True:
                while seen < len(self._notifications):
                    msg, ts = self._notifications[seen]
                    seen += 1
                    if msg.get("method") == method and (predicate is None or predicate(msg)):
                        return msg, ts
                if not self._alive:
                    return None, None
                remaining = deadline - time.time()
                if remaining <= 0:
                    return None, None
                self._event.wait(min(remaining, 0.05))

    def close(self, grace=3.0):
        try:
            self.request("shutdown", None, timeout=grace)
            self.notify("exit", None)
        except Exception:
            pass
        self._alive = False
        try:
            self.proc.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=grace)
        try:
            self.stderr_file.close()
        except Exception:
            pass


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
                    help="JSON file: list of {method, params, kind:request|notification}")
    ap.add_argument("--init-params", default=None, help="JSON file merged into initialize params")
    ap.add_argument("--out", required=True)
    ap.add_argument("--stderr-log", required=True)
    args = ap.parse_args()
    if args.iterations < 2:
        raise RuntimeError("--iterations must be at least 2 (one discarded + one measured)")

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
        "schemaVersion": 2,
        "generatedAtUtc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repository": repository_metadata(),
        "machine": machine_metadata(),
        "label": args.label,
        "serverCommand": [Path(cmd[0]).name, *cmd[1:]],
        "serverArtifacts": server_artifacts(cmd),
        "root": display_project_path(root),
        "openFile": Path(open_path).resolve().relative_to(Path(root).resolve()).as_posix(),
        "iterations": args.iterations,
        "discardedIterations": [0],
        "pre_requests": [],
        "requests": {},
        "notes": [],
        "load_start": loadavg(),
    }

    t_spawn = time.perf_counter()
    cli = LspClient(cmd, cwd=root, stderr_path=args.stderr_log)

    init_params = {
        "processId": os.getpid(),
        "clientInfo": {"name": "al-bench", "version": "1.0"},
        "locale": "en-us",
        "rootPath": root,
        "rootUri": root_uri,
        "workspaceFolders": [{"uri": root_uri, "name": os.path.basename(root.rstrip("/"))}],
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

    pre_total_ms = 0.0
    if args.pre_requests:
        with open(args.pre_requests) as fh:
            pre_raw = fh.read()
        subs = {
            "__ROOT_URI__": root_uri,
            "__ROOT_PATH__": root,
            "__ROOT_NAME__": os.path.basename(root.rstrip("/")),
            "__ALPACKAGES__": os.path.join(root, ".alpackages"),
        }
        for k, v in subs.items():
            pre_raw = pre_raw.replace(k, json.dumps(v)[1:-1])
        for item in json.loads(pre_raw):
            method = item["method"]
            params = item.get("params")
            if item.get("kind") == "notification":
                cli.notify(method, params)
                out["pre_requests"].append({"method": method, "kind": "notification", "ms": 0.0})
                continue
            r, ms = cli.request(method, params, timeout=item.get("timeout", args.pre_timeout))
            pre_total_ms += ms
            out["pre_requests"].append({
                "method": method, "kind": "request", "ms": round(ms, 4),
                "ok": r is not None and "error" not in (r or {}),
                "error": (r or {}).get("error"),
            })
    out["pre_requests_total_ms"] = round(pre_total_ms, 4)

    t_open = time.perf_counter()
    cli.notify("textDocument/didOpen", {
        "textDocument": {"uri": open_uri, "languageId": "al", "version": 1, "text": open_text}
    })

    diag, diag_ts = cli.wait_notification("textDocument/publishDiagnostics", timeout=args.diag_timeout)
    if diag is None:
        out["first_diagnostic_ms"] = None
        out["notes"].append(f"no publishDiagnostics within {args.diag_timeout}s — GAP")
    else:
        out["first_diagnostic_ms"] = round((diag_ts - t_open) * 1000.0, 4)
        out["first_diagnostic_count"] = len(diag.get("params", {}).get("diagnostics", []))
        out["first_diagnostic_file"] = os.path.basename(
            diag.get("params", {}).get("uri", "")
        )

    if out.get("first_diagnostic_ms") is not None:
        out["cold_ready_ms"] = round(
            out["initialize_ms"] + pre_total_ms + out["first_diagnostic_ms"], 4)
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
        entry["result_size"] = result_size
        entry["errors"] = errors
        out["requests"][name] = entry

    cli.close()
    seen = [l for l in loads if l is not None]
    out["load_end"] = loadavg()
    out["load_mean_during_probes"] = round(statistics.mean(seen), 2) if seen else None

    invalid_reasons = []
    if not out["serverArtifacts"]:
        invalid_reasons.append("no benchmark server binary artifact was identified")
    if any(not request.get("ok", False) for request in out["pre_requests"]
           if request.get("kind") == "request"):
        invalid_reasons.append("one or more server-specific pre-requests failed")
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
