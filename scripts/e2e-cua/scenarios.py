"""The scenarios. Each is `async def x(sc)`; register it in SCENARIOS.

sc.box.launch(...)       relaunch the app on a fresh MC_STORE_DIR
sc.box.start_fake(mode)  fresh fake instance + relay in the given mode
sc.step(Step, must_act)  one agent.py step: Jev picks a candidate, judges `done`
sc.check / check_screen  hard checks; every scenario needs at least one
sc.log_after(marker)     fake-server log lines after sc.box.mark(marker)

Candidate actions are complete; none of them may create a wallet, remove an
account for real, or publish anywhere but the fake server (P excepted).
"""
import asyncio, os, time
from agent import Step, Cand
from suite import has

FOCUS = ("focus", "Magic Carpet")
PARK = ("shell", "xdotool mousemove 20 740")   # hover tooltips hide the header
# Rail geometry at 1280x960 (window at 0,0): the avatars sit 55 px apart.
HOUSE_XY, CLAIMANT_XY, PLUS_XY = (34, 190), (34, 245), (34, 300)

BOUNTIES = Cand("open-bounties", "Press super+2 to open the Bounties screen.", [FOCUS, ("key", "super+2")])
DASHBOARD = Cand("open-dashboard", "Press super+1 to open the Dashboard.", [FOCUS, ("key", "super+1")])
WALLET = Cand("open-wallet", "Press super+6 to open the Wallet screen of the selected account.", [FOCUS, ("key", "super+6")])
NEW_FORM = Cand("new-bounty-form", "Press super+n to open the 'New DList + bounty' form.", [FOCUS, ("key", "super+n")])
HOUSE = Cand("click-house-avatar", "Click the first avatar in the left rail to select the house issuer, then press super+2 to return to Bounties.",
             [FOCUS, ("click", HOUSE_XY), ("wait", 2), PARK, ("key", "super+2"), ("wait", 1)])
NEXT_ACCOUNT = Cand("next-account", "Press super+] to select the next account in the rail.", [FOCUS, ("key", "super+bracketright")])
HOUSE_CUES = ("New DList + bounty", "Submit a bounty claim", "Bounties")


def fill(name, cap="200"):
    """The seven form fields; the last Enter submits."""
    words = name.replace("-", " ")
    return Cand("fill-and-publish",
                f"Type the seven fields with Enter after each: singular '{name}', plural, description, criteria, reward 100, cap {cap}, min rank 2. The last Enter submits.",
                [FOCUS, ("type", name), ("key", "Return"), ("wait", 0.4),
                 ("type", name + "s"), ("key", "Return"), ("wait", 0.4),
                 ("type", f"{words}s for the e2e suite"), ("key", "Return"), ("wait", 0.4),
                 ("type", f"A {words} name, one per claim"), ("key", "Return"), ("wait", 0.4),
                 ("key", "ctrl+a"), ("type", "100"), ("key", "Return"), ("wait", 0.4),
                 ("key", "ctrl+a"), ("type", cap), ("key", "Return"), ("wait", 0.4),
                 ("key", "ctrl+a"), ("type", "2"), ("key", "Return"), ("stamp", "submitted")])


async def open_house_form(sc):
    """Bounties screen, house selected, New DList + bounty form open."""
    ok = await sc.step(Step("Show the Bounties screen with the house issuer selected (its header button reads 'New DList + bounty'; the claimant would see 'Submit a bounty claim').",
                            [HOUSE, BOUNTIES], "The Bounties screen is shown and its header button reads 'New DList + bounty'.", settle=2, cues=HOUSE_CUES),
                       label="house on bounties")
    if not (ok and sc.check_screen("house selected", ["New DList + bounty"], ["Submit a bounty claim"])):
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
    sc.store = sc.box.launch("A", sc.suite.run_id)
    if not await open_house_form(sc):
        return
    sc.box.mark("A submit")
    # Submit and the four attempts in one candidate: all of it must land
    # inside the app's 20 s client timeout, and an OCR pass takes ~5 s here.
    # A snap after each attempt is the per-action evidence; the feed's three
    # amber lines below are the per-action verdict.
    await sc.step(Step(
        "Submit a 'hang-city' bounty; while its create request is in flight, try in turn: switch account (super+]), add an account (+), remove the active account (two clicks), and Escape.",
        [Cand("submit-then-try-all-four",
              "Fill and submit the form, then press super+], click the '+' in the rail, click 'Remove this account' then 'Click again to remove' in the sidebar, then press Escape.",
              fill("hang-city").do + [
                  ("wait", 1.5), ("snap", "submitted"),
                  ("key", "super+bracketright"), ("wait", 0.8), ("snap", "after super+]"),
                  ("click", PLUS_XY), ("wait", 0.8), PARK, ("snap", "after plus"),
                  ("click_fine", "Remove this account"), ("wait", 0.8), ("click_fine", "Click again to remove"), ("wait", 0.8), PARK,
                  ("snap", "after remove x2"),
                  ("key", "Escape"), ("stamp", "attempts done"), ("wait", 0.8)])],
        "The bounty form is still open and still reads 'Publishing…'; the header button still reads 'New DList + bounty' (the house is still selected); no account setup screen.",
        tries=1, settle=0.5, cues=("Publishing", "New DList + bounty", "Submit a bounty claim", "Your Nostr account")),
        must_act=True, label="submit and in-flight attempts", second_look=False)   # a later look would be past the timeout
    t_submit = sc.stamps.get("submitted", time.time())
    done_at = sc.stamps.get("attempts done", time.time())
    sc.check("the attempts ran inside the client timeout", done_at - t_submit < 18, f"{done_at - t_submit:.1f} s after submit")
    sc.check_screen("switch, setup, remove and esc all refused",
                    ["Publishing", "Cancel (esc)", "New DList + bounty"], ["Submit a bounty claim", "Your Nostr account"])
    # The client timeout is 20 s; give it 26 from the submit.
    await asyncio.sleep(max(0, t_submit + 26 - time.time()))
    # After the timeout the form reads "Checking the list…" until the
    # refetch lands (7b1dad9), then unlocks with the hint.
    await sc.step(Step("Wait for the create request to time out and the list check after it.", [Cand("wait", "Wait 5 seconds.", [("wait", 5)])],
                       "The bounty form is still open, 'Publishing…' is gone, and an error line says the server may have created this bounty.",
                       tries=3, settle=1, cues=("Publishing", "Checking the list", "may have created")), label="timeout unlocks")
    await sc.wait_for(["may have created"], timeout=20, label="list check done")
    sc.check_screen("form unlocked after the client timeout and the list check", ["Cancel (esc)", "may have created"],
                    ["Publishing", "Checking the list"])
    await sc.step(Step("Open the Dashboard to read the Recent activity feed.", [DASHBOARD],
                       "The Recent activity panel lists 'A submit is in flight; wait for it to finish' at least once.",
                       settle=2, cues=("A submit is in flight", "Recent activity")), label="activity feed")
    n = sc.count("submit is in flight")
    sc.check("amber 'A submit is in flight' line for switch, setup and remove (3x)", n >= 3, f"seen {n}x")
    posts = [l for l in sc.log_after("A submit") if l.startswith("POST /api/bounties")]
    sc.check("exactly one create request went out", len(posts) == 1, posts)
    sc.check("the active account was not removed", sc.box.accounts_in_store(sc.store) == 2, f"{sc.box.accounts_in_store(sc.store)} accounts in store")


# ---------------------------------------------------------------- B, C
async def scenario_b(sc):
    sc.box.start_fake({"create": "502"})
    sc.box.launch("B", sc.suite.run_id)
    if not await open_house_form(sc):
        return
    sc.box.mark("B submit")
    await submit_form(sc, "gateway-city", "The form shows an error saying the server may have created this bounty and to check the list before retrying.",
                      ("may have created", "check the list"))
    # "Checking the list…" first, then the hint once the refetch lands.
    await sc.wait_for(["may have created"], timeout=20, label="after 502")
    sc.check_screen("'may have created' hint shown, form unlocked", ["may have created"], ["Checking the list", "Publishing"])
    lines = sc.log_after("B submit")
    post = next((i for i, l in enumerate(lines) if l.startswith("POST /api/bounties")), None)
    refetch = post is not None and any(l.startswith("GET /api/bounties issuer") for l in lines[post + 1:])
    sc.check("GET /api/bounties refetch after the 502", refetch, lines[-6:])


async def scenario_c(sc):
    sc.box.start_fake({"create": "400"})
    sc.box.launch("C", sc.suite.run_id)
    if not await open_house_form(sc):
        return
    sc.box.mark("C submit")
    await submit_form(sc, "refused-city", "The form shows an error about amountSats and no 'may have created' hint.", ("amountSats", "may have created"))
    await asyncio.sleep(5)
    await sc.see("after 400")
    sc.check_screen("400 shows the refusal, no hint", ["amountSats"], ["may have created"])
    lines = sc.log_after("C submit")
    post = next((i for i, l in enumerate(lines) if l.startswith("POST /api/bounties")), None)
    sc.check("the create went out", post is not None, lines[-6:])
    sc.check("no refetch after the 400", post is not None and not any(l.startswith("GET /api/bounties issuer") for l in lines[post + 1:]), lines[-6:])


# ---------------------------------------------------------------- D
async def scenario_d(sc):
    coord = "39998:%s:dup-city" % sc.suite.keys["house"]["pub"]
    sc.box.start_fake({"create": "ok", "dup_coordinate": coord})
    sc.box.launch("D", sc.suite.run_id)
    if not await open_house_form(sc):
        return
    sc.box.mark("D submit")
    await submit_form(sc, "dup-city", "The form shows 'An open bounty for this list already exists. Pick it from the list instead.'",
                      ("already exists", "Publishing"))
    await asyncio.sleep(2)
    await sc.see("after dup submit")
    sc.check_screen("duplicate refused in the form", ["already exists"])
    sent = [l for l in sc.log_after("D submit") if l.startswith(("POST /api/bounties", "POST strfry/publish"))]
    sc.check("no request sent for the duplicate", not sent, sent)


# ---------------------------------------------------------------- E
async def scenario_e(sc):
    sc.box.start_fake({"create": "401once"})
    sc.box.launch("E", sc.suite.run_id)
    if not await open_house_form(sc):
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
    sc.check_screen("401 once: form closed without an error", [], ["not logged in", "Cancel (esc)"])
    # 401 twice: a new mode file resets the fake's counter.
    sc.box.set_mode({"create": "401always"})
    sc.box.mark("E twice")
    await sc.step(Step("Open the new bounty form again as the house issuer.", [NEW_FORM, BOUNTIES],
                       "A form titled 'New DList + bounty' is open with an 'Item (singular)' field.", settle=2, cues=("Item (singular)",)), label="reopen form")
    await submit_form(sc, "deny-city", "The form is still open with an error line, and no 'Publishing…'.", ("not logged in", "Publishing", "Item (singular)"), settle=4)
    await asyncio.sleep(3)
    await sc.see("after 401 twice")
    lines = sc.log_after("E twice")
    posts = [l for l in lines if l.startswith("POST /api/bounties")]
    logins = [l for l in lines if l.startswith("POST login-user")]
    sc.check("401 twice: two creates, one re-login, then stop", len(posts) == 2 and len(logins) == 1, lines)
    sc.check_screen("401 twice: clean error in the open form", ["Cancel (esc)", "401"], ["Publishing"])
    sc.check("app still running", sc.box.app_alive())


# ---------------------------------------------------------------- F
CLAIMANT_WALLET = Step(
    "Show the Wallet screen of the claimant account. The house's Wallet only says 'The house key's wallet is not managed here.'; if that shows, select the next account.",
    [WALLET, Cand("next-account-wallet", "Press super+] to select the next account, then super+6 for its Wallet.",
                  [FOCUS, ("key", "super+bracketright"), ("wait", 2), ("key", "super+6")])],
    "The Wallet screen is shown for an account that is not the house: no 'house key's wallet is not managed here' line.",
    settle=3, cues=("not managed here", "Wallet", "read this profile", "Try again", "Create a Coinos wallet"))


async def scenario_f(sc):
    sc.box.start_fake({"scan": "fail"})
    sc.box.launch("F", sc.suite.run_id)
    if not await sc.step(CLAIMANT_WALLET, label="claimant wallet"):
        return
    sc.check_screen("scan 500: 'Couldn't read this profile' with 'Try again'", ["read this profile", "Try again"], ["Create a Coinos wallet"])
    sc.box.set_mode({"scan": "ok"})
    sc.box.mark("F retry")
    await sc.step(Step("The profile scan is healthy again; press the 'Try again' button on the Wallet screen. Do not create a wallet.",
                       [Cand("try-again", "Click the 'Try again' button under 'Couldn't read this profile'.", [FOCUS, ("click_text", "Try again"), ("wait", 1), PARK])],
                       "'Couldn't read this profile' is gone and the wallet panel offers 'Create a Coinos wallet for this account'.",
                       tries=2, settle=4, cues=("read this profile", "Create a Coinos wallet", "Checking the profile")), must_act=True, label="try again")
    sc.check_screen("retry reaches the Read state", ["Create a Coinos wallet"], ["read this profile"])
    ok = [l for l in sc.log_after("F retry") if l.startswith("GET /api/strfry/scan") and l.endswith("200 []")]
    sc.check("the retry re-read the profile scan", bool(ok), sc.log_after("F retry")[-4:])


# ---------------------------------------------------------------- G
CHIP_XY = (135, 116)  # the account name at the top of the sidebar
IDENTITY = Step("Open the Identity & keys tab of the active account's page.",
                [Cand("chip-then-identity", "Click the account name at the top of the sidebar (opens the account page), then click the 'Identity & keys' tab.",
                      [FOCUS, ("click", CHIP_XY), ("wait", 1.5), PARK, ("click_text", "Identity"), ("wait", 1), PARK])],
                "The 'Identity & keys' tab shows 'Public identity' and 'Secret key (nsec)'.", settle=2,
                cues=("Public identity", "Secret key", "Identity"))


async def scenario_g(sc):
    sc.box.start_fake({})
    sc.box.launch("G", sc.suite.run_id)
    if not await sc.step(IDENTITY, label="identity tab"):
        return
    await sc.step(Step("Copy the nsec: click the 'copy' button in the 'Secret key (nsec)' row (next to 'Reveal'). Never click Reveal.",
                       [Cand("copy-nsec", "Click the 'copy' button to the right of 'Reveal' in the Secret key row.",
                             [FOCUS, ("click_text", {"text": "copy", "near": "Reveal"}), ("stamp", "copy"), ("wait", 0.5), PARK])],
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
async def remove_armed_flow(sc, who, expect_sats=False):
    """Arm from the sidebar, walk away, arm from the Identity card, walk away."""
    await sc.step(Step(f"Arm 'Remove this account' for the {who}: click the sidebar link once. Do not click a second time.",
                       [Cand("arm-sidebar", "Click 'Remove this account' under the account name in the sidebar, once.",
                             [FOCUS, ("click_fine", "Remove this account"), ("wait", 1), PARK])],
                       "The sidebar link now reads 'Click again to remove'.", tries=1, settle=2, cues=("Click again to remove", "Remove this account")),
                  must_act=True, label=f"{who} arm sidebar")
    sc.check_screen(f"{who}: sidebar arms 'Click again to remove'", ["Click again to remove"])
    if expect_sats:
        # Arming re-reads the balance: "Checking balance…", then the warning.
        screen = await sc.wait_for(["still holds"], ["could not be read"], timeout=25, every=2, label=f"{who} balance")
        sc.check(f"{who} (Coinos wallet): armed remove warns about sats left", has(screen, "still holds"),
                 "balance could not be read" if has(screen, "could not be read") else "no 'This wallet still holds N sats' line while armed")
    await sc.step(Step("Navigate away: press super+1 for the Dashboard.", [DASHBOARD],
                       "The Dashboard is shown and the sidebar reads 'Remove this account' again, not 'Click again to remove'.",
                       settle=2, cues=("Click again to remove", "Remove this account", "Dashboard")), must_act=True, label=f"{who} leave (sidebar)")
    sc.check_screen(f"{who}: navigating away disarms (sidebar)", ["Remove this account"], ["Click again to remove"])
    if not await sc.step(IDENTITY, label=f"{who} identity tab"):
        return
    await sc.step(Step(f"Arm the 'Remove…' button on the {who}'s Identity card ('Remove this account' card at the bottom), once.",
                       [Cand("arm-card", "Click the 'Remove…' button on the right of the 'Remove this account' card, once.",
                             [FOCUS, ("click_text", {"text": "Remove", "near": "Deletes the nsec"}), ("wait", 1), PARK])],
                       "The card's button and the sidebar link read 'Click again to remove'.", tries=1, settle=2,
                       cues=("Click again to remove", "Remove", "Deletes the nsec")), must_act=True, label=f"{who} arm card")
    n = sc.count("click again to remove")
    sc.check(f"{who}: Identity card arms (card + sidebar)", n >= 2, f"seen {n}x")
    await sc.step(Step("Navigate away: press super+2 for Bounties.", [BOUNTIES],
                       "The Bounties screen is shown and nothing reads 'Click again to remove'.", settle=2,
                       cues=("Click again to remove", "Bounties")), must_act=True, label=f"{who} leave (card)")
    sc.check_screen(f"{who}: navigating away disarms (card)", [], ["Click again to remove"])


async def scenario_h(sc):
    sc.box.start_fake({})
    sc.store = sc.box.launch("H", sc.suite.run_id, seed=True)
    await sc.step(Step("Show Bounties with the house issuer selected.", [HOUSE, BOUNTIES],
                       "The Bounties screen is shown and its header button reads 'New DList + bounty'.", settle=2, cues=HOUSE_CUES), label="house")
    await remove_armed_flow(sc, "house")
    # The avatar click is idempotent; a retried super+] would toggle back.
    await sc.step(Step("Select the claimant account and show Bounties (the claimant's header button reads 'Submit a bounty claim').",
                       [Cand("claimant-avatar", "Click the second avatar in the left rail, then press super+2 for Bounties.",
                             [FOCUS, ("click", CLAIMANT_XY), ("wait", 2), PARK, ("key", "super+2"), ("wait", 1)])],
                       "The Bounties header button reads 'Submit a bounty claim'.", settle=3,
                       cues=("Submit a bounty claim", "New DList + bounty")), label="claimant")
    sc.check_screen("claimant selected", ["Submit a bounty claim"])
    await remove_armed_flow(sc, "claimant", expect_sats=True)
    sc.check("nothing was removed", sc.box.accounts_in_store(sc.store) == 2, f"{sc.box.accounts_in_store(sc.store)} accounts in store")


# ---------------------------------------------------------------- P (paid)
async def scenario_p(sc):
    """One real bounty (100 sats, cap 100) and one claim against tapestry.
    Never retried."""
    stamp = sc.suite.run_id.replace("-", "")[-10:]
    name = f"e2e-{stamp}"
    os.environ["LIST_NAME"], os.environ["CLAIM_ITEM"] = name, f"E2E Roasters {stamp}"
    import importlib
    import demo_bounty, demo_claim, demo_settled
    for m in (demo_bounty, demo_claim, demo_settled):
        importlib.reload(m)   # they read LIST_NAME / CLAIM_ITEM at import
    meta = sc.suite.meta
    meta["float_before"] = sc.box.float_sats()
    sc.check("float at least 200 sats", (meta["float_before"] or 0) >= 200, f"{meta['float_before']} sats")
    sc.box.launch("P", sc.suite.run_id, target="instance", seed=True)
    try:
        await paid_flow(sc, name, demo_bounty, demo_claim, demo_settled)
    finally:
        meta["float_after"] = sc.box.float_sats()
        spent = (meta["float_before"] or 0) - (meta["float_after"] or 0)
        sc.check("float dropped by one payout, not more", 0 < spent <= 150, f"{meta['float_before']} -> {meta['float_after']} sats")


async def paid_flow(sc, name, demo_bounty, demo_claim, demo_settled):
    import dataclasses
    once = lambda s: dataclasses.replace(s, tries=1)
    b = demo_bounty.STEPS
    # The app opens on the Dashboard, whose cues overlap demo_bounty's first
    # step; live Jev read it as "already done" (run 20260923-110709). Land on
    # Bounties first, with the action forced.
    if not await sc.step(Step("Open the Bounties screen.", [demo_bounty.OPEN_BOUNTIES],
                              "The screen heading reads 'Bounties' and a hint line mentions 'select' and 'switch account'.",
                              settle=2, cues=("select", "switch account", "Recent activity")), must_act=True, label="open bounties"):
        return
    if not await sc.steps_(b[:3]):
        return
    # Gate: FILL's keystrokes end in Enter. In a claim form they would claim
    # (and pay out) some other bounty, so the house's bounty form must be up.
    if not sc.check_screen("the house's New DList + bounty form is open", ["New DList + bounty", "Cancel (esc)"],
                           ["Claim an item on", "Submit a bounty claim"]):
        await sc.suite.driver.act("key", "Escape")
        return
    # demo_bounty's FILL, with cap = reward = 100: the bounty closes after its
    # one payout, so a leftover e2e bounty can never pay again. tries=1: a
    # retried Enter could create a second bounty.
    publish = dataclasses.replace(b[3], tries=1, cands=[fill(name, cap="100")] + b[3].cands[1:])
    if not await sc.step(publish, must_act=True, label="publish bounty"):
        return
    c = demo_claim.ALL
    if not await sc.steps_(c[:2]):
        return
    # Gate: the claim form must be on this run's bounty, or we'd pay another one.
    if not sc.check_screen("claim form is on this run's bounty", [f"Claim an item on {name}"]):
        await sc.suite.driver.act("key", "Escape")
        sc.notes.append("claim form was not on this run's bounty; cancelled, nothing claimed")
        return
    # tries=1: a second claim would be a second payout.
    if not await sc.step(once(c[2]), must_act=True, label="submit claim"):
        return
    await sc.step(dataclasses.replace(c[3], tries=6), label="wait for settlement")
    screen = await sc.wait_for(["Payment settled", "Zap receipt"], ["Auto-pay failed"], timeout=150, every=10, label="settle poll")
    if has_failed(screen):
        sc.notes.append("Auto-pay failed on the claim card; not retried, not reset")
    await sc.step(demo_settled.STEPS[0], label="settled after refresh")
    both = sc.screen + "\n" + screen
    sc.check_screen("Payment settled and Zap receipt on the claim card", ["Payment settled", "Zap receipt"], ["Auto-pay failed"], screen=both)
    await sc.step(demo_settled.STEPS[1], label="claimant wallet")


def has_failed(screen):
    return "auto-pay failed" in screen.lower()


SCENARIOS = {
    "A": ("submit lock while a create hangs", scenario_a),
    "B": ("502 on create: hint and refetch", scenario_b),
    "C": ("400 on create: no hint, no refetch", scenario_c),
    "D": ("duplicate open bounty refused locally", scenario_d),
    "E": ("401 re-login once", scenario_e),
    "F": ("profile scan 500, then Try again", scenario_f),
    "G": ("nsec leaves the clipboard after 30 s", scenario_g),
    "H": ("two-click remove arms and disarms", scenario_h),
    "P": ("paid: real bounty, claim, settle", scenario_p),
}
