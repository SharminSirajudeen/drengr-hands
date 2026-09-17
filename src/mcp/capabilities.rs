// drengr_query("capabilities") — structured capability catalog for the LLM.
//
// Exists so an MCP client can self-discover the full action surface without
// having to remember the schema enum. Returned shape is whitelist-only:
// no internal paths, no env-var names, no transport class names, no RPC
// ports — only what the LLM needs to invoke an action correctly.
//
// Stable JSON contract:
//   {
//     "actions": [{ name, purpose, required, optional, platforms, destructive, example }, ...],
//     "queries": [{ name, purpose }, ...],
//     "hints_enabled": bool,
//     "active_platform": "android" | "ios" | null
//   }

use serde::Serialize;
use serde_json::{json, Value};

use crate::mcp::actions::ACTIONS;

#[derive(Serialize)]
struct ActionEntry {
    name: &'static str,
    purpose: &'static str,
    required: &'static [&'static str],
    optional: &'static [&'static str],
    platforms: &'static [&'static str],
    destructive: bool,
    example: &'static str,
}

#[derive(Serialize)]
struct QueryEntry {
    name: &'static str,
    purpose: &'static str,
}

const QUERIES: &[QueryEntry] = &[
    QueryEntry {
        name: "setup",
        purpose: "Provision a device, list installed apps, return ready state. Best 'first call'.",
    },
    QueryEntry {
        name: "devices",
        purpose: "List connected devices.",
    },
    QueryEntry {
        name: "connect",
        purpose: "Connect to a specific device by id.",
    },
    QueryEntry {
        name: "activity",
        purpose: "Current top activity / foreground app.",
    },
    QueryEntry {
        name: "crash",
        purpose: "Has the active app crashed?",
    },
    QueryEntry {
        name: "find",
        purpose: "Find an element by text or resource id.",
    },
    QueryEntry {
        name: "explore",
        purpose: "BFS-explore the app, building a screen map.",
    },
    QueryEntry {
        name: "session",
        purpose: "Current session state and step history.",
    },
    QueryEntry {
        name: "logcat",
        purpose: "Recent device logs (Android logcat / iOS unified log).",
    },
    QueryEntry {
        name: "keyboard",
        purpose: "Is the soft keyboard visible?",
    },
    QueryEntry {
        name: "network",
        purpose: "Recent network calls captured from the device.",
    },
    QueryEntry {
        name: "app_state",
        purpose: "Foreground/background state of a specific package.",
    },
    QueryEntry {
        name: "assert",
        purpose: "Assert that an element / text is present.",
    },
    QueryEntry {
        name: "diff",
        purpose: "Diff between two situation reports.",
    },
    QueryEntry {
        name: "analyze",
        purpose: "Heuristic analysis of the current screen for next-action suggestions.",
    },
    QueryEntry {
        name: "ui_dump",
        purpose: "Full UI tree dump.",
    },
    QueryEntry {
        name: "capabilities",
        purpose: "This response — structured catalog of all actions and queries.",
    },
];

/// The advertised question set, from the one catalog that defines it.
///
/// The JSON-Schema `enum` in `tools.rs` is built from this, so a question is
/// advertised in both places or neither. They were two hand-written lists and
/// they drifted: `screen_stream` reached the dispatch match without ever being
/// added to either.
pub fn query_names() -> Vec<&'static str> {
    QUERIES.iter().map(|q| q.name).collect()
}

pub fn capabilities_response(active_platform: Option<&str>, hints_enabled: bool) -> Value {
    let actions: Vec<ActionEntry> = ACTIONS
        .iter()
        .map(|a| ActionEntry {
            name: a.name,
            purpose: a.purpose,
            required: a.required,
            optional: a.optional,
            platforms: a.platforms,
            destructive: a.destructive,
            example: a.example,
        })
        .collect();

    json!({
        "actions": actions,
        "queries": QUERIES,
        "hints_enabled": hints_enabled,
        "active_platform": active_platform,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_shape_is_whitelist_only() {
        let v = capabilities_response(Some("android"), true);
        let obj = v.as_object().unwrap();

        // Top-level keys are exactly these four — no leakage.
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        let expected = ["actions", "active_platform", "hints_enabled", "queries"];
        assert_eq!(keys, expected.iter().collect::<Vec<_>>());

        // First action has only the documented fields, nothing else.
        let first = obj
            .get("actions")
            .unwrap()
            .as_array()
            .unwrap()
            .first()
            .unwrap();
        let action_obj = first.as_object().unwrap();
        let mut action_keys: Vec<&String> = action_obj.keys().collect();
        action_keys.sort();
        let expected_action_keys = [
            "destructive",
            "example",
            "name",
            "optional",
            "platforms",
            "purpose",
            "required",
        ];
        assert_eq!(action_keys, expected_action_keys.iter().collect::<Vec<_>>());
    }

    #[test]
    fn capabilities_query_is_in_query_list() {
        // Sanity: documenting itself prevents future devs from removing the entry.
        assert!(QUERIES.iter().any(|q| q.name == "capabilities"));
    }
}
