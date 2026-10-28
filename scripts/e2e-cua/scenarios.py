"""The scenarios. Each is `async def x(sc)`; register it in SCENARIOS.

sc.box.launch(...)       relaunch the app on a fresh MC_STORE_DIR
sc.box.start_fake(mode)  fresh fake instance + relay in the given mode
sc.step(Step, must_act)  one agent.py step: Jev picks a candidate, judges `done`
sc.check / check_screen  hard checks; every scenario needs at least one
sc.log_after(marker)     fake-server log lines after sc.box.mark(marker)

Candidate actions are complete; none of them may create a wallet, remove an
account for real, or publish anywhere but the fake server (P and R excepted).
"""
import asyncio, os, time
from agent import Step, Cand
from suite import accent_pills, fine_lines, has, norm, ocr_phrase_all, ocr_phrase_pos, shot_bytes

FOCUS = ("focus", "Magic Carpet")
PARK = ("shell", "xdotool mousemove 20 740")   # hover tooltips hide the header
# Rail geometry at 1280x960 (window at 0,0): the avatars sit 55 px apart.
# Rail avatar clicks go through the "avatar" action: the driver maps
# a name to a window-local point, which differs per target (the
# box coordinates date to an older rail layout than the omac one).

BOUNTIES = Cand("open-bounties", "Press super+2 to open the Bounties screen.", [FOCUS, ("key", "super+2")])
DASHBOARD = Cand("open-dashboard", "Press super+1 to open the Dashboard.", [FOCUS, ("key", "super+1")])
WALLET = Cand("open-wallet", "Press super+6 to open the Wallet screen of the selected account.", [FOCUS, ("key", "super+6")])
NEW_FORM = Cand("new-bounty-form", "Press super+n to open the 'New DList + bounty' form.", [FOCUS, ("key", "super+n")])
HOUSE = Cand("click-house-avatar", "Click the first avatar in the left rail to select the house issuer, then press super+2 to return to Bounties.",
             [FOCUS, ("avatar", "house"), ("wait", 2), PARK, ("key", "super+2"), ("wait", 1)])
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
                  ("key", "super+bracketright"), ("wait", 0.5), ("snap", "after super+]"),
                  ("avatar", "plus"), ("wait", 0.5), PARK, ("snap", "after plus"),
                  ("click", REMOVE_LINK_XY), ("wait", 0.5), ("click", REMOVE_LINK_XY), ("wait", 0.5), PARK,
                  ("snap", "after remove x2"),
                  ("key", "Escape"), ("stamp", "attempts done"), ("wait", 0.5)])],
        "The bounty form is still open and still reads 'Publishing…'; the header button still reads 'New DList + bounty' (the house is still selected); no account setup screen.",
        tries=1, settle=0.5, cues=("Publishing", "New DList + bounty", "Submit a bounty claim", "Your Nostr account")),
        must_act=True, label="submit and in-flight attempts", second_look=False)   # a later look would be past the timeout
    t_submit = sc.stamps.get("submitted", time.time())
    done_at = sc.stamps.get("attempts done", time.time())
    sc.check("the attempts ran inside the client timeout", done_at - t_submit < 18, f"{done_at - t_submit:.1f} s after submit")
    if sc.check_screen("switch, setup, remove and esc all refused",
                       ["Publishing", "Cancel (esc)", "New DList + bounty"], ["Submit a bounty claim", "Your Nostr account"]):
        sc.accept_step("submit and in-flight attempts", "Publishing, Cancel (esc) and New DList + bounty on screen after the attempts")
    # The client timeout is 20 s; give it 26 from the submit.
    await asyncio.sleep(max(0, t_submit + 26 - time.time()))
    # After the timeout the form reads "Checking the list…" until the
    # refetch lands (7b1dad9), then unlocks with the hint.
    await sc.step(Step("Wait for the create request to time out and the list check after it.", [Cand("wait", "Wait 5 seconds.", [("wait", 5)])],
                       "The bounty form is still open, 'Publishing…' is gone, and an error line says the bounty may exist already.",
                       tries=3, settle=1, cues=("Publishing", "Checking the list", "may exist already")), label="timeout unlocks")
    await sc.wait_for(["may exist already"], timeout=20, label="list check done")
    sc.check_screen("form unlocked after the client timeout and the list check", ["Cancel (esc)", "may exist already"],
                    ["Publishing", "Checking the list"])
    await sc.step(Step("Open the Dashboard to read the Recent activity feed.", [DASHBOARD],
                       "The Recent activity panel lists 'Still sending. Wait for it to finish.' at least once.",
                       settle=2, cues=("Still sending", "Recent activity")), label="activity feed")
    n = sc.count("Still sending")
    sc.check("amber 'Still sending' line for switch, setup and remove (3x)", n >= 3, f"seen {n}x")
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
    await submit_form(sc, "gateway-city", "The form shows an error saying the bounty may exist already and to check the list before retrying.",
                      ("may exist already", "Check the bounty list"))
    # "Checking the list…" first, then the hint once the refetch lands.
    await sc.wait_for(["may exist already"], timeout=20, label="after 502")
    sc.check_screen("'may exist already' hint shown, form unlocked", ["may exist already"], ["Checking the list", "Publishing"])
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
    await submit_form(sc, "refused-city", "The form shows an error about amountSats and no 'may exist already' hint.", ("amountSats", "may exist already"))
    await asyncio.sleep(5)
    await sc.see("after 400")
    sc.check_screen("400 shows the refusal, no hint", ["amountSats"], ["may exist already"])
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
    # Driven directly: the dup mode already lists dup-city, so the judge half
    # believes a fill step is done and its pick comes back weak.
    for kind, arg in fill("dup-city").do:
        await sc.suite.driver.act(kind, arg)
    await sc.step(Step("Wait for the form's answer to the duplicate.", [Cand("wait", "Wait 2 seconds.", [("wait", 2)])],
                       "The form shows 'This list already has an open bounty. Pick it from the list instead.'",
                       tries=2, settle=1, cues=("already has an open bounty", "Publishing")), label="dup answer")
    await asyncio.sleep(2)
    await sc.see("after dup submit")
    sc.check_screen("duplicate refused in the form", ["already has an open bounty"])
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
                       tries=2, settle=4, cues=("read this profile", "Create a Coinos wallet", "Checking your profile")), must_act=True, label="try again")
    sc.check_screen("retry reaches the Read state", ["Create a Coinos wallet"], ["read this profile"])
    ok = [l for l in sc.log_after("F retry") if l.startswith("GET /api/strfry/scan") and l.endswith("200 []")]
    sc.check("the retry re-read the profile scan", bool(ok), sc.log_after("F retry")[-4:])


# ---------------------------------------------------------------- G
BOUNTIES_XY = (150, 250)  # the "Bounties" sidebar row (window at 0,0, 1280x960)
CHIP_XY = (135, 116)  # the account name at the top of the sidebar
REMOVE_LINK_XY = (143, 155)  # "Remove this account" under the chip (window at 0,0, 1280x960); fixed, so no OCR pass
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
        screen = await sc.wait_for(["still holds"], ["Couldn't read the balance"], timeout=25, every=2, label=f"{who} balance")
        sc.check(f"{who} (Coinos wallet): armed remove warns about sats left", has(screen, "still holds"),
                 "balance not readable" if has(screen, "Couldn't read the balance") else "no 'This wallet still holds N sats' line while armed")
    await sc.step(Step("Navigate away: press super+1 for the Dashboard.", [DASHBOARD],
                       "The Dashboard is shown and the sidebar reads 'Remove this account' again, not 'Click again to remove'.",
                       settle=2, cues=("Click again to remove", "Remove this account", "Dashboard")), must_act=True, label=f"{who} leave (sidebar)")
    sc.check_screen(f"{who}: navigating away disarms (sidebar)", ["Remove this account"], ["Click again to remove"])
    if not await sc.step(IDENTITY, label=f"{who} identity tab"):
        return
    await sc.step(Step(f"Arm the 'Remove…' button on the {who}'s Identity card ('Remove this account' card at the bottom), once.",
                       [Cand("arm-card", "Click the 'Remove…' button on the right of the 'Remove this account' card, once.",
                             # The npub QR card pushes this card below the fold.
                             [FOCUS, ("scroll", "down"), ("wait", 3),
                              ("click_text", {"text": "Remove", "near": "Deletes the nsec"}), ("wait", 1), PARK])],
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
    # Direct: the avatar click is idempotent, and the judge halted here on a
    # weak pick (p 0.44-0.50, abstain ~0.4) in two runs. The literal header
    # check below decides.
    for kind, arg in [FOCUS, ("avatar", "claimant"), ("wait", 2), PARK, ("key", "super+2"), ("wait", 1)]:
        await sc.suite.driver.act(kind, arg)
    await sc.wait_for(["Submit a bounty claim"], timeout=30, every=3, label="claimant")
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
    screen = await sc.wait_for(["Payment settled", "Payment receipt"], ["Auto-pay failed"], timeout=150, every=10, label="settle poll")
    if has_failed(screen):
        sc.notes.append("Auto-pay failed on the claim card; not retried, not reset")
    await sc.step(demo_settled.STEPS[0], label="settled after refresh")
    both = sc.screen + "\n" + screen
    sc.check_screen("Payment settled and Payment receipt on the claim card", ["Payment settled", "Payment receipt"], ["Auto-pay failed"], screen=both)
    await sc.step(demo_settled.STEPS[1], label="claimant wallet")


def has_failed(screen):
    return "auto-pay failed" in screen.lower()


# ---------------------------------------------------------------- R (paid)
# R drives the app through its accessibility tree (atspi.py, box only): each
# step's candidates press controls and focus fields by their stable ids
# (docs/a11y-ids.md), and the waits are for the ids that mark the next
# state. Jev still picks the candidate and judges every postcondition
# from the screenshot, so a press that reaches the wrong control fails the
# step. Labels carry no text over AT-SPI, so the wallet, auto-pay and payment
# wording is still read from the screen.
RAIL_ACCOUNT = "~rail-account-[0-9a-f]{64}"
BOUNTY_ROW = "~bounty-row-[0-9a-f-]{36}"
CLAIM_CARD = "~claim-[0-9a-f]{64}"
# Switching accounts lands on the Dashboard; the sidebar's Bounties tab is
# the way back, and bounties-new carries the issuer/claimant wording.
TO_BOUNTIES = [("wait_id", "nav-bounties"), ("press", "nav-bounties"), PARK]


def a11y_name(sc, target, name=None):
    """The control's AT-SPI name, or None unless exactly one node matches."""
    try:
        return sc.suite.driver.a11y("name", target, name=name)["name"]
    except RuntimeError as e:
        sc.notes.append(f"a11y: {e}"[:300])
        return None


def a11y_count(sc, target, name=None):
    try:
        return sc.suite.driver.a11y("exists", target, name=name)["count"]
    except RuntimeError as e:
        sc.notes.append(f"a11y: {e}"[:300])
        return -1


def create_account(who, stamp):
    """Steps that add one app-made account (and its Coinos wallet)."""
    name = f"E2E Newcomer {who} {stamp}"
    return [
        Step(f"Open account setup from the '+' in the left rail and choose 'Create a new account' for newcomer {who}.",
             [Cand("press-add-then-create", "Press the rail's 'Add account' (+) button, then the 'Create a new account' button.",
                   [("press", "rail-add-account"), ("wait_id", "setup-create"), ("press", "setup-create"),
                    ("wait_id", "setup-name"), PARK])],
             "The 'Create a new account' card is open with a 'Name (optional)' input and a 'Create — Enter' button.",
             tries=1, settle=1, cues=("Name (optional)", "Create — Enter", "Create a new account", "I already have a key")),
        # tries=1: a second Create is a second account and wallet.
        Step(f"Type the name '{name}' and create the account.",
             # Enter, not the button: it keeps the focus in the name input, where the
             # later Enter that leaves setup has to land.
             [Cand("name-then-enter", "Click into the Name input, type the name, then press Enter.",
                   [FOCUS, ("click_id", "setup-name"), ("type", name), ("wait", 0.4), ("key", "Return"), ("wait", 1)])],
             "The account is being created, or the ready panel shows 'Coinos wallet ready'.",
             tries=1, settle=10, cues=("Coinos wallet ready", "Creating", "Create — Enter", "Couldn't")),
    ]


async def newcomer(sc, who, stamp):
    steps = create_account(who, stamp)
    if not await sc.step(steps[0], must_act=True, label=f"newcomer {who} setup"):
        return False
    if not await sc.step(steps[1], must_act=True, label=f"newcomer {who} create"):
        return False
    # Live coinos.io plus a kind-0 publish: allow a minute. "Copy password"
    # exists only after a Coinos signup in this session; the wording itself
    # is a Label, so the screen confirms it.
    await sc.suite.driver.act("wait_id", {"id": "wallet-copy-password", "secs": 60, "soft": True})
    screen = await sc.wait_for(["Coinos wallet ready"], ["failed"], timeout=15, every=4, label=f"newcomer {who} wallet")
    if not sc.check_screen(f"newcomer {who}: Coinos wallet ready", ["Coinos wallet ready"], ["failed"], screen=screen):
        return False
    # Driven directly: the judge kept re-observing instead of pressing Enter.
    # The ready panel's Dashboard button is below the fold at 1280x960 (and
    # gpui cannot press it there); Enter in the setup input leaves setup.
    for kind, arg in (FOCUS, ("key", "Return"), ("wait_id", {"id": "dash-heading", "soft": True})):
        await sc.suite.driver.act(kind, arg)
    label = f"newcomer {who} dashboard"
    met = await sc.step(Step("Confirm the Dashboard is shown.", [Cand("wait", "Wait 2 seconds.", [("wait", 2)])],
                             "The Dashboard is shown with a 'Recent activity' panel and the sidebar.",
                             tries=2, settle=1, cues=("Recent activity", "Dashboard", "Coinos wallet ready")), label=label)
    # The Dashboard also says "Coinos wallet ready: ..." in Needs attention;
    # only the ready panel has the Copy password button.
    if not sc.check_screen(f"newcomer {who}: back on the Dashboard", ["Recent activity"], ["Copy password"]):
        return False
    if not met:
        sc.accept_step(label, "Recent activity on screen, ready panel gone")
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
    # R drives by accessibility id: no bus, no R (before any account exists).
    if not sc.check("accessibility bus up (python3-gi, at-spi, IsEnabled)", sc.box.a11y_up()):
        return
    # Only the house starts in the store; the newcomers are made in the app.
    store = sc.box.launch("R", sc.suite.run_id, target="instance", seed=False, nsecs=sc.box.keys["house"]["sec"], a11y=True)
    try:
        if not await a11y_preflight(sc):
            return
        await newcomers_flow(sc, name, stamp, store)
    finally:
        meta["float_after_r"] = sc.box.float_sats()
        spent = (meta["float_before_r"] or 0) - (meta["float_after_r"] or 0)
        sc.check("float dropped by two payouts, not more", 150 < spent <= 300, f"{meta['float_before_r']} -> {meta['float_after_r']} sats")


async def a11y_preflight(sc):
    """Before any newcomer is made: xwininfo is there, the app's window is
    measurable, and its tree is on the bus (the rail's + is in it)."""
    d = sc.suite.driver
    from suite import dx
    ok = sc.check("xwininfo in mc", dx("command -v xwininfo").returncode == 0)
    try:
        box = await d.window_box()
        ok = sc.check("app window measured", True, str(box)) and ok
    except RuntimeError as e:
        ok = sc.check("app window measured", False, str(e)) and ok
    try:
        await d.act("wait_id", {"id": "rail-add-account", "secs": 20})
        ok = sc.check("app tree on the accessibility bus (rail-add-account)", True) and ok
    except RuntimeError as e:
        ok = sc.check("app tree on the accessibility bus (rail-add-account)", False, str(e)) and ok
    return ok


def fill_by_id(name, cap):
    """The seven bounty fields, each focused by its id: reward 100, `cap`,
    min rank 2. Enter only in the last field, where it submits (Enter in
    the others just moves on)."""
    words = name.replace("-", " ")
    do = [FOCUS]
    for fid, text in (("item-singular", name), ("item-plural", name + "s"),
                      ("description", f"{words}s for the e2e suite"), ("criteria", f"A {words} name, one per claim"),
                      ("amount", "100"), ("cap", cap), ("min-rank", "2")):
        do += [("click_id", f"bounty-form-{fid}"), ("key", "ctrl+a"), ("type", text), ("wait", 0.3)]
    do += [("key", "Return"), ("stamp", "submitted"),
           ("wait_id", {"id": "bounty-detail-title", "name": name, "secs": 45, "soft": True})]
    return Cand("fill-and-publish",
                f"Click each of the seven fields and type it: singular '{name}', plural, description, criteria, reward 100, cap {cap}, min rank 2; Enter in the last field publishes.",
                do)


async def newcomers_flow(sc, name, stamp, store):
    if not await newcomer(sc, "A", stamp):
        return
    if not await newcomer(sc, "B", stamp):
        return
    sc.check("house plus two app-made accounts with Coinos wallets",
             sc.box.accounts_in_store(store) == 3 and sc.box.wallet_accounts_in_store(store) == 2,
             f"{sc.box.accounts_in_store(store)} accounts, {sc.box.wallet_accounts_in_store(store)} app-made with a wallet")
    # The house issues one bounty that pays two claims, then closes.
    house_tab = Cand("press-house-tab", "Press the house issuer's account tab in the left rail, then the sidebar's Bounties tab.",
                     [("press", f"rail-account-{sc.box.keys['house']['pub']}")] + TO_BOUNTIES
                     + [("wait_id", {"id": "bounties-new", "has": ["New DList + bounty"]})])
    if not await sc.step(Step("Show the Bounties screen with the house issuer selected (its header button reads 'New DList + bounty').",
                              [house_tab], "The Bounties screen is shown and its header button reads 'New DList + bounty'.",
                              settle=1, cues=HOUSE_CUES), must_act=True, label="house on bounties"):
        return
    new_name = a11y_name(sc, "bounties-new") or ""
    ok = sc.check_screen("house selected", ["New DList + bounty"], ["Submit a bounty claim"])
    if not (sc.check("house selected (bounties-new)", "New DList + bounty" in new_name, repr(new_name)) and ok):
        return
    if not await sc.step(Step("Open the new bounty form as the house issuer.",
                              [Cand("press-new-bounty", "Press the '+ New DList + bounty' button.",
                                    [("press", "bounties-new"), ("wait_id", "bounty-form-item-singular"), PARK])],
                              "A form titled 'New DList + bounty' is open.", settle=1, cues=("New DList", "Cancel")),
                         label="open form"):
        return
    # Gate: the last Enter publishes; in a claim form it would claim and pay.
    form = a11y_name(sc, "bounty-form")
    ok = sc.check_screen("the house's New DList + bounty form is open", ["New DList + bounty", "Cancel (esc)"],
                         ["Claim an item on", "Submit a bounty claim"])
    ok = sc.check("bounty-form is the house's new list, no claim-form",
                  form == "New DList + bounty" and a11y_count(sc, "claim-form") == 0, f"bounty-form {form!r}") and ok
    if not ok:
        await sc.suite.driver.act("key", "Escape")
        return
    # cap 200 = two payouts of 100; tries=1: a retried Enter is a second bounty.
    publish = Step("Fill the bounty form and publish it.",
                   [fill_by_id(name, "200"), Cand("cancel-form", "Press the form's 'Cancel (esc)' button.", [("press", "bounty-form-cancel")])],
                   "The new bounty is listed and auto-pay is armed.", tries=1, settle=1,
                   cues=(name, "Auto-pay armed", "watching for claims"))
    if not await sc.step(publish, must_act=True, label="publish bounty"):
        return
    # top-auto-pay is in the tree while a bounty is focused; its wording is a Label.
    await sc.suite.driver.act("wait_id", {"id": "top-auto-pay", "secs": 20, "soft": True})
    await sc.wait_for(["Auto-pay armed"], timeout=20, every=3, label="auto-pay armed")
    if not sc.check_screen("auto-pay armed on the new bounty", ["Auto-pay armed"]):
        return
    for who in ("A", "B"):
        item = f"E2E Newcomer {who} {stamp}"
        select = Step(f"Select newcomer {who} in the left rail and show Bounties (the header button reads 'Submit a bounty claim').",
                      [Cand(f"press-newcomer-{who}-tab", f"Press newcomer {who}'s account tab in the left rail, then the sidebar's Bounties tab.",
                            [("press", {"id": RAIL_ACCOUNT, "name": item})] + TO_BOUNTIES
                            + [("wait_id", {"id": "bounties-new", "has": ["Submit a bounty claim"]})])],
                      "The Bounties header button reads 'Submit a bounty claim'.", settle=1,
                      cues=("Submit a bounty claim", "New DList + bounty"))
        if not await sc.step(select, must_act=True, label=f"select newcomer {who}"):
            return
        # Gate on the account: the sidebar chip's name, from the tree and from
        # the sidebar OCR pass (the main pane also lists A's claim on B's turn).
        other = "B" if who == "A" else "A"
        sidebar = sc.screen.split("\n[sidebar]\n")[-1] if "\n[sidebar]\n" in sc.screen else ""
        chip = a11y_name(sc, "sidebar-account")
        ok = sc.check_screen(f"newcomer {who} selected (header)", ["Submit a bounty claim"], ["New DList + bounty"])
        ok = sc.check_screen(f"newcomer {who} selected (sidebar name)", [item], [f"E2E Newcomer {other} {stamp}"], screen=sidebar) and ok
        ok = sc.check(f"newcomer {who} selected (sidebar-account)", chip == item, repr(chip)) and ok
        if not ok:
            await sc.suite.driver.act("key", "Escape")
            return
        # Baseline from the Bounties pane, before the claim form can hide A's card.
        n_settled = sc.count("Payment settled")
        open_form = Step("Open the claim form for this run's bounty.",
                         # The list runs oldest first, so this run's row can sit
                         # below the fold, where it cannot be pressed; the bounty
                         # stays selected from its creation.
                         [Cand("claim-on-selected", f"'{name}' is already open in the detail pane: press the '+ Submit a bounty claim' button.",
                               [("wait_id", {"id": "bounty-detail-title", "name": name, "secs": 3}),
                                ("press", "bounties-new"), ("wait_id", "claim-form-item-name"), PARK]),
                          Cand("press-row-then-claim", f"Press the '{name}' row in the bounty list, then the '+ Submit a bounty claim' button.",
                               [("press", {"id": BOUNTY_ROW, "name": name}), ("wait_id", {"id": "bounty-detail-title", "name": name}),
                                ("press", "bounties-new"), ("wait_id", "claim-form-item-name"), PARK])],
                         "A claim form is open (a 'Cancel' control is visible and the 'Submit a bounty claim' button is gone).",
                         settle=1, cues=("Cancel", "Submit a bounty claim"))
        if not await sc.step(open_form, label=f"open claim form {who}"):
            return
        # Gate: the claim form must be on this run's bounty, or we'd pay another one.
        form = a11y_name(sc, "claim-form") or ""
        ok = sc.check_screen(f"newcomer {who}: claim form is on this run's bounty", [f"Claim an item on {name}"])
        # The exact label (src/bounties.rs claim form): this run's list and
        # its 100-sat reward, proven before the paying Enter.
        want = f"Claim an item on {name} · 100 sats per item"
        ok = sc.check(f"newcomer {who}: claim-form names this run's list and 100 sats", form == want, repr(form)) and ok
        if not ok:
            await sc.suite.driver.act("key", "Escape")
            sc.notes.append(f"claim form for {who} was not on this run's bounty; cancelled, nothing claimed")
            return
        # tries=1: a second submit is a second payout. No cancel candidate:
        # the gate has passed, and a cancel here strands an open auto-pay bounty.
        submit = Step("Submit the claim.",
                      [Cand("type-item-and-submit", f"Click into the Item name input, type '{item}' and press Enter to publish the claim.",
                            [FOCUS, ("click_id", "claim-form-item-name"), ("type", item), ("wait", 0.3), ("key", "Return"),
                             ("wait_id", {"id": CLAIM_CARD, "name": item, "secs": 20, "soft": True})])],
                      "The claim card shows 'Claim submitted'.", tries=1, settle=1, cues=("Claim submitted", item))
        if not await sc.step(submit, must_act=True, label=f"submit claim {who}"):
            return
        # This claim's card, by its item name; its receipt row appears once
        # the zap receipt is in (the payment wording is a Label: the screen).
        card = None
        try:
            card = sc.suite.driver.a11y("name", CLAIM_CARD, name=item)["id"]
        except RuntimeError as e:
            sc.notes.append(f"a11y: newcomer {who}'s claim card: {e}"[:300])
        wait, tries = Cand("wait-for-settlement", "Wait 20 seconds for the auto-pay watcher to pay and the zap receipt to arrive.", [("wait", 20)]), 6
        if card:
            wait, tries = Cand("wait-for-receipt", "Wait up to 150 seconds for this claim's card to show its payment receipt.",
                               [("wait_id", {"id": f"{card}-receipt", "secs": 150, "soft": True})]), 2
        await sc.step(Step("Wait for the payment to settle.", [wait],
                           "The claim card shows 'Payment settled' or 'Zap receipt'.", settle=1, tries=tries,
                           cues=("Payment settled", "Zap receipt", "Auto-pay failed")), label=f"wait for settlement {who}")
        screen = sc.screen
        if not (card and a11y_count(sc, f"{card}-receipt") == 1):
            # The receipt row did not confirm: the old screen poll.
            screen = await sc.wait_for(["Payment settled", "Payment receipt"], ["Auto-pay failed"], timeout=150, every=10, label=f"settle poll {who}")
        if has_failed(screen) or has_failed(sc.screen):
            sc.notes.append(f"Auto-pay failed on newcomer {who}'s claim card; not retried, not reset")
        await sc.step(Step("Show the bounty's claim card with its latest payment state.",
                           [Cand("refresh-bounties", "Press the sidebar's Bounties tab, then super+r to refetch.",
                                 [("press", "nav-bounties"), FOCUS, ("key", "super+r")])],
                           "The claim card shows 'Payment settled' or 'Zap receipt'.", settle=4,
                           cues=("Payment settled", "Zap receipt")), label=f"settled after refresh {who}")
        both = sc.screen + "\n" + screen
        sc.check_screen(f"newcomer {who}: claim item on screen, no Auto-pay failed", [item, "Payment receipt"], ["Auto-pay failed"], screen=both)
        n = max(sc.count("Payment settled", sc.screen), sc.count("Payment settled", screen))
        sc.check(f"newcomer {who}: a new Payment settled card ({n} now, {n_settled} before)", n >= n_settled + 1)
        if card:
            sc.check(f"newcomer {who}: this claim's card has its receipt row", a11y_count(sc, f"{card}-receipt") == 1, card[:16])


# ------------------------------------------------------------ L..Q: trust
# Feature 2: "Your Trusted Community". Every scenario runs its own throwaway
# account key and its own throwaway owner (well-known account) — the house and
# claimant keys never appear here. The app talks Brainstorm to the fake on
# :8788 and trust reads/publishes to the local nak relay on :10547.
import json

LOCAL_RELAY = "ws://127.0.0.1:10547"


def fresh_key(box):
    """A throwaway keypair via nak: hex sec, hex pub, npub."""
    def nak(args, what, sec=None):
        rc, out = box.nak(args, sec=sec)
        if rc != 0 or not out.strip():
            raise RuntimeError(f"nak failed: {what}")
        return out.strip()
    sec = nak(["key", "generate"], "key generate")
    pub = nak(["key", "public", "$NAK_SEC"], "key public", sec=sec)
    npub = nak(["encode", "npub", pub], "encode npub")
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


# The sidebar chip opens the active account's page on its Trust tab
# (shell.rs, `open_account(.., Tab::Trust)`), so the chip click is the whole
# action. A second OCR click on the tab used to follow it and raised whenever
# the 1.5 s wait ended before the software-rendered page had repainted.
TRUST_STEPS = ("Good news: you can do the three needed steps right here",
               "Follow at least one well-known account",
               "Ask Brainstorm to calculate your community's trust scores",
               "Publish your Treasure Map")
TRUST_TAB = Step(
    "Open the active account's page on its 'Trust' tab.",
    [Cand("chip-opens-trust", "Click the account name at the top of the left sidebar; it opens the account page on its 'Trust' tab.",
          [FOCUS, ("click", CHIP_XY), ("wait", 2), PARK]),
     Cand("trust-tab", "The account page is already open on another tab: click its 'Trust' tab, beside 'Identity & keys'.",
          [FOCUS, ("click_text", {"text": "Trust", "near": "Identity & keys"}), ("wait", 1), PARK])],
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
    # The literal tab strip and steps decide when the judge does not.
    if sc.check_screen("the Trust tab's steps are on screen",
                       ["Identity & keys", "Calculate my trust scores", "Publish my Treasure Map"] + list(TRUST_STEPS[1:])):
        sc.accept_step("trust tab", "the tab strip, the three steps and their buttons on screen")
        return True
    return False


async def follow_owner(sc, expect_confirm):
    """Click Follow on the recommended owner row; if the first-list amber box
    shows, approve it. Returns True once '✓ Following' / 'Saved on' shows."""
    # An account with a kind-3 reads "You already follow N account. Go to
    # step 2." over the row; without the last sentence the judge's abstain
    # mass reached 0.32 there and the step halted before any click (W).
    ok = await sc.step(Step(
        "Click 'Follow' on the recommended row ('RECOMMENDED — YOUR OWNER NPUB') in step 1 of the Trust tab. "
        "The test needs the owner in the follow list, so click it even when step 1 says 'You already follow N account'.",
        [Cand("follow-owner", "Click the 'Follow' button on the recommended owner row.",
              [FOCUS, ("click_pill", "RECOMMENDED"), ("wait", 1), PARK])],
        "After the click, either a confirmation box offers the buttons 'Start a new follow list' and 'Cancel', or the row already reads '✓ Following' or 'Saved on'.",
        tries=4, settle=6,
        cues=("Start a new follow list", "Following", "Saved on", "Couldn't check", "RECOMMENDED")),
        must_act=True, label="follow owner")
    if not ok:
        return False
    if has(sc.screen, "Start a new follow list"):
        if not expect_confirm:
            sc.notes.append("unexpected first-list confirmation for an account that has a kind-3")
            return False
        return await sc.step(Step(
            "Approve the first follow list: click 'Start a new follow list' in the confirmation box.",
            [Cand("confirm-first-list", "Click the 'Start a new follow list' button beside 'Cancel' in the confirmation box.",
                  [FOCUS, ("click_pill", CONFIRM_ANCHOR), ("wait", 1), PARK])],
            "The recommended row reads '✓ Following' and a 'Saved on N of M Nostr servers' line is shown.",
            tries=4, settle=8, cues=("Following", "Saved on", "Start a new follow list")),
            must_act=True, label="confirm first list")
    return True


async def calculate(sc, done_cond="Step 2 shows 'Brainstorm confirmed — your community's trust scores are in the process of being calculated'.",
                    label="calculate", tries=4):
    # tries=1 for error paths: every click is one graperank request (the real
    # API rate-limits to 3/30 min), so a retried click falsifies the test.
    return await sc.step(Step(
        "Click 'Calculate my trust scores' in step 2.",
        [Cand("calculate", "Click the 'Calculate my trust scores' button.",
              [FOCUS, ("click_pill", "Ask Brainstorm to calculate"), ("wait", 1), PARK])],
        done_cond, tries=tries, settle=10,
        cues=("Brainstorm confirmed", "Asking the Brainstorm", "Calculate my trust scores",
              "Please wait", "temporarily unavailable")),
        must_act=True, label=label)


async def publish_map(sc):
    ok = await sc.step(Step(
        "Click 'Publish my Treasure Map' in step 3.",
        [Cand("publish-map", "Click the 'Publish my Treasure Map' button.",
              # Step 3 sits below the fold under the Recalculate row.
              [FOCUS, ("scroll_until", {"text": "Publish your Treasure Map", "reach": 90}), PARK,
               ("click_pill", "Publish your Treasure Map"), ("wait", 1), PARK])],
        "The publish started: 'Fetching tag data' shows, or the card already reads 'Located'.",
        tries=3, settle=6,
        cues=("Fetching tag data", "Located", "Publish my Treasure Map")),
        must_act=True, label="publish map")
    if not ok:
        return False
    screen = await sc.wait_for(["Located"], timeout=45, every=4, label="map lands")
    return has(screen, "Located")


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


# ---------------------------------------------------------------- L
async def scenario_l(sc):
    """Fresh account, no kind-3: confirm the first list, follow the owner,
    calculate, publish the map."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    sc.box.launch("L", sc.suite.run_id, nsecs=acct["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"]})
    if not await open_trust(sc):
        return
    if not await follow_owner(sc, expect_confirm=True):
        return
    sc.check_screen("owner followed: '✓ Following' and a 'Saved on' line", ["Following", "Saved on"])
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
    await sc.see("located")
    sc.check_screen("the Trust card reads 'Located'", ["Located"])


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
    sc.box.launch("M", sc.suite.run_id, nsecs=acct["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"]})
    if not await open_trust(sc):
        return
    if not await follow_owner(sc, expect_confirm=False):
        return
    sc.check_screen("owner followed without a confirmation", ["Following"], ["Start a new follow list"])
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
    sc.box.launch("N", sc.suite.run_id, nsecs=acct["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"],
                             "MC_TRUST_RELAYS": f"{LOCAL_RELAY}, ws://127.0.0.1:10599"})
    if not await open_trust(sc):
        return
    await sc.step(Step(
        "Click 'Follow' on the recommended row; the relay answer must come back inconclusive.",
        [Cand("follow-owner", "Click the 'Follow' button on the recommended owner row.",
              [FOCUS, ("click_pill", "RECOMMENDED"), ("wait", 1), PARK])],
        "An error line 'Couldn't check your follow list' is shown.",
        tries=4, settle=8,
        cues=("Couldn't check your follow list", "Start a new follow list", "Following")),
        must_act=True, label="follow inconclusive")
    screen = await sc.wait_for(["Couldn't check your follow list"],
                               ["Following"], timeout=30, every=3, label="inconclusive")
    sc.check_screen("the inconclusive error line shows", ["Couldn't check your follow list"],
                    ["Following"], screen=screen)
    await asyncio.sleep(2)
    sc.check("no kind-3 was published to the local relay", not nak_req(sc.box, 3, acct["pub"]))


# ---------------------------------------------------------------- O
async def scenario_o(sc):
    """graperank 429, then 403, then 503: each click shows the right wording
    and can be retried; followList is posted at most once."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({"graperank": ["429", "403", "503"]})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    nak_publish(sc.box, acct["sec"], 3, "", [f"p={owner['pub']}"])
    await asyncio.sleep(1)
    sc.box.launch("O", sc.suite.run_id, nsecs=acct["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"]})
    if not await open_trust(sc):
        return
    # The account already follows the owner, so step 2 is enabled at once.
    # The Noul postcondition judges the state generically (an error line and
    # the button back); the exact wording per status is the hard check after
    # each click — a second click would spend the next scripted answer.
    sc.box.mark("O 429")
    await calculate(sc, done_cond="Step 2 shows the error 'Please wait a few minutes "
                    "before recalculating' and the 'Calculate my trust scores' button.",
                    label="calc 429", tries=1)
    screen = await sc.wait_for(["Please wait a few minutes before recalculating"],
                               ["Brainstorm confirmed"], timeout=25, every=3, label="429 wording")
    sc.check_screen("429 wording", ["Please wait a few minutes before recalculating"], screen=screen)
    # Presses 2 and 3 are direct: the 429 text is still on screen, so Jev saw
    # nothing to do and abstained. Each waits for its own '-> status' log line,
    # then lets the app repaint before the wording is read.
    for marker, status, wording in (("O 403", "403", "Please wait a few minutes before recalculating"),
                                    ("O 503", "503", "The Brainstorm server is temporarily unavailable")):
        sc.box.mark(marker)
        await sc.suite.driver.act(*FOCUS)
        await sc.suite.driver.act("click_pill", "Ask Brainstorm to calculate")
        await sc.suite.driver.act(*PARK)
        end = time.time() + 25
        while time.time() < end and not any(
                l.startswith("BS POST /user/graperank") and l.endswith(f"-> {status}")
                for l in sc.log_after(marker)):
            await asyncio.sleep(1)
        sc.check(f"{status} request reached the fake", any(
            l.startswith("BS POST /user/graperank") and l.endswith(f"-> {status}")
            for l in sc.log_after(marker)), sc.log_after(marker))
        await asyncio.sleep(4)   # the error line from the previous press is replaced by now
        screen = await sc.wait_for([wording], ["Brainstorm confirmed"], timeout=25, every=3, label=f"{status} wording")
        sc.check_screen(f"{status} wording", [wording], ["Brainstorm confirmed"], screen=screen)
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
    sc.box.launch("Q", sc.suite.run_id, nsecs=acct["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"]})
    if not await open_trust(sc):
        return
    if not await calculate(sc):
        return
    # The line renders under step 3, below the 960 px window: scroll to it.
    await sc.suite.driver.act("scroll", "down")
    await sc.suite.driver.act(*PARK)
    screen = await sc.wait_for(["Brainstorm has no data for this account"], timeout=30, every=3,
                               label="setup missing")
    sc.check_screen("Brainstorm has no data wording", ["Brainstorm has no data for this account"],
                    screen=screen)
    sc.box.mark("Q publish attempt")
    # The button renders disabled: a click must send nothing anywhere.
    try:
        await sc.suite.driver.act("click_pill", "Publish your Treasure Map")
    except Exception as e:
        sc.notes.append(f"publish click found no button ({e})")
    await asyncio.sleep(4)
    sc.check("no kind-10040 on the local relay", not nak_req(sc.box, 10040, acct["pub"]))
    lines = sc.log_after("Q publish attempt")
    posts = [l for l in lines if l.startswith("BS POST")]
    sc.check("the disabled publish sent no Brainstorm request", not posts, posts)


# ---------------------------------------------------------------- S
async def scenario_s(sc):
    coord = "39998:%s:post-city" % sc.suite.keys["house"]["pub"]
    sc.box.start_fake({"create": "ok", "dup_coordinate": coord})
    sc.box.launch("S", sc.suite.run_id)
    # The goal needs only the header button, as in H and R; the first bounty
    # is auto-selected, so its detail pane follows and the check below reads it.
    ok = await sc.step(Step("Show the Bounties screen with the house issuer selected (its header button reads 'New DList + bounty').",
                            [HOUSE, BOUNTIES], "The Bounties screen is shown and its header button reads 'New DList + bounty'.",
                            settle=2, cues=HOUSE_CUES + ("New bounty on this list",)), must_act=True, label="house on bounties")
    if not (ok and sc.check_screen("issuer sees the button", ["New bounty on this list"], ["Submit a bounty claim"])):
        return
    await sc.step(Step("Open the form from the detail pane's button.",
                       [Cand("click-new-on-list", "Click the 'New bounty on this list' button in the detail pane.",
                             [FOCUS, ("click_text", "New bounty on this list"), ("wait", 1), PARK])],
                       "A form titled 'New bounty on this list' is open with a 'Bounty criteria' field and no 'Item (singular)', 'Items (plural)' or 'List description' field.",
                       settle=2, cues=("New bounty on this list", "Item (singular)", "Bounty criteria", "Cancel")), label="open list form")
    # The issuer's header button always reads "+ New DList + bounty (⌘N)",
    # and "List" alone matches it, so check the fields themselves.
    sc.check_screen("existing-list form: criteria, no list fields", ["New bounty on this list", "Bounty criteria", "Cancel (esc)"],
                    ["Item (singular)", "Items (plural)", "List description"])
    sc.box.mark("S submit")
    for kind, arg in [("type", "A post city name, one per claim"), ("key", "Return"), ("wait", 0.4),
                      ("key", "ctrl+a"), ("type", "100"), ("key", "Return"), ("wait", 0.4),
                      ("key", "ctrl+a"), ("type", "200"), ("key", "Return"), ("wait", 0.4),
                      ("key", "ctrl+a"), ("type", "2"), ("key", "Return")]:
        await sc.suite.driver.act(kind, arg)
    # The box can lag the keystrokes by several seconds: a fixed sleep once
    # caught the untouched form before Enter landed. Wait for the second row.
    await sc.wait_for(["House bounties (2)"], timeout=30, label="after create on the existing list")
    lines = sc.log_after("S submit")
    posts = [l for l in lines if l.startswith("POST /api/bounties")]
    sc.check("exactly one create, on the existing list", len(posts) == 1 and posts[0].endswith("coord=post-city"), posts)
    pubs = [l for l in lines if l.startswith("POST strfry/publish")]
    sc.check("no list publish reached the server", not pubs, pubs)
    # The new bounty's detail pane shows "New bounty on this list" again, so
    # look for phrases only the form shows.
    sc.check_screen("form closed after the create", [], ["Cancel (esc)", "Bounty criteria"])
    await sc.step(Step("Select the next account (the claimant) and show the Bounties screen.", [NEXT_ACCOUNT],
                       "The header button reads 'Submit a bounty claim'.", settle=2, cues=HOUSE_CUES + ("New bounty on this list",)),
                  label="claimant selected")
    sc.check_screen("non-issuer has no button", ["Submit a bounty claim"], ["New bounty on this list"])


# ---------------------------------------------------------------- U
async def find_search_box(sc):
    """(x, y) of the empty follow-search box, or None. Found once, before
    anything is typed: the placeholder is the only text that marks it."""
    data, _ = await sc.snap("search box")
    box = ocr_phrase_pos(data, "Search profiles")
    sc.check("the search box is on screen", box is not None)
    return box


def type_into(box, text):
    """Click the box, select what it holds, type `text`: a retry replaces
    the earlier attempt's text instead of appending to it."""
    return [FOCUS, ("click", box), ("wait", 0.5), ("key", "ctrl+a"), ("type", text), ("wait", 2), PARK]


# The first-list box's first line: the amber 'Start a new follow list' pill is
# the nearest pill at or below it. 'cancel' is no anchor: it first shows in
# this box's sentence, nearer the pasted row's Follow pill than the amber one.
CONFIRM_ANCHOR = "No follow list was found"


def row_has_follow(data, pos):
    """An accent pill (the Follow button) on the row at `pos`, right of it.
    OCR cannot read the button's dark-on-violet label."""
    return any(abs(p[1] - pos[1]) < 14 and p[0] > pos[0] for p in accent_pills(data))


async def scenario_u(sc):
    """Fresh account, no kind-3: paste an npub the fake Brainstorm search does
    not know; the one row shows, Follow publishes a kind-3 with its p tag."""
    acct, owner, target = fresh_key(sc.box), fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    sc.box.launch("U", sc.suite.run_id, nsecs=acct["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"]})
    if not await open_trust(sc):
        return
    short = target["npub"][:12]
    # The box itself shows the pasted npub, so the row is only the match
    # below it: remember where the box is before typing into it.
    box = await find_search_box(sc)
    if box is None:
        return
    row_below = box[1] + 12
    met = await sc.step(Step(
        "Click the 'Search profiles to follow' box in step 1 and paste the npub.",
        [Cand("paste-npub", "Click the 'Search profiles to follow' input, then type the npub into it.",
              type_into(box, target["npub"]))],
        # The Follow label is dark on violet and never in the OCR: the
        # condition names only what the judge can read.
        f"The search box holds an npub, and below it one result row is labelled with the short npub "
        f"'{short}…' (the start of the same key).",
        tries=3, settle=4, cues=(short, "No profiles match", "Searching", "private key")),
        must_act=True, label="paste npub")
    data, _ = await sc.snap("pasted row")
    rows = [p for p in ocr_phrase_all(data, short, fold=True) if p[1] > row_below]
    sc.check("the result row below the box is labelled by the short npub", rows, f"box y={box[1]:.0f}")
    sc.check_screen("no 'no match' line", [], ["No profiles match"])
    if not rows:
        return
    # Without its pill, click_pill would fall back to the recommended row's.
    if not sc.check("the pasted row has a Follow button", row_has_follow(data, rows[0]), f"row y={rows[0][1]:.0f}"):
        return
    if not met:
        sc.accept_step("paste npub", "a row labelled by the short npub, with a Follow button, below the box")
    if not await sc.step(Step(
            f"Click 'Follow' on the result row '{short}' (not the recommended row).",
            [Cand("follow-pasted", "Click the 'Follow' button on the result row of the pasted npub.",
                  [FOCUS, ("click_pill", {"text": short, "below": row_below, "fold": True}), ("wait", 1), PARK])],
            "A confirmation box offers 'Start a new follow list' and 'Cancel'.",
            tries=3, settle=6, cues=("Start a new follow list", "Following", "Saved on")),
            must_act=True, label="follow pasted"):
        return
    if has(sc.screen, "Start a new follow list"):
        if not await sc.step(Step(
                "Approve the first follow list: click 'Start a new follow list' in the confirmation box.",
                [Cand("confirm-first-list", "Click the 'Start a new follow list' button beside 'Cancel' in the confirmation box.",
                      [FOCUS, ("click_pill", CONFIRM_ANCHOR), ("wait", 1), PARK])],
                "A 'Saved on N of M Nostr servers' line is shown.",
                tries=4, settle=8, cues=("Saved on", "Following", "Start a new follow list")),
                must_act=True, label="confirm first list"):
            return
    k3 = nak_req(sc.box, 3, acct["pub"])
    sc.check("a kind-3 on the local relay", bool(k3), f"{len(k3)} events")
    if k3:
        sc.check("the newest kind-3 has the pasted key's p tag",
                 any(t[:2] == ["p", target["pub"]] for t in k3[0].get("tags", [])),
                 f"{len(k3[0].get('tags', []))} tags")


# ---------------------------------------------------------------- X
AVATAR_X = (428, 464)   # the avatar sits at x 437-456 in a result row; its label starts at 466


def avatar_spread(data, y):
    """How much of the round avatar on the result row at window y the three
    commonest colours cover (2 bits per channel), from 0 to 1. An initials
    circle is one gradient fill and a letter (about 0.8 measured); the fake's
    sixteen-colour picture spreads over many (about 0.4). The avatar is the
    saturated blob in a fixed window left of the label, shrunk by 3 px. None
    when nothing is found."""
    import io
    from collections import Counter
    from PIL import Image
    cy = int(y)
    im = Image.open(io.BytesIO(data)).convert("RGB")
    px = im.load()
    sat = [(x, y) for y in range(cy - 14, cy + 15) for x in range(*AVATAR_X)
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
    sc.box.launch("X", sc.suite.run_id, nsecs=acct["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"]})
    if not await open_trust(sc):
        return
    box = await find_search_box(sc)
    if box is None:
        return
    sc.box.mark("search")
    met = await sc.step(Step(
        "Click the 'Search profiles to follow' box in step 1 and type 'e2e'.",
        [Cand("search-e2e", "Click the 'Search profiles to follow' input, then type e2e into it.",
              type_into(box, "e2e"))],
        # The Follow labels are dark on violet and never in the OCR (p was
        # 0.44 with every row on screen): the hard check counts the pills.
        "The search box holds 'e2e' and below it result rows show 'Alice E2E', 'Bob E2E' and 'Carol E2E', "
        "each with a round avatar on its left.",
        tries=3, settle=4, cues=("Alice E2E", "Bob E2E", "Carol E2E", "No profiles match", "Searching")),
        must_act=True, label="search e2e")
    await asyncio.sleep(4)   # the picture loads after the row is drawn
    data, _ = await sc.snap("avatars")
    # The rows are found by position: the Follow pills below the box, top to
    # bottom, are Alice, Bob, Carol (OCR lost 'Bob' with the initials circle).
    pills = sorted((p for p in accent_pills(data) if p[1] > box[1] + 12), key=lambda p: p[1])
    if sc.check("three result rows (Alice, Bob, Carol), each with a Follow button, below the box",
                len(pills) == 3, pills) and not met:
        sc.accept_step("search e2e", "three result rows with Follow buttons below the box")
    sc.check("the fake served Alice's picture", "BS GET /pic/alice.png" in sc.log_after("search"))
    if len(pills) != 3:
        return
    pic, plain = avatar_spread(data, pills[0][1]), avatar_spread(data, pills[1][1])
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
WAITING, CALCULATING = "Waiting in Brainstorm", "Calculating your scores"   # OCR may drop the apostrophe
PUBLISHING, DONE_LINE = "Publishing your scores", "Your scores are recalculated and published"
RECALC_WAIT = "Please wait a few minutes before recalculating"
FOLLOW_FIRST = "Follow at least one account first so we can calculate"
SENDING = "Sending your follow list"
RUN_CUES = (WAITING, CALCULATING, PUBLISHING, DONE_LINE, RECALC_WAIT, FOLLOW_FIRST, SENDING)


RECALC_REACH = 90   # px below the 'Saved on' line that the Recalculate pill sits within


async def recalculate(sc, label):
    """One click on the Recalculate pill: the only accent pill between step
    1's 'Saved on' line and step 2's heading (step 2's Calculate pill sits
    below that heading). A missing or ambiguous pill fails the scenario.
    Never retried: every click is one graperank request (3 per 30 min live)."""
    await sc.suite.driver.act(*FOCUS)
    data, _ = await sc.snap(f"{label} before click")
    saved = ocr_phrase_pos(data, "Saved on")
    step2 = ocr_phrase_pos(data, "Ask Brainstorm to calculate")
    if not sc.check(f"{label}: 'Saved on' line on screen", saved is not None):
        return False
    floor = step2[1] if step2 is not None and step2[1] > saved[1] else saved[1] + RECALC_REACH
    pills = [p for p in accent_pills(data) if saved[1] < p[1] < min(floor, saved[1] + RECALC_REACH)]
    if not sc.check(f"{label}: exactly one Recalculate pill under 'Saved on'", len(pills) == 1, pills):
        return False
    await sc.suite.driver.sb.mouse.click(pills[0][0], pills[0][1])
    await sc.suite.driver.act(*PARK)
    return pills[0]


RUN_STATES = (("waiting", WAITING), ("calculating", CALCULATING), ("publishing", PUBLISHING), ("done", DONE_LINE))


async def watch_run(sc, label, pill, timeout=150, every=1.0):
    """Watch one run's status line, the band under the Recalculate `pill`,
    until 'Done' or an error line shows. Each state lasts one poll (20 s),
    and a full see() takes ~40 s on the box, so neither Jev nor see() can
    catch them (run 20261026-173419: the judge first looked at 'Publishing').
    Raw screenshots with OCR of the band alone take a few seconds each.
    Returns [(state, secs after the click, band text)] in the order first
    seen; each new state also lands as an evidence frame."""
    band = (380, max(0, int(pill[1]) - 20), 1220, int(pill[1]) + 110)
    t0, seen, started = time.time(), [], False
    while time.time() - t0 < timeout:
        data = shot_bytes(await sc.suite.sb.screenshot())
        text = "\n".join(line for line, _, _ in fine_lines(data, band, scale=3, thr=50))
        # The previous run's Done line stays until the repaint after the
        # click: nothing counts before this run's own first line shows.
        started = started or any(has(text, p) for p in (SENDING, WAITING, CALCULATING, PUBLISHING,
                                                        RECALC_WAIT, FOLLOW_FIRST))
        for name, phrase in RUN_STATES if started else ():
            if has(text, phrase) and name not in [n for n, _, _ in seen]:
                seen.append((name, round(time.time() - t0, 1), text))
                sc.keep(data, f"{label} {name}", text)
            elif name == "waiting" and has(text, phrase) and "runs ahead" in norm(text):
                # A later waiting frame where OCR read the queue position.
                i = next(i for i, (n, _, _) in enumerate(seen) if n == "waiting")
                if "2 runs ahead" not in norm(seen[i][2]):
                    seen[i] = (name, seen[i][1], text)
        if any(n == "done" for n, _, _ in seen) or has(text, RECALC_WAIT) or has(text, FOLLOW_FIRST):
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
    sc.check(f"{label}: waiting shows the queue position", "2 runs ahead" in norm(waiting), waiting[:200])
    sc.check(f"{label}: the states came in order", names == [n for n, _ in RUN_STATES], names)
    polls = [l.rsplit(" ", 1)[-1] for l in sc.log_after(marker) if l.startswith("BS GET graperankResult -> ")]
    sc.check(f"{label}: the fake answered waiting, calculating, publishing, done",
             polls[:4] == [n for n, _ in RUN_STATES], polls)


async def judge_done(sc, label):
    """Done is the one state that stays: Jev judges it, with the finished
    time from /user/history beside it."""
    done = f"The Recalculate area says 'Done. {DONE_LINE}.' and a 'Last run finished' line shows."
    met = await sc.step(Step("Watch the recalculation finish.",
                             [Cand("wait-for-poll", "Wait a few seconds for the next progress update.", [("wait", 3)])],
                             done, tries=2, settle=1, cues=RUN_CUES + ("Last run finished",)), label=f"{label} done")
    ok = sc.check_screen(f"{label}: 'Done' and 'Last run finished' on screen", [DONE_LINE, "Last run finished"])
    if ok and not met:
        sc.accept_step(f"{label} done", "the Done line and 'Last run finished' on screen")


def bs_posts(sc, marker, path):
    return [l for l in sc.log_after(marker) if l.startswith(f"BS POST {path}")]


async def scenario_v(sc):
    """Follow, Recalculate: waiting, calculating, publishing, done, then the
    finished time from history. A second Recalculate on the unchanged list
    sends graperank only, no followList."""
    acct, owner = fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    sc.box.launch("V", sc.suite.run_id, nsecs=acct["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"], "MC_RECALC_POLL_SECS": RUN_POLL})
    if not await open_trust(sc):
        return
    if not await follow_owner(sc, expect_confirm=True):
        return
    sc.box.mark("V first")
    pill = await recalculate(sc, "first recalculate")
    if not pill:
        return
    check_run(sc, "first run", await watch_run(sc, "first run", pill), "V first")
    await sc.wait_for(["Last run finished"], timeout=20, every=3, label="history")
    await judge_done(sc, "first run")
    sc.check("the first run sent the new follow list once",
             len(bs_posts(sc, "V first", "/user/followList")) == 1, bs_posts(sc, "V first", "/user/followList"))
    sc.check("the first run sent one graperank",
             len(bs_posts(sc, "V first", "/user/graperank")) == 1, bs_posts(sc, "V first", "/user/graperank"))
    sc.check("history was read", any(l.startswith("BS GET /user/history") for l in sc.log_after("V first")))

    sc.box.mark("V second")
    pill = await recalculate(sc, "second recalculate")
    if not pill:
        return
    check_run(sc, "second run", await watch_run(sc, "second run", pill), "V second")
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
    button is disabled and a click sends nothing."""
    acct, owner, other = fresh_key(sc.box), fresh_key(sc.box), fresh_key(sc.box)
    sc.box.start_fake({"graperank": ["429", "403", "ok"], "run": ["waiting", "fail:NO_ELIGIBLE_USERS"]})
    publish_owner_profile(sc.box, owner, sc.suite.run_id)
    nak_publish(sc.box, acct["sec"], 3, "", [f"p={other['pub']}"])
    await asyncio.sleep(1)
    sc.box.launch("W", sc.suite.run_id, nsecs=acct["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"], "MC_RECALC_POLL_SECS": "5"})
    if not await open_trust(sc):
        return
    if not await follow_owner(sc, expect_confirm=False):
        return
    # Counted from the first click: 1, then 2, then 3 — a click that missed
    # (or a stale line from the previous run) cannot pass the next round.
    for n, (marker, want) in enumerate([("W 429", RECALC_WAIT), ("W 403", RECALC_WAIT),
                                        ("W fail", FOLLOW_FIRST)], start=1):
        sc.box.mark(marker)
        if not await recalculate(sc, marker):
            return
        grapes = await wait_log(sc, "W 429", "/user/graperank", n=n)
        if not sc.check(f"{marker}: graperank count is {n}", len(grapes) == n, grapes):
            return
        screen = await sc.wait_for([want], timeout=40, every=3, label=marker)
        sc.check_screen(f"{marker}: its wording shows", [want], [DONE_LINE], screen=screen)
    sc.check("the failed run was polled",
             any(l.startswith("BS GET graperankResult -> fail") for l in sc.log_after("W fail")))
    await asyncio.sleep(6)
    grapes = bs_posts(sc, "W 429", "/user/graperank")
    sc.check("nothing retried: three graperank requests in all", len(grapes) == 3, grapes)
    follows = bs_posts(sc, "W 429", "/user/followList")
    sc.check("followList posted exactly once", len(follows) == 1, follows)

    # A fresh key with no follow list: the Trust tab's button is disabled.
    bare = fresh_key(sc.box)
    sc.box.launch("W2", sc.suite.run_id, nsecs=bare["sec"],
                  extra_env={"MC_ISSUER_NPUB": owner["npub"], "MC_RECALC_POLL_SECS": "5"})
    if not await open_trust(sc):
        return
    screen = sc.screen
    for _ in range(8):
        if has(screen, "Followed someone new"):
            break
        await sc.suite.driver.act("scroll", "down")
        await sc.suite.driver.act(*PARK)
        screen = await sc.see("scroll to web of trust")
    sc.check_screen("no follows: the follow-first line shows", ["Followed someone new", FOLLOW_FIRST], screen=screen)
    # The follow-first line sits right under the Recalculate button. Enabled,
    # the button is an accent pill there; disabled, it is a plain grey one.
    data, _ = await sc.snap("disabled recalculate")
    # The follow-first line itself is too faint for the position OCR: anchor
    # on the readable line above the button.
    line = ocr_phrase_pos(data, "Followed someone new")
    if not sc.check("the line above the Recalculate button was located", line is not None):
        return
    below = [p for p in accent_pills(data) if line[1] < p[1] < line[1] + 80]
    sc.check("the Recalculate button is drawn disabled (no accent pill under the line)", not below, below)
    sc.box.mark("W disabled")
    try:
        await sc.suite.driver.act("click_text", {"text": "Recalculate", "near": "Followed someone new"})
    except Exception as e:
        sc.check("the disabled Recalculate button was clicked", False, e)
        return
    await asyncio.sleep(5)
    # A window re-activation may re-read /setup; only the run's calls count.
    sent = [l for l in sc.log_after("W disabled")
            if l.startswith("BS POST") or "graperankResult" in l or "/user/history" in l]
    sc.check("the disabled Recalculate sent nothing to Brainstorm", not sent, sent)


# ---------------------------------------------------------------- T
ELIGIBLE_TAB = Cand("eligible-tab", "Click the 'Eligible for me to claim' tab above the bounty list.",
                    [FOCUS, ("click_text", "Eligible for me to claim"), ("wait", 1), PARK])


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
    house, claimant = sc.suite.keys["house"]["pub"], sc.suite.keys["claimant"]["pub"]
    other = "e" * 64
    lists = {
        claimant: [["elig-c", f"39998:{other}:eligible-claimant", 3, 3, "e2e"]],
        # The house's own bounty (rank 100 for self) and a rank-2 row on a
        # min-3 auto-pay bounty: the server sends both, the app hides both.
        house: [["elig-h", f"39998:{other}:eligible-house", 2, None, "e2e"],
                ["elig-own", f"39998:{house}:eligible-own", 100, 2, "e2e"],
                ["elig-gated", f"39998:{other}:eligible-gated", 2, 3, "e2e"]],
    }
    sc.box.start_fake({"eligible": lists, "eligible_delay": {claimant: 8}})
    sc.box.mark("T launch")
    sc.box.launch("T", sc.suite.run_id)
    # The claimant is already active at launch. The search box holds the focus
    # then and eats super+2, and click_text "Bounties" can hit the search
    # placeholder: click the sidebar row by its fixed point instead.
    await sc.step(Step("Show the Bounties screen for the claimant (the claimant's header button reads 'Submit a bounty claim').",
                       [Cand("click-bounties", "Click 'Bounties' in the left sidebar.",
                             [FOCUS, ("click", BOUNTIES_XY), ("wait", 1), PARK])],
                       "The Bounties screen shows the 'Eligible for me to claim' tab and the header button reads 'Submit a bounty claim'.", settle=3,
                       cues=("Submit a bounty claim", "New DList + bounty", "Eligible for me to claim")),
                  must_act=True, label="claimant")
    if not sc.check_screen("claimant selected, Eligible tab shown", ["Submit a bounty claim", "Eligible for me to claim"]):
        return
    # Before the step: its super+] sends the house read.
    sc.box.mark("T switch")
    # One candidate: the switch must land while the claimant's 8 s GET is held.
    await sc.step(Step("Open the 'Eligible for me to claim' tab, then switch to the house account while its list is still loading.",
                       [Cand("tab-then-switch", "Click the 'Eligible for me to claim' tab, then press super+] for the next account.",
                             ELIGIBLE_TAB.do + [("stamp", "tab"), ("key", "super+bracketright"), ("wait", 1)])],
                       "The eligible list shows a row on 'eligible-house' (the house is selected; on this tab its header button reads 'Submit a bounty claim').",
                       tries=1, settle=1, cues=("eligible-house", "Submit a bounty claim", "Eligible for me to claim")),
                  must_act=True, label="tab then switch")
    await asyncio.sleep(max(0, sc.stamps.get("tab", time.time()) + 12 - time.time()))
    await sc.see("after the held claimant reply")
    # On the Eligible tab the house claims too, so its header button reads
    # "Submit a bounty claim"; only the house's eligible row marks the house.
    sc.check_screen("house sees its own eligible list only", ["eligible-house", "you qualify", "Submit a bounty claim"],
                    ["eligible-claimant", "eligible-own", "eligible-gated", "New DList + bounty"])
    lines = sc.log_after("T launch")
    gets = eligible_gets(lines)
    sc.check("every eligible GET is for the session's own account", gets and all(v == s for v, s in gets), gets)
    sc.check("the claimant and the house each read their list",
             (claimant[:8], claimant[:8]) in gets and (house[:8], house[:8]) in gets, gets)
    sc.check("each account logged in before its first eligible read",
             logged_in_first(lines, claimant[:8]) and logged_in_first(lines, house[:8]), lines)
    sc.check("the house read went out after the switch", (house[:8], house[:8]) in eligible_gets(sc.log_after("T switch")),
             sc.log_after("T switch"))
    # Back to the claimant: its list is read again and shows.
    sc.box.mark("T back")
    await sc.suite.driver.act("key", "super+bracketright")
    screen = await sc.wait_for(["eligible-claimant"], timeout=20, every=3, label="claimant list")
    sc.check_screen("the claimant's list shows after switching back", ["eligible-claimant", "Submit a bounty claim"],
                    ["eligible-house"], screen=screen)
    sc.check("a second claimant read went out", (claimant[:8], claimant[:8]) in eligible_gets(sc.log_after("T back")),
             sc.log_after("T back"))
    # Nothing left to claim: the empty line, and the detail pane lets go of
    # the row (the fake still answers its detail with the real coordinate).
    sc.box.set_mode({"eligible": {claimant: []}})
    # xfwm4 grabs Super+r on the box, so XTEST never delivers the app's refresh
    # chord. Clicking the tab that is already open re-reads the list the same way.
    for act in ELIGIBLE_TAB.do:
        await sc.suite.driver.act(*act)
    screen = await sc.wait_for(["No open bounties match"], timeout=20, every=3, label="empty list")
    sc.check_screen("empty eligible list says so, no stale detail", ["No open bounties match", "Select a bounty"],
                    ["eligible-claimant"], screen=screen)
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
}
