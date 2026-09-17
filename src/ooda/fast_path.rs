//! OODA fast-path — skip the LLM call when the right action is obvious.
//!
//! Two layers, both with hard fallback to the normal LLM flow:
//!
//! 1. **Deterministic matcher** — for tasks shaped like "tap login button",
//!    parse the verb + target noun and try to resolve it against the UI tree
//!    directly. Fires only when there is exactly ONE clickable element
//!    matching the noun (case-insensitive, word-boundary). Misses fall through.
//!
//! 2. **Decision cache** — hash `(task, ui_tree_signature)` → cache the LLM's
//!    last decision for this pair. Replays on re-runs of the same scenario.
//!    Per-process, in-memory, capped at 64 entries with 1-hour TTL.
//!
//! Disabled wholesale by setting `DRENGR_FAST_PATH=off`.
//!
//! Telemetry: every hit emits `tracing::info!(target: "ooda_fast_path", ...)`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::ooda::prompt::{OodaAction, OodaDecision};
use crate::screen::annotate::AnnotatedElement;

const CACHE_TTL: Duration = Duration::from_secs(3600);
const CACHE_MAX_ENTRIES: usize = 64;

pub(crate) fn fast_path_disabled() -> bool {
    disabled_by(std::env::var("DRENGR_FAST_PATH").ok().as_deref())
}

fn disabled_by(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.eq_ignore_ascii_case("off") || v == "0")
}

// ─── Layer 1: deterministic matcher ────────────────────────────────────

/// Tap-verb prefixes we recognize. Matched at the start of the task,
/// case-insensitive, word-boundary.
const TAP_VERBS: &[&str] = &["tap", "click", "press", "select", "open", "choose"];

/// Words to drop from the noun phrase before matching (filler).
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "on", "in", "at", "to", "this", "that", "button", "link", "icon", "menu",
    "tab", "option",
];

/// Try to deterministically resolve `task` to a single clickable element
/// in `elements`. Returns `Some(decision)` only when:
///   - The task starts with a tap-verb
///   - The remaining noun phrase resolves to EXACTLY ONE clickable element
///     whose text/content_desc/resource_id contains the noun
///
/// Returns `None` (caller falls through to LLM) on any ambiguity.
pub fn try_deterministic_match(task: &str, elements: &[AnnotatedElement]) -> Option<OodaDecision> {
    if fast_path_disabled() {
        return None;
    }

    let task_lower = task.trim().to_lowercase();
    let mut rest: Option<&str> = None;
    for verb in TAP_VERBS {
        if let Some(after) = task_lower.strip_prefix(verb) {
            // Require word boundary after the verb.
            let after = after.strip_prefix(' ').unwrap_or(after);
            if after.len() < task_lower.len() - verb.len() {
                rest = Some(after);
                break;
            }
            // verb without trailing space → not a verb match (e.g. "tapestry").
        }
    }
    let rest = rest?;

    // Strip stop-words from the noun phrase to get the discriminating tokens.
    let tokens: Vec<&str> = rest
        .split_whitespace()
        .filter(|t| !STOP_WORDS.contains(&t.trim_end_matches(|c: char| !c.is_alphanumeric())))
        .map(|t| t.trim_end_matches(|c: char| !c.is_alphanumeric()))
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    // Discriminating phrase = original (post-strip) noun (preserves multi-word like "sign up").
    let needle = tokens.join(" ");
    if needle.len() < 2 {
        // Single-letter noun is too ambiguous.
        return None;
    }

    let mut candidates: Vec<&AnnotatedElement> = elements
        .iter()
        .filter(|ae| {
            let e = &ae.element;
            if !e.clickable {
                return false;
            }
            let hay = format!(
                "{} {} {}",
                e.text.to_lowercase(),
                e.content_desc.to_lowercase(),
                e.resource_id.to_lowercase()
            );
            // Word-boundary contains: every token must appear as a word in hay.
            tokens.iter().all(|t| word_contains(&hay, t))
        })
        .collect();

    // Tie-break: prefer exact text match if multiple candidates.
    if candidates.len() > 1 {
        let exact: Vec<&AnnotatedElement> = candidates
            .iter()
            .filter(|ae| ae.element.text.eq_ignore_ascii_case(&needle))
            .copied()
            .collect();
        if exact.len() == 1 {
            candidates = exact;
        }
    }

    if candidates.len() != 1 {
        return None;
    }
    let target = candidates[0];

    tracing::info!(
        target: "ooda_fast_path",
        layer = "deterministic",
        needle = %needle,
        element = target.number,
        label = %target.element.display_label(),
        "fast-path tap matched without LLM"
    );

    Some(OodaDecision {
        action: OodaAction::Tap,
        element: Some(target.number),
        text: None,
        direction: None,
        name: None,
        points: None,
        duration_ms: None,
        reasoning: format!(
            "fast-path: '{}' uniquely matched element #{} ({})",
            needle,
            target.number,
            target.element.display_label()
        ),
        done: false,
    })
}

/// True if `hay` contains `needle` as a whole word.
/// `-` and `_` count as word characters (so "re-login" does NOT match "login").
fn word_contains(hay: &str, needle: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    let mut start = 0;
    while let Some(idx) = hay[start..].find(needle) {
        let abs = start + idx;
        let before_ok = abs == 0 || !hay[..abs].chars().last().map(is_word).unwrap_or(false);
        let after = abs + needle.len();
        let after_ok =
            after >= hay.len() || !hay[after..].chars().next().map(is_word).unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

// ─── Layer 2: decision cache ───────────────────────────────────────────

#[derive(Clone)]
struct CachedDecision {
    decision: OodaDecision,
    inserted: Instant,
    /// Element labels at the time of caching — used to reject the cached
    /// decision if the targeted element no longer exists in the current tree.
    expected_label: Option<String>,
}

fn cache() -> &'static Mutex<HashMap<u64, CachedDecision>> {
    static C: OnceLock<Mutex<HashMap<u64, CachedDecision>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Hash (task + ui_tree_signature) into a single u64.
/// `ui_tree_signature` should be a stable, scene-derived string —
/// `TextScene::description` works because it's deterministic per UI state.
pub fn cache_key(task: &str, ui_tree_signature: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    task.hash(&mut h);
    ui_tree_signature.hash(&mut h);
    h.finish()
}

pub fn try_cache_lookup(key: u64, elements: &[AnnotatedElement]) -> Option<OodaDecision> {
    if fast_path_disabled() {
        return None;
    }
    let mut map = cache().lock().unwrap();
    let entry = map.get(&key)?.clone();
    if entry.inserted.elapsed() > CACHE_TTL {
        map.remove(&key);
        return None;
    }
    // Reject if the cached decision targeted an element that no longer
    // exists in the same form in the current tree.
    if let Some(expected) = entry.expected_label.as_deref() {
        if let Some(elem_num) = entry.decision.element {
            let still_present = elements
                .iter()
                .any(|ae| ae.number == elem_num && ae.element.display_label() == expected);
            if !still_present {
                return None;
            }
        }
    }
    tracing::info!(
        target: "ooda_fast_path",
        layer = "cache",
        action = ?entry.decision.action,
        "fast-path cache hit"
    );
    Some(entry.decision)
}

pub fn cache_insert(key: u64, decision: &OodaDecision, elements: &[AnnotatedElement]) {
    if fast_path_disabled() {
        return;
    }
    let expected_label = decision.element.and_then(|n| {
        elements
            .iter()
            .find(|ae| ae.number == n)
            .map(|ae| ae.element.display_label().to_string())
    });
    let mut map = cache().lock().unwrap();
    // Cap size — drop oldest if we hit the max.
    if map.len() >= CACHE_MAX_ENTRIES {
        if let Some(&oldest) = map.iter().min_by_key(|(_, v)| v.inserted).map(|(k, _)| k) {
            map.remove(&oldest);
        }
    }
    map.insert(
        key,
        CachedDecision {
            decision: decision.clone(),
            inserted: Instant::now(),
            expected_label,
        },
    );
}

#[cfg(test)]
pub fn _clear_cache_for_tests() {
    cache().lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::ui_element::{Bounds, UiElement};

    fn ae(number: usize, text: &str, clickable: bool) -> AnnotatedElement {
        AnnotatedElement {
            number,
            tap_x: 100,
            tap_y: 200,
            element: UiElement {
                class: "Button".to_string(),
                text: text.to_string(),
                content_desc: String::new(),
                resource_id: String::new(),
                bounds: Bounds::new(0, 0, 200, 400),
                clickable,
                editable: false,
                is_password: false,
                focused: false,
                scrollable: false,
                enabled: true,
                visible: true,
                checked: false,
                selected: false,
                package: "x".to_string(),
            },
        }
    }

    #[test]
    fn deterministic_matches_single_clickable() {
        let els = vec![
            ae(1, "Email", true),
            ae(2, "Password", true),
            ae(3, "Login", true),
        ];
        let d = try_deterministic_match("tap login", &els).expect("should match");
        assert_eq!(d.action, OodaAction::Tap);
        assert_eq!(d.element, Some(3));
    }

    #[test]
    fn deterministic_handles_button_filler_word() {
        let els = vec![ae(1, "Sign Up", true), ae(2, "Help", true)];
        let d = try_deterministic_match("tap on the Sign Up button", &els).expect("should match");
        assert_eq!(d.element, Some(1));
    }

    #[test]
    fn deterministic_rejects_ambiguous_multi_match() {
        let els = vec![
            ae(1, "Login with Google", true),
            ae(2, "Login with Email", true),
        ];
        // Two clickables both match "login" → bail to LLM.
        assert!(try_deterministic_match("tap login", &els).is_none());
    }

    #[test]
    fn deterministic_breaks_tie_via_exact_match() {
        let els = vec![
            ae(1, "Login with Google", true),
            ae(2, "Login", true),
            ae(3, "Login with Apple", true),
        ];
        let d = try_deterministic_match("tap login", &els).expect("exact should win");
        assert_eq!(d.element, Some(2));
    }

    #[test]
    fn deterministic_skips_non_clickable() {
        let els = vec![
            ae(1, "Login", false), // not clickable
            ae(2, "Submit", true),
        ];
        // Only non-clickable matches — bail.
        assert!(try_deterministic_match("tap login", &els).is_none());
    }

    #[test]
    fn deterministic_no_match_returns_none() {
        let els = vec![ae(1, "Cancel", true), ae(2, "Done", true)];
        assert!(try_deterministic_match("tap login", &els).is_none());
    }

    #[test]
    fn deterministic_requires_tap_verb() {
        let els = vec![ae(1, "Login", true)];
        // No tap-verb prefix — skip.
        assert!(try_deterministic_match("login to the app", &els).is_none());
    }

    #[test]
    fn word_contains_respects_word_boundaries() {
        assert!(word_contains("login button", "login"));
        assert!(word_contains("the login button", "login"));
        assert!(!word_contains("re-login", "login")); // boundary failure
        assert!(!word_contains("loginate", "login"));
    }

    #[test]
    fn cache_round_trip() {
        _clear_cache_for_tests();
        let els = vec![ae(1, "Login", true)];
        let key = cache_key("tap login", "scene-v1");
        assert!(try_cache_lookup(key, &els).is_none());

        let decision = OodaDecision {
            action: OodaAction::Tap,
            element: Some(1),
            text: None,
            direction: None,
            name: None,
            points: None,
            duration_ms: None,
            reasoning: "test".to_string(),
            done: false,
        };
        cache_insert(key, &decision, &els);
        let got = try_cache_lookup(key, &els).expect("should hit");
        assert_eq!(got.element, Some(1));
    }

    #[test]
    fn cache_invalidates_on_label_drift() {
        _clear_cache_for_tests();
        let els1 = vec![ae(1, "Login", true)];
        let key = cache_key("tap login", "scene-v2");
        let decision = OodaDecision {
            action: OodaAction::Tap,
            element: Some(1),
            text: None,
            direction: None,
            name: None,
            points: None,
            duration_ms: None,
            reasoning: "test".to_string(),
            done: false,
        };
        cache_insert(key, &decision, &els1);

        // Same scene signature, but element #1 now has a different label.
        let els2 = vec![ae(1, "Sign In", true)];
        assert!(
            try_cache_lookup(key, &els2).is_none(),
            "label drift must invalidate"
        );
    }

    // Reading the env var is fine; SETTING it here is not. Tests share one
    // process, so a set_var raced every other test that consults this flag.
    #[test]
    fn fast_path_disabled_by_env() {
        assert!(disabled_by(Some("off")));
        assert!(disabled_by(Some("OFF")));
        assert!(disabled_by(Some("0")));
        assert!(!disabled_by(Some("on")));
        assert!(!disabled_by(Some("")));
        assert!(!disabled_by(None));
    }
}
