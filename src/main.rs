//! Magic Carpet — a desktop shell for a Nostr bounty instance, built on
//! gpui-component.
//!
//! The window opens on the Dashboard. The nav's "Chat" row swaps the main pane
//! for a transcript and a composer that streams from the Anthropic Messages API.

use std::sync::Arc;

use gpui::*;
use gpui_component::{Theme, ThemeMode, TitleBar};
use gpui_component::Root;

mod chat;
mod dashboard;
mod data;
mod icons;
mod palette;
mod shell;

fn main() {
    let app = gpui_platform::application().with_assets(icons::Assets);

    app.run(move |cx| {
        // Must run before anything else from gpui-component.
        gpui_component::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        // The design mock is its own palette, not a tint of a stock theme, so
        // the controls gpui-component draws are repainted to match.
        palette::apply(cx);
        cx.bind_keys(shell::keybindings());
        cx.activate(true);

        // Gives gpui a real HTTP client; without it cx.http_client() cannot send.
        //
        // Use this constructor only. `proxy_user_agent_and_read_timeout` wraps the
        // response body in reqwest's `ReadTimeoutBody`, which calls
        // `tokio::time::sleep` on every poll. We poll the body from gpui's
        // foreground executor, which is not a tokio runtime, so the first chunk
        // aborts the process with "there is no reactor running". Verified.
        // The connection keep-alives zed sets (TCP 30 s, HTTP/2 15 s) already
        // catch a dead peer, so the timeout buys nothing here anyway.
        match reqwest_client::ReqwestClient::user_agent("magic-carpet-chat/0.1") {
            Ok(client) => cx.set_http_client(Arc::new(client)),
            Err(error) => eprintln!("could not build the HTTP client: {error}"),
        }

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1180.), px(800.)), cx)),
            titlebar: Some(TitlebarOptions {
                // Name the window for the OS, but let TitleBar draw it.
                title: Some("Magic Carpet".into()),
                ..TitleBar::title_bar_options()
            }),
            ..TitleBar::window_options()
        };

        cx.spawn(async move |cx| {
            cx.open_window(window_options, |window, cx| {
                let view = cx.new(|cx| shell::Shell::new(window, cx));
                // The first view in the window must be a Root.
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open the window");
        })
        .detach();
    });
}
