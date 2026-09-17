use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tracing::{info, warn};

use super::provider::{build_capabilities, AppiumConfig, SessionOptions};
use super::AppiumTransport;

/// One timeout for deleting a session, whether it goes through `disconnect` or
/// `Drop`. They had drifted to 5s and 3s, so the same cleanup got a different
/// budget depending on which path ran it.
pub(super) const SESSION_DELETE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cap on the retained device-log buffer, in lines.
const LOG_BUFFER_MAX: usize = 5000;

/// Per-operation timeout configuration.
pub(super) struct Timeouts {
    pub session_create: Duration,
    pub screenshot: Duration,
    pub ui_tree: Duration,
    pub action: Duration,
    pub health_check: Duration,
    pub script: Duration,
    pub install: Duration,
    pub recording: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            session_create: Duration::from_secs(30),
            screenshot: Duration::from_secs(30),
            ui_tree: Duration::from_secs(20),
            action: Duration::from_secs(10),
            health_check: Duration::from_secs(5),
            script: Duration::from_secs(60),
            install: Duration::from_secs(180),
            recording: Duration::from_secs(120),
        }
    }
}

impl Timeouts {
    fn for_path(&self, path: &str) -> Duration {
        if path.is_empty() {
            self.health_check
        } else if path.contains("screenshot") {
            self.screenshot
        } else if path.contains("source") {
            self.ui_tree
        } else if path.contains("install_app") || path.contains("remove_app") {
            self.install
        } else if path.contains("recording_screen") {
            self.recording
        } else if path.contains("activate_app") {
            self.screenshot
        } else if path.contains("execute") || path.contains("log") || path == "/url" {
            self.script
        } else {
            self.action
        }
    }
}

/// Redact potential credentials/tokens from cloud provider error responses.
/// Strips known sensitive JSON keys and truncates to a safe length.
pub(super) fn sanitize_error_body(text: &str) -> String {
    let truncated = truncate_on_boundary(text, 300);
    if let Ok(mut val) = serde_json::from_str::<Value>(&truncated) {
        redact_sensitive_keys(&mut val);
        return val.to_string();
    }
    truncated
}

/// Cut to at most `max` bytes without splitting a UTF-8 character.
fn truncate_on_boundary(text: &str, max: usize) -> String {
    let end = (0..=text.len().min(max))
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    text[..end].to_string()
}

/// Recursively redact keys that commonly contain secrets in cloud provider responses.
fn redact_sensitive_keys(val: &mut Value) {
    const SENSITIVE_KEYS: &[&str] = &[
        "accesskey",
        "access_key",
        "securitytoken",
        "security_token",
        "password",
        "secret",
        "token",
        "authorization",
        "apikey",
        "api_key",
        "username",
        "user_name",
    ];
    match val {
        Value::Object(map) => {
            for (key, v) in map.iter_mut() {
                if SENSITIVE_KEYS.contains(&key.to_lowercase().as_str()) {
                    *v = Value::String("[REDACTED]".to_string());
                } else {
                    redact_sensitive_keys(v);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                redact_sensitive_keys(v);
            }
        }
        _ => {}
    }
}

/// One WebDriver failure, keeping the route that produced it and the W3C error
/// code apart from the prose. Every check that reads a response can then say
/// which endpoint answered and with what, and callers can match on the code
/// instead of grepping a message whose wording varies by provider.
#[derive(Debug)]
pub(super) struct WebDriverError {
    pub endpoint: String,
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for WebDriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} returned {}", self.endpoint, self.status)?;
        if !self.code.is_empty() {
            write!(f, " {}", self.code)?;
        }
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for WebDriverError {}

impl WebDriverError {
    fn parse(endpoint: String, status: u16, text: &str) -> Self {
        let redacted = serde_json::from_str::<Value>(text).map(|mut v| {
            redact_sensitive_keys(&mut v);
            v
        });
        let (code, message) = match &redacted {
            Ok(v) => (
                v["value"]["error"]
                    .as_str()
                    .map(str::to_string)
                    // JSONWP predates the W3C error strings and carries a number.
                    .or_else(|| v["status"].as_i64().map(|n| format!("status {n}")))
                    .unwrap_or_default(),
                v["value"]["message"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| sanitize_error_body(text)),
            ),
            Err(_) => (String::new(), sanitize_error_body(text)),
        };
        Self {
            endpoint,
            status,
            code,
            message: truncate_on_boundary(&message, 300),
        }
    }

    /// The W3C (or JSONWP) "nothing is showing" answer to an alert query, which
    /// is a real answer rather than a failure to look.
    pub(super) fn is_no_such_alert(&self) -> bool {
        self.code == "no such alert" || self.code == "status 27"
    }
}

/// The route a command addressed, without the session token that sits in the
/// real URL.
fn endpoint(method: &str, path: &str) -> String {
    format!("{method} /session/:id{path}")
}

/// The one shape for "the endpoint answered, but not with what this code
/// assumes". Naming the route and the answer is what lets a single live run
/// identify the broken assumption instead of starting another round of guessing.
pub(super) fn unexpected(
    endpoint: &str,
    got: impl std::fmt::Display,
    expected: &str,
) -> anyhow::Error {
    anyhow::anyhow!(
        "{endpoint} returned {got}; expected {expected}. \
         Appium/WDA version or provider mismatch, see A01 B3."
    )
}

/// The same shape for a route that failed outright rather than answering oddly.
pub(super) fn assumption(endpoint: &str, expected: &str) -> String {
    format!(
        "{endpoint} failed; this code assumes {expected}. \
         Appium/WDA version or provider mismatch, see A01 B3."
    )
}

/// A device identity that holds for the life of the session, tells two sessions
/// on the same device model apart, and is not the session token. That token
/// grants control of a billed cloud device, so it is written nowhere: this is a
/// digest of it, which is all an identity check needs.
fn stable_id(platform: &str, device_name: &str, session_id: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    session_id.hash(&mut h);
    format!(
        "cloud-{}-{}-{:016x}",
        slug(platform),
        slug(device_name),
        h.finish()
    )
}

/// Lowercase `[a-z0-9-]`. Serves the device identity, which has to satisfy
/// `is_valid_device_id`, and recording filenames, which slugged separately.
pub(super) fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

impl Drop for AppiumTransport {
    fn drop(&mut self) {
        if let Some(session_id) = self.session_id.take() {
            let url = self.session_url(&session_id);
            let client = self.http.clone();
            // Spawn a thread to clean up the cloud session — prevents billing for leaked sessions.
            // Can't use async in Drop, so we build a tiny runtime.
            std::thread::spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => {
                        let _ = rt.block_on(async {
                            client
                                .delete(&url)
                                .timeout(SESSION_DELETE_TIMEOUT)
                                .send()
                                .await
                        });
                    }
                    Err(e) => warn!("Failed to clean up cloud session: {}", e),
                }
            });
        }
    }
}

impl AppiumTransport {
    /// Create a session on the cloud device with retry.
    pub async fn connect(config: &AppiumConfig) -> Result<Self> {
        let provider_config = config.provider.config();
        let base_url = config.server_url.clone().unwrap_or(provider_config.hub_url);

        let timeouts = Timeouts::default();
        let http = reqwest::Client::builder()
            .timeout(timeouts.session_create)
            .build()
            .context("Failed to build HTTP client")?;

        let options = SessionOptions::from_env();
        let capabilities = build_capabilities(config, &options);
        let body = json!({
            "desiredCapabilities": capabilities,
            "capabilities": { "alwaysMatch": capabilities },
        });

        let url = format!("{}/session", base_url);

        let mut last_err = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                let delay = Duration::from_secs(2u64 * (1 << (attempt - 1)));
                warn!(
                    "[{}] Session create attempt {} failed, retrying in {:?}",
                    config.provider.name(),
                    attempt,
                    delay
                );
                tokio::time::sleep(delay).await;
            }

            let mut req = http.post(&url).json(&body);
            if provider_config.needs_basic_auth && !config.username.is_empty() {
                req = req.basic_auth(&config.username, Some(&config.access_key));
            }

            let response = match req.timeout(timeouts.session_create).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    continue;
                }
            };

            let status = response.status();
            let text = response.text().await.unwrap_or_default();

            if !status.is_success() {
                last_err = format!("{}: {}", status, sanitize_error_body(&text));
                continue;
            }

            let parsed: Value =
                serde_json::from_str(&text).context("Failed to parse session response")?;

            let session_id = parsed["value"]["sessionId"]
                .as_str()
                .or_else(|| parsed["sessionId"].as_str())
                .ok_or_else(|| anyhow::anyhow!("No sessionId in response (check provider status)"))?
                .to_string();

            // Client default must be >= the longest per-operation timeout so
            // per-request .timeout() overrides work for shorter operations.
            let http = reqwest::Client::builder()
                .timeout(timeouts.install.max(timeouts.recording))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new());

            let transport = Self {
                http,
                base_url,
                stable_id: stable_id(&config.platform, &config.device_name, &session_id),
                session_id: Some(session_id),
                device_name: config.device_name.clone(),
                platform: config.platform.clone(),
                os_version: config.os_version.clone(),
                timeouts,
                recording: std::sync::Mutex::new(None),
                log_buffer: std::sync::Mutex::new(Vec::new()),
            };

            // Wait for device readiness — cloud devices need 30-60s after session creation.
            // BrowserStack returns 200 with empty body while the screenshot service boots.
            for probe in 0..10 {
                if probe > 0 {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                match transport.cmd("GET", "/screenshot", None).await {
                    Ok(_) => {
                        info!(
                            "[{}] Device ready after probe {}/10",
                            config.provider.name(),
                            probe + 1
                        );
                        return Ok(transport);
                    }
                    Err(e) => warn!(
                        "[{}] Device readiness probe {}/10: {}",
                        config.provider.name(),
                        probe + 1,
                        e
                    ),
                }
            }
            warn!(
                "[{}] Device readiness probes exhausted after ~30s, returning transport anyway",
                config.provider.name()
            );
            return Ok(transport);
        }

        anyhow::bail!(
            "{} session creation failed after 3 attempts: {}",
            config.provider.name(),
            last_err
        )
    }

    /// Build a transport pointed at an arbitrary base URL, for tests against a mock hub.
    #[cfg(test)]
    pub(super) fn for_test(base_url: &str, platform: &str) -> Self {
        Self::for_session(base_url, platform, "test-session")
    }

    /// Same, with the session id under test. Two cloud sessions on one device
    /// model must not share an identity, and the session id is the only thing
    /// that separates them.
    #[cfg(test)]
    pub(super) fn for_session(base_url: &str, platform: &str, session_id: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.to_string(),
            stable_id: stable_id(platform, "Test Device", session_id),
            session_id: Some(session_id.to_string()),
            device_name: "Test Device".to_string(),
            platform: platform.to_string(),
            os_version: "1.0".to_string(),
            timeouts: Timeouts::default(),
            recording: std::sync::Mutex::new(None),
            log_buffer: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Session ID — returns error if session was already closed.
    fn sid(&self) -> Result<&str> {
        self.session_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Cloud session closed. Reconnect with drengr_query(question='connect')."
            )
        })
    }

    /// Send a WebDriver command with timeout and retry for transient errors.
    pub(super) async fn cmd(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let timeout = self.timeouts.for_path(path);
        let max_retries: u32 = if path.contains("screenshot") || path.contains("source") {
            3
        } else {
            1
        };

        let mut last_err = String::new();
        for attempt in 0..=max_retries {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 * 2u64.pow(attempt - 1))).await;
            }

            let url = format!("{}/session/{}{}", self.base_url, self.sid()?, path);

            let request = match method {
                "GET" => self.http.get(&url),
                "POST" => self.http.post(&url),
                "DELETE" => self.http.delete(&url),
                _ => anyhow::bail!("Unsupported HTTP method: {}", method),
            };

            let request = if let Some(ref b) = body {
                request.header("Content-Type", "application/json").json(b)
            } else {
                request
            };

            let response = match request.timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    if e.is_timeout() || e.is_connect() {
                        continue;
                    }
                    return Err(e.into());
                }
            };

            let status = response.status();
            let text = response.text().await.unwrap_or_default();

            if status.is_success() {
                if text.is_empty() {
                    // For data queries, an empty body is an error — retry.
                    // For actions (tap, swipe, launch, key), it means success.
                    let needs_data = path.contains("screenshot")
                        || path.contains("source")
                        || path.contains("/element");
                    if needs_data {
                        last_err = format!("{} {} returned empty body (timeout?)", method, path);
                        continue;
                    }
                    return Ok(json!({ "value": null }));
                }
                return serde_json::from_str(&text).with_context(|| {
                    format!(
                        "Failed to parse Appium response ({} bytes): {}",
                        text.len(),
                        sanitize_error_body(&text)
                    )
                });
            }

            let failure = WebDriverError::parse(endpoint(method, path), status.as_u16(), &text);

            // Retry on 503 (Service Unavailable) — common during cloud device transients
            if status.as_u16() == 503 {
                last_err = failure.to_string();
                continue;
            }

            return Err(failure.into());
        }

        anyhow::bail!(
            "{} failed after retries: {}",
            endpoint(method, path),
            last_err
        )
    }

    /// Run an Appium `mobile:` extension command. Pass `Value::Null` for no arguments.
    pub(super) async fn execute(&self, script: &str, args: Value) -> Result<Value> {
        let args = if args.is_null() {
            json!([])
        } else {
            json!([args])
        };
        self.cmd(
            "POST",
            "/execute/sync",
            Some(json!({ "script": script, "args": args })),
        )
        .await
        .with_context(|| format!("{} failed", script))
    }

    /// The session endpoint. Built once so `disconnect` and `Drop` cannot address
    /// different URLs.
    pub(super) fn session_url(&self, session_id: &str) -> String {
        format!("{}/session/{}", self.base_url, session_id)
    }

    /// Close the Appium session explicitly. Preferred over Drop, which can only
    /// fire a detached thread that the process may outlive, leaking a session the
    /// provider keeps billing.
    pub async fn disconnect(&mut self) -> Result<()> {
        if let Some(session_id) = self.session_id.take() {
            let url = self.session_url(&session_id);
            match self
                .http
                .delete(&url)
                .timeout(SESSION_DELETE_TIMEOUT)
                .send()
                .await
            {
                Ok(resp) if !resp.status().is_success() => {
                    warn!("Session cleanup returned {}", resp.status());
                }
                Err(e) => warn!("Session cleanup failed: {}", e),
                _ => {}
            }
        }
        Ok(())
    }

    /// Check if this is an Android device.
    pub(super) fn is_android(&self) -> bool {
        self.platform.to_lowercase().contains("android")
    }

    /// The driver's identifier key for an application.
    pub(super) fn app_id_body(&self, package: &str) -> Value {
        if self.is_android() {
            json!({ "appId": package })
        } else {
            json!({ "bundleId": package })
        }
    }

    /// The log stream carrying app output for this platform.
    pub(super) fn log_type(&self) -> &'static str {
        if self.is_android() {
            "logcat"
        } else {
            "syslog"
        }
    }

    /// Get the active (focused) element ID for text input / clearing.
    pub(super) async fn active_element_id(&self) -> Result<String> {
        let active = self.cmd("POST", "/element/active", Some(json!({}))).await?;
        active["value"]["ELEMENT"]
            .as_str()
            .or_else(|| {
                active["value"]
                    .as_object()
                    .and_then(|o| o.values().next())
                    .and_then(|v| v.as_str())
            })
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("No active element found"))
    }

    /// Answer a showing alert. "Nothing to answer" is a different situation
    /// from "the driver could not answer it", and only the second is a fault.
    pub(super) async fn answer_alert(&self, route: &str, verb: &str) -> Result<()> {
        self.cmd("POST", route, Some(json!({})))
            .await
            .map_err(|e| {
                if e.downcast_ref::<WebDriverError>()
                    .is_some_and(WebDriverError::is_no_such_alert)
                {
                    anyhow::anyhow!("No alert is showing, so nothing was {verb}")
                } else {
                    e.context(assumption(
                        &endpoint("POST", route),
                        "an alert is showing and the driver will answer it; note that \
                     autoAcceptAlerts, if it was turned on, answers alerts before this runs",
                    ))
                }
            })?;
        Ok(())
    }

    /// Send one W3C pointer sequence as a single touch gesture.
    pub(super) async fn pointer_gesture(&self, steps: Vec<Value>) -> Result<()> {
        self.cmd(
            "POST",
            "/actions",
            Some(json!({
                "actions": [{
                    "type": "pointer",
                    "id": "finger1",
                    "parameters": { "pointerType": "touch" },
                    "actions": steps,
                }]
            })),
        )
        .await?;
        Ok(())
    }

    /// Fetch a driver log stream. Appium 3 moved these endpoints under `/se`,
    /// so a server that rejects one is retried on the other before giving up.
    async fn fetch_log(&self, log_type: &str) -> Result<Vec<Value>> {
        let body = Some(json!({ "type": log_type }));
        let resp = match self.cmd("POST", "/log", body.clone()).await {
            Ok(r) => r,
            Err(legacy) => self.cmd("POST", "/se/log", body).await.map_err(|w3c| {
                anyhow::anyhow!(
                    "the {log_type} log is unreachable: POST /session/:id/log said ({legacy}) \
                     and Appium 3's POST /session/:id/se/log said ({w3c}). \
                     Appium/WDA version or provider mismatch, see A01 B3."
                )
            })?,
        };
        resp["value"].as_array().cloned().ok_or_else(|| {
            unexpected(
                &format!("POST /session/:id/log (type {log_type})"),
                "no entries array",
                "an array of log entries",
            )
        })
    }

    /// Drain new device-log lines into the retained buffer and return everything
    /// held since the last `clear_device_log`. Appium's getLog empties the
    /// driver-side buffer on every read, so several readers (crash check,
    /// network capture, `read_logs`) would otherwise steal each other's lines.
    pub(super) async fn drain_device_log(&self) -> Result<Vec<String>> {
        let entries = self.fetch_log(self.log_type()).await?;
        let messages: Vec<String> = entries
            .iter()
            .filter_map(|e| e["message"].as_str())
            .map(str::to_string)
            .collect();
        // Entries in a shape this code cannot read would otherwise drain away
        // silently and leave every reader reporting an empty, healthy log.
        if messages.is_empty() && !entries.is_empty() {
            let fields = entries[0]
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| "a non-object entry".to_string());
            return Err(unexpected(
                &format!("POST /session/:id/log (type {})", self.log_type()),
                format!("{} entries carrying [{}]", entries.len(), fields),
                "each entry to carry a string `message` field",
            ));
        }
        let mut buf = self
            .log_buffer
            .lock()
            .map_err(|_| anyhow::anyhow!("log buffer poisoned"))?;
        buf.extend(messages);
        let overflow = buf.len().saturating_sub(LOG_BUFFER_MAX);
        buf.drain(..overflow);
        Ok(buf.clone())
    }

    /// Drop retained lines and the driver's pending ones, so the next drain is fresh.
    pub(super) async fn clear_device_log(&self) -> Result<()> {
        self.fetch_log(self.log_type()).await?;
        self.log_buffer
            .lock()
            .map_err(|_| anyhow::anyhow!("log buffer poisoned"))?
            .clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_error_body_redacts_credentials() {
        let body = r#"{"value":{"error":"bad","userName":"alice","accessKey":"s3cr3t"}}"#;
        let out = sanitize_error_body(body);
        assert!(!out.contains("s3cr3t"), "access key leaked: {out}");
        assert!(!out.contains("alice"), "username leaked: {out}");
        assert!(out.contains("REDACTED"));
    }

    #[test]
    fn sanitize_error_body_survives_multibyte_truncation() {
        let body = "é".repeat(400);
        let out = sanitize_error_body(&body);
        assert!(out.len() <= 300);
    }

    #[test]
    fn slow_endpoints_get_longer_budgets_than_taps() {
        let t = Timeouts::default();
        assert_eq!(t.for_path("/appium/device/install_app"), t.install);
        assert_eq!(t.for_path("/appium/stop_recording_screen"), t.recording);
        assert_eq!(t.for_path("/execute/sync"), t.script);
        assert_eq!(t.for_path("/log"), t.script);
        assert_eq!(t.for_path("/screenshot"), t.screenshot);
        assert_eq!(t.for_path("/actions"), t.action);
        assert!(t.for_path("/appium/device/install_app") > t.for_path("/actions"));
    }
}
