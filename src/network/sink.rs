//! The one sink every network source pushes into.
//!
//! Three sources observe an app's traffic and they do NOT see the same thing.
//! Merging them into one undifferentiated list would make the output claim a
//! fidelity it does not have, so every entry carries the source that produced
//! it and the fidelity legend that source implies.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::events::NetworkEvent;

/// Fixed capacity of the shared ring — bounds memory over a long MCP session.
const MAX_EVENTS: usize = 2000;

/// Which capture path produced an event. Determines what the event can and
/// cannot contain, so a caller never mistakes a subset for the whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkSource {
    Logcat,
    /// An in-app reporter speaking the protocol in `crate::sdk::messages`.
    /// Drengr's own analytics SDK is one such sender; anything that speaks the
    /// wire format is another.
    InApp,
}

impl NetworkSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Logcat => "logcat",
            Self::InApp => "in_app",
        }
    }

    /// What this source can observe at all: its ceiling, not a promise about
    /// any one entry. What a given entry actually holds is `SinkEntry::fidelity`.
    pub fn fidelity(&self) -> &'static str {
        match self {
            Self::Logcat => "URL, method, status and duration from the app's own OkHttp log lines, plus the response body only when the app logs at BODY level. Never request headers, never the request body.",
            Self::InApp => "URL, method, status, duration and byte sizes reported by a reporter running inside the app, captured above TLS — so it survives certificate pinning and needs no proxy and no CA. Headers and bodies are optional on this wire, so an entry carrying no headers and no bodies came from a sender that does not report them, not from a format that cannot.",
        }
    }
}

/// Which bodies were cut short by a capture cap. Recorded per entry so a
/// truncated body is never read as a complete one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyTruncation {
    #[default]
    None,
    Request,
    Response,
    Both,
}

impl BodyTruncation {
    pub fn of(request: bool, response: bool) -> Self {
        match (request, response) {
            (false, false) => Self::None,
            (true, false) => Self::Request,
            (false, true) => Self::Response,
            (true, true) => Self::Both,
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Request => "request",
            Self::Response => "response",
            Self::Both => "both",
        }
    }
}

/// One captured exchange plus the provenance a caller needs to trust it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkEntry {
    pub source: NetworkSource,
    #[serde(default, skip_serializing_if = "BodyTruncation::is_none")]
    pub truncated: BodyTruncation,
    #[serde(flatten)]
    pub event: NetworkEvent,
}

impl SinkEntry {
    pub fn has_body(&self) -> bool {
        self.event.request_body.is_some() || self.event.response_body.is_some()
    }

    /// What THIS entry holds. A fixed sentence per source starts lying the
    /// moment one SDK version sends bodies and another does not, and a caller
    /// told "this source has no bodies" about an entry that has one will read
    /// a present body as an absent one.
    pub fn fidelity(&self) -> String {
        let e = &self.event;
        let mut held: Vec<&str> = Vec::new();
        if e.request_headers.is_some() {
            held.push("request headers");
        }
        if e.response_headers.is_some() {
            held.push("response headers");
        }
        if e.request_body.is_some() {
            held.push("the request body");
        }
        if e.response_body.is_some() {
            held.push("the response body");
        }
        let holds = if held.is_empty() {
            "This entry carries no headers and no bodies.".to_string()
        } else {
            format!("This entry carries {}.", held.join(", "))
        };
        match self.truncated {
            BodyTruncation::None => format!("{} {}", self.source.fidelity(), holds),
            t => format!(
                "{} {} Cut short at the capture cap: {}.",
                self.source.fidelity(),
                holds,
                t.as_str()
            ),
        }
    }
}

/// Shared bounded ring holding every source's events. Cloning shares the ring.
#[derive(Clone, Default)]
pub struct NetworkSink(Arc<Mutex<VecDeque<SinkEntry>>>);

impl NetworkSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, source: NetworkSource, truncated: BodyTruncation, event: NetworkEvent) {
        if let Ok(mut q) = self.0.lock() {
            while q.len() >= MAX_EVENTS {
                q.pop_front();
            }
            q.push_back(SinkEntry {
                source,
                truncated,
                event,
            });
        }
    }

    pub fn extend(&self, source: NetworkSource, events: impl IntoIterator<Item = NetworkEvent>) {
        for e in events {
            self.push(source, BodyTruncation::None, e);
        }
    }

    pub fn snapshot(&self) -> Vec<SinkEntry> {
        self.0
            .lock()
            .map(|q| q.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Everything captured at or after `timestamp_ms` — how a caller reads back
    /// one capture window without disturbing the rest of the history.
    pub fn since(&self, timestamp_ms: u64) -> Vec<SinkEntry> {
        self.0
            .lock()
            .map(|q| {
                q.iter()
                    .filter(|e| e.event.timestamp_ms >= timestamp_ms)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Remove and return every entry from `source`. Used when a capture session
    /// ends: decrypted bodies carry credentials, so they leave memory once the
    /// session that authorized them is over.
    pub fn drain(&self, source: NetworkSource) -> Vec<SinkEntry> {
        let mut taken = Vec::new();
        if let Ok(mut q) = self.0.lock() {
            let mut kept = VecDeque::with_capacity(q.len());
            for entry in q.drain(..) {
                if entry.source == source {
                    taken.push(entry);
                } else {
                    kept.push_back(entry);
                }
            }
            *q = kept;
        }
        taken
    }

    pub fn len(&self) -> usize {
        self.0.lock().map(|q| q.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Compact per-call summary for LLM consumption, carrying the source so the
/// reader knows which fidelity each line has.
pub fn summarize(entries: &[SinkEntry]) -> Value {
    entries
        .iter()
        .map(|entry| {
            let e = &entry.event;
            let mut obj = json!({
                "method": e.method,
                "url": shorten_url(&e.url),
                "status": e.status,
                "source": entry.source.as_str(),
            });
            if let Some(ms) = e.duration_ms {
                obj["duration_ms"] = json!(ms);
            }
            if !entry.truncated.is_none() {
                obj["truncated"] = json!(entry.truncated.as_str());
            }
            // Only a status we actually saw can say error or not. With none, the
            // reader is told the status is unknown rather than shown a quiet
            // absence of "error" that reads as success.
            match e.is_error() {
                Some(true) => {
                    obj["error"] = json!(true);
                    if let Some(body) = &e.response_body {
                        let preview = super::truncate_on_char_boundary(body, 500);
                        obj["response_preview"] = json!(preview);
                    }
                }
                Some(false) => {}
                None => obj["status_unknown"] = json!(true),
            }
            obj
        })
        .collect::<Vec<_>>()
        .into()
}

/// Per-source counts plus a legend measured from the entries actually present,
/// so the batch's own body coverage is stated rather than assumed from the source.
pub fn provenance(entries: &[SinkEntry]) -> (Value, Value) {
    let mut counts = serde_json::Map::new();
    let mut legend = serde_json::Map::new();
    for source in [NetworkSource::Logcat, NetworkSource::InApp] {
        let of_source: Vec<&SinkEntry> = entries.iter().filter(|e| e.source == source).collect();
        if of_source.is_empty() {
            continue;
        }
        let with_bodies = of_source.iter().filter(|e| e.has_body()).count();
        counts.insert(source.as_str().to_string(), json!(of_source.len()));
        legend.insert(
            source.as_str().to_string(),
            json!(format!(
                "{} {} of {} entries here carry a body.",
                source.fidelity(),
                with_bodies,
                of_source.len()
            )),
        );
    }
    (Value::Object(counts), Value::Object(legend))
}

/// Strip the scheme for LLM readability; the host and path are what identify a call.
fn shorten_url(url: &str) -> &str {
    url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(url: &str, ts: u64) -> NetworkEvent {
        NetworkEvent {
            url: url.to_string(),
            method: Some("GET".to_string()),
            status: Some(200),
            duration_ms: Some(10),
            request_size: Some(0),
            response_size: Some(0),
            timestamp_ms: ts,
            request_headers: None,
            response_headers: None,
            request_body: None,
            response_body: None,
        }
    }

    fn with_bodies(url: &str, ts: u64) -> NetworkEvent {
        NetworkEvent {
            request_headers: Some(vec![("content-type".into(), "application/json".into())]),
            request_body: Some(r#"{"email":"[REDACTED]"}"#.to_string()),
            response_body: Some(r#"{"ok":true}"#.to_string()),
            ..event(url, ts)
        }
    }

    #[test]
    fn two_entries_from_one_source_state_their_own_fidelity() {
        let sink = NetworkSink::new();
        sink.push(
            NetworkSource::InApp,
            BodyTruncation::None,
            with_bodies("/new", 1),
        );
        sink.push(NetworkSource::InApp, BodyTruncation::None, event("/old", 2));

        let snap = sink.snapshot();
        let bearing = snap[0].fidelity();
        let bare = snap[1].fidelity();

        assert!(bearing.contains("the request body"), "got: {bearing}");
        assert!(bearing.contains("the response body"));
        assert!(bearing.contains("request headers"));
        assert!(
            !bearing.contains("carries no headers and no bodies"),
            "an entry that has a body must never be described as having none: {bearing}"
        );
        assert!(
            bare.contains("carries no headers and no bodies"),
            "got: {bare}"
        );
        assert_ne!(bearing, bare, "one string per source cannot describe both");
    }

    #[test]
    fn a_truncated_entry_says_so_in_its_own_fidelity() {
        let sink = NetworkSink::new();
        sink.push(
            NetworkSource::InApp,
            BodyTruncation::Request,
            with_bodies("/x", 1),
        );
        let f = sink.snapshot()[0].fidelity();
        assert!(
            f.contains("Cut short at the capture cap: request"),
            "got: {f}"
        );
    }

    #[test]
    fn the_legend_counts_the_bodies_the_batch_actually_holds() {
        let sink = NetworkSink::new();
        sink.push(
            NetworkSource::InApp,
            BodyTruncation::None,
            with_bodies("/a", 1),
        );
        sink.push(NetworkSource::InApp, BodyTruncation::None, event("/b", 2));

        let (counts, legend) = provenance(&sink.snapshot());
        assert_eq!(counts["in_app"], 2);
        let sdk = legend["in_app"].as_str().unwrap();
        assert!(
            sdk.contains("1 of 2 entries here carry a body"),
            "got: {sdk}"
        );
        assert!(
            !sdk.contains("carries no headers and no bodies"),
            "the source legend must not deny bodies the batch holds: {sdk}"
        );
    }

    #[test]
    fn every_entry_keeps_the_source_that_produced_it() {
        let sink = NetworkSink::new();
        sink.push(NetworkSource::Logcat, BodyTruncation::None, event("/a", 1));
        sink.push(NetworkSource::InApp, BodyTruncation::None, event("/b", 2));
        sink.push(
            NetworkSource::InApp,
            BodyTruncation::Response,
            event("/c", 3),
        );

        let snap = sink.snapshot();
        let sources: Vec<NetworkSource> = snap.iter().map(|e| e.source).collect();
        assert_eq!(
            sources,
            vec![
                NetworkSource::Logcat,
                NetworkSource::InApp,
                NetworkSource::InApp
            ],
            "the sink must not flatten three fidelities into one"
        );
        assert_eq!(snap[2].truncated, BodyTruncation::Response);
    }

    #[test]
    fn summary_reports_source_and_truncation_per_call() {
        let sink = NetworkSink::new();
        sink.push(
            NetworkSource::InApp,
            BodyTruncation::Both,
            event("https://api.example.com/x", 1),
        );
        sink.push(
            NetworkSource::Logcat,
            BodyTruncation::None,
            event("https://api.example.com/y", 2),
        );

        let calls = summarize(&sink.snapshot());
        let arr = calls.as_array().unwrap();
        assert_eq!(arr[0]["source"], "in_app");
        assert_eq!(arr[0]["truncated"], "both");
        assert_eq!(arr[0]["url"], "api.example.com/x");
        assert_eq!(arr[1]["source"], "logcat");
        assert!(
            arr[1].get("truncated").is_none(),
            "untruncated calls carry no flag"
        );
    }

    #[test]
    fn provenance_counts_only_sources_present() {
        let sink = NetworkSink::new();
        sink.push(NetworkSource::Logcat, BodyTruncation::None, event("/a", 1));
        sink.push(NetworkSource::Logcat, BodyTruncation::None, event("/b", 2));

        // A source with no entries must be absent, not reported as zero: the
        // legend states what the batch actually carries.
        let (counts, legend) = provenance(&sink.snapshot());
        assert_eq!(counts["logcat"], 2);
        assert!(counts.get("in_app").is_none());
        assert!(legend["logcat"]
            .as_str()
            .unwrap()
            .contains("Never request headers"));
        assert!(legend.get("in_app").is_none());
    }

    #[test]
    fn since_selects_a_capture_window() {
        let sink = NetworkSink::new();
        sink.push(
            NetworkSource::Logcat,
            BodyTruncation::None,
            event("/old", 100),
        );
        sink.push(
            NetworkSource::InApp,
            BodyTruncation::None,
            event("/new", 200),
        );

        let window = sink.since(150);
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].event.url, "/new");
    }

    #[test]
    fn drain_removes_one_source_and_keeps_the_others_in_order() {
        let sink = NetworkSink::new();
        sink.push(NetworkSource::Logcat, BodyTruncation::None, event("/l1", 1));
        sink.push(NetworkSource::InApp, BodyTruncation::None, event("/m1", 2));
        sink.push(NetworkSource::Logcat, BodyTruncation::None, event("/l2", 3));
        sink.push(NetworkSource::InApp, BodyTruncation::None, event("/m2", 4));

        let drained = sink.drain(NetworkSource::InApp);
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].event.url, "/m1");

        let left = sink.snapshot();
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].event.url, "/l1");
        assert_eq!(left[1].event.url, "/l2");
        assert!(left.iter().all(|e| e.source == NetworkSource::Logcat));
    }

    #[test]
    fn ring_is_bounded_and_drops_the_oldest() {
        let sink = NetworkSink::new();
        for i in 0..(MAX_EVENTS + 50) {
            sink.push(
                NetworkSource::InApp,
                BodyTruncation::None,
                event(&format!("/{i}"), i as u64),
            );
        }
        let snap = sink.snapshot();
        assert_eq!(snap.len(), MAX_EVENTS);
        assert_eq!(snap.first().unwrap().event.url, "/50");
    }

    #[test]
    fn entry_json_carries_source_beside_the_event_fields() {
        let entry = SinkEntry {
            source: NetworkSource::InApp,
            truncated: BodyTruncation::Request,
            event: event("/api", 7),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["source"], "in_app");
        assert_eq!(json["truncated"], "request");
        assert_eq!(json["url"], "/api");
    }
}
