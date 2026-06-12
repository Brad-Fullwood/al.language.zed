#!/usr/bin/env python3
"""Exhaustive audit of every shipped Zed task: substitute variables exactly
as Zed does (textual substitution, then one shell line through fish),
run against real projects, classify the outcome."""
import json, subprocess, os, sys, time

EXT = "/home/braf/Dev/Software/Zed/Zed AL Extension"
JIG = "/home/braf/Dev/AL/JIG UK/JIG UK"
COPY = "/tmp/al audit proj"

ZF = f"{JIG}/objects/General/Codeunit/DataMgmtEventSubs.Codeunit.al"
# find a file in the COPY for mutating file-level tasks
copy_file = None
for root, _, files in os.walk(COPY):
    for f in files:
        if f.endswith(".al"):
            copy_file = os.path.join(root, f); break
    if copy_file: break

SUBS_READONLY = {
    "$ZED_FILE": ZF,
    "$ZED_SYMBOL": "Customer",
    "$ZED_ROW": "30",
}
SUBS_MUTATING = {
    "$ZED_FILE": copy_file or "",
    "$ZED_SYMBOL": "Customer",
    "$ZED_ROW": "1",
}
# per-label symbol overrides (event-flavored tasks need an event name)
SYMBOL_OVERRIDES = {
    "AL: Trace Event '$ZED_SYMBOL'": "OnBeforeSalesShptHeaderInsert",
    "AL: Find Subscribers for '$ZED_SYMBOL'": "OnBeforeSalesShptHeaderInsert",
    "AL: Suggest Events for '$ZED_SYMBOL'": "Sales-Post",
}

# classification
MUTATING = {
    "AL: Format Current File", "AL: Format All", "AL: Apply Quick Fixes (Current File)",
    "AL: Sort Members (Current File)", "AL: Organize File Names (Apply)",
    "AL: Add ApplicationArea (workspace fixup)", "AL: Add ToolTips (workspace fixup)",
    "AL: Add DataClassification (workspace fixup)", "AL: XLIFF Generate Translation File",
    "AL: Generate Permission Set", "AL: Generate Debug Config (.zed/debug.json)",
    "AL: Clear Symbol Cache",
}
SKIP = {
    "AL: Open Object Explorer": "TUI — covered by pty phase",
    "AL: Authenticate to Business Central": "opens browser interactively",
    "AL: Download Symbols (Server)": "needs configured BC server (graceful-fail checked separately)",
    "AL: New Project": "covered separately in empty dir",
    "AL: Mutation Testing": "long-running; covered separately with timeout",
    "AL: Run All Tests": "covered separately (interp run)",
}
# tasks expected to fail gracefully without a BC server — pass if message is actionable
GRACEFUL = {"AL: Debug Start"}
# Correct behaviors that exit non-zero / look unusual on THIS project:
# - Package: JIG UK genuinely fails AS0016 (192 fields missing
#   DataClassification per audit-data) — surfacing it with file:line IS the
#   job. Pass when diagnostics are present.
EXPECT_DIAGNOSTIC_FAIL = {"AL: Package (.app)"}

tasks = json.load(open(f"{EXT}/languages/al/tasks.json"))
results = []
for t in tasks:
    label = t["label"]
    if label in SKIP:
        results.append((label, "SKIP", SKIP[label])); continue
    args = t.get("args", [])
    cmdline = " ".join([t["command"]] + args)
    subs = dict(SUBS_MUTATING if label in MUTATING else SUBS_READONLY)
    if label in SYMBOL_OVERRIDES:
        subs["$ZED_SYMBOL"] = SYMBOL_OVERRIDES[label]
    for k, v in subs.items():
        cmdline = cmdline.replace(k, v)
    cwd = COPY if label in MUTATING else JIG
    t0 = time.time()
    try:
        p = subprocess.run(["/bin/fish", "-c", cmdline], cwd=cwd,
                           capture_output=True, text=True, timeout=180)
        rc, out = p.returncode, (p.stdout + p.stderr).strip()
    except subprocess.TimeoutExpired:
        results.append((label, "TIMEOUT", f">180s: {cmdline}")); continue
    dt = time.time() - t0
    tail = " | ".join(out.splitlines()[-2:])[:160] if out else "(no output)"
    if label in EXPECT_DIAGNOSTIC_FAIL:
        ok = (rc != 0 and ":" in out and "error" in out.lower()) or rc == 0
        results.append((label, "OK" if ok else "FAIL", f"rc={rc} {dt:.1f}s {tail}")); continue
    if label == "AL: Debug Stop":
        ok = "no active debug session" in out or "stopped" in out
        results.append((label, "OK" if ok else "FAIL", f"rc={rc} {dt:.1f}s {tail}")); continue
    if label in GRACEFUL:
        ok = rc != 0 and len(out) > 10 and "panic" not in out.lower()
        results.append((label, "GRACEFUL" if ok else "BAD-FAIL", f"rc={rc} {dt:.1f}s {tail}"))
    elif rc == 0:
        # success but watch for garbage/empty output on report tasks
        results.append((label, "OK", f"{dt:.1f}s {tail}"))
    else:
        results.append((label, "FAIL", f"rc={rc} {dt:.1f}s {tail}"))

w = max(len(l) for l, _, _ in results)
fails = 0
for label, status, detail in results:
    if status in ("FAIL", "BAD-FAIL", "TIMEOUT"):
        fails += 1
    print(f"{status:9} {label:<{w}}  {detail}")
print(f"\n{len(results)} tasks: {fails} failing")
sys.exit(1 if fails else 0)
