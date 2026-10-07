"""Bounded computer-use loop: Cua observes and acts, Jev decides, Fable authored the tables.

One step = goal + candidate table (application-owned, complete actions) + a
Noul postcondition. Jev picks ONE candidate id; unknown ids fail closed; the
postcondition is re-observed after every action. No TYPESAFE_API_KEY -> mock
(first non-abstain candidate), so the loop runs credential-free.
"""
import asyncio, json, os, subprocess, sys, time
from dataclasses import dataclass, field

IP = os.environ.get("MC_SANDBOX_IP", "172.17.0.2")
# Target-neutral paths: the cua box keeps ~/mc; elsewhere MC_E2E_DIR points at
# the run dir (a mode-0700 dir under /tmp on omac).
E2E_DIR = os.environ.get("MC_E2E_DIR", os.path.expanduser("~/mc"))
RUNS = os.path.join(E2E_DIR, "runs"); os.makedirs(RUNS, exist_ok=True)
LOG = open(os.path.join(RUNS, time.strftime("%Y%m%d-%H%M%S") + ".jsonl"), "a")
KEYS = os.path.join(E2E_DIR, "keys.json")

def log(**ev):
    ev["t"] = round(time.time(), 3); LOG.write(json.dumps(ev) + "\n"); LOG.flush(); print(json.dumps(ev)[:400], flush=True)

def ocr(png: bytes) -> str:
    open("/tmp/shot.png", "wb").write(png)
    from PIL import Image   # grayscale + 2x upscale: tesseract misses small text on the dark UI otherwise
    im = Image.open("/tmp/shot.png").convert("L"); im.resize((im.width * 2, im.height * 2), Image.LANCZOS).save("/tmp/ocr.png")
    out = subprocess.run(["tesseract", "/tmp/ocr.png", "-", "--psm", "3"], capture_output=True, text=True).stdout
    return "\n".join(" ".join(l.split()) for l in out.splitlines() if l.strip())

@dataclass
class Cand:
    id: str; desc: str; do: list = field(default_factory=list)   # do = [(kind, arg), ...]

@dataclass
class Step:
    goal: str; cands: list; done: str; tries: int = 3; settle: float = 2.0
    cues: tuple = ()   # phrases whose literal presence in the OCR is handed to Jev as evidence

def evidence(screen, cues):
    return {"screen_lines": screen.split("\n")[:80], "phrases_found": [c for c in cues if c.lower() in screen.lower()],
            "phrases_missing": [c for c in cues if c.lower() not in screen.lower()]}

ABSTAIN = Cand("abstain", "Stop without acting: none of the actions is safe for what the screen shows.")
DONE = Cand("already-done", "Do nothing: the screen already shows the goal state, so no action is needed.", [("wait", 0.5)])
REOBSERVE = Cand("reobserve", "Take a fresh screenshot and wait; the screen may still be loading.", [("wait", 3)])

class Jev:
    def __init__(self):
        self.live = bool(os.environ.get("TYPESAFE_API_KEY"))
        if self.live:
            from typesafe_sdk import TypeSafeClient
            self.c = TypeSafeClient()
    def choose(self, goal, screen, cands, history, cues=()):
        if not self.live:
            pick = next(c.id for c in cands if c.id not in ("abstain", "reobserve"))
            return pick, 1.0, {"mock": True}
        from typesafe_sdk import Choice
        r = self.c.system_one(
            state={"goal": goal, "observation": evidence(screen, cues), "history": history[-6:]},
            questions={"action": Choice(instructions="Which complete action should the driver run next to reach the goal?",
                                         criteria={c.id: c.desc for c in cands})})
        a = r.answers["action"]; return a.choice, a.confidence, dict(a.probabilities)
    def done(self, cond, screen, cues=()):
        if not self.live: return 1.0
        from typesafe_sdk import Noul
        r = self.c.system_one(state={"observation": evidence(screen, cues)}, questions={"done": Noul(instructions=cond)})
        return r.answers["done"].noul

def secret(arg):
    """'$claimant_sec' -> keys.json['claimant']['sec']; resolved at act time, never logged."""
    who, what = arg[1:].lower().split("_", 1)
    return json.load(open(KEYS))[who][what]

class Driver:
    """The cua-box driver: xdotool and the Sandbox mouse inside the mc
    container on DISPLAY=:1. The omac driver lives in suite.py."""
    def __init__(self, sb): self.sb = sb
    async def x(self, cmd): return await self.sb.shell.run(f"DISPLAY=:1 {cmd}", timeout=60)
    async def act(self, kind, arg):
        if kind == "key":     await self.x(f"xdotool key --clearmodifiers {arg}")
        elif kind == "type":  await self.x("xdotool type --delay 20 " + json.dumps(secret(arg) if arg.startswith("$") else arg))
        elif kind == "click": await self.sb.mouse.click(*arg)
        elif kind == "click_text":
            # arg: "phrase" or {"text": "phrase", "near": "anchor phrase"}.
            text = arg["text"] if isinstance(arg, dict) else arg
            near = arg.get("near") if isinstance(arg, dict) else None
            img = await self.sb.screenshot()
            data = img if isinstance(img, (bytes, bytearray)) else getattr(img, "data", None) or img
            open("/tmp/shot.png", "wb").write(data)
            from PIL import Image
            im = Image.open("/tmp/shot.png").convert("L")
            im.resize((im.width * 2, im.height * 2), Image.LANCZOS).save("/tmp/ocr.png")
            tsv = subprocess.run(["tesseract", "/tmp/ocr.png", "-", "--psm", "3", "tsv"],
                                 capture_output=True, text=True).stdout
            lines = {}
            for ln in tsv.splitlines()[1:]:
                p = ln.split("\t")
                if len(p) < 12 or not p[11].strip():
                    continue
                lines.setdefault((p[2], p[3], p[4]), []).append(
                    (p[11], float(p[6]) + float(p[8]) / 2, float(p[7]) + float(p[9]) / 2))
            # [(joined, [(word, cx, cy), ...])] in reading order
            lines = sorted(lines.values(), key=lambda ws: min(w[2] for w in ws))
            def match(needle):
                out = []
                for ws in lines:
                    joined = " ".join(w[0] for w in ws).lower()
                    i = joined.find(needle.lower())
                    if i < 0:
                        continue
                    off, sel = 0, []
                    for w, cx, cy in ws:
                        w0, w1 = off, off + len(w)
                        off += len(w) + 1
                        if w0 < i + len(needle) and w1 > i:
                            sel.append((cx, cy))
                    if sel:
                        out.append((sum(x for x, _ in sel) / len(sel) / 2,
                                    sum(y for _, y in sel) / len(sel) / 2))
                return out
            hits = match(text)
            if not hits:
                preview = [" ".join(w[0] for w in ws) for ws in lines][:40]
                raise RuntimeError(f"click_text: {text!r} not on screen; OCR lines: {preview}")
            if near:
                anchor = match(near)
                if not anchor:
                    preview = [" ".join(w[0] for w in ws) for ws in lines][:40]
                    raise RuntimeError(f"click_text: near {near!r} not on screen; OCR lines: {preview}")
                ay = anchor[0][1]
                hits.sort(key=lambda p: abs(p[1] - ay))
            x, y = hits[0]
            await self.sb.mouse.click(x, y)
            return f"click_text {text!r} -> ({x:.0f},{y:.0f})"
        elif kind == "scroll":
            # gpui scrolls per wheel event, not per delta: park the pointer
            # over the window centre, then click the wheel button repeatedly.
            img = await self.sb.screenshot()
            data = img if isinstance(img, (bytes, bytearray)) else getattr(img, "data", None) or img
            open("/tmp/shot.png", "wb").write(data)
            from PIL import Image
            im = Image.open("/tmp/shot.png")
            cx, cy = im.width // 2, im.height // 2
            await self.sb.mouse.move(cx, cy)
            btn = "5" if arg == "down" else "4"
            await self.x(f"xdotool click --repeat 12 --delay 60 {btn}")
        elif kind == "wait":  await asyncio.sleep(arg)
        elif kind == "shell": await self.x(arg)
        elif kind == "js":    # run JS in the Chromium page via cua-driver's page tool (CDP)
            pid = (await self.x("pgrep -o chromium")).stdout.strip()
            wid = (await self.x("xdotool search --onlyvisible --class chromium | tail -1")).stdout.strip()
            # braces: block scope, so a `const` in one script never collides with the next
            args = {"pid": int(pid), "window_id": int(wid), "action": "execute_javascript", "javascript": "{" + arg + "}"}
            r = await self.x("cua-driver call page --json " + json.dumps(json.dumps(args)))
            return r.stdout[:800]
        elif kind == "focus":
            r = await self.x(f"xdotool search --onlyvisible --name {json.dumps(arg)} | tail -1")
            await self.x(f"xdotool windowactivate --sync {r.stdout.strip()} mousemove 20 740")  # park the pointer: hover tooltips hide the header
        else: raise ValueError(kind)
    async def screen(self): return ocr(await self.sb.screenshot())

async def run(steps, min_conf=0.4):
    from cua import Sandbox   # box-only: the omac target never imports cua
    sb = await Sandbox.connect("mc", http_url=f"http://{IP}:8000")
    d, jev, hist = Driver(sb), Jev(), []
    log(event="start", live=jev.live, steps=len(steps))
    ok = True
    for i, s in enumerate(steps):
        cands = s.cands + [DONE, REOBSERVE, ABSTAIN]
        for attempt in range(s.tries):
            screen = await d.screen()
            pick, conf, probs = jev.choose(s.goal, screen, cands, hist, s.cues)
            c = next((c for c in cands if c.id == pick), None)
            log(event="choice", step=i, goal=s.goal, attempt=attempt, pick=pick, conf=conf, probs=probs, screen=" ".join(screen.split())[:300])
            if c is not None and c.id == "reobserve":   # low confidence here just means "look again"
                await asyncio.sleep(3); continue
            # already-done is a no-op like reobserve: confidence gates the
            # *choice*, and the postcondition Noul is the real verdict — a
            # sub-threshold "already done" must not abort the whole take.
            # Gate on the picked action's own probability and the abstain mass,
            # not the scalar confidence: reobserve/abstain always sit in the
            # menu and soak up mass, so confidence reads ~0.35 even when the
            # right action is the clear argmax.
            p_pick, p_abst = probs.get(pick, conf), probs.get("abstain", 0.0)
            weak = c is not None and c.id != "already-done" and (p_pick < min_conf or p_abst >= 0.3)
            if c is None or c.id == "abstain" or weak:
                log(event="halt", step=i, reason="unknown id" if c is None else c.id, p_pick=p_pick, p_abstain=p_abst); ok = False; break
            for kind, arg in c.do:
                out = await d.act(kind, arg)
                if out: log(event="act", step=i, kind=kind, out=out[:300])
            await asyncio.sleep(s.settle)
            screen = await d.screen(); p = jev.done(s.done, screen, s.cues)
            hist.append({"step": i, "action": c.id, "done_p": p})
            log(event="verify", step=i, cond=s.done, p=p, screen=" ".join(screen.split())[:300])
            if p >= 0.6: break
        else:
            log(event="halt", step=i, reason="postcondition never met"); ok = False
        if not ok: break
    log(event="end", ok=ok); await sb.disconnect()
    ship = os.path.join(E2E_DIR, "ship.py")
    if os.path.exists(ship):
        subprocess.run([sys.executable, ship, LOG.name], check=False)
    return ok

if __name__ == "__main__":
    import importlib
    mod = importlib.import_module(sys.argv[1] if len(sys.argv) > 1 else "demo_steps")
    sys.exit(0 if asyncio.run(run(mod.STEPS)) else 1)
