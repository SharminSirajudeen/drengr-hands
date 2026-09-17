//! Deterministic navigation planner over an explored `ScreenMap`.
//!
//! When the task is a pure navigation phrase ("go to settings") and a fresh
//! BFS screen map exists for the app (`~/.drengr/maps/<pkg>.json`), DECIDE
//! becomes a shortest-path hop executed as a label tap / back press — zero
//! LLM calls until arrival. Any miss (no map, ambiguous goal, stale label,
//! no path) falls through to the normal LLM flow.
//!
//! Shares the `DRENGR_FAST_PATH=off` kill switch with `fast_path`.

use crate::explore::{activity_to_id, bfs_shortest_paths, ScreenMap};
use crate::ooda::fast_path::fast_path_disabled;
use crate::ooda::prompt::{OodaAction, OodaDecision};
use crate::screen::annotate::AnnotatedElement;

/// Maps older than this are ignored — app UI drifts, edges go stale.
const MAP_MAX_AGE_DAYS: i64 = 7;

/// Navigation phrases that mark a pure-nav task. Matched at the start,
/// case-insensitive; the remainder must resolve to exactly one screen.
const NAV_VERBS: &[&str] = &["go to", "goto", "navigate to", "go into", "open", "show"];

/// Filler words dropped from the goal phrase before matching.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "screen", "page", "tab", "section", "menu", "view",
];

/// Planner verdict for the current screen.
pub enum NavStatus {
    /// Current screen IS the goal — hand back to the LLM to confirm/finish.
    Arrived,
    /// Next hop along the BFS shortest path.
    Hop(NavHop),
    /// Goal unreachable from here (or current screen unknown to the map).
    NoPath,
}

/// One edge of the planned path, replayed against the live screen.
pub enum NavHop {
    /// Tap the element whose display label matches.
    Tap(String),
    Back,
}

/// True when the map is recent enough to trust its edges.
pub fn is_fresh(map: &ScreenMap) -> bool {
    chrono::DateTime::parse_from_rfc3339(&map.explored_at)
        .map(|t| {
            chrono::Utc::now().signed_duration_since(t) < chrono::Duration::days(MAP_MAX_AGE_DAYS)
        })
        .unwrap_or(false)
}

/// Classify a pure-nav task into a goal screen id. Conservative: fires only
/// when the whole task is "<nav-verb> <screen name>" and the name resolves
/// to exactly one screen in the map. Anything else → `None` (LLM decides).
pub fn goal_screen_for_task(map: &ScreenMap, task: &str) -> Option<String> {
    if fast_path_disabled() {
        return None;
    }
    let t = task.trim().to_lowercase();
    let rest = NAV_VERBS
        .iter()
        .find_map(|v| t.strip_prefix(v)?.strip_prefix(' '))?;

    let tokens: Vec<&str> = rest
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty() && !STOP_WORDS.contains(w))
        .collect();
    if tokens.is_empty() {
        return None;
    }
    // Screen ids are lowercased activity names with word boundaries collapsed
    // ("NotificationSettings" → "notificationsettings"), so match on concat.
    let joined = tokens.concat();

    let mut ids: Vec<&str> = map.screens.iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();

    let exact: Vec<&str> = ids.iter().copied().filter(|id| *id == joined).collect();
    let candidates = if exact.is_empty() {
        ids.iter()
            .copied()
            .filter(|id| id.contains(&joined))
            .collect()
    } else {
        exact
    };
    if candidates.len() != 1 {
        return None;
    }
    tracing::info!(
        target: "ooda_nav_path",
        goal = candidates[0],
        "task classified as pure navigation"
    );
    Some(candidates[0].to_string())
}

/// Next hop along the BFS shortest path from the current activity to `goal_id`.
pub fn next_hop(map: &ScreenMap, current_activity: &str, goal_id: &str) -> NavStatus {
    let current_id = activity_to_id(current_activity);
    if current_id == goal_id {
        return NavStatus::Arrived;
    }
    let paths = bfs_shortest_paths(map, &current_id);
    let next = match paths.get(goal_id).and_then(|p| p.get(1)) {
        Some(n) => n.clone(),
        None => return NavStatus::NoPath,
    };
    let edge = match map
        .edges
        .iter()
        .find(|e| e.from == current_id && e.to == next)
    {
        Some(e) => e,
        None => return NavStatus::NoPath,
    };
    if edge.action == "back" {
        NavStatus::Hop(NavHop::Back)
    } else if let Some(label) = edge.action.strip_prefix("tap ") {
        NavStatus::Hop(NavHop::Tap(label.to_string()))
    } else {
        NavStatus::NoPath
    }
}

/// Resolve a hop against the live screen. `None` = the map is stale for this
/// screen (label not clickable/present) — caller falls back to the LLM.
pub fn hop_to_decision(
    hop: &NavHop,
    elements: &[AnnotatedElement],
    goal_id: &str,
) -> Option<OodaDecision> {
    match hop {
        NavHop::Back => Some(decision(
            OodaAction::PressBack,
            None,
            format!("nav-path: back toward '{}'", goal_id),
        )),
        NavHop::Tap(label) => {
            let target = elements.iter().find(|ae| {
                ae.element.clickable && ae.element.display_label().eq_ignore_ascii_case(label)
            })?;
            tracing::info!(
                target: "ooda_nav_path",
                label = %label,
                element = target.number,
                goal = goal_id,
                "nav hop resolved without LLM"
            );
            Some(decision(
                OodaAction::Tap,
                Some(target.number),
                format!("nav-path: tap '{}' toward '{}'", label, goal_id),
            ))
        }
    }
}

fn decision(action: OodaAction, element: Option<usize>, reasoning: String) -> OodaDecision {
    OodaDecision {
        action,
        element,
        text: None,
        direction: None,
        name: None,
        points: None,
        duration_ms: None,
        reasoning,
        done: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::{NavigationEdge, ScreenNode};
    use crate::screen::ui_element::{Bounds, UiElement};

    fn test_map() -> ScreenMap {
        ScreenMap {
            app: "com.app".to_string(),
            explored_at: chrono::Utc::now().to_rfc3339(),
            screens: vec![
                ScreenNode {
                    id: "login".into(),
                    activity: "com.app/.LoginActivity".into(),
                    elements: vec![],
                },
                ScreenNode {
                    id: "dashboard".into(),
                    activity: "com.app/.DashboardActivity".into(),
                    elements: vec![],
                },
                ScreenNode {
                    id: "settings".into(),
                    activity: "com.app/.SettingsActivity".into(),
                    elements: vec![],
                },
            ],
            edges: vec![
                NavigationEdge {
                    from: "login".into(),
                    action: "tap Login".into(),
                    to: "dashboard".into(),
                },
                NavigationEdge {
                    from: "dashboard".into(),
                    action: "tap Settings".into(),
                    to: "settings".into(),
                },
                NavigationEdge {
                    from: "settings".into(),
                    action: "back".into(),
                    to: "dashboard".into(),
                },
            ],
        }
    }

    fn ae(number: usize, text: &str, clickable: bool) -> AnnotatedElement {
        AnnotatedElement {
            number,
            tap_x: 10,
            tap_y: 20,
            element: UiElement {
                class: "Button".into(),
                text: text.into(),
                content_desc: String::new(),
                resource_id: String::new(),
                bounds: Bounds::new(0, 0, 100, 100),
                clickable,
                editable: false,
                is_password: false,
                focused: false,
                scrollable: false,
                enabled: true,
                visible: true,
                checked: false,
                selected: false,
                package: "com.app".into(),
            },
        }
    }

    #[test]
    fn classifies_pure_nav_task() {
        let map = test_map();
        assert_eq!(
            goal_screen_for_task(&map, "go to settings"),
            Some("settings".into())
        );
        assert_eq!(
            goal_screen_for_task(&map, "open the Settings screen"),
            Some("settings".into())
        );
        assert_eq!(
            goal_screen_for_task(&map, "navigate to dashboard"),
            Some("dashboard".into())
        );
    }

    #[test]
    fn rejects_non_nav_and_compound_tasks() {
        let map = test_map();
        assert!(goal_screen_for_task(&map, "tap login").is_none()); // no nav verb
        assert!(goal_screen_for_task(&map, "go to settings and enable dark mode").is_none());
        assert!(goal_screen_for_task(&map, "go to checkout").is_none()); // unknown screen
    }

    #[test]
    fn exact_id_beats_substring_ambiguity() {
        let mut map = test_map();
        map.screens.push(ScreenNode {
            id: "settingsdetail".into(),
            activity: "com.app/.SettingsDetailActivity".into(),
            elements: vec![],
        });
        // "settings" is a substring of both, but an exact id — exact wins.
        assert_eq!(
            goal_screen_for_task(&map, "go to settings"),
            Some("settings".into())
        );
    }

    #[test]
    fn concatenated_multiword_screen_matches() {
        let mut map = test_map();
        map.screens.push(ScreenNode {
            id: "notificationsettings".into(),
            activity: "com.app/.NotificationSettingsActivity".into(),
            elements: vec![],
        });
        assert_eq!(
            goal_screen_for_task(&map, "go to notification settings"),
            Some("notificationsettings".into())
        );
    }

    #[test]
    fn next_hop_walks_shortest_path() {
        let map = test_map();
        match next_hop(&map, "com.app/.LoginActivity", "settings") {
            NavStatus::Hop(NavHop::Tap(label)) => assert_eq!(label, "Login"),
            _ => panic!("expected first hop tap Login"),
        }
        match next_hop(&map, "com.app/.DashboardActivity", "settings") {
            NavStatus::Hop(NavHop::Tap(label)) => assert_eq!(label, "Settings"),
            _ => panic!("expected hop tap Settings"),
        }
    }

    #[test]
    fn next_hop_back_edge() {
        let map = test_map();
        match next_hop(&map, "com.app/.SettingsActivity", "dashboard") {
            NavStatus::Hop(NavHop::Back) => {}
            _ => panic!("expected back hop"),
        }
    }

    #[test]
    fn next_hop_arrived_and_no_path() {
        let map = test_map();
        assert!(matches!(
            next_hop(&map, "com.app/.SettingsActivity", "settings"),
            NavStatus::Arrived
        ));
        assert!(matches!(
            next_hop(&map, "com.app/.UnknownActivity", "settings"),
            NavStatus::NoPath
        ));
        // No reverse path from dashboard to login (no edge).
        assert!(matches!(
            next_hop(&map, "com.app/.DashboardActivity", "login"),
            NavStatus::NoPath
        ));
    }

    #[test]
    fn hop_resolves_against_live_elements() {
        let els = vec![ae(1, "Profile", true), ae(2, "Settings", true)];
        let d = hop_to_decision(&NavHop::Tap("Settings".into()), &els, "settings")
            .expect("label present");
        assert_eq!(d.action, OodaAction::Tap);
        assert_eq!(d.element, Some(2));
    }

    #[test]
    fn hop_misses_when_label_gone_or_unclickable() {
        let els = vec![ae(1, "Profile", true), ae(2, "Settings", false)];
        assert!(hop_to_decision(&NavHop::Tap("Settings".into()), &els, "settings").is_none());
        assert!(hop_to_decision(&NavHop::Tap("Billing".into()), &els, "settings").is_none());
    }

    #[test]
    fn back_hop_needs_no_elements() {
        let d = hop_to_decision(&NavHop::Back, &[], "dashboard").expect("back always resolves");
        assert_eq!(d.action, OodaAction::PressBack);
    }

    #[test]
    fn freshness_gate() {
        let mut map = test_map();
        assert!(is_fresh(&map));
        map.explored_at = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        assert!(!is_fresh(&map));
        map.explored_at = "not-a-date".into();
        assert!(!is_fresh(&map));
    }
}
