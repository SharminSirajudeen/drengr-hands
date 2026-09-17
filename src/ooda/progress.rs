//! Deterministic + meta-LLM progress detection.
//!
//! Cheap signals first (keyword scan, scroll-direction counter); the meta-LLM
//! escalation in `LlmClient::check_progress` only fires when no determinstic
//! hint applies and the goal-judge has not already weighed in.

use std::collections::{HashMap, VecDeque};

const SCENE_WINDOW: usize = 5;
const SCROLL_LOOP_THRESHOLD: u32 = 4;
const META_LLM_STEP_GATE: usize = 6;

pub struct ProgressDetector {
    task_keywords: Vec<String>,
    scroll_counts_by_dir: HashMap<String, u32>,
    recent_scenes: VecDeque<String>,
}

impl ProgressDetector {
    pub fn new(task: &str) -> Self {
        let mut keywords: Vec<String> = task
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '.')
            .flat_map(|t| t.split("and"))
            .flat_map(|t| t.split("then"))
            .map(|t| t.trim().to_lowercase())
            .filter(|t| t.len() > 3)
            .collect();
        keywords.sort();
        keywords.dedup();
        Self {
            task_keywords: keywords,
            scroll_counts_by_dir: HashMap::new(),
            recent_scenes: VecDeque::with_capacity(SCENE_WINDOW),
        }
    }

    pub fn observe(&mut self, scene: &str, last_action: &str) {
        if self.recent_scenes.len() == SCENE_WINDOW {
            self.recent_scenes.pop_front();
        }
        self.recent_scenes.push_back(scene.to_string());
        if let Some(rest) = last_action.strip_prefix("Scrolled ") {
            if let Some(dir) = rest.split_whitespace().next() {
                *self
                    .scroll_counts_by_dir
                    .entry(dir.to_string())
                    .or_insert(0) += 1;
            }
        }
    }

    /// True when any task keyword appears in the last 3 observed scenes —
    /// cheap evidence that the agent is on track.
    fn keyword_recently_visible(&self) -> bool {
        let last_three: Vec<&String> = self.recent_scenes.iter().rev().take(3).collect();
        let lowered: Vec<String> = last_three.iter().map(|s| s.to_lowercase()).collect();
        self.task_keywords
            .iter()
            .any(|kw| lowered.iter().any(|s| s.contains(kw)))
    }

    pub fn deterministic_hint(&self) -> Option<String> {
        if self.keyword_recently_visible() {
            return None;
        }
        for (dir, count) in &self.scroll_counts_by_dir {
            if *count >= SCROLL_LOOP_THRESHOLD {
                return Some(format!(
                    "Scroll loop detected: scrolled {} {} times without finding the target. Try a different approach.",
                    dir, count
                ));
            }
        }
        None
    }

    pub fn should_escalate_to_meta_llm(&self, step: usize, judge_fired: bool) -> bool {
        step >= META_LLM_STEP_GATE
            && !judge_fired
            && !self.keyword_recently_visible()
            && self.deterministic_hint().is_none()
    }

    /// Returns the most-recent N scenes oldest-first for prompting.
    pub fn recent_scenes(&self) -> Vec<&str> {
        self.recent_scenes.iter().map(String::as_str).collect()
    }

    /// Read-only view of the per-direction scroll counter, for diagnostics.
    pub fn scroll_counts(&self) -> &HashMap<String, u32> {
        &self.scroll_counts_by_dir
    }

    /// Whether any task keyword has been visible recently — used by the OODA
    /// loop to attribute progress hints accurately in logs.
    pub fn keyword_visible(&self) -> bool {
        self.keyword_recently_visible()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_detector_counts_scroll_directions() {
        let mut d = ProgressDetector::new("find an item somewhere");
        d.observe("scene1", "Tapped #1");
        d.observe("scene2", "Scrolled down");
        d.observe("scene3", "Scrolled down");
        d.observe("scene4", "Scrolled up");
        assert_eq!(d.scroll_counts_by_dir.get("down").copied(), Some(2));
        assert_eq!(d.scroll_counts_by_dir.get("up").copied(), Some(1));
    }

    #[test]
    fn progress_detector_keyword_suppresses_escalation() {
        let mut d = ProgressDetector::new("find playlist named summer");
        // A scene mentioning a task keyword evidences progress.
        d.observe("Header: Playlist results", "Tapped #1");
        assert!(d.deterministic_hint().is_none());
        assert!(!d.should_escalate_to_meta_llm(10, false));
    }

    #[test]
    fn progress_detector_escalates_at_step_6_when_no_keywords() {
        // Task has only short tokens (<=3 chars) → no keywords → can't suppress.
        let mut d = ProgressDetector::new("do it now");
        d.observe("Generic header", "Tapped #1");
        assert!(d.deterministic_hint().is_none());
        assert!(!d.should_escalate_to_meta_llm(5, false));
        assert!(d.should_escalate_to_meta_llm(6, false));
        // judge_fired blocks escalation.
        assert!(!d.should_escalate_to_meta_llm(6, true));
    }

    #[test]
    fn progress_detector_scroll_loop_returns_hint() {
        let mut d = ProgressDetector::new("locate something elsewhere");
        for _ in 0..4 {
            d.observe("same scene", "Scrolled down");
        }
        let hint = d.deterministic_hint().expect("expected scroll loop hint");
        assert!(hint.contains("Scroll loop"));
        assert!(hint.contains("down"));
    }
}
