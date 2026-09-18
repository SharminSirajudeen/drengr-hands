//! RAII handle on a launched drengr-runner xctrunner process.

use std::process::Command;

use super::client::DriverClient;
use super::locks::UdidLock;
use super::port_registry;

pub struct LaunchedRunner {
    pub udid: String,
    pub port: u16,
    pub pid: u32,
    pub client: DriverClient,
    /// The `xcodebuild test-without-building` child process. Kept alive
    /// because it blocks for the lifetime of the xctrunner inside the sim;
    /// killing it propagates SIGTERM down to the xctrunner.
    pub(crate) child: Option<tokio::process::Child>,
    /// Never read: held so the UDID stays claimed for the runner's lifetime
    /// and is released by `Drop` at the same moment the process goes away.
    #[allow(dead_code)]
    pub(crate) lock: UdidLock,
}

impl LaunchedRunner {
    pub fn is_alive(&self) -> bool {
        if self.pid == 0 {
            return false;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(self.pid as i32, 0) == 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}

impl Drop for LaunchedRunner {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        port_registry::release(&self.udid);
        let _ = Command::new("xcrun")
            .args(["simctl", "terminate", &self.udid, super::RUNNER_BUNDLE_ID])
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials;
    use crate::driver::client::DriverClient;
    use crate::driver::locks::UdidLock;

    #[test]
    fn is_alive_returns_true_for_self_pid() {
        let _guard = credentials::test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("DRENGR_HOME", tmp.path());
        let pid = std::process::id();
        let port = 18299;
        let udid = format!("test-udid-{}", pid);
        let lock = UdidLock::try_acquire(&udid, port).expect("acquire lock");
        let runner = LaunchedRunner {
            udid: udid.clone(),
            port,
            pid,
            client: DriverClient::new(port),
            child: None,
            lock,
        };
        assert!(runner.is_alive());
    }
}
