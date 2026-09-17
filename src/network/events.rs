use serde::{Deserialize, Serialize};

/// A captured HTTP network event from the SDK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkEvent {
    pub url: String,
    /// The HTTP method, when the capture actually learned it. `None` means the
    /// request line was never matched to this response, which is not the same
    /// as a request that had no method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The response status, when the capture actually observed one. `None` means
    /// no status was ever seen. It is not a 0, and it is not a success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Round trip time, when the source reported one. `None` is "not timed",
    /// which is not the same as a request that took no time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Body sizes, when the source reported them. `None` is "not measured".
    /// A logcat line carries no sizes at all, and saying 0 there claims an empty
    /// body that nobody looked at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_size: Option<u64>,
    pub timestamp_ms: u64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_headers: Option<Vec<(String, String)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Vec<(String, String)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
}

impl NetworkEvent {
    /// Whether this is an error response (4xx or 5xx). `None` when no status was
    /// captured: a response whose status we never saw is not a success, and
    /// callers must decide what to do about not knowing.
    pub fn is_error(&self) -> Option<bool> {
        self.status.map(|s| s >= 400)
    }

    /// Whether this is a client error (4xx). `None` when no status was captured.
    pub fn is_client_error(&self) -> Option<bool> {
        self.status.map(|s| (400..500).contains(&s))
    }

    /// Whether this is a server error (5xx). `None` when no status was captured.
    pub fn is_server_error(&self) -> Option<bool> {
        self.status.map(|s| s >= 500)
    }
}

/// The capture-honesty guard.
///
/// A capture layer that could not read a field used to write a value that looks
/// like an answer: `status: 0` where no status was parsed, `method: "?"` where a
/// response was never correlated to its request. Downstream cannot tell those
/// from real readings, so `expect_network` had to carry sentinel constants to
/// undo the lie, and `is_error()` answered a confident "no" about a response
/// whose status nobody ever saw.
///
/// The property: no `NetworkEvent` is constructed with a stand-in for a field
/// the capture did not read. `Option` is the only way to say "not captured".
#[cfg(test)]
mod capture_honesty_guard {
    use crate::source_guard::{line_of, rust_files_under, src_root, without_comments};

    /// Literals that have historically stood in for "I could not tell", and the
    /// field each one poisoned. A construction site may not use them.
    const SENTINELS: &[(&str, &str)] = &[
        (
            "\"?\"",
            "method: a response never correlated to its request is not a method named \"?\"",
        ),
        (
            "\"unknown\"",
            "url: a line that carried no URL did not carry a URL called \"unknown\"",
        ),
        (
            "unwrap_or(0)",
            "status: a status that failed to parse is not a 0, and 0 is not an error",
        ),
    ];

    /// Construction sites that still carry a sentinel, with why it is not yet
    /// fixed. An entry here is a deliberate, reviewable admission of a remaining
    /// lie, not a place to park new ones.
    const ALLOWED: &[(&str, &str)] = &[(
        "logcat.rs:parse_cfnetwork_response:url",
        "the CFNetwork status line genuinely carries no URL; making `url` an Option ripples \
             through every glob match and is its own change. Tracked as FIXME(url-sentinel).",
    )];

    /// The fields a capture can fail to read. A sentinel only matters when the
    /// statement minting it is about one of these.
    const POISONED_FIELDS: &[&str] = &[
        "method",
        "status",
        "url",
        "duration_ms",
        "request_size",
        "response_size",
    ];

    /// Which field a sentinel poisons, so an allowlist entry excuses one field
    /// at one site rather than every sentinel in the file.
    fn field_of(sentinel: &str) -> &'static str {
        match sentinel {
            "\"?\"" => "method",
            "\"unknown\"" => "url",
            _ => "status",
        }
    }

    /// Every `NetworkEvent { .. }` literal body in a file, brace-matched.
    ///
    /// Used only for the self-check. Policing the literal ALONE is not enough and
    /// that is not a theoretical worry: the first version of this guard did
    /// exactly that, and it passed with the defect reintroduced, because
    /// `let method = ...; NetworkEvent { method, .. }` launders the sentinel
    /// through a local. The assertion below scans whole files for that reason.
    fn event_literals(code: &str) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for (at, _) in code.match_indices("NetworkEvent {") {
            let body_at = at + "NetworkEvent {".len();
            let mut depth = 1usize;
            let mut end = body_at;
            for (k, c) in code[body_at..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = body_at + k;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.push((line_of(code, at), code[body_at..end].to_string()));
        }
        out
    }

    #[test]
    fn no_network_event_is_built_from_a_stand_in_for_unknown() {
        let sources = rust_files_under(&src_root());

        // Self-check: this guard is worthless if it stopped finding the literals
        // it polices, so prove it still sees the real construction sites.
        let found: usize = sources
            .iter()
            .map(|(_, s)| event_literals(&without_comments(s)).len())
            .sum();
        assert!(
            found >= 4,
            "guard found only {found} NetworkEvent literals; it has stopped reading what it polices"
        );

        let mut offenders = Vec::new();
        for (name, src) in &sources {
            let code = without_comments(src);
            // Whole file, not just the literal: a capture file that builds a
            // NetworkEvent may not mint these values ANYWHERE, because the
            // laundering path is a local binding the literal then names.
            if !code.contains("NetworkEvent {") {
                continue;
            }
            let base = name.rsplit('/').next().unwrap_or(name);
            // This guard's own tables name every sentinel, so skip its source.
            let policed = match code.find("mod capture_honesty_guard {") {
                Some(at) => &code[..at],
                None => &code[..],
            };
            for (sentinel, why) in SENTINELS {
                for (at, _) in policed.match_indices(sentinel) {
                    // The minting must actually be about a NetworkEvent field,
                    // or a lock fallback like `q.len().unwrap_or(0)` reads as a
                    // fabricated status. The window is the sentinel's line plus
                    // the two above it, NOT the enclosing statement: an `else {`
                    // brace ends a statement scan right before the sentinel, which
                    // is precisely how the laundering case escaped the last
                    // version of this check.
                    let line = line_of(policed, at);
                    let lines: Vec<&str> = policed.lines().collect();
                    let window = lines[line.saturating_sub(3)..line.min(lines.len())].join("\n");
                    if !POISONED_FIELDS.iter().any(|f| window.contains(f)) {
                        continue;
                    }
                    let allowed = ALLOWED.iter().any(|(site, _)| {
                        site.starts_with(base) && site.ends_with(field_of(sentinel))
                    });
                    if !allowed {
                        offenders.push(format!(
                            "{name}:{line} mints {sentinel} for a NetworkEvent field — {why}"
                        ));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "a NetworkEvent is being built with a value that stands in for \"not captured\".\n{}\n\
             Use None. A field the capture did not read must be indistinguishable from absent, \
             never from a reading.",
            offenders.join("\n")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> NetworkEvent {
        NetworkEvent {
            url: "/api/login".to_string(),
            method: Some("POST".to_string()),
            status: Some(200),
            duration_ms: Some(150),
            request_size: Some(100),
            response_size: Some(500),
            timestamp_ms: 1709500000000,
            request_headers: None,
            response_headers: None,
            request_body: None,
            response_body: None,
        }
    }

    #[test]
    fn test_is_error() {
        let mut e = sample_event();
        assert_eq!(e.is_error(), Some(false));

        e.status = Some(404);
        assert_eq!(e.is_error(), Some(true));
        assert_eq!(e.is_client_error(), Some(true));
        assert_eq!(e.is_server_error(), Some(false));

        e.status = Some(500);
        assert_eq!(e.is_error(), Some(true));
        assert_eq!(e.is_client_error(), Some(false));
        assert_eq!(e.is_server_error(), Some(true));
    }

    /// The point of the whole change: a status nobody captured answers "I do not
    /// know" to every one of these, and never the reassuring "not an error" that
    /// a fabricated 0 used to give.
    #[test]
    fn an_uncaptured_status_answers_none_not_false() {
        let mut e = sample_event();
        e.status = None;
        assert_eq!(e.is_error(), None);
        assert_eq!(e.is_client_error(), None);
        assert_eq!(e.is_server_error(), None);

        // And it must not be mistaken for a real status on the wire either.
        let json = serde_json::to_string(&e).unwrap();
        assert!(
            !json.contains("\"status\""),
            "absent status must be omitted, got {json}"
        );
    }

    #[test]
    fn test_serde_roundtrip() {
        let event = sample_event();
        let json = serde_json::to_string(&event).unwrap();
        let back: NetworkEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.url, "/api/login");
        assert_eq!(back.status, Some(200));
        assert_eq!(back.duration_ms, Some(150));
    }

    #[test]
    fn test_optional_fields_omitted() {
        let event = sample_event();
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("request_headers"));
        assert!(!json.contains("request_body"));
    }

    #[test]
    fn test_with_headers() {
        let mut event = sample_event();
        event.request_headers = Some(vec![(
            "Content-Type".to_string(),
            "application/json".to_string(),
        )]);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Content-Type"));
    }
}
