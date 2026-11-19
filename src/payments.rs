//! The Payments screen: the active account's ledger, one row per claim, in
//! two tabs (what it pays, what it is paid) with a strip of totals over each.
//!
//! Everything renders from the ledger `Shell` already holds; the screen does
//! no I/O. A row click opens its bounty's detail pane on the Bounties screen,
//! where the claim's full timeline lives. ↑/↓ move the highlight and Enter
//! opens it, as on Bounties.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::{h_flex, v_flex};
use nostr_sdk::prelude::*;

use magic_carpet_chat::bounty_search::coordinate_dtag;
use magic_carpet_chat::ledger::{self, Direction, Row, Status};

use crate::a11y::Accessible as _;
use crate::dashboard::MONO;
use crate::palette::*;
use crate::shell::{Shell, short_npub};
use crate::timefmt;

// ---------------------------------------------------------------- helpers

/// "Paid by me (N)" / "Paid to me (N)".
fn tab_label(direction: Direction, count: usize) -> String {
    let name = match direction {
        Direction::PaidByMe => "Paid by me",
        Direction::PaidToMe => "Paid to me",
    };
    format!("{name} ({count})")
}

/// The muted line under the item: who the sats go to or come from, and on
/// which list. Lists carry no title of their own, so the d-tag names them,
/// as on the Bounties screen.
fn subtitle(direction: Direction, name: &str, coordinate: &str) -> String {
    let way = match direction {
        Direction::PaidByMe => "to",
        Direction::PaidToMe => "from",
    };
    format!("{way} {name} · list {}", coordinate_dtag(coordinate))
}

/// "1,300 sats".
fn sats(n: u64) -> String {
    format!("{} sats", timefmt::fmt_sats(n))
}

/// How a status pill is drawn, so each status reads apart at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PillLook {
    text: u32,
    border: u32,
    /// `0xRRGGBBAA`; `None` leaves the pill unfilled.
    fill: Option<u32>,
    /// Unknown alone: the outcome was never learned, so the pill is drawn
    /// as an outline of Paying rather than the thing itself.
    dashed: bool,
}

fn pill_look(status: Status) -> PillLook {
    let plain = |text, border| PillLook { text, border, fill: None, dashed: false };
    match status {
        Status::Paid => PillLook { fill: Some(GREEN_WASH), ..plain(GREEN, GREEN_EDGE) },
        Status::PaidNoReceipt => PillLook { fill: Some(ACCENT_WASH), ..plain(ACCENT_LIGHT, ACCENT) },
        // Solid amber with dark text: money is out right now.
        Status::Paying => PillLook { fill: Some(AMBER << 8 | 0xff), ..plain(BG_RAIL, AMBER) },
        Status::Unknown => PillLook { dashed: true, ..plain(AMBER, AMBER) },
        Status::Owed => plain(TEXT, BORDER_3),
        Status::NotPayable => plain(RED, RED),
        Status::Expired => plain(TEXT_DIM, BORDER_2),
    }
}

/// The counterparty's name: a stored account's profile name, else a short
/// npub. No profile cache covers other keys, so strangers show as npubs.
fn counterparty_name(shell: &Shell, pubkey: &str) -> String {
    if let Some(name) = shell.view(pubkey).and_then(|v| v.kind0_name.clone()) {
        return name;
    }
    PublicKey::from_hex(pubkey)
        .ok()
        .and_then(|key| key.to_bech32().ok())
        .map(|npub| short_npub(&npub))
        .unwrap_or_else(|| short_npub(pubkey))
}

/// A row's key, `(bounty id, claim event id)`. A claim on a list is a claim
/// on every bounty on it, so one claim event id can head several rows.
pub(crate) type RowKey = (String, String);

fn row_key(row: &Row) -> RowKey {
    (row.bounty_id.clone(), row.claim_event_id.clone())
}

pub(crate) fn is_row(row: &Row, key: &RowKey) -> bool {
    row.bounty_id == key.0 && row.claim_event_id == key.1
}

/// ↑/↓ on the shown rows: the row `step` rows from `current`, clamped to
/// the ends; the first row when nothing (or a row no longer shown) is
/// highlighted. `None` with no rows.
pub(crate) fn step(rows: &[&Row], current: Option<&RowKey>, step: isize) -> Option<RowKey> {
    let last = rows.len().checked_sub(1)?;
    let next = match current.and_then(|key| rows.iter().position(|r| is_row(r, key))) {
        Some(ix) => ix.saturating_add_signed(step).min(last),
        None => 0,
    };
    Some(row_key(rows[next]))
}

/// The per-row id stem, unique across both tabs: the bounty and the claim.
fn row_id(row: &Row) -> String {
    format!("payment-row-{}-{}", row.bounty_id, row.claim_event_id)
}

// ------------------------------------------------------------------ parts

fn tab(direction: Direction, count: usize, on: bool, cx: &mut Context<Shell>) -> AnyElement {
    let id = match direction {
        Direction::PaidByMe => "payments-tab-by-me",
        Direction::PaidToMe => "payments-tab-to-me",
    };
    let label = tab_label(direction, count);
    div()
        .id(id)
        .a11y(id, Role::Tab, label.clone())
        .aria_selected(on)
        .px(px(10.))
        .py(px(5.))
        .rounded(px(7.))
        .cursor_pointer()
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .map(|this| {
            if on {
                this.bg(rgb(BG_NAV_ON)).text_color(rgb(TEXT))
            } else {
                this.text_color(rgb(TEXT_MUTED))
                    .hover(|this| this.bg(rgb(BG_HOVER)))
            }
        })
        .child(SharedString::from(label))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.payments_tab = direction;
            this.payment_selected = None;
            cx.notify();
        }))
        .into_any_element()
}

/// One small stat card of the totals strip.
fn total(id: &'static str, title: &'static str, value: u64) -> AnyElement {
    let value = sats(value);
    v_flex()
        .flex_1()
        .min_w(px(0.))
        .px(px(14.))
        .py(px(10.))
        .rounded(px(10.))
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(BG_CARD))
        .gap(px(3.))
        .child(
            div()
                .a11y(format!("{id}-title"), Role::Label, title)
                .text_size(px(9.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_DIM))
                .child(title),
        )
        .child(
            div()
                .a11y(id, Role::Label, value.clone())
                .font_family(MONO)
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(value)),
        )
        .into_any_element()
}

fn totals_strip(shell: &Shell) -> AnyElement {
    let rows = shell.ledger_rows();
    let cards = match shell.payments_tab {
        Direction::PaidByMe => {
            let t = ledger::totals(rows, Direction::PaidByMe);
            let me = shell.active.as_deref().unwrap_or_default();
            vec![
                total("payments-total-owed", "OWED", t.owed),
                total("payments-total-paying", "PAYING", t.paying),
                total("payments-total-paid", "PAID", t.paid),
                total(
                    "payments-total-pledged",
                    "PLEDGED",
                    ledger::pledged(shell.ledger_issued(), rows, me),
                ),
            ]
        }
        Direction::PaidToMe => {
            let t = ledger::totals(rows, Direction::PaidToMe);
            vec![
                total("payments-total-owed-to-me", "OWED TO ME", t.owed),
                total("payments-total-received", "RECEIVED", t.paid),
            ]
        }
    };
    h_flex().gap(px(10.)).mb(px(14.)).children(cards).into_any_element()
}

fn status_pill(id: String, status: Status) -> AnyElement {
    let look = pill_look(status);
    let label = status.label();
    div()
        .a11y(id, Role::Label, label)
        .px(px(8.))
        .py(px(2.))
        .rounded_full()
        .border_1()
        .border_color(rgb(look.border))
        .when(look.dashed, |this| this.border_dashed())
        .when_some(look.fill, |this, fill| this.bg(wash(fill)))
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(look.text))
        .child(label)
        .into_any_element()
}

fn payment_row(shell: &Shell, row: &Row, now: u64, cx: &mut Context<Shell>) -> AnyElement {
    let id = row_id(row);
    let selected = shell.payment_selected.as_ref().is_some_and(|key| is_row(row, key));
    let line = subtitle(
        row.direction,
        &counterparty_name(shell, &row.counterparty),
        &row.list_coordinate,
    );
    let amount = sats(row.sats);
    let when = timefmt::relative(now, row.at);
    let key = row_key(row);
    let direction = row.direction;
    h_flex()
        .id(SharedString::from(id.clone()))
        .a11y(id.clone(), Role::Group, row.item.clone())
        .gap(px(14.))
        .items_start()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(10.))
        .border_1()
        .cursor_pointer()
        .map(|this| {
            if selected {
                this.bg(rgb(BG_NAV_ON)).border_color(rgb(ACCENT))
            } else {
                this.bg(rgb(BG_CARD))
                    .border_color(rgb(BORDER))
                    .hover(|this| this.bg(rgb(BG_HOVER)))
            }
        })
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .gap(px(3.))
                .child(
                    div()
                        .a11y(format!("{id}-item"), Role::Label, row.item.clone())
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(row.item.clone())),
                )
                .child(
                    div()
                        .a11y(format!("{id}-who"), Role::Label, line.clone())
                        .text_size(px(11.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(SharedString::from(line)),
                ),
        )
        .child(
            div()
                .a11y(format!("{id}-sats"), Role::Label, amount.clone())
                .w(px(100.))
                .flex_shrink_0()
                .text_right()
                .font_family(MONO)
                .text_size(px(12.))
                .child(SharedString::from(amount)),
        )
        .child(
            v_flex()
                .w(px(150.))
                .flex_shrink_0()
                .items_end()
                .gap(px(3.))
                .child(status_pill(format!("{id}-status"), row.status))
                .when_some(row.why.clone(), |this, why| {
                    this.child(
                        div()
                            .a11y(format!("{id}-why"), Role::Label, why.clone())
                            .text_right()
                            .text_size(px(10.))
                            .text_color(rgb(TEXT_DIM))
                            .child(SharedString::from(why)),
                    )
                }),
        )
        .child(
            div()
                .a11y(format!("{id}-when"), Role::Label, when.clone())
                .w(px(80.))
                .flex_shrink_0()
                .text_right()
                .text_size(px(11.))
                .text_color(rgb(TEXT_DIM))
                .child(SharedString::from(when)),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.payment_selected = Some(key.clone());
            this.open_payment(key.0.clone(), direction, window, cx);
        }))
        .into_any_element()
}

/// The notice over rows that predate a failed refresh.
fn stale_notice(now: u64, read_at: u64, retrying: bool) -> String {
    let next = if retrying { "Retrying…" } else { "⌘R tries again." };
    format!(
        "Couldn't refresh. Showing payments as of {}. {next}",
        timefmt::relative(now, read_at)
    )
}

fn line(id: &'static str, text: impl Into<SharedString>, color: u32) -> AnyElement {
    let text = text.into();
    div()
        .a11y(id, Role::Label, text.clone())
        .py(px(20.))
        .text_size(px(12.5))
        .text_color(rgb(color))
        .child(text)
        .into_any_element()
}

// ----------------------------------------------------------------- screen

pub fn render(shell: &Shell, cx: &mut Context<Shell>) -> impl IntoElement {
    let heading = div()
        .a11y("payments-title", Role::Heading, "Payments")
        .mb(px(18.))
        .text_size(px(21.))
        .font_weight(FontWeight::BOLD)
        .child("Payments");

    // Read-only mode has no ledger to read.
    if shell.active.is_none() {
        return v_flex().child(heading).child(line(
            "payments-no-account",
            "No account. Press + in the rail to add one.",
            TEXT_MUTED,
        ));
    }
    if shell.ledger_loading() {
        return v_flex()
            .child(heading)
            .child(line("payments-loading", "Loading payments…", TEXT_DIM));
    }
    if let Some(message) = shell.ledger_failed() {
        return v_flex()
            .child(heading)
            .child(line(
                "payments-error",
                format!("Payments didn't load: {message}"),
                RED,
            ))
            .child(
                div()
                    .a11y("payments-retry-hint", Role::Label, "⌘R tries again.")
                    .text_size(px(11.))
                    .text_color(rgb(TEXT_DIM))
                    .child("⌘R tries again."),
            );
    }

    let by_me = shell.ledger_rows_for(Direction::PaidByMe).count();
    let to_me = shell.ledger_rows_for(Direction::PaidToMe).count();
    let tabs = h_flex()
        .a11y("payments-tabs", Role::TabList, "Payment lists")
        .gap(px(6.))
        .mb(px(12.))
        .child(tab(Direction::PaidByMe, by_me, shell.payments_tab == Direction::PaidByMe, cx))
        .child(tab(Direction::PaidToMe, to_me, shell.payments_tab == Direction::PaidToMe, cx));

    let now = timefmt::now_unix();
    let stale = shell.ledger_stale().map(|(read_at, retrying)| {
        let text = stale_notice(now, read_at, retrying);
        div()
            .a11y("payments-stale", Role::Label, text.clone())
            .mb(px(10.))
            .text_size(px(11.))
            .text_color(rgb(AMBER))
            .child(SharedString::from(text))
    });
    let rows: Vec<AnyElement> = shell
        .ledger_rows_for(shell.payments_tab)
        .map(|row| payment_row(shell, row, now, cx))
        .collect();
    let body = if rows.is_empty() {
        let text = match shell.payments_tab {
            Direction::PaidByMe => "You have not paid for any claims yet.",
            Direction::PaidToMe => "Nobody has paid you for a claim yet.",
        };
        line("payments-empty", text, TEXT_DIM)
    } else {
        v_flex()
            .a11y("payments-list", Role::List, "Payments")
            .gap(px(8.))
            .children(rows)
            .into_any_element()
    };

    v_flex()
        .child(heading)
        .children(stale)
        .child(tabs)
        .child(totals_strip(shell))
        .child(body)
}

#[cfg(test)]
mod tests {
    use super::{
        Direction, Row, RowKey, Status, pill_look, row_id, sats, stale_notice, step, subtitle, tab_label,
    };
    use crate::palette::{AMBER, GREEN, RED};

    #[test]
    fn tabs_count_their_rows() {
        assert_eq!(tab_label(Direction::PaidByMe, 3), "Paid by me (3)");
        assert_eq!(tab_label(Direction::PaidToMe, 0), "Paid to me (0)");
    }

    #[test]
    fn the_subtitle_names_the_counterparty_and_the_list_dtag() {
        assert_eq!(
            subtitle(Direction::PaidByMe, "alice", "39998:aa:us-city"),
            "to alice · list us-city"
        );
        assert_eq!(
            subtitle(Direction::PaidToMe, "npub1abc…wxyz", "39998:bb:tacos"),
            "from npub1abc…wxyz · list tacos"
        );
        // A coordinate with no colons is its own d-tag.
        assert_eq!(subtitle(Direction::PaidByMe, "a", "bare"), "to a · list bare");
    }

    fn claim(id: &str) -> Row {
        Row {
            direction: Direction::PaidByMe,
            bounty_id: "b".into(),
            list_coordinate: "39998:aa:l".into(),
            claim_address: String::new(),
            claim_event_id: id.into(),
            item: "x".into(),
            counterparty: "cc".into(),
            sats: 1,
            status: Status::Owed,
            why: None,
            at: 0,
        }
    }

    /// The key of claim `id` on `claim`'s bounty "b".
    fn on_b(id: &str) -> RowKey {
        ("b".into(), id.into())
    }

    #[test]
    fn arrows_step_through_the_rows_and_stop_at_the_ends() {
        let (a, b, c) = (claim("a"), claim("b"), claim("c"));
        let rows = [&a, &b, &c];
        assert_eq!(step(&rows, None, 1), Some(on_b("a")));
        assert_eq!(step(&rows, Some(&on_b("a")), 1), Some(on_b("b")));
        assert_eq!(step(&rows, Some(&on_b("c")), 1), Some(on_b("c")));
        assert_eq!(step(&rows, Some(&on_b("a")), -1), Some(on_b("a")));
        assert_eq!(step(&rows, Some(&on_b("c")), -1), Some(on_b("b")));
        // A highlight the tab no longer shows starts over at the top.
        assert_eq!(step(&rows, Some(&on_b("gone")), -1), Some(on_b("a")));
        assert_eq!(step(&[], None, 1), None);
    }

    #[test]
    fn one_claim_on_two_bounties_is_two_rows() {
        // A claim on a list claims every bounty on it: X heads two rows.
        let x1 = Row { bounty_id: "b1".into(), ..claim("x") };
        let x2 = Row { bounty_id: "b2".into(), ..claim("x") };
        let y = claim("y");
        let rows = [&x1, &x2, &y];
        let key = |b: &str, c: &str| (b.to_string(), c.to_string());
        assert_eq!(step(&rows, None, 1), Some(key("b1", "x")));
        assert_eq!(step(&rows, Some(&key("b1", "x")), 1), Some(key("b2", "x")));
        assert_eq!(step(&rows, Some(&key("b2", "x")), 1), Some(key("b", "y")));
        assert_eq!(step(&rows, Some(&key("b2", "x")), -1), Some(key("b1", "x")));
        assert_ne!(row_id(&x1), row_id(&x2));
    }

    #[test]
    fn sats_read_with_thousands_and_unit() {
        assert_eq!(sats(1_300), "1,300 sats");
        assert_eq!(sats(0), "0 sats");
    }

    #[test]
    fn every_status_has_its_own_pill() {
        let all = [
            Status::Paid,
            Status::PaidNoReceipt,
            Status::Paying,
            Status::Unknown,
            Status::NotPayable,
            Status::Expired,
            Status::Owed,
        ];
        for (ix, a) in all.iter().enumerate() {
            for b in &all[ix + 1..] {
                assert_ne!(pill_look(*a), pill_look(*b), "{a:?} and {b:?} look alike");
            }
        }
        assert_eq!(pill_look(Status::Paid).text, GREEN);
        assert_eq!(pill_look(Status::NotPayable).text, RED);
        // Paying is filled amber; Unknown is the dashed outline of it.
        assert_eq!(pill_look(Status::Paying).fill, Some(0xf2b544ff));
        let unknown = pill_look(Status::Unknown);
        assert!(unknown.dashed && unknown.fill.is_none());
        assert_eq!(unknown.text, AMBER);
    }

    #[test]
    fn the_stale_notice_says_how_old_the_rows_are_and_what_comes_next() {
        assert_eq!(
            stale_notice(1_000, 820, true),
            "Couldn't refresh. Showing payments as of 3 min ago. Retrying…"
        );
        assert_eq!(
            stale_notice(1_000, 820, false),
            "Couldn't refresh. Showing payments as of 3 min ago. ⌘R tries again."
        );
    }
}
