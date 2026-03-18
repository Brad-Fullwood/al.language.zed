#!/usr/bin/env python3
"""
Spawn EditorServices.Host directly, send the full DAP sequence, and log
every single message in both directions. No Zed, no proxy.

This gives us the exact protocol for: initialize, launch, setBreakpoints,
configurationDone, and any events (stopped, output, etc.)
"""

import subprocess
import json
import sys
import os
import time
import selectors
import re
import threading

PROJECT_ROOT = "/home/bradf/Dev/AL/AL-ForNAV-Direct-Print-On-Event/ForNAV Direct Print On Event/Core"
ES_HOST = "/home/bradf/.cursor/extensions/ms-dynamics-smb.al-18.0.2190758/bin/linux/Microsoft.Dynamics.Nav.EditorServices.Host"

# Load launch config
with open(os.path.join(PROJECT_ROOT, ".zed/debug.json")) as f:
    content = re.sub(r",\s*([}\]])", r"\1", f.read())
configs = json.loads(content)
launch_config = next(c for c in configs if c.get("request") == "launch")
launch_args = {k: v for k, v in launch_config.items() if k not in ("adapter", "label", "build")}

print(f"Project: {PROJECT_ROOT}")
print(f"EditorServices: {ES_HOST}")
print(f"Tenant: {launch_args.get('tenant', '?')}")
print(f"Environment: {launch_args.get('environmentType', '?')}/{launch_args.get('environmentName', '?')}")
print()

# Find a .al file with executable code for breakpoints
bp_file = None
bp_line = None
for root, dirs, files in os.walk(os.path.join(PROJECT_ROOT, "src")):
    for f in files:
        if f.endswith(".al") and "Codeunit" in f:
            full = os.path.join(root, f)
            with open(full) as fh:
                lines = fh.readlines()
            for i, line in enumerate(lines, 1):
                stripped = line.strip()
                if stripped.startswith("begin") or stripped.startswith("if ") or stripped.startswith("this."):
                    bp_file = full
                    bp_line = i
                    break
            if bp_file:
                break
    if bp_file:
        break

print(f"Breakpoint target: {os.path.basename(bp_file)}:{bp_line}")
print()

# Spawn EditorServices.Host
proc = subprocess.Popen(
    [ES_HOST, "/startDebugging", f"/projectRoot:{PROJECT_ROOT}"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    cwd=PROJECT_ROOT,
)

# Stderr reader thread
def read_stderr():
    while True:
        line = proc.stderr.readline()
        if not line:
            break
        text = line.decode(errors="replace").strip()
        if text:
            print(f"### STDERR: {text}")

stderr_thread = threading.Thread(target=read_stderr, daemon=True)
stderr_thread.start()

# DAP framing
def send_dap(msg):
    body = json.dumps(msg).encode()
    header = f"Content-Length: {len(body)}\r\n\r\n".encode()
    proc.stdin.write(header + body)
    proc.stdin.flush()
    print(f"\n>>> SEND [{msg.get('command', msg.get('type', '?'))}] seq={msg.get('seq', '?')}")
    print(f"    {json.dumps(msg)[:500]}")

def read_dap(timeout=60):
    sel = selectors.DefaultSelector()
    sel.register(proc.stdout, selectors.EVENT_READ)
    buf = b""
    deadline = time.time() + timeout

    while time.time() < deadline:
        events = sel.select(timeout=1)
        if not events:
            continue
        chunk = proc.stdout.read1(8192)
        if not chunk:
            sel.close()
            return None
        buf += chunk

        # Try to parse complete messages
        while b"\r\n\r\n" in buf:
            header_end = buf.index(b"\r\n\r\n") + 4
            headers = buf[:header_end].decode()
            length = 0
            for line in headers.strip().split("\r\n"):
                if line.startswith("Content-Length:"):
                    length = int(line.split(":")[1].strip())
            if length == 0:
                buf = buf[header_end:]
                continue
            if len(buf) >= header_end + length:
                body = buf[header_end:header_end + length]
                buf = buf[header_end + length:]
                msg = json.loads(body)
                sel.close()
                return msg
            else:
                break  # Wait for more data

    sel.close()
    return None

def read_all_messages(timeout=5):
    """Read all available messages within timeout."""
    messages = []
    deadline = time.time() + timeout
    while time.time() < deadline:
        msg = read_dap(timeout=max(1, int(deadline - time.time())))
        if msg is None:
            break
        messages.append(msg)
        # Print it
        t = msg.get("type", "?")
        cmd = msg.get("command", msg.get("event", ""))
        success = msg.get("success", "")
        body = msg.get("body", {})
        output = body.get("output", "") if isinstance(body, dict) else ""

        if t == "event" and cmd == "output" and output:
            # Truncate long output
            print(f"<<< EVENT [output]: {output.strip()[:150]}")
        elif t == "event":
            print(f"<<< EVENT [{cmd}]: {json.dumps(body)[:300]}")
        elif t == "response":
            status = "OK" if msg.get("success") else "FAIL"
            rmsg = msg.get("message", "")
            print(f"<<< RESPONSE [{cmd}] {status}: {rmsg}")
            if body and body != {}:
                print(f"    body: {json.dumps(body)[:500]}")
        else:
            print(f"<<< [{t}] {cmd}: {json.dumps(msg)[:300]}")

        # If this completes a key exchange, return early
        if t == "response":
            break
    return messages

seq = 0
def next_seq():
    global seq
    seq += 1
    return seq

# ============================================================
# FULL DAP SEQUENCE
# Correct order: initialize → launch → (wait for initialized) → setBreakpoints → configurationDone
# ============================================================

print("=" * 60)
print("1. INITIALIZE")
print("=" * 60)

send_dap({
    "type": "request", "seq": next_seq(), "command": "initialize",
    "arguments": {
        "clientID": "al-capture",
        "clientName": "AL Protocol Capture",
        "adapterID": "al",
        "locale": "en-US",
        "linesStartAt1": True,
        "columnsStartAt1": True,
        "pathFormat": "path",
        "supportsVariableType": True,
        "supportsRunInTerminalRequest": False,
        "supportsStartDebuggingRequest": True,
    }
})
read_all_messages(10)

print("\n" + "=" * 60)
print("2. LAUNCH (compile + publish + connect)")
print("=" * 60)

# Patch string values to booleans — EditorServices expects booleans not strings
patched_args = dict(launch_args)
for key in ["breakOnError", "breakOnRecordWrite"]:
    val = patched_args.get(key)
    if isinstance(val, str):
        patched_args[key] = val.lower() not in ("none", "false", "0")

send_dap({
    "type": "request", "seq": next_seq(), "command": "launch",
    "arguments": patched_args,
})

# Read messages for up to 120 seconds (compile + publish + connect)
print("\nWaiting for launch completion (up to 120s)...")
launch_done = False
start = time.time()
while time.time() - start < 120:
    msg = read_dap(timeout=5)
    if msg is None:
        continue

    t = msg.get("type", "?")
    cmd = msg.get("command", msg.get("event", ""))
    body = msg.get("body", {})

    if t == "event" and cmd == "output":
        output = body.get("output", "").strip()
        if output:
            print(f"<<< OUTPUT: {output[:200]}")
    elif t == "event":
        print(f"<<< EVENT [{cmd}]: {json.dumps(body)[:500]}")
    elif t == "response":
        status = "OK" if msg.get("success") else "FAIL"
        rmsg = msg.get("message", "")
        print(f"<<< RESPONSE [{cmd}] {status}: {rmsg}")
        if body and body != {}:
            print(f"    body: {json.dumps(body)[:500]}")
        if cmd == "launch" or cmd is None or cmd == "":
            launch_done = True
            break

if not launch_done:
    print("TIMEOUT waiting for launch response")
    proc.terminate()
    proc.wait(timeout=5)
    sys.exit(1)

print("\n" + "=" * 60)
print("3. SET BREAKPOINTS (after launch)")
print("=" * 60)

send_dap({
    "type": "request", "seq": next_seq(), "command": "setBreakpoints",
    "arguments": {
        "source": {"path": bp_file},
        "breakpoints": [{"line": bp_line}],
        "sourceModified": False,
    }
})
read_all_messages(10)

print("\n" + "=" * 60)
print("4. CONFIGURATION DONE")
print("=" * 60)

send_dap({
    "type": "request", "seq": next_seq(), "command": "configurationDone",
})
read_all_messages(5)

print("\n" + "=" * 60)
print("5. WAITING FOR STOPPED EVENT (breakpoint hit) - 60s")
print("=" * 60)

# Read events for 60 seconds looking for a "stopped" event
stopped = False
start = time.time()
while time.time() - start < 60:
    msg = read_dap(timeout=5)
    if msg is None:
        continue

    t = msg.get("type", "?")
    cmd = msg.get("command", msg.get("event", ""))
    body = msg.get("body", {})

    print(f"<<< [{t}] {cmd}: {json.dumps(body)[:500]}")

    if t == "event" and cmd == "stopped":
        stopped = True
        print("\n*** BREAKPOINT HIT! ***")
        break

if stopped:
    print("\n" + "=" * 60)
    print("6. THREADS")
    print("=" * 60)
    send_dap({"type": "request", "seq": next_seq(), "command": "threads"})
    read_all_messages(10)

    print("\n" + "=" * 60)
    print("7. STACK TRACE")
    print("=" * 60)
    send_dap({
        "type": "request", "seq": next_seq(), "command": "stackTrace",
        "arguments": {"threadId": 1, "startFrame": 0, "levels": 20}
    })
    read_all_messages(10)

    print("\n" + "=" * 60)
    print("8. SCOPES (frame 0)")
    print("=" * 60)
    send_dap({
        "type": "request", "seq": next_seq(), "command": "scopes",
        "arguments": {"frameId": 0}
    })
    read_all_messages(10)

    print("\n" + "=" * 60)
    print("9. VARIABLES")
    print("=" * 60)
    send_dap({
        "type": "request", "seq": next_seq(), "command": "variables",
        "arguments": {"variablesReference": 1}
    })
    read_all_messages(10)

    print("\n" + "=" * 60)
    print("10. EVALUATE")
    print("=" * 60)
    send_dap({
        "type": "request", "seq": next_seq(), "command": "evaluate",
        "arguments": {"expression": "1 + 1", "frameId": 0, "context": "watch"}
    })
    read_all_messages(10)

    print("\n" + "=" * 60)
    print("11. CONTINUE")
    print("=" * 60)
    send_dap({
        "type": "request", "seq": next_seq(), "command": "continue",
        "arguments": {"threadId": 1}
    })
    read_all_messages(10)

    print("\n" + "=" * 60)
    print("12. NEXT (step over)")
    print("=" * 60)
    # Wait for another stop first
    time.sleep(2)
    send_dap({
        "type": "request", "seq": next_seq(), "command": "next",
        "arguments": {"threadId": 1}
    })
    read_all_messages(10)

else:
    print("No breakpoint hit within 60s")

print("\n" + "=" * 60)
print("13. DISCONNECT")
print("=" * 60)
send_dap({
    "type": "request", "seq": next_seq(), "command": "disconnect",
    "arguments": {"terminateDebuggee": True}
})
read_all_messages(5)

proc.terminate()
proc.wait(timeout=5)
print("\nDone. Full protocol capture complete.")
