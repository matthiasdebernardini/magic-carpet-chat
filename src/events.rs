//! Event builders and parsers for the Magic Carpet instance.
//!
//! Tag layouts here must match the production server byte for byte:
//! - d-tags mirror magic-carpet-v2/src/lib/dtag.js (`slug`, `hash8`,
//!   `headerDTag`, `childDTag`) — SHA-256, first 8 hex chars.
//! - The DList header mirrors ui/src/pages/lists/NewDList.jsx.
//! - The claim carries the item name in BOTH the content (the shape the
//!   proven CLI-paid claims used) and the `name` tag (the shape
//!   NewDListItem.jsx publishes and the tapestry web UI reads), so every
//!   consumer — server, web UI, demo-audit, older tooling — sees the name.
//! - Receipt parsing mirrors src/api/bounties.js: the payer identity is the
//!   pubkey inside the description-tag JSON, never the receipt's own pubkey
//!   (that one belongs to the LNURL provider's zapper).

use std::str::FromStr;

use lightning_invoice::Bolt11Invoice;
use nostr_sdk::prelude::*;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// DList header, addressable. Coordinate = `39998:<pubkey>:<dtag>`.
pub const DLIST_KIND: u16 = 39998;
/// Claim (list item), addressable, tagged `["z", <list coordinate>]`.
pub const CLAIM_KIND: u16 = 39999;
/// NIP-57 zap receipt. Joins a claim through its `e` tag.
pub const RECEIPT_KIND: u16 = 9735;
/// NIP-57 zap request: signed by the payer, carried to the recipient's LNURL
/// server, embedded in the receipt's `description` tag.
pub const ZAP_REQUEST_KIND: u16 = 9734;

#[derive(thiserror::Error, Debug)]
pub enum EventError {
    #[error("{0}")]
    Invalid(String),
    #[error("Couldn't sign: {0}.")]
    Sign(String),
}

/// Canonical slug, mirroring dtag.js `slug()` exactly: lowercase, NFD with
/// combining marks U+0300–U+036F stripped, non-alphanumeric runs collapsed to
/// one hyphen, no leading or trailing hyphen.
pub fn slug(name: &str) -> String {
    let lowered = name.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut gap = false;
    for c in lowered.nfd() {
        if ('\u{0300}'..='\u{036f}').contains(&c) {
            continue;
        }
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

/// First 8 hex chars of SHA-256, mirroring dtag.js `hash8()`.
pub fn hash8(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    digest[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// d-tag for a DList header: `slug(name)`.
pub fn header_dtag(name: &str) -> String {
    slug(name)
}

/// d-tag for a claim: `slug(name)-hash8(parent coordinate)`.
pub fn child_dtag(name: &str, parent_ref: &str) -> String {
    format!("{}-{}", slug(name), hash8(parent_ref))
}

/// The addressable coordinate of a DList header.
pub fn header_coordinate(pubkey: &PublicKey, dtag: &str) -> String {
    format!("{DLIST_KIND}:{}:{dtag}", pubkey.to_hex())
}

fn tag<'a>(parts: impl IntoIterator<Item = &'a str>) -> Result<Tag, EventError> {
    Tag::parse(parts.into_iter().map(str::to_string).collect::<Vec<_>>())
        .map_err(|e| EventError::Invalid(e.to_string()))
}

/// Signed kind-39998 DList header, the shape NewDList.jsx publishes:
/// content "", `["d", slug]`, `["names", singular, plural]` (one 3-element
/// tag), optional `["description", text]`. No amount or slot tags — bounty
/// economics live server-side.
pub fn dlist_header(
    keys: &Keys,
    singular: &str,
    plural: &str,
    description: Option<&str>,
) -> Result<Event, EventError> {
    let singular = singular.trim();
    if singular.is_empty() {
        return Err(EventError::Invalid(
            "Enter a singular item name for the list.".into(),
        ));
    }
    let dtag = header_dtag(singular);
    if dtag.is_empty() {
        return Err(EventError::Invalid(format!(
            "\"{singular}\" needs at least one English letter or digit. Use a different name."
        )));
    }
    // NewDList.jsx: `['names', singular, plural || singular]`.
    let plural = match plural.trim() {
        "" => singular,
        p => p,
    };
    let mut tags = vec![
        tag(["d", dtag.as_str()])?,
        tag(["names", singular, plural])?,
    ];
    if let Some(text) = description {
        let text = text.trim();
        if !text.is_empty() {
            tags.push(tag(["description", text])?);
        }
    }
    EventBuilder::new(Kind::Custom(DLIST_KIND), "")
        .tags(tags)
        .finalize(keys)
        .map_err(|e| EventError::Sign(e.to_string()))
}

/// Signed kind-39999 claim: `["d", childDTag]`, exactly one
/// `["z", coordinate]`, `["name", name]` — and the item name in the content
/// as well. The proven CLI-paid claims carried the name in the content while
/// the web UI reads the `name` tag; writing both is harmless and keeps the
/// claim legible to every consumer. Must go out through
/// POST /api/strfry/publish — a claim published anywhere else is invisible to
/// the bounty machinery.
pub fn claim(keys: &Keys, name: &str, list_coordinate: &str) -> Result<Event, EventError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(EventError::Invalid(
            "Enter an item name for the claim.".into(),
        ));
    }
    let list_coordinate = list_coordinate.trim();
    if list_coordinate.is_empty() {
        return Err(EventError::Invalid(
            "Pick a bounty first. A claim needs its list.".into(),
        ));
    }
    let dtag = child_dtag(name, list_coordinate);
    if dtag.starts_with('-') {
        return Err(EventError::Invalid(format!(
            "\"{name}\" needs at least one English letter or digit. Use a different name."
        )));
    }
    EventBuilder::new(Kind::Custom(CLAIM_KIND), name)
        .tags([
            tag(["d", dtag.as_str()])?,
            tag(["z", list_coordinate])?,
            tag(["name", name])?,
        ])
        .finalize(keys)
        .map_err(|e| EventError::Sign(e.to_string()))
}

/// Signed kind-9734 zap request for paying a claim on one bounty: the shape
/// magic-carpet-v2/src/lib/zap-node.js `buildZapRequest` signs (content "",
/// `["p", claimant]`, `["e", claim id]`, `["a", list coordinate]`,
/// `["amount", msats]`, `["relays", …]`) plus `["bounty", <server bounty
/// id>]`, so two bounties on one list each pay a claim once and a receipt
/// for one never counts for the other. The server matches a receipt by its
/// `e` tag and the request's pubkey only, so the extra tag changes nothing
/// there; it counts the claim paid when that pubkey is the issuer, so `keys`
/// must be the issuer's.
pub fn zap_request(
    keys: &Keys,
    claimant_hex: &str,
    claim_event_id: &str,
    list_coordinate: &str,
    bounty_id: &str,
    sats: u64,
    relays: &[String],
) -> Result<Event, EventError> {
    if sats == 0 {
        return Err(EventError::Invalid("A zap needs at least 1 sat.".into()));
    }
    let msats = sats.saturating_mul(1000).to_string();
    let relay_tag: Vec<&str> = std::iter::once("relays")
        .chain(relays.iter().map(String::as_str))
        .collect();
    EventBuilder::new(Kind::Custom(ZAP_REQUEST_KIND), "")
        .tags([
            tag(["p", claimant_hex])?,
            tag(["e", claim_event_id])?,
            tag(["a", list_coordinate])?,
            tag(["bounty", bounty_id])?,
            tag(["amount", msats.as_str()])?,
            tag(relay_tag)?,
        ])
        .finalize(keys)
        .map_err(|e| EventError::Sign(e.to_string()))
}

/// A kind-9735 receipt, reduced to the facts the app acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub receipt_id: String,
    pub created_at: u64,
    /// Every `e` tag — the join keys back to claim event ids.
    pub claim_event_ids: Vec<String>,
    /// `JSON.parse(description).pubkey`: the payer the server validates
    /// (issuer or the issuer's auto-pay delegate). The receipt's own signer is
    /// the LNURL provider's zapper key and proves nothing about who paid.
    pub payer_pubkey: Option<String>,
    /// The uppercase `P` tag (NIP-57 sender). Decoration; do not validate it.
    pub sender_pubkey: Option<String>,
    /// Amount read out of the bolt11 tag. Should equal the bounty amount.
    pub amount_msats: Option<u64>,
}

impl Receipt {
    pub fn amount_sats(&self) -> Option<u64> {
        self.amount_msats.map(|msats| msats / 1000)
    }

    pub fn pays_claim(&self, claim_event_id: &str) -> bool {
        self.claim_event_ids.iter().any(|id| id == claim_event_id)
    }
}

/// Whether a relay's kind-9735 pays `claim_event_id` on `payer`'s behalf:
/// the receipt names the claim, and the zap request inside its
/// `description` is a valid kind-9734 SIGNED by `payer` that names the claim
/// too. Stricter than the server, which reads the description's pubkey
/// without checking its signature: anyone can publish a 9735, so a forged
/// one could otherwise use up a bounty's slots from the issuer's own view.
pub fn receipt_pays_claim_for(receipt: &Event, claim_event_id: &str, payer: &str) -> bool {
    let names = |event: &Event, name: &str, value: &str| {
        event.tags.iter().any(|t| {
            let t = t.as_slice();
            t.first().map(String::as_str) == Some(name) && t.get(1).map(String::as_str) == Some(value)
        })
    };
    if receipt.kind.as_u16() != RECEIPT_KIND || !names(receipt, "e", claim_event_id) {
        return false;
    }
    let Some(description) = receipt.tags.iter().find_map(|t| {
        let t = t.as_slice();
        (t.first().map(String::as_str) == Some("description")).then(|| t.get(1)).flatten()
    }) else {
        return false;
    };
    let Ok(request) = serde_json::from_str::<Event>(description) else {
        return false;
    };
    request.kind.as_u16() == ZAP_REQUEST_KIND
        && request.pubkey.to_hex() == payer
        && request.verify().is_ok()
        && names(&request, "e", claim_event_id)
}

/// [`receipt_pays_claim_for`], and the zap request asks for exactly the
/// bounty's reward (`amount`, msats) and, when it names a list (`a`), names
/// this bounty's. A request naming a bounty (`["bounty", id]`, this app's
/// zaps) pays only that bounty. One with no bounty tag (a payment by hand
/// from another client, older zaps) pays the claim on every bounty on its
/// list: that can only make the app pay too little, never twice.
pub fn receipt_pays_bounty_claim(
    receipt: &Event,
    claim_event_id: &str,
    payer: &str,
    list_coordinate: &str,
    bounty_id: &str,
    reward_sats: u64,
) -> bool {
    if !receipt_pays_claim_for(receipt, claim_event_id, payer) {
        return false;
    }
    let Some(request) = receipt.tags.iter().find_map(|t| {
        let t = t.as_slice();
        (t.first().map(String::as_str) == Some("description"))
            .then(|| t.get(1))
            .flatten()
            .and_then(|d| serde_json::from_str::<Event>(d).ok())
    }) else {
        return false;
    };
    let first = |name: &str| {
        request.tags.iter().find_map(|t| {
            let t = t.as_slice();
            (t.first().map(String::as_str) == Some(name))
                .then(|| t.get(1).cloned())
                .flatten()
        })
    };
    let msats = reward_sats.saturating_mul(1000).to_string();
    first("bounty").is_none_or(|b| b == bounty_id)
        && first("amount").as_deref() == Some(msats.as_str())
        && first("a").is_none_or(|a| a == list_coordinate)
}

/// Parse a live relay event as a receipt. `None` when it is not kind-9735.
pub fn parse_receipt(event: &Event) -> Option<Receipt> {
    let tags: Vec<Vec<String>> = event.tags.iter().map(|t| t.as_slice().to_vec()).collect();
    receipt_from_parts(
        event.kind.as_u16(),
        &event.id.to_hex(),
        event.created_at.as_secs(),
        &tags,
    )
}

/// Shared core, so the API's raw-JSON copy of a receipt parses through the
/// same rules as a live relay event.
pub(crate) fn receipt_from_parts(
    kind: u16,
    id: &str,
    created_at: u64,
    tags: &[Vec<String>],
) -> Option<Receipt> {
    if kind != RECEIPT_KIND {
        return None;
    }
    let values = |name: &str| -> Vec<&str> {
        tags.iter()
            .filter(|t| t.first().map(String::as_str) == Some(name))
            .filter_map(|t| t.get(1).map(String::as_str))
            .collect()
    };
    let payer_pubkey = values("description").first().and_then(|description| {
        serde_json::from_str::<serde_json::Value>(description)
            .ok()?
            .get("pubkey")?
            .as_str()
            .map(str::to_string)
    });
    let amount_msats = values("bolt11")
        .first()
        .and_then(|bolt11| Bolt11Invoice::from_str(bolt11).ok())
        .and_then(|invoice| invoice.amount_milli_satoshis());
    Some(Receipt {
        receipt_id: id.to_string(),
        created_at,
        claim_event_ids: values("e").iter().map(|s| s.to_string()).collect(),
        payer_pubkey,
        sender_pubkey: values("P").first().map(|s| s.to_string()),
        amount_msats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parent coordinate used for the hardcoded vectors below. Expected
    /// values computed with node against magic-carpet-v2/src/lib/dtag.js.
    const COORD: &str =
        "39998:853baa94b4b12d23931ade03ceb854a2b36cf1e24b5e3a82e68c8ca3a8ced2ba:us-city";

    fn tags_of(event: &Event) -> Vec<Vec<String>> {
        event.tags.iter().map(|t| t.as_slice().to_vec()).collect()
    }

    #[test]
    fn dtags_match_the_node_implementation() {
        // node -e 'require("./src/lib/dtag.js")...' in magic-carpet-v2:
        assert_eq!(slug("City"), "city");
        assert_eq!(slug("Café  du Monde!"), "cafe-du-monde");
        assert_eq!(header_dtag("US City"), "us-city");
        assert_eq!(hash8(COORD), "e0ea60d1");
        assert_eq!(child_dtag("Memphis", COORD), "memphis-e0ea60d1");
        assert_eq!(child_dtag("Café du Monde", COORD), "cafe-du-monde-e0ea60d1");
    }

    #[test]
    fn dlist_header_matches_the_ui_shape_exactly() {
        let keys = Keys::generate();
        let event =
            dlist_header(&keys, "US City", "US Cities", Some("Cities in the USA")).unwrap();
        assert_eq!(event.kind.as_u16(), DLIST_KIND);
        assert_eq!(event.content, "");
        assert!(event.verify().is_ok());
        assert_eq!(
            tags_of(&event),
            vec![
                vec!["d".to_string(), "us-city".to_string()],
                vec![
                    "names".to_string(),
                    "US City".to_string(),
                    "US Cities".to_string()
                ],
                vec!["description".to_string(), "Cities in the USA".to_string()],
            ],
            "tag layout must match NewDList.jsx byte for byte"
        );
        assert_eq!(
            header_coordinate(&keys.public_key(), "us-city"),
            format!("39998:{}:us-city", keys.public_key().to_hex())
        );
    }

    #[test]
    fn dlist_header_plural_falls_back_to_singular_and_description_is_optional() {
        let keys = Keys::generate();
        let event = dlist_header(&keys, "City", "", None).unwrap();
        assert_eq!(
            tags_of(&event),
            vec![
                vec!["d".to_string(), "city".to_string()],
                vec!["names".to_string(), "City".to_string(), "City".to_string()],
            ]
        );
        assert!(dlist_header(&keys, "", "x", None).is_err());
        assert!(dlist_header(&keys, "!!!", "x", None).is_err(), "empty slug");
    }

    #[test]
    fn claim_carries_the_name_in_both_content_and_tag() {
        let keys = Keys::generate();
        let event = claim(&keys, "Memphis", COORD).unwrap();
        assert_eq!(event.kind.as_u16(), CLAIM_KIND);
        // The proven CLI-paid claims put the item name in the content; the
        // web UI reads the name tag. The claim carries both.
        assert_eq!(event.content, "Memphis");
        assert!(event.verify().is_ok());
        assert_eq!(
            tags_of(&event),
            vec![
                vec!["d".to_string(), "memphis-e0ea60d1".to_string()],
                vec!["z".to_string(), COORD.to_string()],
                vec!["name".to_string(), "Memphis".to_string()],
            ],
            "tag layout must match NewDListItem.jsx byte for byte"
        );
        // Exactly one z tag — stableClaimAddress rejects anything else.
        let z_count = tags_of(&event)
            .iter()
            .filter(|t| t.first().map(String::as_str) == Some("z"))
            .count();
        assert_eq!(z_count, 1);
        assert!(claim(&keys, "", COORD).is_err());
        assert!(claim(&keys, "Memphis", "").is_err());
    }

    #[test]
    fn receipt_parses_the_live_fixture() {
        // The real receipt from bounty 5f44688e ("Memphis", 2026-08-07).
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/bounty-5f44688e.json")).unwrap();
        let receipt_json = fixture["claims"][0]["zapReceipt"].clone();
        let event: Event = serde_json::from_value(receipt_json).unwrap();
        assert!(event.verify().is_ok(), "the fixture receipt is a signed event");

        let receipt = parse_receipt(&event).unwrap();
        assert_eq!(
            receipt.receipt_id,
            "b3b3f887aab52d97796777aeac056f72ddaf745fd98329a3fcdb9ac37ba332c7"
        );
        assert!(receipt.pays_claim(
            "afb4bfe5b6556aa4a65a3c51a82d0e974c234dbe9551963d347208aa5d4ebc85"
        ));
        // The payer is the description-JSON pubkey (the delegate), NOT the
        // receipt's signer (Strike's zapper key 8b7cd498…).
        assert_eq!(
            receipt.payer_pubkey.as_deref(),
            Some("caca367dde1c413c59049519f73669d750f63609936b71c5d933e85836047d38")
        );
        assert_ne!(
            receipt.payer_pubkey.as_deref(),
            Some(event.pubkey.to_hex().as_str())
        );
        assert_eq!(receipt.sender_pubkey, receipt.payer_pubkey);
        assert_eq!(receipt.amount_msats, Some(100_000));
        assert_eq!(receipt.amount_sats(), Some(100));
        assert_eq!(receipt.created_at, 1_786_093_481);
    }

    #[test]
    fn a_zap_request_matches_the_server_shape() {
        let issuer = Keys::generate();
        let claimant = "aa".repeat(32);
        let relays = vec![
            "wss://magic-carpet.brainstorm.world/relay".to_string(),
            "wss://nos.lol".to_string(),
        ];
        let event = zap_request(&issuer, &claimant, "e1", COORD, "b1", 100, &relays).unwrap();
        assert_eq!(event.kind.as_u16(), ZAP_REQUEST_KIND);
        assert_eq!(event.content, "");
        assert_eq!(event.pubkey, issuer.public_key(), "signed by the issuer");
        assert!(event.verify().is_ok());
        assert_eq!(
            tags_of(&event),
            vec![
                vec!["p".to_string(), claimant],
                vec!["e".to_string(), "e1".to_string()],
                vec!["a".to_string(), COORD.to_string()],
                vec!["bounty".to_string(), "b1".to_string()],
                vec!["amount".to_string(), "100000".to_string()],
                vec![
                    "relays".to_string(),
                    relays[0].clone(),
                    relays[1].clone(),
                ],
            ]
        );
        assert!(zap_request(&issuer, "bb", "e1", COORD, "b1", 0, &relays).is_err());
    }

    #[test]
    fn only_a_receipt_around_the_payers_signed_request_pays_the_claim() {
        let issuer = Keys::generate();
        let zapper = Keys::generate();
        let relays = vec!["wss://nos.lol".to_string()];
        let receipt = |request: &Event, e: &str| -> Event {
            EventBuilder::new(Kind::Custom(RECEIPT_KIND), "")
                .tags([
                    tag(["e", e]).unwrap(),
                    tag(["description", serde_json::to_string(request).unwrap().as_str()]).unwrap(),
                ])
                .finalize(&zapper)
                .unwrap()
        };
        let request = zap_request(&issuer, &"aa".repeat(32), "c1", COORD, "b1", 21, &relays).unwrap();
        let payer = issuer.public_key().to_hex();
        assert!(receipt_pays_claim_for(&receipt(&request, "c1"), "c1", &payer));
        // Another claim, another payer, or a receipt naming a claim its
        // request does not: none of them pay.
        assert!(!receipt_pays_claim_for(&receipt(&request, "c1"), "c2", &payer));
        assert!(!receipt_pays_claim_for(&receipt(&request, "c2"), "c2", &payer));
        let stranger = Keys::generate().public_key().to_hex();
        assert!(!receipt_pays_claim_for(&receipt(&request, "c1"), "c1", &stranger));
        // A request whose signature does not hold is not the payer's.
        let mut forged: serde_json::Value = serde_json::to_value(&request).unwrap();
        forged["content"] = "x".into();
        let forged: Event = serde_json::from_value(forged).unwrap();
        assert!(!receipt_pays_claim_for(&receipt(&forged, "c1"), "c1", &payer));
    }

    #[test]
    fn a_receipt_pays_a_bounty_claim_only_for_its_reward_and_list() {
        let issuer = Keys::generate();
        let zapper = Keys::generate();
        let relays = vec!["wss://nos.lol".to_string()];
        let receipt = |request: &Event| -> Event {
            EventBuilder::new(Kind::Custom(RECEIPT_KIND), "")
                .tags([
                    tag(["e", "c1"]).unwrap(),
                    tag(["description", serde_json::to_string(request).unwrap().as_str()]).unwrap(),
                ])
                .finalize(&zapper)
                .unwrap()
        };
        let payer = issuer.public_key().to_hex();
        let right = zap_request(&issuer, &"aa".repeat(32), "c1", COORD, "b1", 100, &relays).unwrap();
        assert!(receipt_pays_bounty_claim(&receipt(&right), "c1", &payer, COORD, "b1", 100));
        // One sat short, or a zap aimed at another list, pays nothing here.
        let short = zap_request(&issuer, &"aa".repeat(32), "c1", COORD, "b1", 99, &relays).unwrap();
        assert!(!receipt_pays_bounty_claim(&receipt(&short), "c1", &payer, COORD, "b1", 100));
        let other = zap_request(&issuer, &"aa".repeat(32), "c1", "39998:aa:other", "b1", 100, &relays).unwrap();
        assert!(!receipt_pays_bounty_claim(&receipt(&other), "c1", &payer, COORD, "b1", 100));
    }

    #[test]
    fn a_receipt_pays_only_the_bounty_its_request_names() {
        let issuer = Keys::generate();
        let zapper = Keys::generate();
        let relays = vec!["wss://nos.lol".to_string()];
        let receipt = |request: &Event| -> Event {
            EventBuilder::new(Kind::Custom(RECEIPT_KIND), "")
                .tags([
                    tag(["e", "c1"]).unwrap(),
                    tag(["description", serde_json::to_string(request).unwrap().as_str()]).unwrap(),
                ])
                .finalize(&zapper)
                .unwrap()
        };
        let payer = issuer.public_key().to_hex();
        let for_b1 = zap_request(&issuer, &"aa".repeat(32), "c1", COORD, "b1", 100, &relays).unwrap();
        assert!(receipt_pays_bounty_claim(&receipt(&for_b1), "c1", &payer, COORD, "b1", 100));
        assert!(!receipt_pays_bounty_claim(&receipt(&for_b1), "c1", &payer, COORD, "b2", 100));
        // A request with no bounty tag (a payment by hand from another
        // client) pays the claim on every bounty on its list.
        let untagged = EventBuilder::new(Kind::Custom(ZAP_REQUEST_KIND), "")
            .tags([
                tag(["p", "aa"]).unwrap(),
                tag(["e", "c1"]).unwrap(),
                tag(["a", COORD]).unwrap(),
                tag(["amount", "100000"]).unwrap(),
            ])
            .finalize(&issuer)
            .unwrap();
        assert!(receipt_pays_claim_for(&receipt(&untagged), "c1", &payer));
        assert!(receipt_pays_bounty_claim(&receipt(&untagged), "c1", &payer, COORD, "b1", 100));
        assert!(receipt_pays_bounty_claim(&receipt(&untagged), "c1", &payer, COORD, "b2", 100));
        // Still only for the reward, and only on the list it names.
        assert!(!receipt_pays_bounty_claim(&receipt(&untagged), "c1", &payer, COORD, "b1", 99));
        assert!(!receipt_pays_bounty_claim(&receipt(&untagged), "c1", &payer, "39998:aa:other", "b1", 100));
    }

    #[test]
    fn a_non_receipt_does_not_parse() {
        let keys = Keys::generate();
        let event = claim(&keys, "Memphis", COORD).unwrap();
        assert!(parse_receipt(&event).is_none());
    }
}
