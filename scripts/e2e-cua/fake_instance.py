#!/usr/bin/env python3
"""Fake Magic Carpet instance for the fault scenarios. Runs inside the `mc`
container on 127.0.0.1:8787 (stdlib only).

The mode lives in $E2E_DIR/mode.json and is re-read on every request:
  {"create": ok|hang|502|400|401once|401always, "hang_secs": 45,
   "scan": ok|fail, "dup_coordinate": "<39998:pubkey:dtag or empty>"}
Writing the mode file resets the create counter. Every request is appended to
$E2E_DIR/server.log; the suite reads that log for its request checks."""
import json, os, time, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

DIR = os.environ.get("E2E_DIR", "/tmp/e2e")
MODE = os.path.join(DIR, "mode.json")
LOG = os.path.join(DIR, "server.log")
state = {"mtime": None, "creates": 0, "made": {}}   # made: bounties created here, by id


def mode():
    m = {"create": "ok", "hang_secs": 45, "scan": "ok", "dup_coordinate": ""}
    try:
        st = os.stat(MODE)
        if st.st_mtime != state["mtime"]:
            state["mtime"] = st.st_mtime
            state["creates"] = 0
        m.update(json.load(open(MODE)))
    except (FileNotFoundError, ValueError):
        pass
    return m


def log(line):
    with open(LOG, "a") as f:
        f.write("%.3f %s\n" % (time.time(), line))


def bounty(bid, coord, status="open", criteria="e2e"):
    parts = coord.split(":")
    return {"id": bid, "issuer_pubkey": parts[1] if len(parts) >= 3 else "",
            "list_coordinate": coord, "amount_sats": 100, "criteria": criteria,
            "created_at": int(time.time()), "status": status, "derivedStatus": status}


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, code, obj, headers=()):
        body = obj if isinstance(obj, bytes) else json.dumps(obj).encode()
        try:
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            for k, v in headers:
                self.send_header(k, v)
            self.end_headers()
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            log("   (client went away)")

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        try:
            return json.loads(raw or b"{}")
        except ValueError:
            return {}

    def do_GET(self):
        m = mode()
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if u.path == "/api/bounties":
            bs = [bounty("dup00000-0000-0000-0000-000000000000", m["dup_coordinate"])] if m["dup_coordinate"] else []
            bs += list(state["made"].values())
            log("GET /api/bounties issuer=%s -> %d bounties" % (q.get("issuer", ["?"])[0][:8], len(bs)))
            return self.send(200, {"success": True, "bounties": bs})
        if u.path.startswith("/api/bounties/"):
            bid = u.path.rsplit("/", 1)[1]
            log("GET /api/bounties/%s" % bid)
            made = state["made"].get(bid) or bounty(bid, m["dup_coordinate"] or "39998:00:x")
            return self.send(200, {"success": True, "bounty": made, "claims": []})
        if u.path == "/api/strfry/scan":
            f = q.get("filter", [""])[0]
            if m["scan"] == "fail":
                log("GET /api/strfry/scan %s -> 500" % f[:40])
                return self.send(500, {"error": "scan down (e2e)"})
            log("GET /api/strfry/scan %s -> 200 []" % f[:40])
            return self.send(200, {"success": True, "events": []})
        log("GET %s -> 404" % u.path)
        self.send(404, {"error": "not found"})

    def made(self, coord, b):
        """A created bounty, remembered so list and detail reads agree."""
        new = bounty(str(uuid.uuid4()), coord, criteria=b.get("criteria") or "e2e")
        state["made"][new["id"]] = new
        return new

    def do_POST(self):
        m = mode()
        u = urlparse(self.path)
        b = self.body()
        if u.path == "/api/auth/verify-user":
            log("POST verify-user")
            return self.send(200, {"authorized": True, "challenge": uuid.uuid4().hex})
        if u.path == "/api/auth/login-user":
            log("POST login-user")
            return self.send(200, {"success": True}, [("Set-Cookie", "sid=%s; Path=/" % uuid.uuid4().hex)])
        if u.path == "/api/strfry/publish":
            log("POST strfry/publish kind=%s" % b.get("event", {}).get("kind"))
            return self.send(200, {"success": True})
        if u.path == "/api/bounties":
            state["creates"] += 1
            n, c = state["creates"], m["create"]
            coord = b.get("listCoordinate", "")
            log("POST /api/bounties #%d mode=%s coord=%s" % (n, c, coord.rsplit(":", 1)[-1]))
            if c == "hang":
                time.sleep(float(m["hang_secs"]))
                log("   hang over for #%d" % n)
                return self.send(200, {"success": True, "bounty": self.made(coord, b)})
            if c == "502":
                return self.send(502, b"<html>Bad Gateway</html>")
            if c == "400":
                return self.send(400, {"error": "amountSats must be positive (e2e)"})
            if c == "401always" or (c == "401once" and n == 1):
                return self.send(401, {"error": "not logged in (e2e)"})
            return self.send(200, {"success": True, "bounty": self.made(coord, b)})
        log("POST %s -> 404" % u.path)
        self.send(404, {"error": "not found"})


if __name__ == "__main__":
    os.makedirs(DIR, exist_ok=True)
    ThreadingHTTPServer(("127.0.0.1", int(os.environ.get("E2E_PORT", "8787"))), H).serve_forever()
