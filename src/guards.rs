//! Structural guards. Each of these forks was resolved once; these tests make
//! the fork impossible to reintroduce rather than merely discouraged.
//!
//! Every needle is assembled at runtime, so this file never matches itself.

#[cfg(test)]
mod tests {

    use std::path::{Path, PathBuf};

    fn src_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// Every `src/**/*.rs` line matching `is_violation`, as `src/<rel>:<line>`.
    /// `exempt` holds the paths allowed to own the pattern.
    fn offenders(exempt: &[&str], is_violation: &dyn Fn(&str) -> bool) -> Vec<String> {
        let root = src_root();
        let mut stack = vec![root.clone()];
        let mut hits = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src/ is readable").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&root)
                    .expect("under src/")
                    .to_string_lossy()
                    .replace('\\', "/");
                if exempt.contains(&rel.as_str()) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("rust file is utf-8");
                for (i, line) in text.lines().enumerate() {
                    if is_violation(line) {
                        hits.push(format!("src/{}:{}", rel, i + 1));
                    }
                }
            }
        }
        hits.sort();
        hits
    }

    fn env_read() -> String {
        format!("env::{}(", "var")
    }

    /// `DRENGR_HOME` is honoured only if every path runs through `paths`. A
    /// hand-built home join silently reads and writes a different tree: it made
    /// `uninstall` erase one directory while the license cache survived in
    /// another, and made tests scribble on the developer's real storage.
    #[test]
    fn no_file_builds_the_drengr_directory_by_hand() {
        let dir = format!(".{}", "drengr");
        let needles = [format!("join(\"{}", dir), format!("join(s!(\"{}", dir)];
        let hits = offenders(&["paths.rs"], &|line| {
            needles.iter().any(|n| line.contains(n.as_str()))
        });
        assert!(
            hits.is_empty(),
            "these sites bypass paths::drengr_dir() and so ignore DRENGR_HOME: {:?}",
            hits
        );
    }

    /// A second reader that consults only `ANDROID_HOME` disagrees with
    /// `android_sdk::sdk_root` on any machine exporting only `ANDROID_SDK_ROOT`,
    /// which is what Google's own CI images set.
    #[test]
    fn only_one_module_reads_the_android_sdk_root() {
        let read = env_read();
        let vars = [
            format!("ANDROID_{}", "HOME"),
            format!("ANDROID_{}", "SDK_ROOT"),
        ];
        let hits = offenders(&["transport/android_sdk.rs"], &|line| {
            line.contains(read.as_str()) && vars.iter().any(|v| line.contains(v.as_str()))
        });
        assert!(
            hits.is_empty(),
            "these sites re-read the SDK root instead of calling android_sdk::sdk_root(): {:?}",
            hits
        );
    }

    /// One resolver decides the endpoint. A second call site that reads a
    /// provider default directly would silently ignore DRENGR_BASE_URL.
    #[test]
    fn only_the_llm_client_resolves_its_endpoint() {
        let read = env_read();
        let var = format!("DRENGR_BASE{}", "_URL");
        let hits = offenders(&["ooda/llm/mod.rs"], &|line| {
            line.contains(read.as_str()) && line.contains(var.as_str())
        });
        assert!(
            hits.is_empty(),
            "these sites re-derive the endpoint instead of calling LlmClient::base_url(): {:?}",
            hits
        );
    }

    /// Nothing in the open tree may reach for a module that stayed closed, nor
    /// carry a commercial surface. Needles are assembled so this file never
    /// matches itself.
    #[test]
    fn no_public_file_reaches_for_a_closed_module() {
        let closed = [
            "license",
            "quota",
            "telemetry",
            "rate_limit",
            "fingerprint",
            "supabase",
            "cli_login",
        ];
        let mut needles: Vec<String> = closed.iter().map(|m| format!("crate::{}::", m)).collect();
        needles.push(format!("{}{}", "obf", "str"));
        needles.push(format!("{}{}", "gateway.", "drengr.dev"));
        needles.push(format!("{}{}", "drengr.dev/", "signup"));
        needles.push(format!("{} {}", "Upgrade to", "Pro"));

        for needle in &needles {
            let hits = offenders(&[], &|line: &str| line.contains(needle.as_str()));
            assert!(
                hits.is_empty(),
                "closed surface {:?} reappeared at {:?}",
                needle,
                hits
            );
        }
    }

    /// A rejected action must be recorded, not just logged. Without the history
    /// push the next prompt is byte-identical, so the model re-proposes the same
    /// invalid action every step until the budget is gone. Verified on a device:
    /// the diagnostic bundle went from 1 action to 5 once this landed.
    #[test]
    fn a_rejected_action_is_recorded_in_history() {
        let src = std::fs::read_to_string(src_root().join("ooda/mod.rs")).expect("ooda/mod.rs");
        let arm = src
            .split("Action rejected at step")
            .nth(1)
            .expect("the rejection arm still logs a warning");
        let arm = &arm[..arm.len().min(600)];
        let needle = format!("history{}push", ".");
        assert!(
            arm.contains(needle.as_str()),
            "the rejection arm logs but does not record the step, so the next prompt cannot differ:\n{arm}"
        );
    }
}
