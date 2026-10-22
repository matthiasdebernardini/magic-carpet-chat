# e2e-cua: the end-to-end suite on the cua box

One command from the Mac, one report at the end:

    scripts/e2e-cua/run.sh <run-id|path-to-binary|installed>

- `<run-id>`: a GitHub Actions run; `gh run download` fetches its `magic-carpet-chat-linux-x86_64` artifact.
- `<path>`: a Linux x86_64 binary or a `.tar.gz` holding one.
- `installed`: keep the e2e binary already in the `mc` container.

`run.sh` copies the binary into `mc` as `/usr/local/bin/magic-carpet-chat-e2e`, beside the demo's `magic-carpet-chat`, which it never touches. It syncs this folder to `~/mc/e2e/` on `magic-carpet-cua.exe.xyz`, runs `suite.py` there, and copies the run back to `/tmp/mc-e2e/<timestamp>/`. It exits nonzero if any scenario fails. The box never compiles and never needs a GitHub token. The suite takes over the desktop and relaunches `~/mc/run-app.sh` at the end.

Development flags (after the first argument): `--no-paid` skips P and R, `--only A,G` runs a subset, `--mock-jev` makes no TypeSafe calls, `--no-restore` leaves the desktop as the suite left it.

## `--target omac`: the same suite on an Omarchy host

    scripts/e2e-cua/run.sh --target omac <path-to-binary>

Runs everything locally: the app as an XWayland client on `DISPLAY=:0`,
`fake_instance.py` (instance on 127.0.0.1:8787 **and** a fake Brainstorm on
127.0.0.1:8788 — `MC_BRAINSTORM_URL` / `MC_TRUST_RELAYS` point at them),
`nak serve` on 127.0.0.1:10547, `wf-recorder` videos, `grim` screenshots.
Reports land in `/tmp/mc-e2e/<timestamp>/` exactly as on the box. The work
dir `/tmp/mc-e2e-work/<timestamp>/` (mode 0700, holds the per-scenario
nsec stores) is removed at the end.

Setup once: a venv with pillow and the TypeSafe SDK —

    uv venv ~/.config/magic-carpet-e2e/.venv
    uv pip install --python ~/.config/magic-carpet-e2e/.venv/bin/python pillow typesafe-sdk

(`run.sh` falls back to `python3`, where Jev runs mock and the report says
so.) Keys in `~/.config/magic-carpet-e2e/keys.json` (house, claimant),
`TYPESAFE_API_KEY` in `.env` there. Tools needed on PATH: `xdotool`, `grim`,
`wf-recorder`, `tesseract`, `nak`, `wl-paste`, `hyprctl`.

Differences from the box: P reports **SKIP** ("needs the real instance and
its float"); R reports **BLOCKED** ("needs the real instance and live Coinos"); H reports **BLOCKED** too — a new result value the exit code
ignores — because there is no seeded store with the claimant's Coinos
login. L–Q (the Feature-2 trust panel) generate their own account and owner
keys per scenario, publish the owner's kind-0 name to the local relay, and
verify the relay with `nak req`.

### Driver gaps measured on omac (Hyprland 0.56.2, cua-driver 0.34.0)

The original plan was cua-driver with `delivery_mode: foreground`. Measured:

- Background delivery refuses the app outright (`client_not_qualified` —
  the gpui window is not AT-SPI qualified).
- Every foreground call (click / type_text / press_key / hotkey) returns
  `primary_target_busy` / `foreground_unavailable`: the app binds seat
  resources the input-v3 Hyprland plugin counts as busy. The same calls
  succeed against foot.
- `hl.dsp.send_shortcut` and `wtype` deliver into foot but have zero effect
  on the gpui window.
- Omarchy binds super+digit / super+`]` / super+n itself, so those chords
  can never reach the app as compositor keystrokes.
- `set_window_frame` cannot place or resize a Wayland window; the Hyprland
  Lua eval (`hl.dsp.window.float / resize / move`) can.

So the omac driver runs the app under XWayland (no `WAYLAND_DISPLAY`) and
drives it with `xdotool --window` — window-local XTEST events, the same
mechanism the box uses against its Xvfb. `super+1/2/6/n/]` then land as real
app keystrokes because Hyprland never sees them. Window placement to
0,0 1280x960 uses `hl.dsp.*` eval calls; screenshots are `grim` at the live
`hyprctl clients` geometry (window-local pixels, so SIDEBAR/CONTENT crops
are unchanged); the clipboard is `wl-paste`; recording is `wf-recorder -g`.
App shutdown is `SIGTERM` on the suite's own child (Cua `kill_app` is
unaffected by the input gap, but the suite holds the pid anyway).

## What each scenario checks

Every scenario starts from a clean state: a fresh `MC_STORE_DIR`, the app relaunched with its own env, and an ffmpeg x11grab recording. A to H run against `fake_instance.py` (inside `mc` on 127.0.0.1:8787) and a `nak serve` relay on 127.0.0.1:10547, so nothing reaches the real instance. Two exceptions reach live services: trust reads of the fake-target scenarios go to the local relay, and R publishes two "E2E Newcomer …" kind-0 profiles (with a Lightning address) to the public relays, because the instance target leaves `MC_TRUST_RELAYS` unset. H and P talk to live coinos.io for the claimant's wallet balance. R makes two new Coinos wallets there. H needs that wallet to hold sats, or its sats-left check fails.

| ID | Checks |
|---|---|
| A | A create hangs. Switching account, opening account setup (`+`) and removing the active account are refused with the amber "Still sending. Wait for it to finish." line. Esc keeps the form open. After the 20 s client timeout the form unlocks. One create request, both accounts still stored. |
| B | A 502 on create shows the "may exist already" hint, and the fake server logs a `GET /api/bounties` refetch after it. |
| C | A 400 on create shows the refusal with no hint and no refetch. |
| D | A second open bounty for the same list is refused in the form; no publish and no create request reach the server. |
| E | 401 then 2xx: one re-login, then the create succeeds. 401 twice: one re-login, two creates, then a clean error in the open form. |
| F | The profile scan returns 500: the claimant's Wallet shows "Couldn't read this profile" and "Try again". With the scan healthy, Try again reaches the Read state (the create-wallet offer; never clicked). |
| G | Copying the nsec puts it on the X11 clipboard (xclip); it is still there at 20 s and gone at 31 s. |
| H | The first remove click arms "Click again to remove" in the sidebar and on the Identity card; navigating away disarms both; nothing is removed. For the claimant (Coinos wallet, store seeded from `/root/mc-store`) the armed state warns about the sats left. |
| L | Fresh account, no kind-3: the recommended owner row's Follow shows the "Start a new follow list" confirmation; approving it publishes a kind-3 with the owner's `p` tag; Calculate then Publish lands a kind-10040 whose tags equal the fake `/setup` rows verbatim; the Trust card reads "Located". |
| M | A seeded kind-3 (three follows, content "e2e old content"): Follow needs no confirmation; the newest kind-3 has four `p` tags and unchanged content. |
| N | `MC_TRUST_RELAYS` = local relay + a dead one: Follow shows "Couldn't check your follow list" and no kind-3 is published. |
| O | graperank answers 429, 403, 503: each shows its wording ("Please wait a few minutes before recalculating…" twice, then "The Brainstorm server is temporarily unavailable…"), Calculate stays clickable each time (three graperank requests), and `followList` is posted at most once. |
| Q | graperank ok but `/setup` 404: "Publish my Treasure Map" stays disabled — clicking it sends nothing, no kind-10040 lands — and the "Brainstorm has no data for this account" line shows. |
| S | The house issuer selects its open bounty on a list: "New bounty on this list" shows, the claimant does not see it. Clicking it opens a form of that name with a "Bounty criteria" field and no "Item (singular)", "Items (plural)" or "List description" field; Enter through criteria, amount, cap and rank sends exactly one `POST /api/bounties` on the existing list's coordinate and no list publish. The list's open bounty does not stop it. Free: the fake server only. |
| T | The claimant opens "Eligible for me to claim" and switches to the house while its `GET /api/bounties/eligible` is held 8 s. After 12 s the house's tab shows "eligible-house" with the "you qualify" chip, and neither the claimant's row nor the two rows the server sends but the app hides (the house's own bounty, and a rank-2 row on a min-3 auto-pay bounty). Every eligible GET in the fake log has viewer = session, each account logs in before its first one, and the house read goes out after the switch. Switching back reads and shows "eligible-claimant"; with that list emptied, ⌘R shows "No open bounties match your rank right now." and the detail pane lets go of the row ("Select a bounty"), though the fake still answers its detail. Free: the fake server only. |
| P | Paid, against the real tapestry instance. The house creates a 100-sat bounty with cap 100 (closed after its one payout) on a list named `e2e-<run>`; the claimant claims it; the card must show "Payment settled" and "Payment receipt". The claim form must name this run's list before Enter, so no other bounty gets paid. The create and the claim run once each, never retried. |
| R | Paid, against the real tapestry instance, with only the house in the store. Two newcomers (A, B) each create an account in the app (`+`, "Create a new account"): the Coinos wallet is made and "Coinos wallet ready" shows. The store then holds three accounts, two app-made with a wallet. The house publishes a 100-sat bounty with cap 200 on `e2e-new-<run>` ("Auto-pay armed"); A and B each claim it and each card must show "Payment settled" and "Payment receipt". Same gates and never-retried create and claims as P. |

A scenario passes when every judged step is met, or accepted by a hard check on the same screen (shown as "(N by hard check)"), and every hard check passes (literal OCR phrases, fake-server log lines, the clipboard, the float).

## The float

P needs at least 200 sats in the instance's agent-wallet, R at least 250 (two payouts plus fees); below that the suite stops before it starts and says so. P spends about 102 sats (100 payout plus fees), R about 204; a run with both P and R needs 352 and spends about 306. A run that stops between R's publish and B's payout leaves an open auto-pay bounty with up to 200 sats of cap on the instance; with the box's trust-gate bypass, any key that reaches the instance could claim a leftover bounty. Each R run also leaves about 100 sats in each of two throwaway Coinos wallets, whose logins end up in `/root/.trash` with the store. An e2e bounty closes after its payout, so a leftover one never pays again. To top up, on the box:

    ~/mc/fund.sh 500

and pay the invoice it prints (it expires in 10 minutes). If a payment fails, the suite reports it and does nothing else; see "Auto-pay operations" in `~/mc/README.md` for a manual reset.

## Output

The scenario stores (`/root/e2e-stores/<run>` in `mc`) hold copies of the nsecs; the suite moves them to `mc`'s root-only `/root/.trash/` at the end of each run.

`/tmp/mc-e2e/<timestamp>/`: `report.md`, `report.json` (scenario, result, evidence path, Jev picks and verdicts, hard checks, duration), `suite.out`, and per scenario `<ID>/<ID>.mp4`, `frames/` (PNG plus OCR text per observation), `app.log`, `fake-server.log`. Evidence shows at most the first 8 characters of an nsec.

## Add a scenario

1. Write `async def scenario_x(sc)` in `scenarios.py`: `sc.box.start_fake({...})` (modes are in `fake_instance.py`), `sc.box.launch("X", sc.suite.run_id)`, then `await sc.step(Step(goal, [Cand(...)], done, cues=...))` for each UI action. Add `must_act=True` when the goal state is already on screen before the action.
2. Add hard checks with `sc.check_screen(...)`, `sc.check(...)` or `sc.log_after(marker)`. A scenario with no hard check fails.
3. Register it in `SCENARIOS`. Candidates must never create a wallet, really remove an account, or publish outside the fake server (except P and R).
