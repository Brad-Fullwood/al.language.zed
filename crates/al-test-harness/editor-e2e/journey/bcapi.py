#!/usr/bin/env python3
"""Automation API helper for journey stage 8. CDX tenant only. Token comes from BC_ACCESS_TOKEN.

  bcapi.py find APP_ID       print one line per matching extension row, exit 0 when none
  bcapi.py remove APP_ID     uninstall and unpublish every row for APP_ID, wait until gone
  bcapi.py devpublish FILE   reference upload through /dev/apps (the route bcbd.py uses)
"""
import json, os, sys, time, urllib.request, urllib.error, uuid

TENANT = "1b7a9471-d313-4aa4-99e8-7a6c762c8e7d"
ENVN = "Sandbox"
COMPANY = "CRONUS UK Ltd."
BASE = f"https://api.businesscentral.dynamics.com/v2.0/{TENANT}/{ENVN}/api/microsoft/automation/v2.0"


def req(method, url, data=None):
    tok = os.environ.get("BC_ACCESS_TOKEN")
    if not tok:
        sys.exit("BC_ACCESS_TOKEN not set")
    h = {"Authorization": "Bearer " + tok, "Accept": "application/json", "Content-Type": "application/json"}
    r = urllib.request.Request(url, data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(r, timeout=120) as resp:
            b = resp.read()
            return resp.status, (json.loads(b) if b else {})
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:500]


def company():
    s, d = req("GET", BASE + "/companies")
    if s >= 300 or not isinstance(d, dict):
        sys.exit(f"companies lookup failed {s}")
    for c in d["value"]:
        if c["name"].lower() == COMPANY.lower():
            return f"{BASE}/companies({c['id']})"
    sys.exit("company not found")


def rows(co, app_id):
    s, d = req("GET", f"{co}/extensions?$filter=id%20eq%20{app_id}")
    if s >= 300 or not isinstance(d, dict):
        sys.exit(f"extensions query failed {s} {d}")
    return d["value"]


def line(e):
    v = f"{e['versionMajor']}.{e['versionMinor']}.{e['versionBuild']}.{e['versionRevision']}"
    return f"{e['displayName']} {v} installed={e['isInstalled']} publishedAs={(e.get('publishedAs') or '').strip()} package={e['packageId']}"


def wait(co, app_id, pred, what):
    for _ in range(30):
        if pred(rows(co, app_id)):
            return True
        time.sleep(5)
    print(f"timeout waiting for {what}")
    return False


def devpublish(path):
    name = os.path.basename(path)
    b = uuid.uuid4().hex
    body = (f'--{b}\r\nContent-Disposition: form-data; name="file"; filename="{name}"\r\n'
            f'Content-Type: application/octet-stream\r\n\r\n').encode() + open(path, "rb").read() + f"\r\n--{b}--\r\n".encode()
    tok = os.environ.get("BC_ACCESS_TOKEN") or sys.exit("BC_ACCESS_TOKEN not set")
    url = f"https://api.businesscentral.dynamics.com/v2.0/{TENANT}/{ENVN}/dev/apps?SchemaUpdateMode=synchronize&DependencyPublishingOption=default"
    r = urllib.request.Request(url, data=body, method="POST", headers={
        "Authorization": "Bearer " + tok, "Content-Type": f"multipart/form-data; boundary={b}"})
    try:
        with urllib.request.urlopen(r, timeout=600) as resp:
            print(f"devpublish -> {resp.status}")
            return 0
    except urllib.error.HTTPError as e:
        print(f"devpublish -> {e.code} {e.read().decode()[:300]}")
        return 1


def main():
    if sys.argv[1] == "devpublish":
        return devpublish(sys.argv[2])
    cmd, app_id = sys.argv[1], sys.argv[2]
    co = company()
    rs = rows(co, app_id)
    if cmd == "find":
        for e in rs:
            print(line(e))
        return 0
    if cmd == "remove":
        for e in rs:
            if e["isInstalled"]:
                s, d = req("POST", f"{co}/extensions({e['packageId']})/Microsoft.NAV.uninstall", b"{}")
                print(f"uninstall {e['packageId']} -> {s}")
        if any(e["isInstalled"] for e in rs):
            if not wait(co, app_id, lambda r: not any(x["isInstalled"] for x in r), "uninstall"):
                return 1
        for e in rows(co, app_id):
            s, d = req("POST", f"{co}/extensions({e['packageId']})/Microsoft.NAV.unpublish", b"{}")
            print(f"unpublish {e['packageId']} -> {s}")
        if rs and not wait(co, app_id, lambda r: not r, "unpublish"):
            return 1
        left = rows(co, app_id)
        print(f"remaining rows: {len(left)}")
        return 0 if not left else 1
    sys.exit("unknown command")


sys.exit(main())
