//! Centralized lookup for `~/.drengr/`. Honors the `DRENGR_HOME` env var
//! so tests (and power users) can redirect storage without touching `$HOME`.

use std::path::PathBuf;

/// Returns `~/.drengr/` (or `$DRENGR_HOME` if set). `None` only when both
/// `dirs::home_dir()` and the env var are unset — practically never.
pub fn drengr_dir() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("DRENGR_HOME") {
        if !s.is_empty() {
            return Some(PathBuf::from(s));
        }
    }
    Some(dirs::home_dir()?.join(".drengr"))
}

/// Ensures `~/.drengr/` exists and is 0o700 on Unix.
pub fn ensure_drengr_dir() -> std::io::Result<PathBuf> {
    let dir = drengr_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"))?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(dir)
}

/// `~/.drengr/` (or `$DRENGR_HOME`), falling back to `<fallback>/.drengr` on the
/// practically-impossible machine with no home directory.
pub fn drengr_dir_or(fallback: &str) -> PathBuf {
    drengr_dir().unwrap_or_else(|| PathBuf::from(fallback).join(".drengr"))
}
