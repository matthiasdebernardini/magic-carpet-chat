//! A small in-memory diagnostic log. The runtime shows fixed wording on
//! screen and drops the underlying network error; the sites that do so
//! [`note`] the real cause here, and "Copy debug info" reads it back with
//! [`snapshot`].
//!
//! Callers never pass a secret: no nsec, Coinos password or token, no request
//! body or header. [`redact`] is a backstop for a stray nsec, not the defence.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::timefmt;

/// How many lines are kept; the oldest is dropped past this.
const CAP: usize = 500;

static LOG: Mutex<Log> = Mutex::new(Log {
    lines: VecDeque::new(),
    last: String::new(),
    repeats: 1,
});

/// The kept lines, plus the last message and how many times in a row it
/// came, so a relay that fails on every poll takes one line, not all 500.
struct Log {
    lines: VecDeque<String>,
    last: String,
    repeats: u32,
}

/// Append one line, stamped with the UTC time and run through [`redact`].
pub fn note(line: impl Into<String>) {
    let now = timefmt::now_unix();
    let stamp = format!("{} {}Z", timefmt::month_day_year(now), timefmt::hhmmss_utc(now));
    let message = redact(&line.into());
    let mut log = LOG.lock().unwrap_or_else(|p| p.into_inner());
    record(&mut log, &stamp, message);
}

/// A message equal to the one before it replaces that line with the new
/// time and a count; any other message is a new line.
fn record(log: &mut Log, stamp: &str, message: String) {
    if !log.lines.is_empty() && log.last == message {
        log.repeats += 1;
        log.lines.pop_back();
        log.lines
            .push_back(format!("{stamp} {message} (x{} in a row)", log.repeats));
        return;
    }
    push(&mut log.lines, format!("{stamp} {message}"));
    log.last = message;
    log.repeats = 1;
}

/// Every kept line, oldest first.
pub fn snapshot() -> Vec<String> {
    let log = LOG.lock().unwrap_or_else(|p| p.into_inner());
    log.lines.iter().cloned().collect()
}

fn push(log: &mut VecDeque<String>, line: String) {
    if log.len() == CAP {
        log.pop_front();
    }
    log.push_back(line);
}

const BECH32: &str = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";

/// Replaces every `nsec1` and the bech32 characters after it with
/// `<redacted>`, in either case: bech32 may be written all in capitals.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // ASCII lowercasing keeps every byte offset, so `at` indexes `rest` too.
    while let Some(at) = rest.to_ascii_lowercase().find("nsec1") {
        let after = &rest[at + "nsec1".len()..];
        let key_len = after
            .find(|c: char| !BECH32.contains(c.to_ascii_lowercase()))
            .unwrap_or(after.len());
        if key_len == 0 {
            out.push_str(&rest[..at + "nsec1".len()]);
        } else {
            out.push_str(&rest[..at]);
            out.push_str("<redacted>");
        }
        rest = &after[key_len..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NSEC: &str = "nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5";
    const NPUB: &str = "npub1sg6plzptd64u62a878hep2kev88swjh3tw00gjsfl8f237lmu63q0uf63m";

    #[test]
    fn redact_removes_an_nsec_inside_a_longer_line() {
        let line = format!("signing with {NSEC}, then retry; owner {NPUB}.");
        let clean = redact(&line);
        assert_eq!(clean, format!("signing with <redacted>, then retry; owner {NPUB}."));
        assert!(!clean.contains("nsec1"));
    }

    #[test]
    fn redact_removes_an_nsec_written_in_capitals() {
        let clean = redact(&format!("key {} end", NSEC.to_ascii_uppercase()));
        assert_eq!(clean, "key <redacted> end");
    }

    #[test]
    fn a_repeated_message_takes_one_line_with_a_count() {
        let mut log = Log { lines: VecDeque::new(), last: String::new(), repeats: 1 };
        record(&mut log, "t1", "relay down".into());
        record(&mut log, "t2", "relay down".into());
        record(&mut log, "t3", "relay down".into());
        record(&mut log, "t4", "other".into());
        let lines: Vec<&str> = log.lines.iter().map(String::as_str).collect();
        assert_eq!(lines, ["t3 relay down (x3 in a row)", "t4 other"]);
    }

    #[test]
    fn the_log_drops_its_oldest_line_past_the_cap() {
        let mut log = VecDeque::new();
        for n in 0..CAP + 3 {
            push(&mut log, format!("line {n}"));
        }
        assert_eq!(log.len(), CAP);
        assert_eq!(log.front().map(String::as_str), Some("line 3"));
        assert_eq!(log.back().map(String::as_str), Some("line 502"));
    }

    #[test]
    fn note_stamps_and_redacts() {
        note(format!("boom {NSEC}"));
        // The log is process-wide and `cargo test` runs tests on threads, so
        // another test's line may follow this one: look for it, not at the end.
        let lines = snapshot();
        let line = lines
            .iter()
            .find(|line| line.contains("boom <redacted>"))
            .expect("the noted line");
        assert!(line.contains("Z boom <redacted>"), "{line}");
        assert!(!line.contains("nsec1"), "{line}");
    }
}
