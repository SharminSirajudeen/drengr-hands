//! Auto-boot helpers for local Android emulators and iOS simulators.
//! Pre-transport by design — these are the *only* place in the crate where
//! external binaries are spawned outside `DeviceTransport` (boot has to run
//! before a transport exists). Keep that exception narrow.

use std::process::Stdio;
use std::time::{Duration, Instant};

use super::android_sdk::sdk_root;
use anyhow::{anyhow, Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::sleep;

use super::adb::resolve_adb;

/// Resolve the `emulator` binary path. Mirrors `resolve_adb` / `resolve_xcrun`
/// so the boot path is testable and respects `ANDROID_HOME` like the rest of
/// the codebase. Priority: `DRENGR_EMULATOR_PATH` > `$ANDROID_HOME/emulator/emulator` > `"emulator"` on PATH.
pub fn resolve_emulator() -> String {
    if let Ok(p) = std::env::var("DRENGR_EMULATOR_PATH") {
        if !p.is_empty() {
            return p;
        }
    }
    if let Some(sdk) = sdk_root() {
        let candidate = sdk.join("emulator").join("emulator");
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "emulator".to_string()
}

/// How long we wait for `boot_completed` before giving up.
/// Cold-start an x86_64 emulator on a slow Mac can take ~90s; pad to 120s.
const ANDROID_BOOT_TIMEOUT: Duration = Duration::from_secs(120);
/// iOS sim boots much faster but cold runtimes can stall; 60s covers the worst.
const IOS_BOOT_TIMEOUT: Duration = Duration::from_secs(60);

/// Outcome of a boot call. `started_by_us` lets the caller decide whether to
/// shut the device down at the end of the run (CI tear-down) or leave it
/// (already-booted local dev sims).
#[derive(Debug, Clone)]
pub struct BootedDevice {
    pub id: String,
    pub started_by_us: bool,
}

/// Emulator CLI args. `-no-window` is the only headless-vs-watch difference;
/// the rest enforce a clean, deterministic boot and stay on in both modes.
fn android_boot_args(avd: &str, headless: bool) -> Vec<&str> {
    let mut args = vec!["-avd", avd];
    if headless {
        args.push("-no-window");
    }
    args.extend([
        "-no-snapshot",
        "-no-audio",
        "-no-boot-anim",
        "-gpu",
        "swiftshader_indirect",
        "-accel",
        "auto",
    ]);
    args
}

/// `cmdline-tools` installs under a version directory, and the name of that
/// directory differs between a local SDK and a CI image, so it is discovered
/// rather than assumed.
fn cmdline_tool(name: &str) -> Option<std::path::PathBuf> {
    let base = sdk_root()?.join("cmdline-tools");
    let mut dirs: Vec<_> = std::fs::read_dir(&base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .map(|d| d.join("bin").join(name))
        .find(|p| p.exists())
}

fn default_system_image() -> String {
    std::env::var("DRENGR_ANDROID_IMAGE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "system-images;android-34;google_apis;x86_64".to_string())
}

/// Create one AVD. `avdmanager` asks whether to define a custom hardware
/// profile and blocks on the answer, so stdin is answered rather than closed:
/// a closed stdin leaves it waiting until the caller times out.
async fn create_avd(name: &str) -> Result<()> {
    let sdkmanager = cmdline_tool("sdkmanager").ok_or_else(|| {
        anyhow!(
            "no Android cmdline-tools found under {:?}; install them from the SDK manager",
            sdk_root()
        )
    })?;
    let avdmanager = cmdline_tool("avdmanager")
        .ok_or_else(|| anyhow!("no avdmanager found under {:?}", sdk_root()))?;
    let image = default_system_image();

    eprintln!(
        "   No emulator configured. Installing {} (one time, this is a large download)...",
        image
    );
    let install = Command::new(&sdkmanager)
        .arg(&image)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to run sdkmanager")?;
    if !install.status.success() {
        return Err(anyhow!(
            "could not install {}: {}",
            image,
            String::from_utf8_lossy(&install.stderr).trim()
        ));
    }

    let mut child = Command::new(&avdmanager)
        .args(["create", "avd", "-n", name, "-k", &image, "--force"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run avdmanager")?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"no\n").await;
    }
    let out = child
        .wait_with_output()
        .await
        .context("avdmanager did not finish")?;
    if !out.status.success() {
        return Err(anyhow!(
            "could not create an emulator: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    eprintln!("   Created emulator `{}`.", name);
    Ok(())
}

/// The AVD to boot: the first one that exists, else one created here. A machine
/// with an Android SDK and no AVD is the default state after an SDK install, so
/// failing there sends every new user to the docs for no reason.
async fn ensure_avd() -> Result<String> {
    if let Some(existing) = first_avd().await {
        return Ok(existing);
    }
    create_avd("drengr").await?;
    first_avd()
        .await
        .ok_or_else(|| anyhow!("created an emulator but `emulator -list-avds` still reports none"))
}

/// Boot a local Android emulator. `headless=true` adds `-no-window` (no host
/// surface — the CI/agent default); `false` opens the emulator window so a
/// human can watch gestures. If `avd_name` is `None`, picks the first AVD from
/// `emulator -list-avds`.
///
/// Returns the device serial (e.g. `emulator-5554`) once `sys.boot_completed`
/// is `1`. Caller is responsible for shutdown via `adb -s <id> emu kill`.
pub async fn boot_android(avd_name: Option<&str>, headless: bool) -> Result<BootedDevice> {
    let avd = match avd_name {
        Some(n) => n.to_string(),
        None => ensure_avd().await?,
    };

    // Pre-flight serial inventory excludes `offline` devices: a stale offline
    // serial that flips to `device` mid-boot would otherwise look "new" and
    // we'd hand back the wrong id.
    let pre_serials = adb_device_serials().await.unwrap_or_default();

    let mut child = Command::new(resolve_emulator())
        .args(android_boot_args(&avd, headless))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "failed to spawn emulator for AVD `{}` — ensure ANDROID_HOME/emulator is on PATH",
                avd
            )
        })?;

    // Guard pattern: any error after spawn must kill the orphan or the AVD
    // file lock blocks the next boot attempt. mem::forget on success leaks
    // the handle so the OS process outlives this function.
    let new_serial = match wait_for_new_emulator(&pre_serials, ANDROID_BOOT_TIMEOUT, &avd).await {
        Ok(s) => s,
        Err(e) => {
            let _ = child.kill().await;
            return Err(e);
        }
    };
    if let Err(e) = wait_for_boot_completed(&new_serial, ANDROID_BOOT_TIMEOUT).await {
        let _ = child.kill().await;
        return Err(e);
    }

    // Tokio's `Child` drop does NOT kill the OS process by default — letting
    // the handle drop here is what we want: the emulator keeps running, but
    // the parent's pidfd / stdio FDs are released. `mem::forget` would leak
    // those across repeated auto-boots in a long-lived MCP server.
    drop(child);
    Ok(BootedDevice {
        id: new_serial,
        started_by_us: true,
    })
}

/// Boot an iOS simulator. If `udid` is `None`, picks the first shutdown sim
/// reported by `xcrun simctl list devices`. Skips Simulator.app launch — runs
/// "headless" on macOS (the sim still renders, but no host window opens),
/// matching how GitHub Actions macOS runners drive simctl.
pub async fn boot_ios_simulator(udid: Option<&str>) -> Result<BootedDevice> {
    if let Some(existing) = first_booted_ios().await {
        if udid.is_none_or(|want| want == existing) {
            return Ok(BootedDevice {
                id: existing,
                started_by_us: false,
            });
        }
    }

    let target = match udid {
        Some(u) => u.to_string(),
        None => first_shutdown_ios()
            .await
            .context("no shutdown iOS simulators — create one in Xcode > Devices first")?,
    };

    let status = Command::new("xcrun")
        .args(["simctl", "boot", &target])
        .status()
        .await
        .context("failed to spawn `xcrun simctl boot`")?;

    // exit 149 (`Unable to boot device in current state: Booted`) is benign —
    // means another caller booted the same UDID first. Treat as success.
    if !status.success() && status.code() != Some(149) {
        return Err(anyhow!("simctl boot exited {}", status));
    }

    wait_for_ios_booted(&target, IOS_BOOT_TIMEOUT).await?;
    Ok(BootedDevice {
        id: target,
        started_by_us: true,
    })
}

/// `emulator -list-avds` → first non-empty line.
async fn first_avd() -> Option<String> {
    let out = Command::new(resolve_emulator())
        .arg("-list-avds")
        .output()
        .await
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Booted Android serials currently in `device` state. `offline` is filtered
/// out — a stale offline serial that races to `device` mid-boot would look
/// new to `wait_for_new_emulator` and we'd return the wrong id.
async fn adb_device_serials() -> Result<Vec<String>> {
    let adb = resolve_adb();
    let out = crate::transport::adb::run_adb(&adb, None, &["devices"], 10).await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let serial = parts.next()?;
            let state = parts.next()?;
            (state == "device").then(|| serial.to_string())
        })
        .collect())
}

/// Poll `adb devices` until a serial appears that wasn't in `pre`. `avd` is
/// only used for the timeout error message — operators staring at a stalled
/// boot need to know *which* AVD wedged.
async fn wait_for_new_emulator(pre: &[String], deadline: Duration, avd: &str) -> Result<String> {
    let start = Instant::now();
    loop {
        let now = adb_device_serials().await.unwrap_or_default();
        if let Some(new) = now.into_iter().find(|s| !pre.contains(s)) {
            return Ok(new);
        }
        if start.elapsed() > deadline {
            return Err(anyhow!(
                "AVD `{}` did not appear in `adb devices` within {:?}",
                avd,
                deadline
            ));
        }
        sleep(Duration::from_millis(750)).await;
    }
}

/// Poll `getprop sys.boot_completed` until it's `1` or we time out.
async fn wait_for_boot_completed(serial: &str, deadline: Duration) -> Result<()> {
    let adb = resolve_adb();
    let start = Instant::now();
    loop {
        let out = crate::transport::adb::run_adb(
            &adb,
            Some(serial),
            &["shell", "getprop", "sys.boot_completed"],
            10,
        )
        .await;
        if let Ok(o) = out {
            if String::from_utf8_lossy(&o).trim() == "1" {
                return Ok(());
            }
        }
        if start.elapsed() > deadline {
            return Err(anyhow!(
                "android emulator {} did not finish booting within {:?}",
                serial,
                deadline
            ));
        }
        sleep(Duration::from_millis(750)).await;
    }
}

async fn first_booted_ios() -> Option<String> {
    pick_ios_sim(IosSimFilter::Booted).await
}

async fn first_shutdown_ios() -> Option<String> {
    pick_ios_sim(IosSimFilter::Shutdown).await
}

#[derive(Clone, Copy)]
enum IosSimFilter {
    Booted,
    Shutdown,
}

/// Pick the first iOS sim matching `filter` AND whose runtime is available.
/// The `isAvailable` gate matters: a sim with a missing runtime appears in
/// `simctl list devices` and `simctl boot` will fail opaquely. Use the JSON
/// output (`-j`) so we can read the flag rather than guess from text shape.
async fn pick_ios_sim(filter: IosSimFilter) -> Option<String> {
    let out = Command::new("xcrun")
        .args(["simctl", "list", "-j", "devices"])
        .output()
        .await
        .ok()?;
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let want_state = match filter {
        IosSimFilter::Booted => "Booted",
        IosSimFilter::Shutdown => "Shutdown",
    };
    pick_ios_sim_from_json(&parsed, want_state)
}

async fn wait_for_ios_booted(udid: &str, deadline: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        if let Some(current) = pick_ios_sim(IosSimFilter::Booted).await {
            if current == udid {
                return Ok(());
            }
        }
        if start.elapsed() > deadline {
            return Err(anyhow!(
                "ios sim {} did not boot within {:?}",
                udid,
                deadline
            ));
        }
        sleep(Duration::from_millis(500)).await;
    }
}

/// Pick the first iOS sim of `state` from a parsed `simctl list -j devices`
/// JSON document, requiring `isAvailable=true`. Pulled out so the predicate
/// is unit-testable without spawning a process.
fn pick_ios_sim_from_json(parsed: &serde_json::Value, state: &str) -> Option<String> {
    let devices = parsed.get("devices")?.as_object()?;
    for (_runtime, list) in devices {
        for d in list.as_array()? {
            let available = d
                .get("isAvailable")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let st = d.get("state").and_then(|x| x.as_str()).unwrap_or("");
            if available && st == state {
                if let Some(udid) = d.get("udid").and_then(|x| x.as_str()) {
                    return Some(udid.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_boot_args_toggles_window() {
        let headless = android_boot_args("Pixel_7", true);
        let watch = android_boot_args("Pixel_7", false);
        assert!(headless.contains(&"-no-window"));
        assert!(!watch.contains(&"-no-window"));
        // Determinism flags and AVD selection are mode-independent.
        for set in [&headless, &watch] {
            assert_eq!(set[0], "-avd");
            assert_eq!(set[1], "Pixel_7");
            assert!(set.contains(&"-no-snapshot"));
            assert!(set.contains(&"-no-audio"));
            assert!(set.contains(&"swiftshader_indirect"));
        }
    }

    #[test]
    fn picks_first_available_with_state() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "devices": {
                "com.apple.CoreSimulator.SimRuntime.iOS-18-5": [
                    {"udid": "AAAA1234-AAAA-AAAA-AAAA-AAAAAAAAAAAA", "state": "Shutdown", "isAvailable": true,  "name": "iPhone 15"},
                    {"udid": "BBBB1234-BBBB-BBBB-BBBB-BBBBBBBBBBBB", "state": "Booted",   "isAvailable": true,  "name": "iPhone 16"}
                ]
            }
        }"#).unwrap();
        assert_eq!(
            pick_ios_sim_from_json(&json, "Shutdown").as_deref(),
            Some("AAAA1234-AAAA-AAAA-AAAA-AAAAAAAAAAAA")
        );
        assert_eq!(
            pick_ios_sim_from_json(&json, "Booted").as_deref(),
            Some("BBBB1234-BBBB-BBBB-BBBB-BBBBBBBBBBBB")
        );
    }

    #[test]
    fn skips_unavailable_runtimes() {
        // Sim with isAvailable=false (missing runtime download) must never be
        // returned — `simctl boot` would fail opaquely.
        let json: serde_json::Value = serde_json::from_str(r#"{
            "devices": {
                "com.apple.CoreSimulator.SimRuntime.iOS-99-0": [
                    {"udid": "AAAA1234-AAAA-AAAA-AAAA-AAAAAAAAAAAA", "state": "Shutdown", "isAvailable": false, "name": "iPhone 99"}
                ],
                "com.apple.CoreSimulator.SimRuntime.iOS-18-5": [
                    {"udid": "BBBB1234-BBBB-BBBB-BBBB-BBBBBBBBBBBB", "state": "Shutdown", "isAvailable": true,  "name": "iPhone 16"}
                ]
            }
        }"#).unwrap();
        assert_eq!(
            pick_ios_sim_from_json(&json, "Shutdown").as_deref(),
            Some("BBBB1234-BBBB-BBBB-BBBB-BBBBBBBBBBBB")
        );
    }

    #[test]
    fn returns_none_when_no_match() {
        let json: serde_json::Value = serde_json::from_str(r#"{
            "devices": {
                "com.apple.CoreSimulator.SimRuntime.iOS-18-5": [
                    {"udid": "AAAA1234-AAAA-AAAA-AAAA-AAAAAAAAAAAA", "state": "Shutdown", "isAvailable": true, "name": "iPhone 15"}
                ]
            }
        }"#).unwrap();
        assert!(pick_ios_sim_from_json(&json, "Booted").is_none());
    }

    #[test]
    fn system_image_defaults_and_is_overridable() {
        std::env::remove_var("DRENGR_ANDROID_IMAGE");
        assert!(default_system_image().starts_with("system-images;"));
        std::env::set_var(
            "DRENGR_ANDROID_IMAGE",
            "system-images;android-35;google_apis;arm64-v8a",
        );
        assert_eq!(
            default_system_image(),
            "system-images;android-35;google_apis;arm64-v8a"
        );
        std::env::set_var("DRENGR_ANDROID_IMAGE", "");
        assert!(default_system_image().starts_with("system-images;"));
        std::env::remove_var("DRENGR_ANDROID_IMAGE");
    }

    #[test]
    fn cmdline_tool_is_discovered_under_a_versioned_directory() {
        let tmp = std::env::temp_dir().join(format!("drengr-sdk-{}", std::process::id()));
        let bin = tmp.join("cmdline-tools").join("19.0").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("avdmanager"), b"#!/bin/sh\n").unwrap();
        std::env::set_var("ANDROID_SDK_ROOT", &tmp);

        let found = cmdline_tool("avdmanager").expect("must find the tool under any version dir");
        assert!(found.ends_with("19.0/bin/avdmanager"), "got {:?}", found);
        assert!(
            cmdline_tool("sdkmanager").is_none(),
            "a missing tool must be None, never a guess"
        );

        std::env::remove_var("ANDROID_SDK_ROOT");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_devices_key_is_none() {
        let json: serde_json::Value = serde_json::from_str("{}").unwrap();
        assert!(pick_ios_sim_from_json(&json, "Booted").is_none());
    }
}
