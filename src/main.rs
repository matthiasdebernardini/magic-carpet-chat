//! Magic Carpet — a desktop shell for a Nostr bounty instance, built on
//! gpui-kit.
//!
//! The window opens on the Dashboard. The nav's "Chat" row swaps the main pane
//! for a transcript and a composer that streams from the Anthropic Messages API.

use std::sync::Arc;

use gpui_kit::*;
use gpui_kit::component::{Theme, ThemeMode, TitleBar};
use gpui_kit::component::Root;

mod account_setup;
mod bounties;
mod chat;
mod dashboard;
mod icons;
mod palette;
mod shell;
mod timefmt;
mod wallet;

fn main() {
    // ring before any TLS client exists: sentry's reqwest transport is built
    // at init, which is before spawn_runtime() installs the same provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    // MC_SENTRY_DSN turns on crash and error reporting; unset means off. The
    // guard lives for the whole of main. The transport sends each event as it
    // arrives and panics flush themselves; the drop only matters for the ping,
    // because Cmd+Q exits inside app.run and never returns here.
    let sentry_dsn = std::env::var("MC_SENTRY_DSN").unwrap_or_default();
    let sentry_guard = (!sentry_dsn.is_empty()).then(|| {
        // ClientOptions is non_exhaustive in 0.49, so no struct expression.
        let mut options = sentry::ClientOptions::default();
        options.release = sentry::release_name!();
        sentry::init((sentry_dsn.as_str(), options))
    });

    // `--sentry-ping`: send one test event, print its id, and exit.
    if std::env::args().any(|arg| arg == "--sentry-ping") {
        let Some(guard) = sentry_guard else {
            eprintln!("MC_SENTRY_DSN is not set");
            std::process::exit(1);
        };
        let id = sentry::capture_message("magic-carpet-chat sentry ping", sentry::Level::Info);
        drop(guard);
        println!("{id}");
        return;
    }

    // `--import-keys`: read MC_NSECS (comma-separated), add them to the
    // accounts file, print ONLY the derived npubs, and exit. The secrets
    // themselves never touch stdout — `Secret` redacts, and only bech32
    // public keys are printed here.
    if std::env::args().any(|arg| arg == "--import-keys") {
        match magic_carpet_chat::secrets::import_from_env() {
            Ok(npubs) if npubs.is_empty() => {
                eprintln!("nothing to import: set MC_NSECS");
                std::process::exit(1);
            }
            Ok(npubs) => {
                for npub in npubs {
                    println!("{npub}");
                }
                return;
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
    }

    // The same import at every launch, before the window opens, so
    // LoadAccounts sees an MC_NSECS key on its first read. Idempotent: a key
    // already stored is left alone.
    if let Err(error) = magic_carpet_chat::secrets::import_from_env() {
        eprintln!("MC_NSECS import failed: {error}");
    }
    // Surface an unreadable accounts file at launch, not mid-demo. Not
    // fatal: browsing works with no account, and the first save says the
    // same thing on screen.
    if let Err(error) = magic_carpet_chat::secrets::load() {
        eprintln!("{error}");
    }

    let app = gpui_kit::application().with_assets(icons::Assets);

    app.run(move |cx| {
        // Must run before anything else from gpui-component.
        gpui_kit::init(cx);
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
        // The connection keep-alives the client sets (TCP 30 s, HTTP/2 15 s) already
        // catch a dead peer, so the timeout buys nothing here anyway.
        match reqwest_client::ReqwestClient::user_agent(magic_carpet_chat::USER_AGENT) {
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
