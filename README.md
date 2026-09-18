# Magic Carpet Chat

## Download

Ready-made builds are on the [releases page](https://github.com/matthiasdebernardini/magic-carpet-chat/releases/latest):
a Mac app (Apple Silicon and Intel) and Linux binaries for x86_64 and ARM64.
No Rust toolchain needed. The release notes carry the first-open steps: on a
Mac the app is not yet Developer-ID signed, so the first launch goes through
System Settings > Privacy & Security > Open Anyway.

Every push of a `v*` tag builds a new release via `.github/workflows/release.yml`.

A desktop chat shell in Rust on [gpui-kit](https://github.com/longbridge/gpui-kit).
One window, a scrolling transcript, and a composer. Enter streams a reply from the
Anthropic Messages API.

## Build and run

Everything that compiles goes through [mbx](https://mr-boxington.jdx.dev), the
machine-wide Cargo build cache; `target` is a symlink into it. The toolchain is
pinned in `rust-toolchain.toml`.

```sh
mbx build
mbx run
```

The Chat pane reads an Anthropic key from `~/.config/anthropic/key` or
`ANTHROPIC_API_KEY`. Without one the pane still opens and says what to set.

## Dependencies

The UI stack is one crates.io dependency, `gpui-kit` 0.6.0. It bundles gpui
(published as the `gpui-pre-*` crates), the platform layer, gpui-base,
gpui-component and the icon assets at matching versions. The only other UI
crate is `gpui-pre-reqwest-client`, the HTTP client gpui-kit does not
re-export; its version must resolve to the same `gpui-pre` minor gpui-kit uses.

## Coinos wallet

A generated key has no Lightning address, so its claims are accepted but never
paid. The account screen (first launch, or the "+" in the rail) and the Wallet
screen (⌘6) can open a hosted [Coinos](https://coinos.io) wallet bound to the
key; "Create a new account" does it in the same click. `src/coinos.rs` registers
`carpet<random>@coinos.io`; the runtime merges that `lud16` into the existing
kind-0 (never a rebuilt profile) and publishes it to the instance, damus,
nos.lol and primal; the panel shows a `lightning:` LNURL QR to fund it from any
Lightning wallet and polls the balance every 3 s, so the sats show up on
screen. The Wallet screen can also send sats from that wallet to any Lightning
address (another account here, a Strike address): a recipient, an amount,
Send. Coinos holds the sats, so keep balances small. Every account's nsec and
Coinos login (username, password, API token) live in one file,
`~/Library/Application Support/magic-carpet-chat/accounts.json` (mode 0600,
written atomically), so they survive app updates; `MC_NSECS` adds keys to it at
launch. The login is written before the signup request and deleted only when
Coinos refuses outright, so a signup that times out on the way back is never
lost. "Copy username" and "Copy password" on the panel are the only way the
password leaves the file.

`MC_SENTRY_DSN` turns on crash and error reporting to any Sentry-protocol
receiver (unset means off). `magic-carpet-chat --sentry-ping` sends one test
event to that DSN and prints its id.

## End-to-end check

`tests/e2e.rs` proves two brand-new keys can open wallets and pay each other
through the runtime alone: it creates accounts A and B (each with a Coinos
wallet and a published lud16), funds A with 21 sats, sends A to B, sends B
back to A, and checks that B is refused a send it cannot afford. It is
`#[ignore]`, so `mbx nextest run` stays offline. Run it with:

```sh
mbx nextest run --run-ignored only --no-capture -E 'binary(e2e)'
```

To have the test fund A itself, pass `MC_E2E_PAYER_TOKEN`, the `coinos.token`
of a funded account in `~/Library/Application Support/magic-carpet-chat/accounts.json`
(here the first account in the file):

```sh
MC_E2E_PAYER_TOKEN=$(jq -r '.accounts[0].coinos.token' ~/Library/Application\ Support/magic-carpet-chat/accounts.json) mbx nextest run --run-ignored only --no-capture -E 'binary(e2e)'
```

Without it the test prints A's address and waits for a phone wallet
(`MC_E2E_ZAP_WAIT_SECS`, default 600).

## What is here

Two halves. `src/lib.rs` is the protocol layer, free of gpui so it runs on its
own tokio thread and tests offline: `api` (the instance REST API), `nostr` (the
runtime, relays, signing, the bounty watcher), `events` (event kinds and
dtags), `coinos` (wallet signup and payments), `secrets` (accounts.json and the
`Secret` type). `src/main.rs` is the window: `shell` owns all state and paints
the screens (`dashboard`, `bounties`, `wallet`, `chat`, `account_setup`), with
`palette`, `icons` and `timefmt` as helpers. The Chat pane streams Claude from
the Anthropic Messages API on the window's own executor; every other network
call crosses to the tokio thread as a Command and comes back as an Update.

The full map, structure by structure, is `docs/SYSTEM.md` and the interactive
`docs/atlas.html`; see `docs/README.md`.
