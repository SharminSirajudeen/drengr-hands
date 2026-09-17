//! Parse drengr-runner `tree_hint` JSON into a flat `Vec<UiElement>`.

use crate::screen::ui_element::{Bounds, UiElement};

/// NOTE: tree_hint is informational only. The OODA layer should not use the
/// `clickable` field of returned `UiElement`s as authoritative — SwiftUI
/// buttons often appear as `Other` here without flags. The LLM decides
/// tappability from the screenshot.
///
/// Flatten the tree_hint JSON (Application root) into UiElements for OODA.
/// Returns empty on null/non-object input.
pub fn parse_tree_hint(json: &serde_json::Value) -> Vec<UiElement> {
    let mut out = Vec::new();
    if json.is_null() || !json.is_object() {
        return out;
    }
    walk(json, &mut out);
    out
}

fn walk(node: &serde_json::Value, out: &mut Vec<UiElement>) {
    if !node.is_object() {
        return;
    }

    let type_name = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let id = node.get("id").and_then(|v| v.as_str()).unwrap_or("");
    // iOS labels arrive as JSON strings, so real newlines always survived here:
    // this path was open before Android reached parity with it.
    let label = crate::screen::ui_element::sanitize_device_text(
        node.get("label").and_then(|v| v.as_str()).unwrap_or(""),
    );
    let value = crate::screen::ui_element::sanitize_device_text(
        node.get("value").and_then(|v| v.as_str()).unwrap_or(""),
    );
    let enabled = node
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let frame = node.get("frame").and_then(|v| v.as_array());
    let (x, y, w, h) = match frame {
        Some(arr) if arr.len() >= 4 => (
            arr[0].as_f64().unwrap_or(0.0),
            arr[1].as_f64().unwrap_or(0.0),
            arr[2].as_f64().unwrap_or(0.0),
            arr[3].as_f64().unwrap_or(0.0),
        ),
        _ => (0.0, 0.0, 0.0, 0.0),
    };

    let is_root = type_name == "Application";
    let is_keyboard = type_name == "Keyboard";
    let zero_area = w == 0.0 || h == 0.0;
    let clickable = is_clickable(type_name);
    let editable = is_editable(type_name);
    let scrollable = is_scrollable(type_name);
    let has_label = !id.is_empty() || !label.is_empty() || !value.is_empty();

    let should_emit = !is_root
        && !is_keyboard
        && !zero_area
        && (clickable || editable || scrollable || has_label);

    if should_emit {
        let text = if !label.is_empty() {
            label.to_string()
        } else if !value.is_empty() {
            value.to_string()
        } else {
            String::new()
        };

        out.push(UiElement {
            class: type_name.to_string(),
            text,
            content_desc: id.to_string(),
            resource_id: id.to_string(),
            bounds: Bounds {
                left: x as i32,
                top: y as i32,
                right: (x + w) as i32,
                bottom: (y + h) as i32,
            },
            clickable,
            editable,
            is_password: type_name == "SecureTextField",
            focused: false,
            scrollable,
            enabled,
            visible: true,
            checked: false,
            selected: false,
            package: String::new(),
        });
    }

    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for c in children {
            walk(c, out);
        }
    }
}

fn is_clickable(type_name: &str) -> bool {
    matches!(
        type_name,
        "Button"
            | "Link"
            | "Cell"
            | "Switch"
            | "SwitchOption"
            | "Tab"
            | "MenuItem"
            | "MenuButton"
            | "SegmentedControl"
            | "CheckBox"
            | "RadioButton"
            | "Key"
            | "TextField"
            | "TextView"
            | "SearchField"
            | "SecureTextField"
    )
}

fn is_editable(type_name: &str) -> bool {
    matches!(
        type_name,
        "TextField" | "TextView" | "SearchField" | "SecureTextField"
    )
}

fn is_scrollable(type_name: &str) -> bool {
    matches!(
        type_name,
        "ScrollView" | "Table" | "CollectionView" | "PageIndicator"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_button() {
        let src = serde_json::json!({
            "type": "Application",
            "frame": [0, 0, 440, 956],
            "children": [{
                "type": "Button",
                "label": "Close",
                "frame": [378, 72, 62, 62],
                "enabled": true
            }]
        });
        let els = parse_tree_hint(&src);
        assert_eq!(els.len(), 1);
        let e = &els[0];
        assert_eq!(e.class, "Button");
        assert_eq!(e.text, "Close");
        assert!(e.clickable);
        assert_eq!(e.bounds.left, 378);
        assert_eq!(e.bounds.top, 72);
        assert_eq!(e.bounds.right, 440);
        assert_eq!(e.bounds.bottom, 134);
    }

    #[test]
    fn skips_zero_area_nodes() {
        let src = serde_json::json!({
            "type": "Application",
            "frame": [0, 0, 440, 956],
            "children": [{
                "type": "Button",
                "label": "Hidden",
                "frame": [0, 0, 0, 0]
            }]
        });
        let els = parse_tree_hint(&src);
        assert!(els.is_empty());
    }

    #[test]
    fn null_input_returns_empty() {
        let els = parse_tree_hint(&serde_json::Value::Null);
        assert!(els.is_empty());
    }

    #[test]
    fn handles_deep_nesting() {
        let src = serde_json::json!({
            "type": "Application",
            "frame": [0, 0, 440, 956],
            "children": [{
                "type": "Window",
                "frame": [0, 0, 440, 956],
                "children": [{
                    "type": "ScrollView",
                    "frame": [0, 0, 440, 800],
                    "children": [{
                        "type": "Cell",
                        "frame": [0, 0, 440, 60],
                        "children": [{
                            "type": "Button",
                            "label": "Deep",
                            "frame": [10, 10, 100, 44]
                        }]
                    }]
                }]
            }]
        });
        let els = parse_tree_hint(&src);
        let names: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert!(names.contains(&"Deep"));
        assert!(els.iter().any(|e| e.class == "Button" && e.text == "Deep"));
    }

    #[test]
    fn is_clickable_recognizes_button_short_name() {
        assert!(is_clickable("Button"));
        assert!(!is_clickable("XCUIElementTypeButton"));
    }

    #[test]
    fn text_field_is_clickable_and_editable() {
        assert!(is_clickable("TextField"));
        assert!(is_editable("TextField"));
        assert!(is_clickable("SearchField"));
        assert!(is_editable("SearchField"));
        assert!(is_clickable("SecureTextField"));
        assert!(is_editable("SecureTextField"));
    }

    #[test]
    fn secure_text_field_flagged_as_password() {
        let src = serde_json::json!({
            "type": "Application",
            "frame": [0, 0, 440, 956],
            "children": [{
                "type": "SecureTextField",
                "id": "pwd",
                "frame": [10, 100, 400, 40]
            }]
        });
        let els = parse_tree_hint(&src);
        assert_eq!(els.len(), 1);
        assert!(els[0].is_password);
        assert!(els[0].editable);
    }

    #[test]
    fn skips_application_root() {
        let src = serde_json::json!({
            "type": "Application",
            "frame": [0, 0, 440, 956],
            "id": "com.example",
            "children": []
        });
        let els = parse_tree_hint(&src);
        assert!(els.is_empty());
    }

    #[test]
    fn label_falls_back_to_value() {
        let src = serde_json::json!({
            "type": "Application",
            "frame": [0, 0, 440, 956],
            "children": [{
                "type": "TextField",
                "value": "hello",
                "frame": [0, 0, 200, 40]
            }]
        });
        let els = parse_tree_hint(&src);
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].text, "hello");
    }
}
