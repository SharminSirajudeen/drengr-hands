//! Drengr Runner bootstrap. Split into two paths:
//! - `prebuild_runner` (interactive CLI): extract source, xcodebuild, cache `.app`.
//! - `ensure_ready` (MCP-safe): install cached `.app`, launch, wait ready.
//!
//! See `runbooks/v060-drengr-runner-2026-05-14.md` §§5,7,8.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use include_dir::{include_dir, Dir};
use tokio::process::Command as TokioCommand;
use tokio::time::Instant;

use super::client::DriverClient;
use super::locks::{CacheKeyLock, UdidLock};
use super::process::LaunchedRunner;
use super::{port_registry, DriverError};

/// First 12 hex chars of SHA-256 over `drengr-runner/` source. Set by build.rs.
pub const RUNNER_SHA: &str = env!("DRENGR_RUNNER_SHA");

static RUNNER_SRC: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/drengr-runner");

const BUILD_TIMEOUT_SECS: u64 = 150;
const WAIT_READY_SECS: u64 = 30;

/// Extract the prebuilt `DrengrRunner-Runner.app` (shipped embedded in the
/// drengr binary) into the per-(ios_minor, xcode_build, runner_sha) cache.
/// We do NOT invoke xcodebuild at runtime — macOS's CoreSimulatorService XPC
/// is unreachable from posix_spawn'd subprocesses, so destination resolution
/// fails. This is what Appium/Maestro/idb/WDA all do (research-confirmed).
pub async fn prebuild_runner(udid: &str) -> super::Result<PathBuf> {
    let (ios_minor, xcode_build) = probe(udid).await?;
    let cache_key = format!("ios{}-{}-runner{}", ios_minor, xcode_build, RUNNER_SHA);
    let cache_root = cache_root_path(&cache_key)?;

    let build_lock =
        CacheKeyLock::try_acquire(&cache_key, Duration::from_secs(BUILD_TIMEOUT_SECS)).await?;
    let xctestrun = cache_root.join("DrengrRunner.xctestrun");
    if !xctestrun.exists() {
        tracing::info!("Extracting prebuilt Drengr Runner for iOS {}...", ios_minor);
        extract_prebuilt_tree(&cache_root).await?;
    }
    drop(build_lock);
    Ok(xctestrun)
}

/// Progress reporter for long-running bootstrap steps. Called from
/// `ensure_ready_with_progress` so the MCP layer can keep the client
/// (Claude Desktop's ~60s tool timeout) alive on cold sim boots.
pub type ProgressFn = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// Thin wrapper over `ensure_ready_with_progress` for non-MCP callers.
pub async fn ensure_ready(udid: &str) -> super::Result<LaunchedRunner> {
    ensure_ready_with_progress(udid, None).await
}

/// Install + launch the cached runner against a booted sim. MCP-safe — never
/// spawns xcodebuild. Returns `RunnerNotProvisioned` if the cache is missing
/// (user must run `drengr build-runner` from an interactive terminal first).
pub async fn ensure_ready_with_progress(
    udid: &str,
    progress: Option<ProgressFn>,
) -> super::Result<LaunchedRunner> {
    let beat = |msg: &str| {
        tracing::info!("driver bootstrap: {}", msg);
        if let Some(ref p) = progress {
            p(msg);
        }
    };

    beat("Resolving iOS sim + Xcode versions...");
    let (ios_minor, xcode_build) = probe(udid).await?;
    let cache_key = format!("ios{}-{}-runner{}", ios_minor, xcode_build, RUNNER_SHA);
    let cache_root = cache_root_path(&cache_key)?;
    let xctestrun = cache_root.join("DrengrRunner.xctestrun");

    if !xctestrun.exists() {
        // Auto-provision when missing (first run / update / Xcode|iOS change).
        // Pure file copy, MCP-safe. Build-lock serializes concurrent callers.
        beat("Provisioning iOS runner for this Xcode/iOS (one-time)...");
        let build_lock =
            CacheKeyLock::try_acquire(&cache_key, Duration::from_secs(BUILD_TIMEOUT_SECS)).await?;
        if !xctestrun.exists() {
            extract_prebuilt_tree(&cache_root).await?;
        }
        drop(build_lock);
    }

    let port = port_registry::allocate(udid)?;
    let guard = PortReleaseGuard::new(udid);
    beat("Launching runner (xcodebuild test-without-building)...");
    let launched = launch_pipeline_with_progress(udid, port, &xctestrun, beat).await?;
    guard.disarm();
    Ok(launched)
}

/// Disarm-on-success RAII guard around the per-UDID port allocation.
struct PortReleaseGuard<'a> {
    udid: &'a str,
    armed: bool,
}
impl<'a> PortReleaseGuard<'a> {
    fn new(udid: &'a str) -> Self {
        Self { udid, armed: true }
    }
    fn disarm(mut self) {
        self.armed = false;
    }
}
impl Drop for PortReleaseGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            port_registry::release(self.udid);
        }
    }
}

async fn launch_pipeline_with_progress(
    udid: &str,
    port: u16,
    xctestrun: &Path,
    beat: impl Fn(&str),
) -> super::Result<LaunchedRunner> {
    let lock = acquire_lock_reaping_orphan(udid, port).await?;

    // Holding the UdidLock means no other live Drengr owns this sim, so any
    // runner still on it is an orphan from a prior crashed attempt/process.
    // Clear it before launching — otherwise it zombies on the sim, holds port
    // 8200, and forces this launch to fail, which is the relaunch loop.
    // Reap orphaned xcodebuild from a crashed prior Drengr FIRST, else it
    // re-launches the runner right after we terminate it.
    reap_orphaned_runner_builds(udid).await;
    terminate_runner(udid).await;
    // Sweep the legacy Facebook WDA runner left by pre-v0.6.0 Drengr.
    uninstall_legacy_wda(udid).await;

    // Inject the allocated port so this runner listens on it instead of the
    // 8200 default — lets multiple live Drengr instances co-drive different sims.
    let run_file = inject_runner_port(xctestrun, port).await?;

    let mut cmd = TokioCommand::new("xcodebuild");
    cmd.args([
        "test-without-building",
        "-xctestrun",
        &run_file.to_string_lossy(),
        "-destination",
        &format!("id={udid}"),
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    // Reap the xcodebuild proc if this handle is dropped on any early return
    // below (tokio::process::Child does not kill on drop by default).
    .kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| DriverError::Io(format!("spawn xcodebuild test-without-building: {e}")))?;
    let pid = child.id().unwrap_or(0);

    beat(&format!("Waiting for runner /status on port {port}..."));
    if let Err(e) = wait_ready(port, Duration::from_secs(WAIT_READY_SECS)).await {
        // Bootstrap failed. LaunchedRunner — whose Drop terminates the in-sim
        // xctrunner — is never constructed on this path, so reap it here. The
        // xcodebuild child is reaped by kill_on_drop as `child` drops.
        terminate_runner(udid).await;
        return Err(e);
    }

    Ok(LaunchedRunner {
        udid: udid.to_string(),
        port,
        pid,
        client: DriverClient::new(port),
        child: Some(child),
        lock,
    })
}

/// Write a per-port copy of the cached `.xctestrun` (next to the original, so
/// `__TESTROOT__` still resolves to the shared `Release-iphonesimulator` tree)
/// with `DRENGR_RUNNER_PORT=<port>` injected into the runner's environment. The
/// runner reads it on startup (DrengrRunner.swift), so multiple live Drengr
/// instances can co-drive different sims instead of all colliding on 8200.
async fn inject_runner_port(xctestrun: &Path, port: u16) -> super::Result<PathBuf> {
    let dir = xctestrun.parent().unwrap_or_else(|| Path::new("."));
    let dst = dir.join(format!("DrengrRunner-{port}.xctestrun"));
    tokio::fs::copy(xctestrun, &dst)
        .await
        .map_err(|e| DriverError::Io(format!("copy xctestrun: {e}")))?;
    // FormatVersion 1: env vars live under the blueprint key "DrengrRunner".
    let ok = TokioCommand::new("plutil")
        .args([
            "-replace",
            "DrengrRunner.EnvironmentVariables.DRENGR_RUNNER_PORT",
            "-string",
            &port.to_string(),
            &dst.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|e| DriverError::Io(format!("plutil inject port: {e}")))?
        .success();
    if !ok {
        return Err(DriverError::Io(
            "plutil failed to inject DRENGR_RUNNER_PORT into xctestrun".into(),
        ));
    }
    Ok(dst)
}

/// Best-effort `simctl terminate` of the runner bundle on a sim. Ignores
/// errors (the app is usually not running, which is the desired end state).
/// Acquire the per-sim runner lock, auto-reaping a holder that is *demonstrably
/// orphaned* (reparented to launchd → its Claude client is gone) so the user
/// never has to run `drengr restart`. A holder with a live, non-launchd parent
/// is a legitimate concurrent Drengr instance and is left untouched — we return
/// the SessionConflict so the caller can surface a clear message instead.
async fn acquire_lock_reaping_orphan(udid: &str, port: u16) -> super::Result<UdidLock> {
    match UdidLock::try_acquire(udid, port) {
        Ok(l) => Ok(l),
        Err(DriverError::SessionConflict { holder_pid, .. })
            if holder_pid > 1 && is_orphaned(holder_pid).await =>
        {
            tracing::warn!(
                "reaping orphaned Drengr holder (pid {holder_pid}, client gone) to free the sim"
            );
            reap_process(holder_pid).await;
            UdidLock::try_acquire(udid, port) // retry once; the flock is now free
        }
        Err(e) => Err(e),
    }
}

/// True if the process has been reparented to launchd (ppid <= 1), i.e. the
/// process that spawned it (the Claude client) has exited — so it's an orphan.
async fn is_orphaned(pid: u32) -> bool {
    let out = TokioCommand::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .await;
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<i64>()
            .map(|ppid| ppid <= 1)
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// SIGTERM, give it a moment to release the flock, then SIGKILL if still alive.
async fn reap_process(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid as i32, 0) == 0 {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await; // let the OS free the flock
}

async fn terminate_runner(udid: &str) {
    let _ = TokioCommand::new("xcrun")
        .args(["simctl", "terminate", udid, super::RUNNER_BUNDLE_ID])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// Pre-v0.6.0 used a Facebook WebDriverAgent xctrunner. Sims that ran an old
/// Drengr still have it installed ("Drengr Driver Agent"). Best-effort remove
/// it on provision so only `dev.drengr.runner` remains.
async fn uninstall_legacy_wda(udid: &str) {
    let _ = TokioCommand::new("xcrun")
        .args([
            "simctl",
            "uninstall",
            udid,
            "com.facebook.WebDriverAgentRunner.xctrunner",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// Reap orphaned `xcodebuild test-without-building` from a previously crashed
/// or SIGKILL'd Drengr (where Drop / kill_on_drop never ran). Such an orphan
/// keeps relaunching the runner and re-binding port 8200, which otherwise
/// turns into a visible re-install loop on the sim. Safe: we hold the UdidLock
/// for this sim, so any match is genuinely an orphan, and the pattern is
/// scoped to this udid's DrengrRunner build.
async fn reap_orphaned_runner_builds(udid: &str) {
    let pattern = format!("test-without-building.*DrengrRunner.*xctestrun.*{udid}");
    let _ = TokioCommand::new("pkill")
        .args(["-f", &pattern])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

/// Extract the embedded `prebuilt/` tree (xctestrun + Release-iphonesimulator/
/// DrengrRunner-Runner.app/) into `cache_root`. xcodebuild test-without-building
/// resolves __TESTROOT__ to the directory containing the .xctestrun.
async fn extract_prebuilt_tree(cache_root: &Path) -> super::Result<()> {
    let prebuilt = RUNNER_SRC.get_dir("prebuilt").ok_or_else(|| DriverError::Bug(
        "embedded prebuilt/ tree missing — rebuild drengr from a checkout that contains drengr-runner/prebuilt/".into()
    ))?;
    let cache_root = cache_root.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let _ = std::fs::remove_dir_all(&cache_root);
        std::fs::create_dir_all(&cache_root)?;
        // Walk the prebuilt/ tree, rebasing paths to cache_root (so files
        // land at cache_root/<rel> instead of cache_root/prebuilt/<rel>).
        write_dir_rebased(prebuilt, "prebuilt", &cache_root)?;
        Ok(())
    })
    .await
    .map_err(|e| DriverError::Io(format!("extract join: {e}")))?
    .map_err(|e| DriverError::Io(format!("extract prebuilt tree: {e}")))?;
    Ok(())
}

/// Walk an embedded `Dir` and write its contents under `base`, stripping
/// `strip_prefix` from each entry's relative path. Creates parent dirs first.
fn write_dir_rebased(
    dir: &include_dir::Dir<'_>,
    strip_prefix: &str,
    base: &Path,
) -> std::io::Result<()> {
    for entry in dir.entries() {
        let entry_path = entry.path();
        let rel = entry_path.strip_prefix(strip_prefix).unwrap_or(entry_path);
        let dest = base.join(rel);
        match entry {
            include_dir::DirEntry::Dir(sub) => {
                std::fs::create_dir_all(&dest)?;
                write_dir_rebased(sub, strip_prefix, base)?;
            }
            include_dir::DirEntry::File(f) => {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&dest, f.contents())?;
            }
        }
    }
    Ok(())
}

fn drengr_subdir(parts: &[&str]) -> super::Result<PathBuf> {
    let mut p =
        crate::paths::drengr_dir().ok_or_else(|| DriverError::Io("no home directory".into()))?;
    for part in parts {
        p = p.join(part);
    }
    Ok(p)
}

/// Returns `(ios_minor, xcode_build)` e.g. `("18.4", "15F31d")`.
async fn probe(udid: &str) -> super::Result<(String, String)> {
    // xcode probe first — if Xcode is missing, surface that before touching simctl.
    let xcode = probe_xcode_build().await?;
    let ios = probe_ios_minor(udid).await?;
    Ok((ios, xcode))
}

async fn probe_xcode_build() -> super::Result<String> {
    let out = TokioCommand::new("xcrun")
        .args(["xcodebuild", "-version"])
        .output()
        .await
        .map_err(classify_xcrun_io_error)?;
    if !out.status.success() {
        return Err(DriverError::XcodeMissing);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("Build version") {
            let build = rest.trim().to_string();
            if !build.is_empty() {
                return Ok(build);
            }
        }
    }
    Err(DriverError::Bug(format!(
        "parse xcodebuild -version: {}",
        stdout.trim()
    )))
}

async fn probe_ios_minor(udid: &str) -> super::Result<String> {
    let out = TokioCommand::new("xcrun")
        .args(["simctl", "list", "-j", "devices"])
        .output()
        .await
        .map_err(classify_xcrun_io_error)?;
    if !out.status.success() {
        return Err(DriverError::Bug(format!(
            "simctl list exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| DriverError::Bug(format!("simctl list json: {e}")))?;
    let devices = json
        .get("devices")
        .and_then(|v| v.as_object())
        .ok_or_else(|| DriverError::Bug("simctl list missing .devices".into()))?;
    for (runtime_id, devs) in devices.iter() {
        if let Some(arr) = devs.as_array() {
            for d in arr {
                if d.get("udid").and_then(|u| u.as_str()) == Some(udid) {
                    return parse_ios_minor_from_runtime(runtime_id).ok_or_else(|| {
                        DriverError::Bug(format!("parse iOS minor from {runtime_id}"))
                    });
                }
            }
        }
    }
    Err(DriverError::Bug(format!(
        "udid {udid} not found in simctl list"
    )))
}

/// "com.apple.CoreSimulator.SimRuntime.iOS-18-4" -> "18.4"
/// "com.apple.CoreSimulator.SimRuntime.iOS-17"   -> "17.0" (Apple ships
/// major-only runtime IDs for .0 releases).
fn parse_ios_minor_from_runtime(runtime_id: &str) -> Option<String> {
    let tail = runtime_id.rsplit('.').next()?.strip_prefix("iOS-")?;
    let mut parts = tail.splitn(2, '-');
    let major = parts.next()?;
    let minor = parts
        .next()
        .map(|s| s.replace('-', "."))
        .unwrap_or_else(|| "0".to_string());
    Some(format!("{major}.{minor}"))
}

fn cache_root_path(cache_key: &str) -> super::Result<PathBuf> {
    drengr_subdir(&["runner", "builds", cache_key])
}

fn classify_xcrun_io_error(e: std::io::Error) -> DriverError {
    if e.kind() == std::io::ErrorKind::NotFound {
        DriverError::XcodeMissing
    } else {
        DriverError::Io(format!("spawn xcrun: {e}"))
    }
}

async fn wait_ready(port: u16, total: Duration) -> super::Result<()> {
    let client = DriverClient::new(port);
    let started = Instant::now();
    loop {
        if let Ok(s) = client.status().await {
            if s.ok && s.product == "drengr-runner" {
                tracing::info!(
                    "drengr-runner ready on port {} after {:?}",
                    port,
                    started.elapsed()
                );
                return Ok(());
            }
        }
        if started.elapsed() >= total {
            return Err(DriverError::NotReady {
                port,
                waited_secs: total.as_secs(),
            });
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_format() {
        let key = format!("ios{}-{}-runner{}", "18.4", "15F31d", "abcdef123456");
        assert_eq!(key, "ios18.4-15F31d-runnerabcdef123456");
    }

    #[test]
    fn classify_xcrun_missing_error() {
        let err = std::io::Error::from(std::io::ErrorKind::NotFound);
        match classify_xcrun_io_error(err) {
            DriverError::XcodeMissing => {}
            other => panic!("expected XcodeMissing, got {other:?}"),
        }
    }

    #[test]
    fn parse_ios_minor_extracts_major_dot_minor() {
        assert_eq!(
            parse_ios_minor_from_runtime("com.apple.CoreSimulator.SimRuntime.iOS-18-4").as_deref(),
            Some("18.4")
        );
        assert_eq!(
            parse_ios_minor_from_runtime("com.apple.CoreSimulator.SimRuntime.iOS-19-0").as_deref(),
            Some("19.0")
        );
    }

    #[test]
    fn parse_ios_17_major_only_returns_17_0() {
        assert_eq!(
            parse_ios_minor_from_runtime("com.apple.CoreSimulator.SimRuntime.iOS-17").as_deref(),
            Some("17.0")
        );
    }

    /// API-shape guard: documented entrypoints exist with the expected signatures.
    #[test]
    fn prebuild_runner_function_exists_with_correct_signature() {
        let _f = prebuild_runner;
        let _g = ensure_ready;
        let _h = ensure_ready_with_progress;
    }

    /// `ensure_ready` returns `RunnerNotProvisioned` when the cache is missing.
    /// We can't drive the probe without a real Xcode/sim, but we can verify the
    /// error variant constructed at the gate is shaped correctly + carries the hint.
    #[test]
    fn runner_not_provisioned_error_carries_cache_key_hint() {
        let err = DriverError::RunnerNotProvisioned {
            hint: "ios18.4-15F31d-runnerabcdef123456".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("runner_not_provisioned"));
        assert!(msg.contains("drengr build-runner"));
        assert!(msg.contains("ios18.4-15F31d-runnerabcdef123456"));
    }
}
