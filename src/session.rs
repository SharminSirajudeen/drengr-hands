use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::network::sink::{NetworkSource, SinkEntry};

/// A recorded test session with all steps, network calls, and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub app_package: String,
    pub device_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// Scene segmentation computed at `finish()` — the dashboard tiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollup: Option<SessionRollup>,
    pub steps: Vec<SessionStep>,
}

/// A single step in a test session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStep {
    pub step: usize,
    pub action: String,
    pub activity: String,
    pub timestamp_ms: u64,

    /// Screen changed after this action?
    pub screen_changed: bool,

    /// Every call the sink saw during this action, from every source. Carrying
    /// the `SinkEntry` rather than the bare event keeps the source with the
    /// call, so a reader can tell an SDK observation from a logcat one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_calls: Vec<SinkEntry>,

    /// Errors in network calls (for quick filtering).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_errors: Vec<NetworkErrorSummary>,

    /// Screenshot file path (relative to session dir).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
}

/// Compact error summary for quick scanning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkErrorSummary {
    pub url: String,
    pub status: u16,
    pub message: String,
    /// Which capture path saw it. An error only logcat saw and an error only the
    /// in-app SDK saw are different facts about the app.
    pub source: NetworkSource,
}

/// One dashboard tile: a run of consecutive steps sharing an activity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    pub activity: String,
    pub first_step: usize,
    pub last_step: usize,
    pub step_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    pub network_errors: usize,
}

/// Scene segmentation + totals — the dashboard session-view payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRollup {
    pub scenes: Vec<Scene>,
    pub total_steps: usize,
    pub total_network_errors: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub outcome: String,
}

impl Session {
    pub fn new(app_package: &str, device_id: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            app_package: app_package.to_string(),
            device_id: device_id.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            ended_at: None,
            rollup: None,
            steps: Vec::new(),
        }
    }

    /// Record a step with its network calls.
    pub fn record_step(
        &mut self,
        step: usize,
        action: &str,
        activity: &str,
        screen_changed: bool,
        network_calls: Vec<SinkEntry>,
        screenshot_path: Option<String>,
    ) {
        // `is_error` is the one definition of "failed request" in this codebase,
        // so this does not restate the 4xx threshold. Keeping only entries that
        // HAVE a status first is what lets the summary hold a plain u16 without
        // an unwrap that could fabricate a 0 for a status nobody captured.
        let network_errors: Vec<NetworkErrorSummary> = network_calls
            .iter()
            .filter_map(|entry| entry.event.status.map(|status| (entry, status)))
            .filter(|(entry, _)| entry.event.is_error() == Some(true))
            .map(|(entry, status)| {
                let e = &entry.event;
                let message = e
                    .response_body
                    .as_deref()
                    .and_then(|b| {
                        // Try to extract "message" from JSON error response
                        serde_json::from_str::<serde_json::Value>(b)
                            .ok()
                            .and_then(|v| {
                                v.get("message").and_then(|m| m.as_str()).map(String::from)
                            })
                    })
                    .unwrap_or_else(|| format!("HTTP {}", status));

                NetworkErrorSummary {
                    url: e.url.clone(),
                    status,
                    message,
                    source: entry.source,
                }
            })
            .collect();

        self.steps.push(SessionStep {
            step,
            action: action.to_string(),
            activity: activity.to_string(),
            timestamp_ms: now_ms(),
            screen_changed,
            network_calls,
            network_errors,
            screenshot_path,
        });
    }

    /// Finalize the session: stamp the end time and compute the scene rollup
    /// (the dashboard tiles).
    pub fn finish(&mut self) {
        self.ended_at = Some(chrono::Utc::now().to_rfc3339());
        self.rollup = Some(self.compute_rollup());
    }

    /// Collapse consecutive steps sharing an `activity` into scenes — one scene
    /// per `activity_changed` boundary. Each scene is a dashboard tile.
    pub fn compute_rollup(&self) -> SessionRollup {
        let mut scenes: Vec<Scene> = Vec::new();
        for s in &self.steps {
            match scenes.last_mut() {
                Some(scene) if scene.activity == s.activity => {
                    scene.last_step = s.step;
                    scene.step_count += 1;
                    scene.network_errors += s.network_errors.len();
                    if scene.screenshot_path.is_none() {
                        scene.screenshot_path = s.screenshot_path.clone();
                    }
                }
                _ => scenes.push(Scene {
                    activity: s.activity.clone(),
                    first_step: s.step,
                    last_step: s.step,
                    step_count: 1,
                    screenshot_path: s.screenshot_path.clone(),
                    network_errors: s.network_errors.len(),
                }),
            }
        }
        let duration_ms = match (self.steps.first(), self.steps.last()) {
            (Some(a), Some(b)) => Some(b.timestamp_ms.saturating_sub(a.timestamp_ms)),
            _ => None,
        };
        let outcome = self
            .steps
            .last()
            .map(|s| s.activity.clone())
            .unwrap_or_else(|| "no_steps".to_string());
        SessionRollup {
            scenes,
            total_steps: self.steps.len(),
            total_network_errors: self.total_network_errors(),
            duration_ms,
            outcome,
        }
    }

    /// Save session to ~/.drengr/sessions/<id>.json
    pub fn save(&self) -> anyhow::Result<PathBuf> {
        let dir = session_dir()?;
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", self.id));
        let json = serde_json::to_string_pretty(self)?;
        // Write with owner-only permissions (session data may contain API response bodies)
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)?
                .write_all(json.as_bytes())?;
            return Ok(path);
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&path, json)?;
            Ok(path)
        }
    }

    /// Total network calls across all steps.
    pub fn total_network_calls(&self) -> usize {
        self.steps.iter().map(|s| s.network_calls.len()).sum()
    }

    /// Total network errors across all steps.
    pub fn total_network_errors(&self) -> usize {
        self.steps.iter().map(|s| s.network_errors.len()).sum()
    }
}

/// Save a screenshot to the session directory.
pub fn save_screenshot(session_id: &str, step: usize, png_data: &[u8]) -> anyhow::Result<String> {
    let dir = session_dir()?.join(session_id);
    std::fs::create_dir_all(&dir)?;
    let filename = format!("step_{:03}.png", step);
    let path = dir.join(&filename);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?
            .write_all(png_data)?;
        return Ok(filename);
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, png_data)?;
        Ok(filename)
    }
}

fn session_dir() -> anyhow::Result<PathBuf> {
    Ok(crate::paths::drengr_dir_or(".").join("sessions"))
}

/// Epoch milliseconds, the same scale `SinkEntry.event.timestamp_ms` uses, so a
/// caller can ask the sink for the window covering one step.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The one-count guard.
///
/// `drengr_query(session)`, `drengr_query(network)` and `drengr_query(analyze)`
/// all answer "how many failed requests". They read different data:
/// `session` reads `SessionStep.network_errors`, the other two read the shared
/// sink. `do_action` passed `record_step` only the logcat calls, so on an app
/// running the Drengr SDK — capture above TLS, the one that survives pinning —
/// `session` reported fewer errors than the other two, or none.
///
/// The unit test below fixes the counting. This guard fixes the WIRING, which is
/// where the defect actually lived: a correct `record_step` fed the wrong subset
/// still reports the wrong number.
#[cfg(test)]
mod one_count_guard {
    use crate::source_guard::{line_of, rust_files_under, src_root, without_comments};

    #[test]
    fn record_step_is_fed_the_whole_sink_not_one_source() {
        let files = rust_files_under(&src_root());
        let (name, src) = files
            .iter()
            .find(|(n, _)| n == "mcp/handlers/do_action.rs")
            .expect("guard must still read the one record_step call site");
        let code = without_comments(src);

        let at = code.find("record_step(").unwrap_or_else(|| {
            panic!("{name}: no record_step call; this guard is reading the wrong file")
        });
        let end = at
            + code[at..]
                .find(");")
                .expect("unterminated record_step call");
        let args = &code[at..end];

        assert!(
            !args.contains("logcat_calls"),
            "{name}:{} feeds record_step only the logcat calls, so drengr_query(session) \
             cannot see SDK-sourced errors and disagrees with network and analyze. \
             Pass the sink window for the step instead.",
            line_of(&code, at)
        );
        assert!(
            args.contains("network_history"),
            "{name}:{} no longer feeds record_step from the shared sink, so the three \
             error counts can drift apart again.",
            line_of(&code, at)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_new() {
        let s = Session::new("com.app", "emulator-5554");
        assert_eq!(s.app_package, "com.app");
        assert_eq!(s.device_id, "emulator-5554");
        assert!(!s.id.is_empty());
        assert!(s.ended_at.is_none());
        assert!(s.steps.is_empty());
    }

    #[test]
    fn test_finish_computes_ended_at_and_scene_rollup() {
        let mut s = Session::new("com.app", "dev1");
        s.record_step(
            1,
            "open",
            "HomeActivity",
            true,
            vec![],
            Some("step_001.png".into()),
        );
        s.record_step(2, "scroll", "HomeActivity", true, vec![], None);
        s.record_step(
            3,
            "tap cart",
            "CartActivity",
            true,
            vec![],
            Some("step_003.png".into()),
        );
        s.finish();

        assert!(s.ended_at.is_some(), "finish() must stamp ended_at");
        let r = s.rollup.expect("finish() must compute a rollup");
        // Two scenes: a Home run (steps 1-2), then a Cart run (step 3).
        assert_eq!(r.scenes.len(), 2);
        assert_eq!(r.scenes[0].activity, "HomeActivity");
        assert_eq!(r.scenes[0].first_step, 1);
        assert_eq!(r.scenes[0].last_step, 2);
        assert_eq!(r.scenes[0].step_count, 2);
        assert_eq!(r.scenes[0].screenshot_path.as_deref(), Some("step_001.png"));
        assert_eq!(r.scenes[1].activity, "CartActivity");
        assert_eq!(r.scenes[1].first_step, 3);
        assert_eq!(r.total_steps, 3);
        assert_eq!(r.outcome, "CartActivity");
    }

    #[test]
    fn test_record_step() {
        let mut s = Session::new("com.app", "dev1");
        s.record_step(1, "Tapped #1", "LoginActivity", true, vec![], None);
        assert_eq!(s.steps.len(), 1);
        assert_eq!(s.steps[0].step, 1);
        assert_eq!(s.steps[0].action, "Tapped #1");
        assert!(s.steps[0].screen_changed);
    }

    /// One captured call. The source is a parameter because which capture path
    /// saw a failure is now part of what a step records.
    fn entry(source: NetworkSource, url: &str, status: u16, body: Option<&str>) -> SinkEntry {
        SinkEntry {
            source,
            truncated: crate::network::sink::BodyTruncation::None,
            event: crate::network::events::NetworkEvent {
                url: url.to_string(),
                method: Some("POST".to_string()),
                status: Some(status),
                duration_ms: Some(10),
                request_size: None,
                response_size: None,
                timestamp_ms: 0,
                request_headers: None,
                response_headers: None,
                request_body: None,
                response_body: body.map(str::to_string),
            },
        }
    }

    #[test]
    fn test_record_step_with_network_error() {
        let mut s = Session::new("com.app", "dev1");
        let error_event = entry(
            NetworkSource::Logcat,
            "https://api.example.com/save",
            400,
            Some(r#"{"message":"Invalid date"}"#),
        );
        s.record_step(
            1,
            "Tapped Continue",
            "DeliveryActivity",
            false,
            vec![error_event],
            None,
        );

        assert_eq!(s.steps[0].network_errors.len(), 1);
        assert_eq!(s.steps[0].network_errors[0].status, 400);
        assert_eq!(s.steps[0].network_errors[0].message, "Invalid date");
        assert_eq!(s.steps[0].network_errors[0].source, NetworkSource::Logcat);
    }

    /// The defect this change exists for. `drengr_query(session)` must count a
    /// failure the in-app SDK saw exactly the same as one logcat saw, because
    /// `network` and `analyze` already do and a user comparing the three numbers
    /// is entitled to one answer.
    #[test]
    fn a_failure_counts_once_whichever_source_saw_it() {
        let mut s = Session::new("com.app", "dev1");
        s.record_step(
            1,
            "tap Pay",
            "CheckoutActivity",
            true,
            vec![
                entry(NetworkSource::Logcat, "/api/charge", 500, None),
                entry(NetworkSource::Sdk, "/api/receipt", 500, None),
            ],
            None,
        );

        assert_eq!(
            s.total_network_errors(),
            2,
            "both sources' failures must count"
        );
        let sources: Vec<NetworkSource> =
            s.steps[0].network_errors.iter().map(|e| e.source).collect();
        assert!(
            sources.contains(&NetworkSource::Sdk),
            "the SDK failure must be attributed to the SDK"
        );
    }

    /// A status nobody captured is not a failure and is not a success.
    #[test]
    fn a_call_with_no_captured_status_is_not_counted_as_an_error() {
        let mut s = Session::new("com.app", "dev1");
        let mut unknown = entry(NetworkSource::Sdk, "/api/silent", 500, None);
        unknown.event.status = None;
        s.record_step(1, "tap", "A", true, vec![unknown], None);
        assert_eq!(s.total_network_errors(), 0);
        assert_eq!(s.total_network_calls(), 1, "it is still a call we saw");
    }

    #[test]
    fn test_session_totals() {
        let mut s = Session::new("com.app", "dev1");
        let ok_event = entry(NetworkSource::Logcat, "/api/ok", 200, None);
        let err_event = entry(NetworkSource::Logcat, "/api/fail", 500, None);

        s.record_step(1, "tap", "A", true, vec![ok_event.clone()], None);
        s.record_step(2, "tap", "B", true, vec![ok_event, err_event], None);

        assert_eq!(s.total_network_calls(), 3);
        assert_eq!(s.total_network_errors(), 1);
    }

    #[test]
    fn test_session_finish() {
        let mut s = Session::new("com.app", "dev1");
        assert!(s.ended_at.is_none());
        s.finish();
        assert!(s.ended_at.is_some());
    }

    #[test]
    fn test_session_serde_roundtrip() {
        let mut s = Session::new("com.app", "dev1");
        s.record_step(1, "Tapped #1", "LoginActivity", true, vec![], None);
        let json = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, s.id);
        assert_eq!(back.steps.len(), 1);
    }
}
