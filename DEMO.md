# Magic Carpet — stage demo, keyboard only

Every step below is a keystroke. gpui ignores synthetic mouse events, so this
path is also the only one automation can drive. The app never pays anything —
the prod watcher pays; this app publishes lists and claims and watches the
money move.

## First launch (David)

1. **Get past Gatekeeper once.** The app is not signed, so macOS blocks the
   first open: double-click the app, click **Done** on the "could not verify"
   dialog (NOT "Move to Trash"), then open **System Settings →
   Privacy & Security**, scroll to *"Magic Carpet Chat" was blocked*, click
   **Open Anyway**, then **Open**. macOS asks this once. (The zip ships a
   "READ ME FIRST.txt" with the same steps.)
2. The app opens on the onboarding screen when no key is stored anywhere.
3. Paste your Nostr secret key into the masked field and press `Enter`. It
   starts with `nsec1` — copy it from your Nostr app (Settings > Keys in
   Primal or Damus). The app stores it in the Mac keychain, shows your npub,
   your profile name, and whether the profile has a Lightning address
   (payouts need one — without it, claims are accepted but never paid).
4. Press `Enter` again (Start) — or press `esc` at any point to just look
   around read-only.
5. **No Lightning address?** The ready panel offers **Create a Coinos wallet
   for this key** (also on the Wallet screen, `⌘6`). One click opens a
   hosted Coinos wallet bound to the key, writes its address into the
   profile, and shows a QR: scan it with Strike or any Lightning wallet to
   add sats. Coinos holds the funds — keep the balance small. The login is
   in the Mac keychain under `magic-carpet-chat` / `claimant-coinos-login`
   (Copy username is on the panel; the password is in Keychain Access).

## One-time setup (operator)

1. Import both keys (secrets live in `magic-carpet-v2/.fallow/` — never print them):

   ```
   MC_ISSUER_NSEC=… MC_CLAIMANT_NSEC=… ./target/release/magic-carpet-chat --import-keys
   ```

   It prints the two npubs and exits. Keys land in the macOS keychain under
   `magic-carpet-chat`. The `MC_ISSUER_NSEC`/`MC_CLAIMANT_NSEC` env vars also
   work directly at launch — env wins over the keychain, and a key in the env
   skips the onboarding.

2. Build fresh: `/Users/md/.cargo/bin/cargo build --release`.

## The demo path

| Step | Keys | What happens |
|---|---|---|
| 1 | launch the app | Dashboard opens. Rail dot turns **green** when the relay socket is really up (~3 s). "Needs attention" is all-clear once both keys load. |
| 2 | `⌘2` | Bounties. The issuer's live list loads; the first bounty is auto-selected and watched. `↑`/`↓` move the selection. |
| 3 | `⌘N` | "New DList + bounty" form opens (the house issuer is the active account at launch), caret in the first field. |
| 4 | type, `Enter` after each field | Singular → plural → description → criteria → reward (prefilled 100) → cap (prefilled 400) → min rank (prefilled 2). `Enter` on the last field publishes the kind-39998 list through the instance, then creates the auto-pay bounty with the session login. |
| 5 | watch | Activity logs "Published list …" then "Bounty … created — watching for claims". The new bounty is selected; the top-bar pill reads **Auto-pay armed** because the bounty really has autoPay. |
| 6 | `⌘]` | Switch to the claimant (Matthias). The rail ring moves; the sidebar shows his real npub and kind-0 name. **Trap:** while a text field has focus, `⌘[`/`⌘]` indent instead of switching accounts — submit or cancel the form first (both return focus to the shell), or press `⌘2` to park focus. |
| 7 | `⌘N` | Claim form for the selected bounty. |
| 8 | type the item name, `Enter` | Publishes the kind-39999 claim through `POST /api/strfry/publish` (the only door the bounty machinery sees). |
| 9 | watch | The claim card walks the real state machine live: *Claim submitted → Auto-pay attempting… → Paid — waiting for the zap receipt → Payment settled*, then **"Zap receipt … N s after the claim"**. `⌘1` shows the same beats in Recent activity. If the receipt lags, the money has already moved — show the Strike notification, or `↑`/`↓` to the settled rehearsal bounty (`cities-tennessee-rehearsal`) as the receipt exhibit. |

## Other keys

- `⌘1`…`⌘8` — nav (Dashboard, Bounties, …, Chat). Always works, even from inside a text field.
- `⌘R` — refetch the bounty list and both accounts.
- `esc` or `⌘.` — cancel a form (focus returns to the shell).
- `⌘[` / `⌘]` — previous / next account.

## Caveats — read before going on stage

- `esc` inside a field cancels the form only when the field has nothing of its
  own to do with it; `⌘.` cancels from anywhere, always.
- `⌘]` into an account with no key opens the sidebar's paste-a-key input with
  the caret in it — and while that input has focus, `⌘[`/`⌘]` indent instead of
  switching accounts. `esc` closes the input and hands focus back.
- On the onboarding screen, `esc` is "just look around" (read-only dashboard).
- The ISSUER key slot is operator-only: importing a key there changes whose
  bounties the app lists (a claimant who pastes their own nsec as issuer sees
  an empty list). Claiming never needs an issuer key — the app reads the house
  issuer's bounties without one, and the issuer input says so in amber.
- `⌘N` (or the claim/new-bounty button) with no key for the active account
  opens the sidebar key input instead of a form.
- "Forget this key" (under the account name in the sidebar) deletes the
  keychain entry only. A key set through the env vars survives it — env wins.
- Selecting an old bounty replays its historical payment facts into the
  activity feed once, with their real (old) timestamps. That is the watch
  reporting each fact exactly once, not a bug.
- The claim's trust gate is server-side and silent: a claimant below the min
  rank gets **no** payment row ("No auto-payment row yet" stays). Preflight
  with `configure-prod.yml preflight_claimants=…` before the show.
- The search field and the Payments/Claimants/Leaderboards/Tags/
  Accounts/Settings screens are not built; they say so on screen.
- Chat (`⌘8`) needs `ANTHROPIC_API_KEY` in the launching shell.
- NEVER start the laptop `agent-wallet` daemon while the prod instance is
  live — it shares the prod wallet seed and drains the node's outbound.
