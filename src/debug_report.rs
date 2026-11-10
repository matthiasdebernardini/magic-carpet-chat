//! The "Copy debug info" report: plain text a person can paste into a chat.
//!
//! The shell gathers [`Report`] from its own state; [`render`] is pure, so a
//! test builds the whole report without a window. No nsec, Coinos password or
//! token ever enters a [`Report`], and the text still goes through
//! [`diag::redact`] on the way out.

use magic_carpet_chat::diag;

/// `MC_*` variables whose value is a plain URL or path: these print their
/// value. Every other one prints "set" (`MC_NSECS` and `MC_SENTRY_DSN` hold
/// secrets).
const OPEN_VARS: [&str; 5] = [
    "MC_BASE_URL",
    "MC_RELAY_URL",
    "MC_BRAINSTORM_URL",
    "MC_TRUST_RELAYS",
    "MC_STORE_DIR",
];

/// One stored account.
pub(crate) struct AccountLine {
    pub name: String,
    pub npub: String,
    pub lud16: Option<String>,
    /// The Coinos username when a Coinos login is stored.
    pub coinos_username: Option<String>,
    pub balance: Option<u64>,
}

pub(crate) struct Report {
    pub version: String,
    pub os: String,
    pub arch: String,
    pub now: String,
    pub store_dir: String,
    pub base_url: String,
    pub relay_url: String,
    pub public_relays: Vec<String>,
    /// `(name, shown value)`; see [`env_lines`].
    pub env: Vec<(String, String)>,
    pub relay_connected: bool,
    pub screen: String,
    pub tab: Option<String>,
    pub active: Option<String>,
    pub accounts: Vec<AccountLine>,
    /// `(id, role, label)` from `a11y::on_screen`.
    pub on_screen: Vec<(String, String, String)>,
    pub activity: Vec<String>,
    pub diagnostics: Vec<String>,
}

/// The `MC_*` variables that are set and `ANTHROPIC_API_KEY` (set or unset),
/// each with the value the report may show. The key itself never prints.
pub(crate) fn env_lines(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = vars
        .filter(|(name, _)| name.starts_with("MC_") || name == "ANTHROPIC_API_KEY")
        .map(|(name, value)| {
            let shown = if OPEN_VARS.contains(&name.as_str()) { value } else { "set".into() };
            (name, shown)
        })
        .collect();
    if !lines.iter().any(|(name, _)| name == "ANTHROPIC_API_KEY") {
        lines.push(("ANTHROPIC_API_KEY".into(), "unset".into()));
    }
    lines.sort();
    lines
}

pub(crate) fn render(report: &Report) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    let none = |value: &Option<String>| value.clone().unwrap_or_else(|| "none".into());

    line("== App ==".into());
    line(format!("version: {}", report.version));
    line(format!("os: {} {}", report.os, report.arch));
    line(format!("time: {}", report.now));

    line("\n== Config ==".into());
    line(format!("store dir: {}", report.store_dir));
    line(format!("instance: {}", report.base_url));
    line(format!("relay: {}", report.relay_url));
    line(format!("public relays: {}", report.public_relays.join(", ")));
    for (name, value) in &report.env {
        line(format!("{name}: {value}"));
    }

    line("\n== Connection ==".into());
    line(format!(
        "relay: {}",
        if report.relay_connected { "connected" } else { "disconnected" }
    ));

    line("\n== Screen ==".into());
    line(format!("screen: {}", report.screen));
    line(format!("tab: {}", none(&report.tab)));
    line(format!("active account: {}", none(&report.active)));

    line("\n== Accounts ==".into());
    for account in &report.accounts {
        line(format!("{} {}", account.name, account.npub));
        line(format!("  lud16: {}", none(&account.lud16)));
        line(format!(
            "  coinos login: {}",
            match &account.coinos_username {
                Some(username) => format!("yes ({username})"),
                None => "no".into(),
            }
        ));
        line(format!(
            "  balance: {}",
            account.balance.map_or("unknown".into(), |sats| format!("{sats} sats"))
        ));
    }

    line("\n== On screen ==".into());
    for (id, role, label) in &report.on_screen {
        line(format!("{id} [{role}] {label}"));
    }

    line("\n== Recent activity ==".into());
    for item in &report.activity {
        line(item.clone());
    }

    line("\n== Diagnostics ==".into());
    for item in &report.diagnostics {
        line(item.clone());
    }

    diag::redact(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Report {
        Report {
            version: "0.6.0".into(),
            os: "macos".into(),
            arch: "aarch64".into(),
            now: "Nov 10, 2026 12:00:00Z".into(),
            store_dir: "/tmp/store".into(),
            base_url: "https://example.test".into(),
            relay_url: "wss://relay.example.test".into(),
            public_relays: vec!["wss://one.test".into()],
            env: vec![("MC_NSECS".into(), "set".into())],
            relay_connected: true,
            screen: "Account".into(),
            tab: Some("Wallet".into()),
            active: Some("Dana".into()),
            accounts: vec![AccountLine {
                name: "Dana".into(),
                npub: "npub1sg6plzptd64u62a878hep2kev88swjh3tw00gjsfl8f237lmu63q0uf63m".into(),
                lud16: Some("dana@coinos.io".into()),
                coinos_username: Some("dana".into()),
                balance: Some(1200),
            }],
            on_screen: vec![(
                "wallet-publish-error".into(),
                "Label".into(),
                "The Nostr servers didn't accept the profile update.".into(),
            )],
            activity: vec!["12:00:00 Wallet opened".into()],
            // A stray key in a diagnostic line must not survive.
            diagnostics: vec![
                "relay refused: blocked nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5"
                    .into(),
            ],
        }
    }

    #[test]
    fn the_report_holds_the_warning_the_npub_and_diagnostics_and_no_nsec() {
        let text = render(&sample());
        assert!(text.contains("The Nostr servers didn't accept the profile update."));
        assert!(text.contains("npub1sg6plzptd64u62a878hep2kev88swjh3tw00gjsfl8f237lmu63q0uf63m"));
        assert!(text.contains("relay refused: blocked <redacted>"));
        assert!(!text.contains("nsec1"));
    }

    #[test]
    fn env_values_show_only_for_the_open_ones() {
        let vars = [
            ("MC_NSECS", "nsec1secret"),
            ("MC_BASE_URL", "http://localhost:3000"),
            ("HOME", "/Users/x"),
        ]
        .map(|(name, value)| (name.to_string(), value.to_string()));
        assert_eq!(
            env_lines(vars.into_iter()),
            [
                ("ANTHROPIC_API_KEY".to_string(), "unset".to_string()),
                ("MC_BASE_URL".to_string(), "http://localhost:3000".to_string()),
                ("MC_NSECS".to_string(), "set".to_string()),
            ]
        );
    }
}
