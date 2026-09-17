// Regression test for quality bug 9.2: `pgrep -f drengr` over-matches
// any process whose command line contains "drengr" (e.g., `npm install drengr`,
// `vim drengr.txt`, a shell grep). The fix is to use `-x drengr` (exact argv[0]).

use std::fs;

#[test]
fn restart_uses_pgrep_exact_match_not_full_cmdline() {
    // The kill logic now lives in the shared `other_drengr_pids` +
    // `kill_other_drengr_processes` helpers (reused by restart AND uninstall).
    let src = fs::read_to_string("src/main.rs").expect("read src/main.rs");
    let start = src
        .find("fn other_drengr_pids")
        .expect("other_drengr_pids fn");
    let rest = &src[start..];
    let end = rest.find("\nasync fn ").unwrap_or(rest.len());
    let body = &rest[..end];

    assert!(
        body.contains("\"pgrep\""),
        "kill helper should still invoke pgrep"
    );
    assert!(
        body.contains("\"-x\""),
        "kill helper must use pgrep -x (exact match) to avoid over-matching"
    );
    assert!(
        !body.contains("\"-f\""),
        "kill helper must NOT use pgrep -f (matches full cmdline, over-matches)"
    );
    assert!(
        body.contains("pid > 1"),
        "kill helper must guard against signaling pid 0/1"
    );
}
