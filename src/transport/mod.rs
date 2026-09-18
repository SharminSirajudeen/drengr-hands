pub mod adb;
pub mod android_sdk;
pub mod appium;
pub mod boot;
pub mod detect;
pub mod probe;
pub mod simctl;

#[cfg(test)]
pub(crate) mod contract_test;

use anyhow::Result;
use async_trait::async_trait;

use crate::network::events::NetworkEvent;
use crate::screen::ui_element::{Bounds, DeviceInfo, Point, UiElement};

/// Default screen dimensions when screen_size() fails.
pub const DEFAULT_SCREEN_SIZE: (u32, u32) = (1080, 2340);

/// Widest frame any decoder here will accept, in either axis. Two paths decode
/// a full screenshot and both must refuse the same sizes.
pub const MAX_SCREENSHOT_DIM: u32 = 4096;

/// How long a swipe takes when the caller states a direction but no velocity.
pub const DEFAULT_SWIPE_DURATION_MS: u32 = 300;

/// Swipes one `scroll_to_top` / `scroll_to_bottom` will attempt before giving up.
pub const MAX_SCROLL_SWIPES: usize = 10;

/// User-facing description of one installed app, returned by
/// `list_apps_with_names`. Pairs the canonical `package` (bundle id on iOS,
/// package name on Android) with the launcher label and a coarse user/system
/// classification.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppInfo {
    /// Canonical id used by `launch_app` / `terminate_app`.
    pub package: String,
    /// Launcher label (e.g. "Maps"). Falls back to `package` if unresolved.
    pub display_name: String,
    /// User-installed vs system / preinstalled.
    pub kind: AppKind,
    /// Whether `display_name` is known or inferred.
    pub name_source: NameSource,
}

/// Where an app's display name came from, so a caller can tell a name we know from
/// one we guessed off the package id. Android carries no label in `dumpsys` and the
/// only exact route is pulling the APK (153 MB for one app) and running aapt2, so
/// most Android names are inferred and must say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NameSource {
    /// The platform or a curated table gave us the real name.
    Known,
    /// Derived from the package id. Best effort, can be wrong.
    Derived,
}

/// Coarse user-vs-system classification for `AppInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppKind {
    User,
    System,
}

/// One look at the device. `tree_error` is `Some` when the element list is empty
/// because the dump failed rather than because the screen has no elements: the
/// two look identical and call for opposite responses, so they must not be
/// collapsed into an empty vector.
pub struct Observation {
    pub frame: Vec<u8>,
    pub elements: Vec<UiElement>,
    pub tree_error: Option<String>,
}

/// Core abstraction for device communication.
/// Every implementation (ADB, simctl, Appium) provides these 13 methods.
#[async_trait]
pub trait DeviceTransport: Send + Sync {
    /// Stable platform tag for logs and run summaries.
    fn platform_kind(&self) -> &'static str {
        "unknown"
    }

    /// The device this transport drives. Used to stamp an observation so a later
    /// process can refuse to resolve numbers recorded for a different device:
    /// coordinates are in each device's own logical space, so a cross-device
    /// resolve always succeeds and is always wrong. Empty means unknown, which
    /// callers must treat as "cannot verify", never as a match.
    fn id(&self) -> &str {
        ""
    }

    /// Resolve the device's stable identity, if it has one that differs from the
    /// transport id. Called once after connecting, before `id()` is relied on.
    /// Default is a no-op: most transports already know who they are.
    async fn resolve_identity(&self) {}

    /// Capture a PNG screenshot of the device screen.
    async fn screenshot(&self) -> Result<Vec<u8>>;

    /// Dump the UI accessibility tree as structured elements.
    async fn ui_tree(&self) -> Result<Vec<UiElement>>;

    /// Capture screenshot + UI elements together. Default: two calls
    /// (`screenshot` + `ui_tree`). iOS overrides this with a single runner
    /// `observe()` that already returns both, dropping a redundant capture.
    async fn observe(&self) -> Result<Observation> {
        // Deliberately serial, tree first. Run in parallel the screenshot lands
        // in ~0.5s while a slow dump takes seconds, so the image was older than
        // the tree beside it: a launch returned a splash frame with markers
        // floating over blank space while the tree already held the real screen.
        // A settle poll cannot fix this on its own, because a static splash is
        // two matching frames and reads as settled.
        let (elements, tree_error) = match self.ui_tree().await {
            Ok(t) => (t, None),
            // A dead tree must not cost a good screenshot, but the caller has to
            // be told: an empty list is otherwise indistinguishable from a screen
            // that genuinely has no elements, and they need opposite responses.
            Err(e) => {
                tracing::warn!("UI tree unavailable ({e}) — observing with screenshot only");
                (Vec::new(), Some(e.to_string()))
            }
        };
        Ok(Observation {
            frame: self.screenshot().await?,
            elements,
            tree_error,
        })
    }

    /// Dump the raw UI tree source (XML for Android, accessibility hierarchy for iOS).
    /// Returns the unprocessed platform-native format for debugging and deep inspection.
    async fn raw_ui_tree(&self) -> Result<String> {
        Err(anyhow::anyhow!(
            "raw_ui_tree not supported on this transport"
        ))
    }

    /// Tap at screen coordinates.
    async fn tap(&self, x: i32, y: i32) -> Result<()>;

    /// Long press at screen coordinates.
    async fn long_press(&self, x: i32, y: i32, duration_ms: u32) -> Result<()>;

    /// Swipe from one point to another.
    async fn swipe(&self, from: Point, to: Point, duration_ms: u32) -> Result<()>;

    /// Swipe at a target velocity (points/second). Default derives a duration
    /// from `(distance/velocity)`, clamped to ≥80ms (iOS momentum threshold).
    async fn swipe_with_velocity(
        &self,
        from: Point,
        to: Point,
        velocity_pts_per_sec: f32,
    ) -> Result<()> {
        let duration_ms = swipe_duration_for_velocity(from, to, velocity_pts_per_sec);
        self.swipe(from, to, duration_ms).await
    }

    /// Trace a path of points as a single gesture. Default produces segmented
    /// strokes — pen-lifted between segments — by chaining `swipe()` per pair.
    /// Implementations that support continuous strokes (e.g. W3C actions)
    /// override this for true freehand input.
    async fn draw_path(&self, points: &[Point], duration_ms: u32) -> Result<()> {
        if points.len() < 2 {
            return Ok(());
        }
        let segs = (points.len() - 1) as u32;
        let per_seg_ms = draw_path_per_segment_ms(duration_ms, segs);
        for w in points.windows(2) {
            self.swipe(w[0], w[1], per_seg_ms).await?;
        }
        Ok(())
    }

    /// List installed third-party apps as bundle/package IDs.
    /// Default: not supported. An empty list is a real answer ("nothing
    /// third-party installed") and must never stand in for "could not ask".
    async fn list_installed_apps(&self) -> Result<Vec<String>> {
        anyhow::bail!("list_installed_apps not supported on this transport")
    }

    /// List installed apps with launcher labels. Default falls back to
    /// `list_installed_apps()` and uses the package as the display name.
    /// Transports (adb, simctl) override with platform-native queries.
    async fn list_apps_with_names(&self) -> Result<Vec<AppInfo>> {
        Ok(self
            .list_installed_apps()
            .await?
            .into_iter()
            .map(|p| AppInfo {
                display_name: p.clone(),
                package: p,
                kind: AppKind::User,
                // The package id standing in for a label is not a label.
                name_source: NameSource::Derived,
            })
            .collect())
    }

    /// Open a system search affordance with a query (iOS Spotlight, Android: no-op).
    /// Default: bail.
    async fn spotlight_search(&self, _query: &str) -> Result<()> {
        anyhow::bail!("spotlight_search not supported on this transport")
    }

    /// Send the device to its home screen / launcher.
    /// Default: emit Android HOME keycode.
    async fn go_home(&self) -> Result<()> {
        self.press_key(keycode::HOME).await
    }

    /// Type text into the currently focused field.
    async fn type_text(&self, text: &str) -> Result<()>;

    /// Press a key by keycode (e.g. KEYCODE_BACK = 4, KEYCODE_HOME = 3).
    async fn press_key(&self, keycode: i32) -> Result<()>;

    /// Launch an app by package name.
    async fn launch_app(&self, package: &str) -> Result<()>;

    /// Get screen dimensions (width, height) in pixels.
    async fn screen_size(&self) -> Result<(u32, u32)>;

    /// Check if device is still connected.
    async fn is_connected(&self) -> bool;

    /// Check if a specific app is in the foreground.
    async fn is_app_in_foreground(&self, package: &str) -> Result<bool>;

    /// Diagnose why `package`'s process is gone (or fine). Returns
    /// (reason, optional explaining log line). reason ∈ running | crashed | anr
    /// | killed | clean_exit | device_lost | unknown. Default is coarse; adb
    /// overrides with crash/ANR/lmkd-kill detection from the historical +
    /// system logcat buffers — the case where a bare "crashed:false" misleads.
    async fn death_report(&self, package: &str) -> (String, Option<String>) {
        if !self.is_connected().await {
            return ("device_lost".to_string(), None);
        }
        match self.is_app_in_foreground(package).await {
            Ok(true) => ("running".to_string(), None),
            _ => ("unknown".to_string(), None),
        }
    }

    /// Clear the currently focused text field.
    async fn clear_focused_field(&self) -> Result<()>;

    /// Get device metadata (ID, OS, model).
    async fn device_info(&self) -> Result<DeviceInfo>;

    /// Get the current foreground activity/screen name.
    async fn current_activity(&self) -> Result<String>;

    /// Clear HTTP log buffer so next capture only gets fresh entries.
    /// Default: not supported. A clear that silently did not happen makes the
    /// next capture hand back stale calls as if this action had caused them.
    async fn clear_http_logs(&self) -> Result<()> {
        anyhow::bail!("clear_http_logs not supported on this transport")
    }

    /// Capture HTTP network events from device logs (logcat for Android, os_log for iOS).
    /// Default: not supported. An empty vec means "the app made no calls", the
    /// opposite conclusion from "this transport cannot read the log".
    async fn capture_http_logs(&self) -> Result<Vec<NetworkEvent>> {
        anyhow::bail!("capture_http_logs not supported on this transport")
    }

    /// Check logcat for FATAL exceptions or ANR for the given package.
    /// Returns `true` if a real crash/ANR was detected in recent logs.
    /// A transport that cannot read logs says so: "no crash found" and
    /// "could not look" are different answers and must not collapse.
    async fn check_crash_logcat(&self, _package: &str) -> Result<bool> {
        anyhow::bail!("check_crash_logcat not supported on this transport")
    }

    /// Read device logs filtered by package and optional text/tag pattern.
    /// Default: not supported. An empty vec reads as "the app logged nothing".
    async fn read_logs(
        &self,
        _package: &str,
        _filter: Option<&str>,
        _lines: usize,
    ) -> Result<Vec<LogEntry>> {
        anyhow::bail!("read_logs not supported on this transport")
    }

    /// Check if the software keyboard is currently visible.
    /// Default: not supported. `false` reads as "looked, no keyboard", and a
    /// caller acts on it by typing into a field the keyboard is covering.
    async fn is_keyboard_visible(&self) -> Result<bool> {
        anyhow::bail!("is_keyboard_visible not supported on this transport")
    }

    /// Dismiss the software keyboard if visible.
    /// Default: not supported. A silent no-op leaves the keyboard up while the
    /// caller proceeds as though the screen beneath it were reachable.
    async fn dismiss_keyboard(&self) -> Result<()> {
        anyhow::bail!("dismiss_keyboard not supported on this transport")
    }

    /// Install an app. The argument is a local file path on a transport that
    /// drives a device on this machine, and a provider app id or fetchable URL
    /// (`bs://…`, `storage:…`) on a cloud hub, which resolves it server-side.
    /// Default: not supported.
    async fn install_app(&self, _path: &str) -> Result<()> {
        anyhow::bail!("App install not supported on this transport")
    }

    /// Uninstall an app by package/bundle id. Default: not supported.
    async fn uninstall_app(&self, _package: &str) -> Result<()> {
        anyhow::bail!("App uninstall not supported on this transport")
    }

    /// Pre-grant a system permission to an app so its consent dialog never
    /// appears. iOS: `simctl privacy grant <service> <bundle>`. Android:
    /// `pm grant <pkg> <android.permission.X>`. Default: not supported.
    async fn grant_permission(&self, _permission: &str, _package: &str) -> Result<()> {
        anyhow::bail!("grant_permission not supported on this transport")
    }

    // ── Extended capabilities (parity between the iOS runner and Android ADB) ──

    /// Toggle dark/light appearance mode.
    /// iOS: `simctl ui appearance`. Android: `cmd uimode night`.
    async fn set_appearance(&self, _dark: bool) -> Result<()> {
        anyhow::bail!("set_appearance not supported on this transport")
    }

    /// Force-terminate an app by package/bundle ID.
    /// iOS: `simctl terminate`. Android: `am force-stop`.
    async fn terminate_app(&self, _package: &str) -> Result<()> {
        anyhow::bail!("terminate_app not supported on this transport")
    }

    /// Wipe app data (like a fresh install): clears caches, prefs, databases.
    /// Android: `pm clear`. iOS simulator: no-op (simctl has no data-only clear).
    async fn clear_app_data(&self, _package: &str) -> Result<()> {
        anyhow::bail!("clear_app_data not supported on this transport")
    }

    /// Force-stop, wipe data where possible, relaunch. `terminate_app` and
    /// `clear_app_data` are best-effort — system apps on physical devices refuse
    /// `pm clear`, and the iOS simulator has no data-only clear at all, so this
    /// is a cold start on Android and a warm one on iOS. Only `launch_app` is
    /// mandatory.
    async fn reset_app(&self, package: &str) -> Result<()> {
        let _ = self.terminate_app(package).await;
        if let Err(e) = self.clear_app_data(package).await {
            tracing::warn!("reset_app: clear_app_data failed for {}: {}", package, e);
        }
        self.launch_app(package).await
    }

    /// Open a URL / deep link on the device.
    /// iOS: `simctl openurl`. Android: `am start -a VIEW -d <url>`.
    async fn open_url(&self, _url: &str) -> Result<()> {
        anyhow::bail!("open_url not supported on this transport")
    }

    /// Wake and unlock the device screen.
    /// Android: KEYCODE_WAKEUP + KEYCODE_MENU. Not available on the iOS simulator,
    /// which has no lock screen to dismiss.
    async fn unlock(&self) -> Result<()> {
        anyhow::bail!("unlock not supported on this transport")
    }

    /// Set device orientation. 0=portrait, 1=landscape-left, 2=portrait-upside-down, 3=landscape-right.
    /// iOS: the bundled runner's orientation endpoint. Android: `settings put system user_rotation`.
    async fn set_orientation(&self, _rotation: u8) -> Result<()> {
        anyhow::bail!("set_orientation not supported on this transport")
    }

    /// Get alert/dialog text if one is currently showing, or None.
    /// `Ok(None)` means "looked, nothing there"; the error means "this transport
    /// cannot look" — a caller must be able to tell those apart.
    /// Both platforms read it out of the element tree; there is no alert API in either.
    async fn alert_text(&self) -> Result<Option<String>> {
        anyhow::bail!("alert_text not supported on this transport")
    }

    /// Accept/dismiss the current alert/dialog.
    /// Both platforms find the accept button in the element tree and tap it.
    async fn alert_accept(&self) -> Result<()> {
        anyhow::bail!("alert_accept not supported on this transport")
    }

    /// Dismiss the current alert/dialog.
    async fn alert_dismiss(&self) -> Result<()> {
        anyhow::bail!("alert_dismiss not supported on this transport")
    }

    /// Query app lifecycle state. Returns: 0=not-installed, 1=not-running,
    /// 2=background-suspended, 3=background, 4=foreground. 0 is a real answer
    /// from a hub that does not hold the app, not a stand-in for unknown: a
    /// transport that cannot determine the state errors instead.
    /// iOS: `simctl spawn launchctl list`. Android: `pidof` + `dumpsys activity`.
    async fn app_state(&self, _package: &str) -> Result<u8> {
        anyhow::bail!("app_state not supported on this transport")
    }

    /// Simulate biometric authentication (fingerprint/face). `matches`=true for success.
    /// iOS: `simctl ui biometric`. Android: `adb emu finger touch` (emulator only).
    async fn simulate_biometric(&self, _matches: bool) -> Result<()> {
        anyhow::bail!("simulate_biometric not supported on this transport")
    }

    /// Set simulated GPS location.
    /// iOS: `simctl location set`. Android: `adb emu geo fix` (emulator).
    async fn set_location(&self, _lat: f64, _lng: f64) -> Result<()> {
        anyhow::bail!("set_location not supported on this transport")
    }

    /// Clear simulated GPS location (revert to device default).
    async fn clear_location(&self) -> Result<()> {
        anyhow::bail!("clear_location not supported on this transport")
    }

    /// Get clipboard/pasteboard content as UTF-8 text.
    async fn pasteboard_get(&self) -> Result<String> {
        anyhow::bail!("pasteboard_get not supported on this transport")
    }

    /// Set clipboard/pasteboard content.
    async fn pasteboard_set(&self, _text: &str) -> Result<()> {
        anyhow::bail!("pasteboard_set not supported on this transport")
    }

    // ── Screen recording + live streaming ──────────────────────────────

    /// Start recording the device screen to a file. Returns the path
    /// where the recording will be saved when `stop_recording` is called.
    ///
    /// - **iOS:** `xcrun simctl io <udid> recordVideo <path>` (works headlessly).
    /// - **Android:** `adb shell screenrecord /sdcard/drengr_<ts>.mp4` → `adb pull` on stop.
    ///
    /// The recording runs as a background process; the caller continues
    /// running actions while the screen is captured. Call `stop_recording`
    /// to finalize the file.
    ///
    /// Default: not supported.
    async fn start_recording(&self) -> Result<String> {
        anyhow::bail!("Screen recording not supported on this transport")
    }

    /// Stop an in-progress recording and return the path to the finalized
    /// video file. The file is ready for playback immediately after this
    /// returns.
    ///
    /// Default: not supported.
    async fn stop_recording(&self) -> Result<String> {
        anyhow::bail!("No recording in progress")
    }

    /// Post-run teardown. iOS: terminates the bundled runner so the next run
    /// starts cold. Android: no-op.
    /// Default: no-op.
    async fn cleanup_runtime(&self) -> Result<()> {
        Ok(())
    }

    /// URL of a live MJPEG screen stream for this device. No transport ships
    /// one yet, so every implementation answers `None`; the callers poll
    /// screenshots instead. `None` is the answer, not a stand-in for an error.
    async fn screen_stream_url(&self) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Create a transport for a detected device.
pub fn create_transport(device: &DetectedDevice) -> Box<dyn DeviceTransport> {
    match device.os {
        DeviceOs::Android => Box::new(adb::AdbTransport::new(&device.id)),
        DeviceOs::Ios => Box::new(simctl::SimctlTransport::new(&device.id)),
    }
}

/// Parse a `"[left,top][right,bottom]"` bounds string from UI tree XML.
pub fn parse_bounds(s: &str) -> Option<Bounds> {
    let nums: Vec<i32> = s
        .replace(['[', ']', ','], " ")
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    if nums.len() >= 4 {
        Some(Bounds::new(nums[0], nums[1], nums[2], nums[3]))
    } else {
        None
    }
}

/// Extract an XML attribute value as a borrowed slice (zero allocation).
/// Use for boolean checks like `extract_attr_ref(tag, "clickable") == Some("true")`.
pub fn extract_attr_ref<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    // Scan for `name="` without allocating a format string
    let name_bytes = name.as_bytes();
    let tag_bytes = tag.as_bytes();
    let mut i = 0;
    while i + name_bytes.len() + 2 <= tag_bytes.len() {
        if &tag_bytes[i..i + name_bytes.len()] == name_bytes
            && tag_bytes[i + name_bytes.len()] == b'='
            && tag_bytes[i + name_bytes.len() + 1] == b'"'
            && (i == 0 || tag_bytes[i - 1] == b' ' || tag_bytes[i - 1] == b'<')
        {
            let value_start = i + name_bytes.len() + 2;
            if let Some(end) = tag[value_start..].find('"') {
                return Some(&tag[value_start..value_start + end]);
            }
        }
        i += 1;
    }
    None
}

/// Extract an XML attribute value from a tag string (allocates a String).
/// Use when you need to store the result. For boolean checks, use `extract_attr_ref`.
pub fn extract_attr(tag: &str, name: &str) -> Option<String> {
    extract_attr_ref(tag, name).map(decode_xml_entities)
}

/// Decode the XML entities uiautomator and Appium emit inside attribute values.
/// Labels arrived as "A softer way&#10;through change.", so anything matching on
/// element text had to know to escape, while curly quotes came through raw: the
/// encoding was not uniform. Single pass, so an escaped entity like `&amp;#10;`
/// decodes to the literal `&#10;` rather than a newline.
pub fn decode_xml_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        // Scan bytes, never slice the str: tail.len().min(12) is a BYTE index and
        // cutting there lands mid-character on any CJK or emoji label, panicking the
        // whole process. A byte offset of ASCII ';' is always a char boundary.
        let Some(semi) = tail.as_bytes()[..tail.len().min(12)]
            .iter()
            .position(|&b| b == b';')
        else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix('#')
                .and_then(|num| match num.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => num.parse::<u32>().ok(),
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Screen-settle polling interval. Callers pass their own min wait and
/// timeout because a launch needs longer to settle than a tap.
const SETTLE_POLL_MS: u64 = 150;

/// Settle bounds, beside the function that uses them. They were copied into two
/// callers when the helper moved here, so changing one made the MCP and OODA
/// paths settle differently.
pub const SETTLE_ACTION_MIN_MS: u64 = 150;
pub const SETTLE_ACTION_TIMEOUT_MS: u64 = 2_000;
pub const SETTLE_LAUNCH_MIN_MS: u64 = 800;
pub const SETTLE_LAUNCH_TIMEOUT_MS: u64 = 5_000;

/// Poll screenshots until the screen settles (two consecutive frames match)
/// or `timeout` elapses. Returns the last frame so the caller can reuse it
/// in the next OBSERVE; `None` when a screenshot failed (fail-open: behaves
/// like the old blind sleep and the caller re-fetches fresh).
pub async fn wait_for_screen_stable(
    transport: &dyn DeviceTransport,
    min_wait: std::time::Duration,
    timeout: std::time::Duration,
) -> Option<Vec<u8>> {
    let start = std::time::Instant::now();
    let poll = std::time::Duration::from_millis(SETTLE_POLL_MS);
    tokio::time::sleep(min_wait.min(timeout)).await;
    let mut prev: Option<Vec<u8>> = None;
    loop {
        let shot = match transport.screenshot().await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("settle poll: screenshot failed ({e}); blind-waiting instead");
                tokio::time::sleep(poll).await;
                return None;
            }
        };
        if let Some(p) = &prev {
            if crate::screen::optimize::frames_settled(p, &shot) {
                return Some(shot);
            }
        }
        if start.elapsed() >= timeout {
            tracing::debug!(
                "settle poll: still animating at {}ms cap",
                timeout.as_millis()
            );
            return Some(shot);
        }
        prev = Some(shot);
        tokio::time::sleep(poll).await;
    }
}

/// Get current activity, returning "Unknown" on failure.
pub async fn activity_or_unknown(transport: &dyn DeviceTransport) -> String {
    transport
        .current_activity()
        .await
        .unwrap_or_else(|_| "Unknown".to_string())
}

/// The one rendering of an `app_state` code. Both `drengr_do(action='app_state')`
/// and `drengr_query(question='app_state')` go through here so the two surfaces
/// can never name the same lifecycle state differently.
pub fn app_state_name(state: u8) -> &'static str {
    match state {
        0 => "not_installed",
        1 => "not_running",
        2 => "background_suspended",
        3 => "background",
        4 => "foreground",
        _ => "unrecognized",
    }
}

/// Per-segment duration for the default segmented `draw_path`. Splits the
/// total evenly across segments and floors to ≥1ms so `input swipe` accepts it.
pub fn draw_path_per_segment_ms(total_duration_ms: u32, segments: u32) -> u32 {
    if segments == 0 {
        return 0;
    }
    (total_duration_ms / segments).max(1)
}

/// Duration (ms) for a velocity-controlled swipe, clamped to ≥80ms so iOS
/// UIScrollView still registers momentum on tiny travels.
pub fn swipe_duration_for_velocity(from: Point, to: Point, velocity_pts_per_sec: f32) -> u32 {
    let dx = (to.x - from.x) as f32;
    let dy = (to.y - from.y) as f32;
    let distance = (dx * dx + dy * dy).sqrt();
    let v = velocity_pts_per_sec.max(1.0);
    ((distance / v) * 1000.0).max(80.0) as u32
}

/// Calculate swipe start/end coordinates from direction and screen dimensions.
pub fn swipe_coords(direction: &str, width: u32, height: u32) -> (Point, Point) {
    let cx = width as i32 / 2;
    let cy = height as i32 / 2;
    let margin = height as i32 / 4;

    match direction {
        "up" => (Point::new(cx, cy + margin), Point::new(cx, cy - margin)),
        "down" => (Point::new(cx, cy - margin), Point::new(cx, cy + margin)),
        "left" => (Point::new(cx + margin, cy), Point::new(cx - margin, cy)),
        "right" => (Point::new(cx - margin, cy), Point::new(cx + margin, cy)),
        _ => (Point::new(cx, cy + margin), Point::new(cx, cy - margin)),
    }
}

/// Resolve credentials for a cloud provider from environment variables.
fn cloud_credentials(provider: &appium::CloudProvider) -> (String, String) {
    match provider {
        appium::CloudProvider::BrowserStack => (
            std::env::var("BROWSERSTACK_USERNAME").unwrap_or_default(),
            std::env::var("BROWSERSTACK_ACCESS_KEY").unwrap_or_default(),
        ),
        appium::CloudProvider::SauceLabs { .. } => (
            std::env::var("SAUCE_USERNAME").unwrap_or_default(),
            std::env::var("SAUCE_ACCESS_KEY").unwrap_or_default(),
        ),
        appium::CloudProvider::AwsDeviceFarm => (
            std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_default(),
            std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_default(),
        ),
        appium::CloudProvider::LambdaTest => (
            std::env::var("LAMBDATEST_USERNAME").unwrap_or_default(),
            std::env::var("LAMBDATEST_ACCESS_KEY").unwrap_or_default(),
        ),
        appium::CloudProvider::Perfecto { .. } => (
            "perfecto".to_string(), // Perfecto uses securityToken, not username
            std::env::var("PERFECTO_SECURITY_TOKEN").unwrap_or_default(),
        ),
        appium::CloudProvider::Kobiton => (
            std::env::var("KOBITON_USERNAME").unwrap_or_default(),
            std::env::var("KOBITON_API_KEY").unwrap_or_default(),
        ),
        appium::CloudProvider::Custom { .. } => (
            std::env::var("APPIUM_USERNAME").unwrap_or_default(),
            std::env::var("APPIUM_ACCESS_KEY").unwrap_or_default(),
        ),
    }
}

/// Detect platform from device name. iphone/ipad → iOS, everything else → Android.
fn detect_platform(device_name: &str, _os_version: &str) -> &'static str {
    let name_lower = device_name.to_lowercase();
    if name_lower.contains("iphone") || name_lower.contains("ipad") {
        "ios"
    } else {
        "android"
    }
}

/// Create a cloud device transport via Appium (universal — works with any provider).
/// `app` is optional: BrowserStack/Sauce require an uploaded app URL (e.g., "bs://abc123"),
/// or an env var `BROWSERSTACK_APP_URL` / `SAUCE_APP_URL` can provide a default.
pub async fn create_cloud_transport(
    cloud: &str,
    device_name: &str,
    os_version: &str,
    app: Option<&str>,
) -> Result<Box<dyn DeviceTransport>> {
    let provider = appium::CloudProvider::parse(cloud);
    let (username, access_key) = cloud_credentials(&provider);

    // Custom hub allows empty credentials (local Appium has no auth)
    if !matches!(provider, appium::CloudProvider::Custom { .. })
        && (username.is_empty() || access_key.is_empty())
    {
        anyhow::bail!(
            "{} credentials not set. See drengr.dev/getting-started for setup.",
            provider.name()
        );
    }

    let platform = detect_platform(device_name, os_version);

    // Resolve app URL: explicit param > env var > None
    let app_url = app.map(String::from).or_else(|| {
        std::env::var("BROWSERSTACK_APP_URL")
            .or_else(|_| std::env::var("SAUCE_APP_URL"))
            .or_else(|_| std::env::var("APPIUM_APP"))
            .ok()
    });

    let config = appium::AppiumConfig {
        provider,
        server_url: None,
        username,
        access_key,
        device_name: device_name.to_string(),
        platform: platform.to_string(),
        os_version: os_version.to_string(),
        app: app_url,
    };

    let transport = appium::AppiumTransport::connect(&config).await?;
    Ok(Box::new(transport))
}

/// A single log entry from device logs (Android logcat / iOS os_log).
#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub tag: String,
    pub message: String,
}

/// Android keycode constants.
pub mod keycode {
    pub const HOME: i32 = 3;
    pub const BACK: i32 = 4;
    pub const ENTER: i32 = 66;
    pub const TAB: i32 = 61;
    pub const DELETE: i32 = 67;
    pub const ESCAPE: i32 = 111;
    pub const MOVE_END: i32 = 123;
}

/// Detected device with transport-agnostic metadata.
#[derive(Debug, Clone)]
pub struct DetectedDevice {
    pub id: String,
    pub os: DeviceOs,
    pub model: String,
    pub sdk_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceOs {
    Android,
    Ios,
}

impl std::fmt::Display for DeviceOs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceOs::Android => write!(f, "android"),
            DeviceOs::Ios => write!(f, "ios"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transport that implements only the required methods, so every
    /// capability method below falls through to the trait default.
    struct BareDevice;

    #[async_trait]
    impl DeviceTransport for BareDevice {
        async fn screenshot(&self) -> Result<Vec<u8>> {
            unimplemented!()
        }
        async fn ui_tree(&self) -> Result<Vec<UiElement>> {
            unimplemented!()
        }
        async fn tap(&self, _: i32, _: i32) -> Result<()> {
            unimplemented!()
        }
        async fn long_press(&self, _: i32, _: i32, _: u32) -> Result<()> {
            unimplemented!()
        }
        async fn swipe(&self, _: Point, _: Point, _: u32) -> Result<()> {
            unimplemented!()
        }
        async fn type_text(&self, _: &str) -> Result<()> {
            unimplemented!()
        }
        async fn press_key(&self, _: i32) -> Result<()> {
            unimplemented!()
        }
        async fn launch_app(&self, _: &str) -> Result<()> {
            unimplemented!()
        }
        async fn screen_size(&self) -> Result<(u32, u32)> {
            unimplemented!()
        }
        async fn is_connected(&self) -> bool {
            true
        }
        async fn is_app_in_foreground(&self, _: &str) -> Result<bool> {
            unimplemented!()
        }
        async fn clear_focused_field(&self) -> Result<()> {
            unimplemented!()
        }
        async fn device_info(&self) -> Result<DeviceInfo> {
            unimplemented!()
        }
        async fn current_activity(&self) -> Result<String> {
            unimplemented!()
        }
    }

    /// "I cannot look" must never be reported as "I looked, there is no alert".
    /// The default used to return Ok(None), which an agent reads as "no dialog".
    #[tokio::test]
    async fn alert_text_default_errors_instead_of_claiming_no_alert() {
        let err = BareDevice
            .alert_text()
            .await
            .expect_err("a transport that cannot inspect alerts must not return Ok(None)");
        assert!(err.to_string().contains("not supported"), "got: {err}");
    }

    /// Same defect one method down: Ok(0) looked like a lifecycle answer.
    #[tokio::test]
    async fn app_state_default_errors_instead_of_returning_unknown() {
        let err = BareDevice
            .app_state("com.example.app")
            .await
            .expect_err("a transport that cannot query lifecycle must not return Ok(0)");
        assert!(err.to_string().contains("not supported"), "got: {err}");
    }

    #[test]
    fn app_state_names_cover_every_documented_code() {
        assert_eq!(app_state_name(1), "not_running");
        assert_eq!(app_state_name(2), "background_suspended");
        assert_eq!(app_state_name(3), "background");
        assert_eq!(app_state_name(4), "foreground");
        assert_eq!(app_state_name(0), "not_installed");
        assert_eq!(app_state_name(5), "unrecognized");
    }

    /// Stands in for a Flutter/canvas screen: `uiautomator dump` hangs, so the
    /// tree call times out while the screenshot is perfectly fine.
    struct TreelessDevice {
        screenshot_ok: bool,
    }

    const FAKE_PNG: [u8; 4] = [0x89, b'P', b'N', b'G'];

    #[async_trait]
    impl DeviceTransport for TreelessDevice {
        async fn screenshot(&self) -> Result<Vec<u8>> {
            if self.screenshot_ok {
                Ok(FAKE_PNG.to_vec())
            } else {
                Err(anyhow::anyhow!("device lost"))
            }
        }
        async fn ui_tree(&self) -> Result<Vec<UiElement>> {
            Err(anyhow::anyhow!("adb shell timed out after 10s"))
        }
        async fn tap(&self, _: i32, _: i32) -> Result<()> {
            unimplemented!()
        }
        async fn long_press(&self, _: i32, _: i32, _: u32) -> Result<()> {
            unimplemented!()
        }
        async fn swipe(&self, _: Point, _: Point, _: u32) -> Result<()> {
            unimplemented!()
        }
        async fn type_text(&self, _: &str) -> Result<()> {
            unimplemented!()
        }
        async fn press_key(&self, _: i32) -> Result<()> {
            unimplemented!()
        }
        async fn launch_app(&self, _: &str) -> Result<()> {
            unimplemented!()
        }
        async fn screen_size(&self) -> Result<(u32, u32)> {
            unimplemented!()
        }
        async fn is_connected(&self) -> bool {
            true
        }
        async fn is_app_in_foreground(&self, _: &str) -> Result<bool> {
            unimplemented!()
        }
        async fn clear_focused_field(&self) -> Result<()> {
            unimplemented!()
        }
        async fn device_info(&self) -> Result<DeviceInfo> {
            unimplemented!()
        }
        async fn current_activity(&self) -> Result<String> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn observe_survives_a_hung_ui_tree() {
        // A treeless screen used to fail the whole observation, so drengr_look
        // returned an error instead of the screenshot vision could still read.
        let o = TreelessDevice {
            screenshot_ok: true,
        }
        .observe()
        .await
        .expect("a dead tree must not cost us a good screenshot");
        assert_eq!(o.frame, FAKE_PNG.to_vec());
        assert!(o.elements.is_empty());
        // The caller must be able to tell this from a screen with no elements.
        assert!(
            o.tree_error.is_some(),
            "a swallowed failure is an invisible one"
        );
    }

    #[tokio::test]
    async fn observe_still_fails_without_a_screenshot() {
        // The screenshot IS the observation — losing it stays fatal.
        assert!(TreelessDevice {
            screenshot_ok: false
        }
        .observe()
        .await
        .is_err());
    }

    #[test]
    fn xml_entities_decode_to_real_characters() {
        // The founder's literal case: labels arrived carrying &#10; so anyone
        // matching on element_text had to know to escape.
        assert_eq!(
            decode_xml_entities("A softer way&#10;through change."),
            "A softer way\nthrough change."
        );
        assert_eq!(decode_xml_entities("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(decode_xml_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_xml_entities("&#x41;&#66;"), "AB");
        // Curly quotes were already raw and must survive untouched.
        assert_eq!(decode_xml_entities("TODAY\u{2019}S"), "TODAY\u{2019}S");
        // A stray ampersand is not an entity and must not eat the text.
        assert_eq!(decode_xml_entities("R&D budget"), "R&D budget");
        // These panicked: the 12-byte window was sliced off the str before the
        // search, cutting mid-character. uiautomator always escapes & and newlines,
        // so these are ordinary app labels, not hostile input.
        assert_eq!(decode_xml_entities("促銷&amp;優惠活動"), "促銷&優惠活動");
        assert_eq!(
            decode_xml_entities("感謝&#10;ありがとう"),
            "感謝\nありがとう"
        );
        assert_eq!(
            decode_xml_entities("Über & Größe wählen"),
            "Über & Größe wählen"
        );
        assert_eq!(
            decode_xml_entities("Sale &amp;\u{1f389}\u{1f389} now"),
            "Sale &\u{1f389}\u{1f389} now"
        );
        // Single pass: an escaped entity stays literal instead of decoding twice.
        assert_eq!(decode_xml_entities("&amp;#10;"), "&#10;");
    }

    #[test]
    fn test_device_os_display() {
        assert_eq!(DeviceOs::Android.to_string(), "android");
        assert_eq!(DeviceOs::Ios.to_string(), "ios");
    }

    #[test]
    fn test_keycodes() {
        assert_eq!(keycode::BACK, 4);
        assert_eq!(keycode::HOME, 3);
        assert_eq!(keycode::ENTER, 66);
        assert_eq!(keycode::MOVE_END, 123);
    }

    #[test]
    fn test_parse_bounds_valid() {
        let b = parse_bounds("[100,200][300,400]").unwrap();
        assert_eq!(b.left, 100);
        assert_eq!(b.top, 200);
        assert_eq!(b.right, 300);
        assert_eq!(b.bottom, 400);
    }

    #[test]
    fn test_parse_bounds_origin() {
        let b = parse_bounds("[0,0][1080,2340]").unwrap();
        assert_eq!(b.left, 0);
        assert_eq!(b.right, 1080);
        assert_eq!(b.bottom, 2340);
    }

    #[test]
    fn test_parse_bounds_invalid() {
        assert!(parse_bounds("garbage").is_none());
        assert!(parse_bounds("").is_none());
        assert!(parse_bounds("[0,0]").is_none());
    }

    #[test]
    fn test_extract_attr_found() {
        let tag = r#"<node class="Button" text="Login" bounds="[0,0][100,50]">"#;
        assert_eq!(extract_attr(tag, "class"), Some("Button".to_string()));
        assert_eq!(extract_attr(tag, "text"), Some("Login".to_string()));
        assert_eq!(
            extract_attr(tag, "bounds"),
            Some("[0,0][100,50]".to_string())
        );
    }

    #[test]
    fn test_extract_attr_missing() {
        let tag = r#"<node class="Button">"#;
        assert_eq!(extract_attr(tag, "missing"), None);
    }

    #[test]
    fn test_default_screen_size() {
        assert_eq!(DEFAULT_SCREEN_SIZE, (1080, 2340));
    }

    #[test]
    fn draw_path_default_per_segment_duration() {
        // Even split across segments.
        assert_eq!(draw_path_per_segment_ms(800, 4), 200);
        // Floor: total smaller than segments still yields at least 1ms per seg.
        assert_eq!(draw_path_per_segment_ms(3, 12), 1);
        // Zero segments degenerates to 0 (caller short-circuits before using it).
        assert_eq!(draw_path_per_segment_ms(800, 0), 0);
    }

    #[test]
    fn swipe_with_velocity_clamps_to_80ms_minimum() {
        // Tiny travel at fast velocity → would compute <80ms; clamped to 80.
        let d = swipe_duration_for_velocity(Point::new(0, 0), Point::new(10, 0), 5000.0);
        assert_eq!(d, 80);
        // Long travel at slow velocity → well over 80.
        let d2 = swipe_duration_for_velocity(Point::new(0, 0), Point::new(1000, 0), 1000.0);
        assert_eq!(d2, 1000);
    }
}
