//! The payments ledger: one row per claim, with where its money stands.
//!
//! Two kinds of bounty feed it. On bounties the issuer's own app pays, the
//! status comes from the claim's facts, its recorded attempt and its
//! `autopay::plan` decision. On bounties the server's auto-payer pays, it
//! comes from the server's claim row. Both end in the same seven statuses so
//! one screen, one debts card and one badge can sum them.
//!
//! Everything here is pure; the caller reads the clock and the stores.

use crate::api::{Bounty, Claim};
use crate::autopay::{self, rank_text, AttemptState, ClaimFacts, Hold, Policy};

/// Which way the sats go, seen from the signed-in account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    PaidByMe,
    PaidToMe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// A verified receipt exists.
    Paid,
    /// The wallet confirmed the payment; no verified receipt.
    PaidNoReceipt,
    /// A payment is out right now.
    Paying,
    /// A payment went out and its outcome was never learned. It may have
    /// paid, so it holds its slot and is never sent again.
    Unknown,
    /// The rules say this claim is not paid.
    NotPayable,
    /// The bounty ended before the claim was paid.
    Expired,
    /// Payable and not paid yet.
    Owed,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Paid => "Paid",
            Status::PaidNoReceipt => "Paid, no receipt",
            Status::Paying => "Paying",
            Status::Unknown => "Unknown",
            Status::NotPayable => "Not payable",
            Status::Expired => "Expired",
            Status::Owed => "Owed",
        }
    }
}

/// One claim on one bounty the issuer's app pays, as the caller gathered it.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimView<'a> {
    pub fact: &'a ClaimFacts,
    /// This claim's entry from `autopay::plan`.
    pub decision: &'a Result<(), Hold>,
    /// The issuer's attempt was last touched inside the receipt grace window.
    pub attempt_fresh: bool,
    pub bounty_expired: bool,
    /// Why a payable claim still waits (no Lightning address, low balance...).
    pub note: Option<&'a str>,
}

const SENT_NO_ANSWER: &str = "sent, no answer yet";
const WAITING_FOR_RECEIPT: &str = "waiting for the receipt";
const NO_RECEIPT: &str = "no receipt arrived";
const LAST_FAILED: &str = "last payment failed";
const ENDED_UNPAID: &str = "bounty ended unpaid";

/// The claim's status and a short "why" line. Money already moved (or maybe
/// moved) wins over the rules and over expiry, so a payment in doubt never
/// reads as owed or expired and is never paid twice.
pub fn classify(view: &ClaimView) -> (Status, Option<String>) {
    let fact = view.fact;
    if fact.paid {
        return (Status::Paid, None);
    }
    match fact.attempt {
        Some(AttemptState::Paid) => {
            let why = if view.attempt_fresh { WAITING_FOR_RECEIPT } else { NO_RECEIPT };
            return (Status::PaidNoReceipt, Some(why.to_string()));
        }
        Some(AttemptState::Sending) if view.attempt_fresh => return (Status::Paying, None),
        Some(AttemptState::Sending | AttemptState::Unknown) => {
            return (Status::Unknown, Some(SENT_NO_ANSWER.to_string()));
        }
        Some(AttemptState::Failed) | None => {}
    }
    let not_payable = match view.decision {
        Err(Hold::RankTooLow { rank: Some(rank), min }) => {
            Some(format!("rank {}, bounty needs {min}", rank_text(*rank)))
        }
        Err(Hold::RankTooLow { rank: None, min }) => Some(format!("not ranked, bounty needs {min}")),
        Err(Hold::NoSlots) => Some("no reward slots left".to_string()),
        Err(Hold::OverLimit) => Some("per-account limit reached".to_string()),
        Err(Hold::OwnClaim) => Some("the issuer's own claim".to_string()),
        _ => None,
    };
    if let Some(why) = not_payable {
        return (Status::NotPayable, Some(why));
    }
    if view.bounty_expired {
        return (Status::Expired, Some(ENDED_UNPAID.to_string()));
    }
    let why = if fact.attempt == Some(AttemptState::Failed) {
        Some(LAST_FAILED.to_string())
    } else {
        match view.decision {
            Err(Hold::AutoPayOff) => Some("manual pay".to_string()),
            Ok(()) => view.note.map(str::to_string),
            Err(_) => None,
        }
    };
    (Status::Owed, why)
}

/// `autopay::plan` for the ledger. `decide` stops at a failed attempt before
/// the cap, the limit and the rank, but a failed attempt holds no slot, so
/// here it plans as unattempted: a failed claim the rules would no longer
/// pay reads Not payable, not Owed. Classify with the real facts.
pub fn plan(policy: &Policy, facts: &[ClaimFacts]) -> Vec<(String, Result<(), Hold>)> {
    let cleared: Vec<ClaimFacts> = facts
        .iter()
        .map(|f| ClaimFacts {
            attempt: f.attempt.filter(|a| *a != AttemptState::Failed),
            ..f.clone()
        })
        .collect();
    autopay::plan(policy, &cleared)
}

/// The status of a claim on a bounty the server's auto-payer pays, from the
/// server's claim row. The server's `paymentStatus` is "paid" (a receipt),
/// the auto-payment state while that row holds the slot ("attempting",
/// "paid", "settled", "paid_unreceipted", or "failed" for an ambiguous
/// send), "payable" or "closed" (magic-carpet-v2/src/lib/bounty-policy.js
/// `annotateClaimsWithPaymentState`). The auto-payment row is read first
/// because the status reuses its words: "paid" alone is ambiguous.
pub fn classify_server(claim: &Claim, bounty_expired: bool) -> (Status, Option<String>) {
    if claim.zap_receipt.is_some() {
        return (Status::Paid, None);
    }
    let payment = claim.auto_payment.as_ref();
    let failed = match payment.map(|p| p.state.as_str()) {
        // The server saw the receipt.
        Some("settled") => return (Status::Paid, None),
        Some("paid") => return (Status::PaidNoReceipt, Some(WAITING_FOR_RECEIPT.to_string())),
        Some("paid_unreceipted") => return (Status::PaidNoReceipt, Some(NO_RECEIPT.to_string())),
        Some("attempting") => return (Status::Paying, None),
        // An ambiguous send holds its slot on the server too.
        Some("failed")
            if payment
                .and_then(|p| p.reason.as_deref())
                .is_some_and(|r| r.starts_with("ambiguous_send:")) =>
        {
            return (Status::Unknown, Some(SENT_NO_ANSWER.to_string()));
        }
        Some("failed") => true,
        _ => false,
    };
    if claim.payment_status.as_deref() == Some("closed") {
        // The row's closedReason (cap, contributor limit) is not kept.
        return (Status::NotPayable, Some("no reward slot for this claim".to_string()));
    }
    if bounty_expired {
        return (Status::Expired, Some(ENDED_UNPAID.to_string()));
    }
    let why = if failed {
        Some(LAST_FAILED.to_string())
    } else {
        claim
            .auto_pay_blocked_reason
            .as_deref()
            .filter(|r| !r.is_empty())
            .map(str::to_string)
    };
    (Status::Owed, why)
}

/// One line of the ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub direction: Direction,
    pub bounty_id: String,
    pub list_coordinate: String,
    pub claim_address: String,
    pub claim_event_id: String,
    /// What the claimant named.
    pub item: String,
    /// Pubkey hex: the claimant for `PaidByMe`, the issuer for `PaidToMe`.
    pub counterparty: String,
    pub sats: u64,
    pub status: Status,
    pub why: Option<String>,
    /// Unix seconds, for sorting and the "when" column.
    pub at: u64,
}

/// One account's payments, as `nostr::Command::FetchLedger` builds them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ledger {
    /// Both directions, newest first.
    pub rows: Vec<Row>,
    /// The account's own bounties, counted the way the app pays them, so
    /// [`pledged`] and [`debts`] work on them.
    pub issued: Vec<Bounty>,
    /// Other issuers' bounties the account has a claim on.
    pub claimed: Vec<Bounty>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub owed: u64,
    /// `Paying` and `Unknown`: sats that may already have left.
    pub paying: u64,
    pub paid: u64,
}

fn sum(rows: &[Row], keep: impl Fn(&Row) -> bool) -> u64 {
    rows.iter().filter(|r| keep(r)).map(|r| r.sats).sum()
}

pub fn totals(rows: &[Row], direction: Direction) -> Totals {
    let mine = |r: &Row, statuses: &[Status]| r.direction == direction && statuses.contains(&r.status);
    Totals {
        owed: sum(rows, |r| mine(r, &[Status::Owed])),
        paying: sum(rows, |r| mine(r, &[Status::Paying, Status::Unknown])),
        paid: sum(rows, |r| mine(r, &[Status::Paid, Status::PaidNoReceipt])),
    }
}

/// Sats `me` still promises on its open bounties: the open slots times the
/// reward. On a bounty the app pays, an owed claim holds no slot yet, so its
/// sats come off here or pledged plus owed would count it twice. The server
/// already takes a slot off `openRewardSlots` for every payable claim (the
/// rows `classify_server` calls Owed), so nothing comes off its bounties.
pub fn pledged(bounties: &[Bounty], rows: &[Row], me: &str) -> u64 {
    bounties
        .iter()
        .filter(|b| b.issuer_pubkey == me && b.effective_status() == "open")
        .map(|b| {
            let open = b.payment_state.as_ref().map_or(0, |s| s.open_reward_slots);
            let owed = if b.server_auto_pay_on() {
                0
            } else {
                sum(rows, |r| {
                    r.direction == Direction::PaidByMe && r.status == Status::Owed && r.bounty_id == b.id
                })
            };
            open.saturating_mul(b.amount_sats).saturating_sub(owed)
        })
        .sum()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Debts {
    pub pledged_outstanding: u64,
    /// Owed, paying and unknown claims `me` pays.
    pub owed_unpaid: u64,
}

impl Debts {
    pub fn total(&self) -> u64 {
        self.pledged_outstanding + self.owed_unpaid
    }
}

fn unsettled(row: &Row) -> bool {
    row.direction == Direction::PaidByMe
        && matches!(row.status, Status::Owed | Status::Paying | Status::Unknown)
}

pub fn debts(bounties: &[Bounty], rows: &[Row], me: &str) -> Debts {
    Debts {
        pledged_outstanding: pledged(bounties, rows, me),
        owed_unpaid: sum(rows, unsettled),
    }
}

/// The nav badge: claims `me` pays that are not settled.
pub fn unsettled_count(rows: &[Row]) -> usize {
    rows.iter().filter(|r| unsettled(r)).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AutoPaymentRow, RawEvent};

    fn fact() -> ClaimFacts {
        ClaimFacts {
            address: "bob:item".into(),
            event_id: "e1".into(),
            claimant: "bob".into(),
            created_at: 1,
            paid: false,
            attempt: None,
            rank: Some(5.0),
        }
    }

    fn run(
        fact: &ClaimFacts,
        decision: Result<(), Hold>,
        fresh: bool,
        expired: bool,
    ) -> (Status, Option<String>) {
        classify(&ClaimView {
            fact,
            decision: &decision,
            attempt_fresh: fresh,
            bounty_expired: expired,
            note: Some("no Lightning address"),
        })
    }

    fn with(attempt: AttemptState) -> ClaimFacts {
        ClaimFacts { attempt: Some(attempt), ..fact() }
    }

    fn why(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn a_receipt_is_paid_and_beats_everything() {
        let paid = ClaimFacts { paid: true, attempt: Some(AttemptState::Unknown), ..fact() };
        assert_eq!(run(&paid, Err(Hold::Paid), false, true), (Status::Paid, None));
        assert_eq!(run(&paid, Err(Hold::NoSlots), true, true), (Status::Paid, None));
    }

    #[test]
    fn a_wallet_confirmed_attempt_is_paid_without_receipt() {
        let f = with(AttemptState::Paid);
        let decision = Err(Hold::Attempted(AttemptState::Paid));
        assert_eq!(run(&f, decision, true, false), (Status::PaidNoReceipt, why("waiting for the receipt")));
        assert_eq!(run(&f, decision, false, true), (Status::PaidNoReceipt, why("no receipt arrived")));
    }

    #[test]
    fn a_fresh_send_is_paying() {
        let f = with(AttemptState::Sending);
        assert_eq!(run(&f, Err(Hold::Attempted(AttemptState::Sending)), true, false), (Status::Paying, None));
    }

    #[test]
    fn a_stale_send_or_unknown_outcome_is_unknown_even_when_expired() {
        let stale = with(AttemptState::Sending);
        let decision = Err(Hold::Attempted(AttemptState::Sending));
        assert_eq!(run(&stale, decision, false, false), (Status::Unknown, why("sent, no answer yet")));
        let unknown = with(AttemptState::Unknown);
        let decision = Err(Hold::Attempted(AttemptState::Unknown));
        assert_eq!(run(&unknown, decision, true, true), (Status::Unknown, why("sent, no answer yet")));
    }

    #[test]
    fn rule_holds_are_not_payable_even_when_expired() {
        let f = fact();
        let cases = [
            (Hold::RankTooLow { rank: Some(2.0), min: 3 }, "rank 2, bounty needs 3"),
            (Hold::RankTooLow { rank: Some(2.5), min: 3 }, "rank 2.5, bounty needs 3"),
            (Hold::RankTooLow { rank: None, min: 3 }, "not ranked, bounty needs 3"),
            (Hold::NoSlots, "no reward slots left"),
            (Hold::OverLimit, "per-account limit reached"),
            (Hold::OwnClaim, "the issuer's own claim"),
        ];
        for (hold, text) in cases {
            assert_eq!(run(&f, Err(hold), false, false), (Status::NotPayable, why(text)));
            assert_eq!(run(&f, Err(hold), false, true), (Status::NotPayable, why(text)));
        }
    }

    #[test]
    fn an_expired_bounty_with_no_rule_against_the_claim_is_expired() {
        assert_eq!(run(&fact(), Ok(()), false, true), (Status::Expired, why("bounty ended unpaid")));
        let failed = with(AttemptState::Failed);
        let decision = Err(Hold::Attempted(AttemptState::Failed));
        assert_eq!(run(&failed, decision, false, true), (Status::Expired, why("bounty ended unpaid")));
        assert_eq!(run(&fact(), Err(Hold::AutoPayOff), false, true).0, Status::Expired);
    }

    #[test]
    fn otherwise_the_claim_is_owed_and_says_why() {
        assert_eq!(run(&fact(), Ok(()), false, false), (Status::Owed, why("no Lightning address")));
        assert_eq!(run(&fact(), Err(Hold::AutoPayOff), false, false), (Status::Owed, why("manual pay")));
        let failed = with(AttemptState::Failed);
        let decision = Err(Hold::Attempted(AttemptState::Failed));
        assert_eq!(run(&failed, decision, true, false), (Status::Owed, why("last payment failed")));
        let quiet = ClaimView {
            fact: &fact(),
            decision: &Ok(()),
            attempt_fresh: false,
            bounty_expired: false,
            note: None,
        };
        assert_eq!(classify(&quiet), (Status::Owed, None));
    }

    #[test]
    fn a_failed_attempt_is_classified_by_the_plan_decision() {
        let failed = with(AttemptState::Failed);
        assert_eq!(run(&failed, Err(Hold::NoSlots), false, false), (Status::NotPayable, why("no reward slots left")));
        assert_eq!(run(&failed, Ok(()), false, false), (Status::Owed, why("last payment failed")));
    }

    #[test]
    fn a_failed_attempt_holds_no_slot_in_the_ledger() {
        let one_slot = Policy { issuer: "me".into(), auto_pay: true, min_rank: 2, slots: 1, per_npub: Some(1) };
        let claim = |n: u64, claimant: &str| ClaimFacts {
            address: format!("{claimant}:item"),
            event_id: format!("e{n}"),
            claimant: claimant.into(),
            created_at: n,
            ..fact()
        };
        let first = ClaimFacts { attempt: Some(AttemptState::Failed), ..claim(1, "bob") };
        let statuses = |facts: &[ClaimFacts]| -> Vec<(Status, Option<String>)> {
            let decisions = plan(&one_slot, facts);
            facts
                .iter()
                .map(|f| {
                    let (_, decision) = decisions.iter().find(|(a, _)| *a == f.address).unwrap();
                    run(f, *decision, false, false)
                })
                .collect()
        };

        // Before claim 2 is paid: the older claim takes the one slot.
        let before = [first.clone(), claim(2, "carol")];
        assert_eq!(
            statuses(&before),
            [(Status::Owed, why("last payment failed")), (Status::NotPayable, why("no reward slots left"))]
        );

        // Claim 2 paid: claim 1 has no slot left.
        let after = [first, ClaimFacts { paid: true, ..claim(2, "carol") }];
        assert_eq!(
            statuses(&after),
            [(Status::NotPayable, why("no reward slots left")), (Status::Paid, None)]
        );
    }

    #[test]
    fn labels_read_as_the_screen_shows_them() {
        assert_eq!(Status::PaidNoReceipt.label(), "Paid, no receipt");
        assert_eq!(Status::NotPayable.label(), "Not payable");
        assert_eq!(Status::Owed.label(), "Owed");
    }

    fn event(id: &str) -> RawEvent {
        RawEvent {
            id: id.into(),
            pubkey: "bob".into(),
            kind: Some(39999),
            created_at: Some(1),
            tags: vec![],
            content: "Memphis".into(),
            sig: None,
        }
    }

    fn server(status: &str, auto: Option<(&str, Option<&str>)>) -> Claim {
        let mut claim = Claim::from_event(event("e1"));
        claim.payment_status = Some(status.into());
        claim.auto_payment = auto.map(|(state, reason)| AutoPaymentRow {
            id: "p1".into(),
            claim_event_id: "e1".into(),
            bounty_id: "b1".into(),
            issuer_pubkey: None,
            claimant_pubkey: None,
            claim_address: None,
            amount_sats: Some(100),
            state: state.into(),
            reason: reason.map(str::to_string),
            created_at: None,
            updated_at: None,
        });
        claim
    }

    #[test]
    fn server_rows_map_to_the_same_statuses() {
        let mut receipt = server("paid", None);
        receipt.zap_receipt = Some(event("r1"));
        assert_eq!(classify_server(&receipt, true), (Status::Paid, None));
        assert_eq!(classify_server(&server("settled", Some(("settled", None))), false), (Status::Paid, None));
        assert_eq!(
            classify_server(&server("paid", Some(("paid", None))), true),
            (Status::PaidNoReceipt, why("waiting for the receipt"))
        );
        assert_eq!(
            classify_server(&server("paid_unreceipted", Some(("paid_unreceipted", Some("receipt_timeout")))), false),
            (Status::PaidNoReceipt, why("no receipt arrived"))
        );
        assert_eq!(classify_server(&server("attempting", Some(("attempting", None))), true), (Status::Paying, None));
        assert_eq!(
            classify_server(&server("failed", Some(("failed", Some("ambiguous_send:timeout")))), true),
            (Status::Unknown, why("sent, no answer yet"))
        );
        assert_eq!(
            classify_server(&server("closed", Some(("failed", Some("no_route")))), true),
            (Status::NotPayable, why("no reward slot for this claim"))
        );
        assert_eq!(classify_server(&server("payable", None), true), (Status::Expired, why("bounty ended unpaid")));
        assert_eq!(
            classify_server(&server("payable", Some(("failed", Some("no_route")))), false),
            (Status::Owed, why("last payment failed"))
        );
        let mut blocked = server("payable", None);
        blocked.auto_pay_blocked_reason = Some("daily limit reached".into());
        assert_eq!(classify_server(&blocked, false), (Status::Owed, why("daily limit reached")));
        assert_eq!(classify_server(&server("something_new", None), false), (Status::Owed, None));
        assert_eq!(classify_server(&server("something_new", None), true).0, Status::Expired);
    }

    fn row(bounty: &str, direction: Direction, status: Status, sats: u64) -> Row {
        Row {
            direction,
            bounty_id: bounty.into(),
            list_coordinate: "39998:me:list".into(),
            claim_address: "bob:item".into(),
            claim_event_id: "e1".into(),
            item: "Memphis".into(),
            counterparty: "bob".into(),
            sats,
            status,
            why: None,
            at: 1,
        }
    }

    fn rows() -> Vec<Row> {
        use Direction::*;
        vec![
            row("b1", PaidByMe, Status::Owed, 100),
            row("b1", PaidByMe, Status::Paying, 100),
            row("b1", PaidByMe, Status::Unknown, 100),
            row("b1", PaidByMe, Status::Paid, 100),
            row("b1", PaidByMe, Status::PaidNoReceipt, 100),
            row("b1", PaidByMe, Status::NotPayable, 100),
            row("b1", PaidByMe, Status::Expired, 100),
            row("b2", PaidByMe, Status::Owed, 50),
            row("x", PaidToMe, Status::Owed, 7),
            row("x", PaidToMe, Status::Paid, 9),
        ]
    }

    fn bounty(id: &str, issuer: &str, status: &str, amount: u64, open: u64) -> Bounty {
        serde_json::from_value(serde_json::json!({
            "id": id, "issuer_pubkey": issuer, "list_coordinate": "39998:me:list",
            "amount_sats": amount, "derivedStatus": status,
            "paymentState": { "openRewardSlots": open },
        }))
        .unwrap()
    }

    #[test]
    fn totals_sum_one_direction() {
        assert_eq!(totals(&rows(), Direction::PaidByMe), Totals { owed: 150, paying: 200, paid: 200 });
        assert_eq!(totals(&rows(), Direction::PaidToMe), Totals { owed: 7, paying: 0, paid: 9 });
    }

    #[test]
    fn pledged_takes_owed_claims_off_the_open_slots() {
        let bounties = vec![
            bounty("b1", "me", "open", 100, 3),  // 300 - 100 owed
            bounty("b2", "me", "open", 50, 0),   // 0 - 50 owed, saturates
            bounty("b3", "me", "closed", 100, 5), // not open
            bounty("b4", "them", "open", 100, 5), // not mine
        ];
        assert_eq!(pledged(&bounties, &rows(), "me"), 200);
        let d = debts(&bounties, &rows(), "me");
        assert_eq!(d, Debts { pledged_outstanding: 200, owed_unpaid: 350 });
        assert_eq!(d.total(), 550);
    }

    #[test]
    fn pledged_leaves_owed_claims_on_server_paid_bounties_alone() {
        // 5 slots x 100 sats; the server's 3 open slots already exclude its
        // 2 payable claims.
        let b: Bounty = serde_json::from_value(serde_json::json!({
            "id": "b1", "issuer_pubkey": "me", "list_coordinate": "39998:me:list",
            "amount_sats": 100, "bounty_cap_sats": 500, "derivedStatus": "open", "auto_pay": 1,
            "paymentState": { "openRewardSlots": 3 },
        }))
        .unwrap();
        assert!(b.server_auto_pay_on());
        let owed = vec![
            row("b1", Direction::PaidByMe, Status::Owed, 100),
            row("b1", Direction::PaidByMe, Status::Owed, 100),
        ];
        assert_eq!(pledged(std::slice::from_ref(&b), &owed, "me"), 300);
        assert_eq!(debts(&[b], &owed, "me").total(), 500);
    }

    #[test]
    fn the_badge_counts_unsettled_claims_i_pay() {
        assert_eq!(unsettled_count(&rows()), 4);
        assert_eq!(unsettled_count(&[]), 0);
    }
}
