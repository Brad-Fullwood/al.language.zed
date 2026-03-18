#!/usr/bin/env python3
"""
Capture the actual SignalR WebSocket messages between EditorServices.Host and BC.

Approach: Spawn EditorServices.Host, send DAP initialize + launch + setBreakpoints,
but also intercept the SignalR WebSocket connection by setting HTTPS_PROXY.

Since we can't easily MITM TLS, instead we'll patch the EditorServices.Host config
to log SignalR messages, or we'll capture at the HTTP level.

Alternative approach used here: Run EditorServices.Host with verbose logging and
capture its debug output which includes SignalR messages.
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

BP_FILE = os.path.join(PROJECT_ROOT, "src/Page/Report Selection.Page.al")
BP_LINE = 169

launch_args = {
    "request": "launch",
    "environmentType": "Sandbox",
    "environmentName": "sandbox",
    "tenant": "1b7a9471-d313-4aa4-99e8-7a6c762c8e7d",
    "startupObjectId": 77704,
    "startupObjectType": "Page",
    "breakOnError": True,
    "breakOnRecordWrite": False,
    "launchBrowser": True,
    "enableSqlInformationDebugger": True,
    "enableLongRunningSqlStatements": True,
    "longRunningSqlStatementsThreshold": 500,
    "numberOfSqlStatements": 10,
    "useMcpServerForDebugging": True,
}

# Enable .NET SignalR logging
env = os.environ.copy()
env["Logging__LogLevel__Microsoft.AspNetCore.SignalR"] = "Debug"
env["Logging__LogLevel__Microsoft.AspNetCore.Http.Connections"] = "Debug"
env["DOTNET_ENVIRONMENT"] = "Development"

# Also check EditorServices log files
es_log = os.path.expanduser("~/.cursor/extensions/ms-dynamics-smb.al-18.0.2190758/bin/linux/EditorServices.log")

proc = subprocess.Popen(
    [ES_HOST, "/startDebugging", f"/projectRoot:{PROJECT_ROOT}"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    cwd=PROJECT_ROOT,
    env=env,
)

# Capture stderr (EditorServices logs here)
stderr_lines = []
def read_stderr():
    while True:
        line = proc.stderr.readline()
        if not line: break
        text = line.decode(errors="replace").strip()
        if text:
            stderr_lines.append(text)
            # Print SignalR-related lines
            lower = text.lower()
            if any(k in lower for k in ["signalr", "hub", "websocket", "breakpoint", "addbreakpoint",
                                          "invoke", "sending", "received", "connection"]):
                print(f"### {text[:200]}")

stderr_t = threading.Thread(target=read_stderr, daemon=True)
stderr_t.start()

def send_dap(msg):
    body = json.dumps(msg).encode()
    header = f"Content-Length: {len(body)}\r\n\r\n".encode()
    proc.stdin.write(header + body)
    proc.stdin.flush()

def read_dap(timeout=60):
    sel = selectors.DefaultSelector()
    sel.register(proc.stdout, selectors.EVENT_READ)
    buf = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        events = sel.select(timeout=1)
        if not events: continue
        chunk = proc.stdout.read1(8192)
        if not chunk: break
        buf += chunk
        while b"\r\n\r\n" in buf:
            header_end = buf.index(b"\r\n\r\n") + 4
            headers = buf[:header_end].decode()
            length = 0
            for line in headers.strip().split("\r\n"):
                if line.startswith("Content-Length:"):
                    length = int(line.split(":")[1].strip())
            if length == 0: buf = buf[header_end:]; continue
            if len(buf) >= header_end + length:
                body = buf[header_end:header_end + length]
                buf = buf[header_end + length:]
                sel.close()
                return json.loads(body)
            else: break
    sel.close()
    return None

def read_until_response(cmd=None, timeout=120):
    start = time.time()
    while time.time() - start < timeout:
        msg = read_dap(timeout=5)
        if msg is None: continue
        t = msg.get("type","")
        c = msg.get("command", msg.get("event",""))
        if t == "event" and c == "output":
            out = msg.get("body",{}).get("output","").strip()
            if out and "warning" not in out.lower():
                print(f"  {out[:150]}")
        elif t == "event":
            print(f"  [{c}] {json.dumps(msg.get('body',{}))[:200]}")
        elif t == "response":
            ok = msg.get("success", False)
            m = msg.get("message","")
            print(f"  {'OK' if ok else 'FAIL'}: {c or '?'} {m[:150]}")
            if cmd is None or c == cmd:
                return msg
    return None

seq = [0]
def ns(): seq[0] += 1; return seq[0]

print("=" * 60)
print("SIGNALR CAPTURE: EditorServices.Host → BC")
print("=" * 60)

# 1. Initialize
print("\n[1] Initialize")
send_dap({"type": "request", "seq": ns(), "command": "initialize",
           "arguments": {"clientID": "capture", "adapterID": "al", "linesStartAt1": True,
                         "columnsStartAt1": True, "pathFormat": "path"}})
read_until_response("initialize", timeout=10)

# 2. Launch
print("\n[2] Launch")
send_dap({"type": "request", "seq": ns(), "command": "launch", "arguments": launch_args})
launch = read_until_response("launch", timeout=120)
if not launch or not launch.get("success"):
    print("Launch failed")
    # Still continue to see what we can

# 3. Set breakpoints
print(f"\n[3] Set breakpoints: {os.path.basename(BP_FILE)}:{BP_LINE}")
send_dap({"type": "request", "seq": ns(), "command": "setBreakpoints",
           "arguments": {"source": {"path": BP_FILE}, "breakpoints": [{"line": BP_LINE}],
                         "sourceModified": False}})
bp_resp = read_until_response("setBreakpoints", timeout=15)
if bp_resp:
    bps = bp_resp.get("body",{}).get("breakpoints",[])
    for bp in bps:
        print(f"  BP: id={bp.get('id')} verified={bp.get('verified')} line={bp.get('line')} msg={bp.get('message','')}")
    print(f"  Full response: {json.dumps(bp_resp.get('body',{}))}")

# 4. ConfigurationDone
print("\n[4] ConfigurationDone")
send_dap({"type": "request", "seq": ns(), "command": "configurationDone"})
read_until_response(timeout=10)

# 5. Wait for stopped
print("\n[5] Waiting for stopped event (45s)...")
start = time.time()
while time.time() - start < 45:
    msg = read_dap(timeout=5)
    if msg is None: continue
    t = msg.get("type","")
    c = msg.get("command", msg.get("event",""))
    body = msg.get("body", {})
    if t == "event" and c == "stopped":
        print(f"\n*** STOPPED: {json.dumps(body)} ***")
        thread_id = body.get("threadId", 1)

        # Stack trace
        send_dap({"type": "request", "seq": ns(), "command": "stackTrace",
                   "arguments": {"threadId": thread_id, "startFrame": 0, "levels": 10}})
        st = read_until_response("stackTrace", timeout=10)
        if st:
            frames = st.get("body",{}).get("stackFrames",[])
            for f in frames[:5]:
                print(f"  Frame: {f.get('name','?')} ({f.get('source',{}).get('name','?')}:{f.get('line','?')})")

        # Scopes
        frame_id = frames[0]["id"] if frames else 0
        send_dap({"type": "request", "seq": ns(), "command": "scopes",
                   "arguments": {"frameId": frame_id}})
        sc = read_until_response("scopes", timeout=10)
        if sc:
            for s in sc.get("body",{}).get("scopes",[]):
                print(f"  Scope: {s.get('name')} ref={s.get('variablesReference')}")

                # Get variables for first scope
                send_dap({"type": "request", "seq": ns(), "command": "variables",
                           "arguments": {"variablesReference": s.get("variablesReference", 0)}})
                vr = read_until_response("variables", timeout=10)
                if vr:
                    vars_ = vr.get("body",{}).get("variables",[])
                    for v in vars_[:5]:
                        print(f"    {v.get('name','?')} = {v.get('value','?')} ({v.get('type','?')})")

        # Continue
        send_dap({"type": "request", "seq": ns(), "command": "continue",
                   "arguments": {"threadId": thread_id}})
        read_until_response("continue", timeout=5)
        break
    elif t == "event":
        print(f"  [{c}] {json.dumps(body)[:150]}")

# Disconnect
print("\n[6] Disconnect")
send_dap({"type": "request", "seq": ns(), "command": "disconnect",
           "arguments": {"terminateDebuggee": True}})
read_until_response(timeout=5)

proc.terminate(); proc.wait(timeout=5)

# Check EditorServices log
if os.path.exists(es_log):
    print(f"\n--- EditorServices.log (last 30 lines) ---")
    with open(es_log) as f:
        lines = f.readlines()
    for line in lines[-30:]:
        if any(k in line.lower() for k in ["breakpoint", "addbreakpoint", "signalr", "hub", "invoke"]):
            print(f"  {line.strip()[:200]}")

# Print all captured stderr that mentions SignalR
print(f"\n--- SignalR-related stderr ({len(stderr_lines)} total lines) ---")
for line in stderr_lines:
    lower = line.lower()
    if any(k in lower for k in ["signalr", "hub", "breakpoint", "invoke", "websocket", "connection"]):
        print(f"  {line[:200]}")

print("\nDone.")
