//! Client for the Magic Carpet instance REST API.
//!
//! One [`Api`] per account: the kind-22242 login lives in a session cookie,
//! and the cookie jar is per-client, so sharing an `Api` between the issuer
//! and the claimant would cross their sessions.
//!
//! Response types mirror what the live server actually sends (see
//! tests/fixtures/bounty-5f44688e.json, fetched read-only from prod), with
//! `Option` everywhere prod drift allows: live `autoPayment` rows omit
//! `claimant_pubkey`/`claim_address`, and live claims can omit `claimAddress`
//! entirely.

use std::time::Duration;

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::events::{self, Receipt};

pub const API_BASE: &str = "https://magic-carpet.brainstorm.world";

/// `MC_BASE_URL` overrides the prod instance, for rehearsals against a local
/// server.
pub fn base_url() -> String {
    std::env::var("MC_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| API_BASE.to_string())
}

#[derive(thiserror::Error, Debug)]
pub enum ApiError {
    #[error("http client could not be built: {0}")]
    Client(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("api returned {status}: {message}")]
    Status { status: u16, message: String },
    #[error("login denied: {0}")]
    Denied(String),
    #[error("could not sign: {0}")]
    Sign(String),
    #[error("could not read api response: {0}")]
    Decode(String),
    /// HTTP 200 with `success: false` — the server's way of refusing.
    #[error("{0}")]
    Rejected(String),
}

/// POST /api/bounties body. The issuer is always the session pubkey; the
/// server ignores any issuer in the body. Field spelling mirrors
/// `buildBountyCreateBody` in magic-carpet-v2/bin/agent.js, including the
/// omission of unset flags.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBounty {
    pub list_coordinate: String,
    pub amount_sats: u64,
    pub criteria: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounty_cap_sats: Option<u64>,
    #[serde(skip_serializing_if = "is_false")]
    pub reward_per_item: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rewards_per_npub: Option<u64>,
    /// Needs owner/admin/allowlist on the server (403 otherwise). The house
    /// issuer 853baa94… is allowlisted.
    #[serde(skip_serializing_if = "is_false")]
    pub auto_pay: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_pay_min_rank: Option<u64>,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// A bounty row as the server returns it. SQLite stores booleans as 0/1 and
/// the API passes them straight through, so they are kept raw with helpers.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Bounty {
    pub id: String,
    pub issuer_pubkey: String,
    pub list_coordinate: String,
    pub amount_sats: u64,
    #[serde(default)]
    pub criteria: Option<String>,
    #[serde(default)]
    pub expiration: Option<u64>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub bounty_cap_sats: Option<u64>,
    #[serde(default)]
    pub reward_per_item: Option<serde_json::Value>,
    #[serde(default)]
    pub max_rewards_per_npub: Option<u64>,
    #[serde(default)]
    pub auto_pay: Option<serde_json::Value>,
    #[serde(default)]
    pub auto_pay_min_rank: Option<u64>,
    #[serde(rename = "derivedStatus", default)]
    pub derived_status: Option<String>,
    #[serde(rename = "paymentState", default)]
    pub payment_state: Option<PaymentState>,
}

fn truthy(value: &Option<serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0) != 0,
        _ => false,
    }
}

impl Bounty {
    pub fn auto_pay_on(&self) -> bool {
        truthy(&self.auto_pay)
    }

    pub fn reward_per_item_on(&self) -> bool {
        truthy(&self.reward_per_item)
    }

    /// `derivedStatus` when the server computed one, otherwise the raw status.
    pub fn effective_status(&self) -> &str {
        self.derived_status
            .as_deref()
            .or(self.status.as_deref())
            .unwrap_or("unknown")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PaymentState {
    pub reward_amount_sats: u64,
    pub bounty_cap_sats: u64,
    pub reward_per_item: bool,
    pub max_rewards_per_npub: Option<u64>,
    pub total_reward_slots: u64,
    pub paid_reward_count: u64,
    pub held_reward_count: u64,
    pub remaining_reward_slots: u64,
    pub payable_reward_count: u64,
    pub reconciliation_reward_count: u64,
    pub open_reward_slots: u64,
    pub fulfilled: bool,
}


/// An event as the API stores it. Kept as raw JSON fields — not a
/// `nostr_sdk::Event` — because the server's durable-only rows carry events
/// with no signature, which a strict Event parse would reject.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RawEvent {
    pub id: String,
    pub pubkey: String,
    #[serde(default)]
    pub kind: Option<u16>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub tags: Vec<Vec<String>>,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub sig: Option<String>,
}

impl RawEvent {
    pub fn tag_value(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.first().map(String::as_str) == Some(name))
            .and_then(|t| t.get(1).map(String::as_str))
    }

    /// Parse this event as a receipt through the same rules as a live relay
    /// event. `None` when it is not kind-9735.
    pub fn receipt(&self) -> Option<Receipt> {
        events::receipt_from_parts(
            self.kind.unwrap_or(0),
            &self.id,
            self.created_at.unwrap_or(0),
            &self.tags,
        )
    }
}

/// The state-machine row from the server's `auto_payments` table.
/// `attempting -> paid -> settled | paid_unreceipted (receipt_timeout)`;
/// `attempting -> failed`. Live rows omit `claimant_pubkey`/`claim_address`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AutoPaymentRow {
    pub id: String,
    pub claim_event_id: String,
    pub bounty_id: String,
    #[serde(default)]
    pub issuer_pubkey: Option<String>,
    #[serde(default)]
    pub claimant_pubkey: Option<String>,
    #[serde(default)]
    pub claim_address: Option<String>,
    #[serde(default)]
    pub amount_sats: Option<u64>,
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub updated_at: Option<u64>,
}

impl AutoPaymentRow {
    /// Still in flight: no receipt decision yet.
    pub fn is_pending(&self) -> bool {
        matches!(self.state.as_str(), "attempting" | "paid")
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Claim {
    pub event: RawEvent,
    #[serde(rename = "zapReceipt", default)]
    pub zap_receipt: Option<RawEvent>,
    #[serde(rename = "autoPayment", default)]
    pub auto_payment: Option<AutoPaymentRow>,
    #[serde(rename = "claimAddress", default)]
    pub claim_address: Option<String>,
    #[serde(rename = "paymentClaimEventId", default)]
    pub payment_claim_event_id: Option<String>,
    #[serde(rename = "paymentStatus", default)]
    pub payment_status: Option<String>,
    #[serde(rename = "paymentAmountSats", default)]
    pub payment_amount_sats: Option<u64>,
    #[serde(rename = "autoPayBlockedReason", default)]
    pub auto_pay_blocked_reason: Option<String>,
}

impl Claim {
    /// What the claimant named: the `name` tag (UI-shaped claims), falling
    /// back to the content (agent-CLI claims like the fixture's "Memphis").
    pub fn item_name(&self) -> &str {
        self.event
            .tag_value("name")
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.event.content)
    }

    pub fn receipt(&self) -> Option<Receipt> {
        self.zap_receipt.as_ref().and_then(RawEvent::receipt)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BountyDetail {
    pub bounty: Bounty,
    pub claims: Vec<Claim>,
}

#[derive(Debug, Clone, Deserialize)]
struct BountyDetailResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    error: Option<String>,
    bounty: Option<Bounty>,
    #[serde(default)]
    claims: Vec<Claim>,
}

/// Split from the network call so the fixture test and the watcher's tests
/// exercise the exact parse path `get_bounty` uses.
pub(crate) fn parse_detail(json: &str) -> Result<BountyDetail, ApiError> {
    let resp: BountyDetailResponse =
        serde_json::from_str(json).map_err(|e| ApiError::Decode(e.to_string()))?;
    if !resp.success {
        return Err(ApiError::Rejected(
            resp.error.unwrap_or_else(|| "bounty lookup failed".into()),
        ));
    }
    let bounty = resp
        .bounty
        .ok_or_else(|| ApiError::Decode("response held no bounty".into()))?;
    Ok(BountyDetail {
        bounty,
        claims: resp.claims,
    })
}

#[derive(Debug, Clone, Deserialize)]
struct BountyListResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    bounties: Vec<Bounty>,
}

/// Split from the network call so the list parse is testable offline, like
/// [`parse_detail`].
pub(crate) fn parse_bounty_list(json: &str) -> Result<Vec<Bounty>, ApiError> {
    let resp: BountyListResponse =
        serde_json::from_str(json).map_err(|e| ApiError::Decode(e.to_string()))?;
    if !resp.success {
        return Err(ApiError::Rejected(
            resp.error.unwrap_or_else(|| "bounty list failed".into()),
        ));
    }
    Ok(resp.bounties)
}

#[derive(Debug, Clone, Deserialize)]
struct VerifyUserResponse {
    #[serde(default)]
    authorized: bool,
    #[serde(default)]
    challenge: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct LoginUserResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateBountyResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    bounty: Option<Bounty>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PublishResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ScanResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    events: Vec<RawEvent>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    /// A fresh client with its own cookie jar, against `base_url()`.
    pub fn new() -> Result<Self, ApiError> {
        Self::with_base(base_url())
    }

    pub fn with_base(base: impl Into<String>) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .user_agent(crate::USER_AGENT)
            .cookie_store(true)
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| ApiError::Client(e.to_string()))?;
        Ok(Self {
            base: base.into().trim_end_matches('/').to_string(),
            http,
        })
    }

    async fn read<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, ApiError> {
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| ApiError::Http(e.to_string()))?;
        if !status.is_success() {
            // The server puts its reason in `error` (or `message` on auth).
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .or_else(|| v.get("message"))
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| text.chars().take(200).collect());
            return Err(ApiError::Status {
                status: status.as_u16(),
                message,
            });
        }
        serde_json::from_str(&text).map_err(|e| ApiError::Decode(e.to_string()))
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl Serialize,
    ) -> Result<T, ApiError> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .map_err(|e| ApiError::Http(e.to_string()))?;
        Self::read(response).await
    }

    /// The kind-22242 login flow: verify-user hands out a challenge, the
    /// signed challenge comes back through login-user, and the session lands
    /// in this client's cookie jar.
    pub async fn login(&self, keys: &Keys) -> Result<(), ApiError> {
        let verify: VerifyUserResponse = self
            .post_json(
                "/api/auth/verify-user",
                &serde_json::json!({ "pubkey": keys.public_key().to_hex() }),
            )
            .await?;
        if !verify.authorized {
            return Err(ApiError::Denied(
                verify.message.unwrap_or_else(|| "not authorized".into()),
            ));
        }
        let challenge = verify
            .challenge
            .ok_or_else(|| ApiError::Decode("verify-user sent no challenge".into()))?;
        let event = EventBuilder::new(Kind::Custom(22242), "Tapestry authentication")
            .tags([Tag::parse(vec!["challenge".to_string(), challenge])
                .map_err(|e| ApiError::Sign(e.to_string()))?])
            .finalize(keys)
            .map_err(|e| ApiError::Sign(e.to_string()))?;
        let login: LoginUserResponse = self
            .post_json("/api/auth/login-user", &serde_json::json!({ "event": event }))
            .await?;
        if !login.success {
            return Err(ApiError::Denied(
                login.message.unwrap_or_else(|| "login failed".into()),
            ));
        }
        Ok(())
    }

    /// Needs a prior `login` on this same `Api` (session cookie).
    pub async fn create_bounty(&self, req: &CreateBounty) -> Result<Bounty, ApiError> {
        let resp: CreateBountyResponse = self.post_json("/api/bounties", req).await?;
        if !resp.success {
            return Err(ApiError::Rejected(
                resp.error.unwrap_or_else(|| "bounty creation refused".into()),
            ));
        }
        resp.bounty
            .ok_or_else(|| ApiError::Decode("create-bounty response held no bounty".into()))
    }

    /// GET /api/bounties?issuer=…&status=all — every bounty this issuer
    /// posted, each with its `paymentState` counters. Public, no login.
    pub async fn list_bounties(&self, issuer: &str) -> Result<Vec<Bounty>, ApiError> {
        let response = self
            .http
            .get(format!("{}/api/bounties", self.base))
            .query(&[("issuer", issuer), ("status", "all")])
            .send()
            .await
            .map_err(|e| ApiError::Http(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| ApiError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(ApiError::Status {
                status: status.as_u16(),
                message: text.chars().take(200).collect(),
            });
        }
        parse_bounty_list(&text)
    }

    pub async fn get_bounty(&self, id: &str) -> Result<BountyDetail, ApiError> {
        let response = self
            .http
            .get(format!("{}/api/bounties/{id}", self.base))
            .send()
            .await
            .map_err(|e| ApiError::Http(e.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| ApiError::Http(e.to_string()))?;
        if !status.is_success() {
            return Err(ApiError::Status {
                status: status.as_u16(),
                message: text.chars().take(200).collect(),
            });
        }
        parse_detail(&text)
    }

    /// Publish a signed event through the instance. This is the only door:
    /// a claim published to public relays never reaches the bounty machinery.
    /// The endpoint answers HTTP 200 with `success:false` on a strfry import
    /// failure, so the body is checked, not just the status.
    pub async fn publish_event(&self, event: &Event) -> Result<(), ApiError> {
        let resp: PublishResponse = self
            .post_json(
                "/api/strfry/publish",
                &serde_json::json!({ "event": event, "signAs": "client" }),
            )
            .await?;
        if !resp.success {
            return Err(ApiError::Rejected(
                resp.error.unwrap_or_else(|| "strfry publish failed".into()),
            ));
        }
        Ok(())
    }

    /// GET /api/strfry/scan with a Nostr filter (public, read-only).
    pub async fn scan(&self, filter: &serde_json::Value) -> Result<Vec<RawEvent>, ApiError> {
        let response = self
            .http
            .get(format!("{}/api/strfry/scan", self.base))
            .query(&[("filter", filter.to_string())])
            .send()
            .await
            .map_err(|e| ApiError::Http(e.to_string()))?;
        let resp: ScanResponse = Self::read(response).await?;
        if !resp.success {
            return Err(ApiError::Rejected(
                resp.error.unwrap_or_else(|| "strfry scan failed".into()),
            ));
        }
        Ok(resp.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/bounty-5f44688e.json");

    #[test]
    fn the_live_fixture_parses_through_the_real_path() {
        let detail = parse_detail(FIXTURE).unwrap();

        let bounty = &detail.bounty;
        assert_eq!(bounty.id, "5f44688e-fcb3-4c6a-bee6-e287995f7d3e");
        assert_eq!(
            bounty.issuer_pubkey,
            "853baa94b4b12d23931ade03ceb854a2b36cf1e24b5e3a82e68c8ca3a8ced2ba"
        );
        assert_eq!(bounty.amount_sats, 100);
        // SQLite booleans arrive as 1, not true.
        assert!(bounty.auto_pay_on());
        assert!(bounty.reward_per_item_on());
        assert_eq!(bounty.auto_pay_min_rank, Some(3));
        assert_eq!(bounty.effective_status(), "fulfilled");
        let state = bounty.payment_state.as_ref().unwrap();
        assert!(state.fulfilled);
        assert_eq!(state.paid_reward_count, 1);

        assert_eq!(detail.claims.len(), 1);
        let claim = &detail.claims[0];
        assert_eq!(claim.item_name(), "Memphis");
        assert_eq!(claim.payment_status.as_deref(), Some("paid"));
        assert_eq!(claim.payment_amount_sats, Some(100));

        // Prod drift the types must tolerate: the live autoPayment row omits
        // claimant_pubkey and claim_address, and the claim omits claimAddress.
        let row = claim.auto_payment.as_ref().unwrap();
        assert_eq!(row.state, "settled");
        assert!(!row.is_pending());
        assert_eq!(row.claimant_pubkey, None);
        assert_eq!(row.claim_address, None);
        assert_eq!(row.updated_at, Some(1_786_093_481));
        assert_eq!(claim.claim_address, None);

        // The receipt round-trips through the shared parser.
        let receipt = claim.receipt().unwrap();
        assert_eq!(receipt.amount_sats(), Some(100));
        assert!(receipt.pays_claim(&claim.event.id));
    }

    #[test]
    fn a_pending_row_and_a_failed_row_classify_correctly() {
        for (state, pending) in [
            ("attempting", true),
            ("paid", true),
            ("settled", false),
            ("paid_unreceipted", false),
            ("failed", false),
        ] {
            let row: AutoPaymentRow = serde_json::from_str(&format!(
                r#"{{"id":"x","claim_event_id":"c","bounty_id":"b","state":"{state}"}}"#
            ))
            .unwrap();
            assert_eq!(row.is_pending(), pending, "{state}");
        }
    }

    #[test]
    fn create_bounty_serializes_like_the_agent_cli() {
        let full = CreateBounty {
            list_coordinate: "39998:aa:us-city".into(),
            amount_sats: 100,
            criteria: "US cities".into(),
            bounty_cap_sats: Some(400),
            reward_per_item: true,
            max_rewards_per_npub: Some(1),
            auto_pay: true,
            auto_pay_min_rank: Some(2),
        };
        assert_eq!(
            serde_json::to_value(&full).unwrap(),
            serde_json::json!({
                "listCoordinate": "39998:aa:us-city",
                "amountSats": 100,
                "criteria": "US cities",
                "bountyCapSats": 400,
                "rewardPerItem": true,
                "maxRewardsPerNpub": 1,
                "autoPay": true,
                "autoPayMinRank": 2,
            })
        );

        // Unset flags are omitted, not sent as false/null.
        let minimal = CreateBounty {
            list_coordinate: "39998:aa:us-city".into(),
            amount_sats: 100,
            criteria: "US cities".into(),
            bounty_cap_sats: None,
            reward_per_item: false,
            max_rewards_per_npub: None,
            auto_pay: false,
            auto_pay_min_rank: None,
        };
        assert_eq!(
            serde_json::to_value(&minimal).unwrap(),
            serde_json::json!({
                "listCoordinate": "39998:aa:us-city",
                "amountSats": 100,
                "criteria": "US cities",
            })
        );
    }

    #[test]
    fn a_success_false_body_is_a_rejection_not_a_parse_error() {
        let err = parse_detail(r#"{"success":false,"error":"bounty not found"}"#).unwrap_err();
        assert!(matches!(err, ApiError::Rejected(ref m) if m == "bounty not found"));
    }

    #[test]
    fn the_bounty_list_parses_through_the_real_path() {
        // The list endpoint returns the same bountyForClient rows as the
        // detail endpoint, so the fixture's bounty doubles as a list row.
        let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let list = serde_json::json!({
            "success": true,
            "bounties": [fixture["bounty"].clone()],
        });
        let bounties = parse_bounty_list(&list.to_string()).unwrap();
        assert_eq!(bounties.len(), 1);
        assert_eq!(bounties[0].id, "5f44688e-fcb3-4c6a-bee6-e287995f7d3e");
        assert!(bounties[0].auto_pay_on());

        let err = parse_bounty_list(r#"{"success":false,"error":"nope"}"#).unwrap_err();
        assert!(matches!(err, ApiError::Rejected(ref m) if m == "nope"));
    }
}
