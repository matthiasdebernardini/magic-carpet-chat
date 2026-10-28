#!/usr/bin/env python3
"""Find the app's controls in the AT-SPI tree by their stable accessible id
(docs/a11y-ids.md) and press, measure or read them. Box.setup() copies this
into the mc container; it runs there with DBUS_SESSION_BUS_ADDRESS set to the
e2e session bus (see suite.A11Y_ENV). Needs python3-gi and gir1.2-atspi-2.0.

    atspi.py press  <id> [--name S] [--within WxH]
                                       Action "click" on the one match
    atspi.py rect   <id> [--name S] [--within WxH]
                                       its extents (window-relative)
    atspi.py name   <id> [--name S]    its role, name and description
    atspi.py exists <id> [--name S]    how many nodes match (never an error)
    atspi.py wait   <id> <secs> [--name S] [--has S ...]
                                       until one node matches (and its name
                                       holds one of the --has strings)
    atspi.py dump                      every node that has an id
    atspi.py selfcheck                 on the empty-store setup screen: the
                                       choose card's ids, then Create a new
                                       account opens its card and Back returns

<id> is an exact accessible id, or ~regex for a full-match pattern
(~rail-account-[0-9a-f]{64}). --name keeps nodes whose name is exactly S.
Labels (role label) reach AT-SPI with an empty name and no Text interface, so
their text cannot be read here: match a Label by id only. Extents are
window-relative (Atspi.CoordType.WINDOW, what xdotool --window takes). --within fails
unless the node's centre lies inside a window of that size: gpui presses a
node by clicking its centre and has no scroll-into-view.
Only the app owned by `pidof -s magic-carpet-chat-e2e` is searched.
Prints one JSON object. Exit 1 with {"error": ...} unless exactly one node
matches: two matches mean the wrong element could be pressed.
"""
import json, re, subprocess, sys, time

import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, GLib  # noqa: E402

APP = "magic-carpet"   # the app's AT-SPI name is its binary name (magic-carpet-chat-e2e on the box)
BIN = "magic-carpet-chat-e2e"   # pidof name of the binary under test (suite.APP_BIN); the demo's binary is never read


class Fail(Exception):
    pass


def app_pid():
    """The pid of the binary under test (pidof -s), or None."""
    r = subprocess.run(["pidof", "-s", BIN], capture_output=True, text=True)
    return int(r.stdout.split()[0]) if r.returncode == 0 and r.stdout.split() else None


def apps():
    """The app's AT-SPI application node(s): named like the app and owned
    by the pid of the binary under test, never another app on the bus."""
    pid = app_pid()
    desk = Atspi.get_desktop(0)
    out = []
    for i in range(desk.get_child_count()):
        a = desk.get_child_at_index(i)
        if a is None or APP not in (a.get_name() or "").lower():
            continue
        try:
            if pid is not None and a.get_process_id() == pid:
                out.append(a)
        except GLib.Error:
            continue
    return out


def walk(node):
    """Every node under `node`, depth first. Nodes that vanish mid-walk are skipped."""
    stack = [node]
    while stack:
        n = stack.pop()
        yield n
        try:
            kids = [n.get_child_at_index(i) for i in range(n.get_child_count())]
        except GLib.Error:
            continue
        stack.extend(k for k in reversed(kids) if k is not None)


def acc_id(n):
    try:
        return n.get_accessible_id() or ""
    except GLib.Error:
        return ""


def info(n):
    d = {"id": acc_id(n), "role": n.get_role_name(), "name": n.get_name() or "",
         "description": n.get_description() or ""}
    try:
        d["showing"] = n.get_state_set().contains(Atspi.StateType.SHOWING)
    except GLib.Error:
        pass
    comp = n.get_component_iface()
    if comp is not None:
        e = comp.get_extents(Atspi.CoordType.WINDOW)
        d["rect"] = {"x": e.x, "y": e.y, "w": e.width, "h": e.height}
    return d


def inside(n, within):
    """Fail unless the node's centre lies inside a window of size `within`
    ("WxH"). gpui has no scroll-into-view: a control below the fold still
    reports extents, and a press or click there would land on something else."""
    if not within:
        return
    w, h = (int(v) for v in within.lower().split("x"))
    r = info(n).get("rect")
    if r is None:
        raise Fail(f"{acc_id(n)} has no Component extents")
    cx, cy = r["x"] + r["w"] / 2, r["y"] + r["h"] / 2
    if r["w"] <= 0 or r["h"] <= 0 or not (0 <= cx < w and 0 <= cy < h):
        raise Fail(f"{acc_id(n)} is not inside the {within} window: {r}")


def find(pattern, name=None, fresh=False):
    """Nodes whose id is `pattern` (or full-matches ~regex) and, with `name`,
    whose name is exactly that. A wait (`fresh`) also waits for the app."""
    rx = re.compile(pattern[1:]) if pattern.startswith("~") else None
    hits = []
    found = apps()
    if not found and not fresh:
        raise Fail(f"no AT-SPI app *{APP}* with the pid of {BIN} on this bus (is it running with this DBUS_SESSION_BUS_ADDRESS?)")
    for app in found:
        if fresh:
            app.clear_cache()   # a long wait must see the tree as it is now
        for n in walk(app):
            aid = acc_id(n)
            if not aid or not (rx.fullmatch(aid) if rx else aid == pattern):
                continue
            if name is not None and (n.get_name() or "") != name:
                continue
            hits.append(n)
    return hits


def one(pattern, name=None):
    hits = find(pattern, name)
    if len(hits) != 1:
        what = pattern + (f" named {name!r}" if name is not None else "")
        raise Fail(f"{len(hits)} nodes match {what}; need exactly one"
                   + (f": {[acc_id(h) for h in hits][:8]}" if hits else ""))
    return hits[0]


def press(n):
    """Run the node's one action ("click"). Read it first: the press often
    replaces the node, and a gone node answers every call with an error."""
    d = info(n)
    act = n.get_action_iface()
    if act is None or act.get_n_actions() < 1:
        raise Fail(f"{d['id']} has no action to press")
    if not act.do_action(0):
        raise Fail(f"{d['id']}: do_action(0) returned false")
    return {**d, "pressed": True}


def wait(pattern, secs, name=None, has=()):
    end = time.time() + secs
    while True:
        hits = find(pattern, name, fresh=True)
        if len(hits) > 1:
            raise Fail(f"{len(hits)} nodes match {pattern}; need exactly one: {[acc_id(h) for h in hits][:8]}")
        if hits:
            d = info(hits[0])
            if not has or any(h in d["name"] for h in has):
                return {**d, "waited": round(secs - (end - time.time()), 1)}
        if time.time() >= end:
            seen = f"; last name {info(hits[0])['name']!r}" if hits else ""
            raise Fail(f"{pattern} did not appear{' holding ' + repr(list(has)) if has else ''} within {secs} s{seen}")
        time.sleep(0.5)


def selfcheck():
    """Read-only apart from opening and closing the create card: never presses
    setup-create-go, so no account or wallet is made."""
    out = {}
    for aid in ("setup-title-choose", "setup-create", "setup-paste", "setup-browse"):
        out[aid] = info(one(aid))["role"]
    press(one("setup-create"))
    for aid in ("setup-title-create", "setup-name", "setup-create-go", "setup-back"):
        wait(aid, 10)
    r = info(one("setup-name"))["rect"]
    if r["w"] <= 0 or r["h"] <= 0:
        raise Fail(f"setup-name has no extents: {r}")
    out["setup-name rect"] = r
    press(one("setup-back"))
    wait("setup-title-choose", 10)
    return {"ok": True, "checked": out}


def main(argv):
    args, opts = [], {"name": None, "has": [], "within": None}
    it = iter(argv)
    for a in it:
        if a == "--name":
            opts["name"] = next(it)
        elif a == "--within":
            opts["within"] = next(it)
        elif a == "--has":
            opts["has"].append(next(it))
        else:
            args.append(a)
    if not args:
        raise Fail(__doc__.strip().split("\n\n")[1])
    cmd, rest = args[0], args[1:]
    if cmd == "dump":
        return {"nodes": [info(n) for app in apps() for n in walk(app) if acc_id(n)]}
    if cmd == "selfcheck":
        return selfcheck()
    if not rest:
        raise Fail(f"{cmd} needs an id")
    if cmd == "press":
        n = one(rest[0], opts["name"])
        inside(n, opts["within"])
        return press(n)
    if cmd in ("rect", "name"):
        n = one(rest[0], opts["name"])
        if cmd == "rect":
            inside(n, opts["within"])
        d = info(n)
        if cmd == "rect" and "rect" not in d:
            raise Fail(f"{rest[0]} has no Component extents")
        return d
    if cmd == "exists":
        return {"count": len(find(rest[0], opts["name"]))}
    if cmd == "wait":
        return wait(rest[0], float(rest[1]) if len(rest) > 1 else 10, opts["name"], opts["has"])
    raise Fail(f"unknown command {cmd!r}")


if __name__ == "__main__":
    try:
        print(json.dumps(main(sys.argv[1:])))
    except (Fail, GLib.Error) as e:   # GLib.Error: a node went away mid-call
        print(json.dumps({"error": str(e)}))
        sys.exit(1)
