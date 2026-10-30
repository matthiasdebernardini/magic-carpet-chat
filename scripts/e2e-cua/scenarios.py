"""The scenarios. Each is `async def x(sc)`; register it in SCENARIOS.

start(sc, name, ...)     relaunch the app on a fresh MC_STORE_DIR, with the
                         accessibility bus and the preflight
sc.box.start_fake(mode)  fresh fake instance + relay in the given mode
sc.step(Step, must_act)  one agent.py step: Jev picks a candidate, judges `done`
sc.check / check_tree    hard checks; every scenario needs at least one
sc.log_after(marker)     fake-server log lines after sc.box.mark(marker)

Every candidate acts through the app's accessibility tree, by the ids in
docs/a11y-ids.md (suite.AppDriver), and every hard check on the app reads the
tree. Jev still judges each step from the screenshot.

Candidate actions are complete; none of them may create a wallet, remove an
account for real, or publish anywhere but the fake server (P, R and Y excepted).
"""
import asyncio, time
from agent import Step, Cand
from suite import shot_bytes

FOCUS = ("focus", "Magic Carpet")   # before "key" and "type": the app's window takes them
PARK = ("park", None)   # pointer to the rail's foot: hover tooltips hide the header
# Account switches keep the screen; the sidebar's Bounties tab is the way to
# Bounties, and bounties-new carries the issuer/claimant wording.
TO_BOUNTIES = [("wait_id", "nav-bounties"), ("press", "nav-bounties"), PARK]
BOUNTIES = Cand("open-bounties", "Press the sidebar's Bounties tab.", TO_BOUNTIES + [("wait_id", "bounties-title")])
DASHBOARD = Cand("open-dashboard", "Press the sidebar's Dashboard tab.", [("press", "nav-dashboard"), ("wait_id", "dash-heading"), PARK])
NEW_FORM = Cand("new-bounty-form", "Press the '+ New DList + bounty' button in the Bounties header.",
                [("press", "bounties-new"), ("wait_id", "bounty-form"), PARK])
HOUSE_CUES = ("New DList + bounty", "Submit a bounty claim", "Bounties")
ISSUER = ("bounties-new", "New DList + bounty")       # the header button as the house sees it
CLAIMANT = ("bounties-new", "Submit a bounty claim")  # and as anyone else does
# A bounty row: any id under bounty-row- but its pills' (fake ids are not UUIDs).
ROW = "~bounty-row-(?!.*-(?:status|sats|paid|auto-pay|qualify)$).+"
# Recent activity rows: dash-activity-<at>-<hash>, "-2", "-3" for repeats.
ACTIVITY = "~dash-activity-[0-9]+-[0-9a-f]+(?:-[0-9]+)?"
STILL_SENDING = "Still sending. Wait for it to finish."


def tab(sc, who):
    """The rail tab of keys.json's `who` (house, claimant)."""
    return f"rail-account-{sc.suite.keys[who]['pub']}"


def house(sc):
    return Cand("press-house-tab", "Press the house issuer's account tab in the left rail, then the sidebar's Bounties tab.",
                [("press", tab(sc, "house"))] + TO_BOUNTIES + [("wait_id", {"id": "bounties-new", "has": [ISSUER[1]], "soft": True})])


async def start(sc, name, ready="rail-add-account", **kw):
    """The accessibility bus up, then the app launched on it and the
    preflight passed. The store, or None (the checks say why)."""
    if not sc.check("accessibility bus up (python3-gi, at-spi, IsEnabled)", sc.box.a11y_up()):
        return None
    store = sc.box.launch(name, sc.suite.run_id, **kw)
    return store if await a11y_preflight(sc, ready) else None


async def a11y_preflight(sc, ready="rail-add-account"):
    """Right after a launch: xwininfo is there, the app's window is
    measurable, and its tree is on the bus (`ready` is in it: the rail's +,
    or the choose card's Create on a first launch)."""
    who = f" ({sc.app.name})" if sc.app else ""
    d = sc.suite.driver
    from suite import dx
    ok = sc.check(f"xwininfo in mc{who}", dx("command -v xwininfo").returncode == 0)
    try:
        box = await d.window_box()
        ok = sc.check(f"app window measured{who}", True, str(box)) and ok
    except RuntimeError as e:
        ok = sc.check(f"app window measured{who}", False, str(e)) and ok
    try:
        await d.act("wait_id", {"id": ready, "secs": 20})
        ok = sc.check(f"app tree on the accessibility bus ({ready}){who}", True) and ok
    except RuntimeError as e:
        ok = sc.check(f"app tree on the accessibility bus ({ready}){who}", False, str(e)) and ok
    # A scenario needs a hard check of its own, after these. Only the first
    # launch's: Y relaunches, and its earlier checks still count.
    if not sc.checks_from:
        sc.checks_from = len(sc.checks)
    return ok


async def act(sc, name, actions):
    """Driver actions outside a Jev step. True, or a failed check `name`
    with the reason (a press that found no node, say)."""
    try:
        for kind, arg in actions:
            await sc.suite.driver.act(kind, arg)
        return True
    except Exception as e:
        return sc.check(name, False, f"{type(e).__name__}: {e}")


BOUNTY_FIELDS = ("item-singular", "item-plural", "description", "criteria", "amount", "cap", "min-rank")


def type_fields(pairs):
    """Each (field, text): focus bounty-form-<field> by its id, replace its
    text. A retry replaces an earlier attempt's text instead of appending."""
    do = [FOCUS]
    for fid, text in pairs:
        do += [("click_id", f"bounty-form-{fid}"), ("key", "ctrl+a"), ("type", text), ("wait", 0.3)]
    return do


def fill_fields(name, cap="200"):
    """The seven bounty fields: reward 100, `cap`, min rank 2. No Enter: the
    caller's Enter, in the last field, submits (in the others it moves on)."""
    words = name.replace("-", " ")
    return type_fields(zip(BOUNTY_FIELDS, (name, name + "s", f"{words}s for the e2e suite",
                                           f"A {words} name, one per claim", "100", cap, "2")))


def fill(name, cap="200", wait=False):
    """Fill and submit. wait: until the new bounty's detail pane names it."""
    do = fill_fields(name, cap) + [("key", "Return"), ("stamp", "submitted")]
    if wait:
        do.append(("wait_id", {"id": "bounty-detail-title", "name": name, "secs": 45, "soft": True}))
    return Cand("fill-and-publish",
                f"Click each of the seven fields and type it: singular '{name}', plural, description, criteria, reward 100, cap {cap}, min rank 2; Enter in the last field publishes.",
                do)


async def open_house_form(sc):
    """Bounties screen, house selected, New DList + bounty form open."""
    ok = await sc.step(Step("Show the Bounties screen with the house issuer selected (its header button reads 'New DList + bounty'; the claimant would see 'Submit a bounty claim').",
                            [house(sc), BOUNTIES], "The Bounties screen is shown and its header button reads 'New DList + bounty'.", settle=2, cues=HOUSE_CUES),
                       label="house on bounties")
    if not (ok and sc.check_tree("house selected", [ISSUER], [CLAIMANT])):
        return False
    return await sc.step(Step("Open the new bounty form as the house issuer.", [NEW_FORM],
                              "A form titled 'New DList + bounty' is open with an 'Item (singular)' field.", settle=2,
                              cues=("New DList + bounty", "Item (singular)", "Cancel")), label="open form")


async def submit_form(sc, name, done, cues, settle=3.0):
    return await sc.step(Step(f"Fill the bounty form for '{name}' and submit it.", [fill(name)], done, tries=1, settle=settle, cues=cues),
                         must_act=True, label=f"fill {name}")


# ---------------------------------------------------------------- A
async def scenario_a(sc):
    sc.box.start_fake({"create": "hang", "hang_secs": 45})
    sc.store = await start(sc, "A")
    if not sc.store or not await open_house_form(sc):
        return
    sc.box.mark("A submit")
    # Submit and the four attempts in one candidate: all of it must land
    # inside the app's 20 s client timeout, and an OCR pass takes ~5 s here.
    # A snap after each attempt is the per-action evidence; the feed's three
    # amber lines below are the per-action verdict. The switch is the
    # claimant's rail tab: super+] and the tab both go through set_account.
    await sc.step(Step(
        "Submit a 'hang-city' bounty; while its create request is in flight, try in turn: switch account (the claimant's rail tab), add an account (+), remove the active account (two presses), and Escape.",
        [Cand("submit-then-try-all-four",
              "Fill and submit the form, then press the claimant's tab in the rail, the '+' in the rail, 'Remove this account' twice in the sidebar, then Escape.",
              fill_fields("hang-city") + [
                  ("key", "Return"), ("stamp", "submitted"),
                  ("wait", 1.5), ("snap", "submitted"),
                  ("press", tab(sc, "claimant")), ("wait", 0.5), ("snap", "after switch"),
                  ("press", "rail-add-account"), ("wait", 0.5), PARK, ("snap", "after plus"),
                  ("press", "sidebar-remove-account"), ("wait", 0.5), ("press", "sidebar-remove-account"), ("wait", 0.5), PARK,
                  ("snap", "after remove x2"),
                  ("key", "Escape"), ("stamp", "attempts done"), ("wait", 0.5)])],
        "The bounty form is still open and still reads 'Publishing…'; the header button still reads 'New DList + bounty' (the house is still selected); no account setup screen.",
        tries=1, settle=0.5, cues=("Publishing", "New DList + bounty", "Submit a bounty claim", "Your Nostr account")),
        must_act=True, label="submit and in-flight attempts", second_look=False)   # a later look would be past the timeout
    t_submit = sc.stamps.get("submitted", time.time())
    done_at = sc.stamps.get("attempts done", time.time())
    sc.check("the attempts ran inside the client timeout", done_at - t_submit < 18, f"{done_at - t_submit:.1f} s after submit")
    if sc.check_tree("switch, setup, remove and esc all refused",
                     [("bounty-form-status", "Publishing"), ("bounty-form-cancel", None), ISSUER],
                     [CLAIMANT, ("~setup-.+", None)]):
        sc.accept_step("submit and in-flight attempts", "Publishing, the form's Cancel and the issuer's header button in the tree after the attempts")
    # The client timeout is 20 s; give it 26 from the submit.
    await asyncio.sleep(max(0, t_submit + 26 - time.time()))
    # After the timeout the form reads "Checking the list…" until the
    # refetch lands (7b1dad9), then unlocks with the hint.
    await sc.step(Step("Wait for the create request to time out and the list check after it.", [Cand("wait", "Wait 5 seconds.", [("wait", 5)])],
                       "The bounty form is still open, 'Publishing…' is gone, and an error line says the bounty may exist already.",
                       tries=3, settle=1, cues=("Publishing", "Checking the list", "may exist already")), label="timeout unlocks")
    sc.wait_tree("bounty-form-error", 20, has="may exist already")
    sc.check_tree("form unlocked after the client timeout and the list check",
                  [("bounty-form-cancel", None), ("bounty-form-error", "may exist already")],
                  [("bounty-form-status", "Publishing"), ("bounty-form-status", "Checking the list")])
    await sc.step(Step("Open the Dashboard to read the Recent activity feed.", [DASHBOARD],
                       "The Recent activity panel lists 'Still sending. Wait for it to finish.' at least once.",
                       settle=2, cues=("Still sending", "Recent activity")), label="activity feed")
    n = sc.tree(ACTIVITY, name=STILL_SENDING)
    sc.check("amber 'Still sending' line for switch, setup and remove (3x)", n >= 3, f"{n} activity rows")
    posts = [l for l in sc.log_after("A submit") if l.startswith("POST /api/bounties")]
    sc.check("exactly one create request went out", len(posts) == 1, posts)
    sc.check("the active account was not removed", sc.box.accounts_in_store(sc.store) == 2, f"{sc.box.accounts_in_store(sc.store)} accounts in store")


# ---------------------------------------------------------------- B, C
async def scenario_b(sc):
    sc.box.start_fake({"create": "502"})
    if not await start(sc, "B") or not await open_house_form(sc):
        return
    sc.box.mark("B submit")
    await submit_form(sc, "gateway-city", "The form shows an error saying the bounty may exist already and to check the list before retrying.",
                      ("may exist already", "Check the bounty list"))
    # "Checking the list…" first, then the hint once the refetch lands.
    sc.wait_tree("bounty-form-error", 20, has="may exist already")
    sc.check_tree("'may exist already' hint shown, form unlocked", [("bounty-form-error", "may exist already")],
                  [("bounty-form-status", "Checking the list"), ("bounty-form-status", "Publishing")])
    lines = sc.log_after("B submit")
    post = next((i for i, l in enumerate(lines) if l.startswith("POST /api/bounties")), None)
    refetch = post is not None and any(l.startswith("GET /api/bounties issuer") for l in lines[post + 1:])
    sc.check("GET /api/bounties refetch after the 502", refetch, lines[-6:])


async def scenario_c(sc):
    sc.box.start_fake({"create": "400"})
    if not await start(sc, "C") or not await open_house_form(sc):
        return
    sc.box.mark("C submit")
    await submit_form(sc, "refused-city", "The form shows an error about amountSats and no 'may exist already' hint.", ("amountSats", "may exist already"))
    sc.wait_tree("bounty-form-error", 10, has="amountSats")
    sc.check_tree("400 shows the refusal, no hint", [("bounty-form-error", "amountSats")], [("bounty-form-error", "may exist already")])
    lines = sc.log_after("C submit")
    post = next((i for i, l in enumerate(lines) if l.startswith("POST /api/bounties")), None)
    sc.check("the create went out", post is not None, lines[-6:])
    sc.check("no refetch after the 400", post is not None and not any(l.startswith("GET /api/bounties issuer") for l in lines[post + 1:]), lines[-6:])


# ---------------------------------------------------------------- D
async def scenario_d(sc):
    coord = "39998:%s:dup-city" % sc.suite.keys["house"]["pub"]
    sc.box.start_fake({"create": "ok", "dup_coordinate": coord})
    if not await start(sc, "D") or not await open_house_form(sc):
        return
    sc.box.mark("D submit")
    # Driven directly: the dup mode already lists dup-city, so the judge half
    # believes a fill step is done and its pick comes back weak.
    if not await act(sc, "the duplicate's form filled and submitted", fill("dup-city").do):
        return
    await sc.step(Step("Wait for the form's answer to the duplicate.", [Cand("wait", "Wait 2 seconds.", [("wait", 2)])],
                       "The form shows 'This list already has an open bounty. Pick it from the list instead.'",
                       tries=2, settle=1, cues=("already has an open bounty", "Publishing")), label="dup answer")
    sc.wait_tree("bounty-form-error", 10, has="already has an open bounty")
    sc.check_tree("duplicate refused in the form", [("bounty-form-error", "already has an open bounty")])
    sent = [l for l in sc.log_after("D submit") if l.startswith(("POST /api/bounties", "POST strfry/publish"))]
    sc.check("no request sent for the duplicate", not sent, sent)


# ---------------------------------------------------------------- E
async def scenario_e(sc):
    sc.box.start_fake({"create": "401once"})
    if not await start(sc, "E") or not await open_house_form(sc):
        return
    sc.box.mark("E once")
    # Positive wording: Jev scores absences ("no error") poorly; the hard
    # check below covers them.
    await submit_form(sc, "auth-city", "The bounty 'auth-city' is shown in the list and in a detail pane with 'open' and '100 sats'.",
                      ("auth-city", "100 sats", "open"), settle=4)
    lines = sc.log_after("E once")
    posts = [i for i, l in enumerate(lines) if l.startswith("POST /api/bounties")]
    relog = len(posts) == 2 and sum(1 for l in lines[posts[0]:posts[1]] if l.startswith("POST login-user")) == 1
    sc.check("401 then re-login then a second create", relog, lines)
    sc.check_tree("401 once: form closed without an error", [], [("bounty-form", None)])
    # 401 twice: a new mode file resets the fake's counter.
    sc.box.set_mode({"create": "401always"})
    sc.box.mark("E twice")
    await sc.step(Step("Open the new bounty form again as the house issuer.", [NEW_FORM, BOUNTIES],
                       "A form titled 'New DList + bounty' is open with an 'Item (singular)' field.", settle=2, cues=("Item (singular)",)), label="reopen form")
    await submit_form(sc, "deny-city", "The form is still open with an error line, and no 'Publishing…'.", ("not logged in", "Publishing", "Item (singular)"), settle=4)
    sc.wait_tree("bounty-form-error", 10)
    lines = sc.log_after("E twice")
    posts = [l for l in lines if l.startswith("POST /api/bounties")]
    logins = [l for l in lines if l.startswith("POST login-user")]
    sc.check("401 twice: two creates, one re-login, then stop", len(posts) == 2 and len(logins) == 1, lines)
    sc.check_tree("401 twice: clean error in the open form", [("bounty-form-cancel", None), ("bounty-form-error", "401")],
                  [("bounty-form-status", "Publishing")])
    sc.check("app still running", sc.box.app_alive())


# ---------------------------------------------------------------- F
def claimant_wallet(sc):
    return Step(
        "Show the Wallet screen of the claimant account. The house's Wallet only says 'The house key's wallet is not managed here.'",
        [Cand("claimant-wallet", "Press the claimant's account tab in the left rail, then the sidebar's Wallet tab.",
              [("press", tab(sc, "claimant")), ("press", "nav-wallet"), ("wait_id", "wallet-title"), PARK])],
        "The Wallet screen is shown for an account that is not the house: no 'house key's wallet is not managed here' line.",
        settle=3, cues=("not managed here", "Wallet", "read this profile", "Try again", "Create a Coinos wallet"))


async def scenario_f(sc):
    sc.box.start_fake({"scan": "fail"})
    if not await start(sc, "F") or not await sc.step(claimant_wallet(sc), label="claimant wallet"):
        return
    sc.wait_tree("wallet-profile-error", 15)
    sc.check_tree("scan 500: 'Couldn't read this profile' with 'Try again'",
                  [("wallet-profile-error", "Couldn't read this profile"), ("wallet-profile-retry", None)],
                  [("wallet-create", None), ("wallet-managed-elsewhere", None)])
    sc.box.set_mode({"scan": "ok"})
    sc.box.mark("F retry")
    await sc.step(Step("The profile scan is healthy again; press the 'Try again' button on the Wallet screen. Do not create a wallet.",
                       [Cand("try-again", "Press the 'Try again' button under 'Couldn't read this profile'.",
                             [("press", "wallet-profile-retry"), ("wait_id", {"id": "wallet-create", "secs": 20, "soft": True}), PARK])],
                       "'Couldn't read this profile' is gone and the wallet panel offers 'Create a Coinos wallet for this account'.",
                       tries=2, settle=4, cues=("read this profile", "Create a Coinos wallet", "Checking your profile")), must_act=True, label="try again")
    sc.check_tree("retry reaches the Read state", [("wallet-create", None)], [("wallet-profile-error", None)])
    ok = [l for l in sc.log_after("F retry") if l.startswith("GET /api/strfry/scan") and l.endswith("200 []")]
    sc.check("the retry re-read the profile scan", bool(ok), sc.log_after("F retry")[-4:])


# ---------------------------------------------------------------- G
# The sidebar chip opens the active account's page on its Trust tab
# (shell.rs, `open_account(.., Tab::Trust)`); its Identity & keys tab is next.
IDENTITY = Step("Open the Identity & keys tab of the active account's page.",
                [Cand("chip-then-identity", "Press the account chip at the top of the sidebar (opens the account page), then its 'Identity & keys' tab.",
                      [("press", "sidebar-account"), ("wait_id", "account-tab-identity"), ("press", "account-tab-identity"),
                       ("wait_id", "account-copy-nsec"), PARK])],
                "The 'Identity & keys' tab shows 'Public identity' and 'Secret key (nsec)'.", settle=2,
                cues=("Public identity", "Secret key", "Identity"))


async def scenario_g(sc):
    sc.box.start_fake({})
    if not await start(sc, "G") or not await sc.step(IDENTITY, label="identity tab"):
        return
    await sc.step(Step("Copy the nsec: press the 'copy' button in the 'Secret key (nsec)' row (next to 'Reveal'). Never press Reveal.",
                       [Cand("copy-nsec", "Press the 'copy' button to the right of 'Reveal' in the Secret key row.",
                             [("press", "account-copy-nsec"), ("stamp", "copy"), ("wait", 0.5), PARK])],
                       "The Secret key row's button now reads 'copied'.", tries=1, settle=0.5, cues=("copied", "Reveal")), must_act=True, label="copy nsec")
    t0 = sc.stamps.get("copy", time.time())   # the click itself, not the end of the step
    # Poll the clipboard; keep only booleans and the first 8 characters.
    timeline, prefix = [], ""
    while time.time() - t0 < 34:
        clip = sc.box.clipboard()
        if clip.startswith("nsec1") and not prefix:
            prefix = clip[:8]
        timeline.append((round(time.time() - t0, 1), clip.startswith("nsec1"), clip.strip() == ""))
        clip = None
        await asyncio.sleep(2)
    sc.notes.append({"clipboard_timeline": [f"{t}s:{'nsec' if n else ('empty' if e else 'other')}" for t, n, e in timeline]})
    held = [t for t, n, _ in timeline if n]
    sc.check("clipboard holds the nsec after the copy", bool(timeline) and timeline[0][1],
             f"prefix {prefix!r}, first read {timeline[0][0] if timeline else None} s after the click")
    sc.check("the nsec stays about 30 s (still there at 25 s)", held and max(held) >= 25, f"last seen at {max(held) if held else None} s")
    sc.check("app still running (an app that died also empties the clipboard)", sc.box.app_alive())
    sc.check("clipboard empty at 31 s and after", all(e for t, _, e in timeline if t >= 31) and any(t >= 31 for t, _, _ in timeline),
             [x for x in timeline if x[0] >= 31])


# ---------------------------------------------------------------- H
REMOVE_ARMED = "Click again to remove"
SIDEBAR_REMOVE = "sidebar-remove-account"


async def remove_armed_flow(sc, who, expect_sats=False):
    """Arm from the sidebar, walk away, arm from the Identity card, walk away."""
    await sc.step(Step(f"Arm 'Remove this account' for the {who}: press the sidebar link once. Do not press it a second time.",
                       [Cand("arm-sidebar", "Press 'Remove this account' under the account name in the sidebar, once.",
                             [("press", SIDEBAR_REMOVE), ("wait", 1), PARK])],
                       "The sidebar link now reads 'Click again to remove'.", tries=1, settle=2, cues=(REMOVE_ARMED, "Remove this account")),
                  must_act=True, label=f"{who} arm sidebar")
    sc.check_tree(f"{who}: sidebar arms 'Click again to remove'", [(SIDEBAR_REMOVE, REMOVE_ARMED)])
    if expect_sats:
        # Arming re-reads the balance: "Checking balance…", then the warning.
        sc.wait_tree("sidebar-remove-warning", 25, has=("still holds", "Couldn't read the balance"))
        warning = sc.tree_name("sidebar-remove-warning") or ""
        sc.check(f"{who} (Coinos wallet): armed remove warns about sats left", "still holds" in warning,
                 "balance not readable" if "read the balance" in warning else f"sidebar-remove-warning reads {warning!r}")
    await sc.step(Step("Navigate away: press the sidebar's Dashboard tab.", [DASHBOARD],
                       "The Dashboard is shown and the sidebar reads 'Remove this account' again, not 'Click again to remove'.",
                       settle=2, cues=(REMOVE_ARMED, "Remove this account", "Dashboard")), must_act=True, label=f"{who} leave (sidebar)")
    sc.check_tree(f"{who}: navigating away disarms (sidebar)", [(SIDEBAR_REMOVE, "Remove this account")], [(SIDEBAR_REMOVE, REMOVE_ARMED)])
    if not await sc.step(IDENTITY, label=f"{who} identity tab"):
        return
    await sc.step(Step(f"Arm the 'Remove…' button on the {who}'s Identity card ('Remove this account' card at the bottom), once.",
                       [Cand("arm-card", "Press the 'Remove…' button on the right of the 'Remove this account' card, once.",
                             # The npub QR card pushes this card below the fold.
                             [("scroll_to", "account-remove"), ("press", "account-remove"), ("wait", 1), PARK])],
                       "The card's button and the sidebar link read 'Click again to remove'.", tries=1, settle=2,
                       cues=(REMOVE_ARMED, "Remove", "Deletes the nsec")), must_act=True, label=f"{who} arm card")
    sc.check_tree(f"{who}: Identity card arms (card + sidebar)", [("account-remove", REMOVE_ARMED), (SIDEBAR_REMOVE, REMOVE_ARMED)])
    await sc.step(Step("Navigate away: press the sidebar's Bounties tab.", [BOUNTIES],
                       "The Bounties screen is shown and nothing reads 'Click again to remove'.", settle=2,
                       cues=(REMOVE_ARMED, "Bounties")), must_act=True, label=f"{who} leave (card)")
    sc.check_tree(f"{who}: navigating away disarms (card)", [], [("~(sidebar-remove-account|account-remove)", REMOVE_ARMED)])


async def scenario_h(sc):
    sc.box.start_fake({})
    sc.store = await start(sc, "H", seed=True)
    if not sc.store:
        return
    await sc.step(Step("Show Bounties with the house issuer selected.", [house(sc), BOUNTIES],
                       "The Bounties screen is shown and its header button reads 'New DList + bounty'.", settle=2, cues=HOUSE_CUES),
                  must_act=True, label="house")   # the press is idempotent; Jev took the Dashboard for done
    await remove_armed_flow(sc, "house")
    # Direct: the tab press is idempotent, and the judge halted here on a
    # weak pick (p 0.44-0.50, abstain ~0.4) in two runs. The header check
    # below decides.
    await act(sc, "claimant's tab and Bounties pressed", [("press", tab(sc, "claimant"))] + TO_BOUNTIES)
    sc.wait_tree("bounties-new", 30, has=CLAIMANT[1])
    sc.check_tree("claimant selected", [CLAIMANT])
    await remove_armed_flow(sc, "claimant", expect_sats=True)
    sc.check("nothing was removed", sc.box.accounts_in_store(sc.store) == 2, f"{sc.box.accounts_in_store(sc.store)} accounts in store")


# ---------------------------------------------------------------- P (paid)
async def scenario_p(sc):
    """One real bounty (100 sats, cap 100) and one claim against tapestry,
    with R's gates. Never retried."""
    stamp = sc.suite.run_id.replace("-", "")[-10:]
    name, item = f"e2e-{stamp}", f"E2E Roasters {stamp}"
    meta = sc.suite.meta
    meta["float_before"] = sc.box.float_sats()
    sc.check("float at least 200 sats", (meta["float_before"] or 0) >= 200, f"{meta['float_before']} sats")
    if not await start(sc, "P", target="instance", seed=True):
        return
    try:
        await paid_flow(sc, name, item)
    finally:
        meta["float_after"] = sc.box.float_sats()
        spent = (meta["float_before"] or 0) - (meta["float_after"] or 0)
        sc.check("float dropped by one payout, not more", 0 < spent <= 150, f"{meta['float_before']} -> {meta['float_after']} sats")


async def paid_flow(sc, name, item):
    # cap = reward = 100: the bounty closes after its one payout, so a
    # leftover e2e bounty can never pay again.
    if not await house_publishes(sc, name, cap="100"):
        return
    claimant = tab(sc, "claimant")
    acct = sc.tree_name(claimant)
    if not sc.check("the claimant's account tab names it", bool(acct), repr(acct)):
        return
    if not await claim_and_settle(sc, "claimant", name, item, claimant, acct, r_open_form(name)):
        return
    await sc.step(Step("Show the claimant's wallet balance.",
                       [Cand("open-wallet", "Press the sidebar's Wallet tab.", [("press", "nav-wallet"), ("wait_id", "wallet-balance"), PARK])],
                       "The Wallet screen is shown with a sats balance.", settle=5, cues=("Wallet", "sats", "coinos")),
                  label="claimant wallet")


# ---------------------------------------------------------------- R (paid)
# R drives the app through its accessibility tree like every scenario: the
# waits are for the ids that mark the next state, and the gates before each
# paying Enter read the tree. Jev still picks the candidate and judges every
# postcondition from the screenshot, so a press that reaches the wrong
# control fails the step.
RAIL_ACCOUNT = "~rail-account-[0-9a-f]{64}"
BOUNTY_ROW = "~bounty-row-[0-9a-f-]{36}"
CLAIM_CARD = "~claim-[0-9a-f]{64}"
PAYMENT = "~claim-[0-9a-f]{64}-payment"   # every claim card's payment state


def create_account(who, stamp, first_run=False):
    """Steps that add one app-made account (and its Coinos wallet). With
    first_run the app has no account yet and opens on the choose card."""
    name = f"E2E Newcomer {who} {stamp}"
    if first_run:
        open_setup = Step(f"On the first-run 'Your Nostr account' card, choose 'Create a new account' for newcomer {who}.",
                          [Cand("press-create", "Press the 'Create a new account' button.",
                                [("wait_id", "setup-create"), ("press", "setup-create"), ("wait_id", "setup-name"), PARK])],
                          "The 'Create a new account' card is open with a 'Name (optional)' input and a 'Create — Enter' button.",
                          tries=1, settle=1, cues=("Name (optional)", "Create — Enter", "Create a new account", "I already have a key"))
    else:
        open_setup = Step(f"Open account setup from the '+' in the left rail and choose 'Create a new account' for newcomer {who}.",
                          [Cand("press-add-then-create", "Press the rail's 'Add account' (+) button, then the 'Create a new account' button.",
                                [("press", "rail-add-account"), ("wait_id", "setup-create"), ("press", "setup-create"),
                                 ("wait_id", "setup-name"), PARK])],
                          "The 'Create a new account' card is open with a 'Name (optional)' input and a 'Create — Enter' button.",
                          tries=1, settle=1, cues=("Name (optional)", "Create — Enter", "Create a new account", "I already have a key"))
    return [
        open_setup,
        # tries=1: a second Create is a second account and wallet.
        Step(f"Type the name '{name}' and create the account.",
             # Enter, not the button: it keeps the focus in the name input, where the
             # later Enter that leaves setup has to land.
             [Cand("name-then-enter", "Click into the Name input, type the name, then press Enter.",
                   [FOCUS, ("click_id", "setup-name"), ("type", name), ("wait", 0.4), ("key", "Return"), ("wait", 1)])],
             "The account is being created, or the ready panel shows 'Coinos wallet ready'.",
             tries=1, settle=10, cues=("Coinos wallet ready", "Creating", "Create — Enter", "Couldn't")),
    ]


async def newcomer(sc, who, stamp, first_run=False):
    steps = create_account(who, stamp, first_run)
    if not await sc.step(steps[0], must_act=True, label=f"newcomer {who} setup"):
        return False
    if not await sc.step(steps[1], must_act=True, label=f"newcomer {who} create"):
        return False
    # Live coinos.io plus a kind-0 publish: allow a minute. "Copy password"
    # exists only after a Coinos signup in this session.
    try:
        await sc.suite.driver.act("wait_id", {"id": "wallet-copy-password", "secs": 60, "soft": True})
    except Exception as e:   # a soft wait still raises when the driver cannot run at all
        sc.notes.append(f"newcomer {who}: wallet wait: {e}"[:300])
    sc.wait_tree("setup-step-wallet-tag", 15, has="Coinos wallet ready")
    if not sc.check_tree(f"newcomer {who}: Coinos wallet ready", [("setup-step-wallet-tag", "Coinos wallet ready")],
                         [("setup-error", None), ("wallet-error", None)]):
        return False
    # Driven directly: the judge kept re-observing instead of pressing Enter.
    # The ready panel's Dashboard button is below the fold at 1280x960 (and
    # gpui cannot press it there); Enter in the setup input leaves setup.
    try:
        for kind, arg in (FOCUS, ("key", "Return"), ("wait_id", {"id": "dash-heading", "soft": True})):
            await sc.suite.driver.act(kind, arg)
    except Exception as e:   # outside sc.step: a focus check or a hung docker exec stops here, cleanly
        sc.check(f"newcomer {who}: Enter leaves setup", False, f"{type(e).__name__}: {e}")
        return False
    label = f"newcomer {who} dashboard"
    met = await sc.step(Step("Confirm the Dashboard is shown.", [Cand("wait", "Wait 2 seconds.", [("wait", 2)])],
                             "The Dashboard is shown with a 'Recent activity' panel and the sidebar.",
                             tries=2, settle=1, cues=("Recent activity", "Dashboard", "Coinos wallet ready")), label=label)
    # The Dashboard also says "Coinos wallet ready: ..." in Needs attention;
    # only the ready panel has the Copy password button.
    if not sc.check_tree(f"newcomer {who}: back on the Dashboard", [("dash-heading", None), ("dash-activity-heading", None)],
                         [("wallet-copy-password", None)]):
        return False
    if not met:
        sc.accept_step(label, "dash-heading and Recent activity in the tree, ready panel gone")
    return True


async def scenario_r(sc):
    """Two brand-new accounts made in the app each claim one bounty seat on the
    same list (cap 200, reward 100) and both get auto-paid. Never retried."""
    # Depends on the box instance's trust-gate bypass (NODE_ENV=development,
    # DEV_SKIP_TRUST_CHECK=true): with the gate on, brand-new keys never get paid.
    stamp = sc.suite.run_id.replace("-", "")[-10:]
    name = f"e2e-new-{stamp}"
    meta = sc.suite.meta
    meta["float_before_r"] = sc.box.float_sats()
    if not sc.check("float at least 250 sats (two payouts plus fees)", (meta["float_before_r"] or 0) >= 250, f"{meta['float_before_r']} sats"):
        return
    # Only the house starts in the store; the newcomers are made in the app.
    # No bus or no tree: no R, before any account exists.
    store = await start(sc, "R", target="instance", seed=False, nsecs=sc.box.keys["house"]["sec"])
    if not store:
        return
    try:
        await newcomers_flow(sc, name, stamp, store)
    finally:
        meta["float_after_r"] = sc.box.float_sats()
        spent = (meta["float_before_r"] or 0) - (meta["float_after_r"] or 0)
        sc.check("float dropped by two payouts, not more", 150 < spent <= 300, f"{meta['float_before_r']} -> {meta['float_after_r']} sats")


async def newcomers_flow(sc, name, stamp, store):
    if not await newcomer(sc, "A", stamp):
        return
    if not await newcomer(sc, "B", stamp):
        return
    sc.check("house plus two app-made accounts with Coinos wallets",
             sc.box.accounts_in_store(store) == 3 and sc.box.wallet_accounts_in_store(store) == 2,
             f"{sc.box.accounts_in_store(store)} accounts, {sc.box.wallet_accounts_in_store(store)} app-made with a wallet")
    if not await house_publishes(sc, name):
        return
    for who in ("A", "B"):
        item = f"E2E Newcomer {who} {stamp}"
        if not await claim_and_settle(sc, f"newcomer {who}", name, item, {"id": RAIL_ACCOUNT, "name": item}, item, r_open_form(name)):
            return


async def house_publishes(sc, name, cap="200"):
    """The house (in the store, with the claimants) selected on Bounties,
    the form gates, one publish of `name` (reward 100, `cap`), auto-pay
    armed. False at the first unmet step or failed gate."""
    house_tab = Cand("press-house-tab", "Press the house issuer's account tab in the left rail, then the sidebar's Bounties tab.",
                     [("press", tab(sc, "house"))] + TO_BOUNTIES + [("wait_id", {"id": "bounties-new", "has": [ISSUER[1]]})])
    if not await sc.step(Step("Show the Bounties screen with the house issuer selected (its header button reads 'New DList + bounty').",
                              [house_tab], "The Bounties screen is shown and its header button reads 'New DList + bounty'.",
                              settle=1, cues=HOUSE_CUES), must_act=True, label="house on bounties"):
        return False
    if not sc.check_tree("house selected (bounties-new)", [ISSUER], [CLAIMANT]):
        return False
    if not await sc.step(Step("Open the new bounty form as the house issuer.",
                              [Cand("press-new-bounty", "Press the '+ New DList + bounty' button.",
                                    [("press", "bounties-new"), ("wait_id", "bounty-form-item-singular"), PARK])],
                              "A form titled 'New DList + bounty' is open.", settle=1, cues=("New DList", "Cancel")),
                         label="open form"):
        return False
    # Gate: the last Enter publishes; in a claim form it would claim and pay.
    form = sc.tree_name("bounty-form")
    if not sc.check("bounty-form is the house's new list, no claim-form",
                    form == "New DList + bounty" and sc.tree("claim-form") == 0, f"bounty-form {form!r}"):
        await sc.suite.driver.act("key", "Escape")
        return False
    # tries=1: a retried Enter is a second bounty.
    publish = Step("Fill the bounty form and publish it.",
                   [fill(name, cap, wait=True), Cand("cancel-form", "Press the form's 'Cancel (esc)' button.", [("press", "bounty-form-cancel")])],
                   "The new bounty is listed and auto-pay is armed.", tries=1, settle=1,
                   cues=(name, "Auto-pay armed", "watching for claims"))
    if not await sc.step(publish, must_act=True, label="publish bounty"):
        return False
    # top-auto-pay is in the tree while a bounty is focused.
    sc.wait_tree("top-auto-pay", 20, has="Auto-pay armed")
    if not sc.check_tree("auto-pay armed on the new bounty", [("top-auto-pay", "Auto-pay armed")]):
        return False
    # Gate before any claim: a field the click missed keeps the form's
    # default (cap 400), and the claims would still pay. Never retried.
    title, reward, got_cap = (sc.tree_name(i) or "" for i in ("bounty-detail-title", "bounty-detail-reward", "bounty-detail-cap"))
    return sc.check(f"the new bounty is {name}, reward 100 sats, cap {cap} sats",
                    title == name and reward.startswith("100 sats") and got_cap.startswith(f"{cap} sats"),
                    f"title {title!r}, reward {reward!r}, cap {got_cap!r}")


def r_open_form(name):
    """The claim-form step candidates: the bounty is still selected from its
    creation, or it is selected with Down (the list runs oldest first, so its
    row can sit below the fold, where it cannot be pressed)."""
    return [Cand("claim-on-selected", f"'{name}' is already open in the detail pane: press the '+ Submit a bounty claim' button.",
                 [("wait_id", {"id": "bounty-detail-title", "name": name, "secs": 3}),
                  ("press", "bounties-new"), ("wait_id", "claim-form-item-name"), PARK]),
            Cand("keys-then-claim", f"Press Down until '{name}' is the selected bounty, then the '+ Submit a bounty claim' button.",
                 [("select_bounty", name), ("press", "bounties-new"), ("wait_id", "claim-form-item-name"), PARK])]


async def claim_and_settle(sc, who, name, item, tab_, acct, open_cands, refresh=None):
    """`who` (its rail tab `tab_`, its account name `acct`) selected and
    gated on Bounties, the claim form opened with `open_cands` and gated on
    this run's list and 100 sats, one claim of `item`, settlement and its
    checks. `refresh` is the candidate that shows the card's latest state
    (by default: Bounties, then this run's bounty again with Down; nothing
    on the box can make a running app refetch, see Y). False where the
    flow has to stop."""
    select = Step(f"Select {who} in the left rail and show Bounties (the header button reads 'Submit a bounty claim').",
                  [Cand("press-account-tab", f"Press {who}'s account tab in the left rail, then the sidebar's Bounties tab.",
                        [("press", tab_)] + TO_BOUNTIES + [("wait_id", {"id": "bounties-new", "has": [CLAIMANT[1]]})])],
                  "The Bounties header button reads 'Submit a bounty claim'.", settle=1,
                  cues=("Submit a bounty claim", "New DList + bounty"))
    if not await sc.step(select, must_act=True, label=f"select {who}"):
        return False
    # Gate on the account: the header, and the sidebar chip's name.
    chip = sc.tree_name("sidebar-account")
    ok = sc.check_tree(f"{who} selected (header)", [CLAIMANT], [ISSUER])
    ok = sc.check(f"{who} selected (sidebar-account)", chip == acct, repr(chip)) and ok
    if not ok:
        await sc.suite.driver.act("key", "Escape")
        return False
    # Baseline before the claim form can hide earlier cards.
    n_settled = sc.tree(PAYMENT, has="Payment settled")
    open_form = Step("Open the claim form for this run's bounty.", open_cands,
                     "A claim form is open (a 'Cancel' control is visible and the 'Submit a bounty claim' button is gone).",
                     settle=1, cues=("Cancel", "Submit a bounty claim"))
    if not await sc.step(open_form, label=f"open claim form {who}"):
        return False
    # Gate: the claim form must be on this run's bounty, or we'd pay another
    # one. The exact label (src/bounties.rs claim form): this run's list and
    # its 100-sat reward, proven before the paying Enter.
    form = sc.tree_name("claim-form") or ""
    want = f"Claim an item on {name} · 100 sats per item"
    if not sc.check(f"{who}: claim-form names this run's list and 100 sats", form == want, repr(form)):
        await sc.suite.driver.act("key", "Escape")
        sc.notes.append(f"claim form for {who} was not on this run's bounty; cancelled, nothing claimed")
        return False
    # tries=1: a second submit is a second payout. No cancel candidate:
    # the gate has passed, and a cancel here strands an open auto-pay bounty.
    submit = Step("Submit the claim.",
                  [Cand("type-item-and-submit", f"Click into the Item name input, type '{item}' and press Enter to publish the claim.",
                        [FOCUS, ("click_id", "claim-form-item-name"), ("type", item), ("wait", 0.3), ("key", "Return"),
                         ("wait_id", {"id": CLAIM_CARD, "name": item, "secs": 20, "soft": True})])],
                  "The claim card shows 'Claim submitted'.", tries=1, settle=1, cues=("Claim submitted", item))
    if not await sc.step(submit, must_act=True, label=f"submit claim {who}"):
        return False
    # This claim's card, by its item name; its receipt row appears once
    # the zap receipt is in.
    node = sc.tree_node(CLAIM_CARD, name=item)
    card = node["id"] if node else None
    wait, tries = Cand("wait-for-settlement", "Wait 20 seconds for the auto-pay watcher to pay and the zap receipt to arrive.", [("wait", 20)]), 6
    if card:
        wait, tries = Cand("wait-for-receipt", "Wait up to 150 seconds for this claim's card to show its payment receipt.",
                           [("wait_id", {"id": f"{card}-receipt", "secs": 150, "soft": True})]), 2
    await sc.step(Step("Wait for the payment to settle.", [wait],
                       "The claim card shows 'Payment settled' or 'Zap receipt'.", settle=1, tries=tries,
                       cues=("Payment settled", "Zap receipt", "Auto-pay failed")), label=f"wait for settlement {who}")
    if card:
        sc.wait_tree(f"{card}-payment", 30, has=("Payment settled", "Auto-pay failed"))
        if (sc.tree_name(f"{card}-payment") or "").startswith("Auto-pay failed"):
            sc.notes.append(f"Auto-pay failed on {who}'s claim card; not retried, not reset")
    await sc.step(Step("Show the bounty's claim card with its latest payment state.", [refresh or reselect(name)],
                       "The claim card shows 'Payment settled' or 'Zap receipt'.", settle=4,
                       cues=("Payment settled", "Zap receipt")), label=f"settled after refresh {who}")
    if not sc.check(f"{who}: this claim's card is in the tree", bool(card), item):
        return True
    sc.check_tree(f"{who}: the claim card settled, with its receipt row",
                  [(f"{card}-receipt", None), (f"{card}-payment", "Payment settled")], [(f"{card}-payment", "Auto-pay failed")])
    n = sc.tree(PAYMENT, has="Payment settled")
    sc.check(f"{who}: a new Payment settled card ({n} now, {n_settled} before)", n >= n_settled + 1)
    return True


# ---------------------------------------------------------------- Y (paid)
# Y is R with two separate app processes side by side (the suite resizes
# the VNC screen for Y, refuses to start Y on a narrower one, and records
# all of it). App A starts with the house only and makes newcomer A through
# the rail's "+". App B starts beside it as a first launch with no key and
# makes newcomer B on the choose card. Both newcomers exist before the house
# publishes from App A, as in R. Each newcomer claims in its own app; after
# each payout the other app's own tree must show that claim.
#
# After the publish, A claims in App A; then App B is restarted on its own
# store: on the box nothing can make a running app refetch its bounty list
# (its refresh is cmd-r, super+r, and xfce binds super+r to its Application
# Finder), and a launch fetches the list (Shell::new sends FetchBounties).
# App B must then show A's claim, B claims, and App A, which watches the
# bounty (WatchBounty), must show B's claim live.
#
# Every action, AT-SPI read and screenshot is pinned to one app (sc.use):
# its pid, its X window, its half of the screen. Steps run fast
# (Scenario.step): no Jev choose for a one-candidate step, Jev still
# judges every after-look.
# Window frames (x, y): xfwm's frame is 5 px left and right, so App A's
# 1290 px ends where App B's begins, on suite.WIDE's 2580 px screen.
APP_A_AT, APP_B_AT = (0, 0), (1290, 0)


def select_row(name):
    """Select this run's bounty in the pinned app with Down (its row can be
    below the fold), which also starts the app's live watch of its claims."""
    return [("press", "nav-bounties"), ("wait_id", {"id": BOUNTY_ROW, "name": name, "secs": 30}), ("select_bounty", name)]


def pinned_open_form(name):
    """App B's one claim-form candidate: B never saw the bounty created, so
    it is selected with Down. (App A takes R's candidates.)"""
    return [Cand("select-then-claim", f"Press Down until '{name}' is the selected bounty, then press '+ Submit a bounty claim'.",
                 select_row(name) + [("press", "bounties-new"), ("wait_id", "claim-form-item-name"), PARK])]


def reselect(name):
    return Cand("reselect-bounty", f"Press Bounties and select '{name}' again with Down.", select_row(name) + [PARK])


async def sees_claim(sc, app, item, name):
    """`app`'s own tree (its pid) shows the claim card `item` on this run's
    bounty; the screenshot is judged too."""
    sc.use(app)
    label = f"{app.name} sees {item}"
    met = await sc.step(Step(f"In this app, show this run's bounty '{name}' with the claim card '{item}'.",
                             [Cand("select-and-wait", f"Select '{name}' with Down and wait for the '{item}' card.",
                                   select_row(name) + [("wait_id", {"id": CLAIM_CARD, "name": item, "secs": 60, "soft": True}), PARK])],
                             f"The detail pane shows '{name}' and a claim card for '{item}'.", settle=1, cues=(name, item)),
                        label=label)
    n = sc.tree(CLAIM_CARD, name=item)
    if sc.check(f"{app.name} (pid {app.pid}) has {item}'s claim card in its own tree", n == 1, f"{n} cards") and not met:
        sc.accept_step(label, "the claim card is in this app's tree")


def store_check(sc, app, n):
    sc.check(f"{app.name}: {n} account(s), one app-made with a Coinos wallet",
             sc.box.accounts_in_store(app.store) == n and sc.box.wallet_accounts_in_store(app.store) == 1,
             f"{sc.box.accounts_in_store(app.store)} accounts, {sc.box.wallet_accounts_in_store(app.store)} app-made with a wallet")


async def account_name(sc, pub):
    """The pinned app's name for account `pub` (its rail tab), once the
    app has read its kind-0; None after 60 s."""
    end = time.time() + 60
    while time.time() < end:
        n = sc.tree_name(f"rail-account-{pub}")
        if n and n != "New account":
            return n
        await asyncio.sleep(2)
    return None


async def scenario_y(sc):
    """Two newcomers in two app processes: A made in the house's app, B in a
    fresh install. The house's bounty (cap 200, reward 100) pays both, and
    each app shows the other's claim. Never retried.

    With --reuse-wallets <run-id> no Coinos account is made (Coinos limits
    signups per network): A's store starts with that run's first app-made
    newcomer and B's with the second, and their names come from the app."""
    stamp = sc.suite.run_id.replace("-", "")[-10:]
    name = f"e2e-two-{stamp}"
    meta = sc.suite.meta
    meta["float_before_y"] = sc.box.float_sats()
    if not sc.check("float at least 250 sats (two payouts plus fees)", (meta["float_before_y"] or 0) >= 250, f"{meta['float_before_y']} sats"):
        return
    reuse = None
    if sc.suite.args.reuse_wallets:
        src, pubs = sc.box.reusable_wallets(sc.suite.args.reuse_wallets)
        if not sc.check("the reused R store holds exactly two app-made Coinos accounts", pubs is not None, str(src)):
            return
        reuse = (src, pubs)
    if not sc.check("accessibility bus up (python3-gi, at-spi, IsEnabled)", sc.box.a11y_up()):
        return
    sc.fast = True
    sc.box.launch("Y-A", sc.suite.run_id, target="instance", nsecs=sc.box.keys["house"]["sec"], at=APP_A_AT,
                  seed_account=(reuse[0], reuse[1][0]) if reuse else None)
    try:
        await two_apps_flow(sc, sc.box.last_app, name, stamp, reuse)
    finally:
        meta["float_after_y"] = sc.box.float_sats()
        spent = (meta["float_before_y"] or 0) - (meta["float_after_y"] or 0)
        sc.check("float dropped by two payouts, not more", 150 < spent <= 300, f"{meta['float_before_y']} -> {meta['float_after_y']} sats")


async def reused_name(sc, who, pub):
    """A reused newcomer's name from the pinned app, gated: it must be an
    earlier run's "E2E Newcomer …" account."""
    n = await account_name(sc, pub)
    ok = sc.check(f"reused newcomer {who}: the app names it E2E Newcomer …", bool(n) and n.startswith("E2E Newcomer "), repr(n))
    return n if ok else None


async def two_apps_flow(sc, app_a, name, stamp, reuse=None):
    names = None
    sc.use(app_a)
    if not await a11y_preflight(sc):
        return
    if reuse:
        name_a = await reused_name(sc, "A", reuse[1][0])
        if not name_a:
            return
    elif not await newcomer(sc, "A", stamp):
        return
    store_check(sc, app_a, 2)
    # Beside App A: newcomer B exists before any publish, either made in a
    # fresh install or seeded from the reused store.
    sc.box.launch("Y-B", sc.suite.run_id, target="instance", nsecs="", keep=True, at=APP_B_AT,
                  seed_account=(reuse[0], reuse[1][1]) if reuse else None)
    app_b = sc.box.last_app
    sc.use(app_b)
    if not await a11y_preflight(sc, ready="rail-add-account" if reuse else "setup-create"):
        return
    if reuse:
        name_b = await reused_name(sc, "B", reuse[1][1])
        if not name_b or not sc.check("the reused newcomers have different names", name_b != name_a, f"{name_a!r} / {name_b!r}"):
            return
        names = {"A": name_a, "B": name_b}
    elif not await newcomer(sc, "B", stamp, first_run=True):
        return
    store_check(sc, app_b, 1)
    sc.use(app_a)
    if not await house_publishes(sc, name):
        return
    names = names or {w: f"E2E Newcomer {w} {stamp}" for w in ("A", "B")}

    def claim(who, open_cands):
        n = names[who]
        return claim_and_settle(sc, f"newcomer {who}", name, n, {"id": RAIL_ACCOUNT, "name": n}, n, open_cands, reselect(name))
    # A claims in App A, which is up and has the bounty selected.
    if not await claim("A", r_open_form(name)):
        return
    # Then App B again, on its own store: its launch reads the new bounty,
    # and selecting it must show A's claim.
    sc.box.kill_pid(app_b.pid)
    sc.box.launch("Y-B2", sc.suite.run_id, target="instance", nsecs="", keep=True, at=APP_B_AT, store=app_b.store)
    app_b = sc.box.last_app
    sc.use(app_b)
    if not await a11y_preflight(sc):
        return
    await sees_claim(sc, app_b, names["A"], name)
    # B claims in App B; App A, watching the bounty, must show it live.
    sc.use(app_b)
    if not await claim("B", pinned_open_form(name)):
        return
    await sees_claim(sc, app_a, names["B"], name)


# ------------------------------------------------------------ L..Q: trust
# Feature 2: "Your Trusted Community". Every scenario runs its own throwaway
# account key and its own throwaway owner (well-known account) — the house and
# claimant keys never appear here. The app talks Brainstorm to the fake on
# :8788 and trust reads/publishes to the local nak relay on :10547.
import json

LOCAL_RELAY = "ws://127.0.0.1:10547"


def nak_out(box, args, what, sec=None):
    rc, out = box.nak(args, sec=sec)
    if rc != 0 or not out.strip():
        raise RuntimeError(f"nak failed: {what}")
    return out.strip()


def nak_pub(box, sec):
    """The hex public key of a hex secret."""
    return nak_out(box, ["key", "public", "$NAK_SEC"], "key public", sec=sec)


def fresh_key(box):
    """A throwaway keypair via nak: hex sec, hex pub, npub."""
    sec = nak_out(box, ["key", "generate"], "key generate")
    pub = nak_pub(box, sec)
    npub = nak_out(box, ["encode", "npub", pub], "encode npub")
    return {"sec": sec, "pub": pub, "npub": npub}


def nak_publish(box, sec, kind, content, tags=()):
    """Sign and publish an event to the local relay. sec goes by environment
    name, never logged; nak itself is trusted (same as on the box)."""
    cmd = ["event", "-k", str(kind), "-c", content]
    for t in tags:
        cmd += ["-t", t]
    cmd += ["--sec", "$NAK_SEC", LOCAL_RELAY]
    return box.nak(cmd, sec=sec)[0] == 0


def nak_req(box, kind, author_hex):
    """Events for (kind, author) on the local relay, newest first."""
    out = box.nak(["req", "-k", str(kind), "-a", author_hex, LOCAL_RELAY])[1]
    evs = []
    for line in out.splitlines():
        line = line.strip()
        if line.startswith("{"):
            try:
                evs.append(json.loads(line))
            except ValueError:
                pass
    return sorted(evs, key=lambda e: e.get("created_at", 0), reverse=True)


def bs_setup_rows(box, pub):
    return json.loads(box.bs_get(f"/setup/{pub}"))


def publish_owner_profile(box, owner, run_id):
    """A kind-0 name so the recommended row shows a name, not a bare npub."""
    nak_publish(box, owner["sec"], 0, json.dumps(
        {"name": f"E2E Owner {run_id[-4:]}", "display_name": f"E2E Owner {run_id[-4:]}"}))


# The people the fake's /search/pubkeys returns. The fake serves only keys, so
# their names and pictures are kind-0s published to the local relay. The
# secrets are the public secp256k1 test keys 1, 2 and 3, worth nothing.
SEARCH_PEOPLE = [
    ("0" * 63 + "1", {"name": "alice", "display_name": "Alice E2E", "picture": "http://127.0.0.1:8788/pic/alice.png"}),
    ("0" * 63 + "2", {"name": "bob", "display_name": "Bob E2E"}),
    ("0" * 63 + "3", {"name": "carol", "display_name": "Carol E2E"}),
]


def publish_search_profiles(box):
    for sec, profile in SEARCH_PEOPLE:
        nak_publish(box, sec, 0, json.dumps(profile))


TRUST_STEPS = ("Good news: you can do the three needed steps right here",
               "Follow at least one well-known account",
               "Ask Brainstorm to calculate your community's trust scores",
               "Publish your Treasure Map")
# The sidebar chip opens the active account's page on its Trust tab
# (shell.rs, `open_account(.., Tab::Trust)`), so the chip press is the whole
# action; step 1's heading marks the tab drawn.
TRUST_READY = ("wait_id", {"id": "community-step-follow", "secs": 20, "soft": True})
TRUST_TAB = Step(
    "Open the active account's page on its 'Trust' tab.",
    [Cand("chip-opens-trust", "Press the account chip at the top of the left sidebar; it opens the account page on its 'Trust' tab.",
          [("press", "sidebar-account"), TRUST_READY, PARK]),
     Cand("trust-tab", "The account page is already open on another tab: press its 'Trust' tab, beside 'Identity & keys'.",
          [("press", "account-tab-trust"), TRUST_READY, PARK])],
    # The Trust tab has no "Your Trusted Community" heading (that is the
    # setup screen's step 3); naming it sank the judge to p~0.53 every time.
    "The account page is open with its tab strip 'Identity & keys, Trust, Progress, Wallet, Limits & defaults', "
    "a 'Treasure Map' card, the line 'Good news: you can do the three needed steps right here.', and the three "
    "numbered steps 'Follow at least one well-known account', 'Ask Brainstorm to calculate your community's trust "
    "scores' and 'Publish your Treasure Map'.",
    tries=3, settle=3,
    cues=("Identity & keys", "Treasure Map") + TRUST_STEPS)


async def open_trust(sc):
    if await sc.step(TRUST_TAB, label="trust tab"):
        return True
    # The tab, the three steps and their buttons in the tree decide when the judge does not.
    if sc.check_tree("the Trust tab's steps are in the tree",
                     [("account-tab-trust", None), ("community-step-follow", None), ("community-step-calculate", None),
                      ("community-step-publish-map", None), ("~community-calc(-retry)?", None), ("community-map", None)]):
        sc.accept_step("trust tab", "the tab, the three steps and their buttons in the tree")
        return True
    return False


def owner_follow(owner):
    return f"community-owner-{owner['pub']}-follow"


async def follow_owner(sc, owner, expect_confirm):
    """Press Follow on the recommended owner row; if the first-list amber box
    shows, approve it. Returns True once '✓ Following' / 'Saved on' shows."""
    # An account with a kind-3 reads "You already follow N account. Go to
    # step 2." over the row; without the last sentence the judge's abstain
    # mass reached 0.32 there and the step halted before any press (W).
    ok = await sc.step(Step(
        "Press 'Follow' on the recommended row ('RECOMMENDED — YOUR OWNER NPUB') in step 1 of the Trust tab. "
        "The test needs the owner in the follow list, so press it even when step 1 says 'You already follow N account'.",
        [Cand("follow-owner", "Press the 'Follow' button on the recommended owner row.",
              [("scroll_to", owner_follow(owner)), ("press", owner_follow(owner)), ("wait", 1), PARK])],
        "After the press, either a confirmation box offers the buttons 'Start a new follow list' and 'Cancel', or the row already reads '✓ Following' or 'Saved on'.",
        tries=4, settle=6,
        cues=("Start a new follow list", "Following", "Saved on", "Couldn't check", "RECOMMENDED")),
        must_act=True, label="follow owner")
    if not ok:
        return False
    if sc.tree("community-first-list") > 0:
        if not expect_confirm:
            return sc.check("no first-list confirmation for an account that has a kind-3", False,
                            "community-first-list is in the tree")
        return await confirm_first_list(sc, "The recommended row reads '✓ Following' and a 'Saved on N of M Nostr servers' line is shown.")
    return True


async def confirm_first_list(sc, done):
    return await sc.step(Step(
        "Approve the first follow list: press 'Start a new follow list' in the confirmation box.",
        [Cand("confirm-first-list", "Press the 'Start a new follow list' button beside 'Cancel' in the confirmation box.",
              [("scroll_to", "community-first-list"), ("press", "community-first-list"),
               ("wait_id", {"id": "community-follow-saved", "secs": 20, "soft": True}), PARK])],
        done, tries=4, settle=8, cues=("Following", "Saved on", "Start a new follow list")),
        must_act=True, label="confirm first list")


async def calculate(sc, done_cond="Step 2 shows 'Brainstorm confirmed — your community's trust scores are in the process of being calculated'.",
                    label="calculate", tries=4):
    # tries=1 for error paths: every press is one graperank request (the real
    # API rate-limits to 3/30 min), so a retried press falsifies the test.
    return await sc.step(Step(
        "Press 'Calculate my trust scores' in step 2.",
        [Cand("calculate", "Press the 'Calculate my trust scores' button.",
              [("scroll_to", "community-calc"), ("press", "community-calc"), ("wait", 1), PARK])],
        done_cond, tries=tries, settle=10,
        cues=("Brainstorm confirmed", "Asking the Brainstorm", "Calculate my trust scores",
              "Please wait", "temporarily unavailable")),
        must_act=True, label=label)


async def publish_map(sc):
    ok = await sc.step(Step(
        "Press 'Publish my Treasure Map' in step 3.",
        [Cand("publish-map", "Press the 'Publish my Treasure Map' button.",
              # Step 3 sits below the fold under the Recalculate row.
              [("scroll_to", "community-map"), ("press", "community-map"), ("wait", 1), PARK])],
        "The publish started: 'Fetching tag data' shows, or the card already reads 'Located'.",
        tries=3, settle=6,
        cues=("Fetching tag data", "Located", "Publish my Treasure Map")),
        must_act=True, label="publish map")
    return ok and sc.wait_tree("account-trust-map-status", 45, has="Located")


def check_map_rows(sc, acct_pub):
    """The relay's kind-10040 tags must equal the fake /setup rows, in order."""
    maps = nak_req(sc.box, 10040, acct_pub)
    sc.check("a kind-10040 exists on the local relay", bool(maps), f"{len(maps)} events")
    if not maps:
        return
    try:
        rows = bs_setup_rows(sc.box, acct_pub)
    except Exception as e:
        sc.check("the fake /setup answered for the check", False, e)
        return
    sc.check("the map's tags are the fake /setup rows verbatim, in order",
             maps[0].get("tags") == rows,
             f"tags={json.dumps(maps[0].get('tags'))[:300]}")


async def trust_start(sc, sid, acct, owner, **env):
    """Launch as `acct` with `owner` as the well-known account, then the Trust tab."""
    return (await start(sc, sid, nsecs=acct["sec"], extra_env={"MC_ISSUER_NPUB": owner["npub"], **env})
            and await open_trust(sc))


# ---------------------------------------------------------------- L
async def scenario_l(sc):
    """Fresh account, no kind-3: confirm the first list, follow the owner,
    calculate, publish the map."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    if not await trust_start(sc, "L", acct, owner):
        return
    if not await follow_owner(sc, owner, expect_confirm=True):
        return
    sc.check_tree("owner followed: '✓ Following' and a 'Saved on' line",
                  [(owner_follow(owner), "✓ Following"), ("community-follow-saved", "Saved on")])
    if not await calculate(sc):
        return
    if not await publish_map(sc):
        return
    k3 = nak_req(sc.box, 3, acct["pub"])
    sc.check("a kind-3 on the local relay", bool(k3), f"{len(k3)} events")
    if k3:
        p_owner = any(t[:2] == ["p", owner["pub"]] for t in k3[0].get("tags", []))
        sc.check("the newest kind-3 follows the owner", p_owner,
                 f"{len(k3[0].get('tags', []))} tags")
    check_map_rows(sc, acct["pub"])
    sc.check_tree("the Trust card reads 'Located'", [("account-trust-map-status", "Located")])


# ---------------------------------------------------------------- M
async def scenario_m(sc):
    """Existing kind-3 (three follows, content 'e2e old content'): following
    the owner appends one p tag and keeps the content byte-for-byte."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    others = [fresh_key(sc.box)["pub"] for _ in range(3)]
    sc.box.start_fake({})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    nak_publish(sc.box, acct["sec"], 3, "e2e old content", [f"p={p}" for p in others])
    await asyncio.sleep(1)
    if not await trust_start(sc, "M", acct, owner):
        return
    if not await follow_owner(sc, owner, expect_confirm=False):
        return
    sc.check_tree("owner followed without a confirmation", [(owner_follow(owner), "✓ Following")], [("community-first-list", None)])
    k3 = nak_req(sc.box, 3, acct["pub"])
    sc.check("a kind-3 on the local relay", bool(k3), f"{len(k3)} events")
    if k3:
        ps = [t for t in k3[0].get("tags", []) if t and t[0] == "p"]
        sc.check("the newest kind-3 has four p tags", len(ps) == 4, f"{len(ps)} p tags")
        sc.check("the owner is among them", any(t[1] == owner["pub"] for t in ps if len(t) > 1))
        sc.check("the content is unchanged", k3[0].get("content") == "e2e old content",
                 k3[0].get("content", "")[:60])


# ---------------------------------------------------------------- N
async def scenario_n(sc):
    """Local relay plus a dead one: the follow read is inconclusive, the error
    line shows, and nothing is published."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    if not await trust_start(sc, "N", acct, owner, MC_TRUST_RELAYS=f"{LOCAL_RELAY}, ws://127.0.0.1:10599"):
        return
    await sc.step(Step(
        "Press 'Follow' on the recommended row; the relay answer must come back inconclusive.",
        [Cand("follow-owner", "Press the 'Follow' button on the recommended owner row.",
              [("scroll_to", owner_follow(owner)), ("press", owner_follow(owner)), ("wait", 1), PARK])],
        "An error line 'Couldn't check your follow list' is shown.",
        tries=4, settle=8,
        cues=("Couldn't check your follow list", "Start a new follow list", "Following")),
        must_act=True, label="follow inconclusive")
    sc.wait_tree("community-follow-error", 30, has="Couldn't check your follow list")
    sc.check_tree("the inconclusive error line shows", [("community-follow-error", "Couldn't check your follow list")],
                  [(owner_follow(owner), "✓ Following")])
    await asyncio.sleep(2)
    sc.check("no kind-3 was published to the local relay", not nak_req(sc.box, 3, acct["pub"]))


# ---------------------------------------------------------------- O
CALC_429 = "Please wait a few minutes before recalculating"
CALC_503 = "The Brainstorm server is temporarily unavailable"


async def scenario_o(sc):
    """graperank 429, then 403, then 503: each press shows the right wording
    and can be retried; followList is posted at most once."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({"graperank": ["429", "403", "503"]})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    nak_publish(sc.box, acct["sec"], 3, "", [f"p={owner['pub']}"])
    await asyncio.sleep(1)
    if not await trust_start(sc, "O", acct, owner):
        return
    # The account already follows the owner, so step 2 is enabled at once.
    # The Noul postcondition judges the state generically (an error line and
    # the button back); the exact wording per status is the hard check after
    # each press — a second press would spend the next scripted answer.
    sc.box.mark("O 429")
    await calculate(sc, done_cond="Step 2 shows the error 'Please wait a few minutes "
                    "before recalculating' and the 'Calculate my trust scores' button.",
                    label="calc 429", tries=1)
    sc.wait_tree("community-calc-status", 25, has=CALC_429)
    sc.check_tree("429 wording", [("community-calc-status", CALC_429)], [("community-calc-status", "Brainstorm confirmed")])
    # Presses 2 and 3 are direct: the 429 text is still on screen, so Jev saw
    # nothing to do and abstained. After a failure the button is the retry
    # one. Each waits for its own '-> status' log line, then lets the app
    # repaint before the wording is read.
    for marker, status, wording in (("O 403", "403", CALC_429), ("O 503", "503", CALC_503)):
        sc.box.mark(marker)
        await act(sc, f"{status}: Calculate pressed again",
                  [("scroll_to", "community-calc-retry"), ("press", "community-calc-retry"), PARK])
        end = time.time() + 25
        while time.time() < end and not any(
                l.startswith("BS POST /user/graperank") and l.endswith(f"-> {status}")
                for l in sc.log_after(marker)):
            await asyncio.sleep(1)
        sc.check(f"{status} request reached the fake", any(
            l.startswith("BS POST /user/graperank") and l.endswith(f"-> {status}")
            for l in sc.log_after(marker)), sc.log_after(marker))
        await asyncio.sleep(4)   # the error line from the previous press is replaced by now
        sc.wait_tree("community-calc-status", 25, has=wording)
        sc.check_tree(f"{status} wording", [("community-calc-status", wording)], [("community-calc-status", "Brainstorm confirmed")])
    lines = sc.box.server_log().splitlines()
    grapes = [l for l in lines if "/user/graperank" in l]
    follows = [l for l in lines if "followList" in l]
    sc.check("exactly three graperank requests", len(grapes) == 3, grapes)
    sc.check("followList posted at most once", len(follows) <= 1, follows)


# ---------------------------------------------------------------- Q
async def scenario_q(sc):
    """graperank ok but /setup 404: Publish stays disabled, a click sends
    nothing, no kind-10040 lands."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({"setup": "404"})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    nak_publish(sc.box, acct["sec"], 3, "", [f"p={owner['pub']}"])
    await asyncio.sleep(1)
    if not await trust_start(sc, "Q", acct, owner):
        return
    if not await calculate(sc):
        return
    sc.wait_tree("community-map-status", 30, has="Brainstorm has no data for this account")
    sc.check_tree("Brainstorm has no data wording", [("community-map-status", "Brainstorm has no data for this account")])
    sc.box.mark("Q publish attempt")
    # The button is drawn disabled and has no click handler, so it has no
    # action to press either: a pointer click on it must send nothing anywhere.
    await act(sc, "the disabled Publish button was clicked", [("scroll_to", "community-map"), ("click_id", "community-map"), PARK])
    await asyncio.sleep(4)
    sc.check("no kind-10040 on the local relay", not nak_req(sc.box, 10040, acct["pub"]))
    lines = sc.log_after("Q publish attempt")
    posts = [l for l in lines if l.startswith("BS POST")]
    sc.check("the disabled publish sent no Brainstorm request", not posts, posts)


# ---------------------------------------------------------------- U
def search_for(text, hit=None):
    """Focus the follow-search box by its id, replace what it holds with
    `text` (a retry replaces the earlier attempt's), and wait for `hit`."""
    do = [FOCUS, ("scroll_to", "community-search"), ("click_id", "community-search"), ("wait", 0.5),
          ("key", "ctrl+a"), ("type", text)]
    if hit:
        do.append(("wait_id", {"id": hit, "secs": 20, "soft": True}))
    return do + [("wait", 2), PARK]


async def scenario_u(sc):
    """Fresh account, no kind-3: paste an npub the fake Brainstorm search does
    not know; its one row shows, Follow publishes a kind-3 with its p tag."""
    acct, owner, target = fresh_key(sc.box), fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    if not await trust_start(sc, "U", acct, owner):
        return
    short = target["npub"][:12]
    row = f"community-hit-{target['pub']}"
    met = await sc.step(Step(
        "Click the 'Search profiles to follow' box in step 1 and paste the npub.",
        [Cand("paste-npub", "Click the 'Search profiles to follow' input, then type the npub into it.", search_for(target["npub"], row))],
        f"The search box holds an npub, and below it one result row is labelled with the short npub "
        f"'{short}…' (the start of the same key).",
        tries=3, settle=4, cues=(short, "No profiles match", "Searching", "private key")),
        must_act=True, label="paste npub")
    # The row is the pasted key's by its id: no reading of the npub it shows.
    if not sc.check_tree("the pasted key's result row, with its Follow button", [(row, None), (f"{row}-follow", None)],
                         [("community-search-status", "No profiles match")]):
        return
    if not met:
        sc.accept_step("paste npub", "the pasted key's result row and its Follow button in the tree")
    if not await sc.step(Step(
            f"Press 'Follow' on the result row '{short}' (not the recommended row).",
            [Cand("follow-pasted", "Press the 'Follow' button on the result row of the pasted npub.",
                  [("scroll_to", f"{row}-follow"), ("press", f"{row}-follow"), ("wait", 1), PARK])],
            "A confirmation box offers 'Start a new follow list' and 'Cancel'.",
            tries=3, settle=6, cues=("Start a new follow list", "Following", "Saved on")),
            must_act=True, label="follow pasted"):
        return
    if sc.tree("community-first-list") > 0 and not await confirm_first_list(sc, "A 'Saved on N of M Nostr servers' line is shown."):
        return
    k3 = nak_req(sc.box, 3, acct["pub"])
    sc.check("a kind-3 on the local relay", bool(k3), f"{len(k3)} events")
    if k3:
        sc.check("the newest kind-3 has the pasted key's p tag",
                 any(t[:2] == ["p", target["pub"]] for t in k3[0].get("tags", [])),
                 f"{len(k3[0].get('tags', []))} tags")


# ---------------------------------------------------------------- X
def avatar_region(win, rect):
    """Screen pixels (x0, y0, x1, y1) around a search hit's avatar, from the
    window's place (`win`, AppDriver.window_box) and the hit row's tree rect
    (community-hit-<pubkey>): the row pads 12 px, then a 20 px avatar, then
    the label 10 px on (community.rs). The avatar has no node of its own."""
    x0, cy = win[0] + rect["x"], win[1] + rect["y"] + rect["h"] // 2
    return (x0 + 4, cy - 14, x0 + 40, cy + 15)


def avatar_spread(data, region):
    """How much of the round avatar in `region` the three commonest colours
    cover (2 bits per channel), from 0 to 1. An initials circle is one
    gradient fill and a letter (about 0.8 measured); the fake's
    sixteen-colour picture spreads over many (about 0.4). The avatar is the
    saturated blob in the region, shrunk by 3 px. None when nothing is found."""
    import io
    from collections import Counter
    from PIL import Image
    rx0, ry0, rx1, ry1 = region
    im = Image.open(io.BytesIO(data)).convert("RGB")
    px = im.load()
    sat = [(x, y) for y in range(ry0, ry1) for x in range(rx0, rx1)
           if 0 <= x < im.width and 0 <= y < im.height and max(px[x, y]) - min(px[x, y]) > 60]
    if len(sat) < 40:
        return None
    x0, x1 = min(x for x, _ in sat) + 3, max(x for x, _ in sat) - 3
    y0, y1 = min(y for _, y in sat) + 3, max(y for _, y in sat) - 3
    seen = Counter(tuple(c >> 6 for c in px[x, y]) for y in range(y0, y1 + 1) for x in range(x0, x1 + 1))
    total = sum(seen.values())
    return round(sum(n for _, n in seen.most_common(3)) / total, 2) if total else None


async def scenario_x(sc):
    """Search hits draw their kind-0 picture: Alice's (served by the fake) is a
    sixteen-colour PNG, Bob has none and keeps the initials circle."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    publish_search_profiles(sc.box)
    alice, bob, carol = (f"community-hit-{nak_pub(sc.box, sec)}" for sec, _ in SEARCH_PEOPLE)
    if not await trust_start(sc, "X", acct, owner):
        return
    sc.box.mark("search")
    met = await sc.step(Step(
        "Click the 'Search profiles to follow' box in step 1 and type 'e2e'.",
        [Cand("search-e2e", "Click the 'Search profiles to follow' input, then type e2e into it.", search_for("e2e", carol))],
        "The search box holds 'e2e' and below it result rows show 'Alice E2E', 'Bob E2E' and 'Carol E2E', "
        "each with a round avatar on its left.",
        tries=3, settle=4, cues=("Alice E2E", "Bob E2E", "Carol E2E", "No profiles match", "Searching")),
        must_act=True, label="search e2e")
    rows = sc.check_tree("three result rows (Alice, Bob, Carol), each with a Follow button",
                         [(r, None) for r in (alice, bob, carol)] + [(f"{r}-follow", None) for r in (alice, bob, carol)])
    if rows and not met:
        sc.accept_step("search e2e", "the three hits and their Follow buttons in the tree")
    sc.check("the fake served Alice's picture", "BS GET /pic/alice.png" in sc.log_after("search"))
    if not rows:
        return
    await asyncio.sleep(4)   # the picture loads after the row is drawn
    # Pixels, because the tree cannot say it: whether a hit draws its
    # picture or its initials. The region comes from the rows' tree rects.
    d = sc.suite.driver
    try:
        win = await d.window_box()
        within = f"{win[2]}x{win[3]}"
        rects = [d.a11y("rect", r, within=within)["rect"] for r in (alice, bob)]
    except RuntimeError as e:
        sc.check("Alice's and Bob's rows are inside the window", False, e)
        return
    data, _ = await sc.snap("avatars")
    pic, plain = (avatar_spread(data, avatar_region(win, r)) for r in rects)
    sc.check("both avatars were found on screen", pic is not None and plain is not None,
             f"picture={pic} initials={plain}")
    if pic is not None and plain is not None:
        # Relative, not fixed: the share drifts with where the row lands
        # (0.38 and 0.59 for the same picture in two runs); initials measure
        # 0.80-0.91, so the ceiling fails two initials circles.
        sc.check("Alice's avatar is a many-coloured picture, Bob's a one-colour initials circle",
                 pic <= 0.72 and plain >= 0.7 and plain - pic >= 0.15, f"top-3 colour share: picture={pic} initials={plain}")


# ---------------------------------------------------------------- V, W
RUN_POLL = "20"   # MC_RECALC_POLL_SECS: each run state stays up ~20 s
WAITING, CALCULATING = "Waiting in Brainstorm", "Calculating your scores"
PUBLISHING, DONE_LINE = "Publishing your scores", "Your scores are recalculated and published"
RECALC_WAIT = CALC_429
FOLLOW_FIRST = "Follow at least one account first so we can calculate"
SENDING = "Sending your follow list"
RUN_CUES = (WAITING, CALCULATING, PUBLISHING, DONE_LINE, RECALC_WAIT, FOLLOW_FIRST, SENDING)
STEP1_RECALC = "community-recalc"   # step 1's Recalculate, under its 'Saved on' line


async def recalculate(sc, label, rc=STEP1_RECALC):
    """One press of the Recalculate button `rc`; it must be in the window and
    not 'unavailable'. Never retried: every press is one graperank request
    (3 per 30 min live)."""
    if not await act(sc, f"{label}: Recalculate scrolled into the window", [("scroll_to", rc)]):
        return False
    n = sc.tree_node(rc)
    if not sc.check(f"{label}: the Recalculate button is there and available", n is not None and "unavailable" not in n["description"],
                    n and n["description"]):
        return False
    return await act(sc, f"{label}: Recalculate pressed", [("press", rc), PARK])


RUN_STATES = (("waiting", WAITING), ("calculating", CALCULATING), ("publishing", PUBLISHING), ("done", DONE_LINE))


async def watch_run(sc, label, rc=STEP1_RECALC, timeout=150, every=1.0):
    """Watch one run's status line (`rc`-status) in the tree until 'Done'
    or an error line shows. Each state lasts one poll (20 s), and a full
    see() takes ~40 s on the box, so neither Jev nor see() can catch them
    (run 20261026-173419: the judge first looked at 'Publishing'); a tree
    read takes about a second. Returns [(state, secs after the press,
    status text)] in the order first seen; each new state also lands as an
    evidence frame."""
    t0, seen, started = time.time(), [], False
    while time.time() - t0 < timeout:
        try:   # absent between states: not a note every second
            text = sc.suite.driver.a11y("name", f"{rc}-status")["name"]
        except RuntimeError:
            text = ""
        # The previous run's Done line stays until the repaint after the
        # press: nothing counts before this run's own first line shows.
        started = started or any(p in text for p in (SENDING, WAITING, CALCULATING, PUBLISHING,
                                                     RECALC_WAIT, FOLLOW_FIRST))
        for name, phrase in RUN_STATES if started else ():
            if phrase in text and name not in [n for n, _, _ in seen]:
                seen.append((name, round(time.time() - t0, 1), text))
                data = shot_bytes(await sc.suite.sb.screenshot())
                sc.keep(data, f"{label} {name}", text)
        if any(n == "done" for n, _, _ in seen) or RECALC_WAIT in text or FOLLOW_FIRST in text:
            break
        await asyncio.sleep(every)
    sc.notes.append({f"{label} states": [f"{n}@{t}s" for n, t, _ in seen]})
    return seen


def check_run(sc, label, seen, marker):
    """Every state shown, waiting with the fake's queue position (2), in
    order; and the fake answered the polls in that order."""
    names = [n for n, _, _ in seen]
    for name, phrase in RUN_STATES:
        sc.check(f"{label}: '{phrase}' shown", name in names, names)
    waiting = next((text for n, _, text in seen if n == "waiting"), "")
    sc.check(f"{label}: waiting shows the queue position", "2 runs ahead" in waiting, waiting[:200])
    sc.check(f"{label}: the states came in order", names == [n for n, _ in RUN_STATES], names)
    polls = [l.rsplit(" ", 1)[-1] for l in sc.log_after(marker) if l.startswith("BS GET graperankResult -> ")]
    sc.check(f"{label}: the fake answered waiting, calculating, publishing, done",
             polls[:4] == [n for n, _ in RUN_STATES], polls)


async def judge_done(sc, label, rc=STEP1_RECALC):
    """Done is the one state that stays: Jev judges it, with the finished
    time from /user/history beside it."""
    done = f"The Recalculate area says 'Done. {DONE_LINE}.' and a 'Last run finished' line shows."
    met = await sc.step(Step("Watch the recalculation finish.",
                             [Cand("wait-for-poll", "Wait a few seconds for the next progress update.", [("wait", 3)])],
                             done, tries=2, settle=1, cues=RUN_CUES + ("Last run finished",)), label=f"{label} done")
    ok = sc.check_tree(f"{label}: 'Done' and 'Last run finished' in the tree",
                       [(f"{rc}-status", DONE_LINE), (f"{rc}-last-run", "Last run finished")])
    if ok and not met:
        sc.accept_step(f"{label} done", "the Done line and 'Last run finished' in the tree")


def bs_posts(sc, marker, path):
    return [l for l in sc.log_after(marker) if l.startswith(f"BS POST {path}")]


async def scenario_v(sc):
    """Follow, Recalculate: waiting, calculating, publishing, done, then the
    finished time from history. A second Recalculate on the unchanged list
    sends graperank only, no followList."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    if not await trust_start(sc, "V", acct, owner, MC_RECALC_POLL_SECS=RUN_POLL):
        return
    if not await follow_owner(sc, owner, expect_confirm=True):
        return
    sc.box.mark("V first")
    if not await recalculate(sc, "first recalculate"):
        return
    check_run(sc, "first run", await watch_run(sc, "first run"), "V first")
    sc.wait_tree(f"{STEP1_RECALC}-last-run", 20, has="Last run finished")
    await judge_done(sc, "first run")
    sc.check("the first run sent the new follow list once",
             len(bs_posts(sc, "V first", "/user/followList")) == 1, bs_posts(sc, "V first", "/user/followList"))
    sc.check("the first run sent one graperank",
             len(bs_posts(sc, "V first", "/user/graperank")) == 1, bs_posts(sc, "V first", "/user/graperank"))
    sc.check("history was read", any(l.startswith("BS GET /user/history") for l in sc.log_after("V first")))

    sc.box.mark("V second")
    if not await recalculate(sc, "second recalculate"):
        return
    check_run(sc, "second run", await watch_run(sc, "second run"), "V second")
    follows, grapes = bs_posts(sc, "V second", "/user/followList"), bs_posts(sc, "V second", "/user/graperank")
    sc.check("the unchanged follow list was not sent again", not follows, follows)
    sc.check("the second run sent exactly one graperank", len(grapes) == 1, grapes)


async def wait_log(sc, marker, path, n=1, timeout=25):
    """Poll the fake log until `n` `path` POSTs show after `marker`."""
    end = time.time() + timeout
    while time.time() < end and len(bs_posts(sc, marker, path)) < n:
        await asyncio.sleep(1)
    return bs_posts(sc, marker, path)


async def scenario_w(sc):
    """graperank 429, then 403, then a run that fails NO_ELIGIBLE_USERS: each
    shows its wording and spends exactly one graperank. With no follows the
    button is unavailable and a click sends nothing."""
    acct, owner, other = fresh_key(sc.box), fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({"graperank": ["429", "403", "ok"], "run": ["waiting", "fail:NO_ELIGIBLE_USERS"]})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    nak_publish(sc.box, acct["sec"], 3, "", [f"p={other['pub']}"])
    await asyncio.sleep(1)
    if not await trust_start(sc, "W", acct, owner, MC_RECALC_POLL_SECS="5"):
        return
    if not await follow_owner(sc, owner, expect_confirm=False):
        return
    # Counted from the first press: 1, then 2, then 3 — a press that missed
    # (or a stale line from the previous run) cannot pass the next round.
    status = f"{STEP1_RECALC}-status"
    for n, (marker, want) in enumerate([("W 429", RECALC_WAIT), ("W 403", RECALC_WAIT),
                                        ("W fail", FOLLOW_FIRST)], start=1):
        sc.box.mark(marker)
        if not await recalculate(sc, marker):
            return
        grapes = await wait_log(sc, "W 429", "/user/graperank", n=n)
        if not sc.check(f"{marker}: graperank count is {n}", len(grapes) == n, grapes):
            return
        sc.wait_tree(status, 40, has=want)
        sc.check_tree(f"{marker}: its wording shows", [(status, want)], [(status, DONE_LINE)])
    sc.check("the failed run was polled",
             any(l.startswith("BS GET graperankResult -> fail") for l in sc.log_after("W fail")))
    await asyncio.sleep(6)
    grapes = bs_posts(sc, "W 429", "/user/graperank")
    sc.check("nothing retried: three graperank requests in all", len(grapes) == 3, grapes)
    follows = bs_posts(sc, "W 429", "/user/followList")
    sc.check("followList posted exactly once", len(follows) == 1, follows)

    # A fresh key with no follow list: the Trust tab's Recalculate (in its
    # web-of-trust card, under 'Followed someone new?') is unavailable.
    bare = fresh_key(sc.box)
    if not await trust_start(sc, "W2", bare, owner, MC_RECALC_POLL_SECS="5"):
        return
    rc = "account-trust-recalc"
    if not await act(sc, "the Trust tab's Recalculate scrolled into the window", [("scroll_to", rc), PARK]):
        return
    sc.check_tree("no follows: the follow-first line shows", [(f"{rc}-follow-first", FOLLOW_FIRST)])
    n = sc.tree_node(rc)
    sc.check("the Recalculate button is unavailable (its description)", n is not None and "unavailable" in n["description"],
             n and n["description"])
    sc.box.mark("W disabled")
    # It has no click handler, so no action to press: the pointer click a
    # user would make.
    if not await act(sc, "the disabled Recalculate button was clicked", [("click_id", rc), PARK]):
        return
    await asyncio.sleep(5)
    # A window re-activation may re-read /setup; only the run's calls count.
    sent = [l for l in sc.log_after("W disabled")
            if l.startswith("BS POST") or "graperankResult" in l or "/user/history" in l]
    sc.check("the disabled Recalculate sent nothing to Brainstorm", not sent, sent)


# ---------------------------------------------------------------- S
async def scenario_s(sc):
    coord = "39998:%s:post-city" % sc.suite.keys["house"]["pub"]
    sc.box.start_fake({"create": "ok", "dup_coordinate": coord})
    if not await start(sc, "S"):
        return
    # The goal needs only the header button, as in H and R; the first bounty
    # is auto-selected, so its detail pane follows and the check below reads it.
    ok = await sc.step(Step("Show the Bounties screen with the house issuer selected (its header button reads 'New DList + bounty').",
                            [house(sc), BOUNTIES], "The Bounties screen is shown and its header button reads 'New DList + bounty'.",
                            settle=2, cues=HOUSE_CUES + ("New bounty on this list",)), must_act=True, label="house on bounties")
    if not (ok and sc.check_tree("issuer sees the button", [("bounty-detail-new-on-list", None)], [CLAIMANT])):
        return
    await sc.step(Step("Open the form from the detail pane's button.",
                       [Cand("press-new-on-list", "Press the 'New bounty on this list' button in the detail pane.",
                             [("press", "bounty-detail-new-on-list"), ("wait_id", "bounty-form-criteria"), PARK])],
                       "A form titled 'New bounty on this list' is open with a 'Bounty criteria' field and no 'Item (singular)', 'Items (plural)' or 'List description' field.",
                       settle=2, cues=("New bounty on this list", "Item (singular)", "Bounty criteria", "Cancel")), label="open list form")
    sc.check_tree("existing-list form: criteria, no list fields",
                  [("bounty-form", "New bounty on this list"), ("bounty-form-criteria", None), ("bounty-form-cancel", None)],
                  [(f"bounty-form-{f}", None) for f in ("item-singular", "item-plural", "description")])
    sc.box.mark("S submit")
    await act(sc, "the existing-list form filled and submitted",
              type_fields([("criteria", "A post city name, one per claim"), ("amount", "100"), ("cap", "200"), ("min-rank", "2")])
              + [("key", "Return")])
    # The box can lag the keystrokes by several seconds: a fixed sleep once
    # caught the untouched form before Enter landed. Wait for the second row.
    sc.wait_tree("bounties-tab-house", 30, has="House bounties (2)")
    lines = sc.log_after("S submit")
    posts = [l for l in lines if l.startswith("POST /api/bounties")]
    sc.check("exactly one create, on the existing list", len(posts) == 1 and posts[0].endswith("coord=post-city"), posts)
    pubs = [l for l in lines if l.startswith("POST strfry/publish")]
    sc.check("no list publish reached the server", not pubs, pubs)
    sc.check_tree("form closed after the create", [("bounties-tab-house", "House bounties (2)")], [("bounty-form", None)])
    # Direct, as in H: the rail avatars all read "New account", so the judge
    # cannot tell the claimant's tab apart and halted here (p 0.56, abstain
    # 0.40). The tree check below decides.
    await act(sc, "claimant's tab and Bounties pressed", [("press", tab(sc, "claimant"))] + TO_BOUNTIES)
    sc.wait_tree("bounties-new", 30, has=CLAIMANT[1])
    sc.check_tree("non-issuer has no button", [CLAIMANT], [("bounty-detail-new-on-list", None)])


# ---------------------------------------------------------------- T
ELIGIBLE_TAB = Cand("eligible-tab", "Press the 'Eligible for me to claim' tab above the bounty list.",
                    [("press", "bounties-tab-eligible"), ("wait", 1), PARK])


def eligible_gets(lines):
    """(viewer, session) of each eligible GET line in the fake's log."""
    out = []
    for l in lines:
        if l.startswith("GET /api/bounties/eligible"):
            f = dict(p.split("=", 1) for p in l.split() if "=" in p)
            out.append((f.get("viewer"), f.get("session")))
    return out


def logged_in_first(lines, who):
    """`who` (8-char prefix) logged in before its first eligible GET."""
    login = next((i for i, l in enumerate(lines) if l == f"POST login-user pubkey={who}"), None)
    get = next((i for i, l in enumerate(lines)
                if l.startswith(f"GET /api/bounties/eligible viewer={who} session={who}")), None)
    return login is not None and get is not None and login < get


async def scenario_t(sc):
    house_pub, claimant = sc.suite.keys["house"]["pub"], sc.suite.keys["claimant"]["pub"]
    other = "e" * 64
    lists = {
        claimant: [["elig-c", f"39998:{other}:eligible-claimant", 3, 3, "e2e"]],
        # The house's own bounty (rank 100 for self) and a rank-2 row on a
        # min-3 auto-pay bounty: the server sends both, the app hides both.
        house_pub: [["elig-h", f"39998:{other}:eligible-house", 2, None, "e2e"],
                    ["elig-own", f"39998:{house_pub}:eligible-own", 100, 2, "e2e"],
                    ["elig-gated", f"39998:{other}:eligible-gated", 2, 3, "e2e"]],
    }
    sc.box.start_fake({"eligible": lists, "eligible_delay": {claimant: 8}})
    sc.box.mark("T launch")
    # The claimant is already active at launch.
    if not await start(sc, "T"):
        return
    await sc.step(Step("Show the Bounties screen for the claimant (the claimant's header button reads 'Submit a bounty claim').",
                       [Cand("press-bounties", "Press the sidebar's Bounties tab.", TO_BOUNTIES + [("wait_id", {"id": "bounties-tab-eligible", "soft": True})])],
                       "The Bounties screen shows the 'Eligible for me to claim' tab and the header button reads 'Submit a bounty claim'.", settle=3,
                       cues=("Submit a bounty claim", "New DList + bounty", "Eligible for me to claim")),
                  must_act=True, label="claimant")
    if not sc.check_tree("claimant selected, Eligible tab shown", [CLAIMANT, ("bounties-tab-eligible", None)]):
        return
    # Before the step: its switch sends the house read.
    sc.box.mark("T switch")
    # One candidate: the switch must land while the claimant's 8 s GET is
    # held. The house's rail tab switches as super+] does (set_account) and
    # keeps the screen.
    await sc.step(Step("Open the 'Eligible for me to claim' tab, then switch to the house account while its list is still loading.",
                       [Cand("tab-then-switch", "Press the 'Eligible for me to claim' tab, then the house's account tab in the left rail.",
                             [("press", "bounties-tab-eligible"), ("stamp", "tab"), ("press", tab(sc, "house")), ("wait", 1), PARK])],
                       "The eligible list shows a row on 'eligible-house' (the house is selected; on this tab its header button reads 'Submit a bounty claim').",
                       tries=1, settle=1, cues=("eligible-house", "Submit a bounty claim", "Eligible for me to claim")),
                  must_act=True, label="tab then switch")
    await asyncio.sleep(max(0, sc.stamps.get("tab", time.time()) + 12 - time.time()))
    # On the Eligible tab the house claims too, so its header button reads
    # "Submit a bounty claim"; only the house's eligible row marks the house.
    row = sc.tree_node(ROW, name="eligible-house")
    sc.check_tree("house sees its own eligible list only",
                  [(ROW, "eligible-house"), CLAIMANT] + ([(f"{row['id']}-qualify", "you qualify")] if row else []),
                  [(ROW, "eligible-claimant"), (ROW, "eligible-own"), (ROW, "eligible-gated"), ISSUER,
                   ("bounty-detail-title", "eligible-claimant")])
    lines = sc.log_after("T launch")
    gets = eligible_gets(lines)
    sc.check("every eligible GET is for the session's own account", gets and all(v == s for v, s in gets), gets)
    sc.check("the claimant and the house each read their list",
             (claimant[:8], claimant[:8]) in gets and (house_pub[:8], house_pub[:8]) in gets, gets)
    sc.check("each account logged in before its first eligible read",
             logged_in_first(lines, claimant[:8]) and logged_in_first(lines, house_pub[:8]), lines)
    sc.check("the house read went out after the switch", (house_pub[:8], house_pub[:8]) in eligible_gets(sc.log_after("T switch")),
             sc.log_after("T switch"))
    # Back to the claimant: its list is read again and shows.
    sc.box.mark("T back")
    await act(sc, "the claimant's tab pressed", [("press", tab(sc, "claimant")), PARK])
    sc.wait_tree(ROW, 20, name="eligible-claimant")
    sc.check_tree("the claimant's list shows after switching back", [(ROW, "eligible-claimant"), CLAIMANT],
                  [(ROW, "eligible-house"), ("bounty-detail-title", "eligible-house")])
    sc.check("a second claimant read went out", (claimant[:8], claimant[:8]) in eligible_gets(sc.log_after("T back")),
             sc.log_after("T back"))
    # Nothing left to claim: the empty line, and the detail pane lets go of
    # the row (the fake still answers its detail with the real coordinate).
    sc.box.set_mode({"eligible": {claimant: []}})
    # xfwm4 grabs Super+r on the box, so XTEST never delivers the app's refresh
    # chord. Pressing the tab that is already open re-reads the list the same way.
    await act(sc, "the open Eligible tab pressed again", ELIGIBLE_TAB.do)
    sc.wait_tree("bounties-empty", 20)
    sc.check_tree("empty eligible list says so, no stale detail",
                  [("bounties-empty", "No open bounties match"), ("bounty-detail-empty", None)], [(ROW, "eligible-claimant")])
    sc.check("app still running", sc.box.app_alive())


SCENARIOS = {
    "A": ("submit lock while a create hangs", scenario_a),
    "B": ("502 on create: hint and refetch", scenario_b),
    "C": ("400 on create: no hint, no refetch", scenario_c),
    "D": ("duplicate open bounty refused locally", scenario_d),
    "E": ("401 re-login once", scenario_e),
    "F": ("profile scan 500, then Try again", scenario_f),
    "G": ("nsec leaves the clipboard after 30 s", scenario_g),
    "H": ("two-click remove arms and disarms", scenario_h),
    "L": ("trust: first list, follow owner, calc, map", scenario_l),
    "M": ("trust: follow merges into an existing kind-3", scenario_m),
    "N": ("trust: inconclusive contact read refuses", scenario_n),
    "O": ("trust: graperank 429/403/503 wording and retries", scenario_o),
    "Q": ("trust: /setup 404 keeps publish disabled", scenario_q),
    "U": ("trust: follow a pasted npub the search does not know", scenario_u),
    "X": ("trust: search hit draws its picture, initials without one", scenario_x),
    "V": ("trust: Recalculate shows each run state and skips an unchanged list", scenario_v),
    "W": ("trust: Recalculate 429/403/NO_ELIGIBLE_USERS wording, disabled with no follows", scenario_w),
    "S": ("issuer posts a new bounty on an existing list", scenario_s),
    "T": ("eligible list survives an account switch mid-request", scenario_t),
    "P": ("paid: real bounty, claim, settle", scenario_p),
    "R": ("newcomers: create two accounts in the app, claim the house bounty, both paid", scenario_r),
    "Y": ("two apps: newcomers in separate app processes claim the house bounty, both paid, each sees the other", scenario_y),
}
