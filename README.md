# Magic Carpet Chat

A desktop chat shell in Rust on [gpui-component](https://github.com/longbridge/gpui-component).
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

## Pinned versions

`Cargo.toml` names three git dependencies. Only gpui-component carries a `rev`.
gpui-component's own manifest asks for `gpui = { git = ".../zed" }` with no rev,
so a rev on our zed lines would build a second copy of gpui and every type would
stop matching. `Cargo.lock` holds the zed revision instead.

| Crate | Source | Revision |
| --- | --- | --- |
| `gpui-component` 0.5.2 | github.com/longbridge/gpui-component | `6d07863fe7077f85abfa0ec2fcb05f3e17c573b2` |
| `gpui-component-assets` 0.5.1 | github.com/longbridge/gpui-component | `6d07863fe7077f85abfa0ec2fcb05f3e17c573b2` |
| `gpui` 0.2.2 | github.com/zed-industries/zed | `f66ed399cdde86092af8af3dc7b418abf45f37f8` |
| `gpui_platform` 0.1.0 | github.com/zed-industries/zed | `f66ed399cdde86092af8af3dc7b418abf45f37f8` |
| `http_client` 0.1.0 | github.com/zed-industries/zed | `f66ed399cdde86092af8af3dc7b418abf45f37f8` |
| `reqwest_client` 0.1.0 | github.com/zed-industries/zed | `f66ed399cdde86092af8af3dc7b418abf45f37f8` |

The zed revision is the one in gpui-component's own `Cargo.lock` at that commit,
so the pair is the combination their CI builds. Cargo first resolved zed to its
branch head; `cargo update gpui --precise f66ed399…` moved the whole zed git
source back to the tested revision.

Adding any new zed crate re-resolves that git source to the branch head, so run
the `--precise` command again afterwards and check `Cargo.lock` still says
`f66ed399`.

To move to a newer gpui-component: change both `rev` values, run
`cargo update`, then read the new gpui-component `Cargo.lock` for the zed
revision it expects and pass that to `cargo update gpui --precise`.

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
