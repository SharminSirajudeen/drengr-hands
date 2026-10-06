//! What a run leaves behind when asked: one PNG per executed step, a JSON
//! trail of what each action changed, the calls the task made, and the device
//! log tail when it failed. `drengr test` writes one folder per task under its
//! evidence directory; a wrapper turns those folders into a page.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::network::sink::{NetworkSource, SinkEntry};
use crate::situation::report::SituationReport;
use crate::transport::{DeviceTransport, LogEntry};

/// Lines of device log kept in `logs.txt` for a failed task.
const LOG_TAIL_LINES: usize = 200;
/// Lines fetched before scrubbing. An app logging at BODY level fills a window
/// with OkHttp lines; fetching only the tail would scrub it down to nothing
/// and push the crash trace out of view.
const LOG_FETCH_LINES: usize = 500;
const TRAIL_FILE: &str = "steps.json";

/// One executed step as the trail records it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrailStep {
    pub step: usize,
    pub action: String,
    pub activity: String,
    pub screen_changed: bool,
    pub activity_changed: bool,
    pub crash: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_elements: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disappeared_elements: Vec<String>,
    /// File name of the screen the decision was made on, when it was fetched
    /// fresh for this step; otherwise the previous step's `screenshot` is it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_before: Option<String>,
    /// File name of the screen after this action, relative to the task folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
}

/// Recorder the OODA loop writes into. The trail is persisted after every
/// step, so a task that times out still leaves what it did up to that moment.
/// With no directory it writes nothing, so `drengr run` costs nothing extra.
pub struct Trail {
    dir: Option<PathBuf>,
    steps: Vec<TrailStep>,
}

impl Trail {
    /// A folder that cannot be created is given up once, with one warning,
    /// rather than once per frame. The trail file is written at once, so a
    /// task that ends before its first step still leaves `[]` on disk.
    pub fn new(dir: Option<PathBuf>) -> Self {
        let dir = dir.filter(|d| match std::fs::create_dir_all(d) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!("evidence folder {}: {}; recording nothing", d.display(), e);
                false
            }
        });
        let trail = Self {
            dir,
            steps: Vec::new(),
        };
        trail.persist();
        trail
    }

    /// Write `<name>.png` into the task folder. Returns the file name, or
    /// `None` when nothing is being recorded or there was nothing to write.
    pub fn frame(&self, name: &str, png: &[u8]) -> Option<String> {
        let dir = self.dir.as_ref()?;
        if png.is_empty() {
            return None;
        }
        let file = format!("{name}.png");
        match std::fs::write(dir.join(&file), png) {
            Ok(()) => Some(file),
            Err(e) => {
                tracing::warn!("evidence frame {}: {}", file, e);
                None
            }
        }
    }

    /// Record what an action did. `None` for the report means the action was
    /// refused before it reached the device.
    pub fn acted(
        &mut self,
        step: usize,
        action: &str,
        activity: &str,
        report: Option<&SituationReport>,
        screenshot_before: Option<String>,
        screenshot: Option<String>,
    ) {
        let mut entry = TrailStep {
            step,
            action: action.to_string(),
            activity: activity.to_string(),
            screen_changed: false,
            activity_changed: false,
            crash: false,
            new_elements: Vec::new(),
            disappeared_elements: Vec::new(),
            screenshot_before,
            screenshot,
        };
        if let Some(r) = report {
            entry.screen_changed = r.screen_changed;
            entry.activity_changed = r.activity_changed;
            entry.crash = r.crash;
            entry.new_elements = r.new_elements.clone();
            entry.disappeared_elements = r.disappeared_elements.clone();
        }
        self.steps.push(entry);
        self.persist();
    }

    #[cfg(test)]
    pub fn steps(&self) -> &[TrailStep] {
        &self.steps
    }

    fn persist(&self) {
        let Some(dir) = &self.dir else { return };
        let json = serde_json::to_vec_pretty(&self.steps).unwrap_or_default();
        if let Err(e) = std::fs::write(dir.join(TRAIL_FILE), json) {
            tracing::warn!("evidence trail: {}", e);
        }
    }
}

/// The trail a task left on disk; empty when it left none.
pub fn read_trail(dir: &Path) -> Vec<TrailStep> {
    std::fs::read(dir.join(TRAIL_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// `01-checkout-flow`: sortable, filesystem-safe, derived from the task name.
pub fn task_folder(index: usize, name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().take(60) {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("{:02}-task", index + 1)
    } else {
        format!("{:02}-{}", index + 1, slug)
    }
}

/// A call as the evidence records it: no headers, no bodies, no query string.
/// The folder is uploaded as a CI artifact and summarised on a pull request,
/// so the credential-bearing parts of a capture never leave the runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub source: NetworkSource,
}

impl CallRecord {
    pub fn from_entry(entry: &SinkEntry) -> Self {
        let e = &entry.event;
        Self {
            method: e.method.clone(),
            url: scrub_url(&e.url),
            status: e.status,
            duration_ms: e.duration_ms,
            source: entry.source,
        }
    }
}

/// Keep scheme, host and path. Query strings and fragments carry tokens and
/// session ids; userinfo carries passwords. None of it is evidence of a bug.
pub fn scrub_url(url: &str) -> String {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    let kept = &url[..end];
    let Some(scheme_end) = kept.find("://") else {
        return kept.to_string();
    };
    let authority_start = scheme_end + 3;
    let authority_end = kept[authority_start..]
        .find('/')
        .map_or(kept.len(), |i| authority_start + i);
    match kept[authority_start..authority_end].rfind('@') {
        Some(at) => format!(
            "{}{}",
            &kept[..authority_start],
            &kept[authority_start + at + 1..]
        ),
        None => kept.to_string(),
    }
}

/// Everything a task left behind besides the screenshots.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskCapture {
    pub trail: Vec<TrailStep>,
    pub calls: Vec<CallRecord>,
    pub log_tail: Vec<String>,
    /// `death_report`'s verdict when the process is not running: "crashed: …".
    pub death: Option<String>,
}

/// Gather, write and prune one task's evidence. A passing task keeps its
/// trail and calls as text and loses its screenshots, and its trail then
/// names no frame; a failing task keeps everything and adds the device log
/// tail and the process verdict.
pub async fn capture_task(
    transport: &dyn DeviceTransport,
    package: &str,
    dir: &Path,
    window: &[SinkEntry],
    failed: bool,
) -> TaskCapture {
    let (log_tail, death) = if failed {
        let (logs, death) = tokio::join!(
            transport.read_logs(package, None, LOG_FETCH_LINES),
            transport.death_report(package)
        );
        let tail = match logs {
            Ok(entries) => log_tail_from(&entries),
            Err(e) => vec![format!("device log unavailable: {e:#}")],
        };
        (tail, death_line(death))
    } else {
        (Vec::new(), None)
    };
    let mut capture = TaskCapture {
        trail: read_trail(dir),
        calls: window.iter().map(CallRecord::from_entry).collect(),
        log_tail,
        death,
    };
    if !failed {
        capture.trail = prune_passed(dir, capture.trail);
    }
    if let Err(e) = write_task(dir, &capture) {
        tracing::warn!("evidence in {}: {:#}", dir.display(), e);
    }
    capture
}

/// A task that passed keeps its text and loses its screenshots, and its
/// trail then names none of them, so no reader follows a name to a frame
/// that is gone.
fn prune_passed(dir: &Path, trail: Vec<TrailStep>) -> Vec<TrailStep> {
    drop_frames(dir);
    let trail = forget_frames(trail);
    if let Ok(json) = serde_json::to_vec_pretty(&trail) {
        let _ = std::fs::write(dir.join(TRAIL_FILE), json);
    }
    trail
}

/// The trail of a task whose frames were dropped names none of them.
fn forget_frames(trail: Vec<TrailStep>) -> Vec<TrailStep> {
    trail
        .into_iter()
        .map(|s| TrailStep {
            screenshot_before: None,
            screenshot: None,
            ..s
        })
        .collect()
}

fn death_line((reason, line): (String, Option<String>)) -> Option<String> {
    if reason == "running" {
        return None;
    }
    Some(match line {
        Some(l) => format!("{reason}: {l}"),
        None => reason,
    })
}

/// The device log as the evidence keeps it. Lines from the OkHttp logging
/// interceptor are dropped whole: at HEADERS or BODY level they carry request
/// headers and bodies verbatim, and the calls themselves are already in
/// `network.json` without either. Every other line keeps its shape with
/// credentials and the usual PII patterns redacted. The last `LOG_TAIL_LINES`
/// that survive are kept, so a BODY-level app cannot scrub its own crash
/// trace out of the window.
pub(crate) fn log_tail_from(entries: &[LogEntry]) -> Vec<String> {
    let kept: Vec<String> = entries.iter().filter_map(scrub_log_line).collect();
    let start = kept.len().saturating_sub(LOG_TAIL_LINES);
    kept[start..].to_vec()
}

fn scrub_log_line(e: &LogEntry) -> Option<String> {
    if e.tag.to_ascii_lowercase().contains("okhttp") {
        return None;
    }
    let line = format!("{} {}/{}: {}", e.timestamp, e.level, e.tag, e.message);
    let line = header_pattern().replace_all(&line, "${1}: <redacted>");
    let line = value_pattern().replace_all(&line, "${1}: <redacted>");
    Some(crate::redact::redact(&line))
}

/// A header carries its whole value to the end of the line (`Bearer x`,
/// `Basic y`), so everything after the name goes. Over-redacting a log line
/// costs a reader a little; leaking a token into a CI artifact costs more.
fn header_pattern() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r#"(?i)["']?\b(authorization|proxy-authorization|cookie|set-cookie|x-api-key|api[-_]?key)\b["']?\s*[:=].*$"#)
            .expect("header pattern compiles")
    })
}

/// A value key, quoted or bare as in JSON or `k=v`, loses its one value,
/// quoted or bare.
fn value_pattern() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r#"(?i)["']?\b(access[-_]?token|refresh[-_]?token|id[-_]?token|token|password|passwd|secret|client[-_]?secret)\b["']?\s*[:=]\s*["']?[^"'\s,}]+["']?"#)
            .expect("value pattern compiles")
    })
}

/// `network.json`, and for a failure `logs.txt` and `death.txt`. The trail
/// is the loop's to write; it is already on disk by the time this runs.
pub fn write_task(dir: &Path, capture: &TaskCapture) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    std::fs::write(
        dir.join("network.json"),
        serde_json::to_vec_pretty(&capture.calls)?,
    )?;
    if !capture.log_tail.is_empty() {
        std::fs::write(dir.join("logs.txt"), capture.log_tail.join("\n") + "\n")?;
    }
    if let Some(d) = &capture.death {
        std::fs::write(dir.join("death.txt"), format!("{d}\n"))?;
    }
    Ok(())
}

/// Remove the PNGs of a task that passed; its text files stay.
pub fn drop_frames(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "png") {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// One captured call for tests across the crate: a POST with the given
/// status, from logcat, no headers and no bodies.
#[cfg(test)]
pub(crate) fn sample_call(url: &str, status: Option<u16>) -> SinkEntry {
    SinkEntry {
        source: NetworkSource::Logcat,
        truncated: crate::network::sink::BodyTruncation::None,
        event: crate::network::events::NetworkEvent {
            url: url.into(),
            method: Some("POST".into()),
            status,
            duration_ms: Some(12),
            request_size: None,
            response_size: None,
            timestamp_ms: 0,
            request_headers: None,
            response_headers: None,
            request_body: None,
            response_body: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(crash: bool, new: &[&str]) -> SituationReport {
        SituationReport {
            step: 1,
            action: "x".into(),
            screen_changed: true,
            activity: "CheckoutActivity".into(),
            activity_changed: false,
            crash,
            stuck: false,
            new_elements: new.iter().map(|s| s.to_string()).collect(),
            disappeared_elements: vec![],
            scrollable: false,
            tree_unavailable: false,
            element_count: 1,
            interactive_count: 1,
            hint: None,
        }
    }

    fn log(tag: &str, message: &str) -> LogEntry {
        LogEntry {
            timestamp: "10:00:01.000".into(),
            level: "I".into(),
            tag: tag.into(),
            message: message.into(),
        }
    }

    #[test]
    fn trail_persists_after_every_step_so_a_timeout_loses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut trail = Trail::new(Some(dir.path().to_path_buf()));
        let shot = trail.frame("step_001", b"png-bytes");
        assert_eq!(shot.as_deref(), Some("step_001.png"));
        assert!(dir.path().join("step_001.png").exists());

        trail.acted(
            1,
            "Tapped #4 (Pay)",
            "CheckoutActivity",
            Some(&report(false, &["Processing"])),
            Some("step_001_before.png".into()),
            shot,
        );
        assert_eq!(
            read_trail(dir.path()).len(),
            1,
            "written before the run ends"
        );

        trail.acted(
            2,
            "Waited 1s",
            "CheckoutActivity",
            Some(&report(true, &[])),
            None,
            None,
        );
        let back = read_trail(dir.path());
        assert_eq!(back, trail.steps());
        assert!(back[1].crash);
        assert_eq!(back[0].new_elements, vec!["Processing".to_string()]);
    }

    #[test]
    fn a_trail_without_a_folder_writes_nothing_and_keeps_the_steps() {
        let mut trail = Trail::new(None);
        assert_eq!(trail.frame("step_001", b"png"), None);
        trail.acted(1, "tap", "A", None, None, None);
        assert_eq!(trail.steps().len(), 1);
        assert!(
            !trail.steps()[0].screen_changed,
            "a refused action changed nothing"
        );
    }

    #[test]
    fn an_empty_frame_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let trail = Trail::new(Some(dir.path().to_path_buf()));
        assert_eq!(trail.frame("step_001", b""), None);
        assert!(!dir.path().join("step_001.png").exists());
    }

    #[test]
    fn scrub_url_keeps_the_path_and_drops_what_carries_secrets() {
        assert_eq!(
            scrub_url("https://api.example.com/v1/charge?token=abc#frag"),
            "https://api.example.com/v1/charge"
        );
        assert_eq!(
            scrub_url("https://user:pw@api.example.com/x"),
            "https://api.example.com/x"
        );
        assert_eq!(scrub_url("/relative/path"), "/relative/path");
        assert_eq!(
            scrub_url("https://h/p@th"),
            "https://h/p@th",
            "an @ in the path is not userinfo"
        );
    }

    #[test]
    fn a_call_record_carries_no_headers_or_bodies() {
        let mut e = sample_call("https://api.example.com/charge?sid=1", Some(500));
        e.event.request_body = Some("card=4111".into());
        e.event.response_body = Some("secret".into());
        let json = serde_json::to_string(&CallRecord::from_entry(&e)).unwrap();
        assert!(!json.contains("4111") && !json.contains("secret") && !json.contains("sid=1"));
        assert!(json.contains("https://api.example.com/charge"));
    }

    #[test]
    fn task_folder_is_sortable_and_filesystem_safe() {
        assert_eq!(
            task_folder(0, "Checkout flow (UAE / Visa)"),
            "01-checkout-flow-uae-visa"
        );
        assert_eq!(task_folder(9, "///"), "10-task");
    }

    #[test]
    fn the_log_tail_drops_okhttp_lines_and_redacts_credentials_everywhere_else() {
        let entries = vec![
            log(
                "okhttp.OkHttpClient",
                "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.header",
            ),
            log("okhttp.OkHttpClient", "{\"card\":\"4111111111111111\"}"),
            log("MyApp", "retrying with x-api-key=sk_live_abcdef123456"),
            log("MyApp", "signed in as a@b.co"),
            log("Net", "Authorization: Basic dXNlcjpwYXNz"),
            log(
                "Net",
                "{\"password\":\"hunter2\",\"access_token\":\"abc123\"}",
            ),
            log("AndroidRuntime", "at com.app.Checkout.pay(Checkout.kt:42)"),
        ];
        let tail = log_tail_from(&entries);
        assert_eq!(tail.len(), 5, "both OkHttp lines are gone whole");
        assert!(tail[0].ends_with("x-api-key: <redacted>"), "{}", tail[0]);
        assert!(!tail[0].contains("sk_live"), "{}", tail[0]);
        assert!(tail[1].ends_with("signed in as <email>"), "{}", tail[1]);
        assert!(
            tail[2].ends_with("Authorization: <redacted>"),
            "a two-token header value goes whole: {}",
            tail[2]
        );
        assert!(
            !tail[3].contains("hunter2") && !tail[3].contains("abc123"),
            "quoted keys lose their values: {}",
            tail[3]
        );
        assert_eq!(
            tail[4],
            "10:00:01.000 I/AndroidRuntime: at com.app.Checkout.pay(Checkout.kt:42)"
        );
        let joined = tail.join("\n");
        assert!(!joined.contains("eyJ") && !joined.contains("4111") && !joined.contains("dXNl"));
    }

    #[test]
    fn the_tail_is_cut_after_scrubbing_so_okhttp_noise_cannot_push_the_crash_out() {
        let mut entries: Vec<LogEntry> = (0..400)
            .map(|i| log("okhttp.OkHttpClient", &format!("--> GET /x/{i}")))
            .collect();
        entries.insert(0, log("AndroidRuntime", "FATAL EXCEPTION: main"));
        let tail = log_tail_from(&entries);
        assert_eq!(tail.len(), 1);
        assert!(tail[0].ends_with("FATAL EXCEPTION: main"));
        let many: Vec<LogEntry> = (0..300).map(|i| log("App", &format!("line {i}"))).collect();
        let tail = log_tail_from(&many);
        assert_eq!(tail.len(), LOG_TAIL_LINES);
        assert!(
            tail[LOG_TAIL_LINES - 1].ends_with("line 299"),
            "the last lines are kept"
        );
    }

    #[test]
    fn a_new_trail_writes_an_empty_file_so_a_zero_step_task_still_has_one() {
        let dir = tempfile::tempdir().unwrap();
        let _trail = Trail::new(Some(dir.path().to_path_buf()));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("steps.json")).unwrap(),
            "[]"
        );
    }

    #[test]
    fn a_folder_that_cannot_be_made_records_nothing_and_says_so_once() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let inside = file.path().join("x");
        let mut trail = Trail::new(Some(inside));
        assert_eq!(trail.frame("step_001", b"png"), None, "no folder, no frame");
        trail.acted(1, "tap", "A", None, None, None);
        assert_eq!(trail.steps().len(), 1, "the steps are still kept in memory");
    }

    #[test]
    fn a_passing_task_loses_its_frames_and_its_trail_forgets_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("step_001.png"), b"png").unwrap();
        std::fs::write(dir.path().join("step_001_before.png"), b"png").unwrap();
        let named = vec![TrailStep {
            step: 1,
            action: "a".into(),
            activity: "A".into(),
            screen_changed: true,
            activity_changed: false,
            crash: false,
            new_elements: vec![],
            disappeared_elements: vec![],
            screenshot_before: Some("step_001_before.png".into()),
            screenshot: Some("step_001.png".into()),
        }];
        let trail = prune_passed(dir.path(), named);
        assert!(trail[0].screenshot.is_none() && trail[0].screenshot_before.is_none());
        assert!(
            !dir.path().join("step_001.png").exists()
                && !dir.path().join("step_001_before.png").exists()
        );
        let on_disk = std::fs::read_to_string(dir.path().join("steps.json")).unwrap();
        assert!(
            !on_disk.contains("screenshot"),
            "the file on disk names no frame either: {on_disk}"
        );
    }

    #[test]
    fn a_death_verdict_is_kept_only_when_the_process_is_not_running() {
        assert_eq!(death_line(("running".into(), None)), None);
        assert_eq!(
            death_line(("crashed".into(), Some("FATAL EXCEPTION: main".into()))),
            Some("crashed: FATAL EXCEPTION: main".into())
        );
        assert_eq!(death_line(("unknown".into(), None)), Some("unknown".into()));
    }

    #[test]
    fn a_passing_task_keeps_text_and_loses_screenshots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("step_001.png"), b"png").unwrap();
        let cap = TaskCapture {
            trail: vec![],
            calls: vec![CallRecord::from_entry(&sample_call(
                "https://h/ok",
                Some(200),
            ))],
            log_tail: vec![],
            death: None,
        };
        write_task(dir.path(), &cap).unwrap();
        drop_frames(dir.path());
        assert!(!dir.path().join("step_001.png").exists());
        assert!(dir.path().join("network.json").exists());
        assert!(
            !dir.path().join("logs.txt").exists(),
            "no log tail was gathered"
        );
        assert!(!dir.path().join("death.txt").exists());
    }

    #[test]
    fn a_failing_task_writes_its_log_and_its_death() {
        let dir = tempfile::tempdir().unwrap();
        let cap = TaskCapture {
            log_tail: vec!["line".into()],
            death: Some("crashed: boom".into()),
            ..Default::default()
        };
        write_task(dir.path(), &cap).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("logs.txt")).unwrap(),
            "line\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("death.txt")).unwrap(),
            "crashed: boom\n"
        );
    }
}
