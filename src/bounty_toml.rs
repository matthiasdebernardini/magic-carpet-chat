//! "Paste TOML" for the new-bounty form: the clipboard text, parsed into the
//! form's field values. Pure, so it is tested offline.

/// The TOML keys, in the form's field order (`BOUNTY_FIELD_DEFS`).
pub const KEYS: [&str; 8] = [
    "item_singular",
    "item_plural",
    "description",
    "criteria",
    "amount",
    "cap",
    "auto_pay",
    "min_rank",
];

/// How many leading keys are the list's own (hidden in existing-list mode).
const LIST_KEYS: usize = 3;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Number,
    Switch,
}

fn kind(ix: usize) -> Kind {
    match ix {
        4 | 5 | 7 => Kind::Number,
        6 => Kind::Switch,
        _ => Kind::Text,
    }
}

fn expected(kind: Kind) -> &'static str {
    match kind {
        Kind::Text => "a string",
        Kind::Number => "an integer (or a string of digits)",
        Kind::Switch => "a boolean (true or false)",
    }
}

/// Error text here is fixed wording: nothing taken from the clipboard (it can
/// hold a key or a password) ever appears in it, apart from the fixed key names.
const NOT_TOML: &str = "That is not valid TOML";

/// "line L, column C" of byte offset `at` in `text`.
fn line_column(text: &str, at: usize) -> (usize, usize) {
    let before = &text[..at.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, column)
}

/// A text value on one line: the form's inputs are single-line, so each run
/// of line breaks (with the whitespace around it) becomes one space.
fn one_line(s: &str) -> String {
    let s = s.replace("\r\n", "\n").replace(['\r', '\u{2028}', '\u{2029}'], "\n");
    s.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Parses the clipboard `text`. On success: (index into [`KEYS`], text for
/// that field) for every key present. On any error nothing is returned.
pub fn parse(text: Option<&str>, existing_list: bool) -> Result<Vec<(usize, String)>, String> {
    let text = text.map(str::trim).filter(|t| !t.is_empty());
    let Some(text) = text else {
        return Err("The clipboard has no text to paste.".to_string());
    };
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| match e.span() {
        Some(span) => {
            let (line, column) = line_column(text, span.start);
            format!("{NOT_TOML} (line {line}, column {column}).")
        }
        None => format!("{NOT_TOML}."),
    })?;
    let mut fills = Vec::new();
    for (key, value) in &table {
        let Some(ix) = KEYS.iter().position(|k| k == key) else {
            return Err(format!(
                "The document has a key this form does not know. Valid keys: {}.",
                KEYS.join(", ")
            ));
        };
        let key = KEYS[ix];
        if existing_list && ix < LIST_KEYS {
            return Err(format!(
                "\"{key}\" does not apply: this form adds to an existing list."
            ));
        }
        let kind = kind(ix);
        let filled = match (kind, value) {
            (Kind::Text, toml::Value::String(s)) => Some(one_line(s)),
            (Kind::Number, toml::Value::Integer(n)) if *n >= 0 => Some(n.to_string()),
            (Kind::Number, toml::Value::String(s))
                if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) =>
            {
                Some(s.clone())
            }
            (Kind::Switch, toml::Value::Boolean(on)) => {
                Some(if *on { "on" } else { "off" }.to_string())
            }
            _ => None,
        };
        let Some(filled) = filled else {
            return Err(format!("\"{key}\" must be {}.", expected(kind)));
        };
        fills.push((ix, filled));
    }
    if fills.is_empty() {
        return Err("There is nothing to fill: the document has no form keys.".to_string());
    }
    fills.sort_by_key(|(ix, _)| *ix);
    Ok(fills)
}

/// What cmd-V in a form field does with the clipboard.
#[derive(Debug, PartialEq)]
pub enum PasteDecision {
    /// Not form TOML: the input pastes the text as usual, silently.
    Normal,
    /// Form TOML: fill these fields; the text is not pasted.
    Fill(Vec<(usize, String)>),
    /// Form TOML with a problem: nothing is pasted, show this error.
    Reject(String),
}

/// The text is "ours" when it is TOML, a non-empty table, and every key is a
/// form key. Anything else (syntax error, an unknown key, no keys, plain
/// text) is a normal paste.
pub fn paste_decision(text: Option<&str>, existing_list: bool) -> PasteDecision {
    let Some(text) = text.map(str::trim) else {
        return PasteDecision::Normal;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return PasteDecision::Normal;
    };
    if table.is_empty() || !table.keys().all(|k| KEYS.contains(&k.as_str())) {
        return PasteDecision::Normal;
    }
    match parse(Some(text), existing_list) {
        Ok(fills) => PasteDecision::Fill(fills),
        Err(message) => PasteDecision::Reject(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"
item_singular = "US City"
item_plural = "US Cities"
description = "Cities of the USA"
criteria = "A real city"
amount = 100
cap = 400
auto_pay = true
min_rank = 2
"#;

    #[test]
    fn a_full_document_fills_every_field() {
        let fills = parse(Some(FULL), false).unwrap();
        let texts: Vec<&str> = fills.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            ["US City", "US Cities", "Cities of the USA", "A real city", "100", "400", "on", "2"]
        );
        let ixs: Vec<usize> = fills.iter().map(|(ix, _)| *ix).collect();
        assert_eq!(ixs, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn a_partial_document_returns_only_its_keys() {
        let fills = parse(Some("cap = 900\nauto_pay = false"), false).unwrap();
        assert_eq!(fills, [(5, "900".to_string()), (6, "off".to_string())]);
    }

    #[test]
    fn an_unknown_key_errors_and_lists_the_valid_keys() {
        let err = parse(Some("amout = 5\ncap = 6"), false).unwrap_err();
        assert!(err.contains("min_rank") && err.contains("does not know"), "{err}");
    }

    #[test]
    fn a_wrong_type_errors() {
        let err = parse(Some("amount = true"), false).unwrap_err();
        assert!(err.contains("\"amount\"") && err.contains("integer"), "{err}");
        let err = parse(Some("criteria = 3"), false).unwrap_err();
        assert!(err.contains("\"criteria\"") && err.contains("string"), "{err}");
        let err = parse(Some("auto_pay = \"on\""), false).unwrap_err();
        assert!(err.contains("\"auto_pay\"") && err.contains("boolean"), "{err}");
        assert!(parse(Some("cap = \"4x\""), false).is_err());
        assert!(parse(Some("cap = -4"), false).is_err());
    }

    #[test]
    fn a_list_key_in_existing_list_mode_errors() {
        let err = parse(Some("item_plural = \"x\""), true).unwrap_err();
        assert!(err.contains("existing list"), "{err}");
        assert_eq!(parse(Some("criteria = \"x\""), true).unwrap().len(), 1);
    }

    #[test]
    fn nothing_is_filled_when_any_key_is_bad() {
        assert!(parse(Some("cap = 5\nnope = 1"), false).is_err());
    }

    #[test]
    fn a_syntax_error_gives_fixed_wording_and_a_position() {
        let err = parse(Some("cap = 1\namount = \n"), false).unwrap_err();
        assert!(err.starts_with("That is not valid TOML (line 2, column"), "{err}");
    }

    #[test]
    fn no_error_echoes_the_clipboard() {
        let nsec = "nsec1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let docs = [
            nsec.to_string(),
            hex.to_string(),
            format!("cap = 5\n{nsec}"),
            format!("amount = {hex}"),
            format!("\"{nsec}\" = \"{hex}\""),
            format!("secret = \"{nsec}\""),
            format!("criteria = 3\nx = \"{hex}\""),
            format!("cap = \"{hex}\""),
        ];
        for doc in docs {
            for existing in [false, true] {
                if let Err(err) = parse(Some(&doc), existing) {
                    assert!(!err.contains("nsec1") && !err.contains(hex), "{err}");
                }
                if let PasteDecision::Reject(err) = paste_decision(Some(&doc), existing) {
                    assert!(!err.contains("nsec1") && !err.contains(hex), "{err}");
                }
            }
        }
    }

    #[test]
    fn line_breaks_in_text_become_one_space() {
        let doc = "description = \"\"\"\nCities of\n  the USA\n\n\"\"\"\ncriteria = \"a\\r\\nb\"";
        let fills = parse(Some(doc), false).unwrap();
        assert_eq!(fills, [(2, "Cities of the USA".to_string()), (3, "a b".to_string())]);
    }

    #[test]
    fn every_kind_of_line_break_becomes_a_space() {
        let doc = "criteria = \"a\\rb\\u2028c\\u2029d\\r\\ne\"";
        assert_eq!(parse(Some(doc), false).unwrap(), [(3, "a b c d e".to_string())]);
    }

    #[test]
    fn a_comment_only_document_has_nothing_to_fill() {
        let err = parse(Some("# nothing"), false).unwrap_err();
        assert!(err.contains("nothing to fill"), "{err}");
    }

    #[test]
    fn the_cmd_v_decision() {
        let full = "item_singular = \"a\"\nitem_plural = \"b\"\ncriteria = \"c\"";
        assert!(matches!(paste_decision(Some("cap = 5"), false), PasteDecision::Fill(f) if f == [(5, "5".to_string())]));
        for text in ["US City", "a sentence, with words.", "x = 1", "cap = 5\nx = 1", "", "# only a comment", "cap = ="] {
            assert_eq!(paste_decision(Some(text), false), PasteDecision::Normal, "{text}");
        }
        assert_eq!(paste_decision(None, false), PasteDecision::Normal);
        assert!(matches!(paste_decision(Some(full), true), PasteDecision::Reject(m) if m.contains("existing list")));
        assert!(matches!(paste_decision(Some("cap = true"), false), PasteDecision::Reject(m) if m.contains("\"cap\"")));
    }

    #[test]
    fn integers_and_digit_strings_both_work() {
        let fills = parse(Some("amount = 7\ncap = \"12\""), false).unwrap();
        assert_eq!(fills, [(4, "7".to_string()), (5, "12".to_string())]);
    }

    #[test]
    fn an_empty_clipboard_says_so() {
        assert!(parse(None, false).unwrap_err().contains("no text"));
        assert!(parse(Some("  \n"), false).unwrap_err().contains("no text"));
    }
}
