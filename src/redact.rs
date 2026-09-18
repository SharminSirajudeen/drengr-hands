//! PII redaction for diagnostic bundles. Best-effort, regex-based.

use std::sync::OnceLock;

use regex::Regex;

/// Redact common PII patterns from a string. Idempotent.
pub fn redact(input: &str) -> String {
    let mut out = input.to_string();
    for (re, replacement) in patterns() {
        out = re.replace_all(&out, *replacement).to_string();
    }
    out
}

/// Replace any `type(text)`-style action with a length+kind summary so we
/// keep the action shape for debugging without leaking what was typed.
pub fn summarize_typed(text: &str) -> String {
    let len = text.chars().count();
    let kind = if text.chars().all(|c| c.is_ascii_digit()) {
        "digits"
    } else if text.chars().all(|c| c.is_ascii_alphabetic()) {
        "alpha"
    } else if text.contains('@') {
        "emaillike"
    } else {
        "mixed"
    };
    format!("<typed {}-char {}>", len, kind)
}

fn patterns() -> &'static [(Regex, &'static str)] {
    static CELL: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    CELL.get_or_init(|| {
        vec![
            (
                Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b").unwrap(),
                "<email>",
            ),
            (
                Regex::new(r"(?:\+\d{1,3}[\s-]?)?\(?\d{3}\)?[\s-]?\d{3}[\s-]?\d{4}\b").unwrap(),
                "<phone>",
            ),
            (
                Regex::new(r"\b\d{4}[\s-]?\d{4}[\s-]?\d{4}[\s-]?\d{4}\b").unwrap(),
                "<card>",
            ),
            (Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(), "<ssn>"),
            (Regex::new(r#"https?://[^\s<>'"]+"#).unwrap(), "<url>"),
            (Regex::new(r"[A-Za-z0-9_-]{32,}").unwrap(), "<token>"),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_email() {
        assert_eq!(
            redact("contact me at sharmin@example.com please"),
            "contact me at <email> please"
        );
    }

    #[test]
    fn redacts_phone() {
        assert_eq!(redact("call +1 415-555-0100 now"), "call <phone> now");
    }

    #[test]
    fn redacts_card_and_url() {
        let out = redact("4111 1111 1111 1111 https://api.foo/secret");
        assert!(out.contains("<card>"));
        assert!(out.contains("<url>"));
    }

    #[test]
    fn idempotent() {
        let once = redact("send to a@b.co");
        assert_eq!(once, redact(&once));
    }

    #[test]
    fn summarize_typed_classifies_kind() {
        assert!(summarize_typed("abc").contains("alpha"));
        assert!(summarize_typed("12345").contains("digits"));
        assert!(summarize_typed("a@b.co").contains("emaillike"));
        assert!(summarize_typed("a1!").contains("mixed"));
    }
}
