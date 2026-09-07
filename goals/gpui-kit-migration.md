# Goal: migrate magic-carpet-chat to gpui-kit

## Context

`magic-carpet-chat/` is a gpui desktop client. Today Cargo.toml pulls six
git dependencies (gpui, gpui_platform, gpui-component, gpui-component-assets,
http_client, reqwest_client) from zed and longbridge, rev-less on the zed side
so that only one copy of gpui ends up in the graph. That is fragile and the
README documents the workaround.

gpui-component was renamed gpui-kit and published to crates.io on 2026-09-03
as version 0.6.0. It bundles gpui (as the `gpui-pre-*` crates), the platform
layer, the reqwest http client, gpui-base, gpui-component and the assets at
matching versions. Docs: https://gpui-kit.com and https://docs.rs/gpui-kit.

## Outcome

`magic-carpet-chat` builds, its tests pass, and the app launches and reaches
the chat view with `gpui-kit = "0.6.0"` as the only UI-stack dependency. No
git dependencies on zed or longbridge remain.

## Tasks

1. In `Cargo.toml`, replace the six git deps with `gpui-kit = "0.6.0"`.
   Delete the rev-less comment blocks that explained the old workaround.
   Update `[profile.dev.package]` to the new crate names (`gpui-pre`,
   `gpui-pre-platform`, and whatever gpui-kit's own workspace uses; copy from
   the gpui-kit repo).
2. Rewrite imports in `src/*.rs`:
   - `gpui::` becomes `gpui_kit::`
   - `gpui_component::` becomes `gpui_kit::component::`
   - `gpui_component_assets::Assets` becomes `gpui_kit::assets::Assets`
     (icons.rs)
   - `gpui_platform::application()` becomes `gpui_kit::application()`
     (main.rs)
   - `http_client::` and `reqwest_client::` map to their gpui-kit re-exports
     (chat.rs, main.rs). Check docs.rs for the exact paths.
3. Fix whatever API drift the compiler reports between our old pinned rev
   `6d07863` and 0.6.0. Keep fixes minimal; no redesign.
4. Update `README.md`: remove the git-pin section, state the single dep.
5. Run `mbx build` and `mbx nextest run`, then launch the app once and confirm
   the window renders the chat view with icons and theme intact.

## Acceptance criteria

- `grep -r "zed-industries\|longbridge" Cargo.toml` returns nothing.
- `cargo tree -i gpui-pre` shows exactly one version.
- `mbx nextest run` passes.
- App launches, title bar, icons and dark theme look the same as before.
- Diff touches only Cargo.toml, Cargo.lock, README.md and `src/`.

## Constraints

- Do not change behavior or layout. This is a dependency swap.
- Do not add other dependencies.
- rustls provider install in `spawn_runtime()` stays; verify ring is still
  what gpui-kit's reqwest pulls, adjust the comment if not.
- Use `mbx`, never bare `cargo build`.
