#!/usr/bin/env python3
"""End-to-end test of the native DAP server.

Sends the full DAP sequence and reports results for each step.
"""

import subprocess
import json
import sys
import os
import re
import time
import selectors

PROJECT_ROOT = "/home/bradf/Dev/AL/AL-ForNAV-Direct-Print-On-Event/ForNAV Direct Print On Event/Core"
AL_LSP = os.path.expanduser("~/Dev/Software/Zed/Zed AL Extension/target/debug/al-lsp")

# Read launch config
debug_json_path = os.path.join(PROJECT_ROOT, ".zed", "debug.json")
with open(debug_json_path) as f:
    content = f.read()
content = re.sub(r',\s*([}\]])', r'\1', content)
configs = json.loads(content)
launch_config = next((c for c in configs if c.get("request") == "launch"), None)
if not launch_config:
    print("ERROR: No launch config found")
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
    """Read one DAP message."""
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
test_file = os.path.join(PROJECT_ROOT, "src/Codeunit/Print Management.Codeunit.al")
if not os.path.exists(test_file):
    # Find any .al file
    for root, dirs, files in os.walk(os.path.join(PROJECT_ROOT, "src")):
        for f in files:
            if f.endswith(".al"):
                test_file = os.path.join(root, f)
                break
        if test_file:
            break

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
