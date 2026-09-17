//! Drengr Runner driver — replaces src/wda/ for v0.6.0.
//! Vision-first, framework-blind. 3-route HTTP API to the Swift XCTest target
//! at drengr-runner/. See runbooks/v060-drengr-runner-2026-05-14.md and
//! feedback_drengr_touch_is_a_touch.md.

pub mod bootstrap;
pub mod client;
pub mod locks;
pub mod port_registry;
pub mod process;
pub mod source_parse;

pub const RUNNER_BUNDLE_ID: &str = "dev.drengr.runner.xctrunner";
pub const RUNNER_DEFAULT_PORT: u16 = 8200;
pub const RUNNER_VERSION: &str = "0.6.0";

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("driver_xcode_missing — install Xcode from developer.apple.com, then run `sudo xcode-select -s /Applications/Xcode.app`")]
    XcodeMissing,

    #[error("driver_sim_runtime_missing ios_major={ios_major} — run `xcodebuild -downloadPlatform iOS` to install the runtime")]
    SimRuntimeMissing { ios_major: u8 },

    #[error("driver_build_failed exit_code={exit_code}\n{log_tail}")]
    BuildFailed { exit_code: i32, log_tail: String },

    #[error("runner_not_provisioned — run `drengr build-runner` from your terminal to build the iOS runner ({hint})")]
    RunnerNotProvisioned { hint: String },

    #[error("driver_build_lock_timeout cache_key={cache_key} waited_secs={waited_secs}")]
    BuildLockTimeout { cache_key: String, waited_secs: u64 },

    #[error("driver_not_ready port={port} waited_secs={waited_secs}")]
    NotReady { port: u16, waited_secs: u64 },

    #[error(
        "driver_session_conflict udid={udid} holder_pid={holder_pid} holder_port={holder_port}"
    )]
    SessionConflict {
        udid: String,
        holder_pid: u32,
        holder_port: u16,
    },

    #[error("driver_http {0}")]
    HttpFailed(#[from] reqwest::Error),

    #[error("driver_bad_response status={status} body={body}")]
    BadResponse { status: u16, body: String },

    #[error("driver_io {0}")]
    Io(String),

    #[error("driver_bug {0}")]
    Bug(String),
}

impl DriverError {
    /// Stable, bounded, PII-free slug for telemetry. The set is fixed and the
    /// values carry no free-form text, so it is safe to ship as `error_kind`.
    /// `driver_error_code` recovers the same slug from a stringified error;
    /// the two are kept in sync by `driver_error_code_matches_display`.
    pub fn code(&self) -> &'static str {
        match self {
            DriverError::XcodeMissing => "xcode_missing",
            DriverError::SimRuntimeMissing { .. } => "sim_runtime_missing",
            DriverError::BuildFailed { .. } => "build_failed",
            DriverError::RunnerNotProvisioned { .. } => "runner_not_provisioned",
            DriverError::BuildLockTimeout { .. } => "build_lock_timeout",
            DriverError::NotReady { .. } => "driver_not_ready",
            DriverError::SessionConflict { .. } => "session_conflict",
            DriverError::HttpFailed(_) => "driver_http",
            DriverError::BadResponse { .. } => "driver_bad_response",
            DriverError::Io(_) => "driver_io",
            DriverError::Bug(_) => "driver_bug",
        }
    }
}

impl From<std::io::Error> for DriverError {
    fn from(e: std::io::Error) -> Self {
        DriverError::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, DriverError>;

/// Recover the bounded driver error code from a stringified error (an
/// `anyhow`/`ToolResult` text that wrapped a `DriverError`), by matching the
/// stable slug each variant embeds in its `Display`. Returns `None` when the
/// message is not a driver error. Most-specific slugs are checked first so a
/// shorter slug never shadows a longer one.
pub fn driver_error_code(msg: &str) -> Option<&'static str> {
    const TABLE: &[(&str, &str)] = &[
        ("driver_xcode_missing", "xcode_missing"),
        ("driver_sim_runtime_missing", "sim_runtime_missing"),
        ("driver_build_lock_timeout", "build_lock_timeout"),
        ("driver_build_failed", "build_failed"),
        ("runner_not_provisioned", "runner_not_provisioned"),
        ("driver_not_ready", "driver_not_ready"),
        ("driver_session_conflict", "session_conflict"),
        ("driver_bad_response", "driver_bad_response"),
        ("driver_http", "driver_http"),
        ("driver_io", "driver_io"),
        ("driver_bug", "driver_bug"),
    ];
    let lower = msg.to_lowercase();
    TABLE
        .iter()
        .find(|(slug, _)| lower.contains(slug))
        .map(|(_, code)| *code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rot guard: every variant's `Display` slug must map back to its `code()`.
    /// If a `#[error("...")]` message is reworded without updating
    /// `driver_error_code`'s TABLE, this fails. `HttpFailed` is omitted —
    /// `reqwest::Error` has no public constructor — but its slug is in the TABLE.
    #[test]
    fn driver_error_code_matches_display() {
        let variants = [
            DriverError::XcodeMissing,
            DriverError::SimRuntimeMissing { ios_major: 18 },
            DriverError::BuildFailed {
                exit_code: 65,
                log_tail: "tail".into(),
            },
            DriverError::RunnerNotProvisioned {
                hint: "ios18.4".into(),
            },
            DriverError::BuildLockTimeout {
                cache_key: "k".into(),
                waited_secs: 1,
            },
            DriverError::NotReady {
                port: 8200,
                waited_secs: 30,
            },
            DriverError::SessionConflict {
                udid: "u".into(),
                holder_pid: 1,
                holder_port: 8200,
            },
            DriverError::BadResponse {
                status: 500,
                body: "b".into(),
            },
            DriverError::Io("io".into()),
            DriverError::Bug("bug".into()),
        ];
        for v in &variants {
            assert_eq!(
                driver_error_code(&v.to_string()),
                Some(v.code()),
                "Display slug for {v:?} no longer maps to code() — update driver_error_code TABLE"
            );
        }
    }

    #[test]
    fn driver_error_code_none_for_non_driver_text() {
        assert_eq!(driver_error_code("some unrelated failure"), None);
    }
}
