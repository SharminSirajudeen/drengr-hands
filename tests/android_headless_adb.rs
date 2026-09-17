//! End-to-end Android integration smoke. **Gated behind `--features android-e2e`**
//! because it needs a running emulator on `emulator-5554`.
//!
//! Run locally:
//!
//! ```bash
//! emulator @Pixel_6_API_34 -no-window -no-audio &   # or any AVD
//! adb wait-for-device
//! cargo test --features android-e2e --test android_headless_adb -- --nocapture
//! ```
//!
//! Or on CI (see `.github/workflows/android-e2e.yml`).
//!
//! What it proves: drengr's `AdbTransport` can drive a real Android device
//! end-to-end — screenshot capture, accessibility-tree extraction, tap
//! execution, and app launch — with NO APK fixture. Tests against the
//! built-in launcher / Settings app so it works on any emulator image.

#![cfg(feature = "android-e2e")]

use std::time::Duration;

use drengr_hands::transport::adb::AdbTransport;
use drengr_hands::transport::DeviceTransport;

const SETTINGS_PKG: &str = "com.android.settings";

#[tokio::test]
async fn adb_transport_smoke_end_to_end() -> anyhow::Result<()> {
    let device_id =
        std::env::var("DRENGR_TEST_DEVICE").unwrap_or_else(|_| "emulator-5554".to_string());
    eprintln!("[test] using device {}", device_id);

    let transport = AdbTransport::new(&device_id);

    // Connectivity probe — fails fast with a useful error if no emulator is up.
    assert!(
        transport.is_connected().await,
        "no Android device on {} — boot an emulator first",
        device_id,
    );

    // Screen size — proves wm-size pipeline.
    let (w, h) = transport.screen_size().await?;
    eprintln!("[test] screen_size = {}x{}", w, h);
    assert!(w > 0 && h > 0, "screen_size returned zero dimensions");

    // Screenshot — proves the binary capture path (PNG bytes).
    let png = transport.screenshot().await?;
    eprintln!("[test] screenshot bytes = {}", png.len());
    assert!(
        png.len() > 1024,
        "screenshot too small ({} bytes)",
        png.len()
    );
    assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n", "not a PNG header");

    // UI tree on the launcher — proves the uiautomator dump pipeline.
    let initial_tree = transport.ui_tree().await?;
    eprintln!("[test] launcher ui_tree elements = {}", initial_tree.len());
    assert!(
        !initial_tree.is_empty(),
        "ui_tree returned no elements on launcher",
    );

    // Launch Settings — proves package-name validation + am start path.
    transport.launch_app(SETTINGS_PKG).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let activity = transport.current_activity().await.unwrap_or_default();
    eprintln!("[test] post-launch activity = {}", activity);
    assert!(
        activity.starts_with(SETTINGS_PKG),
        "expected Settings foreground, got: {}",
        activity,
    );

    // Settings UI tree should differ from the launcher's.
    let settings_tree = transport.ui_tree().await?;
    eprintln!("[test] settings ui_tree elements = {}", settings_tree.len());
    assert!(
        !settings_tree.is_empty(),
        "ui_tree empty in Settings — accessibility broken?",
    );

    // Tap somewhere harmless — middle of screen. Just proves input pipeline
    // doesn't error; we don't assert what changes (that's app-specific).
    transport.tap((w / 2) as i32, (h / 2) as i32).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Press back to leave Settings clean for any subsequent test runs.
    transport
        .press_key(drengr_hands::transport::keycode::BACK)
        .await?;

    Ok(())
}
