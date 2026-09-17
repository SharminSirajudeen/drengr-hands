use crate::screen::ui_element::{Bounds, UiElement};
use crate::transport::{extract_attr, parse_bounds};

/// Lightweight XML parser for Appium page source — extracts UiElements.
pub(super) fn parse_page_source(xml: &str) -> Vec<UiElement> {
    let mut elements = Vec::new();

    for line in xml.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('<') || trimmed.starts_with("</") || trimmed.starts_with("<?") {
            continue;
        }

        let class = extract_attr(trimmed, "class")
            .or_else(|| extract_attr(trimmed, "type"))
            .unwrap_or_default();

        // Skip container-only tags (hierarchy, AppiumAUT, etc.)
        if class.is_empty() && extract_attr(trimmed, "bounds").is_none() {
            continue;
        }

        let text = crate::screen::ui_element::sanitize_device_text(
            &extract_attr(trimmed, "text")
                .or_else(|| extract_attr(trimmed, "label"))
                .or_else(|| extract_attr(trimmed, "name"))
                .unwrap_or_default(),
        );
        let content_desc = crate::screen::ui_element::sanitize_device_text(
            &extract_attr(trimmed, "content-desc")
                .or_else(|| extract_attr(trimmed, "accessibility-id"))
                .unwrap_or_default(),
        );
        let resource_id = crate::screen::ui_element::sanitize_device_text(
            &extract_attr(trimmed, "resource-id").unwrap_or_default(),
        );
        // Android: bounds="[left,top][right,bottom]"
        // iOS (XCUITest): x="50" y="100" width="150" height="50"
        let bounds = extract_attr(trimmed, "bounds")
            .and_then(|b| parse_bounds(&b))
            .or_else(|| {
                let x = extract_attr(trimmed, "x")?.parse::<i32>().ok()?;
                let y = extract_attr(trimmed, "y")?.parse::<i32>().ok()?;
                let w = extract_attr(trimmed, "width")?.parse::<i32>().ok()?;
                let h = extract_attr(trimmed, "height")?.parse::<i32>().ok()?;
                Some(Bounds::new(x, y, x + w, y + h))
            })
            .unwrap_or_default();

        let flag = |name: &str, default: bool| {
            extract_attr(trimmed, name)
                .map(|v| v == "true")
                .unwrap_or(default)
        };

        let editable = class.contains("EditText")
            || class.contains("TextField")
            || class.contains("XCUIElementTypeTextField")
            || class.contains("XCUIElementTypeSecureTextField");

        elements.push(UiElement {
            class,
            text,
            content_desc,
            resource_id,
            bounds,
            clickable: flag("clickable", false),
            editable,
            is_password: flag("password", false),
            focused: flag("focused", false),
            scrollable: flag("scrollable", false),
            enabled: flag("enabled", true),
            visible: true,
            checked: flag("checked", false),
            selected: flag("selected", false),
            package: extract_attr(trimmed, "package").unwrap_or_default(),
        });
    }

    elements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_page_source() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<hierarchy>
  <node class="android.widget.Button" text="Submit" content-desc="" resource-id="btn_submit" bounds="[10,20][200,80]" clickable="true" enabled="true" scrollable="false" focused="false" checked="false" selected="false" password="false" />
  <node class="android.widget.EditText" text="" content-desc="Email" resource-id="et_email" bounds="[10,100][200,150]" clickable="true" enabled="true" scrollable="false" focused="true" checked="false" selected="false" password="false" />
</hierarchy>"#;

        let elements = parse_page_source(xml);
        assert_eq!(elements.len(), 2);

        assert_eq!(elements[0].class, "android.widget.Button");
        assert_eq!(elements[0].text, "Submit");
        assert!(elements[0].clickable);
        assert!(!elements[0].editable);

        assert_eq!(elements[1].class, "android.widget.EditText");
        assert_eq!(elements[1].content_desc, "Email");
        assert!(elements[1].editable);
        assert!(elements[1].focused);
    }

    #[test]
    fn test_parse_ios_page_source() {
        let xml = r#"<?xml version="1.0"?>
<AppiumAUT>
  <XCUIElementTypeButton type="XCUIElementTypeButton" label="Sign In" name="Sign In" bounds="[50,100][200,150]" enabled="true" />
  <XCUIElementTypeTextField type="XCUIElementTypeTextField" label="" name="Email" bounds="[50,200][300,250]" enabled="true" focused="true" />
</AppiumAUT>"#;

        let elements = parse_page_source(xml);
        assert_eq!(elements.len(), 2);

        assert_eq!(elements[0].class, "XCUIElementTypeButton");
        assert_eq!(elements[0].text, "Sign In");

        assert_eq!(elements[1].class, "XCUIElementTypeTextField");
        assert!(elements[1].editable);
    }

    #[test]
    fn test_parse_ios_xy_bounds() {
        let xml = r#"<XCUIElementTypeButton type="XCUIElementTypeButton" name="Go" x="50" y="100" width="150" height="50" enabled="true" />"#;
        let elements = parse_page_source(xml);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].bounds, Bounds::new(50, 100, 200, 150));
    }

    #[test]
    fn test_enabled_defaults_true_but_clickable_defaults_false() {
        let xml = r#"<node class="android.widget.TextView" bounds="[0,0][10,10]" />"#;
        let elements = parse_page_source(xml);
        assert_eq!(elements.len(), 1);
        assert!(elements[0].enabled);
        assert!(!elements[0].clickable);
    }
}
