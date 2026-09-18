//! Smoke test for the v0.6.0 iOS driver. Requires a booted iOS simulator.
//!
//! Run: DRENGR_TEST_UDID=<udid> cargo run --release --example driver_smoke

use drengr_hands::driver::{self, bootstrap, client::Action};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("drengr=debug".parse().unwrap()),
        )
        .init();

    let udid = std::env::var("DRENGR_TEST_UDID")
        .expect("set DRENGR_TEST_UDID to a booted iOS simulator UDID");

    println!(">>> prebuild_runner({udid})");
    let cache_path = match bootstrap::prebuild_runner(&udid).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("prebuild_runner failed: {e}");
            std::process::exit(1);
        }
    };
    println!(">>> prebuilt at: {}", cache_path.display());

    println!(">>> ensure_ready({udid})");
    let runner = match driver::bootstrap::ensure_ready(&udid).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ensure_ready failed: {e}");
            std::process::exit(1);
        }
    };
    println!(">>> launched: pid={} port={}", runner.pid, runner.port);

    println!(">>> client.status()");
    match runner.client.status().await {
        Ok(s) => println!(
            "    ok={} product={} version={} ios_major={} screen={}x{}@{}",
            s.ok,
            s.product,
            s.version,
            s.ios_major,
            s.screen.width,
            s.screen.height,
            s.screen.scale
        ),
        Err(e) => eprintln!("    status failed: {e}"),
    }

    println!(">>> client.act(Tap{{ x: 100, y: 100 }}) BEFORE observe");
    match runner.client.act(Action::Tap { x: 100.0, y: 100.0 }).await {
        Ok(()) => println!("    tap ok"),
        Err(e) => eprintln!("    tap failed: {e}"),
    }

    println!(">>> client.observe()");
    match runner.client.observe(None).await {
        Ok(o) => println!(
            "    screenshot_b64 len={} tree_hint={}",
            o.screenshot_b64.len(),
            if o.tree_hint.is_some() {
                "present"
            } else {
                "null"
            }
        ),
        Err(e) => eprintln!("    observe failed: {e}"),
    }

    println!(">>> client.act(Tap{{ x: 100, y: 100 }})");
    match runner.client.act(Action::Tap { x: 100.0, y: 100.0 }).await {
        Ok(()) => println!("    tap ok"),
        Err(e) => eprintln!("    tap failed: {e}"),
    }

    println!(">>> dropping LaunchedRunner (terminates xctrunner + releases port + drops UdidLock)");
    drop(runner);
    println!(">>> done.");
}
