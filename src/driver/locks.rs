//! Per-UDID and per-cache-key advisory locks for the Drengr Runner driver.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::{DriverError, RUNNER_VERSION};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockPayload {
    pub pid: u32,
    pub port: u16,
    pub udid: String,
    pub started_at: String,
    pub drengr_version: String,
}

#[derive(Debug)]
pub struct UdidLock {
    file: File,
}

impl UdidLock {
    pub fn try_acquire(udid: &str, port: u16) -> crate::driver::Result<Self> {
        let path = flock_path_under(".locks", udid)?;
        let mut file = open_lock_file(&path)?;

        match file.try_lock_exclusive() {
            Ok(()) => {
                file.seek(SeekFrom::Start(0))?;
                file.set_len(0)?;

                let payload = LockPayload {
                    pid: std::process::id(),
                    port,
                    udid: udid.to_string(),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    drengr_version: RUNNER_VERSION.to_string(),
                };
                let json = serde_json::to_vec_pretty(&payload)
                    .map_err(|e| DriverError::Io(format!("serialize lock payload: {e}")))?;
                file.write_all(&json)?;
                file.sync_data()?;

                Ok(Self { file })
            }
            Err(_contended) => {
                let mut existing = String::new();
                let _ = file.read_to_string(&mut existing);
                let holder: Option<LockPayload> = serde_json::from_str(&existing).ok();
                Err(DriverError::SessionConflict {
                    udid: udid.to_string(),
                    holder_pid: holder.as_ref().map(|h| h.pid).unwrap_or(0),
                    holder_port: holder.as_ref().map(|h| h.port).unwrap_or(0),
                })
            }
        }
    }
}

impl Drop for UdidLock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug)]
pub struct CacheKeyLock {
    file: File,
}

impl CacheKeyLock {
    pub async fn try_acquire(cache_key: &str, timeout: Duration) -> crate::driver::Result<Self> {
        let path = flock_path_under(".buildlocks", cache_key)?;
        let file = open_lock_file(&path)?;

        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Self { file }),
                Err(_) => {
                    if Instant::now() >= deadline {
                        return Err(DriverError::BuildLockTimeout {
                            cache_key: cache_key.to_string(),
                            waited_secs: timeout.as_secs(),
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }
}

impl Drop for CacheKeyLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn flock_path_under(subdir: &str, name: &str) -> crate::driver::Result<PathBuf> {
    let base =
        crate::paths::drengr_dir().ok_or_else(|| DriverError::Io("no home directory".into()))?;
    Ok(base
        .join("runner")
        .join(subdir)
        .join(format!("{name}.lock")))
}

fn open_lock_file(path: &PathBuf) -> crate::driver::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials;

    /// Pin DRENGR_HOME to a unique tempdir for the duration of one test.
    /// Other tests (key_store, telemetry) mutate DRENGR_HOME under the
    /// process-wide credentials::test_lock(); without joining that lock our
    /// lock-file fds can be invalidated mid-test when their tempdir is dropped.
    fn fresh_drengr_home() -> (std::sync::MutexGuard<'static, ()>, tempfile::TempDir) {
        let guard = credentials::test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("DRENGR_HOME", tmp.path());
        (guard, tmp)
    }

    #[test]
    fn acquire_release_round_trip() {
        let (_guard, _tmp) = fresh_drengr_home();
        let udid = "TEST-driver-locks-acquire-release";
        let lock = UdidLock::try_acquire(udid, 9100).expect("first acquire");
        drop(lock);
        let lock2 = UdidLock::try_acquire(udid, 9101).expect("second acquire");
        drop(lock2);
    }

    #[test]
    fn double_acquire_is_conflict() {
        let (_guard, _tmp) = fresh_drengr_home();
        let udid = "TEST-driver-locks-double-acquire";
        let _held = UdidLock::try_acquire(udid, 9200).expect("first acquire");
        let err = UdidLock::try_acquire(udid, 9201).expect_err("second should conflict");
        match err {
            DriverError::SessionConflict { holder_pid, .. } => {
                assert_eq!(holder_pid, std::process::id());
            }
            other => panic!("expected SessionConflict, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn cache_key_lock_timeout() {
        let (_guard, _tmp) = fresh_drengr_home();
        let key = "TEST-driver-cache-key-timeout";
        let _held = CacheKeyLock::try_acquire(key, Duration::from_millis(100))
            .await
            .expect("first acquire");
        let start = Instant::now();
        let err = CacheKeyLock::try_acquire(key, Duration::from_millis(200))
            .await
            .expect_err("second should timeout");
        assert!(start.elapsed() >= Duration::from_millis(200));
        match err {
            DriverError::BuildLockTimeout {
                cache_key,
                waited_secs,
            } => {
                assert_eq!(cache_key, key);
                assert_eq!(waited_secs, 0);
            }
            other => panic!("expected BuildLockTimeout, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn cache_key_lock_release_round_trip() {
        let (_guard, _tmp) = fresh_drengr_home();
        let key = "TEST-driver-cache-key-release";
        let lock = CacheKeyLock::try_acquire(key, Duration::from_millis(100))
            .await
            .expect("first acquire");
        drop(lock);
        let lock2 = CacheKeyLock::try_acquire(key, Duration::from_millis(100))
            .await
            .expect("second acquire after release");
        drop(lock2);
    }
}
