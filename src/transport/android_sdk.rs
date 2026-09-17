//! The one resolver for the Android SDK root.
//!
//! `ANDROID_SDK_ROOT` wins over the older `ANDROID_HOME`: Google's own CI
//! images export only the former, and a second reader that consults just
//! `ANDROID_HOME` silently disagrees with this one about where the SDK is.

use std::path::PathBuf;

pub fn sdk_root() -> Option<PathBuf> {
    for var in ["ANDROID_SDK_ROOT", "ANDROID_HOME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                return Some(PathBuf::from(v));
            }
        }
    }
    None
}

/// The resolved root as a string, for surfaces that pass it on as an env value.
pub fn sdk_root_env() -> Option<String> {
    sdk_root().map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialized against the other env-mutating tests by running both vars in
    /// one test: process env is global and `cargo test` threads share it.
    #[test]
    fn sdk_root_prefers_sdk_root_over_home_and_ignores_empty() {
        let prev_root = std::env::var("ANDROID_SDK_ROOT").ok();
        let prev_home = std::env::var("ANDROID_HOME").ok();

        std::env::set_var("ANDROID_SDK_ROOT", "/from-sdk-root");
        std::env::set_var("ANDROID_HOME", "/from-home");
        assert_eq!(sdk_root(), Some(PathBuf::from("/from-sdk-root")));

        std::env::set_var("ANDROID_SDK_ROOT", "");
        assert_eq!(
            sdk_root(),
            Some(PathBuf::from("/from-home")),
            "a set-but-empty var must fall through, not win"
        );

        std::env::remove_var("ANDROID_SDK_ROOT");
        std::env::remove_var("ANDROID_HOME");
        assert_eq!(sdk_root(), None);

        match prev_root {
            Some(v) => std::env::set_var("ANDROID_SDK_ROOT", v),
            None => std::env::remove_var("ANDROID_SDK_ROOT"),
        }
        match prev_home {
            Some(v) => std::env::set_var("ANDROID_HOME", v),
            None => std::env::remove_var("ANDROID_HOME"),
        }
    }
}
