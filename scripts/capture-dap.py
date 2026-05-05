#!/usr/bin/env python3
"""Capture DAP protocol exchange with EditorServices.Host via al-lsp --dap proxy.

Sends DAP initialize → initialized → configurationDone → launch sequence,
captures all responses and events. Output goes to the file given by
--capture-log (or $AL_DAP_CAPTURE).
"""

import argparse
import subprocess
import json
import sys
import os
import time
import re

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--project", required=True,
                    help="Path to AL project root (must contain .zed/debug.json)")
parser.add_argument("--al-lsp", default=None,
                    help="Path to the al-lsp binary (default: $AL_LSP or `which al-lsp`)")
parser.add_argument("--debug-json", default=None,
                    help="Override path to launch config (default: <project>/.zed/debug.json)")
parser.add_argument("--config", default=None,
                    help="Label of the launch config to use (default: first launch config)")
parser.add_argument("--capture-log", default=None,
                    help="DAP capture log file (default: $AL_DAP_CAPTURE or /tmp/dap-capture.log)")
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
CAPTURE_LOG = args.capture_log or os.environ.get("AL_DAP_CAPTURE") or "/tmp/dap-capture.log"

with open(debug_json_path) as f:
    content = f.read()
# Strip trailing commas (Zed allows them, Python doesn't)
content = re.sub(r',\s*([}\]])', r'\1', content)
configs = json.loads(content)

# Pick a launch config: explicit --config label if given, else first launch.
launch_config = None
for c in configs:
    if c.get("request") != "launch":
        continue
    if args.config is None or c.get("label") == args.config:
        launch_config = c
        break

if not launch_config:
    label_hint = f" labelled {args.config!r}" if args.config else ""
    print(f"No launch config{label_hint} found in {debug_json_path}")
    sys.exit(1)

print(f"Using config: {launch_config.get('label')}")

env = os.environ.copy()
env["AL_DAP_CAPTURE"] = CAPTURE_LOG

proc = subprocess.Popen(
    [AL_LSP, "--dap"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=sys.stderr,
    cwd=PROJECT_ROOT,
    env=env,
)

def send_dap(msg):
    body = json.dumps(msg).encode()
    header = f"Content-Length: {len(body)}\r\n\r\n".encode()
    proc.stdin.write(header + body)
    proc.stdin.flush()
    print(f">>> SENT: {msg.get('command', msg.get('type', '?'))}")

def read_dap():
    """Read one DAP message from stdout.

    F-025 invariant: consume exactly one frame and leave any additional
    buffered bytes on `proc.stdout` for the next call. `readline()` and
    `read(length)` both honour their counts exactly on Python's
    BufferedReader, so we never over-consume into the next frame.
    """
    headers = {}
    while True:
        line = proc.stdout.readline().decode().strip()
        if not line:
            break
        if ":" in line:
            key, val = line.split(":", 1)
            headers[key.strip()] = val.strip()

    length = int(headers.get("Content-Length", 0))
    if length == 0:
        return None
    body = proc.stdout.read(length)
    msg = json.loads(body)
    msg_type = msg.get("type", "?")
    cmd = msg.get("command", msg.get("event", ""))
    print(f"<<< RECV [{msg_type}]: {cmd}")
    return msg

seq = 1

# 1. Initialize
send_dap({
    "seq": seq, "type": "request", "command": "initialize",
    "arguments": {
        "clientID": "al-capture",
        "clientName": "AL DAP Capture",
        "adapterID": "al",
        "pathFormat": "path",
        "linesStartAt1": True,
        "columnsStartAt1": True,
        "supportsRunInTerminalRequest": False,
    }
})
seq += 1

# Read responses until we get initialize response + initialized event
for _ in range(10):
    msg = read_dap()
    if msg is None:
        break
    if msg.get("type") == "event" and msg.get("event") == "initialized":
        break

# 2. Configuration done
send_dap({"seq": seq, "type": "request", "command": "configurationDone"})
seq += 1

for _ in range(5):
    msg = read_dap()
    if msg is None:
        break
    if msg.get("type") == "response" and msg.get("command") == "configurationDone":
        break

# 3. Launch
launch_args = dict(launch_config)
# Remove Zed-specific fields
for k in ["adapter", "label", "build"]:
    launch_args.pop(k, None)
# Patch booleans to match what EditorServices expects
if isinstance(launch_args.get("breakOnError"), str):
    val = launch_args["breakOnError"]
    launch_args["breakOnError"] = val.lower() not in ("none", "false")
if isinstance(launch_args.get("breakOnRecordWrite"), str):
    val = launch_args["breakOnRecordWrite"]
    launch_args["breakOnRecordWrite"] = val.lower() not in ("none", "false")

send_dap({
    "seq": seq, "type": "request", "command": "launch",
    "arguments": launch_args,
})
seq += 1

# Read all responses and events for 60 seconds
print("\nWaiting for launch response and events (60s)...")
import select
start = time.time()
while time.time() - start < 60:
    # Check if there's data available
    import selectors
    sel = selectors.DefaultSelector()
    sel.register(proc.stdout, selectors.EVENT_READ)
    events = sel.select(timeout=2)
    sel.close()
    if events:
        msg = read_dap()
        if msg is None:
            print("EOF from server")
            break
        # Print full message for analysis
        print(f"    Full: {json.dumps(msg, indent=2)[:500]}")

    # Check if process died
    if proc.poll() is not None:
        print(f"Process exited with code {proc.returncode}")
        break

proc.terminate()
proc.wait(timeout=5)

print(f"\nCapture log: {CAPTURE_LOG}")
if os.path.exists(CAPTURE_LOG):
    with open(CAPTURE_LOG) as f:
        print(f"Log size: {len(f.read())} bytes")
