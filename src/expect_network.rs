//! Assert on the traffic a task produced, not just on what the screen said.
//!
//! The load-bearing rule here is that "did not match" and "could not observe"
//! are different answers. `NetworkSource` fidelity differs per source: the SDK
//! wire format carries no bodies at all, logcat carries no request body, and
//! only the in-app SDK carries both. A `body_includes` expectation checked against a
//! source that cannot see bodies must say so, because reporting it as a plain
//! failure blames the app for the capture layer's blindness.

use serde::{Deserialize, Serialize};

use crate::network::sink::{BodyTruncation, NetworkSource, SinkEntry};

#[derive(PartialEq)]
enum Field {
    Match,
    Differs,
    Unknown,
}

fn check_method(want: Option<&String>, got: Option<&String>) -> Field {
    match (want, got) {
        (None, _) => Field::Match,
        (Some(_), None) => Field::Unknown,
        (Some(w), Some(g)) if w.eq_ignore_ascii_case(g) => Field::Match,
        (Some(_), Some(_)) => Field::Differs,
    }
}

fn check_status(want: Option<u16>, got: Option<u16>) -> Field {
    match (want, got) {
        (None, _) => Field::Match,
        (Some(_), None) => Field::Unknown,
        (Some(w), Some(g)) if w == g => Field::Match,
        (Some(_), Some(_)) => Field::Differs,
    }
}

/// One expectation about the traffic a task should have produced.
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectNetwork {
    /// Glob against the full URL. `*` matches any run of characters.
    pub url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub status: Option<u16>,
    /// Substring that must appear in the request or response body.
    #[serde(default)]
    pub body_includes: Option<String>,
    /// Exact number of matches required. Omitted means at least one.
    #[serde(default)]
    pub times: Option<usize>,
}

/// The four honest answers. `Unobservable` and `Inconclusive` exist so a blind
/// capture layer never reports itself as a failing app.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ExpectOutcome {
    Met,
    NotMet { detail: String },
    Unobservable { detail: String },
    Inconclusive { detail: String },
}

impl ExpectOutcome {
    /// Only a definite miss fails a task. Blindness is reported, never counted
    /// as evidence the app misbehaved.
    pub fn fails_task(&self) -> bool {
        matches!(self, Self::NotMet { .. })
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Met => "",
            Self::NotMet { detail }
            | Self::Unobservable { detail }
            | Self::Inconclusive { detail } => detail,
        }
    }
}

/// `*` is the only wildcard. Anchored at both ends.
fn glob_match(pattern: &str, text: &str) -> bool {
    let mut segments = pattern.split('*');
    let Some(first) = segments.next() else {
        return true;
    };
    if !text.starts_with(first) {
        return false;
    }
    let mut cursor = first.len();
    let mut trailing_wildcard = pattern.starts_with('*') && pattern.len() == 1;
    for segment in segments {
        trailing_wildcard = segment.is_empty();
        if segment.is_empty() {
            continue;
        }
        match text[cursor..].find(segment) {
            Some(at) => cursor += at + segment.len(),
            None => return false,
        }
    }
    trailing_wildcard || cursor == text.len()
}

fn bodies_of(entry: &SinkEntry) -> [Option<&String>; 2] {
    [
        entry.event.request_body.as_ref(),
        entry.event.response_body.as_ref(),
    ]
}

fn truncated_anywhere(entry: &SinkEntry) -> bool {
    !matches!(entry.truncated, BodyTruncation::None)
}

fn sources_present(entries: &[&SinkEntry]) -> Vec<NetworkSource> {
    let mut seen: Vec<NetworkSource> = Vec::new();
    for e in entries {
        if !seen.contains(&e.source) {
            seen.push(e.source);
        }
    }
    seen
}

fn fidelity_note(sources: &[NetworkSource]) -> String {
    sources
        .iter()
        .map(|s| format!("{} ({})", s.as_str(), s.fidelity()))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Evaluate one expectation against the entries captured during a task.
pub fn evaluate(expect: &ExpectNetwork, entries: &[SinkEntry]) -> ExpectOutcome {
    // The URL is the one field every source records, so it always narrows.
    let by_url: Vec<&SinkEntry> = entries
        .iter()
        .filter(|e| glob_match(&expect.url, &e.event.url))
        .collect();

    let mut candidates: Vec<&SinkEntry> = Vec::new();
    let mut blind: Vec<&SinkEntry> = Vec::new();
    for e in &by_url {
        let m = check_method(expect.method.as_ref(), e.event.method.as_ref());
        let st = check_status(expect.status, e.event.status);
        if m == Field::Differs || st == Field::Differs {
            continue;
        }
        if m == Field::Unknown || st == Field::Unknown {
            blind.push(e);
        } else {
            candidates.push(e);
        }
    }

    // Nothing matched, but something was excluded only because the capture
    // could not record the field. That is blindness, not a failing app.
    if candidates.is_empty() && !blind.is_empty() {
        let sources = sources_present(&blind);
        return ExpectOutcome::Unobservable {
            detail: format!(
                "{} request{} matched `{}` but could not be checked: the capture recorded no {} for {}. Source fidelity: {}",
                blind.len(),
                if blind.len() == 1 { "" } else { "s" },
                expect.url,
                if expect.method.is_some() && blind.iter().any(|e| e.event.method.is_none()) {
                    "method"
                } else {
                    "status"
                },
                if blind.len() == 1 { "it" } else { "them" },
                fidelity_note(&sources),
            ),
        };
    }

    if candidates.is_empty() {
        let want = expect.times.unwrap_or(1);
        if want == 0 {
            return ExpectOutcome::Met;
        }
        // Nothing was captured at all. That is a dead capture path, not a
        // misbehaving app: an absent interceptor, an app that does not use
        // OkHttp, an unreadable logcat, a sink nobody fed. Failing the build
        // here would make a blind instrument look like a caught defect, which
        // is the exact confusion this enum exists to prevent.
        if entries.is_empty() {
            return ExpectOutcome::Unobservable {
                detail: format!(
                    "capture produced no requests at all for this task, so `{}` could not be checked. Nothing is proven either way",
                    expect.url,
                ),
            };
        }
        return ExpectOutcome::NotMet {
            detail: format!(
                "no captured request matched url `{}`{}{} (saw {} request{} in this task)",
                expect.url,
                expect
                    .method
                    .as_ref()
                    .map(|m| format!(" method {m}"))
                    .unwrap_or_default(),
                expect
                    .status
                    .map(|s| format!(" status {s}"))
                    .unwrap_or_default(),
                entries.len(),
                if entries.len() == 1 { "" } else { "s" },
            ),
        };
    }

    let matched: Vec<&SinkEntry> = match &expect.body_includes {
        None => candidates,
        Some(needle) => {
            let observable: Vec<&&SinkEntry> = candidates
                .iter()
                .filter(|e| bodies_of(e).iter().any(Option::is_some))
                .collect();

            if observable.is_empty() {
                let sources = sources_present(&candidates);
                return ExpectOutcome::Unobservable {
                    detail: format!(
                        "cannot observe a body for `{}`: {} matching request{} captured, none carrying a body. Source fidelity: {}",
                        expect.url,
                        candidates.len(),
                        if candidates.len() == 1 { "" } else { "s" },
                        fidelity_note(&sources),
                    ),
                };
            }

            let hits: Vec<&SinkEntry> = observable
                .iter()
                .filter(|e| {
                    bodies_of(e)
                        .iter()
                        .flatten()
                        .any(|body| body.contains(needle.as_str()))
                })
                .copied()
                .copied()
                .collect();

            if hits.is_empty() && observable.iter().any(|e| truncated_anywhere(e)) {
                return ExpectOutcome::Inconclusive {
                    detail: format!(
                        "`{needle}` not found, but a matching body was truncated at the capture cap, so absence is not established"
                    ),
                };
            }
            hits
        }
    };

    let want = expect.times.unwrap_or(1);
    let got = matched.len();
    let satisfied = match expect.times {
        Some(n) => got == n,
        None => got >= 1,
    };

    if satisfied {
        ExpectOutcome::Met
    } else {
        ExpectOutcome::NotMet {
            detail: format!(
                "expected {}{} match{}, got {}",
                if expect.times.is_some() {
                    "exactly "
                } else {
                    "at least "
                },
                want,
                if want == 1 { "" } else { "es" },
                got,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::events::NetworkEvent;

    fn event(url: &str, method: &str, status: u16) -> NetworkEvent {
        NetworkEvent {
            url: url.to_string(),
            method: Some(method.to_string()),
            status: Some(status),
            duration_ms: Some(5),
            request_size: Some(0),
            response_size: Some(0),
            timestamp_ms: 1,
            request_headers: None,
            response_headers: None,
            request_body: None,
            response_body: None,
        }
    }

    fn entry(source: NetworkSource, url: &str, method: &str, status: u16) -> SinkEntry {
        SinkEntry {
            source,
            truncated: BodyTruncation::None,
            event: event(url, method, status),
        }
    }

    fn with_response_body(mut e: SinkEntry, body: &str) -> SinkEntry {
        e.event.response_body = Some(body.to_string());
        e
    }

    fn expect(url: &str) -> ExpectNetwork {
        ExpectNetwork {
            url: url.to_string(),
            method: None,
            status: None,
            body_includes: None,
            times: None,
        }
    }

    #[test]
    fn glob_anchors_both_ends() {
        assert!(glob_match(
            "https://x.com/v1/checkout",
            "https://x.com/v1/checkout"
        ));
        assert!(!glob_match("https://x.com/v1", "https://x.com/v1/checkout"));
        assert!(glob_match("*/v1/checkout", "https://x.com/v1/checkout"));
        assert!(glob_match("https://x.com/*", "https://x.com/v1/checkout"));
        assert!(glob_match("*checkout*", "https://x.com/v1/checkout?a=1"));
        assert!(!glob_match("*/orders", "https://x.com/v1/checkout"));
    }

    #[test]
    fn a_plain_url_match_is_met() {
        let entries = vec![entry(
            NetworkSource::Sdk,
            "https://x.com/v1/checkout",
            "POST",
            200,
        )];
        assert_eq!(
            evaluate(&expect("*/v1/checkout"), &entries),
            ExpectOutcome::Met
        );
    }

    #[test]
    fn an_empty_sink_is_unobservable_not_a_failure() {
        let out = evaluate(&expect("*/v1/checkout"), &[]);
        assert!(
            matches!(out, ExpectOutcome::Unobservable { .. }),
            "a dead capture path must not read as a caught defect: {out:?}"
        );
        assert!(!out.fails_task());
        assert!(out.detail().contains("no requests at all"));
    }

    #[test]
    fn an_empty_sink_still_satisfies_times_zero() {
        let mut e = expect("*/v1/checkout");
        e.times = Some(0);
        assert_eq!(evaluate(&e, &[]), ExpectOutcome::Met);
    }

    #[test]
    fn no_matching_request_is_a_real_failure() {
        let entries = vec![entry(
            NetworkSource::Sdk,
            "https://x.com/v1/cart",
            "POST",
            200,
        )];
        let out = evaluate(&expect("*/v1/checkout"), &entries);
        assert!(out.fails_task());
        assert!(matches!(out, ExpectOutcome::NotMet { .. }));
    }

    #[test]
    fn body_check_against_the_sdk_source_is_unobservable_not_failed() {
        let entries = vec![entry(
            NetworkSource::Sdk,
            "https://x.com/v1/checkout",
            "POST",
            200,
        )];
        let mut e = expect("*/v1/checkout");
        e.body_includes = Some("checkout_completed".into());
        let out = evaluate(&e, &entries);
        assert!(
            matches!(out, ExpectOutcome::Unobservable { .. }),
            "the SDK wire format carries no bodies; this must not read as an app failure: {out:?}"
        );
        assert!(!out.fails_task());
        assert!(out.detail().contains("no headers and no bodies"));
    }

    #[test]
    fn body_check_against_logcat_without_a_body_is_unobservable() {
        let entries = vec![entry(
            NetworkSource::Logcat,
            "https://x.com/v1/checkout",
            "POST",
            200,
        )];
        let mut e = expect("*/v1/checkout");
        e.body_includes = Some("checkout_completed".into());
        assert!(!evaluate(&e, &entries).fails_task());
    }

    #[test]
    fn body_present_and_matching_is_met() {
        let entries = vec![with_response_body(
            entry(NetworkSource::Sdk, "https://x.com/v1/checkout", "POST", 200),
            r#"{"event":"checkout_completed","revenue":42.5}"#,
        )];
        let mut e = expect("*/v1/checkout");
        e.body_includes = Some("checkout_completed".into());
        assert_eq!(evaluate(&e, &entries), ExpectOutcome::Met);
    }

    #[test]
    fn body_present_and_absent_needle_is_a_real_failure() {
        let entries = vec![with_response_body(
            entry(NetworkSource::Sdk, "https://x.com/v1/checkout", "POST", 200),
            r#"{"event":"cart_viewed"}"#,
        )];
        let mut e = expect("*/v1/checkout");
        e.body_includes = Some("checkout_completed".into());
        assert!(evaluate(&e, &entries).fails_task());
    }

    #[test]
    fn a_truncated_body_that_misses_is_inconclusive_not_failed() {
        let mut cut = with_response_body(
            entry(NetworkSource::Sdk, "https://x.com/v1/checkout", "POST", 200),
            r#"{"event":"cart_"#,
        );
        cut.truncated = BodyTruncation::Response;
        let mut e = expect("*/v1/checkout");
        e.body_includes = Some("checkout_completed".into());
        let out = evaluate(&e, &[cut]);
        assert!(matches!(out, ExpectOutcome::Inconclusive { .. }), "{out:?}");
        assert!(!out.fails_task());
    }

    #[test]
    fn times_counts_exactly() {
        let entries = vec![
            entry(NetworkSource::Sdk, "https://x.com/v1/checkout", "POST", 200),
            entry(NetworkSource::Sdk, "https://x.com/v1/checkout", "POST", 200),
        ];
        let mut e = expect("*/v1/checkout");
        e.times = Some(1);
        assert!(evaluate(&e, &entries).fails_task());
        e.times = Some(2);
        assert_eq!(evaluate(&e, &entries), ExpectOutcome::Met);
    }

    #[test]
    fn times_zero_asserts_absence() {
        let entries = vec![entry(
            NetworkSource::Sdk,
            "https://x.com/v1/cart",
            "POST",
            200,
        )];
        let mut e = expect("*/v1/checkout");
        e.times = Some(0);
        assert_eq!(evaluate(&e, &entries), ExpectOutcome::Met);
    }

    #[test]
    fn an_unparsed_status_is_unobservable_not_a_failure() {
        // logcat.rs:35 and sdk/mod.rs:368 write status 0 when there was nothing
        // to parse. Reading that as "not 200" blames the app for the capture.
        let mut e0 = entry(
            NetworkSource::Logcat,
            "https://x.com/v1/checkout",
            "POST",
            0,
        );
        e0.event.status = None;
        let mut e = expect("*/v1/checkout");
        e.status = Some(200);
        let out = evaluate(&e, &[e0]);
        assert!(
            matches!(out, ExpectOutcome::Unobservable { .. }),
            "status 0 means unparsed, not 'a status that is not 200': {out:?}"
        );
        assert!(!out.fails_task());
        assert!(out.detail().contains("status"));
    }

    #[test]
    fn an_uncorrelated_method_is_unobservable_not_a_failure() {
        // logcat leaves `method` absent when a response cannot be tied to its
        // request. Absent is not the method "?", which would merely differ.
        let mut e0 = entry(
            NetworkSource::Logcat,
            "https://x.com/v1/checkout",
            "GET",
            200,
        );
        e0.event.method = None;
        let mut e = expect("*/v1/checkout");
        e.method = Some("POST".into());
        let out = evaluate(&e, &[e0]);
        assert!(matches!(out, ExpectOutcome::Unobservable { .. }), "{out:?}");
        assert!(!out.fails_task());
    }

    #[test]
    fn a_known_status_that_differs_is_still_a_real_failure() {
        // The fix must not swallow genuine mismatches.
        let e0 = entry(NetworkSource::Sdk, "https://x.com/v1/checkout", "POST", 500);
        let mut e = expect("*/v1/checkout");
        e.status = Some(200);
        assert!(evaluate(&e, &[e0]).fails_task());
    }

    #[test]
    fn a_real_match_alongside_a_blind_one_still_passes() {
        let blind = entry(NetworkSource::Logcat, "https://x.com/v1/checkout", "?", 200);
        let good = entry(NetworkSource::Sdk, "https://x.com/v1/checkout", "POST", 200);
        let mut e = expect("*/v1/checkout");
        e.method = Some("POST".into());
        assert_eq!(evaluate(&e, &[blind, good]), ExpectOutcome::Met);
    }

    #[test]
    fn method_and_status_narrow_the_candidates() {
        let entries = vec![
            entry(NetworkSource::Sdk, "https://x.com/v1/checkout", "GET", 200),
            entry(NetworkSource::Sdk, "https://x.com/v1/checkout", "POST", 500),
        ];
        let mut e = expect("*/v1/checkout");
        e.method = Some("post".into());
        e.status = Some(200);
        assert!(evaluate(&e, &entries).fails_task());
        e.status = Some(500);
        assert_eq!(evaluate(&e, &entries), ExpectOutcome::Met);
    }
}
