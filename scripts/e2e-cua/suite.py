#!/usr/bin/env python3
"""magic-carpet-chat end-to-end suite. `--target box` (default) drives the app
inside the `mc` container on the magic-carpet-cua box host. `--target omac`
drives a local XWayland binary on this Omarchy host. Writes one report per run.

    suite.py --run-id 20260923-1400 [--target box|omac] [--no-paid] [--only A,B] [--mock-jev]

Every UI step goes through the agent.py pattern: the scenario owns a candidate
table, Jev picks one id, Jev judges the postcondition from the OCR. On top of
Jev, each scenario makes hard checks (literal OCR phrases, fake-server log
lines, the clipboard, the float) that decide PASS or FAIL.

Secrets: keys.json is read here and handed to the app only through environment
values passed to its launch, never on a command line, in a log or in the report.
"""
import argparse, asyncio, dataclasses, json, os, re, shlex, shutil, subprocess, sys, time, traceback

HOME = os.path.expanduser("~")
MC = os.path.join(HOME, "mc")
E2E = os.path.dirname(os.path.abspath(__file__))
KEYS = os.path.join(MC, "keys.json")
OMAC_CONF = os.path.join(HOME, ".config", "magic-carpet-e2e")
CDIR = "/tmp/e2e"                       # work dir inside the mc container
APP_BIN = "/usr/local/bin/magic-carpet-chat-e2e"   # run.sh installs here; the demo's binary is left alone
WALLET = "node /usr/local/lib/node_modules/brainstorm/node_modules/.bin/agent-wallet"
FAKE_URL, RELAY_URL = "http://127.0.0.1:8787", "ws://127.0.0.1:10547"
BS_URL = "http://127.0.0.1:8788"        # fake Brainstorm (same fake_instance.py)

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


def accent_pills(data):
    """The accent-gradient buttons ('Follow', 'Calculate my trust scores',
    'Publish my Treasure Map') paint dark text on violet — OCR cannot read
    them. Detect the pills by colour instead: connected blobs of ACCENT-ish
    pixels, returned as [(cx, cy, w, h)]."""
    import io
    from PIL import Image
    im = Image.open(io.BytesIO(data)).convert("RGB")
    w, h = im.size
    cell = 4
    px = im.load()
    grid = set()
    for gy in range(h // cell):
        for gx in range(w // cell):
            n = 0
            for y in range(gy * cell, (gy + 1) * cell):
                for x in range(gx * cell, (gx + 1) * cell):
                    r, g, b = px[x, y][:3]
                    # ACCENT violet, or the AMBER fill of 'Start a new
                    # follow list' — both unreadable to OCR (dark text).
                    if (0x60 <= r <= 0xb0 and 0x50 <= g <= 0xa0 and b >= 0xd0) or \
                       (r >= 0xd0 and 0x90 <= g <= 0xd0 and b <= 0x90):
                        n += 1
            if n >= 8:
                grid.add((gx, gy))
    pills = []
    while grid:
        stack, comp = [grid.pop()], []
        while stack:
            c = stack.pop()
            comp.append(c)
            for dx_, dy_ in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                nb = (c[0] + dx_, c[1] + dy_)
                if nb in grid:
                    grid.discard(nb)
                    stack.append(nb)
        xs = [c[0] for c in comp]
        ys = [c[1] for c in comp]
        x0, x1, y0, y1 = min(xs) * cell, (max(xs) + 1) * cell, min(ys) * cell, (max(ys) + 1) * cell
        if x1 - x0 >= 30 and 15 <= y1 - y0 <= 60:   # a pill, not text glyphs
            pills.append(((x0 + x1) / 2, (y0 + y1) / 2, x1 - x0, y1 - y0))
    return pills


def ocr_phrase_pos(data, phrase):
    """(x, y) of `phrase`'s first occurrence in the 2x OCR, or None."""
    import io
    from PIL import Image
    im = Image.open(io.BytesIO(data)).convert("L")
    im.resize((im.width * 2, im.height * 2), Image.LANCZOS).save("/tmp/ocr-pos.png")
    tsv = subprocess.run(["tesseract", "/tmp/ocr-pos.png", "-", "--psm", "3", "tsv"],
                         capture_output=True, text=True).stdout
    lines = {}
    for ln in tsv.splitlines()[1:]:
        p = ln.split("\t")
        if len(p) < 12 or not p[11].strip():
            continue
        lines.setdefault((p[2], p[3], p[4]), []).append(
            (p[11], float(p[6]) + float(p[8]) / 2, float(p[7]) + float(p[9]) / 2))
    needle = phrase.lower()
    for ws in sorted(lines.values(), key=lambda ws: (min(w[2] for w in ws), min(w[1] for w in ws))):
        joined = " ".join(w[0] for w in ws).lower()
        i = joined.find(needle)
        if i < 0:
            continue
        off, sel = 0, []
        for w_, cx, cy in ws:
            w0, w1 = off, off + len(w_)
            off += len(w_) + 1
            if w0 < i + len(needle) and w1 > i:
                sel.append((cx, cy))
        if sel:
            return (sum(x for x, _ in sel) / len(sel) / 2,
                    sum(y for _, y in sel) / len(sel) / 2)
    return None


class FineDriver(Driver):
    """agent.Driver plus `click_fine` (sidebar OCR) and `click_pill`
    (accent buttons by colour — OCR cannot read their dark-on-violet text)."""

    # Named rail targets -> window-local point. The box coordinates were
    # measured on the suite's first build; the current rail puts the first
    # avatar higher, which OmacDriver overrides below.
    AVATARS = {"house": (34, 190), "claimant": (34, 245), "plus": (34, 300)}

    async def act(self, kind, arg):
        if kind == "avatar":
            await self.sb.mouse.click(*self.AVATARS[arg])
            return None
        if kind == "stamp":     # remember when this point of the action ran
            self.scenario.stamps[arg] = time.time()
            return None
        if kind == "snap":      # a raw evidence frame between actions, no OCR
            return await self.scenario.snap(arg)
        if kind == "click_pill":
            # arg: an OCR anchor phrase (or its prefix): the accent pill
            # nearest it — same row for "RECOMMENDED", below a step heading
            # for the calc/publish buttons.
            data = shot_bytes(await self.sb.screenshot())
            pills = accent_pills(data)
            if not pills:
                raise RuntimeError("click_pill: no accent pill on screen")
            pos = ocr_phrase_pos(data, arg)
            if pos is None:
                raise RuntimeError(f"click_pill: anchor {arg!r} not on screen")
            ax, ay = pos
            # Same-row anchors (the recommended row) beat vertical distance.
            best = min(pills, key=lambda p: (abs(p[1] - ay) * 8) + abs(p[0] - ax))
            await self.sb.mouse.click(best[0], best[1])
            return f"click_pill near {arg!r} -> ({best[0]:.0f},{best[1]:.0f})"
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
        self.cdir = CDIR   # so collect() can address both targets uniformly
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

    def launch(self, name, run_id, target="fake", seed=False, nsecs=None, extra_env=None):
        """Relaunch with a fresh MC_STORE_DIR. seed copies the demo store
        (/root/mc-store/accounts.json, which holds the claimant's Coinos
        login) into it, container-internal, never read here. `nsecs` overrides
        the default house+claimant pair (per-scenario throwaway keys)."""
        self.kill_app()
        store = f"/root/e2e-stores/{run_id}/{name}"
        dx(f"mkdir -p {shlex.quote(store)} && chmod 700 /root/e2e-stores {shlex.quote(store)}")
        if seed:
            dx(f"cp /root/mc-store/accounts.json {store}/ && chmod 600 {store}/accounts.json")
        base, relay = (FAKE_URL, RELAY_URL) if target == "fake" else ("http://tapestry", "ws://tapestry/relay")
        env = dict(os.environ)
        env.update({
            "MC_NSECS": nsecs or f"{self.keys['house']['sec']},{self.keys['claimant']['sec']}",
            "MC_ISSUER_NPUB": self.keys["house"]["npub"], "MC_BASE_URL": base, "MC_RELAY_URL": relay,
            "MC_STORE_DIR": store, "MC_SENTRY_DSN": "", "DISPLAY": ":1", "RUST_BACKTRACE": "1",
        })
        if target == "fake":
            # Not for "instance": a new account's kind-0 goes to the
            # MC_TRUST_RELAYS set when it is set, and must reach public relays.
            env.update({"MC_BRAINSTORM_URL": BS_URL, "MC_TRUST_RELAYS": relay})
        env.update(extra_env or {})
        env.pop("ANTHROPIC_API_KEY", None)
        dx(f"exec {APP_BIN} > {CDIR}/logs/{name}.app.log 2>&1", detach=True,
           env_names=tuple(k for k in env if k.startswith("MC_") or k in ("DISPLAY", "RUST_BACKTRACE")),
           env=env)
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

    def wallet_accounts_in_store(self, store):
        """Accounts the app created itself, with a Coinos wallet stored.
        jq prints the count only; the logins stay in the container."""
        r = dx(f"jq '[.accounts[] | select(.created_in_app == true and .coinos != null)] | length' {store}/accounts.json")
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

    # --- nak and the fake Brainstorm: inside the container, where they run ---
    def nak(self, args, input=None, sec=None):
        """(returncode, stdout) of nak in the container. A "$NAK_SEC" argument
        expands to `sec` from the environment, so the key never sits in a
        script string."""
        line = " ".join(a if a == "$NAK_SEC" else shlex.quote(a) for a in args)
        env = dict(os.environ, NAK_SEC=sec) if sec else None
        r = dx(f"{CDIR}/nak {line}", input=input, env_names=("NAK_SEC",) if sec else (), env=env, timeout=30)
        return r.returncode, r.stdout

    def bs_get(self, path):
        r = dx(f"curl -sf {BS_URL}{path}", timeout=15)
        if r.returncode != 0:
            raise RuntimeError(f"GET {path} failed (curl {r.returncode})")
        return r.stdout

    # --- the float (P) ----------------------------------------------------
    def float_sats(self):
        r = run(["docker", "exec", "tapestry", "sh", "-c", WALLET + " balance"], timeout=60)
        try:
            return int(json.loads(r.stdout.strip().splitlines()[-1])["balance_sats"])
        except (ValueError, KeyError, IndexError):
            return None


# ============================================================== --target omac
# The app runs as a local XWayland client (DISPLAY=:0, no WAYLAND_DISPLAY) so
# xdotool reaches it exactly like on the box, window-targeted with --window.
# Hyprland places/keeps it floating at 0,0 1280x960 via hl.dsp eval calls, so
# window-local coordinates equal screen coordinates and the SIDEBAR/CONTENT
# crops keep working. Screenshots are grim grabs of that exact region.
#
# Measured on omac (Hyprland 0.56.2, cua-driver 0.34.0):
# - cua-driver background delivery refuses gpui (client_not_qualified) and
#   every foreground action refuses with primary_target_busy — the app binds
#   seat resources the Hyprland input-v3 plugin counts as busy (foot accepts
#   the same calls).
# - hl.dsp.send_shortcut and wtype deliver to foot but produce zero effect on
#   the app, and Omarchy binds super+digit/bracket/n/r anyway.
# - xdotool --window over XTEST is the reliable path (same as the box).

def hy_eval(expr):
    r = run(["hyprctl", "eval", expr], timeout=15)
    return r.returncode == 0 and "error" not in (r.stdout + r.stderr)


class _LocalShell:
    async def run(self, cmd, timeout=60):
        return run(["sh", "-c", cmd], timeout=timeout)


class _XdotoolMouse:
    """Sandbox mouse over xdotool --window (window-local pixels)."""
    def __init__(self, sb):
        self.sb = sb

    async def click(self, x, y):
        wid = self.sb.wid
        run(["xdotool", "mousemove", "--window", str(wid), str(int(x)), str(int(y)),
             "click", "1"], env={**os.environ, "DISPLAY": ":0"})

    async def move(self, x, y):
        run(["xdotool", "mousemove", "--window", str(self.sb.wid), str(int(x)), str(int(y))],
            env={**os.environ, "DISPLAY": ":0"})


class OmacSandbox:
    """Drop-in for the box's cua Sandbox: screenshot/shell/mouse/disconnect."""

    def __init__(self):
        self.wid = 0
        self.shell = _LocalShell()
        self.mouse = _XdotoolMouse(self)

    async def screenshot(self):
        """PNG bytes of the app window: grim over its live hyprctl geometry."""
        geo = run(["hyprctl", "clients", "-j"], timeout=10).stdout
        at, size = (0, 0), (1280, 960)
        for c in json.loads(geo):
            if "Magic Carpet" in c.get("title", ""):
                at, size = c["at"], c["size"]
                break
        png = f"/tmp/mc-e2e-shot-{os.getpid()}.png"
        run(["grim", "-g", f"{at[0]},{at[1]} {size[0]}x{size[1]}", png], timeout=20)
        data = open(png, "rb").read()
        os.unlink(png)
        return data

    async def disconnect(self):
        pass


class OmacDriver(FineDriver):
    """agent.Driver's kinds over local XWayland xdotool, window-targeted."""

    # Measured on the current rail at 1280x960: first avatar ~y141, then
    # one every ~54 px, "+" below the last account.
    AVATARS = {"house": (35, 141), "claimant": (35, 195), "plus": (35, 250)}

    async def x(self, cmd):
        return await self.sb.shell.run(f"DISPLAY=:0 {cmd}", timeout=60)

    async def act(self, kind, arg):
        wid = self.sb.wid
        if kind == "key":
            await self.x(f"xdotool key --window {wid} --clearmodifiers {arg}")
        elif kind == "type":
            # --delay 40: at 20 gpui's masked input drops keystrokes on a
            # 63-char nsec (measured; 45 worked, 40 keeps fields brisk).
            await self.x(f"xdotool type --window {wid} --delay 40 " +
                         json.dumps(agent.secret(arg) if arg.startswith("$") else arg))
        elif kind == "focus":
            # windowactivate is unreliable on rootless XWayland; the Hyprland
            # focus dispatcher plus a pointer park does it.
            addr = self.sb.addr
            hy_eval(f'local w=hl.get_window("{addr}") hl.dispatch(hl.dsp.focus({{window=w}}))')
            await self.x("xdotool mousemove 20 740")
        else:
            return await super().act(kind, arg)


class OmacBox:
    """Local twin of Box: fake server + relay + app + wf-recorder, all on
    this host under /tmp/mc-e2e-work/<run-id> (mode 700)."""

    def __init__(self, keys):
        self.keys = keys
        self.proc = None
        self.addr = ""
        self.sandbox = None       # set by Suite once the sandbox exists
        self.fake_proc = None
        self.relay_proc = None
        self.rec_proc = None

    def setup(self):
        self.cdir = os.environ["MC_E2E_DIR"]
        os.makedirs(os.path.join(self.cdir, "rec"), exist_ok=True)
        os.makedirs(os.path.join(self.cdir, "logs"), exist_ok=True)
        os.chmod(self.cdir, 0o700)

    def app_version(self, app_bin):
        r = run(["sh", "-c", f"strings {shlex.quote(app_bin)} | grep -m1 -oE 'magic-carpet-chat/[0-9]+\\.[0-9]+\\.[0-9]+(-rc[0-9]+)?'; "
                             f"sha256sum {shlex.quote(app_bin)} | cut -c1-16"])
        return " ".join(r.stdout.split())

    # --- fake instance + Brainstorm + relay (fresh per scenario) ----------
    def start_fake(self, mode):
        self.stop_fake()
        self.set_mode(mode)
        open(os.path.join(self.cdir, "server.log"), "w").close()
        env = {**os.environ, "E2E_DIR": self.cdir}
        self.fake_proc = subprocess.Popen(
            ["python3", os.path.join(E2E, "fake_instance.py")],
            env=env, stdout=open(os.path.join(self.cdir, "fake.out"), "w"), stderr=subprocess.STDOUT)
        self.relay_proc = subprocess.Popen(
            ["nak", "serve", "--hostname", "127.0.0.1", "--port", "10547"],
            stdout=open(os.path.join(self.cdir, "relay.log"), "w"), stderr=subprocess.STDOUT)
        for _ in range(40):
            if run(["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}",
                    f"{FAKE_URL}/api/bounties"], timeout=5).stdout.strip() == "200":
                return
            time.sleep(0.25)
        raise RuntimeError("the fake instance did not come up")

    def stop_fake(self):
        run(["pkill", "-f", "fake_instance.py"])
        run(["pkill", "-f", "nak serve"])
        for p in (getattr(self, "fake_proc", None), getattr(self, "relay_proc", None)):
            if p and p.poll() is None:
                p.terminate()
        self.fake_proc = self.relay_proc = None

    def set_mode(self, mode):
        full = {"create": "ok", "hang_secs": 45, "scan": "ok", "dup_coordinate": "",
                "graperank": "ok", "setup": "ok", "delay_secs": 0}
        full.update(mode)
        tmp = os.path.join(self.cdir, "mode.tmp")
        open(tmp, "w").write(json.dumps(full))
        os.rename(tmp, os.path.join(self.cdir, "mode.json"))

    def mark(self, text):
        with open(os.path.join(self.cdir, "server.log"), "a") as f:
            f.write("%.3f === %s\n" % (time.time(), text))

    def server_log(self):
        try:
            return open(os.path.join(self.cdir, "server.log")).read()
        except FileNotFoundError:
            return ""

    # --- the app -----------------------------------------------------------
    def kill_app(self):
        if self.proc and self.proc.poll() is None:
            self.proc.terminate()
            for _ in range(20):
                if self.proc.poll() is not None:
                    break
                time.sleep(0.25)
            if self.proc.poll() is None:
                self.proc.kill()
        self.proc = None
        run(["pkill", "-x", "magic-carpet-ch"])

    def app_alive(self):
        return self.proc is not None and self.proc.poll() is None

    def launch(self, name, run_id, target="fake", seed=False, nsecs=None, extra_env=None, app_bin=None):
        """Launch locally under XWayland with a fresh mode-0700 MC_STORE_DIR.
        No seeded store exists on omac (seed is a no-op)."""
        self.kill_app()
        app_bin = app_bin or getattr(self, "app_bin", None)
        store = os.path.join(self.cdir, "stores", name)
        os.makedirs(store, exist_ok=True)
        os.chmod(store, 0o700)
        base, relay = (FAKE_URL, RELAY_URL) if target == "fake" else ("http://tapestry", "ws://tapestry/relay")
        env = dict(os.environ)
        env.update({
            # XWayland: no WAYLAND_DISPLAY, so gpui takes the X11 backend and
            # xdotool --window reaches it, bypassing the Hyprland super binds.
            "DISPLAY": ":0",
            "MC_NSECS": nsecs or f"{self.keys['house']['sec']},{self.keys['claimant']['sec']}",
            "MC_ISSUER_NPUB": self.keys["house"]["npub"], "MC_BASE_URL": base, "MC_RELAY_URL": relay,
            "MC_STORE_DIR": store, "MC_SENTRY_DSN": "", "RUST_BACKTRACE": "1",
        })
        if target == "fake":
            env.update({"MC_BRAINSTORM_URL": BS_URL, "MC_TRUST_RELAYS": relay})
        env.update(extra_env or {})
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("ANTHROPIC_API_KEY", None)
        logf = open(os.path.join(self.cdir, "logs", f"{name}.app.log"), "w")
        self.proc = subprocess.Popen([app_bin], env=env, stdout=logf, stderr=subprocess.STDOUT)   # noqa: F821 -- set from argv
        wid = ""
        for _ in range(60):
            r = run(["xdotool", "search", "--name", "Magic Carpet"],
                    env={**os.environ, "DISPLAY": ":0"}, timeout=10)
            wid = r.stdout.strip().splitlines()[:1]
            if wid:
                wid = wid[0]
                break
            time.sleep(0.5)
        if not wid:
            raise RuntimeError("the app window never appeared; see the app log")
        self.wid = wid
        # Float the window to 0,0 1280x960 so window-local == screen coords.
        for _ in range(40):
            addr = ""
            for c in json.loads(run(["hyprctl", "clients", "-j"], timeout=10).stdout):
                if c.get("pid") == self.proc.pid and "Magic Carpet" in c.get("title", ""):
                    addr = c["address"]
                    break
            if addr:
                break
            time.sleep(0.25)
        if not addr:
            raise RuntimeError("hyprctl never saw the app window")
        self.addr = addr
        if self.sandbox is not None:
            self.sandbox.wid, self.sandbox.addr = int(wid), addr
        for expr in ('hl.dsp.window.float({action="set", window=w})',
                     'hl.dsp.window.resize({x=1280,y=960,window=w})',
                     'hl.dsp.window.move({x=0,y=0,window=w})'):
            hy_eval(f'local w=hl.get_window("{addr}") hl.dispatch({expr})')
        time.sleep(6)   # first bounty list, profile scans, relay connect
        return store

    def accounts_in_store(self, store):
        r = run(["jq", ".accounts | length", os.path.join(store, "accounts.json")])
        try:
            return int(r.stdout.strip())
        except ValueError:
            return -1

    # --- recording ---------------------------------------------------------
    def rec_start(self, name):
        self.rec_proc = subprocess.Popen(
            ["wf-recorder", "-g", "0,0 1280x960", "-f",
             os.path.join(self.cdir, "rec", f"{name}.mp4")],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def rec_stop(self, name):
        if self.rec_proc and self.rec_proc.poll() is None:
            self.rec_proc.send_signal(2)   # SIGINT finalizes the mp4's moov atom
            try:
                self.rec_proc.wait(10)
            except subprocess.TimeoutExpired:
                self.rec_proc.kill()
        self.rec_proc = None

    # --- clipboard (G) -----------------------------------------------------
    def clipboard(self):
        return run(["timeout", "5", "wl-paste"], timeout=10).stdout

    # --- nak and the fake Brainstorm: host-local on omac ------------------
    def nak(self, args, input=None, sec=None):
        args = [sec if a == "$NAK_SEC" else a for a in args]
        r = run(["nak", *args], input=input, timeout=30)
        return r.returncode, r.stdout

    def bs_get(self, path):
        import urllib.request
        return urllib.request.urlopen(f"{BS_URL}{path}", timeout=10).read().decode()

    # --- the float (P): no tapestry agent-wallet on omac -------------------
    def float_sats(self):
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
                rec["halt"] = "unknown id" if c is None else (f"weak {c.id} (p={p_pick:.2f}, abstain={p_abst:.2f})" if weak else c.id)
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

    def accept_step(self, label, reason):
        """A hard check settles a step the judge called unmet: mark the latest
        unmet record with that label, keeping its p and picks as they were."""
        rec = next((r for r in reversed(self.steps) if r["step"] == label and not r["met"]), None)
        if rec is None:
            return
        if "act_errors" in rec:
            print(f"  [{self.sid}] step '{label}' not accepted: actions raised", flush=True)
            return
        rec["met"], rec["override"] = True, reason
        print(f"  [{self.sid}] step '{label}' accepted by hard check: {reason}", flush=True)

    def skip(self, why):
        self.skip_reason = why


class Suite:
    def __init__(self, args):
        self.args = args
        self.target = args.target
        self.run_id = args.run_id
        if self.target == "omac":
            self.cdir = os.environ.get("MC_E2E_DIR", os.path.join("/tmp/mc-e2e-work", self.run_id))
            os.environ["MC_E2E_DIR"] = self.cdir   # anything that reads it late sees the same dir
            agent.E2E_DIR, agent.KEYS = self.cdir, os.path.join(OMAC_CONF, "keys.json")
            self.keys_path = os.path.join(OMAC_CONF, "keys.json")
            self.env_path = os.path.join(OMAC_CONF, ".env")
            self.out = os.path.join("/tmp/mc-e2e", self.run_id)
            self.app_bin = args.app_bin
        else:
            self.keys_path = KEYS
            self.env_path = os.path.join(MC, ".env")
            self.out = os.path.join(E2E, "runs", self.run_id)
            self.app_bin = APP_BIN
        os.makedirs(self.out, exist_ok=True)
        self.keys = json.load(open(self.keys_path))
        self.secrets = [self.keys[w]["sec"] for w in ("house", "claimant") if w in self.keys]
        self.box = OmacBox(self.keys) if self.target == "omac" else Box(self.keys)
        self.results = []

    async def run(self, registry):
        load_env(self.env_path)
        if self.args.mock_jev:
            os.environ.pop("TYPESAFE_API_KEY", None)
        self.jev = Jev()
        wanted = [s.strip().upper() for s in self.args.only.split(",")] if self.args.only else list(registry)
        paid = ("P" in wanted or "R" in wanted) and not self.args.no_paid and self.target == "box"
        meta = {"run_id": self.run_id, "target": self.target,
                "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                "jev": "live" if self.jev.live else "mock", "paid": paid}
        self.meta = meta
        if paid:
            bal = self.box.float_sats()
            meta["float_before_suite"] = bal
            need = 352 if "P" in wanted and "R" in wanted else 250 if "R" in wanted else 200   # R pays two newcomers
            if bal is None or bal < need:
                print(f"PREFLIGHT: the auto-pay float is {bal} sats, below {need}. "
                      f"Stopping. fund with ~/mc/fund.sh 500", flush=True)
                meta["stopped"] = f"float below {need} sats"
                self.write(meta)
                return 3
        self.box.setup()
        if self.target == "omac":
            if not self.app_bin or not os.path.exists(self.app_bin):
                print("no app binary given/existing: run.sh --target omac <binary>", flush=True)
                meta["stopped"] = "no app binary"
                self.write(meta)
                return 2
            meta["app"] = self.box.app_version(self.app_bin)
            self.sb = OmacSandbox()
            self.box.sandbox = self.sb
            self.box.app_bin = self.app_bin
            self.driver = OmacDriver(self.sb)
        else:
            if dx(f"test -x {APP_BIN}").returncode != 0:
                print(f"no e2e binary at {APP_BIN}; install one with run.sh <run-id|path>", flush=True)
                meta["stopped"] = f"no binary at {APP_BIN}"
                self.write(meta)
                return 2
            meta["app"] = self.box.app_version()
            from cua import Sandbox
            self.sb = await Sandbox.connect("mc", http_url=f"http://{agent.IP}:8000")
            self.driver = FineDriver(self.sb)
        print(f"suite {self.run_id}: app {meta['app']}, jev {meta['jev']}, paid {paid}", flush=True)
        try:
            for sid, (title, fn) in registry.items():
                if sid not in wanted:
                    continue
                if sid == "R" and self.target == "omac":
                    self.results.append({"scenario": sid, "title": title, "result": "BLOCKED",
                                         "why": "needs the real instance and live Coinos",
                                         "duration_s": 0, "evidence": "", "judge": [], "checks": []})
                    continue
                if sid == "P" and self.target == "omac":
                    self.results.append({"scenario": sid, "title": title, "result": "SKIP",
                                         "why": "needs the real instance and its float", "duration_s": 0,
                                         "evidence": "", "judge": [], "checks": []})
                    continue
                if sid in ("P", "R") and not paid:
                    self.results.append({"scenario": sid, "title": title, "result": "SKIP",
                                         "why": "--no-paid", "duration_s": 0, "evidence": "", "judge": [], "checks": []})
                    continue
                if sid == "H" and self.target == "omac":
                    # No seeded demo store with the claimant's Coinos login on
                    # omac: the sats-left check cannot run. BLOCKED, not FAIL.
                    self.results.append({"scenario": sid, "title": title, "result": "BLOCKED",
                                         "why": "no seeded Coinos login for the claimant on omac; "
                                                "fund the claimant's Coinos wallet to run H",
                                         "duration_s": 0, "evidence": "", "judge": [], "checks": []})
                    continue
                self.results.append(await self.one(sid, title, fn))
                self.write(meta)
        finally:
            await self.sb.disconnect()
            self.box.kill_app()
            self.box.stop_fake()
            if self.target == "omac":
                # The stores hold nsec copies; the suite created them under
                # /tmp, so plain removal is the rule here.
                run(["rm", "-rf", self.cdir])
            else:
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
                        "met": s["met"], **({"override": s["override"]} if "override" in s else {}),
                        **({"halt": s["halt"]} if "halt" in s else {}),
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
        for src, dst in ((f"{self.box.cdir}/rec/{sid}.mp4", f"{sid}.mp4"), (f"{self.box.cdir}/logs/{sid}.app.log", "app.log"),
                         (f"{self.box.cdir}/server.log", "fake-server.log")):
            p = os.path.join(d, dst)
            if self.target == "omac":
                try:
                    shutil.copy2(src, p)
                except FileNotFoundError:
                    continue
            else:
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
            n_over = sum(1 for s in r["judge"] if "override" in s)
            by_check = f" ({n_over} by hard check)" if n_over else ""
            rows.append(f"| {r['scenario']} {r['title']} | **{r['result']}** | {r['duration_s']} s | "
                        f"{met}/{len(r['judge'])}{by_check} | {ok}/{len(r['checks'])}{' — ' + extra if extra else ''} | "
                        f"{r['evidence']}/ |")
        head = [f"# magic-carpet-chat e2e run {meta['run_id']}", "",
                f"Target: {meta.get('target', 'box')} · App: `{meta.get('app', '?')}` · Jev: {meta['jev']} · "
                f"paid scenario: {meta['paid']}"]
        if "float_before" in meta or "float_after" in meta:
            head.append(f"Float: {meta.get('float_before')} sats before, {meta.get('float_after')} sats after")
        if "float_before_r" in meta or "float_after_r" in meta:
            head.append(f"Float (R): {meta.get('float_before_r')} sats before, {meta.get('float_after_r')} sats after")
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
    ap.add_argument("--target", choices=["box", "omac"], default="box",
                    help="box: the exe.dev sandbox over the cua API; "
                         "omac: this machine, app on XWayland, xdotool + grim")
    ap.add_argument("--app-bin", default="", help="omac only: path to the binary under test")
    ap.add_argument("--no-paid", action="store_true", help="development only: skip P and R")
    ap.add_argument("--only", default="", help="comma list of scenario ids, e.g. A,G")
    ap.add_argument("--mock-jev", action="store_true", help="development only: no TypeSafe calls")
    ap.add_argument("--no-restore", action="store_true", help="box only: do not relaunch run-app.sh at the end")
    args = ap.parse_args()
    _quiet_agent_log()
    from scenarios import SCENARIOS
    suite = Suite(args)
    code = asyncio.run(suite.run(SCENARIOS))
    print(open(os.path.join(suite.out, "report.md")).read(), flush=True)
    sys.exit(code)


if __name__ == "__main__":
    main()
