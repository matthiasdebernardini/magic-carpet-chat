//! The Bounties screen: the issuer's live bounty list beside a detail pane
//! with per-claim payment timelines, plus the two forms that drive the demo —
//! the issuer's "New DList + bounty" and everyone else's claim.
//!
//! Everything renders from `Shell` state fed by the nostr runtime. The whole
//! screen is keyboard-reachable: ↑/↓ select, ⌘N opens the form for whichever
//! account is active, Enter walks and submits the form, Esc (or ⌘.) cancels.
//! DEMO.md holds the full path.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::{h_flex, input::Input, v_flex};

use magic_carpet_chat::api::{Bounty, Claim};

use crate::dashboard::{MONO, count_badge};
use crate::palette::*;
use crate::shell::{BOUNTY_FIELD_DEFS, Load, RANK_NOTE, Shell, short_id};
use crate::timefmt;

/// The human end of a list coordinate: `39998:<pk>:us-city` → `us-city`.
fn coordinate_dtag(coordinate: &str) -> &str {
    coordinate.rsplit(':').next().unwrap_or(coordinate)
}

fn status_hue(status: &str) -> u32 {
    match status {
        "open" => GREEN,
        "fulfilled" => ACCENT_LIGHT,
        "expired" | "cancelled" => TEXT_DIM,
        _ => AMBER,
    }
}

fn pill(text: impl Into<SharedString>, hue: u32) -> impl IntoElement {
    div()
        .px(px(8.))
        .py(px(2.))
        .rounded_full()
        .border_1()
        .border_color(rgb(BORDER_3))
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(hue))
        .child(text.into())
}

fn hint(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(11.))
        .text_color(rgb(TEXT_DIM))
        .child(text.into())
}

/// A dot + line + optional meta row, the same visual grammar as the activity
/// feed, reused for the per-claim payment timeline.
fn timeline_row(dot: u32, text: String, meta: Option<String>) -> AnyElement {
    h_flex()
        .gap(px(9.))
        .items_start()
        .py(px(3.))
        .child(
            div()
                .size(px(7.))
                .mt(px(5.))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(dot)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .child(div().text_size(px(12.)).child(SharedString::from(text)))
                .when_some(meta, |this, meta| {
                    this.child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(TEXT_DIM))
                            .font_family(MONO)
                            .child(SharedString::from(meta)),
                    )
                }),
        )
        .into_any_element()
}

// ------------------------------------------------------------------- list

fn bounty_row(bounty: &Bounty, selected: bool, cx: &mut Context<Shell>) -> AnyElement {
    let id = bounty.id.clone();
    let dtag = coordinate_dtag(&bounty.list_coordinate).to_string();
    let status = bounty.effective_status().to_string();
    let counts = bounty
        .payment_state
        .as_ref()
        .map(|s| format!("paid {}/{}", s.paid_reward_count, s.total_reward_slots));

    v_flex()
        .id(SharedString::from(format!("bounty-{}", bounty.id)))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(10.))
        .border_1()
        .cursor_pointer()
        .map(|this| {
            if selected {
                // Unmissable from stage: fill + accent border + a soft glow.
                this.bg(rgb(BG_NAV_ON)).border_color(rgb(ACCENT)).shadow(vec![
                    BoxShadow::new(px(0.), px(0.), wash(ACCENT_GLOW))
                        .spread_radius(px(2.))
                        .blur_radius(px(10.)),
                ])
            } else {
                this.bg(rgb(BG_CARD))
                    .border_color(rgb(BORDER))
                    .hover(|this| this.bg(rgb(BG_HOVER)))
            }
        })
        .child(
            h_flex()
                .gap(px(8.))
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(dtag)),
                )
                .child(pill(status.clone(), status_hue(&status))),
        )
        .child(
            h_flex()
                .mt(px(3.))
                .gap(px(8.))
                .items_center()
                .font_family(MONO)
                .text_size(px(10.5))
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(format!(
                    "{} sats{}",
                    timefmt::fmt_sats(bounty.amount_sats),
                    if bounty.reward_per_item_on() {
                        " / item"
                    } else {
                        ""
                    }
                )))
                .when_some(counts, |this, counts| {
                    this.child(SharedString::from(counts))
                })
                .when(bounty.auto_pay_on(), |this| {
                    this.child(div().text_color(rgb(GREEN)).child("auto-pay"))
                }),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select(id.clone());
            cx.notify();
        }))
        .into_any_element()
}

fn list_pane(shell: &Shell, cx: &mut Context<Shell>) -> AnyElement {
    let body: Vec<AnyElement> = match &shell.bounties {
        Load::Loading => vec![
            div()
                .py(px(20.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_DIM))
                .child("Loading bounties…")
                .into_any_element(),
        ],
        Load::Failed(message) => vec![
            v_flex()
                .gap(px(8.))
                .py(px(12.))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(RED))
                        .child(SharedString::from(format!("Could not load: {message}"))),
                )
                .child(
                    div()
                        .id("retry-list")
                        .px(px(11.))
                        .py(px(5.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(rgb(BORDER_3))
                        .cursor_pointer()
                        .text_size(px(11.5))
                        .text_color(rgb(ACCENT_LIGHT))
                        .child("Retry (⌘R)")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
                .into_any_element(),
        ],
        Load::Ready(list) if list.is_empty() => vec![
            div()
                .py(px(20.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_DIM))
                .child("No bounties yet. ⌘N creates the first one.")
                .into_any_element(),
        ],
        Load::Ready(list) => list
            .iter()
            .map(|bounty| {
                bounty_row(
                    bounty,
                    shell.selected.as_deref() == Some(bounty.id.as_str()),
                    cx,
                )
            })
            .collect(),
    };

    // The pane scrolls on its own (the screen does not). The tracked handle
    // is what BountyCreated uses to bring the new bounty into view; the rows
    // must stay DIRECT children so scroll_to_item's indices match the list.
    v_flex()
        .id("bounty-list")
        .w(px(300.))
        .flex_shrink_0()
        .min_h(px(0.))
        .overflow_y_scroll()
        .track_scroll(&shell.bounty_list_scroll)
        .gap(px(8.))
        .pb(px(12.))
        .children(body)
        .into_any_element()
}

// ----------------------------------------------------------------- detail

fn claim_card(claim: &Claim) -> AnyElement {
    let now = timefmt::now_unix();
    let claim_ts = claim.event.created_at.unwrap_or(0);
    let mut rows: Vec<AnyElement> = vec![timeline_row(
        TEXT_MUTED,
        "Claim submitted".into(),
        Some(format!(
            "{} UTC · {}",
            timefmt::hhmmss_utc(claim_ts),
            timefmt::relative(now, claim_ts)
        )),
    )];

    if let Some(reason) = claim
        .auto_pay_blocked_reason
        .as_deref()
        .filter(|r| !r.is_empty())
    {
        rows.push(timeline_row(
            AMBER,
            format!("Auto-pay blocked: {reason}"),
            None,
        ));
    }

    match &claim.auto_payment {
        Some(row) => {
            // The poll can miss the short-lived `attempting` state entirely.
            // The row's created_at IS when the attempt started, so the beat
            // renders retroactively and the timeline never jumps straight
            // from "Claim submitted" to "Paid".
            if row.state != "attempting"
                && let Some(started) = row.created_at.filter(|ts| *ts > 0) {
                    rows.push(timeline_row(
                        ACCENT,
                        "Auto-pay attempted".into(),
                        Some(format!(
                            "{} UTC · {}",
                            timefmt::hhmmss_utc(started),
                            timefmt::relative(now, started)
                        )),
                    ));
                }
            let amount = row
                .amount_sats
                .map(|sats| format!("{} sats", timefmt::fmt_sats(sats)));
            let ts = row.updated_at.or(row.created_at);
            let meta = ts.map(|ts| {
                format!(
                    "{} UTC · {}{}",
                    timefmt::hhmmss_utc(ts),
                    timefmt::relative(now, ts),
                    amount
                        .as_deref()
                        .map(|a| format!(" · {a}"))
                        .unwrap_or_default()
                )
            });
            let why = row
                .reason
                .as_deref()
                .filter(|r| !r.is_empty())
                .map(|r| format!(" ({r})"))
                .unwrap_or_default();
            let (dot, text) = match row.state.as_str() {
                "attempting" => (ACCENT, "Auto-pay attempting…".to_string()),
                "paid" => (AMBER, "Paid — waiting for the zap receipt".to_string()),
                "settled" => (GREEN, "Payment settled".to_string()),
                "paid_unreceipted" => (AMBER, format!("Paid, no receipt{why}")),
                "failed" => (RED, format!("Auto-pay failed{why}")),
                other => (TEXT_MUTED, format!("Payment state: {other}{why}")),
            };
            rows.push(timeline_row(dot, text, meta));
        }
        None => rows.push(timeline_row(
            TEXT_DIM,
            "No auto-payment row yet".into(),
            None,
        )),
    }

    if let Some(receipt) = claim.receipt() {
        let secs = receipt.created_at.saturating_sub(claim_ts);
        let amount = receipt
            .amount_sats()
            .map(|sats| format!("{} sats · ", timefmt::fmt_sats(sats)))
            .unwrap_or_default();
        rows.push(timeline_row(
            GREEN,
            format!("Zap receipt {}", short_id(&receipt.receipt_id)),
            Some(format!("{amount}{secs} s after the claim")),
        ));
    }

    v_flex()
        .px(px(14.))
        .py(px(11.))
        .rounded(px(10.))
        .bg(rgb(BG_APP))
        .border_1()
        .border_color(rgb(BORDER))
        .child(
            h_flex()
                .gap(px(8.))
                .items_center()
                .mb(px(4.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(claim.item_name().to_string())),
                )
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(10.5))
                        .text_color(rgb(TEXT_DIM))
                        .child(SharedString::from(format!(
                            "by {}",
                            short_id(&claim.event.pubkey)
                        ))),
                ),
        )
        .children(rows)
        .into_any_element()
}

fn fact(label: &'static str, value: String) -> AnyElement {
    v_flex()
        .child(
            div()
                .text_size(px(9.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_DIM))
                .child(label),
        )
        .child(
            div()
                .font_family(MONO)
                .text_size(px(12.5))
                .child(SharedString::from(value)),
        )
        .into_any_element()
}

fn detail_pane(shell: &Shell) -> AnyElement {
    let Some(selected) = shell.selected.as_deref() else {
        return div()
            .flex_1()
            .py(px(20.))
            .text_size(px(12.5))
            .text_color(rgb(TEXT_DIM))
            .child("Select a bounty (↑/↓).")
            .into_any_element();
    };

    let detail = shell.details.get(selected);
    let Some(bounty) = shell.selected_bounty() else {
        return div()
            .flex_1()
            .py(px(20.))
            .text_size(px(12.5))
            .text_color(rgb(TEXT_DIM))
            .child("Loading bounty…")
            .into_any_element();
    };

    let status = bounty.effective_status().to_string();
    let mut facts: Vec<AnyElement> = vec![fact(
        "REWARD",
        format!(
            "{} sats{}",
            timefmt::fmt_sats(bounty.amount_sats),
            if bounty.reward_per_item_on() {
                " / item"
            } else {
                ""
            }
        ),
    )];
    if let Some(cap) = bounty.bounty_cap_sats {
        facts.push(fact("CAP", format!("{} sats", timefmt::fmt_sats(cap))));
    }
    if let Some(rank) = bounty.auto_pay_min_rank {
        facts.push(fact("MIN RANK", rank.to_string()));
    }
    if let Some(state) = &bounty.payment_state {
        facts.push(fact(
            "SLOTS",
            format!(
                "{} paid · {} open",
                state.paid_reward_count, state.open_reward_slots
            ),
        ));
    }

    let claims: Vec<AnyElement> = match detail {
        None => vec![
            div()
                .py(px(8.))
                .text_size(px(12.))
                .text_color(rgb(TEXT_DIM))
                .child(if shell.selected_is_optimistic() {
                    // The header above renders the just-submitted values; be
                    // honest that the server has not echoed them back yet.
                    "Created — syncing with the instance…"
                } else {
                    "Loading claims…"
                })
                .into_any_element(),
        ],
        Some(detail) if detail.claims.is_empty() => vec![
            div()
                .py(px(8.))
                .text_size(px(12.))
                .text_color(rgb(TEXT_DIM))
                .child(
                    "No claims yet. Claims from keys the issuer's web of trust ranks \
                     below 2 stay on the relay but are not listed here.",
                )
                .into_any_element(),
        ],
        Some(detail) => detail.claims.iter().map(claim_card).collect(),
    };
    let claim_count = detail.map(|d| d.claims.len()).unwrap_or(0);

    // Header (title / status / metrics / "Claims" heading) is laid out as
    // fixed children; ONLY the claim list below them scrolls — the header
    // stays pinned while a long claim list moves under it.
    v_flex()
        .flex_1()
        .min_w(px(0.))
        .min_h(px(0.))
        .bg(rgb(BG_CARD))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(14.))
        .px(px(20.))
        .py(px(18.))
        .gap(px(12.))
        .child(
            h_flex()
                .gap(px(10.))
                .items_center()
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::BOLD)
                        .child(SharedString::from(
                            coordinate_dtag(&bounty.list_coordinate).to_string(),
                        )),
                )
                .child(pill(status.clone(), status_hue(&status)))
                .child(if bounty.auto_pay_on() {
                    pill("auto-pay", GREEN)
                } else {
                    pill("manual pay", TEXT_MUTED)
                }),
        )
        .when_some(
            bounty.criteria.clone().filter(|c| !c.is_empty()),
            |this, criteria| {
                this.child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child(SharedString::from(criteria)),
                )
            },
        )
        .child(h_flex().gap(px(22.)).items_start().children(facts))
        .child(
            h_flex()
                .gap(px(8.))
                .items_baseline()
                .mt(px(4.))
                .child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Claims"),
                )
                .when(claim_count > 0, |this| {
                    this.child(count_badge(claim_count.to_string()))
                }),
        )
        .child(
            v_flex()
                .id("claim-list")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .gap(px(8.))
                .pb(px(8.))
                .children(claims),
        )
        .into_any_element()
}

// ------------------------------------------------------------------ forms

fn form_card(
    title: String,
    rows: Vec<AnyElement>,
    error: Option<String>,
    busy: Option<&'static str>,
    hint_text: &'static str,
    cx: &mut Context<Shell>,
) -> AnyElement {
    v_flex()
        .w_full()
        .bg(rgb(BG_CARD))
        .border_1()
        .border_color(rgb(BORDER_3))
        .rounded(px(14.))
        .px(px(20.))
        .py(px(18.))
        .gap(px(10.))
        .mb(px(16.))
        .child(
            h_flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(15.))
                        .font_weight(FontWeight::BOLD)
                        .child(SharedString::from(title)),
                )
                .child(
                    div()
                        .id("cancel-form")
                        .px(px(11.))
                        .py(px(5.))
                        .rounded(px(7.))
                        .border_1()
                        .border_color(rgb(BORDER_3))
                        .cursor_pointer()
                        .text_size(px(11.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child("Cancel (esc)")
                        .on_click(cx.listener(|this, _, window, cx| this.close_forms(window, cx))),
                ),
        )
        .children(rows)
        .when_some(error, |this, error| {
            this.child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(RED))
                    .child(SharedString::from(error)),
            )
        })
        .child(hint(busy.unwrap_or(hint_text)))
        .into_any_element()
}

fn labeled_input(label: &'static str, input: AnyElement) -> AnyElement {
    v_flex()
        .gap(px(3.))
        .child(
            div()
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_DIM))
                .child(label),
        )
        .child(input)
        .into_any_element()
}

fn forms(shell: &Shell, cx: &mut Context<Shell>) -> Option<AnyElement> {
    if let Some(form) = &shell.bounty_form {
        let rows: Vec<AnyElement> = BOUNTY_FIELD_DEFS
            .iter()
            .zip(&form.fields)
            .map(|((label, _, _), field)| {
                labeled_input(label, Input::new(field).into_any_element())
            })
            .collect();
        return Some(form_card(
            "New DList + bounty".into(),
            rows,
            form.error.clone(),
            if form.checking_list {
                Some("Checking the list…")
            } else {
                form.submitting.then_some("Publishing…")
            },
            "Enter advances · Enter on the last field publishes the 39998 list, then creates the auto-pay bounty · esc cancels",
            cx,
        ));
    }
    if let Some(form) = &shell.claim_form {
        let dtag = coordinate_dtag(&form.coordinate).to_string();
        // Several bounties can share a list, so name the reward too.
        let bounty = shell
            .selected_bounty()
            .filter(|b| b.id == form.bounty_id);
        let reward = bounty
            .map(|b| format!(" · {} sats per item", timefmt::fmt_sats(b.amount_sats)))
            .unwrap_or_default();
        // Say up front how the payout works and why a new key's claim may
        // not show: the toast after publishing is too brief to carry it.
        // The auto-pay rank and the listing rank in RANK_NOTE are different
        // thresholds; keep the payout sentence rank-free so they never read
        // as a contradiction.
        let payout = bounty
            .map(|b| {
                let sats = timefmt::fmt_sats(b.amount_sats);
                if b.auto_pay_on() {
                    format!("Auto-pay: {sats} sats within about a minute. ")
                } else {
                    format!("Manual pay: the issuer reviews the claim and sends {sats} sats by hand. ")
                }
            })
            .unwrap_or_default();
        let rows = vec![
            labeled_input("Item name", Input::new(&form.name).into_any_element()),
            hint(format!("{payout}{RANK_NOTE}")).into_any_element(),
        ];
        return Some(form_card(
            format!("Claim an item on {dtag}{reward}"),
            rows,
            form.error.clone(),
            form.submitting.then_some("Publishing…"),
            "Enter publishes the kind-39999 claim through the instance · esc cancels",
            cx,
        ));
    }
    None
}

// ------------------------------------------------------------------ screen

pub fn render(shell: &Shell, cx: &mut Context<Shell>) -> impl IntoElement {
    let action_label = if shell.is_active_issuer() {
        "+ New DList + bounty (⌘N)"
    } else {
        "+ Submit a bounty claim (⌘N)"
    };
    // No account: the CTA dims, and a click (like ⌘N) opens the account
    // screen instead of a form that cannot submit.
    let keyless = shell.active_view().is_none();

    v_flex()
        .flex_1()
        .min_h(px(0.))
        .child(
            h_flex()
                .gap(px(14.))
                .items_center()
                .mb(px(18.))
                .child(
                    div()
                        .text_size(px(21.))
                        .font_weight(FontWeight::BOLD)
                        .child("Bounties"),
                )
                .child(hint("↑/↓ select · ⌘N new · ⌘R refresh · ⌘] switch account"))
                .child(div().flex_1())
                .child(
                    div()
                        .id("new-on-bounties")
                        .flex_shrink_0()
                        .px(px(16.))
                        .py(px(8.))
                        .rounded(px(9.))
                        .cursor_pointer()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(!keyless, |this| {
                            this.bg(grad(ACCENT, ACCENT_DEEP)).text_color(rgb(BG_RAIL))
                        })
                        .when(keyless, |this| {
                            this.border_1()
                                .border_color(rgb(BORDER_3))
                                .text_color(rgb(TEXT_DIM))
                        })
                        .child(action_label)
                        .on_click(cx.listener(|this, _, window, cx| this.new_item(window, cx))),
                ),
        )
        .children(forms(shell, cx))
        .child(
            // Both panes run the remaining height and scroll independently;
            // the screen itself never scrolls (see Shell::main).
            h_flex()
                .flex_1()
                .min_h(px(0.))
                .gap(px(16.))
                .items_stretch()
                .child(list_pane(shell, cx))
                .child(detail_pane(shell)),
        )
}
