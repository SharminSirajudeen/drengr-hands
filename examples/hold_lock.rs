//! Test fixture: grabs the real per-sim runner lock and holds it, simulating a
//! pre-0.7.13 orphaned `drengr mcp` straggler (no self-exit watchdog). Launch it
//! detached so it reparents to launchd (ppid 1), then verify a fresh Drengr
//! reaps it. Not shipped — used only by the session-conflict reap test.
//!
//!   cargo run --release --example hold_lock -- <udid>

use drengr_hands::driver::locks::UdidLock;

fn main() {
    let udid = std::env::args().nth(1).expect("usage: hold_lock <udid>");
    match UdidLock::try_acquire(&udid, 8200) {
        Ok(_lock) => {
            println!("HELD pid={}", std::process::id());
            // Hold it until reaped (or 10 min, whichever comes first).
            std::thread::sleep(std::time::Duration::from_secs(600));
        }
        Err(e) => {
            eprintln!("could not acquire: {e}");
            std::process::exit(2);
        }
    }
}
