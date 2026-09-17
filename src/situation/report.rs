use serde::Serialize;

/// The situation report — what changed after an action.
/// This is the core output of the ORIENT phase.
#[derive(Debug, Clone, Serialize)]
pub struct SituationReport {
    /// Step number (1-based, increments on each drengr_do).
    pub step: usize,

    /// Human-readable description of the action taken.
    pub action: String,

    /// Did the screen content change?
    pub screen_changed: bool,

    /// Current activity/screen name.
    pub activity: String,

    /// Did the activity change (navigation)?
    pub activity_changed: bool,

    /// Did the app crash?
    pub crash: bool,

    /// Is the screen unchanged (agent might be stuck)?
    pub stuck: bool,

    /// Elements that appeared on screen.
    pub new_elements: Vec<String>,

    /// Elements that disappeared from screen.
    pub disappeared_elements: Vec<String>,

    /// Whether the current screen is scrollable.
    pub scrollable: bool,

    /// True when the dump failed, so the diff fields say nothing rather than
    /// claiming a change nobody observed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub tree_unavailable: bool,
    /// Elements Drengr will address on this screen (see `interactive_count` for
    /// the tappable subset).
    pub element_count: usize,
    /// The tappable subset of `element_count`. Kept separate because "how many
    /// things can I read" and "how many can I tap" are different questions.
    pub interactive_count: usize,

    /// Optional one-line tip suggesting a more efficient action. Populated by
    /// the HintEngine; absent on most responses (per-session dedup, conditional
    /// triggers). Skipped from JSON when None to keep responses lean.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl SituationReport {
    /// Convert to JSON value for MCP response.
    /// Excludes `step` and `action` (those go at the top level of drengr_do responses).
    pub fn to_json(&self) -> serde_json::Value {
        let mut val = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        // Remove fields that belong at the top level of drengr_do, not inside "situation"
        if let Some(obj) = val.as_object_mut() {
            obj.remove("step");
            obj.remove("action");
        }
        val
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> SituationReport {
        SituationReport {
            step: 1,
            action: "Tapped #1 (Login)".to_string(),
            screen_changed: true,
            activity: "DashboardActivity".to_string(),
            activity_changed: true,
            crash: false,
            stuck: false,
            new_elements: vec!["Welcome".to_string(), "Profile".to_string()],
            disappeared_elements: vec!["Login".to_string()],
            scrollable: false,
            tree_unavailable: false,
            element_count: 5,
            interactive_count: 5,
            hint: None,
        }
    }

    #[test]
    fn test_to_json_contains_all_fields() {
        let report = sample_report();
        let json = report.to_json();

        assert_eq!(json["screen_changed"], true);
        assert_eq!(json["activity"], "DashboardActivity");
        assert_eq!(json["activity_changed"], true);
        assert_eq!(json["crash"], false);
        assert_eq!(json["stuck"], false);
        assert_eq!(json["scrollable"], false);
        assert_eq!(json["element_count"], 5);

        let new = json["new_elements"].as_array().unwrap();
        assert_eq!(new.len(), 2);
        assert_eq!(new[0], "Welcome");

        let gone = json["disappeared_elements"].as_array().unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0], "Login");
    }

    #[test]
    fn test_serialize_roundtrip() {
        let report = sample_report();
        let json_str = serde_json::to_string(&report).unwrap();
        assert!(json_str.contains("DashboardActivity"));
        assert!(json_str.contains("Welcome"));
    }
}
