//! `drengr doctor` system checks. Each returns a (status, detail) pair the
//! command renders; none of them decide anything, so they stay out of dispatch.

pub(crate) async fn check_xcode_installed() -> (&'static str, String) {
    let fut = tokio::process::Command::new("xcrun")
        .args(["xcodebuild", "-version"])
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(o)) if o.status.success() => o,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                "[✗]",
                "not installed — install Xcode, then sudo xcode-select -s /Applications/Xcode.app"
                    .to_string(),
            );
        }
        _ => {
            return (
                "[✗]",
                "xcrun xcodebuild -version failed (Xcode CLT only?) — install full Xcode"
                    .to_string(),
            );
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut version = String::new();
    let mut build = String::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Xcode ") {
            version = rest.to_string();
        } else if let Some(rest) = t.strip_prefix("Build version ") {
            build = rest.to_string();
        }
    }
    let label = match (version.is_empty(), build.is_empty()) {
        (false, false) => format!("Xcode {} ({})", version, build),
        (false, true) => format!("Xcode {}", version),
        _ => "installed".to_string(),
    };
    ("[✓]", label)
}

/// `xcrun simctl list -j runtimes` → ("[✓]", "iOS 18.5, iOS 17.5") or ("[✗]", hint).
pub(crate) async fn check_ios_sim_runtimes() -> (&'static str, String) {
    let fut = tokio::process::Command::new("xcrun")
        .args(["simctl", "list", "-j", "runtimes"])
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(o)) if o.status.success() => o,
        _ => {
            return (
                "[✗]",
                "not installed — run xcodebuild -downloadPlatform iOS".to_string(),
            )
        }
    };
    let json: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => {
            return (
                "[✗]",
                "simctl list runtimes returned malformed JSON".to_string(),
            )
        }
    };
    let mut versions: Vec<String> = Vec::new();
    if let Some(arr) = json.get("runtimes").and_then(|v| v.as_array()) {
        for r in arr {
            let id = r.get("identifier").and_then(|v| v.as_str()).unwrap_or("");
            let available = r
                .get("isAvailable")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if available && id.contains(".iOS-") {
                if let Some(v) = r.get("version").and_then(|v| v.as_str()) {
                    versions.push(format!("iOS {}", v));
                }
            }
        }
    }
    if versions.is_empty() {
        (
            "[✗]",
            "not installed — run xcodebuild -downloadPlatform iOS".to_string(),
        )
    } else {
        versions.sort();
        versions.dedup();
        ("[✓]", versions.join(", "))
    }
}

/// `xcrun simctl list -j devices` → ("[✓]", "N available (X booted)") or ("[–]", hint).
pub(crate) async fn check_ios_sim_devices() -> (&'static str, String) {
    let fut = tokio::process::Command::new("xcrun")
        .args(["simctl", "list", "-j", "devices", "available"])
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(5), fut).await {
        Ok(Ok(o)) if o.status.success() => o,
        _ => {
            return (
                "[–]",
                "none configured — see: xcrun simctl create -h".to_string(),
            )
        }
    };
    let json: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => {
            return (
                "[–]",
                "simctl list devices returned malformed JSON".to_string(),
            )
        }
    };
    let mut total = 0usize;
    let mut booted = 0usize;
    if let Some(map) = json.get("devices").and_then(|v| v.as_object()) {
        for (runtime, list) in map {
            if !runtime.contains(".iOS-") {
                continue;
            }
            if let Some(arr) = list.as_array() {
                for d in arr {
                    let avail = d
                        .get("isAvailable")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    if !avail {
                        continue;
                    }
                    total += 1;
                    if d.get("state").and_then(|s| s.as_str()) == Some("Booted") {
                        booted += 1;
                    }
                }
            }
        }
    }
    if total == 0 {
        (
            "[–]",
            "none configured — see: xcrun simctl create -h".to_string(),
        )
    } else {
        ("[✓]", format!("{} available ({} booted)", total, booted))
    }
}

/// Probe whether the runner port is free. Drops the listener before return.
pub(crate) fn check_port_8200() -> (&'static str, &'static str) {
    match std::net::TcpListener::bind(("127.0.0.1", drengr_hands::driver::RUNNER_DEFAULT_PORT)) {
        Ok(_) => ("[✓]", "free"),
        Err(_) => ("[–]", "in use (another runner may be running)"),
    }
}

/// Walk `~/.drengr/runner/builds/` for any cached `*-Runner.app`.
/// Best-effort: cache-key freshness is verified at use time, not here.
pub(crate) fn check_ios_runner_cache() -> (bool, String) {
    let builds = match drengr_hands::paths::drengr_dir() {
        Some(d) => d.join("runner").join("builds"),
        None => return (false, "no home directory".to_string()),
    };
    if !builds.exists() {
        return (
            false,
            "not cached — provisions automatically on first use".to_string(),
        );
    }
    let entries = match std::fs::read_dir(&builds) {
        Ok(e) => e,
        Err(_) => {
            return (
                false,
                "not cached — provisions automatically on first use".to_string(),
            )
        }
    };
    for entry in entries.flatten() {
        let key_dir = entry.path();
        if !key_dir.is_dir() {
            continue;
        }
        if let Ok(inner) = std::fs::read_dir(&key_dir) {
            for sub in inner.flatten() {
                let name = sub.file_name();
                let s = name.to_string_lossy();
                if s.ends_with("-Runner.app") {
                    return (true, format!("cached {}", sub.path().display()));
                }
            }
        }
    }
    (
        false,
        "not cached — provisions automatically on first use".to_string(),
    )
}

/// Sum the total size of all files under `path`, in megabytes.
pub(crate) fn fs_dir_size_mb(path: &std::path::Path) -> Option<f64> {
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let entries = match std::fs::read_dir(&p) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    Some(total as f64 / 1_048_576.0)
}

/// `drengr doctor --clean-runner-cache` — wipe `~/.drengr/runner/builds/`.
pub(crate) async fn run_clean_runner_cache(yes: bool) -> anyhow::Result<()> {
    let builds = drengr_hands::paths::drengr_dir()
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?
        .join("runner")
        .join("builds");
    if !builds.exists() {
        println!(
            "Runner cache does not exist ({}); nothing to clean.",
            builds.display()
        );
        return Ok(());
    }
    let mut to_delete: Vec<std::path::PathBuf> = Vec::new();
    let mut total_bytes_mb = 0.0f64;
    for entry in std::fs::read_dir(&builds)?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(mb) = fs_dir_size_mb(&path) {
            total_bytes_mb += mb;
        }
        to_delete.push(path);
    }
    if to_delete.is_empty() {
        println!("No cached runner builds found.");
        return Ok(());
    }
    println!(
        "\n  Runner cache cleanup ({:.1} MB to reclaim)\n",
        total_bytes_mb
    );
    for p in &to_delete {
        println!("    {}", p.display());
    }
    println!();
    if !yes {
        use std::io::Write;
        print!("  Delete these directories? [y/N] ");
        std::io::stdout().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("  Cancelled.");
            return Ok(());
        }
    }
    for p in &to_delete {
        if let Err(e) = std::fs::remove_dir_all(p) {
            eprintln!("  ✗ failed to delete {}: {}", p.display(), e);
        }
    }
    println!("\n  ✓ Reclaimed ~{:.1} MB\n", total_bytes_mb);
    Ok(())
}

/// `drengr doctor --runner-status` — one-line machine-friendly summary.
pub(crate) async fn run_runner_status() -> anyhow::Result<()> {
    use drengr_hands::driver::{RUNNER_DEFAULT_PORT, RUNNER_VERSION};

    let (cached, _) = check_ios_runner_cache();
    let cache_glyph = if cached { "cached" } else { "missing" };

    let port_free = std::net::TcpListener::bind(("127.0.0.1", RUNNER_DEFAULT_PORT)).is_ok();
    let port_glyph = if port_free { "free" } else { "in-use" };

    let booted_count =
        tokio::process::Command::new(drengr_hands::transport::simctl::resolve_xcrun())
            .args(["simctl", "list", "devices", "booted"])
            .output()
            .await
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .matches("(Booted)")
                    .count()
            })
            .unwrap_or(0);

    println!(
        "runner {} {} • port {} {} • {} sim{} booted",
        RUNNER_VERSION,
        cache_glyph,
        RUNNER_DEFAULT_PORT,
        port_glyph,
        booted_count,
        if booted_count == 1 { "" } else { "s" },
    );
    Ok(())
}
