//! Per-process UDID → Runner port allocator.

use std::collections::{HashMap, HashSet};
use std::net::TcpListener;
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::{DriverError, RUNNER_DEFAULT_PORT};

static REGISTRY: OnceLock<Mutex<HashMap<String, u16>>> = OnceLock::new();

fn registry() -> MutexGuard<'static, HashMap<String, u16>> {
    let mu = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    mu.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn allocate(udid: &str) -> crate::driver::Result<u16> {
    let mut reg = registry();

    if let Some(&p) = reg.get(udid) {
        return Ok(p);
    }

    let env_port = std::env::var("DRENGR_RUNNER_PORT").ok();
    let port = if reg.is_empty() && env_port.is_some() {
        env_port
            .unwrap()
            .parse::<u16>()
            .map_err(|e| DriverError::Io(format!("DRENGR_RUNNER_PORT invalid: {e}")))?
    } else {
        let taken: HashSet<u16> = reg.values().copied().collect();
        find_next_free_port(RUNNER_DEFAULT_PORT, &taken)?
    };

    reg.insert(udid.to_string(), port);
    Ok(port)
}

fn find_next_free_port(start: u16, taken: &HashSet<u16>) -> crate::driver::Result<u16> {
    let end = start.saturating_add(100);
    for port in start..end {
        if taken.contains(&port) {
            continue;
        }
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(DriverError::Io(format!(
        "no free localhost port in range {start}..{end}"
    )))
}

pub fn release(udid: &str) {
    registry().remove(udid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        match ENV_MUTEX.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn allocate_returns_same_port_for_same_udid() {
        let _guard = lock_env();
        std::env::remove_var("DRENGR_RUNNER_PORT");
        let udid = "TEST-driver-port-registry-stable";
        release(udid);
        let p1 = allocate(udid).expect("first allocate");
        let p2 = allocate(udid).expect("second allocate");
        assert_eq!(p1, p2, "repeated allocate should return cached port");
        release(udid);
    }

    #[test]
    fn allocate_different_udids_get_different_ports() {
        let _guard = lock_env();
        std::env::remove_var("DRENGR_RUNNER_PORT");
        let a = "TEST-driver-port-registry-a";
        let b = "TEST-driver-port-registry-b";
        release(a);
        release(b);
        let pa = allocate(a).expect("allocate a");
        let pb = allocate(b).expect("allocate b");
        assert_ne!(pa, pb);
        release(a);
        release(b);
    }

    #[test]
    fn env_override_wins_over_scan() {
        let _guard = lock_env();
        let udid = "TEST-driver-port-registry-env";
        release(udid);
        std::env::set_var("DRENGR_RUNNER_PORT", "54322");
        let p = allocate(udid).expect("allocate with env override");
        std::env::remove_var("DRENGR_RUNNER_PORT");
        release(udid);
        assert_eq!(p, 54322);
    }

    #[test]
    fn env_override_only_affects_first_allocation() {
        let _guard = lock_env();
        let a = "TEST-driver-port-registry-env-first";
        let b = "TEST-driver-port-registry-env-second";
        release(a);
        release(b);
        std::env::set_var("DRENGR_RUNNER_PORT", "54323");
        let pa = allocate(a).expect("first allocate honors env");
        let pb = allocate(b).expect("second allocate scans");
        std::env::remove_var("DRENGR_RUNNER_PORT");
        release(a);
        release(b);
        assert_eq!(pa, 54323, "first allocation should match env override");
        assert_ne!(pb, 54323, "second allocation must not reuse env port");
    }

    #[test]
    fn default_port_starts_at_runner_default() {
        let _guard = lock_env();
        std::env::remove_var("DRENGR_RUNNER_PORT");
        let udid = "TEST-driver-port-registry-default";
        release(udid);
        let p = allocate(udid).expect("allocate default");
        assert!(
            p >= RUNNER_DEFAULT_PORT,
            "port {} should be >= {}",
            p,
            RUNNER_DEFAULT_PORT
        );
        release(udid);
    }
}
