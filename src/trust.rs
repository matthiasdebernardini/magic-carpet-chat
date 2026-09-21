//! The read side of "does this npub have its NIP-85 plumbing done?".
//!
//! Three relay/HTTP reads land here as already-fetched material:
//!
//! - kind 10040, the account's own "Treasure Map" designation — which
//!   kind-30382 assertions it wants scored, from which provider keys, on which
//!   relays. An empty-tag event is how a npub withdraws the whole thing.
//! - kind 30382, the provider's signed Trusted Assertions about a subject —
//!   rank, hops, followers, muters, reporters.
//! - kind 3, the account's contact list — the follow count is a dashboard
//!   signal on its own.
//!
//! Everything is signature-verified; relays lie, and a 30382 from the wrong
//! key or about the wrong `d` tag is just spam. Parsers take plain data
//! (events, plus the relay each was read from where the verdict needs it)
//! so the runtime keeps the networking and the UI keeps the wording.

use std::collections::HashSet;

use nostr_sdk::prelude::*;

/// The rank a claimant needs before any bounty on this instance pays —
/// mirrored from magic-carpet-v2/src/api/bounties.js:306 and :473.
pub const INSTANCE_MIN_RANK: u64 = 2;

/// The Brainstorm "assign a rank provider" endpoint; the account's hex pubkey
/// is appended. `404` means the npub has no Brainstorm account at all.
pub(crate) const BRAINSTORM_SETUP: &str = "https://api.brainstorm.world/setup/";

/// One row of a kind-10040: which assertion tag, whose key, on which relay.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The full first element, e.g. `30382:rank` — the tag's kind prefix is
    /// part of the name, so it is kept verbatim rather than split.
    pub kind_tag: String,
    /// The provider's 64-char hex public key.
    pub key: String,
    pub relay: String,
}

/// What the account's newest valid kind-10040 says.
#[derive(Debug, Clone, PartialEq)]
pub enum Designation {
    /// Relays answered but no valid event exists — genuinely not set up.
    None,
    /// The newest valid event carries no tags: the map was withdrawn.
    /// `raw` is the withdrawing event itself, for the raw toggle.
    Deactivated { at: u64, raw: String },
    Set {
        rows: Vec<Row>,
        at: u64,
        /// The relays that returned the winning event's id.
        found_on: Vec<String>,
        /// The winning event, pretty-printed for the raw toggle.
        raw: String,
    },
}

impl Designation {
    /// The `30382:rank` row, if the map names one — the one row the dashboard
    /// and the assertions card cannot work without.
    pub fn rank_row(&self) -> Option<&Row> {
        match self {
            Designation::Set { rows, .. } => {
                rows.iter().find(|row| row.kind_tag == "30382:rank")
            }
            _ => None,
        }
    }
}

/// One verified kind-30382 from a known provider about this account.
#[derive(Debug, Clone, PartialEq)]
pub struct Assertion {
    /// The signer (hex) — always the key the map or the instance names.
    pub provider: String,
    /// The relay it was read from; the runtime fills this in after parse.
    pub relay: String,
    pub rank: u8,
    /// `None` when the tag is absent or malformed — never synthesised.
    pub hops: Option<u32>,
    pub followers: Option<u32>,
    pub muters: Option<u32>,
    pub reporters: Option<u32>,
    pub at: u64,
    /// The verified event, pretty-printed for the raw toggle.
    pub raw: String,
}

/// The Brainstorm setup endpoint's answer for an npub that never designated.
#[derive(Debug, Clone, PartialEq)]
pub enum BrainstormKey {
    /// HTTP 404 — the npub is not registered with Brainstorm.
    NoAccount,
    Assigned { key: String, relay: String },
}

/// `true` for exactly 64 lowercase hex chars — the strict form a 10040 row
/// key must take before we trust it enough to query with it.
fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The value half of a `["name", "value", …]` tag.
fn tag_value<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    event.tags.iter().find_map(|tag| {
        let parts = tag.as_slice();
        (parts.first().map(String::as_str) == Some(name))
            .then(|| parts.get(1).map(String::as_str))
            .flatten()
    })
}

/// One `30382:<suffix>` tag → a [`Row`], or `None` when any element is off.
/// A bad row is skipped, not fatal — the rest of the map still counts.
fn parse_row(tag: &Tag) -> Option<Row> {
    let parts = tag.as_slice();
    let kind_tag = parts.first()?;
    let suffix = kind_tag.strip_prefix("30382:")?;
    if suffix.is_empty()
        || !suffix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '_')
    {
        return None;
    }
    let key = parts.get(1)?;
    let relay = parts.get(2)?;
    if !is_hex64(key) || !(relay.starts_with("wss://") || relay.starts_with("ws://")) {
        return None;
    }
    Some(Row {
        kind_tag: kind_tag.clone(),
        key: key.clone(),
        relay: relay.clone(),
    })
}

/// The account's designation verdict over `(relay, event)` pairs read from
/// its outbox relays. [`Designation::None`] means the relays answered and no
/// valid event came back — the "all relays failed" case is the caller's to
/// report, the parser cannot tell it apart from "never had a map".
///
/// Only signature-verified kind-10040s authored by `pubkey` play; the newest
/// wins, and an empty-tag newest means the map was withdrawn.
pub fn parse_designation(events: &[(String, Event)], pubkey: &str) -> Designation {
    // Winning candidate plus the relays that returned its id.
    let mut best: Option<(Event, Vec<String>)> = None;
    for (relay, event) in events {
        if event.kind != Kind::Custom(10040)
            || event.pubkey.to_hex() != pubkey
            || event.verify().is_err()
        {
            continue;
        }
        match &mut best {
            Some((winner, found_on)) if event.id == winner.id => {
                if !found_on.contains(relay) {
                    found_on.push(relay.clone());
                }
            }
            Some((winner, found_on)) if event.created_at <= winner.created_at => {
                let _ = found_on;
            }
            _ => {
                best = Some((event.clone(), vec![relay.clone()]));
            }
        }
    }
    let Some((event, found_on)) = best else {
        return Designation::None;
    };
    let at = event.created_at.as_secs();
    let raw = serde_json::to_string_pretty(&event).unwrap_or_default();
    if event.tags.is_empty() {
        return Designation::Deactivated { at, raw };
    }
    Designation::Set {
        rows: event.tags.iter().filter_map(parse_row).collect(),
        at,
        found_on,
        raw,
    }
}

/// The newest verified kind-30382 signed by `provider` about `subject`
/// (`#d` tag), or `None`. Rank is required and capped at 100; hops is kept
/// absent when the tag is absent rather than defaulted to zero.
pub fn parse_assertion(events: &[Event], provider: &str, subject: &str) -> Option<Assertion> {
    let mut best: Option<Assertion> = None;
    for event in events {
        if event.kind != Kind::Custom(30382)
            || event.pubkey.to_hex() != provider
            || event.verify().is_err()
            || tag_value(event, "d") != Some(subject)
        {
            continue;
        }
        let Some(rank) = tag_value(event, "rank").and_then(|v| v.parse::<u32>().ok()) else {
            continue;
        };
        if rank > 100 {
            continue;
        }
        let number = |name: &str| tag_value(event, name).and_then(|v| v.parse::<u32>().ok());
        let assertion = Assertion {
            provider: provider.to_string(),
            relay: String::new(),
            rank: rank as u8,
            hops: number("hops"),
            followers: number("followers"),
            muters: number("muters"),
            reporters: number("reporters"),
            at: event.created_at.as_secs(),
            raw: serde_json::to_string_pretty(&event).unwrap_or_default(),
        };
        if best.as_ref().is_none_or(|b| event.created_at.as_secs() > b.at) {
            best = Some(assertion);
        }
    }
    best
}

/// The Brainstorm setup answer: the `30382:rank` triple's key and relay, or
/// `None` for garbage, a non-array body, or an array with no rank triple.
pub fn parse_setup(body: &str) -> Option<BrainstormKey> {
    let triples: serde_json::Value = serde_json::from_str(body).ok()?;
    for triple in triples.as_array()? {
        let Some(parts) = triple.as_array() else {
            continue;
        };
        if parts.first().and_then(|v| v.as_str()) == Some("30382:rank")
            && let (Some(key), Some(relay)) = (
                parts.get(1).and_then(|v| v.as_str()),
                parts.get(2).and_then(|v| v.as_str()),
            )
        {
            return Some(BrainstormKey::Assigned {
                key: key.to_string(),
                relay: relay.to_string(),
            });
        }
    }
    None
}

/// Distinct valid `p` tags on the account's verified kind-3 — the headline
/// "following" number. `None` for the wrong kind, wrong author, or a bad
/// signature, so the caller can still tell "no list" from "not this list".
pub fn follow_count(event: &Event, pubkey: &str) -> Option<u32> {
    if event.kind != Kind::ContactList
        || event.pubkey.to_hex() != pubkey
        || event.verify().is_err()
    {
        return None;
    }
    let mut follows: HashSet<&str> = HashSet::new();
    for tag in event.tags.iter() {
        let parts = tag.as_slice();
        if let [name, key, ..] = parts
            && name == "p"
            && key.len() == 64
            && key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            follows.insert(key.as_str());
        }
    }
    Some(follows.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUSE_10040: &str = include_str!("../tests/fixtures/trust-10040-house.json");
    const HOUSE_30382: &str = include_str!("../tests/fixtures/trust-30382-self.json");
    const HOUSE_SETUP: &str = include_str!("../tests/fixtures/trust-setup-house.json");

    const HOUSE_HEX: &str =
        "853baa94b4b12d23931ade03ceb854a2b36cf1e24b5e3a82e68c8ca3a8ced2ba";
    const PROVIDER_HEX: &str =
        "c63c30b4e2f7966c3e329149dbdb5d5e0898b5cb4ddf1f97ed5011fe5b91c025";

    fn fixture_event(json: &str) -> Event {
        serde_json::from_str(json).expect("fixture is an event")
    }

    fn hex64(byte: u8) -> String {
        format!("{:064x}", byte)
    }

    fn sign_10040(keys: &Keys, tags: Vec<Tag>, at: u64) -> Event {
        EventBuilder::new(Kind::Custom(10040), "")
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(at))
            .finalize(keys)
            .expect("signing works")
    }

    fn sign_30382(keys: &Keys, subject: &str, fields: &[(&str, &str)], at: u64) -> Event {
        let mut tags = vec![Tag::parse(["d", subject]).unwrap()];
        tags.extend(
            fields
                .iter()
                .map(|(name, value)| Tag::parse([*name, *value]).unwrap()),
        );
        EventBuilder::new(Kind::Custom(30382), "")
            .tags(tags)
            .custom_created_at(Timestamp::from_secs(at))
            .finalize(keys)
            .expect("signing works")
    }

    fn sign_contacts(keys: &Keys, follows: &[&str], at: u64) -> Event {
        EventBuilder::new(Kind::ContactList, "")
            .tags(follows.iter().map(|f| Tag::parse(["p", *f]).unwrap()))
            .custom_created_at(Timestamp::from_secs(at))
            .finalize(keys)
            .expect("signing works")
    }

    /// Break the signature of a fixture without touching anything else.
    fn flip_sig(json: &str) -> Event {
        let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
        let sig = value["sig"].as_str().unwrap();
        let mut broken = sig[..sig.len() - 1].to_string();
        broken.push(if sig.ends_with('0') { '1' } else { '0' });
        value["sig"] = serde_json::Value::String(broken);
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn house_fixture_parses_to_set_with_two_rows() {
        let event = fixture_event(HOUSE_10040);
        let designation =
            parse_designation(&[("wss://magic-carpet.brainstorm.world/relay".into(), event)], HOUSE_HEX);
        let Designation::Set {
            rows,
            at,
            found_on,
            raw,
        } = designation
        else {
            panic!("house fixture should be Set: {designation:?}");
        };
        assert_eq!(at, 1_789_657_292);
        assert_eq!(found_on, ["wss://magic-carpet.brainstorm.world/relay"]);
        assert_eq!(
            rows,
            vec![
                Row {
                    kind_tag: "30382:rank".into(),
                    key: PROVIDER_HEX.into(),
                    relay: "wss://scores.brainstorm.world".into(),
                },
                Row {
                    kind_tag: "30382:followers".into(),
                    key: PROVIDER_HEX.into(),
                    relay: "wss://scores.brainstorm.world".into(),
                },
            ]
        );
        assert!(raw.contains("\"kind\": 10040"));
    }

    #[test]
    fn newer_valid_event_wins_over_older() {
        let keys = Keys::generate();
        let subject = keys.public_key().to_hex();
        let old = sign_10040(&keys, vec![], 100);
        let new = sign_10040(
            &keys,
            vec![Tag::parse([
                "30382:rank",
                &hex64(7),
                "wss://scores.brainstorm.world",
            ])
            .unwrap()],
            200,
        );
        let designation = parse_designation(
            &[
                ("wss://a".into(), old),
                ("wss://b".into(), new.clone()),
            ],
            &subject,
        );
        let Designation::Set { at, found_on, .. } = designation else {
            panic!("newer Set should win: {designation:?}");
        };
        assert_eq!(at, 200);
        assert_eq!(found_on, ["wss://b"]);
    }

    #[test]
    fn newer_empty_event_deactivates_an_older_set() {
        let keys = Keys::generate();
        let subject = keys.public_key().to_hex();
        let set = sign_10040(
            &keys,
            vec![Tag::parse(["30382:rank", &hex64(7), "wss://r"]).unwrap()],
            100,
        );
        let empty = sign_10040(&keys, vec![], 200);
        let designation =
            parse_designation(&[("wss://a".into(), set), ("wss://a".into(), empty)], &subject);
        assert!(
            matches!(designation, Designation::Deactivated { at: 200, .. }),
            "newer empty event should deactivate: {designation:?}"
        );
    }

    #[test]
    fn a_malformed_row_is_skipped_not_fatal() {
        let keys = Keys::generate();
        let subject = keys.public_key().to_hex();
        let event = sign_10040(
            &keys,
            vec![
                // Non-hex key — skipped.
                Tag::parse(["30382:rank", "nothex", "wss://r"]).unwrap(),
                Tag::parse(["30382:followers", &hex64(7), "wss://r"]).unwrap(),
            ],
            100,
        );
        let Designation::Set { rows, .. } =
            parse_designation(&[("wss://a".into(), event)], &subject)
        else {
            panic!("one bad row should not sink the map");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind_tag, "30382:followers");
    }

    #[test]
    fn rows_need_a_relay_and_a_30382_prefix() {
        let keys = Keys::generate();
        let subject = keys.public_key().to_hex();
        let event = sign_10040(
            &keys,
            vec![
                Tag::parse(["30382:rank", &hex64(7)]).unwrap(), // no relay
                Tag::parse(["other:rank", &hex64(7), "wss://r"]).unwrap(), // not 30382:
                Tag::parse(["30382:", &hex64(7), "wss://r"]).unwrap(), // empty suffix
                Tag::parse(["30382:rank", &hex64(7), "https://r"]).unwrap(), // not ws
                Tag::parse(["30382:rank", &hex64(7), "wss://r"]).unwrap(),
            ],
            100,
        );
        let Designation::Set { rows, .. } =
            parse_designation(&[("wss://a".into(), event)], &subject)
        else {
            panic!("the valid row should survive");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, hex64(7));
    }

    #[test]
    fn event_from_another_author_is_ignored() {
        let account = Keys::generate();
        let other = Keys::generate();
        let event = sign_10040(
            &other,
            vec![Tag::parse(["30382:rank", &hex64(7), "wss://r"]).unwrap()],
            100,
        );
        assert_eq!(
            parse_designation(&[("wss://a".into(), event)], &account.public_key().to_hex()),
            Designation::None
        );
    }

    #[test]
    fn house_fixture_with_flipped_signature_is_rejected() {
        let event = flip_sig(HOUSE_10040);
        assert_eq!(
            parse_designation(&[("wss://a".into(), event)], HOUSE_HEX),
            Designation::None
        );
    }

    #[test]
    fn empty_input_is_none() {
        assert_eq!(parse_designation(&[], HOUSE_HEX), Designation::None);
    }

    #[test]
    fn found_on_lists_every_relay_with_the_winning_id() {
        let event = fixture_event(HOUSE_10040);
        let designation = parse_designation(
            &[
                ("wss://a".into(), event.clone()),
                ("wss://b".into(), event.clone()),
                ("wss://a".into(), event),
            ],
            HOUSE_HEX,
        );
        let Designation::Set { found_on, .. } = designation else {
            panic!("house fixture should be Set");
        };
        assert_eq!(found_on, ["wss://a", "wss://b"]);
    }

    #[test]
    fn house_30382_parses() {
        let event = fixture_event(HOUSE_30382);
        let assertion =
            parse_assertion(std::slice::from_ref(&event), PROVIDER_HEX, HOUSE_HEX).unwrap();
        assert_eq!(assertion.provider, PROVIDER_HEX);
        assert_eq!(assertion.rank, 100);
        assert_eq!(assertion.hops, Some(0));
        assert_eq!(assertion.followers, Some(0));
        assert_eq!(assertion.at, 1_789_657_482);
        assert!(assertion.raw.contains("\"kind\": 30382"));
    }

    #[test]
    fn assertion_needs_the_right_d_tag_and_author() {
        let keys = Keys::generate();
        let subject = keys.public_key().to_hex();
        let other_subject = hex64(9);
        let wrong_d = sign_30382(&keys, &other_subject, &[("rank", "42")], 100);
        assert_eq!(
            parse_assertion(&[wrong_d], &keys.public_key().to_hex(), &subject),
            None
        );

        let wrong_author = sign_30382(&Keys::generate(), &subject, &[("rank", "42")], 100);
        assert_eq!(
            parse_assertion(&[wrong_author], &keys.public_key().to_hex(), &subject),
            None
        );
    }

    #[test]
    fn rank_101_is_rejected() {
        let keys = Keys::generate();
        let subject = keys.public_key().to_hex();
        let event = sign_30382(&keys, &subject, &[("rank", "101")], 100);
        assert_eq!(
            parse_assertion(&[event], &keys.public_key().to_hex(), &subject),
            None
        );
    }

    #[test]
    fn missing_rank_rejects_but_missing_extras_do_not() {
        let keys = Keys::generate();
        let provider = keys.public_key().to_hex();
        let subject = hex64(1);
        let no_rank = sign_30382(&keys, &subject, &[("hops", "2")], 100);
        assert_eq!(parse_assertion(&[no_rank], &provider, &subject), None);

        let bare = sign_30382(&keys, &subject, &[("rank", "3")], 100);
        let a = parse_assertion(&[bare], &provider, &subject).unwrap();
        assert_eq!(a.rank, 3);
        assert_eq!(a.hops, None);
        assert_eq!(a.followers, None);
    }

    #[test]
    fn hops_removed_between_events_stays_absent() {
        let keys = Keys::generate();
        let provider = keys.public_key().to_hex();
        let subject = hex64(1);
        let with_hops = sign_30382(&keys, &subject, &[("rank", "3"), ("hops", "2")], 100);
        let without = sign_30382(&keys, &subject, &[("rank", "3")], 200);
        let a = parse_assertion(&[with_hops, without], &provider, &subject).unwrap();
        assert_eq!(a.at, 200);
        assert_eq!(a.hops, None);
    }

    #[test]
    fn brainstrorm_setup_round_trips() {
        assert_eq!(
            parse_setup(HOUSE_SETUP),
            Some(BrainstormKey::Assigned {
                key: PROVIDER_HEX.into(),
                relay: "wss://scores.brainstorm.world".into(),
            })
        );
        assert_eq!(parse_setup("[]"), None);
        assert_eq!(parse_setup("not json"), None);
        assert_eq!(parse_setup("{\"a\":1}"), None);
    }

    #[test]
    fn follows_count_distinct_p_tags() {
        let keys = Keys::generate();
        let subject = keys.public_key().to_hex();
        let event = sign_contacts(
            &keys,
            &[&hex64(1), &hex64(2), &hex64(3), &hex64(2), "bogus"],
            100,
        );
        assert_eq!(follow_count(&event, &subject), Some(3));
    }

    #[test]
    fn follow_count_rejects_foreign_or_unsigned_events() {
        let keys = Keys::generate();
        let event = sign_contacts(&keys, &[&hex64(1)], 100);
        assert_eq!(follow_count(&event, &hex64(5)), None);
        // A kind-10040 is not a contact list even when everything else fits.
        let wrong_kind = sign_10040(
            &keys,
            vec![Tag::parse(["p", &hex64(1)]).unwrap()],
            100,
        );
        assert_eq!(follow_count(&wrong_kind, &keys.public_key().to_hex()), None);
    }
}
