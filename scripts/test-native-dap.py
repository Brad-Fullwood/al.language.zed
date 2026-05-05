#!/usr/bin/env python3
"""End-to-end test of the native DAP server.

Sends the full DAP sequence and reports results for each step.
"""

import argparse
import subprocess
import json
import sys
import os
import re
import time
import selectors

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--project", required=True,
                    help="Path to AL project root (must contain .zed/debug.json)")
parser.add_argument("--al-lsp", default=None,
                    help="Path to the al-lsp binary (default: $AL_LSP or `which al-lsp`)")
parser.add_argument("--debug-json", default=None,
                    help="Override path to launch config (default: <project>/.zed/debug.json)")
parser.add_argument("--config", default=None,
                    help="Label of launch config to use (default: first launch config)")
parser.add_argument("--breakpoint-file", default=None,
                    help="AL file to set a breakpoint in (default: first .al file under <project>/src)")
args = parser.parse_args()

PROJECT_ROOT = os.path.abspath(os.path.expanduser(args.project))
if not os.path.isdir(PROJECT_ROOT):
    parser.error(f"--project not found: {PROJECT_ROOT}")

AL_LSP = args.al_lsp or os.environ.get("AL_LSP")
if not AL_LSP:
    from shutil import which
    AL_LSP = which("al-lsp")
if not AL_LSP or not os.path.isfile(AL_LSP):
    parser.error("Could not locate al-lsp; pass --al-lsp or set $AL_LSP")
AL_LSP = os.path.abspath(os.path.expanduser(AL_LSP))

debug_json_path = args.debug_json or os.path.join(PROJECT_ROOT, ".zed", "debug.json")
if not os.path.isfile(debug_json_path):
    parser.error(f"debug.json not found: {debug_json_path}")

with open(debug_json_path) as f:
    content = f.read()
content = re.sub(r',\s*([}\]])', r'\1', content)
configs = json.loads(content)
launch_config = None
for c in configs:
    if c.get("request") != "launch":
        continue
    if args.config is None or c.get("label") == args.config:
        launch_config = c
        break
if not launch_config:
    label_hint = f" labelled {args.config!r}" if args.config else ""
    print(f"ERROR: No launch config{label_hint} found in {debug_json_path}")
    sys.exit(1)

print(f"Config: {launch_config.get('label')}")
print(f"Tenant: {launch_config.get('tenant', 'default')}")
print(f"Environment: {launch_config.get('environmentType', '?')}/{launch_config.get('environmentName', '?')}")
print()

proc = subprocess.Popen(
    [AL_LSP, "--dap"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    cwd=PROJECT_ROOT,
)

seq = [0]

def next_seq():
    seq[0] += 1
    return seq[0]

def send(command, arguments=None):
    msg = {"seq": next_seq(), "type": "request", "command": command}
    if arguments:
        msg["arguments"] = arguments
    body = json.dumps(msg).encode()
    header = f"Content-Length: {len(body)}\r\n\r\n".encode()
    proc.stdin.write(header + body)
    proc.stdin.flush()
    return msg["seq"]

def recv(timeout_sec=120):
    """Read one DAP message.

    F-025 invariant: this function MUST consume exactly one DAP frame
    (header block + Content-Length-many body bytes) and leave any
    additional buffered bytes on `proc.stdout` for the next call.
    `proc.stdout` is a BufferedReader and `read(n)` honours `n` exactly,
    so the byte-by-byte header read + exact-size body read pattern below
    is sufficient — do NOT switch to `read1()` or any chunked read that
    might over-consume into the next frame.
    """
    sel = selectors.DefaultSelector()
    sel.register(proc.stdout, selectors.EVENT_READ)

    # Read headers
    headers = {}
    header_buf = b""
    deadline = time.time() + timeout_sec
    while time.time() < deadline:
        events = sel.select(timeout=1)
        if not events:
            # Check stderr for output
            try:
                sel2 = selectors.DefaultSelector()
                sel2.register(proc.stderr, selectors.EVENT_READ)
                if sel2.select(timeout=0):
                    err = proc.stderr.read1(4096).decode(errors='replace')
                    if err.strip():
                        for line in err.strip().split('\n'):
                            if 'INFO' not in line and 'DEBUG' not in line:
                                print(f"  [stderr] {line.strip()}")
                sel2.close()
            except:
                pass
            continue
        byte = proc.stdout.read(1)
        if not byte:
            sel.close()
            return None
        header_buf += byte
        if header_buf.endswith(b"\r\n\r\n"):
            break

    sel.close()

    for line in header_buf.decode().strip().split("\r\n"):
        if ":" in line:
            k, v = line.split(":", 1)
            headers[k.strip()] = v.strip()

    length = int(headers.get("Content-Length", 0))
    if length == 0:
        return None

    body = b""
    while len(body) < length:
        chunk = proc.stdout.read(length - len(body))
        if not chunk:
            return None
        body += chunk

    return json.loads(body)

def recv_until(command=None, event=None, timeout_sec=120):
    """Read messages until we get the one we want."""
    start = time.time()
    while time.time() - start < timeout_sec:
        msg = recv(timeout_sec=max(1, timeout_sec - int(time.time() - start)))
        if msg is None:
            return None
        msg_type = msg.get("type")
        msg_cmd = msg.get("command", "")
        msg_evt = msg.get("event", "")

        if msg_type == "event" and msg_evt == "output":
            output = msg.get("body", {}).get("output", "")
            if output.strip():
                print(f"  [output] {output.strip()}")
        elif msg_type == "event":
            print(f"  [event] {msg_evt}")

        if command and msg_type == "response" and msg_cmd == command:
            return msg
        if event and msg_type == "event" and msg_evt == event:
            return msg
    return None

def test_step(name, send_cmd, send_args=None, expect_cmd=None, timeout=120):
    print(f"\n{'='*60}")
    print(f"TEST: {name}")
    print(f"{'='*60}")
    req_seq = send(send_cmd, send_args)
    resp = recv_until(command=expect_cmd or send_cmd, timeout_sec=timeout)
    if resp is None:
        print(f"  FAIL: No response (timeout)")
        return False
    success = resp.get("success", False)
    if success:
        print(f"  PASS: {send_cmd} succeeded")
        body = resp.get("body")
        if body:
            # Print abbreviated body
            body_str = json.dumps(body)
            if len(body_str) > 200:
                body_str = body_str[:200] + "..."
            print(f"  Body: {body_str}")
    else:
        msg = resp.get("message", "unknown error")
        print(f"  FAIL: {msg}")
    return success

# ===== RUN TESTS =====

print("Starting native DAP test sequence...")
print()

# 1. Initialize
ok = test_step("Initialize", "initialize", {
    "clientID": "dap-test",
    "adapterID": "al",
    "pathFormat": "path",
    "linesStartAt1": True,
    "columnsStartAt1": True,
})
if not ok:
    print("\nFATAL: Initialize failed")
    proc.terminate()
    sys.exit(1)

# Read initialized event
evt = recv_until(event="initialized", timeout_sec=5)
if evt:
    print("  Got 'initialized' event")

# 2. ConfigurationDone
ok = test_step("ConfigurationDone", "configurationDone")

# 3. Launch (compile + publish + connect)
launch_args = dict(launch_config)
for k in ["adapter", "label", "build"]:
    launch_args.pop(k, None)

ok = test_step("Launch (compile + publish + debug connect)", "launch", launch_args, timeout=180)
if not ok:
    print("\nLaunch failed — stopping tests")
    send("disconnect")
    proc.terminate()
    proc.wait(timeout=5)
    sys.exit(1)

# 4. Set breakpoints
test_file = args.breakpoint_file
if test_file:
    test_file = os.path.abspath(os.path.expanduser(test_file))
if not test_file or not os.path.exists(test_file):
    # Find any .al file under <project>/src.
    test_file = None
    for root, dirs, files in os.walk(os.path.join(PROJECT_ROOT, "src")):
        for f in files:
            if f.endswith(".al"):
                test_file = os.path.join(root, f)
                break
        if test_file:
            break
    if not test_file:
        print("ERROR: No .al file found under <project>/src and --breakpoint-file not given")
        proc.terminate()
        sys.exit(1)

print(f"\n  Using breakpoint file: {os.path.basename(test_file)}")
ok = test_step("Set Breakpoints", "setBreakpoints", {
    "source": {"path": test_file},
    "breakpoints": [{"line": 10}],
})

# 5. Threads
ok = test_step("Threads", "threads")

# 6. Stack Trace (may be empty if not stopped)
ok = test_step("Stack Trace", "stackTrace", {"threadId": 1})

# 7. Scopes
ok = test_step("Scopes", "scopes", {"frameId": 0})

# 8. Variables
ok = test_step("Variables", "variables", {"variablesReference": 1})

# 9. Evaluate
ok = test_step("Evaluate Expression", "evaluate", {
    "expression": "1 + 1",
    "frameId": 0,
    "context": "watch",
})

# 10. Continue
ok = test_step("Continue", "continue", {"threadId": 1})

# 11. Disconnect
ok = test_step("Disconnect", "disconnect")

print(f"\n{'='*60}")
print("TEST SEQUENCE COMPLETE")
print(f"{'='*60}")

proc.wait(timeout=5)
