//! Tier 2 diagnostic bundles — written locally on stuck/crash, uploaded
//! ONLY when the user runs `drengr diag share <run_id>`. Everything is
//! redacted client-side via `crate::redact` before it ever touches disk.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::redact;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticBundle {
    pub run_id: String,
    pub platform: String,
    pub outcome: String,
    pub model: String,
    pub provider: String,
    pub version: String,
    pub task_kind: String,
    pub final_activity_kind: String,
    pub step_count: u32,
    pub duration_ms: u32,
    /// Action sequence with text content redacted (only kind + length retained).
    pub action_trail: Vec<DiagnosticStep>,
    /// Last few text scenes, redacted.
    pub recent_scenes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticStep {
    pub step: u32,
    pub action: String,
    pub outcome: String,
    pub screen_changed: bool,
}

pub fn bundles_dir() -> PathBuf {
    crate::paths::drengr_dir_or(".").join("diagnostics")
}

pub fn bundle_path(run_id: &str) -> PathBuf {
    bundles_dir().join(format!("{}.json", run_id))
}

/// Write a redacted bundle to disk. Returns the path so callers can show it.
pub fn write_bundle(bundle: &DiagnosticBundle) -> Result<PathBuf> {
    let dir = bundles_dir();
    std::fs::create_dir_all(&dir).context("create diagnostics dir")?;
    let path = bundle_path(&bundle.run_id);
    let payload = serde_json::to_vec_pretty(bundle).context("serialize bundle")?;
    std::fs::write(&path, &payload).context("write bundle file")?;
    Ok(path)
}

/// List run_ids of bundles on disk, newest first.
pub fn list_bundles() -> Result<Vec<PathBuf>> {
    let dir = bundles_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .context("read diagnostics dir")?
        .filter_map(|r| r.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    out.sort_by(|a, b| {
        let am = std::fs::metadata(a).and_then(|m| m.modified()).ok();
        let bm = std::fs::metadata(b).and_then(|m| m.modified()).ok();
        bm.cmp(&am)
    });
    Ok(out)
}

/// Load a bundle by run_id.
pub fn load_bundle(run_id: &str) -> Result<DiagnosticBundle> {
    let path = bundle_path(run_id);
    let bytes = std::fs::read(&path).with_context(|| format!("no bundle at {}", path.display()))?;
    serde_json::from_slice(&bytes).context("deserialize bundle")
}

/// Build a redacted bundle from an OODA history slice.
pub fn build_from_history(
    run_id: &str,
    platform: &str,
    outcome: &str,
    model: &str,
    provider: &str,
    version: &str,
    task_kind: &str,
    final_activity_kind: &str,
    step_count: u32,
    duration_ms: u32,
    history: &[crate::ooda::OodaStepSummary],
    recent_scenes: &[&str],
) -> DiagnosticBundle {
    let action_trail = history
        .iter()
        .map(|s| DiagnosticStep {
            step: s.step as u32,
            action: redact_action(&s.action),
            outcome: redact::redact(&s.outcome),
            screen_changed: s.screen_changed,
        })
        .collect();
    let recent_scenes = recent_scenes.iter().map(|s| redact::redact(s)).collect();
    DiagnosticBundle {
        run_id: run_id.to_string(),
        platform: platform.to_string(),
        outcome: outcome.to_string(),
        model: model.to_string(),
        provider: provider.to_string(),
        version: version.to_string(),
        task_kind: task_kind.to_string(),
        final_activity_kind: final_activity_kind.to_string(),
        step_count,
        duration_ms,
        action_trail,
        recent_scenes,
    }
}

/// Redact any user-typed text inside a `Typed "..."` action description.
fn redact_action(action: &str) -> String {
    if let Some(rest) = action.strip_prefix("Typed \"") {
        if let Some(end) = rest.rfind('"') {
            let typed = &rest[..end];
            return format!("Typed {}", redact::summarize_typed(typed));
        }
    }
    redact::redact(action)
}

/// Upload a bundle to Supabase. Used by the `drengr diag share <run_id>` CLI.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_action_strips_typed_text() {
        let a = redact_action(r#"Typed "user@example.com""#);
        assert!(!a.contains("user@example.com"));
        assert!(a.contains("emaillike"));
    }

    #[test]
    fn redact_action_runs_general_redact_on_other_actions() {
        let a = redact_action("Tapped #5 (Login as user@example.com)");
        assert!(!a.contains("user@example.com"));
        assert!(a.contains("<email>"));
    }

    #[test]
    fn build_from_history_redacts_scenes() {
        let history = vec![crate::ooda::OodaStepSummary {
            step: 1,
            action: r#"Typed "user@example.com""#.to_string(),
            outcome: "Email sent to user@example.com".to_string(),
            screen_changed: true,
        }];
        let scenes = vec!["Screen showing user@example.com"];
        let b = build_from_history(
            "rid",
            "ios",
            "judge_pass",
            "qwen2.5vl:7b",
            "ollama",
            "0.3.0",
            "open_app",
            "foreground_app",
            1,
            1000,
            &history,
            &scenes,
        );
        assert_eq!(b.action_trail.len(), 1);
        assert!(!b.action_trail[0].action.contains("user@example.com"));
        assert!(!b.action_trail[0].outcome.contains("user@example.com"));
        assert!(!b.recent_scenes[0].contains("user@example.com"));
    }
}
