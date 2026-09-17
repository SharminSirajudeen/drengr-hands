//! Proves the capture -> parse -> assert chain against a real Android device.
//!
//! Everything below `expect_network` was unit-tested against synthetic entries.
//! This test uses none. It launches a real app on a real device, reads the real
//! logcat the app produced, parses it with the real parser, and evaluates real
//! expectations against it. A pass here means a person can point Drengr at a
//! device and have it fail a build when a call stops firing.
//!
//! Requires a booted Android device with `dev.drengr.demoshop.android`
//! installed. Ignored by default because it needs hardware:
//!
//!   cargo test --test expect_network_device -- --ignored --nocapture

use drengr_hands::expect_network::{evaluate, ExpectNetwork, ExpectOutcome};
use drengr_hands::network::sink::{NetworkSink, NetworkSource};
use drengr_hands::transport::adb::AdbTransport;
use drengr_hands::transport::DeviceTransport;

const APP: &str = "dev.drengr.demoshop.android";

fn expect(url: &str) -> ExpectNetwork {
    ExpectNetwork {
        url: url.to_string(),
        method: None,
        status: None,
        body_includes: None,
        times: None,
    }
}

#[tokio::test]
#[ignore = "requires a booted Android device with the demo shop installed"]
async fn expectations_resolve_against_real_device_traffic() -> anyhow::Result<()> {
    let device_id =
        std::env::var("DRENGR_TEST_DEVICE").unwrap_or_else(|_| "emulator-5554".to_string());
    let transport = AdbTransport::new(&device_id);

    // Setup goes through adb directly rather than the transport's private
    // shell(), so this test needs no change to transport's API surface.
    let adb = |args: &[&str]| {
        std::process::Command::new("adb")
            .args(["-s", &device_id])
            .args(args)
            .output()
            .expect("adb must be on PATH")
    };
    // Force-stop first: monkey on an already-foregrounded app just raises it,
    // no new fetch fires, and the capture window comes back empty.
    adb(&["shell", "am", "force-stop", APP]);
    adb(&["logcat", "-c"]);
    adb(&[
        "shell",
        "monkey",
        "-p",
        APP,
        "-c",
        "android.intent.category.LAUNCHER",
        "1",
    ]);
    tokio::time::sleep(std::time::Duration::from_secs(12)).await;

    let sink = NetworkSink::new();
    let captured = transport.capture_http_logs().await?;
    println!("captured {} events from a real device", captured.len());
    for e in &captured {
        println!(
            "  {} {} -> {} ({}ms)",
            e.method.as_deref().unwrap_or("<method not captured>"),
            e.url,
            e.status
                .map_or("<status not captured>".to_string(), |s| s.to_string()),
            e.duration_ms.map_or("?".to_string(), |d| d.to_string())
        );
    }
    assert!(
        !captured.is_empty(),
        "no traffic parsed from the device. The app must ship an OkHttp logging \
         interceptor logging under tag `okhttp.OkHttpClient`, or the capture \
         precondition is unmet and this test proves nothing"
    );
    sink.extend(NetworkSource::Logcat, captured);
    let window = sink.snapshot();

    // The control. This call is what the product list screen makes on launch,
    // so it must be green. Without it a red result below is unfalsifiable.
    let control = evaluate(&expect("*dummyjson.com/products*"), &window);
    println!("\ncontrol  *dummyjson.com/products*  -> {control:?}");
    assert_eq!(
        control,
        ExpectOutcome::Met,
        "the control must pass or the subject's failure means nothing"
    );

    // The subject. This endpoint does not exist, so a correct tool goes red.
    let subject = evaluate(&expect("*/v1/checkout"), &window);
    println!("subject  */v1/checkout               -> {subject:?}");
    assert!(
        subject.fails_task(),
        "a call that never fired must fail the build, got {subject:?}"
    );

    // Method and status narrow against real values, not invented ones.
    let mut precise = expect("*dummyjson.com/products*");
    precise.method = Some("GET".into());
    precise.status = Some(200);
    precise.times = Some(1);
    let exact = evaluate(&precise, &window);
    println!("exact    GET + 200 + times:1        -> {exact:?}");
    assert_eq!(exact, ExpectOutcome::Met);

    // times: 0 correctly asserts a call was NOT made.
    let mut absent = expect("*/legacy/track*");
    absent.times = Some(0);
    assert_eq!(evaluate(&absent, &window), ExpectOutcome::Met);

    println!("\nreal device, real traffic, real verdicts.");
    Ok(())
}
