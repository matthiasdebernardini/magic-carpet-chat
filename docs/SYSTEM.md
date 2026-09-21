# Magic Carpet Chat — System Definition

_**This file is the living source of truth for the map.** The interactive atlas is built from the same data. It describes commit 3bf9877; the working tree holds an uncommitted refactor (see NA)._

_Question status: **53 open · 2 routed · 4 resolved**._

## One paragraph

Magic Carpet Chat is a macOS desktop app, one window on gpui-kit, for a Nostr bounty instance. An issuer publishes a DList (kind 39998) and a bounty; a claimant claims an item (kind 39999); the instance's own auto-pay watcher pays sats and a zap receipt (kind 9735) proves it. The app never pays: it signs events, posts them through one REST door, polls the instance, and shows the money move. Nostr, instance and Coinos traffic runs on a second thread with its own tokio runtime; the window only exchanges Command and Update messages with it. The Chat pane is the exception: it streams Claude replies from the Anthropic API on the window's own executor. Keys live in the macOS keychain. A claimant with no Lightning address can open a Coinos wallet in one click. A worker in the repo does the chat job on a separate tailnet box, and nothing wires the two together. Five of twelve screens are placeholders.

## Decisions locked

| Axis | Decision | ADR |
|---|---|---|
| Executor | Nostr, instance and Coinos work runs on a dedicated tokio thread; the window awaits only two futures channels. gpui's executor is not tokio and a tokio future polled there aborts the process. The Chat pane is the one exception: it polls its SSE body on gpui's executor, which is why only ReqwestClient::user_agent may be used. | src/nostr.rs:3-6, src/main.rs:73-80, src/chat.rs:150-152 |
| TLS | rustls's ring provider is installed before any client exists, because gpui-kit and reqwest pull ring and aws-lc-rs both and rustls refuses to guess. | src/nostr.rs:264-269 |
| UI stack | One crates.io dependency, gpui-kit 0.6.0, plus the reqwest client it does not re-export. Cargo refusing two gpui-pre versions is the safety net. | Cargo.toml:8-13 |
| Publish door | DLists and claims leave through POST /api/strfry/publish, never a relay socket; an event sent to public relays never reaches the bounty machinery. The kind-0 profile update goes through the door first and then to three public relays. | src/api.rs:523-526, src/nostr.rs:665-684 |
| Receipt truth | A kind-9735 pushed by the relay only triggers an early poll. A receipt is a fact only when the server returns it in claims[].zapReceipt, because only the server knows the delegate key. | src/nostr.rs:1062-1068 |
| Profile write | The kind-0 is merged field by field and only lud16 changes. A profile rebuilt from nothing would wipe the name and picture on every relay that accepts it. | src/nostr.rs:613-622 |
| Key precedence | MC_ISSUER_NSEC / MC_CLAIMANT_NSEC win over the keychain. Forget this key deletes the keychain entry only, so an env key comes straight back. | src/secrets.rs:55-56, 216-218 |
| Coinos order | The Coinos login is written to the keychain before POST /api/register and deleted only on an explicit refusal, so a signup that times out on the way back is never lost. | src/secrets.rs:223-226 |
| Watching | One live bounty watcher at a time. Ledgers outlive watchers, so re-selecting a bounty never replays its history into the feed. | src/nostr.rs:345-350, 456-458 |
| Validate first | The bounty form is checked before anything publishes, because the DList goes out first and cannot be unpublished. | src/shell.rs:1268-1273 |
| Secrets | nsecs, the Coinos password and the Anthropic key cross threads only inside Secret, whose Debug prints <redacted>. Error text is fixed, never the input. | src/secrets.rs:4-7 |
| Chat backend | The desktop chat calls api.anthropic.com directly. The worker exists for clients that should not hold a key and is not called from src/. | src/chat.rs:29, worker/README.md:4-6 |
| Chat cell | One turn per conversation cell, 409 for a second POST. A finally block releases the cell on every ending the runtime can see; a 30 s stale clock covers the one it cannot, a client hangup. No auth: the tailnet is the perimeter. | worker/src/index.ts:12-14, 55-56, 130-134, 318-325 |
| Packaging | Ship an unsigned .app with a READ ME FIRST for Gatekeeper rather than sign and notarize. | scripts/make-app.sh:8-17 |

## Cost model

No cost model to plan. The chat pane bills the owner's Anthropic key per turn and resends the whole transcript each time; everything else is free relay and REST traffic.
## Reading order (the atlas chapters)

1. **One window, one Shell** — The app is one 1180x800 window; one view called the Shell owns every piece of key, account and bounty state and paints whatever screen is up. _(adds SH, DB, NR)_
2. **Two threads, one bridge** — gpui's executor is not tokio, so everything that talks Nostr, the instance or Coinos lives on a second thread and speaks to the window through two channels. _(adds BR, RT)_
3. **Who you are: keys** — Two account slots, issuer and claimant, each an nsec in the macOS keychain; a first launch with no key anywhere opens onboarding. _(adds KS, OB)_
4. **Reading the instance** — Bounties, claims and payment rows all come from the instance's REST API; the relay socket only feeds the rail dot until a bounty is watched. _(adds IA, IN, RL)_
5. **The issuer publishes** — cmd-N as the issuer: seven fields, one Enter, a kind-39998 DList (the replaceable header every bounty points at) through the publish door, POST /api/strfry/publish, then a logged-in POST that creates the auto-pay bounty. _(adds FM, EV, BO)_
6. **The claimant claims** — cmd-] to the claimant, cmd-N, one item name, Enter: a signed kind-39999 claim goes through the same door, POST /api/strfry/publish, and the feed says so. _(adds AF)_
7. **Watching the money move** — The app never pays: the server's auto-pay watcher does, and this app's bounty watcher polls the detail and reports each payment fact exactly once. _(adds BW, AP)_
8. **A wallet for the key** — A generated key has no Lightning address, so its claims fail at payment; one click opens a Coinos wallet bound to the key. _(adds WA, CO, CX)_
9. **Telling the payers the address** — Still inside the wallet command, the new address is merged into the key's existing kind-0 profile, only lud16 changed, and published to the instance and then three public relays. _(adds PR)_
10. **Chat, two ways** — The cmd-8 pane streams Claude straight from Anthropic with the owner's key; a worker in the repo does the same job on a separate tailnet box, and nothing connects them. _(adds CP, AN, CC)_
11. **Under the floor, and what is moving** — One UI dependency, a copied palette, an unsigned bundle; five placeholder screens; and an uncommitted refactor that changes the key store. _(adds PK, UB, NA)_
12. **The whole system** — Everything at once: pick a flow bottom left, hover to read, click to pin, arrow right to go inside.

## Structures

### Screens

#### OB · Onboarding screen

**In one line.** First launch with no key anywhere: paste or generate an nsec as the claimant, or press esc to look around read-only.

**What it does.** A masked field, a "No key yet? Create a new one" button, and a "Just look around" card. After a paste the card becomes a ready panel: profile name, short npub, and either the Lightning address, a "could not check" line, or a Coinos wallet offer. Enter starts the app.

**How it's built.** Shown only when both accounts are confirmed missing, decided once per launch (`shell.rs:215-224, 877-901`). Enter sends `Command::ImportKey { Claimant, Secret }`; "Create a new one" runs `Keys::generate()` on the UI thread and takes the same path (`shell.rs:904-960`). The runtime answers `AccountLoaded`, then `ProfileLoaded` only if a kind-0 was found, then `ImportReady { profile_checked, profile_found }`. A paste with no kind-0 gets an amber "double-check you pasted the right one"; a generated key does not (`onboarding.rs:150-166`). esc or cmd-. calls `leave_onboarding`: Dashboard as claimant, sidebar key input open if still keyless (`shell.rs:983-1005`).

**Steps in execution.**

1. **Decide** — Both keys missing: Screen::Onboarding, active account Claimant, caret in the masked field.
2. **Paste or generate** — Enter sends ImportKey; the button generates a key and sends the same command.
3. **Validate and store** — Runtime checks the shape, writes the keychain, drops the cached session.
4. **Announce** — AccountLoaded; a kind-0 probe through the instance; ProfileLoaded if found; ImportReady.
5. **Show** — Name, npub, Lightning address or the Coinos offer; amber warning if a paste found no profile.
6. **Leave** — Enter or esc: clear the field, Dashboard as claimant.

**Questions.**

- **Q-OB1** Esc while "Checking the key…": the key is stored and the account row updates, but the ready panel (address check, wallet offer) is lost (shell.rs:596-623).
- **Q-OB2** The kind-0 probe reads the instance only; a profile that lives on public relays alone shows the wrong-key warning and a wallet offer.
- **Q-OB3** The ready panel says "Key saved to this Mac's keychain" in debug builds too, where the key is a plain file.

#### DB · Dashboard screen

**In one line.** The cmd-1 home: "Needs attention" derived from live state beside "Recent activity", the feed the runtime writes.

**What it does.** Header: how many of the two keys loaded and which host the app watches. Card one lists up to three problems: claimant key missing, relay down, bounty list failed. Empty means an all-clear line. Card two is the activity feed, newest first. A button jumps to Bounties and opens the form for the active account.

**How it's built.** `attention_rows` (`dashboard.rs:106-145`) builds rows from `view(Claimant).missing`, `relay_connected` and `Load::Failed`. A missing issuer key is deliberately not a row: the app lists the house issuer's bounties without one. "Keys loaded" is claimed only when both npubs exist (`dashboard.rs:203-214`). Activity rows render `ActivityItem { dot, text, at }` with the time in UTC and a relative age ("just now", then s, min, h, d); a 30 s clock task repaints so ages move (`shell.rs:421-430`). The rail dot lives in `shell.rs:1535-1548`, not here.

**Steps in execution.**

1. **Summary** — Count accounts with an npub; host is the API base with the scheme stripped.
2. **Attention** — Claimant key missing, relay down, list failed: up to three rows, badge is the count.
3. **All clear** — "Loading the bounty list…" while loading, else one of two green lines.
4. **Activity** — Feed rows with dot, text, HH:MM:SS UTC and a relative age.
5. **CTA** — Go to Bounties and open the form; keyless opens the key input instead.

**Questions.**

- **Q-DB1** A list failure after a good load only adds a red feed line; the stale list stays and "Needs attention" stays all-clear (shell.rs:670-676).
- **Q-DB2** Absolute times are UTC, so a Nashville audience reads 16:13 for an 11:13 event (timefmt.rs:3-5).

#### BO · Bounties screen

**In one line.** cmd-2: the issuer's bounties beside a detail pane with one payment timeline per claim; the two forms render above both.

**What it does.** A 300 px list (d-tag, status pill, sats, paid x/y, auto-pay) and a detail pane (criteria, reward, cap, min rank, slots, then claim cards). Up and down arrows or a click select a bounty and start watching it. Each claim card is a dot-and-line timeline built from the claim's payment row and its zap receipt.

**How it's built.** `bounties.rs:657-716` renders from Shell state; `select` sends `Command::WatchBounty` and retargets an open claim form (`shell.rs:1108-1128`). Card rows (`bounties.rs:240-332`): "Claim submitted"; "Auto-pay blocked: reason" if the server set one; a retroactive "Auto-pay attempted" from the row's created_at, because a 2 s poll usually misses the attempting state; one row for the current state; "Zap receipt id… · N sats · N s after the claim". State text: attempting "Auto-pay attempting…", paid "Paid — waiting for the zap receipt", settled "Payment settled", paid_unreceipted "Paid, no receipt (reason)", failed "Auto-pay failed (reason)", anything else "Payment state: other", no row "No auto-payment row yet". List and claims scroll independently so the header stays pinned.

**Steps in execution.**

1. **Load** — Loading, Failed with Retry (cmd-R), empty, or rows.
2. **Select** — Click or arrows set the selection and send WatchBounty; idempotent for a live watcher.
3. **Detail** — Snapshot from details, else the list, else the optimistic copy of a just-created bounty.
4. **Claim cards** — Current state plus a retroactive "Auto-pay attempted"; the other transitions live in the feed.
5. **Forms** — cmd-N: seven fields for the issuer, one field for the claimant, above both panes.

**Questions.**

- **Q-BO1** A claim with no created_at gets timestamp 0: a 1970 time and a huge receipt delta (bounties.rs:242).
- **Q-BO2** Up and down do nothing off this screen, while the list is loading or failed, or when it is empty (shell.rs:1129-1139).
- **Q-BO3** The card ignores Claim.payment_status and payment_amount_sats; is autoPayment the only source of truth?

#### NR · Rail, sidebar and shortcuts

**In one line.** Seventeen shortcut bindings for sixteen actions, a 70 px rail with both account markers and the relay dot, and a sidebar with the paste-a-key card.

**What it does.** cmd-1 to cmd-8 switch screens even from inside a text field. cmd-[ and cmd-] switch account. cmd-N opens a form, cmd-R refreshes, esc or cmd-. cancels. The sidebar shows the active account's name, npub and payout address, "Forget this key" when a key exists, three sats cards, and the paste-a-key card when something opened it.

**How it's built.** `keybindings()` (`shell.rs:109-138`): the cmd bindings have no context so they fire inside inputs; up, down and enter are scoped to key context "Shell" so a focused input's own Enter wins (`shell.rs:2114-2120`). `step_account` toggles Issuer and Claimant (`shell.rs:1089-1095`). The paste card opens on cmd-] into a keyless slot (caret in it), cmd-N as a keyless account, a click on the sidebar header, or leaving onboarding keyless; esc closes it (`shell.rs:1026-1080, 1157-1168`). It imports into whichever account is active, issuer included, with an amber "Operators only" note. Rail markers show the kind-0 picture, else initials in the account hue, else a dashed "?" (`shell.rs:1450-1481`). The dot is GREEN when `relay_connected`. Sidebar stat cards: open bounties, committed sats, settled to date (receipted payouts only).

**Steps in execution.**

1. **Bind** — main.rs installs the bindings after gpui_kit::init.
2. **Dispatch** — The root element tracks focus, key context Shell, sixteen on_action handlers.
3. **Switch account** — cmd-] into a keyless slot opens the paste card focused; submit or esc returns focus to the shell.
4. **Paste a key** — Enter sends ImportKey for the active slot; AccountLoaded closes the card.
5. **Forget** — Deletes the keychain entry only; the runtime re-announces, so an env key comes back.

**Questions.**

- **Q-NR1** While a text field has focus, cmd-[ and cmd-] indent instead of switching accounts; the binding is gpui-kit's (shell.rs:864-866). Rebind, or document only?
- **Q-NR2** The top-bar search field reads "Search lists, bounties, profiles…" and has no handler (shell.rs:456-458, 1904). Hide it until built?

#### AC · Accounts and Account screens

**In one line.** The rail's Accounts button lists every stored account; each card's Manage opens one account's page: header plus five tabs, of which Trust is the one with real data.

**What it does.** Accounts renders one card per account in store order — avatar, name, short npub, "active" pill, balance, Switch and Manage. The Account page shows the account it was opened for (not necessarily the active one): "← All accounts", avatar, name, short npub, "View profile" to brainstorm.world, "Switch to this account" when inactive. Tabs: Identity & keys, Trust, Progress, Wallet, Limits & defaults — the four others say "Not built yet." The Trust tab is five cards: Treasure Map (kind-10040 verdict, relays that returned it, rank row, raw event), Trusted Assertions (kind-30382 from the account's provider and the instance issuer's, in two labelled groups), Minimum rank (INSTANCE_MIN_RANK, read-only), Trust Determination (owner PoV, read-only), Web of trust (follows and verified followers).

**How it's built.** `src/account.rs` renders both screens from `shell.views()` plus `shell.trust`, the per-pubkey `TrustState` map in `src/trust_state.rs`. `Shell::open_account(pubkey, tab)` sets `account_page` and goes to `Screen::Account`; entry points are the dashboard's Fix rows, Accounts › Manage, and the sidebar account chip (its chevron alone still steps accounts, via stop_propagation). Raw-event toggles and the "copied" tick live on `AccountPage`. Every event the tab shows was signature-verified in `trust.rs`; a failed read says so and keeps the prior answer instead of reading as absence.

**Steps in execution.**

1. **Open** — Dashboard "Fix →", Accounts › Manage, or the sidebar chip: open_account(pubkey, Trust).
2. **Fetch** — A stale or never-run read kicks Command::FetchTrust; TrustState::begin bumps the generation and marks parts Loading.
3. **Render** — Five cards; each part renders its verdict, a failure note, or "Checking…".
4. **Tabs** — The other four tabs render "Not built yet."

#### WA · Wallet screen

**In one line.** cmd-6: one card with the account's Lightning address, a funding QR, the Coinos username, and the create or retry button.

**What it does.** The same panel sits inside the onboarding ready panel. It shows the wallet created in this launch first, else the profile's lud16. With an address: the QR and "Scan with Strike or any Lightning wallet to add sats." Without one: an amber note and "Create a Coinos wallet for this key" for the claimant, or "The house key's wallet is not managed here." for the issuer.

**How it's built.** `wallet::panel` (`wallet.rs:128-224`) reads `AccountView.payout_address()` and `WalletState { submitting, created, error, qr }`. `sync_qr` re-encodes only when the address changes: `lnurl_pay` builds https://domain/.well-known/lnurlp/user for whatever domain the address carries and bech32-encodes it (HRP lnurl, original checksum, uppercase), then `QrCode::new("lightning:LNURL1…")`. The QR is a gpui canvas: one black `paint_quad` per dark module, 220 px, 4-module quiet zone, no image crate (`wallet.rs:91-125`). The username line and "Copy username" appear only for a wallet created in this launch; the password never reaches this module. "Retry publishing" re-sends the same command.

**Steps in execution.**

1. **Open** — cmd-6 or the onboarding ready panel.
2. **Address** — The wallet created in this launch, else lud16 from the profile.
3. **QR** — lightning: plus the uppercase LNURL, painted module by module.
4. **Create** — Button sends CreateCoinosWallet; claimant only.
5. **Retry** — Publish failed: amber note, error text, Retry publishing.

**Questions.**

- **Q-WA1** The QR encodes only the LNURL (LUD-01); wallets that prefer LUD-16 text must decode it. Intended?
- **Q-WA2** Retry re-runs the whole command including the Coinos probe, so a Coinos outage blocks a pure relay retry (nostr.rs:740-745).
- **Q-WA3** After a relaunch the username is gone from the panel; only the address remains. Fine?

#### CP · Chat pane

**In one line.** cmd-8: an in-memory transcript and a composer that streams Claude replies token by token straight from Anthropic, on the window's own executor.

**What it does.** Enter sends the whole transcript to the Anthropic Messages API; each text chunk lands in the pending bubble, which renders as markdown. Shift+Enter makes a new line. Nothing persists: quit and it is gone. The key comes from ANTHROPIC_API_KEY or ~/.config/anthropic/key. This is the one network path that bypasses the runtime thread.

**How it's built.** `chat.rs:25-29`: MODEL claude-sonnet-5, MAX_TOKENS 64000, API_URL api.anthropic.com/v1/messages. `send` pushes the user message and an empty assistant bubble, builds the body from every non-empty message with no system prompt, takes `cx.http_client()` (gpui's own reqwest client from `main.rs`) and `cx.spawn`s `stream_reply` on gpui's foreground executor, holding the Task in `_stream` because gpui tasks cancel on drop (`chat.rs:119-156`). Because the body is polled there, only `ReqwestClient::user_agent` is safe; the read-timeout constructor aborts the process (`main.rs:73-80`). The SSE body is read line by line; content_block_delta text is appended, a max_tokens stop adds an italic note, an error event replaces the bubble with "Something went wrong" (`chat.rs:322-354`). The Chat entity is built once and lives as long as the Shell, so the transcript survives screen switches.

**Steps in execution.**

1. **Enter** — PressEnter, not shift, not streaming, non-empty: composer cleared.
2. **Bubbles** — User message plus an empty assistant bubble shown as "thinking…".
3. **Request** — POST with stream:true and the full transcript; headers anthropic-version and x-api-key.
4. **Stream** — Each content_block_delta appends text, scrolls to bottom, repaints.
5. **Finish** — Error text or "The model sent an empty reply." replaces the bubble.

**Questions.**

- **Q-CP1** Error bubbles, including the "No API key" notice, are resent to the model as assistant turns on the next Enter, because only empty messages are filtered (chat.rs:139).
- **Q-CP2** No cancel while streaming; the only way out is to close the window (chat.rs:91, 152).
- **Q-CP3** A mid-stream error overwrites the text already streamed (chat.rs:172). Keep the partial reply?
- **Q-CP4** message_stop is never checked; a reply is "done" when the connection closes, so a graceful cut-off looks finished (chat.rs:351).
- **Q-CP5** The whole transcript is resent every turn with no cap and no caching; cost grows linearly.

### Shell and runtime

#### SH · Shell

**In one line.** The one gpui view that owns every piece of key, account and bounty state and paints rail, sidebar, top bar and the active screen.

**What it does.** Which screen is up, which of the two identities is active, what the runtime knows about each key and profile, the bounty list and per-bounty snapshots, the open form, the activity feed. Nothing else holds key, instance or bounty state; the Chat pane keeps only its own transcript. Every Update from the runtime is folded into this struct, then the frame repaints.

**How it's built.** `Shell` (`shell.rs:342-388`): `screen`, `account` (Issuer at launch), `issuer` and `claimant: AccountView`, `relay_connected`, `activity`, `bounties: Load<Vec<Bounty>>`, `selected`, `details`, `optimistic` (a just-created bounty until the server echoes it), the three forms, `onboarding`, `first_screen_decided`, `pending_bounty` (the bounty half parked while the DList publishes), `commands`, `chat`, `focus`. `apply` (`shell.rs:532-843`) matches all seventeen Update variants and ends with `cx.notify()`. `main.rs` builds it inside a Root after theme, palette, shortcuts and the HTTP client, in a 1180x800 window.

**Steps in execution.**

1. **Boot** — main.rs: theme Dark, palette::apply, bind_keys, set_http_client, open the window, Root(Shell).
2. **Shell::new** — Take focus back from the Chat composer, spawn the runtime, then send Connect, LoadAccounts, FetchBounties.
3. **First screen** — Onboarding only when both keys are confirmed missing, decided once.
4. **Render** — Onboarding fills the window; otherwise rail | sidebar | top bar + main.
5. **Apply** — One Update, one state change, one cx.notify.
6. **Chain** — DListPublished sends CreateBounty; BountyCreated and ClaimPublished send WatchBounty; a fresh bounty list selects and watches the first row when nothing is selected.

**Questions.**

- **Q-SH1** Command::Shutdown exists but nothing sends it; the runtime thread dies with the process. Intentional?
- **Q-SH2** Every unbounded_send drops its Result; if the runtime thread died, a submit does nothing beyond the earlier Error { Runtime } line.

#### BR · Runtime bridge

**In one line.** Two unbounded channels: Commands go down fire-and-forget, Updates come up and are drained by one gpui task into Shell::apply.

**What it does.** The only contact between the window and the runtime thread. Eleven Command variants down, seventeen Update variants up. Every Update is a deduplicated fact: the runtime decides what is new, the Shell only records it. Secrets cross only inside a wrapper that prints redacted.

**How it's built.** `spawn_runtime()` returns `NostrHandle { commands: UnboundedSender<Command>, updates: UnboundedReceiver<Update> }` (`nostr.rs:240-243`). The Shell keeps the sender and hands the receiver to `cx.spawn_in`: `while let Some(update) = updates.next().await { shell.apply(update) }`, held in `_updates` so dropping the Shell cancels it (`shell.rs:407-419`). Commands: Connect, LoadAccounts, FetchBounties, PublishDList, CreateBounty, PublishClaim, ImportKey, ForgetKey, CreateCoinosWallet, WatchBounty, Shutdown. Updates: RelayStatus, AccountLoaded, AccountMissing, ProfileLoaded, ImportFailed, ImportReady, Bounties, BountiesFailed, BountyDetail, DListPublished, BountyCreated, ClaimPublished, PaymentState, ReceiptSeen, WalletCreated, WalletFailed, Error { source }. An Error fails a form only when its source is that form's own command (`shell.rs:815-841`).

**Steps in execution.**

1. **Send** — commands.unbounded_send(Command::…), result ignored.
2. **Dispatch** — Connect is awaited inline and WatchBounty aborts the old watcher inline; every other command is tokio::spawned.
3. **Emit** — The task sends one Update per fact.
4. **Drain** — The gpui task awaits the next Update and calls apply.
5. **Repaint** — apply ends with cx.notify; one frame per Update.

**Questions.**

- **Q-BR1** Both channels are unbounded; a window that stops draining never applies backpressure. Fine for one window?

#### RT · Runtime thread

**In one line.** An OS thread named nostr-runtime with its own two-worker tokio runtime; everything that talks Nostr, the instance or Coinos lives here.

**What it does.** gpui's executor is not tokio, and nostr-sdk and reqwest need tokio. So the network half is a second thread with its own runtime, and the window never awaits a tokio future. The thread owns one relay client, one session per account for writes, throwaway keyless clients for reads, the live bounty watcher, and the ledgers.

**How it's built.** `spawn_runtime` (`nostr.rs:263-303`): install the ring TLS provider, open the two channels, start the thread, `block_on(run)`. `run` (`nostr.rs:327-484`) builds `Client::default()`, adds `relay_url()`, spawns the 3 s status poller, and loops over Commands. Every command except Connect, WatchBounty and Shutdown is `tokio::spawn`ed with the update sender; publish and wallet work also gets the sessions map, while LoadAccounts and FetchBounties read keys directly and build their own keyless Api. `Session { keys, api, logged_in }` is built lazily per account behind two mutexes (`nostr.rs:492-509`); ImportKey and ForgetKey evict it so a cached session never signs with a stale key. Errors fold into `BridgeError` and go out tagged with an `ErrorSource` (Runtime, Accounts, DList, Bounty, Claim, Watch).

**Steps in execution.**

1. **Install ring** — rustls::crypto::ring::default_provider().install_default() before any client.
2. **Spawn** — std::thread "nostr-runtime", tokio multi_thread with 2 workers.
3. **Relay** — Client::default(), add_relay, spawn watch_relay_status.
4. **Dispatch** — Match each Command; tokio::spawn account work and watches.
5. **Sessions** — First write per account: read the key, build an Api with its own cookie jar.
6. **Shutdown** — Break the loop, abort the poller and watchers, disconnect. Never sent today.

**Questions.**

- **Q-RT1** spawn_runtime reports a thread or tokio build failure as Error { Runtime }, but the handle then has no live receiver. Does the UI treat that as fatal?
- **Q-RT2** No 401 handling resets Session.logged_in; only re-pasting the key evicts the session, so an expired cookie turns CreateBounty into 401 until then (nostr.rs:535-538, 868).

#### FM · Forms

**In one line.** cmd-N: the issuer's seven-field "New DList + bounty" form or the claimant's one-field claim form; Enter advances and submits.

**What it does.** Issuer fields: item singular, items plural, list description, bounty criteria, reward per item (100), bounty cap (400), auto-pay minimum rank (2). Five are checked before anything publishes, because the DList goes out first and cannot be unpublished; plural and description are optional. Claimant field: the item name for the selected bounty.

**How it's built.** `new_item` (`shell.rs:1157-1177`): keyless account opens the sidebar key input instead; else go to Bounties and open by role. `submit_bounty_form` (`shell.rs:1253-1366`) validates singular, criteria, reward, cap and rank, parks a `CreateBounty` in `pending_bounty` with `reward_per_item: true`, `max_rewards_per_npub: 1`, `auto_pay: true` hardcoded, and sends `PublishDList`. `DListPublished` fills the coordinate and sends `CreateBounty`; `BountyCreated` closes the form, selects the id, builds the optimistic card, sends `WatchBounty` and `FetchBounties`. The claim form snapshots bounty id and coordinate and sends `PublishClaim` (`shell.rs:1368-1428`). A form-scoped Error re-enables the form with the message.

**Steps in execution.**

1. **Open** — cmd-N or the CTA; keyless goes to the key input.
2. **Fill** — Enter in field N focuses N+1; Enter on the last submits.
3. **Validate** — Empty singular or criteria, non-positive sats, cap below reward, bad rank: caret returns to the field.
4. **Publish DList** — PublishDList; the bounty half waits in pending_bounty.
5. **Create bounty** — On DListPublished: CreateBounty with the coordinate.
6. **Claim** — One field, Enter sends PublishClaim; a sticky toast explains the rank gate.

**Questions.**

- **Q-FM1** esc between PublishDList and DListPublished orphans the DList: published and logged, no bounty (shell.rs:1187, 692-693).
- **Q-FM2** autoPay is hardcoded true, so an issuer off the server allowlist gets 403 every time. Is a manual-pay bounty ever needed from this UI?
- **Q-FM3** An open, unsubmitted claim form retargets to whichever bounty you arrow to (shell.rs:1116-1127). Intended for a stage demo?

#### AF · Activity feed

**In one line.** Newest-first, capped at 200 rows, in memory only; dedup is the runtime's job, not the feed's.

**What it does.** Every notable Update writes one line: key imported, wallet ready, DList published, bounty created, claim submitted, each payment state change, each zap receipt, each error. Payment and receipt lines carry the server's timestamp, so a fact learned late never reads as instant.

**How it's built.** `push_activity` inserts at index 0 and truncates to 200 (`shell.rs:510-513`). Writers live in `apply` (`shell.rs:596-841`): PaymentState maps attempting, paid, settled, paid_unreceipted and failed to a dot colour and text using the claim's item name; ReceiptSeen writes "Zap receipt … landed N s after the claim". The feed never dedups: the runtime keeps one `WatchLedger` per bounty that outlives its watcher, so re-watching never replays (`nostr.rs:347-350`). The first watch of an old bounty this launch writes one line per claim with its current state, plus one per receipt, with their old timestamps.

**Steps in execution.**

1. **Write** — apply calls push_activity(dot, text, at).
2. **Timestamp** — PaymentState.ts and ReceiptSeen.at when present, else arrival time.
3. **Render** — Dashboard rows, or "Nothing yet — claims, payments, and receipts appear here live."
4. **Age** — A 30 s clock repaints so "N min ago" moves.

**Questions.**

- **Q-AF1** A relaunch loses the feed and re-selecting a bounty replays its history once more. Should it read server history?
- **Q-AF2** Rows always insert at index 0, so a late poll with an old server timestamp can appear above newer rows.

#### EV · Event builders

**In one line.** Builds kind-39998 DList headers and kind-39999 claims exactly like the server's dtag.js, and reads kind-9735 receipts.

**What it does.** A DList header is a replaceable event whose d-tag is the slug of the singular name; its coordinate ties the bounty to it. A claim names one item, carries the DList coordinate in a z tag, and holds the item name twice so old tooling reads it. A receipt is reduced to payer, claim ids and amount.

**How it's built.** `slug` (`events.rs:40-59`): lowercase, NFD, drop combining marks, keep ASCII letters and digits, collapse the rest to one hyphen. `hash8`: SHA-256, first 8 hex chars. Header: kind 39998, content "", tags `d`=slug(singular), `names`=[singular, plural], optional `description` (`events.rs:91-126`). Claim: kind 39999, content=name, tags `d`=slug(name)-hash8(coordinate), exactly one `z`=coordinate, `name` (`events.rs:135-158`). Receipt: the payer is the pubkey inside the `description` JSON, never the signer, which is the provider's zapper key; amount from the bolt11 invoice (`events.rs:200-234`). Tests pin the header d-tag, the coordinate and the receipt parse against the prod fixture; the fixture's claim was made by the agent CLI with its own d-tag scheme.

**Steps in execution.**

1. **slug** — "Café  du Monde!" becomes cafe-du-monde.
2. **hash8** — First 8 hex chars of SHA-256 of the coordinate.
3. **Header** — Kind 39998; coordinate 39998:pubkey:dtag.
4. **Claim** — Kind 39999; d-tag memphis-a0576ef1 for "Memphis" on the rehearsal DList.
5. **Receipt** — Kind 9735 only; e tags are claim ids, bolt11 gives msats.

**Questions.**

- ~~**Q-EV1** slug keeps only ASCII alphanumerics after NFD, so a Cyrillic or CJK name slugs to empty and claim() errors. Does dtag.js do the same?~~ ✓ Yes: dtag.js slugs them to empty too and returns "-" without complaint; the app refuses instead (magic-carpet-v2/src/lib/dtag.js:23-29, verified 2026-09-10).
- **Q-EV2** The live fixture claim uses d-tag mc-1786093454-7 and no name tag; the server keys on the z tag, so both schemes work. Will it stay that way?

#### BW · Bounty watcher and ledger

**In one line.** This app's one live watcher polls the bounty detail at 2, 10 or 30 s; a per-bounty ledger makes every payment fact reach the feed exactly once.

**What it does.** Selecting a bounty starts its watcher and stops any other. Each loop fetches the detail, sends the snapshot when it changed, and asks the ledger what is new: a payment state transition, a zap receipt. A receipt pushed by the relay only shortens the wait; the server's copy is the fact. The watch ends when the bounty is terminal with nothing pending.

**How it's built.** `watch_bounty` (`nostr.rs:1102-1261`): fresh keyless Api, GET /api/bounties/{id}; interval 2 s while a payment is pending, 10 s on an open auto-pay bounty, 30 s manual; errors back off 2 to 60 s, one Error { Watch } per distinct message. A claim with no payment row counts as pending for ROWLESS_GRACE_SECS (600) on an open auto-pay bounty; after that the poll just slows to 10 s, with no line anywhere. Subscription `Filter kind 9735 .events(all claim ids)` on the instance relay, re-issued when the claim set changes, never while the bounty has no claims. `WatchLedger { claim_created, last_payment, seen_receipts, relay_triggered }` (`nostr.rs:975-1084`) emits PaymentState when (state, reason) changed and ReceiptSeen once per receipt id; secs_from_claim is receipt.created_at minus claim.created_at. paid_unreceipted is not pending, but the watch stays alive while the bounty is open so a late settled flip is caught.

**Steps in execution.**

1. **Start** — WatchBounty: keep a live watcher for this id, else abort the others and spawn with the persistent ledger.
2. **Poll** — GET the detail; on change emit BountyDetail first.
3. **Ingest** — The ledger yields new PaymentState and ReceiptSeen facts.
4. **Pace** — Terminal and nothing pending: stop. Else sleep 2, 10 or 30 s.
5. **Subscribe** — Claim ids changed: resubscribe kind 9735 by #e.
6. **Wake early** — A verified pushed receipt the ledger accepts ends the sleep now.

**Questions.**

- **Q-BW1** Silent server gates (rank below min rank, daily cap) leave no row and no log line, and the app never says so: after 600 s the poll slows from 2 s to 10 s. Should the app preflight rank before claiming?
- **Q-BW2** After the watch ends, re-selecting a settled bounty does one poll and stops again. Enough for the receipt exhibit in the demo?
- **Q-BW3** nostr-sdk pushes an event only the first time the client sees it; a receipt already pushed under an earlier watcher is not re-pushed, so the poll is its only path.

#### IA · Instance API client

**In one line.** reqwest clients for the instance: one per account with a cookie jar for writes, throwaway keyless ones for reads; challenge login, bounties, the publish door, profile scans.

**What it does.** The issuer's and claimant's sessions never cross. The login signs a kind-22242 challenge and keeps the cookie; only creating a bounty needs it. Publishing an event is a plain POST with the signed event. Response types mirror what the server sends, including the payment row and the receipt per claim.

**How it's built.** Base `https://magic-carpet.brainstorm.world`, override `MC_BASE_URL`, 20 s timeout, user agent magic-carpet-chat/version (`api.rs:20-30, 373-388`). `login`: POST /api/auth/verify-user {pubkey}, sign kind 22242 with content "Tapestry authentication" and a challenge tag, POST /api/auth/login-user {event} (`api.rs:435-463`). `create_bounty`: POST /api/bounties, camelCase, unset flags omitted. `list_bounties`: GET /api/bounties?issuer=hex&status=all. `get_bounty`: GET /api/bounties/{id}. `publish_event`: POST /api/strfry/publish {event, signAs:"client"}; an HTTP 200 carrying success:false is `ApiError::Rejected`, and scan answers the same shape. `scan`: GET /api/strfry/scan?filter= for kind-0 lookups. Booleans stay raw because SQLite sends 0/1.

**Steps in execution.**

1. **Build** — cookie_store(true), 20 s timeout, base from env or the default.
2. **Login** — verify-user, sign 22242, login-user; cookie in the jar; once per session.
3. **Create** — POST /api/bounties with the cookie; the server takes the issuer from the session.
4. **Read** — list and detail are public GETs from throwaway clients.
5. **Publish** — POST /api/strfry/publish; success:false is still a failure.
6. **Scan** — A Nostr filter as a query string; the app uses it for profiles.

**Questions.**

- **Q-IA1** list_bounties and get_bounty bypass the read() helper: a non-2xx gives 200 chars of raw body instead of the server's error field (api.rs:494-498).
- ~~**Q-IA2** Claim.autoPayBlockedReason and paymentClaimEventId are parsed but absent from the fixture; does the server send them?~~ ✓ The current server sends paymentClaimEventId and claimAddress on every claim; autoPayBlockedReason only when the daily limit blocks a payable claim. The 2026-08-07 fixture predates them (magic-carpet-v2/src/api/bounties.js:118-133, 206-207, verified 2026-09-10).

### The instance and the money

#### IN · Magic Carpet instance

**In one line.** The server at magic-carpet.brainstorm.world: a REST API in front of a strfry relay, holding bounties, claims, payment rows and receipts.

**What it does.** The app reads and writes only here. Bounties and payment state come from GET /api/bounties. Events go in through POST /api/strfry/publish, which pipes them into strfry. Login is a signed challenge. The house issuer 853baa94… is the default whose bounties the app lists; an imported issuer key or MC_ISSUER_NPUB overrides it.

**How it's built.** Lives in the sibling repo magic-carpet-v2. The publish route runs the global auth middleware but is absent from its write list, so it falls through as read-only access; it checks only that id, sig and pubkey are present and never verifies the signature before `strfry import` (10 s timeout). verify-user mints the session cookie and accepts any well-formed pubkey; the real gate is on POST /api/bounties, where autoPay needs owner, admin or AUTO_PAY_ALLOWLIST_PUBKEYS (403 otherwise). Claims from keys ranked below 2 are hidden from claims[] unless they already hold a payment row. The detail endpoint embeds the auto_payments row and the validated kind-9735 per claim; the server checks the receipt's description pubkey against the issuer or its delegate, which this app cannot know. A kind-0 posted through the door is also fanned out to the server's own profile relays.

**Steps in execution.**

1. **Publish** — POST /api/strfry/publish: presence check, strfry import.
2. **Login** — verify-user mints the cookie and stores a challenge; login-user checks pubkey, challenge tag and Schnorr signature.
3. **Create** — POST /api/bounties: authed, normalized, allowlist-gated for auto-pay.
4. **List and detail** — Public GETs with derivedStatus, paymentState, claims[].autoPayment, claims[].zapReceipt.

**Questions.**

- ~~**Q-IN1** MC_BASE_URL and MC_RELAY_URL exist for rehearsals; does the repo-root .env set them?~~ ✓ The binary reads no .env: no dotenv crate, no read in src/. Overrides must be in the launching shell (Cargo.toml at HEAD, verified 2026-09-10).

#### RL · Instance relay

**In one line.** wss://magic-carpet.brainstorm.world/relay: the socket behind the rail dot and the kind-9735 receipt subscriptions, never a publish target.

**What it does.** The runtime holds one nostr-sdk client on it. Every 3 s it reads the real connection status and tells the window only when it changed. The bounty watcher subscribes here for receipts that name its claim ids, but a pushed receipt is only a reason to poll sooner.

**How it's built.** `INSTANCE_RELAY` (`nostr.rs:28`), override `MC_RELAY_URL`. `Command::Connect` calls `client.connect()` without waiting. `watch_relay_status` (`nostr.rs:951-973`) polls `relay.status() == Connected` and emits `RelayStatus(bool)` on change; the first poll always emits. No event is ever sent through this client; the throwaway client for public relays is separate.

**Steps in execution.**

1. **Add** — run() adds the relay once; a bad URL is Error { Runtime } and the loop continues.
2. **Connect** — Idempotent, returns at once.
3. **Poll** — Every 3 s: connected or not; emit on change.
4. **Subscribe** — Per watched bounty: kind 9735 with #e = claim ids.

**Questions.**

- **Q-RL1** The dot reflects this socket only; API reachability shows up separately as a failed list row.
- **Q-RL2** "Relay disconnected" has no retry button. Is reconnect fully automatic in nostr-sdk, or can the user get stuck?

#### AP · Auto-pay watcher (server)

**In one line.** The instance's own process that pays claims. It lives in magic-carpet-v2, not here; this app only watches its rows.

**What it does.** Every 30 s it lists payable claims, skips self-claims and anyone below the bounty's minimum rank, writes attempting, pays the claimant's Lightning address, writes paid, waits up to 60 s for a zap receipt, then writes settled or paid_unreceipted with reason receipt_timeout. A throw before the send writes failed; a missing Lightning address is one such throw, so it is not silent.

**How it's built.** `magic-carpet-v2/src/services/autoPayWatcher.js`: `runAutoPayTick` on AUTO_PAY_INTERVAL_MS; the rank gate is a bare `continue` with no row and no log line (463-464); the daily cap refuses inside insertAttemptingPayment with no row; no lud16 writes attempting then failed "claimant has no Lightning address" (114-147, 189-197); a throw after the invoice is persisted asks the wallet and lands as paid, failed, or failed with reason ambiguous_send. Every tick first turns attempting rows older than 600 s into failed (stuck_timeout). `payBolt11`, then `pollForReceipt` in 5 s steps bridging AUTO_PAY_ZAP_RELAYS. The receipt is signed by the claimant's provider (Strike's zapper for Matthias); the payer inside its description is the delegate, the key the server zaps from on the issuer's behalf. Wallet float and the 5,000 sat daily limit are on the server.

**Steps in execution.**

1. **Tick** — Every 30 s, list payable claims.
2. **Gate** — Self-claim or below min rank: continue, silently. Daily cap: refused, no row.
3. **Attempt** — Row state attempting; no lud16 fails here with a reason.
4. **Pay** — Send sats to the lud16; row paid.
5. **Receipt** — Wait 60 s; settled, or paid_unreceipted (receipt_timeout).

**Questions.**

- **Q-AP1** Two server gates: below rank 2 the claim is hidden, between 2 and min rank it is listed but never paid. Should the app show which gate applies?
- **Q-AP2** The project notes list a missing lud16 among the silent no-row gates; the watcher code writes a failed row for it. Which is current on prod?

#### PR · Public relays

**In one line.** relay.damus.io, nos.lol and relay.primal.net: written once, when a Coinos address is added to the profile, because payers read them.

**What it does.** Strike, Primal and the server's payer look up a Lightning address on the public relays, so the merged kind-0 must land there. It goes to the instance first so this app's own kind-0 probe, which reads only the instance, sees the address at once. One public acceptance is enough.

**How it's built.** `PUBLIC_RELAYS` (`nostr.rs:35-39`). `publish_lud16` (`nostr.rs:649-689`) uses a throwaway `Client::default()`: add the three relays, `connect().and_wait(10 s)`, `send_event`, disconnect; Ok only if `output.success` is non-empty. A failure after the instance accepted travels inside `WalletCreated.publish_error` and the panel offers Retry.

**Steps in execution.**

1. **Instance first** — POST /api/strfry/publish; a refusal stops here.
2. **Connect** — Throwaway client, wait up to 10 s.
3. **Send** — The same signed kind-0.
4. **Judge** — Any one relay accepted: success.

**Questions.**

- **Q-PR1** publish_lud16 reads the existing kind-0 from the instance only; a newer profile on the public relays gets replaced by the instance copy plus lud16.
- **Q-PR2** Instance and public relays are written in sequence; a public failure leaves them disagreeing until Retry.
- **Q-PR3** The source disagrees with itself about which one the prod payer reads: nostr.rs:30-34 says the public relays, nostr.rs:667 says the instance. Which is true?

#### BS · Brainstorm (brainstorm.world)

**In one line.** The service that owns trust: the setup API at api.brainstorm.world, the scores relay at wss://scores.brainstorm.world, and the profile pages the Account screen links to.

**What it does.** GET https://api.brainstorm.world/setup/{hex} answers a JSON array of [type, key, relay] triples — the account's current Brainstorm keys, one per score type. wss://scores.brainstorm.world holds kind-30382 Trusted Assertions, addressable by the subject pubkey in the d tag. The app reads both; it never writes to either.

**How it's built.** `fetch_brainstorm_key` (`nostr.rs:1405-1435`) builds its own reqwest client (USER_AGENT, 10 s); `trust::parse_setup` takes the `30382:rank` triple as the rank provider: Assigned { key, relay }. HTTP 404 means the account has never set up at Brainstorm → NoAccount; any other non-2xx or an unparseable body is Err, so a broken endpoint reads as "couldn't check", never as "no account". Kind-30382 reads go to whichever relay the winning Treasure Map row names for its provider, queried with `Filter kind 30382 .identifier(subject)`; the fetched event must be authored by the provider and carry a rank, and the signature is verified.

**Steps in execution.**

1. **Setup** — GET api.brainstorm.world/setup/{hex}: the [type, key, relay] triple whose type is 30382:rank is the provider; 404 is NoAccount.
2. **Assertion** — kind 30382 on the provider relay, #d = subject, authored by the provider.

### Keys and wallet

#### KS · Key store

**In one line.** The macOS keychain, service magic-carpet-chat: two nsecs and the Coinos login. Env vars win; debug builds use plain files.

**What it does.** Entries issuer-nsec, claimant-nsec, issuer-coinos-login, claimant-coinos-login. Reads check MC_ISSUER_NSEC and MC_CLAIMANT_NSEC first. Secrets leave the module as nostr Keys, whose Debug prints only the public key, or inside Secret, which prints redacted. The --import-keys flag copies env keys into the keychain and prints only npubs.

**How it's built.** `load_nsec` (`secrets.rs:260-278`): env trimmed and non-empty, else `keyring::Entry(SERVICE, entry)`. `parse_secret` rejects an npub with "that's your public key" and anything not nsec1 or 64 hex; error text never echoes the input. `DEV_STORE = cfg!(debug_assertions)` swaps the keychain for 0600 files under ~/Library/Application Support/magic-carpet-chat/dev-secrets/, because every unsigned debug binary is a new app to macOS (`secrets.rs:139-160`). `forget_nsec` deletes the entry only. `store_available` is fatal for --import-keys and a stderr warning at launch. With a valid env override set, a paste is still written but stays inert: the env key is announced and signs.

**Steps in execution.**

1. **Resolve** — Env first, else the keychain entry.
2. **Validate** — npub: PublicKeyPasted; not a key: NotAKey; else Keys::parse.
3. **Store** — Trimmed text under the entry; a 0600 file in debug builds.
4. **Forget** — Delete the entry; the env override is untouched.
5. **CLI** — --import-keys: store both env keys, print issuer: npub… and claimant: npub….

**Questions.**

- **Q-KS1** An invalid MC_*_NSEC poisons the account: every publish fails with the env error even after a good paste, and onboarding can open with it still in force (nostr.rs:500, 784-791, 861-864). Validate at launch?
- **Q-KS2** InvalidKey always names the env var, even when the bad value came from the keychain (secrets.rs:277).
- **Q-KS3** A key imported with a debug build is invisible to the release .app, and the reverse.

#### CO · Coinos wallet flow

**In one line.** One click opens a hosted Coinos account bound to the claimant key: keychain first, then register, then the address into the profile.

**What it does.** The app invents a username (carpet plus 8 random characters) and a 32-character password, writes them to the keychain before any network call, and registers. Coinos serves a Lightning address at once. The app then merges it into the key's profile and publishes. A retry reuses the saved login and asks Coinos whether the wallet exists before registering again.

**How it's built.** `create_coinos_wallet` (`nostr.rs:701-773`): session, stored login, `Login::reuse_or_fresh` (a login for another pubkey is replaced), `store_coinos_login`, probe `GET /.well-known/lnurlp/user` only on reuse, `POST https://coinos.io/api/register {user:{username,password,pubkey}}` with a 30 s timeout. The reply token is shape-checked and dropped, so the app never shows a balance. Only `CoinosError::Refused` (any non-2xx) deletes the login; transport errors and a 2xx without a token keep it. Then `publish_lud16`, and `WalletCreated { username, lightning_address, publish_error }` always, `ProfileLoaded` on success. Claimant only: `has_local_wallet` is false for the issuer.

**Steps in execution.**

1. **Session** — Keys and Api for the claimant; no key is WalletFailed.
2. **Saved login** — Reuse if the pubkey matches; unreadable JSON stops with the entry name.
3. **Store first** — Write the fresh login before any HTTP.
4. **Probe on retry** — lnurlp: 404 register again, payRequest skip, anything else stops with WalletFailed.
5. **Register** — POST /api/register; a refusal deletes the login.
6. **Publish** — Merge lud16 into the kind-0, instance then public relays.
7. **Report** — WalletCreated, then ProfileLoaded.

**Questions.**

- **Q-CO1** Any non-2xx from /api/register counts as Refused and deletes the login; a proxy 502 after Coinos created the account strands a real password (coinos.rs:169-172).
- **Q-CO2** A login stored for a different pubkey is silently overwritten and the old wallet's password is lost. → _the in-flight refactor, which binds the login to the account record_
- **Q-CO3** Forget this key leaves the Coinos login in place. Intended, so a re-import finds its wallet?

#### CX · Coinos (coinos.io)

**In one line.** The hosted Lightning wallet. Two endpoints are ever called: register, and the lnurlp probe. No login, no balance, no sends.

**What it does.** Coinos holds the sats, so balances should stay small. Funding works because the lnurlp endpoint is live the moment the account exists; the QR on the Wallet screen points at it. The password is in the keychain; the app never calls /login, which runs a captcha.

**How it's built.** `coinos.rs`: BASE https://coinos.io, one static reqwest client, 30 s timeout, on the runtime thread. `create_wallet` POSTs /api/register: a non-2xx is Refused, a 2xx without a token is an Http error. `wallet_exists` GETs /.well-known/lnurlp/user: 404 means no account, tag payRequest means it exists, anything else is an error. `lnurl_pay` bech32-encodes the lnurlp URL with the original checksum, uppercase, per LUD-01. Usernames drop l, o, 0 and 1; Coinos lowercases them itself.

**Steps in execution.**

1. **Register** — POST /api/register {user:{…}}: 2xx with token; non-2xx is Refused; 2xx without a token is Http.
2. **Probe** — GET /.well-known/lnurlp/user on retry.
3. **Pay endpoint** — What every Lightning wallet hits when it scans the QR.

**Questions.**

- **Q-CX1** The register body shape and the 2-24 lowercase rule are asserted from coinos-server source with no live contract test.

### Chat, two ways

#### AN · Anthropic Messages API

**In one line.** POST https://api.anthropic.com/v1/messages with stream:true, answered as Server-Sent Events. Both chat paths call it the same way.

**What it does.** Model claude-sonnet-5, max_tokens 64000, header anthropic-version 2023-06-01, key in x-api-key. The body is the transcript as alternating user and assistant messages and nothing else: no system prompt, no tools, no temperature.

**How it's built.** Desktop: `chat.rs:300-307` builds the request on gpui's HttpClient, reads only lines prefixed `data: `, and acts on content_block_delta, message_delta with stop_reason max_tokens, and error; message_stop is ignored and the stream ends when the connection closes. Worker: `worker/src/index.ts:159-176` does the same with `fetch` and copies the bytes through byte for byte. The constants are duplicated, not shared.

**Steps in execution.**

1. **Request** — POST with model, max_tokens, stream, messages.
2. **Events** — message_start, content_block_delta, message_delta, message_stop, ping, error.
3. **Errors** — Every error body is cut at 300 chars; 401 and 429 get their own wording.

#### CC · Chat cell (worker)

**In one line.** One Durable Object per conversation id on celld, a self-hosted Workers runtime on the nashauto-git box: one turn at a time, Anthropic upstream, the reply streamed back and written to SQLite as it arrives.

**What it does.** A tailnet client POSTs /chat/:id with a message and gets Anthropic's own SSE back; GET /chat/:id returns the transcript. A second POST while a reply streams gets 409. A reply that never finished stays on disk flagged incomplete. There is no authentication: the tailnet is the perimeter. The desktop app does not call it.

**How it's built.** `worker/src/index.ts`: table `messages(id, role, content, created_at, complete)`, migrated in the constructor. Lock: `inFlight`, `progressAt`, a `turn` counter; 409 only if in flight and progressed within STALE_TURN_MS (30 s), because a client hangup kills the relay mid-await and no finally runs. Orphan user row deleted, user row inserted, upstream POST, `relay()` copies bytes and saves on the first delta then every 16 deltas or 250 ms; complete=1 only after message_stop; no text at all rolls the user row back. Deploy: `deploy.sh` substitutes the key into a temp config and writes to the chat prefix of an S3 bucket that also holds dgit, the private git forge; a deploy to the bucket root would break every hosted repository. The box runs `celld-chat.service` on 127.0.0.1:8081 behind Tailscale port 10000. Never rename the script: Durable Object data is bound to it.

**Steps in execution.**

1. **Route** — /chat/:id, 1-128 safe chars; idFromName picks the cell.
2. **Lock** — 409 if a fresh turn is in flight; else take the cell.
3. **Insert** — Drop a trailing orphan user row, insert this one.
4. **Upstream** — POST the whole history; a thrown fetch is 502, an upstream HTTP error passes through; both delete the row.
5. **Relay** — Copy bytes to the client; save fragments with complete=0.
6. **Finish** — complete=1 on message_stop; the current turn releases the cell.
7. **Deploy** — celld deploy to the chat prefix, then restart celld-chat on the box, never celld.service.

**Questions.**

- **Q-CC1** complete=0 fragments are fed back to the model as full assistant turns on the next POST (index.ts:174-175).
- **Q-CC2** Any tailnet member can read or append to any conversation by guessing its id.
- **Q-CC3** Is celld-chat running on the box today? The repo holds no deploy record.
- **Q-CC4** The key sits in cleartext in the bundle metadata in S3; who else can read that bucket?

### Under the floor

#### PK · Plumbing and packaging

**In one line.** gpui-kit 0.6.0, the palette from the design mock, eleven copied SVG icons, UTC time formatting, and an unsigned .app bundle.

**What it does.** The whole UI is one crates.io dependency plus the HTTP client it does not re-export. Every colour is a hex value copied from the mock; only gpui-component's own controls are repainted through the theme. Times print in UTC. make-app.sh wraps the release binary and writes a READ ME FIRST for Gatekeeper.

**How it's built.** `Cargo.toml`: gpui-kit 0.6.0, gpui-pre-reqwest-client 0.3.4, nostr-sdk 0.45, tokio, reqwest with rustls, keyring 4, lightning-invoice, bech32, qrcode without default features, sha2, unicode-normalization. `palette::apply` overwrites fifteen theme colours and two radii after `Theme::change(Dark)`. `icons::Assets` serves `icons/mc/name.svg` from an include_bytes table and falls through to gpui-kit's lucide set. `timefmt`: HH:MM:SS UTC, relative ages, thousands-grouped sats. `scripts/make-app.sh`: bundle id world.MagicCarpet.magic-carpet-chat, macOS 12 minimum, unsigned; both dist/ and dist-live/ hold a bundle. Thirty-nine unit tests across seven modules; the worker has only tsc.

**Steps in execution.**

1. **Init order** — gpui_kit::init, Theme Dark, palette::apply, bind_keys, http client.
2. **Icons** — Screen::icon names map to icons/mc/*.svg; the colour is passed in.
3. **Bundle** — install the binary and icon.icns, write Info.plist and PkgInfo, write READ ME FIRST.txt.
4. **Ship** — ditto zip plus the txt at the zip root.

**Questions.**

- **Q-PK1** make-app.sh says the bundle id must match src/config.rs and a directories crate; neither exists. Which should the id track?
- **Q-PK2** README.md still describes a 400-line chat toy with MODEL in main.rs; DEMO.md omits the ~/.config/anthropic/key fallback. Rewrite both?
- **Q-PK3** aws-lc-rs compiles in although ring is the provider; reqwest's rustls-tls feature pulls it too, so a gpui-kit feature alone cannot drop it.

### Not built, or moving (designed for, not built)

#### UB · Unbuilt screens _(not switched on)_

**In one line.** Payments, Claimants, Leaderboards, Tags and Settings render "This screen is not built yet." The search field looks live and is not; so do four of the Account page's five tabs.

**What it does.** Five of the twelve Screen variants are placeholders. cmd-3, 4, 5 and 7 reach four of them; Settings is a rail button with no shortcut. Accounts is built (AC), and on the Account page only the Trust tab has real data — Identity & keys, Progress, Wallet and Limits & defaults say "Not built yet." The placeholders exist so the nav matches the design mock.

**How it's built.** `Screen` (`shell.rs:36-49`); `placeholder()` at `shell.rs:1944-1962`; the search InputState at `shell.rs:456-458, 1904`; the four tab stubs in `account.rs`.

**Steps in execution.**

1. **Navigate** — cmd-3 Payments, cmd-4 Claimants, cmd-5 Leaderboards, cmd-7 Tags; rail button for Settings.
2. **Render** — Label plus the not-built line.

**Questions.**

- ~~**Q-UB1** Do Accounts and Settings stay placeholders for the demo, or does the refactor's account setup take the Accounts slot?~~ ✓ Accounts is built now: account cards plus the per-account page with the Trust tab (src/account.rs, this working tree). Settings is still a placeholder.

#### NA · N-account store (in flight) _(not switched on)_

**In one line.** Uncommitted work in the working tree, written by another session on 2026-09-10: accounts.json replaces the keychain and N accounts replace the two slots.

**What it does.** Direction of travel, read at 22:20 local: one accounts.json under Application Support (mode 0600, atomic rename, MC_STORE_DIR override) holds every nsec and Coinos login; accounts are keyed by pubkey with an active flag; an Account setup screen with create, paste and browse paths replaces Onboarding and is also reachable from a plus in the rail; creating an account always opens a Coinos wallet; the Coinos token is kept and GET /api/me shows a balance on a 3 s poll; MC_NSECS imports a comma-separated list at every launch; the issuer for reads is MC_ISSUER_NPUB or the house key, never a stored key.

**How it's built.** Working tree only: `src/account_setup.rs`, `src/secrets.rs` with `Store { active, accounts }`, `Command::AddAccount / RemoveAccount / SetActive / FetchBalance`, `Update::AccountsLoaded / AccountAdded / AddAccountFailed / Balance`, `ErrorSource::Wallet`, `merge_lud16(existing, lud16, name)`, keyring removed from Cargo.toml, shell.rs rewritten to match. Every structure in this atlas keyed on Issuer/Claimant, the keychain entries, and the env override rule changes when it lands.

**Steps in execution.**

1. **Store** — accounts.json, 0600, written to .tmp then renamed under a lock.
2. **Add** — Dedupe by pubkey, make active, open a wallet in the same step.
3. **Remove** — Drop nsec and Coinos login together; fall back to the first remaining account.
4. **Balance** — GET https://coinos.io/api/me with the kept token, every 3 s while the panel shows.

**Questions.**

- **Q-NA1** Which revision should the atlas track once the refactor lands: HEAD, the refactor, or both with a moving marker? → _rebuild after the refactor commit_

## Flows (representative packets)

Payload shapes are illustrative; the ids and the 27 s timing come from the 2026-08-07 rehearsal fixture (tests/fixtures/bounty-5f44688e.json).

### Claim, payment, receipt

| # | From → To | Packet | Representative payload |
|---|---|---|---|
| 1 | NR → SH | cmd-] then cmd-N | `{"account":"Claimant","action":"NewItem"}` |
| 2 | SH → FM | open claim form | `{"bounty_id":"5f44688e…","coordinate":"39998:853baa94…:cities-tennessee-rehearsal"}` |
| 3 | FM → SH | Enter | `{"name":"Memphis"}` |
| 4 | SH → BR | Command::PublishClaim | `{"account":"Claimant","bounty_id":"5f44688e…","name":"Memphis"}` |
| 5 | BR → RT | dispatch | `{"spawn":"publish_claim"}` |
| 6 | RT → KS | read claimant nsec | `{"env":"MC_CLAIMANT_NSEC","entry":"claimant-nsec","value":"<redacted>"}` |
| 7 | RT → EV | build kind 39999 | `{"kind":39999,"content":"Memphis","tags":[["d","memphis-a0576ef1"],["z","39998:…:cities-tennessee-rehearsal"],["name","Memphis"]]}` |
| 8 | RT → IA | publish_event | `{"signAs":"client"}` |
| 9 | IA → IN | POST /api/strfry/publish | `{"event":"<signed 39999>","signAs":"client"}` |
| 10 | IN → IA | 200 success | `{"success":true}` |
| 11 | RT → BR | Update::ClaimPublished | `{"bounty_id":"5f44688e…","event_id":"afb4bfe5…"}` |
| 12 | BR → SH | apply | `{"feed":"Claim afb4bfe5… submitted","toast":"Claim published — auto-pay bounty"}` |
| 13 | SH → BR | Command::WatchBounty | `{"id":"5f44688e…"}` |
| 14 | BR → RT | dispatch | `{"watcher":"one live at a time"}` |
| 15 | RT → BW | spawn watch_bounty | `{"ledger":"reused or new"}` |
| 16 | BW → IA | get_bounty every 2 s | `{"GET":"/api/bounties/5f44688e…"}` |
| 17 | IA → IN | GET detail | `{}` |
| 18 | IN → IA | snapshot | `{"claims":1,"autoPayment":null}` |
| 19 | IA → BW | ingest | `{"pending":"rowless, within 600 s"}` |
| 20 | BW → RL | subscribe kind 9735 #e | `{"kinds":[9735],"#e":["afb4bfe5…"]}` |
| 21 | AP → IN | pays the claim (server) | `{"state":"attempting → paid","sats":100}` |
| 22 | IN → IA | snapshot | `{"autoPayment":{"state":"paid"},"zapReceipt":null}` |
| 23 | AP → IN | receipt seen: settled | `{"state":"settled","zapReceipt":"b3b3f887…"}` |
| 24 | RL → BW | receipt pushed: poll now | `{"kind":9735,"effect":"early poll, never a fact"}` |
| 25 | BW → BR | PaymentState, ReceiptSeen | `{"state":"settled","receipt_id":"b3b3f887…","secs_from_claim":27}` |
| 26 | BR → SH | apply | `{"details":"updated","feed":"Zap receipt b3b3f887… landed 27 s after the claim"}` |
| 27 | SH → BO | claim card | `{"rows":["Claim submitted","Auto-pay attempted","Payment settled","Zap receipt · 100 sats · 27 s after the claim"]}` |

### Launch

| # | From → To | Packet | Representative payload |
|---|---|---|---|
| 1 | SH → RT | spawn nostr-runtime | `{"thread":"nostr-runtime","workers":2,"tls":"ring"}` |
| 2 | SH → BR | Connect, LoadAccounts, FetchBounties | `{"commands":3}` |
| 3 | BR → RT | dispatch | `{}` |
| 4 | RT → RL | connect | `{"relay":"wss://magic-carpet.brainstorm.world/relay"}` |
| 5 | RT → KS | read both keys | `{"order":["Issuer","Claimant"],"env_first":true}` |
| 6 | RT → IA | scan kind 0 | `{"filter":{"kinds":[0],"authors":["<pubkey>"],"limit":1}}` |
| 7 | IA → IN | GET /api/strfry/scan | `{}` |
| 8 | IN → IA | kind 0 | `{"name":"Matthias","lud16":"m_f_debern@strike.me"}` |
| 9 | RT → BR | AccountLoaded, ProfileLoaded | `{"npub":"npub1su7gycf…","name":"Matthias","lud16":"m_f_debern@strike.me"}` |
| 10 | RT → IA | list_bounties | `{"issuer":"853baa94…","status":"all"}` |
| 11 | IA → IN | GET /api/bounties | `{}` |
| 12 | IN → IA | bounties | `{"count":4}` |
| 13 | RT → BR | Update::Bounties | `{"count":4,"side_effect":"select and watch the first row"}` |
| 14 | BR → SH | apply | `{}` |
| 15 | RL → RT | status poll, 3 s | `{"connected":true}` |
| 16 | RT → BR | RelayStatus(true) | `{}` |
| 17 | BR → SH | green dot | `{"relay_connected":true}` |
| 18 | SH → DB | paint | `{"summary":"2 of 2 account keys · watching magic-carpet.brainstorm.world","attention":"All clear"}` |

### Issuer publishes

| # | From → To | Packet | Representative payload |
|---|---|---|---|
| 1 | NR → SH | cmd-N | `{"account":"Issuer"}` |
| 2 | SH → FM | seven fields | `{"singular":"US City","plural":"US Cities","reward":100,"cap":400,"min_rank":2}` |
| 3 | FM → SH | Enter on the last field | `{"validated":true,"parked":"pending_bounty"}` |
| 4 | SH → BR | Command::PublishDList | `{"singular":"US City","plural":"US Cities"}` |
| 5 | BR → RT | dispatch | `{}` |
| 6 | RT → EV | build kind 39998 | `{"kind":39998,"tags":[["d","us-city"],["names","US City","US Cities"]]}` |
| 7 | RT → IA | publish_event | `{}` |
| 8 | IA → IN | POST /api/strfry/publish | `{"event":"<signed 39998>"}` |
| 9 | IN → IA | 200 success | `{"success":true}` |
| 10 | RT → BR | Update::DListPublished | `{"coordinate":"39998:853baa94…:us-city"}` |
| 11 | BR → SH | apply: feed, then CreateBounty | `{"feed":"Published list 39998:853baa94…:us-city"}` |
| 12 | SH → BR | Command::CreateBounty | `{"listCoordinate":"39998:…:us-city","amountSats":100,"bountyCapSats":400,"autoPay":true,"autoPayMinRank":2}` |
| 13 | BR → RT | dispatch | `{}` |
| 14 | RT → IA | login once, then create | `{"login":"kind 22242 \"Tapestry authentication\""}` |
| 15 | IA → IN | POST /api/bounties | `{"cookie":"session","autoPay":true}` |
| 16 | IN → IA | bounty | `{"success":true,"bounty":{"id":"<uuid>","auto_pay":1}}` |
| 17 | RT → BR | Update::BountyCreated | `{"id":"<uuid>"}` |
| 18 | BR → SH | apply | `{"feed":"Bounty … created — watching for claims","optimistic":true}` |
| 19 | SH → BO | select + Auto-pay armed | `{"pill":"Auto-pay armed"}` |

### Coinos wallet

| # | From → To | Packet | Representative payload |
|---|---|---|---|
| 1 | WA → SH | Create a Coinos wallet | `{"button":"wallet-create"}` |
| 2 | SH → BR | Command::CreateCoinosWallet | `{"account":"Claimant"}` |
| 3 | BR → RT | dispatch | `{}` |
| 4 | RT → KS | write login first | `{"entry":"claimant-coinos-login","username":"carpetab29cd34","password":"<redacted>"}` |
| 5 | RT → CX | POST /api/register | `{"user":{"username":"carpetab29cd34","password":"<redacted>","pubkey":"873c8261…"}}` |
| 6 | CX → RT | 2xx with token | `{"token":"checked, dropped"}` |
| 7 | RT → IA | scan kind 0 | `{"filter":{"kinds":[0]}}` |
| 8 | IA → IN | GET /api/strfry/scan | `{}` |
| 9 | IN → IA | existing profile | `{"name":"Matthias","about":"…"}` |
| 10 | RT → IA | merged, signed kind 0 | `{"lud16":"carpetab29cd34@coinos.io","other_fields":"untouched"}` |
| 11 | IA → IN | POST /api/strfry/publish | `{"event":"<signed kind 0>"}` |
| 12 | RT → PR | send the same kind 0 | `{"relays":["relay.damus.io","nos.lol","relay.primal.net"],"wait":"10 s"}` |
| 13 | RT → BR | WalletCreated, ProfileLoaded | `{"lightning_address":"carpetab29cd34@coinos.io","publish_error":null}` |
| 14 | BR → SH | apply | `{"feed":"Coinos wallet ready: carpetab29cd34@coinos.io"}` |
| 15 | SH → WA | QR | `{"qr":"lightning:LNURL1…"}` |

### Chat turn, both ways

| # | From → To | Packet | Representative payload |
|---|---|---|---|
| 1 | CP → AN | POST /v1/messages | `{"model":"claude-sonnet-5","max_tokens":64000,"stream":true,"messages":"[whole transcript]"}` |
| 2 | AN → CP | SSE deltas | `{"type":"content_block_delta","delta":{"text":"Hel"}}` |
| 3 | CC → AN | POST /v1/messages, after a tailnet POST /chat/:id | `{"messages":"[SQLite history]"}` |
| 4 | AN → CC | SSE, copied through | `{"save":"first delta, then every 16 deltas or 250 ms","complete":0}` |

### First launch keys

| # | From → To | Packet | Representative payload |
|---|---|---|---|
| 1 | OB → SH | Enter with an nsec | `{"secret":"<redacted>"}` |
| 2 | SH → BR | Command::ImportKey | `{"account":"Claimant","secret":"Secret(<redacted>)"}` |
| 3 | BR → RT | dispatch | `{}` |
| 4 | RT → KS | store nsec | `{"entry":"claimant-nsec"}` |
| 5 | RT → IA | scan kind 0 | `{}` |
| 6 | IA → IN | GET /api/strfry/scan | `{}` |
| 7 | IN → IA | kind 0 | `{"name":"Matthias"}` |
| 8 | RT → BR | AccountLoaded, ProfileLoaded, ImportReady | `{"npub":"npub1…","profile_checked":true,"profile_found":true}` |
| 9 | BR → SH | apply | `{"feed":"Imported the claimant key (npub1…)"}` |
| 10 | SH → OB | ready panel | `{"name":"Matthias","lud16":"m_f_debern@strike.me"}` |
| 11 | OB → SH | Enter: Start | `{"screen":"Dashboard","account":"Claimant"}` |

### Trust fetch for one account

| # | From → To | Packet | Representative payload |
|---|---|---|---|
| 1 | DB → SH | "Fix →" on a trust row | `{"pubkey":"<account>","action":"OpenTrust"}` |
| 2 | SH → BR | Command::FetchTrust | `{"pubkey":"<account>","generation":"n+1 (begin marks parts Loading)"}` |
| 3 | BR → RT | dispatch | `{"spawn":"fetch_trust, throwaway client"}` |
| 4 | RT → PR | kind-10040 and kind-3 per relay | `{"relays":"outbox (kind 10002) ∪ TRUST_FALLBACK_RELAYS","exit":"EOSE or 10 s","verify":"sig, author, tags"}` |
| 5 | RT → BS | GET api.brainstorm.world/setup/{hex} | `{"body":"[[type, key, relay], …]","rank":"the 30382:rank triple; 404 = NoAccount"}` |
| 6 | RT → BS | kind 30382 on the provider relay | `{"kinds":[30382],"#d":"<pubkey>","author":"provider from the map row"}` |
| 7 | RT → BS | kind 30382 for the instance issuer | `{"subject":"issuer_pubkey()","provider":"cached per launch"}` |
| 8 | RT → BR | Update::Trust ×5 parts | `{"parts":["Designation","Follows","Brainstorm","OwnAssertion","InstanceAssertion"]}` |
| 9 | BR → SH | apply: drop stale generation | `{"failed":"reread keeps prior answer"}` |
| 10 | SH → AC | Trust tab cards | `{"cards":["Treasure Map","Trusted Assertions","Minimum rank","Trust Determination","Web of trust"]}` |
| 11 | SH → DB | attention rows | `{"rows":["Publish your Treasure Map","Follow someone","Gain a follower","Couldn’t check your Treasure Map"]}` |

## Questions — index

Reference by ID. ✓ resolved (with date) · → routed to a named next step · otherwise open.

- **Q-OB1** (OB) Esc while "Checking the key…": the key is stored and the account row updates, but the ready panel (address check, wallet offer) is lost (shell.rs:596-623).
- **Q-OB2** (OB) The kind-0 probe reads the instance only; a profile that lives on public relays alone shows the wrong-key warning and a wallet offer.
- **Q-OB3** (OB) The ready panel says "Key saved to this Mac's keychain" in debug builds too, where the key is a plain file.
- **Q-DB1** (DB) A list failure after a good load only adds a red feed line; the stale list stays and "Needs attention" stays all-clear (shell.rs:670-676).
- **Q-DB2** (DB) Absolute times are UTC, so a Nashville audience reads 16:13 for an 11:13 event (timefmt.rs:3-5).
- **Q-BO1** (BO) A claim with no created_at gets timestamp 0: a 1970 time and a huge receipt delta (bounties.rs:242).
- **Q-BO2** (BO) Up and down do nothing off this screen, while the list is loading or failed, or when it is empty (shell.rs:1129-1139).
- **Q-BO3** (BO) The card ignores Claim.payment_status and payment_amount_sats; is autoPayment the only source of truth?
- **Q-NR1** (NR) While a text field has focus, cmd-[ and cmd-] indent instead of switching accounts; the binding is gpui-kit's (shell.rs:864-866). Rebind, or document only?
- **Q-NR2** (NR) The top-bar search field reads "Search lists, bounties, profiles…" and has no handler (shell.rs:456-458, 1904). Hide it until built?
- **Q-WA1** (WA) The QR encodes only the LNURL (LUD-01); wallets that prefer LUD-16 text must decode it. Intended?
- **Q-WA2** (WA) Retry re-runs the whole command including the Coinos probe, so a Coinos outage blocks a pure relay retry (nostr.rs:740-745).
- **Q-WA3** (WA) After a relaunch the username is gone from the panel; only the address remains. Fine?
- **Q-CP1** (CP) Error bubbles, including the "No API key" notice, are resent to the model as assistant turns on the next Enter, because only empty messages are filtered (chat.rs:139).
- **Q-CP2** (CP) No cancel while streaming; the only way out is to close the window (chat.rs:91, 152).
- **Q-CP3** (CP) A mid-stream error overwrites the text already streamed (chat.rs:172). Keep the partial reply?
- **Q-CP4** (CP) message_stop is never checked; a reply is "done" when the connection closes, so a graceful cut-off looks finished (chat.rs:351).
- **Q-CP5** (CP) The whole transcript is resent every turn with no cap and no caching; cost grows linearly.
- **Q-SH1** (SH) Command::Shutdown exists but nothing sends it; the runtime thread dies with the process. Intentional?
- **Q-SH2** (SH) Every unbounded_send drops its Result; if the runtime thread died, a submit does nothing beyond the earlier Error { Runtime } line.
- **Q-BR1** (BR) Both channels are unbounded; a window that stops draining never applies backpressure. Fine for one window?
- **Q-RT1** (RT) spawn_runtime reports a thread or tokio build failure as Error { Runtime }, but the handle then has no live receiver. Does the UI treat that as fatal?
- **Q-RT2** (RT) No 401 handling resets Session.logged_in; only re-pasting the key evicts the session, so an expired cookie turns CreateBounty into 401 until then (nostr.rs:535-538, 868).
- **Q-FM1** (FM) esc between PublishDList and DListPublished orphans the DList: published and logged, no bounty (shell.rs:1187, 692-693).
- **Q-FM2** (FM) autoPay is hardcoded true, so an issuer off the server allowlist gets 403 every time. Is a manual-pay bounty ever needed from this UI?
- **Q-FM3** (FM) An open, unsubmitted claim form retargets to whichever bounty you arrow to (shell.rs:1116-1127). Intended for a stage demo?
- **Q-AF1** (AF) A relaunch loses the feed and re-selecting a bounty replays its history once more. Should it read server history?
- **Q-AF2** (AF) Rows always insert at index 0, so a late poll with an old server timestamp can appear above newer rows.
- ~~**Q-EV1**~~ (EV) ✓ Yes: dtag.js slugs them to empty too and returns "-" without complaint; the app refuses instead (magic-carpet-v2/src/lib/dtag.js:23-29, verified 2026-09-10).
- **Q-EV2** (EV) The live fixture claim uses d-tag mc-1786093454-7 and no name tag; the server keys on the z tag, so both schemes work. Will it stay that way?
- **Q-BW1** (BW) Silent server gates (rank below min rank, daily cap) leave no row and no log line, and the app never says so: after 600 s the poll slows from 2 s to 10 s. Should the app preflight rank before claiming?
- **Q-BW2** (BW) After the watch ends, re-selecting a settled bounty does one poll and stops again. Enough for the receipt exhibit in the demo?
- **Q-BW3** (BW) nostr-sdk pushes an event only the first time the client sees it; a receipt already pushed under an earlier watcher is not re-pushed, so the poll is its only path.
- **Q-IA1** (IA) list_bounties and get_bounty bypass the read() helper: a non-2xx gives 200 chars of raw body instead of the server's error field (api.rs:494-498).
- ~~**Q-IA2**~~ (IA) ✓ The current server sends paymentClaimEventId and claimAddress on every claim; autoPayBlockedReason only when the daily limit blocks a payable claim. The 2026-08-07 fixture predates them (magic-carpet-v2/src/api/bounties.js:118-133, 206-207, verified 2026-09-10).
- ~~**Q-IN1**~~ (IN) ✓ The binary reads no .env: no dotenv crate, no read in src/. Overrides must be in the launching shell (Cargo.toml at HEAD, verified 2026-09-10).
- **Q-RL1** (RL) The dot reflects this socket only; API reachability shows up separately as a failed list row.
- **Q-RL2** (RL) "Relay disconnected" has no retry button. Is reconnect fully automatic in nostr-sdk, or can the user get stuck?
- **Q-AP1** (AP) Two server gates: below rank 2 the claim is hidden, between 2 and min rank it is listed but never paid. Should the app show which gate applies?
- **Q-AP2** (AP) The project notes list a missing lud16 among the silent no-row gates; the watcher code writes a failed row for it. Which is current on prod?
- **Q-PR1** (PR) publish_lud16 reads the existing kind-0 from the instance only; a newer profile on the public relays gets replaced by the instance copy plus lud16.
- **Q-PR2** (PR) Instance and public relays are written in sequence; a public failure leaves them disagreeing until Retry.
- **Q-PR3** (PR) The source disagrees with itself about which one the prod payer reads: nostr.rs:30-34 says the public relays, nostr.rs:667 says the instance. Which is true?
- **Q-KS1** (KS) An invalid MC_*_NSEC poisons the account: every publish fails with the env error even after a good paste, and onboarding can open with it still in force (nostr.rs:500, 784-791, 861-864). Validate at launch?
- **Q-KS2** (KS) InvalidKey always names the env var, even when the bad value came from the keychain (secrets.rs:277).
- **Q-KS3** (KS) A key imported with a debug build is invisible to the release .app, and the reverse.
- **Q-CO1** (CO) Any non-2xx from /api/register counts as Refused and deletes the login; a proxy 502 after Coinos created the account strands a real password (coinos.rs:169-172).
- **Q-CO2** (CO) A login stored for a different pubkey is silently overwritten and the old wallet's password is lost. → routed: _the in-flight refactor, which binds the login to the account record_
- **Q-CO3** (CO) Forget this key leaves the Coinos login in place. Intended, so a re-import finds its wallet?
- **Q-CX1** (CX) The register body shape and the 2-24 lowercase rule are asserted from coinos-server source with no live contract test.
- **Q-CC1** (CC) complete=0 fragments are fed back to the model as full assistant turns on the next POST (index.ts:174-175).
- **Q-CC2** (CC) Any tailnet member can read or append to any conversation by guessing its id.
- **Q-CC3** (CC) Is celld-chat running on the box today? The repo holds no deploy record.
- **Q-CC4** (CC) The key sits in cleartext in the bundle metadata in S3; who else can read that bucket?
- **Q-PK1** (PK) make-app.sh says the bundle id must match src/config.rs and a directories crate; neither exists. Which should the id track?
- **Q-PK2** (PK) README.md still describes a 400-line chat toy with MODEL in main.rs; DEMO.md omits the ~/.config/anthropic/key fallback. Rewrite both?
- **Q-PK3** (PK) aws-lc-rs compiles in although ring is the provider; reqwest's rustls-tls feature pulls it too, so a gpui-kit feature alone cannot drop it.
- ~~**Q-UB1**~~ (UB) ✓ Accounts is built now: account cards plus the per-account page with the Trust tab (src/account.rs, this working tree). Settings is still a placeholder.
- **Q-NA1** (NA) Which revision should the atlas track once the refactor lands: HEAD, the refactor, or both with a moving marker? → routed: _rebuild after the refactor commit_

## What the platform gives vs what we own

**Platform gives:** gpui-kit: window, layout, inputs, markdown, scrollbars, theme, an HTTP client. nostr-sdk: keys, signing, a relay client. The instance: login, bounty rules, payment, receipt validation. Coinos: a Lightning address that works the second it exists.

**We own:** The Shell and its state, the runtime thread and the Command/Update protocol, event builders that match the server's dtag.js byte for byte, the bounty watcher and its ledger, keychain access, the Coinos signup order, the QR, the chat SSE reader, and the worker cell.

## Planned filesystem

```
magic-carpet-chat/
  src/main.rs         entry: --import-keys, gpui boot, one window
  src/shell.rs        Shell state, shortcuts, rail, sidebar, forms, apply()
  src/nostr.rs        runtime thread, Command/Update, relay, watcher, ledger, profile
  src/api.rs          instance REST client, Bounty/Claim/AutoPaymentRow shapes
  src/events.rs       kind 39998 / 39999 builders, kind 9735 parser, slug + hash8
  src/secrets.rs      keychain, Secret, env overrides, --import-keys
  src/coinos.rs       Coinos signup, LNURL
  src/wallet.rs       Wallet panel, QR
  src/onboarding.rs   first-launch screen
  src/dashboard.rs    Needs attention, Recent activity
  src/bounties.rs     list, detail, claim timeline, form cards
  src/chat.rs         Chat pane, Anthropic SSE
  src/palette.rs src/icons.rs src/timefmt.rs
  worker/src/index.ts celld ChatCell (server chat, not wired in)
  scripts/make-app.sh unsigned .app + READ ME FIRST
  docs/               this atlas
```

## How this file is maintained

Generated from `docs/atlas/data.mjs` by `node docs/atlas/build.mjs`, which also builds the interactive atlas (`atlas.html`, published at http://localhost:8765/atlas.html). Edit the data file, rebuild, republish — never edit this file by hand.
