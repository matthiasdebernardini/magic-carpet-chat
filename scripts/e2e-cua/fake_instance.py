#!/usr/bin/env python3
"""Fake Magic Carpet instance for the fault scenarios. Runs inside the `mc`
container on 127.0.0.1:8787 (stdlib only).

The mode lives in $E2E_DIR/mode.json and is re-read on every request:
  {"create": ok|hang|502|400|401once|401always, "hang_secs": 45,
   "scan": ok|fail, "dup_coordinate": "<39998:pubkey:dtag or empty>",
   "graperank": "ok"|"429"|"403"|"503"|[answers consumed one per call],
   "setup": "ok"|"404", "delay_secs": 0,
   "eligible": {"<viewer hex>": [[id, coord, issuerRank, auto_pay_min_rank|null, criteria], ...]},
   "eligible_delay": {"<viewer hex>": secs}}
GET /api/bounties/eligible needs the sid cookie login-user set: 401 without
one, 403 when ?viewer= is not the session pubkey, like
magic-carpet-v2/src/api/bounties.js:294-320. A null auto_pay_min_rank is a
manual-pay row. GET /api/bounties/<id> answers an id the eligible route
served with that row, even after the mode drops it.
Writing the mode file resets the create counter. Every request is appended to
$E2E_DIR/server.log; the suite reads that log for its request checks.

The same process also serves a fake Brainstorm on 127.0.0.1:$BS_PORT (8788):
  GET  /authChallenge/{hex}            -> {"code":200,"data":{"challenge":"<hex32>"}}
  POST /authChallenge/{hex}/verify     -> {"data":{"token":"tok-<uuid>"}} (token never logged)
  POST /user/followList                -> 200, requires Bearer tok-…
  POST /user/graperank                 -> mode "graperank" answer
  GET  /setup/{hex}                    -> mode "setup": six rows like the live body, or 404
  GET  /search/byText                  -> canned data.results profiles filtered by text"""
import json, os, re, threading, time, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

DIR = os.environ.get("E2E_DIR", "/tmp/e2e")
MODE = os.path.join(DIR, "mode.json")
LOG = os.path.join(DIR, "server.log")
state = {"mtime": None, "creates": 0, "made": {}, "grape": [],   # made: bounties created here, by id
         "eligible_seen": {}}   # eligible rows served, by id, without issuerRank (the detail route has none)
SESSIONS = {}   # sid cookie -> the pubkey that logged in with it

BS_PORT = int(os.environ.get("BS_PORT", "8788"))
BS_PROVIDER = "c63c30b4e2f7966c3e329149dbdb5d5e0898b5cb4ddf1f97ed5011fe5b91c025"
BS_RELAY = os.environ.get("BS_RELAY", "ws://127.0.0.1:10547")
BS_CHALLENGES = {}   # hex -> challenge, kept only until verify


def mode():
    m = {"create": "ok", "hang_secs": 45, "scan": "ok", "dup_coordinate": "",
         "graperank": "ok", "setup": "ok", "delay_secs": 0,
         "eligible": {}, "eligible_delay": {}}
    try:
        st = os.stat(MODE)
        if st.st_mtime != state["mtime"]:
            state["mtime"] = st.st_mtime
            state["creates"] = 0
            loaded = json.load(open(MODE))
            state["grape"] = list(loaded["graperank"]) if isinstance(loaded.get("graperank"), list) else []
        m.update(json.load(open(MODE)))
    except (FileNotFoundError, ValueError):
        pass
    return m


def log(line):
    with open(LOG, "a") as f:
        f.write("%.3f %s\n" % (time.time(), line))


def bounty(bid, coord, status="open", criteria="e2e", issuerRank=None, auto_pay=False, auto_pay_min_rank=None):
    parts = coord.split(":")
    b = {"id": bid, "issuer_pubkey": parts[1] if len(parts) >= 3 else "",
         "list_coordinate": coord, "amount_sats": 100, "criteria": criteria,
         "created_at": int(time.time()), "status": status, "derivedStatus": status}
    if issuerRank is not None:
        b["issuerRank"] = issuerRank
    if auto_pay:
        b["auto_pay"] = 1
    if auto_pay_min_rank is not None:
        b["auto_pay_min_rank"] = auto_pay_min_rank
    return b


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
        # Before the "/api/bounties/<id>" branch, like the server's route order.
        if u.path == "/api/bounties/eligible":
            return self.eligible(m, q)
        if u.path.startswith("/api/bounties/"):
            bid = u.path.rsplit("/", 1)[1]
            log("GET /api/bounties/%s" % bid)
            # An id the eligible route served answers with its own coordinate
            # and issuer, even after the mode drops it from the list.
            made = (state["made"].get(bid) or state["eligible_seen"].get(bid)
                    or bounty(bid, m["dup_coordinate"] or "39998:00:x"))
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

    def session(self):
        """The pubkey behind this request's sid cookie, or None."""
        sid = re.search(r"(?:^|;\s*)sid=([0-9a-f]+)", self.headers.get("Cookie") or "")
        return SESSIONS.get(sid.group(1)) if sid else None

    def eligible(self, m, q):
        who = self.session()
        viewer = (q.get("viewer", [who or ""])[0] or "").lower()
        if not who:
            log("GET /api/bounties/eligible viewer=%s session=none -> 401" % viewer[:8])
            return self.send(401, {"success": False, "error": "Not authenticated"})
        if not re.fullmatch(r"[0-9a-f]{64}", viewer):
            log("GET /api/bounties/eligible viewer=%s session=%s -> 400" % (viewer[:8], who[:8]))
            return self.send(400, {"success": False, "error": "viewer must be a 64-char hex pubkey"})
        if viewer != who:
            log("GET /api/bounties/eligible viewer=%s session=%s -> 403" % (viewer[:8], who[:8]))
            return self.send(403, {"success": False, "error": "viewer must match session pubkey"})
        rows = (m.get("eligible") or {}).get(viewer, [])
        bs = [bounty(bid, coord, criteria=crit, issuerRank=rank, auto_pay=apm is not None, auto_pay_min_rank=apm)
              for bid, coord, rank, apm, crit in rows]
        for b in bs:
            state["eligible_seen"][b["id"]] = {k: v for k, v in b.items() if k != "issuerRank"}
        log("GET /api/bounties/eligible viewer=%s session=%s -> %d" % (viewer[:8], who[:8], len(bs)))
        d = float((m.get("eligible_delay") or {}).get(viewer) or 0)
        if d > 0:
            time.sleep(d)
            log("   eligible reply for %s after %.0f s" % (viewer[:8], d))
        return self.send(200, {"success": True, "bounties": bs})

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
            pubkey = ((b.get("event") or {}).get("pubkey") or "").lower()
            sid = uuid.uuid4().hex
            SESSIONS[sid] = pubkey
            log("POST login-user pubkey=%s" % pubkey[:8])
            return self.send(200, {"success": True}, [("Set-Cookie", "sid=%s; Path=/" % sid)])
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


# ---------------------------------------------------------------- fake Brainstorm
BS_SEARCH = [
    {"pubkey": "a" * 64, "name": "alice", "display_name": "Alice E2E",
     "nip05": "alice@e2e.test", "picture": ""},
    {"pubkey": "b" * 64, "name": "bob", "display_name": "Bob E2E",
     "nip05": "bob@e2e.test", "picture": ""},
    {"pubkey": "c" * 64, "name": "carol", "display_name": "Carol E2E",
     "nip05": "carol@e2e.test", "picture": ""},
]


def bs_setup_rows():
    return [["30382:rank", BS_PROVIDER, BS_RELAY],
            ["30382:followers", BS_PROVIDER, BS_RELAY],
            ["30382:reporters", BS_PROVIDER, BS_RELAY],
            ["30382:muters", BS_PROVIDER, BS_RELAY],
            ["30382:hops", BS_PROVIDER, BS_RELAY],
            ["30392", BS_PROVIDER, BS_RELAY]]


class B(BaseHTTPRequestHandler):
    """Fake Brainstorm API on its own port; stdlib only; tokens never logged."""

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
            log("   (brainstorm client went away)")

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        try:
            return json.loads(raw or b"{}")
        except ValueError:
            return {}

    def authed(self):
        return (self.headers.get("Authorization") or "").startswith("Bearer tok-")

    def delay(self, m):
        d = float(m.get("delay_secs") or 0)
        if d > 0:
            time.sleep(d)

    def do_GET(self):
        m = mode()
        u = urlparse(self.path)
        q = parse_qs(u.query)
        hm = re.fullmatch(r"/authChallenge/([0-9a-f]{64})", u.path)
        if hm:
            ch = uuid.uuid4().hex
            BS_CHALLENGES[hm.group(1)] = ch
            log("BS GET /authChallenge/%s" % hm.group(1)[:8])
            return self.send(200, {"code": 200, "data": {"challenge": ch}})
        sm = re.fullmatch(r"/setup/([0-9a-f]{64})", u.path)
        if sm:
            self.delay(m)
            log("BS GET /setup/%s mode=%s" % (sm.group(1)[:8], m["setup"]))
            if m["setup"] == "404":
                return self.send(404, {"error": "no setup data"})
            return self.send(200, bs_setup_rows())
        if u.path == "/search/byText":
            text = (q.get("text") or q.get("q") or [""])[0].lower()
            hits = [p for p in BS_SEARCH if text in p["name"] or text in p["display_name"].lower()]
            log("BS GET /search/byText %r -> %d" % (text[:20], len(hits)))
            return self.send(200, {"code": 200, "data": {"results": hits}})
        log("BS GET %s -> 404" % u.path)
        self.send(404, {"error": "not found"})

    def do_POST(self):
        m = mode()
        u = urlparse(self.path)
        b = self.body()
        vm = re.fullmatch(r"/authChallenge/([0-9a-f]{64})/verify", u.path)
        if vm:
            ev = b.get("signed_event") or b
            tags = ev.get("tags") or []
            ok = (ev.get("kind") == 22242 and ev.get("pubkey") == vm.group(1)
                  and ["t", "brainstorm_login"] in tags
                  and any(t[0] == "challenge" and len(t) > 1 and t[1] == BS_CHALLENGES.get(vm.group(1))
                          for t in tags if isinstance(t, list) and t))
            log("BS POST verify %s -> %s" % (vm.group(1)[:8], "token" if ok else "401"))
            if ok:
                return self.send(200, {"data": {"token": "tok-%s" % uuid.uuid4()}})
            return self.send(401, {"error": "bad login event"})
        if u.path == "/user/followList":
            ev = b.get("signed_event") or b.get("event") or b
            ptags = sum(1 for t in (ev.get("tags") or []) if isinstance(t, list) and t and t[0] == "p")
            log("BS POST /user/followList auth=%s kind=%s p=%d" % (self.authed(), ev.get("kind"), ptags))
            if not self.authed():
                return self.send(401, {"error": "unauthorized"})
            return self.send(200, {"success": True})
        if u.path == "/user/graperank":
            self.delay(m)
            g = state["grape"].pop(0) if state["grape"] else m["graperank"]
            if isinstance(g, list):
                g = "ok"
            log("BS POST /user/graperank auth=%s -> %s" % (self.authed(), g))
            if not self.authed():
                return self.send(401, {"error": "unauthorized"})
            if g == "ok":
                return self.send(200, {"success": True})
            if g == "403":
                return self.send(403, {"detail": "Last GrapeRank run was too recent"})
            return self.send(int(g), {"error": "graperank %s" % g})
        log("BS POST %s -> 404" % u.path)
        self.send(404, {"error": "not found"})


if __name__ == "__main__":
    os.makedirs(DIR, exist_ok=True)
    threading.Thread(target=lambda: ThreadingHTTPServer(("127.0.0.1", BS_PORT), B).serve_forever(),
                     daemon=True).start()
    ThreadingHTTPServer(("127.0.0.1", int(os.environ.get("E2E_PORT", "8787"))), H).serve_forever()
