use crate::screen::ui_element::UiElement;

/// Builds a ~300 token text description of the current screen.
/// Used as a cheap alternative to sending annotated screenshots.
pub struct TextSceneBuilder {
    width: u32,
    height: u32,
    activity: Option<String>,
    scrollable: bool,
}

impl TextSceneBuilder {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            activity: None,
            scrollable: false,
        }
    }

    pub fn with_activity(mut self, activity: impl Into<String>) -> Self {
        self.activity = Some(activity.into());
        self
    }

    pub fn with_scrollable(mut self, scrollable: bool) -> Self {
        self.scrollable = scrollable;
        self
    }

    /// Build by auto-numbering 1..N. Use for one-shot callers and tests; for
    /// multi-step OODA runs use `build_with_ids` so the same element keeps the
    /// same number across steps (delegate via an `ElementRegistry`).
    pub fn build(&self, elements: &[UiElement]) -> TextScene {
        self.build_capped(elements, crate::screen::ui_element::max_addressable())
    }

    /// Same selection as the annotator, with the caller's cap. Without this the
    /// text format ignored a per-call max_elements the image format honoured.
    pub fn build_capped(&self, elements: &[UiElement], max: usize) -> TextScene {
        let numbered: Vec<(usize, &UiElement)> =
            crate::screen::ui_element::addressable(elements, max)
                .into_iter()
                .enumerate()
                .map(|(i, e)| (i + 1, e))
                .collect();
        self.build_with_ids(&numbered)
    }

    /// Build from pre-numbered (stable-id, element) pairs. Same element →
    /// same number across calls when the registry is reused.
    pub fn build_with_ids(&self, numbered: &[(usize, &UiElement)]) -> TextScene {
        // Same cap as the annotator. Without it the scene listed numbers past 50
        // that no tap target existed for, and the text format is the only handle
        // an agent has in that mode.
        let interactive: Vec<(usize, &UiElement)> = numbered
            .iter()
            .filter(|(_, e)| e.is_relevant())
            .take(crate::screen::ui_element::max_addressable())
            .map(|(n, e)| (*n, *e))
            .collect();

        let mut lines = Vec::new();

        // Header: screen info
        let activity_name = self
            .activity
            .as_deref()
            .and_then(|a| a.rsplit('/').next())
            .or(self.activity.as_deref())
            .unwrap_or("Unknown");
        lines.push(format!(
            "Screen: {} ({}x{})",
            activity_name, self.width, self.height
        ));

        // Numbered elements (pre-assigned stable ids)
        for (n, elem) in &interactive {
            // display_label() falls back to the class name, so an unlabelled node
            // read as though it were labelled "View". Say what is true instead.
            let label = if elem.is_labelled() {
                elem.label_or_empty()
            } else {
                "(unlabelled)"
            };
            let class = elem.short_class();

            let mut attrs = Vec::new();
            if !elem.is_interactive() {
                attrs.push("read-only");
            }
            if elem.is_password {
                attrs.push("password");
            }
            if elem.focused {
                attrs.push("focused");
            }
            if elem.checked {
                attrs.push("checked");
            }
            if elem.selected {
                attrs.push("selected");
            }
            if elem.scrollable {
                attrs.push("scrollable");
            }

            let attr_str = if attrs.is_empty() {
                String::new()
            } else {
                format!(", {}", attrs.join(", "))
            };

            lines.push(format!("[{}] {} ({}{})", *n, label, class, attr_str));
        }

        // Footer
        if self.scrollable {
            lines.push("Scrollable — more content may be off-screen.".to_string());
        } else {
            lines.push("Not scrollable.".to_string());
        }

        let description = lines.join("\n");
        let labeled_count = interactive.iter().filter(|(_, e)| e.has_label()).count();
        let total = interactive.len();
        // None, not 1.0. A ratio of nothing is not "everything is labelled", and
        // printing 1.00 for an empty screen reads as the opposite of the truth.
        let labeled_ratio = (total > 0).then(|| labeled_count as f64 / total as f64);
        let max_id = interactive.iter().map(|(n, _)| *n).max().unwrap_or(0);

        TextScene {
            description,
            element_count: total,
            labeled_ratio,
            max_id,
        }
    }
}

/// The output of TextSceneBuilder — a text description of the screen.
#[derive(Debug, Clone)]
pub struct TextScene {
    /// The text description (~300 tokens).
    pub description: String,

    /// Number of interactive elements found.
    pub element_count: usize,

    /// Ratio of elements with text labels (0.0 to 1.0).
    /// None when there were no interactive elements to measure.
    pub labeled_ratio: Option<f64>,

    /// Highest stable id present in the scene; the LLM is told to pick a
    /// number in [1, max_id]. 0 when the scene is empty.
    pub max_id: usize,
}

impl TextScene {
    /// Whether the text scene is sufficient or vision (image) should be used instead.
    /// If >40% of elements lack labels, text alone won't help the LLM pick the right one.
    /// An empty scene escalates too: no accessibility tree at all (Flutter, webview,
    /// canvas) is where text is useless, not where it is sufficient.
    pub fn should_escalate_to_vision(&self) -> bool {
        self.element_count == 0 || self.labeled_ratio.is_some_and(|r| r < 0.6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_scene_says_unlabelled_instead_of_naming_the_class() {
        // The fourth element projection. display_label() falls back to the class,
        // so an unlabelled node read as one genuinely labelled "View".
        let mut bare = crate::screen::ui_element::UiElement {
            class: "android.view.View".to_string(),
            text: String::new(),
            content_desc: String::new(),
            resource_id: String::new(),
            bounds: crate::screen::ui_element::Bounds::new(0, 0, 10, 10),
            clickable: true,
            editable: false,
            is_password: false,
            focused: false,
            scrollable: false,
            enabled: true,
            visible: true,
            checked: false,
            selected: false,
            package: "com.app".to_string(),
        };
        let scene = TextSceneBuilder::new(100, 100).build(std::slice::from_ref(&bare));
        assert!(
            scene.description.contains("(unlabelled)"),
            "an unlabelled element must not be reported as labelled: {}",
            scene.description
        );

        bare.text = "Submit".to_string();
        let named = TextSceneBuilder::new(100, 100).build(std::slice::from_ref(&bare));
        assert!(named.description.contains("Submit"));
        assert!(!named.description.contains("(unlabelled)"));
    }
    use crate::screen::ui_element::{Bounds, UiElement};

    fn make_element(text: &str, class: &str, clickable: bool) -> UiElement {
        UiElement {
            class: class.to_string(),
            text: text.to_string(),
            content_desc: String::new(),
            resource_id: String::new(),
            bounds: Bounds::new(0, 0, 100, 50),
            clickable,
            editable: class.contains("EditText"),
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
    fn test_build_basic_scene() {
        let elements = vec![
            make_element("Email", "android.widget.EditText", true),
            make_element("Password", "android.widget.EditText", true),
            make_element("Login", "android.widget.Button", true),
        ];

        let scene = TextSceneBuilder::new(1080, 2340)
            .with_activity("com.app/.LoginActivity")
            .build(&elements);

        assert!(scene.description.contains("LoginActivity"));
        assert!(scene.description.contains("[1] Email"));
        assert!(scene.description.contains("[2] Password"));
        assert!(scene.description.contains("[3] Login"));
        assert!(scene.description.contains("Not scrollable"));
        assert_eq!(scene.element_count, 3);
    }

    #[test]
    fn test_build_with_scrollable() {
        let elements = vec![make_element("Item", "Button", true)];
        let scene = TextSceneBuilder::new(1080, 2340)
            .with_scrollable(true)
            .build(&elements);

        assert!(scene.description.contains("Scrollable"));
        assert!(scene.description.contains("more content"));
    }

    #[test]
    fn test_build_filters_hidden_includes_labeled() {
        let visible_button = make_element("Click me", "Button", true);
        let mut hidden_button = make_element("Hidden", "Button", true);
        hidden_button.visible = false;
        let non_clickable = make_element("Label", "TextView", false);
        let mut empty_non_clickable = make_element("", "FrameLayout", false);
        empty_non_clickable.content_desc = String::new();

        let elements = vec![
            visible_button,
            hidden_button,
            non_clickable,
            empty_non_clickable,
        ];
        let scene = TextSceneBuilder::new(1080, 2340).build(&elements);

        // Interactive + labeled non-interactive included; hidden and empty excluded
        assert_eq!(scene.element_count, 2);
        assert!(scene.description.contains("Click me"));
        assert!(!scene.description.contains("Hidden"));
        assert!(scene.description.contains("Label"));
        assert!(scene.description.contains("read-only")); // non-interactive marked
    }

    #[test]
    fn test_build_password_attribute() {
        let mut pwd = make_element("", "android.widget.EditText", true);
        pwd.is_password = true;
        pwd.content_desc = "Password".to_string();

        let scene = TextSceneBuilder::new(1080, 2340).build(&[pwd]);
        assert!(scene.description.contains("password"));
    }

    #[test]
    fn test_build_focused_attribute() {
        let mut focused = make_element("Email", "EditText", true);
        focused.focused = true;

        let scene = TextSceneBuilder::new(1080, 2340).build(&[focused]);
        assert!(scene.description.contains("focused"));
    }

    #[test]
    fn an_empty_screen_has_no_labelled_ratio() {
        let scene = TextSceneBuilder::new(1080, 1920).build_with_ids(&[]);
        assert_eq!(
            scene.labeled_ratio, None,
            "a ratio of nothing is not 1.0; printing 1.00 for an empty screen says everything is labelled"
        );
        assert!(
            scene.should_escalate_to_vision(),
            "an empty screen still has to escalate to vision"
        );
    }

    #[test]
    fn test_labeled_ratio_all_labeled() {
        let elements = vec![
            make_element("Email", "EditText", true),
            make_element("Login", "Button", true),
        ];
        let scene = TextSceneBuilder::new(1080, 2340).build(&elements);
        assert!((scene.labeled_ratio.expect("labelled elements exist") - 1.0).abs() < f64::EPSILON);
        assert!(!scene.should_escalate_to_vision());
    }

    #[test]
    fn test_labeled_ratio_none_labeled() {
        let mut e1 = make_element("", "Button", true);
        e1.content_desc = String::new();
        let mut e2 = make_element("", "Button", true);
        e2.content_desc = String::new();

        let scene = TextSceneBuilder::new(1080, 2340).build(&[e1, e2]);
        assert!((scene.labeled_ratio.expect("elements exist") - 0.0).abs() < f64::EPSILON);
        assert!(scene.should_escalate_to_vision());
    }

    #[test]
    fn test_labeled_ratio_mixed() {
        let labeled = make_element("Login", "Button", true);
        let mut unlabeled = make_element("", "Button", true);
        unlabeled.content_desc = String::new();

        let scene = TextSceneBuilder::new(1080, 2340).build(&[labeled, unlabeled]);
        assert!((scene.labeled_ratio.expect("elements exist") - 0.5).abs() < f64::EPSILON);
        assert!(scene.should_escalate_to_vision()); // 50% < 60% threshold
    }

    #[test]
    fn test_empty_elements() {
        let scene = TextSceneBuilder::new(1080, 2340)
            .with_activity("com.app/.Empty")
            .build(&[]);

        assert_eq!(scene.element_count, 0);
        assert_eq!(
            scene.labeled_ratio, None,
            "nothing to measure is not a full score"
        );
        assert!(scene.should_escalate_to_vision());
    }

    #[test]
    fn treeless_screen_escalates_to_vision() {
        // Flutter / webview / canvas: no accessibility tree at all. The ratio is None
        // because there is nothing to measure, which must NOT read as "text is enough".
        let scene = TextSceneBuilder::new(1080, 2340)
            .with_activity("com.app/.FlutterActivity")
            .build(&[]);

        assert_eq!(scene.element_count, 0);
        assert!(
            scene.should_escalate_to_vision(),
            "a screen with no elements is exactly where text cannot help"
        );
    }

    #[test]
    fn test_activity_extraction() {
        let scene = TextSceneBuilder::new(1080, 2340)
            .with_activity("com.app/.ui.LoginActivity")
            .build(&[]);

        // Should extract just the activity name after "/"
        assert!(
            scene.description.contains(".ui.LoginActivity")
                || scene.description.contains("LoginActivity")
        );
    }
}
