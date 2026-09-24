# e2e-cua: the end-to-end suite on the cua box

One command from the Mac, one report at the end:

    scripts/e2e-cua/run.sh <run-id|path-to-binary|installed>

- `<run-id>`: a GitHub Actions run; `gh run download` fetches its `magic-carpet-chat-linux-x86_64` artifact.
- `<path>`: a Linux x86_64 binary or a `.tar.gz` holding one.
- `installed`: keep the e2e binary already in the `mc` container.

`run.sh` copies the binary into `mc` as `/usr/local/bin/magic-carpet-chat-e2e`, beside the demo's `magic-carpet-chat`, which it never touches. It syncs this folder to `~/mc/e2e/` on `magic-carpet-cua.exe.xyz`, runs `suite.py` there, and copies the run back to `/tmp/mc-e2e/<timestamp>/`. It exits nonzero if any scenario fails. The box never compiles and never needs a GitHub token. The suite takes over the desktop and relaunches `~/mc/run-app.sh` at the end.

Development flags (after the first argument): `--no-paid` skips P, `--only A,G` runs a subset, `--mock-jev` makes no TypeSafe calls, `--no-restore` leaves the desktop as the suite left it.

## What each scenario checks

Every scenario starts from a clean state: a fresh `MC_STORE_DIR`, the app relaunched with its own env, and an ffmpeg x11grab recording. A to H run against `fake_instance.py` (inside `mc` on 127.0.0.1:8787) and a `nak serve` relay on 127.0.0.1:10547, so nothing reaches the real instance. Two exceptions reach live services: trust reads go to public relays (read only), and H and P talk to live coinos.io for the claimant's wallet balance. H needs that wallet to hold sats, or its sats-left check fails.

| ID | Checks |
|---|---|
| A | A create hangs. Switching account, opening account setup (`+`) and removing the active account are refused with the amber "A submit is in flight; wait for it to finish" line. Esc keeps the form open. After the 20 s client timeout the form unlocks. One create request, both accounts still stored. |
| B | A 502 on create shows the "may have created" hint, and the fake server logs a `GET /api/bounties` refetch after it. |
| C | A 400 on create shows the refusal with no hint and no refetch. |
| D | A second open bounty for the same list is refused in the form; no publish and no create request reach the server. |
| E | 401 then 2xx: one re-login, then the create succeeds. 401 twice: one re-login, two creates, then a clean error in the open form. |
| F | The profile scan returns 500: the claimant's Wallet shows "Couldn't read this profile" and "Try again". With the scan healthy, Try again reaches the Read state (the create-wallet offer; never clicked). |
| G | Copying the nsec puts it on the X11 clipboard (xclip); it is still there at 20 s and gone at 31 s. |
| H | The first remove click arms "Click again to remove" in the sidebar and on the Identity card; navigating away disarms both; nothing is removed. For the claimant (Coinos wallet, store seeded from `/root/mc-store`) the armed state warns about the sats left. |
| P | Paid, against the real tapestry instance. The house creates a 100-sat bounty with cap 100 (closed after its one payout) on a list named `e2e-<run>`; the claimant claims it; the card must show "Payment settled" and "Zap receipt". The claim form must name this run's list before Enter, so no other bounty gets paid. The create and the claim run once each, never retried. |

A scenario passes when every Jev postcondition is met and every hard check passes (literal OCR phrases, fake-server log lines, the clipboard, the float).

## The float

P needs at least 200 sats in the instance's agent-wallet; below that the suite stops before it starts and says so. One run spends about 102 sats (100 payout plus fees). An e2e bounty closes after its payout, so a leftover one never pays again. To top up, on the box:

    ~/mc/fund.sh 500

and pay the invoice it prints (it expires in 10 minutes). If a payment fails, the suite reports it and does nothing else; see "Auto-pay operations" in `~/mc/README.md` for a manual reset.

## Output

The scenario stores (`/root/e2e-stores/<run>` in `mc`) hold copies of the nsecs; the suite moves them to `mc`'s root-only `/root/.trash/` at the end of each run.

`/tmp/mc-e2e/<timestamp>/`: `report.md`, `report.json` (scenario, result, evidence path, Jev picks and verdicts, hard checks, duration), `suite.out`, and per scenario `<ID>/<ID>.mp4`, `frames/` (PNG plus OCR text per observation), `app.log`, `fake-server.log`. Evidence shows at most the first 8 characters of an nsec.

## Add a scenario

1. Write `async def scenario_x(sc)` in `scenarios.py`: `sc.box.start_fake({...})` (modes are in `fake_instance.py`), `sc.box.launch("X", sc.suite.run_id)`, then `await sc.step(Step(goal, [Cand(...)], done, cues=...))` for each UI action. Add `must_act=True` when the goal state is already on screen before the action.
2. Add hard checks with `sc.check_screen(...)`, `sc.check(...)` or `sc.log_after(marker)`. A scenario with no hard check fails.
3. Register it in `SCENARIOS`. Candidates must never create a wallet, really remove an account, or publish outside the fake server.
