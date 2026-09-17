//! Tier 1 run-outcome telemetry — anonymous aggregate of how each OODA run ended.
//! No raw task strings, no UI text, no screenshots, no package/activity ids.

use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcomeKind {
    JudgePass,
    /// The model said it was finished and no judge was available to check.
    /// Distinct from JudgePass so a self-report is never counted as verified.
    SelfReported,
    Timeout,
    StepCap,
    Crash,
    ProgressStuck,
    DuplicateScreen,
    UserKilled,
    Error,
}

impl RunOutcomeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::JudgePass => "judge_pass",
            Self::SelfReported => "self_reported",
            Self::Timeout => "timeout",
            Self::StepCap => "step_cap",
            Self::Crash => "crash",
            Self::ProgressStuck => "progress_stuck",
            Self::DuplicateScreen => "duplicate_screen",
            Self::UserKilled => "user_killed",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressTrigger {
    ScrollSaturation,
    KeywordAbsent,
    DeadEnd,
    MetaLlmStuck,
}

impl ProgressTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ScrollSaturation => "scroll_saturation",
            Self::KeywordAbsent => "keyword_absent",
            Self::DeadEnd => "dead_end",
            Self::MetaLlmStuck => "meta_llm_stuck",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    OpenApp,
    TapTarget,
    TypeInput,
    Navigate,
    Draw,
    Mixed,
    Unknown,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenApp => "open_app",
            Self::TapTarget => "tap_target",
            Self::TypeInput => "type_input",
            Self::Navigate => "navigate",
            Self::Draw => "draw",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }

    /// Classify by action distribution. Local-only; never sees the raw task string.
    pub fn from_histogram(hist: &HashMap<String, u32>) -> Self {
        let total: u32 = hist.values().sum();
        if total == 0 {
            return Self::Unknown;
        }
        let map_one = |action: &str| match action {
            "open_app" => Self::OpenApp,
            "tap" => Self::TapTarget,
            "type" => Self::TypeInput,
            "scroll" => Self::Navigate,
            "draw_path" => Self::Draw,
            _ => Self::Unknown,
        };
        if hist.len() == 1 {
            return map_one(hist.keys().next().unwrap());
        }
        let (top, top_count) = hist.iter().max_by_key(|(_, c)| *c).unwrap();
        let pct = (*top_count as f32) / (total as f32);
        if pct >= 0.6 {
            return map_one(top);
        }
        Self::Mixed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalActivityKind {
    HomeScreen,
    ForegroundApp,
    CrashScreen,
    Unknown,
}

impl FinalActivityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HomeScreen => "home_screen",
            Self::ForegroundApp => "foreground_app",
            Self::CrashScreen => "crash_screen",
            Self::Unknown => "unknown",
        }
    }

    /// Classify locally from the raw activity. Never logs the raw value.
    pub fn from_activity(activity: &str, app_package: &str) -> Self {
        let a = activity.to_lowercase();
        if a.is_empty() || a == "unknown" {
            return Self::Unknown;
        }
        if a.contains("springboard") || a.contains("launcher") || a.contains("nexuslauncher") {
            return Self::HomeScreen;
        }
        if a.contains("crash") || a.contains("anr") {
            return Self::CrashScreen;
        }
        if !app_package.is_empty() && activity.starts_with(app_package) {
            return Self::ForegroundApp;
        }
        Self::ForegroundApp
    }
}

/// Accumulator built up during a run; emit once at any exit point.
pub struct RunOutcomeBuilder {
    pub run_id: String,
    pub started_at: Instant,
    pub platform: String,
    pub model: String,
    pub provider: String,
    pub action_histogram: HashMap<String, u32>,
    pub judge_fired: bool,
    pub judge_verdict: Option<bool>,
    pub progress_detector_fired: bool,
    pub progress_trigger: Option<ProgressTrigger>,
}

impl RunOutcomeBuilder {
    pub fn new(platform: &str, model: &str, provider: &str) -> Self {
        Self {
            run_id: uuid::Uuid::new_v4().to_string(),
            started_at: Instant::now(),
            platform: platform.to_string(),
            model: model.to_string(),
            provider: provider.to_string(),
            action_histogram: HashMap::new(),
            judge_fired: false,
            judge_verdict: None,
            progress_detector_fired: false,
            progress_trigger: None,
        }
    }

    pub fn record_action(&mut self, action_name: &str) {
        *self
            .action_histogram
            .entry(action_name.to_string())
            .or_insert(0) += 1;
    }

    pub fn record_judge(&mut self, verdict: bool) {
        self.judge_fired = true;
        self.judge_verdict = Some(verdict);
    }

    pub fn record_progress(&mut self, trigger: ProgressTrigger) {
        self.progress_detector_fired = true;
        self.progress_trigger = Some(trigger);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_kind_single_action() {
        let mut h = HashMap::new();
        h.insert("open_app".to_string(), 1);
        assert_eq!(TaskKind::from_histogram(&h), TaskKind::OpenApp);
    }

    #[test]
    fn task_kind_dominant_plurality() {
        let mut h = HashMap::new();
        h.insert("tap".to_string(), 7);
        h.insert("scroll".to_string(), 1);
        assert_eq!(TaskKind::from_histogram(&h), TaskKind::TapTarget);
    }

    #[test]
    fn task_kind_mixed_when_no_plurality() {
        let mut h = HashMap::new();
        h.insert("tap".to_string(), 3);
        h.insert("scroll".to_string(), 3);
        h.insert("type".to_string(), 2);
        assert_eq!(TaskKind::from_histogram(&h), TaskKind::Mixed);
    }

    #[test]
    fn task_kind_empty_is_unknown() {
        let h = HashMap::new();
        assert_eq!(TaskKind::from_histogram(&h), TaskKind::Unknown);
    }

    #[test]
    fn final_activity_classifies_home() {
        assert_eq!(
            FinalActivityKind::from_activity("com.apple.springboard", "com.apple.mobilesafari"),
            FinalActivityKind::HomeScreen
        );
        assert_eq!(
            FinalActivityKind::from_activity("com.android.launcher3", ""),
            FinalActivityKind::HomeScreen
        );
    }

    #[test]
    fn final_activity_unknown_for_empty() {
        assert_eq!(
            FinalActivityKind::from_activity("", ""),
            FinalActivityKind::Unknown
        );
        assert_eq!(
            FinalActivityKind::from_activity("unknown", ""),
            FinalActivityKind::Unknown
        );
    }

    #[test]
    fn final_activity_foreground_for_target_app() {
        assert_eq!(
            FinalActivityKind::from_activity("com.example.app/.MainActivity", "com.example.app"),
            FinalActivityKind::ForegroundApp
        );
    }

    #[test]
    fn builder_records_actions_and_emits_via_fire() {
        let mut b = RunOutcomeBuilder::new("ios", "qwen2.5vl:7b", "ollama");
        b.record_action("tap");
        b.record_action("tap");
        b.record_action("scroll");
        b.record_judge(true);
        b.record_progress(ProgressTrigger::ScrollSaturation);
        assert_eq!(b.action_histogram.get("tap").copied(), Some(2));
        assert!(b.judge_fired);
        assert!(b.progress_detector_fired);
        // Don't actually emit in tests (no running tokio runtime guaranteed).
    }
}
