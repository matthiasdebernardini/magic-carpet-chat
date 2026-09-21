//! Per-account trust state on the UI side: what the last `FetchTrust`
//! proved, and which dashboard/Trust-tab verdicts fall out of it.
//!
//! The rule that shapes the whole file: an `Err` part means *this read
//! failed*, not *the answer changed* — so a failed read never erases a prior
//! verdict. `Evidence.last` keeps the last successful answer; `status` says
//! how the latest attempt went. Verdicts read `last` only: a `Failed` with a
//! prior answer keeps the prior row, and a `Failed` with none is the one
//! "Couldn't check" state.

use magic_carpet_chat::nostr::TrustPart;
use magic_carpet_chat::trust::{Assertion, BrainstormKey, Designation};

/// Which of the three map-attention rows fires — all render as "Publish your
/// Treasure Map" with different copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MapVerdict {
    /// Relays answered; no valid kind-10040 exists.
    Missing,
    /// The newest valid kind-10040 is empty — the map was withdrawn.
    Deactivated,
    /// A map exists but names no `30382:rank` provider.
    NoRankRow,
}

/// How the most recent attempt went. `Loading` also covers "never fetched".
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Status {
    Loading,
    Fresh,
    Failed(String),
}

/// The last successful answer plus the status of the latest attempt.
#[derive(Debug, Clone)]
pub(crate) struct Evidence<T> {
    pub last: Option<T>,
    pub status: Status,
}

impl<T> Default for Evidence<T> {
    fn default() -> Self {
        Self {
            last: None,
            status: Status::Loading,
        }
    }
}

impl<T> Evidence<T> {
    /// `Ok` replaces the answer; `Err` keeps it and records the failure.
    pub(crate) fn apply(&mut self, result: Result<T, String>) {
        match result {
            Ok(value) => {
                self.last = Some(value);
                self.status = Status::Fresh;
            }
            Err(message) => self.status = Status::Failed(message),
        }
    }

    /// A new fetch: the next answer is pending, the old one stays.
    fn begin(&mut self) {
        self.status = Status::Loading;
    }
}

/// Everything the UI knows about one account's NIP-85 state.
#[derive(Debug, Default)]
pub(crate) struct TrustState {
    /// The fetch this state answers to; updates with an older one are stale.
    pub generation: u32,
    /// When the current generation was requested (for the re-activation
    /// throttle).
    pub requested_at: u64,
    pub designation: Evidence<Designation>,
    pub brainstorm: Evidence<BrainstormKey>,
    /// `Some(None)` = read ok, no contact list; `Some(Some(n))` = n follows.
    pub follows: Evidence<Option<u32>>,
    pub own: Evidence<Option<Assertion>>,
    pub instance: Evidence<Option<Assertion>>,
    /// The provider the instance resolved to, once known.
    pub instance_provider: Option<(String, String)>,
    /// When the designation last read clean — the card's "last successful
    /// check" line.
    pub checked_at: Option<u64>,
}

impl TrustState {
    /// A new fetch for this account: bump the generation, mark every part
    /// pending, keep every prior answer.
    pub(crate) fn begin(&mut self, generation: u32, now: u64) {
        self.generation = generation;
        self.requested_at = now;
        self.designation.begin();
        self.brainstorm.begin();
        self.follows.begin();
        self.own.begin();
        self.instance.begin();
    }

    /// Whether an update stamped `generation` still belongs to this state.
    pub(crate) fn accepts(&self, generation: u32) -> bool {
        generation >= self.generation
    }

    /// Fold one finished part into the state.
    pub(crate) fn apply(&mut self, part: TrustPart, now: u64) {
        match part {
            TrustPart::Designation(result) => {
                let fresh = result.is_ok();
                self.designation.apply(result);
                if fresh {
                    self.checked_at = Some(now);
                }
            }
            TrustPart::Brainstorm(result) => self.brainstorm.apply(result),
            TrustPart::Follows(result) => self.follows.apply(result),
            TrustPart::OwnAssertion(result) => self.own.apply(result),
            TrustPart::InstanceAssertion { provider, result } => {
                if let Some(provider) = provider {
                    self.instance_provider = Some(provider);
                }
                self.instance.apply(result);
            }
        }
    }

    /// The map row, if one fires. Reads `last` only: `Loading` and `Failed`
    /// with no prior answer produce no verdict (the latter is
    /// [`map_unreadable`](Self::map_unreadable)).
    pub(crate) fn needs_map(&self) -> Option<MapVerdict> {
        match self.designation.last.as_ref()? {
            Designation::None => Some(MapVerdict::Missing),
            Designation::Deactivated { .. } => Some(MapVerdict::Deactivated),
            designation => (designation.rank_row().is_none()).then_some(MapVerdict::NoRankRow),
        }
    }

    /// `Some(message)` when the designation read failed and there is no
    /// prior verdict to keep — the dashboard's "Couldn't check" row. The
    /// message is the runtime's fixed wording, relay count included.
    pub(crate) fn map_unreadable(&self) -> Option<&str> {
        if self.designation.last.is_some() {
            return None;
        }
        self.map_read_failed()
    }

    /// The runtime's failure message, whenever the last attempt failed —
    /// even when a prior verdict survives on screen.
    pub(crate) fn map_read_failed(&self) -> Option<&str> {
        match &self.designation.status {
            Status::Failed(message) => Some(message),
            _ => None,
        }
    }

    /// "Follow someone": the kind-3 read succeeded and found zero follows
    /// (no list, or a list with no valid `p` tags).
    pub(crate) fn no_follows(&self) -> bool {
        matches!(self.follows.last, Some(None) | Some(Some(0)))
    }

    /// "Gain a follower": the instance assertion read succeeded and showed
    /// nobody — no assertion at all, or one reporting zero followers.
    pub(crate) fn no_verified_follower(&self) -> bool {
        match self.instance.last.as_ref() {
            Some(None) => true,
            Some(Some(assertion)) => assertion.followers == Some(0),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magic_carpet_chat::trust::Row;

    const RELAY: &str = "wss://scores.brainstorm.world";
    const PROVIDER: &str =
        "c63c30b455f63f2bd9d92dd8c17f6e48646cb93a2aa5b12195e1a5157a04ee6b";

    fn set_designation() -> Designation {
        Designation::Set {
            rows: vec![Row {
                kind_tag: "30382:rank".into(),
                key: PROVIDER.into(),
                relay: RELAY.into(),
            }],
            at: 100,
            found_on: vec!["wss://a".into()],
            raw: "{}".into(),
        }
    }

    fn assertion(followers: Option<u32>) -> Assertion {
        Assertion {
            provider: PROVIDER.into(),
            relay: RELAY.into(),
            rank: 4,
            hops: Some(1),
            followers,
            muters: None,
            reporters: None,
            at: 100,
            raw: "{}".into(),
        }
    }

    #[test]
    fn err_preserves_the_prior_answer() {
        let mut evidence: Evidence<u32> = Evidence::default();
        evidence.apply(Ok(7));
        evidence.apply(Err("nope".into()));
        assert_eq!(evidence.last, Some(7));
        assert_eq!(evidence.status, Status::Failed("nope".into()));
        evidence.apply(Ok(9));
        assert_eq!(evidence.last, Some(9));
        assert_eq!(evidence.status, Status::Fresh);
    }

    #[test]
    fn a_set_map_needs_nothing() {
        let mut state = TrustState::default();
        state.apply(TrustPart::Designation(Ok(set_designation())), 1);
        assert_eq!(state.needs_map(), None);
        assert_eq!(state.map_unreadable(), None);
        assert_eq!(state.checked_at, Some(1));
    }

    #[test]
    fn the_three_map_verdicts() {
        let mut state = TrustState::default();
        state.apply(TrustPart::Designation(Ok(Designation::None)), 1);
        assert_eq!(state.needs_map(), Some(MapVerdict::Missing));

        state.apply(
            TrustPart::Designation(Ok(Designation::Deactivated {
                at: 50,
                raw: "{}".into(),
            })),
            2,
        );
        assert_eq!(state.needs_map(), Some(MapVerdict::Deactivated));

        let mut rows_only_followers = match set_designation() {
            Designation::Set { mut rows, at, found_on, raw } => {
                rows.clear();
                rows.push(Row {
                    kind_tag: "30382:followers".into(),
                    key: PROVIDER.into(),
                    relay: RELAY.into(),
                });
                Designation::Set { rows, at, found_on, raw }
            }
            other => other,
        };
        let _ = &mut rows_only_followers;
        state.apply(TrustPart::Designation(Ok(rows_only_followers)), 3);
        assert_eq!(state.needs_map(), Some(MapVerdict::NoRankRow));
    }

    #[test]
    fn all_relays_failed_is_unreadable_not_missing() {
        let mut state = TrustState::default();
        state.apply(
            TrustPart::Designation(Err("Couldn't read kind-10040 from 7 relays.".into())),
            1,
        );
        assert_eq!(
            state.map_unreadable(),
            Some("Couldn't read kind-10040 from 7 relays.")
        );
        assert_eq!(state.needs_map(), None);
        // …and a failed re-read keeps the prior verdict.
        state.apply(TrustPart::Designation(Ok(set_designation())), 2);
        state.apply(
            TrustPart::Designation(Err("Couldn't read kind-10040 from 7 relays.".into())),
            3,
        );
        assert_eq!(state.map_unreadable(), None);
        assert_eq!(state.needs_map(), None);
        assert_eq!(state.checked_at, Some(2));
    }

    #[test]
    fn follows_zero_or_absent_flags_follow_someone() {
        let mut state = TrustState::default();
        assert!(!state.no_follows()); // never read
        state.apply(TrustPart::Follows(Ok(Some(2))), 1);
        assert!(!state.no_follows());
        state.apply(TrustPart::Follows(Ok(Some(0))), 2);
        assert!(state.no_follows());
        state.apply(TrustPart::Follows(Ok(None)), 3);
        assert!(state.no_follows());
        state.apply(TrustPart::Follows(Err("Couldn't read".into())), 4);
        assert!(state.no_follows()); // prior verdict survives
    }

    #[test]
    fn instance_assertion_zero_followers_flags_gain_a_follower() {
        let mut state = TrustState::default();
        state.apply(
            TrustPart::InstanceAssertion {
                provider: Some((PROVIDER.into(), RELAY.into())),
                result: Ok(Some(assertion(Some(3)))),
            },
            1,
        );
        assert!(!state.no_verified_follower());
        state.apply(
            TrustPart::InstanceAssertion {
                provider: None,
                result: Ok(Some(assertion(Some(0)))),
            },
            2,
        );
        assert!(state.no_verified_follower());
        state.apply(
            TrustPart::InstanceAssertion {
                provider: None,
                result: Ok(None),
            },
            3,
        );
        assert!(state.no_verified_follower());
        // A failed read with no fresh answer is not "nobody follows you".
        let mut fresh = TrustState::default();
        fresh.apply(
            TrustPart::InstanceAssertion {
                provider: None,
                result: Err("Couldn't read".into()),
            },
            1,
        );
        assert!(!fresh.no_verified_follower());
    }

    #[test]
    fn stale_generations_do_not_apply() {
        let mut state = TrustState::default();
        state.begin(2, 10);
        assert!(state.accepts(2));
        assert!(state.accepts(3));
        assert!(!state.accepts(1));
    }
}
