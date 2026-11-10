//! Searching and ordering the bounty list: fuzzy match, frecency (how often
//! and how recently a bounty was opened), and the visit log behind it.
//! Pure apart from the log's file; the UI keeps a [`Snapshot`] so the list
//! never jumps under the cursor.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::Bounty;
use crate::{diag, secrets};

/// The human end of a list coordinate: `39998:<pk>:us-city` → `us-city`.
/// This is the name a bounty row shows.
pub fn coordinate_dtag(coordinate: &str) -> &str {
    coordinate.rsplit(':').next().unwrap_or(coordinate)
}

/// Only this many characters of a text are scored: criteria can be published
/// by anyone, and the match runs on the UI thread. Names are far shorter.
pub const MAX_SCORED_CHARS: usize = 2000;

/// Case-insensitive subsequence score of one term, best alignment, in
/// O(term x text). Rewards consecutive matches, a match at the start and at
/// word starts; gaps cost one point per skipped character.
fn term_score(term: &[char], text: &[char]) -> Option<i64> {
    // best[j]: best score with the current query char matched at text[j].
    let mut best: Vec<Option<i64>> = vec![None; text.len()];
    for (i, q) in term.iter().enumerate() {
        let mut next: Vec<Option<i64>> = vec![None; text.len()];
        // Max of best[k] + k over k <= j - 2: a gap from k to j costs
        // j - k - 1, so best[k] - (j - k - 1) = (best[k] + k) - (j - 1).
        let mut gapped: Option<i64> = None;
        for j in 0..text.len() {
            if j >= 2 {
                let prev = best[j - 2].map(|s| s + (j - 2) as i64);
                gapped = gapped.max(prev);
            }
            if text[j] != *q {
                continue;
            }
            let word_start = j == 0 || !text[j - 1].is_alphanumeric();
            let here = 10 + if j == 0 { 15 } else { 0 } + if word_start { 10 } else { 0 };
            let before = if i == 0 {
                Some(0)
            } else {
                let consecutive = j.checked_sub(1).and_then(|k| best[k]).map(|s| s + 15);
                let gap = gapped.map(|g| g - (j as i64 - 1));
                consecutive.max(gap)
            };
            next[j] = before.map(|b| b + here);
        }
        best = next;
    }
    best.into_iter().flatten().max()
}

/// Every whitespace-separated term of `query` must match `text` (the first
/// [`MAX_SCORED_CHARS`] of it), in order within the term; the score is the
/// sum. `None` when one does not match. An empty query matches with 0.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    let head: String = text.chars().take(MAX_SCORED_CHARS).collect();
    let text: Vec<char> = head.to_lowercase().chars().collect();
    query.split_whitespace().try_fold(0, |sum, term| {
        let term: Vec<char> = term.to_lowercase().chars().collect();
        term_score(&term, &text).map(|s| sum + s)
    })
}

/// The query after the search input changed to `typed`, or `None` when it is
/// the same text (nothing to retake). Also what clearing the input becomes:
/// `set_value` emits no change event, so the caller feeds it through here.
pub fn next_query(current: &str, typed: &str) -> Option<String> {
    (current != typed).then(|| typed.to_string())
}

/// Visit times (unix seconds) per bounty id, the 10 most recent each.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Visits(BTreeMap<String, Vec<u64>>);

const KEEP_VISITS: usize = 10;
const FORGET_AFTER: u64 = 90 * 24 * 3600;
const FILE_NAME: &str = "frecency.json";

impl Visits {
    pub fn record(&mut self, id: &str, now: u64) {
        let times = self.0.entry(id.to_string()).or_default();
        times.push(now);
        times.sort_unstable();
        let excess = times.len().saturating_sub(KEEP_VISITS);
        times.drain(..excess);
    }

    /// Drops every bounty whose visits are all older than 90 days.
    fn prune(&mut self, now: u64) {
        self.0.retain(|_, times| times.iter().any(|t| now.saturating_sub(*t) < FORGET_AFTER));
    }

    pub fn frecency(&self, id: &str, now: u64) -> u32 {
        const HOUR: u64 = 3600;
        const DAY: u64 = 24 * HOUR;
        let weight = |age: u64| match age {
            a if a < 4 * HOUR => 100,
            a if a < DAY => 80,
            a if a < 3 * DAY => 60,
            a if a < 7 * DAY => 40,
            a if a < 30 * DAY => 20,
            a if a < 90 * DAY => 10,
            _ => 0,
        };
        self.0
            .get(id)
            .map_or(0, |times| times.iter().map(|t| weight(now.saturating_sub(*t))).sum())
    }

    /// A missing or unreadable file is an empty log.
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                diag::note(format!("Couldn't read {}: {e}.", path.display()));
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                diag::note(format!("Couldn't read {}: {e}.", path.display()));
                Self::default()
            }
        }
    }

    /// Temp file (named for this process, so two instances never share one),
    /// then rename over the real one: atomic. No fsync: this is a ranking
    /// hint, and an update lost between two instances is acceptable.
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)?;
        drop(file);
        std::fs::rename(&tmp, path)
    }

    fn path() -> Option<PathBuf> {
        secrets::store_dir().ok().map(|dir| dir.join(FILE_NAME))
    }

    /// The log in the store directory.
    pub fn load() -> Self {
        Self::path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    /// Re-reads the file (another instance may have written), adds the visit
    /// and writes it back. Without a store directory the visit is still
    /// kept in memory. Failures are noted, not shown.
    pub fn record_and_save(&mut self, id: &str, now: u64) {
        self.record_and_save_at(Self::path().as_deref(), id, now);
    }

    fn record_and_save_at(&mut self, path: Option<&Path>, id: &str, now: u64) {
        let mut visits = path.map_or_else(|| self.clone(), Self::load_from);
        visits.record(id, now);
        visits.prune(now);
        if let Some(path) = path
            && let Err(e) = visits.save_to(path)
        {
            diag::note(format!("Couldn't save {}: {e}.", path.display()));
        }
        *self = visits;
    }
}

/// Indexes into `bounties`: every one by frecency, then newest (a missing
/// `created_at` last), then id; with a query, only matches, by score then
/// that same order. A match in the name ranks above any criteria-only match.
pub fn order(bounties: &[Bounty], query: &str, visits: &Visits, now: u64) -> Vec<usize> {
    // (row, matched in the name, score of that match)
    let mut rows: Vec<(usize, bool, i64)> = bounties
        .iter()
        .enumerate()
        .filter_map(|(ix, b)| {
            if let Some(score) = fuzzy_score(query, coordinate_dtag(&b.list_coordinate)) {
                return Some((ix, true, score));
            }
            let score = fuzzy_score(query, b.criteria.as_deref()?)?;
            Some((ix, false, score))
        })
        .collect();
    rows.sort_by(|(a, name_a, sa), (b, name_b, sb)| {
        let (a, b) = (&bounties[*a], &bounties[*b]);
        name_b
            .cmp(name_a)
            .then_with(|| sb.cmp(sa))
            .then_with(|| visits.frecency(&b.id, now).cmp(&visits.frecency(&a.id, now)))
            .then_with(|| Reverse(a.created_at).cmp(&Reverse(b.created_at)))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows.into_iter().map(|(ix, _, _)| ix).collect()
}

/// The order the list was last shown in. Recording a visit does not touch
/// it; it is retaken only when the list, the query or the tab changes.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Every id of the list it was taken from, sorted.
    universe: Vec<String>,
    shown: Vec<String>,
}

fn sorted_ids(bounties: &[Bounty]) -> Vec<String> {
    let mut ids: Vec<String> = bounties.iter().map(|b| b.id.clone()).collect();
    ids.sort();
    ids
}

impl Snapshot {
    pub fn take(bounties: &[Bounty], query: &str, visits: &Visits, now: u64) -> Self {
        Self::of(bounties, &order(bounties, query, visits, now))
    }

    /// The snapshot of an `order` already computed for `bounties`.
    pub fn of(bounties: &[Bounty], order: &[usize]) -> Self {
        Self {
            universe: sorted_ids(bounties),
            shown: order.iter().map(|ix| bounties[*ix].id.clone()).collect(),
        }
    }

    /// The rows in snapshot order, or `None` when `bounties` is not the list
    /// it was taken from (the caller then orders afresh).
    pub fn view<'a>(&self, bounties: &'a [Bounty]) -> Option<Vec<&'a Bounty>> {
        (sorted_ids(bounties) == self.universe).then(|| {
            self.shown
                .iter()
                .filter_map(|id| bounties.iter().find(|b| b.id == *id))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounty(id: &str, dtag: &str, criteria: Option<&str>, created_at: Option<u64>) -> Bounty {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "issuer_pubkey": "pk",
            "list_coordinate": format!("39998:pk:{dtag}"),
            "amount_sats": 100,
            "criteria": criteria,
            "created_at": created_at,
        }))
        .unwrap()
    }

    #[test]
    fn subsequence_matches_and_non_matches() {
        assert!(fuzzy_score("crsnt", "nashville-croissant-spot").is_some());
        assert!(fuzzy_score("CRSNT", "Nashville-Croissant").is_some());
        assert_eq!(fuzzy_score("xyz", "croissant"), None);
        assert_eq!(fuzzy_score("tc", "croissant"), None, "order matters");
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn consecutive_and_word_start_beat_scattered() {
        let consecutive = fuzzy_score("cro", "xcrox").unwrap();
        let scattered = fuzzy_score("cro", "xcxrxox").unwrap();
        assert!(consecutive > scattered);
        let word_start = fuzzy_score("sp", "nashville-spot").unwrap();
        let mid_word = fuzzy_score("sp", "nashvillespot").unwrap();
        assert!(word_start > mid_word);
    }

    #[test]
    fn every_term_must_match_in_any_order() {
        assert!(fuzzy_score("spot nash", "nashville-spot").is_some());
        assert_eq!(fuzzy_score("spot zzz", "nashville-spot"), None);
    }

    #[test]
    fn frecency_weights_at_the_bucket_edges() {
        const H: u64 = 3600;
        const D: u64 = 24 * H;
        let now = 1_000_000_000;
        let at = |age: u64| {
            let mut v = Visits::default();
            v.record("a", now - age);
            v.frecency("a", now)
        };
        assert_eq!(at(4 * H - 1), 100);
        assert_eq!(at(4 * H), 80);
        assert_eq!(at(D - 1), 80);
        assert_eq!(at(D), 60);
        assert_eq!(at(3 * D), 40);
        assert_eq!(at(7 * D), 20);
        assert_eq!(at(30 * D), 10);
        assert_eq!(at(90 * D - 1), 10);
        assert_eq!(at(90 * D), 0);
        assert_eq!(Visits::default().frecency("a", now), 0);
    }

    #[test]
    fn only_the_ten_most_recent_visits_are_kept() {
        let mut v = Visits::default();
        for t in 0..15 {
            v.record("a", 1000 + t);
        }
        assert_eq!(v.0["a"].len(), 10);
        assert_eq!(v.0["a"][0], 1005);
    }

    #[test]
    fn default_order_is_frecency_then_newest_missing_last() {
        let now = 1_000_000;
        let list = [
            bounty("old", "a", None, Some(10)),
            bounty("none", "b", None, None),
            bounty("new", "c", None, Some(90)),
            bounty("fav", "d", None, Some(1)),
        ];
        let mut visits = Visits::default();
        visits.record("fav", now - 10);
        visits.record("fav", now - 20);
        let ids: Vec<&str> =
            order(&list, "  ", &visits, now).into_iter().map(|i| list[i].id.as_str()).collect();
        assert_eq!(ids, ["fav", "new", "old", "none"]);
    }

    #[test]
    fn a_name_match_outranks_a_criteria_only_match() {
        let list = [
            bounty("c", "other", Some("a real croissant shop"), Some(99)),
            bounty("n", "croissant-list", None, Some(1)),
            bounty("x", "other", Some("nothing"), Some(50)),
        ];
        let got = order(&list, "croissant", &Visits::default(), 0);
        assert_eq!(got, [1, 0], "non-matching rows are dropped");
    }

    #[test]
    fn a_recorded_visit_does_not_reorder_the_snapshot() {
        let now = 1_000_000;
        let list = [bounty("a", "a", None, Some(1)), bounty("b", "b", None, Some(2))];
        let mut visits = Visits::default();
        let snap = Snapshot::take(&list, "", &visits, now);
        let ids = |rows: Vec<&Bounty>| rows.iter().map(|b| b.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(snap.view(&list).unwrap()), ["b", "a"]);
        visits.record("a", now);
        assert_eq!(ids(snap.view(&list).unwrap()), ["b", "a"]);
        assert_eq!(order(&list, "", &visits, now), [0, 1], "a retake does");
        assert!(snap.view(&list[..1]).is_none(), "another list is not the snapshot");
    }

    #[test]
    fn the_log_round_trips_and_forgets_old_bounties() {
        let dir = std::env::temp_dir().join(format!("mc-frecency-test-{}", std::process::id()));
        let path = dir.join(FILE_NAME);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Visits::load_from(&path), Visits::default());
        let now = 500 * 24 * 3600;
        let mut visits = Visits::default();
        visits.record_and_save_at(Some(&path), "stale", now - 100 * 24 * 3600);
        visits.record_and_save_at(Some(&path), "fresh", now);
        assert!(visits.0.contains_key("fresh") && !visits.0.contains_key("stale"));
        assert_eq!(Visits::load_from(&path), visits);
        let mut memory = Visits::default();
        memory.record_and_save_at(None, "kept", now);
        assert!(memory.0.contains_key("kept"), "no store directory: still recorded in memory");
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(Visits::load_from(&path), Visits::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_scorer_keeps_its_scores() {
        for (q, t, want) in [
            ("crsnt", "nashville-croissant-spot", Some(86)),
            ("cro", "xcrox", Some(60)),
            ("cro", "xcxrxox", Some(28)),
            ("sp", "nashville-spot", Some(45)),
            ("sp", "nashvillespot", Some(35)),
            ("spot nash", "nashville-spot", Some(205)),
            ("croissant", "croissant-list", Some(235)),
            ("croissant", "a real croissant shop", Some(220)),
            ("ab", "axxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxb", Some(14)),
            ("tc", "croissant", None),
            ("abc", "a-b-c abc", Some(73)),
        ] {
            assert_eq!(fuzzy_score(q, t), want, "{q} in {t}");
        }
    }

    #[test]
    fn non_ascii_text_matches() {
        assert!(fuzzy_score("mün", "Café München").is_some());
        assert!(fuzzy_score("caf", "Café München").is_some());
        assert!(fuzzy_score("café", "CAFÉ MÜNCHEN").is_some());
        assert_eq!(fuzzy_score("xyz", "Café München"), None);
        assert_eq!(fuzzy_score("mun", "Café München"), None, "no accent folding");
    }

    #[test]
    fn only_the_first_chars_are_scored() {
        let long = format!("{}zz", "a".repeat(MAX_SCORED_CHARS));
        assert_eq!(fuzzy_score("zz", &long), None, "a match past the cap is not seen");
        let huge = "a".repeat(50_000);
        assert!(fuzzy_score("aa", &huge).is_some());
        let within = format!("zz{}", "a".repeat(50_000));
        assert!(fuzzy_score("zz", &within).is_some());
    }

    #[test]
    fn a_long_gap_name_match_still_outranks_a_criteria_match() {
        let gappy = format!("a{}b", "x".repeat(60));
        assert!(fuzzy_score("ab", &gappy).unwrap() < 0);
        let list = [
            bounty("c", "other", Some("ab"), Some(99)),
            bounty("n", &gappy, None, Some(1)),
        ];
        assert_eq!(order(&list, "ab", &Visits::default(), 0), [1, 0]);
    }

    #[test]
    fn a_query_change_is_only_a_change_when_the_text_differs() {
        assert_eq!(next_query("cro", "cro"), None);
        assert_eq!(next_query("cro", ""), Some(String::new()));
        assert_eq!(next_query("", "x"), Some("x".to_string()));
    }
}
