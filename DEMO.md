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
2. The app opens on the account screen when no account is stored.
3. **Create a new account** (the primary button): type a name if you like
   and press `Enter`. The app makes a key, opens a Coinos wallet for it,
   writes the wallet's address into the profile, and shows a QR. Scan it
   with Strike or any Lightning wallet; the line under the QR turns green
   with "Received N sats" when the money lands. Coinos holds the funds —
   keep the balance small.
4. Or **I already have a key**: paste your Nostr secret key (it starts with
   `nsec1` — Settings > Keys in Primal or Damus) and press `Enter`. The app
   shows your npub, your profile name, and whether the profile has a
   Lightning address (payouts need one — without it, claims are accepted but
   never paid). No address? The ready panel offers **Create a Coinos wallet
   for this account** (also on the Wallet screen, `⌘6`). The Wallet screen
   can also send sats from that wallet to any Lightning address.
5. Press `Enter` again (Start) — or press `esc` at any point to just look
   around read-only. Keys and Coinos logins live in
   `~/Library/Application Support/magic-carpet-chat/accounts.json`; **Copy
   username** / **Copy password** on the wallet panel are how you get into
   coinos.io itself, if you ever want to.

## One-time setup (operator)

1. Import both keys (secrets live in `magic-carpet-v2/.fallow/` — never print them):

   ```
   MC_NSECS=<issuer nsec>,<claimant nsec> ./target/release/magic-carpet-chat --import-keys
   ```

   It prints the npubs and exits. Keys land in
   `~/Library/Application Support/magic-carpet-chat/accounts.json`. `MC_NSECS`
   also works directly at launch — the same import runs before the window
   opens, and a key already stored is left alone. The issuer role is fixed:
   the account whose pubkey is the house issuer (or `MC_ISSUER_NPUB`) gets
   the bounty form, every other account gets the claim form.

2. Build fresh: `/Users/md/.cargo/bin/cargo build --release`.

## The demo path

| Step | Keys | What happens |
|---|---|---|
| 1 | launch the app | Dashboard opens. Rail dot turns **green** when the relay socket is really up (~3 s). "Needs attention" is all-clear once both keys load. |
| 2 | `⌘2` | Bounties. The issuer's live list loads; the first bounty is auto-selected and watched. `↑`/`↓` move the selection. |
| 3 | `⌘N` | "New DList + bounty" form opens (the rail ring must sit on the house issuer — click its avatar or `⌘[`/`⌘]`), caret in the first field. |
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
- `⌘[` / `⌘]` cycle through the accounts in the rail and do nothing with
  fewer than two. The "+" at the end of the rail adds one (paste or create).
- On the account screen, `esc` is "just look around" on first launch and
  Cancel when it was opened from "+".
- The issuer is fixed to the house key (or `MC_ISSUER_NPUB`): adding any other
  key never changes whose bounties the app lists, and only the account holding
  the issuer key gets the bounty form. Claiming never needs the issuer key.
- `⌘N` (or the claim/new-bounty button) with no account opens the account
  screen instead of a form.
- "Remove this account" (under the account name in the sidebar) takes two
  clicks — the first arms it, "Click again to remove" — and deletes the nsec
  AND its Coinos login from the accounts file. Copy the password first if the
  wallet holds sats. A key in `MC_NSECS` comes back on the next launch.
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
