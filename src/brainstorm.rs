//! The Brainstorm API (api.brainstorm.world): the challenge login, the
//! follow-list ingest, the graperank trigger and its run's progress and
//! history, the `/setup` rows a Treasure Map is built from, and the profile
//! search the follow step uses.
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

/// The line shown while the account follows no one — Brainstorm's own copy,
/// and the wording for a run that failed with `NO_ELIGIBLE_USERS`.
pub const FOLLOW_FIRST: &str = "Follow at least one account first so we can calculate your scores.";

/// Where a recalculation run stands, read from `GET /user/graperankResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Queued; `ahead` is how many runs Brainstorm will do first.
    Waiting { ahead: u64 },
    /// The scores are being calculated.
    Calculating,
    /// Calculated; the scores are being published.
    Publishing,
    /// Both publications succeeded.
    Done,
    /// The run failed; fixed wording from [`run_error_text`].
    Failed(&'static str),
}

impl RunState {
    /// Done or failed: nothing more will change, so polling stops.
    pub fn is_final(self) -> bool {
        matches!(self, RunState::Done | RunState::Failed(_))
    }
}

/// A failed run's `error.code` in plain words. Fixed strings — the server's
/// message never reaches the screen.
pub fn run_error_text(code: Option<&str>) -> &'static str {
    match code {
        Some("NO_ELIGIBLE_USERS") => FOLLOW_FIRST,
        Some("NEO4J_ERROR" | "RESULT_PAYLOAD_LOST") => {
            "Brainstorm had a problem on its side. Please wait a few minutes and try again."
        }
        _ => "Brainstorm couldn't calculate your scores. Please wait a few minutes and try again.",
    }
}

/// A `/user/graperankResult` body. `Some(None)` when the account has never
/// had a run (`data` is null); `None` when the body is not the envelope or
/// carries a status this client does not know.
///
/// Failed is `status` = `failure` (or a failed publication, which would
/// otherwise never finish); done is `ta_status` and
/// `internal_publication_status` both `success`.
pub fn parse_run(body: &str) -> Option<Option<RunState>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = value.get("data")?;
    if data.is_null() {
        return Some(None);
    }
    let status = field(data, "status")?;
    let ta = field(data, "ta_status");
    let internal = field(data, "internal_publication_status");
    let code = data.get("error").and_then(|error| field(error, "code"));
    let state = if status == "failure" || ta == Some("failure") || internal == Some("failure") {
        RunState::Failed(run_error_text(code))
    } else if ta == Some("success") && internal == Some("success") {
        RunState::Done
    } else {
        match status {
            "waiting" => RunState::Waiting {
                ahead: data
                    .get("how_many_others_with_priority")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            },
            "ongoing" => RunState::Calculating,
            "success" => RunState::Publishing,
            _ => return None,
        }
    };
    Some(Some(state))
}

/// A `/user/history` body: `data.last_time_calculated_graperank` as unix
/// seconds. `Some(None)` when no run has finished yet; `None` for a body
/// that is not the envelope or a time that does not parse.
pub fn parse_history(body: &str) -> Option<Option<u64>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = value.get("data")?.as_object()?;
    match data.get("last_time_calculated_graperank") {
        None | Some(serde_json::Value::Null) => Some(None),
        Some(time) => iso_utc(time.as_str()?).map(Some),
    }
}

/// `2026-10-09T12:34:56[.ffffff][Z|±HH:MM]` → unix seconds. No offset means
/// UTC: the server stores naive UTC times.
fn iso_utc(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b' ')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| num_of(text.get(range)?);
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut rest = &text[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &fraction[digits..];
    }
    let offset = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let zone = &rest[1..];
            if zone.len() != 5 || zone.as_bytes()[2] != b':' {
                return None;
            }
            let (h, m) = (num_of(zone.get(0..2)?)?, num_of(zone.get(3..5)?)?);
            sign * (h * 3_600 + m * 60)
        }
    };
    // Days from civil (Howard Hinnant's algorithm), the inverse of
    // `timefmt::month_day_year`.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset;
    u64::try_from(seconds).ok()
}

fn num_of(part: &str) -> Option<i64> {
    part.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| part.parse().ok())
        .flatten()
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

/// `GET {base}/user/graperankResult` — the latest run's state; `Ok(None)`
/// when the account never had one. Not rate-limited: the poll's read.
pub async fn get_graperank_result(token: &Secret) -> Result<Option<RunState>, Fail> {
    let url = format!("{}/user/graperankResult", base());
    let (status, body) = send(
        http().map_err(Fail::Text)?
            .get(url)
            .header("Authorization", format!("Bearer {}", token.expose()))
            .header("access_token", token.expose()),
    )
    .await?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Fail::Unauthorized);
    }
    if !status.is_success() {
        return Err(Fail::Text(error_text(status.as_u16(), &body)));
    }
    parse_run(&body).ok_or(Fail::Text(GENERIC))
}

/// `GET {base}/user/history` — when the last run finished, as unix seconds;
/// `Ok(None)` when none has (or Brainstorm has no history for the account).
pub async fn get_history(token: &Secret) -> Result<Option<u64>, Fail> {
    let url = format!("{}/user/history", base());
    let (status, body) = send(
        http().map_err(Fail::Text)?
            .get(url)
            .header("Authorization", format!("Bearer {}", token.expose()))
            .header("access_token", token.expose()),
    )
    .await?;
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Fail::Unauthorized);
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(Fail::Text(error_text(status.as_u16(), &body)));
    }
    parse_history(&body).ok_or(Fail::Text(GENERIC))
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
    fn parse_run_reads_each_state_of_a_run() {
        let run = |status: &str, ta: &str, internal: &str, extra: &str| {
            format!(
                r#"{{"code":200,"data":{{"private_id":7,"status":"{status}","ta_status":{ta},
                    "internal_publication_status":{internal},"count_values":null,"password":"x",
                    "algorithm":"graperank","parameters":"aa","how_many_others_with_priority":3,
                    "pubkey":"aa","created_at":"2026-10-09T12:00:00","updated_at":"2026-10-09T12:00:00"{extra}}}}}"#
            )
        };
        assert_eq!(
            parse_run(&run("waiting", r#""waiting""#, r#""waiting""#, "")),
            Some(Some(RunState::Waiting { ahead: 3 }))
        );
        assert_eq!(
            parse_run(&run("ongoing", r#""waiting""#, r#""waiting""#, "")),
            Some(Some(RunState::Calculating))
        );
        assert_eq!(
            parse_run(&run("success", r#""ongoing""#, r#""success""#, "")),
            Some(Some(RunState::Publishing))
        );
        assert_eq!(
            parse_run(&run("success", "null", "null", "")),
            Some(Some(RunState::Publishing))
        );
        assert_eq!(
            parse_run(&run("success", r#""success""#, r#""success""#, "")),
            Some(Some(RunState::Done))
        );
        assert_eq!(
            parse_run(&run(
                "failure",
                r#""waiting""#,
                r#""waiting""#,
                r#","error":{"code":"NO_ELIGIBLE_USERS","message":null}"#
            )),
            Some(Some(RunState::Failed(FOLLOW_FIRST)))
        );
        assert_eq!(
            parse_run(&run("failure", "null", "null", r#","error":null"#)),
            Some(Some(RunState::Failed(run_error_text(None))))
        );
        assert!(RunState::Done.is_final());
        assert!(RunState::Failed(FOLLOW_FIRST).is_final());
        assert!(!RunState::Publishing.is_final());
        // No run yet, versus a body that is not the envelope.
        assert_eq!(parse_run(r#"{"code":200,"data":null}"#), Some(None));
        assert_eq!(parse_run("{}"), None);
        assert_eq!(parse_run("not json"), None);
        assert_eq!(parse_run(&run("paused", "null", "null", "")), None);
    }

    #[test]
    fn run_error_codes_have_plain_fixed_wording() {
        assert_eq!(
            run_error_text(Some("NO_ELIGIBLE_USERS")),
            "Follow at least one account first so we can calculate your scores."
        );
        let theirs = "Brainstorm had a problem on its side. Please wait a few minutes and try again.";
        assert_eq!(run_error_text(Some("NEO4J_ERROR")), theirs);
        assert_eq!(run_error_text(Some("RESULT_PAYLOAD_LOST")), theirs);
        let generic =
            "Brainstorm couldn't calculate your scores. Please wait a few minutes and try again.";
        for code in [
            Some("MALFORMED_PARAMS"),
            Some("UNKNOWN_RELATIONSHIP"),
            Some("ALGORITHM_EXCEPTION"),
            Some("UNKNOWN"),
            Some("<script>"),
            None,
        ] {
            assert_eq!(run_error_text(code), generic);
        }
    }

    #[test]
    fn parse_history_reads_the_last_finished_time() {
        let body = |time: &str| {
            format!(
                r#"{{"code":200,"data":{{"pubkey":"aa","ta_pubkey":"bb",
                    "last_time_calculated_graperank":{time},"last_time_triggered_graperank":null,
                    "created_at":"2026-10-09T12:00:00","updated_at":"2026-10-09T12:00:00"}}}}"#
            )
        };
        // 2026-08-11 00:00:00 UTC is timefmt's own vector.
        assert_eq!(parse_history(&body(r#""2026-08-11T00:00:00""#)), Some(Some(1_786_406_400)));
        assert_eq!(
            parse_history(&body(r#""2026-08-11T00:00:05.123456""#)),
            Some(Some(1_786_406_405))
        );
        assert_eq!(parse_history(&body(r#""2026-08-11T00:00:00Z""#)), Some(Some(1_786_406_400)));
        assert_eq!(
            parse_history(&body(r#""2026-08-11T02:00:00+02:00""#)),
            Some(Some(1_786_406_400))
        );
        assert_eq!(parse_history(&body(r#""2020-02-29 00:00:00""#)), Some(Some(1_582_934_400)));
        assert_eq!(parse_history(&body("null")), Some(None));
        assert_eq!(parse_history(&body(r#""yesterday""#)), None);
        assert_eq!(parse_history(&body(r#""2026-13-01T00:00:00""#)), None);
        assert_eq!(parse_history(&body(r#""2026-08-11T00:00:00+0é:00""#)), None);
        assert_eq!(parse_history(r#"{"data":null}"#), None);
        assert_eq!(parse_history("not json"), None);
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
