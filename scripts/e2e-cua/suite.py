#!/usr/bin/env python3
"""magic-carpet-chat end-to-end suite. Runs on the magic-carpet-cua box host,
drives the app inside the `mc` container, and writes one report per run.

    ~/mc/.venv/bin/python ~/mc/e2e/suite.py --run-id 20260923-1400 [--no-paid] [--only A,B] [--mock-jev]

Every UI step goes through the agent.py pattern: the scenario owns a candidate
table, Jev picks one id, Jev judges the postcondition from the OCR. On top of
Jev, each scenario makes hard checks (literal OCR phrases, fake-server log
lines, the clipboard, the float) that decide PASS or FAIL.

Secrets: keys.json is read here and handed to the app only through the docker
client's environment (`docker exec -e MC_NSECS`, no value on the command
line). Nothing secret is printed, logged or written to the report.
"""
import argparse, asyncio, dataclasses, json, os, re, shlex, subprocess, sys, time, traceback

HOME = os.path.expanduser("~")
MC = os.path.join(HOME, "mc")
E2E = os.path.dirname(os.path.abspath(__file__))
KEYS = os.path.join(MC, "keys.json")
CDIR = "/tmp/e2e"                       # work dir inside the mc container
APP_BIN = "/usr/local/bin/magic-carpet-chat-e2e"   # run.sh installs here; the demo's binary is left alone
WALLET = "node /usr/local/lib/node_modules/brainstorm/node_modules/.bin/agent-wallet"
FAKE_URL, RELAY_URL = "http://127.0.0.1:8787", "ws://127.0.0.1:10547"

sys.path.insert(0, MC)
sys.path.insert(0, E2E)
import agent  # noqa: E402  (agent.py opens a runs/ log on import; see _quiet_agent_log)
from agent import Step, Cand, Driver, Jev, DONE, REOBSERVE, ABSTAIN  # noqa: E402

NSEC_RE = re.compile(r"nsec1[02-9ac-hj-np-z]{20,}")


def redact(text, secrets=()):
    """Evidence shows at most the first 8 characters of an nsec."""
    text = NSEC_RE.sub(lambda m: m.group(0)[:8] + "…[redacted]", text)
    for s in secrets:
        if s:
            text = text.replace(s, s[:4] + "…[redacted]")
    return text


def norm(s):
    s = s.replace("’", "'").replace("‘", "'").replace("…", "...")
    return " ".join(s.lower().split())


def has(screen, phrase):
    return norm(phrase) in norm(screen)


def count(screen, phrase):
    return norm(screen).count(norm(phrase))


def load_env(path):
    """KEY=VALUE lines into os.environ; values never echoed."""
    if not os.path.exists(path):
        return
    for line in open(path):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


def run(cmd, input=None, env=None, check=False, timeout=120):
    return subprocess.run(cmd, input=input, capture_output=True, text=True,
                          env=env, check=check, timeout=timeout)


def dx(script, input=None, env_names=(), env=None, detach=False, timeout=120):
    """sh -c inside the mc container. env_names are passed by name only, so
    their values come from `env` and never appear on a command line."""
    cmd = ["docker", "exec"] + (["-d"] if detach else []) + (["-i"] if input is not None else [])
    for name in env_names:
        cmd += ["-e", name]
    cmd += ["mc", "sh", "-c", script]
    return run(cmd, input=input, env=env, timeout=timeout)


SIDEBAR = (70, 95, 290, 240)   # account chip: name, npub, address, remove link, sats warning
CONTENT = (70, 90, 1280, 960)  # the window below its title bars


def fine_lines(data, box=SIDEBAR, scale=4, thr=60):
    """OCR for the 10 px sidebar text the 2x full-screen pass misses: crop,
    upscale 4x, binarize (dark text on white). Returns [(line, cx, cy)] in
    screen coordinates, in reading order."""
    import io
    from PIL import Image
    im = Image.open(io.BytesIO(data)).convert("L").crop(box)
    im = im.resize((im.width * scale, im.height * scale), Image.LANCZOS).point(lambda v: 0 if v > thr else 255)
    im.save("/tmp/e2e-fine.png")
    tsv = subprocess.run(["tesseract", "/tmp/e2e-fine.png", "-", "--psm", "6", "tsv"],
                         capture_output=True, text=True).stdout
    rows = {}
    for ln in tsv.splitlines()[1:]:
        p = ln.split("\t")
        if len(p) < 12 or not p[11].strip():
            continue
        rows.setdefault((p[2], p[3], p[4]), []).append(
            (p[11], float(p[6]) + float(p[8]) / 2, float(p[7]) + float(p[9]) / 2))
    out = []
    for ws in sorted(rows.values(), key=lambda ws: min(w[2] for w in ws)):
        out.append((" ".join(w[0] for w in ws), box[0] + sum(w[1] for w in ws) / len(ws) / scale,
                    box[1] + sum(w[2] for w in ws) / len(ws) / scale))
    return out


def shot_bytes(img):
    return img if isinstance(img, (bytes, bytearray)) else getattr(img, "data", None) or img


class FineDriver(Driver):
    """agent.Driver plus `click_fine`: click a sidebar phrase found by fine_lines."""

    async def act(self, kind, arg):
        if kind == "stamp":     # remember when this point of the action ran
            self.scenario.stamps[arg] = time.time()
            return None
        if kind == "snap":      # a raw evidence frame between actions, no OCR
            return await self.scenario.snap(arg)
        if kind != "click_fine":
            return await super().act(kind, arg)
        text = arg["text"] if isinstance(arg, dict) else arg
        box = tuple(arg.get("box", SIDEBAR)) if isinstance(arg, dict) else SIDEBAR
        lines = fine_lines(shot_bytes(await self.sb.screenshot()), box)
        hit = next(((x, y) for line, x, y in lines if norm(text) in norm(line)), None)
        if hit is None:
            raise RuntimeError(f"click_fine: {text!r} not in the sidebar; saw {[l for l, _, _ in lines]}")
        await self.sb.mouse.click(*hit)
        return f"click_fine {text!r} -> ({hit[0]:.0f},{hit[1]:.0f})"


class Box:
    """Container-side plumbing: fake server, relay, app, recording, clipboard."""

    def __init__(self, keys):
        self.keys = keys

    def setup(self):
        dx(f"mkdir -p {CDIR}/rec {CDIR}/logs && chmod 700 {CDIR}")
        run(["docker", "cp", os.path.join(E2E, "fake_instance.py"), f"mc:{CDIR}/fake_instance.py"], check=True)
        if dx(f"test -x {CDIR}/nak").returncode != 0:
            run(["docker", "cp", os.path.join(HOME, ".local/bin/nak"), f"mc:{CDIR}/nak"], check=True)

    def app_version(self):
        r = dx(f"strings {APP_BIN} | grep -m1 -oE 'magic-carpet-chat/[0-9]+\\.[0-9]+\\.[0-9]+(-rc[0-9]+)?'; sha256sum {APP_BIN} | cut -c1-16")
        return " ".join(r.stdout.split())

    # --- fake instance and relay (fresh per scenario) ---------------------
    def start_fake(self, mode):
        self.stop_fake()
        self.set_mode(mode)
        dx(f": > {CDIR}/server.log")
        dx(f"E2E_DIR={CDIR} exec python3 {CDIR}/fake_instance.py > {CDIR}/fake.out 2>&1", detach=True)
        dx(f"exec {CDIR}/nak serve --hostname 127.0.0.1 --port 10547 > {CDIR}/relay.log 2>&1", detach=True)
        for _ in range(40):
            ok = dx(f"curl -s -o /dev/null -w '%{{http_code}}' {FAKE_URL}/api/bounties").stdout.strip() == "200"
            if ok:
                return
            time.sleep(0.25)
        raise RuntimeError("the fake instance did not come up")

    def stop_fake(self):
        dx("pkill -f fake_instance.py; pkill -f 'nak serve'; true")

    def set_mode(self, mode):
        full = {"create": "ok", "hang_secs": 45, "scan": "ok", "dup_coordinate": ""}
        full.update(mode)
        # write-then-rename so a request never reads half a file
        dx(f"cat > {CDIR}/mode.tmp && mv {CDIR}/mode.tmp {CDIR}/mode.json", input=json.dumps(full))

    def mark(self, text):
        dx(f"echo \"$(date +%s.%N | cut -c1-14) === {text}\" >> {CDIR}/server.log")

    def server_log(self):
        return dx(f"cat {CDIR}/server.log").stdout

    # --- the app ----------------------------------------------------------
    def kill_app(self):
        dx("pkill -x magic-carpet-ch; true")
        for _ in range(20):
            if dx("pgrep -x magic-carpet-ch").returncode != 0:
                return
            time.sleep(0.25)
        dx("pkill -9 -x magic-carpet-ch; true")

    def app_alive(self):
        return dx("pgrep -x magic-carpet-ch").returncode == 0

    def launch(self, name, run_id, target="fake", seed=False):
        """Relaunch with a fresh MC_STORE_DIR. seed copies the demo store
        (/root/mc-store/accounts.json, which holds the claimant's Coinos
        login) into it, container-internal, never read here."""
        self.kill_app()
        store = f"/root/e2e-stores/{run_id}/{name}"
        dx(f"mkdir -p {shlex.quote(store)} && chmod 700 /root/e2e-stores {shlex.quote(store)}")
        if seed:
            dx(f"cp /root/mc-store/accounts.json {store}/ && chmod 600 {store}/accounts.json")
        base, relay = (FAKE_URL, RELAY_URL) if target == "fake" else ("http://tapestry", "ws://tapestry/relay")
        env = dict(os.environ)
        env.update({
            "MC_NSECS": f"{self.keys['house']['sec']},{self.keys['claimant']['sec']}",
            "MC_ISSUER_NPUB": self.keys["house"]["npub"], "MC_BASE_URL": base, "MC_RELAY_URL": relay,
            "MC_STORE_DIR": store, "MC_SENTRY_DSN": "", "DISPLAY": ":1", "RUST_BACKTRACE": "1",
        })
        env.pop("ANTHROPIC_API_KEY", None)
        dx(f"exec {APP_BIN} > {CDIR}/logs/{name}.app.log 2>&1", detach=True,
           env_names=("MC_NSECS", "MC_ISSUER_NPUB", "MC_BASE_URL", "MC_RELAY_URL", "MC_STORE_DIR",
                      "MC_SENTRY_DSN", "DISPLAY", "RUST_BACKTRACE"), env=env)
        wid = ""
        for _ in range(60):
            wid = dx("DISPLAY=:1 xdotool search --name 'Magic Carpet' | head -1").stdout.strip()
            if wid:
                break
            time.sleep(0.5)
        if not wid:
            raise RuntimeError("the app window never appeared; see the app log")
        dx(f"DISPLAY=:1 xdotool windowsize {wid} 1280 960 windowmove {wid} 0 0 windowactivate {wid} mousemove 20 740")
        time.sleep(6)   # first bounty list, profile scans, relay connect
        return store

    def accounts_in_store(self, store):
        r = dx(f"jq '.accounts | length' {store}/accounts.json")
        try:
            return int(r.stdout.strip())
        except ValueError:
            return -1

    # --- recording --------------------------------------------------------
    def rec_start(self, name):
        dx(f"exec ffmpeg -y -loglevel error -f x11grab -video_size 1280x960 -framerate 10 -i :1 "
           f"-c:v libx264 -preset ultrafast -crf 30 -pix_fmt yuv420p {CDIR}/rec/{name}.mp4 "
           f"> {CDIR}/rec/{name}.ffmpeg.log 2>&1", detach=True)

    def rec_stop(self, name):
        dx(f"pkill -INT -f 'rec/{name}.mp4'; true")
        for _ in range(40):
            if dx(f"pgrep -f 'rec/{name}.mp4'").returncode != 0:
                return
            time.sleep(0.25)

    # --- clipboard (G) ----------------------------------------------------
    def clipboard(self):
        """The X11 CLIPBOARD selection. Stays in memory; callers only derive
        booleans and an 8-character prefix from it."""
        return dx("DISPLAY=:1 timeout 5 xclip -o -selection clipboard 2>/dev/null").stdout

    # --- the float (P) ----------------------------------------------------
    def float_sats(self):
        r = run(["docker", "exec", "tapestry", "sh", "-c", WALLET + " balance"], timeout=60)
        try:
            return int(json.loads(r.stdout.strip().splitlines()[-1])["balance_sats"])
        except (ValueError, KeyError, IndexError):
            return None


class Scenario:
    """One scenario's context: its Jev loop, frames, checks and verdicts."""

    def __init__(self, suite, sid, title):
        self.suite, self.box, self.sid, self.title = suite, suite.box, sid, title
        self.dir = os.path.join(suite.out, sid)
        os.makedirs(os.path.join(self.dir, "frames"), exist_ok=True)
        self.steps, self.checks, self.frame_n, self.screen = [], [], 0, ""
        self.hist, self.skip_reason, self.store, self.notes = [], None, None, []
        self.seen_at, self.stamps = 0.0, {}

    # --- observe ----------------------------------------------------------
    async def snap(self, label):
        data = shot_bytes(await self.suite.sb.screenshot())
        self.frame_n += 1
        stem = f"{self.frame_n:02d}-{re.sub(r'[^a-z0-9]+', '-', label.lower())[:40]}"
        open(os.path.join(self.dir, "frames", stem + ".png"), "wb").write(data)
        return data, stem

    async def stable(self, max_wait=15.0, gap=0.8, tolerance=400):
        """Wait until two captures `gap` apart differ in fewer than
        `tolerance` pixels (a blinking caret is ~50). The app renders in
        software on a 2-CPU box shared with ffmpeg and tesseract, so a
        repaint can trail the input by seconds."""
        import io
        from PIL import Image, ImageChops
        grab = lambda b: Image.open(io.BytesIO(b)).convert("L").point(lambda v: 255 if v > 40 else 0)
        prev = grab(shot_bytes(await self.suite.sb.screenshot()))
        end = time.time() + max_wait
        while time.time() < end:
            await asyncio.sleep(gap)
            cur = grab(shot_bytes(await self.suite.sb.screenshot()))
            if sum(ImageChops.difference(prev, cur).histogram()[1:]) < tolerance:
                return True
            prev = cur
        return False

    async def see(self, label="look"):
        """Screenshot, then three OCR passes: agent.ocr (2x, as the demos) and
        a binarized 3x pass for the dim 10-12 px text. The capture happens
        first, so the OCR time never delays what the frame shows."""
        data, stem = await self.snap(label)
        self.seen_at = time.time()
        fine = "\n".join(l for l, _, _ in fine_lines(data, CONTENT, scale=3, thr=50))
        side = "\n".join(l for l, _, _ in fine_lines(data, SIDEBAR, scale=4, thr=60))
        text = redact(agent.ocr(data) + "\n[fine]\n" + fine + "\n[sidebar]\n" + side, self.suite.secrets)
        open(os.path.join(self.dir, "frames", stem + ".txt"), "w").write(text)
        self.screen = text
        return text

    # --- one agent.py-style step -------------------------------------------
    async def step(self, s: Step, must_act=False, label=None, min_conf=0.4, second_look=True):
        """Mirror of agent.run for one Step. must_act drops the 'already-done'
        candidate, for steps whose goal state is already on screen before the
        action (the refusals): the action has to happen to test anything."""
        label = label or s.goal[:40]
        cands = s.cands + ([] if must_act else [DONE]) + [REOBSERVE, ABSTAIN]
        rec = {"step": label, "goal": s.goal, "cond": s.done, "picks": [], "p": None, "met": False}
        t0 = time.time()
        for attempt in range(s.tries):
            # A must_act step reuses a fresh previous observation as its
            # "before": timed scenarios (A) cannot afford a second OCR.
            if must_act and self.screen and time.time() - self.seen_at < 20:
                screen = self.screen
            else:
                screen = await self.see(f"{label}-before")
            pick, conf, probs = await self.suite.jev_call("choose", s.goal, screen, cands, self.hist, s.cues)
            c = next((c for c in cands if c.id == pick), None)
            rec["picks"].append(pick)
            if c is not None and c.id == "reobserve":
                await asyncio.sleep(3)
                continue
            p_pick, p_abst = probs.get(pick, conf), probs.get("abstain", 0.0)
            weak = c is not None and c.id != "already-done" and (p_pick < min_conf or p_abst >= 0.3)
            if c is None or c.id == "abstain" or weak:
                rec["halt"] = "unknown id" if c is None else ("weak " + c.id if weak else c.id)
                break
            for kind, arg in c.do:
                try:
                    await self.suite.driver.act(kind, arg)
                except Exception as e:   # e.g. click_text found nothing: the attempt fails, the loop goes on
                    rec.setdefault("act_errors", []).append(redact(str(e), self.suite.secrets)[:300])
                    break
            await asyncio.sleep(s.settle)
            await self.stable()
            screen = await self.see(f"{label}-after")
            p = await self.suite.jev_call("done", s.done, screen, s.cues)
            if p < 0.6 and second_look:
                # A second look before the step counts as unmet: the render
                # can trail, and a borderline verdict should not redo an
                # action (a second Enter is a second bounty or claim).
                await asyncio.sleep(3)
                await self.stable()
                screen = await self.see(f"{label}-second-look")
                p = max(p, await self.suite.jev_call("done", s.done, screen, s.cues))
            rec["p"] = round(float(p), 3)
            self.hist.append({"step": label, "action": c.id, "done_p": p})
            if p >= 0.6:
                rec["met"] = True
                break
        rec["secs"] = round(time.time() - t0, 1)
        self.steps.append(rec)
        print(f"  [{self.sid}] step {label!r}: picks={rec['picks']} p={rec['p']} met={rec['met']}", flush=True)
        return rec["met"]

    async def steps_(self, steps, **kw):
        for s in steps:
            if not await self.step(s, **kw):
                return False
        return True

    # --- hard checks --------------------------------------------------------
    def check(self, name, ok, detail=""):
        self.checks.append({"check": name, "ok": bool(ok), "detail": redact(str(detail), self.suite.secrets)[:400]})
        shown = redact(str(detail), self.suite.secrets) if not ok else ""
        print(f"  [{self.sid}] check {'ok  ' if ok else 'FAIL'} {name} {shown}"[:300], flush=True)
        return bool(ok)

    def count(self, phrase, screen=None):
        """How often `phrase` shows, taking the better of the two OCR passes
        (they read the same pixels, so their counts must not be summed)."""
        screen = self.screen if screen is None else screen
        return max(count(part, phrase) for part in re.split(r"\n\[(?:fine|sidebar)\]\n", screen))

    def check_screen(self, name, must=(), must_not=(), screen=None):
        screen = self.screen if screen is None else screen
        missing = [p for p in must if not has(screen, p)]
        present = [p for p in must_not if has(screen, p)]
        return self.check(name, not missing and not present,
                          f"missing={missing} unexpected={present}" if missing or present else "")

    async def wait_for(self, must=(), must_not_seen=(), timeout=60, every=3, label="wait"):
        """Poll the screen until every phrase in `must` is on it. Returns the
        last OCR; stops early if a `must_not_seen` phrase shows up."""
        end = time.time() + timeout
        while True:
            screen = await self.see(label)
            if any(has(screen, p) for p in must_not_seen):
                return screen
            if all(has(screen, p) for p in must) or time.time() >= end:
                return screen
            await asyncio.sleep(every)

    def log_after(self, marker):
        """Fake server log lines after `=== marker` (the whole log if absent)."""
        lines = self.box.server_log().splitlines()
        idx = max((i for i, l in enumerate(lines) if l.split(" ", 1)[-1] == f"=== {marker}"), default=-1)
        return [l.split(" ", 1)[-1] for l in lines[idx + 1:]]

    def skip(self, why):
        self.skip_reason = why


class Suite:
    def __init__(self, args):
        self.args = args
        self.run_id = args.run_id
        self.out = os.path.join(E2E, "runs", self.run_id)
        os.makedirs(self.out, exist_ok=True)
        self.keys = json.load(open(KEYS))
        self.secrets = [self.keys[w]["sec"] for w in ("house", "claimant")]
        self.box = Box(self.keys)
        self.results = []

    async def run(self, registry):
        load_env(os.path.join(MC, ".env"))
        if self.args.mock_jev:
            os.environ.pop("TYPESAFE_API_KEY", None)
        self.jev = Jev()
        wanted = [s.strip().upper() for s in self.args.only.split(",")] if self.args.only else list(registry)
        paid = "P" in wanted and not self.args.no_paid
        meta = {"run_id": self.run_id, "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                "jev": "live" if self.jev.live else "mock", "paid": paid}
        self.meta = meta
        if paid:
            bal = self.box.float_sats()
            meta["float_before_suite"] = bal
            if bal is None or bal < 200:
                print(f"PREFLIGHT: the auto-pay float is {bal} sats, below 200. "
                      f"Stopping. fund with ~/mc/fund.sh 500", flush=True)
                meta["stopped"] = "float below 200 sats"
                self.write(meta)
                return 3
        self.box.setup()
        if dx(f"test -x {APP_BIN}").returncode != 0:
            print(f"no e2e binary at {APP_BIN}; install one with run.sh <run-id|path>", flush=True)
            meta["stopped"] = f"no binary at {APP_BIN}"
            self.write(meta)
            return 2
        meta["app"] = self.box.app_version()
        print(f"suite {self.run_id}: app {meta['app']}, jev {meta['jev']}, paid {paid}", flush=True)
        from cua import Sandbox
        self.sb = await Sandbox.connect("mc", http_url=f"http://{agent.IP}:8000")
        self.driver = FineDriver(self.sb)
        try:
            for sid, (title, fn) in registry.items():
                if sid not in wanted:
                    continue
                if sid == "P" and not paid:
                    self.results.append({"scenario": sid, "title": title, "result": "SKIP",
                                         "why": "--no-paid", "duration_s": 0, "evidence": "", "judge": [], "checks": []})
                    continue
                self.results.append(await self.one(sid, title, fn))
                self.write(meta)
        finally:
            await self.sb.disconnect()
            self.box.kill_app()
            self.box.stop_fake()
            # The stores hold copies of keys.json's nsecs. The box rule is
            # "mv to ~/.trash, never delete": into root's 0700 trash they go.
            run_dir = shlex.quote(f"/root/e2e-stores/{self.run_id}")
            dx(f"mkdir -p /root/.trash/e2e-stores && chmod 700 /root/.trash /root/.trash/e2e-stores && "
               f"test -d {run_dir} && mv {run_dir} /root/.trash/e2e-stores/; true")
            if not self.args.no_restore:
                run([os.path.join(MC, "run-app.sh")], timeout=90)   # leave the demo desktop as it was
        meta["finished"] = time.strftime("%Y-%m-%dT%H:%M:%S%z")
        self.write(meta)
        return 0 if all(r["result"] != "FAIL" for r in self.results) else 1

    async def jev_call(self, method, *args):
        """Jev with three tries: a TypeSafe 5xx must not fail a scenario."""
        for attempt in range(3):
            try:
                return getattr(self.jev, method)(*args)
            except Exception as e:
                if attempt == 2:
                    raise
                print(f"  jev {method} failed ({type(e).__name__}); retrying", flush=True)
                await asyncio.sleep(3 * (attempt + 1))

    async def one(self, sid, title, fn):
        sc = Scenario(self, sid, title)
        self.driver.scenario = sc
        print(f"== {sid}: {title}", flush=True)
        t0, err = time.time(), None
        self.box.rec_start(sid)
        try:
            await fn(sc)
        except Exception as e:
            err = redact(f"{type(e).__name__}: {e}", self.secrets)
            print(redact(traceback.format_exc(), self.secrets), flush=True)
        finally:
            try:
                await sc.see("final")
            except Exception:
                pass
            self.box.rec_stop(sid)
            self.collect(sid)
        judged = [s for s in sc.steps]
        steps_ok = all(s["met"] for s in judged)
        checks_ok = all(c["ok"] for c in sc.checks)
        if sc.skip_reason and not err:
            result = "SKIP"
        else:
            result = "PASS" if (steps_ok and checks_ok and not err and sc.checks) else "FAIL"
        r = {"scenario": sid, "title": title, "result": result, "duration_s": round(time.time() - t0, 1),
             "evidence": os.path.relpath(sc.dir, self.out), "recording": f"{sid}/{sid}.mp4",
             "judge": [{"step": s["step"], "cond": s["cond"], "picks": s["picks"], "p": s["p"],
                        "met": s["met"], **({"halt": s["halt"]} if "halt" in s else {}),
                        **({"act_errors": s["act_errors"]} if "act_errors" in s else {})} for s in judged],
             "checks": sc.checks, "notes": sc.notes}
        if err:
            r["error"] = err
        if sc.skip_reason:
            r["why"] = sc.skip_reason
        print(f"== {sid}: {result} in {r['duration_s']} s", flush=True)
        return r

    def collect(self, sid):
        d = os.path.join(self.out, sid)
        for src, dst in ((f"{CDIR}/rec/{sid}.mp4", f"{sid}.mp4"), (f"{CDIR}/logs/{sid}.app.log", "app.log"),
                         (f"{CDIR}/server.log", "fake-server.log")):
            p = os.path.join(d, dst)
            run(["docker", "cp", f"mc:{src}", p])
            if dst.endswith(".log") and os.path.exists(p):
                txt = open(p, errors="replace").read()
                open(p, "w").write(redact(txt, self.secrets))

    def write(self, meta):
        rep = {"meta": meta, "results": self.results}
        open(os.path.join(self.out, "report.json"), "w").write(json.dumps(rep, indent=2))
        rows = ["| Scenario | Result | Duration | Judge (steps met) | Hard checks | Evidence |",
                "|---|---|---|---|---|---|"]
        for r in self.results:
            met = sum(1 for s in r["judge"] if s["met"])
            ok = sum(1 for c in r["checks"] if c["ok"])
            fails = "; ".join(c["check"] for c in r["checks"] if not c["ok"])
            extra = r.get("error") or r.get("why") or (f"failed: {fails}" if fails else "")
            rows.append(f"| {r['scenario']} {r['title']} | **{r['result']}** | {r['duration_s']} s | "
                        f"{met}/{len(r['judge'])} | {ok}/{len(r['checks'])}{' — ' + extra if extra else ''} | "
                        f"{r['evidence']}/ |")
        head = [f"# magic-carpet-chat e2e run {meta['run_id']}", "",
                f"App: `{meta.get('app', '?')}` · Jev: {meta['jev']} · paid scenario: {meta['paid']}"]
        if "float_before" in meta or "float_after" in meta:
            head.append(f"Float: {meta.get('float_before')} sats before, {meta.get('float_after')} sats after")
        if meta.get("stopped"):
            head.append(f"STOPPED: {meta['stopped']} (fund with ~/mc/fund.sh 500)")
        open(os.path.join(self.out, "report.md"), "w").write("\n".join(head + [""] + rows) + "\n")


def _quiet_agent_log():
    """agent.py opened an empty runs/*.jsonl on import; move it out of the
    way so the demo run history only holds demo runs."""
    try:
        name = agent.LOG.name
        agent.LOG.close()
        if os.path.getsize(name) == 0:
            os.makedirs(os.path.join(HOME, ".trash"), exist_ok=True)
            os.replace(name, os.path.join(HOME, ".trash", os.path.basename(name)))
    except OSError:
        pass
    agent.LOG = open(os.devnull, "w")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run-id", default=time.strftime("%Y%m%d-%H%M%S"))
    ap.add_argument("--no-paid", action="store_true", help="development only: skip scenario P")
    ap.add_argument("--only", default="", help="comma list of scenario ids, e.g. A,G")
    ap.add_argument("--mock-jev", action="store_true", help="development only: no TypeSafe calls")
    ap.add_argument("--no-restore", action="store_true", help="do not relaunch run-app.sh at the end")
    args = ap.parse_args()
    _quiet_agent_log()
    from scenarios import SCENARIOS
    suite = Suite(args)
    code = asyncio.run(suite.run(SCENARIOS))
    print(open(os.path.join(suite.out, "report.md")).read(), flush=True)
    sys.exit(code)


if __name__ == "__main__":
    main()
