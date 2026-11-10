//! Issuer-side auto-pay, decided offline: which claims on a bounty the
//! issuer's own app pays from the issuer's own Coinos wallet.
//!
//! The server only pays bounties its allowlist lets it pay, and it ranks
//! claimants by the house's scores, so for any other issuer the app does the
//! work: claims come from the relay, "paid" comes from zap receipts the
//! issuer signed the request for, and the rank is the issuer's own (its
//! Treasure Map's kind-30382). The slot and per-npub arithmetic mirrors
//! magic-carpet-v2/src/lib/bounty-policy.js so the two never disagree on a
//! bounty both can see.
//!
//! Everything here is pure; the runtime (`nostr::autopay_tick`) gathers the
//! facts and acts on the decisions.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::api::Bounty;

/// One payment attempt as the account store keeps it. Recorded as `Sending`
/// BEFORE the payment request goes out, so a crash, a restart or a second
/// app instance sees the attempt and never pays the claim again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    /// Recorded, and the payment request is (or was) out. Still `Sending`
    /// after the app stopped means the outcome was never learned.
    Sending,
    /// Coinos accepted the payment.
    Paid,
    /// Coinos refused, or the request never reached it: no sats moved.
    Failed,
    /// The request went out and no answer came back. The sats may be gone.
    Unknown,
}

impl AttemptState {
    /// Whether the attempt holds a reward slot. Only a definite failure
    /// frees one; an unknown outcome may have paid.
    pub fn holds_slot(self) -> bool {
        !matches!(self, AttemptState::Failed)
    }
}

/// The bounty's payout rules as the issuer's app applies them.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    pub issuer: String,
    /// The issuer turned auto-pay on for this bounty (the account store).
    pub auto_pay: bool,
    pub min_rank: u64,
    /// How many rewards the bounty pays in all.
    pub slots: u64,
    /// How many rewards one claimant may get; `None` is no limit.
    pub per_npub: Option<u64>,
}

/// `(slots, per_npub)` of a bounty, as bounty-policy.js `bountyPolicy`
/// computes them: the cap (default the reward) over the reward, at least
/// one; per-item bounties allow `max_rewards_per_npub` per claimant (no
/// limit when unset), anything else one.
pub fn slots_and_limit(bounty: &Bounty) -> (u64, Option<u64>) {
    let amount = bounty.amount_sats;
    let cap = bounty.bounty_cap_sats.filter(|c| *c > 0).unwrap_or(amount);
    let slots = cap.checked_div(amount).map_or(0, |n| n.max(1));
    let per_npub = if bounty.reward_per_item_on() {
        bounty.max_rewards_per_npub.filter(|n| *n > 0)
    } else {
        Some(1)
    };
    (slots, per_npub)
}

impl Policy {
    pub fn of(bounty: &Bounty, auto_pay: bool, min_rank: u64) -> Self {
        let (slots, per_npub) = slots_and_limit(bounty);
        Self {
            issuer: bounty.issuer_pubkey.clone(),
            auto_pay,
            min_rank,
            slots,
            per_npub,
        }
    }
}

/// What is known about one claim when the decision runs.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimFacts {
    /// `<claimant pubkey>:<d tag>`: the claim's stable address. A claimant
    /// can replace a claim (kind 39999 is addressable), so attempts are
    /// keyed by this, never by the event id.
    pub address: String,
    pub event_id: String,
    pub claimant: String,
    pub created_at: u64,
    /// A zap receipt for the claim whose request the issuer signed.
    pub paid: bool,
    /// The issuer's recorded attempt for this address, if any.
    pub attempt: Option<AttemptState>,
    /// The claimant's rank in the issuer's own scores; `None` when the
    /// issuer's scores do not rank the claimant (or could not be read).
    pub rank: Option<f64>,
}

impl ClaimFacts {
    /// Paid, or an attempt that may have paid: the claim holds a slot.
    fn holds_slot(&self) -> bool {
        self.paid || self.attempt.is_some_and(AttemptState::holds_slot)
    }
}

/// Why a claim is not paid now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hold {
    Paid,
    Attempted(AttemptState),
    OwnClaim,
    AutoPayOff,
    NoSlots,
    OverLimit,
    RankTooLow { rank: Option<f64>, min: u64 },
}

/// A rank as people read it: a whole number without ".0".
pub fn rank_text(rank: f64) -> String {
    if rank.fract() == 0.0 {
        format!("{rank:.0}")
    } else {
        rank.to_string()
    }
}

impl Hold {
    /// The plain-words line the claim card shows. `None` for a paid claim:
    /// the receipt row already says so.
    pub fn describe(&self) -> Option<String> {
        Some(match self {
            Hold::Paid => return None,
            Hold::Attempted(AttemptState::Sending | AttemptState::Unknown) => {
                "Payment outcome unknown. The app won't try again; check the issuer's Coinos wallet."
                    .to_string()
            }
            Hold::Attempted(AttemptState::Paid) => {
                "Paid. Waiting for the payment receipt.".to_string()
            }
            Hold::Attempted(AttemptState::Failed) => {
                "Payment failed. The app won't try again.".to_string()
            }
            Hold::OwnClaim => "The issuer's own claim. It is not paid.".to_string(),
            Hold::AutoPayOff => "Auto-pay is off for this bounty. The issuer pays by hand.".to_string(),
            Hold::NoSlots => "No reward slots left on this bounty.".to_string(),
            Hold::OverLimit => {
                "This account already has the most rewards one account can get here.".to_string()
            }
            Hold::RankTooLow { rank: Some(rank), min } => format!(
                "Rank too low: the issuer's scores rank this account {}, and the bounty needs {min}.",
                rank_text(*rank)
            ),
            Hold::RankTooLow { rank: None, min } => format!(
                "Rank too low: the issuer's scores don't rank this account yet, and the bounty needs {min}."
            ),
        })
    }
}

/// The decision for one claim, given how many slots are already held and how
/// many of them the claimant holds. `Ok(())` means pay it now.
pub fn decide(
    policy: &Policy,
    claim: &ClaimFacts,
    slots_held: u64,
    claimant_held: u64,
) -> Result<(), Hold> {
    if claim.paid {
        return Err(Hold::Paid);
    }
    if let Some(state) = claim.attempt {
        return Err(Hold::Attempted(state));
    }
    if claim.claimant == policy.issuer {
        return Err(Hold::OwnClaim);
    }
    if !policy.auto_pay {
        return Err(Hold::AutoPayOff);
    }
    if slots_held >= policy.slots {
        return Err(Hold::NoSlots);
    }
    if policy.per_npub.is_some_and(|limit| claimant_held >= limit) {
        return Err(Hold::OverLimit);
    }
    match claim.rank {
        Some(rank) if rank >= policy.min_rank as f64 => Ok(()),
        rank => Err(Hold::RankTooLow {
            rank,
            min: policy.min_rank,
        }),
    }
}

/// The slots held on a bounty, in all and per claimant.
pub fn usage(claims: &[ClaimFacts]) -> (u64, HashMap<String, u64>) {
    let mut per: HashMap<String, u64> = HashMap::new();
    let mut held = 0;
    for claim in claims.iter().filter(|c| c.holds_slot()) {
        held += 1;
        *per.entry(claim.claimant.clone()).or_default() += 1;
    }
    (held, per)
}

/// Every claim's decision, oldest claim first (created_at, then id, as the
/// server orders them). A claim decided "pay" takes its slot before the next
/// claim is decided, so one pass never pays past the cap.
pub fn plan(policy: &Policy, claims: &[ClaimFacts]) -> Vec<(String, Result<(), Hold>)> {
    let (mut held, mut per) = usage(claims);
    let mut ordered: Vec<&ClaimFacts> = claims.iter().collect();
    ordered.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });
    ordered
        .into_iter()
        .map(|claim| {
            let mine = per.get(&claim.claimant).copied().unwrap_or(0);
            let decision = decide(policy, claim, held, mine);
            if decision.is_ok() {
                held += 1;
                *per.entry(claim.claimant.clone()).or_default() += 1;
            }
            (claim.address.clone(), decision)
        })
        .collect()
}

/// Whether `viewer` may still earn a reward on the bounty: a slot is left
/// and the viewer is under the per-npub limit.
pub fn open_for(slots: u64, per_npub: Option<u64>, claims: &[ClaimFacts], viewer: &str) -> bool {
    let (held, per) = usage(claims);
    held < slots
        && per_npub.is_none_or(|limit| per.get(viewer).copied().unwrap_or(0) < limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUER: &str = "issuer";

    fn policy() -> Policy {
        Policy {
            issuer: ISSUER.into(),
            auto_pay: true,
            min_rank: 2,
            slots: 2,
            per_npub: Some(1),
        }
    }

    fn claim(n: u64, claimant: &str, rank: Option<f64>) -> ClaimFacts {
        ClaimFacts {
            address: format!("{claimant}:item-{n}"),
            event_id: format!("e{n}"),
            claimant: claimant.into(),
            created_at: n,
            paid: false,
            attempt: None,
            rank,
        }
    }

    #[test]
    fn a_ranked_claim_with_a_slot_left_pays() {
        assert_eq!(decide(&policy(), &claim(1, "bob", Some(2.0)), 0, 0), Ok(()));
        assert_eq!(decide(&policy(), &claim(1, "bob", Some(7.5)), 1, 0), Ok(()));
    }

    #[test]
    fn a_rank_below_the_minimum_does_not_pay_and_says_the_number() {
        let hold = decide(&policy(), &claim(1, "bob", Some(1.0)), 0, 0).unwrap_err();
        assert_eq!(hold, Hold::RankTooLow { rank: Some(1.0), min: 2 });
        assert_eq!(
            hold.describe().unwrap(),
            "Rank too low: the issuer's scores rank this account 1, and the bounty needs 2."
        );
        let unranked = decide(&policy(), &claim(1, "bob", None), 0, 0).unwrap_err();
        assert_eq!(unranked, Hold::RankTooLow { rank: None, min: 2 });
    }

    #[test]
    fn an_attempted_claim_is_never_tried_again() {
        for state in [
            AttemptState::Sending,
            AttemptState::Paid,
            AttemptState::Failed,
            AttemptState::Unknown,
        ] {
            let mut c = claim(1, "bob", Some(5.0));
            c.attempt = Some(state);
            assert_eq!(decide(&policy(), &c, 0, 0), Err(Hold::Attempted(state)));
        }
        let mut paid = claim(1, "bob", Some(5.0));
        paid.paid = true;
        assert_eq!(decide(&policy(), &paid, 0, 0), Err(Hold::Paid));
    }

    #[test]
    fn no_slot_left_does_not_pay() {
        assert_eq!(
            decide(&policy(), &claim(1, "bob", Some(5.0)), 2, 0),
            Err(Hold::NoSlots)
        );
    }

    #[test]
    fn a_claimant_over_the_per_npub_limit_does_not_pay() {
        assert_eq!(
            decide(&policy(), &claim(1, "bob", Some(5.0)), 1, 1),
            Err(Hold::OverLimit)
        );
        let unlimited = Policy { per_npub: None, ..policy() };
        assert_eq!(decide(&unlimited, &claim(1, "bob", Some(5.0)), 1, 1), Ok(()));
    }

    #[test]
    fn the_issuers_own_claim_does_not_pay() {
        assert_eq!(
            decide(&policy(), &claim(1, ISSUER, Some(100.0)), 0, 0),
            Err(Hold::OwnClaim)
        );
    }

    #[test]
    fn auto_pay_off_does_not_pay() {
        let off = Policy { auto_pay: false, ..policy() };
        assert_eq!(
            decide(&off, &claim(1, "bob", Some(5.0)), 0, 0),
            Err(Hold::AutoPayOff)
        );
    }

    #[test]
    fn the_plan_pays_oldest_first_and_stops_at_the_cap() {
        let mut held = claim(1, "carol", Some(9.0));
        held.attempt = Some(AttemptState::Unknown);
        let claims = vec![
            claim(4, "dave", Some(3.0)),
            claim(2, "bob", Some(3.0)),
            claim(3, "bob", Some(3.0)),
            held,
        ];
        let plan = plan(&policy(), &claims);
        let by: HashMap<String, Result<(), Hold>> = plan.into_iter().collect();
        // carol's unknown outcome holds one slot; bob's older claim takes
        // the other; bob's second is over his limit; dave finds none left.
        assert_eq!(by["carol:item-1"], Err(Hold::Attempted(AttemptState::Unknown)));
        assert_eq!(by["bob:item-2"], Ok(()));
        assert_eq!(by["bob:item-3"], Err(Hold::NoSlots));
        assert_eq!(by["dave:item-4"], Err(Hold::NoSlots));
    }

    #[test]
    fn a_failed_attempt_frees_its_slot() {
        let mut failed = claim(1, "carol", Some(9.0));
        failed.attempt = Some(AttemptState::Failed);
        let claims = vec![failed, claim(2, "bob", Some(3.0))];
        let one_slot = Policy { slots: 1, ..policy() };
        let by: HashMap<_, _> = plan(&one_slot, &claims).into_iter().collect();
        assert_eq!(by["bob:item-2"], Ok(()));
    }

    #[test]
    fn eligibility_counts_paid_claims_against_slots_and_the_viewer() {
        let mut paid = claim(1, "bob", None);
        paid.paid = true;
        let claims = vec![paid, claim(2, "dave", None)];
        assert!(open_for(2, Some(1), &claims, "carol"));
        assert!(!open_for(2, Some(1), &claims, "bob"), "bob has his one reward");
        assert!(!open_for(1, None, &claims, "carol"), "the one slot is taken");
        assert!(open_for(2, None, &claims, "bob"));
    }

    #[test]
    fn slots_follow_the_server_policy() {
        let bounty = |extra: serde_json::Value| -> Bounty {
            let mut row = serde_json::json!({
                "id": "b", "issuer_pubkey": "aa", "list_coordinate": "39998:aa:x", "amount_sats": 100,
            });
            if let (Some(row), Some(extra)) = (row.as_object_mut(), extra.as_object()) {
                row.extend(extra.clone());
            }
            serde_json::from_value(row).unwrap()
        };
        assert_eq!(slots_and_limit(&bounty(serde_json::json!({}))), (1, Some(1)));
        assert_eq!(
            slots_and_limit(&bounty(serde_json::json!({"bounty_cap_sats": 450, "reward_per_item": 1}))),
            (4, None)
        );
        assert_eq!(
            slots_and_limit(&bounty(serde_json::json!({
                "bounty_cap_sats": 400, "reward_per_item": true, "max_rewards_per_npub": 2
            }))),
            (4, Some(2))
        );
    }
}
