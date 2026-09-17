use anyhow::Result;

use super::{DetectedDevice, DeviceOs};

/// Detect all connected Android and iOS devices.
pub async fn detect_devices() -> Vec<DetectedDevice> {
    let mut devices = Vec::new();

    // Detect Android devices via ADB
    if let Ok(android) = detect_android().await {
        devices.extend(android);
    }

    // Detect iOS simulators via simctl
    if let Ok(ios) = detect_ios().await {
        devices.extend(ios);
    }

    devices
}

/// Detect Android devices from `adb devices -l` output. No `-s`: this is the
/// call that discovers what could be targeted.
async fn detect_android() -> Result<Vec<DetectedDevice>> {
    let out = super::adb::run_adb(&super::adb::resolve_adb(), None, &["devices", "-l"], 10).await?;
    Ok(parse_adb_devices(&String::from_utf8_lossy(&out)))
}

/// Parse `adb devices -l` output into detected devices.
pub fn parse_adb_devices(output: &str) -> Vec<DetectedDevice> {
    let mut devices = Vec::new();

    for line in output.lines().skip(1) {
        // Skip header
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.contains("device") || trimmed.contains("offline") {
            continue;
        }

        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 2 && parts[1] == "device" {
            let id = parts[0].to_string();
            let model = parts
                .iter()
                .find(|p| p.starts_with("model:"))
                .map(|p| p.trim_start_matches("model:").to_string())
                .unwrap_or_else(|| "Android Device".to_string());

            devices.push(DetectedDevice {
                id,
                os: DeviceOs::Android,
                model,
                sdk_version: None,
            });
        }
    }

    devices
}

/// Detect iOS simulators from `simctl list`.
async fn detect_ios() -> Result<Vec<DetectedDevice>> {
    let xcrun = super::simctl::resolve_xcrun();
    // Bounded like detect_android's run_adb. This sits on the `drengr mcp`
    // startup path, and a wedged CoreSimulator is a real macOS failure mode: an
    // unbounded wait here means the server never reaches its read loop.
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        super::simctl::device_process(&xcrun, ["simctl", "list", "devices", "booted", "-j"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("simctl list devices timed out after 10s"))??;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_booted_ios_devices(&stdout))
}

/// Booted iOS sims + runtime version (which parse_simctl_devices drops).
fn parse_booted_ios_devices(json: &str) -> Vec<DetectedDevice> {
    let parsed: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    if let Some(map) = parsed.get("devices").and_then(|d| d.as_object()) {
        for (runtime, list) in map {
            let version = ios_version_from_runtime_key(runtime);
            if let Some(arr) = list.as_array() {
                for d in arr {
                    if d.get("state").and_then(|s| s.as_str()) != Some("Booted") {
                        continue;
                    }
                    if let Some(udid) = d.get("udid").and_then(|u| u.as_str()) {
                        out.push(DetectedDevice {
                            id: udid.to_string(),
                            os: DeviceOs::Ios,
                            model: d
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string(),
                            sdk_version: version.clone(),
                        });
                    }
                }
            }
        }
    }
    out
}

/// "...SimRuntime.iOS-18-2" -> "18.2". None for non-iOS runtimes.
fn ios_version_from_runtime_key(key: &str) -> Option<String> {
    let tail = key.rsplit('.').next()?.strip_prefix("iOS-")?;
    if tail.is_empty() {
        return None;
    }
    Some(tail.replace('-', "."))
}

/// Auto-select the best device (first available, prefer Android).
/// What the connected set means, decided without doing any I/O so it can be
/// tested. Picking silently when several devices are attached is the failure
/// this exists to prevent: the run succeeds against a device the user never
/// chose and never sees named.
#[derive(Debug)]
pub enum Selection {
    None,
    /// Exactly one device, or `DRENGR_DEVICE` named one that is attached.
    Resolved(DetectedDevice),
    /// Several attached and nothing pinned. The caller must ask or announce.
    Ambiguous(Vec<DetectedDevice>),
    /// `DRENGR_DEVICE` named something that is not attached.
    PinnedButMissing {
        wanted: String,
        available: Vec<String>,
    },
}

pub fn classify(devices: Vec<DetectedDevice>, pinned: Option<String>) -> Selection {
    if devices.is_empty() {
        return Selection::None;
    }
    if let Some(want) = pinned.filter(|w| !w.trim().is_empty()) {
        let want = want.trim();
        return match devices.iter().position(|d| d.id == want) {
            Some(i) => {
                Selection::Resolved(devices.into_iter().nth(i).expect("index from position"))
            }
            None => Selection::PinnedButMissing {
                wanted: want.to_string(),
                available: devices.into_iter().map(|d| d.id).collect(),
            },
        };
    }
    if devices.len() == 1 {
        return Selection::Resolved(devices.into_iter().next().expect("len checked"));
    }
    Selection::Ambiguous(devices)
}

/// True only when a person is actually watching and can answer. MCP runs with
/// stdin as the JSON-RPC pipe, so reading a line there would corrupt the
/// transport; a pipe is not a terminal, which is exactly what this checks.
fn a_human_is_present() -> bool {
    use std::io::IsTerminal;
    if std::env::var_os("CI").is_some() || std::env::var_os("GITHUB_ACTIONS").is_some() {
        return false;
    }
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn describe(d: &DetectedDevice) -> String {
    format!("{} — {} ({:?})", d.id, d.model, d.os)
}

/// Prompt on stderr, never stdout: stdout is the MCP transport and the
/// machine-readable test output.
fn ask_which(devices: &[DetectedDevice]) -> Option<usize> {
    use std::io::Write;
    let mut err = std::io::stderr();
    let _ = writeln!(err, "\n{} devices are connected:\n", devices.len());
    for (i, d) in devices.iter().enumerate() {
        let _ = writeln!(err, "  {}) {}", i + 1, describe(d));
    }
    let _ = write!(err, "\nWhich one? [1-{}] ", devices.len());
    let _ = err.flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return None;
    }
    let n: usize = line.trim().parse().ok()?;
    (1..=devices.len()).contains(&n).then(|| n - 1)
}

pub async fn auto_select_device() -> Result<DetectedDevice> {
    let pinned = std::env::var("DRENGR_DEVICE").ok();
    match classify(detect_devices().await, pinned) {
        Selection::Resolved(d) => Ok(d),
        Selection::None => Err(anyhow::anyhow!(
            "No connected devices found. Connect a device or start an emulator."
        )),
        Selection::PinnedButMissing { wanted, available } => Err(anyhow::anyhow!(
            "DRENGR_DEVICE is set to `{wanted}`, which is not connected. Attached: {}",
            available.join(", ")
        )),
        Selection::Ambiguous(devices) => {
            if a_human_is_present() {
                if let Some(i) = ask_which(&devices) {
                    return Ok(devices.into_iter().nth(i).expect("index from ask_which"));
                }
                return Err(anyhow::anyhow!(
                    "No device chosen. Re-run and pick one, or set DRENGR_DEVICE to an id."
                ));
            }
            // Nobody to ask. Still refuse to choose silently: say which one and
            // how to override, so a CI log shows the device the run used.
            let chosen = devices.into_iter().next().expect("len > 1");
            eprintln!(
                "warning: several devices are connected and none is pinned. Using {}. \
                 Set DRENGR_DEVICE to choose.",
                describe(&chosen)
            );
            Ok(chosen)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str) -> DetectedDevice {
        DetectedDevice {
            id: id.to_string(),
            os: DeviceOs::Android,
            model: "test".into(),
            sdk_version: None,
        }
    }

    #[test]
    fn one_device_resolves_without_asking() {
        match classify(vec![dev("emulator-5554")], None) {
            Selection::Resolved(d) => assert_eq!(d.id, "emulator-5554"),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn several_devices_are_ambiguous_never_silently_first() {
        match classify(vec![dev("emulator-5554"), dev("RF8WC0SEPKF")], None) {
            Selection::Ambiguous(v) => assert_eq!(v.len(), 2),
            other => panic!("picking one here is the defect; got {other:?}"),
        }
    }

    #[test]
    fn a_pin_wins_over_ambiguity() {
        let pinned = Some("RF8WC0SEPKF".to_string());
        match classify(vec![dev("emulator-5554"), dev("RF8WC0SEPKF")], pinned) {
            Selection::Resolved(d) => assert_eq!(d.id, "RF8WC0SEPKF"),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn a_pin_naming_an_absent_device_fails_loudly() {
        let pinned = Some("NOT-ATTACHED".to_string());
        match classify(vec![dev("emulator-5554")], pinned) {
            Selection::PinnedButMissing { wanted, available } => {
                assert_eq!(wanted, "NOT-ATTACHED");
                assert_eq!(available, vec!["emulator-5554".to_string()]);
            }
            other => panic!("a pin must never fall back to another device: {other:?}"),
        }
    }

    #[test]
    fn a_blank_pin_is_ignored_rather_than_matched() {
        match classify(vec![dev("emulator-5554")], Some("   ".to_string())) {
            Selection::Resolved(d) => assert_eq!(d.id, "emulator-5554"),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    /// Picking `.next()` off `detect_devices()` is how a run silently targets a
    /// device nobody chose. Every consumer must go through `classify`, which
    /// returns Ambiguous instead of guessing. Fails naming the file.
    #[test]
    fn no_call_site_picks_a_device_off_the_raw_list() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let this = root.join("transport").join("detect.rs");
        let mut offenders = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path == this {
                    continue;
                }
                let flat = std::fs::read_to_string(&path)
                    .unwrap()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                // Any way of reaching into the raw list and taking one:
                // .next(), .find(), .first(), .pop(). The first version of this
                // guard only knew about .next() and missed a real .find() site.
                let bypasses = ["next()", "find(", "first()", "pop()"];
                let reached = flat.split("detect_devices()").skip(1).any(|after| {
                    let head: String = after.chars().take(60).collect();
                    bypasses.iter().any(|b| head.contains(b))
                });
                if reached {
                    offenders.push(path.display().to_string());
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these pick a device off the raw list instead of classify(), so they \
             can target a device the user never chose: {offenders:?}"
        );
    }

    #[test]
    fn no_devices_is_none() {
        assert!(matches!(classify(vec![], None), Selection::None));
    }

    #[test]
    fn test_parse_adb_devices_single() {
        let output = "List of devices attached\nemulator-5554          device product:sdk_gphone64_arm64 model:sdk_gphone64_arm64 device:emulator64_arm64 transport_id:1\n\n";
        let devices = parse_adb_devices(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "emulator-5554");
        assert_eq!(devices[0].os, DeviceOs::Android);
        assert_eq!(devices[0].model, "sdk_gphone64_arm64");
    }

    #[test]
    fn test_parse_adb_devices_multiple() {
        let output = "List of devices attached\nemulator-5554          device model:Pixel_7\nR5CT300ABCD            device model:SM-S911B\n\n";
        let devices = parse_adb_devices(output);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].id, "emulator-5554");
        assert_eq!(devices[1].id, "R5CT300ABCD");
    }

    #[test]
    fn test_parse_adb_devices_offline() {
        let output = "List of devices attached\nemulator-5554          offline\n\n";
        let devices = parse_adb_devices(output);
        assert!(devices.is_empty());
    }

    #[test]
    fn test_parse_adb_devices_empty() {
        let output = "List of devices attached\n\n";
        let devices = parse_adb_devices(output);
        assert!(devices.is_empty());
    }

    #[test]
    fn test_parse_adb_devices_no_model() {
        let output = "List of devices attached\nemulator-5554          device\n\n";
        let devices = parse_adb_devices(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].model, "Android Device");
    }

    #[test]
    fn test_ios_version_from_runtime_key() {
        assert_eq!(
            ios_version_from_runtime_key("com.apple.CoreSimulator.SimRuntime.iOS-18-2").as_deref(),
            Some("18.2")
        );
        assert_eq!(
            ios_version_from_runtime_key("com.apple.CoreSimulator.SimRuntime.iOS-17").as_deref(),
            Some("17")
        );
        assert_eq!(
            ios_version_from_runtime_key("com.apple.CoreSimulator.SimRuntime.watchOS-11-0"),
            None
        );
    }

    #[test]
    fn test_parse_booted_ios_devices_populates_version() {
        let json = r#"{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-18-2":[
            {"udid":"ABC","name":"iPhone 17 Pro","state":"Booted"},
            {"udid":"DEF","name":"iPhone 15","state":"Shutdown"}]}}"#;
        let devices = parse_booted_ios_devices(json);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "ABC");
        assert_eq!(devices[0].model, "iPhone 17 Pro");
        assert_eq!(devices[0].sdk_version.as_deref(), Some("18.2"));
    }
}
