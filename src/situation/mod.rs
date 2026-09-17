pub mod hints;
pub mod report;

use std::collections::{HashMap, HashSet};

use crate::screen::ui_element::UiElement;
use hints::{HintContext, HintEngine};
use report::SituationReport;

/// What a single observation saw. Grouped because these four always travel
/// together and describe one screen, not four unrelated parameters.
pub struct ObservedScreen<'a> {
    pub activity: &'a str,
    pub package: &'a str,
    pub elements: &'a [UiElement],
    /// False when the dump failed. The diff is then skipped entirely rather than
    /// comparing against nothing and calling the result a change.
    pub tree_available: bool,
}

/// Tracks per-device state for generating situation reports.
/// This is the ORIENT phase of OODA — tells the LLM what CHANGED.
pub struct SituationEngine {
    devices: HashMap<String, DeviceState>,
    /// Per-process hint engine — fires at most once per HintId for the
    /// lifetime of this engine. Reads DRENGR_HINTS=off at construction.
    hint_engine: HintEngine,
}

/// Tracked state for a single device.
struct DeviceState {
    step_count: usize,
    last_activity: Option<String>,
    last_element_texts: Vec<String>,
    last_screen_hash: Option<u64>,
    /// Number of consecutive steps where `stuck=true`. Resets when the screen
    /// changes. Used by HintEngine to detect "tap-loop" patterns.
    consecutive_stuck_steps: usize,
}

impl SituationEngine {
    pub fn new() -> Self {
        Self {
            devices: HashMap::new(),
            hint_engine: HintEngine::new(),
        }
    }

    /// Record initial observation (from drengr_look). Stores baseline state.
    pub fn observe(&mut self, device_id: &str, activity: &str, elements: &[UiElement]) {
        let state = self
            .devices
            .entry(device_id.to_string())
            .or_insert_with(|| DeviceState {
                step_count: 0,
                last_activity: None,
                last_element_texts: Vec::new(),
                last_screen_hash: None,
                consecutive_stuck_steps: 0,
            });

        state.last_activity = Some(activity.to_string());
        state.last_element_texts = extract_element_texts(elements);
        state.last_screen_hash = Some(hash_elements(elements));
    }

    /// Generate a situation report after an action (from drengr_do).
    /// Compares current state against previous state and returns the diff.
    pub fn report_after_action(
        &mut self,
        device_id: &str,
        action_name: &str,
        action_description: &str,
        screen: ObservedScreen<'_>,
    ) -> SituationReport {
        let ObservedScreen {
            activity: current_activity,
            package: current_package,
            elements: current_elements,
            tree_available,
        } = screen;
        let state = self
            .devices
            .entry(device_id.to_string())
            .or_insert_with(|| DeviceState {
                step_count: 0,
                last_activity: None,
                last_element_texts: Vec::new(),
                last_screen_hash: None,
                consecutive_stuck_steps: 0,
            });

        state.step_count += 1;
        let step = state.step_count;

        // Compute diffs
        let activity_changed = state
            .last_activity
            .as_deref()
            .map(|last| last != current_activity)
            .unwrap_or(true);

        let current_texts = extract_element_texts(current_elements);
        let (new_elements, disappeared_elements) =
            diff_element_texts(&state.last_element_texts, &current_texts);

        let current_hash = hash_elements(current_elements);
        let screen_changed = state
            .last_screen_hash
            .map(|last| last != current_hash)
            .unwrap_or(true);

        let stuck = !screen_changed && step > 1;
        if stuck {
            state.consecutive_stuck_steps += 1;
        } else {
            state.consecutive_stuck_steps = 0;
        }
        let consecutive_stuck = state.consecutive_stuck_steps;

        let crash = detect_crash(current_elements, current_package);

        let scrollable = current_elements.iter().any(|e| e.scrollable);

        let mut report = SituationReport {
            step,
            action: action_description.to_string(),
            screen_changed,
            activity: current_activity.to_string(),
            activity_changed,
            crash,
            stuck,
            new_elements,
            disappeared_elements,
            scrollable,
            tree_unavailable: false,
            // Matches drengr_look's element_count (and the docs, which read
            // element_count == 0 as "no tree"). Counting only clickables meant
            // a screen full of readable text reported 0 and looked treeless.
            element_count: current_elements.iter().filter(|e| e.is_relevant()).count(),
            interactive_count: current_elements
                .iter()
                .filter(|e| e.is_interactive())
                .count(),
            hint: None,
        };

        // Nothing was observed, so nothing can be claimed about what changed.
        if !tree_available {
            report.screen_changed = false;
            report.activity_changed = false;
            report.stuck = false;
            report.new_elements.clear();
            report.disappeared_elements.clear();
            report.tree_unavailable = true;
        }

        // Evaluate contextual hint AFTER report is built (engine inspects fields).
        // `action_name` is the canonical action string the caller already matched on
        // (e.g. "tap", "launch_app") — passed through directly, no string parsing.
        let ctx = HintContext {
            last_action_name: Some(action_name),
            consecutive_stuck_steps: consecutive_stuck,
            last_action_was_launch_app: action_name == "launch_app",
        };
        report.hint = self.hint_engine.evaluate(&report, &ctx).map(|h| h.text);

        // A failed dump is not an empty screen. Writing it as the baseline made one
        // flaky dump produce two confident wrong reports: everything disappeared,
        // then everything is new. Skip the write and let the next good observation
        // diff against the last real one.
        if tree_available {
            state.last_activity = Some(current_activity.to_string());
            state.last_element_texts = current_texts;
            state.last_screen_hash = Some(current_hash);
        }

        report
    }

    /// Get the current step count for a device.
    pub fn step_count(&self, device_id: &str) -> usize {
        self.devices
            .get(device_id)
            .map(|s| s.step_count)
            .unwrap_or(0)
    }

    /// Get the last known activity for a device.
    pub fn last_activity(&self, device_id: &str) -> Option<&str> {
        self.devices
            .get(device_id)
            .and_then(|s| s.last_activity.as_deref())
    }

    /// Reset state for a device (e.g. starting a new test run).
    pub fn reset(&mut self, device_id: &str) {
        self.devices.remove(device_id);
    }
}

impl Default for SituationEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract display labels from every relevant element, not just the tappable
/// ones: an app answering a tap by rewriting a status label changes nothing
/// clickable, and diffing only clickables reported that working flow as stuck.
fn extract_element_texts(elements: &[UiElement]) -> Vec<String> {
    elements
        .iter()
        .filter(|e| e.is_relevant())
        .map(|e| e.display_label().to_string())
        .collect()
}

/// Diff old vs new element texts. Returns (new_elements, disappeared_elements).
/// Uses HashSet for O(n+m) instead of O(n*m).
fn diff_element_texts(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let old_set: HashSet<&str> = old.iter().map(|s| s.as_str()).collect();
    let new_set: HashSet<&str> = new.iter().map(|s| s.as_str()).collect();

    let new_elements = new
        .iter()
        .filter(|t| !old_set.contains(t.as_str()))
        .cloned()
        .collect();
    let disappeared = old
        .iter()
        .filter(|t| !new_set.contains(t.as_str()))
        .cloned()
        .collect();

    (new_elements, disappeared)
}

/// Simple hash of element state for screen-change detection.
fn hash_elements(elements: &[UiElement]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Every relevant element, not just the interactive ones: an app answering a
    // tap by rewriting a status label changes nothing clickable, and hashing
    // only clickables reported that working flow as `stuck`.
    for e in elements.iter().filter(|e| e.is_relevant()) {
        e.text.hash(&mut hasher);
        e.content_desc.hash(&mut hasher);
        e.class.hash(&mut hasher);
        e.bounds.left.hash(&mut hasher);
        e.bounds.top.hash(&mut hasher);
        // Selection state is the whole change when a radio or checkbox flips.
        e.checked.hash(&mut hasher);
        e.selected.hash(&mut hasher);
    }
    hasher.finish()
}

/// Detect app crashes by looking for crash dialog text in the UI tree.
fn detect_crash(elements: &[UiElement], package: &str) -> bool {
    let crash_indicators = [
        "has stopped",
        "keeps stopping",
        "isn't responding",
        "has crashed",
        "unfortunately",
        "close app",
        "wait",
    ];

    elements.iter().any(|e| {
        let text_lower = e.text.to_lowercase();
        crash_indicators
            .iter()
            .any(|indicator| text_lower.contains(indicator))
            && (e.package.contains("android") || e.package == package || e.text.contains(package))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::ui_element::Bounds;

    fn make_elem(text: &str, clickable: bool) -> UiElement {
        UiElement {
            class: "android.widget.Button".to_string(),
            text: text.to_string(),
            content_desc: String::new(),
            resource_id: String::new(),
            bounds: Bounds::new(0, 0, 100, 50),
            clickable,
            editable: false,
            is_password: false,
            focused: false,
            scrollable: false,
            enabled: true,
            visible: true,
            checked: false,
            selected: false,
            package: "com.app".to_string(),
        }
    }

    #[test]
    fn test_first_observation_stores_state() {
        let mut engine = SituationEngine::new();
        let elements = vec![make_elem("Login", true), make_elem("Cancel", true)];

        engine.observe("device1", "com.app/.LoginActivity", &elements);

        assert_eq!(
            engine.last_activity("device1"),
            Some("com.app/.LoginActivity")
        );
        assert_eq!(engine.step_count("device1"), 0);
    }

    #[test]
    fn test_report_screen_changed() {
        let mut engine = SituationEngine::new();
        let login_elements = vec![make_elem("Login", true), make_elem("Cancel", true)];
        engine.observe("d1", "LoginActivity", &login_elements);

        let dashboard_elements = vec![make_elem("Welcome", true), make_elem("Profile", true)];
        let report = engine.report_after_action(
            "d1",
            "tap",
            "Tapped #1 (Login)",
            crate::situation::ObservedScreen {
                activity: "DashboardActivity",
                package: "com.app",
                elements: &dashboard_elements,
                tree_available: true,
            },
        );

        assert!(report.screen_changed);
        assert!(report.activity_changed);
        assert_eq!(report.step, 1);
        assert!(report.new_elements.contains(&"Welcome".to_string()));
        assert!(report.new_elements.contains(&"Profile".to_string()));
        assert!(report.disappeared_elements.contains(&"Login".to_string()));
        assert!(report.disappeared_elements.contains(&"Cancel".to_string()));
        assert!(!report.stuck);
        assert!(!report.crash);
    }

    #[test]
    fn a_failed_dump_does_not_poison_the_baseline() {
        // observe() returns an empty list when the dump fails, and writing that as
        // the baseline made ONE flaky dump produce TWO confident wrong reports:
        // everything disappeared, then everything is new.
        let mut engine = SituationEngine::new();
        let screen = vec![make_elem("Login", true), make_elem("Cancel", true)];
        engine.observe("d1", "LoginActivity", &screen);

        let failed = engine.report_after_action(
            "d1",
            "tap",
            "Tapped #1",
            crate::situation::ObservedScreen {
                activity: "LoginActivity",
                package: "com.app",
                elements: &[],
                tree_available: false,
            },
        );
        assert!(
            failed.tree_unavailable,
            "the caller must be told nothing was observed"
        );
        assert!(
            !failed.screen_changed,
            "nothing was observed, so nothing can be claimed"
        );
        assert!(
            failed.disappeared_elements.is_empty(),
            "the screen did not empty, the dump failed"
        );

        // The next good observation diffs against the last REAL screen.
        let good = engine.report_after_action(
            "d1",
            "tap",
            "Tapped #1",
            crate::situation::ObservedScreen {
                activity: "LoginActivity",
                package: "com.app",
                elements: &screen,
                tree_available: true,
            },
        );
        assert!(!good.tree_unavailable);
        assert!(
            good.new_elements.is_empty(),
            "the screen never changed, so nothing is new: got {:?}",
            good.new_elements
        );
    }

    #[test]
    fn status_text_change_is_not_stuck() {
        // Aure's core interaction: pressing the blob leaves every clickable
        // identical and rewrites one status label. Hashing clickables only
        // reported screen_changed=false / stuck=true on a flow that worked,
        // which tells an agent to abandon it.
        let mut engine = SituationEngine::new();
        let before = vec![
            make_elem("Support", true),
            make_elem("Press and release to check in \u{2192}", false),
        ];
        engine.observe("d1", "MainActivity", &before);

        let after = vec![
            make_elem("Support", true),
            make_elem("Thank you for noticing.", false),
        ];
        let report = engine.report_after_action(
            "d1",
            "tap",
            "Tapped the blob",
            crate::situation::ObservedScreen {
                activity: "MainActivity",
                package: "com.app",
                elements: &after,
                tree_available: true,
            },
        );

        assert!(
            report.screen_changed,
            "a changed status label is a changed screen"
        );
        assert!(
            !report.stuck,
            "a working interaction must never report stuck"
        );
        assert!(report
            .new_elements
            .contains(&"Thank you for noticing.".to_string()));
    }

    #[test]
    fn selection_state_change_is_a_change() {
        // A radio flip leaves labels and bounds identical; only `selected` moves.
        let mut engine = SituationEngine::new();
        let mut before = vec![make_elem("Daily", true), make_elem("Weekly", true)];
        before[0].selected = true;
        engine.observe("d1", "SettingsActivity", &before);

        let mut after = vec![make_elem("Daily", true), make_elem("Weekly", true)];
        after[1].selected = true;
        let report = engine.report_after_action(
            "d1",
            "tap",
            "Tapped Weekly",
            crate::situation::ObservedScreen {
                activity: "SettingsActivity",
                package: "com.app",
                elements: &after,
                tree_available: true,
            },
        );

        assert!(report.screen_changed, "a selection change is a change");
        assert!(!report.stuck);
    }

    #[test]
    fn test_report_stuck_detection() {
        let mut engine = SituationEngine::new();
        let elements = vec![make_elem("Login", true)];
        engine.observe("d1", "Login", &elements);

        // First action — same screen (step 1 is never "stuck" — baseline)
        let report1 = engine.report_after_action(
            "d1",
            "tap",
            "Tapped #1",
            crate::situation::ObservedScreen {
                activity: "Login",
                package: "com.app",
                elements: &elements,
                tree_available: true,
            },
        );
        assert!(!report1.screen_changed);
        assert!(!report1.stuck); // Step 1 is not stuck — first action

        // Second action — still same screen → NOW stuck
        let report2 = engine.report_after_action(
            "d1",
            "tap",
            "Tapped #1",
            crate::situation::ObservedScreen {
                activity: "Login",
                package: "com.app",
                elements: &elements,
                tree_available: true,
            },
        );
        assert!(!report2.screen_changed);
        assert!(report2.stuck); // Step 2, still no change → stuck
        assert_eq!(report2.step, 2);
    }

    #[test]
    fn test_report_crash_detection() {
        let mut engine = SituationEngine::new();
        engine.observe("d1", "Login", &[make_elem("Login", true)]);

        let crash_elements = vec![
            UiElement {
                text: "com.app has stopped".to_string(),
                package: "android".to_string(),
                class: "TextView".to_string(),
                clickable: false,
                enabled: true,
                visible: true,
                bounds: Bounds::new(0, 0, 100, 50),
                ..make_elem("", false)
            },
            UiElement {
                text: "Close app".to_string(),
                package: "android".to_string(),
                class: "Button".to_string(),
                clickable: true,
                enabled: true,
                visible: true,
                bounds: Bounds::new(0, 50, 100, 100),
                ..make_elem("", true)
            },
        ];

        let report = engine.report_after_action(
            "d1",
            "tap",
            "Tapped #1",
            crate::situation::ObservedScreen {
                activity: "Crash",
                package: "com.app",
                elements: &crash_elements,
                tree_available: true,
            },
        );
        assert!(report.crash);
    }

    #[test]
    fn test_step_counter_increments() {
        let mut engine = SituationEngine::new();
        let elements = vec![make_elem("A", true)];

        engine.observe("d1", "Screen1", &elements);
        engine.report_after_action(
            "d1",
            "tap",
            "tap",
            crate::situation::ObservedScreen {
                activity: "Screen1",
                package: "com.app",
                elements: &elements,
                tree_available: true,
            },
        );
        engine.report_after_action(
            "d1",
            "tap",
            "tap",
            crate::situation::ObservedScreen {
                activity: "Screen1",
                package: "com.app",
                elements: &elements,
                tree_available: true,
            },
        );
        engine.report_after_action(
            "d1",
            "tap",
            "tap",
            crate::situation::ObservedScreen {
                activity: "Screen1",
                package: "com.app",
                elements: &elements,
                tree_available: true,
            },
        );

        assert_eq!(engine.step_count("d1"), 3);
    }

    #[test]
    fn test_reset_clears_state() {
        let mut engine = SituationEngine::new();
        engine.observe("d1", "Screen1", &[make_elem("A", true)]);
        assert_eq!(engine.last_activity("d1"), Some("Screen1"));

        engine.reset("d1");
        assert_eq!(engine.last_activity("d1"), None);
        assert_eq!(engine.step_count("d1"), 0);
    }

    #[test]
    fn test_multiple_devices_independent() {
        let mut engine = SituationEngine::new();
        let elem_a = vec![make_elem("A", true)];
        let elem_b = vec![make_elem("B", true)];

        engine.observe("device1", "ScreenA", &elem_a);
        engine.observe("device2", "ScreenB", &elem_b);

        assert_eq!(engine.last_activity("device1"), Some("ScreenA"));
        assert_eq!(engine.last_activity("device2"), Some("ScreenB"));

        engine.report_after_action(
            "device1",
            "tap",
            "tap",
            crate::situation::ObservedScreen {
                activity: "ScreenA2",
                package: "com.app",
                elements: &elem_a,
                tree_available: true,
            },
        );
        assert_eq!(engine.step_count("device1"), 1);
        assert_eq!(engine.step_count("device2"), 0);
    }

    #[test]
    fn test_scrollable_detected() {
        let mut engine = SituationEngine::new();
        engine.observe("d1", "Screen", &[]);

        let mut scrollable_elem = make_elem("List", true);
        scrollable_elem.scrollable = true;

        let report = engine.report_after_action(
            "d1",
            "tap",
            "tap",
            crate::situation::ObservedScreen {
                activity: "Screen",
                package: "com.app",
                elements: &[scrollable_elem],
                tree_available: true,
            },
        );
        assert!(report.scrollable);
    }

    #[test]
    fn element_count_matches_look_and_interactive_count_is_separate() {
        // These used to be one number called element_count, counting clickables
        // only, while drengr_look's element_count counted everything relevant.
        // Both appeared in a single drengr_do response with different values,
        // and the docs read element_count == 0 as "no tree", so a screen of
        // readable text with nothing tappable looked treeless.
        let mut engine = SituationEngine::new();
        engine.observe("d1", "Screen", &[]);

        let elements = vec![
            make_elem("Button", true),
            make_elem("Label", false), // readable, not tappable
            make_elem("Link", true),
        ];

        let report = engine.report_after_action(
            "d1",
            "tap",
            "tap",
            crate::situation::ObservedScreen {
                activity: "Screen",
                package: "com.app",
                elements: &elements,
                tree_available: true,
            },
        );
        assert_eq!(
            report.element_count, 3,
            "counts everything relevant, like look"
        );
        assert_eq!(
            report.interactive_count, 2,
            "the tappable subset stays available"
        );
    }

    #[test]
    fn test_diff_element_texts() {
        let old = vec!["Login".to_string(), "Cancel".to_string()];
        let new = vec![
            "Welcome".to_string(),
            "Cancel".to_string(),
            "Profile".to_string(),
        ];

        let (new_elems, disappeared) = diff_element_texts(&old, &new);
        assert!(new_elems.contains(&"Welcome".to_string()));
        assert!(new_elems.contains(&"Profile".to_string()));
        assert!(disappeared.contains(&"Login".to_string()));
        assert!(!disappeared.contains(&"Cancel".to_string())); // Still present
    }
}
