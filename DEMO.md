# Magic Carpet — stage demo, keyboard only

Every step below is a keystroke. gpui ignores synthetic mouse events, so this
path is also the only one automation can drive. The app never pays anything —
the prod watcher pays; this app publishes lists and claims and watches the
money move.

## One-time setup

1. Import both keys (secrets live in `magic-carpet-v2/.fallow/` — never print them):

   ```
   MC_ISSUER_NSEC=… MC_CLAIMANT_NSEC=… ./target/release/magic-carpet-chat --import-keys
   ```

   It prints the two npubs and exits. Keys land in the macOS keychain under
   `magic-carpet-chat`.

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
- Selecting an old bounty replays its historical payment facts into the
  activity feed once, with their real (old) timestamps. That is the watch
  reporting each fact exactly once, not a bug.
- The claim's trust gate is server-side and silent: a claimant below the min
  rank gets **no** payment row ("No auto-payment row yet" stays). Preflight
  with `configure-prod.yml preflight_claimants=…` before the show.
- The search field and the Payments/Claimants/Leaderboards/Wallet/Tags/
  Accounts/Settings screens are not built; they say so on screen.
- Chat (`⌘8`) needs `ANTHROPIC_API_KEY` in the launching shell.
- NEVER start the laptop `agent-wallet` daemon while the prod instance is
  live — it shares the prod wallet seed and drains the node's outbound.
