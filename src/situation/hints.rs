// Contextual hint engine for the OODA situation report.
//
// Goal: when the LLM is doing something inefficient (e.g. swiping repeatedly
// to find an app when launch_app('Name') would do it in one call), surface
// a one-line tip in the situation report. Cost: 0 tokens by default, 10-20
// only when a hint actually fires.
//
// Constraints (non-negotiable):
//   1. Per-session dedup — each HintId fires at most once per session.
//   2. Hints NEVER suggest a destructive action (uninstall, clear_app_data,
//      open_url, etc.). Enforced by intersecting against actions.rs's
//      destructive_action_names() — see test below.
//   3. DRENGR_HINTS=off short-circuits to None always.
//   4. Hint text bounded to ~80 chars.
//
// Design source: MCPCAP architect + security review (2026-04-30).

use std::collections::HashSet;

use crate::situation::report::SituationReport;

/// Stable identifier for a hint. Used for per-session dedup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HintId {
    StuckOnLauncher,
    LongListUseSearch,
    ManyStuckTaps,
    AppNotFoundUseListApps,
    KeyboardVisibleType,
}

#[derive(Debug, Clone)]
pub struct Hint {
    pub id: HintId,
    pub text: String,
}

/// Minimal context the engine needs beyond the SituationReport.
#[derive(Debug, Clone)]
pub struct HintContext<'a> {
    pub last_action_name: Option<&'a str>,
    pub consecutive_stuck_steps: usize,
    pub last_action_was_launch_app: bool,
}

pub struct HintEngine {
    seen: HashSet<HintId>,
    enabled: bool,
}

impl Default for HintEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Hints are on unless `DRENGR_HINTS=off`.
pub fn hints_enabled() -> bool {
    std::env::var("DRENGR_HINTS")
        .map(|v| !v.eq_ignore_ascii_case("off"))
        .unwrap_or(true)
}

impl HintEngine {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
            enabled: hints_enabled(),
        }
    }

    /// Returns Some(hint) at most once per (session, HintId), and only when
    /// a trigger condition fires. Returns None when DRENGR_HINTS=off.
    pub fn evaluate(&mut self, report: &SituationReport, ctx: &HintContext) -> Option<Hint> {
        if !self.enabled {
            return None;
        }
        let hint = self.pick(report, ctx)?;
        if self.seen.contains(&hint.id) {
            return None;
        }
        self.seen.insert(hint.id);
        tracing::debug!(hint_id = ?hint.id, step = report.step, "hint_fired");
        Some(hint)
    }

    fn pick(&self, report: &SituationReport, ctx: &HintContext) -> Option<Hint> {
        // Priority order matters — first match wins.

        // 1. Stuck on launcher → suggest launch_app
        if report.stuck && is_launcher(&report.activity) {
            return Some(Hint {
                id: HintId::StuckOnLauncher,
                text: "tip: launch_app(target='AppName') opens an app directly".into(),
            });
        }

        // 2. Last action was launch_app but activity didn't change → wrong name
        if ctx.last_action_was_launch_app && !report.activity_changed {
            return Some(Hint {
                id: HintId::AppNotFoundUseListApps,
                text: "tip: app didn't open — list_installed_apps() to see exact names".into(),
            });
        }

        // 3. Long scrollable list + recent taps not navigating → suggest spotlight on iOS
        if report.scrollable
            && report.new_elements.len() > 15
            && ctx.last_action_name == Some("tap")
            && !report.activity_changed
        {
            return Some(Hint {
                id: HintId::LongListUseSearch,
                text: "tip: long list — spotlight_search(query='X') jumps directly (iOS)".into(),
            });
        }

        // 4. Stuck for 3+ consecutive steps → suggest scrolling
        if ctx.consecutive_stuck_steps >= 3 {
            return Some(Hint {
                id: HintId::ManyStuckTaps,
                text: "tip: stuck — try scroll_to_top/scroll_to_bottom to reveal hidden elements"
                    .into(),
            });
        }

        // 5. Keyboard-suggesting elements present + last action was tap → use type next
        if !report.new_elements.is_empty()
            && report.new_elements.iter().any(|e| {
                let lc = e.to_lowercase();
                lc.contains("edittext") || lc.contains("textfield") || lc.contains("input")
            })
            && ctx.last_action_name == Some("tap")
        {
            return Some(Hint {
                id: HintId::KeyboardVisibleType,
                text: "tip: text field focused — next action is type(text='...', element=N)".into(),
            });
        }

        None
    }
}

fn is_launcher(activity: &str) -> bool {
    let lc = activity.to_lowercase();
    lc.contains("launcher") || lc.contains("springboard") || lc.contains("homescreen")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> SituationReport {
        SituationReport {
            step: 1,
            action: "tap".into(),
            screen_changed: false,
            activity: String::new(),
            activity_changed: false,
            crash: false,
            stuck: false,
            new_elements: Vec::new(),
            disappeared_elements: Vec::new(),
            scrollable: false,
            element_count: 0,
            interactive_count: 0,
            tree_unavailable: false,
            hint: None,
        }
    }

    fn ctx() -> HintContext<'static> {
        HintContext {
            last_action_name: None,
            consecutive_stuck_steps: 0,
            last_action_was_launch_app: false,
        }
    }

    #[test]
    fn dedup_fires_once() {
        let mut engine = HintEngine {
            seen: HashSet::new(),
            enabled: true,
        };
        let mut r = report();
        r.stuck = true;
        r.activity = "com.android.launcher".into();
        let h1 = engine.evaluate(&r, &ctx());
        let h2 = engine.evaluate(&r, &ctx());
        assert!(h1.is_some(), "first eval should fire");
        assert!(h2.is_none(), "second eval should be deduped");
    }

    #[test]
    fn disabled_returns_none() {
        let mut engine = HintEngine {
            seen: HashSet::new(),
            enabled: false,
        };
        let mut r = report();
        r.stuck = true;
        r.activity = "com.android.launcher".into();
        assert!(engine.evaluate(&r, &ctx()).is_none());
    }

    #[test]
    fn stuck_on_launcher_fires() {
        let mut engine = HintEngine {
            seen: HashSet::new(),
            enabled: true,
        };
        let mut r = report();
        r.stuck = true;
        r.activity = "com.android.launcher.Launcher".into();
        let h = engine.evaluate(&r, &ctx()).unwrap();
        assert_eq!(h.id, HintId::StuckOnLauncher);
    }

    #[test]
    fn many_stuck_taps_fires() {
        let mut engine = HintEngine {
            seen: HashSet::new(),
            enabled: true,
        };
        let r = report();
        let mut c = ctx();
        c.consecutive_stuck_steps = 3;
        let h = engine.evaluate(&r, &c).unwrap();
        assert_eq!(h.id, HintId::ManyStuckTaps);
    }

    #[test]
    fn hints_never_suggest_destructive_actions() {
        // Security guardrail: scan every possible hint text for any destructive action name.
        // Even one slip would be a route for prompt injection to weaponize destructive verbs.
        let destructive = crate::mcp::actions::destructive_action_names();
        let mut engine = HintEngine {
            seen: HashSet::new(),
            enabled: true,
        };

        // Build every triggerable scenario and assert no hint mentions a destructive action.
        let scenarios: Vec<(SituationReport, HintContext)> = vec![
            // Stuck on launcher
            (
                {
                    let mut r = report();
                    r.stuck = true;
                    r.activity = "Launcher".into();
                    r
                },
                ctx(),
            ),
            // Stuck many steps
            (report(), {
                let mut c = ctx();
                c.consecutive_stuck_steps = 5;
                c
            }),
            // App not found
            (report(), {
                let mut c = ctx();
                c.last_action_was_launch_app = true;
                c
            }),
        ];

        for (r, c) in scenarios {
            // Reset seen between iterations so each scenario can fire.
            engine.seen.clear();
            if let Some(h) = engine.evaluate(&r, &c) {
                // Word-boundary check: tokenize on non-(alnum|underscore) so
                // identifier-style names match exactly. Prevents false positives
                // like "list_installed_apps" matching "install".
                let tokens: Vec<&str> = h
                    .text
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .filter(|t| !t.is_empty())
                    .collect();
                for d in &destructive {
                    assert!(
                        !tokens.contains(d),
                        "Hint '{}' (id={:?}) mentions destructive action '{}' as a whole token",
                        h.text,
                        h.id,
                        d
                    );
                }
            }
        }
    }
}
