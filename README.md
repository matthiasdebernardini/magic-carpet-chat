# Magic Carpet Chat

A desktop chat shell in Rust on [gpui-kit](https://github.com/longbridge/gpui-kit).
One window, a scrolling transcript, and a composer. Enter streams a reply from the
Anthropic Messages API.

## Build and run

Homebrew's `cargo` shadows rustup on this Mac. Use the rustup one:

```sh
export ANTHROPIC_API_KEY=sk-ant-…
/Users/md/.cargo/bin/cargo build
/Users/md/.cargo/bin/cargo run
```

Without the key the app still opens and says what to set.

## Dependencies

The UI stack is one crates.io dependency, `gpui-kit` 0.6.0. It bundles gpui
(published as the `gpui-pre-*` crates), the platform layer, gpui-base,
gpui-component and the icon assets at matching versions. The only other UI
crate is `gpui-pre-reqwest-client`, the HTTP client gpui-kit does not
re-export; its version must resolve to the same `gpui-pre` minor gpui-kit uses.

## Coinos wallet

A generated key has no Lightning address, so its claims are accepted but never
paid. Onboarding and the Wallet screen (⌘6) can open a hosted
[Coinos](https://coinos.io) wallet bound to the key. `src/coinos.rs` registers
`carpet<random>@coinos.io`; the runtime merges that `lud16` into the existing
kind-0 (never a rebuilt profile) and publishes it to the instance, damus,
nos.lol and primal; the panel shows a `lightning:` LNURL QR to fund it from any
Lightning wallet. Coinos holds the sats, so keep balances small. The login is
in the Mac keychain under `magic-carpet-chat` / `claimant-coinos-login`,
written before the signup request and deleted only when Coinos refuses
outright, so a signup that times out on the way back is never lost.

## What is here

`src/main.rs`, about 400 lines.

- Model — `MODEL` at the top of `main.rs`, `claude-sonnet-5`.
- Network — `reqwest_client::ReqwestClient` installed with `cx.set_http_client`,
  so `cx.http_client()` can POST. It runs its own tokio runtime; the UI thread
  only polls the socket, it never blocks on it.
- Streaming — one `POST /v1/messages` with `stream: true`. A foreground
  `cx.spawn` reads the Server-Sent Events line by line and pushes each
  `text_delta` into the pending bubble. The whole transcript goes with every
  request.
- Failures — a missing key, an HTTP error, or a broken stream all land in the
  pending bubble as plain English. Nothing panics and the key is never logged.

- `TitleBar` — drawn by the app, so the window stays on the dark theme instead of
  the system appearance.
- Transcript — a `track_scroll` + `overflow_y_scroll` area with gpui-component's
  overlay scrollbar. The app owns the `ScrollHandle`, so a new message can call
  `scroll_to_bottom()`.
- Assistant bubbles — `TextView::markdown`, selectable.
- Composer — `TextareaState` with `auto_grow(1, 6)` and `submit_on_enter(true)`.
  Enter sends, Shift+Enter makes a line.
