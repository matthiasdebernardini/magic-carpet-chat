# Feature 1 walkthrough driver: launches the real app, drives it with
# window-addressed input, records the window through mac.see() frames and
# judges each storyboard step with Jev (scripts/walkthrough/judge.py).
#
# Run:  set -a; . ~/Projects/jev-trycua-astra-blender/.env; set +a; \
#       macos-harness < scripts/walkthrough/walkthrough.py
#
# Outputs land in /tmp/mc-walkthrough/: frames/, frames/frames.jsonl,
# judge.jsonl, steps.log, app.log.

import json
import os
import signal
import time

REPO = "/Users/md/NashvilleAutomation/NostrFabrica/magic-carpet-chat"
BIN = os.path.join(REPO, "target", "debug", "magic-carpet-chat")
OUT = Path("/tmp/mc-walkthrough")
FRAMES = OUT / "frames"
FRAMES.mkdir(parents=True, exist_ok=True)
STEPS_LOG = OUT / "steps.log"
FRAMES_JSONL = FRAMES / "frames.jsonl"
JUDGE_JSONL = OUT / "judge.jsonl"
for stale in (FRAMES_JSONL, JUDGE_JSONL, STEPS_LOG):
    stale.write_text("")

PROC = None          # the app process
PID = None           # its pid, used as app= everywhere
FRAME_N = 0          # capture counter
PTR = None           # virtual pointer position, screenshot coords
POINTER_MOVE = False # rung-3 fallback flag, set by the click probe
LAST_FRAME = None    # last captured png path


def log(msg):
    line = "%.3f %s" % (time.time(), msg)
    print(line, flush=True)
    with open(STEPS_LOG, "a") as f:
        f.write(line + "\n")


def capture(record=True):
    """One window frame; recorded ones feed the assembled video."""
    global FRAME_N, LAST_FRAME
    FRAME_N += 1
    name = "%05d.png" % FRAME_N
    cap = mac.see(app=PID, path=str(FRAMES / name), show_pointer=True,
                  max_width=2400, max_height=2400)
    LAST_FRAME = str(FRAMES / name)
    if record:
        with open(FRAMES_JSONL, "a") as f:
            f.write(json.dumps({"file": name, "ts": time.time()}) + "\n")
    return cap


def glide(x, y, steps=10):
    """Move the virtual pointer to (x, y), filming every increment."""
    global PTR
    x0, y0 = PTR if PTR else (x, y)
    for i in range(1, steps + 1):
        ix = x0 + (x - x0) * i / steps
        iy = y0 + (y - y0) * i / steps
        mac.move(ix, iy, duration=0.04)
        capture()
    PTR = (x, y)


def linger(seconds):
    """Hold the current screen: frames at ~4 fps."""
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        capture()
        time.sleep(0.22)


def ocr(png, tsv=False):
    """tesseract on a 2x upscale. tsv=True returns the TSV text (tesseract
    writes it to <outbase>.tsv, not to stdout). This tesseract build rejects
    absolute input paths, so everything runs relative to the output dir."""
    subprocess.run(["ffmpeg", "-y", "-v", "error", "-i", png,
                    "-vf", "scale=iw*2:ih*2", "ocr.png"],
                   check=True, cwd=str(OUT))
    if tsv:
        subprocess.run(["tesseract", "ocr.png", "ocr_tsv", "tsv"],
                       capture_output=True, check=True, cwd=str(OUT))
        return (OUT / "ocr_tsv.tsv").read_text(errors="replace")
    r = subprocess.run(["tesseract", "ocr.png", "stdout"],
                       capture_output=True, cwd=str(OUT))
    return r.stdout.decode("utf-8", errors="replace")


def tsv_lines(png):
    """Group tesseract words into lines: [(line_text, [(cx,cy), ...])]."""
    rows = {}
    for ln in ocr(png, tsv=True).splitlines()[1:]:
        parts = ln.split("\t")
        if len(parts) < 12:
            continue
        try:
            left, top, w, h = (int(float(v)) for v in parts[6:10])
        except ValueError:
            continue
        text = parts[11].strip()
        if not text or float(parts[10]) < 0:
            continue
        key = (parts[2], parts[3], parts[4])  # block, par, line
        rows.setdefault(key, []).append(
            (text, left + w / 2, top + h / 2, left, top, w, h))
    lines = []
    for words in rows.values():
        words.sort(key=lambda w: w[3])
        joined = " ".join(w[0] for w in words)
        lines.append((joined, words))
    # Reading order: top to bottom.
    lines.sort(key=lambda item: min(w[2] for w in item[1]))
    return lines


def find_texts(png, needle):
    """All matches of `needle` as (cx, cy) in screenshot coords (2x / 2)."""
    tokens = needle.lower().split()
    hits = []
    for joined, words in tsv_lines(png):
        low = joined.lower()
        if " ".join(tokens) not in low:
            continue
        # Locate the word span covering the needle inside the line.
        covered = []
        if len(tokens) == 1:
            covered = [w for w in words if tokens[0] in w[0].lower()]
            if not covered:
                covered = words
        else:
            first = next((i for i, w in enumerate(words)
                          if tokens[0] in w[0].lower()), None)
            last = next((i for i in range(len(words) - 1, -1, -1)
                         if tokens[-1] in words[i][0].lower()), None)
            if first is not None and last is not None and last >= first:
                covered = words[first:last + 1]
            else:
                covered = words
        cx = sum(w[1] for w in covered) / len(covered) / 2
        cy = sum(w[2] for w in covered) / len(covered) / 2
        hits.append((cx, cy))
    return hits


def find_text(needle):
    """Topmost match centre, with two capture-and-retry rounds."""
    for attempt in range(3):
        hits = find_texts(LAST_FRAME, needle)
        if hits:
            hits.sort(key=lambda p: p[1])
            return hits[0]
        time.sleep(1)
        capture()
    log("OCR_MISS needle=%r" % needle)
    return None


def click_xy(x, y, why=""):
    glide(x, y)
    receipt = mac.click(x, y, app=PID, pointer_move=POINTER_MOVE)
    log("CLICK %s at (%.0f,%.0f) ax=%s contains=%s front=%s->%s" % (
        why, x, y, receipt.get("ax_at_point"),
        (receipt.get("window") or {}).get("contains_point"),
        receipt.get("frontmost_before"), receipt.get("frontmost_after")))
    capture()
    return receipt


def click_text(needle):
    pos = find_text(needle)
    if pos is None:
        return None
    return click_xy(pos[0], pos[1], why=repr(needle))


def judge(step, expected, forbidden=None):
    """OCR the latest frame, ask Jev, honour reobserve up to 5 rounds.
    Never raises: a failed judgement logs JUDGE_FAIL and the take goes on."""
    forbidden = forbidden or []
    result = None
    for round_n in range(5):
        text = ocr(LAST_FRAME)
        payload = {"step": step, "expected": expected,
                   "ocr": text, "forbidden": forbidden}
        try:
            r = subprocess.run(
                ["uv", "run", "--with", "typesafe-sdk", "python",
                 "scripts/walkthrough/judge.py", json.dumps(payload)],
                capture_output=True, text=True, cwd=REPO, timeout=120,
                env=os.environ)
            if r.returncode != 0 or not r.stdout.strip():
                log("JUDGE_ERR step=%s stderr=%s" % (step, r.stderr[-300:]))
                result = {"step": step, "next": "abort", "expected_visible": 0,
                          "error": r.stderr[-300:]}
            else:
                result = json.loads(r.stdout.strip().splitlines()[-1])
        except Exception as e:  # noqa: BLE001 - keep the take alive
            log("JUDGE_ERR step=%s exc=%r" % (step, e))
            result = {"step": step, "next": "abort", "expected_visible": 0,
                      "error": repr(e)}
        with open(JUDGE_JSONL, "a") as f:
            f.write(json.dumps(result) + "\n")
        log("JUDGE step=%s %s" % (step, json.dumps(result)))
        nxt = result.get("next")
        if nxt == "reobserve" and round_n < 4:
            time.sleep(1)
            capture()
            continue
        if nxt == "proceed" and result.get("expected_visible", 0) >= 0.5:
            return True
        log("JUDGE_FAIL step=%s result=%s" % (step, json.dumps(result)))
        return False
    log("JUDGE_FAIL step=%s exhausted reobserves" % step)
    return False


def wait_for(pred, timeout, interval, what):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            v = pred()
        except Exception:  # noqa: BLE001
            v = None
        if v:
            return v
        time.sleep(interval)
    log("TIMEOUT waiting for %s" % what)
    return None


def scroll_page(delta, steps=4, x=None, y=None):
    """Scroll the page area, filming between increments. gpui ignores
    window-addressed wheel events exactly like window-addressed clicks, so
    the wheel goes through the HID tap with the physical cursor warped over
    the content, then warped back. Positive `delta` scrolls DOWN the page
    (negative wheel deltaY reveals lower content)."""
    import ApplicationServices as AS
    cap = capture()
    h = cap.get("height", 1600)
    x = x if x is not None else 1500
    y = y if y is not None else h / 2
    glide(x, y, steps=4)
    pt = mac._screen_point(x, y, "screenshot", PID)
    home = AS.CGEventGetLocation(AS.CGEventCreate(None))
    AS.CGWarpMouseCursorPosition(pt)
    time.sleep(0.05)
    per = int(-delta / steps)
    for _ in range(steps):
        ev = AS.CGEventCreateScrollWheelEvent(
            mac._event_source, AS.kCGScrollEventUnitPixel, 1, per)
        AS.CGEventPost(AS.kCGHIDEventTap, ev)
        time.sleep(0.15)
        capture()
    AS.CGWarpMouseCursorPosition(home)


# ----------------------------------------------------------------- the take

# The app is launched by the shell wrapper before this script runs:
#   MC_SENTRY_DSN= target/debug/magic-carpet-chat > /tmp/mc-walkthrough/app.log 2>&1 &
# Resolve it by name — the binary registers as an unnamed-bundle app, so the
# pid that spawned it is not always what list_apps reports.
def app_pid():
    hits = [a["pid"] for a in mac.list_apps()
            if a.get("name") == "magic-carpet-chat"
            and "magic-carpet-chat" in (a.get("path") or "")]
    return hits[0] if len(hits) == 1 else None


PID = wait_for(app_pid, 60, 1.0, "magic-carpet-chat in list_apps")
if PID is None:
    raise SystemExit("no magic-carpet-chat process (or more than one)")
log("APP pid=%d" % PID)

try:
    wait_for(lambda: mac.windows(PID) or None, 30, 0.5, "app window")
    capture()

    # --- Dashboard settles: trust reads must have answered ---------------
    def settled():
        capture()
        t = ocr(LAST_FRAME).lower()
        return ("treasure map" in t or "needs attention" in t) \
            and "checking" not in t

    # Trust reads across three accounts can take a while (relay timeouts).
    wait_for(settled, 90, 2.0, "dashboard needs-attention settle")

    # --- Rung-1 probe: does a window-addressed click move this gpui app? --
    cap = capture()
    sx = cap["width"] / cap["bounds"]["width"] if cap.get("bounds") else 2.0
    sy = cap["height"] / cap["bounds"]["height"] if cap.get("bounds") else 2.0
    rail_x = 35 * sx
    acct_y = (cap["bounds"]["height"] - 82) * sy
    mac.click(rail_x, acct_y, app=PID)
    time.sleep(0.8)
    capture(record=False)
    # "Add an account"/"Manage" exist only on the Accounts screen; the
    # Dashboard's own "3 accounts" text makes bare "accounts" a false match.
    def on_accounts():
        t = ocr(LAST_FRAME).lower()
        return "add an account" in t or "manage" in t
    if on_accounts():
        log("RUNG1 ok: window-addressed click reached the Accounts screen")
    else:
        POINTER_MOVE = True
        log("RUNG1 miss: falling back to pointer_move=True for the take")
        mac.click(rail_x, acct_y, app=PID, pointer_move=True)
        time.sleep(0.8)
        capture(record=False)
        log("RUNG3 probe: %s" % on_accounts())
    mac.key("cmd+1", app=PID)
    time.sleep(1.0)
    capture()
    linger(3)

    judge("dashboard",
          "Magic Carpet Dashboard with a Needs attention panel listing rows "
          "titled 'Publish your Treasure Map' and 'Follow someone', each with "
          "a 'Fix →' button, and a Recent activity panel",
          ["Coming soon", "Couldn’t check your Treasure Map"])

    # --- Step 2: first "Publish your Treasure Map" row's Fix --------------
    row_hits = find_texts(LAST_FRAME, "Publish your Treasure Map")
    if not row_hits:
        log("OCR_MISS needle='Publish your Treasure Map'")
        click_text("Fix")  # last-resort: first Fix on screen
    else:
        row_hits.sort(key=lambda p: p[1])
        row_y = row_hits[0][1]
        fixes = find_texts(LAST_FRAME, "Fix")
        if not fixes:
            log("OCR_MISS needle='Fix'")
        else:
            fixes.sort(key=lambda p: abs(p[1] - row_y))
            click_xy(fixes[0][0], fixes[0][1], why="Fix on first map row")
    linger(5)
    judge("trust_not_located",
          "An Account page with tabs 'Identity & keys', 'Trust', 'Progress', "
          "'Wallet', 'Limits & defaults'; the Trust tab shows a 'Treasure Map' "
          "card with a 'Not located' pill, a note saying the kind-10040 "
          "Treasure Map could not be found and to go to Brainstorm, an "
          "'Open Brainstorm' link and a 'Check again' button; a 'Trusted "
          "Assertions' card; 'Minimum rank' with the value 2; 'Trust "
          "Determination'; 'Web of trust'",
          ["matches your assigned key", "Coming soon"])

    # --- Step 3: Check again ----------------------------------------------
    click_text("Check again")
    capture()  # catch the transient "Checking" if it renders
    linger(4)
    judge("check_again",
          "The same Trust tab, Treasure Map still 'Not located' after "
          "re-checking",
          ["matches your assigned key"])

    # --- Step 4: scroll the page ------------------------------------------
    scroll_page(600)
    linger(2)
    scroll_page(-600)
    linger(1)

    # --- Step 5: back to Accounts ------------------------------------------
    click_text("All accounts")
    linger(4)
    judge("accounts",
          "An 'Accounts' screen with three account cards, each with a name, "
          "an npub, a sats balance, and 'Manage' buttons; one card has an "
          "'active' pill and the others also have 'Switch' buttons",
          ["Coming soon"])

    # --- Step 6: the house issuer's card -> Trust tab ----------------------
    issuer = find_text("npub1s5a") or find_text("s5a654")
    if issuer is None:
        log("OCR_MISS needle='npub1s5a' — cannot pick the issuer card")
    else:
        manages = find_texts(LAST_FRAME, "Manage")
        if not manages:
            log("OCR_MISS needle='Manage'")
        else:
            manages.sort(key=lambda p: abs(p[1] - issuer[1]))
            click_xy(manages[0][0], manages[0][1],
                     why="Manage on issuer card")
    linger(6)
    judge("trust_located",
          "The Trust tab for the house issuer: Treasure Map card with a "
          "'Located' pill, 'Published' with a date, 'Found on' listing "
          "relays, 'Score entries' showing 30382:rank and 30382:followers, "
          "'Rank provider' hex starting c63c30b4, 'Provider relay' "
          "wss://scores.brainstorm.world, 'Brainstorm key' 'matches your "
          "assigned key', and a 'View raw event' toggle; Trusted Assertions "
          "with groups 'From your score provider' and 'From the issuer's "
          "score provider' showing rank pills",
          ["Not located", "Couldn"])

    # --- Step 7: raw event --------------------------------------------------
    click_text("View raw event")
    linger(4)
    judge("raw_event",
          "A monospace JSON block is visible showing an event with kind "
          "10040, a pubkey, created_at, tags with 30382:rank, and a sig",
          [])

    # --- Step 8: scroll to the bottom ---------------------------------------
    for _ in range(3):
        scroll_page(500)
        linger(1)
    linger(3)
    judge("web_of_trust",
          "The 'Web of trust' card with 'Following' and 'Verified followers' "
          "tiles and a link 'See the full graph on Brainstorm'",
          [])

    # --- Step 9: tabs -------------------------------------------------------
    # Scroll distance tracks wheel-event count, not delta, so climb in short
    # bursts until the tab bar is back on screen.
    for _ in range(6):
        scroll_page(-800, steps=6)
        if "identity" in ocr(LAST_FRAME).lower():
            break
    linger(1)
    click_text("Progress")
    linger(2)
    judge("other_tab",
          "The Progress tab selected, showing the text 'Coming soon.'",
          [])
    click_text("Trust")
    linger(1)

    # --- Step 10: home ------------------------------------------------------
    mac.key("cmd+1", app=PID)
    linger(3)
    judge("back_to_dashboard",
          "The Dashboard again with the Needs attention rows", [])
    linger(1)
finally:
    if PID is not None:
        try:
            os.kill(PID, signal.SIGTERM)
            log("KILL pid=%d" % PID)
        except ProcessLookupError:
            pass
    log("DONE frames=%d" % FRAME_N)
