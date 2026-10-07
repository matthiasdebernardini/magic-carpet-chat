//! The Brainstorm API (api.brainstorm.world): the challenge login, the
//! follow-list ingest, the graperank trigger, the `/setup` rows a Treasure
//! Map is built from, and the profile search the follow step uses.
//!
//! The pure half (parsers, error wording, the login event template) is
//! unit-tested here; the async half is only ever awaited on the tokio
//! runtime — gpui's executor would abort the process.

use std::time::Duration;

use nostr_sdk::prelude::*;

use crate::secrets::Secret;

/// `MC_BRAINSTORM_URL` replaces the base for EVERY Brainstorm call, so a
/// scripted test can point the whole feature at a stub. Trailing slashes are
/// trimmed; empty or unset means the live API.
pub fn base() -> String {
    base_from(std::env::var("MC_BRAINSTORM_URL").ok().as_deref())
}

fn base_from(env: Option<&str>) -> String {
    env.map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.trim_end_matches('/').to_string())
        .unwrap_or_else(|| "https://api.brainstorm.world".to_string())
}

/// One `/search/byText` result. `pubkey` is always 64 lowercase hex —
/// anything else the endpoint returned never makes it out of the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub pubkey: String,
    pub name: Option<String>,
    pub display_name: Option<String>,
    pub nip05: Option<String>,
    pub picture: Option<String>,
}

fn field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `data.results[]` of a `/search/byText` answer. `None` when the body is
/// not the expected envelope; hits with an off-shape pubkey are skipped.
pub fn parse_search(body: &str) -> Option<Vec<SearchHit>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let results = value.get("data")?.get("results")?.as_array()?;
    let mut hits = Vec::new();
    for result in results {
        let Some(pubkey) = field(result, "pubkey").filter(|p| is_hex64(p)) else {
            continue;
        };
        hits.push(SearchHit {
            pubkey: pubkey.to_string(),
            name: field(result, "name").map(String::from),
            display_name: field(result, "display_name").map(String::from),
            nip05: field(result, "nip05").map(String::from),
            picture: field(result, "picture").map(String::from),
        });
    }
    Some(hits)
}

/// The `/setup/{hex}` body: an array of rows of strings, kept VERBATIM and
/// in order — they become the kind-10040's tags unchanged. `None` for a
/// non-array body, an empty array, any row under 2 cells, or any non-string
/// cell: a map built from a partial read would be wrong, not just thin.
pub fn setup_rows(body: &str) -> Option<Vec<Vec<String>>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let rows = value.as_array()?;
    if rows.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let cells = row.as_array()?;
        if cells.len() < 2 {
            return None;
        }
        let mut parsed = Vec::with_capacity(cells.len());
        for cell in cells {
            parsed.push(cell.as_str()?.to_string());
        }
        out.push(parsed);
    }
    Some(out)
}

/// The wording Brainstorm's own UI uses for a failed graperank request.
/// Fixed strings — the server's raw body never reaches the screen.
pub fn error_text(status: u16, body: &str) -> &'static str {
    let lower = body.to_lowercase();
    let asked_to_wait = ["recent", "rate", "too many", "wait", "cooldown"]
        .iter()
        .any(|word| lower.contains(word));
    if status == 429 || (status == 403 && asked_to_wait) {
        "Please wait a few minutes before recalculating. The server needs time between requests."
    } else if matches!(status, 502..=504) {
        "The Brainstorm server is temporarily unavailable. Please wait a few minutes and try again."
    } else {
        "Something went wrong. Please wait a moment and try again."
    }
}

/// The one login template: kind 22242, the challenge in a tag, empty
/// content, signed locally (Brainstorm's `loginTemplate`).
pub fn login_event(keys: &Keys, challenge: &str) -> Event {
    EventBuilder::new(Kind::Custom(22242), "")
        .tags(vec![
            Tag::parse(["t", "brainstorm_login"]).expect("a fixed tag parses"),
            Tag::parse(["challenge", challenge]).expect("two strings parse"),
        ])
        .finalize(keys)
        .expect("in-memory keys sign")
}

/// How a Brainstorm call failed. `Unauthorized` is split out because it is
/// the one failure the caller can fix: mint a fresh token and retry once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fail {
    Unauthorized,
    Text(&'static str),
}

const GENERIC: &str = "Something went wrong. Please wait a moment and try again.";

fn http() -> Result<reqwest::Client, &'static str> {
    reqwest::Client::builder()
        .user_agent(crate::USER_AGENT)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| GENERIC)
}

/// The response body, kept only long enough to classify — never shown.
async fn send(request: reqwest::RequestBuilder) -> Result<(reqwest::StatusCode, String), Fail> {
    let response = request.send().await.map_err(|_| Fail::Text(GENERIC))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Ok((status, body))
}

/// `GET {base}/authChallenge/{hex}` → the challenge string to sign.
pub async fn auth_challenge(hex: &str) -> Result<String, &'static str> {
    let url = format!("{}/authChallenge/{hex}", base());
    let (status, body) = send(http()?.get(url)).await.map_err(|f| match f {
        Fail::Unauthorized => GENERIC,
        Fail::Text(text) => text,
    })?;
    if !status.is_success() {
        return Err(error_text(status.as_u16(), &body));
    }
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|_| GENERIC)?;
    value
        .get("data")
        .and_then(|data| field(data, "challenge"))
        .map(String::from)
        .ok_or(GENERIC)
}

/// `POST {base}/authChallenge/{hex}/verify` with the signed kind-22242 → the
/// session token. It returns inside a [`Secret`] and never leaves the
/// runtime thread: no disk, no log, no Update.
pub async fn verify_challenge(hex: &str, event: &Event) -> Result<Secret, &'static str> {
    let url = format!("{}/authChallenge/{hex}/verify", base());
    let body = serde_json::json!({ "signed_event": event });
    let (status, body) = send(http()?.post(url).json(&body))
        .await
        .map_err(|f| match f {
            Fail::Unauthorized => GENERIC,
            Fail::Text(text) => text,
        })?;
    if !status.is_success() {
        return Err(error_text(status.as_u16(), &body));
    }
    serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("data")?.get("token")?.as_str().map(Secret::new))
        .ok_or(GENERIC)
}

/// `POST {base}/user/followList` — ingest the signed kind-3 so scoring runs
/// on real follows rather than relay propagation. Shared rate limit: 3
/// requests per IP per 30 min, so the caller never re-sends an accepted id.
pub async fn post_follow_list(token: &Secret, event: &Event) -> Result<(), Fail> {
    let url = format!("{}/user/followList", base());
    let body = serde_json::json!({ "signed_event": event });
    let (status, body) = send(
        http().map_err(Fail::Text)?
            .post(url)
            .json(&body)
            .header("Authorization", format!("Bearer {}", token.expose()))
            .header("access_token", token.expose()),
    )
    .await?;
    if status.is_success() {
        Ok(())
    } else if status == reqwest::StatusCode::UNAUTHORIZED {
        Err(Fail::Unauthorized)
    } else {
        Err(Fail::Text(error_text(status.as_u16(), &body)))
    }
}

/// `POST {base}/user/graperank` — ask for the run. Rate limit shared with
/// `post_follow_list`.
pub async fn post_graperank(token: &Secret) -> Result<(), Fail> {
    let url = format!("{}/user/graperank", base());
    let (status, body) = send(
        http().map_err(Fail::Text)?
            .post(url)
            .header("Authorization", format!("Bearer {}", token.expose()))
            .header("access_token", token.expose()),
    )
    .await?;
    if status.is_success() {
        Ok(())
    } else if status == reqwest::StatusCode::UNAUTHORIZED {
        Err(Fail::Unauthorized)
    } else {
        Err(Fail::Text(error_text(status.as_u16(), &body)))
    }
}

/// `GET {base}/setup/{hex}` (no auth): `Ok(Some(rows))` on 200, `Ok(None)`
/// on 404 — the npub has no setup yet.
pub async fn get_setup(hex: &str) -> Result<Option<Vec<Vec<String>>>, &'static str> {
    let url = format!("{}/setup/{hex}", base());
    let (status, body) = send(http()?.get(url)).await.map_err(|f| match f {
        Fail::Unauthorized => GENERIC,
        Fail::Text(text) => text,
    })?;
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(error_text(status.as_u16(), &body));
    }
    setup_rows(&body).map(Some).ok_or(GENERIC)
}

/// `GET {base}/search/byText` (no auth): the follow step's profile search.
/// Fixed failure text; a malformed 200 reads as an error, not as "no hits".
pub async fn search_by_text(query: &str) -> Result<Vec<SearchHit>, &'static str> {
    let url = format!("{}/search/byText", base());
    let (status, body) = send(
        http()?.get(url).query(&[
            ("text", query),
            ("onlyRanked", "true"),
            ("ownPubkey", "false"),
            ("maxHits", "12"),
        ]),
    )
    .await
    .map_err(|f| match f {
        Fail::Unauthorized => GENERIC,
        Fail::Text(text) => text,
    })?;
    if !status.is_success() {
        return Err(error_text(status.as_u16(), &body));
    }
    parse_search(&body).ok_or(GENERIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_defaults_and_trims() {
        assert_eq!(base_from(None), "https://api.brainstorm.world");
        assert_eq!(base_from(Some("")), "https://api.brainstorm.world");
        assert_eq!(base_from(Some("  ")), "https://api.brainstorm.world");
        assert_eq!(base_from(Some("http://localhost:8080/")), "http://localhost:8080");
        assert_eq!(
            base_from(Some("http://localhost:8080///")),
            "http://localhost:8080"
        );
    }

    #[test]
    fn parse_search_reads_results_and_skips_bad_pubkeys() {
        let key = "82341f88".to_string() + &"0".repeat(56);
        let body = format!(
            r#"{{"code":200,"data":{{"query":"jack","numResults":3,"results":[
                {{"name":"jack","pubkey":"{key}","picture":"https://x/p.png","nip05":"jack@primal.net","display_name":"jack (n/acc)"}},
                {{"name":"bad","pubkey":"not-hex"}},
                {{"pubkey":"{key}","name":null}}
            ]}}}}"#
        );
        let hits = parse_search(&body).expect("a real answer parses");
        assert_eq!(hits.len(), 2, "the bad pubkey is skipped");
        assert_eq!(hits[0].name.as_deref(), Some("jack"));
        assert_eq!(hits[0].display_name.as_deref(), Some("jack (n/acc)"));
        assert_eq!(hits[0].nip05.as_deref(), Some("jack@primal.net"));
        assert_eq!(hits[1].name, None);
        // Not the envelope → None, not an empty list.
        assert_eq!(parse_search("{}"), None);
        assert_eq!(parse_search("[]"), None);
        assert_eq!(parse_search("not json"), None);
    }

    #[test]
    fn setup_rows_are_verbatim_or_nothing() {
        let provider = "c63c30b455f63f2bd9d92dd8c17f6e48646cb93a2aa5b12195e1a5157a04ee6b";
        let body = format!(
            r#"[["30382:rank","{provider}","wss://scores.brainstorm.world"],
                ["30382:followers","{provider}","wss://scores.brainstorm.world"],
                ["30382:reporters","{provider}","wss://scores.brainstorm.world"],
                ["30382:muters","{provider}","wss://scores.brainstorm.world"],
                ["30382:hops","{provider}","wss://scores.brainstorm.world"],
                ["30392","{provider}","wss://scores.brainstorm.world"]]"#
        );
        let rows = setup_rows(&body).expect("six rows");
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0][0], "30382:rank");
        assert_eq!(rows[5][0], "30392");
        assert_eq!(rows[5][2], "wss://scores.brainstorm.world");
        // Rejects.
        assert_eq!(setup_rows("{}"), None);
        assert_eq!(setup_rows("[]"), None);
        assert_eq!(setup_rows("not json"), None);
        assert_eq!(setup_rows(r#"[["30382:rank"]]"#), None, "under 2 cells");
        assert_eq!(
            setup_rows(&format!(r#"[["30382:rank","{provider}",7]]"#)),
            None,
            "a non-string cell"
        );
    }

    #[test]
    fn setup_rows_round_trip_the_live_house_body() {
        // Today's live `/setup` answer: five 30382 rows then a bare 30392.
        let rows = setup_rows(include_str!("../tests/fixtures/trust-setup-house-6.json"))
            .expect("the live body parses");
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0][0], "30382:rank");
        assert_eq!(rows[5][0], "30392");
    }

    #[test]
    fn error_text_matches_the_brainstorm_wording() {
        let wait = "Please wait a few minutes before recalculating. The server needs time between requests.";
        let down = "The Brainstorm server is temporarily unavailable. Please wait a few minutes and try again.";
        let generic = "Something went wrong. Please wait a moment and try again.";
        assert_eq!(error_text(429, ""), wait);
        assert_eq!(error_text(403, "request too recent"), wait);
        assert_eq!(error_text(403, "Rate limited"), wait);
        assert_eq!(error_text(403, "forbidden"), generic);
        assert_eq!(error_text(502, ""), down);
        assert_eq!(error_text(503, ""), down);
        assert_eq!(error_text(504, ""), down);
        assert_eq!(error_text(500, ""), generic);
        assert_eq!(error_text(400, "bad input"), generic);
    }

    #[test]
    fn login_event_is_kind_22242_with_the_challenge_tag() {
        let keys = Keys::generate();
        let event = login_event(&keys, "the-challenge");
        assert_eq!(event.kind, Kind::Custom(22242));
        assert_eq!(event.content, "");
        let tags: Vec<Vec<String>> = event
            .tags
            .iter()
            .map(|tag| tag.as_slice().to_vec())
            .collect();
        assert_eq!(
            tags,
            vec![
                vec!["t".to_string(), "brainstorm_login".to_string()],
                vec!["challenge".to_string(), "the-challenge".to_string()],
            ]
        );
        assert!(event.verify().is_ok());
    }
}
