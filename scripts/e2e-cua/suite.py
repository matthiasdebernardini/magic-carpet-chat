#!/usr/bin/env python3
"""magic-carpet-chat end-to-end suite. Drives the app inside the `mc` container
on the magic-carpet-cua box host. Writes one report per run.

    suite.py --run-id 20260923-1400 [--no-paid] [--only A,B] [--mock-jev]

Every UI step goes through the agent.py pattern: the scenario owns a candidate
table, Jev picks one id, Jev judges the postcondition from the screenshot's
OCR. The candidates act through the app's accessibility tree, by the ids in
docs/a11y-ids.md (AppDriver). On top of Jev, each scenario makes hard checks
(tree reads, fake-server log lines, the clipboard, the float) that decide
PASS or FAIL.

Secrets: keys.json is read here and handed to the app only through environment
values passed to its launch, never on a command line, in a log or in the report.
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
BS_URL = "http://127.0.0.1:8788"        # fake Brainstorm (same fake_instance.py)
PAID = ("P", "R", "Y")                  # move real sats: skipped by --no-paid
# Two 1280x960 apps side by side: xfwm adds 5 px of frame left and right, so
# each window is 1290 wide and two fit 2580 exactly (scenarios.APP_B_AT).
OPT_IN = ("Y",)                         # never in a default run, only via --only
WIDE = {"Y": (2580, 960)}               # the screen and the recording are this size
# The e2e session bus inside mc: the app registers its AccessKit tree on the
# accessibility bus this bus's at-spi-bus-launcher hands out, and atspi.py
# reads it there. Box.a11y_up gives the bus its own XDG_RUNTIME_DIR, so that
# socket is not the demo desktop's (/root/.cache/at-spi/bus_1).
A11Y_ENV = f"DBUS_SESSION_BUS_ADDRESS=unix:path={CDIR}/a11y-bus"

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


# The OCR passes see() hands Jev, beside the full-screen one: the 10 px
# sidebar text and the dim content text it misses. Nothing clicks or checks by them.
SIDEBAR = (70, 95, 290, 240)   # account chip: name, npub, address, remove link, sats warning
CONTENT = (70, 90, 1280, 960)  # the window below its title bars


def atspi(cmd, target=None, name=None, has=(), secs=None, within=None, pid=None):
    """atspi.py in the mc container. Returns its JSON; raises
    RuntimeError with the helper's reason when the node is not exactly one,
    is outside the window, or a wait times out. `pid` pins the app (two-app
    scenarios); without it the helper reads `pidof -s` of the e2e binary."""
    args = [cmd] + ([target] if target else []) + ([str(secs)] if secs is not None else [])
    if name is not None:
        args += ["--name", name]
    for h in has:
        args += ["--has", h]
    if within:
        args += ["--within", within]
    if pid is not None:
        args += ["--pid", str(pid)]
    try:
        r = dx(f"{A11Y_ENV} python3 {CDIR}/atspi.py " + " ".join(shlex.quote(a) for a in args),
               timeout=(secs or 0) + 30)
    except subprocess.TimeoutExpired:
        raise RuntimeError(f"atspi {cmd} {target}: docker exec timed out")
    try:
        out = json.loads(r.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        raise RuntimeError(f"atspi {cmd} {target}: no answer (exit {r.returncode}): {r.stderr.strip()[-300:]}")
    if "error" in out:
        raise RuntimeError(f"atspi {cmd}: {out['error']}")
    return out


def fine_lines(data, box=SIDEBAR, scale=4, thr=60):
    """OCR (for Jev) of the 10 px sidebar text the 2x full-screen pass misses: crop,
    upscale 4x, binarize (dark text on white). Returns the lines in reading order."""
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
        rows.setdefault((p[2], p[3], p[4]), []).append((p[11], float(p[7]) + float(p[9]) / 2))
    return [" ".join(w for w, _ in ws) for ws in sorted(rows.values(), key=lambda ws: min(y for _, y in ws))]


def shot_bytes(img):
    return img if isinstance(img, (bytes, bytearray)) else getattr(img, "data", None) or img


@dataclasses.dataclass
class App:
    """One app process of a two-app scenario: its pid (AT-SPI), X window
    (xdotool), store, and the 1280x960 part of the screen its screenshots
    are cropped to, so Jev's OCR boxes line up as with one app."""
    name: str
    pid: int
    wid: str
    store: str
    crop: tuple


class AppDriver(Driver):
    """agent.Driver ("key", "type", "wait", "focus") plus the accessibility-
    tree kinds (docs/a11y-ids.md). Nothing here clicks or reads by OCR or by
    fixed screen points: every control is found by its id.

      ("press", t)     press the control through AT-SPI (its "click" action)
      ("click_id", t)  click the control's centre with xdotool: a text input
                       has no action, so this focuses it for "type"; a step
                       button drawn disabled has no click handler, so this
                       is the pointer click a user would make on it
      ("wait_id", t)   wait until the control is in the tree
      ("scroll_to", t) wheel the pane under the control until its centre is
                       inside the window, reading its extents from the tree
                       after each turn (gpui has no scroll-into-view)

      ("select_bounty", list name)  Down, one row at a time, until
                       bounty-detail-title names that list (at most rows + 1)
      ("park", None)   move the pointer to the rail's foot, off any hover
      ("stamp", name)  remember when this point of the action ran
      ("snap", label)  a raw evidence frame between actions, no OCR

    t is an id, a "~regex" id, or {"id": ..., "name": exact name, "has":
    [name substrings], "secs": wait timeout, "soft": True}. Exactly one node
    must match, and press/click_id refuse a control outside the window. A
    soft wait_id returns on timeout instead of raising.

    When the scenario has an app pinned (Scenario.use, two-app scenarios),
    every AT-SPI call reads that app's pid, every window action addresses its
    X window, and "key"/"type" first make that window the active one and
    check it is: keys never land in the other app."""

    last_wait_ok = False   # the last action was a wait_id that found its control

    # The window of the binary under test, by pid: the demo app's window has
    # the same title.
    APP_WINDOW = f"$(xdotool search --all --pid $(pidof -s {os.path.basename(APP_BIN)}) --name 'Magic Carpet' | head -1)"

    @property
    def app(self):
        return getattr(getattr(self, "scenario", None), "app", None)

    def win(self):
        """The X window to address: the pinned app's, else the e2e binary's."""
        return self.app.wid if self.app else self.APP_WINDOW

    def a11y(self, cmd, target=None, **kw):
        if self.app:
            kw.setdefault("pid", self.app.pid)
        return atspi(cmd, target, **kw)

    async def focus_app(self):
        """Make the pinned app's window the active one, and check it is."""
        wid = self.app.wid
        if (await self.x("xdotool getactivewindow")).stdout.strip() != wid:
            await self.x(f"xdotool windowactivate --sync {wid}")
            got = (await self.x("xdotool getactivewindow")).stdout.strip()
            if got != wid:
                raise RuntimeError(f"focus: window {got} is active, not {self.app.name}'s {wid}")

    async def window_box(self):
        """(x, y, w, h) of the app window on the display. xwininfo, not
        xdotool getwindowgeometry, which is off by the window frame."""
        r = await self.x(f"xwininfo -id {self.win()}")
        v = dict(re.findall(r"(Absolute upper-left X|Absolute upper-left Y|Width|Height):\s+(-?\d+)", r.stdout))
        if len(v) != 4:
            raise RuntimeError("no app window for xwininfo")
        return (int(v["Absolute upper-left X"]), int(v["Absolute upper-left Y"]), int(v["Width"]), int(v["Height"]))

    async def act(self, kind, arg):
        if kind != "park":
            self.last_wait_ok = False
        if kind == "park":
            if self.app:
                await self.x(f"xdotool mousemove --window {self.app.wid} 15 684")
            else:
                await self.x("xdotool mousemove 20 740")
            return None
        if self.app and kind in ("key", "type"):
            # Active and checked, then addressed to the window: a focus change
            # in between drops the input instead of sending it to the other app.
            await self.focus_app()
            wid = self.app.wid
            if kind == "key":
                await self.x(f"xdotool key --window {wid} --clearmodifiers {arg}")
            else:
                await self.x(f"xdotool type --window {wid} --delay 20 " + json.dumps(agent.secret(arg) if arg.startswith("$") else arg))
            return None
        if self.app and kind == "focus":
            await self.focus_app()
            return await self.act("park", None)
        if self.app and kind in ("click_id", "scroll_to"):
            # A pointer click or wheel turn goes to whatever window is on top
            # there: raise this app's first (and leave it the active one).
            await self.focus_app()
        if kind == "select_bounty":
            # Down, one row at a time, until the detail pane is this list's
            # bounty: a row below the fold cannot be pressed (gpui has no
            # scroll-into-view), but the Shell's Down selects and scrolls.
            # After each Down the title must change before the next decision
            # (the repaint can trail the key), and a match must still hold a
            # second later. At most one press per row, plus one. Never Enter.
            if self.app:
                await self.focus_app()
                wid = self.app.wid
            else:
                wid = (await self.x(f"echo {self.APP_WINDOW}")).stdout.strip()
                await self.x(f"xdotool windowactivate --sync {wid}")

            def title():
                try:
                    return self.a11y("name", "bounty-detail-title")["name"]
                except RuntimeError:
                    return None
            cap = self.a11y("exists", "~bounty-row-[0-9a-f-]{36}")["count"] + 1
            cur = title()
            for i in range(cap + 1):
                if cur == arg:
                    await asyncio.sleep(1)
                    cur = title()
                    if cur == arg:
                        return f"select_bounty {arg!r} after {i} Down"
                    continue   # moved on after all: decide again on what it shows now
                if i == cap:
                    break
                await self.x(f"xdotool key --window {wid} Down")
                prev, end = cur, time.time() + 4
                while time.time() < end:
                    await asyncio.sleep(0.3)
                    cur = title()
                    if cur != prev:
                        break
            raise RuntimeError(f"select_bounty: {arg!r} not reached in {cap} Down presses (last {cur!r})")
        if kind in ("press", "click_id", "wait_id", "scroll_to"):
            t = arg if isinstance(arg, dict) else {"id": arg}
            if kind == "wait_id":
                try:
                    out = self.a11y("wait", t["id"], name=t.get("name"), has=t.get("has", ()), secs=t.get("secs", 10))
                except RuntimeError as e:
                    if not t.get("soft"):
                        raise
                    return f"wait_id {t['id']}: {e}"
                self.last_wait_ok = True
                return f"wait_id {out['id']} ({out['waited']} s)"
            wx, wy, ww, wh = await self.window_box()
            if kind == "scroll_to":
                # The wheel turns the pane under the pointer: put it at the
                # node's column, mid-window. Three clicks a turn, then the
                # tree says where the node went (the repaint can trail).
                # A node with no size has nowhere to scroll to: it fails
                # as it is, unscrolled.
                for turn in range(16):
                    r = self.a11y("rect", t["id"], name=t.get("name"))["rect"]
                    if r["w"] <= 0 or r["h"] <= 0:
                        raise RuntimeError(f"scroll_to: {t['id']} has no extents ({r})")
                    cy = r["y"] + r["h"] / 2
                    if 0 <= cy < wh:
                        return f"scroll_to {t['id']} at y={cy:.0f} in the window"
                    if turn == 15:
                        break
                    px_ = min(max(r["x"] + r["w"] // 2, 0), ww - 1)
                    await self.x(f"xdotool mousemove --window {self.win()} {px_} {wh // 2} "
                                 f"click --repeat 3 --delay 60 {5 if cy >= wh else 4}")
                    await asyncio.sleep(1.5)
                raise RuntimeError(f"scroll_to: {t['id']} still outside the {ww}x{wh} window after 15 turns ({r})")
            if kind == "press":
                out = self.a11y("press", t["id"], name=t.get("name"), within=f"{ww}x{wh}")
                return f"press {out['id']} {out['name']!r}"
            out = self.a11y("rect", t["id"], name=t.get("name"), within=f"{ww}x{wh}")
            r = out["rect"]
            cx, cy = r["x"] + r["w"] // 2, r["y"] + r["h"] // 2
            dw, dh = (int(v) for v in (await self.x("xdotool getdisplaygeometry")).stdout.split())
            if not (0 <= wx + cx < dw and 0 <= wy + cy < dh):
                raise RuntimeError(f"click_id: {out['id']} is off the display at ({wx + cx},{wy + cy})")
            await self.x(f"xdotool mousemove --window {self.win()} {cx} {cy} click 1")
            return f"click_id {out['id']} -> ({cx},{cy}) in the window"
        if kind == "stamp":
            self.scenario.stamps[arg] = time.time()
            return None
        if kind == "snap":
            return await self.scenario.snap(arg)
        return await super().act(kind, arg)


class Box:
    """Container-side plumbing: fake server, relay, app, recording, clipboard."""

    def __init__(self, keys):
        self.keys = keys

    def setup(self):
        dx(f"mkdir -p {CDIR}/rec {CDIR}/logs && chmod 700 {CDIR}")
        run(["docker", "cp", os.path.join(E2E, "fake_instance.py"), f"mc:{CDIR}/fake_instance.py"], check=True)
        run(["docker", "cp", os.path.join(E2E, "atspi.py"), f"mc:{CDIR}/atspi.py"], check=True)
        if dx(f"test -x {CDIR}/nak").returncode != 0:
            run(["docker", "cp", os.path.join(HOME, ".local/bin/nak"), f"mc:{CDIR}/nak"], check=True)

    def a11y_up(self):
        """The e2e session bus, its accessibility bus and IsEnabled, which
        every scenario drives the app through; kept while they still answer.
        True when all three are up."""
        if dx("python3 -c 'import gi; gi.require_version(\"Atspi\", \"2.0\")'").returncode != 0:
            print("a11y: python3-gi / gir1.2-atspi-2.0 missing in mc; nothing can drive by id", flush=True)
            return False
        # The bus activates at-spi-bus-launcher itself on the first
        # org.a11y.Bus call, with the daemon's environment: its own
        # XDG_RUNTIME_DIR keeps the accessibility socket off the desktop's.
        ping = "dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.GetId >/dev/null 2>&1"
        dx(f"export {A11Y_ENV}; {ping} || {{ rm -f {CDIR}/a11y-bus; mkdir -p {CDIR}/xdg && chmod 700 {CDIR}/xdg && "
           f"XDG_RUNTIME_DIR={CDIR}/xdg dbus-daemon --session --address=$DBUS_SESSION_BUS_ADDRESS --fork --nopidfile; }}")
        bus = "gdbus call --session --dest org.a11y.Bus --object-path /org/a11y/bus"
        for _ in range(20):
            if dx(f"{A11Y_ENV} {bus} --method org.freedesktop.DBus.Properties.Set "
                  f"org.a11y.Status IsEnabled '<true>' >/dev/null 2>&1").returncode == 0:
                return True
            time.sleep(0.25)
        print("a11y: the accessibility bus never came up; nothing can drive by id", flush=True)
        return False

    def set_screen(self, w, h):
        """Resize the VNC screen on :1 (Xtigervnc takes any mode via xrandr).
        True when the screen reports that size."""
        mode = f"{w}x{h}"
        out = "$(xrandr | awk '/ connected/{print $1; exit}')"
        dx(f"export DISPLAY=:1; xrandr --newmode {mode} 0 {w} 0 0 0 {h} 0 0 0 2>/dev/null; "
           f"xrandr --addmode {out} {mode} 2>/dev/null; xrandr --output {out} --mode {mode}")
        time.sleep(1)
        return f"{mode} pixels" in dx("DISPLAY=:1 xdpyinfo | grep dimensions").stdout

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

    def kill_pid(self, pid):
        """Stop one app process (the other app of a two-app scenario keeps running)."""
        dx(f"kill {int(pid)}; true")
        for _ in range(20):
            if dx(f"kill -0 {int(pid)}").returncode != 0:
                return
            time.sleep(0.25)
        dx(f"kill -9 {int(pid)}; true")

    def screen_size(self):
        """(w, h) of the screen on :1, or None."""
        try:
            w, h = dx("DISPLAY=:1 xdotool getdisplaygeometry").stdout.split()
            return int(w), int(h)
        except ValueError:
            return None

    def app_alive(self):
        return dx("pgrep -x magic-carpet-ch").returncode == 0

    def launch(self, name, run_id, target="fake", seed=False, nsecs=None, extra_env=None,
               keep=False, at=None, store=None, seed_account=None):
        """Relaunch with a fresh MC_STORE_DIR. seed copies the demo store
        (/root/mc-store/accounts.json, which holds the claimant's Coinos
        login) into it, container-internal, never read here. `nsecs` overrides
        the default house+claimant pair (per-scenario throwaway keys); "" is
        no key at all (a first launch).

        Two-app scenarios pass `at` (the window's x, y): the app is then found
        by its own pid, placed there, and recorded as `self.last_app` (an
        App). keep=True leaves the apps already running alone. `store`
        relaunches on an existing MC_STORE_DIR instead of a fresh one.
        seed_account=(accounts.json, pubkey) starts the fresh store with that
        one account, copied inside the container (see reusable_wallets).
        The app always gets the e2e session bus, so its AccessKit tree
        reaches atspi.py (a11y_up must have answered first)."""
        if not keep:
            self.kill_app()
        before = set(dx(f"pidof {os.path.basename(APP_BIN)}").stdout.split())
        store = store or f"/root/e2e-stores/{run_id}/{name}"
        dx(f"mkdir -p {shlex.quote(store)} && chmod 700 /root/e2e-stores {shlex.quote(store)}")
        if seed:
            dx(f"cp /root/mc-store/accounts.json {store}/ && chmod 600 {store}/accounts.json")
        if seed_account:
            src, pub = seed_account
            if not re.fullmatch(r"[0-9a-f]{64}", pub):
                raise RuntimeError("seed_account: not a hex pubkey")
            # jq inside mc: the account's nsec and Coinos login never leave it.
            r = dx(f"(umask 077; jq --arg p {pub} '{{active: $p, accounts: [.accounts[] | select(.pubkey == $p)]}}' "
                   f"{shlex.quote(src)} > {store}/accounts.json.tmp) && mv {store}/accounts.json.tmp {store}/accounts.json "
                   f"&& chmod 600 {store}/accounts.json && jq '.accounts | length' {store}/accounts.json")
            if r.stdout.strip() != "1":
                raise RuntimeError(f"seed_account: the seeded store does not hold the one account (jq said {r.stdout.strip()!r})")
        base, relay = (FAKE_URL, RELAY_URL) if target == "fake" else ("http://tapestry", "ws://tapestry/relay")
        env = dict(os.environ)
        env.update({
            "MC_NSECS": nsecs if nsecs is not None else f"{self.keys['house']['sec']},{self.keys['claimant']['sec']}",
            "MC_ISSUER_NPUB": self.keys["house"]["npub"], "MC_BASE_URL": base, "MC_RELAY_URL": relay,
            "MC_STORE_DIR": store, "MC_SENTRY_DSN": "", "DISPLAY": ":1",
            "DBUS_SESSION_BUS_ADDRESS": A11Y_ENV.split("=", 1)[1],
        })
        if target == "fake":
            # Not for "instance": a new account's kind-0 goes to the
            # MC_TRUST_RELAYS set when it is set, and must reach public relays.
            env.update({"MC_BRAINSTORM_URL": BS_URL, "MC_TRUST_RELAYS": relay})
        env.update(extra_env or {})
        env.pop("ANTHROPIC_API_KEY", None)
        if env["MC_NSECS"] == "":
            env.pop("MC_NSECS")
        dx(f"exec {APP_BIN} > {CDIR}/logs/{name}.app.log 2>&1", detach=True,
           env_names=tuple(k for k in env if k.startswith("MC_") or k in ("DISPLAY", "RUST_BACKTRACE", "DBUS_SESSION_BUS_ADDRESS")),
           env=env)
        if at is not None:
            self.last_app = self._place(name, before, at, store)
            time.sleep(6)   # first bounty list, profile scans, relay connect
            return store
        wid = ""
        for _ in range(60):
            wid = dx("DISPLAY=:1 xdotool search --name 'Magic Carpet' | head -1").stdout.strip()
            if wid:
                break
            time.sleep(0.5)
        if not wid:
            raise RuntimeError("the app window never appeared; see the app log")
        dx(f"DISPLAY=:1 xdotool windowsize {wid} 1280 960 windowmove {wid} 0 0 windowactivate {wid} mousemove 20 740")
        self._fit_height(wid)
        time.sleep(6)   # first bounty list, profile scans, relay connect
        return store

    def _measure(self, wid):
        """(client top, client height, frame bottom) of a window, or None."""
        v = dict(re.findall(r"(Absolute upper-left Y|Height):\s+(-?\d+)", dx(f"DISPLAY=:1 xwininfo -id {wid}").stdout))
        ext = re.findall(r"\d+", dx(f"DISPLAY=:1 xprop -id {wid} _NET_FRAME_EXTENTS").stdout.split("=")[-1])
        if len(v) != 2 or len(ext) != 4:
            return None
        return int(v["Absolute upper-left Y"]), int(v["Height"]), int(ext[3])

    def _fit_height(self, wid):
        """Shorten the window so all of it is on the screen: a 960-high
        client below the panel and title bar hangs ~60 px off the bottom,
        where the last bounty row can be neither seen nor pressed. Client
        height = screen height - the client's top - the frame's bottom.
        The window manager sets the frame and the position after the map, so
        both are read until two reads agree."""
        size = self.screen_size()
        if not size:
            raise RuntimeError("cannot read the screen size to fit the window")
        last = None
        for _ in range(20):
            time.sleep(0.3)
            cur = self._measure(wid)
            if cur and cur == last:
                break
            last = cur
        else:
            raise RuntimeError(f"window {wid}: its frame and position never settled ({last})")
        top, height, bottom = cur
        h = size[1] - top - bottom
        if h < height:
            dx(f"DISPLAY=:1 xdotool windowsize --sync {wid} 1280 {h}")
            time.sleep(0.3)
            got = self._measure(wid)
            if not got or got[1] != h or got[0] + got[1] + got[2] > size[1]:
                raise RuntimeError(f"window {wid}: asked for height {h} to fit the screen, it is {got}")
        return h

    def _place(self, name, before, at, store):
        """The just-launched app (the e2e pid not in `before`): its window,
        sized 1280x960 at `at` and raised. Returns its App."""
        pid = wid = ""
        for _ in range(60):
            new = set(dx(f"pidof {os.path.basename(APP_BIN)}").stdout.split()) - before
            pid = next(iter(new), "") if len(new) == 1 else ""
            if pid:
                wid = dx(f"DISPLAY=:1 xdotool search --all --pid {pid} --name 'Magic Carpet' | head -1").stdout.strip()
            if wid:
                break
            time.sleep(0.5)
        if not wid:
            raise RuntimeError(f"{name}: the app window never appeared; see the app log")
        # The window can exist before the window manager places it, and the
        # placement then overrides an early move: move until it holds.
        x = y = None
        for _ in range(20):
            dx(f"DISPLAY=:1 xdotool windowsize {wid} 1280 960 windowmove {wid} {at[0]} {at[1]} windowactivate --sync {wid}")
            time.sleep(0.5)
            v = dict(re.findall(r"(Absolute upper-left X|Absolute upper-left Y):\s+(-?\d+)", dx(f"DISPLAY=:1 xwininfo -id {wid}").stdout))
            x, y = int(v.get("Absolute upper-left X", -1)), int(v.get("Absolute upper-left Y", -1))
            if abs(x - at[0]) <= 10 and abs(y - at[1]) <= 60:
                break
        else:
            raise RuntimeError(f"{name}: the window stays at ({x},{y}), not at {at}")
        self._fit_height(wid)
        # One app's screen had the window client at (5, 56): crop so it is again.
        x0, y0 = max(0, x - 5), max(0, y - 56)
        return App(name, int(pid), wid, store, (x0, y0, x0 + 1280, y0 + 960))

    def reusable_wallets(self, run_id):
        """(path, [pubkey, pubkey]) of the two app-made Coinos accounts in an
        earlier run's R store, moved to the trash at the end of that run, or
        (path, None) when it is missing or does not hold exactly two. Only
        the pubkeys leave the container; the logins stay in it."""
        if not re.fullmatch(r"\d{8}-\d{6}", run_id or ""):
            return None, None
        path = f"/root/.trash/e2e-stores/{run_id}/R/accounts.json"
        # An empty token is a signup whose outcome was never known: not a wallet.
        r = dx(f"jq -r '.accounts[] | select(.created_in_app == true and .coinos != null "
               f"and (.coinos.username // \"\") != \"\" and (.coinos.token // \"\") != \"\") | .pubkey' {path}")
        pubs = r.stdout.split() if r.returncode == 0 else []
        ok = len(pubs) == 2 and all(re.fullmatch(r"[0-9a-f]{64}", p) for p in pubs)
        return path, (pubs if ok else None)

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
    def rec_start(self, name, size="1280x960"):
        dx(f"exec ffmpeg -y -loglevel error -f x11grab -video_size {size} -framerate 10 -i :1 "
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


class Scenario:
    """One scenario's context: its Jev loop, frames, checks and verdicts."""

    def __init__(self, suite, sid, title):
        self.suite, self.box, self.sid, self.title = suite, suite.box, sid, title
        self.dir = os.path.join(suite.out, sid)
        os.makedirs(os.path.join(self.dir, "frames"), exist_ok=True)
        self.steps, self.checks, self.frame_n, self.screen = [], [], 0, ""
        self.hist, self.skip_reason, self.store, self.notes = [], None, None, []
        self.seen_at, self.stamps = 0.0, {}
        # Two-app scenarios: the app every action, AT-SPI read and
        # screenshot addresses (see use), and the fast step mode.
        self.app, self.fast = None, False
        # Checks before this index are the launch preflight's (a11y_preflight):
        # they do not count as the scenario's hard check.
        self.checks_from = 0

    def use(self, app):
        """Pin the driver, the AT-SPI reads and the screenshots to `app`.
        The last observation was of the other app, so it is dropped."""
        self.app, self.screen, self.seen_at = app, "", 0.0

    # --- observe ----------------------------------------------------------
    async def grab(self):
        """A screenshot; with an app pinned, cropped to that app's 1280x960."""
        data = shot_bytes(await self.suite.sb.screenshot())
        if self.app is None:
            return data
        import io
        from PIL import Image
        out = io.BytesIO()
        Image.open(io.BytesIO(data)).crop(self.app.crop).save(out, "PNG")
        return out.getvalue()

    async def snap(self, label):
        data = await self.grab()
        return data, self.keep(data, label)

    def keep(self, data, label, text=None):
        """Save `data` (and its `text`: OCR, or a tree read) as the next evidence frame."""
        self.frame_n += 1
        stem = f"{self.frame_n:02d}-{re.sub(r'[^a-z0-9]+', '-', label.lower())[:40]}"
        open(os.path.join(self.dir, "frames", stem + ".png"), "wb").write(data)
        if text is not None:
            open(os.path.join(self.dir, "frames", stem + ".txt"), "w").write(redact(text, self.suite.secrets))
        return stem

    async def stable(self, max_wait=15.0, gap=0.8, tolerance=400):
        """Wait until two captures `gap` apart differ in fewer than
        `tolerance` pixels (a blinking caret is ~50). The app renders in
        software on a 2-CPU box shared with ffmpeg and tesseract, so a
        repaint can trail the input by seconds."""
        import io
        from PIL import Image, ImageChops
        grab = lambda b: Image.open(io.BytesIO(b)).convert("L").point(lambda v: 255 if v > 40 else 0)
        prev = grab(await self.grab())
        end = time.time() + max_wait
        while time.time() < end:
            await asyncio.sleep(gap)
            cur = grab(await self.grab())
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
        fine = "\n".join(fine_lines(data, CONTENT, scale=3, thr=50))
        side = "\n".join(fine_lines(data, SIDEBAR, scale=4, thr=60))
        text = redact(agent.ocr(data) + "\n[fine]\n" + fine + "\n[sidebar]\n" + side, self.suite.secrets)
        open(os.path.join(self.dir, "frames", stem + ".txt"), "w").write(text)
        self.screen = text
        return text

    # --- one agent.py-style step -------------------------------------------
    async def step(self, s: Step, must_act=False, label=None, min_conf=0.4, second_look=True):
        """Mirror of agent.run for one Step. must_act drops the 'already-done'
        candidate, for steps whose goal state is already on screen before the
        action (the refusals): the action has to happen to test anything.

        With self.fast (two-app scenario Y): a step with one candidate runs it
        without a Jev choose, its "before" is the previous after-look when
        fresh (else a frame without OCR), and when its actions end in a
        wait_id that found its control, the tree marks the next state and
        stable() is skipped. Jev still judges every after-look."""
        label = label or s.goal[:40]
        cands = s.cands + ([] if must_act else [DONE]) + [REOBSERVE, ABSTAIN]
        rec = {"step": label, "goal": s.goal, "cond": s.done, "picks": [], "p": None, "met": False, "phases": {}}
        ph = rec["phases"]

        def took(phase, since):
            ph[phase] = round(ph.get(phase, 0.0) + time.time() - since, 2)
        fixed = self.fast and len(s.cands) == 1
        t0 = time.time()
        for attempt in range(s.tries):
            # Whether this attempt's actions raised: accept_step reads the
            # last attempt only, since its screen is the one a check reads.
            rec["raised"] = False
            fresh = self.screen and time.time() - self.seen_at < 20
            t = time.time()
            if fixed:
                c = s.cands[0]
                rec["picks"].append(c.id)
                if not fresh:
                    await self.snap(f"{label}-before")
                took("ocr", t)
            else:
                # A must_act step reuses a fresh previous observation as its
                # "before": timed scenarios (A) cannot afford a second OCR.
                if (must_act or self.fast) and fresh:
                    screen = self.screen
                else:
                    screen = await self.see(f"{label}-before")
                took("ocr", t)
                t = time.time()
                pick, conf, probs = await self.suite.jev_call("choose", s.goal, screen, cands, self.hist, s.cues)
                took("jev_choose", t)
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
            t = time.time()
            self.suite.driver.last_wait_ok = False
            for kind, arg in c.do:
                try:
                    await self.suite.driver.act(kind, arg)
                except Exception as e:   # e.g. a press found no node: the attempt fails, the loop goes on
                    rec.setdefault("act_errors", []).append(redact(str(e), self.suite.secrets)[:300])
                    rec["raised"] = True
                    break
            took("act", t)
            t = time.time()
            await asyncio.sleep(s.settle)
            took("settle", t)
            t = time.time()
            if not (self.fast and self.suite.driver.last_wait_ok and not rec["raised"]):
                await self.stable()
            took("stable", t)
            t = time.time()
            screen = await self.see(f"{label}-after")
            took("ocr", t)
            t = time.time()
            p = await self.suite.jev_call("done", s.done, screen, s.cues)
            took("jev_done", t)
            if p < 0.6 and second_look:
                # A second look before the step counts as unmet: the render
                # can trail, and a borderline verdict should not redo an
                # action (a second Enter is a second bounty or claim).
                t = time.time()
                await asyncio.sleep(3)
                await self.stable()
                took("stable", t)
                t = time.time()
                screen = await self.see(f"{label}-second-look")
                took("ocr", t)
                t = time.time()
                p = max(p, await self.suite.jev_call("done", s.done, screen, s.cues))
                took("jev_done", t)
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

    # --- the accessibility tree (docs/a11y-ids.md) ----------------------
    # Reads go through the driver, so a pinned app's own tree (its pid) is read.
    def tree(self, target, has=None, name=None):
        """How many nodes match `target` (an id or ~regex) and, with `has`,
        whose name holds it; with `name`, whose name is exactly that. -1,
        and a note, when the tree cannot be read."""
        try:
            return self.suite.driver.a11y("exists", target, name=name, has=(has,) if has else ())["count"]
        except RuntimeError as e:
            self.notes.append(f"a11y: {e}"[:300])
            return -1

    def tree_node(self, target, name=None):
        """The one node matching `target` (id, role, name, description,
        rect), or None, and a note, unless exactly one matches."""
        try:
            return self.suite.driver.a11y("name", target, name=name)
        except RuntimeError as e:
            self.notes.append(f"a11y: {e}"[:300])
            return None

    def tree_name(self, target, name=None):
        n = self.tree_node(target, name)
        return n["name"] if n else None

    def wait_tree(self, target, secs, has=(), name=None):
        """True once exactly one node matches `target` (its name holding one
        of `has`) within `secs`; False, and a note, after that."""
        try:
            self.suite.driver.a11y("wait", target, name=name, has=(has,) if isinstance(has, str) else has, secs=secs)
            return True
        except RuntimeError as e:
            self.notes.append(f"a11y: {e}"[:300])
            return False

    def check_tree(self, name, want=(), unwanted=()):
        """A hard check on the tree. `want` and `unwanted` hold (target,
        phrase) pairs: a node matching `target` whose name holds `phrase`
        (None: any node matching it). Every `want` must be in the tree, and
        no `unwanted`."""
        missing = [w for w in want if self.tree(*w) < 1]
        present = [u for u in unwanted if self.tree(*u) != 0]
        return self.check(name, not missing and not present,
                          f"missing={missing} unexpected={present}" if missing or present else "")

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
        if rec.get("raised"):
            print(f"  [{self.sid}] step '{label}' not accepted: the last attempt's actions raised", flush=True)
            return
        rec["met"], rec["override"] = True, reason
        print(f"  [{self.sid}] step '{label}' accepted by hard check: {reason}", flush=True)

    def skip(self, why):
        self.skip_reason = why


class Suite:
    def __init__(self, args):
        self.args = args
        self.run_id = args.run_id
        self.env_path = os.path.join(MC, ".env")
        self.out = os.path.join(E2E, "runs", self.run_id)
        os.makedirs(self.out, exist_ok=True)
        self.keys = json.load(open(KEYS))
        self.secrets = [self.keys[w]["sec"] for w in ("house", "claimant") if w in self.keys]
        self.box = Box(self.keys)
        self.results = []

    async def run(self, registry):
        load_env(self.env_path)
        if self.args.mock_jev:
            os.environ.pop("TYPESAFE_API_KEY", None)
        self.jev = Jev()
        # Y is opt-in: it runs only when --only names it (two app-made
        # accounts and up to ~556 sats with P and R).
        wanted = [s.strip().upper() for s in self.args.only.split(",")] if self.args.only else [s for s in registry if s not in OPT_IN]
        paid = any(s in wanted for s in PAID) and not self.args.no_paid
        meta = {"run_id": self.run_id,
                "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                "jev": "live" if self.jev.live else "mock", "paid": paid}
        self.meta = meta
        if paid:
            bal = self.box.float_sats()
            meta["float_before_suite"] = bal
            # P pays one claim (200 alone); R and Y pay two newcomers each
            # (250 for the first, about 204 for each further one, 102 for P).
            two = sum(1 for s in ("R", "Y") if s in wanted)
            need = 250 + 204 * (two - 1) + (102 if "P" in wanted else 0) if two else 200
            if bal is None or bal < need:
                print(f"PREFLIGHT: the auto-pay float is {bal} sats, below {need}. "
                      f"Stopping. fund with ~/mc/fund.sh 500", flush=True)
                meta["stopped"] = f"float below {need} sats"
                self.write(meta)
                return 3
        self.box.setup()
        if dx(f"test -x {APP_BIN}").returncode != 0:
            print(f"no e2e binary at {APP_BIN}; install one with run.sh <run-id|path>", flush=True)
            meta["stopped"] = f"no binary at {APP_BIN}"
            self.write(meta)
            return 2
        meta["app"] = self.box.app_version()
        from cua import Sandbox
        self.sb = await Sandbox.connect("mc", http_url=f"http://{agent.IP}:8000")
        self.driver = AppDriver(self.sb)
        print(f"suite {self.run_id}: app {meta['app']}, jev {meta['jev']}, paid {paid}", flush=True)
        try:
            for sid, (title, fn) in registry.items():
                if sid not in wanted:
                    continue
                if sid in PAID and not paid:
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
        wide = sid in WIDE
        prior = None
        try:
            if wide:
                # No two-app scenario on a screen that cannot hold both apps.
                prior = self.box.screen_size()
                self.box.set_screen(*WIDE[sid])
                got = self.box.screen_size()
                if not got or got[0] < WIDE[sid][0] or got[1] < WIDE[sid][1]:
                    raise RuntimeError(f"the screen is {got}, not {WIDE[sid][0]}x{WIDE[sid][1]}; nothing launched")
                self.box.rec_start(sid, "%dx%d" % WIDE[sid])
            else:
                self.box.rec_start(sid)
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
            if wide:
                self.box.kill_app()   # both apps, before the screen shrinks under them
                if not prior:
                    sc.notes.append("the screen size before this scenario could not be read; the wide screen was not restored")
                elif not self.box.set_screen(*prior):
                    sc.notes.append(f"the screen did not go back to {prior[0]}x{prior[1]}")
            self.collect(sid)
        judged = [s for s in sc.steps]
        steps_ok = all(s["met"] for s in judged)
        checks_ok = all(c["ok"] for c in sc.checks)
        if sc.skip_reason and not err:
            result = "SKIP"
        else:
            result = "PASS" if (steps_ok and checks_ok and not err and sc.checks[sc.checks_from:]) else "FAIL"
        r = {"scenario": sid, "title": title, "result": result, "duration_s": round(time.time() - t0, 1),
             "evidence": os.path.relpath(sc.dir, self.out), "recording": f"{sid}/{sid}.mp4",
             "judge": [{"step": s["step"], "cond": s["cond"], "picks": s["picks"], "p": s["p"],
                        "met": s["met"], **({"override": s["override"]} if "override" in s else {}),
                        **({"halt": s["halt"]} if "halt" in s else {}),
                        **({"act_errors": s["act_errors"]} if "act_errors" in s else {}),
                        **({"phases": s["phases"]} if "phases" in s else {})} for s in judged],
             "checks": sc.checks, "notes": sc.notes}
        if err:
            r["error"] = err
        if sc.skip_reason:
            r["why"] = sc.skip_reason
        totals = {}
        for s in judged:
            for k, v in s.get("phases", {}).items():
                totals[k] = round(totals.get(k, 0.0) + v, 1)
        r["phase_totals"] = totals
        print(f"== {sid}: step phases (s): " + ", ".join(f"{k} {v}" for k, v in sorted(totals.items())), flush=True)
        print(f"== {sid}: {result} in {r['duration_s']} s", flush=True)
        return r

    def collect(self, sid):
        d = os.path.join(self.out, sid)
        apps = [(f"{CDIR}/logs/{sid}-{a}.app.log", f"app-{a}.log") for a in ("A", "B", "B2")] if sid in WIDE else []
        for src, dst in ((f"{CDIR}/rec/{sid}.mp4", f"{sid}.mp4"), (f"{CDIR}/logs/{sid}.app.log", "app.log"),
                         (f"{CDIR}/server.log", "fake-server.log"), *apps):
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
            n_over = sum(1 for s in r["judge"] if "override" in s)
            by_check = f" ({n_over} by hard check)" if n_over else ""
            rows.append(f"| {r['scenario']} {r['title']} | **{r['result']}** | {r['duration_s']} s | "
                        f"{met}/{len(r['judge'])}{by_check} | {ok}/{len(r['checks'])}{' — ' + extra if extra else ''} | "
                        f"{r['evidence']}/ |")
        head = [f"# magic-carpet-chat e2e run {meta['run_id']}", "",
                f"App: `{meta.get('app', '?')}` · Jev: {meta['jev']} · "
                f"paid scenario: {meta['paid']}"]
        if "float_before" in meta or "float_after" in meta:
            head.append(f"Float: {meta.get('float_before')} sats before, {meta.get('float_after')} sats after")
        if "float_before_r" in meta or "float_after_r" in meta:
            head.append(f"Float (R): {meta.get('float_before_r')} sats before, {meta.get('float_after_r')} sats after")
        if "float_before_y" in meta or "float_after_y" in meta:
            head.append(f"Float (Y): {meta.get('float_before_y')} sats before, {meta.get('float_after_y')} sats after")
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
    ap.add_argument("--no-paid", action="store_true", help="development only: skip P, R and Y")
    ap.add_argument("--only", default="", help="comma list of scenario ids, e.g. A,G")
    ap.add_argument("--mock-jev", action="store_true", help="development only: no TypeSafe calls")
    ap.add_argument("--no-restore", action="store_true", help="do not relaunch run-app.sh at the end")
    ap.add_argument("--reuse-wallets", default="", metavar="RUN_ID",
                    help="Y only: seed its two newcomers from that run's R store in the trash "
                         "(two app-made Coinos accounts) instead of new Coinos signups")
    args = ap.parse_args()
    _quiet_agent_log()
    from scenarios import SCENARIOS
    suite = Suite(args)
    code = asyncio.run(suite.run(SCENARIOS))
    print(open(os.path.join(suite.out, "report.md")).read(), flush=True)
    sys.exit(code)


if __name__ == "__main__":
    main()
