mod client;
mod provider;
mod source;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::prelude::{Engine as _, BASE64_STANDARD};
use serde_json::{json, Value};

use crate::network::events::NetworkEvent;
use crate::screen::ui_element::{DeviceInfo, Point, UiElement};
use crate::transport::{keycode, DeviceTransport, LogEntry};

use client::{assumption, unexpected};

pub use provider::{AppiumConfig, CloudProvider, ProviderConfig, SauceRegion};

/// WebDriver/Appium transport — connects to cloud device farms or local Appium.
/// Works with ANY provider that speaks W3C WebDriver: BrowserStack, Sauce Labs,
/// AWS Device Farm, LambdaTest, Perfecto, Kobiton, or any custom Appium hub.
pub struct AppiumTransport {
    http: reqwest::Client,
    base_url: String,
    /// Session-scoped device identity. See `client::stable_id`.
    stable_id: String,
    session_id: Option<String>,
    device_name: String,
    platform: String,
    os_version: String,
    timeouts: client::Timeouts,
    recording: std::sync::Mutex<Option<String>>,
    log_buffer: std::sync::Mutex<Vec<String>>,
}

/// A WebDriver "no alert open" response, which is a real answer ("nothing is
/// showing"), not a failure to look. Matched on the W3C error code, not on the
/// message: message wording varies by provider, and a substring search for
/// "no alert" reads a sentence like "could not reach the device, no alert
/// state available" as proof that no alert was showing.
fn is_no_alert(e: &anyhow::Error) -> bool {
    e.downcast_ref::<client::WebDriverError>()
        .is_some_and(client::WebDriverError::is_no_such_alert)
}

/// A string that is strictly base64 and decodes to UTF-8 text. XCUITest
/// documents `mobile: getPasteboard` as honouring the encoding it is asked for;
/// a build that ignores that and answers in base64 would otherwise hand the
/// caller an encoded blob as though it were the clipboard's contents.
fn decodes_as_base64_text(s: &str) -> bool {
    if s.len() < 4 || !s.len().is_multiple_of(4) {
        return false;
    }
    let Ok(raw) = BASE64_STANDARD.decode(s) else {
        return false;
    };
    let Ok(text) = String::from_utf8(raw) else {
        return false;
    };
    BASE64_STANDARD.encode(&text) == s
}

fn validated_package(package: &str) -> Result<&str> {
    if !crate::validate::is_valid_package_name(package) {
        anyhow::bail!("Invalid package/bundle id: {}", package);
    }
    Ok(package)
}

#[async_trait]
impl DeviceTransport for AppiumTransport {
    fn platform_kind(&self) -> &'static str {
        "cloud"
    }

    /// Cloud devices reported no identity at all, so every observation they
    /// produced was stamped `""` and `observation_for_device` refused all of
    /// them as unattributable. This stamps something a later check can actually
    /// verify. It is deliberately per-session: `connect` mints a new session
    /// every time, and a new session may be a different physical device, so a
    /// record from the previous one must not resolve against this one.
    fn id(&self) -> &str {
        &self.stable_id
    }

    async fn screenshot(&self) -> Result<Vec<u8>> {
        let resp = self.cmd("GET", "/screenshot", None).await?;
        let b64 = resp["value"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No screenshot data"))?;
        BASE64_STANDARD
            .decode(b64)
            .context("Failed to decode screenshot base64")
    }

    async fn ui_tree(&self) -> Result<Vec<UiElement>> {
        Ok(source::parse_page_source(&self.raw_ui_tree().await?))
    }

    async fn raw_ui_tree(&self) -> Result<String> {
        let resp = self.cmd("GET", "/source", None).await?;
        resp["value"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("Page source response carried no XML"))
    }

    async fn tap(&self, x: i32, y: i32) -> Result<()> {
        self.pointer_gesture(vec![
            json!({"type": "pointerMove", "duration": 0, "x": x, "y": y}),
            json!({"type": "pointerDown", "button": 0}),
            json!({"type": "pause", "duration": 50}),
            json!({"type": "pointerUp", "button": 0}),
        ])
        .await
    }

    async fn long_press(&self, x: i32, y: i32, duration_ms: u32) -> Result<()> {
        self.pointer_gesture(vec![
            json!({"type": "pointerMove", "duration": 0, "x": x, "y": y}),
            json!({"type": "pointerDown", "button": 0}),
            json!({"type": "pause", "duration": duration_ms}),
            json!({"type": "pointerUp", "button": 0}),
        ])
        .await
    }

    async fn swipe(&self, from: Point, to: Point, duration_ms: u32) -> Result<()> {
        self.pointer_gesture(vec![
            json!({"type": "pointerMove", "duration": 0, "x": from.x, "y": from.y}),
            json!({"type": "pointerDown", "button": 0}),
            json!({"type": "pointerMove", "duration": duration_ms, "x": to.x, "y": to.y}),
            json!({"type": "pointerUp", "button": 0}),
        ])
        .await
    }

    /// One continuous stroke — the pen never lifts between points, unlike the
    /// chained-swipe default.
    async fn draw_path(&self, points: &[Point], duration_ms: u32) -> Result<()> {
        if points.len() < 2 {
            return Ok(());
        }
        let per_seg =
            crate::transport::draw_path_per_segment_ms(duration_ms, points.len() as u32 - 1);
        let mut steps = vec![
            json!({"type": "pointerMove", "duration": 0, "x": points[0].x, "y": points[0].y}),
            json!({"type": "pointerDown", "button": 0}),
        ];
        for p in &points[1..] {
            steps.push(json!({"type": "pointerMove", "duration": per_seg, "x": p.x, "y": p.y}));
        }
        steps.push(json!({"type": "pointerUp", "button": 0}));
        self.pointer_gesture(steps).await
    }

    async fn type_text(&self, text: &str) -> Result<()> {
        let elem_id = self.active_element_id().await?;
        self.cmd(
            "POST",
            &format!("/element/{}/value", elem_id),
            Some(json!({"text": text})),
        )
        .await?;
        Ok(())
    }

    async fn press_key(&self, code: i32) -> Result<()> {
        if self.is_android() {
            self.cmd(
                "POST",
                "/appium/device/press_keycode",
                Some(json!({"keycode": code})),
            )
            .await?;
            return Ok(());
        }
        match code {
            keycode::BACK => {
                self.cmd("POST", "/back", Some(json!({}))).await?;
            }
            keycode::HOME => {
                self.execute("mobile: pressButton", json!({"name": "home"})).await?;
            }
            keycode::ENTER => {
                let elem_id = self.active_element_id().await?;
                self.cmd(
                    "POST",
                    &format!("/element/{}/value", elem_id),
                    Some(json!({"text": "\n"})),
                )
                .await?;
            }
            other => anyhow::bail!(
                "Android keycode {} has no XCUITest equivalent; only BACK, HOME and ENTER map to iOS",
                other
            ),
        }
        Ok(())
    }

    async fn launch_app(&self, package: &str) -> Result<()> {
        let body = self.app_id_body(validated_package(package)?);
        self.cmd("POST", "/appium/device/activate_app", Some(body))
            .await?;
        Ok(())
    }

    async fn terminate_app(&self, package: &str) -> Result<()> {
        let body = self.app_id_body(validated_package(package)?);
        let resp = self
            .cmd("POST", "/appium/device/terminate_app", Some(body))
            .await?;
        if resp["value"] == json!(false) {
            anyhow::bail!("{} was not running, so nothing was terminated", package);
        }
        Ok(())
    }

    async fn install_app(&self, path: &str) -> Result<()> {
        if path.is_empty() {
            anyhow::bail!("install_app needs an app path or provider app id");
        }
        // On a cloud hub this is resolved server-side: a provider id (bs://…,
        // storage:…) or a URL the hub can fetch, never a path on this machine.
        self.cmd(
            "POST",
            "/appium/device/install_app",
            Some(json!({"appPath": path})),
        )
        .await?;
        Ok(())
    }

    async fn uninstall_app(&self, package: &str) -> Result<()> {
        let body = self.app_id_body(validated_package(package)?);
        let resp = self
            .cmd("POST", "/appium/device/remove_app", Some(body))
            .await?;
        if resp["value"] == json!(false) {
            anyhow::bail!("{} was not installed, so nothing was removed", package);
        }
        Ok(())
    }

    async fn clear_app_data(&self, package: &str) -> Result<()> {
        let package = validated_package(package)?;
        if !self.is_android() {
            anyhow::bail!(
                "clear_app_data needs UiAutomator2's `mobile: clearApp`. XCUITest has no \
                 data-only clear, so reinstall {} to reset it",
                package
            );
        }
        self.execute("mobile: clearApp", json!({"appId": package}))
            .await?;
        Ok(())
    }

    async fn app_state(&self, package: &str) -> Result<u8> {
        let body = self.app_id_body(validated_package(package)?);
        let resp = self
            .cmd("POST", "/appium/device/app_state", Some(body))
            .await?;
        let state = resp["value"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("app_state returned no numeric state"))?;
        if state > 4 {
            anyhow::bail!("app_state returned an out-of-range value: {}", state);
        }
        Ok(state as u8)
    }

    async fn is_app_in_foreground(&self, package: &str) -> Result<bool> {
        Ok(self.app_state(package).await? == 4)
    }

    async fn list_installed_apps(&self) -> Result<Vec<String>> {
        if !self.is_android() {
            anyhow::bail!(
                "list_installed_apps needs UiAutomator2's `mobile: shell`, gated behind the \
                 server feature `uiautomator2:adb_shell`. XCUITest has no equivalent command \
                 at all, so no iOS session can list installed apps."
            );
        }
        let resp = self
            .execute(
                "mobile: shell",
                json!({"command": "pm", "args": ["list", "packages", "-3"]}),
            )
            .await
            .context(
                "list_installed_apps needs the Appium server feature `uiautomator2:adb_shell`, \
                 which cloud providers leave off by default; ask the provider to enable it or \
                 run against your own hub started with --allow-insecure=uiautomator2:adb_shell",
            )?;
        let out = resp["value"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("mobile: shell returned no stdout"))?;
        Ok(out
            .lines()
            .filter_map(|l| l.trim().strip_prefix("package:"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    async fn screen_size(&self) -> Result<(u32, u32)> {
        let resp = self.cmd("GET", "/window/current/size", None).await?;
        match (
            resp["value"]["width"].as_u64(),
            resp["value"]["height"].as_u64(),
        ) {
            // A guessed screen size puts every derived tap in the wrong place,
            // so an unreadable answer has to be an error, not a default.
            (Some(w), Some(h)) if w > 0 && h > 0 => Ok((w as u32, h as u32)),
            _ => anyhow::bail!("window size response carried no usable width/height"),
        }
    }

    async fn is_connected(&self) -> bool {
        self.session_id.is_some() && self.cmd("GET", "", None).await.is_ok()
    }

    async fn clear_focused_field(&self) -> Result<()> {
        let elem_id = self.active_element_id().await?;
        self.cmd(
            "POST",
            &format!("/element/{}/clear", elem_id),
            Some(json!({})),
        )
        .await?;
        Ok(())
    }

    async fn current_activity(&self) -> Result<String> {
        if self.is_android() {
            let resp = self
                .cmd("GET", "/appium/device/current_activity", None)
                .await?;
            return resp["value"]
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("current_activity returned no value"));
        }
        let resp = self.cmd("GET", "", None).await?;
        resp["value"]["capabilities"]["CFBundleIdentifier"]
            .as_str()
            .or_else(|| resp["value"]["CFBundleIdentifier"].as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("session capabilities carry no CFBundleIdentifier"))
    }

    async fn is_keyboard_visible(&self) -> Result<bool> {
        if self.is_android() {
            let resp = self
                .cmd("GET", "/appium/device/is_keyboard_shown", None)
                .await?;
            return resp["value"]
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("is_keyboard_shown returned no boolean"));
        }
        Ok(self
            .raw_ui_tree()
            .await?
            .contains("XCUIElementTypeKeyboard"))
    }

    async fn dismiss_keyboard(&self) -> Result<()> {
        // No keyboard is a genuine no-op; a keyboard we failed to hide is not.
        if !self.is_keyboard_visible().await? {
            return Ok(());
        }
        self.cmd("POST", "/appium/device/hide_keyboard", Some(json!({})))
            .await?;
        Ok(())
    }

    async fn device_info(&self) -> Result<DeviceInfo> {
        // The same identity `id()` reports. Two ids for one device is how a
        // record written under one and looked up under the other never matches.
        Ok(DeviceInfo {
            id: self.stable_id.clone(),
            os: self.platform.clone(),
            model: self.device_name.clone(),
            sdk_version: Some(self.os_version.clone()),
        })
    }

    async fn open_url(&self, url: &str) -> Result<()> {
        crate::validate::validate_url(url)
            .map_err(|e| anyhow::anyhow!("open_url rejected: {e}"))?;
        self.cmd("POST", "/url", Some(json!({"url": url}))).await?;
        Ok(())
    }

    async fn unlock(&self) -> Result<()> {
        self.cmd("POST", "/appium/device/unlock", Some(json!({})))
            .await
            .with_context(|| {
                assumption(
                    "POST /session/:id/appium/device/unlock",
                    "the driver still serves this JSONWP route; UiAutomator2 has been moving \
                     it to `mobile: unlock`, which requires a `key` and `type` no unlocked \
                     session carries, and XCUITest never served it at all",
                )
            })?;
        Ok(())
    }

    async fn set_orientation(&self, rotation: u8) -> Result<()> {
        // 0=portrait, 1=landscape-left, 2=upside-down, 3=landscape-right,
        // matching adb.rs and simctl.rs. /rotation carries the exact angle;
        // /orientation would collapse both landscapes into one value.
        let z = match rotation {
            0 => 0,
            1 => 90,
            2 => 180,
            3 => 270,
            _ => anyhow::bail!("Invalid rotation: {} (0-3)", rotation),
        };
        self.cmd("POST", "/rotation", Some(json!({"x": 0, "y": 0, "z": z})))
            .await
            .with_context(|| {
                assumption(
                    "POST /session/:id/rotation",
                    "the driver takes an absolute rotation; some XCUITest builds serve only \
                     POST /session/:id/orientation, which cannot tell the two landscapes apart",
                )
            })?;
        Ok(())
    }

    async fn set_appearance(&self, dark: bool) -> Result<()> {
        if self.is_android() {
            self.execute(
                "mobile: setUiMode",
                json!({"mode": "night", "value": if dark { "yes" } else { "no" }}),
            )
            .await?;
        } else {
            self.execute(
                "mobile: setAppearance",
                json!({"style": if dark { "dark" } else { "light" }}),
            )
            .await?;
        }
        Ok(())
    }

    async fn grant_permission(&self, permission: &str, package: &str) -> Result<()> {
        let package = validated_package(package)?;
        if permission.is_empty()
            || !permission
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            anyhow::bail!("Invalid permission name: {}", permission);
        }
        if self.is_android() {
            self.execute(
                "mobile: changePermissions",
                json!({"permissions": permission, "appPackage": package, "action": "grant"}),
            )
            .await?;
        } else {
            self.execute(
                "mobile: setPermission",
                json!({"bundleId": package, "access": {permission: "yes"}}),
            )
            .await
            .context(
                "grant_permission needs XCUITest's `mobile: setPermission`, which the driver \
                 implements for Simulators only; a cloud iOS session is a real device and has \
                 no permission-granting command",
            )?;
        }
        Ok(())
    }

    async fn alert_text(&self) -> Result<Option<String>> {
        let route = "GET /session/:id/alert/text";
        match self.cmd("GET", "/alert/text", None).await {
            Ok(resp) => resp["value"]
                .as_str()
                .map(|s| Some(s.to_string()))
                .ok_or_else(|| unexpected(route, "no string value", "the alert's text")),
            Err(e) if is_no_alert(&e) => Ok(None),
            Err(e) => Err(e).with_context(|| {
                assumption(
                    route,
                    "a driver with no alert showing answers with the W3C code `no such alert`; \
                     a different code here means a live alert may be reported as absent",
                )
            }),
        }
    }

    async fn alert_accept(&self) -> Result<()> {
        self.answer_alert("/alert/accept", "accepted").await
    }

    async fn alert_dismiss(&self) -> Result<()> {
        self.answer_alert("/alert/dismiss", "dismissed").await
    }

    async fn simulate_biometric(&self, matches: bool) -> Result<()> {
        if self.is_android() {
            if !matches {
                anyhow::bail!(
                    "Android fingerprint emulation can only replay an enrolled finger; it cannot simulate a rejected one"
                );
            }
            self.execute("mobile: fingerprint", json!({"fingerprintId": 1}))
                .await?;
        } else {
            self.execute("mobile: touchId", json!({"match": matches}))
                .await
                .context(
                    "simulate_biometric needs XCUITest's `mobile: touchId`, which the driver \
                 implements for Simulators only; a cloud iOS session is a real device and \
                 cannot have a fingerprint injected",
                )?;
        }
        Ok(())
    }

    async fn set_location(&self, lat: f64, lng: f64) -> Result<()> {
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
            anyhow::bail!("Coordinates out of range: {}, {}", lat, lng);
        }
        self.cmd(
            "POST",
            "/location",
            Some(json!({"location": {"latitude": lat, "longitude": lng, "altitude": 0.0}})),
        )
        .await?;
        Ok(())
    }

    async fn pasteboard_get(&self) -> Result<String> {
        if self.is_android() {
            let resp = self.execute("mobile: getClipboard", Value::Null).await?;
            let b64 = resp["value"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("getClipboard returned no content"))?;
            if b64.is_empty() {
                return Ok(String::new());
            }
            let raw = BASE64_STANDARD
                .decode(b64)
                .context("clipboard was not valid base64")?;
            return String::from_utf8(raw).context("clipboard was not valid UTF-8");
        }
        let route = "mobile: getPasteboard";
        let resp = self
            .execute("mobile: getPasteboard", json!({"encoding": "utf8"}))
            .await
            .with_context(|| {
                assumption(
                    route,
                    "the pasteboard is readable over the session; XCUITest documents this \
                     command as Simulator-only, so a cloud real device refuses it",
                )
            })?;
        let text = resp["value"]
            .as_str()
            .ok_or_else(|| unexpected(route, "no string value", "the pasteboard content"))?;
        if decodes_as_base64_text(text) {
            return Err(unexpected(
                route,
                format!(
                    "{} characters that decode cleanly as base64 text",
                    text.len()
                ),
                "plain utf8, the encoding this call asked for; returning it as-is would hand \
                 back an encoded blob as though it were the clipboard",
            ));
        }
        Ok(text.to_string())
    }

    async fn pasteboard_set(&self, text: &str) -> Result<()> {
        if self.is_android() {
            self.execute(
                "mobile: setClipboard",
                json!({"content": BASE64_STANDARD.encode(text), "contentType": "plaintext"}),
            )
            .await?;
        } else {
            self.execute(
                "mobile: setPasteboard",
                json!({"content": text, "encoding": "utf8"}),
            )
            .await?;
        }
        Ok(())
    }

    async fn read_logs(
        &self,
        package: &str,
        filter: Option<&str>,
        lines: usize,
    ) -> Result<Vec<LogEntry>> {
        let package = validated_package(package)?;
        let messages = self.drain_device_log().await?;
        let mut entries = crate::transport::adb::parse_logcat_lines(&messages.join("\n"));
        entries.retain(|e| e.message.contains(package) || e.tag.contains(package));
        if let Some(f) = filter {
            let f = f.to_lowercase();
            entries.retain(|e| {
                e.tag.to_lowercase().contains(&f) || e.message.to_lowercase().contains(&f)
            });
        }
        if entries.len() > lines {
            entries.drain(..entries.len() - lines);
        }
        Ok(entries)
    }

    async fn capture_http_logs(&self) -> Result<Vec<NetworkEvent>> {
        let joined = self.drain_device_log().await?.join("\n");
        Ok(if self.is_android() {
            crate::network::logcat::parse_okhttp_logcat(&joined)
        } else {
            crate::network::logcat::parse_ios_network_log(&joined)
        })
    }

    async fn clear_http_logs(&self) -> Result<()> {
        self.clear_device_log().await
    }

    async fn check_crash_logcat(&self, package: &str) -> Result<bool> {
        if !self.is_android() {
            anyhow::bail!("check_crash_logcat reads logcat, which XCUITest has no equivalent of; death_report diagnoses an iOS exit");
        }
        let package = validated_package(package)?;
        let lines = self
            .drain_device_log()
            .await
            .context("check_crash_logcat could not read the device log")?;
        Ok(lines.iter().any(|l| {
            l.contains(package) && (l.contains("FATAL EXCEPTION") || l.contains("ANR in"))
        }))
    }

    async fn start_recording(&self) -> Result<String> {
        {
            let guard = self
                .recording
                .lock()
                .map_err(|_| anyhow::anyhow!("recording state poisoned"))?;
            if let Some(path) = guard.as_deref() {
                anyhow::bail!("Recording already in progress → {}", path);
            }
        }
        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let dir = crate::paths::drengr_dir_or("/tmp").join("recordings");
        std::fs::create_dir_all(&dir)?;
        let path = dir
            .join(format!(
                "cloud_{}_{}.mp4",
                client::slug(&self.device_name),
                ts
            ))
            .to_string_lossy()
            .to_string();

        self.cmd(
            "POST",
            "/appium/start_recording_screen",
            Some(json!({"options": {"forceRestart": true}})),
        )
        .await
        .with_context(|| {
            assumption(
                "POST /session/:id/appium/start_recording_screen",
                "the provider leaves Appium's own screen recorder enabled; BrowserStack and \
                 Sauce Labs record session video themselves and can refuse this route",
            )
        })?;

        *self
            .recording
            .lock()
            .map_err(|_| anyhow::anyhow!("recording state poisoned"))? = Some(path.clone());
        Ok(path)
    }

    async fn stop_recording(&self) -> Result<String> {
        let path = self
            .recording
            .lock()
            .map_err(|_| anyhow::anyhow!("recording state poisoned"))?
            .take()
            .ok_or_else(|| anyhow::anyhow!("No recording in progress"))?;

        let route = "POST /session/:id/appium/stop_recording_screen";
        let resp = self
            .cmd("POST", "/appium/stop_recording_screen", Some(json!({})))
            .await?;
        let b64 = resp["value"]
            .as_str()
            .ok_or_else(|| unexpected(route, "no string value", "the video as base64"))?;
        if b64.is_empty() {
            return Err(unexpected(route, "an empty payload", "the video as base64"));
        }
        let bytes = BASE64_STANDARD
            .decode(b64)
            .context("recording was not valid base64")?;
        std::fs::write(&path, &bytes).with_context(|| format!("writing recording to {path}"))?;
        Ok(path)
    }

    async fn death_report(&self, package: &str) -> (String, Option<String>) {
        if !self.is_connected().await {
            return ("device_lost".to_string(), None);
        }
        match self.app_state(package).await {
            Ok(4) => ("running".to_string(), None),
            Ok(2) | Ok(3) => ("running".to_string(), Some("backgrounded".to_string())),
            Ok(0) => (
                "unknown".to_string(),
                Some(format!("{package} is not installed")),
            ),
            // Not running. A crash line is the only evidence that says why;
            // without one, "it exited cleanly" would be a guess wearing a
            // reason's clothes.
            Ok(_) => match self.check_crash_logcat(package).await {
                Ok(true) => (
                    "crashed".to_string(),
                    Some(format!("crash or ANR logged for {package}")),
                ),
                Ok(false) => (
                    "unknown".to_string(),
                    Some("not running; no crash line in the device log".to_string()),
                ),
                Err(e) => (
                    "unknown".to_string(),
                    Some(format!(
                        "not running; the device log could not be read: {e}"
                    )),
                ),
            },
            Err(e) => ("unknown".to_string(), Some(e.to_string())),
        }
    }
}
