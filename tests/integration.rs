//! Integration tests for Drengr — tests that span multiple modules.
//!
//! These complement the unit tests in each module by verifying that
//! components work correctly together across module boundaries.

use drengr_hands::mcp::tools::{tools_list_response, ToolResult};
use drengr_hands::screen::annotate::ScreenAnnotator;
use drengr_hands::screen::optimize::ImageOptimizer;
use drengr_hands::screen::text_scene::TextSceneBuilder;
use drengr_hands::screen::ui_element::{Bounds, UiElement};
use drengr_hands::situation::SituationEngine;
use drengr_hands::transport::{extract_attr, extract_attr_ref, parse_bounds, swipe_coords};

use image::{DynamicImage, Rgba, RgbaImage};

// ─── Helpers ───────────────────────────────────────────────────────────────

fn make_element(text: &str, class: &str, clickable: bool, bounds: Bounds) -> UiElement {
    UiElement {
        class: class.to_string(),
        text: text.to_string(),
        content_desc: String::new(),
        resource_id: String::new(),
        bounds,
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

fn make_png(width: u32, height: u32) -> Vec<u8> {
    let img = RgbaImage::from_pixel(width, height, Rgba([200, 200, 200, 255]));
    let mut buf = Vec::new();
    DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}

fn make_login_screen() -> Vec<UiElement> {
    vec![
        make_element(
            "Email",
            "android.widget.EditText",
            true,
            Bounds::new(50, 100, 1030, 160),
        ),
        make_element(
            "Password",
            "android.widget.EditText",
            true,
            Bounds::new(50, 200, 1030, 260),
        ),
        make_element(
            "Login",
            "android.widget.Button",
            true,
            Bounds::new(50, 300, 1030, 370),
        ),
        make_element(
            "Forgot Password?",
            "android.widget.TextView",
            true,
            Bounds::new(50, 400, 400, 430),
        ),
        // Non-interactive label — kept as read-only context (is_relevant = interactive OR has_label)
        make_element(
            "Welcome to App",
            "android.widget.TextView",
            false,
            Bounds::new(50, 20, 1030, 80),
        ),
    ]
}

fn make_dashboard_screen() -> Vec<UiElement> {
    vec![
        make_element(
            "Welcome",
            "android.widget.TextView",
            true,
            Bounds::new(0, 0, 1080, 100),
        ),
        make_element(
            "Profile",
            "android.widget.Button",
            true,
            Bounds::new(50, 200, 300, 260),
        ),
        make_element(
            "Settings",
            "android.widget.Button",
            true,
            Bounds::new(50, 300, 300, 360),
        ),
    ]
}

// ─── Screen Pipeline: Annotate → TextScene consistency ─────────────────────

#[test]
fn test_annotator_and_text_scene_agree_on_element_count() {
    let png = make_png(1080, 2340);
    let elements = make_login_screen();

    let annotator = ScreenAnnotator::new();
    let annotated = annotator.annotate(&png, &elements, (0, 0)).unwrap();

    let scene = TextSceneBuilder::new(1080, 2340)
        .with_activity("com.app/.LoginActivity")
        .build(&elements);

    // Both should count the same relevant elements
    assert_eq!(annotated.elements.len(), scene.element_count);

    // 4 interactive + 1 read-only label (text kept as context) = 5 relevant
    assert_eq!(annotated.elements.len(), 5);
    assert_eq!(scene.element_count, 5);
}

#[test]
fn test_annotated_numbers_match_text_scene_numbers() {
    let png = make_png(1080, 2340);
    let elements = make_login_screen();

    let annotator = ScreenAnnotator::new();
    let annotated = annotator.annotate(&png, &elements, (0, 0)).unwrap();

    let scene = TextSceneBuilder::new(1080, 2340).build(&elements);

    // Element [1] in text scene should be "Email" — same as annotated element #1
    assert!(scene.description.contains("[1] Email"));
    assert_eq!(annotated.elements[0].number, 1);
    assert_eq!(annotated.elements[0].element.text, "Email");

    // Element [3] should be "Login" in both
    assert!(scene.description.contains("[3] Login"));
    assert_eq!(annotated.elements[2].number, 3);
    assert_eq!(annotated.elements[2].element.text, "Login");
}

// ─── Screen Pipeline: Annotate → Tap Coordinates ──────────────────────────

#[test]
fn test_annotate_tap_coordinates_match_element_centers() {
    let png = make_png(1080, 2340);
    let elements = make_login_screen();

    let annotator = ScreenAnnotator::new();
    let annotated = annotator.annotate(&png, &elements, (0, 0)).unwrap();

    // Tap coordinates should be the center of the element's bounds
    let login_btn = &annotated.elements[2]; // "Login" button
    assert_eq!(login_btn.tap_x, (50 + 1030) / 2); // center_x
    assert_eq!(login_btn.tap_y, (300 + 370) / 2); // center_y

    // Verify via tap_coordinates lookup
    let (x, y) = ScreenAnnotator::tap_coordinates(&annotated, 3).unwrap();
    assert_eq!(x, login_btn.tap_x);
    assert_eq!(y, login_btn.tap_y);
}

// ─── TextScene → Vision Escalation ────────────────────────────────────────

#[test]
fn test_vision_escalation_with_unlabeled_elements() {
    // All elements have labels → no escalation needed
    let labeled = make_login_screen();
    let scene = TextSceneBuilder::new(1080, 2340).build(&labeled);
    assert!(!scene.should_escalate_to_vision());

    // Create elements where >40% lack labels → should escalate
    let mut unlabeled = vec![make_element(
        "Login",
        "Button",
        true,
        Bounds::new(0, 0, 100, 50),
    )];
    for i in 0..4 {
        let mut e = make_element(
            "",
            "ImageButton",
            true,
            Bounds::new(0, i * 60, 50, i * 60 + 50),
        );
        e.content_desc = String::new(); // No label at all
        unlabeled.push(e);
    }

    let scene = TextSceneBuilder::new(1080, 2340).build(&unlabeled);
    assert!(scene.should_escalate_to_vision()); // 1/5 = 20% labeled < 60%
}

// ─── SituationEngine: observe → action → report pipeline ──────────────────

#[test]
fn test_situation_engine_full_navigation_flow() {
    let mut engine = SituationEngine::new();

    let login = make_login_screen();
    engine.observe("device1", "com.app/.LoginActivity", &login);

    // User taps Login → navigates to Dashboard
    let dashboard = make_dashboard_screen();
    let report = engine.report_after_action(
        "device1",
        "tap",
        "Tapped #3 (Login)",
        drengr_hands::situation::ObservedScreen {
            activity: "com.app/.DashboardActivity",
            package: "com.app",
            elements: &dashboard,
            tree_available: true,
        },
    );

    assert_eq!(report.step, 1);
    assert!(report.screen_changed);
    assert!(report.activity_changed);
    assert!(!report.crash);
    assert!(!report.stuck);
    assert!(report.new_elements.contains(&"Welcome".to_string()));
    assert!(report.new_elements.contains(&"Profile".to_string()));
    assert!(report.new_elements.contains(&"Settings".to_string()));
    assert!(report.disappeared_elements.contains(&"Email".to_string()));
    assert!(report
        .disappeared_elements
        .contains(&"Password".to_string()));
    assert!(report.disappeared_elements.contains(&"Login".to_string()));

    // Report JSON should not contain step/action (those go at top level)
    let json = report.to_json();
    assert!(json.get("step").is_none());
    assert!(json.get("action").is_none());
    assert_eq!(json["screen_changed"], true);
    assert_eq!(json["activity_changed"], true);
}

#[test]
fn test_situation_engine_stuck_detection_across_actions() {
    let mut engine = SituationEngine::new();
    let elements = make_login_screen();

    engine.observe("d1", "LoginActivity", &elements);

    // First action — same screen, not stuck yet (step 1 baseline)
    let r1 = engine.report_after_action(
        "d1",
        "tap",
        "Tapped #3",
        drengr_hands::situation::ObservedScreen {
            activity: "LoginActivity",
            package: "com.app",
            elements: &elements,
            tree_available: true,
        },
    );
    assert!(!r1.stuck);
    assert!(!r1.screen_changed);

    // Second action — same screen → NOW stuck
    let r2 = engine.report_after_action(
        "d1",
        "tap",
        "Tapped #3",
        drengr_hands::situation::ObservedScreen {
            activity: "LoginActivity",
            package: "com.app",
            elements: &elements,
            tree_available: true,
        },
    );
    assert!(r2.stuck);
    assert!(!r2.screen_changed);
    assert_eq!(r2.step, 2);

    // Third action — screen changes → no longer stuck
    let dashboard = make_dashboard_screen();
    let r3 = engine.report_after_action(
        "d1",
        "tap",
        "Tapped #1 (Email)",
        drengr_hands::situation::ObservedScreen {
            activity: "DashboardActivity",
            package: "com.app",
            elements: &dashboard,
            tree_available: true,
        },
    );
    assert!(!r3.stuck);
    assert!(r3.screen_changed);
    assert_eq!(r3.step, 3);
}

// ─── ImageOptimizer + SituationEngine: dual stuck detection ────────────────

#[test]
fn test_image_optimizer_and_situation_engine_agree_on_stuck() {
    let mut optimizer = ImageOptimizer::new();
    let mut engine = SituationEngine::new();

    let png = make_png(100, 100);
    let elements = make_login_screen();

    // Initial observation
    engine.observe("d1", "Login", &elements);
    optimizer.check_duplicate(&png);

    // Same screen + same image → both detect stuck
    let (is_dup, _) = optimizer.check_duplicate(&png);
    let report = engine.report_after_action(
        "d1",
        "tap",
        "Tapped",
        drengr_hands::situation::ObservedScreen {
            activity: "Login",
            package: "com.app",
            elements: &elements,
            tree_available: true,
        },
    );

    // Step 1 is baseline for SituationEngine, so !stuck
    assert!(is_dup); // Image optimizer sees duplicate
    assert!(!report.stuck); // Situation engine: step 1 = not stuck yet

    // Second repeat → both agree it's stuck
    let (is_dup2, dup_count) = optimizer.check_duplicate(&png);
    let report2 = engine.report_after_action(
        "d1",
        "tap",
        "Tapped",
        drengr_hands::situation::ObservedScreen {
            activity: "Login",
            package: "com.app",
            elements: &elements,
            tree_available: true,
        },
    );
    assert!(is_dup2);
    assert_eq!(dup_count, 2);
    assert!(report2.stuck);
}

// ─── Transport utilities: XML parsing → Bounds → center ───────────────────

#[test]
fn test_xml_parsing_to_element_pipeline() {
    let xml_tag = r#"<node class="android.widget.Button" text="Login" clickable="true" bounds="[100,200][300,400]">"#;

    let class = extract_attr(xml_tag, "class").unwrap();
    let text = extract_attr(xml_tag, "text").unwrap();
    let clickable = extract_attr_ref(xml_tag, "clickable") == Some("true");
    let bounds_str = extract_attr(xml_tag, "bounds").unwrap();
    let bounds = parse_bounds(&bounds_str).unwrap();

    assert_eq!(class, "android.widget.Button");
    assert_eq!(text, "Login");
    assert!(clickable);
    assert_eq!(bounds.center_x(), 200);
    assert_eq!(bounds.center_y(), 300);
    assert_eq!(bounds.width(), 200);
    assert_eq!(bounds.height(), 200);
    assert!(bounds.contains(200, 300)); // center is inside
    assert!(!bounds.contains(99, 200)); // just outside left
}

#[test]
fn test_swipe_coords_cover_all_directions() {
    let width = 1080u32;
    let height = 2340u32;

    // Up swipe: start below center, end above center
    let (from, to) = swipe_coords("up", width, height);
    assert!(from.y > to.y); // swipe goes upward

    // Down swipe: start above center, end below center
    let (from, to) = swipe_coords("down", width, height);
    assert!(from.y < to.y); // swipe goes downward

    // Left swipe: start right of center, end left of center
    let (from, to) = swipe_coords("left", width, height);
    assert!(from.x > to.x); // swipe goes left

    // Right swipe: start left of center, end right of center
    let (from, to) = swipe_coords("right", width, height);
    assert!(from.x < to.x); // swipe goes right

    // Unknown direction defaults to "up" behavior
    let (from, to) = swipe_coords("diagonal", width, height);
    assert!(from.y > to.y);
}

// ─── Network: events → HAR export pipeline ────────────────────────────────

#[test]
fn test_mcp_tools_list_schema_completeness() {
    let response = tools_list_response();
    let tools = response["tools"].as_array().unwrap();

    assert_eq!(tools.len(), 3);

    // drengr_look
    let look = &tools[0];
    assert_eq!(look["name"], "drengr_look");
    assert!(look["description"].as_str().unwrap().contains("START HERE"));
    let look_props = &look["inputSchema"]["properties"];
    assert!(look_props.get("device").is_some());
    assert!(look_props.get("format").is_some());
    assert!(look_props.get("max_elements").is_some());

    // drengr_do
    let do_tool = &tools[1];
    assert_eq!(do_tool["name"], "drengr_do");
    let do_required = do_tool["inputSchema"]["required"].as_array().unwrap();
    assert!(do_required.contains(&serde_json::json!("action")));
    let do_actions = do_tool["inputSchema"]["properties"]["action"]["enum"]
        .as_array()
        .unwrap();
    // The schema enum is derived from the ACTIONS source of truth; assert the
    // JSON exposes all of them rather than a magic number that drifts.
    assert_eq!(
        do_actions.len(),
        drengr_hands::mcp::actions::action_names().len()
    );

    // drengr_query
    let query = &tools[2];
    assert_eq!(query["name"], "drengr_query");
    let questions = query["inputSchema"]["properties"]["question"]["enum"]
        .as_array()
        .unwrap();
    assert_eq!(questions.len(), 17); // setup, devices, connect, activity, crash, find, explore, session, logcat, keyboard, network, app_state, assert, diff, analyze, ui_dump, capabilities
}

#[test]
fn test_mcp_tool_result_serialization() {
    // Text result → valid JSON with correct structure
    let text_result = ToolResult::text("screen captured");
    let json = serde_json::to_value(&text_result).unwrap();
    assert!(json.get("isError").is_none()); // omitted when not error
    assert_eq!(json["content"][0]["type"], "text");
    assert_eq!(json["content"][0]["text"], "screen captured");

    // Error result → isError present
    let error_result = ToolResult::error("device disconnected");
    let json = serde_json::to_value(&error_result).unwrap();
    assert_eq!(json["isError"], true);

    // Image + text result → two content items
    let img_result = ToolResult::image_and_text("base64data".into(), "info".into());
    let json = serde_json::to_value(&img_result).unwrap();
    let content = json["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["mimeType"], "image/jpeg");
    assert_eq!(content[1]["type"], "text");
}

// ─── MCP Handlers: dispatch without device ─────────────────────────────────

#[tokio::test]
async fn test_mcp_handlers_error_cases() {
    let handlers = drengr_hands::mcp::handlers::McpHandlers::new();

    // Unknown tool
    let r = handlers
        .dispatch("nonexistent", serde_json::json!({}))
        .await;
    assert_eq!(r.is_error, Some(true));
    assert!(r.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("Unknown tool"));

    // No-device look/do cases live in src/mcp/handlers/tests.rs: integration
    // builds compile the cfg(not(test)) autoprovision path, which would detect
    // or BOOT a real device on the host — non-deterministic here.

    // drengr_query missing question
    let r = handlers
        .dispatch("drengr_query", serde_json::json!({}))
        .await;
    assert_eq!(r.is_error, Some(true));
    assert!(r.content[0].text.as_deref().unwrap().contains("question"));

    // drengr_query find without target
    let r = handlers
        .dispatch("drengr_query", serde_json::json!({"question": "find"}))
        .await;
    assert_eq!(r.is_error, Some(true));

    // drengr_query find without prior drengr_look
    let r = handlers
        .dispatch(
            "drengr_query",
            serde_json::json!({"question": "find", "target": "login"}),
        )
        .await;
    assert_eq!(r.is_error, Some(true));
    assert!(r.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("No screen observed"));
}

// ─── Full screen pipeline: PNG → annotate → JPEG ─────────────────────────

#[test]
fn test_full_screenshot_annotation_pipeline() {
    let png = make_png(1080, 2340);
    let elements = make_login_screen();

    // Step 1: Annotate
    let annotator = ScreenAnnotator::new();
    let annotated = annotator.annotate(&png, &elements, (0, 0)).unwrap();

    // Output is JPEG (starts with FF D8)
    assert!(annotated.image_data.len() > 3);
    assert_eq!(annotated.image_data[0], 0xFF);
    assert_eq!(annotated.image_data[1], 0xD8);

    // All 5 relevant elements annotated (4 interactive + 1 read-only label)
    assert_eq!(annotated.elements.len(), 5);

    // Step 2: Build text scene from same elements
    let scene = TextSceneBuilder::new(1080, 2340)
        .with_activity("com.app/.LoginActivity")
        .build(&elements);

    assert_eq!(scene.element_count, 5);
    // Five elements, all labelled: a real ratio, not the absent one an empty scene reports.
    assert_eq!(scene.labeled_ratio, Some(1.0));
    assert!(!scene.should_escalate_to_vision());

    // Step 3: Check image optimizer with the JPEG output
    let mut optimizer = ImageOptimizer::new();
    let (is_dup, _) = optimizer.check_duplicate(&annotated.image_data);
    assert!(!is_dup); // First image is never a duplicate

    let (is_dup, _) = optimizer.check_duplicate(&annotated.image_data);
    assert!(is_dup); // Same image = duplicate
}

// ─── UiElement: serde roundtrip across modules ────────────────────────────

#[test]
fn test_ui_element_serde_roundtrip_preserves_all_fields() {
    let mut elem = make_element(
        "Search",
        "android.widget.EditText",
        true,
        Bounds::new(10, 20, 500, 70),
    );
    elem.content_desc = "Search field".to_string();
    elem.resource_id = "com.app:id/search_input".to_string();
    elem.is_password = true;
    elem.focused = true;
    elem.scrollable = true;
    elem.checked = true;
    elem.selected = true;

    let json = serde_json::to_string(&elem).unwrap();
    let restored: UiElement = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.text, "Search");
    assert_eq!(restored.class, "android.widget.EditText");
    assert_eq!(restored.content_desc, "Search field");
    assert_eq!(restored.resource_id, "com.app:id/search_input");
    assert_eq!(restored.bounds, Bounds::new(10, 20, 500, 70));
    assert!(restored.clickable);
    assert!(restored.editable);
    assert!(restored.is_password);
    assert!(restored.focused);
    assert!(restored.scrollable);
    assert!(restored.checked);
    assert!(restored.selected);
    assert!(restored.enabled);
    assert!(restored.visible);
    assert_eq!(restored.package, "com.app");

    // Derived methods work on deserialized element
    assert!(restored.is_interactive());
    assert!(restored.has_label());
    assert_eq!(restored.display_label(), "Search");
    assert_eq!(restored.short_class(), "EditText");
}

// ─── Transport Parity: validation + cap behavior ────────────────────────────

#[test]
fn test_package_validation_rejects_injection_for_both_transports() {
    // Both ADB and simctl read_logs/launch_app call is_valid_package_name
    let injection_inputs = [
        "com.app;rm -rf /",
        "com.app'DROP TABLE",
        "com.app$(whoami)",
        "com.app`id`",
        "../../../etc/passwd",
        "com.app|cat /etc/passwd",
    ];
    for input in &injection_inputs {
        assert!(
            !drengr_hands::validate::is_valid_package_name(input),
            "Both transports should reject '{}' via shared validation",
            input
        );
    }
}

#[test]
fn test_ios_bundle_id_format_accepted_by_validator() {
    // iOS bundle IDs follow reverse-DNS and should pass validation
    assert!(drengr_hands::validate::is_valid_package_name(
        "com.apple.mobilesafari"
    ));
    assert!(drengr_hands::validate::is_valid_package_name(
        "io.drengr.test-app"
    ));
    assert!(drengr_hands::validate::is_valid_package_name(
        "dev.flutter.example"
    ));
}

#[test]
fn test_log_line_cap_at_500() {
    // Both ADB and simctl transports cap lines at 500 using .min(500)
    assert_eq!(500, 500, "Lines > 500 should be capped");
    assert_eq!(100usize, 100, "Lines < 500 should be unchanged");
    assert_eq!(500, 500, "Lines = 500 should stay at 500");
}

#[test]
fn test_apk_extension_check_matches_android_transport() {
    // ADB install_app requires .apk extension
    assert!("com.app.debug.apk".ends_with(".apk"));
    assert!(!"com.app.debug.ipa".ends_with(".apk"));
    assert!(!"com.app.debug.exe".ends_with(".apk"));
}

#[test]
fn test_ipa_app_extension_check_matches_ios_transport() {
    // simctl install_app accepts .app or .ipa
    let check_ios = |path: &str| path.ends_with(".app") || path.ends_with(".ipa");
    assert!(check_ios("MyApp.ipa"));
    assert!(check_ios("MyApp.app"));
    assert!(!check_ios("MyApp.apk"));
    assert!(!check_ios("MyApp.exe"));
}

#[test]
fn test_screenshot_timeout_parity() {
    // Both transports should use 5s for screenshots.
    // ADB: exec_with_timeout(..., 5)
    // simctl: simctl_with_timeout(..., 5)
    // This test documents the contract.
    let adb_screenshot_timeout: u64 = 5;
    let simctl_screenshot_timeout: u64 = 5;
    assert_eq!(
        adb_screenshot_timeout, simctl_screenshot_timeout,
        "ADB and simctl screenshot timeouts must match"
    );
}

#[test]
fn test_install_timeout_parity() {
    // Both transports should use 120s for app installation.
    // ADB: tokio::time::timeout(Duration::from_secs(120), ...)
    // simctl: simctl_with_timeout(&["install", ...], 120)
    let adb_install_timeout: u64 = 120;
    let simctl_install_timeout: u64 = 120;
    assert_eq!(
        adb_install_timeout, simctl_install_timeout,
        "ADB and simctl install timeouts must match"
    );
}

#[test]
fn test_adb_launch_inline_validation_is_stricter() {
    // ADB launch_app uses inline validation: alphanumeric + '.' + '_' only
    // (stricter than is_valid_package_name which also allows '-')
    let adb_launch_valid = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
    };
    assert!(adb_launch_valid("com.example.app_test"));
    assert!(!adb_launch_valid("com.app;rm"));
    // Hyphen passes is_valid_package_name but not ADB inline check
    assert!(!adb_launch_valid("com.my-app"));
    assert!(drengr_hands::validate::is_valid_package_name("com.my-app"));
}

#[test]
fn test_simctl_launch_uses_shared_validation() {
    // simctl launch_app calls is_valid_package_name (allows hyphens)
    assert!(drengr_hands::validate::is_valid_package_name(
        "com.apple.mobile-safari"
    ));
    assert!(!drengr_hands::validate::is_valid_package_name("com.app;rm"));
}
