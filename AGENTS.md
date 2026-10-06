# magic-carpet-chat — notes for coding agents

Desktop client for a Magic Carpet Nostr bounty instance: one window on gpui-kit 0.6.0. The full map is `docs/SYSTEM.md` (generated) and `docs/atlas.html`. `CLAUDE.md` imports this file. Edit this one.

## This repo is PUBLIC

Never commit:

- client messages, call notes or quotes (David, NosFabrica),
- David's design mock,
- goal docs and plans that quote the client,
- hostnames from the private tailnet (`*.ts.net`), keys, `.env`, accounts.json.

Those live in the **private** NostrFabrica repo, one directory up.

## Where things live

| Path | Repo | What it is |
|---|---|---|
| `src/`, `tests/`, `assets/` | this repo, public | the app |
| `docs/atlas/`, `docs/SYSTEM.md`, `docs/atlas.html` | this repo, public | architecture atlas; `data.mjs` is the source |
| `docs/test-cycles/` | this repo, public | the "app test cycles with Cua, Jev, Claude Code and Cap" guide, a static site (`wrangler deploy` there publishes `public/`) |
| `scripts/walkthrough/` | this repo, public | Feature 1 walkthrough: drives the app with macos-harness and judges each step with Jev |
| `scripts/e2e-cua/` | this repo, public | the e2e suite for the cua demo box |
| `goals/gpui-kit-migration.md`, `goals/profile-json-fidelity.md` | this repo, public | older goals, already public |
| `../magic-carpet-chat-context/goals/` | NostrFabrica, **private** | current goals: Feature 1 (`feature-1-treasure-map-goal.md`), the next pages (`treasure-map-and-next-pages.md`), copy gaps per screen (`artifact-gap.md`), the cua demo box runbook (`cua-jev-demo-box.md`) |
| `../magic-carpet-chat-context/docs/mock/` | NostrFabrica, **private** | David's Claude Design mock: every screen and its copy |

Paths inside the private goal docs: `goals/…` and `docs/mock/…` resolve inside `../magic-carpet-chat-context/`. `src/…`, `tests/…` and `scripts/…` mean this repo. New goal docs that quote the client go in the private directory.

## Build and test

- `mbx build`, `mbx nextest run`, `mbx clippy --all-targets`. Never bare `cargo` for anything that compiles; `target` is a symlink into the mbx cache.
- The toolchain is pinned in `rust-toolchain.toml`. Bump it on purpose: each bump is one cold rebuild of about 900 crates.
- `mbx nextest run` is offline. `tests/e2e.rs` is `#[ignore]` and moves real sats through Coinos; the command is in README.md.
- The Chat pane needs an Anthropic key: `~/.config/anthropic/key` or `ANTHROPIC_API_KEY`. `.env` here is gitignored.

## Two threads

- gpui's executor is not tokio; a tokio future polled on it aborts the process. Everything that talks Nostr, the instance or Coinos lives in the library (`src/lib.rs`: api, coinos, events, nostr, secrets) and runs on one tokio thread started by `nostr::spawn_runtime`. The UI modules under `main.rs` send it Commands and receive Updates. The Chat pane is the one exception: it polls its SSE body on gpui's executor, so only `ReqwestClient::user_agent` may be used there.
- `spawn_runtime` installs rustls's ring provider before any client exists. ring and aws-lc-rs are both in the graph and rustls refuses to choose.

## Secrets

- Every account (nsec, Coinos login) lives in `~/Library/Application Support/magic-carpet-chat/accounts.json` (Linux: `~/.config/magic-carpet-chat/`), mode 0600, written atomically. `MC_NSECS` imports keys at launch.
- Secrets cross threads only inside `Secret`, whose Debug prints `<redacted>`. Error text is fixed wording, never the input.

## Release

- Push a `v*` tag: `.github/workflows/release.yml` builds the Mac universal app and the Linux binaries. `scripts/make-app.sh` makes the unsigned .app.

## Docs and code intelligence

- `docs/atlas/data.mjs` is the single source. `node docs/atlas/build.mjs` writes `docs/SYSTEM.md` and `docs/atlas.html`; never edit those two by hand. Question IDs (`Q-<code><n>`) are stable: resolve or route in `data.mjs`, never delete.
- The atlas data still describes the keychain era (commit 3bf9877). The multi-account store (accounts.json, `src/account_setup.rs`) landed in f08d191; update `data.mjs` before trusting chapters 3 and 11.
- codegraph is indexed here (`.codegraph/`, gitignored), but no machine registers it as an MCP server yet.
