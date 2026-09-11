# Magic Carpet Chat glossary

One line per noun. The atlas and SYSTEM.md use these words and no synonyms.

- **Instance**: the Magic Carpet server at magic-carpet.brainstorm.world, a REST API in front of a strfry relay. Holds bounties, claims, payments and receipts.
- **Instance relay**: the strfry WebSocket at wss://magic-carpet.brainstorm.world/relay. The app subscribes to it for zap receipts and polls its status for the rail dot.
- **Public relays**: relay.damus.io, nos.lol, relay.primal.net. The app writes to them once, when it adds a Lightning address to a profile.
- **House issuer**: pubkey 853baa94…, the account that posts and pays bounties. The default issuer for the bounty list when no issuer key is stored.
- **Issuer**: the account slot that publishes lists and creates bounties. Operator only.
- **Claimant**: the account slot that claims an item and gets paid. What a pasted key becomes.
- **DList**: a kind-39998 Nostr event. The list header a bounty attaches to. Coordinate `39998:<pubkey>:<d-tag>`.
- **Claim**: a kind-39999 Nostr event naming one item on a DList. Content and `name` tag both carry the item name.
- **Bounty**: a server row tied to a DList coordinate: reward per item, cap, min rank, auto-pay flag.
- **Auto-pay watcher**: the server process in magic-carpet-v2 that pays claims. Not part of this app.
- **Bounty watcher**: this app's poller for the selected bounty, one live at a time, with a ledger per bounty.
- **Delegate**: the key the server zaps from on the issuer's behalf. Its pubkey sits inside a receipt's `description` tag and is what the server validates.
- **Shortcut**: a keyboard binding such as cmd-N. "Key" always means an nsec or npub.
- **Auto-payment row**: the server's `auto_payments` record for one claim. States: attempting, paid, settled, paid_unreceipted, failed.
- **Zap receipt**: a kind-9735 event from the claimant's Lightning provider. The payer is the pubkey inside its `description` tag, not its signer.
- **Trust gate**: the server-side rank check. Below rank 2 the claim is hidden. Between 2 and the bounty's min rank it is listed but never paid.
- **Shell**: the one gpui view that owns all app state and paints the window.
- **Runtime thread**: the OS thread named nostr-runtime that runs tokio, nostr-sdk and reqwest.
- **Command**: a message from the Shell to the runtime thread. Eleven variants.
- **Update**: a message from the runtime thread to the Shell. Seventeen variants. Each one is a deduplicated fact.
- **Watch ledger**: per-bounty memory of what the feed already reported. Outlives the watcher.
- **Activity feed**: the in-memory list on the Dashboard, newest first, capped at 200 rows.
- **Key store**: the macOS keychain, service `magic-carpet-chat`. Debug builds use 0600 files under `~/Library/Application Support/magic-carpet-chat/dev-secrets/` instead.
- **Secret**: the wrapper every nsec, password and API key travels in. Debug prints `<redacted>`.
- **Session**: one key plus one HTTP client with its own cookie jar, per account, built on first use.
- **Publish door**: `POST /api/strfry/publish`. The only way an event reaches the instance from this app.
- **Coinos**: the hosted Lightning wallet at coinos.io. One click opens an account bound to the claimant key.
- **lud16**: the Lightning address field in a kind-0 profile. Payouts need one.
- **LNURL**: the bech32 form of the Coinos pay endpoint. What the funding QR encodes, prefixed `lightning:`.
- **Chat pane**: the cmd-8 screen that streams Claude replies straight from the Anthropic API.
- **Chat cell**: the worker's Durable Object, one per conversation id. Not called by the desktop app.
- **celld**: the self-hosted Workers runtime the chat cell deploys to, on nashauto-git.exe.xyz.
- **Refactor in flight**: uncommitted work in the working tree that replaces the keychain with accounts.json and the two slots with N accounts.
