//! v0.6.0 iOS driver smoke test. Requires a booted iOS simulator + Xcode.
//! Run: DRENGR_TEST_UDID=<udid> cargo test --release --test ios_driver_smoke -- --ignored --nocapture

use drengr_hands::driver::{bootstrap, client::Action, RUNNER_VERSION};

#[tokio::test]
#[ignore]
async fn ios_driver_bootstrap_smoke() {
    let udid = std::env::var("DRENGR_TEST_UDID").unwrap_or_else(|_| {
        panic!(
            "DRENGR_TEST_UDID is required: set it to a booted iOS simulator UDID. \
             Run `xcrun simctl list devices booted` to find one."
        )
    });

    let cache_path = bootstrap::prebuild_runner(&udid)
        .await
        .expect("prebuild_runner must succeed against a booted sim + Xcode");
    assert!(
        cache_path.exists(),
        "prebuild_runner returned a path that doesn't exist: {}",
        cache_path.display()
    );

    let runner = bootstrap::ensure_ready(&udid)
        .await
        .expect("ensure_ready must launch the runner against the booted sim");

    let status = runner
        .client
        .status()
        .await
        .expect("status() must succeed on a ready runner");
    assert_eq!(status.product, "drengr-runner", "status.product mismatch");
    assert_eq!(status.version, RUNNER_VERSION, "status.version mismatch");
    assert!(status.ok, "status.ok was false");

    let obs = runner
        .client
        .observe(None)
        .await
        .expect("observe() must succeed");
    assert!(
        !obs.screenshot_b64.is_empty(),
        "screenshot_b64 must be non-empty"
    );
    assert!(
        obs.screenshot_b64.len() > 1_000_000,
        "screenshot_b64 should exceed 1MB (got {} bytes)",
        obs.screenshot_b64.len()
    );

    runner
        .client
        .act(Action::Tap { x: 100.0, y: 100.0 })
        .await
        .expect("act(Tap) must succeed");

    // Rotate, then confirm the runner SURVIVED (NWListener can drop its socket
    // when SpringBoard rotates the device). A follow-up observe must still work.
    runner
        .client
        .act(Action::Orientation {
            orientation: "landscape_left".into(),
        })
        .await
        .expect("act(Orientation landscape) must succeed");

    let rotated = runner
        .client
        .observe(None)
        .await
        .expect("observe() must still succeed after rotation (runner survived)");
    assert!(
        !rotated.screenshot_b64.is_empty(),
        "post-rotation screenshot must be non-empty"
    );

    runner
        .client
        .act(Action::Orientation {
            orientation: "portrait".into(),
        })
        .await
        .expect("act(Orientation portrait) must restore orientation");

    drop(runner);
}
