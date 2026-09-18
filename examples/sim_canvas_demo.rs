//! Stands up the iOS runner, opens a URL in the simulator's Safari, and holds
//! the runner alive on :8200 so an external vision loop can drive it via HTTP
//! (/observe, /act). Used to validate coordinate tap/draw on a treeless canvas.
//!
//! DRENGR_TEST_UDID=<booted sim udid> CANVAS_URL=http://localhost:8765/ \
//!   cargo run --release --example sim_canvas_demo

use drengr_hands::driver::bootstrap;

#[tokio::main]
async fn main() {
    let udid = std::env::var("DRENGR_TEST_UDID").expect("set DRENGR_TEST_UDID");
    let url = std::env::var("CANVAS_URL").expect("set CANVAS_URL");

    eprintln!("[demo] ensuring runner on {udid} …");
    let runner = bootstrap::ensure_ready(&udid)
        .await
        .expect("ensure_ready failed");
    eprintln!("[demo] runner ready on http://127.0.0.1:{}", runner.port);

    // Bring Safari (the canvas) to the foreground last so taps land on it.
    let status = std::process::Command::new("xcrun")
        .args(["simctl", "openurl", &udid, &url])
        .status();
    eprintln!("[demo] openurl {url} -> {status:?}");

    eprintln!("[demo] READY — runner alive on :{} for 20 min", runner.port);
    tokio::time::sleep(std::time::Duration::from_secs(1200)).await;
    drop(runner);
}
