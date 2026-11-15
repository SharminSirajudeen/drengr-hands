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

/// Both tests drive the same emulator, and cargo runs tests in parallel.
static DEVICE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn device_id() -> String {
    std::env::var("DRENGR_TEST_DEVICE").unwrap_or_else(|_| "emulator-5554".to_string())
}

#[tokio::test]
async fn adb_transport_smoke_end_to_end() -> anyhow::Result<()> {
    let _device = DEVICE.lock().await;
    let device_id = device_id();
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

const TYPING_SEED: u64 = 0x7E57_7E87;
/// Mirrors `PASTE_THRESHOLD` in `AdbTransport::type_text`: longer text is pasted.
const PASTE_THRESHOLD: usize = 16;

/// The four hypothesis strings on both paths, then generated ones from a fixed
/// seed. No leading or trailing space: the UI tree reports text trimmed.
fn typing_cases() -> Vec<String> {
    use proptest::prelude::*;
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, RngSeed, TestRunner};

    let pool: Vec<char> = (' '..='~')
        .chain("مرحبا".chars())
        .chain(['😀', '🚀'])
        .collect();
    let short =
        proptest::collection::vec(proptest::sample::select(pool.clone()), 1..=PASTE_THRESHOLD);
    let long = proptest::collection::vec(proptest::sample::select(pool), PASTE_THRESHOLD + 1..=40);
    let text =
        prop_oneof![short, long].prop_map(|v| v.into_iter().collect::<String>().trim().to_string());
    let mut runner = TestRunner::new(Config {
        rng_seed: RngSeed::Fixed(TYPING_SEED),
        ..Config::default()
    });
    let mut cases: Vec<String> = [
        "50%",
        "a%sb",
        "don't",
        "a;b|$(c)",
        "it's 50% off; a&b | $(id) %s ok",
        "مرحبا",
        "hi 😀",
    ]
    .map(String::from)
    .into();
    while cases.len() < 50 {
        let t = text.new_tree(&mut runner).expect("generate").current();
        if !t.is_empty() {
            cases.push(t);
        }
    }
    cases
}

fn char_class(s: &str) -> &'static str {
    let arabic = s.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c));
    let emoji = s.chars().any(|c| c > '\u{FFFF}');
    match (arabic, emoji) {
        (false, false) => "ascii",
        (true, false) => "arabic",
        (false, true) => "emoji",
        (true, true) => "arabic+emoji",
    }
}

fn describe(tree: &[drengr_hands::screen::ui_element::UiElement]) -> String {
    tree.iter()
        .map(|e| {
            format!(
                "{} text={:?} id={:?} focused={}",
                e.class, e.text, e.resource_id, e.focused
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text of the focused text field, or an error carrying the whole tree.
async fn focused_field_text(t: &AdbTransport) -> anyhow::Result<String> {
    let tree = t.ui_tree().await?;
    match tree.iter().find(|e| e.editable && e.focused) {
        Some(e) => Ok(e.text.clone()),
        None => anyhow::bail!("no focused text field:\n{}", describe(&tree)),
    }
}

/// Opens Settings' search box and focuses it. Needs no installed fixture.
async fn open_settings_search(t: &AdbTransport) -> anyhow::Result<()> {
    t.launch_app(SETTINGS_PKG).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    for _ in 0..10 {
        // A dump can fail while the app is still drawing; the next try decides.
        let tree = t.ui_tree().await.unwrap_or_default();
        if tree.iter().any(|e| e.editable && e.focused) {
            return Ok(());
        }
        let target = tree.iter().find(|e| e.editable).or_else(|| {
            tree.iter().find(|e| {
                let label =
                    format!("{} {} {}", e.text, e.content_desc, e.resource_id).to_lowercase();
                label.contains("search")
            })
        });
        if let Some(e) = target {
            t.tap(e.bounds.center_x(), e.bounds.center_y()).await?;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!(
        "Settings search never focused:\n{}",
        describe(&t.ui_tree().await?)
    )
}

/// Types each case through `type_text` into the Settings search box and reads
/// it back from the UI tree. Runs every case, then fails once with all mismatches.
#[tokio::test]
async fn typed_text_arrives_unchanged() -> anyhow::Result<()> {
    let _device = DEVICE.lock().await;
    let t = AdbTransport::new(device_id());
    let adb_version = std::process::Command::new(drengr_hands::transport::adb::resolve_adb())
        .arg("version")
        .output()?;
    eprintln!("[typing] seed = {TYPING_SEED:#x}");
    eprintln!(
        "[typing] {}",
        String::from_utf8_lossy(&adb_version.stdout).replace('\n', " | ")
    );

    open_settings_search(&t).await?;
    t.clear_focused_field().await?;
    let empty = focused_field_text(&t).await?;
    eprintln!("[typing] empty field reads {empty:?}");

    let mut tally: std::collections::BTreeMap<(&str, &str), (u32, u32)> = Default::default();
    let mut failures = Vec::new();
    let mut reopened = 0;
    for (i, text) in typing_cases().iter().enumerate() {
        let path = if text.chars().count() > PASTE_THRESHOLD {
            "paste"
        } else {
            "input"
        };
        // Settings search closed by itself once on CI, between cases; reopen
        // it so one lost screen does not hide the remaining cases.
        if focused_field_text(&t).await.is_err() {
            reopened += 1;
            eprintln!("[typing] search field lost before #{i:02}; reopening");
            open_settings_search(&t).await?;
        }
        t.clear_focused_field().await?;
        let before = focused_field_text(&t).await?;
        let outcome = if before != empty {
            Err(format!("field not cleared, reads {before:?}"))
        } else {
            match t.type_text(text).await {
                Err(e) => Err(format!("type_text error: {e}")),
                Ok(()) => {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    let got = focused_field_text(&t).await?;
                    if got == *text {
                        Ok(())
                    } else {
                        Err(format!("got {got:?}"))
                    }
                }
            }
        };
        let entry = tally.entry((path, char_class(text))).or_default();
        entry.1 += 1;
        match &outcome {
            Ok(()) => entry.0 += 1,
            Err(why) => {
                if let Ok(raw) = t.raw_ui_tree().await {
                    let node = raw
                        .split("<node ")
                        .find(|n| n.contains("focused=\"true\"") && n.contains("EditText"));
                    eprintln!(
                        "[typing]      raw node: {}",
                        node.unwrap_or("<none>").trim()
                    );
                }
                failures.push(format!("#{i:02} {path} sent {text:?}: {why}"));
            }
        }
        eprintln!(
            "[typing] #{i:02} {path:5} {:4} sent {text:?} {}",
            if outcome.is_ok() { "ok" } else { "FAIL" },
            outcome.err().unwrap_or_default()
        );
    }

    eprintln!("[typing] search reopened {reopened} times");
    eprintln!("[typing] summary (path, class): passed/total");
    for ((path, class), (pass, total)) in &tally {
        eprintln!("[typing]   {path:5} {class:12} {pass}/{total}");
    }
    for _ in 0..2 {
        t.press_key(drengr_hands::transport::keycode::BACK).await?;
    }
    assert!(
        failures.is_empty(),
        "{} typed strings arrived changed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}
