use serde::Serialize;
use serde_json::{json, Value};

/// MCP tool result returned from tool calls.
#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct ToolContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

impl ToolResult {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent {
                content_type: "text".to_string(),
                text: Some(content.into()),
                data: None,
                mime_type: None,
            }],
            is_error: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent {
                content_type: "text".to_string(),
                text: Some(message.into()),
                data: None,
                mime_type: None,
            }],
            is_error: Some(true),
        }
    }

    pub fn image_and_text(image_base64: String, text: String) -> Self {
        Self {
            content: vec![
                ToolContent {
                    content_type: "image".to_string(),
                    text: None,
                    data: Some(image_base64),
                    mime_type: Some("image/jpeg".to_string()),
                },
                ToolContent {
                    content_type: "text".to_string(),
                    text: Some(text),
                    data: None,
                    mime_type: None,
                },
            ],
            is_error: None,
        }
    }
}

pub fn tools_list_response() -> Value {
    json!({
        "tools": [
            drengr_look_definition(),
            drengr_do_definition(),
            drengr_query_definition(),
        ]
    })
}

fn drengr_look_definition() -> Value {
    let desc = "START HERE. Observe the current screen of the connected Android or iOS device. Returns an annotated screenshot with numbered interactive elements plus structured element data. You decide what to do next based on what you see. Use format='text' for a ~300 token text description instead of an image (100x cheaper). WORKFLOW: 1) drengr_query(question='connect') to connect — auto-detects local Android (ADB) and iOS (simctl) devices. 2) drengr_look to see the screen. 3) drengr_do to act. Repeat 2-3 until task is done.".to_string();
    // No feature suffix — all features are free. Only daily usage limit applies.

    json!({
        "name": "drengr_look",
        "title": "Observe Mobile Screen",
        "description": desc,
        "inputSchema": {
            "type": "object",
            "properties": {
                "device": {
                    "type": "string",
                    "description": "Device ID. Auto-detects if omitted."
                },
                "format": {
                    "type": "string",
                    "enum": ["image", "clean", "text", "grid"],
                    "description": "Response format. 'image' (default): annotated JPEG with numbered markers. 'clean': the same screenshot with no markers drawn on it, for judging typography, spacing and layout (element numbers still resolve). 'text': ~300 token text scene, no image and no duplicate element array; omits bounds, so use 'image' or 'clean' when you need them. 'grid': screenshot overlaid with a 0–100% coordinate grid — use when element_count is 0 (Flutter/webview/games/canvas) to aim coordinate taps: read the grid, then drengr_do(action='tap', x=<0-1>, y=<0-1>)."
                },
                "max_elements": {
                    "type": "integer",
                    "description": "Max interactive elements to annotate (default 50)."
                }
            }
        },
        "annotations": {
            "title": "Observe Screen",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": true
        }
    })
}

fn drengr_do_definition() -> Value {
    json!({
        "name": "drengr_do",
        "title": "Act on Mobile Device",
        "description": "Execute an action on the connected mobile device (Android or iOS phone) — tap, type, swipe, scroll, launch an app — then get a situation report + fresh screen observation + any network calls that occurred. The loop is: drengr_look once → drengr_do repeatedly until the task is done. Each drengr_do returns what changed (new elements, navigation, crashes) and network_calls (HTTP requests captured from device logs) so you know exactly what happened. When the task goal is achieved, stop calling drengr_do.",
        "inputSchema": {
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    // Schema enum derived from the single source of truth in `mcp::actions::ACTIONS`.
                    // New action = add one entry to ACTIONS; the schema picks it up automatically.
                    // For per-action descriptions/examples/destructiveness, call drengr_query("capabilities").
                    "enum": crate::mcp::actions::action_names(),
                    "description": "The action to perform. Call drengr_query(question='capabilities') for the full catalog with per-action descriptions, required params, and platform support."
                },
                "element": {
                    "type": "integer",
                    "description": "Element number from the last drengr_look/drengr_do response. Use when an element tree IS available."
                },
                "duration_ms": {
                    "type": "integer",
                    "description": "Hold time in milliseconds for long_press (default 1000, clamped 50-10000). Use when press duration carries meaning in the app."
                },
                "x": {
                    "type": "number",
                    "description": "Normalized X (0.0–1.0, fraction of screen width) for coordinate tap/swipe/long_press. Use this when the screen has NO usable element tree (element_count is 0 — e.g. Flutter, games, in-app web/canvas): read the screenshot and estimate where to tap. 0.0=left edge, 1.0=right edge."
                },
                "y": {
                    "type": "number",
                    "description": "Normalized Y (0.0–1.0, fraction of screen height) for coordinate tap/swipe/long_press. 0.0=top, 1.0=bottom."
                },
                "x2": {
                    "type": "number",
                    "description": "Normalized X of the swipe END point (0.0–1.0). With x/y/x2/y2 set, 'swipe' drags from (x,y) to (x2,y2) — e.g. scroll a list by dragging up."
                },
                "y2": {
                    "type": "number",
                    "description": "Normalized Y of the swipe END point (0.0–1.0)."
                },
                "text": {
                    "type": "string",
                    "description": "Text to type. Required for 'type' action."
                },
                "direction": {
                    "type": "string",
                    "enum": ["up", "down", "left", "right"],
                    "description": "Swipe direction. Required for 'swipe' action."
                },
                "package": {
                    "type": "string",
                    "description": "App package name. Required for 'launch' action."
                },
                "format": {
                    "type": "string",
                    "enum": ["image", "clean", "text"],
                    "description": "Response format for the post-action observation. 'image' (default): annotated JPEG with numbered markers. 'clean': the same frame with nothing drawn on it, for judging typography and layout. 'text': text scene only, no image and no duplicate element array."
                },
                "device": {
                    "type": "string",
                    "description": "Device ID. Auto-detects if omitted."
                },
                "keycode": {
                    "type": "string",
                    "description": "Key to send (for 'key' action). Named keys: 'enter', 'delete', 'tab', 'move_end'. Or a numeric Android keycode."
                },
                "until": {
                    "type": "string",
                    "description": "Wait condition (for 'wait' action). 'stable': wait for screen to stop changing. 'element:TEXT': wait until element containing TEXT appears. 'network:idle': wait for network activity to stop. Omit for a simple 1s pause."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Max wait time in seconds (default 5). Only used with 'until' parameter."
                },
                "scroll_to_find": {
                    "type": "boolean",
                    "description": "If true, auto-scroll to find the element before tapping. Used with 'element_text' or when element number is off-screen."
                },
                "element_text": {
                    "type": "string",
                    "description": "Find and tap an element by its text label. Used with scroll_to_find=true to find off-screen elements."
                },
                "max_scroll": {
                    "type": "integer",
                    "description": "Max scroll attempts when scroll_to_find=true (default 12)."
                },
                "apk": {
                    "type": "string",
                    "description": "Path to APK/app bundle to install (for 'install' action)."
                },
                "url": {
                    "type": "string",
                    "description": "URL for 'open_url' (http/https) or 'deep_link' (app scheme, e.g. myapp://path)."
                },
                "lat": {
                    "type": "number",
                    "description": "Latitude for 'set_location'."
                },
                "lng": {
                    "type": "number",
                    "description": "Longitude for 'set_location'."
                },
                "dark": {
                    "type": "boolean",
                    "description": "For 'set_appearance': true=dark mode, false=light mode."
                },
                "matches": {
                    "type": "boolean",
                    "description": "For 'simulate_biometric': true=successful match, false=failed match."
                },
                "permission": {
                    "type": "string",
                    "description": "For 'grant_permission': iOS service (location, photos, camera, microphone, contacts, all) or Android android.permission.* string."
                },
                "orientation": {
                    "type": "string",
                    "enum": ["portrait", "landscape", "landscape_left", "landscape_right", "portrait_upside_down"],
                    "description": "For 'set_orientation': target device orientation."
                }
            }
        },
        "annotations": {
            "title": "Perform Action",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": true
        }
    })
}

fn drengr_query_definition() -> Value {
    let desc = "Quick read-only queries about the connected mobile device (Android/iOS) that don't need a full screen capture. All queries are available on all tiers. Use for checking devices, current activity, crash status, logs, network calls, or finding specific elements.".to_string();

    json!({
        "name": "drengr_query",
        "title": "Query Mobile Device",
        "description": desc,
        "inputSchema": {
            "type": "object",
            "required": ["question"],
            "properties": {
                "question": {
                    "type": "string",
                    "enum": crate::mcp::capabilities::query_names(),
                    "description": "What to query. 'capabilities': structured catalog of every action and query (use this to discover the full surface). 'setup': PREFERRED FIRST STEP — single call that detects (or auto-boots, with `headless=true`) a device, connects a transport, and returns installed apps with display names. Use this to skip the connect+devices+launch dance. 'connect': lower-level — connect to an already-booted device. Auto-detects local Android (ADB) and iOS (simctl) devices. Cloud devices supported (BrowserStack, SauceLabs, AWS, LambdaTest, Perfecto, Kobiton, custom Appium). 'devices': list connected devices. 'activity': current screen/app. 'crash': crash status. 'find': find element by text. 'explore': map app screens. 'session': recording info. 'logcat': device logs. 'keyboard': keyboard state. 'network': HTTP calls. 'app_state': foreground/background. 'assert': verify conditions. 'diff': compare to baseline. 'analyze': session analysis. 'ui_dump': raw platform-native UI tree (Android XML / iOS hierarchy) — use when drengr_look misses elements or you need the full unfiltered view hierarchy."
                },
                "platform": {
                    "type": "string",
                    "enum": ["android", "ios", "any"],
                    "description": "Target platform for question='setup'. Default 'any' — picks an existing booted device of either OS first; if none and headless=true, auto-boots Android (faster than iOS sim runtime cold-start). Pass 'ios' explicitly to force iOS preference."
                },
                "headless": {
                    "type": "boolean",
                    "description": "For question='setup': auto-boot a device if none is connected. Android emulator uses CI-friendly flags (-no-window, -no-snapshot, -no-audio, software GPU); iOS sim boots without launching Simulator.app. Default false. Calling setup twice is safe — the second call detects the same device and returns started_by_us=false (only the first caller is responsible for tear-down)."
                },
                "show_window": {
                    "type": "boolean",
                    "description": "For question='setup' with headless auto-boot: open the Android emulator window so a human can watch gestures (drops -no-window). Default false. No effect on iOS — the sim renders without a window; use `open -a Simulator` to watch it."
                },
                "app_kind": {
                    "type": "string",
                    "enum": ["user", "system", "all"],
                    "description": "For question='setup': filter the returned app list. Default 'user' — short list of user-installable apps. Use 'all' to include system packages; the response caps at 60 apps and surfaces app_count_omitted so callers know to narrow."
                },
                "target": {
                    "type": "string",
                    "description": "Element description to find (for question='find')."
                },
                "package": {
                    "type": "string",
                    "description": "App package name (for question='crash')."
                },
                "device": {
                    "type": "string",
                    "description": "Device ID. Auto-detects if omitted."
                },
                "cloud": {
                    "type": "string",
                    "description": "Cloud provider for connect: browserstack, saucelabs, aws, lambdatest, perfecto, kobiton, custom, or any Appium hub URL."
                },
                "os_version": {
                    "type": "string",
                    "description": "OS version for cloud device (for question='connect')."
                },
                "app": {
                    "type": "string",
                    "description": "App URL for cloud devices (for question='connect'). BrowserStack: 'bs://hash', Sauce: 'sauce-storage:file'. If omitted, reads from BROWSERSTACK_APP_URL or SAUCE_APP_URL env var."
                },
                "filter": {
                    "type": "string",
                    "description": "Text/tag pattern to filter logs (for question='logcat')."
                },
                "lines": {
                    "type": "integer",
                    "description": "Number of log lines to return (default 50, max 500). For question='logcat'."
                },
                "last": {
                    "type": "integer",
                    "description": "Number of recent network calls to return (default 20). For question='network'."
                },
                "url_filter": {
                    "type": "string",
                    "description": "Filter network calls by URL pattern. For question='network'."
                },
                "status_filter": {
                    "type": "integer",
                    "description": "Filter network calls by HTTP status code. For question='network'."
                },
                "source": {
                    "type": "string",
                    "enum": ["logcat", "sdk"],
                    "description": "Return only calls seen by one capture source. For question='network'. 'logcat': the app's own OkHttp log lines, no request headers or body. 'sdk': the in-app Drengr SDK, metadata only. Omit to get both, each call tagged with its source."
                },
                "conditions": {
                    "type": "string",
                    "description": "JSON array of assertion conditions. Each: {\"text\": \"...\", \"visible\": true, \"type\": \"Button\"}. For question='assert'."
                },
                "baseline": {
                    "type": "string",
                    "description": "Path to baseline screenshot PNG for comparison (for question='diff')."
                }
            }
        },
        "annotations": {
            "title": "Query Device",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_list_has_3_tools() {
        let response = tools_list_response();
        let tools = response["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
    }

    #[test]
    fn test_tool_names() {
        let response = tools_list_response();
        let tools = response["tools"].as_array().unwrap();

        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

        assert_eq!(names, vec!["drengr_look", "drengr_do", "drengr_query"]);
    }

    #[test]
    fn test_drengr_look_has_format_param() {
        let def = drengr_look_definition();
        let format_prop = &def["inputSchema"]["properties"]["format"];
        assert_eq!(format_prop["type"], "string");
        let enums = format_prop["enum"].as_array().unwrap();
        assert!(enums.contains(&json!("image")));
        assert!(enums.contains(&json!("text")));
    }

    #[test]
    fn test_drengr_do_action_required() {
        let def = drengr_do_definition();
        let required = def["inputSchema"]["required"].as_array().unwrap();
        assert!(required.contains(&json!("action")));
    }

    #[test]
    fn test_drengr_do_actions() {
        // Schema enum is now derived from `mcp::actions::ACTIONS` — count assertion
        // anchors to that single source of truth instead of a hardcoded number,
        // so adding a new action doesn't require touching this test.
        let def = drengr_do_definition();
        let actions = def["inputSchema"]["properties"]["action"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(actions.len(), crate::mcp::actions::ACTIONS.len());
        // Spot-check some core actions are present.
        assert!(actions.contains(&json!("tap")));
        assert!(actions.contains(&json!("type")));
        assert!(actions.contains(&json!("swipe")));
        assert!(actions.contains(&json!("back")));
        assert!(actions.contains(&json!("launch")));
        assert!(actions.contains(&json!("launch_app")));
        assert!(actions.contains(&json!("draw_path")));
        assert!(actions.contains(&json!("spotlight_search")));
    }

    #[test]
    fn test_drengr_query_questions() {
        let def = drengr_query_definition();
        let questions = def["inputSchema"]["properties"]["question"]["enum"]
            .as_array()
            .unwrap();
        // 16 original + 1 new ("capabilities") = 17
        assert_eq!(questions.len(), 17);
        assert!(questions.contains(&json!("capabilities")));
        assert!(questions.contains(&json!("setup")));
        assert!(questions.contains(&json!("devices")));
        assert!(questions.contains(&json!("connect")));
        assert!(questions.contains(&json!("activity")));
        assert!(questions.contains(&json!("crash")));
        assert!(questions.contains(&json!("find")));
        assert!(questions.contains(&json!("ui_dump")));
        assert!(questions.contains(&json!("analyze")));
        assert!(questions.contains(&json!("explore")));
        assert!(questions.contains(&json!("session")));
        assert!(questions.contains(&json!("logcat")));
        assert!(questions.contains(&json!("keyboard")));
        assert!(questions.contains(&json!("network")));
        assert!(questions.contains(&json!("app_state")));
        assert!(questions.contains(&json!("assert")));
        assert!(questions.contains(&json!("diff")));
    }

    #[test]
    fn test_tool_result_text() {
        let result = ToolResult::text("hello");
        assert!(result.is_error.is_none());
        assert_eq!(result.content.len(), 1);
        assert_eq!(result.content[0].text.as_deref(), Some("hello"));
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolResult::error("something broke");
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content[0].text.as_deref(), Some("something broke"));
    }

    #[test]
    fn test_tool_result_image_and_text() {
        let result = ToolResult::image_and_text("base64data".to_string(), "info".to_string());
        assert_eq!(result.content.len(), 2);
        assert_eq!(result.content[0].content_type, "image");
        assert_eq!(result.content[0].data.as_deref(), Some("base64data"));
        assert_eq!(result.content[1].content_type, "text");
        assert_eq!(result.content[1].text.as_deref(), Some("info"));
    }

    #[test]
    fn test_tool_descriptions_contain_steering() {
        let look = drengr_look_definition();
        let desc = look["description"].as_str().unwrap();
        assert!(desc.contains("START HERE"));

        let do_def = drengr_do_definition();
        let desc = do_def["description"].as_str().unwrap();
        assert!(desc.contains("drengr_look once"));
        assert!(desc.contains("stop calling drengr_do"));
    }
}
